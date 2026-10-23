//! AF-789: actual startup/periodic paths, with no access to real tmux.
#![cfg(unix)]
use amux_server::runtime_jobs::{pane_size, registry};
use std::{
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Clone)]
struct LogWriter(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn row() -> registry::Snapshot {
    registry::snapshot()
        .into_iter()
        .find(|r| r.id == "pane_size")
        .expect("actual spawn must register pane_size")
}

async fn completed(ticks: u64) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while row().ticks < ticks {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("actual pane-size tick must finish");
}

#[test]
fn pane_size_startup_and_periodic_work_share_isolation() {
    let Ok(mode) = std::env::var("AMUX_PANE_ISOLATION_TEST_MODE") else {
        let scratch = tempfile::tempdir().unwrap();
        let fake = scratch.path().join("tmux");
        // PATH contains ONLY this directory. Even a malformed fixture cannot
        // fall through to the developer's tmux server. /bin/sh uses builtins.
        std::fs::write(&fake, "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$AMUX_PANE_ISOLATION_TEST_CALLS\"\nif [ \"$1\" = list-windows ]; then\n  printf 'amux-af789|80|24|0|manual\\n'\nfi\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        for mode in [
            "AMUX_ISOLATED",
            "AMUX_NO_FLEET",
            "AMUX_PANE_SIZE_SECS",
            "enabled",
        ] {
            let calls = scratch.path().join(format!("{mode}.calls"));
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "pane_size_startup_and_periodic_work_share_isolation",
                    "--nocapture",
                ])
                .env("AMUX_PANE_ISOLATION_TEST_MODE", mode)
                .env("AMUX_PANE_ISOLATION_TEST_CALLS", &calls)
                .env("PATH", scratch.path())
                .env("AMUX_TMUX_COLS", "220")
                .env("AMUX_TMUX_ROWS", "50")
                .env_remove("AMUX_ISOLATED")
                .env_remove("AMUX_NO_FLEET")
                .env_remove("AMUX_PANE_SIZE_SECS");
            if mode != "enabled" {
                command.env(
                    mode,
                    if mode == "AMUX_PANE_SIZE_SECS" {
                        "0"
                    } else {
                        "1"
                    },
                );
            }
            let output = command.output().unwrap();
            eprintln!(
                "{mode}: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                output.status.success(),
                "pane-size subprocess failed: {mode}"
            );
        }
        return;
    };
    let calls = std::env::var_os("AMUX_PANE_ISOLATION_TEST_CALLS").unwrap();
    // A positive control in EVERY case proves this process can execute and
    // observe the fake command; a missing/broken fixture cannot count as zero.
    assert!(std::process::Command::new("tmux")
        .arg("fixture-control")
        .status()
        .unwrap()
        .success());
    assert_eq!(
        std::fs::read_to_string(&calls).unwrap(),
        "fixture-control\n"
    );
    std::fs::write(&calls, "").unwrap();
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let writer = LogWriter(bytes.clone());
    tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_max_level(tracing::Level::INFO)
        .with_writer(move || writer.clone())
        .try_init()
        .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        pane_size::note_resize("amux-af789");
        let job = pane_size::spawn();
        if mode == "enabled" {
            completed(1).await;
            let first = std::fs::read_to_string(&calls).unwrap();
            let lines: Vec<_> = first.lines().collect();
            assert_eq!(
                lines.len(),
                4,
                "boot must restore even with a fresh lease: {first}"
            );
            assert!(lines[0].starts_with("list-windows -a -F "));
            assert_eq!(lines[1], "set-option -t =amux-af789: default-size 220x50");
            assert_eq!(lines[2], "resize-window -t =amux-af789: -x 220 -y 50");
            assert_eq!(lines[3], "set-option -w -t =amux-af789: window-size latest");
            assert!(row().disabled_reason.is_none());
            pane_size::note_resize("amux-af789");
            assert!(registry::trigger("pane_size"));
            completed(2).await;
            let second = std::fs::read_to_string(&calls).unwrap();
            assert!(second.starts_with(&first));
            let extra: Vec<_> = second[first.len()..].lines().collect();
            assert_eq!(
                extra.len(),
                1,
                "later tick must respect fresh viewer lease: {second}"
            );
            assert!(extra[0].starts_with("list-windows -a -F "));
        } else {
            // Drive the runtime long enough for the OLD unguarded startup
            // task to execute. The pre-fix specimen records all four calls.
            tokio::time::sleep(Duration::from_secs(1)).await;
            assert!(job.is_finished());
            assert_eq!(row().ticks, 0);
            assert_eq!(
                row().disabled_reason.as_deref(),
                Some(
                    format!(
                        "{mode}={}",
                        if mode == "AMUX_PANE_SIZE_SECS" { 0 } else { 1 }
                    )
                    .as_str()
                )
            );
            assert!(!registry::trigger("pane_size"));
            assert_eq!(
                std::fs::read_to_string(&calls).unwrap(),
                "",
                "isolated startup MUST NOT enumerate or resize panes"
            );
        }
        job.abort();
    });
    let logs = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    eprintln!("{logs}");
    if mode == "enabled" {
        assert_eq!(logs.matches("one-shot repair complete").count(), 1);
        assert!(logs.contains("count=1") && logs.contains("amux-af789"));
        assert!(!logs.contains("periodic job suppressed"));
    } else {
        assert!(logs.contains("periodic job suppressed") && logs.contains(&mode));
        assert!(
            !logs.contains("restoring detached window")
                && !logs.contains("one-shot repair complete")
        );
    }
}
