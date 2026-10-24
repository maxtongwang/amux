// Explicit approval identity and correlation — Max 2026-10-23, refs maxtongwang/ai-config#160.
//! A rendered owner Messages question or exact owner assistant turn opens a reply window. The very next
//! owner input may answer the latest display; any intervening owner input or
//! owner-session output closes it. cmd_history order is the authority; the
//! resolutions table records outcomes only.
use super::{delegation::{Action, Target, OWNER_AGENT}, max_ref, AppState};
use axum::{body::Body, extract::{Path, State}, http::{HeaderMap, Request, StatusCode}, response::{IntoResponse, Response}, Json};
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

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DisplayReceipt {
    surface: String,
    text: String,
    after_row: i64,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RenderStart { surface: String }

/// Close the previous window before a new question can reach the DOM. Without
/// this barrier a delayed render ACK could let a yes fund the previous display.
pub(crate) async fn prepare_display(State(state): State<AppState>, headers: HeaderMap, Json(input): Json<RenderStart>) -> Response {
    if let Some(r) = display_refusal(&headers, &input.surface) { return r; }
    let after = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
    let saved = after.clone();
    let result = state.store.write(move |conn| {
        conn.execute("INSERT INTO cmd_history(session,text,type,origin,delivery,ts) VALUES(?1,'Preparing owner Messages display','assistant',?1,'approval_render_pending',?2)", rusqlite::params![OWNER_AGENT,(crate::config::now_f64()*1000.0) as i64])?;
        saved.store(conn.last_insert_rowid(),std::sync::atomic::Ordering::SeqCst);
        Ok(crate::db::WriteOutcome { applied:true, events:vec![] })
    });
    match result {
        Ok(_) => {
            let after_row=after.load(std::sync::atomic::Ordering::SeqCst);
            tracing::info!(after_row, verdict="delegation_request_render_pending", measured=true, n_considered=1, "closed the old window before displaying owner Messages");
            Json(json!({"ok":true,"after_row":after_row})).into_response()
        },
        Err(e) => refusal("request_display_prepare_failed", &e.to_string()),
    }
}

fn display_refusal(headers: &HeaderMap, surface: &str) -> Option<Response> {
    if surface != "owner-messages" {
        return Some(refusal("request_display_surface_unsupported", "this surface has no approval render hook; open owner Messages"));
    }
    if !super::session_verbs::owner_input_process_origin(headers)
        .is_some_and(|p| matches!(p, "outside" | "remote" | "remote-forwarded")) {
        return Some(refusal("request_display_not_owner", "only the owner UI can record a rendered question"));
    }
    None
}

/// Called by the owner Messages renderer only after its expanded question is
/// visible. Reads, creation, terminal delivery and notification dispatch never
/// count as display. Repeat receipts cannot reopen a closed window.
pub(crate) async fn display(State(state): State<AppState>, Path(ask): Path<i64>, headers: HeaderMap, Json(input): Json<DisplayReceipt>) -> Response {
    if let Some(r) = display_refusal(&headers, &input.surface) { return r; }
    let Some(identity) = incarnation(OWNER_AGENT) else {
        return refusal("request_reply_identity_changed", "owner incarnation unavailable");
    };
    let at = (crate::config::now_f64() * 1000.0) as i64;
    let result = state.store.write(move |conn| {
        let mut st = conn.prepare("SELECT g.code,g.actor,g.shown_row FROM _amux_delegation_grants g JOIN cmd_history q ON q.id=g.ask_row WHERE g.ask_row=?1 AND g.reply_session=?2 AND g.reply_incarnation=?3 AND g.ask_text=?4 AND q.text=g.ask_text AND q.session=g.reply_session AND q.delivery='approval_request' AND q.delegation_session_incarnation=g.reply_incarnation AND (g.shown_row IS NULL OR EXISTS(SELECT 1 FROM cmd_history d WHERE d.id=g.shown_row AND d.delivery='approval_display' AND d.repeat_of=g.ask_row)) AND g.status='pending' AND g.resolution_code IS NULL AND g.expires_at>?5 AND g.ask_row<=?6 AND EXISTS(SELECT 1 FROM cmd_history p WHERE p.id=?6 AND p.session=g.reply_session AND p.delivery='approval_render_pending') AND NOT EXISTS(SELECT 1 FROM cmd_history h WHERE h.session=g.reply_session AND h.id>?6 AND COALESCE(h.delivery,'')<>'approval_display')")?;
        let request = st.query_row(rusqlite::params![ask, OWNER_AGENT, identity, input.text, at as f64 / 1000.0, input.after_row], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<i64>>(2)?)));
        drop(st);
        let (code, actor, shown) = match request {
            Ok(r) => r,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(crate::db::WriteOutcome { applied: false, events: vec![] }),
            Err(e) => return Err(e),
        };
        if shown.is_none() {
            conn.execute("INSERT INTO cmd_history(text,type,session,ts,origin,delivery,delegation_session_incarnation,repeat_of,delivered_at) VALUES(?1,'session',?2,?3,?4,'approval_display',?5,?6,?3)", rusqlite::params![input.text, OWNER_AGENT, at, actor, identity, ask])?;
            let displayed = conn.last_insert_rowid();
            conn.execute("UPDATE _amux_delegation_grants SET shown_row=?2,resolution_note='displayed in owner Messages; awaiting the very next owner input' WHERE code=?1", rusqlite::params![code,displayed])?;
            tracing::info!(request_id = code, ask_row = ask, shown_row = displayed, displayed_at_ms = at, surface = "owner-messages", verdict = "delegation_request_displayed", measured = true, n_considered = 1, "owner renderer recorded question display");
        }
        Ok(crate::db::WriteOutcome { applied: true, events: vec![] })
    });
    match result {
        Ok(o) if o.applied => Json(json!({"ok":true,"ask_row":ask,"surface":"owner-messages"})).into_response(),
        Ok(_) => refusal("request_display_ineligible", "question is missing, changed, refused, expired or outside the current owner incarnation"),
        Err(e) => refusal("request_display_failed", &e.to_string()),
    }
}

