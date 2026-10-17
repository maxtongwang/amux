//! Every tmux child this crate spawns is built here, so a TEST can never reach
//! the host's real tmux server (the fleet's panes).
//!
//! Incident, 2026-10-16: a reviewer's rename probe renamed the live
//! `amux-dev-lead` session through the default tmux socket. A test that shells
//! out to tmux with the inherited environment talks to whatever server the
//! machine runs, and on an amux box that server IS the fleet.
//!
//! The rule, applied when this is a test build (`cfg(test)`) or a process a
//! test started (`AMUX_TEST_ISOLATED=1`, exported by scripts/lib/test-isolation.sh
//! and by [`isolate_tests`]):
//!
//! - `TMUX` and `TMUX_PANE` are removed from the child, so no client attaches
//!   to the caller's server through them;
//! - `TMUX_TMPDIR` is set to a PRIVATE directory, so the default socket name
//!   resolves under it, never under the system tmp;
//! - a private directory is required: with `TMUX_TMPDIR` unset, or set to the
//!   system tmp, the socket would be the real user's default one, and the
//!   guard panics ([`assert_private_tmux_dir`]) rather than spawn.
//!
//! A unit-test process with no private dir configured gets one of its own
//! (`<tmp>/amux-test-tmux-<pid>`), and its server is killed at process exit.
//! Outside tests every constructor here is exactly `Command::new(bin)`.
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The environment variable a test harness sets to put spawned processes
/// (the server binary, the CLI) under this guard.
pub const TEST_ISOLATED_ENV: &str = "AMUX_TEST_ISOLATED";

fn guard_active() -> bool {
    cfg!(test) || std::env::var(TEST_ISOLATED_ENV).is_ok_and(|v| v == "1")
}

/// Is `dir` a socket directory that is NOT the real user's default? The
/// default is `TMUX_TMPDIR` unset (tmux uses /tmp) or the system tmp itself
/// (/tmp, /private/tmp, or the OS temp dir root such as macOS's `$TMPDIR`).
pub fn is_private_tmux_dir(dir: Option<&Path>) -> bool {
    let Some(dir) = dir.filter(|d| !d.as_os_str().is_empty()) else { return false };
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let d = canon(dir);
    let system = [PathBuf::from("/tmp"), PathBuf::from("/private/tmp"), PathBuf::from("/var/tmp"), std::env::temp_dir()];
    !system.iter().any(|s| canon(s) == d) && d != Path::new("/")
}

/// Refuse the default socket: panics naming the directory when `dir` would
/// resolve to the real user's tmux server.
pub fn assert_private_tmux_dir(dir: Option<&Path>) {
    assert!(
        is_private_tmux_dir(dir),
        "tmux guard REFUSED: TMUX_TMPDIR={:?} resolves to the real user's default tmux socket. \
         A test must never reach the host tmux server (2026-10-16: a test renamed the live \
         amux-dev-lead session). Use scripts/lib/test-isolation.sh or tmux_guard::isolate_tests().",
        dir
    );
}

/// The private socket directory for this process.
fn private_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("TMUX_TMPDIR").map(PathBuf::from) {
        if is_private_tmux_dir(Some(&d)) {
            return d;
        }
    }
    if cfg!(test) {
        return per_process_dir().clone();
    }
    let configured = std::env::var_os("TMUX_TMPDIR").map(PathBuf::from);
    assert_private_tmux_dir(configured.as_deref());
    unreachable!("assert_private_tmux_dir panics on a non-private directory")
}

/// A unit-test process's own socket directory, created once, with its tmux
/// server killed when the process exits.
fn per_process_dir() -> &'static PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let d = std::env::temp_dir().join(format!("amux-test-tmux-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        extern "C" fn kill_private_server() {
            if let Some(d) = DIR_FOR_EXIT.get() {
                kill_server_in(d);
                let _ = std::fs::remove_dir_all(d);
            }
        }
        let _ = DIR_FOR_EXIT.set(d.clone());
        // SAFETY: registering a plain extern "C" fn with no captured state.
        unsafe { libc::atexit(kill_private_server) };
        d
    })
}
static DIR_FOR_EXIT: OnceLock<PathBuf> = OnceLock::new();

