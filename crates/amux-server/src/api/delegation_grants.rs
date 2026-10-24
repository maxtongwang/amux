// Explicit requests and grants for widening changes — Max 2026-10-23: approval identity resolution; refs maxtongwang/ai-config#160.
//! POST /api/delegation/requests derives a pending request through the real
//! authorization table, stores the exact structured change, stable actor/target
//! and owner-session incarnations, and queues a generated question in the owner
//! session's Messages. A render receipt, not creation, establishes display. Asker-selected reply sessions and free-text funding are
//! superseded (Max 2026-10-23 20:08).
//! Its grant code is the request ID. Free text never creates requests.
//! Typed affirmative correlation lives in delegation_requests; passkey approval
//! still requires a direct Remote owner. Every application cites request_id and
//! compares operands plus method/path/body; internal replay rechecks identities.
//! A reply is spent once in the grant claim transaction (0127/0128). Failed,
//! interrupted and undone grants retain their evidence. Handler/filesystem
//! writes are separate resources: applying grants are never blindly replayed.
//! Undo restores the original recorded delta only while identities and current
//! state match; it never releases or transfers approval to another request.
use super::AppState;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, Request, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// How long a pending (passkey-mode) grant can be approved.
pub(crate) const GRANT_TTL_S: f64 = 86_400.0;
/// The approval mode key (global scope or server env).
pub(crate) const MODE_KEY: &str = "AMUX_DELEGATION_APPROVAL";
/// Server-only: set on the internal replay of an approved pending grant.
pub(crate) const GRANT_HEADER: &str = "x-amux-grant";
/// Response header naming the grant a cited change applied as.
pub(crate) const GRANT_CODE_HEADER: &str = "x-amux-grant-code";
/// Unambiguous code alphabet (no 0/O, 1/I/L, U).
const ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTVWXYZ";
const CODE_LEN: usize = 4;
/// Bound random-code collision retries; existing request IDs are never replaced.
const CODE_MINT_ATTEMPTS: usize = 10;
/// Recent request/resolution rows returned by each list surface.
const LIST_LIMIT_ROWS: i64 = 200;
/// A request body larger than this is not captured, so it cannot become a grant.
const CAPTURE_LIMIT: usize = 1 << 20;
/// Fail closed: a write failure never releases an already reserved citation.
const APPLY_FAILED_REASON: &str = "failed: re-ask the owner; any cited pair remains spent";
/// The paths whose handlers can make a grant.
const CAPTURE_PREFIXES: [&str; 8] = [
    "/api/sessions", "/api/workers", "/api/scope", "/api/groups/", "/api/settings/", "/api/config/", "/api/env/", "/api/alert/config",
];
/// Request fields that are delegation metadata, never part of the change.
const META: [&str; 3] = super::delegation::META_FIELDS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Cite,
    Passkey,
}

/// The configured approval mode: process env, then the global layer
/// (`amux.env`); `cite` unless it says `passkey`. Read on every request, so
/// switching it changes behaviour with no restart and no code change.
pub(crate) fn mode() -> Mode {
    let raw = std::env::var(MODE_KEY).ok().filter(|v| !v.trim().is_empty()).or_else(|| {
        super::session_verbs::EnvFile::load(&crate::config::amux_home().join("amux.env")).get(MODE_KEY).map(str::to_string)
    });
    match raw.as_deref().map(|v| v.trim().to_ascii_lowercase()) {
        Some(v) if v == "passkey" => Mode::Passkey,
        _ => Mode::Cite,
    }
}

fn mode_str(m: Mode) -> &'static str {
    match m {
        Mode::Cite => "cite",
        Mode::Passkey => "passkey",
    }
}

/// THE approval proof for a pending grant (passkey mode, the Approve button).
/// Today: the request came from a NON-LOCAL Remote peer (#7's verdict, never
/// the forwarded one). The passkey (WebAuthn) follow-up extends this function.
pub(crate) fn approval_proof_ok(headers: &HeaderMap) -> bool {
    process_origin(headers) == super::process_identity::REMOTE_DIRECT
}

fn process_origin(headers: &HeaderMap) -> &str {
    headers.get(super::process_identity::PROCESS_ORIGIN_HEADER).and_then(|v| v.to_str().ok()).unwrap_or("")
}

/// The state a delegated config change can write, as it reads now: env files
/// key by key, rules/memory text whole, and every row of each table in
/// [`TABLES`] with ALL of its columns (review B2: a grant stored in `prefs`
/// was invisible to undo, and the four-column group_config copy wiped
/// `execution_policy`/`execution_rev` on restore).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Snapshot {
    env: BTreeMap<String, BTreeMap<String, String>>,
    /// Each env file's exact bytes, so undo can write a file back as it was.
    env_raw: BTreeMap<String, String>,
    text: BTreeMap<String, String>,
    /// table -> row key (its key columns as JSON) -> the whole row.
    tables: BTreeMap<String, BTreeMap<String, Map<String, Value>>>,
}

/// Every table a grant kind writes, with its key columns:
/// `prefs` (connectors at every level, skins from an instance apply),
/// `session_gates` (worker and group gates), `statuses` (global gates, column
/// labels/positions from an instance apply), `group_config` (group config and
/// group create). Undo restores rows here by UPDATE of the changed columns.
const TABLES: &[(&str, &[&str])] = &[
    ("prefs", &["key"]),
    ("session_gates", &["session", "status"]),
    ("statuses", &["id"]),
    ("group_config", &["name"]),
];

fn sql_value(v: rusqlite::types::ValueRef<'_>) -> Value {
    use rusqlite::types::ValueRef;
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => json!(i),
        ValueRef::Real(f) => json!(f),
        ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
        ValueRef::Blob(b) => json!({"blob": b.iter().map(|x| format!("{x:02x}")).collect::<String>()}),
    }
}

fn to_sql(v: &Value) -> rusqlite::types::Value {
    use rusqlite::types::Value as S;
    match v {
        Value::Null => S::Null,
        Value::Bool(b) => S::Integer(*b as i64),
        Value::Number(n) => n.as_i64().map(S::Integer).unwrap_or_else(|| S::Real(n.as_f64().unwrap_or(0.0))),
        Value::String(t) => S::Text(t.clone()),
        Value::Object(o) if o.contains_key("blob") => {
            let h = o["blob"].as_str().unwrap_or("");
            S::Blob((0..h.len() / 2).filter_map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).ok()).collect())
        }
        other => S::Text(other.to_string()),
    }
}

/// Every row of `table` as {key -> row}; a missing table reads as empty.
fn table_rows(conn: &rusqlite::Connection, table: &str, keys: &[&str]) -> BTreeMap<String, Map<String, Value>> {
    let mut out = BTreeMap::new();
    let Ok(mut st) = conn.prepare(&format!("SELECT * FROM {table}")) else { return out };
    let cols: Vec<String> = st.column_names().into_iter().map(str::to_string).collect();
    let Ok(mut rows) = st.query([]) else { return out };
    while let Ok(Some(r)) = rows.next() {
        let mut row = Map::new();
        for (i, c) in cols.iter().enumerate() {
            row.insert(c.clone(), r.get_ref(i).map(sql_value).unwrap_or(Value::Null));
        }
        let key = Value::Array(keys.iter().map(|k| row.get(*k).cloned().unwrap_or(Value::Null)).collect()).to_string();
        out.insert(key, row);
    }
    out
}

fn rel(home: &std::path::Path, p: &std::path::Path) -> String {
    p.strip_prefix(home).unwrap_or(p).to_string_lossy().into_owned()
}

/// An env file's configuration pairs. `CC_INCARNATION` is the worker's
/// identity (amux #3), which every env writer preserves: a grant never changes
/// it and undo never restores it, so it is not part of a change.
fn config_pairs(p: &std::path::Path) -> BTreeMap<String, String> {
    super::session_verbs::EnvFile::load(p).pairs().iter()
        .filter(|(k, _)| k != super::rename_identity::INCARNATION_KEY).cloned().collect()
}

fn snapshot(state: &AppState) -> Snapshot {
    let home = crate::config::amux_home();
    let mut snap = Snapshot::default();
    for dir in [home.clone(), home.join("sessions"), home.join("env")] {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("env") && p.is_file() {
                snap.env.insert(rel(&home, &p), config_pairs(&p));
                if let Ok(t) = std::fs::read_to_string(&p) {
                    snap.env_raw.insert(rel(&home, &p), t);
                }
            }
        }
    }
    // `rules/<w>.md` is derived from group rules; restoring it with the rules
    // leaves every worker's context exactly as before.
    for dir in [home.join("memory"), home.join("memory").join("tags"), home.join("rules")] {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("md") && p.is_file() {
                if let Ok(t) = std::fs::read_to_string(&p) {
                    snap.text.insert(rel(&home, &p), t);
                }
            }
        }
    }
    if let Ok(conn) = state.store.read() {
        for (t, keys) in TABLES {
            snap.tables.insert(t.to_string(), table_rows(&conn, t, keys));
        }
    }
    snap
}

/// What changed between two snapshots: per env key, per text file, per group
/// config row, each as {before, after}. A secret key is never stored.
fn diff(before: &Snapshot, after: &Snapshot) -> (Value, bool) {
    let mut env = Map::new();
    let mut env_raw = Map::new();
    let mut undoable = true;
    let files: std::collections::BTreeSet<&String> = before.env.keys().chain(after.env.keys()).collect();
    let empty = BTreeMap::new();
    for f in files {
        let (b, a) = (before.env.get(f).unwrap_or(&empty), after.env.get(f).unwrap_or(&empty));
        let keys: std::collections::BTreeSet<&String> = b.keys().chain(a.keys()).collect();
        let mut changed = Map::new();
        for k in keys.into_iter().filter(|k| b.get(*k) != a.get(*k)) {
            if super::delegation::is_secret_key(k) {
                undoable = false;
                changed.insert(k.clone(), json!({"secret": true}));
            } else {
                changed.insert(k.clone(), json!({"before": b.get(k), "after": a.get(k)}));
            }
        }
        if !changed.is_empty() {
            env.insert(f.clone(), Value::Object(changed));
            // The exact bytes, only for a file that holds no secret key: a
            // grant never stores a secret value (review B2).
            if !b.keys().chain(a.keys()).any(|k| super::delegation::is_secret_key(k)) {
                env_raw.insert(f.clone(), json!({"before": before.env_raw.get(f), "after": after.env_raw.get(f)}));
            }
        }
    }
    let mut text = Map::new();
    let tfiles: std::collections::BTreeSet<&String> = before.text.keys().chain(after.text.keys()).collect();
    for f in tfiles.into_iter().filter(|f| before.text.get(*f) != after.text.get(*f)) {
        text.insert(f.clone(), json!({"before": before.text.get(f), "after": after.text.get(f)}));
    }
    let mut tables = Map::new();
    let empty_t = BTreeMap::new();
    for (t, _) in TABLES {
        let (b, a) = (before.tables.get(*t).unwrap_or(&empty_t), after.tables.get(*t).unwrap_or(&empty_t));
        let rows: std::collections::BTreeSet<&String> = b.keys().chain(a.keys()).collect();
        let mut changed = Map::new();
        for k in rows.into_iter().filter(|k| b.get(*k) != a.get(*k)) {
            // A secret-looking pref key's value is never stored.
            if super::delegation::is_secret_key(&k.replace(['[', ']', '"'], "")) {
                undoable = false;
                changed.insert(k.clone(), json!({"secret": true}));
            } else {
                changed.insert(k.clone(), json!({"before": b.get(k), "after": a.get(k)}));
            }
        }
        if !changed.is_empty() {
            tables.insert(t.to_string(), Value::Object(changed));
        }
    }
    // Env files the change created: undo removes them again once empty.
    let created: Vec<&String> = after.env.keys().filter(|f| !before.env.contains_key(*f)).collect();
    (json!({"env": env, "env_raw": env_raw, "text": text, "tables": tables, "created": created}), undoable)
}

