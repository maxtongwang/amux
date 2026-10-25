//! Standing vault grants through the shipped router, with private homes and
//! file keystores only. No live vault, Keychain, network delivery or tmux server.
use amux_core::{ids::WorkerId, provider::ProviderId, session::BackendId, worker::WorkerConfig};
use amux_server::{
    api::{router, AppState},
    db::{queries::WorkerRow, Store, WriteOutcome},
};
use axum::{body::Body, http::Request, Router};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
};
use tower::ServiceExt;

const GRANT: &str = "vault-owner-global-write-v1";
const OWNER: &str = "test-owner-credential";
const WORKER_TOKEN: &str = "test-owner-agent-credential";
const OTHER_TOKEN: &str = "test-peer-agent-credential";
const INC: &str = "0123456789abcdef0123456789abcdef";
const OTHER_INC: &str = "fedcba9876543210fedcba9876543210";
const SENTINEL: &str = "unique-vault-write-canary-6ccf1d99";
// Bound a child that never reaches the crash boundary; do not strand the suite.
#[cfg(unix)]
const CRASH_CHILD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);
impl Write for Logs {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn logs() -> Logs {
    static LOGS: OnceLock<Logs> = OnceLock::new();
    LOGS.get_or_init(|| {
        let log = Logs::default();
        let sink = log.clone();
        tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || sink.clone())
            .try_init()
            .expect("test tracing subscriber");
        log
    })
    .clone()
}
struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    state: AppState,
    app: Router,
    worker_id: String,
}
impl Fixture {
    fn new(managed: bool) -> Self {
        Self::from_temp(tempfile::tempdir().unwrap(), managed)
    }
    fn from_temp(temp: tempfile::TempDir, managed: bool) -> Self {
        let home = temp.path().join("amux-home");
        std::fs::create_dir_all(home.join("sessions")).unwrap();
        std::fs::create_dir_all(home.join("worker-tokens")).unwrap();
        std::fs::create_dir_all(temp.path().join("tmux")).unwrap();
        std::env::set_var("AMUX_HOME", &home);
        std::env::set_var("HOME", temp.path());
        std::env::set_var("AMUX_DB", home.join("test.db"));
        std::env::set_var("CC_HOME", &home);
        std::env::set_var("TMUX_TMPDIR", temp.path().join("tmux"));
        std::env::remove_var("TMUX");
        std::env::set_var("AMUX_VAULT_KEYSTORE", "file");
        // The grant must authenticate even with rule-10 enforcement OFF.
        std::env::set_var("AMUX_CONTRACT_DONE", "0");
        for (name, inc, token) in [
            ("mrpopo", INC, WORKER_TOKEN),
            ("peer", OTHER_INC, OTHER_TOKEN),
        ] {
            std::fs::write(
                home.join(format!("sessions/{name}.env")),
                format!("CC_INCARNATION={inc}\nCC_TAGS=leads\n"),
            )
            .unwrap();
            std::fs::write(
                home.join(format!("worker-tokens/{inc}.sha256")),
                hex::encode(Sha256::digest(token.as_bytes())),
            )
            .unwrap();
        }
        let store = Arc::new(Store::open(&home.join("test.db")).unwrap());
        let id = WorkerId::from_ulid(ulid::Ulid::new());
        if managed {
            let config = WorkerConfig {
                display_name: "mrpopo".into(),
                name_aliases: vec![],
                cwd: home.to_string_lossy().into(),
                provider: ProviderId::new("codex"),
                model: None,
                backend: BackendId::from("tmux"),
                environment: Default::default(),
                permissions: vec![],
                group: None,
                worker_type: Default::default(),
            };
            let row = WorkerRow::new(&id, &config, &chrono::Utc::now().to_rfc3339());
            store
                .write(move |c| {
                    amux_server::db::queries::insert_worker(c, &row)?;
                    Ok(WriteOutcome {
                        applied: true,
                        events: vec![],
                    })
                })
                .unwrap();
        }
        let state = AppState {
            store,
            started: std::time::Instant::now(),
            build_hash: "test".into(),
            auth_token: Some(OWNER.into()),
            reconciled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        };
        let app = router(state.clone());
        Self {
            _temp: temp,
            home,
            state,
            app,
            worker_id: id.to_string(),
        }
    }
    async fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        worker: Option<(&str, Option<&str>)>,
    ) -> (u16, Value) {
        let mut req = Request::builder()
            .method(method)
            .uri(path)
            .header("authorization", format!("Bearer {OWNER}"))
            .header("content-type", "application/json");
        if let Some((name, token)) = worker {
            req = req.header("x-amux-session", name);
            if let Some(t) = token {
                req = req.header("x-amux-worker-token", t);
            }
        }
        let response = self
            .app
            .clone()
            .oneshot(
                req.body(
                    body.map(|b| Body::from(b.to_string()))
                        .unwrap_or_else(Body::empty),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status().as_u16();
        let raw = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert!(
            !String::from_utf8_lossy(&raw).contains(SENTINEL),
            "response leaked the canary"
        );
        (status, serde_json::from_slice(&raw).unwrap_or(Value::Null))
    }
    async fn grantee(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        self.call(method, path, body, Some(("mrpopo", Some(WORKER_TOKEN))))
            .await
    }
    async fn create(&self) -> String {
        let (status, body) = self
            .grantee("POST", "/api/vault/secrets", Some(secret("global", "")))
            .await;
        assert_eq!(status, 201, "grantee global create");
        assert_eq!(body["item"]["by"], "mrpopo");
        body["item"]["id"].as_str().expect("created item id").into()
    }
    fn replay_seed(&mut self) {
        self.state
            .store
            .write(|c| {
                c.execute_batch(include_str!("../migrations/0126_vault_write_grant.sql"))?;
                Ok(WriteOutcome {
                    applied: true,
                    events: vec![],
                })
            })
            .unwrap();
        self.app = router(self.state.clone());
    }
}
fn secret(scope: &str, target: &str) -> Value {
    json!({"key": "STRIPE_API_KEY", "scope": scope, "target": target, "value": SENTINEL})
}

#[tokio::test]
async fn vault_grantee_global_create_and_update_allowed() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new(true);
    let id = f.create().await;
    for (method, version) in [("PATCH", 2), ("PUT", 3)] {
        let (status, body) = f
            .grantee(
                method,
                &format!("/api/vault/secrets/{id}"),
                Some(json!({"value": SENTINEL})),
            )
            .await;
        assert_eq!(status, 200, "grantee global update");
        assert_eq!(body["item"]["version"], version);
    }
}

