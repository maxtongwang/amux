//! Worker-token regressions: production launch program, private tmux, fake provider.
use super::*;
use crate::api::{rename_identity, worker_identity, worker_launch_env};
use std::process::{Command, Output};

const LANE: &str = "token-proof";
const TIMEOUT: Duration = Duration::from_secs(10);
// Short socket paths work on both macOS and Linux, even with a long user TMPDIR.
const PRIVATE_TMUX_ROOT: &str = "/tmp";

fn private_home(root: &Path) -> PathBuf {
    let home = root.join("amux");
    std::fs::create_dir_all(home.join("sessions")).unwrap();
    std::fs::write(
        home.join("sessions/token-proof.env"),
        "AMUX_CONTRACT_DONE=1\nAMUX_CONTRACT_RULES_OFF=1,2,3,4,5,6,7,8,9\n",
    )
    .unwrap();
    home
}

fn write_executable(path: &Path, content: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, content).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

struct PrivateTmux {
    root: tempfile::TempDir,
    label: String,
    home: PathBuf,
}

impl PrivateTmux {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("amux-token-")
            .tempdir_in(PRIVATE_TMUX_ROOT)
            .unwrap();
        let home = private_home(root.path());
        let fixture = Self {
            root,
            home,
            label: format!("amux-token-{}", ulid::Ulid::new()),
        };
        let created = fixture.tmux(&[
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            "amux-token-proof",
            "/bin/bash --noprofile --norc",
        ]);
        assert!(created.status.success(), "private tmux creation failed");
        // tmux can report exit 0 even when sandbox policy refused socket bind.
        let readback = fixture.tmux(&["has-session", "-t", "=amux-token-proof"]);
        assert!(
            readback.status.success(),
            "private tmux socket unavailable: create={}, readback={}",
            String::from_utf8_lossy(&created.stderr),
            String::from_utf8_lossy(&readback.stderr)
        );
        write_executable(
            &fixture.root.path().join("claude"),
            r#"#!/usr/bin/env python3
import hashlib,json,os,sys,time
with open(os.environ['TOKEN_PROOF_RESULT'],'w') as f:
    json.dump({'pid':os.getpid(),'argv':sys.argv[1:], 'digest':hashlib.sha256(os.environ.get('AMUX_WORKER_TOKEN','').encode()).hexdigest()},f)
time.sleep(60)
"#,
        );
        fixture
    }

    fn tmux(&self, args: &[&str]) -> Output {
        Command::new("tmux")
            .arg("-L")
            .arg(&self.label)
            .args(args)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap())
            .env("HOME", self.root.path())
            .env("TMPDIR", self.root.path())
            .env("TMUX_TMPDIR", self.root.path())
            .output()
            .unwrap()
    }

    async fn submit(&self, line: String) -> bool {
        self.tmux(&shell_line_args("=amux-token-proof:0.0", &line))
            .status
            .success()
    }

    async fn launch(&self, resume: bool, import: bool) -> (Value, String) {
        let token = worker_identity::mint(&self.home, LANE)
            .unwrap()
            .expect("rule 10 enabled");
        let result = self
            .root
            .path()
            .join(format!("result-{}.json", ulid::Ulid::new()));
        assert!(self
            .tmux(&[
                "set-environment",
                "-t",
                "=amux-token-proof",
                worker_identity::TOKEN_ENV,
                &token
            ])
            .status
            .success());
        let setup = format!(
            "export TOKEN_PROOF_RESULT={}; unset AMUX_WORKER_TOKEN; export PATH={}:\"$PATH\"",
            sh_quote(&result.to_string_lossy()),
            sh_quote(&self.root.path().to_string_lossy())
        );
        shell_command_with(LANE, &setup, false, TIMEOUT, |line| self.submit(line))
            .await
            .unwrap();
        let keys = vec![worker_identity::TOKEN_ENV.to_string()];
        let imports = if import {
            worker_launch_env::import_command("=amux-token-proof", &keys, &[])
        } else {
            String::new()
        };
        let command = format!(
            "{} {}",
            sh_quote(&self.root.path().join("claude").to_string_lossy()),
            if resume {
                "--resume fixture-conversation"
            } else {
                "--name token-proof"
            }
        );
        let program = provider_command_in_workspace(
            self.root.path().to_str().unwrap(),
            &command,
            false,
            &imports,
        );
        shell_command_with(LANE, &program, true, TIMEOUT, |line| self.submit(line))
            .await
            .unwrap();
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        let receipt = loop {
            if let Ok(bytes) = std::fs::read(&result) {
                if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                    break value;
                }
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "fake provider never wrote its launch receipt"
            );
            sleep_ms(20).await;
        };
        let inc = rename_identity::incarnation(&self.home, LANE).unwrap();
        let digest = std::fs::read_to_string(worker_identity::hash_path(&self.home, &inc)).unwrap();
        // No token values in commands typed to the pane or retained output.
        assert!(!program.contains(&token));
        (receipt, digest)
    }

    async fn stop_provider(&self, receipt: &Value) {
        let pid = receipt["pid"].as_u64().unwrap() as i32;
        assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
        // Completion of the next setup submission proves the original shell
        // survived and reaped its provider, without respawning the pane.
        shell_command_with(LANE, "true", false, TIMEOUT, |line| self.submit(line))
            .await
            .unwrap();
    }
}

