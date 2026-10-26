//! Offline job credentials, vault scoping, real CLI execution, and log canaries.
//! refs maxtongwang/ai-config#310. Uses only a temporary home and ephemeral port.
use amux_server::api::{router, AppState};
use axum::{body::Body, extract::ConnectInfo, http::Request};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

const CANARY: &str = "obviously-fake-job-secret-sentinel";
const JOB_TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OTHER_TOKEN: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<u8>>>);
impl Write for Log {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

async fn call(
    app: &axum::Router,
    path: &str,
    body: Value,
    token: &str,
    peer: Option<&str>,
) -> (u16, Value, String) {
    let mut builder = Request::builder().method("POST").uri(path);
    // Job requests carry only their job token, never an owner credential.
    // Empty-token calls also prove that the owner bearer cannot authorize jobs.
    if token.is_empty() {
        builder = builder.header("authorization", "Bearer fake-owner");
    }
    let mut req = builder
        .header("x-amux-worker-token", token)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    if let Some(peer) = peer {
        req.extensions_mut()
            .insert(ConnectInfo(peer.parse::<std::net::SocketAddr>().unwrap()));
    }
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status().as_u16();
    let cache = res
        .headers()
        .get("cache-control")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap(), cache)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registered_jobs_are_scoped_authenticated_and_never_logged_as_values() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    std::env::set_var("AMUX_HOME", home);
    std::env::set_var("AMUX_VAULT_KEYSTORE", "file");
    std::fs::create_dir_all(home.join("sessions")).unwrap();
    std::fs::write(home.join("sessions/com.test.allowed.env"), "CC_TAGS=ops\n").unwrap();
    let logs = Log::default();
    let sink = logs.clone();
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || sink.clone())
        .try_init()
        .unwrap();
    let store = Arc::new(amux_server::db::Store::open(&home.join("test.db")).unwrap());
    let app = router(AppState {
        store,
        started: std::time::Instant::now(),
        build_hash: "test".into(),
        auth_token: Some("fake-owner".into()),
        reconciled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
    });
    let registry = json!({"jobs": [
        {"label": "com.test.allowed", "groups": ["ops"], "allowed_names": ["MAX_TEST_KEY", "MAX_WRONG_GROUP", "MAX_MISSING", "MAX_WORKER_ONLY"], "token_sha256": hex::encode(Sha256::digest(JOB_TOKEN.as_bytes()))},
        {"label": "com.test.other", "groups": ["other"], "allowed_names": ["MAX_TEST_KEY"], "token_sha256": hex::encode(Sha256::digest(OTHER_TOKEN.as_bytes()))}
    ]});
    let registry_path = home.join("secret-jobs.json");
    std::fs::write(&registry_path, registry.to_string()).unwrap();
    std::fs::set_permissions(&registry_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    for (name, scope, target) in [
        ("MAX_TEST_KEY", "global", ""),
        ("MAX_TEST_KEY", "group", "ops"),
        ("MAX_TEST_KEY", "worker", "com.test.allowed"),
        ("MAX_EXTRA", "global", ""),
        ("MAX_WRONG_GROUP", "group", "other"),
        ("MAX_WORKER_ONLY", "worker", "com.test.allowed"),
    ] {
        let (status, response, _) = call(
            &app,
            "/api/vault/secrets",
            json!({"key": name, "scope": scope, "target": target, "value": format!("{CANARY}:{name}:{scope}:{target}")}),
            "",
            None,
        )
        .await;
        assert_eq!(status, 201, "{response}");
        assert!(!response.to_string().contains(CANARY));
    }
    // A job credential does not exempt sibling vault routes from owner attribution.
    let (status, response, _) = call(
        &app,
        "/api/vault/secrets",
        json!({"key": "MAX_JOB_WRITE", "scope": "global", "target": "", "value": CANARY}),
        JOB_TOKEN,
        Some("127.0.0.1:12345"),
    )
    .await;
    assert_eq!(status, 403, "{response}");
    assert_eq!(response["code"], "worker_origin_unattributed");
    assert!(!response.to_string().contains(CANARY));
    let payload = |job: &str, names: &[&str]| json!({"job": job, "names": names});
    let route = "/api/vault/job-secrets";
    let (status, result, cache) = call(
        &app,
        route,
        payload("com.test.allowed", &["MAX_TEST_KEY"]),
        JOB_TOKEN,
        Some("127.0.0.1:12345"),
    )
    .await;
    assert_eq!(status, 200, "{result}");
    assert_eq!(
        result,
        json!({"values": {"MAX_TEST_KEY": format!("{CANARY}:MAX_TEST_KEY:group:ops")}})
    );
    assert_eq!(cache, "no-store");
    for (job, names, token, peer, reason) in [
        (
            "com.test.other",
            vec!["MAX_TEST_KEY"],
            JOB_TOKEN,
            Some("127.0.0.1:1"),
            "job_identity_refused",
        ),
        (
            "com.test.unknown",
            vec!["MAX_TEST_KEY"],
            JOB_TOKEN,
            Some("127.0.0.1:1"),
            "job_identity_refused",
        ),
        (
            "com.test.allowed",
            vec!["MAX_TEST_KEY"],
            "",
            Some("127.0.0.1:1"),
            "job_identity_refused",
        ),
        (
            "com.test.allowed",
            vec!["MAX_TEST_KEY", "MAX_EXTRA"],
            JOB_TOKEN,
            Some("127.0.0.1:1"),
            "name_not_allowed",
        ),
        (
            "com.test.allowed",
            vec!["MAX_WRONG_GROUP"],
            JOB_TOKEN,
            Some("127.0.0.1:1"),
            "missing_or_wrong_group",
        ),
        (
            "com.test.allowed",
            vec!["MAX_TEST_KEY", "MAX_MISSING"],
            JOB_TOKEN,
            Some("127.0.0.1:1"),
            "missing_or_wrong_group",
        ),
        (
            "com.test.allowed",
            vec!["MAX_WORKER_ONLY"],
            JOB_TOKEN,
            Some("127.0.0.1:1"),
            "missing_or_wrong_group",
        ),
        (
            "com.test.allowed",
            vec!["MAX_TEST_KEY"],
            JOB_TOKEN,
            Some("192.0.2.1:1"),
            "loopback_required",
        ),
        (
            "com.test.allowed",
            vec!["MAX_TEST_KEY"],
            JOB_TOKEN,
            None,
            "loopback_required",
        ),
    ] {
        let (status, result, _) = call(&app, route, payload(job, &names), token, peer).await;
        assert_eq!(status, 403, "{reason}: {result}");
        assert_eq!(result["error"], reason);
        assert!(result["names"].as_array().is_some_and(|n| !n.is_empty()));
        assert!(result.get("values").is_none());
        assert!(!result.to_string().contains(CANARY));
    }
    // Exercise endpoint discovery and verified TLS through the production router.
    std::fs::create_dir_all(home.join("job-tokens")).unwrap();
    let credential = home.join("job-tokens/com.test.allowed.token");
    std::fs::write(&credential, JOB_TOKEN).unwrap();
    std::fs::set_permissions(&credential, std::fs::Permissions::from_mode(0o600)).unwrap();
    let material = amux_server::tls::load_or_generate(&home.join("tls")).unwrap();
    let config = axum_server::tls_rustls::RustlsConfig::from_pem(
        material.cert_pem.into_bytes(),
        material.key_pem.into_bytes(),
    )
    .await
    .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let service = app
        .clone()
        .into_make_service_with_connect_info::<std::net::SocketAddr>();
    let server = tokio::spawn(async move {
        axum_server::from_tcp_rustls(listener, config)
            .serve(service)
            .await
            .unwrap();
    });
    std::fs::write(
        home.join("endpoint.json"),
        json!({
            "canonical_url": format!("https://localhost:{}", address.port())
        })
        .to_string(),
    )
    .unwrap();
    let cli = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../amux");
    let output = tokio::process::Command::new("bash").arg(cli)
        .args(["secret-exec", "--job", "com.test.allowed", "--names", "MAX_TEST_KEY", "--", "python3", "-c"])
        .arg(format!("import os,sys; assert os.environ['MAX_TEST_KEY'] == '{CANARY}:MAX_TEST_KEY:group:ops'; assert 'MAX_EXTRA' not in os.environ; assert sys.argv[1:] == ['with spaces', 'a\\\"b']; print('child ok')"))
        .args(["with spaces", "a\"b"])
        .env("HOME", home).env("CC_HOME", home).env("AMUX_HOME", home)
        .env_remove("AMUX_API").env_remove("AMUX_URL").env("AMUX_ISOLATED_RUN", "0")
        .env_remove("TMUX_PANE").env_remove("AMUX_SESSION").output().await.unwrap();
    server.abort();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "child ok\n");
    for bytes in [&output.stdout, &output.stderr] {
        assert!(!String::from_utf8_lossy(bytes).contains(CANARY));
    }
    // Wait for the asynchronous logger by a positive event, never an empty-log pass.
    let mut request_log = String::new();
    for _ in 0..50 {
        let req = Request::builder()
            .uri("/api/logs?limit=1000")
            .header("authorization", "Bearer fake-owner")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap();
        request_log = String::from_utf8(bytes.to_vec()).unwrap();
        if request_log.contains("job_identity_refused") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        request_log.contains("job_identity_refused"),
        "request logger must record the refusal"
    );
    assert!(!request_log.contains(CANARY) && !request_log.contains(JOB_TOKEN));
    let log = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(log.contains("secret_job_refused") && log.contains("secret_job_delivered"));
    assert!(!log.contains(CANARY) && !log.contains(JOB_TOKEN));
    let audit = std::fs::read_to_string(home.join("logs/vault-audit.jsonl")).unwrap();
    assert!(audit.contains("secrets_job_delivered"));
    assert!(audit.contains("secrets_job_refused") && audit.contains("unknown_job"));
    assert!(!audit.contains(CANARY) && !audit.contains(JOB_TOKEN));
    // Unsafe or malformed registry state fails closed without echoing file contents.
    std::fs::set_permissions(&registry_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let (status, response, _) = call(
        &app,
        route,
        payload("com.test.allowed", &["MAX_TEST_KEY"]),
        JOB_TOKEN,
        Some("127.0.0.1:1"),
    )
    .await;
    assert_eq!(status, 403);
    assert_eq!(response["error"], "job_identity_refused");
    std::fs::set_permissions(&registry_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&registry_path, CANARY).unwrap();
    let (status, response, _) = call(
        &app,
        route,
        payload("com.test.allowed", &["MAX_TEST_KEY"]),
        JOB_TOKEN,
        Some("127.0.0.1:1"),
    )
    .await;
    assert_eq!(status, 403);
    assert_eq!(response["error"], "job_identity_refused");
    assert!(!response.to_string().contains(CANARY));
    let (status, response, _) = call(
        &app,
        route,
        payload("com.test.allowed", &["MAX_TEST_KEY"]),
        "",
        Some("127.0.0.1:1"),
    )
    .await;
    assert_eq!(status, 403);
    assert_eq!(response["error"], "job_identity_refused");
    let mut malformed = registry.clone();
    malformed["jobs"][0]["token_sha256"] = json!("A".repeat(64));
    std::fs::write(&registry_path, malformed.to_string()).unwrap();
    let (status, response, _) = call(
        &app,
        route,
        payload("com.test.allowed", &["MAX_TEST_KEY"]),
        JOB_TOKEN,
        Some("127.0.0.1:1"),
    )
    .await;
    assert_eq!(status, 403);
    assert_eq!(response["error"], "job_identity_refused");
    let audit = std::fs::read_to_string(home.join("logs/vault-audit.jsonl")).unwrap();
    assert!(audit.contains("job_registry_token_hash_invalid"));
    assert!(audit.contains("job_registry_invalid") && audit.contains("job_registry_permissions"));
    assert!(!audit.contains(CANARY) && !audit.contains(JOB_TOKEN));
    // Registry revocation is immediate; no cached approval survives a restart.
    std::fs::write(&registry_path, "{\"jobs\":[]}").unwrap();
    let (status, response, _) = call(
        &app,
        route,
        payload("com.test.allowed", &["MAX_TEST_KEY"]),
        JOB_TOKEN,
        Some("127.0.0.1:1"),
    )
    .await;
    assert_eq!(status, 403);
    assert_eq!(response["error"], "job_identity_refused");
}
