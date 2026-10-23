// Private tmux recovery proof — validates socket ownership before any signal; refs #20.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn decide_private_tmux_target(
    target_pid: u32,
    private_socket: &Path,
    tempdir: &Path,
    private_owner_pid: Option<u32>,
    live_pid: Option<u32>,
    live_socket: &Path,
) -> Result<(), String> {
    let refusal = |reason: &str| {
        format!(
            "{reason}: target_pid={target_pid} private_owner_pid={private_owner_pid:?} live_pid={live_pid:?} private_socket={} live_socket={}",
            private_socket.display(), live_socket.display()
        )
    };
    if !private_socket.starts_with(tempdir) || private_socket == tempdir {
        return Err(refusal("private socket is outside test tempdir"));
    }
    if target_pid <= 1 || target_pid > i32::MAX as u32 || target_pid == std::process::id() {
        return Err(refusal("invalid signal target"));
    }
    if private_owner_pid != Some(target_pid) {
        return Err(refusal("target pid does not own private socket"));
    }
    if live_pid == Some(target_pid) {
        return Err(refusal("target pid is the live default tmux server"));
    }
    Ok(())
}

fn read_tmux_pid(socket: &Path) -> Result<Option<u32>, String> {
    let output = Command::new("tmux")
        .arg("-S")
        .arg(socket)
        .args(["-N", "display-message", "-p", "#{pid}"])
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env_remove("TMUX_TMPDIR")
        .output()
        .map_err(|e| format!("tmux read-only pid at {}: {e}", socket.display()))?;
    if !output.status.success() {
        return Ok(None);
    }
    let pid = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .map_err(|e| format!("invalid tmux pid at {}: {e}", socket.display()))?;
    Ok(Some(pid))
}

fn guarded_private_tmux_pid(private_socket: &Path, tempdir: &Path) -> Result<u32, String> {
    // Never query the real default socket. Only the private server is contacted; the
    // inherited pane's PID is an additional refusal check, not a source of test state.
    let live_socket = PathBuf::from(format!("/tmp/tmux-{}/default", unsafe { libc::geteuid() }));
    let live_pid = std::env::var("TMUX").ok()
        .and_then(|v| v.split(',').nth(1).and_then(|pid| pid.parse().ok()));
    let canonical_tempdir = tempdir.canonicalize().map_err(|e| format!("private tempdir: {e}"))?;
    let canonical_socket = private_socket.canonicalize().map_err(|e| format!("private socket: {e}"))?;
    if !canonical_socket.starts_with(&canonical_tempdir) || canonical_socket == canonical_tempdir {
        return Err("private socket resolved outside tempdir; refusing even a read".into());
    }
    let private_owner_pid = read_tmux_pid(&canonical_socket)?;
    let target_pid = private_owner_pid.ok_or("private tmux server is not running")?;
    decide_private_tmux_target(target_pid, &canonical_socket, &canonical_tempdir,
        private_owner_pid, live_pid, &live_socket)?;
    let identity = Command::new("tmux").arg("-S").arg(&canonical_socket)
        .args(["-N", "display-message", "-p", "#{socket_path}"])
        .env_remove("TMUX").env_remove("TMUX_PANE").env_remove("TMUX_TMPDIR")
        .output().map_err(|e| format!("private socket identity: {e}"))?;
    let reported = PathBuf::from(String::from_utf8_lossy(&identity.stdout).trim());
    if !identity.status.success() || reported.canonicalize().ok().as_ref() != Some(&canonical_socket) {
        return Err("private server returned a different socket identity".into());
    }
    eprintln!("tmux guard: pid={target_pid} private_socket={}", canonical_socket.display());
    Ok(target_pid)
}

