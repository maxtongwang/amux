// Structured grants: a widening change mrpopo asks for is stored, shown to Max as one exact sentence, and applied once on his approval — refs maxtongwang/ai-config#160.
// Replaces max_ref phrase matching (dev-lead, 2026-10-17): nothing is inferred from wording; Max approves an exact stored change.
//! FLOW.
//! 1. A change the table marks `NeedsGrant` (`delegation::decide`) is NOT
//!    applied. `delegation::authorize` calls [`request`], which stores the exact
//!    request (method, path, body) as a PENDING grant and answers 202 with
//!    `{grant_code, sentence}`: `amux grant 7K3Q: <the exact change>`.
//! 2. Approval applies EXACTLY the stored request, once:
//!    - TYPED: Max types or pastes a line containing `amux grant <code>`. Every
//!      Messages row passes through [`on_row`] when it is written; the row must
//!      qualify as the owner's own words for the grant's actor
//!      (`max_ref::row_qualifies`: human-typed, not an amux delivery, and in the
//!      actor's own lane only a Remote owner send).
//!    In passkey mode typed approvals are disabled; the Approve button must
//!    pass delegation_passkeys::approval_proof_ok with a grant-bound UV assertion.
//!    - APPROVE BUTTON (cite mode): `POST /api/delegation/grants/<code>/approve` by the
//!      owner over a Remote-verified connection (#7's process verdict; the
//!      owner token alone is readable by every same-user worker).
//! 3. [`apply`] flips `pending` to `applying` in one conditional UPDATE (so a
//!    replay finds nothing to flip), replays the stored request through the
//!    router as the actor with a server-only `x-amux-grant` header, and records
//!    `applied` or `failed`. `authorize` accepts that header only for an
//!    `applying` grant whose actor, action and target match.
//! 4. The owner may deny; a grant past `expires_at` never applies. Every
//!    transition (requested, approved, applied, failed, denied, expired) is a
//!    delegation-log entry, and the request is posted to the leads feed with
//!    its sentence.
//!
//! KNOWN LIMIT (ai-config#136): a same-user process typing into Max's pane
//! with raw `tmux send-keys` produces a row indistinguishable from his.
use super::AppState;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, Request, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

/// How long a pending grant can be approved.
pub(crate) const GRANT_TTL_S: f64 = 86_400.0;
/// Server-only: set on the internal replay of an approved grant; `enforce`
/// strips any copy that arrives over a socket.
pub(crate) const GRANT_HEADER: &str = "x-amux-grant";
/// Unambiguous code alphabet (no 0/O, 1/I/L, U).
const ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTVWXYZ";
const CODE_LEN: usize = 4;
/// A request body larger than this is not captured, so it cannot become a grant.
const CAPTURE_LIMIT: usize = 1 << 20;
/// The paths whose handlers can ask for a grant.
const CAPTURE_PREFIXES: [&str; 8] = [
    "/api/sessions/", "/api/workers", "/api/scope", "/api/groups/", "/api/settings/", "/api/config/", "/api/env/", "/api/alerts/config",
];

/// The mutating request being handled, for [`request`] to store verbatim.
#[derive(Clone, Debug)]
pub(crate) struct Captured {
    pub method: String,
    pub path: String,
    pub body: Value,
}

tokio::task_local! {
    static CAPTURED: Captured;
}

/// Middleware: buffer a mutating request to a delegable path so a grant can
/// store it exactly. Anything else passes through untouched.
pub async fn capture(req: Request<Body>, next: Next) -> Response {
    let mutating = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    let path = req.uri().path().to_string();
    if !mutating || !CAPTURE_PREFIXES.iter().any(|p| path.starts_with(p)) {
        return next.run(req).await;
    }
    let (parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, CAPTURE_LIMIT).await {
        Ok(b) => b,
        Err(e) => return (StatusCode::PAYLOAD_TOO_LARGE, Json(json!({"ok": false, "code": "body_too_large", "error": e.to_string()}))).into_response(),
    };
    let captured = Captured {
        method: parts.method.to_string(),
        path: parts.uri.path_and_query().map(|p| p.as_str().to_string()).unwrap_or(path),
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    };
    let req = Request::from_parts(parts, Body::from(bytes));
    CAPTURED.scope(captured, next.run(req)).await
}

fn captured() -> Option<Captured> {
    CAPTURED.try_with(Clone::clone).ok()
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
    let mut body = c.body.clone();
    if let Some(o) = body.as_object_mut() {
        o.remove("why");
    }
    let body = if body.is_null() { String::new() } else { format!(" {}", body) };
    let body: String = body.chars().take(400).collect();
    format!("amux grant {code}: {actor} asks to {action} on {target}: {} {}{body}", c.method, c.path)
}