#[tokio::test]
async fn vault_grantee_key_policy_all_write_paths() {
    let _serial = SERIAL.lock().await;
    let log = logs();
    let f = Fixture::new(true);
    let safe_id = f.create().await;
    for (key, rule) in [
        ("NODE_OPTIONS", "injection_sensitive"),
        ("BASH_ENV", "injection_sensitive"),
        ("DYLD_INSERT_LIBRARIES", "injection_sensitive"),
        ("ANTHROPIC_BASE_URL", "client_auth_or_endpoint"),
        ("ANTHROPIC_API_KEY", "client_auth_or_endpoint"),
        ("ANTHROPIC_AUTH_TOKEN", "client_auth_or_endpoint"),
        ("OPENAI_API_KEY", "client_auth_or_endpoint"),
    ] {
        let payload = json!({"key": key, "scope": "global", "value": SENTINEL});
        let (status, body) = f
            .grantee("POST", "/api/vault/secrets", Some(payload.clone()))
            .await;
        assert_key_refusal(status, &body, rule);
        // Owner-created process-control keys cannot be rotated or edited by
        // the grantee. This also proves the owner path remains unchanged.
        let (status, body) = f
            .call("POST", "/api/vault/secrets", Some(payload), None)
            .await;
        assert_eq!(status, 201, "owner can create {key}");
        let unsafe_id = body["item"]["id"].as_str().unwrap();
        for method in ["PUT", "PATCH"] {
            let (status, body) = f
                .grantee(
                    method,
                    &format!("/api/vault/secrets/{unsafe_id}"),
                    Some(json!({"value": SENTINEL})),
                )
                .await;
            assert_key_refusal(status, &body, rule);
        }
        let (status, body) = f
            .grantee(
                "PATCH",
                &format!("/api/vault/secrets/{safe_id}"),
                Some(json!({"key": key, "value": SENTINEL})),
            )
            .await;
        assert_key_refusal(status, &body, rule);
    }
    let tracing = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
    for rule in ["injection_sensitive", "client_auth_or_endpoint"] {
        assert!(tracing
            .lines()
            .any(|l| l.contains("vault_grant_key_not_permitted") && l.contains(rule)));
    }
    assert!(!tracing.contains(SENTINEL));
}