/// The mutating request being handled, and the grant it applies (if any).
#[derive(Clone)]
pub(crate) struct Captured {
    pub method: String,
    pub path: String,
    pub body: Value,
    armed: Arc<Mutex<Option<(String, Snapshot)>>>,
    checked_env: Arc<Mutex<BTreeMap<std::path::PathBuf, BTreeMap<String, String>>>>,
    compare_failed: Arc<Mutex<bool>>,
}

// Owner and delegated HTTP config writes share this lock, including across
// awaits in a handler. EnvFile also compares under its cross-process lock.
static CONFIG_MUTATION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

tokio::task_local! {
    static CAPTURED: Captured;
}

/// Remember exactly the env state used by authorization.
pub(crate) fn expect_env(path: &std::path::Path) {
    if let Some(c) = captured() {
        let pairs = super::session_verbs::EnvFile::load(path).pairs().iter().cloned().collect();
        c.checked_env.lock().unwrap_or_else(|e| e.into_inner()).insert(path.to_path_buf(), pairs);
    }
}

/// Called WITH EnvFile's process and file locks held, before any write.
/// Compare the keys this write touches plus stable identity. Unrelated writes
/// merge normally; a changed operand or incarnation refuses the stale write.
pub(crate) fn compare_env(path: &std::path::Path, keys: &[&str]) -> std::io::Result<()> {
    let Some(c) = captured() else { return Ok(()) };
    let checked = c.checked_env.lock().unwrap_or_else(|e| e.into_inner());
    let Some(expected) = checked.get(path) else { return Ok(()) };
    let current: BTreeMap<String, String> = super::session_verbs::EnvFile::load(path).pairs().iter().cloned().collect();
    if keys.iter().copied().chain(std::iter::once(super::rename_identity::INCARNATION_KEY)).any(|key| current.get(key) != expected.get(key)) {
        *c.compare_failed.lock().unwrap_or_else(|e| e.into_inner()) = true;
        tracing::warn!(verdict = "delegation_state_changed", measured = true, n_considered = 1,
            "delegated env compare-and-set refused a concurrent state change");
        return Err(std::io::Error::new(std::io::ErrorKind::WouldBlock, "delegation_state_changed"));
    }
    Ok(())
}

/// Advance the comparison after our own successful write, for multi-step handlers.
pub(crate) fn env_written(path: &std::path::Path) {
    if captured().is_some_and(|c| c.checked_env.lock().unwrap_or_else(|e| e.into_inner()).contains_key(path)) {
        expect_env(path);
    }
}

/// Middleware: buffer a mutating request to a delegable path so it can be
/// stored exactly, and finalize the grant the handler applied.
pub async fn capture(State(state): State<AppState>, req: Request<Body>, next: Next) -> Response {
    let mutating = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    let path = req.uri().path().to_string();
    if !mutating || !CAPTURE_PREFIXES.iter().any(|p| path.starts_with(p)) {
        return next.run(req).await;
    }
    // Sends can synchronously approve and replay a grant. Never hold this
    // lock around /send or /report: their replay takes the config lock itself.
    let serial = path == "/api/scope" || path == "/api/workers" || path == "/api/sessions"
        || path.ends_with("/config") || path.ends_with("/rename")
        || ["/api/groups/", "/api/settings/", "/api/config/", "/api/env/"].iter().any(|p| path.starts_with(p))
        || path.strip_prefix("/api/workers/").is_some_and(|p| !p.contains('/'));
    let _mutation = if serial { Some(CONFIG_MUTATION.lock().await) } else { None };
    if serial && (path.starts_with("/api/env/") || path == "/api/sessions") {
        tracing::info!(path = %path, verdict = "delegation_config_serialized", measured = true, n_considered = 1,
            "env apply and owner create share serialization through preflight and writes");
    }
    let (parts, body) = req.into_parts();
    // A body declared larger than any config change (a long prompt to
    // `/send`, whose routes carry no body limit) passes through uncaptured;
    // a grant then finds no captured body and is refused (`grant_unavailable`).
    let declared = parts.headers.get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0).max(axum::body::HttpBody::size_hint(&body).lower());
    if declared > CAPTURE_LIMIT as u64 {
        return next.run(Request::from_parts(parts, body)).await;
    }
    let bytes = match axum::body::to_bytes(body, CAPTURE_LIMIT).await {
        Ok(b) => b,
        Err(e) => return (StatusCode::PAYLOAD_TOO_LARGE, Json(json!({"ok": false, "code": "body_too_large", "error": e.to_string()}))).into_response(),
    };
    let captured = Captured {
        method: parts.method.to_string(),
        path: parts.uri.path_and_query().map(|p| p.as_str().to_string()).unwrap_or(path),
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        armed: Arc::new(Mutex::new(None)),
        checked_env: Arc::new(Mutex::new(BTreeMap::new())),
        compare_failed: Arc::new(Mutex::new(false)),
    };
    let armed = captured.armed.clone();
    let compare_failed = captured.compare_failed.clone();
    let req = Request::from_parts(parts, Body::from(bytes));
    let mut resp = CAPTURED.scope(captured, next.run(req)).await;
    let comparison_failed = *compare_failed.lock().unwrap_or_else(|e| e.into_inner());
    if comparison_failed {
        resp = (StatusCode::CONFLICT, Json(super::delegation::refusal_body(
            "delegation_state_changed", "configuration changed after authorization; read it and ask again"))).into_response();
    }
    let taken = armed.lock().ok().and_then(|mut a| a.take());
    if let Some((code, before)) = taken {
        finalize(&state, &code, &before, resp.status());
        if !resp.status().is_success() && !comparison_failed {
            resp = (resp.status(), Json(json!({"ok": false, "code": "grant_apply_failed",
                "error": APPLY_FAILED_REASON, "grant_code": code}))).into_response();
        }
        if let Ok(v) = axum::http::HeaderValue::from_str(&code) {
            resp.headers_mut().insert(GRANT_CODE_HEADER, v);
        }
    }
    resp
}

fn captured() -> Option<Captured> {
    CAPTURED.try_with(Clone::clone).ok()
}

/// The stored body: the change only, without delegation metadata.
fn change_body(body: &Value) -> Value {
    let mut b = body.clone();
    if let Some(o) = b.as_object_mut() {
        for k in META {
            o.remove(k);
        }
    }
    b
}

fn now() -> f64 {
    crate::config::now_f64()
}

fn new_code() -> Option<String> {
    use std::io::Read;
    let mut b = [0u8; CODE_LEN];
    std::fs::File::open("/dev/urandom").ok()?.read_exact(&mut b).ok()?;
    Some(b.iter().map(|x| ALPHABET[*x as usize % ALPHABET.len()] as char).collect())
}

/// The change, in words Max can check: actor, action, target and the exact request.
fn sentence(code: &str, actor: &str, action: &str, target: &str, c: &Captured) -> String {
    let body = change_body(&c.body);
    let body = if body.is_null() { String::new() } else { format!(" {body}") };
    format!("amux grant {code}: {actor} asks to {action} on {target}: {} {}{body}", c.method, c.path)
}

fn forbid(code: &str, error: String) -> Response {
    tracing::warn!(code, verdict = "delegation_grant_refused", measured = true, n_considered = 1, "a grant was refused");
    (StatusCode::FORBIDDEN, Json(super::delegation::refusal_body(code, &error))).into_response()
}

struct NewGrant<'a> {
    actor: &'a str,
    class: &'a str,
    action: &'a str,
    target: &'a str,
    why: Option<String>,
    reply_session: &'a str,
    change: Value,
    target_incarnation: Option<String>,
}

/// Persist a question, not a display claim. Owner Messages or the native owner
/// assistant turn supplies the receipt. Identity is read, never minted.
#[allow(clippy::result_large_err)]
fn insert(state: &AppState, g: &NewGrant, c: &Captured) -> Result<(String, String), Response> {
    let actor_id = super::delegation_requests::incarnation(g.actor)
        .ok_or_else(|| forbid("request_actor_identity_missing", "asking worker needs an existing stable identity".into()))?;
    let reply_id = super::delegation_requests::incarnation(g.reply_session)
        .ok_or_else(|| forbid("request_reply_identity_missing", "reply session needs an existing stable identity".into()))?;
    let created = now();
    for _ in 0..CODE_MINT_ATTEMPTS {
        let Some(code) = new_code() else { break };
        let text = sentence(&code, g.actor, g.action, g.target, c);
        // The whole structured change is visible, without truncating operands.
        let question = format!("Approval request {code} from {}: {} {} {}. Reply yes to approve this exact change.", g.actor, c.method, c.path, g.change);
        let (id, actor, class, action, target, why, method, path, body, sentence, reply_session, change, actor_id, reply_id, target_id) =
            (code.clone(), g.actor.to_string(), g.class.to_string(), g.action.to_string(), g.target.to_string(), g.why.clone(), c.method.clone(), c.path.clone(), change_body(&c.body).to_string(), text.clone(), g.reply_session.to_string(), g.change.to_string(), actor_id.clone(), reply_id.clone(), g.target_incarnation.clone());
        let stored = state.store.write(move |conn| {
            if conn.query_row("SELECT EXISTS(SELECT 1 FROM _amux_delegation_grants WHERE code=?1)", [&id], |r| r.get::<_, bool>(0))? {
                return Ok(crate::db::WriteOutcome { applied: false, events: vec![] });
            }
            conn.execute("INSERT INTO cmd_history(text,type,session,ts,origin,delivery,delegation_session_incarnation) VALUES(?1,'session',?2,?3,?4,'approval_request',?5)",
                rusqlite::params![question,reply_session,(created*1000.0) as i64,actor,reply_id])?;
            let ask = conn.last_insert_rowid();
            conn.execute("INSERT INTO _amux_delegation_grants(code,actor,class,action,target,method,path,body,sentence,why,created_at,expires_at,status,mode,target_incarnation,actor_incarnation,reply_session,reply_incarnation,shown_row,change_spec,ask_row,ask_text,resolution_note) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,'pending',?13,?14,?15,?16,?17,NULL,?19,?18,?20,'awaiting display in owner Messages')",
                rusqlite::params![id,actor,class,action,target,method,path,body,sentence,why,created,created+GRANT_TTL_S,mode_str(mode()),target_id,actor_id,reply_session,reply_id,ask,change,question])?;
            Ok(crate::db::WriteOutcome { applied: true, events: vec![crate::db::PendingEvent {
                entity_type: amux_core::revision::EntityType::Message, entity_id: format!("MSG-{ask}"),
                mutation: amux_core::revision::MutationKind::Created, payload: None,
            }] })
        });
        match stored {
            Ok(o) if o.applied => return Ok((code, text)),
            Ok(_) => continue,
            Err(e) => return Err(forbid("request_create_failed", e.to_string())),
        }
    }
    Err(forbid("request_id_unavailable", "could not mint a unique request ID".into()))
}

/// Arm the running request to finalize `code` after its handler, with the
/// state as it reads now.
fn arm(state: &AppState, code: &str) -> bool {
    let Some(c) = captured() else { return false };
    let Ok(mut slot) = c.armed.lock() else { return false };
    *slot = Some((code.to_string(), snapshot(state)));
    true
}