impl Drop for PrivateTmux {
    fn drop(&mut self) {
        let _ = self.tmux(&["kill-server"]);
    }
}

#[tokio::test]
async fn worker_token_fresh_provider_matches_minted_digest() {
    let fixture = PrivateTmux::new();
    let (receipt, digest) = fixture.launch(false, true).await;
    assert_eq!(receipt["digest"], digest);
}

#[tokio::test]
async fn worker_token_surviving_tmux_restart_matches_rotated_digest() {
    let fixture = PrivateTmux::new();
    let (first, old_digest) = fixture.launch(false, true).await;
    assert_eq!(first["digest"], old_digest);
    let pane = fixture
        .tmux(&[
            "display-message",
            "-p",
            "-t",
            "=amux-token-proof:0.0",
            "#{pane_pid}",
        ])
        .stdout;
    fixture.stop_provider(&first).await;
    let (second, digest) = fixture.launch(false, true).await;
    assert_ne!(digest, old_digest);
    assert_eq!(second["digest"], digest);
    assert_eq!(
        fixture
            .tmux(&[
                "display-message",
                "-p",
                "-t",
                "=amux-token-proof:0.0",
                "#{pane_pid}"
            ])
            .stdout,
        pane
    );
}

#[tokio::test]
async fn worker_token_resume_provider_matches_minted_digest() {
    let fixture = PrivateTmux::new();
    let (receipt, digest) = fixture.launch(true, true).await;
    assert_eq!(receipt["argv"], json!(["--resume", "fixture-conversation"]));
    assert_eq!(receipt["digest"], digest);
}

#[tokio::test]
async fn worker_token_missing_import_emits_process_warning() {
    let fixture = PrivateTmux::new();
    let (receipt, digest) = fixture.launch(true, false).await;
    assert_ne!(
        receipt["digest"], digest,
        "negative control must actually remove the import"
    );
    let logs = super::tests::CapturedLogs(std::sync::Arc::new(std::sync::Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(logs.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        worker_launch_env::check(LANE, "claude", &[receipt["pid"].as_u64().unwrap() as u32]);
    });
    let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("WARN")
            && logs.contains("worker_token_missing_in_process")
            && logs.contains("measured=true"),
        "missing token warning was not measured: {logs}"
    );
    assert!(
        !logs.contains(&digest),
        "diagnostics must report names/status only"
    );
}

#[test]
fn worker_token_missing_process_probe_warns_without_exposing_values() {
    use std::os::unix::process::CommandExt;
    // Direct provider stand-in: no tmux or server is involved. argv[0] names
    // the provider, while the kernel process env genuinely lacks its token.
    let mut child = Command::new("/bin/sleep")
        .arg0("claude")
        .arg("30")
        .env_clear()
        .env("OTHER_KEY", "never-print-this-fixture-value")
        .spawn()
        .unwrap();
    let logs = super::tests::CapturedLogs(std::sync::Arc::new(std::sync::Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(logs.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        worker_launch_env::check(LANE, "claude", &[child.id()])
    });
    child.kill().unwrap();
    child.wait().unwrap();
    let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("WARN")
            && logs.contains("worker_token_missing_in_process")
            && logs.contains("measured=true"),
        "missing token warning was not measured: {logs}"
    );
    assert!(!logs.contains("never-print-this-fixture-value"));
}