fn assert_key_refusal(status: u16, body: &Value, rule: &str) {
    assert_eq!(status, 403);
    assert_eq!(body["code"], "vault_grant_key_not_permitted");
    assert_eq!(body["rule"], rule);
}

#[tokio::test]
async fn vault_grantee_key_shape_and_reserved_patterns() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new(true);
    let id = f.create().await;
    // Credential-shaped reserved names prove the explicit deny rules, rather
    // than relying on the suffix filter to exclude runtime controls.
    for (rule, keys) in [
        (
            "not_credential_shaped",
            vec![
                "_STRIPE_TOKEN",
                "stripe_TOKEN",
                "9STRIPE_TOKEN",
                "STRIPE-TOKEN",
                "STRIPE_Å_TOKEN",
                "STRIPE_KEY",
                "STRIPE_PASSWORD",
                "STRIPE_OPTIONS",
                "",
            ],
        ),
        (
            "client_auth_or_endpoint",
            vec![
                "CLAUDE_API_KEY",
                "CODEX_TOKEN",
                "STRIPE_BASE_URL",
                "STRIPE_API_BASE",
            ],
        ),
        (
            "injection_sensitive",
            vec![
                "PATH",
                "HOME",
                "SHELL",
                "USER",
                "TMPDIR",
                "ENV",
                "ZDOTDIR",
                "LD_API_KEY",
                "DYLD_TOKEN",
                "NODE_SECRET",
                "PYTHONPATH_TOKEN",
                "BASH_TOKEN",
                "PROMPT_COMMAND",
                "GIT_CLIENT_ID",
                "SSH_CLIENT_SECRET",
                "PERL5LIB_TOKEN",
                "RUBYOPT_TOKEN",
                "CC_TOKEN",
                "AMUX_TOKEN",
            ],
        ),
    ] {
        for key in keys {
            for (method, path) in [
                ("POST", "/api/vault/secrets".to_string()),
                ("PATCH", format!("/api/vault/secrets/{id}")),
            ] {
                let (status, body) = f
                    .grantee(method, &path, Some(json!({"key": key, "value": SENTINEL})))
                    .await;
                assert_key_refusal(status, &body, rule);
            }
        }
    }
    for key in [
        "PAYMENTS_API_KEY",
        "PAYMENTS_TOKEN",
        "PAYMENTS_SECRET",
        "PAYMENTS_CLIENT_ID",
        "PAYMENTS_CLIENT_SECRET",
    ] {
        let (status, body) = f
            .grantee(
                "POST",
                "/api/vault/secrets",
                Some(json!({"key": key, "value": SENTINEL})),
            )
            .await;
        assert_eq!(status, 201, "permitted suffix: {key}");
        let created = body["item"]["id"].as_str().unwrap();
        assert_eq!(
            f.grantee(
                "PATCH",
                &format!("/api/vault/secrets/{created}"),
                Some(json!({"key": format!("RENAMED_{key}"), "value": SENTINEL}))
            )
            .await
            .0,
            200
        );
    }
}

