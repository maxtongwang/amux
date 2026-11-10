//! Own the private tmux server for a test binary and its session fixtures.

use std::ffi::OsString;
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
const CLEANUP_POLL: Duration = Duration::from_millis(10);

struct Owner {
    socket: String,
    tmpdir: Option<OsString>,
}

static OWNER: OnceLock<Owner> = OnceLock::new();

// The system exit hook needs no runtime or new dependency. Keeping this leaf
// std-only also lets the small real-tmux proof compile without the server.
unsafe extern "C" {
    fn atexit(callback: extern "C" fn()) -> std::os::raw::c_int;
}

fn owner() -> &'static Owner {
    OWNER.get_or_init(|| {
        // SAFETY: cleanup has C ABI, never unwinds, and remains valid until exit.
        assert_eq!(
            unsafe { atexit(cleanup) },
            0,
            "could not register test tmux cleanup"
        );
        let owner = Owner {
            socket: format!("amux-test-{}", std::process::id()),
            tmpdir: std::env::var_os("TMUX_TMPDIR"),
        };
        log(format_args!(
            "verdict=test_tmux_owner_registered socket={}",
            owner.socket
        ));
        owner
    })
}

pub(super) fn socket() -> String {
    owner().socket.clone()
}

fn command_for(owner: &Owner) -> Command {
    let mut command = Command::new("tmux");
    command
        .args(["-L", &owner.socket])
        .env_remove("TMUX")
        .env_remove("TMUX_PANE");
    match &owner.tmpdir {
        Some(path) => command.env("TMUX_TMPDIR", path),
        None => command.env_remove("TMUX_TMPDIR"),
    };
    command
}

pub(super) fn configure(command: &mut Command) {
    *command = command_for(owner());
}

fn log(message: std::fmt::Arguments<'_>) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "{message}");
}

fn stop(owner: &Owner, args: &[&str], scope: &str) {
    let output = (|| {
        let mut child = command_for(owner)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        let deadline = Instant::now() + CLEANUP_TIMEOUT;
        while child.try_wait()?.is_none() {
            if Instant::now() >= deadline {
                // Only this cleanup client's PID is signalled, never the server.
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "tmux cleanup timed out",
                ));
            }
            std::thread::sleep(CLEANUP_POLL);
        }
        child.wait_with_output()
    })();
    match output {
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if output.status.success() {
                log(format_args!(
                    "verdict=test_tmux_cleanup_stopped socket={} scope={scope}",
                    owner.socket
                ));
            } else if !stderr.contains("no server running")
                && !stderr.contains("No such file or directory")
                && !stderr.contains("can't find session")
            {
                log(format_args!("verdict=test_tmux_cleanup_failed socket={} scope={scope} status={} error={stderr}", owner.socket, output.status));
            }
        }
        Err(error) => log(format_args!(
            "verdict=test_tmux_cleanup_failed socket={} scope={scope} error={error}",
            owner.socket
        )),
    }
}

extern "C" fn cleanup() {
    // libtest catches failed assertions and exits normally with code 101.
    // Process cleanup must wait until then, so one panic cannot kill peers'
    // fixtures while they are still running in this binary.
    let result = std::panic::catch_unwind(|| {
        if let Some(owner) = OWNER.get() {
            stop(owner, &["kill-server"], "process");
        }
    });
    if result.is_err() {
        log(format_args!(
            "verdict=test_tmux_cleanup_failed scope=process error=cleanup_panicked"
        ));
    }
}

pub(super) struct SessionGuard(pub(super) String);

impl Drop for SessionGuard {
    fn drop(&mut self) {
        let target = format!("={}", self.0);
        stop(owner(), &["kill-session", "-t", &target], "session");
    }
}