/// Kill the tmux server whose sockets live in `dir` (never the default one).
pub fn kill_server_in(dir: &Path) {
    if !is_private_tmux_dir(Some(dir)) {
        return;
    }
    let _ = std::process::Command::new("tmux")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env("TMUX_TMPDIR", dir)
        .args(["kill-server"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

fn guard_std(cmd: &mut std::process::Command) {
    if guard_active() {
        let dir = private_dir();
        assert_private_tmux_dir(Some(&dir));
        cmd.env_remove("TMUX").env_remove("TMUX_PANE").env("TMUX_TMPDIR", dir);
    }
}

fn guard_tokio(cmd: &mut tokio::process::Command) {
    if guard_active() {
        let dir = private_dir();
        assert_private_tmux_dir(Some(&dir));
        cmd.env_remove("TMUX").env_remove("TMUX_PANE").env("TMUX_TMPDIR", dir);
    }
}

/// `std::process::Command::new("tmux")`, guarded.
pub fn std_tmux() -> std::process::Command {
    std_cmd("tmux")
}

/// `tokio::process::Command::new("tmux")`, guarded.
pub fn tokio_tmux() -> tokio::process::Command {
    tokio_cmd("tmux")
}

/// A guarded std command for a tmux binary path (`bin`).
pub fn std_cmd(bin: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    let mut c = std::process::Command::new(bin);
    guard_std(&mut c);
    c
}

/// A guarded tokio command for a tmux binary path (`bin`).
pub fn tokio_cmd(bin: impl AsRef<std::ffi::OsStr>) -> tokio::process::Command {
    let mut c = tokio::process::Command::new(bin);
    guard_tokio(&mut c);
    c
}

/// Put the CURRENT process, and every child it spawns, on a private tmux
/// server, for an integration test (tests/*.rs links this crate without
/// `cfg(test)`). Sets `AMUX_TEST_ISOLATED=1` and `TMUX_TMPDIR=<dir>/tmux`,
/// removes `TMUX`/`TMUX_PANE`; the returned guard kills that server on drop.
/// Process env is global: call it once, at the top of the test, and hold it.
pub fn isolate_tests(dir: &Path) -> PrivateTmux {
    let sock = dir.join("tmux");
    std::fs::create_dir_all(&sock).expect("private tmux dir");
    assert_private_tmux_dir(Some(&sock));
    std::env::remove_var("TMUX");
    std::env::remove_var("TMUX_PANE");
    std::env::set_var("TMUX_TMPDIR", &sock);
    std::env::set_var(TEST_ISOLATED_ENV, "1");
    PrivateTmux { dir: sock }
}

/// A private tmux server's socket directory; drop kills its server.
pub struct PrivateTmux {
    pub dir: PathBuf,
}

impl Drop for PrivateTmux {
    fn drop(&mut self) {
        kill_server_in(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guard refuses the default socket: unset, the system tmp, /private/tmp.
    #[test]
    fn the_default_socket_is_refused() {
        assert!(!is_private_tmux_dir(None));
        assert!(!is_private_tmux_dir(Some(Path::new(""))));
        assert!(!is_private_tmux_dir(Some(Path::new("/tmp"))));
        assert!(!is_private_tmux_dir(Some(&std::env::temp_dir())));
        let refused = std::panic::catch_unwind(|| assert_private_tmux_dir(None));
        assert!(refused.is_err(), "an unset TMUX_TMPDIR must panic");
        let refused = std::panic::catch_unwind(|| assert_private_tmux_dir(Some(Path::new("/tmp"))));
        assert!(refused.is_err(), "the system tmp must panic");
        let private = tempfile::tempdir().unwrap();
        assert!(is_private_tmux_dir(Some(private.path())), "a private subdirectory is allowed");
    }

    /// Every guarded command carries a private TMUX_TMPDIR and no TMUX/TMUX_PANE.
    #[test]
    fn guarded_commands_carry_a_private_socket() {
        let c = std_tmux();
        let envs: Vec<(String, Option<String>)> = c.get_envs()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned())))
            .collect();
        let get = |k: &str| envs.iter().find(|(n, _)| n == k).cloned();
        assert_eq!(get("TMUX"), Some(("TMUX".into(), None)), "TMUX removed");
        assert_eq!(get("TMUX_PANE"), Some(("TMUX_PANE".into(), None)), "TMUX_PANE removed");
        let dir = get("TMUX_TMPDIR").and_then(|(_, v)| v).expect("TMUX_TMPDIR set");
        assert!(is_private_tmux_dir(Some(Path::new(&dir))), "{dir}");
        let t = tokio_tmux();
        assert!(t.as_std().get_envs().any(|(k, v)| k == "TMUX_TMPDIR" && v.is_some()));
    }

    /// A rename and a new-session in a guarded test land on the private server.
    /// Only the private server is ever asked (`tmux -L`/TMUX_TMPDIR scoped);
    /// the default socket is never called, not even to list.
    #[test]
    fn a_guarded_rename_lands_on_the_private_server() {
        if std::process::Command::new("tmux").arg("-V").output().is_err() {
            eprintln!("tmux not installed; the private-server cell needs it");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("tmux");
        std::fs::create_dir_all(&sock).unwrap();
        let tm = |args: &[&str]| {
            let mut c = std_tmux();
            c.env("TMUX_TMPDIR", &sock).args(args);
            c.output().unwrap()
        };
        let made = tm(&["new-session", "-d", "-s", "amux-guard-probe", "sleep 30"]);
        assert!(made.status.success(), "new-session on the private server: {}", String::from_utf8_lossy(&made.stderr));
        assert!(tm(&["rename-session", "-t", "=amux-guard-probe", "amux-guard-renamed"]).status.success());
        let ls = String::from_utf8_lossy(&tm(&["ls", "-F", "#{session_name}"]).stdout).into_owned();
        assert_eq!(ls.trim(), "amux-guard-renamed", "the private server holds exactly the probe's session");
        kill_server_in(&sock);
        assert!(!tm(&["has-session", "-t", "=amux-guard-renamed"]).status.success(), "teardown killed the private server");
    }
}