#[tokio::test]
async fn vault_grant_bad_receipts_quarantined_without_blocking_writes() {
    let _serial = SERIAL.lock().await;
    let log = logs();
    let mut f = Fixture::new(true);
    let id = f.create().await;
    let path = f.home.join("vault/secrets.json");
    let invalid = vec![
        json!({"receipt_id": "bad-id"}),
        json!({"receipt_id": "vault-grant:bad-fields", "event": "wrong-event"}),
        json!({"receipt_id": "vault-grant:bad-time", "event": "secret_granted_write", "key": "STRIPE_API_KEY",
            "item": id, "grantee": "mrpopo", "grantee_id": f.worker_id, "ts": "invalid"}),
        json!({"receipt_id": "vault-grant:bad-scope", "event": "secret_granted_write", "key": "STRIPE_API_KEY",
            "item": id, "grantee": "mrpopo", "grantee_id": f.worker_id, "ts": 1.0, "scope": "worker",
            "grant_id": GRANT, "version": 1}),
        Value::Null,
    ];
    let mut doc: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    doc["grant_receipts"] = json!(invalid);
    std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
    assert_eq!(
        f.grantee(
            "PUT",
            &format!("/api/vault/secrets/{id}"),
            Some(json!({"value": SENTINEL}))
        )
        .await
        .0,
        200
    );
    let doc: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(doc["grant_receipts_quarantine"], json!(invalid));
    assert_eq!(doc["grant_receipts"], json!([]));
    // A malformed queue container is quarantined on startup too. It cannot
    // poison the next save, and restarts never duplicate the quarantine.
    let malformed = json!({"not": "a queue"});
    let mut doc = doc;
    doc["grant_receipts"] = malformed.clone();
    std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
    f.app = router(f.state.clone());
    f.app = router(f.state.clone());
    assert_eq!(
        f.grantee(
            "POST",
            "/api/vault/secrets",
            Some(json!({"key": "OTHER_API_KEY", "value": SENTINEL}))
        )
        .await
        .0,
        201
    );
    let doc: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mut expected = invalid;
    expected.push(malformed);
    assert_eq!(doc["grant_receipts_quarantine"], json!(expected));
    assert_eq!(doc["grant_receipts"], json!([]));
    let conn = f.state.store.read().unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM session_events WHERE type='vault.granted_write'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        3
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM conversation_posts WHERE author_key='system:vault'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        3
    );
    let tracing = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
    assert!(tracing.contains("vault_grant_receipt_quarantined"));
    assert!(!tracing.contains(SENTINEL));
}

#[tokio::test]
async fn vault_regrant_missing_owner_is_actionable_conflict() {
    let _serial = SERIAL.lock().await;
    let log = logs();
    let f = Fixture::new(true);
    assert_eq!(
        f.call(
            "POST",
            &format!("/api/vault/grants/{GRANT}/revoke"),
            None,
            None
        )
        .await
        .0,
        200
    );
    std::fs::remove_file(f.home.join("sessions/mrpopo.env")).unwrap();
    let (status, body) = f
        .call(
            "POST",
            &format!("/api/vault/grants/{GRANT}/regrant"),
            None,
            None,
        )
        .await;
    assert_eq!(status, 409);
    assert_eq!(body["code"], "vault_grant_owner_unregistered");
    assert!(body["error"].as_str().unwrap().contains("register"));
    let (_, listing) = f.call("GET", "/api/vault/grants", None, None).await;
    assert!(listing["grants"][0]["revoked_at"].as_f64().is_some());
    assert!(String::from_utf8(log.0.lock().unwrap().clone())
        .unwrap()
        .contains("vault_grant_owner_unregistered"));
}