fn forbid(code: &str, error: String) -> Response {
    (StatusCode::FORBIDDEN, Json(super::delegation::refusal_body(code, &error))).into_response()
}

/// Store a pending grant for the request being handled and answer 202 with
/// its code and sentence. Logged as `grant:requested` and posted to the leads.
pub(crate) fn request(state: &AppState, actor: &str, class: &str, action: &str, target: &str, why: Option<String>) -> Response {
    let Some(c) = captured() else {
        return forbid("grant_unavailable", format!("{action} needs Max's approval, but this request could not be stored as a grant (no captured body)"));
    };
    let created = now();
    for _ in 0..10 {
        let Some(code) = new_code() else { break };
        let text = sentence(&code, actor, action, target, &c);
        let row = (code.clone(), actor.to_string(), class.to_string(), action.to_string(), target.to_string(),
            c.method.clone(), c.path.clone(), c.body.to_string(), text.clone(), why.clone());
        let inserted = state.store.write(move |conn| {
            let n = conn.execute(
                "INSERT OR IGNORE INTO _amux_delegation_grants (code, actor, class, action, target, method, path, body, sentence, why, created_at, expires_at, status) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,'pending')",
                rusqlite::params![row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7, row.8, row.9, created, created + GRANT_TTL_S],
            )?;
            Ok(crate::db::WriteOutcome { applied: n == 1, events: vec![] })
        });
        match inserted {
            Ok(o) if o.applied => {
                log_transition(state, &code, "requested", actor, class, target, json!({"sentence": text, "method": c.method, "path": c.path, "body": c.body}), why);
                tracing::info!(actor, action, target, grant = %code, verdict = "delegation_grant_requested", measured = true, n_considered = 1,
                    "a widening change was stored as a pending grant for Max's approval");
                return (StatusCode::ACCEPTED, Json(json!({"ok": true, "pending": true, "grant_code": code, "sentence": text,
                    "expires_at": created + GRANT_TTL_S,
                    "how": format!("Max approves by typing `amux grant {code}` (in a lane other than {actor}'s), or with Approve in Settings > Delegated changes")}))).into_response();
            }
            Ok(_) => continue,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e.to_string()}))).into_response(),
        }
    }
    (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": "could not mint a grant code"}))).into_response()
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
}

const COLS: &str = "code, actor, class, action, target, method, path, body, sentence, status, expires_at, evidence_row";

