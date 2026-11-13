//! Fleet memory follows live worker lifecycle; refs maxtongwang/ai-config#1492.
use amux_server::api::{router, AppState};
use amux_server::db::Store;
use axum::{body::Body, http::{Request, StatusCode}};
use serde_json::{json, Value};
use sha2::Digest;
use std::{os::unix::fs::PermissionsExt, path::PathBuf, sync::Arc};
use tower::ServiceExt;

struct Fleet {
    root: tempfile::TempDir,
    app: axum::Router,
}

impl Fleet {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path();
        std::fs::create_dir_all(path.join("sessions")).unwrap();
        std::fs::create_dir_all(path.join("bin")).unwrap();
        std::fs::write(path.join("bin/tmux"), r#"#!/bin/sh
case "$1" in
  list-sessions) cat "$AMUX_HOME/live" ;;
  capture-pane) if [ -f "$AMUX_HOME/stopped" ]; then printf '$ '; fi ;;
  send-keys) case "$*" in *'/exit'*) touch "$AMUX_HOME/stopped";; esac ;;
esac
exit 0
"#).unwrap();
        std::fs::set_permissions(path.join("bin/tmux"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("AMUX_HOME", path);
        std::env::set_var("HOME", path);
        std::env::set_var("PATH", format!("{}:/usr/bin:/bin", path.join("bin").display()));
        std::fs::write(path.join("live"), "amux-reader\namux-retiring\namux-isolated\namux-archived\namux-held\n").unwrap();
        for name in ["reader", "retiring", "stopped", "isolated", "archived", "held"] {
            std::fs::create_dir_all(path.join(name)).unwrap();
            let flags = match name {
                "isolated" => "CC_ISOLATED=1\n",
                "archived" => "CC_ARCHIVED=1\n",
                "held" => "CC_REVIEW_HELD=1\n",
                _ => "",
            };
            std::fs::write(path.join(format!("sessions/{name}.env")),
                format!("CC_DIR={}/{name}\nCC_TAGS=dev\n{flags}", path.display())).unwrap();
        }
        let store = Store::open(&path.join("test.db")).unwrap();
        let app = router(AppState {
            store: Arc::new(store), started: std::time::Instant::now(),
            build_hash: "fixture".into(), auth_token: None,
            reconciled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        });
        Self { root, app }
    }

    fn memory(&self, name: &str) -> PathBuf {
        let project: String = format!("{}/{name}", self.root.path().display()).chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        self.root.path().join(".claude/projects").join(project).join("memory/MEMORY.md")
    }

    async fn call(&self, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
        let ui_token = hex::encode(sha2::Sha256::digest(b"amux-ui-guard:"));
        let response = self.app.clone().oneshot(Request::builder().method(method).uri(path)
            .header("x-amux-ui-token", &ui_token[..40])
            .header("Content-Type", "application/json").body(Body::from(body.to_string())).unwrap()).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    async fn compose(&self) {
        for name in ["reader", "retiring"] {
            let (status, body) = self.call("POST", &format!("/api/sessions/{name}/memory"), json!({"content":"fixture"})).await;
            assert_eq!(status, StatusCode::OK, "{body}");
        }
    }

    async fn wait_for_memory(&self, needle: &str, present: bool) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let memory = std::fs::read_to_string(self.memory("reader")).unwrap();
                if memory.contains(needle) == present { break; }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        }).await.expect("lifecycle must refresh fleet memory within five seconds");
    }
}

#[tokio::test]
async fn stopped_worker_with_env_is_not_in_fleet_roster() {
    let f = Fleet::new();
    f.compose().await;
    let memory = std::fs::read_to_string(f.memory("reader")).unwrap();
    assert!(memory.contains("| `reader` |") && memory.contains("| `retiring` |"), "{memory}");
    assert!(!memory.contains("| `stopped` |"), "stopped env leaked into fleet roster: {memory}");
    for name in ["isolated", "archived", "held"] {
        assert!(!memory.contains(&format!("| `{name}` |")), "{name} leaked into roster: {memory}");
    }
    assert!(memory.contains("native subagents (Agent tool)"), "{memory}");
}

#[tokio::test]
async fn stop_archive_and_rename_refresh_fleet_rosters_without_a_timer() {
    for verb in ["stop", "archive", "rename"] {
        let f = Fleet::new();
        f.compose().await;
        std::fs::write(f.root.path().join("live"), "amux-reader\n").unwrap();
        let (method, path, body) = if verb == "rename" {
            ("PATCH", "/api/sessions/retiring/config".to_string(), json!({"rename":"renamed"}))
        } else {
            ("POST", format!("/api/sessions/retiring/{verb}"), json!({}))
        };
        let (status, body) = f.call(method, &path, body).await;
        let expected = if verb == "stop" { StatusCode::ACCEPTED } else { StatusCode::OK };
        assert_eq!(status, expected, "{verb}: {body}");
        f.wait_for_memory("| `retiring` |", false).await;
        let memory = std::fs::read_to_string(f.memory("reader")).unwrap();
        assert!(memory.contains("| `reader` |"), "{verb}: {memory}");
        assert!(!memory.contains("| `retiring` |"), "{verb}: {memory}");
    }
}

#[tokio::test]
async fn delete_refreshes_other_workers_and_retired_project_fleet_rosters() {
    let f = Fleet::new();
    f.compose().await;
    let retired_file = f.memory("retiring");
    let notes = std::fs::read_to_string(&retired_file).unwrap() + "\n## Owner notes\nKeep this exact note.\n";
    std::fs::write(&retired_file, notes).unwrap();
    std::fs::write(f.root.path().join("live"), "amux-reader\n").unwrap();
    let (status, body) = f.call("POST", "/api/sessions/retiring/delete", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!f.root.path().join("sessions/retiring.env").exists());
    for name in ["reader", "retiring"] {
        let memory = std::fs::read_to_string(f.memory(name)).unwrap();
        assert!(!memory.contains("| `retiring` |"), "deleted worker in {}: {memory}", f.memory(name).display());
    }
    assert!(std::fs::read_to_string(retired_file).unwrap().contains("Keep this exact note."));
}

#[tokio::test]
async fn fleet_roster_check_detects_stale_and_missing_tables_then_passes_after_refresh() {
    let f = Fleet::new();
    f.compose().await;
    let file = f.memory("reader");
    let memory = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, memory.replace("| `reader` |", "| `ghost` |")).unwrap();
    let (status, body) = f.call("GET", "/api/health/fleet-roster", Value::Null).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["exit_status"], 1);
    assert_eq!(body["measured"], true);
    assert!(body.to_string().contains("ghost") && body.to_string().contains("reader"), "{body}");
    assert!(body.to_string().contains(file.to_str().unwrap()), "{body}");
    let (status, body) = f.call("POST", "/api/sessions/reader/start", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    f.wait_for_memory("| `ghost` |", false).await;
    let (status, body) = f.call("GET", "/api/health/fleet-roster", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["exit_status"], 0);
    assert!(body["n_considered"].as_u64().unwrap() >= 2, "{body}");
    std::fs::write(&file, "<!-- amux:session-memory -->\nfixture\n").unwrap();
    let (status, body) = f.call("GET", "/api/health/fleet-roster", Value::Null).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    std::fs::rename(f.root.path().join(".claude/projects"), f.root.path().join("saved-projects")).unwrap();
    let (status, body) = f.call("GET", "/api/health/fleet-roster", Value::Null).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["measured"], false);
    assert_eq!(body["n_considered"], 0);
}