#[tokio::test]
async fn vault_grant_listing_requires_owner_auth() {
    let _serial = SERIAL.lock().await;
    let mut f = Fixture::new(true);
    std::env::set_var("AMUX_RS_NO_LOOPBACK_BYPASS", "0");
    assert_eq!(f.call("GET", "/api/vault/grants", None, None).await.0, 200);
    assert_eq!(f.grantee("GET", "/api/vault/grants", None).await.0, 403);
    for tokenless_host in [false, true] {
        if tokenless_host {
            f.state.auth_token = None;
            f.app = router(f.state.clone());
        }
        for auth in [None, Some("Bearer invalid")] {
            // Localhost can bypass the outer middleware; the route must still
            // protect pinned identities independently of transport locality.
            let mut req =
                Request::builder()
                    .uri("/api/vault/grants")
                    .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                        [127, 0, 0, 1],
                        12345,
                    ))));
            if let Some(auth) = auth {
                req = req.header("authorization", auth);
            }
            let response = f
                .app
                .clone()
                .oneshot(req.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), 403);
            let raw = axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap();
            let body: Value = serde_json::from_slice(&raw).unwrap();
            assert_eq!(body["code"], "vault_grants_auth_required");
            assert!(body.get("grants").is_none());
            for identity in [INC, f.worker_id.as_str(), "mrpopo"] {
                assert!(!String::from_utf8_lossy(&raw).contains(identity));
            }
        }
    }
}

#[tokio::test]
async fn vault_other_worker_and_unverified_claim_refused() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new(true);
    for (name, token) in [
        ("peer", Some(OTHER_TOKEN)),
        ("mrpopo", Some(OTHER_TOKEN)),
        ("mrpopo", None),
    ] {
        let (status, body) = f
            .call(
                "POST",
                "/api/vault/secrets",
                Some(secret("global", "")),
                Some((name, token)),
            )
            .await;
        assert_eq!(status, 403);
        assert_eq!(body["code"], "vault_grant_required");
    }
}

#[tokio::test]
async fn vault_grantee_non_global_sources_and_destinations_refused() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new(true);
    let id = f.create().await;
    for (scope, target) in [("group", "leads"), ("worker", "peer")] {
        let (status, body) = f
            .grantee("POST", "/api/vault/secrets", Some(secret(scope, target)))
            .await;
        assert_eq!(status, 403);
        assert_eq!(body["code"], "vault_scope_not_granted");
        for method in ["PATCH", "PUT"] {
            let (status, body) = f
                .grantee(
                    method,
                    &format!("/api/vault/secrets/{id}"),
                    Some(secret(scope, target)),
                )
                .await;
            assert_eq!(status, 403);
            assert_eq!(body["code"], "vault_scope_not_granted");
        }
        let (status, body) = f
            .call(
                "POST",
                "/api/vault/secrets",
                Some(secret(scope, target)),
                None,
            )
            .await;
        assert_eq!(status, 201);
        let narrow = body["item"]["id"].as_str().unwrap();
        for method in ["PATCH", "PUT"] {
            let (status, _) = f
                .grantee(
                    method,
                    &format!("/api/vault/secrets/{narrow}"),
                    Some(secret("global", "")),
                )
                .await;
            assert_eq!(status, 403, "cannot promote or rotate a narrower secret");
        }
    }
}

#[tokio::test]
async fn vault_grantee_reveal_delete_import_and_audit_refused() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new(true);
    let id = f.create().await;
    for (method, path, body) in [
        ("POST", format!("/api/vault/secrets/{id}/reveal"), None),
        ("DELETE", format!("/api/vault/secrets/{id}"), None),
        (
            "POST",
            "/api/vault/secrets/import".into(),
            Some(json!({"key":"IMPORTED_KEY"})),
        ),
        ("GET", "/api/vault/secrets/audit".into(), None),
        ("GET", format!("/api/vault/secrets/{id}/access"), None),
    ] {
        assert_eq!(
            f.grantee(method, &path, body).await.0,
            403,
            "owner-only route {path}"
        );
    }
}

