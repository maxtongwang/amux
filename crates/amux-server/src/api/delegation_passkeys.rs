// Owner passkeys — UV assertions bind single-use server challenges to exact delegated grants (refs maxtongwang/ai-config#160).
use super::AppState;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use webauthn_rs::prelude::*;

const TTL: f64 = 120.0;
const DEFAULT_ORIGIN: &str = "https://amux.maxwangestates.com";
type Result<T> = std::result::Result<T, String>;

pub(crate) fn passkey_mode() -> bool {
    // Unknown values fail closed, rather than silently downgrading to cite.
    std::env::var("AMUX_DELEGATION_APPROVAL").is_ok_and(|v| v != "cite")
}

fn now() -> f64 {
    crate::config::now_f64()
}
fn id(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

// Store::write serializes challenge consumption, revocation and counter updates.
// Commit refused attempts too: even an invalid finish burns its challenge.
fn write<T: Send + 'static>(
    state: &AppState,
    f: impl FnOnce(&Connection) -> Result<T> + Send + 'static,
) -> Result<T> {
    let out = Arc::new(Mutex::new(None));
    let dest = out.clone();
    state
        .store
        .write(move |c| {
            *dest.lock().unwrap() = Some(f(c));
            Ok(crate::db::WriteOutcome {
                applied: true,
                events: vec![],
            })
        })
        .map_err(|e| e.to_string())?;
    let mut output = out.lock().unwrap();
    output.take().ok_or("passkey write returned no result")?
}
fn sql<T>(v: rusqlite::Result<T>) -> Result<T> {
    v.map_err(|e| e.to_string())
}
fn json_value<T: Serialize>(v: T) -> Result<Value> {
    serde_json::to_value(v).map_err(|e| e.to_string())
}
fn parse<T: serde::de::DeserializeOwned>(v: Value) -> Result<T> {
    serde_json::from_value(v).map_err(|e| e.to_string())
}

fn owner(state: &AppState, h: &HeaderMap, uri: &Uri) -> Result<()> {
    if state.auth_token.is_none()
        || !super::auth::has_owner_token(state, h, uri)
        || !super::session_verbs::worker_origin(h).is_empty()
        || super::org::is_verified_local_member(h)
    {
        return Err("only the owner bearer may manage or use delegation passkeys".into());
    }
    Ok(())
}
fn remote(h: &HeaderMap) -> bool {
    h.get(super::process_identity::PROCESS_ORIGIN_HEADER)
        .is_some_and(|v| v == "remote")
}