/// Record an output boundary before server-owned output reaches the owner.
/// One boundary per open display suffices; never store provider output/secrets.
pub(crate) fn output_boundary(conn: &rusqlite::Connection, session: &str) -> rusqlite::Result<()> {
    if session != OWNER_AGENT { return Ok(()); }
    let at = (crate::config::now_f64() * 1000.0) as i64;
    let n = conn.execute("INSERT INTO cmd_history(session,text,type,origin,delivery,ts) SELECT ?1,'Owner-session output closed the approval reply window','assistant',?1,'approval_output',?2 WHERE EXISTS(SELECT 1 FROM _amux_delegation_grants g WHERE g.reply_session=?1 AND g.status='pending' AND g.resolution_code IS NULL AND g.shown_row IS NOT NULL AND NOT EXISTS(SELECT 1 FROM cmd_history h WHERE h.session=?1 AND h.id>g.shown_row AND h.delivery='approval_output'))", rusqlite::params![session,at])?;
    if n > 0 {
        tracing::info!(session, verdict = "delegation_request_output_boundary", measured = true, n_considered = n, "owner-session output closed the approval window");
    }
    Ok(())
}

pub(crate) fn observe_output(state: &AppState, session: &str) -> Result<(), String> {
    let session = super::chat_worker::companion_parent(session).unwrap_or(session);
    if session != OWNER_AGENT { return Ok(()); }
    let session = session.to_string();
    state.store.write(move |conn| {
        output_boundary(conn, &session)?;
        Ok(crate::db::WriteOutcome { applied: true, events: vec![] })
    }).map(|_| ()).map_err(|e| e.to_string())
}