#[tokio::test]
async fn vault_revoked_seed_stays_revoked_and_worker_cannot_regrant() {
    let _serial = SERIAL.lock().await;
    let mut f = Fixture::new(true);
    let id = f.create().await;
    assert_eq!(
        f.call(
            "POST",
            &format!("/api/vault/grants/{GRANT}/revoke"),
            None,
            None
        )
        .await
        .0,
        200
    );
    for path in [
        "/api/vault/grants".to_string(),
        format!("/api/vault/grants/{GRANT}/regrant"),
        format!("/api/vault/grants/{GRANT}/revoke"),
    ] {
        assert_eq!(
            f.grantee("POST", &path, Some(json!({"revoked_at": null})))
                .await
                .0,
            403
        );
    }
    f.replay_seed();
    f.replay_seed();
    let (status, body) = f
        .grantee(
            "PUT",
            &format!("/api/vault/secrets/{id}"),
            Some(json!({"value": SENTINEL})),
        )
        .await;
    assert_eq!(status, 403);
    assert_eq!(body["code"], "vault_grant_required");
    assert_eq!(
        f.grantee(
            "POST",
            "/api/vault/secrets",
            Some(json!({"key":"NEW_KEY", "value": SENTINEL}))
        )
        .await
        .0,
        403
    );
    let (_, listing) = f.call("GET", "/api/vault/grants", None, None).await;
    assert_eq!(listing["grants"].as_array().unwrap().len(), 1);
    assert!(listing["grants"][0]["revoked_at"].as_f64().is_some());
    assert_eq!(
        f.call(
            "POST",
            &format!("/api/vault/grants/{GRANT}/regrant"),
            None,
            None
        )
        .await
        .0,
        200
    );
    assert_eq!(
        f.grantee(
            "PUT",
            &format!("/api/vault/secrets/{id}"),
            Some(json!({"value": SENTINEL}))
        )
        .await
        .0,
        200
    );
}

#[tokio::test]
async fn vault_replacement_registry_id_or_incarnation_cannot_inherit() {
    let _serial = SERIAL.lock().await;
    let mut f = Fixture::new(true);
    f.create().await;
    // Same owner name and valid token, DIFFERENT stable registry ID.
    let new_id = WorkerId::from_ulid(ulid::Ulid::new()).to_string();
    f.state
        .store
        .write(move |c| {
            c.execute(
                "UPDATE _amux_workers SET id=?1 WHERE display_name='mrpopo'",
                [new_id],
            )?;
            Ok(WriteOutcome {
                applied: true,
                events: vec![],
            })
        })
        .unwrap();
    f.replay_seed(); // startup must not silently bind the replacement
    assert_eq!(
        f.grantee(
            "POST",
            "/api/vault/secrets",
            Some(json!({"key":"REPLACEMENT_KEY", "value": SENTINEL}))
        )
        .await
        .0,
        403
    );
    // Put the original ID back; a renamed peer now occupies the owner's env name.
    let original = f.worker_id.clone();
    f.state
        .store
        .write(move |c| {
            c.execute(
                "UPDATE _amux_workers SET id=?1 WHERE display_name='mrpopo'",
                [original],
            )?;
            Ok(WriteOutcome {
                applied: true,
                events: vec![],
            })
        })
        .unwrap();
    std::fs::copy(
        f.home.join("sessions/peer.env"),
        f.home.join("sessions/mrpopo.env"),
    )
    .unwrap();
    assert_eq!(
        f.call(
            "POST",
            "/api/vault/secrets",
            Some(json!({"key":"FORGED_KEY", "value": SENTINEL})),
            Some(("mrpopo", Some(OTHER_TOKEN)))
        )
        .await
        .0,
        403
    );
}

#[tokio::test]
async fn vault_legacy_owner_uses_pinned_token_bound_incarnation() {
    let _serial = SERIAL.lock().await;
    let mut f = Fixture::new(false);
    f.create().await;
    let (_, listing) = f.call("GET", "/api/vault/grants", None, None).await;
    assert_eq!(listing["grants"][0]["grantee_id"], INC);
    std::fs::copy(
        f.home.join("sessions/peer.env"),
        f.home.join("sessions/mrpopo.env"),
    )
    .unwrap();
    f.replay_seed();
    assert_eq!(
        f.call(
            "POST",
            "/api/vault/secrets",
            Some(json!({"key":"NEW_KEY", "value": SENTINEL})),
            Some(("mrpopo", Some(OTHER_TOKEN)))
        )
        .await
        .0,
        403
    );
}

