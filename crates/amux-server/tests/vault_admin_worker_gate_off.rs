//! Security rule 1 (maxtongwang/amux, contract 11e6bea91): with the fork gate
//! AMUX_ADMIN_WORKERS_AS_OWNER off (the default), an admin-group worker is an
//! ordinary worker on the vault: its item is held pending, and it cannot
//! activate or remove items. Successor of upstream's
//! `admin_group_workers_add_edit_and_remove_vault_items_and_others_cannot`,
//! which still runs with the gate on in tests/vault_admin_worker.rs.
//!
//! One test function on purpose: it sets AMUX_HOME for this process.

use amux_server::api::{router, AppState};
use axum::body::Body;
use axum::http::Request;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "test-owner-token-vault-admin-gate-off";

async fn call(app: &axum::Router, method: &str, path: &str, body: Option<Value>, worker: Option<&str>) -> (u16, Value, String) {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {TOKEN}"));
    if let Some(w) = worker {
        b = b.header("x-amux-session", w);
    }
    let req = b.body(body.map(|v| Body::from(v.to_string())).unwrap_or_else(Body::empty)).unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status().as_u16();
    let bytes = axum::body::to_bytes(res.into_body(), 8 * 1024 * 1024).await.unwrap();
    let raw = String::from_utf8_lossy(&bytes).into_owned();
    (status, serde_json::from_str(&raw).unwrap_or(Value::Null), raw)
}

#[tokio::test]
async fn admin_group_workers_are_ordinary_workers_on_the_vault_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("amux-home");
    std::fs::create_dir_all(home.join("sessions")).unwrap();
    std::fs::write(home.join("sessions/boss.env"), "CC_TAGS=\"ops,admin\"\n").unwrap();
    let tmux_dir = dir.path().join("tmux");
    std::fs::create_dir_all(&tmux_dir).unwrap();
    std::env::set_var("TMUX_TMPDIR", &tmux_dir);
    std::env::remove_var("TMUX");
    std::env::set_var("AMUX_HOME", &home);
    std::env::remove_var("AMUX_ADMIN_WORKERS_AS_OWNER");
    let store = amux_server::db::Store::open(&home.join("test.db")).unwrap();
    let app = router(AppState {
        store: Arc::new(store),
        started: std::time::Instant::now(),
        build_hash: "test".into(),
        auth_token: Some(TOKEN.into()),
        reconciled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
    });

    let card = json!({"name": "admin card", "kind": "card",
        "fields": {"number": "4111111111111111", "exp": "12/39", "cvc": "321", "name": "Test Holder", "zip": "10001"},
        "rules": {"max_usd_without_approval": 25.0}});
    let (s, b, raw) = call(&app, "POST", "/api/vault", Some(card), Some("boss")).await;
    assert_eq!(s, 201, "{raw}");
    assert_eq!(b["item"]["status"], "pending_owner_activation", "gate off: an admin worker's item waits for the owner: {raw}");
    let id = b["item"]["id"].as_str().unwrap().to_string();
    let (s, _, raw) = call(&app, "PATCH", &format!("/api/vault/{id}"), Some(json!({"activate": true})), Some("boss")).await;
    assert_eq!(s, 403, "gate off: an admin worker may not activate: {raw}");
    let (s, _, raw) = call(&app, "DELETE", &format!("/api/vault/{id}"), None, Some("boss")).await;
    assert_eq!(s, 403, "gate off: an admin worker may not delete: {raw}");
    // The owner still can.
    let (s, b, raw) = call(&app, "PATCH", &format!("/api/vault/{id}"), Some(json!({"activate": true})), None).await;
    assert_eq!(s, 200, "{raw}");
    assert_eq!(b["item"]["status"], "active", "{raw}");
}
