// Explicit approval identity and correlation — Max 2026-10-23, refs maxtongwang/ai-config#160.
//! Request IDs are pending grant codes, not inferred questions. The server shows
//! the exact structured change in the owner session's Messages and retains its
//! row ID and stable incarnation. Asker-selected reply sessions are retired.
//! Each human reply closes a question window. One unanswered request shown in
//! that session/incarnation since the previous human reply can resolve; two
//! are ambiguous, zero is no candidate. Older windows never receive a later yes.
use super::{delegation::{Action, Target, OWNER_AGENT}, max_ref, AppState};
use axum::{body::Body, extract::State, http::{HeaderMap, Request, StatusCode}, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};

/// Maximum ledger rows inspected per reconciliation; a full scan refuses.
const RESOLUTION_SCAN_LIMIT: usize = 500;
tokio::task_local! { static DRAFT_SESSION: String; }

pub(crate) fn draft_session() -> Option<String> { DRAFT_SESSION.try_with(Clone::clone).ok() }

pub(crate) fn incarnation(name: &str) -> Option<String> {
    if matches!(name, "." | "..") || !super::session_verbs::valid_session_name(name) { return None; }
    super::rename_identity::incarnation(&crate::config::amux_home(), name)
}

/// Structured operands, including the effective direction of toggles. Metadata
/// is excluded; paths and methods are also compared at the captured boundary.
pub(crate) fn change(target: &Target, action: &Action, body: Option<&Value>) -> Value {
    let mut body = body.cloned().unwrap_or(Value::Null);
    if let Some(map) = body.as_object_mut() {
        for key in super::delegation::META_FIELDS { map.remove(key); }
    }
    let mut toggles = serde_json::Map::new();
    if let (Target::Worker(worker), Action::SetConfig { keys }) = (target, action) {
        let cfg = super::session_verbs::EnvFile::load(&crate::config::amux_home().join("sessions").join(format!("{worker}.env")));
        for key in keys.iter().filter(|k| matches!(k.as_str(), "toggle_yolo" | "toggle_auto_continue")) {
            toggles.insert(key.clone(), Value::Bool(!super::session_verbs::yolo_enabled(cfg.get_or("CC_FLAGS", ""), cfg.get("CC_AUTO_CONTINUE"))));
        }
    }
    json!({"target": target, "action": action, "body": body, "toggle_values": toggles})
}

