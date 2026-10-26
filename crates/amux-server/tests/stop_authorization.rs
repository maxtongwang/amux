//! Stop uses the existing group gate before scheduling any process lifecycle work.
use amux_server::{
    api::{self, AppState},
    db::Store,
};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::Value;
use std::{sync::Arc, time::Instant};
use tower::ServiceExt;

#[tokio::test]
async fn stop_authorization_and_unknown_worker() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("sessions")).unwrap();
    for (name, group) in [
        ("lead", "business"),
        ("target", "business"),
        ("outsider", "other"),
    ] {
        std::fs::write(
            home.path().join(format!("sessions/{name}.env")),
            format!("CC_TAGS={group}\nCC_WORKER_TYPE=chat\n"),
        )
        .unwrap();
    }
    // This integration test owns its process and all worker state.
    unsafe {
        std::env::set_var("AMUX_HOME", home.path());
    }
    let state = AppState {
        store: Arc::new(Store::open(&home.path().join("test.db")).unwrap()),
        started: Instant::now(),
        build_hash: "test".into(),
        auth_token: None,
        reconciled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
    };
    let app = api::session_verbs::routes().with_state(state.clone());
    let mut completed = 0;
    for (caller, target, expected) in [
        ("outsider", "target", StatusCode::FORBIDDEN),
        ("lead", "missing", StatusCode::NOT_FOUND),
        ("lead", "target", StatusCode::ACCEPTED),
        ("", "target", StatusCode::ACCEPTED),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/sessions/{target}/stop"))
                    .header("X-Amux-Worker", caller)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            expected,
            "caller={caller}, target={target}"
        );
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        if expected.is_client_error() {
            assert!(
                body["error"].as_str().is_some_and(|s| !s.is_empty()),
                "{body}"
            );
        } else {
            assert_eq!(body["ok"], true);
            completed += 1;
            let deadline = Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let count = state.store.read().unwrap().query_row(
                    "SELECT count(*) FROM session_events WHERE session='target' AND type='session.stopped'",
                    [], |row| row.get::<_, i64>(0)).unwrap_or(0);
                if count == completed {
                    break;
                }
                assert!(Instant::now() < deadline, "accepted stop never completed");
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        }
    }
}
