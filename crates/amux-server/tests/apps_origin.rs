//! Apps inside amux on their own origin (`/app/<id>/`, apps_origin.rs), through
//! the real routers: the dashboard origin hands a signed-in person a one-time
//! link, the apps origin trades it for a cookie bound to that one app, and
//! serves nothing else. Replaces tests/port_proxy.rs (`/proxy/` is gone).

use amux_server::api::{apps_router, router, AppState};
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use tower::ServiceExt;

const TOKEN: &str = "owner-secret-for-apps-test";
const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"; // approved
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"; // pending (a worker asked)
const C: &str = "cccccccccccccccccccccccccccccccc"; // approved, another app
const D: &str = "dddddddddddddddddddddddddddddddd"; // approved, but a refused port

fn home() -> &'static std::path::Path {
    static HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let h = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("AMUX_HOME", h.path());
            std::env::set_var("AMUX_APPS_URL", "https://apps.test:8443");
        }
        h
    })
    .path()
}

fn dashboard() -> axum::Router {
    home();
    let db = tempfile::tempdir().unwrap();
    let store = amux_server::db::Store::open(&db.path().join("t.db")).unwrap();
    std::mem::forget(db);
    router(AppState {
        store: std::sync::Arc::new(store),
        started: std::time::Instant::now(),
        build_hash: "test".into(),
        auth_token: Some(TOKEN.into()),
        reconciled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
    })
}

fn owner_cookie() -> String {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(format!("amux-owner-session:{TOKEN}"));
    format!("__Host-amux_owner={}", hex::encode(h.finalize()))
}

/// A loopback app that answers with the request head it received.
async fn echo_app() -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else { return };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).replace('<', "&lt;");
                let body = format!("<h1>internal app</h1><pre>{head}</pre>");
                let _ = s
                    .write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nSet-Cookie: amux_member=evil; Path=/\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes())
                    .await;
            });
        }
    });
    port
}

fn write_links(port: u16) {
    let links = serde_json::json!({"links": [
        {"id": A, "kind": "internal", "port": port, "approved_by": "owner"},
        {"id": B, "kind": "internal", "port": port, "approved_by": ""},
        {"id": C, "kind": "internal", "port": port, "approved_by": "owner"},
        {"id": D, "kind": "internal", "port": 2019, "approved_by": "owner"},
    ]});
    std::fs::write(home().join("public-links.json"), links.to_string()).unwrap();
}

async fn open(id: &str, person: bool) -> (StatusCode, serde_json::Value) {
    let mut b = Request::builder().method("POST").uri(format!("/api/page-links/{id}/open"))
        .header(header::HOST, "sandbox.amux.io").header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
    if person {
        b = b.header(header::COOKIE, owner_cookie());
    }
    let res = dashboard().oneshot(b.body(Body::empty()).unwrap()).await.unwrap();
    let st = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    (st, serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null))
}

async fn app_get(path: &str, cookie: Option<&str>) -> (StatusCode, String, String, String) {
    let mut b = Request::builder().uri(path).header(header::HOST, "apps.test:8443");
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    let res = apps_router().oneshot(b.body(Body::empty()).unwrap()).await.unwrap();
    let st = res.status();
    let loc = res.headers().get(header::LOCATION).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let set = res.headers().get_all(header::SET_COOKIE).iter().filter_map(|v| v.to_str().ok()).collect::<Vec<_>>().join(" | ");
    let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    (st, loc, set, String::from_utf8_lossy(&body).to_string())
}

#[tokio::test]
async fn an_app_opens_on_its_own_origin_only_from_pages_and_only_itself() {
    let port = echo_app().await;
    write_links(port);

    // Only a signed-in person gets a handoff, and only for an approved app.
    let (st, _) = open(A, false).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "a worker's bearer got an app handoff");
    let (st, _) = open(B, true).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "a pending app got a handoff");
    let (st, v) = open(A, true).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let url = v["url"].as_str().unwrap().to_string();
    assert!(url.starts_with(&format!("https://apps.test:8443/app/{A}/?amux_t=")), "{url}");
    let path = url.trim_start_matches("https://apps.test:8443");

    // Without the handoff or a cookie: nothing.
    let (st, _, _, body) = app_get(&format!("/app/{A}/"), None).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("Pages"), "{body}");

    // The token is traded for a cookie bound to this app, once.
    let (st, loc, set, _) = app_get(path, None).await;
    assert_eq!((st, loc.as_str()), (StatusCode::SEE_OTHER, format!("/app/{A}/").as_str()));
    assert!(set.contains(&format!("amux_app_{A}=")) && set.contains("HttpOnly") && set.contains(&format!("Path=/app/{A}/")), "{set}");
    let session = set.split(';').next().unwrap().to_string();
    let (st, _, _, _) = app_get(path, None).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "a handoff token worked twice");

    // With the cookie: the app, and nothing of amux's reaches it or comes back.
    let cookies = format!("amux_door=d; __Host-amux_owner=o; appsid=1; {session}");
    let (st, _, set, body) = app_get(&format!("/app/{A}/page?x=1"), Some(&cookies)).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert!(body.contains("<h1>internal app</h1>") && body.contains("GET /page?x=1 "), "{body}");
    assert!(body.contains("appsid=1"), "the app's own cookie passes: {body}");
    for leaked in ["amux_door", "amux_owner", "amux_app_", TOKEN] {
        assert!(!body.contains(leaked), "{leaked} reached the app: {body}");
    }
    assert!(!set.contains("amux_member"), "the app set an amux cookie: {set}");

    // That cookie opens this app only; a planted value opens nothing.
    let other = session.replace(A, C);
    let (st, _, _, _) = app_get(&format!("/app/{C}/"), Some(&other)).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "one app's session opened another");
    let (st, _, _, _) = app_get(&format!("/app/{A}/"), Some(&format!("amux_app_{A}=planted"))).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "a planted cookie opened the app");

    // A pending app, a refused port, and anything that is not /app/: never.
    let (st, _, _, _) = app_get(&format!("/app/{B}/"), Some(&session.replace(A, B))).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _, _, _) = app_get(&format!("/app/{D}/"), None).await;
    assert_ne!(st, StatusCode::OK, "Caddy's admin port opened");
    for p in ["/", "/api/sessions", "/api/page-links", "/pub/x/", "/proxy/8891/"] {
        let (st, _, _, _) = app_get(p, None).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "the apps origin served {p}");
    }
}

/// `/proxy/<port>/` was removed (Ethan, 2026-10-25: "remove the proxies"): the
/// dashboard answers it like any unknown path, never by relaying an app.
#[tokio::test]
async fn the_dashboard_no_longer_serves_proxy_links() {
    let port = echo_app().await;
    let res = dashboard()
        .oneshot(Request::builder().uri(format!("/proxy/{port}/")).header(header::HOST, "localhost:8824")
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}")).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert!(!String::from_utf8_lossy(&body).contains("internal app"), "/proxy/ relayed an app");
}
