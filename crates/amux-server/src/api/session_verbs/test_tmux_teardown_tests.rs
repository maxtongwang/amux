//! Real child-process proofs for private test tmux lifetime (ai-config #1213).

use super::{test_tmux_socket, tmux_command};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const CHILD_MODE: &str = "AMUX_TEST_TMUX_TEARDOWN_CHILD";
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
const PROBE_INTERVAL: Duration = Duration::from_millis(20);

fn child_command(socket: &str) -> Command {
    let mut command = Command::new("tmux");
    command
        .args(["-L", socket])
        .env_remove("TMUX")
        .env_remove("TMUX_PANE");
    command
}

fn assert_no_server(socket: &str) {
    let deadline = Instant::now() + CLEANUP_TIMEOUT;
    loop {
        let out = child_command(socket).arg("list-sessions").output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !out.status.success()
            && (stderr.contains("no server running")
                || stderr.contains("No such file or directory"))
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "private server survived its test process: {socket}: {out:?}"
        );
        std::thread::sleep(PROBE_INTERVAL);
    }
}

// Clean up the specimen even when the pre-fix assertion correctly fails.
struct ChildServer(String);
impl Drop for ChildServer {
    fn drop(&mut self) {
        let _ = child_command(&self.0).arg("kill-server").output();
    }
}

fn run_child(name: &str, mode: &str) -> (ChildServer, Output) {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env(CHILD_MODE, mode)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let server = ChildServer(format!("amux-test-{}", child.id()));
    let deadline = Instant::now() + CLEANUP_TIMEOUT;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!("child test timed out: {output:?}");
        }
        std::thread::sleep(PROBE_INTERVAL);
    }
    (server, child.wait_with_output().unwrap())
}

#[test]
fn private_server_stops_on_pass_and_failure() {
    if let Ok(mode) = std::env::var(CHILD_MODE) {
        assert!(["pass", "panic", "exit", "changed-tmpdir"].contains(&mode.as_str()));
        let out = tmux_command()
            .args([
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                "exit-probe",
                "sleep 120",
            ])
            .output()
            .expect("real tmux is required for this proof");
        assert!(
            out.status.success(),
            "tmux did not create the specimen: {out:?}"
        );
        let live = tmux_command()
            .args(["has-session", "-t", "=exit-probe"])
            .output()
            .unwrap();
        assert!(
            live.status.success(),
            "the child must create a real session"
        );
        eprintln!(
            "verdict=test_tmux_specimen_created socket={} mode={mode}",
            test_tmux_socket()
        );
        if mode == "panic" {
            panic!("deliberate child test failure after creating a session");
        }
        if mode == "exit" {
            std::process::exit(23);
        }
        if mode == "changed-tmpdir" {
            // This child runs one test. Only its own environment changes.
            std::env::set_var("TMUX_TMPDIR", "/a/nonexistent/test-tmux-directory");
        }
        return;
    }

    let name = concat!(module_path!(), "::private_server_stops_on_pass_and_failure");
    let (_, name) = name.split_once("::").unwrap();
    for (mode, expected_exit) in [
        ("pass", 0),
        ("panic", 101),
        ("exit", 23),
        ("changed-tmpdir", 0),
    ] {
        let (server, out) = run_child(name, mode);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(expected_exit), "{out:?}");
        assert!(
            stderr.contains("verdict=test_tmux_specimen_created"),
            "child did not run: {out:?}"
        );
        assert!(
            stderr.contains(&server.0),
            "receipt must identify the actual child PID: {out:?}"
        );
        assert_no_server(&server.0);
        assert!(
            stderr.contains("scope=process"),
            "process cleanup must log its result: {out:?}"
        );
        eprintln!(
            "verdict=test_tmux_teardown_pass socket={} mode={mode}",
            server.0
        );
    }
}

#[test]
fn session_guard_cleans_up_on_return_and_unwind() {
    use super::test_tmux::SessionGuard;

    let sibling = SessionGuard("teardown-sibling".into());
    let out = tmux_command()
        .args([
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            &sibling.0,
            "sleep 120",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");

    for panic in [false, true] {
        let result = std::panic::catch_unwind(|| {
            let session = SessionGuard("teardown-session".into());
            let out = tmux_command()
                .args(["new-session", "-d", "-s", &session.0, "sleep 120"])
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
            if panic {
                panic!("deliberate assertion failure with a session guard");
            }
        });
        assert_eq!(result.is_err(), panic);
        let removed = tmux_command()
            .args(["has-session", "-t", "=teardown-session"])
            .output()
            .unwrap();
        assert!(
            !removed.status.success(),
            "session guard left a session behind"
        );
        let live = tmux_command()
            .args(["has-session", "-t", "=teardown-sibling"])
            .output()
            .unwrap();
        assert!(
            live.status.success(),
            "a session guard killed another test's session"
        );
    }
}
