//! Reap child processes nobody is waiting on (Mac RCA 2026-10-13: the live
//! server, pid 35820, held 616 zombie children).
//!
//! The server deploys by exec'ing the new build into its own pid (self-adoption,
//! AMUX-3458). Every child the previous image started is still this pid's child,
//! and when it exits the new image holds no handle for it, so nothing calls
//! `waitpid` and it stays a zombie for the life of the process. Measured: 4
//! zombies born at 17:23:50Z, the exact second of a self-exec, and 80-200 more
//! every day since the process started on 10-08. A few were born at other times,
//! which is a spawn site dropping its `Child` without waiting; this reaps those
//! too.
//!
//! A zombie is reaped only when it was ALSO a zombie on the previous tick. A
//! child whose `Child` handle is alive is reaped by its owner within moments of
//! exiting (tokio waits on SIGCHLD), so a zombie that survives a whole tick has
//! no waiter, and `waitpid` on it cannot take a status someone else expects.

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::Duration;

fn tick_secs() -> u64 {
    std::env::var("AMUX_ZOMBIE_REAP_TICK_S")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(60)
}

/// Pids of `parent`'s zombie children in `ps -A -o pid=,ppid=,stat=` output.
pub(crate) fn zombie_children(ps_out: &str, parent: i32) -> BTreeSet<i32> {
    ps_out
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse::<i32>().ok()?;
            let ppid = it.next()?.parse::<i32>().ok()?;
            let stat = it.next()?;
            (ppid == parent && stat.starts_with('Z')).then_some(pid)
        })
        .collect()
}

/// The zombies to reap now: those seen on the previous tick too.
pub(crate) fn due(previous: &BTreeSet<i32>, now: &BTreeSet<i32>) -> Vec<i32> {
    now.intersection(previous).copied().collect()
}

/// `waitpid` each pid without blocking; returns (reaped, failed).
pub(crate) fn reap(pids: &[i32]) -> (usize, usize) {
    let (mut reaped, mut failed) = (0usize, 0usize);
    for pid in pids {
        let mut status: libc::c_int = 0;
        // SAFETY: waitpid on one specific pid with WNOHANG; it only writes the
        // child's exit status into `status`.
        let r = unsafe { libc::waitpid(*pid, &mut status, libc::WNOHANG) };
        if r == *pid { reaped += 1; } else { failed += 1; }
    }
    (reaped, failed)
}

fn seen() -> &'static Mutex<BTreeSet<i32>> {
    static S: Mutex<BTreeSet<i32>> = Mutex::new(BTreeSet::new());
    &S
}

async fn tick() {
    let me = std::process::id() as i32;
    let out = match tokio::process::Command::new("ps").args(["-A", "-o", "pid=,ppid=,stat="]).output().await {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        Ok(o) => {
            tracing::warn!(status = ?o.status, verdict = "orphan_zombies_unmeasured", measured = false,
                "zombie reaper: ps failed; children not checked this tick");
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, verdict = "orphan_zombies_unmeasured", measured = false,
                "zombie reaper: could not run ps; children not checked this tick");
            return;
        }
    };
    let now = zombie_children(&out, me);
    let to_reap = {
        let mut prev = seen().lock().unwrap_or_else(|p| p.into_inner());
        let d = due(&prev, &now);
        *prev = now.clone();
        d
    };
    let (reaped, failed) = reap(&to_reap);
    if reaped > 0 || failed > 0 {
        tracing::warn!(reaped, failed, zombies = now.len(), pid = me, verdict = "orphan_zombies_reaped",
            measured = true, n_considered = now.len(),
            "reaped zombie children nobody was waiting on (children of a pre-exec image, or a dropped Child)");
    } else {
        tracing::debug!(zombies = now.len(), pid = me, verdict = "orphan_zombies_checked", measured = true,
            n_considered = now.len(), "zombie reaper: nothing due");
    }
}

pub fn spawn() {
    let secs = tick_secs();
    if secs == 0 {
        tracing::info!(verdict = "orphan_zombie_reaper_disabled", "AMUX_ZOMBIE_REAP_TICK_S=0: zombie reaper off");
        return;
    }
    let interval = Duration::from_secs(secs);
    let h = tokio::spawn(async move {
        let mut t = tokio::time::interval(interval);
        loop {
            t.tick().await;
            let id = crate::runtime_jobs::registry::ids::ZOMBIE_REAPER;
            crate::runtime_jobs::registry::tick_start(id);
            tick().await;
            crate::runtime_jobs::registry::tick_end(id);
        }
    });
    crate::runtime_jobs::registry::adopt(crate::runtime_jobs::registry::ids::ZOMBIE_REAPER, Some(interval), &h);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_only_this_processes_zombies() {
        let ps = "  100     1 Ss\n  200   100 Z\n  201   100 Z+\n  202   100 S\n  300   999 Z\n bad line\n";
        assert_eq!(zombie_children(ps, 100), [200, 201].into_iter().collect());
        assert!(zombie_children(ps, 1).is_empty());
    }

    #[test]
    fn a_zombie_is_reaped_only_after_surviving_a_whole_tick() {
        let first: BTreeSet<i32> = [200, 201].into_iter().collect();
        assert!(due(&BTreeSet::new(), &first).is_empty(), "a new zombie may still have a waiter");
        let second: BTreeSet<i32> = [201, 202].into_iter().collect();
        assert_eq!(due(&first, &second), vec![201]);
    }

    /// The real thing: a child dropped without waiting becomes a zombie, and
    /// the reaper's waitpid clears it. Only this test's own pid is reaped, so
    /// no concurrently running test loses a child it is waiting on.
    #[tokio::test]
    async fn reaps_a_real_dropped_child() {
        let child = std::process::Command::new("true").spawn().expect("spawn true");
        let pid = child.id() as i32;
        drop(child);
        // Process enumeration is a host privilege on macOS. The production
        // waitpid boundary can prove a real dropped child was reaped without
        // ps; the parser and two-tick selection have independent assertions.
        const REAP_ATTEMPTS: usize = 50;
        const REAP_RETRY_DELAY: Duration = Duration::from_millis(50);
        let observed = [pid].into_iter().collect();
        assert!(due(&BTreeSet::new(), &observed).is_empty());
        let candidates = due(&observed, &observed);
        assert_eq!(candidates, vec![pid]);
        let mut reaped = false;
        for _ in 0..REAP_ATTEMPTS {
            if reap(&candidates) == (1, 0) { reaped = true; break; }
            tokio::time::sleep(REAP_RETRY_DELAY).await;
        }
        assert!(reaped, "production waitpid must reap the real dropped child");
        assert_eq!(reap(&candidates), (0, 1), "a second reap must not claim success");
        let mut status = 0;
        // SAFETY: this is the test's own child PID; WNOHANG never waits.
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) }, -1);
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ECHILD), "child has actually been reaped");
    }
}