// Each HTTPS hostname is its own RP: unrelated tailnet and public DNS names
// cannot share an rpId in browsers. Register once at each hostname you use.
fn webauthn(origin: &str) -> Result<Webauthn> {
    let allowed =
        std::env::var("AMUX_DELEGATION_PASSKEY_ORIGINS").unwrap_or_else(|_| DEFAULT_ORIGIN.into());
    if !allowed.split(',').map(str::trim).any(|s| s == origin) {
        return Err("passkey origin is not in AMUX_DELEGATION_PASSKEY_ORIGINS".into());
    }
    let url = Url::parse(origin).map_err(|e| e.to_string())?;
    if url.scheme() != "https"
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("passkeys require an exact HTTPS origin".into());
    }
    let rp = url.host_str().ok_or("origin has no hostname")?;
    WebauthnBuilder::new(rp, &url)
        .map_err(|e| e.to_string())?
        .rp_name("amux delegated changes")
        .timeout(std::time::Duration::from_secs(TTL as u64))
        .build()
        .map_err(|e| e.to_string())
}
fn rp(wa: &Webauthn) -> &str {
    wa.get_allowed_origins()[0]
        .host_str()
        .expect("validated HTTPS hostname")
}
fn origin(h: &HeaderMap) -> Result<String> {
    h.get("origin")
        .and_then(|s| s.to_str().ok())
        .map(str::to_string)
        .ok_or("an Origin header is required".into())
}
fn credentials(c: &Connection, rp: &str) -> Result<Vec<Passkey>> {
    let mut q = sql(c.prepare(
        "SELECT credential FROM _amux_delegation_passkeys WHERE rp_id=?1 AND revoked_at IS NULL",
    ))?;
    let rows = sql(q.query_map([rp], |r| r.get::<_, String>(0)))?;
    rows.map(|r| serde_json::from_str(&sql(r)?).map_err(|e| e.to_string()))
        .collect()
}
fn total(c: &Connection) -> Result<i64> {
    sql(
        c.query_row("SELECT count(*) FROM _amux_delegation_passkeys", [], |r| {
            r.get(0)
        }),
    )
}
fn store_ceremony(c: &Connection, purpose: &str, origin: &str, state: Value) -> Result<String> {
    let token = Uuid::new_v4().to_string();
    sql(c.execute(
        "DELETE FROM _amux_delegation_ceremonies WHERE expires_at<?1",
        [now()],
    ))?;
    // A bounded store under repeated begin requests. Unexpired ceremonies are never evicted.
    let count: i64 = sql(c.query_row(
        "SELECT count(*) FROM _amux_delegation_ceremonies",
        [],
        |r| r.get(0),
    ))?;
    if count >= 64 {
        return Err("too many passkey ceremonies; wait two minutes".into());
    }
    sql(c.execute(
        "INSERT INTO _amux_delegation_ceremonies VALUES (?1,?2,?3,?4,?5)",
        params![token, purpose, origin, state.to_string(), now() + TTL],
    ))?;
    Ok(token)
}
fn take(c: &Connection, token: &str, purpose: &str, origin: &str) -> Result<Value> {
    let row: Option<(String,String,String,f64)> = sql(c.query_row(
        "DELETE FROM _amux_delegation_ceremonies WHERE id=?1 RETURNING purpose,origin,state,expires_at", [token],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional())?;
    let (p, o, s, expiry) = row.ok_or("passkey challenge was used or does not exist")?;
    if p != purpose || o != origin || expiry < now() {
        return Err("passkey challenge expired or belongs to a different operation/origin".into());
    }
    serde_json::from_str(&s).map_err(|e| e.to_string())
}

#[derive(Deserialize)]
pub(crate) struct Proof {
    pub ceremony: String,
    pub credential: PublicKeyCredential,
}
fn verify(
    c: &Connection,
    wa: &Webauthn,
    assertion: &PublicKeyCredential,
    auth: PasskeyAuthentication,
) -> Result<()> {
    let result = wa
        .finish_passkey_authentication(assertion, &auth)
        .map_err(|e| format!("passkey assertion refused: {e}"))?;
    if !result.user_verified() {
        return Err("passkey user verification is required".into());
    }
    let key_id = id(result.cred_id());
    let row: Option<(String,u32)> = sql(c.query_row("SELECT credential,counter FROM _amux_delegation_passkeys WHERE id=?1 AND revoked_at IS NULL", [&key_id], |r| Ok((r.get(0)?,r.get(1)?))).optional())?;
    let (data, counter) = row.ok_or("passkey is revoked or unknown")?;
    // Compare with the CURRENT counter, not just the begin-time snapshot.
    // Synced authenticators legitimately report zero until they support counters.
    if (counter > 0 || result.counter() > 0) && result.counter() <= counter {
        return Err("passkey counter did not advance".into());
    }
    let mut pk: Passkey = serde_json::from_str(&data).map_err(|e| e.to_string())?;
    pk.update_credential(&result)
        .ok_or("passkey identity mismatch")?;
    sql(c.execute("UPDATE _amux_delegation_passkeys SET credential=?1,counter=?2,modified_at=?3 WHERE id=?4 AND revoked_at IS NULL", params![json_value(pk)?.to_string(), result.counter(), now(), key_id]))?;
    Ok(())
}

pub(crate) fn approval_proof_ok(
    state: &AppState,
    code: &str,
    h: &HeaderMap,
    proof: Proof,
) -> Result<()> {
    let o = origin(h)?;
    let wa = webauthn(&o)?;
    let purpose = format!("grant:{}", code.to_ascii_uppercase());
    write(state, move |c| {
        let auth = parse(take(c, &proof.ceremony, &purpose, &o)?)?;
        verify(c, &wa, &proof.credential, auth)
    })
}

#[derive(Serialize, Deserialize)]
struct Registration {
    registration: PasskeyRegistration,
    authorization: Option<PasskeyAuthentication>,
    bootstrap: bool,
}
fn begin_registration(state: &AppState, o: String, is_remote: bool) -> Result<Value> {
    let wa = webauthn(&o)?;
    write(state, move |c| {
        let bootstrap = total(c)? == 0;
        let creds = credentials(c, rp(&wa))?;
        let (challenge, registration) = wa
            .start_passkey_registration(
                Uuid::from_u128(1),
                "owner",
                "amux owner",
                Some(creds.iter().map(|p| p.cred_id().clone()).collect()),
            )
            .map_err(|e| e.to_string())?;
        let (authorization, auth_options) = if !bootstrap && !is_remote {
            let (opts, auth) = wa
                .start_passkey_authentication(&creds)
                .map_err(|e| e.to_string())?;
            (Some(auth), Some(opts))
        } else {
            (None, None)
        };
        let token = store_ceremony(
            c,
            "register",
            &o,
            json_value(Registration {
                registration,
                authorization,
                bootstrap,
            })?,
        )?;
        let mut options = json_value(challenge)?;
        options["publicKey"]["authenticatorSelection"]["residentKey"] = json!("preferred");
        options["publicKey"]["hints"] = json!(["client-device", "hybrid"]);
        Ok(
            json!({"ceremony": token, "options": options, "authorization": auth_options, "bootstrap": bootstrap}),
        )
    })
}
fn audit(state: &AppState, action: &str, target: &str) {
    let change = super::delegation_log::DelegatedChange {
        ts: now(),
        actor: "owner".into(),
        class: "owner".into(),
        action: action.into(),
        target: target.into(),
        before: Value::Null,
        after: json!({"event": action}),
        grant: None,
        evidence_row: None,
        why: None,
    };
    if let Err(e) = super::delegation_log::record(state, change) {
        tracing::error!(verdict="delegation_passkey_audit_failed", error=%e, "passkey change audit failed");
    }
}
fn response(r: Result<Value>) -> Response {
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => {
            tracing::warn!(verdict="delegation_passkey_refused", reason=%e, measured=true, n_considered=1, "passkey ceremony refused");
            (
                StatusCode::FORBIDDEN,
                Json(json!({"ok": false, "error": e})),
            )
                .into_response()
        }
    }
}
async fn register_start(State(st): State<AppState>, h: HeaderMap, uri: Uri) -> Response {
    response(owner(&st, &h, &uri).and_then(|_| begin_registration(&st, origin(&h)?, remote(&h))))
}
#[derive(Deserialize)]
struct RegisterFinish {
    ceremony: String,
    credential: RegisterPublicKeyCredential,
    authorization: Option<PublicKeyCredential>,
}
async fn register_finish(
    State(st): State<AppState>,
    h: HeaderMap,
    uri: Uri,
    Json(b): Json<RegisterFinish>,
) -> Response {
    let r = (|| {
        owner(&st, &h, &uri)?;
        let o = origin(&h)?;
        let wa = webauthn(&o)?;
        write(&st, move |c| {
            let reg: Registration = parse(take(c, &b.ceremony, "register", &o)?)?;
            if reg.bootstrap {
                if total(c)? != 0 {
                    return Err("first registration was already completed".into());
                }
            } else if let Some(auth) = reg.authorization {
                verify(
                    c,
                    &wa,
                    &b.authorization
                        .ok_or("existing passkey proof is required")?,
                    auth,
                )?;
            } else if !remote(&h) {
                return Err("registration requires a Remote owner or existing passkey".into());
            }
            let pk = wa
                .finish_passkey_registration(&b.credential, &reg.registration)
                .map_err(|e| e.to_string())?;
            let key_id = id(pk.cred_id());
            sql(c.execute("INSERT INTO _amux_delegation_passkeys(id,rp_id,credential,created_at,modified_at) VALUES(?1,?2,?3,?4,?4)", params![key_id, rp(&wa), json_value(pk)?.to_string(), now()]))?;
            Ok(json!({"ok": true, "id": key_id, "bootstrap": reg.bootstrap}))
        })
    })();
    if let Ok(v) = &r {
        let first = v["bootstrap"] == true;
        if first {
            tracing::warn!(verdict="delegation_passkey_first_registration", measured=true, n_considered=1, "FIRST owner passkey registered with owner bearer bootstrap; verify enrollment in Delegated changes");
        }
        audit(
            &st,
            if first {
                "passkey:first_registration"
            } else {
                "passkey:registered"
            },
            v["id"].as_str().unwrap_or(""),
        );
    }
    response(r)
}
async fn list(State(st): State<AppState>, h: HeaderMap, uri: Uri) -> Response {
    response((|| {
        owner(&st, &h, &uri)?;
        let conn = st.store.read().map_err(|e| e.to_string())?;
        let mut q = sql(conn.prepare("SELECT id,rp_id,created_at,revoked_at FROM _amux_delegation_passkeys ORDER BY created_at"))?;
        let rows = sql(q.query_map([], |r| Ok(json!({"id": r.get::<_,String>(0)?, "rp_id": r.get::<_,String>(1)?, "created_at": r.get::<_,f64>(2)?, "revoked_at": r.get::<_,Option<f64>>(3)?}))))?;
        let keys = sql(rows.collect::<rusqlite::Result<Vec<_>>>())?;
        Ok(
            json!({"credentials": keys, "mode": if passkey_mode() {"passkey"} else {"cite"}, "measured": true, "n_considered": keys.len()}),
        )
    })())
}
#[derive(Deserialize)]
struct Begin {
    purpose: String,
}
async fn authenticate_start(
    State(st): State<AppState>,
    h: HeaderMap,
    uri: Uri,
    Json(b): Json<Begin>,
) -> Response {
    response((|| {
        owner(&st, &h, &uri)?;
        if let Some(code) = b.purpose.strip_prefix("grant:") {
            let grant = super::delegation_grants::load(&st, code).ok_or("no such grant")?;
            if !passkey_mode() || grant.status != "pending" || grant.expires_at < now() {
                return Err("grant is not pending in passkey mode".into());
            }
        } else if !b.purpose.starts_with("revoke:") {
            return Err("unknown passkey purpose".into());
        }
        let o = origin(&h)?;
        let wa = webauthn(&o)?;
        write(&st, move |c| {
            let creds = credentials(c, rp(&wa))?;
            let (options, auth) = wa
                .start_passkey_authentication(&creds)
                .map_err(|e| e.to_string())?;
            let token = store_ceremony(c, &b.purpose, &o, json_value(auth)?)?;
            Ok(json!({"ceremony": token, "options": options}))
        })
    })())
}
async fn revoke(
    State(st): State<AppState>,
    h: HeaderMap,
    uri: Uri,
    Path(key): Path<String>,
    Json(b): Json<Value>,
) -> Response {
    let target = key.clone();
    let r = (|| {
        owner(&st, &h, &uri)?;
        let is_remote = remote(&h);
        let o = origin(&h)?;
        let wa = webauthn(&o)?;
        write(&st, move |c| {
            if !is_remote {
                let proof: Proof = parse(b)?;
                let auth = parse(take(c, &proof.ceremony, &format!("revoke:{key}"), &o)?)?;
                verify(c, &wa, &proof.credential, auth)?;
            }
            let n = sql(c.execute("UPDATE _amux_delegation_passkeys SET revoked_at=?1,modified_at=?1 WHERE id=?2 AND revoked_at IS NULL", params![now(),key]))?;
            if n != 1 {
                return Err("credential is unknown or already revoked".into());
            }
            Ok(json!({"ok": true}))
        })
    })();
    if r.is_ok() {
        audit(&st, "passkey:revoked", &target);
    }
    response(r)
}
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/delegation/passkeys", get(list))
        .route(
            "/api/delegation/passkeys/register/start",
            post(register_start),
        )
        .route(
            "/api/delegation/passkeys/register/finish",
            post(register_finish),
        )
        .route(
            "/api/delegation/passkeys/authenticate/start",
            post(authenticate_start),
        )
        .route("/api/delegation/passkeys/{key}/revoke", post(revoke))
}

