//! /api/map/location through the real router (AMUX-5458).
//!
//! Writes are owner-only: with an owner token configured, an ingest without
//! that bearer is refused even though the router-level test has no peer
//! address (the same refusal a worker on loopback gets). With the bearer it is
//! stored once, a retry is a duplicate, and the timeline reads it back with
//! `measured` and `n_considered`.

use amux_server::api::{router, AppState};
use amux_server::db::Store;
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

fn app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("AMUX_HOME", dir.path());
    // These requests carry a synthetic loopback ConnectInfo that no process
    // holds, so process attribution (api/process_identity.rs) would rightly
    // refuse them as unproven; this file tests the owner-bearer rules alone.
    std::env::set_var("AMUX_PROCESS_ATTRIBUTION", "0");
    let store = std::sync::Arc::new(Store::open(&dir.path().join("amux-test.db")).unwrap());
    let state = AppState {
        store,
        started: std::time::Instant::now(),
        build_hash: "test".into(),
        auth_token: Some("owner-tok".into()),
        reconciled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
    };
    (router(state), dir)
}

async fn call(app: &axum::Router, method: &str, uri: &str, bearer: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
    call_from(app, method, uri, bearer, body, false).await
}

/// `loopback`: the request arrives from 127.0.0.1, as every worker's does. The
/// router's auth layer lets loopback through, so this is the case where the
/// location module's own owner rule is the only thing standing.
async fn call_from(app: &axum::Router, method: &str, uri: &str, bearer: Option<&str>, body: Option<Value>, loopback: bool) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(t) = bearer {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let req = match body {
        Some(v) => b.header(header::CONTENT_TYPE, "application/json").body(Body::from(v.to_string())).unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let mut req = req;
    if loopback {
        req.extensions_mut().insert(axum::extract::ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 40000))));
    }
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn ingest_is_owner_only_idempotent_and_readable() {
    let (app, _dir) = app();
    let t = 1_790_000_000.0;
    let batch = json!({"device": "iphone-test", "points": [
        {"id": "p1", "ts": t, "lat": 40.7, "lon": -74.0, "h_acc": 5.0, "activity": "walking"},
        {"id": "p2", "ts": t + 10.0, "lat": 40.7001, "lon": -74.0, "h_acc": 5.0, "activity": "walking"},
    ]});

    let (s, _) = call(&app, "POST", "/api/map/location/points", None, Some(batch.clone())).await;
    assert!(s == StatusCode::FORBIDDEN || s == StatusCode::UNAUTHORIZED, "no bearer must not write: {s}");
    let (s, _) = call(&app, "POST", "/api/map/location/points", Some("wrong"), Some(batch.clone())).await;
    assert!(s == StatusCode::FORBIDDEN || s == StatusCode::UNAUTHORIZED, "a wrong bearer must not write: {s}");

    // THE CASE THE MODULE EXISTS FOR: a worker on this machine (loopback, so
    // the general auth layer lets it through) with no owner bearer.
    let (s, v) = call_from(&app, "POST", "/api/map/location/points", None, Some(batch.clone()), true).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "loopback without the owner bearer must not write history: {v}");
    // Loopback still reads, so the owner's agents can look things up.
    let (s, _) = call_from(&app, "GET", "/api/map/location/summary", None, None, true).await;
    assert_eq!(s, StatusCode::OK);

    let (s, v) = call(&app, "POST", "/api/map/location/points", Some("owner-tok"), Some(batch.clone())).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["accepted"], 2);
    let (_, v) = call(&app, "POST", "/api/map/location/points", Some("owner-tok"), Some(batch)).await;
    assert_eq!((v["accepted"].as_i64(), v["duplicate"].as_i64()), (Some(0), Some(2)), "{v}");

    let (s, v) = call(&app, "GET", &format!("/api/map/location/timeline?from={}&to={}", t - 60.0, t + 3600.0),
        Some("owner-tok"), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["measured"], true);
    assert_eq!(v["n_considered"], 2);

    let (_, v) = call(&app, "GET", "/api/map/location/summary", Some("owner-tok"), None).await;
    assert_eq!(v["points"], 2);
    assert_eq!(v["devices"][0]["device"], "iphone-test");
}

/// Raw capture: a fix the cleaned view would skip is still stored and
/// exported with every field; motion is owner-only; stats and the heatmap
/// say whether they measured.
#[tokio::test]
async fn raw_fixes_motion_stats_heatmap_and_export() {
    let (app, _dir) = app();
    let t = 1_790_000_000.0;
    let batch = json!({"device": "iphone-test", "points": [
        {"id": "good", "ts": t, "lat": 40.7, "lon": -74.0, "h_acc": 5.0, "speed": 1.2, "speed_acc": 0.4, "ell_alt": -31.5,
         "floor": 3, "simulated": false, "accessory": false, "age_s": 0.3},
        {"id": "poor", "ts": t + 1.0, "lat": 40.7, "lon": -74.0, "h_acc": 450.0, "speed": -1.0, "course": -1.0},
    ]});
    let (_, v) = call(&app, "POST", "/api/map/location/points", Some("owner-tok"), Some(batch)).await;
    assert_eq!(v["accepted"], 2, "a poor fix is stored raw, not refused: {v}");
    let (_, v) = call(&app, "GET", &format!("/api/map/location/timeline?from={}&to={}", t - 60.0, t + 60.0), Some("owner-tok"), None).await;
    assert_eq!((v["n_considered"].as_i64(), v["n_raw"].as_i64()), (Some(1), Some(2)), "{v}");

    let motion = json!({"device": "iphone-test", "motion": [{"id": "m1", "ts": t, "walking": true, "confidence": "high"}]});
    let (s, _) = call_from(&app, "POST", "/api/map/location/motion", None, Some(motion.clone()), true).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "motion is owner-only too");
    let (s, v) = call(&app, "POST", "/api/map/location/motion", Some("owner-tok"), Some(motion)).await;
    assert_eq!((s, v["accepted"].as_i64()), (StatusCode::OK, Some(1)), "{v}");

    let (s, v) = call(&app, "GET", &format!("/api/map/location/stats?from={}&to={}&bucket=week", t - 86400.0, t + 86400.0), Some("owner-tok"), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!((v["measured"].as_bool(), v["n_considered"].as_i64()), (Some(true), Some(1)), "{v}");
    let (_, v) = call(&app, "GET", "/api/map/location/heatmap", Some("owner-tok"), None).await;
    assert_eq!((v["measured"].as_bool(), v["n_considered"].as_i64(), v["n_cells"].as_i64()), (Some(true), Some(1), Some(1)), "{v}");

    let req = Request::builder()
        .uri(format!("/api/map/location/export?from={}&to={}&format=csv", t - 60.0, t + 60.0))
        .header(header::AUTHORIZATION, "Bearer owner-tok")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["x-amux-rows"], "2");
    assert!(res.headers()[header::CONTENT_DISPOSITION].to_str().unwrap().ends_with(".csv\""));
    let csv = String::from_utf8(axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec()).unwrap();
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines.len(), 3, "{csv}");
    assert!(lines[0].contains("ell_alt") && lines[0].contains("speed_acc") && lines[0].contains("simulated"));
    assert!(lines[2].contains("450") && lines[2].contains("-1"), "the poor fix is exported raw: {csv}");
}