/// Only the explicit creation endpoint enters this function through draft scope.
pub(crate) fn request_explicit(state: &AppState, actor: &str, class: &str, target: &super::delegation::Target, action: &super::delegation::Action, body: Option<&Value>, reply_session: &str) -> Response {
    let Some(c) = captured() else { return forbid("grant_unavailable", "request body was not captured".into()); };
    let target_id = match target {
        super::delegation::Target::Worker(w) => match super::delegation_requests::incarnation(w) {
            Some(id) => Some(id),
            None => return forbid("request_target_identity_missing", "target worker needs an existing stable identity".into()),
        },
        _ => None,
    };
    let action_name = action.label();
    let target_name = target.name();
    let change = super::delegation_requests::change(target, action, body);
    let (code, text) = match insert(state, &NewGrant { actor, class, action: &action_name, target: &target_name,
        why: super::delegation::request_why(body), reply_session, change, target_incarnation: target_id }, &c) {
        Ok(x) => x, Err(r) => return r,
    };
    tracing::info!(request_id = %code, actor, reply_session, verdict = "delegation_request_created", measured = true, n_considered = 1, "approval question awaits an owner Messages or native assistant render receipt");
    (StatusCode::ACCEPTED, Json(json!({"ok":true,"pending":true,"request_id":code,"grant_code":code,"sentence":text,
        "reply_session":reply_session,"question":load(state, &code).and_then(|g|g.ask_text),"how":"Display the exact question in owner Messages or mrpopo's own assistant turn with its request ID, then reply yes as the very next owner input; apply with request_id"}))).into_response()
}

/// Cite-mode applications spend the request's resolved reply; no new grant
/// is minted, and the stored change is checked before claiming it.
pub(crate) fn begin_requested(state: &AppState, id: &str, actor: &str, change: &Value) -> Result<String, (&'static str, String)> {
    let mut g = load(state, id).ok_or(("request_not_found", "unknown request ID".into()))?;
    let session = g.reply_session.as_deref().ok_or(("request_not_explicit", "legacy grant has no explicit request".into()))?;
    super::delegation_requests::resolve_session(state, session).map_err(|e| ("request_resolution_failed", e))?;
    g = load(state, id).ok_or(("request_not_found", "unknown request ID".into()))?;
    validate_requested(state, &g, actor, change)?;
    if let Some(e) = not_applicable(state, &g) { return Err(e); }
    if mode() == Mode::Passkey { return Err(("request_passkey_required", "approve this request with the Remote owner proof".into())); }
    let reply = g.resolution_row.ok_or((resolution_refusal(g.resolution_code.as_deref()), g.resolution_note.clone().unwrap_or_default()))?;
    let row = super::max_ref::one(state, reply).map_err(|e| (e.code(), "owner reply unavailable".into()))?
        .ok_or(("request_reply_missing", "owner reply unavailable".into()))?;
    if row.session != super::delegation::OWNER_AGENT || row.incarnation != g.reply_incarnation {
        return Err(("request_reply_identity_changed", "reply is outside the stored owner incarnation".into()));
    }
    super::max_ref::typed_by_max(state, &row).ok_or(("max_ref_not_human_typed", "reply is not owner input".into()))?;
    claim_pending(state, &g.code, &format!("owner:reply:{reply}"), Some(reply))?;
    if !arm(state, &g.code) { return Err(("grant_unavailable", "request could not be armed; reply remains spent".into())); }
    Ok(g.code)
}

fn resolution_refusal(code: Option<&str>) -> &'static str {
    match code {
        Some("request_ambiguous") => "request_ambiguous",
        Some("request_no_candidate") => "request_no_candidate",
        Some("request_reply_not_affirmative") => "request_reply_not_affirmative",
        Some("request_reply_identity_changed") => "request_reply_identity_changed",
        Some("request_reply_outside_window") => "request_reply_outside_window",
        Some("request_reply_not_owner_session") => "request_reply_not_owner_session",
        _ => "request_unresolved",
    }
}

fn validate_request_identity(state: &AppState, g: &GrantRow, actor: &str) -> Result<(), (&'static str, String)> {
    if g.reply_session.as_deref() != Some(super::delegation::OWNER_AGENT) {
        return Err(("request_reply_not_owner_session", "superseded asker-selected reply session; ask again in the owner session".into()));
    }
    if g.shown_row.is_none() { return Err(("request_not_shown", "request was never shown in a reply session".into())); }
    let conn = state.store.read().map_err(|_|("request_read_failed", "request message ledger unavailable".into()))?;
    let shown: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM cmd_history WHERE id=?1 AND session=?2 AND delegation_session_incarnation=?5 AND repeat_of=?6 AND delivered_at=ts AND ((origin=?3 AND type='session' AND text=?4 AND delivery='approval_display') OR (origin=session AND type='assistant' AND delivery='approval_pane_display' AND instr(text,?4)>0 AND instr(text,?7)>0)))",
        rusqlite::params![g.shown_row,g.reply_session,g.actor,g.ask_text,g.reply_incarnation,g.ask_row,g.code],|r|r.get(0))
        .map_err(|_|("request_read_failed", "request message ledger unavailable".into()))?;
    if !shown { return Err(("request_not_shown", "the recorded question is absent from the reply session".into())); }
    drop(conn);
    if !same_target(g) { return Err(("request_target_identity_changed", "request target identity changed".into())); }
    if !g.reply_session.as_ref().is_some_and(|session| g.reply_incarnation.is_some() && g.reply_incarnation == super::delegation_requests::incarnation(session)) {
        return Err(("request_reply_identity_changed", "shown reply session identity changed".into()));
    }
    if g.actor != actor || g.actor_incarnation.is_none() || g.actor_incarnation != super::delegation_requests::incarnation(actor) {
        return Err(("request_actor_identity_changed", "request belongs to a different asking worker incarnation".into()));
    }
    Ok(())
}

pub(crate) fn validate_requested(state: &AppState, g: &GrantRow, actor: &str, change: &Value) -> Result<(), (&'static str, String)> {
    validate_request_identity(state,g,actor)?;
    let Some(c) = captured() else { return Err(("grant_unavailable", "mutation not captured".into())); };
    if g.change_spec.as_deref().and_then(|s| serde_json::from_str::<Value>(s).ok()).as_ref() != Some(change)
        || g.method != c.method || g.path != c.path || serde_json::from_str::<Value>(&g.body).ok() != Some(change_body(&c.body)) {
        return Err(("request_change_mismatch", "applied action, target, setting, value, direction, method, path and body must equal the stored request".into()));
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub(crate) struct GrantRow {
    pub code: String,
    pub actor: String,
    pub class: String,
    pub action: String,
    pub target: String,
    pub method: String,
    pub path: String,
    pub body: String,
    pub sentence: String,
    pub status: String,
    pub expires_at: f64,
    pub evidence_row: Option<i64>,
    pub cited_excerpt: Option<String>,
    pub changes: Option<String>,
    /// The citation pair (0126): the ask's row and both texts.
    pub ask_row: Option<i64>,
    pub ask_text: Option<String>,
    pub reply_text: Option<String>,
    pub target_incarnation: Option<String>,
    pub cite_spent_at: Option<f64>,
    pub actor_incarnation: Option<String>,
    pub reply_session: Option<String>,
    pub reply_incarnation: Option<String>,
    pub shown_row: Option<i64>,
    pub change_spec: Option<String>,
    pub resolution_row: Option<i64>,
    pub resolution_code: Option<String>,
    pub resolution_note: Option<String>,
    pub created_at: f64,
}

const COLS: &str = "code, actor, class, action, target, method, path, body, sentence, status, expires_at, evidence_row, cited_excerpt, changes, ask_row, ask_text, reply_text, target_incarnation, cite_spent_at, actor_incarnation, reply_session, reply_incarnation, shown_row, change_spec, resolution_row, resolution_code, resolution_note, created_at";

fn map_row(r: &rusqlite::Row) -> rusqlite::Result<GrantRow> {
    Ok(GrantRow {
        code: r.get(0)?, actor: r.get(1)?, class: r.get(2)?, action: r.get(3)?, target: r.get(4)?, method: r.get(5)?,
        path: r.get(6)?, body: r.get(7)?, sentence: r.get(8)?, status: r.get(9)?, expires_at: r.get(10)?, evidence_row: r.get(11)?,
        cited_excerpt: r.get(12)?, changes: r.get(13)?, ask_row: r.get(14)?, ask_text: r.get(15)?, reply_text: r.get(16)?, target_incarnation: r.get(17)?, cite_spent_at: r.get(18)?, actor_incarnation: r.get(19)?, reply_session: r.get(20)?, reply_incarnation: r.get(21)?, shown_row: r.get(22)?, change_spec: r.get(23)?, resolution_row: r.get(24)?, resolution_code: r.get(25)?, resolution_note: r.get(26)?, created_at:r.get(27)?,
    })
}

pub(crate) fn load(state: &AppState, code: &str) -> Option<GrantRow> {
    let conn = state.store.read().ok()?;
    conn.query_row(&format!("SELECT {COLS} FROM _amux_delegation_grants WHERE code=?1"), [code.to_ascii_uppercase()], map_row).ok()
}

fn set_status(state: &AppState, code: &str, from: &str, to: &str, by: &str, evidence: Option<i64>) -> bool {
    let (code, from, to, by) = (code.to_string(), from.to_string(), to.to_string(), by.to_string());
    let at = now();
    state.store.write(move |conn| {
        let n = conn.execute(
            "UPDATE _amux_delegation_grants SET status=?1, decided_by=COALESCE(decided_by, ?2), decided_at=?3, evidence_row=COALESCE(?4, evidence_row) \
             WHERE code=?5 AND status=?6",
            rusqlite::params![to, by, at, evidence, code, from],
        )?;
        Ok(crate::db::WriteOutcome { applied: n == 1, events: vec![] })
    }).is_ok_and(|o| o.applied)
}

#[allow(clippy::too_many_arguments)] // one flat record per grant; a context struct would only rename the fields
fn log_transition(state: &AppState, code: &str, what: &str, actor: &str, class: &str, target: &str, after: Value, why: Option<String>) {
    let change = super::delegation_log::DelegatedChange {
        ts: now(),
        actor: actor.to_string(),
        class: class.to_string(),
        action: format!("grant:{what}"),
        target: target.to_string(),
        before: Value::Null,
        after,
        grant: Some(code.to_string()),
        evidence_row: None,
        why,
    };
    if let Err(e) = super::delegation_log::record(state, change) {
        tracing::warn!(grant = code, what, error = %e, verdict = "delegation_grant_log_failed", measured = true, n_considered = 1,
            "a grant transition could not be logged");
    }
}

/// After the handler: record what changed and the outcome, once.
fn finalize(state: &AppState, code: &str, before: &Snapshot, status: StatusCode) {
    let Some(g) = load(state, code) else { return };
    let (changes, undoable) = diff(before, &snapshot(state));
    let to = if status.is_success() { "applied" } else { "failed" };
    let (c2, ch) = (code.to_string(), changes.to_string());
    let stored = state.store.write(move |conn| {
        let n = conn.execute("UPDATE _amux_delegation_grants SET status=?1, changes=?2, undoable=?3, decided_at=?4 WHERE code=?5 AND status='applying'",
            rusqlite::params![to, ch, undoable as i64, now(), c2])?;
        Ok(crate::db::WriteOutcome { applied: n == 1, events: vec![] })
    });
    if !stored.is_ok_and(|o| o.applied) {
        tracing::warn!(grant = code, verdict = "delegation_grant_finalize_lost", measured = true, n_considered = 1,
            "a grant's handler finished but the grant was not applying; nothing recorded twice");
        return;
    }
    log_transition(state, code, to, &g.actor, &g.class, &g.target,
        json!({"sentence": g.sentence, "cited_excerpt": g.cited_excerpt, "ask_text": g.ask_text, "reply_text": g.reply_text,
            "status": status.as_u16(), "failure_reason": failure_reason(to), "changes": changes, "undoable": undoable}), None);
    if to == "failed" {
        tracing::warn!(grant = code, actor = %g.actor, status = status.as_u16(), reason = APPLY_FAILED_REASON,
            verdict = "delegation_grant_failed", measured = true, n_considered = 1,
            "a grant failed; its citation remains spent");
    } else {
        tracing::info!(grant = code, actor = %g.actor, status = status.as_u16(), undoable,
            verdict = "delegation_grant_applied", measured = true, n_considered = 1, "a grant's change ran");
    }
}

fn failure_reason(status: &str) -> Option<&'static str> {
    (status == "failed").then_some(APPLY_FAILED_REASON)
}

/// A pending grant past its expiry becomes `expired` (logged once).
fn expire_if_due(state: &AppState, g: &GrantRow) -> bool {
    if g.status == "pending" && now() > g.expires_at {
        if set_status(state, &g.code, "pending", "expired", "clock", None) {
            log_transition(state, &g.code, "expired", &g.actor, &g.class, &g.target, json!({"sentence": g.sentence}), None);
        }
        return true;
    }
    false
}

/// A worker grant keys on its incarnation. Layer targets have no worker id.
fn worker_identity(target: &str) -> Result<Option<String>, String> {
    let home = crate::config::amux_home();
    if !super::session_verbs::valid_session_name(target) || !home.join("sessions").join(format!("{target}.env")).exists() {
        return Ok(None);
    }
    super::rename_identity::incarnation(&home, target).map(Some).ok_or_else(|| "worker has no stable incarnation; record a fresh ask".into())
}

fn same_target(g: &GrantRow) -> bool {
    worker_identity(&g.target).is_ok_and(|id| id == g.target_incarnation)
}


fn not_applicable(state: &AppState, g: &GrantRow) -> Option<(&'static str, String)> {
    if !same_target(g) {
        return Some(("grant_target_identity_changed", format!("grant {} belongs to a different worker incarnation", g.code)));
    }
    if expire_if_due(state, g) {
        return Some(("grant_expired", format!("grant {} expired; ask again", g.code)));
    }
    (g.status != "pending").then(|| ("grant_not_pending", format!("grant {} is {}, not pending; it applies once", g.code, g.status)))
}

/// Reserve a typed approval's reply in the status transaction, shared with
/// cite mode's UNIQUE evidence_row constraint. Button approvals have no row.
fn claim_pending(state: &AppState, code: &str, by: &str, evidence: Option<i64>) -> Result<(), (&'static str, String)> {
    let (owned_code, by) = (code.to_string(), by.to_string());
    let at = now();
    let result = state.store.write(move |conn| {
        let binding: (Option<i64>, Option<i64>, Option<String>) = conn.query_row(
            "SELECT shown_row,resolution_row,actor_incarnation FROM _amux_delegation_grants WHERE code=?1", [&owned_code],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        if binding.0.is_none() || binding.2.is_none() || (evidence.is_some() && evidence != binding.1) {
            return Err(rusqlite::Error::InvalidParameterName("request_reply_mismatch".into()));
        }
        if let Some(reply) = evidence {
            let approval_reply_spent = super::max_ref::reply_spent(conn, reply)?;
            if approval_reply_spent {
                return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
                    std::io::Error::other("cite_pair_spent"),
                )));
            }
        }
        let n = conn.execute(
            "UPDATE _amux_delegation_grants SET status='applying', decided_by=?1, decided_at=?2, evidence_row=?3, \
             cite_spent_at=CASE WHEN ?3 IS NOT NULL THEN ?2 ELSE cite_spent_at END, \
             reply_text=(SELECT text FROM cmd_history WHERE id=?3),cited_excerpt=(SELECT text FROM cmd_history WHERE id=?3) WHERE code=?4 AND status='pending'",
            rusqlite::params![by, at, evidence, owned_code],
        )?;
        Ok(crate::db::WriteOutcome { applied: n == 1, events: vec![] })
    });
    match result {
        Ok(o) if o.applied => Ok(()),
        Ok(_) => Err(("grant_not_pending", format!("grant {code} was already taken"))),
        Err(e) => {
            let refusal = if e.to_string().contains("cite_pair_spent") { "cite_pair_spent" } else if e.to_string().contains("request_reply_mismatch") { "request_reply_mismatch" } else { "grant_claim_failed" };
            tracing::warn!(grant = code, evidence_row = ?evidence, error = %e, code = refusal,
                verdict = "delegation_grant_claim_refused", measured = true, n_considered = 1,
                "pending grant could not reserve owner evidence; no replay occurred");
            Err((refusal, format!("grant {code} could not reserve owner evidence: {e}")))
        }
    }
}

