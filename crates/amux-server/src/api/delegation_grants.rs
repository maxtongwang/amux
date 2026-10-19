// Structured grants: the audit unit for every widening change mrpopo makes — cite-and-apply by default, passkey-approved when the owner sets it — refs maxtongwang/ai-config#160.
// Replaces max_ref phrase matching (dev-lead, 2026-10-17). Max's rule: automation is not traded for security; audit, feed and one-step undo are the control.
//! MODES (`AMUX_DELEGATION_APPROVAL`, global; owner-only, in HELD_FOR_MAX):
//! - `cite` (default): mrpopo submits the change WITH `cite` (a row id, or a
//!   session and time) pointing at one of Max's typed rows. The row must
//!   qualify (`max_ref::cite_row`: human-typed, not an amux delivery, fresh,
//!   in the window, in a lane other than mrpopo's own unless it is a
//!   non-local Remote owner send). The server records a grant (code, exact
//!   change, cited row and excerpt, actor) and the change APPLIES at once.
//! - `passkey`: the change is stored as a PENDING grant (202 with the code and
//!   the exact sentence) and applies only with an approval proof
//!   ([`approval_proof_ok`], the one extension point a passkey follow-up
//!   plugs into): today the owner's Approve button from a NON-LOCAL Remote
//!   peer, or an owner send from one containing `amux grant <code>`.
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
//! refuses and names what changed. A worker that is running keeps its
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

/// The files a delegated config change can write, as they read now:
/// env files key by key, rules/memory text whole.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Snapshot {
    env: BTreeMap<String, BTreeMap<String, String>>,
    text: BTreeMap<String, String>,
    group_config: BTreeMap<String, Value>,
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
            }
        }
    }
    for dir in [home.join("memory"), home.join("memory").join("tags")] {
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
        if let Ok(mut st) = conn.prepare("SELECT name, department, goal, kpis, human_cost FROM group_config") {
            let rows = st.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, json!({"department": r.get::<_, String>(1)?, "goal": r.get::<_, String>(2)?,
                    "kpis": r.get::<_, String>(3)?, "human_cost": r.get::<_, i64>(4)?})))
            });
            if let Ok(rows) = rows {
                snap.group_config.extend(rows.flatten());
            }
        }
    }
    snap
}

/// What changed between two snapshots: per env key, per text file, per group
/// config row, each as {before, after}. A secret key is never stored.
fn diff(before: &Snapshot, after: &Snapshot) -> (Value, bool) {
    let mut env = Map::new();
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
        }
    }
    let mut text = Map::new();
    let tfiles: std::collections::BTreeSet<&String> = before.text.keys().chain(after.text.keys()).collect();
    for f in tfiles.into_iter().filter(|f| before.text.get(*f) != after.text.get(*f)) {
        text.insert(f.clone(), json!({"before": before.text.get(f), "after": after.text.get(f)}));
    }
    let mut groups = Map::new();
    let gnames: std::collections::BTreeSet<&String> = before.group_config.keys().chain(after.group_config.keys()).collect();
    for g in gnames.into_iter().filter(|g| before.group_config.get(*g) != after.group_config.get(*g)) {
        groups.insert(g.clone(), json!({"before": before.group_config.get(g), "after": after.group_config.get(g)}));
    }
    // Env files the change created: undo removes them again once empty.
    let created: Vec<&String> = after.env.keys().filter(|f| !before.env.contains_key(*f)).collect();
    (json!({"env": env, "text": text, "group_config": groups, "created": created}), undoable)
}

/// The mutating request being handled, and the grant it applies (if any).
#[derive(Clone)]
pub(crate) struct Captured {
    pub method: String,
    pub path: String,
    pub body: Value,
    armed: Arc<Mutex<Option<(String, Snapshot)>>>,
}

tokio::task_local! {
    static CAPTURED: Captured;
}