#[test]
fn private_tmux_signal_guard_refuses_unsafe_targets() {
    let tmp = Path::new("/tmp/fixture");
    let private = tmp.join("tmux-501/default");
    let live = Path::new("/tmp/tmux-501/default");
    assert!(decide_private_tmux_target(44, &private, tmp, Some(44), Some(44), live).is_err());
    assert!(decide_private_tmux_target(44, live, tmp, Some(44), None, live).is_err());
    assert!(decide_private_tmux_target(44, &private, tmp, Some(45), None, live).is_err());
    assert!(decide_private_tmux_target(44, &private, tmp, Some(44), Some(45), live).is_ok());
    assert!(decide_private_tmux_target(0, &private, tmp, Some(0), None, live).is_err());
    assert!(decide_private_tmux_target(1, &private, tmp, Some(1), None, live).is_err());
    assert!(decide_private_tmux_target(u32::MAX, &private, tmp, Some(u32::MAX), None, live).is_err());
}

#[test]
fn private_tmux_guard_refuses_a_socket_symlink_before_any_probe() {
    let d = tempfile::tempdir().unwrap();
    let private = d.path().join("private");
    std::fs::create_dir(&private).unwrap();
    let outside = d.path().join("outside");
    std::fs::write(&outside, "not a socket").unwrap();
    let socket = private.join("socket");
    std::os::unix::fs::symlink(&outside, &socket).unwrap();
    assert_eq!(guarded_private_tmux_pid(&socket, &private).unwrap_err(),
        "private socket resolved outside tempdir; refusing even a read");
}

// This is the sole tmux PID signal site in this specimen. The read-only guard
// must finish successfully before SIGKILL is even attempted.
fn signal_private_tmux(private_socket: &Path, tempdir: &Path) {
    let pid = guarded_private_tmux_pid(private_socket, tempdir)
        .unwrap_or_else(|e| panic!("tmux signal refused: {e}"));
    let result = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    assert_eq!(result, 0, "private tmux SIGKILL failed: {}", std::io::Error::last_os_error());
}

struct RecoveryRig {
    temp: Option<tempfile::TempDir>,
    root: PathBuf,
    home: PathBuf,
    socket: PathBuf,
    starts: PathBuf,
    log: PathBuf,
    port: u16,
    server: Option<Child>,
}