/// Apply a pending grant: exactly the stored request, once.
pub(crate) async fn apply(state: &AppState, code: &str, by: &str, evidence: Option<i64>) -> Result<Value, (&'static str, String)> {
    let g = load(state, code).ok_or(("grant_not_found", format!("no grant {code}")))?;
    validate_request_identity(state,&g,&g.actor)?;
    if let Some(why) = not_applicable(state, &g) {
        return Err(why);
    }
    claim_pending(state, &g.code, by, evidence)?;
    log_transition(state, &g.code, "approved", by, "owner", &g.target, json!({"sentence": g.sentence, "evidence_row": evidence}), None);
    let mut req = Request::builder()
        .method(g.method.as_str())
        .uri(g.path.parse::<Uri>().unwrap_or_else(|_| Uri::from_static("/")))
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .header("x-amux-session", g.actor.as_str())
        .header(GRANT_HEADER, g.code.as_str());
    if let Some(t) = state.auth_token.as_deref() {
        req = req.header(axum::http::header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let mut stored_body: Value = serde_json::from_str(&g.body).map_err(|_| ("request_change_invalid", "stored body is invalid".into()))?;
    stored_body["request_id"] = json!(g.code);
    let body = Body::from(stored_body.to_string());
    let req = match req.body(body) {
        Ok(r) => r,
        Err(e) => {
            let _ = set_status(state, &g.code, "applying", "failed", by, None);
            return Err(("grant_replay_invalid", e.to_string()));
        }
    };
    use tower::ServiceExt;
    let resp = Box::pin(super::router(state.clone()).oneshot(req)).await.map_err(|e| ("grant_replay_failed", e.to_string()))?;
    let status = resp.status();
    let out: Value = axum::body::to_bytes(resp.into_body(), 1 << 20).await.ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null);
    // The replay's own middleware finalized it; a replay that never reached
    // the gate (an early 4xx) is closed here.
    if set_status(state, &g.code, "applying", "failed", by, None) {
        log_transition(state, &g.code, "failed", &g.actor, &g.class, &g.target, json!({"status": status.as_u16(), "response": out}), None);
    }
    if status.is_success() { Ok(out) } else { Err(("grant_apply_failed", format!("the stored change answered {status}: {out}"))) }
}

/// `authorize`'s check on the replay of a pending grant: the request carries
/// an `applying` grant's code and is that grant's actor, action and target.
/// Arms the request so its middleware finalizes the grant.
pub(crate) fn claim_applying(state: &AppState, code: &str, actor: &str, action: &str, target: &str, change: &Value) -> Result<Option<i64>, String> {
    let g = load(state, code).ok_or_else(|| format!("no grant {code}"))?;
    if !same_target(&g) { return Err("grant_target_identity_changed".into()); }
    validate_requested(state, &g, actor, change).map_err(|(code, note)| format!("{code}: {note}"))?;
    if g.status != "applying" {
        return Err(format!("grant {code} is {}, not being applied", g.status));
    }
    if g.actor != actor || g.action != action || g.target != target {
        return Err(format!("grant {code} is for {} {} on {}, not {actor} {action} on {target}", g.actor, g.action, g.target));
    }
    if !arm(state, code) {
        return Err(format!("grant {code} could not be armed"));
    }
    Ok(g.evidence_row)
}

/// Every recorded human reply is correlated, even if it names no request.
/// Passkey mode additionally requires the direct Remote owner proof before replay.
pub(crate) async fn on_row(state: &AppState, row_id: i64) {
    if let Err(e) = super::delegation_requests::resolve_observed(state, row_id) {
        tracing::warn!(row_id, error = %e, verdict = "delegation_request_resolution_failed", measured = false, n_considered = 0, "reply correlation failed closed");
        return;
    }
    if mode() != Mode::Passkey || super::max_ref::row_qualifies(state, row_id).is_err() { return; }
    let code: Option<String> = state.store.read().ok().and_then(|conn| conn.query_row(
        "SELECT request_id FROM _amux_delegation_resolutions WHERE reply_row=?1", [row_id], |r| r.get(0)).ok()).flatten();
    if let Some(code) = code {
        if let Err((c, why)) = apply(state, &code, &format!("owner:sent:{row_id}"), Some(row_id)).await {
            tracing::warn!(row_id, request_id = %code, code = c, why, verdict = "delegation_grant_sent_not_applied", measured = true, n_considered = 1, "resolved approval did not apply");
        }
    }
}

fn owner_refusal(state: &AppState, headers: &HeaderMap, uri: &Uri) -> Option<Response> {
    if !super::session_verbs::worker_origin(headers).is_empty() {
        return Some(forbid("grant_owner_only", "only the owner decides a grant".into()));
    }
    if super::org::is_verified_local_member(headers) {
        return Some(forbid("grant_owner_only", "an invited member cannot decide a grant".into()));
    }
    if state.auth_token.is_some() && !super::auth::has_owner_token(state, headers, uri) {
        return Some(forbid("grant_owner_only", "send the owner bearer".into()));
    }
    None
}

/// The Approve button's precondition: the owner, with the approval proof.
fn approve_refusal(state: &AppState, headers: &HeaderMap, uri: &Uri) -> Option<Response> {
    if let Some(r) = owner_refusal(state, headers, uri) {
        return Some(r);
    }
    (!approval_proof_ok(headers)).then(|| forbid("grant_approve_requires_remote",
        "Approve needs the approval proof: the owner from a non-local Remote peer (the phone or another device on the tailnet). A forwarded verdict (tailscale serve on this Mac) is not proof, and neither is the owner token alone".into()))
}

fn decision_log(headers: &HeaderMap, verb: &str, code: &str) {
    let po = process_origin(headers);
    tracing::info!(grant = code, verb, process_origin = %po, forwarded = po == super::process_identity::REMOTE_FORWARDED,
        verdict = "delegation_grant_decision_origin", measured = true, n_considered = 1, "a grant decision request and its process origin");
}

async fn approve(State(state): State<AppState>, uri: Uri, headers: HeaderMap, Path(code): Path<String>) -> Response {
    decision_log(&headers, "approve", &code);
    if let Some(r) = approve_refusal(&state, &headers, &uri) {
        return r;
    }
    match apply(&state, &code, "owner:approve", None).await {
        Ok(out) => Json(json!({"ok": true, "grant_code": code.to_ascii_uppercase(), "applied": true, "response": out})).into_response(),
        Err((c, why)) => (StatusCode::CONFLICT, Json(super::delegation::refusal_body(c, &why))).into_response(),
    }
}

async fn deny(State(state): State<AppState>, uri: Uri, headers: HeaderMap, Path(code): Path<String>) -> Response {
    decision_log(&headers, "deny", &code);
    if let Some(r) = owner_refusal(&state, &headers, &uri) {
        return r;
    }
    let Some(g) = load(&state, &code) else {
        return (StatusCode::NOT_FOUND, Json(super::delegation::refusal_body("grant_not_found", &format!("no grant {code}")))).into_response();
    };
    if let Some((c, why)) = not_applicable(&state, &g) {
        return (StatusCode::CONFLICT, Json(super::delegation::refusal_body(c, &why))).into_response();
    }
    if !set_status(&state, &g.code, "pending", "denied", "owner", None) {
        return (StatusCode::CONFLICT, Json(super::delegation::refusal_body("grant_not_pending", "already decided"))).into_response();
    }
    log_transition(&state, &g.code, "denied", "owner", "owner", &g.target, json!({"sentence": g.sentence}), None);
    Json(json!({"ok": true, "grant_code": g.code, "denied": true})).into_response()
}

/// Restore every changed key/file/row of an applied grant, when each still
/// reads as the grant left it.
/// Returns how many items came back, and whether every env file came back
/// byte for byte.
fn restore(state: &AppState, changes: &Value) -> Result<(usize, bool), String> {
    let home = crate::config::amux_home();
    let now_snap = snapshot(state);
    let mut conflicts = Vec::new();
    let empty = BTreeMap::new();
    for (f, keys) in changes["env"].as_object().into_iter().flatten() {
        let cur = now_snap.env.get(f).unwrap_or(&empty);
        for (k, ba) in keys.as_object().into_iter().flatten() {
            if ba.get("secret").is_some() {
                return Err(format!("{f}:{k} is a secret value; it was never stored, so this grant cannot be undone"));
            }
            if cur.get(k).map(|s| Value::String(s.clone())).unwrap_or(Value::Null) != ba["after"] {
                conflicts.push(format!("{f}:{k}"));
            }
        }
    }
    for (f, ba) in changes["text"].as_object().into_iter().flatten() {
        if now_snap.text.get(f).map(|s| Value::String(s.clone())).unwrap_or(Value::Null) != ba["after"] {
            conflicts.push(f.clone());
        }
    }
    let empty_t = BTreeMap::new();
    for (t, rows) in changes["tables"].as_object().into_iter().flatten() {
        if !TABLES.iter().any(|(n, _)| n == t) {
            return Err(format!("{t} is not a table undo restores"));
        }
        let cur = now_snap.tables.get(t).unwrap_or(&empty_t);
        for (k, ba) in rows.as_object().into_iter().flatten() {
            if ba.get("secret").is_some() {
                return Err(format!("{t}:{k} is a secret value; it was never stored, so this grant cannot be undone"));
            }
            if cur.get(k).cloned().map(Value::Object).unwrap_or(Value::Null) != ba["after"] {
                conflicts.push(format!("{t}:{k}"));
            }
        }
    }
    if !conflicts.is_empty() {
        return Err(format!("changed since the grant applied: {}; undo by hand", conflicts.join(", ")));
    }
    let mut n = 0usize;
    let mut exact = true;
    for (f, keys) in changes["env"].as_object().into_iter().flatten() {
        let updates: Vec<(String, Option<String>)> = keys.as_object().into_iter().flatten()
            .map(|(k, ba)| (k.clone(), ba["before"].as_str().map(str::to_string))).collect();
        n += updates.len();
        let p = home.join(f);
        let raw = &changes["env_raw"][f];
        // Untouched since the grant: write the before bytes back exactly.
        if raw.is_object() && now_snap.env_raw.get(f).map(String::as_str) == raw["after"].as_str() {
            match raw["before"].as_str() {
                Some(t) => super::session_verbs::EnvFile::replace_text(&p, t).map_err(|e| format!("{f}: {e}"))?,
                None => std::fs::remove_file(&p).map_err(|e| format!("{f}: {e}"))?,
            }
            continue;
        }
        exact = false;
        super::session_verbs::EnvFile::merge_plain(&p, &updates).map_err(|e| format!("{f}: {e}"))?;
    }
    for f in changes["created"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        let p = home.join(f);
        if config_pairs(&p).is_empty() {
            let _ = std::fs::remove_file(&p);
        }
    }
    for (f, ba) in changes["text"].as_object().into_iter().flatten() {
        let p = home.join(f);
        match ba["before"].as_str() {
            Some(t) => std::fs::write(&p, t).map_err(|e| format!("{f}: {e}"))?,
            None => std::fs::remove_file(&p).map_err(|e| format!("{f}: {e}"))?,
        }
        n += 1;
    }
    let table_changes = changes["tables"].clone();
    let rows_restored = state.store.write(move |conn| {
        let mut n = 0usize;
        for (t, keys) in TABLES {
            for ba in table_changes[*t].as_object().into_iter().flatten().map(|(_, ba)| ba) {
                n += restore_row(conn, t, keys, &ba["before"], &ba["after"])?;
            }
        }
        Ok(crate::db::WriteOutcome { applied: n > 0, events: vec![] })
    }).map_err(|e| e.to_string())?;
    if rows_restored.applied {
        n += changes["tables"].as_object().into_iter().flatten().map(|(_, r)| r.as_object().map_or(0, |o| o.len())).sum::<usize>();
    }
    Ok((n, exact))
}

/// Put one row back: a row the grant created is deleted, one it deleted is
/// re-inserted whole, and one it changed gets an UPDATE of exactly the
/// columns that differ, so a column the grant never touched is never written.
fn restore_row(conn: &rusqlite::Connection, table: &str, keys: &[&str], before: &Value, after: &Value) -> rusqlite::Result<usize> {
    let ident = |c: &str| format!("\"{}\"", c.replace('"', "\"\""));
    let key_of = |row: &Value| -> Vec<rusqlite::types::Value> { keys.iter().map(|k| to_sql(&row[*k])).collect() };
    let where_keys = |from: usize| keys.iter().enumerate().map(|(i, k)| format!("{}=?{}", ident(k), from + i)).collect::<Vec<_>>().join(" AND ");
    match (before.as_object(), after.as_object()) {
        (None, Some(_)) => {
            let sql = format!("DELETE FROM {table} WHERE {}", where_keys(1));
            conn.execute(&sql, rusqlite::params_from_iter(key_of(after)))
        }
        (Some(b), None) => {
            let cols: Vec<&String> = b.keys().collect();
            let sql = format!("INSERT INTO {table} ({}) VALUES ({})", cols.iter().map(|c| ident(c)).collect::<Vec<_>>().join(","),
                (1..=cols.len()).map(|i| format!("?{i}")).collect::<Vec<_>>().join(","));
            conn.execute(&sql, rusqlite::params_from_iter(cols.iter().map(|c| to_sql(&b[*c]))))
        }
        (Some(b), Some(a)) => {
            let changed: Vec<&String> = b.keys().chain(a.keys()).collect::<std::collections::BTreeSet<_>>().into_iter()
                .filter(|c| b.get(*c) != a.get(*c)).collect();
            if changed.is_empty() {
                return Ok(0);
            }
            let sets = changed.iter().enumerate().map(|(i, c)| format!("{}=?{}", ident(c), i + 1)).collect::<Vec<_>>().join(", ");
            let sql = format!("UPDATE {table} SET {sets} WHERE {}", where_keys(changed.len() + 1));
            let mut params: Vec<rusqlite::types::Value> = changed.iter().map(|c| to_sql(b.get(*c).unwrap_or(&Value::Null))).collect();
            params.extend(key_of(before));
            conn.execute(&sql, rusqlite::params_from_iter(params))
        }
        (None, None) => Ok(0),
    }
}

/// `POST /api/delegation/grants/<code>/undo`: the owner, or mrpopo.
async fn undo(State(state): State<AppState>, uri: Uri, headers: HeaderMap, Path(code): Path<String>) -> Response {
    decision_log(&headers, "undo", &code);
    let origin = super::delegation::verified_origin(&headers);
    let by = if origin.is_empty() {
        if let Some(r) = owner_refusal(&state, &headers, &uri) {
            return r;
        }
        "owner".to_string()
    } else if origin == super::delegation::MRPOPO {
        origin.clone()
    } else {
        return forbid("grant_undo_owner_or_mrpopo", format!("'{origin}' may not undo a grant; the owner or mrpopo may"));
    };
    let Some(g) = load(&state, &code) else {
        return (StatusCode::NOT_FOUND, Json(super::delegation::refusal_body("grant_not_found", &format!("no grant {code}")))).into_response();
    };
    if g.status != "applied" {
        return (StatusCode::CONFLICT, Json(super::delegation::refusal_body("grant_not_applied", &format!("grant {} is {}; only an applied grant is undone", g.code, g.status)))).into_response();
    }
    // Reversal checks the original target. A retired asking worker must not
    // strand the owner's undo; it is not funding a new change.
    if !same_target(&g) {
        return forbid("grant_target_identity_changed", "undo target has a different worker incarnation".into());
    }
    let changes: Value = g.changes.as_deref().and_then(|c| serde_json::from_str(c).ok()).unwrap_or(Value::Null);
    match restore(&state, &changes) {
        // Review B2: an undo that restored nothing is not an undo. The grant
        // stays `applied` and the caller hears why, instead of
        // {undone:true, restored:0} over a change that is still live.
        Ok((0, _)) => {
            tracing::warn!(grant = %g.code, verdict = "delegation_grant_undo_nothing_restorable", measured = true, n_considered = 0,
                "an undo found no recorded before-state to restore; the grant stays applied");
            (StatusCode::CONFLICT, Json(super::delegation::refusal_body("grant_undo_nothing_restorable", &format!(
                "grant {} recorded no before-state that undo can restore; it stays applied. Undo it by hand: {}", g.code, g.sentence)))).into_response()
        }
        Ok((n, byte_exact)) => {
            if !set_status(&state, &g.code, "applied", "undone", &by, None) {
                return (StatusCode::CONFLICT, Json(super::delegation::refusal_body("grant_not_applied", "already undone"))).into_response();
            }
            if !byte_exact {
                tracing::info!(grant = %g.code, verdict = "delegation_grant_undo_keys_only", measured = true, n_considered = n,
                    "an env file was restored key by key: it changed elsewhere since, or holds a secret whose bytes a grant never stores");
            }
            log_transition(&state, &g.code, "undone", &by, if by == "owner" { "owner" } else { "mrpopo" }, &g.target,
                json!({"sentence": g.sentence, "restored": n, "byte_exact": byte_exact, "changes": changes}), None);
            Json(json!({"ok": true, "grant_code": g.code, "undone": true, "restored": n, "byte_exact": byte_exact})).into_response()
        }
        Err(why) => (StatusCode::CONFLICT, Json(super::delegation::refusal_body("grant_undo_conflict", &why))).into_response(),
    }
}

fn request_view(g: &GrantRow) -> Value {
        let status = if g.status == "pending" && now() > g.expires_at { "expired" } else { g.status.as_str() };
        json!({"code": g.code, "actor": g.actor, "class": g.class, "action": g.action, "target": g.target, "sentence": g.sentence,
            "status": status, "failure_reason": failure_reason(status), "expires_at": g.expires_at, "method": g.method, "path": g.path, "cited_row": g.evidence_row,
            "target_incarnation": g.target_incarnation, "cite_spent_at": g.cite_spent_at,
            "request_id": g.code, "created_at":g.created_at, "actor_incarnation":g.actor_incarnation, "reply_session":g.reply_session, "reply_incarnation":g.reply_incarnation,
            "shown_row":g.shown_row, "change":g.change_spec.as_deref().and_then(|s|serde_json::from_str::<Value>(s).ok()),
            "resolution_row":g.resolution_row,"resolution_code":g.resolution_code,"resolution_note":g.resolution_note,
            "cited_excerpt": g.cited_excerpt, "ask_row": g.ask_row, "ask_text": g.ask_text, "reply_text": g.reply_text, "body": serde_json::from_str::<Value>(&g.body).unwrap_or(Value::Null)})
}

async fn read_request(State(state): State<AppState>, uri: Uri, headers: HeaderMap, Path(code): Path<String>) -> Response {
    if let Some(r) = super::delegation_log::read_refusal(&state, &headers, &uri) { return r; }
    let Some(g) = load(&state, &code) else { return forbid("request_not_found", "unknown request ID".into()); };
    if let Some(session) = g.reply_session.as_deref() {
        if let Err(e) = super::delegation_requests::resolve_session(&state, session) { return forbid("request_resolution_failed", e); }
    }
    match load(&state, &code) {
        Some(g) => Json(json!({"request":request_view(&g),"measured":true,"n_considered":1})).into_response(),
        None => forbid("request_not_found", "unknown request ID".into()),
    }
}

/// `GET /api/delegation/grants`: newest first; readers as the delegation log.
async fn list(State(state): State<AppState>, uri: Uri, headers: HeaderMap) -> Response {
    if let Some(r) = super::delegation_log::read_refusal(&state, &headers, &uri) {
        return r;
    }
    let mut rows = match list_rows(&state) {
        Ok(rows) => rows,
        Err(e) => return forbid("request_read_failed", e),
    };
    let sessions: std::collections::BTreeSet<_> = rows.iter().filter(|g|g.status=="pending").filter_map(|g|g.reply_session.clone()).collect();
    for session in sessions {
        if let Err(e) = super::delegation_requests::resolve_session(&state,&session) { return forbid("request_resolution_failed",e); }
    }
    rows = match list_rows(&state) { Ok(rows) => rows, Err(e) => return forbid("request_read_failed",e) };
    for g in &rows { expire_if_due(&state,g); }
    let rows: Vec<Value> = rows.iter().map(request_view).collect();
    let resolutions = match resolution_views(&state) {
        Ok(rows) => rows,
        Err(e) => return forbid("request_resolution_read_failed", e),
    };
    Json(json!({"requests": rows, "grants": rows, "resolutions":resolutions, "mode": mode_str(mode()), "measured": true, "n_considered": rows.len()+resolutions.len()})).into_response()
}

fn list_rows(state: &AppState) -> Result<Vec<GrantRow>, String> {
    let conn = state.store.read().map_err(|e|e.to_string())?;
    let mut st = conn.prepare(&format!("SELECT {COLS} FROM _amux_delegation_grants ORDER BY created_at DESC, rowid DESC LIMIT ?1")).map_err(|e|e.to_string())?;
    let rows = st.query_map([LIST_LIMIT_ROWS],map_row).map_err(|e|e.to_string())?;
    rows.collect::<rusqlite::Result<_>>().map_err(|e|e.to_string())
}

fn resolution_views(state: &AppState) -> Result<Vec<Value>, String> {
    let conn = state.store.read().map_err(|e|e.to_string())?;
    let mut st = conn.prepare("SELECT reply_row,request_id,session,code,note FROM _amux_delegation_resolutions ORDER BY reply_row DESC LIMIT ?1").map_err(|e|e.to_string())?;
    let rows = st.query_map([LIST_LIMIT_ROWS], |r| Ok(json!({"reply_row":r.get::<_,i64>(0)?,"request_id":r.get::<_,Option<String>>(1)?,"session":r.get::<_,String>(2)?,"code":r.get::<_,String>(3)?,"note":r.get::<_,String>(4)?}))).map_err(|e|e.to_string())?;
    rows.collect::<rusqlite::Result<_>>().map_err(|e|e.to_string())
}

/// `GET /api/delegation/whoami`: the caller as the server sees it (process
/// origin, forwarded or direct, worker identity), so Max can check in one click.
async fn whoami(headers: HeaderMap) -> Response {
    let po = process_origin(&headers).to_string();
    let worker = super::delegation::verified_origin(&headers);
    tracing::info!(process_origin = %po, worker = %worker, verdict = "delegation_whoami", measured = true, n_considered = 1,
        "whoami: the caller's process origin");
    Json(json!({
        "process_origin": if po.is_empty() { Value::Null } else { json!(po) },
        "forwarded": po == super::process_identity::REMOTE_FORWARDED,
        "approval_proof": approval_proof_ok(&headers),
        "worker": if worker.is_empty() { Value::Null } else { json!(worker) },
        "mode": mode_str(mode()),
        "measured": true, "n_considered": 1,
    })).into_response()
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/delegation/grants", get(list))
        .route("/api/delegation/requests", get(list).post(super::delegation_requests::create))
        .route("/api/delegation/requests/{code}", get(read_request))
        .route("/api/delegation/questions/render", post(super::delegation_requests::prepare_display))
        .route("/api/delegation/questions/{ask}/display", post(super::delegation_requests::display))
        .route("/api/delegation/whoami", get(whoami))
        .route("/api/delegation/grants/{code}/approve", post(approve))
        .route("/api/delegation/grants/{code}/deny", post(deny))
        .route("/api/delegation/grants/{code}/undo", post(undo))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;

    fn context(body: Value) -> Captured {
        Captured {
            method: "PATCH".into(), path: "/api/sessions/test-dev-a/config".into(), body,
            armed: Arc::new(Mutex::new(None)), checked_env: Arc::new(Mutex::new(BTreeMap::new())),
            compare_failed: Arc::new(Mutex::new(false)),
        }
    }

    #[tokio::test]
    async fn delegation_citation_unique_constraint_prevents_second_grant() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = crate::api::settings::test_env::set_home(tmp.path());
        let state = test_state(tmp.path());
        let conn = state.store.read().unwrap();
        assert!(conn.prepare("SELECT cite_spent_at,target_incarnation FROM _amux_delegation_grants").is_ok());
        drop(conn);
        let insert = |code: &str, ask: Option<i64>, evidence: Option<i64>| {
            let code = code.to_string();
            state.store.write(move |c| {
                c.execute("INSERT INTO _amux_delegation_grants(code,actor,class,action,target,method,path,body,sentence,created_at,expires_at,status,ask_row,evidence_row,cite_spent_at) VALUES(?1,'mrpopo','mrpopo','x','fleet','PUT','/api/scope','{}','change',1,2,'applying',?2,?3,1)",rusqlite::params![code, ask, evidence])?;
                Ok(crate::db::WriteOutcome{applied:true,events:vec![]})
            })
        };
        insert("ABCD", Some(11), Some(12)).unwrap();
        for ask in [Some(13), None] {
            let err = insert("EFGH", ask, Some(12)).err().expect("a different ask or passkey grant must violate reply uniqueness");
            assert!(err.to_string().contains("UNIQUE constraint failed"),"{err}");
        }
        let conn = state.store.read().unwrap();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM _amux_delegation_grants",[],|r|r.get(0)).unwrap();
        assert_eq!(count,1);
        drop(conn);
        insert("IJKM", None, None).unwrap();
        let err = state.store.write(|c| {
            c.execute("UPDATE _amux_delegation_grants SET evidence_row=12 WHERE code='IJKM'", [])?;
            Ok(crate::db::WriteOutcome { applied: true, events: vec![] })
        }).err().expect("a passkey UPDATE must obey the same unique reply constraint");
        assert!(err.to_string().contains("UNIQUE constraint failed"), "{err}");
    }

    #[tokio::test]
    async fn delegation_env_apply_holds_config_lock_across_handler_awaits() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = crate::api::settings::test_env::set_home(tmp.path());
        let state = test_state(tmp.path());
        // Probe the actual capture middleware at both create routes. This is
        // deterministic: try_lock observes ownership, with no sleeps/races.
        let probe = || async {
            assert!(CONFIG_MUTATION.try_lock().is_err(), "exists-check must run under CONFIG_MUTATION");
            tokio::task::yield_now().await;
            assert!(CONFIG_MUTATION.try_lock().is_err(), "write must retain CONFIG_MUTATION across awaits");
            Json(json!({"ok": true}))
        };
        let app = Router::new()
            .route("/api/env/apply", post(probe))
            .route("/api/sessions", post(probe))
            .route("/api/sessions/test-dev-a/config", post(probe))
            .layer(axum::middleware::from_fn_with_state(state, capture));
        for path in ["/api/env/apply", "/api/sessions", "/api/sessions/test-dev-a/config"] {
            let (status, body, _) = call(&app, "POST", path, None, json!({}), None).await;
            assert!(status.is_success(), "{body}");
        }
    }

    /// Raw env replacement is owner-only, including with a cite. Preserve
    /// that gate and prove its underlying writer has the same atomic failure.
    #[tokio::test]
    async fn delegation_owner_raw_env_failure_preserves_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = crate::api::settings::test_env::set_home(tmp.path());
        let state = test_state(tmp.path());
        let app = super::super::router(state);
        let path = tmp.path().join("sessions/test-dev-a.env");
        let before = std::fs::read(&path).unwrap();
        let (st, v, _) = super::super::session_verbs::TEST_ENV_RENAME_FAILURE.scope(path.clone(),
            call(&app, "PUT", "/api/scope", None, json!({"level": "worker", "name": "test-dev-a",
                "capability": "env", "value": "CC_TAGS=ops\nCC_DIR=/tmp\nCC_INCARNATION=inc-test-dev-a\n"}), None)).await;
        assert_eq!(st, StatusCode::INTERNAL_SERVER_ERROR, "{v}");
        assert_eq!(std::fs::read(&path).unwrap(), before, "replace_text must not partially replace owner env");
    }

    #[tokio::test]
    async fn delegation_env_cas_preserves_concurrent_owner_change() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = crate::api::settings::test_env::set_home(tmp.path());
        let path = tmp.path().join("sessions/test-dev-a.env");
        let _state = test_state(tmp.path());
        // All EnvFile write entrypoints must share the comparison boundary.
        for writer in ["write", "merge_plain", "replace_text"] {
            let mut env = super::super::session_verbs::EnvFile::load(&path);
            env.set("CC_TAGS","dev,ops");
            env.write(&path).unwrap();
            let c = context(json!({"tags":"ops"}));
            let failed = c.compare_failed.clone();
            CAPTURED.scope(c, async {
                expect_env(&path);
                // Another task has no inherited task-local authorization.
                // It is a real owner write through the same locked writer.
                let owner_path = path.clone();
                tokio::spawn(async move {
                    let mut owner = super::super::session_verbs::EnvFile::load(&owner_path);
                    owner.set("CC_TAGS","dev");
                    owner.write(&owner_path).unwrap();
                }).await.unwrap();
                let owner_bytes = std::fs::read(&path).unwrap();
                let result = match writer {
                    "write" => { env.set("CC_TAGS","ops"); env.write(&path) },
                    "merge_plain" => super::super::session_verbs::EnvFile::merge_plain(&path,&[("CC_TAGS".into(),Some("ops".into()))]),
                    "replace_text" => super::super::session_verbs::EnvFile::replace_text(&path,"CC_TAGS=ops\n"),
                    _ => unreachable!(),
                };
                assert_eq!(result.unwrap_err().to_string(),"delegation_state_changed","{writer}");
                assert!(*failed.lock().unwrap());
                assert_eq!(std::fs::read(&path).unwrap(),owner_bytes,"{writer} overwrote concurrent owner state");
            }).await;
        }
    }

    #[tokio::test]
    async fn delegation_env_cas_merges_unrelated_keys_but_refuses_touched_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = crate::api::settings::test_env::set_home(tmp.path());
        let state = test_state(tmp.path());
        for touched in [false, true] {
            let path = tmp.path().join("sessions/test-dev-a.env");
            let handler_state = state.clone();
            let owner_path = path.clone();
            let app = Router::new().route("/api/sessions/test-dev-a/config", axum::routing::patch(move || {
                let state = handler_state.clone();
                let path = owner_path.clone();
                async move {
                    expect_env(&path);
                    let owner_path = path.clone();
                    tokio::spawn(async move {
                        let mut env = super::super::session_verbs::EnvFile::load(&owner_path);
                        env.set(if touched { "CC_TAGS" } else { "CC_DESC" }, if touched { "dev,owner" } else { "owner description" });
                        env.write(&owner_path).unwrap();
                    }).await.unwrap();
                    super::super::session_verbs::config_patch(&state,"test-dev-a",&json!({"tags":"ops"})).await
                }
            })).layer(axum::middleware::from_fn_with_state(state.clone(),capture));
            let (st,v,_) = call(&app,"PATCH","/api/sessions/test-dev-a/config",None,json!({"tags":"ops"}),None).await;
            let env = super::super::session_verbs::EnvFile::load(&path);
            if touched {
                assert_eq!(st, StatusCode::CONFLICT, "{v}");
                assert_eq!(v["code"], "delegation_state_changed");
                assert_eq!(env.get("CC_TAGS"), Some("dev,owner"));
            } else {
                assert!(st.is_success(), "unrelated write must merge: {v}");
                assert_eq!(env.get("CC_TAGS"), Some("ops"));
                assert_eq!(env.get("CC_DESC"), Some("owner description"));
            }
        }
        // Scope's merge_plain has the same touched-key comparison and reload.
        let path = tmp.path().join("sessions/test-dev-a.env");
        CAPTURED.scope(context(json!({})), async {
            expect_env(&path);
            let owner_path = path.clone();
            tokio::spawn(async move {
                let mut env = super::super::session_verbs::EnvFile::load(&owner_path);
                env.set("CC_DESC", "second owner description");
                env.write(&owner_path).unwrap();
            }).await.unwrap();
            super::super::session_verbs::EnvFile::merge_plain(&path, &[("CC_TAGS".into(),Some("ops".into()))]).unwrap();
            let env = super::super::session_verbs::EnvFile::load(&path);
            assert_eq!(env.get("CC_DESC"), Some("second owner description"));
            assert_eq!(env.get("CC_TAGS"), Some("ops"));
        }).await;
        // Re-creating the worker changes identity even if the touched value
        // has not changed. The baseline must still refuse that new holder.
        CAPTURED.scope(context(json!({})), async {
            expect_env(&path);
            let raw = std::fs::read_to_string(&path).unwrap();
            assert!(raw.contains("inc-test-dev-a"));
            std::fs::write(&path,raw.replace("inc-test-dev-a","replacement-worker")).unwrap();
            let mut stale = super::super::session_verbs::EnvFile::load(&path);
            stale.set("CC_TAGS","ops");
            assert_eq!(stale.write(&path).unwrap_err().to_string(),"delegation_state_changed");
        }).await;
    }

    #[tokio::test]
    async fn delegation_env_cas_reports_conflict_at_http_boundary() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = crate::api::settings::test_env::set_home(tmp.path());
        let state = test_state(tmp.path());
        let path = tmp.path().join("sessions/test-dev-a.env");
        let handler_state = state.clone();
        let owner_path = path.clone();
        let app = Router::new().route("/api/sessions/test-dev-a/config", axum::routing::patch(move || {
            let state = handler_state.clone();
            let path = owner_path.clone();
            async move {
                expect_env(&path);
                tokio::spawn(async move {
                    let mut env = super::super::session_verbs::EnvFile::load(&path);
                    env.set("CC_TAGS","dev,owner-choice");
                    env.write(&path).unwrap();
                }).await.unwrap();
                super::super::session_verbs::config_patch(&state,"test-dev-a",&json!({"tags":"ops"})).await
            }
        })).layer(axum::middleware::from_fn_with_state(state,capture));
        let (st,v,_) = call(&app,"PATCH","/api/sessions/test-dev-a/config",None,json!({"tags":"ops"}),None).await;
        assert_eq!(st,StatusCode::CONFLICT,"{v}");
        assert_eq!(v["code"],"delegation_state_changed");
        assert!(std::fs::read_to_string(path).unwrap().contains("dev,owner-choice"));
    }

    #[test]
    fn delegation_tags_normalize_array_and_string_with_one_grammar() {
        let wanted = vec!["dev".to_string(),"ops".to_string()];
        for value in [json!([" OPS ","dev,DEV",""]),json!(" OPS , dev,DEV,")] {
            assert_eq!(super::super::session_verbs::tags_of(Some(&value)).unwrap(),wanted);
        }
        for invalid in [json!(["dev",7]),json!({"dev":true}),json!(7)] {
            assert!(super::super::session_verbs::tags_of(Some(&invalid)).is_err(),"{invalid}");
        }
    }

    #[test]
    fn a_code_uses_only_the_unambiguous_alphabet() {
        for _ in 0..50 {
            let c = new_code().unwrap();
            assert_eq!(c.len(), CODE_LEN);
            assert!(c.bytes().all(|b| ALPHABET.contains(&b)), "{c}");
        }
    }

    fn test_state(dir: &std::path::Path) -> AppState {
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        std::fs::write(dir.join("sessions/test-dev-a.env"), "CC_DIR=\"/tmp\"\nCC_TAGS=\"dev\"\nCC_INCARNATION=\"inc-test-dev-a\"\n").unwrap();
        std::fs::write(dir.join("sessions/mrpopo.env"), "CC_DIR=\"/tmp\"\nCC_TAGS=\"leads\"\nCC_INCARNATION=\"inc-mrpopo\"\n").unwrap();
        std::fs::write(dir.join("sessions/test-dev-lead.env"), "CC_DIR=\"/tmp\"\nCC_TAGS=\"leads,dev\"\nCC_ORCHESTRATOR=\"1\"\nCC_INCARNATION=\"inc-test-dev-lead\"\n").unwrap();
        std::fs::write(dir.join("sessions/test-desk.env"), "CC_INCARNATION=inc-test-desk\n").unwrap();
        AppState {
            store: std::sync::Arc::new(crate::db::Store::open(&dir.join("t.db")).unwrap()),
            started: std::time::Instant::now(),
            build_hash: "test".into(),
            auth_token: None,
            reconciled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }

    async fn call(app: &axum::Router, method: &str, path: &str, who: Option<&str>, body: Value, port: Option<u16>) -> (StatusCode, Value, HeaderMap) {
        let mut b = Request::builder().method(method).uri(path).header(axum::http::header::CONTENT_TYPE, "application/json");
        if let Some(w) = who {
            b = b.header("x-amux-session", w);
        }
        let mut req = b.body(if body.is_null() { Body::empty() } else { Body::from(body.to_string()) }).unwrap();
        if let Some(p) = port {
            req.extensions_mut().insert(axum::extract::ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], p))));
        }
        let res = app.clone().oneshot(req).await.unwrap();
        let (st, h) = (res.status(), res.headers().clone());
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        (st, serde_json::from_slice(&bytes).unwrap_or(Value::Null), h)
    }

    /// Review B2: the table restore covers the kinds mrpopo can no longer
    /// reach after B3 (instance apply and global gates write `statuses` and
    /// `prefs` skins) and puts back only the columns that changed.
    #[test]
    fn a_table_change_is_restored_by_update_of_the_changed_columns() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = crate::api::settings::test_env::set_home(tmp.path());
        let state = test_state(tmp.path());
        state.store.write(|c| {
            c.execute("INSERT INTO statuses (id, label, position, is_builtin, gate) VALUES ('review-col','Review',5,0,'')", [])?;
            c.execute("INSERT INTO prefs (key, value) VALUES ('skin:global', '{\"a\":1}')", [])?;
            Ok(crate::db::WriteOutcome { applied: true, events: vec![] })
        }).unwrap();
        let before = snapshot(&state);
        state.store.write(|c| {
            c.execute("UPDATE statuses SET label='Renamed', gate='human', gate_custom=1 WHERE id='review-col'", [])?;
            c.execute("UPDATE prefs SET value='{\"a\":2}' WHERE key='skin:global'", [])?;
            c.execute("INSERT INTO prefs (key, value) VALUES ('skin:group:dev', '{}')", [])?;
            Ok(crate::db::WriteOutcome { applied: true, events: vec![] })
        }).unwrap();
        let (changes, undoable) = diff(&before, &snapshot(&state));
        assert!(undoable);
        assert_eq!(changes["tables"]["prefs"].as_object().map(|o| o.len()), Some(2), "{changes}");
        assert_eq!(restore(&state, &changes).unwrap().0, 3, "one statuses row and two prefs rows");
        assert_eq!(snapshot(&state).tables, before.tables, "every table row reads as before");
        // An empty change restores nothing: undo must refuse, not report success.
        assert_eq!(restore(&state, &json!({"env": {}, "text": {}, "tables": {}, "created": []})).unwrap().0, 0);
    }

    /// whoami reports each verdict, forwarded included.
    async fn explicit_request(app: &Router, state: &AppState, body: Value) -> String {
        let (st,v,_) = call(app,"POST","/api/delegation/requests",Some("mrpopo"),json!({
            "method":"PATCH","path":"/api/sessions/test-dev-a/config","body":body}),None).await;
        assert_eq!(st,StatusCode::ACCEPTED,"{v}");
        let id = v["request_id"].as_str().unwrap().to_string();
        let grant = load(state, &id).unwrap();
        let mut prepare = Request::builder().method("POST").uri("/api/delegation/questions/render")
            .header(axum::http::header::CONTENT_TYPE,"application/json")
            .body(Body::from(json!({"surface":"owner-messages"}).to_string())).unwrap();
        prepare.extensions_mut().insert(axum::extract::ConnectInfo(std::net::SocketAddr::from(([192,0,2,1],41001))));
        let res=app.clone().oneshot(prepare).await.unwrap(); assert!(res.status().is_success());
        let bytes=axum::body::to_bytes(res.into_body(),usize::MAX).await.unwrap();
        let after_row=serde_json::from_slice::<Value>(&bytes).unwrap()["after_row"].as_i64().unwrap();
        let mut req = Request::builder().method("POST").uri(format!("/api/delegation/questions/{}/display",grant.ask_row.unwrap()))
            .header(axum::http::header::CONTENT_TYPE,"application/json")
            .body(Body::from(json!({"surface":"owner-messages","text":grant.ask_text,"after_row":after_row}).to_string())).unwrap();
        req.extensions_mut().insert(axum::extract::ConnectInfo(std::net::SocketAddr::from(([192,0,2,1],41001))));
        let res = app.clone().oneshot(req).await.unwrap();
        assert!(res.status().is_success(),"fixture must display request before replying");
        id
    }

    #[tokio::test]
    async fn explicit_request_draft_uses_a_distinct_receipt_and_preserves_conflict_checks() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = crate::api::settings::test_env::set_home(tmp.path());
        let state = test_state(tmp.path());
        let before = snapshot(&state);
        let app = super::super::router(state.clone());
        let parent_id = "test-explicit-request";
        let path = "/api/sessions/test-dev-a/config";
        let req = Request::builder().method("POST").uri("/api/delegation/requests")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .header("x-amux-session", "mrpopo")
            .header("x-amux-interaction-id", parent_id)
            .header("x-amux-command-kind", "environment.post")
            .body(Body::from(json!({ "method":"PATCH", "path":path,
                "body":{"worktree_verify":"one"}}).to_string())).unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        assert_eq!(res.headers()["x-amux-interaction-id"], parent_id);
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        let request_id = body["request_id"].as_str().unwrap();
        let grant = load(&state, request_id).unwrap();
        assert_eq!(grant.actor, "mrpopo");
        assert_eq!(grant.status, "pending");
        assert_eq!(snapshot(&state), before, "derivation never executes the change");
        {
            let conn = state.store.read().unwrap();
            let mut stmt = conn.prepare("SELECT id,command_kind,method,path,actor FROM _amux_interactions ORDER BY path").unwrap();
            let receipts = stmt.query_map([], |r| Ok((r.get::<_,String>(0)?, r.get::<_,String>(1)?,
                r.get::<_,String>(2)?, r.get::<_,String>(3)?, r.get::<_,String>(4)?)))
                .unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
            assert_eq!(receipts.len(), 2, "parent and draft both have durable receipts");
            assert_eq!((receipts[0].0.as_str(), receipts[0].1.as_str(), receipts[0].2.as_str(),
                receipts[0].3.as_str(), receipts[0].4.as_str()),
                (parent_id, "environment.post", "POST", "/api/delegation/requests", "mrpopo"));
            assert_ne!(receipts[1].0, parent_id);
            assert_eq!((receipts[1].1.as_str(), receipts[1].2.as_str(), receipts[1].3.as_str(), receipts[1].4.as_str()),
                ("worker.patch", "PATCH", path, "mrpopo"));
        }
        // Reusing that ID on a genuinely different command must still fail.
        let req = Request::builder().method("PATCH").uri(path)
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .header("x-amux-session", "mrpopo")
            .header("x-amux-interaction-id", parent_id)
            .body(Body::from(json!({"worktree_verify":"one"}).to_string())).unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::CONFLICT);
        assert_eq!(snapshot(&state), before);
        assert_eq!(load(&state, request_id).unwrap().status, "pending");
    }

    #[test]
    fn owner_input_process_proof_excludes_workers_and_invited_members() {
        use super::super::session_verbs::owner_input_process_origin;
        let mut headers=HeaderMap::new();
        headers.insert(super::super::process_identity::PROCESS_ORIGIN_HEADER,"remote".parse().unwrap());
        assert_eq!(owner_input_process_origin(&headers),Some("remote"));
        headers.insert("x-amux-session","test-peer".parse().unwrap());
        assert_eq!(owner_input_process_origin(&headers),None);
        headers.remove("x-amux-session");
        headers.insert("x-amux-local-member-verified","1".parse().unwrap());
        assert_eq!(owner_input_process_origin(&headers),None,"even a missing member actor must fail closed");
    }

    #[tokio::test]
    async fn owner_input_recorder_resolves_send_chat_and_queued_replies_once() {
        use super::super::session_verbs::{cmd_hist_record_with_id, DeliveryMeta};
        let tmp=tempfile::tempdir().unwrap();
        let _home=crate::api::settings::test_env::set_home(tmp.path());
        let state=test_state(tmp.path());
        let app=super::super::router(state.clone());
        for (ctype,queued,proof) in [
            ("user",false,"outside"), ("user",true,"remote"),
            ("user",true,"outside"), ("user",false,"remote-forwarded"),
            ("chat",false,"outside"), ("chat",false,"remote"),
        ] {
            let id=explicit_request(&app,&state,json!({"worktree_verify":"one"})).await;
            let meta=DeliveryMeta {
                process_origin:Some(proof),
                ..if queued { DeliveryMeta::queued((now()*1000.0) as i64) } else { DeliveryMeta::direct() }
            };
            let row=cmd_hist_record_with_id(&state,"mrpopo","yes",ctype,"",true,meta).await;
            // No GET or explicit resolver call before this assertion.
            assert_eq!(load(&state,&id).unwrap().resolution_row,Some(row),"{ctype}/{queued}/{proof}");
            on_row(&state,row).await;
            assert_eq!(state.store.read().unwrap().query_row(
                "SELECT COUNT(*) FROM _amux_delegation_resolutions WHERE reply_row=?1",[row],|r|r.get::<_,i64>(0)).unwrap(),1);
            assert!(load(&state,&id).unwrap().evidence_row.is_none(),"correlation must not spend a grant");
        }
        let id=explicit_request(&app,&state,json!({"worktree_verify":"two"})).await;
        for (ctype,proof) in [("session",Some("remote")),("chat",None),("user",Some("worker"))] {
            let row=cmd_hist_record_with_id(&state,"mrpopo","approve",ctype,"peer",true,
                DeliveryMeta { process_origin:proof, ..DeliveryMeta::direct() }).await;
            assert!(load(&state,&id).unwrap().resolution_row.is_none(),"{ctype}/{proof:?} cannot supply owner provenance");
            assert_eq!(state.store.read().unwrap().query_row(
                "SELECT COUNT(*) FROM _amux_delegation_resolutions WHERE reply_row=?1",[row],|r|r.get::<_,i64>(0)).unwrap(),0);
        }
        // A Chat-tab wait closes the old window for a subsequent pane/send yes.
        cmd_hist_record_with_id(&state,"mrpopo","wait","chat","",true,
            DeliveryMeta { process_origin:Some("outside"), ..DeliveryMeta::direct() }).await;
        let next=explicit_request(&app,&state,json!({"worktree_verify":"three"})).await;
        let row=cmd_hist_record_with_id(&state,"mrpopo","yes","user","",true,
            DeliveryMeta { process_origin:Some("outside"), ..DeliveryMeta::direct() }).await;
        assert_eq!(load(&state,&next).unwrap().resolution_row,Some(row));
        assert_eq!(load(&state,&id).unwrap().resolution_code.as_deref(),Some("request_reply_not_affirmative"));
    }

    async fn record_reply(state: &AppState, text: &str, source: &str) -> i64 {
        let session="mrpopo".to_string();
        let text=text.to_string();
        let source=source.to_string();
        let id=Arc::new(std::sync::atomic::AtomicI64::new(0));
        let out=id.clone();
        state.store.write(move|c| {
            c.execute("INSERT INTO cmd_history(session,text,type,origin,delivery,process_origin,ts,delegation_session_incarnation) VALUES(?1,?2,'user',?3,?4,?5,?6,'inc-mrpopo')",
                rusqlite::params![session,text,if source=="pane" {"pane"} else {""},if source=="pane" {"pane"} else {"direct"},source,(now()*1000.0) as i64])?;
            out.store(c.last_insert_rowid(),std::sync::atomic::Ordering::SeqCst);
            Ok(crate::db::WriteOutcome{applied:true,events:vec![]})
        }).unwrap();
        let id=id.load(std::sync::atomic::Ordering::SeqCst);
        on_row(state,id).await;
        id
    }

    #[tokio::test]
    async fn explicit_request_pending_passkey_checks_proof_and_replays_exactly_once() {
        let tmp=tempfile::tempdir().unwrap();
        let _home=crate::api::settings::test_env::set_home(tmp.path());
        let state=test_state(tmp.path());
        std::fs::write(tmp.path().join("amux.env"),format!("{MODE_KEY}=passkey\n")).unwrap();
        let _r=super::super::process_identity::test_support::set_resolver(Arc::new(
            super::super::process_identity::test_support::ByPort(std::collections::HashMap::from([
                (41001u16,super::super::process_identity::Origin::Outside),
                (41002u16,super::super::process_identity::Origin::Remote{forwarded:false}),
                (41003u16,super::super::process_identity::Origin::Remote{forwarded:true})]))));
        let app=super::super::router(state.clone());
        let id=explicit_request(&app,&state,json!({"worktree_verify":"one"})).await;
        for port in [41001u16,41003] {
            let (st,v,_)=call(&app,"POST",&format!("/api/delegation/grants/{id}/approve"),None,Value::Null,Some(port)).await;
            assert_eq!(st,StatusCode::FORBIDDEN,"{v}");
            assert_eq!(v["code"],"grant_approve_requires_remote");
        }
        let (st,v,_)=call(&app,"POST",&format!("/api/delegation/grants/{id}/approve"),None,Value::Null,Some(41002)).await;
        assert!(st.is_success(),"{v}");
        assert_eq!(load(&state,&id).unwrap().status,"applied");
        let id2=explicit_request(&app,&state,json!({"worktree_verify":"two"})).await;
        record_reply(&state,"yes","pane").await;
        assert_eq!(load(&state,&id2).unwrap().status,"pending","pane lacks passkey proof");
        let (st,v,_)=call(&app,"POST",&format!("/api/delegation/grants/{id2}/approve"),None,Value::Null,Some(41002)).await;
        assert!(st.is_success(),"{v}");
        let id3=explicit_request(&app,&state,json!({"worktree_verify":"three"})).await;
        let reply=record_reply(&state,"yes","remote").await;
        let row=load(&state,&id3).unwrap();
        assert_eq!(row.status,"applied");
        assert_eq!(row.evidence_row,Some(reply));
        on_row(&state,reply).await;
        assert_eq!(load(&state,&id3).unwrap().status,"applied");
        assert_eq!(state.store.read().unwrap().query_row("SELECT COUNT(*) FROM _amux_delegation_grants WHERE evidence_row=?1",[reply],|r|r.get::<_,i64>(0)).unwrap(),1);
    }

    #[tokio::test]
    async fn explicit_request_failed_env_write_keeps_reply_spent_and_failure_visible() {
        let tmp=tempfile::tempdir().unwrap();
        let _home=crate::api::settings::test_env::set_home(tmp.path());
        let state=test_state(tmp.path());
        let app=super::super::router(state.clone());
        let id=explicit_request(&app,&state,json!({"worktree_verify":"one"})).await;
        record_reply(&state,"yes","pane").await;
        let path=tmp.path().join("sessions/test-dev-a.env");
        let before=std::fs::read(&path).unwrap();
        let body=json!({"worktree_verify":"one","request_id":id});
        let (st,v,_)=super::super::session_verbs::TEST_ENV_RENAME_FAILURE.scope(path.clone(),
            call(&app,"PATCH","/api/sessions/test-dev-a/config",Some("mrpopo"),body.clone(),None)).await;
        assert_eq!(st,StatusCode::INTERNAL_SERVER_ERROR,"{v}");
        assert_eq!(v["code"],"grant_apply_failed");
        assert_eq!(std::fs::read(path).unwrap(),before);
        let g=load(&state,&id).unwrap();
        assert_eq!(g.status,"failed");
        assert!(g.cite_spent_at.is_some());
        assert!(request_view(&g)["failure_reason"].as_str().unwrap().contains("re-ask"));
        let (st,v,_)=call(&app,"PATCH","/api/sessions/test-dev-a/config",Some("mrpopo"),body,None).await;
        assert_eq!(st,StatusCode::FORBIDDEN,"{v}");
        assert_eq!(v["code"],"cite_pair_spent");
    }

    #[tokio::test]
    async fn explicit_request_claim_refuses_legacy_spent_reply_before_unique_index() {
        let tmp=tempfile::tempdir().unwrap();
        let _home=crate::api::settings::test_env::set_home(tmp.path());
        let state=test_state(tmp.path());
        let app=super::super::router(state.clone());
        let id=explicit_request(&app,&state,json!({"worktree_verify":"one"})).await;
        let reply=record_reply(&state,"yes","pane").await;
        state.store.write(move|c| {
            c.execute("INSERT INTO _amux_delegation_grants(code,actor,class,action,target,method,path,body,sentence,created_at,expires_at,status,evidence_row,legacy_duplicate) VALUES('old','old','old','old','old','PATCH','/old','{}','old',1,2,'undone',?1,1)",[reply])?;
            Ok(crate::db::WriteOutcome{applied:true,events:vec![]})
        }).unwrap();
        assert_eq!(claim_pending(&state,&id,"owner",Some(reply)).unwrap_err().0,"cite_pair_spent");
        assert_eq!(load(&state,&id).unwrap().status,"pending");
        assert!(load(&state,&id).unwrap().evidence_row.is_none());
    }

    #[tokio::test]
    async fn whoami_returns_each_verdict() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = crate::api::settings::test_env::set_home(tmp.path());
        let state = test_state(tmp.path());
        let _r = super::super::process_identity::test_support::set_resolver(std::sync::Arc::new(
            super::super::process_identity::test_support::ByPort(std::collections::HashMap::from([
                (42001u16, super::super::process_identity::Origin::Outside),
                (42002u16, super::super::process_identity::Origin::Remote { forwarded: false }),
                (42003u16, super::super::process_identity::Origin::Remote { forwarded: true }),
            ]))));
        let app = super::super::router(state.clone());
        for (port, po, fwd, proof) in [(42001u16, json!("outside"), false, false), (42002, json!("remote"), false, true), (42003, json!("remote-forwarded"), true, false)] {
            let (st, v, _) = call(&app, "GET", "/api/delegation/whoami", None, Value::Null, Some(port)).await;
            assert!(st.is_success(), "{v}");
            assert_eq!((v["process_origin"].clone(), v["forwarded"].clone(), v["approval_proof"].clone()), (po, json!(fwd), json!(proof)), "port {port}: {v}");
        }
        let (_, v, _) = call(&app, "GET", "/api/delegation/whoami", None, Value::Null, None).await;
        assert_eq!(v["process_origin"], Value::Null, "in-process: no verdict");
    }
}