#[tokio::test]
async fn vault_granted_writes_audit_feed_events_and_listing_never_leak() {
    let _serial = SERIAL.lock().await;
    let log = logs();
    let f = Fixture::new(true);
    let id = f.create().await;
    f.grantee(
        "PUT",
        &format!("/api/vault/secrets/{id}"),
        Some(json!({"value": SENTINEL})),
    )
    .await;
    let (status, listing) = f.call("GET", "/api/vault/grants", None, None).await;
    assert_eq!(status, 200);
    let row = &listing["grants"][0];
    assert_eq!(row["grantee_id"], f.worker_id);
    assert_eq!(row["scope"], "global");
    assert_eq!(row.as_object().unwrap().len(), 6, "metadata allowlist");
    assert!(row["created_at"].as_f64().unwrap() > 1_700_000_000.0);
    for path in [
        "/api/vault/secrets",
        "/api/vault/secrets/audit",
        "/api/conversations/g:leads/posts",
    ] {
        assert_eq!(f.call("GET", path, None, None).await.0, 200);
    }
    let conn = f.state.store.read().unwrap();
    let mut stmt = conn
        .prepare("SELECT data FROM session_events WHERE type='vault.granted_write' ORDER BY id")
        .unwrap();
    let events: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(events.len(), 2, "every write has an owner-visible event");
    for (i, raw) in events.iter().enumerate() {
        assert!(!raw.contains(SENTINEL), "owner event leaked the canary");
        let event: Value = serde_json::from_str(raw).unwrap();
        assert_eq!(event["grant_id"], GRANT);
        assert_eq!(event["grantee_id"], f.worker_id);
        assert_eq!(event["version"], i + 1);
        assert_eq!(event["key"], "STRIPE_API_KEY");
        assert_eq!(event["scope"], "global");
        assert!(event["ts"].as_f64().is_some());
    }
    let mut stmt = conn.prepare("SELECT body FROM conversation_posts WHERE conv='g:leads' AND author_key='system:vault'").unwrap();
    let feed: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(feed.len(), 2, "every write posts to leads");
    assert!(feed
        .iter()
        .all(|s| !s.contains(SENTINEL) && s.contains(GRANT) && s.contains("STRIPE_API_KEY")));
    let audit = std::fs::read_to_string(f.home.join("logs/vault-audit.jsonl")).unwrap();
    assert!(!audit.contains(SENTINEL), "audit leaked the canary");
    assert_eq!(
        audit
            .lines()
            .filter(|s| s.contains("secret_granted_write"))
            .count(),
        2
    );
    let tracing = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
    assert!(!tracing.contains(SENTINEL), "tracing leaked the canary");
    assert!(
        tracing.contains("vault_granted_write"),
        "positive trace observation"
    );
    assert!(!std::fs::read_to_string(f.home.join("vault/secrets.json"))
        .unwrap()
        .contains(SENTINEL));
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "subprocess-only fixture; invoked by vault_grant_sigkill_replays_receipt_once"]
async fn vault_grant_crash_child() {
    let root =
        PathBuf::from(std::env::var_os("VAULT_CRASH_ROOT").expect("parent supplies private root"));
    let f = Fixture::from_temp(tempfile::tempdir_in(&root).unwrap(), true);
    std::fs::create_dir_all(f.home.join("vault")).unwrap();
    std::fs::write(
        f.home.join("vault/secrets.json"),
        serde_json::to_vec(&json!({
            "items": [], "grant_receipts": [{"receipt_id": "bad-before-crash"}]
        }))
        .unwrap(),
    )
    .unwrap();
    // The DB event + feed commit, but the file audit cannot. This is a real
    // failure boundary after the encrypted save, not a test-only source hook.
    std::fs::create_dir_all(f.home.join("logs/vault-audit.jsonl")).unwrap();
    let (status, body) = f
        .grantee("POST", "/api/vault/secrets", Some(secret("global", "")))
        .await;
    assert_eq!(status, 500);
    assert_eq!(body["write_committed"], true);
    assert_eq!(body["code"], "vault_grant_audit_failed");
    let store = std::fs::read_to_string(f.home.join("vault/secrets.json")).unwrap();
    assert!(!store.contains(SENTINEL));
    assert_eq!(
        serde_json::from_str::<Value>(&store).unwrap()["grant_receipts_quarantine"],
        json!([{"receipt_id": "bad-before-crash"}])
    );
    assert_eq!(
        serde_json::from_str::<Value>(&store).unwrap()["grant_receipts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    std::fs::write(root.join("home-path"), f.home.to_string_lossy().as_bytes()).unwrap();
    // No Drop, graceful shutdown or in-memory replay: the parent must recover
    // solely from the actual files/WAL after this process is killed.
    unsafe {
        libc::kill(libc::getpid(), libc::SIGKILL);
    }
    panic!("SIGKILL unexpectedly returned");
}

#[cfg(unix)]
#[tokio::test]
async fn vault_grant_sigkill_replays_receipt_once() {
    use std::os::unix::process::ExitStatusExt;
    let _serial = SERIAL.lock().await;
    let root = tempfile::tempdir().unwrap();
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--ignored",
            "--exact",
            "vault_grant_crash_child",
            "--nocapture",
        ])
        .env("VAULT_CRASH_ROOT", root.path())
        .kill_on_drop(true);
    let child = tokio::time::timeout(CRASH_CHILD_TIMEOUT, command.output())
        .await
        .expect("bounded crash probe timed out")
        .unwrap();
    assert_eq!(
        child.status.signal(),
        Some(libc::SIGKILL),
        "child must reach the crash boundary"
    );
    let home = PathBuf::from(std::fs::read_to_string(root.path().join("home-path")).unwrap());
    // Repair the injected private-home disk obstruction; retain a partial audit
    // line as failed evidence and prove replay starts a distinct valid line.
    std::fs::remove_dir(home.join("logs/vault-audit.jsonl")).unwrap();
    std::fs::write(home.join("logs/vault-audit.jsonl"), "{\"interrupted\":").unwrap();
    std::env::set_var("AMUX_HOME", &home);
    std::env::set_var("HOME", root.path());
    std::env::set_var("AMUX_DB", home.join("test.db"));
    std::env::set_var("CC_HOME", &home);
    std::env::set_var("AMUX_VAULT_KEYSTORE", "file");
    std::env::set_var("TMUX_TMPDIR", root.path().join("no-tmux-server"));
    std::env::remove_var("TMUX");
    let state = AppState {
        store: Arc::new(Store::open(&home.join("test.db")).unwrap()),
        started: std::time::Instant::now(),
        build_hash: "restarted".into(),
        auth_token: Some(OWNER.into()),
        reconciled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
    };
    let _first_restart = router(state.clone());
    let _second_restart = router(state.clone());
    let store: Value =
        serde_json::from_str(&std::fs::read_to_string(home.join("vault/secrets.json")).unwrap())
            .unwrap();
    assert!(store["grant_receipts"].as_array().unwrap().is_empty());
    assert_eq!(
        store["grant_receipts_quarantine"],
        json!([{"receipt_id": "bad-before-crash"}])
    );
    assert_eq!(store["items"].as_array().unwrap().len(), 1);
    assert_eq!(store["items"][0]["version"], 1);
    let conn = state.store.read().unwrap();
    let events: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM session_events WHERE type='vault.granted_write'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let notices: i64 = conn.query_row("SELECT COUNT(*) FROM conversation_posts WHERE conv='g:leads' AND author_key='system:vault'", [], |r| r.get(0)).unwrap();
    assert_eq!(events, 1);
    assert_eq!(notices, 1);
    let audit = std::fs::read_to_string(home.join("logs/vault-audit.jsonl")).unwrap();
    assert!(
        audit.starts_with("{\"interrupted\":\n"),
        "failed evidence retained"
    );
    let entries: Vec<Value> = audit
        .lines()
        .filter_map(|s| serde_json::from_str::<Value>(s).ok())
        .collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["event"], "secret_granted_write");
    assert_eq!(entries[0]["version"], 1);
    assert!(!audit.contains(SENTINEL));
}