impl RecoveryRig {
    fn new() -> Self {
        let temp = tempfile::Builder::new().prefix("tmux-watch-").tempdir_in("/tmp").expect("private test tempdir");
        let home = temp.path().join("home");
        std::fs::create_dir_all(home.join("sessions")).unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert_ne!(port, 8824, "fixture must never bind the live port");
        let socket = temp.path().join(format!("tmux-{}/default", unsafe { libc::geteuid() }));
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(socket.parent().unwrap(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let starts = temp.path().join("provider-starts.txt");
        let log = temp.path().join("server.log");
        let bin = temp.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let fake = bin.join("claude");
        // Every provider invocation records its worker's cwd, then stays alive
        // so the recovered server can adopt its surviving tmux session.
        std::fs::write(&fake, format!("#!/bin/sh\nprintf '%s\\n' \"$(basename \"$PWD\")\" >> '{}'\nprintf '? for shortcuts\\n'\nexec sleep 300\n", starts.display())).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        for name in ["mrpopo", "alpha", "beta", "stopped"] {
            let dir = temp.path().join(name);
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(home.join("sessions").join(format!("{name}.env")),
                format!("CC_NAME={name}\nCC_DIR={}\nCC_PROVIDER=claude\nCC_ISOLATED=1\n", dir.display())).unwrap();
        }
        let root = temp.path().to_path_buf();
        Self { temp: Some(temp), root, home, socket, starts, log, port, server: None }
    }

    fn tmux(&self, args: &[&str]) -> std::process::Output {
        Command::new("tmux").arg("-S").arg(&self.socket).args(args)
            .env_remove("TMUX").env_remove("TMUX_PANE").env_remove("TMUX_TMPDIR")
            .output().expect("private tmux command")
    }

    fn seed_running_fleet(&self) {
        // These are real provider processes in a private tmux server. The
        // server's first real watch tick must discover this running census.
        let fake = self.root.as_path().join("bin/claude");
        for name in ["mrpopo", "alpha", "beta", "stopped"] {
            let session = format!("amux-{name}");
            let cwd = self.root.as_path().join(name);
            let out = Command::new("tmux")
                .arg("-S").arg(&self.socket).args(["-f", "/dev/null", "new-session", "-d", "-s"])
                .arg(&session).arg("-c").arg(&cwd).args(["/bin/sh", "-i"])
                .env_remove("TMUX").env_remove("TMUX_PANE").env_remove("TMUX_TMPDIR")
                .output().unwrap();
            assert!(out.status.success(), "private tmux seed: {}", String::from_utf8_lossy(&out.stderr));
            let line = fake.to_string_lossy().into_owned();
            let sent = self.tmux(&["send-keys", "-t", &session, &line, "Enter"]);
            assert!(sent.status.success(), "private fake provider submission: {}", String::from_utf8_lossy(&sent.stderr));
        }
        assert!(self.socket.starts_with(self.root.as_path()), "private socket must be inside test tempdir");
        let census = self.tmux(&["list-sessions", "-F", "#{session_name}"]);
        assert!(census.status.success(), "private census: stdout={} stderr={} starts={}",
            String::from_utf8_lossy(&census.stdout), String::from_utf8_lossy(&census.stderr),
            std::fs::read_to_string(&self.starts).unwrap_or_default());
    }

    fn spawn_server(&mut self) {
        let out = std::fs::OpenOptions::new().create(true).append(true).open(&self.log).unwrap();
        let err = out.try_clone().unwrap();
        let bin_dir = self.root.as_path().join("bin");
        let path = format!("{}:{}", bin_dir.display(), std::env::var("PATH").unwrap_or_default());
        let child = Command::new(env!("CARGO_BIN_EXE_amux-server"))
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", path)
            .env("SHELL", "/bin/sh")
            .env("AMUX_HOME", &self.home)
            .env("AMUX_DB", self.root.as_path().join("test.db"))
            .env("AMUX_BIND", "127.0.0.1")
            .env("AMUX_RS_PORT", self.port.to_string())
            .env("AMUX_AUTH_TOKEN", "none")
            .env("AMUX_NO_SELF_ADOPT", "1")
            .env("AMUX_RS_BOOTSTRAP_SECS", "3600")
            .env("AMUX_ALWAYS_ALIVE", "mrpopo")
            .env("AMUX_CLAUDE_CMD", bin_dir.join("claude"))
            .env("AMUX_ALLOW_TMUX_SPAWN_FROM_TEST_HOME", "1")
            .env("AMUX_TMUX_WATCH_RESTORE_STEP_DELAY_MS", "8000")
            .env("TMUX_TMPDIR", self.root.as_path())
            .env("RUST_LOG", "info")
            .stdin(Stdio::null()).stdout(out).stderr(err)
            .spawn().expect("spawn private server");
        self.server = Some(child);
    }

    fn kill_server(&mut self) {
        if let Some(mut child) = self.server.take() {
            child.kill().expect("SIGKILL own server child");
            child.wait().unwrap();
        }
    }

    fn start_count(&self, name: &str) -> usize {
        std::fs::read_to_string(&self.starts).unwrap_or_default().lines().filter(|line| *line == name).count()
    }

    fn state(&self) -> serde_json::Value {
        std::fs::read_to_string(self.home.join("tmux-watch.json"))
            .ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    async fn wait_for(&self, claim: &str, mut ready: impl FnMut(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(90);
        while Instant::now() < deadline {
            if ready(self) { return; }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let diagnostic = std::fs::read_to_string(&self.log).unwrap_or_default();
        let relevant = diagnostic.lines().filter(|line| line.contains("tmux_watch") || line.contains("tmux") || line.contains("ERROR") || line.contains("error")).rev().take(25).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
        panic!("timed out waiting for {claim}; state={} starts={} relevant_log={relevant}", self.state(),
            std::fs::read_to_string(&self.starts).unwrap_or_default(),
        );
    }
}

impl Drop for RecoveryRig {
    fn drop(&mut self) {
        if let Some(mut child) = self.server.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if self.socket.exists() {
            // Panic during cleanup must not mask the original failure. The
            // signal function itself still refuses before signalling.
            let _ = std::panic::catch_unwind(|| signal_private_tmux(&self.socket, self.root.as_path()));
        }
        if std::thread::panicking() {
            if let Some(temp) = self.temp.take() {
                let path = temp.keep();
                eprintln!("failed recovery fixture retained at {}", path.display());
            }
        }
    }
}

#[tokio::test]
async fn sigkill_mid_restore_restarts_pending_workers_and_adopts_surviving_starts_once() {
    let mut rig = RecoveryRig::new();
    rig.seed_running_fleet();
    rig.wait_for("four fake providers in initial fleet", |r| ["mrpopo", "alpha", "beta", "stopped"].iter().all(|n| r.start_count(n) == 1)).await;
    rig.spawn_server();
    rig.wait_for("running fleet persisted by real tick", |r| {
        r.state()["workers"].as_array().is_some_and(|a| a.len() == 4) && r.state()["was_up"] == true
    }).await;
    // Real API stop leaves the tmux shell/session alive. A subsequent good census must
    // replace the recorded running set before the injected whole-server loss.
    let response = reqwest::Client::builder().timeout(Duration::from_secs(10)).build().unwrap()
        .post(format!("http://127.0.0.1:{}/api/sessions/stopped/stop", rig.port))
        .send().await.expect("stop fixture worker through API");
    assert!(response.status().is_success(), "API stop failed: {}", response.status());
    rig.wait_for("API-stopped worker excluded from last good census", |r| {
        r.state()["workers"].as_array().is_some_and(|a| a.len() == 3 && !a.iter().any(|n| n == "stopped"))
    }).await;
    assert!(rig.tmux(&["has-session", "-t", "amux-stopped"]).status.success(),
        "API stop must leave a real shell session for the regression");
    assert_eq!(rig.start_count("stopped"), 1);
    assert_eq!([rig.start_count("mrpopo"), rig.start_count("alpha"), rig.start_count("beta")], [1, 1, 1],
        "initial fleet must remain alive until the injected loss");

    // No tmux PID is signalled until the guard verifies the private identity and ownership.
    signal_private_tmux(&rig.socket, rig.root.as_path());
    rig.wait_for("confirmed loss and pending work durably persisted", |r| {
        r.state()["last_loss_at"].is_number()
            && r.state()["pending"].as_array().is_some_and(|a| a.iter().any(|n| n == "beta"))
    }).await;
    rig.wait_for("keeper and first fleet worker started, last worker still pending", |r| {
        r.start_count("mrpopo") == 2 && r.start_count("alpha") == 2 && r.start_count("beta") == 1
    }).await;
    assert!(rig.state()["pending"].as_array().is_some_and(|a| a.iter().any(|n| n == "beta")), "beta must remain durably pending");
    assert!(!std::fs::read_to_string(&rig.log).unwrap_or_default().contains("verdict=\"fleet_restored\""), "no success verdict before all starts");
    rig.kill_server();
    rig.spawn_server();
    rig.wait_for("pending recovery completed after real process restart", |r| {
        r.start_count("beta") == 2 && r.state()["pending"].as_array().is_some_and(|a| a.is_empty())
    }).await;
    assert_eq!(rig.start_count("stopped"), 1, "a deliberately API-stopped worker must never be restored");
    assert!(!rig.tmux(&["has-session", "-t", "amux-stopped"]).status.success(),
        "the stopped shell disappeared with the lost server and must stay absent");
    let counts = [rig.start_count("mrpopo"), rig.start_count("alpha"), rig.start_count("beta")];
    let log = std::fs::read_to_string(&rig.log).unwrap_or_default();
    let recovery_log = log.lines().filter(|line| line.contains("tmux_watch") || line.contains("tmux_watch:") || line.contains("already running") || line.contains("provider_start") || line.contains("worker_started") || line.contains("start_session"))
        .rev().take(45).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
    assert_eq!(counts, [2, 2, 2], "surviving starts adopted; no provider was started twice; state={} census={} recovery_log={recovery_log}",
        rig.state(), String::from_utf8_lossy(&rig.tmux(&["list-sessions", "-F", "#{session_name}"]).stdout));
    assert!(std::fs::read_to_string(&rig.log).unwrap_or_default().contains("fleet_restored"), "completion verdict follows actual recovery");
}
