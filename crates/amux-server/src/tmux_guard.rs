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
//! - `TMUX_TMPDIR` is set to a PRIVATE directory AND the socket is named
//!   explicitly with `-S <dir>/tmux-<uid>/default` (the path tmux itself would
//!   resolve there), as the FIRST argument. TMUX_TMPDIR alone is not enough:
//!   inside a pane `$TMUX` wins over it, and a test's `kill-server` killed the
//!   live server that way (2026-10-16 outage). An explicit `-S` wins over both;
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
        .arg("-S")
        .arg(socket_in(dir))
        .args(["kill-server"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// The socket tmux resolves for `dir` as TMUX_TMPDIR (`<dir>/tmux-<uid>/default`),
/// with its directory created 0700, so an explicit `-S` names the same server
/// a TMUX_TMPDIR-only caller in the same test reaches.
pub fn socket_in(dir: &Path) -> PathBuf {
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    let parent = dir.join(format!("tmux-{uid}"));
    if std::fs::create_dir_all(&parent).is_ok() {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700));
    }
    parent.join("default")
}

fn guard_std(cmd: &mut std::process::Command) {
    if guard_active() {
        let dir = private_dir();
        assert_private_tmux_dir(Some(&dir));
        cmd.env_remove("TMUX").env_remove("TMUX_PANE").env("TMUX_TMPDIR", &dir).arg("-S").arg(socket_in(&dir));
    }
}

fn guard_tokio(cmd: &mut tokio::process::Command) {
    if guard_active() {
        let dir = private_dir();
        assert_private_tmux_dir(Some(&dir));
        cmd.env_remove("TMUX").env_remove("TMUX_PANE").env("TMUX_TMPDIR", &dir).arg("-S").arg(socket_in(&dir));
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

    /// Every guarded command names its socket explicitly (`-S <private>/tmux-<uid>/default`,
    /// first argument) and carries no TMUX/TMUX_PANE.
    #[test]
    fn guarded_commands_carry_a_private_socket() {
        let c = std_tmux();
        let envs: Vec<(String, Option<String>)> = c.get_envs()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned())))
            .collect();
        let get = |k: &str| envs.iter().find(|(n, _)| n == k).cloned();
        assert_eq!(get("TMUX"), Some(("TMUX".into(), None)), "TMUX removed");
        assert_eq!(get("TMUX_PANE"), Some(("TMUX_PANE".into(), None)), "TMUX_PANE removed");
        let args: Vec<String> = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args.first().map(String::as_str), Some("-S"), "explicit socket first: {args:?}");
        let sock = PathBuf::from(&args[1]);
        assert!(sock.ends_with("default"), "{sock:?}");
        let dir = sock.parent().and_then(Path::parent).expect("<dir>/tmux-<uid>/default");
        assert!(is_private_tmux_dir(Some(dir)), "{dir:?}");
        let t = tokio_tmux();
        let targs: Vec<String> = t.as_std().get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(targs.first().map(String::as_str), Some("-S"), "tokio commands too: {targs:?}");
    }

    fn tmux_present() -> bool {
        std_tmux().arg("-V").output().is_ok_and(|o| o.status.success())
    }

    /// A tmux command naming one explicit socket, TMUX/TMUX_PANE cleared.
    fn on_socket(sock: &Path, args: &[&str]) -> std::process::Output {
        std::process::Command::new("tmux")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .arg("-f")
            .arg("/dev/null")
            .arg("-S")
            .arg(sock)
            .args(args)
            .output()
            .unwrap()
    }

    /// A rename and a new-session land on the private socket named with an
    /// explicit -S; the default socket is never called, not even to list.
    #[test]
    fn a_guarded_rename_lands_on_the_private_server() {
        if !tmux_present() {
            eprintln!("tmux not installed; the private-server cell needs it");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let sock = socket_in(&dir.path().join("tmux"));
        let made = on_socket(&sock, &["new-session", "-d", "-s", "amux-guard-probe", "sleep 30"]);
        assert!(made.status.success(), "new-session on the private server: {}", String::from_utf8_lossy(&made.stderr));
        // Exact-match targets, named `st` for tests/tmux_target_audit.rs.
        let st = "=amux-guard-probe";
        assert!(on_socket(&sock, &["rename-session", "-t", st, "amux-guard-renamed"]).status.success());
        let ls = String::from_utf8_lossy(&on_socket(&sock, &["ls", "-F", "#{session_name}"]).stdout).into_owned();
        assert_eq!(ls.trim(), "amux-guard-renamed", "the private server holds exactly the probe's session");
        kill_server_in(&dir.path().join("tmux"));
        let st = "=amux-guard-renamed";
        assert!(!on_socket(&sock, &["has-session", "-t", st]).status.success(), "teardown killed the private server");
    }

    /// THE OUTAGE, reproduced against a decoy: a DECOY tmux server on its own
    /// socket stands in for the live fleet, and `TMUX` points at it as it does
    /// inside a pane. The guarded send-keys, kill-session and kill-server paths
    /// (and kill_server_in) must leave the decoy and its session untouched,
    /// even with TMUX forced onto the child after the guard ran. Fails if the
    /// guard relied on TMUX_TMPDIR alone, which TMUX overrides.
    #[test]
    fn a_decoy_server_survives_every_guarded_destructive_path() {
        if !tmux_present() {
            eprintln!("tmux not installed; the decoy cell needs it");
            return;
        }
        let decoy_dir = tempfile::tempdir().unwrap();
        let decoy = decoy_dir.path().join("decoy.sock");
        let made = on_socket(&decoy, &["new-session", "-d", "-s", "decoy-live", "sleep 300"]);
        assert!(made.status.success(), "decoy server: {}", String::from_utf8_lossy(&made.stderr));
        let pid = String::from_utf8_lossy(&on_socket(&decoy, &["display-message", "-p", "#{pid}"]).stdout).trim().to_string();
        let tmux_env = format!("{},{pid},0", decoy.display());
        let st = "=decoy-live";
        let paths: [&[&str]; 3] = [&["send-keys", "-t", st, "echo pwned", "Enter"], &["kill-session", "-t", st], &["kill-server"]];
        for args in paths {
            let mut c = std_tmux();
            c.env("TMUX", &tmux_env).env("TMUX_PANE", "%0").args(args);
            let _ = c.output();
        }
        // kill_server_in, the teardown, with the decoy as the inherited TMUX of
        // this process's children is covered by its own explicit -S.
        kill_server_in(decoy_dir.path().join("not-the-decoy").as_path());
        assert!(on_socket(&decoy, &["has-session", "-t", st]).status.success(),
            "the decoy server and its session must survive every guarded destructive path");
        let pane = String::from_utf8_lossy(&on_socket(&decoy, &["capture-pane", "-p", "-t", st]).stdout).into_owned();
        assert!(!pane.contains("pwned"), "send-keys must not reach the decoy pane: {pane}");
        let _ = on_socket(&decoy, &["kill-server"]);
    }
}