/// Middleware: buffer a mutating request to a delegable path so it can be
/// stored exactly, and finalize the grant the handler applied.
pub async fn capture(State(state): State<AppState>, req: Request<Body>, next: Next) -> Response {
    let mutating = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    let path = req.uri().path().to_string();
    if !mutating || !CAPTURE_PREFIXES.iter().any(|p| path.starts_with(p)) {
        return next.run(req).await;
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
    };
    let armed = captured.armed.clone();
    let req = Request::from_parts(parts, Body::from(bytes));
    let mut resp = CAPTURED.scope(captured, next.run(req)).await;
    let taken = armed.lock().ok().and_then(|mut a| a.take());
    if let Some((code, before)) = taken {
        finalize(&state, &code, &before, resp.status());
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
            g.cited.map(|x| x.row_id), g.cited.map(|x| x.excerpt.clone()), mode_str(mode()).to_string());
        let inserted = state.store.write(move |conn| {
            let n = conn.execute(
                "INSERT OR IGNORE INTO _amux_delegation_grants (code, actor, class, action, target, method, path, body, sentence, why, created_at, expires_at, status, evidence_row, cited_excerpt, mode) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
                rusqlite::params![row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7, row.8, row.9, created, created + GRANT_TTL_S, row.10, row.11, row.12, row.13],
            )?;
            Ok(crate::db::WriteOutcome { applied: n == 1, events: vec![] })
        });
        match inserted {
            Ok(o) if o.applied => return Ok((code, text)),
            Ok(_) => continue,
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
    tracing::info!(actor, action, target, grant = %code, cited_row = cited.row_id, cited_session = %cited.session,
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
}

const COLS: &str = "code, actor, class, action, target, method, path, body, sentence, status, expires_at, evidence_row, cited_excerpt, changes";

fn map_row(r: &rusqlite::Row) -> rusqlite::Result<GrantRow> {
    Ok(GrantRow {
        code: r.get(0)?, actor: r.get(1)?, class: r.get(2)?, action: r.get(3)?, target: r.get(4)?, method: r.get(5)?,
        path: r.get(6)?, body: r.get(7)?, sentence: r.get(8)?, status: r.get(9)?, expires_at: r.get(10)?, evidence_row: r.get(11)?,
        cited_excerpt: r.get(12)?, changes: r.get(13)?,
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
        json!({"sentence": g.sentence, "cited_excerpt": g.cited_excerpt, "status": status.as_u16(), "changes": changes, "undoable": undoable}), None);
    tracing::info!(grant = code, actor = %g.actor, status = status.as_u16(), undoable,
        verdict = if to == "applied" { "delegation_grant_applied" } else { "delegation_grant_failed" }, measured = true, n_considered = 1,
        "a grant's change ran");
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

fn not_applicable(state: &AppState, g: &GrantRow) -> Option<(&'static str, String)> {
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
fn restore(state: &AppState, changes: &Value) -> Result<usize, String> {
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
    for (g, ba) in changes["group_config"].as_object().into_iter().flatten() {
        if now_snap.group_config.get(g).cloned().unwrap_or(Value::Null) != ba["after"] {
            conflicts.push(format!("group_config:{g}"));
        }
    }
    if !conflicts.is_empty() {
        return Err(format!("changed since the grant applied: {}; undo by hand", conflicts.join(", ")));
    }
    let mut n = 0usize;
    for (f, keys) in changes["env"].as_object().into_iter().flatten() {
        let updates: Vec<(String, Option<String>)> = keys.as_object().into_iter().flatten()
            .map(|(k, ba)| (k.clone(), ba["before"].as_str().map(str::to_string))).collect();
        n += updates.len();
        super::session_verbs::EnvFile::merge_plain(&home.join(f), &updates).map_err(|e| format!("{f}: {e}"))?;
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
    for (g, ba) in changes["group_config"].as_object().into_iter().flatten() {
        let (g, before) = (g.clone(), ba["before"].clone());
        state.store.write(move |conn| {
            conn.execute("DELETE FROM group_config WHERE name=?1", [&g])?;
            if before.is_object() {
                conn.execute("INSERT INTO group_config (name, department, goal, kpis, human_cost, updated) VALUES (?1,?2,?3,?4,?5,?6)",
                    rusqlite::params![g, before["department"].as_str().unwrap_or(""), before["goal"].as_str().unwrap_or(""),
                        before["kpis"].as_str().unwrap_or("[]"), before["human_cost"].as_i64().unwrap_or(0), chrono::Utc::now().timestamp()])?;
            }
            Ok(crate::db::WriteOutcome { applied: true, events: vec![] })
        }).map_err(|e| e.to_string())?;
        n += 1;
    }
    Ok(n)
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
    let changes: Value = g.changes.as_deref().and_then(|c| serde_json::from_str(c).ok()).unwrap_or(Value::Null);
    match restore(&state, &changes) {
        Ok(n) => {
            if !set_status(&state, &g.code, "applied", "undone", &by, None) {
                return (StatusCode::CONFLICT, Json(super::delegation::refusal_body("grant_not_applied", "already undone"))).into_response();
            }
            log_transition(&state, &g.code, "undone", &by, if by == "owner" { "owner" } else { "mrpopo" }, &g.target,
                json!({"sentence": g.sentence, "restored": n, "changes": changes}), None);
            Json(json!({"ok": true, "grant_code": g.code, "undone": true, "restored": n})).into_response()
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
            "status": status, "expires_at": g.expires_at, "method": g.method, "path": g.path, "cited_row": g.evidence_row,
            "cited_excerpt": g.cited_excerpt, "body": serde_json::from_str::<Value>(&g.body).unwrap_or(Value::Null)})
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

    #[test]
    fn codes_are_read_from_a_typed_line_in_any_case() {
        assert_eq!(codes_in("ok — amux grant 7k3q please"), vec!["7K3Q".to_string()]);
        assert_eq!(codes_in("amux grant 7K3Q and amux  grant ABCD"), vec!["7K3Q".to_string(), "ABCD".to_string()]);
        assert!(codes_in("amux granted 7K3Q").is_empty());
        assert!(codes_in("amux grant 7K3QX").is_empty(), "five characters is not a code");
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
        std::fs::write(dir.join("sessions/test-dev-a.env"), "CC_DIR=\"/tmp\"\nCC_TAGS=\"dev\"\n").unwrap();
        std::fs::write(dir.join("sessions/mrpopo.env"), "CC_DIR=\"/tmp\"\nCC_TAGS=\"leads\"\n").unwrap();
        std::fs::write(dir.join("sessions/test-dev-lead.env"), "CC_DIR=\"/tmp\"\nCC_TAGS=\"leads,dev\"\nCC_ORCHESTRATOR=\"1\"\n").unwrap();
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
        let (session, text, ty, origin, delivery, po) = (session.to_string(), text.to_string(), ty.to_string(), origin.to_string(), delivery.to_string(), po.map(str::to_string));
        let id = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
        let idw = id.clone();
        state.store.write(move |c| {
            crate::api::session_verbs::ensure_fleet_tables(c)?;
            c.execute("INSERT INTO cmd_history(session,text,type,origin,delivery,process_origin,ts) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                rusqlite::params![session, text, ty, origin, delivery, po, (ts * 1000.0) as i64])?;
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

    /// Cite mode (the default): a valid citation of another lane's typed row
    /// applies the change at once as a grant, logged and fed to the leads, and
    /// undo restores the files exactly.
    #[tokio::test]
    async fn cite_mode_applies_at_once_and_undo_restores_exactly() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = crate::api::settings::test_env::set_home(tmp.path());
        let state = test_state(tmp.path());
        let app = super::super::router(state.clone());
        let before = config_pairs(&tmp.path().join("sessions/test-dev-a.env"));
        let row = pane(&state, "test-desk", "turn on yolo for test-dev-a");
        let (st, v, h) = call(&app, "PATCH", "/api/sessions/test-dev-a/config", Some("mrpopo"),
            json!({"toggle_yolo": true, "cite": {"row_id": row}, "why": "Max asked"}), None).await;
        assert!(st.is_success(), "{st} {v}");
        let code = h.get(GRANT_CODE_HEADER).and_then(|v| v.to_str().ok()).unwrap().to_string();
        let g = load(&state, &code).unwrap();
        assert_eq!((g.status.as_str(), g.evidence_row), ("applied", Some(row)));
        assert_eq!(g.cited_excerpt.as_deref(), Some("turn on yolo for test-dev-a"));
        assert!(yolo_on(tmp.path()));
        assert_eq!(logged(tmp.path(), "grant:applied"), 1);
        let line = std::fs::read_to_string(super::super::delegation_log::log_path(tmp.path())).unwrap();
        assert!(line.contains("turn on yolo for test-dev-a"), "the cited excerpt is logged: {line}");
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
        let row = pane(&state, "test-desk", "make test-dev-a isolated");
        let (st, v, h) = call(&app, "PATCH", "/api/sessions/test-dev-a/config", Some("mrpopo"), json!({"isolated": true, "cite": {"row_id": row}}), None).await;
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
        let own = pane(&state, "mrpopo", "turn on yolo for test-dev-a");
        let stamped = pane(&state, "test-desk", "[amux-origin: test-peer] turn on yolo for test-dev-a");
        let stale = put_row(&state, "test-desk", "turn on yolo", "user", "pane", "pane", None, now() - 90_000.0);
        let fwd = put_row(&state, "mrpopo", "turn on yolo", "user", "", "direct", Some("remote-forwarded"), now());
        for (cite, code) in [(json!(null), "cite_missing"), (json!({"row_id": own}), "max_ref_own_lane"), (json!({"row_id": stamped}), "max_ref_not_human_typed"),
            (json!({"row_id": stale}), "max_ref_stale"), (json!({"row_id": fwd}), "max_ref_own_lane")] {
            let (st, v, _) = call(&app, "PATCH", "/api/sessions/test-dev-a/config", Some("mrpopo"), json!({"toggle_yolo": true, "cite": cite}), None).await;
            assert_eq!((st, v["code"].clone()), (StatusCode::FORBIDDEN, json!(code)), "{cite}: {v}");
        }
        assert!(!yolo_on(tmp.path()));
        let n: i64 = state.store.read().unwrap().query_row("SELECT COUNT(*) FROM _amux_delegation_grants", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "a refused citation creates no grant");
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