/// Applied native Stop only: the assistant row IS the pane receipt and ordering
/// boundary. Native sequence acceptance, this row and its binding commit together.
pub(crate) fn native_assistant_turn(conn: &rusqlite::Connection, session: &str, event: &Value, launch: &Value) -> rusqlite::Result<()> {
    if session != OWNER_AGENT || super::session_verbs::session_is_isolated(session) { return Ok(()); }
    let Some(text) = event["assistant_message"].as_str().filter(|s| !s.is_empty()) else {
        tracing::warn!(session, verdict="delegation_pane_text_unavailable", measured=false, n_considered=0,
            "native Stop has no complete assistant text; no pane receipt");
        return Ok(());
    };
    let identity = incarnation(session);
    let launch_identity = launch["incarnation"].as_str();
    if launch_identity.is_none() || launch_identity != identity.as_deref() {
        tracing::warn!(session, verdict="delegation_pane_identity_changed", measured=true, n_considered=1,
            "native launch is outside the current owner incarnation; no pane receipt");
        return Ok(());
    }
    let at = (event["event_ts"].as_f64().unwrap_or(0.0) * 1000.0) as i64;
    conn.execute("INSERT INTO cmd_history(session,text,type,origin,delivery,ts,delegation_session_incarnation) VALUES(?1,?2,'assistant',?1,'native_assistant',?3,?4)", rusqlite::params![session,text,at,identity])?;
    let turn = conn.last_insert_rowid();
    let matches: Vec<(String,i64)> = {
        let mut st = conn.prepare("SELECT g.code,g.ask_row FROM _amux_delegation_grants g JOIN cmd_history q ON q.id=g.ask_row WHERE g.reply_session=?1 AND g.reply_incarnation=?2 AND g.status='pending' AND (g.shown_row IS NULL OR EXISTS(SELECT 1 FROM cmd_history d WHERE d.id=g.shown_row AND d.delivery='approval_display' AND d.repeat_of=g.ask_row)) AND g.resolution_row IS NULL AND g.resolution_code IS NULL AND g.expires_at>?3 AND q.text=g.ask_text AND q.session=g.reply_session AND q.delivery='approval_request' AND q.delegation_session_incarnation=g.reply_incarnation AND length(g.ask_text)>0 AND instr(?4,g.ask_text)>0 AND q.id<?5 AND q.ts<=?6")?;
        let rows = st.query_map(rusqlite::params![OWNER_AGENT,identity,at as f64/1000.0,text,turn,at], |r| Ok((r.get(0)?,r.get(1)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    // One turn cannot be the unique shown_row of two requests. Fail closed.
    if matches.len() != 1 {
        tracing::warn!(session, assistant_row=turn, verdict="delegation_pane_no_exact_request", measured=true,
            n_considered=matches.len(), "assistant turn matched no single request awaiting pane display");
        return Ok(());
    }
    let (code,ask) = &matches[0];
    if !text.split(|c: char| c.is_whitespace() || c.is_ascii_punctuation()).any(|token| token == code.as_str()) {
        tracing::warn!(session, assistant_row=turn, verdict="delegation_pane_id_mismatch", measured=true,
            n_considered=1, "assistant turn lacks the exact request ID; no pane receipt");
        return Ok(());
    }
    conn.execute("UPDATE cmd_history SET delivery='approval_pane_display',repeat_of=?2,delivered_at=ts WHERE id=?1", rusqlite::params![turn,ask])?;
    conn.execute("UPDATE _amux_delegation_grants SET resolution_note='displayed in owner assistant turn; awaiting the very next owner input',shown_row=?2 WHERE code=?1", rusqlite::params![code,turn])?;
    tracing::info!(session, request_id=code, ask_row=ask, shown_row=turn, surface="owner-pane",
        verdict="delegation_request_displayed", measured=true, n_considered=1, "native assistant turn recorded pane display");
    Ok(())
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
/// No actor filter: only the latest owner event can identify a displayed request.
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
        // Read the ledger, not processed outcomes: missed hooks cannot leave a
        // window open. Peer deliveries are not owner input or owner output.
        let latest: i64 = conn.query_row("SELECT COALESCE(MAX(id),0) FROM cmd_history WHERE session=?1 AND id<?2 AND (delivery IN ('approval_display','approval_pane_display') OR (COALESCE(delegation_message_type,type) IN ('user','chat') AND ((origin='pane' AND delivery='pane') OR (origin<>'pane' AND delivery IN ('direct','queued') AND process_origin IN ('outside','remote','remote-forwarded')))) OR type='assistant' OR (type='session' AND origin=session AND delivery<>'approval_request'))", rusqlite::params![session, reply], |r| r.get(0))?;
        let candidates: Vec<(String, String, i64)> = {
            let mut st = conn.prepare("SELECT code,reply_incarnation,(SELECT ts FROM cmd_history WHERE id=shown_row) FROM _amux_delegation_grants WHERE reply_session=?1 AND shown_row=?2 AND shown_row<?3 AND status='pending' AND resolution_row IS NULL AND resolution_code IS NULL ORDER BY shown_row DESC")?;
            let rows = st.query_map(rusqlite::params![if owner_reply { session.as_str() } else { "" }, latest, reply], |r| Ok((r.get(0)?,r.get(1)?,r.get::<_, Option<i64>>(2)?.unwrap_or(i64::MAX))))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let (code, note) = if !owner_reply {
            ("request_reply_not_owner_session", "typed input outside the current owner session funds no request")
        } else if !affirmative {
            ("request_reply_not_affirmative", "reply is not an explicit affirmative; request remains pending")
        } else if candidates.is_empty() {
            ("request_no_candidate", "no eligible latest display immediately precedes this owner input")
        } else if identity.as_deref() != Some(candidates[0].1.as_str()) || identity != incarnation(&session) {
            ("request_reply_identity_changed", "reply session incarnation differs from the one shown the request")
        } else if ts < candidates[0].2 as f64 / 1000.0 || ts - candidates[0].2 as f64 / 1000.0 > max_ref::ASK_REPLY_WINDOW_S || ts > at || at - ts > max_ref::MAX_REF_MAX_AGE_S {
            ("request_reply_outside_window", "reply must follow the question in row and clock order within two hours and be fresh")
        } else {
            ("request_resolved", "explicit affirmative resolved this request; awaiting exact application")
        };
        let request = (code == "request_resolved").then(|| candidates[0].0.clone());
        conn.execute("INSERT INTO _amux_delegation_resolutions(reply_row,request_id,session,code,note,created_at) VALUES(?1,?2,?3,?4,?5,?6)", rusqlite::params![reply,request,session,code,note,at])?;
        for (id, _, _) in &candidates {
            conn.execute("UPDATE _amux_delegation_grants SET resolution_row=CASE WHEN ?2='request_resolved' THEN ?3 ELSE NULL END,resolution_code=?2,resolution_note=?4 WHERE code=?1 AND resolution_row IS NULL", rusqlite::params![id,code,reply,note])?;
        }
        if affirmative && code != "request_resolved" {
            tracing::warn!(reply_row = reply, session, code, note, verdict = "delegation_request_refused", measured = true, n_considered = candidates.len(), "owner reply funded no request");
        }
        tracing::info!(reply_row = reply, latest_owner_event_row = latest, session, request_id = ?request, code, note, verdict = "delegation_request_resolution", measured = true, n_considered = candidates.len(), "owner reply correlation recorded");
        Ok(crate::db::WriteOutcome { applied: true, events: vec![] })
    }).map(|_| ()).map_err(|e| e.to_string())
}

/// Hook path records outcomes; window boundaries come directly from history.
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

#[cfg(test)]
#[path = "delegation_reply_tests.rs"]
mod tests;