fn map_row(r: &rusqlite::Row) -> rusqlite::Result<GrantRow> {
    Ok(GrantRow {
        code: r.get(0)?, actor: r.get(1)?, class: r.get(2)?, action: r.get(3)?, target: r.get(4)?, method: r.get(5)?,
        path: r.get(6)?, body: r.get(7)?, sentence: r.get(8)?, status: r.get(9)?, expires_at: r.get(10)?, evidence_row: r.get(11)?,
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

/// Why a grant cannot be applied now, or None.
fn not_applicable(state: &AppState, g: &GrantRow) -> Option<(&'static str, String)> {
    if expire_if_due(state, g) {
        return Some(("grant_expired", format!("grant {} expired; ask again", g.code)));
    }
    (g.status != "pending").then(|| ("grant_not_pending", format!("grant {} is {}, not pending; it applies once", g.code, g.status)))
}

/// Apply a pending grant: exactly the stored request, once. `by` names the
/// approval ("owner:typed:<row>" or "owner:approve").
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
    let (to, what) = if status.is_success() { ("applied", "applied") } else { ("failed", "failed") };
    let _ = set_status(state, &g.code, "applying", to, by, None);
    log_transition(state, &g.code, what, &g.actor, &g.class, &g.target, json!({"status": status.as_u16(), "response": out}), None);
    tracing::info!(grant = %g.code, actor = %g.actor, status = status.as_u16(), verdict = if status.is_success() { "delegation_grant_applied" } else { "delegation_grant_failed" },
        measured = true, n_considered = 1, "an approved grant was replayed");
    if status.is_success() { Ok(out) } else { Err(("grant_apply_failed", format!("the stored change answered {status}: {out}"))) }
}

/// `authorize`'s check on the replay: the request carries an `applying`
/// grant's code and is that grant's actor, action and target. Returns the
/// grant's evidence row.
pub(crate) fn claim_applying(state: &AppState, code: &str, actor: &str, action: &str, target: &str) -> Result<Option<i64>, String> {
    let g = load(state, code).ok_or_else(|| format!("no grant {code}"))?;
    if g.status != "applying" {
        return Err(format!("grant {code} is {}, not being applied", g.status));
    }
    if g.actor != actor || g.action != action || g.target != target {
        return Err(format!("grant {code} is for {} {} on {}, not {actor} {action} on {target}", g.actor, g.action, g.target));
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

/// Called for every Messages row as it is written: a qualifying row naming a
/// pending grant applies it.
pub(crate) async fn on_row(state: &AppState, row_id: i64, text: &str) {
    if super::delegation_passkeys::passkey_mode() { return; }
    for code in codes_in(text) {
        let Some(g) = load(state, &code) else {
            tracing::info!(row_id, grant = %code, verdict = "delegation_grant_unknown_code", measured = true, n_considered = 1,
                "a typed line named a grant code that does not exist");
            continue;
        };
        if let Err(r) = super::max_ref::row_qualifies(state, row_id, &g.actor) {
            tracing::warn!(row_id, grant = %code, actor = %g.actor, code = r.code(), verdict = "delegation_grant_row_not_qualifying",
                measured = true, n_considered = 1, "a line naming a grant did not qualify as Max's own words for this actor");
            continue;
        }
        if let Err((c, why)) = apply(state, &code, &format!("owner:typed:{row_id}"), Some(row_id)).await {
            tracing::warn!(row_id, grant = %code, code = c, why = %why, verdict = "delegation_grant_typed_not_applied", measured = true,
                n_considered = 1, "a qualifying typed approval did not apply its grant");
        }
    }
}

/// The owner Approve button's precondition: the owner (no worker identity,
/// not a member, the owner bearer when one is configured) over a connection
/// #7 attributed as Remote.
fn approve_refusal(state: &AppState, headers: &HeaderMap, uri: &Uri) -> Option<Response> {
    if let Some(r) = owner_refusal(state, headers, uri) {
        return Some(r);
    }
    let origin = headers.get(super::process_identity::PROCESS_ORIGIN_HEADER).and_then(|v| v.to_str().ok());
    (origin != Some("remote")).then(|| forbid("grant_approve_requires_remote",
        "Approve applies only from the owner over a Remote-verified connection (the phone over the tailnet); the owner token alone is readable by workers. Or type `amux grant <code>` in a lane".into()))
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

async fn approve(State(state): State<AppState>, uri: Uri, headers: HeaderMap, Path(code): Path<String>, body: Option<Json<Value>>) -> Response {
    if super::delegation_passkeys::passkey_mode() {
        if let Some(r) = owner_refusal(&state, &headers, &uri) { return r; }
        let proof = body.and_then(|Json(v)| serde_json::from_value(v).ok());
        let result = proof.ok_or_else(|| "a passkey assertion is required".to_string())
            .and_then(|proof| super::delegation_passkeys::approval_proof_ok(&state, &code, &headers, proof));
        if let Err(reason) = result {
            tracing::warn!(grant=%code, reason=%reason, verdict="delegation_passkey_approval_refused", measured=true, n_considered=1,
                "a passkey approval did not prove user verification");
            return forbid("grant_passkey_required", reason);
        }
    } else if let Some(r) = approve_refusal(&state, &headers, &uri) {
        return r;
    }
    match apply(&state, &code, "owner:approve", None).await {
        Ok(out) => Json(json!({"ok": true, "grant_code": code.to_ascii_uppercase(), "applied": true, "response": out})).into_response(),
        Err((c, why)) => (StatusCode::CONFLICT, Json(super::delegation::refusal_body(c, &why))).into_response(),
    }
}

async fn deny(State(state): State<AppState>, uri: Uri, headers: HeaderMap, Path(code): Path<String>) -> Response {
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
            "status": status, "expires_at": g.expires_at, "method": g.method, "path": g.path,
            "body": serde_json::from_str::<Value>(&g.body).unwrap_or(Value::Null)})
    }).collect();
    Json(json!({"grants": rows, "measured": true, "n_considered": rows.len()})).into_response()
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/delegation/grants", get(list))
        .route("/api/delegation/grants/{code}/approve", post(approve))
        .route("/api/delegation/grants/{code}/deny", post(deny))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn put_row(state: &AppState, session: &str, text: &str, process_origin: &str) -> i64 {
        let (session, text, po) = (session.to_string(), text.to_string(), process_origin.to_string());
        let id = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
        let idw = id.clone();
        state.store.write(move |c| {
            crate::api::session_verbs::ensure_fleet_tables(c)?;
            c.execute("INSERT INTO cmd_history(session,text,type,origin,delivery,process_origin,ts) VALUES(?1,?2,'user','','direct',?3,?4)",
                rusqlite::params![session, text, po, (now() * 1000.0) as i64])?;
            idw.store(c.last_insert_rowid(), std::sync::atomic::Ordering::SeqCst);
            Ok(crate::db::WriteOutcome { applied: true, events: vec![] })
        }).unwrap();
        id.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn put_grant(state: &AppState, code: &str) {
        let (code, t) = (code.to_string(), now());
        state.store.write(move |c| {
            c.execute(
                "INSERT INTO _amux_delegation_grants (code, actor, class, action, target, method, path, body, sentence, created_at, expires_at, status) \
                 VALUES (?1,'mrpopo','mrpopo','set_config:toggle_yolo','test-dev-a','PATCH','/api/sessions/test-dev-a/config','{\"toggle_yolo\":true}','s',?2,?3,'pending')",
                rusqlite::params![code, t, t + GRANT_TTL_S],
            )?;
            Ok(crate::db::WriteOutcome { applied: true, events: vec![] })
        }).unwrap();
    }

    /// The session rule on typed approvals, with the verdicts only a real
    /// socket produces: in mrpopo's OWN lane an Outside owner send does not
    /// approve (a worker reaches Outside with `launchctl submit`); a Remote one
    /// does, and applies the stored change.
    #[tokio::test]
    async fn in_the_actors_own_lane_only_a_remote_owner_send_approves() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = crate::api::settings::test_env::set_home(tmp.path());
        std::fs::create_dir_all(tmp.path().join("sessions")).unwrap();
        std::fs::write(tmp.path().join("sessions/test-dev-a.env"), "CC_DIR=\"/tmp\"\nCC_TAGS=\"dev\"\n").unwrap();
        std::fs::write(tmp.path().join("sessions/mrpopo.env"), "CC_DIR=\"/tmp\"\nCC_TAGS=\"leads\"\n").unwrap();
        let state = AppState {
            store: std::sync::Arc::new(crate::db::Store::open(&tmp.path().join("t.db")).unwrap()),
            started: std::time::Instant::now(),
            build_hash: "test".into(),
            auth_token: None,
            reconciled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        };
        put_grant(&state, "7K3Q");
        let outside = put_row(&state, "mrpopo", "amux grant 7K3Q", "outside");
        on_row(&state, outside, "amux grant 7K3Q").await;
        assert_eq!(load(&state, "7K3Q").unwrap().status, "pending", "own-lane Outside does not approve");
        let remote = put_row(&state, "mrpopo", "ok, amux grant 7K3Q", "remote");
        on_row(&state, remote, "ok, amux grant 7K3Q").await;
        let g = load(&state, "7K3Q").unwrap();
        assert_eq!(g.status, "applied", "own-lane Remote approves");
        assert_eq!(g.evidence_row, Some(remote));
        let env = std::fs::read_to_string(tmp.path().join("sessions/test-dev-a.env")).unwrap();
        assert!(env.contains("dangerously") || env.contains("CC_AUTO_CONTINUE=\"1\""), "the stored change applied: {env}");
    }

    #[test]
    fn approve_requires_a_remote_owner() {
        let tmp = tempfile::tempdir().unwrap();
        let state = AppState {
            store: std::sync::Arc::new(crate::db::Store::open(&tmp.path().join("t.db")).unwrap()),
            started: std::time::Instant::now(),
            build_hash: "test".into(),
            auth_token: None,
            reconciled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        };
        let uri: Uri = "/api/delegation/grants/AAAA/approve".parse().unwrap();
        let mut h = HeaderMap::new();
        assert!(approve_refusal(&state, &h, &uri).is_some(), "no process verdict");
        h.insert(super::super::process_identity::PROCESS_ORIGIN_HEADER, "outside".parse().unwrap());
        assert!(approve_refusal(&state, &h, &uri).is_some(), "Outside is not Remote");
        h.insert(super::super::process_identity::PROCESS_ORIGIN_HEADER, "remote".parse().unwrap());
        assert!(approve_refusal(&state, &h, &uri).is_none(), "Remote owner");
        h.insert("x-amux-session", "review-dev-a".parse().unwrap());
        assert!(approve_refusal(&state, &h, &uri).is_some(), "a worker never approves");
    }
}