#[cfg(test)]
mod tests {
    use super::*;
    use webauthn_authenticator_rs::{softpasskey::SoftPasskey, WebauthnAuthenticator};
    type Authenticator = WebauthnAuthenticator<SoftPasskey>;
    fn fixture(dir: &std::path::Path) -> AppState {
        std::env::set_var("AMUX_DELEGATION_APPROVAL", "passkey");
        std::env::set_var("AMUX_DELEGATION_PASSKEY_ORIGINS", DEFAULT_ORIGIN);
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        std::fs::write(
            dir.join("sessions/test-dev-a.env"),
            "CC_DIR=\"/tmp\"\nCC_TAGS=\"dev\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("sessions/mrpopo.env"),
            "CC_DIR=\"/tmp\"\nCC_TAGS=\"leads\"\n",
        )
        .unwrap();
        AppState {
            store: Arc::new(crate::db::Store::open(&dir.join("test.db")).unwrap()),
            started: std::time::Instant::now(),
            build_hash: "test".into(),
            auth_token: Some("test-owner".into()),
            reconciled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }
    fn headers() -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("authorization", "Bearer test-owner".parse().unwrap());
        h.insert("origin", DEFAULT_ORIGIN.parse().unwrap());
        h
    }
    async fn output(r: Response) -> (StatusCode, Value) {
        let status = r.status();
        let b = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
        (status, serde_json::from_slice(&b).unwrap())
    }
    async fn enroll(st: &AppState, client: &mut Authenticator, is_remote: bool) -> Value {
        let begin = begin_registration(st, DEFAULT_ORIGIN.into(), is_remote).unwrap();
        let auth = begin["authorization"].as_object().map(|_| {
            client
                .do_authentication(
                    Url::parse(DEFAULT_ORIGIN).unwrap(),
                    parse(begin["authorization"].clone()).unwrap(),
                )
                .unwrap()
        });
        let credential = client
            .do_registration(
                Url::parse(DEFAULT_ORIGIN).unwrap(),
                parse(begin["options"].clone()).unwrap(),
            )
            .unwrap();
        let mut h = headers();
        if is_remote {
            h.insert(
                super::super::process_identity::PROCESS_ORIGIN_HEADER,
                "remote".parse().unwrap(),
            );
        }
        let (status, out) = output(
            register_finish(
                State(st.clone()),
                h,
                Uri::from_static("/api/delegation/passkeys/register/finish"),
                Json(RegisterFinish {
                    ceremony: begin["ceremony"].as_str().unwrap().into(),
                    credential,
                    authorization: auth,
                }),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "registration failed: {out}");
        out
    }
    fn grant(st: &AppState, code: &str) {
        let code = code.to_string();
        write(st, move |c| {
            sql(c.execute("INSERT INTO _amux_delegation_grants(code,actor,class,action,target,method,path,body,sentence,created_at,expires_at,status) VALUES(?1,'mrpopo','mrpopo','set_config:toggle_yolo','test-dev-a','PATCH','/api/sessions/test-dev-a/config','{\"toggle_yolo\":true}','test',?2,?3,'pending')",params![code,now(),now()+600.0]))?;
            Ok(())
        }).unwrap();
    }
    async fn challenge(st: &AppState, purpose: &str) -> Value {
        let (status, out) = output(
            authenticate_start(
                State(st.clone()),
                headers(),
                Uri::from_static("/api/delegation/passkeys/authenticate/start"),
                Json(Begin {
                    purpose: purpose.into(),
                }),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{out}");
        out
    }
    fn assertion(
        client: &mut Authenticator,
        begin: &Value,
        change: impl FnOnce(&mut Value),
    ) -> Proof {
        let mut options = begin["options"].clone();
        change(&mut options);
        let opts: RequestChallengeResponse = parse(options).unwrap();
        let o = Url::parse(&format!("https://{}", opts.public_key.rp_id)).unwrap();
        let credential = client.do_authentication(o, opts).unwrap();
        Proof {
            ceremony: begin["ceremony"].as_str().unwrap().into(),
            credential,
        }
    }
    #[tokio::test]
    async fn registration_multiple_credentials_and_non_owner_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = super::super::settings::test_env::set_home(tmp.path());
        let st = fixture(tmp.path());
        let mut client = WebauthnAuthenticator::new(SoftPasskey::new(true));
        let first = enroll(&st, &mut client, false).await;
        assert_eq!(first["bootstrap"], true);
        let second = enroll(&st, &mut client, false).await;
        assert_eq!(second["bootstrap"], false);
        assert_eq!(total(&st.store.read().unwrap()).unwrap(), 2);
        for nonowner in [HeaderMap::new(), {
            let mut h = headers();
            h.insert("x-amux-session", "test-worker".parse().unwrap());
            h
        }] {
            let (status, _) = output(
                register_start(
                    State(st.clone()),
                    nonowner,
                    Uri::from_static("/api/delegation/passkeys/register/start"),
                )
                .await,
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "non-owner enrollment");
        }
        let audit =
            std::fs::read_to_string(tmp.path().join("audit/delegated-changes.jsonl")).unwrap();
        assert!(audit.contains("passkey:first_registration"));
    }
    #[tokio::test]
    async fn uv_approval_applies_once_and_typed_lines_cannot_bypass() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = super::super::settings::test_env::set_home(tmp.path());
        let st = fixture(tmp.path());
        let mut client = WebauthnAuthenticator::new(SoftPasskey::new(true));
        enroll(&st, &mut client, false).await;
        grant(&st, "ABCD");
        super::super::delegation_grants::on_row(&st, 1, "amux grant ABCD").await;
        assert_eq!(
            super::super::delegation_grants::load(&st, "ABCD")
                .unwrap()
                .status,
            "pending"
        );
        let begin = challenge(&st, "grant:ABCD").await;
        let p = assertion(&mut client, &begin, |_| {});
        let body = json!({"ceremony":p.ceremony,"credential":p.credential});
        // The real grant endpoint consumes the proof then replays the stored change.
        let router = super::super::delegation_grants::routes().with_state(st.clone());
        use tower::ServiceExt;
        let request = || {
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/delegation/grants/ABCD/approve")
                .header("authorization", "Bearer test-owner")
                .header("origin", DEFAULT_ORIGIN)
                .header("content-type", "application/json")
                .body(axum::body::Body::from(body.to_string()))
                .unwrap()
        };
        let (status, out) = output(router.clone().oneshot(request()).await.unwrap()).await;
        assert_eq!(status, StatusCode::OK, "{out}");
        assert_eq!(
            super::super::delegation_grants::load(&st, "ABCD")
                .unwrap()
                .status,
            "applied"
        );
        let (status, _) = output(router.oneshot(request()).await.unwrap()).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "used proof must not apply twice"
        );
        let env = std::fs::read_to_string(tmp.path().join("sessions/test-dev-a.env")).unwrap();
        assert!(
            env.contains("dangerously") || env.contains("CC_AUTO_CONTINUE=\"1\""),
            "stored change: {env}"
        );
    }
    #[tokio::test]
    async fn grant_binding_replay_expiry_uv_origin_rp_and_revocation_fail_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = super::super::settings::test_env::set_home(tmp.path());
        let st = fixture(tmp.path());
        let mut client = WebauthnAuthenticator::new(SoftPasskey::new(true));
        let key = enroll(&st, &mut client, false).await;
        grant(&st, "ABCD");
        grant(&st, "EFGH");
        let begin = challenge(&st, "grant:ABCD").await;
        let p = assertion(&mut client, &begin, |_| {});
        assert!(
            approval_proof_ok(&st, "EFGH", &headers(), p).is_err(),
            "A challenge cannot approve B"
        );
        let p = assertion(&mut client, &begin, |_| {});
        assert!(
            approval_proof_ok(&st, "ABCD", &headers(), p).is_err(),
            "a refused attempt also consumes its challenge"
        );
        for case in ["uv", "origin", "rp", "expired", "revoked"] {
            let begin = challenge(&st, "grant:ABCD").await;
            let p = if case == "origin" {
                Proof {
                    ceremony: begin["ceremony"].as_str().unwrap().into(),
                    credential: client
                        .do_authentication(
                            Url::parse("https://evil.amux.maxwangestates.com").unwrap(),
                            parse(begin["options"].clone()).unwrap(),
                        )
                        .unwrap(),
                }
            } else {
                assertion(&mut client, &begin, |opts| {
                    if case == "uv" {
                        opts["publicKey"]["userVerification"] = json!("discouraged");
                    }
                    if case == "rp" {
                        opts["publicKey"]["rpId"] = json!("attacker.example");
                    }
                })
            };
            if case == "expired" {
                let token = p.ceremony.clone();
                write(&st, move |c| {
                    sql(c.execute(
                        "UPDATE _amux_delegation_ceremonies SET expires_at=0 WHERE id=?1",
                        [token],
                    ))?;
                    Ok(())
                })
                .unwrap();
            }
            if case == "revoked" {
                let key = key["id"].as_str().unwrap().to_string();
                write(&st, move |c| {
                    sql(c.execute(
                        "UPDATE _amux_delegation_passkeys SET revoked_at=?1 WHERE id=?2",
                        params![now(), key],
                    ))?;
                    Ok(())
                })
                .unwrap();
            }
            assert!(
                approval_proof_ok(&st, "ABCD", &headers(), p).is_err(),
                "{case} must refuse"
            );
        }
        assert_eq!(
            super::super::delegation_grants::load(&st, "ABCD")
                .unwrap()
                .status,
            "pending"
        );
        // Revoking the last credential never reopens bootstrap.
        let begin = begin_registration(&st, DEFAULT_ORIGIN.into(), true).unwrap();
        assert_eq!(begin["bootstrap"], false);
    }
    #[tokio::test]
    async fn cite_mode_does_not_consult_passkey_proof() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = super::super::settings::test_env::set_home(tmp.path());
        let st = fixture(tmp.path());
        std::env::set_var("AMUX_DELEGATION_APPROVAL", "cite");
        grant(&st, "ABCD");
        use tower::ServiceExt;
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/api/delegation/grants/ABCD/approve")
            .header("authorization", "Bearer test-owner")
            .header(
                super::super::process_identity::PROCESS_ORIGIN_HEADER,
                "remote",
            )
            .body(axum::body::Body::empty())
            .unwrap();
        let (status, out) = output(
            super::super::delegation_grants::routes()
                .with_state(st.clone())
                .oneshot(req)
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "cite does not require a passkey or Origin: {out}"
        );
        assert_eq!(
            super::super::delegation_grants::load(&st, "ABCD")
                .unwrap()
                .status,
            "applied"
        );
    }
}
