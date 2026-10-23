// Structured grants: the audit unit for every widening change mrpopo makes — cite-and-apply by default, passkey-approved when the owner sets it — refs maxtongwang/ai-config#160.
// Cite mode verifies an (ask, reply) citation pair (review B4); Max's rule: automation is not traded for security, so audit, feed and one-step undo are the control.
//! MODES (`AMUX_DELEGATION_APPROVAL`, global; owner-only, in HELD_FOR_MAX):
//! - `cite` (default): mrpopo submits the change WITH `cite`
//!   `{ask_row_id, reply_row_id}`: its own message into a session naming the
//!   target and the setting, and Max's first typed reply there within 2 hours
//!   (`max_ref::cite_pair`, ruling of review B4). The server records a grant
//!   (code, exact change, both texts, actor) and the change APPLIES at once.
//! - `passkey`: the change is stored as a PENDING grant (202 with the code and
//!   the exact sentence) and applies only with an approval proof
//!   ([`approval_proof_ok`], the one extension point a passkey follow-up
//!   plugs into): today the owner's Approve button from a NON-LOCAL Remote
//!   peer, or an owner send from one containing `amux grant <code>`.
//!
//! CITATION RESERVATION. One (ask, reply) pair reserves one grant. Its unique
//! pair, spent timestamp, target incarnation and exact change request are
//! committed together before handler execution; failed/interrupted grants
//! keep their evidence spent (`cite_pair_spent`), including after undo. The
//! files and other handler writes are separate resources, not a SQLite ACID
//! transaction; an interrupted `applying` grant is never automatically replayed.
//! B1 ruling (a): EnvFile commits by temp file + same-directory rename. A
//! failed handler returns `grant_apply_failed`, records `failed`, and emits
//! WARN `delegation_grant_failed`. Grant lists expose `failure_reason` with
//! the instruction to re-ask the owner, also shown by `amux grant list
//! [--json]` and Settings > Delegated changes. The pair stays spent.
//! Worker asks record server-derived CC_INCARNATION bindings at message time;
//! legacy asks without them need a fresh ask. Rename/name reuse cannot transfer
//! a grant, including pending apply, internal replay, and undo.
//! Tag asks use `add [the] TAG tag to WORKER` / `remove [the] TAG tag from WORKER`
//! for each changed tag. Mismatches refuse as `cite_ask_missing_change`.
//! Env writes compare their checked state under EnvFile's process/file locks;
//! a mismatch answers 409 `delegation_state_changed` without the stale write.
//!
//! EXACTLY ONCE. The handler that applies a grant runs inside the [`capture`]
//! middleware, which snapshots the files a config change can write before the
//! handler and records the difference after it: the grant moves `applying`
//! -> `applied` (or `failed`) once, keeps the before/after of each changed key
//! for undo, logs `grant:applied` and posts the sentence and the cited excerpt
//! to the leads. A pending grant is replayed through the router with a
//! server-only `x-amux-grant` header; `enforce` strips any copy on a socket.
//!
//! UNDO. `POST /api/delegation/grants/<code>/undo` (the owner, or mrpopo)
//! restores the stored before-state when nothing changed since; otherwise it
//! refuses and names what changed. Every store a grant kind writes is in the
//! snapshot (env files, rules/memory text, and whole rows of [`TABLES`]);
//! table rows come back by UPDATE of the changed columns. An undo with
//! nothing to restore refuses (`grant_undo_nothing_restorable`). A worker that is running keeps its
//! process until its next restart; the files are restored exactly.
//!
//! ACCEPTED LIMIT (ai-config#136): another lane's process can mint a pane row
//! through its own `/report`, and a same-user process can type into Max's
//! pane. In cite mode that is mitigated by the audit log, the leads feed and
//! undo, not prevented. A forwarded Remote verdict (tailscale serve over
//! loopback) is never owner proof: a local process can ride that forward.
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
/// A request body larger than this is not captured, so it cannot become a grant.
const CAPTURE_LIMIT: usize = 1 << 20;
/// Fail closed: a write failure never releases an already reserved citation.
const APPLY_FAILED_REASON: &str = "failed: re-ask the owner; any cited pair remains spent";
/// The paths whose handlers can make a grant.
const CAPTURE_PREFIXES: [&str; 8] = [
    "/api/sessions/", "/api/workers", "/api/scope", "/api/groups/", "/api/settings/", "/api/config/", "/api/env/", "/api/alert/config",
];
/// Request fields that are delegation metadata, never part of the change.
const META: [&str; 2] = ["cite", "why"];

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
/// Compare every checked key, including tags and incarnation, so a concurrent
/// owner change cannot be overwritten by an absolute, stale delegated value.
pub(crate) fn compare_env(path: &std::path::Path) -> std::io::Result<()> {
    let Some(c) = captured() else { return Ok(()) };
    let checked = c.checked_env.lock().unwrap_or_else(|e| e.into_inner());
    let Some(expected) = checked.get(path) else { return Ok(()) };
    let current: BTreeMap<String, String> = super::session_verbs::EnvFile::load(path).pairs().iter().cloned().collect();
    if &current != expected {
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
    let serial = path == "/api/scope" || path == "/api/workers"
        || path.ends_with("/config") || path.ends_with("/rename")
        || ["/api/groups/", "/api/settings/", "/api/config/"].iter().any(|p| path.starts_with(p))
        || path.strip_prefix("/api/workers/").is_some_and(|p| !p.contains('/'));
    let _mutation = if serial { Some(CONFIG_MUTATION.lock().await) } else { None };
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
    let body: String = body.chars().take(400).collect();
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
    status: &'static str,
    cited: Option<&'a super::max_ref::Cited>,
}

/// Insert a grant for the captured request; returns its code and sentence.
#[allow(clippy::result_large_err)] // the refusal IS the handler's response; it is returned as-is
fn insert(state: &AppState, g: &NewGrant, c: &Captured) -> Result<(String, String), Response> {
    let created = now();
    for _ in 0..10 {
        let Some(code) = new_code() else { break };
        let text = sentence(&code, g.actor, g.action, g.target, c);
        let row = (code.clone(), g.actor.to_string(), g.class.to_string(), g.action.to_string(), g.target.to_string(),
            c.method.clone(), c.path.clone(), change_body(&c.body).to_string(), text.clone(), g.why.clone(), g.status.to_string(),
            g.cited.map(|x| x.row_id), g.cited.map(|x| x.excerpt.clone()), mode_str(mode()).to_string(),
            g.cited.map(|x| x.ask_row_id), g.cited.map(|x| x.ask_text.clone()), g.cited.map(|x| x.reply_text.clone()));
        let target_incarnation = match worker_identity(g.target) {
            Ok(id) => id,
            Err(e) => return Err(forbid("grant_target_identity_changed", e)),
        };
        if g.cited.is_some_and(|c| c.target_incarnation != target_incarnation) {
            return Err(forbid("cite_target_identity_changed", "the cited worker changed identity".into()));
        }
        let inserted = state.store.write(move |conn| {
            // The spent marker and exact immutable request (body/path/method)
            // are reserved by the SAME transaction as the grant, before any
            // handler side effect. A failed/interrupted handler never frees it.
            if let (Some(ask), Some(reply)) = (row.14, row.11) {
                let spent: bool = conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM _amux_delegation_grants WHERE ask_row=?1 AND evidence_row=?2)",
                    rusqlite::params![ask, reply], |r| r.get(0))?;
                if spent {
                    return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
                        std::io::Error::other("cite_pair_spent"),
                    )));
                }
            }
            let n = conn.execute(
                "INSERT OR IGNORE INTO _amux_delegation_grants (code, actor, class, action, target, method, path, body, sentence, why, created_at, expires_at, status, evidence_row, cited_excerpt, mode, ask_row, ask_text, reply_text, cite_spent_at, target_incarnation) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)",
                rusqlite::params![row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7, row.8, row.9, created, created + GRANT_TTL_S, row.10, row.11, row.12, row.13, row.14, row.15, row.16, row.14.map(|_| created), target_incarnation],
            )?;
            Ok(crate::db::WriteOutcome { applied: n == 1, events: vec![] })
        });
        match inserted {
            Ok(o) if o.applied => return Ok((code, text)),
            Ok(_) => continue,
            Err(e) if e.to_string().contains("cite_pair_spent") => return Err(forbid("cite_pair_spent", "this citation pair already reserved a grant; ask Max again".into())),
            Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e.to_string()}))).into_response()),
        }
    }
    Err((StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": "could not mint a grant code"}))).into_response())
}

/// Arm the running request to finalize `code` after its handler, with the
/// state as it reads now.
fn arm(state: &AppState, code: &str) -> bool {
    let Some(c) = captured() else { return false };
    let Ok(mut slot) = c.armed.lock() else { return false };
    *slot = Some((code.to_string(), snapshot(state)));
    true
}

/// Cite mode: record the grant for the captured request with its citation and
/// let the handler apply it now. Returns the code.
#[allow(clippy::result_large_err)] // the refusal IS the handler's response; it is returned as-is
pub(crate) fn begin_cited(state: &AppState, actor: &str, class: &str, action: &str, target: &str, why: Option<String>, cited: &super::max_ref::Cited) -> Result<String, Response> {
    let Some(c) = captured() else {
        return Err(forbid("grant_unavailable", format!("{action} could not be recorded as a grant (no captured body)")));
    };
    let (code, text) = insert(state, &NewGrant { actor, class, action, target, why, status: "applying", cited: Some(cited) }, &c)?;
    if !arm(state, &code) {
        return Err(forbid("grant_unavailable", "the grant could not be armed".into()));
    }
    tracing::info!(actor, action, target, grant = %code, cited_row = cited.row_id, ask_row = cited.ask_row_id, cited_session = %cited.session,
        verdict = "delegation_grant_cited", measured = true, n_considered = 1, "a cited widening change is applying as a grant");
    let _ = text;
    Ok(code)
}

/// Passkey mode: store a pending grant for the captured request and answer
/// 202 with its code and sentence.
pub(crate) fn request(state: &AppState, actor: &str, class: &str, action: &str, target: &str, why: Option<String>) -> Response {
    let Some(c) = captured() else {
        return forbid("grant_unavailable", format!("{action} needs Max's approval, but this request could not be stored as a grant (no captured body)"));
    };
    let (code, text) = match insert(state, &NewGrant { actor, class, action, target, why: why.clone(), status: "pending", cited: None }, &c) {
        Ok(x) => x,
        Err(r) => return r,
    };
    log_transition(state, &code, "requested", actor, class, target, json!({"sentence": text, "method": c.method, "path": c.path, "body": change_body(&c.body)}), why);
    tracing::info!(actor, action, target, grant = %code, verdict = "delegation_grant_requested", measured = true, n_considered = 1,
        "a widening change was stored as a pending grant (passkey mode)");
    (StatusCode::ACCEPTED, Json(json!({"ok": true, "pending": true, "grant_code": code, "sentence": text,
        "expires_at": now() + GRANT_TTL_S,
        "how": format!("passkey mode: Max approves with Approve in Settings > Delegated changes, or by sending `amux grant {code}` from his phone")}))).into_response()
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
}

const COLS: &str = "code, actor, class, action, target, method, path, body, sentence, status, expires_at, evidence_row, cited_excerpt, changes, ask_row, ask_text, reply_text, target_incarnation, cite_spent_at";

fn map_row(r: &rusqlite::Row) -> rusqlite::Result<GrantRow> {
    Ok(GrantRow {
        code: r.get(0)?, actor: r.get(1)?, class: r.get(2)?, action: r.get(3)?, target: r.get(4)?, method: r.get(5)?,
        path: r.get(6)?, body: r.get(7)?, sentence: r.get(8)?, status: r.get(9)?, expires_at: r.get(10)?, evidence_row: r.get(11)?,
        cited_excerpt: r.get(12)?, changes: r.get(13)?, ask_row: r.get(14)?, ask_text: r.get(15)?, reply_text: r.get(16)?, target_incarnation: r.get(17)?, cite_spent_at: r.get(18)?,
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

fn same_worker(g: &GrantRow) -> bool {
    worker_identity(&g.target).is_ok_and(|id| id == g.target_incarnation)
}

fn not_applicable(state: &AppState, g: &GrantRow) -> Option<(&'static str, String)> {
    if !same_worker(g) {
        return Some(("grant_target_identity_changed", format!("grant {} belongs to a different worker incarnation", g.code)));
    }
    if expire_if_due(state, g) {
        return Some(("grant_expired", format!("grant {} expired; ask again", g.code)));
    }
    (g.status != "pending").then(|| ("grant_not_pending", format!("grant {} is {}, not pending; it applies once", g.code, g.status)))
}

/// Apply a pending grant: exactly the stored request, once.
pub(crate) async fn apply(state: &AppState, code: &str, by: &str, evidence: Option<i64>) -> Result<Value, (&'static str, String)> {
    let g = load(state, code).ok_or(("grant_not_found", format!("no grant {code}")))?;
    if let Some(why) = not_applicable(state, &g) {
        return Err(why);
    }
    if !set_status(state, &g.code, "pending", "applying", by, evidence) {
        return Err(("grant_not_pending", format!("grant {} was already taken", g.code)));
    }
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
    let body = if g.body == "null" { Body::empty() } else { Body::from(g.body.clone()) };
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
pub(crate) fn claim_applying(state: &AppState, code: &str, actor: &str, action: &str, target: &str) -> Result<Option<i64>, String> {
    let g = load(state, code).ok_or_else(|| format!("no grant {code}"))?;
    if !same_worker(&g) { return Err("grant_target_identity_changed".into()); }
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

/// The grant codes a line names (`amux grant 7K3Q`, any case).
pub(crate) fn codes_in(text: &str) -> Vec<String> {
    // Every Messages row passes through here: compile once.
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?i)\bamux\s+grant\s+([A-Za-z0-9]{4})\b").expect("static regex"));
    re.captures_iter(text).map(|c| c[1].to_ascii_uppercase()).collect()
}

/// Called for every Messages row as it is written: an owner send from a
/// non-local Remote peer naming a pending grant approves it.
pub(crate) async fn on_row(state: &AppState, row_id: i64, text: &str) {
    for code in codes_in(text) {
        let Some(g) = load(state, &code) else {
            tracing::info!(row_id, grant = %code, verdict = "delegation_grant_unknown_code", measured = true, n_considered = 1,
                "a line named a grant code that does not exist");
            continue;
        };
        if let Err(r) = super::max_ref::row_qualifies(state, row_id) {
            tracing::warn!(row_id, grant = %code, actor = %g.actor, code = r.code(), verdict = "delegation_grant_row_not_qualifying",
                measured = true, n_considered = 1, "a line naming a grant is not an owner send from a non-local Remote peer; it does not approve");
            continue;
        }
        if let Err((c, why)) = apply(state, &code, &format!("owner:sent:{row_id}"), Some(row_id)).await {
            tracing::warn!(row_id, grant = %code, code = c, why = %why, verdict = "delegation_grant_sent_not_applied", measured = true,
                n_considered = 1, "a qualifying approval did not apply its grant");
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
    if !same_worker(&g) {
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

/// `GET /api/delegation/grants`: newest first; readers as the delegation log.
async fn list(State(state): State<AppState>, uri: Uri, headers: HeaderMap) -> Response {
    if let Some(r) = super::delegation_log::read_refusal(&state, &headers, &uri) {
        return r;
    }
    let rows: Vec<GrantRow> = state.store.read().ok().map(|conn| {
        conn.prepare(&format!("SELECT {COLS} FROM _amux_delegation_grants ORDER BY created_at DESC LIMIT 200"))
            .and_then(|mut st| st.query_map([], map_row).map(|it| it.flatten().collect()))
            .unwrap_or_default()
    }).unwrap_or_default();
    for g in &rows {
        expire_if_due(&state, g);
    }
    let rows: Vec<Value> = rows.iter().map(|g| {
        let status = if g.status == "pending" && now() > g.expires_at { "expired" } else { g.status.as_str() };
        json!({"code": g.code, "actor": g.actor, "class": g.class, "action": g.action, "target": g.target, "sentence": g.sentence,
            "status": status, "failure_reason": failure_reason(status), "expires_at": g.expires_at, "method": g.method, "path": g.path, "cited_row": g.evidence_row,
            "target_incarnation": g.target_incarnation, "cite_spent_at": g.cite_spent_at,
            "cited_excerpt": g.cited_excerpt, "ask_row": g.ask_row, "ask_text": g.ask_text, "reply_text": g.reply_text, "body": serde_json::from_str::<Value>(&g.body).unwrap_or(Value::Null)})
    }).collect();
    Json(json!({"grants": rows, "mode": mode_str(mode()), "measured": true, "n_considered": rows.len()})).into_response()
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
        .route("/api/delegation/whoami", get(whoami))
        .route("/api/delegation/grants/{code}/approve", post(approve))
        .route("/api/delegation/grants/{code}/deny", post(deny))
        .route("/api/delegation/grants/{code}/undo", post(undo))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;
    use tracing::instrument::WithSubscriber;

    #[derive(Clone, Default)]
    struct GrantTestLogs(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for GrantTestLogs {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }

    #[test]
    fn codes_are_read_from_a_typed_line_in_any_case() {
        assert_eq!(codes_in("ok — amux grant 7k3q please"), vec!["7K3Q".to_string()]);
        assert_eq!(codes_in("amux grant 7K3Q and amux  grant ABCD"), vec!["7K3Q".to_string(), "ABCD".to_string()]);
        assert!(codes_in("amux granted 7K3Q").is_empty());
        assert!(codes_in("amux grant 7K3QX").is_empty(), "five characters is not a code");
    }

    fn context(body: Value) -> Captured {
        Captured {
            method: "PATCH".into(), path: "/api/sessions/test-dev-a/config".into(), body,
            armed: Arc::new(Mutex::new(None)), checked_env: Arc::new(Mutex::new(BTreeMap::new())),
            compare_failed: Arc::new(Mutex::new(false)),
        }
    }

    #[tokio::test]
    async fn delegation_message_recorder_binds_the_ask_to_worker_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = crate::api::settings::test_env::set_home(tmp.path());
        let state = test_state(tmp.path());
        let row = super::super::session_verbs::cmd_hist_record_with_id(
            &state,"test-desk","Max, may I enable yolo for test-dev-a?","session","mrpopo",true,
            super::super::session_verbs::DeliveryMeta { delivery:Some(super::super::session_verbs::Delivery::Direct),..Default::default() }).await;
        assert!(row > 0);
        let conn = state.store.read().unwrap();
        let raw: String = conn.query_row("SELECT cite_worker_ids FROM cmd_history WHERE id=?1",[row],|r|r.get(0)).unwrap();
        let ids: BTreeMap<String,String> = serde_json::from_str(&raw).unwrap();
        assert_eq!(ids.get("test-dev-a").map(String::as_str),Some("inc-test-dev-a"));
    }

    #[tokio::test]
    async fn delegation_citation_insert_refuses_spent_pair_even_without_precheck() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = crate::api::settings::test_env::set_home(tmp.path());
        let state = test_state(tmp.path());
        let cited = super::super::max_ref::Cited { row_id: 2, ask_row_id: 1, session: "test-desk".into(),
            excerpt: "yes".into(), ask_text: "Max, may I enable yolo for test-dev-a?".into(), reply_text: "yes".into(),
            target_incarnation: Some("inc-test-dev-a".into()) };
        let c = context(json!({"toggle_yolo":true}));
        CAPTURED.scope(c, async {
            let code = begin_cited(&state,"mrpopo","mrpopo","set_config:toggle_yolo","test-dev-a",None,&cited).unwrap();
            let response = begin_cited(&state,"mrpopo","mrpopo","set_config:toggle_yolo","test-dev-a",None,&cited).unwrap_err();
            assert_eq!(response.status(),StatusCode::FORBIDDEN);
            let bytes = axum::body::to_bytes(response.into_body(),usize::MAX).await.unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["code"],"cite_pair_spent","insert guard must name spent evidence, not code exhaustion");
            assert_eq!(super::super::max_ref::pair_unspent(&state,&super::super::max_ref::Cite {
                ask_row_id:Some(1),reply_row_id:Some(2),..Default::default() }),Err(super::super::max_ref::MaxRefRefusal::PairSpent));
            let conn = state.store.read().unwrap();
            let (spent, body): (Option<f64>,String) = conn.query_row(
                "SELECT cite_spent_at,body FROM _amux_delegation_grants WHERE code=?1",[&code],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
            assert!(spent.is_some());
            assert_eq!(body,json!({"toggle_yolo":true}).to_string());
        }).await;
    }

    #[tokio::test]
    async fn delegation_citation_unique_constraint_prevents_second_grant() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = crate::api::settings::test_env::set_home(tmp.path());
        let state = test_state(tmp.path());
        let conn = state.store.read().unwrap();
        assert!(conn.prepare("SELECT cite_spent_at,target_incarnation FROM _amux_delegation_grants").is_ok());
        drop(conn);
        let insert = |code: &str| {
            let code = code.to_string();
            state.store.write(move |c| {
                c.execute("INSERT INTO _amux_delegation_grants(code,actor,class,action,target,method,path,body,sentence,created_at,expires_at,status,ask_row,evidence_row,cite_spent_at) VALUES(?1,'mrpopo','mrpopo','x','fleet','PUT','/api/scope','{}','change',1,2,'applying',11,12,1)",[code])?;
                Ok(crate::db::WriteOutcome{applied:true,events:vec![]})
            })
        };
        insert("ABCD").unwrap();
        let err = insert("EFGH").err().expect("a second grant must violate citation uniqueness");
        assert!(err.to_string().contains("UNIQUE constraint failed"),"{err}");
        let conn = state.store.read().unwrap();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM _amux_delegation_grants",[],|r|r.get(0)).unwrap();
        assert_eq!(count,1);
    }

    /// B1 ruling (a): both grant-applicable env entrypoints fail before rename.
    /// The grant/pair are already committed, so recovery requires a fresh ask.
    #[tokio::test]
    async fn delegation_failed_env_write_spends_pair_and_requires_owner_reask() {
        for writer in ["write", "merge_plain"] {
            let tmp = tempfile::tempdir().unwrap();
            let _home = crate::api::settings::test_env::set_home(tmp.path());
            let state = test_state(tmp.path());
            let app = super::super::router(state.clone());
            let path = tmp.path().join("sessions/test-dev-a.env");
            let before = std::fs::read(&path).unwrap();
            let cite = pair(&state, "test-desk", "Max, may I remove the dev tag from test-dev-a?", "yes");
            let (method, route, body) = match writer {
                "write" => ("PATCH", "/api/sessions/test-dev-a/config", json!({"tags": [], "cite": cite})),
                "merge_plain" => ("PUT", "/api/scope", json!({"level": "worker", "name": "test-dev-a", "capability": "env", "value": {"CC_TAGS": ""}, "cite": cite})),
                _ => unreachable!(),
            };
            // Task-local and cfg(test): no HTTP header or production env can
            // enable this seam. Fail AFTER temp write/sync, BEFORE rename.
            let logs = GrantTestLogs::default();
            let sink = logs.clone();
            let subscriber = tracing_subscriber::fmt().without_time().with_ansi(false)
                .with_max_level(tracing::Level::WARN).with_writer(move || sink.clone()).finish();
            let (st, v, h) = super::super::session_verbs::TEST_ENV_RENAME_FAILURE.scope(path.clone(),
                call(&app, method, route, Some("mrpopo"), body.clone(), None)).with_subscriber(subscriber).await;
            assert_eq!(st, StatusCode::INTERNAL_SERVER_ERROR, "{writer}: {v}");
            assert_eq!(v["code"], "grant_apply_failed", "{writer}: {v}");
            assert_eq!(v["error"], APPLY_FAILED_REASON);
            assert!(v["error"].as_str().unwrap().contains("re-ask the owner"));
            let code = h.get(GRANT_CODE_HEADER).unwrap().to_str().unwrap();
            let g = load(&state, code).unwrap();
            assert_eq!(g.status, "failed", "{writer}: failure must be durable");
            let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
            assert!(logs.lines().any(|line| line.contains("WARN") && line.contains("delegation_grant_failed") && line.contains(code)),
                "{writer}: failed grant must announce itself as a WARN with its code: {logs}");
            assert!(g.cite_spent_at.is_some(), "{writer}: citation must stay spent");
            assert_eq!(std::fs::read(&path).unwrap(), before, "{writer}: failed rename changed env bytes");
            let citation = serde_json::from_value::<super::super::max_ref::Cite>(cite).unwrap();
            assert_eq!(super::super::max_ref::pair_unspent(&state, &citation), Err(super::super::max_ref::MaxRefRefusal::PairSpent));
            let (st, rows, _) = call(&app, "GET", "/api/delegation/grants", None, Value::Null, None).await;
            assert_eq!(st, StatusCode::OK, "{rows}");
            let rows = rows["grants"].as_array().unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0]["code"], code);
            assert_eq!(rows[0]["status"], "failed");
            assert_eq!(rows[0]["failure_reason"], APPLY_FAILED_REASON);
            // The seam has ended; the SAME request is refused even though
            // the env is unchanged and a normal write would now succeed.
            let (st, v, _) = call(&app, method, route, Some("mrpopo"), body, None).await;
            assert_eq!(st, StatusCode::FORBIDDEN, "{writer}: {v}");
            assert_eq!(v["code"], "cite_pair_spent", "{writer}: {v}");
            assert_eq!(std::fs::read(&path).unwrap(), before);
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

    #[tokio::test]
    async fn delegation_pending_grant_refuses_a_reused_name_on_apply_claim_and_undo() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = crate::api::settings::test_env::set_home(tmp.path());
        let state = test_state(tmp.path());
        let c = context(json!({"toggle_yolo":true}));
        let g = NewGrant { actor:"mrpopo",class:"mrpopo",action:"set_config:toggle_yolo",target:"test-dev-a",why:None,status:"pending",cited:None };
        let (code,_) = insert(&state,&g,&c).unwrap();
        let row = load(&state,&code).unwrap();
        assert_eq!(row.target_incarnation.as_deref(),Some("inc-test-dev-a"));
        let path = tmp.path().join("sessions/test-dev-a.env");
        std::fs::rename(&path,tmp.path().join("sessions/renamed.env")).unwrap();
        // A new worker with the released name has its own incarnation.
        std::fs::write(&path,"CC_TAGS=dev\nCC_INCARNATION=other-worker\n").unwrap();
        assert_eq!(not_applicable(&state,&row).unwrap().0,"grant_target_identity_changed");
        let err = apply(&state,&code,"owner",None).await.unwrap_err();
        assert_eq!(err.0,"grant_target_identity_changed");
        assert!(set_status(&state,&code,"pending","applying","owner",None));
        assert_eq!(claim_applying(&state,&code,"mrpopo","set_config:toggle_yolo","test-dev-a").unwrap_err(),"grant_target_identity_changed");
        assert!(set_status(&state,&code,"applying","applied","owner",None));
        let app = super::super::router(state.clone());
        let (st,v,_) = call(&app,"POST",&format!("/api/delegation/grants/{code}/undo"),None,Value::Null,None).await;
        assert_eq!(st,StatusCode::FORBIDDEN,"{v}");
        assert_eq!(v["code"],"grant_target_identity_changed");
        assert!(std::fs::read_to_string(&path).unwrap().contains("other-worker"));
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
        AppState {
            store: std::sync::Arc::new(crate::db::Store::open(&dir.join("t.db")).unwrap()),
            started: std::time::Instant::now(),
            build_hash: "test".into(),
            auth_token: None,
            reconciled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }

    fn env_of(dir: &std::path::Path) -> String {
        std::fs::read_to_string(dir.join("sessions/test-dev-a.env")).unwrap()
    }

    fn yolo_on(dir: &std::path::Path) -> bool {
        let env = env_of(dir);
        env.contains("dangerously") || env.contains("CC_AUTO_CONTINUE=\"1\"")
    }

    fn logged(dir: &std::path::Path, action: &str) -> usize {
        std::fs::read_to_string(super::super::delegation_log::log_path(dir)).unwrap_or_default()
            .lines().filter(|l| l.contains(&format!("\"action\":\"{action}\""))).count()
    }

    #[allow(clippy::too_many_arguments)] // test fixture: one cmd_history row's columns
    fn put_row(state: &AppState, session: &str, text: &str, ty: &str, origin: &str, delivery: &str, po: Option<&str>, ts: f64) -> i64 {
        let ids = super::super::max_ref::worker_bindings(text);
        let (session, text, ty, origin, delivery, po) = (session.to_string(), text.to_string(), ty.to_string(), origin.to_string(), delivery.to_string(), po.map(str::to_string));
        let id = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
        let idw = id.clone();
        state.store.write(move |c| {
            crate::api::session_verbs::ensure_fleet_tables(c)?;
            c.execute("INSERT INTO cmd_history(session,text,type,origin,delivery,process_origin,ts,cite_worker_ids) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                rusqlite::params![session, text, ty, origin, delivery, po, (ts * 1000.0) as i64,ids])?;
            idw.store(c.last_insert_rowid(), std::sync::atomic::Ordering::SeqCst);
            Ok(crate::db::WriteOutcome { applied: true, events: vec![] })
        }).unwrap();
        id.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn pane(state: &AppState, session: &str, text: &str) -> i64 {
        put_row(state, session, text, "user", "pane", "pane", None, now())
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

    fn latest_code(state: &AppState) -> String {
        state.store.read().unwrap().query_row("SELECT code FROM _amux_delegation_grants ORDER BY created_at DESC LIMIT 1", [], |r| r.get(0)).unwrap()
    }

    /// A citation pair (review B4): mrpopo's ask into `session`, then Max's
    /// reply typed in that session's pane.
    fn pair(state: &AppState, session: &str, ask_text: &str, reply: &str) -> Value {
        let ask = put_row(state, session, ask_text, "session", "mrpopo", "direct", None, now() - 60.0);
        let reply = put_row(state, session, reply, "user", "pane", "pane", None, now() - 30.0);
        json!({"ask_row_id": ask, "reply_row_id": reply})
    }

    /// Every row of every table undo restores, and every file under the home
    /// except the store and the append-only audit log: the before-state.
    /// (table -> row key -> row, file -> bytes).
    type WholeState = (BTreeMap<String, BTreeMap<String, Map<String, Value>>>, BTreeMap<String, Vec<u8>>);

    fn whole_state(state: &AppState, dir: &std::path::Path) -> WholeState {
        let conn = state.store.read().unwrap();
        let tables = TABLES.iter().map(|(t, k)| (t.to_string(), table_rows(&conn, t, k))).collect();
        let mut files = BTreeMap::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                let r = rel(dir, &p);
                if r.starts_with("audit") || r.starts_with("t.db") || r.ends_with(".lock") {
                    continue;
                }
                if p.is_dir() {
                    stack.push(p);
                } else {
                    files.insert(r, std::fs::read(&p).unwrap());
                }
            }
        }
        (tables, files)
    }

    /// dev-lead 2026-10-19: a lead changes membership only through a cited
    /// grant (its direct own-team-tag removal row is gone). Uncited: refused;
    /// cited by the lead's own ask and Max's reply: applied, as a grant.
    #[tokio::test]
    async fn a_lead_changes_membership_only_through_a_cited_grant() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = crate::api::settings::test_env::set_home(tmp.path());
        let state = test_state(tmp.path());
        let app = super::super::router(state.clone());
        let (st, v, _) = call(&app, "PATCH", "/api/sessions/test-dev-a/config", Some("test-dev-lead"),
            json!({"tags": []}), None).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
        assert!(env_of(tmp.path()).contains("CC_TAGS=\"dev\""), "uncited: nothing changed");
        let ask = put_row(&state, "test-desk", "Max, may I remove the dev tag from test-dev-a?", "session", "test-dev-lead", "direct", None, now() - 60.0);
        let reply = put_row(&state, "test-desk", "yes", "user", "pane", "pane", None, now() - 30.0);
        let (st, v, h) = call(&app, "PATCH", "/api/sessions/test-dev-a/config", Some("test-dev-lead"),
            json!({"tags": [], "cite": {"ask_row_id": ask, "reply_row_id": reply}}), None).await;
        assert!(st.is_success(), "{st} {v}");
        let code = h.get(GRANT_CODE_HEADER).and_then(|v| v.to_str().ok()).unwrap().to_string();
        assert_eq!(load(&state, &code).unwrap().status, "applied");
        assert!(!env_of(tmp.path()).contains("CC_TAGS=\"dev\""), "cited: the tag change applied: {}", env_of(tmp.path()));
    }

    /// Cite mode (the default): a valid citation pair applies the change at
    /// once as a grant, stores BOTH texts, is logged and fed to the leads, and
    /// undo restores the files exactly.
    #[tokio::test]
    async fn cite_mode_applies_at_once_and_undo_restores_exactly() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = crate::api::settings::test_env::set_home(tmp.path());
        let state = test_state(tmp.path());
        let app = super::super::router(state.clone());
        let before = config_pairs(&tmp.path().join("sessions/test-dev-a.env"));
        let ask = "Max, may I turn on yolo for test-dev-a? It needs to run unattended tonight.";
        let cite = pair(&state, "test-desk", ask, "yes, go ahead");
        let (st, v, h) = call(&app, "PATCH", "/api/sessions/test-dev-a/config", Some("mrpopo"),
            json!({"toggle_yolo": true, "cite": cite, "why": "Max asked"}), None).await;
        assert!(st.is_success(), "{st} {v}");
        let code = h.get(GRANT_CODE_HEADER).and_then(|v| v.to_str().ok()).unwrap().to_string();
        let g = load(&state, &code).unwrap();
        assert_eq!((g.status.as_str(), g.evidence_row, g.ask_row), ("applied", cite["reply_row_id"].as_i64(), cite["ask_row_id"].as_i64()));
        assert_eq!(g.cited_excerpt.as_deref(), Some("yes, go ahead"));
        // Review B4: the grant row stores both texts.
        assert_eq!((g.ask_text.as_deref(), g.reply_text.as_deref()), (Some(ask), Some("yes, go ahead")));
        assert!(yolo_on(tmp.path()));
        assert_eq!(logged(tmp.path(), "grant:applied"), 1);
        let line = std::fs::read_to_string(super::super::delegation_log::log_path(tmp.path())).unwrap();
        assert!(line.contains("yes, go ahead") && line.contains("run unattended tonight"), "both texts are logged: {line}");
        // Undo: a lead may not; mrpopo (or the owner) may; it restores exactly.
        let (st, _, _) = call(&app, "POST", &format!("/api/delegation/grants/{code}/undo"), Some("test-dev-lead"), Value::Null, None).await;
        assert_eq!(st, StatusCode::FORBIDDEN);
        let (st, v, _) = call(&app, "POST", &format!("/api/delegation/grants/{code}/undo"), Some("mrpopo"), Value::Null, None).await;
        assert!(st.is_success(), "{st} {v}");
        assert_eq!(config_pairs(&tmp.path().join("sessions/test-dev-a.env")), before, "undo restores every configuration key exactly");
        assert_eq!(load(&state, &code).unwrap().status, "undone");
        assert_eq!(logged(tmp.path(), "grant:undone"), 1);
        let (st, _, _) = call(&app, "POST", &format!("/api/delegation/grants/{code}/undo"), None, Value::Null, None).await;
        assert_eq!(st, StatusCode::CONFLICT, "undo applies once");
    }

    #[tokio::test]
    async fn undo_refuses_when_the_state_changed_since() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = crate::api::settings::test_env::set_home(tmp.path());
        let state = test_state(tmp.path());
        let app = super::super::router(state.clone());
        let cite = pair(&state, "test-desk", "Max: make test-dev-a isolated?", "yes");
        let (st, v, h) = call(&app, "PATCH", "/api/sessions/test-dev-a/config", Some("mrpopo"), json!({"isolated": true, "cite": cite}), None).await;
        assert!(st.is_success(), "{st} {v}");
        let code = h.get(GRANT_CODE_HEADER).and_then(|v| v.to_str().ok()).unwrap().to_string();
        super::super::session_verbs::EnvFile::merge_plain(&tmp.path().join("sessions/test-dev-a.env"), &[("CC_ISOLATED".into(), Some("".into()))]).unwrap();
        let (st, v, _) = call(&app, "POST", &format!("/api/delegation/grants/{code}/undo"), None, Value::Null, None).await;
        assert_eq!(st, StatusCode::CONFLICT, "{v}");
        assert_eq!(v["code"], json!("grant_undo_conflict"));
    }

    #[tokio::test]
    async fn cite_mode_refuses_own_lane_delivered_stale_and_missing_citations() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = crate::api::settings::test_env::set_home(tmp.path());
        let state = test_state(tmp.path());
        let app = super::super::router(state.clone());
        let ask = "Max, may I turn on yolo for test-dev-a?";
        let own = pair(&state, "mrpopo", ask, "yes");
        let desk = |text: &str, reply: &str| pair(&state, &format!("test-desk-{}", text.len() + reply.len()), text, reply);
        let stamped = desk(ask, "[amux-origin: test-peer] yes");
        let unrelated = desk("what time is lunch?", "noon");
        let a = put_row(&state, "test-old", ask, "session", "mrpopo", "direct", None, now() - 90_100.0);
        let r = put_row(&state, "test-old", "yes", "user", "pane", "pane", None, now() - 90_000.0);
        let stale = json!({"ask_row_id": a, "reply_row_id": r});
        let a = put_row(&state, "mrpopo", ask, "session", "mrpopo", "direct", None, now() - 20.0);
        let r = put_row(&state, "mrpopo", "ok", "user", "", "direct", Some("remote-forwarded"), now() - 10.0);
        let fwd = json!({"ask_row_id": a, "reply_row_id": r});
        let lunch = pane(&state, "test-desk", "what time is lunch?");
        for (cite, code) in [(json!(null), "cite_missing"), (json!({"row_id": lunch}), "cite_pair_required"), (own, "max_ref_own_lane"),
            (stamped, "max_ref_not_human_typed"), (stale, "max_ref_stale"), (fwd, "max_ref_own_lane"), (unrelated, "cite_ask_missing_target")] {
            let (st, v, _) = call(&app, "PATCH", "/api/sessions/test-dev-a/config", Some("mrpopo"), json!({"toggle_yolo": true, "cite": cite}), None).await;
            assert_eq!((st, v["code"].clone()), (StatusCode::FORBIDDEN, json!(code)), "{cite}: {v}");
        }
        assert!(!yolo_on(tmp.path()));
        let n: i64 = state.store.read().unwrap().query_row("SELECT COUNT(*) FROM _amux_delegation_grants", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "a refused citation creates no grant");
    }

    /// Review B2: every grant kind records its before-state and undo puts it
    /// back byte for byte: env files, rules text (and the derived per-worker
    /// rules), prefs rows (connectors), session_gates rows (gates), and the
    /// whole group_config row, execution_policy and execution_rev included.
    #[tokio::test]
    async fn every_grant_kind_undoes_to_its_before_state_byte_for_byte() {
        let kinds: Vec<(&str, &str, &str, Value, &str)> = vec![
            ("env (yolo)", "PATCH", "/api/sessions/test-dev-a/config", json!({"toggle_yolo": true}), "turn on yolo for test-dev-a"),
            ("connectors (prefs)", "PUT", "/api/scope", json!({"level": "worker", "name": "test-dev-a", "capability": "connectors",
                "value": {"gmail": {"enabled": true}}}), "enable the gmail connector for test-dev-a"),
            ("gates (session_gates)", "PUT", "/api/scope", json!({"level": "worker", "name": "test-dev-a", "capability": "gates",
                "value": {"review": true}}), "add a review gate for test-dev-a"),
            ("group gates (session_gates)", "PUT", "/api/scope", json!({"level": "group", "name": "dev", "capability": "gates",
                "value": {"review": true}}), "add a review gate for dev"),
            ("group config (group_config)", "PATCH", "/api/groups/dev/config", json!({"department": "eng", "goal": "ship it"}),
                "set the goal for dev"),
            ("group rules (text)", "PUT", "/api/scope", json!({"level": "group", "name": "dev", "capability": "rules",
                "value": "Dev: report blockers first."}), "update the rules for dev"),
        ];
        let mut failures = Vec::new();
        for (name, method, path, body, ask) in kinds {
            let tmp = tempfile::tempdir().unwrap();
            let _home = crate::api::settings::test_env::set_home(tmp.path());
            let state = test_state(tmp.path());
            state.store.write(|c| {
                c.execute("INSERT INTO group_config (name, department, goal, kpis, human_cost, updated, execution_policy, execution_rev) \
                    VALUES ('dev','old','old goal','[]',3,1700000000,'{\"mode\":\"tasks\",\"max_executors\":2}',7)", [])?;
                c.execute("INSERT INTO prefs (key, value) VALUES ('connectors:worker:test-dev-a', '{\"slack\":{\"enabled\":false}}')", [])?;
                Ok(crate::db::WriteOutcome { applied: true, events: vec![] })
            }).unwrap();
            let app = super::super::router(state.clone());
            // Warm the fixture the way a live home already is: one owner env
            // write (canonical quoting, the worker's minted CC_INCARNATION,
            // which undo never touches) and one refused worker request (the
            // identity layer's lazy `renames.version`). The before-state then
            // differs from the after-state only by what the grant wrote.
            let (st, v, _) = call(&app, "PATCH", "/api/sessions/test-dev-a/config", None, json!({"desc": "fixture"}), None).await;
            assert!(st.is_success(), "warm-up: {st} {v}");
            let (st, _, _) = call(&app, method, path, Some("mrpopo"), body.clone(), None).await;
            assert_eq!(st, StatusCode::FORBIDDEN, "{name}: uncited is refused");
            let before = whole_state(&state, tmp.path());
            let mut b = body.clone();
            b["cite"] = pair(&state, "test-desk", &format!("Max, may I {ask}?"), "yes");
            let (st, v, h) = call(&app, method, path, Some("mrpopo"), b, None).await;
            let Some(code) = h.get(GRANT_CODE_HEADER).and_then(|v| v.to_str().ok()).map(str::to_string).filter(|_| st.is_success()) else {
                failures.push(format!("{name}: the grant did not apply: {st} {v}"));
                continue;
            };
            let applied = whole_state(&state, tmp.path());
            if applied == before {
                failures.push(format!("{name}: the grant changed nothing, so its undo proves nothing"));
                continue;
            }
            let (st, v, _) = call(&app, "POST", &format!("/api/delegation/grants/{code}/undo"), None, Value::Null, None).await;
            if !st.is_success() || v["restored"].as_u64().unwrap_or(0) == 0 || v["byte_exact"] != json!(true) {
                failures.push(format!("{name}: undo answered {st} {v}"));
                continue;
            }
            let after = whole_state(&state, tmp.path());
            if after.0 != before.0 {
                failures.push(format!("{name}: table rows differ after undo:\n before {:?}\n after  {:?}", before.0, after.0));
            }
            for (f, bytes) in before.1.iter().chain(after.1.iter()) {
                if before.1.get(f) != after.1.get(f) {
                    failures.push(format!("{name}: {f} differs after undo: {:?} vs {:?}", before.1.get(f).map(|b| String::from_utf8_lossy(b).into_owned()),
                        after.1.get(f).map(|b| String::from_utf8_lossy(b).into_owned())));
                    let _ = bytes;
                    break;
                }
            }
        }
        assert!(failures.is_empty(), "{} grant kinds not undone exactly:\n{}", failures.len(), failures.join("\n"));
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

    /// Review B2: an undo with nothing to restore fails loudly and leaves the
    /// grant applied, never {undone:true, restored:0}.
    #[tokio::test]
    async fn an_undo_with_nothing_restorable_fails_loudly() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = crate::api::settings::test_env::set_home(tmp.path());
        let state = test_state(tmp.path());
        state.store.write(|c| {
            c.execute("INSERT INTO _amux_delegation_grants (code, actor, class, action, target, method, path, body, sentence, created_at, expires_at, status, changes, undoable) \
                VALUES ('ZZZZ','mrpopo','mrpopo','set_config:scope:connectors','test-dev-a','PUT','/api/scope','{}','amux grant ZZZZ',1,2,'applied', \
                '{\"env\":{},\"text\":{},\"group_config\":{},\"created\":[]}',1)", [])?;
            Ok(crate::db::WriteOutcome { applied: true, events: vec![] })
        }).unwrap();
        let app = super::super::router(state.clone());
        let (st, v, _) = call(&app, "POST", "/api/delegation/grants/ZZZZ/undo", None, Value::Null, None).await;
        assert_eq!((st, v["ok"].clone(), v["code"].clone()), (StatusCode::CONFLICT, json!(false), json!("grant_undo_nothing_restorable")), "{v}");
        assert_eq!(load(&state, "ZZZZ").unwrap().status, "applied", "the grant is not marked undone");
    }

    /// Passkey mode: grants are created PENDING; only the approval proof (a
    /// non-local Remote owner) applies one. A forwarded Remote or an Outside
    /// owner, and a forwarded or pane row, do not.
    #[tokio::test]
    async fn passkey_mode_needs_the_approval_proof() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = crate::api::settings::test_env::set_home(tmp.path());
        std::fs::write(tmp.path().join("amux.env"), format!("{MODE_KEY}=\"passkey\"\n")).unwrap();
        let state = test_state(tmp.path());
        assert_eq!(mode(), Mode::Passkey);
        let _r = super::super::process_identity::test_support::set_resolver(std::sync::Arc::new(
            super::super::process_identity::test_support::ByPort(std::collections::HashMap::from([
                (41001u16, super::super::process_identity::Origin::Outside),
                (41002u16, super::super::process_identity::Origin::Remote { forwarded: false }),
                (41003u16, super::super::process_identity::Origin::Remote { forwarded: true }),
            ]))));
        let app = super::super::router(state.clone());
        let (st, v, _) = call(&app, "PATCH", "/api/sessions/test-dev-a/config", Some("mrpopo"), json!({"toggle_yolo": true}), None).await;
        assert_eq!(st, StatusCode::ACCEPTED, "{v}");
        let code = v["grant_code"].as_str().unwrap().to_string();
        assert!(!yolo_on(tmp.path()), "pending, not applied");
        for row in [pane(&state, "test-desk", &format!("amux grant {code}")),
            put_row(&state, "test-desk", &format!("ok amux grant {code}"), "user", "", "direct", Some("remote-forwarded"), now()),
            put_row(&state, "test-desk", &format!("go amux grant {code}"), "user", "", "direct", Some("outside"), now())] {
            on_row(&state, row, &format!("amux grant {code}")).await;
            assert_eq!(load(&state, &code).unwrap().status, "pending", "row {row} is not approval proof");
        }
        for port in [41001u16, 41003] {
            let (st, v, _) = call(&app, "POST", &format!("/api/delegation/grants/{code}/approve"), None, Value::Null, Some(port)).await;
            assert_eq!((st, v["code"].clone()), (StatusCode::FORBIDDEN, json!("grant_approve_requires_remote")), "port {port}: {v}");
        }
        let (st, v, _) = call(&app, "POST", &format!("/api/delegation/grants/{code}/approve"), None, Value::Null, Some(41002)).await;
        assert!(st.is_success(), "a non-local Remote owner approves: {st} {v}");
        assert_eq!(load(&state, &code).unwrap().status, "applied");
        assert!(yolo_on(tmp.path()));
        assert_eq!(logged(tmp.path(), "grant:applied"), 1, "applied once");
        // A second pending grant approved by a non-local Remote owner SEND.
        let (_, v, _) = call(&app, "PATCH", "/api/sessions/test-dev-a/config", Some("mrpopo"), json!({"isolated": true}), None).await;
        let code2 = v["grant_code"].as_str().unwrap().to_string();
        let row = put_row(&state, "test-desk", &format!("amux grant {code2}"), "user", "", "direct", Some("remote"), now());
        on_row(&state, row, &format!("amux grant {code2}")).await;
        assert_eq!(load(&state, &code2).unwrap().status, "applied", "a non-local Remote owner send approves");
        assert_ne!(latest_code(&state), "");
    }

    /// whoami reports each verdict, forwarded included.
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