/// Only routes inspected to abort at authorize before writing may be drafted.
/// Sends, lifecycle and batch env apply are deliberately absent.
fn draft_route(method: &str, path: &str) -> bool {
    let parts: Vec<_> = path.split('/').collect();
    let scoped = matches!(parts.as_slice(), ["", "api", "sessions" | "workers", _, "config" | "commit-guard"]
        | ["", "api", "groups", _, "config"]);
    (method == "PATCH" && (scoped || matches!(path, "/api/settings/default-model" | "/api/settings/commit-guard" | "/api/settings/task-guard" | "/api/alert/config")))
        || (method == "PUT" && matches!(path, "/api/scope" | "/api/config/apply"))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NewRequest {
    method: String,
    path: String,
    body: Value,
}

/// Derive permissions and operands through the actual handler. Draft scope
/// always returns from authorize without executing the mutation. No caller can
/// invent an action label that disagrees with the handler's action.
pub(crate) async fn create(State(state): State<AppState>, headers: HeaderMap, Json(input): Json<NewRequest>) -> Response {
    let actor = super::delegation::verified_origin(&headers);
    if actor.is_empty() || incarnation(&actor).is_none() || incarnation(OWNER_AGENT).is_none() {
        return refusal("request_identity_missing", "asking and owner sessions need existing stable incarnations");
    }
    if !draft_route(&input.method, &input.path) || !input.body.is_object() {
        return refusal("request_route_invalid", "use a supported config mutation path and an object body");
    }
    if input.body.get("request_id").is_some() || input.body.get("cite").is_some() {
        return refusal("request_metadata_invalid", "a new request cannot cite an existing approval");
    }
    let Ok(uri) = input.path.parse::<axum::http::Uri>() else { return refusal("request_route_invalid", "invalid mutation URI"); };
    let mut req = Request::builder().method(input.method.as_str()).uri(uri);
    *req.headers_mut().expect("validated request URI") = headers;
    // The draft is a distinct method/path, so it needs its own receipt. Keep
    // actor/auth headers: the real handler must still judge the same caller.
    let draft_headers = req.headers_mut().expect("validated request URI");
    let parent_interaction = draft_headers.remove("x-amux-interaction-id");
    draft_headers.remove("x-amux-command-kind");
    tracing::info!(verdict = "delegation_request_draft", parent_interaction_id = ?parent_interaction,
        method = input.method, path = input.path, "Deriving approval request with a separate interaction receipt");
    req.headers_mut().expect("validated request URI").remove(axum::http::header::CONTENT_LENGTH);
    req.headers_mut().expect("validated request URI").insert(axum::http::header::CONTENT_TYPE, axum::http::HeaderValue::from_static("application/json"));
    let Ok(req) = req.body(Body::from(input.body.to_string())) else { return refusal("request_route_invalid", "invalid mutation request"); };
    use tower::ServiceExt;
    DRAFT_SESSION.scope(OWNER_AGENT.to_string(), async {
        Box::pin(super::router(state).oneshot(req)).await.unwrap_or_else(|e| match e {})
    }).await
}

pub(crate) fn refusal(code: &str, note: &str) -> Response {
    tracing::warn!(code, verdict = "delegation_request_refused", measured = true, n_considered = 1, "approval identity resolution refused");
    (StatusCode::FORBIDDEN, Json(super::delegation::refusal_body(code, note))).into_response()
}

/// Replay the durable ledger in row order, so a missed hook/restart does not
/// change matching. Bounded reads fail closed; no truncated scan grants consent.
pub(crate) fn resolve_session(state: &AppState, session: &str) -> Result<(), String> {
    let ids: Vec<i64> = {
        let conn = state.store.read().map_err(|e| e.to_string())?;
        let mut st = conn.prepare("SELECT id FROM cmd_history WHERE session=?1 AND id >= (SELECT MIN(shown_row) FROM _amux_delegation_grants WHERE reply_session=?1 AND status='pending') AND COALESCE(delegation_message_type,type) IN ('user','chat') AND NOT EXISTS(SELECT 1 FROM _amux_delegation_resolutions dr WHERE dr.reply_row=cmd_history.id) ORDER BY id LIMIT ?2").map_err(|e| e.to_string())?;
        let rows = st.query_map(rusqlite::params![session, (RESOLUTION_SCAN_LIMIT + 1) as i64], |r| r.get(0)).map_err(|e| e.to_string())?;
        rows.collect::<rusqlite::Result<_>>().map_err(|e| e.to_string())?
    };
    if ids.len() > RESOLUTION_SCAN_LIMIT { return Err("request_resolution_scan_full".into()); }
    for id in ids { resolve_observed(state,id)?; }
    Ok(())
}

/// Resolving and recording candidates uses the serialized writer transaction.
/// No actor filter: every explicit question in the reply session participates.
fn resolve_reply(state: &AppState, row: &max_ref::Row) -> Result<(), String> {
    let (reply, session, identity, text, ts) = (row.id, row.session.clone(), row.incarnation.clone(), row.text.clone(), row.ts);
    let affirmative = max_ref::reply_is_affirmative(&text);
    let at = crate::config::now_f64();
    // Names locate the owner; only the captured stable incarnation can fund it.
    let owner_reply = session == OWNER_AGENT && identity.is_some() && identity == incarnation(OWNER_AGENT);
    state.store.write(move |conn| {
        if conn.query_row("SELECT EXISTS(SELECT 1 FROM _amux_delegation_resolutions WHERE reply_row=?1)", [reply], |r| r.get::<_, bool>(0))? {
            return Ok(crate::db::WriteOutcome { applied: false, events: vec![] });
        }
        // A processed owner reply permanently closes its window, whatever
        // later peer deliveries or message projections do to cmd_history.
        let previous: i64 = conn.query_row("SELECT COALESCE(MAX(reply_row),0) FROM _amux_delegation_resolutions WHERE session=?1 AND reply_row<?2", rusqlite::params![session, reply], |r| r.get(0))?;
        let candidates: Vec<(String, String, i64)> = {
            let mut st = conn.prepare("SELECT code,reply_incarnation,(SELECT ts FROM cmd_history WHERE id=shown_row) FROM _amux_delegation_grants WHERE reply_session=?1 AND shown_row>?2 AND shown_row<?3 AND status='pending' AND resolution_row IS NULL AND resolution_code IS NULL ORDER BY shown_row DESC")?;
            let rows = st.query_map(rusqlite::params![if owner_reply { session.as_str() } else { "" }, previous, reply], |r| Ok((r.get(0)?,r.get(1)?,r.get::<_, Option<i64>>(2)?.unwrap_or(i64::MAX))))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let (code, note) = if !owner_reply {
            ("request_reply_not_owner_session", "typed input outside the current owner session funds no request")
        } else if !affirmative {
            ("request_reply_not_affirmative", "reply is not an explicit affirmative; request remains pending")
        } else if candidates.is_empty() {
            ("request_no_candidate", "no unanswered request was shown in this reply session since Max's previous reply")
        } else if candidates.len() != 1 {
            ("request_ambiguous", "multiple unanswered requests were shown since Max's previous reply; none is approved, ask separately")
        } else if identity.as_deref() != Some(candidates[0].1.as_str()) || identity != incarnation(&session) {
            ("request_reply_identity_changed", "reply session incarnation differs from the one shown the request")
        } else if ts < candidates[0].2 as f64 / 1000.0 || ts - candidates[0].2 as f64 / 1000.0 > max_ref::ASK_REPLY_WINDOW_S || ts > at || at - ts > max_ref::MAX_REF_MAX_AGE_S {
            ("request_reply_outside_window", "reply must follow the question in row and clock order within two hours and be fresh")
        } else {
            ("request_resolved", "explicit affirmative resolved this request; awaiting exact application")
        };
        let request = (code == "request_resolved").then(|| candidates[0].0.clone());
        conn.execute("INSERT INTO _amux_delegation_resolutions(reply_row,request_id,session,code,note,created_at) VALUES(?1,?2,?3,?4,?5,?6)", rusqlite::params![reply,request,session,code,note,at])?;
        for (id, _, _) in candidates.iter().filter(|(id, _, _)| request.as_ref().is_none_or(|selected| selected == id)) {
            conn.execute("UPDATE _amux_delegation_grants SET resolution_row=CASE WHEN ?2='request_resolved' THEN ?3 ELSE NULL END,resolution_code=?2,resolution_note=?4 WHERE code=?1 AND resolution_row IS NULL", rusqlite::params![id,code,reply,note])?;
        }
        if affirmative && code != "request_resolved" {
            tracing::warn!(reply_row = reply, session, code, note, verdict = "delegation_request_refused", measured = true, n_considered = candidates.len(), "owner reply funded no request");
        }
        tracing::info!(reply_row = reply, previous_reply_row = previous, session, request_id = ?request, code, note, verdict = "delegation_request_resolution", measured = true, n_considered = candidates.len(), "owner reply correlation recorded");
        Ok(crate::db::WriteOutcome { applied: true, events: vec![] })
    }).map(|_| ()).map_err(|e| e.to_string())
}

/// Hook path also records a no-candidate reply when no request exists. The
/// stored owner resolution, never a reclassified message, closes the window.
pub(crate) fn resolve_observed(state: &AppState, id: i64) -> Result<(), String> {
    let row = max_ref::one(state, id).map_err(|e| e.code().to_string())?.ok_or("request_reply_missing")?;
    if max_ref::typed_by_max(state, &row).is_none() {
        if row.session == OWNER_AGENT && max_ref::reply_is_affirmative(&row.text) {
            tracing::warn!(reply_row = id, session = row.session, code = "request_reply_not_owner_input",
                verdict = "delegation_request_refused", measured = true, n_considered = 0,
                "peer or echoed affirmative has no owner provenance; no window closed");
        }
        return Ok(());
    }
    resolve_reply(state,&row)
}
