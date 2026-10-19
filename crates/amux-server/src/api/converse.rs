//! Converse: hands-free voice conversations with a worker (the worker view's
//! Converse tab).
//!
//! THE WORKER ANSWERS. The browser turns speech into text, sends each finished
//! utterance through the ordinary `POST /api/sessions/{name}/send` path and
//! reads the reply from `GET /api/sessions/{name}/last-message`, the same two
//! routes the composer and read-aloud use. Nothing here talks to a model; this
//! module only keeps the LOG of those conversations so any of them can be
//! replayed, and expires it.
//!
//! THE FOLDER IS THE STORE, like `recordings.rs`. One conversation is up to two
//! files under `<AMUX_HOME>/converse/<worker>/`:
//!
//! - `<id>.json`: title, engine, start/end, duration and every transcript turn
//!   (`role`, `text`, `t_ms` offset from the start).
//! - `<id>.<ext>`: the session audio the browser recorded with MediaRecorder,
//!   when it could. Optional; replay falls back to speaking the transcript.
//!
//! EXPIRY is computed, never stored: `started_at + AMUX_CONVERSE_RETENTION_DAYS`
//! (default 30), read on every request, so changing the setting re-dates every
//! conversation at once. An expired conversation stops being listed the moment
//! it expires and the `converse-retention` job deletes its files on its next
//! tick, logging one `verdict=` line per sweep.
//!
//! ENGINES. `openai` (default when `OPENAI_API_KEY` is set) is OpenAI's
//! Realtime API over browser WebRTC. This server mints a short-lived client
//! secret per conversation (`POST /api/converse/{worker}/realtime-session`)
//! carrying instructions built from the worker's own records (no model call)
//! and one function tool, `ask_worker`, which the browser answers by asking
//! the worker's chat companion (`<worker>@chat`, the Chat tab's lane). The API
//! key never reaches the page or the log. `browser` (Web Speech in,
//! speechSynthesis out, replies also from the chat companion) is the free
//! fallback. Neither engine ever uses the worker's terminal send path, so a
//! conversation never interrupts the worker.

use super::AppState;
use axum::body::Bytes;
use axum::extract::{Path as AxPath, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const RETENTION_KEY: &str = "AMUX_CONVERSE_RETENTION_DAYS";
pub const OPENAI_KEY: &str = "OPENAI_API_KEY";
pub const MODEL_KEY: &str = "AMUX_CONVERSE_REALTIME_MODEL";
pub const VOICE_KEY: &str = "AMUX_CONVERSE_REALTIME_VOICE";
pub const TRANSCRIBE_KEY: &str = "AMUX_CONVERSE_TRANSCRIBE_MODEL";
/// Overrides the OpenAI API base (tests point it at a local stub).
pub const BASE_KEY: &str = "AMUX_CONVERSE_OPENAI_BASE";
const DEFAULT_MODEL: &str = "gpt-realtime-2.1";
const DEFAULT_VOICE: &str = "marin";
const DEFAULT_TRANSCRIBE: &str = "gpt-4o-mini-transcribe";
const OPENAI_BASE: &str = "https://api.openai.com/v1";
/// A client secret only has to live long enough to open the WebRTC call.
const SECRET_TTL_SECS: u64 = 120;
const CONTEXT_MAX_CHARS: usize = 3000;
const DEFAULT_RETENTION_DAYS: u32 = 30;
const DAY_MS: i64 = 86_400_000;
/// A long conversation at opus bitrates is a few MB per 10 minutes; 200 MB is
/// hours of audio and still bounded.
const MAX_AUDIO_BYTES: usize = 200 * 1024 * 1024;
const MAX_TURNS: usize = 5_000;
const MAX_TURN_CHARS: usize = 40_000;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/converse/config", get(config))
        .route("/api/converse/sweep", post(sweep_now))
        .route("/api/converse/{worker}/realtime-session", post(realtime_session_route))
        .route(
            "/api/converse/{worker}/conversations",
            get(list).post(upsert),
        )
        .route(
            "/api/converse/{worker}/conversations/{id}",
            get(get_one).delete(delete_one),
        )
        .route(
            "/api/converse/{worker}/conversations/{id}/audio",
            get(get_audio)
                .put(put_audio)
                .layer(axum::extract::DefaultBodyLimit::disable()),
        )
}

// ---- paths and validation ---------------------------------------------------

pub(crate) fn store_root(home: &Path) -> PathBuf {
    home.join("converse")
}

fn valid_worker(w: &str) -> bool {
    !w.is_empty()
        && w.len() <= 128
        && !w.contains("..")
        && w.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && w
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@'))
}

fn valid_id(id: &str) -> bool {
    (6..=64).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({"error": msg.into()}))).into_response()
}

/// Retention in days, read live: server.env first, then the process env.
pub(crate) fn retention_days(home: &Path) -> (u32, &'static str) {
    match super::settings::effective_env(home, RETENTION_KEY)
        .and_then(|v| v.trim().parse::<u32>().ok())
    {
        Some(d) if d > 0 => (d.min(3650), "env"),
        _ => (DEFAULT_RETENTION_DAYS, "default"),
    }
}

fn expires_at(started_at: i64, days: u32) -> i64 {
    started_at.saturating_add(i64::from(days) * DAY_MS)
}

/// Whole days left, rounded up, so a conversation is "Expires in 1 day" until
/// the moment it is gone, and never "in 0 days" while still listed.
fn expires_in_days(expires: i64, now: i64) -> i64 {
    let left = expires - now;
    if left <= 0 {
        0
    } else {
        (left + DAY_MS - 1) / DAY_MS
    }
}

fn ext_for_mime(mime: &str) -> &'static str {
    let m = mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    match m.as_str() {
        "audio/webm" | "video/webm" => "webm",
        "audio/ogg" | "application/ogg" => "ogg",
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" | "audio/aac" | "video/mp4" => "m4a",
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        "audio/mpeg" => "mp3",
        _ => "bin",
    }
}

fn read_record(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v.is_object().then_some(v)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    crate::atomic_write::atomic_write(path, bytes).map(|_| ())
}

/// Add the computed fields every reader needs (expiry, audio URL).
fn decorate(mut v: Value, worker: &str, days: u32, now: i64) -> Value {
    let started = v["started_at"].as_i64().unwrap_or(0);
    let exp = expires_at(started, days);
    v["expires_at"] = json!(exp);
    v["expires_in_days"] = json!(expires_in_days(exp, now));
    v["retention_days"] = json!(days);
    if v["audio"].is_object() {
        let id = v["id"].as_str().unwrap_or("").to_string();
        v["audio"]["url"] = json!(format!(
            "/api/converse/{}/conversations/{}/audio",
            urlencode(worker),
            id
        ));
    }
    v
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn summary(v: &Value) -> Value {
    let turns = v["turns"].as_array().cloned().unwrap_or_default();
    let preview = turns
        .iter()
        .find_map(|t| t["text"].as_str().filter(|s| !s.trim().is_empty()))
        .unwrap_or("")
        .chars()
        .take(160)
        .collect::<String>();
    let mut s = v.clone();
    if let Some(o) = s.as_object_mut() {
        o.remove("turns");
    }
    s["turn_count"] = json!(turns.len());
    s["latency"] = latency_summary(&turns);
    s["preview"] = json!(preview);
    s
}

// ---- config -----------------------------------------------------------------

/// The OpenAI key, read server-side only. Never returned, never logged.
fn openai_key(home: &Path) -> Option<String> {
    super::settings::effective_env(home, OPENAI_KEY).map(|k| k.trim().to_string()).filter(|k| !k.is_empty())
}

fn knob(home: &Path, key: &str, dflt: &str) -> String {
    super::settings::effective_env(home, key)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| dflt.to_string())
}

async fn config() -> Response {
    let home = crate::config::amux_home();
    let (days, days_source) = retention_days(&home);
    let has_key = openai_key(&home).is_some();
    let model = knob(&home, MODEL_KEY, DEFAULT_MODEL);
    let voice = knob(&home, VOICE_KEY, DEFAULT_VOICE);
    Json(json!({
        "measured": true,
        "n_considered": 2,
        "engines": [
            {"id": "openai", "label": "OpenAI Realtime", "available": has_key, "default": true,
             "model": model, "voice": voice,
             "reason": if has_key {
                 format!("Speech to speech over WebRTC with {model}; billed per audio minute.")
             } else {
                 format!("{OPENAI_KEY} is not set in ~/.amux/server.env, so browser speech is used.")
             }},
            {"id": "browser", "label": "Browser speech", "available": true,
             "reason": "Free fallback: speech recognition and synthesis built into this browser."},
        ],
        "retention_days": days,
        "retention_source": days_source,
        "retention_key": RETENTION_KEY,
    }))
    .into_response()
}

// ---- OpenAI Realtime --------------------------------------------------------

/// What the voice model knows about the worker before it asks anything:
/// computed from amux's own records, no model call, bounded.
pub(crate) fn worker_context(conn: Option<&rusqlite::Connection>, worker: &str) -> String {
    fn clip(s: &str, n: usize) -> String {
        let t = s.split_whitespace().collect::<Vec<_>>().join(" ");
        if t.chars().count() > n { format!("{}...", t.chars().take(n).collect::<String>()) } else { t }
    }
    let mut out = vec![format!("Worker: {worker}")];
    let env_exists = super::session_verbs::env_path(worker).exists();
    if !env_exists {
        out.push("(No amux worker by this name exists; you have no worker context.)".to_string());
    } else {
        let env = super::session_verbs::parse_env(worker);
        let desc = env.get_or("CC_DESC", "");
        if !desc.trim().is_empty() {
            out.push(format!("What it does: {}", clip(desc, 400)));
        }
        if let Some(conn) = conn {
            let card: Option<(String, String, String)> = conn
                .query_row(
                    "SELECT id, COALESCE(title,''), COALESCE(desc,'') FROM issues WHERE session=?1 AND status='doing' \
                     AND deleted IS NULL AND COALESCE(archived,0)=0 ORDER BY updated DESC LIMIT 1",
                    [worker],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .ok();
            match card {
                Some((id, title, desc)) => out.push(format!("Current card: {id} {}. {}", clip(&title, 160), clip(&desc, 300))),
                None => out.push("Current card: none in progress.".to_string()),
            }
            if let Ok(mut st) = conn.prepare("SELECT text FROM cmd_history WHERE session=?1 ORDER BY id DESC LIMIT 3") {
                let recent: Vec<String> = st
                    .query_map([worker], |r| r.get::<_, String>(0))
                    .map(|it| it.flatten().map(|t| clip(&t, 200)).collect())
                    .unwrap_or_default();
                if !recent.is_empty() {
                    out.push(format!("Recent messages sent to it, newest first: {}", recent.join(" | ")));
                }
            }
        }
        let last = super::session_verbs::last_assistant_message(worker, 2000);
        if !last.trim().is_empty() {
            out.push(format!("Its last reply: {}", clip(&last, 600)));
        }
    }
    let s = out.join("\n");
    s.chars().take(CONTEXT_MAX_CHARS).collect()
}

pub(crate) fn instructions(worker: &str, context: &str) -> String {
    format!(
        "You are the voice of the amux worker \"{worker}\", talking with its owner hands-free. \
         Reply in short, natural spoken sentences with no markdown, lists or code. \
         The context below is all you know directly. For anything about the worker's real current \
         state, its files, code, board or progress, or to get it to do something, call the \
         ask_worker tool with one clear request. That goes to the worker's chat companion, which \
         can look deeper and relay to the worker without interrupting its current work. Say a \
         short filler such as \"Let me check\" before calling it. Never claim something was done \
         unless ask_worker said so.\n\nContext:\n{context}"
    )
}

pub(crate) fn ask_worker_tool() -> Value {
    json!({
        "type": "function",
        "name": "ask_worker",
        "description": "Ask the worker's chat companion a question or hand it a request. It answers with what it found or did, and relays to the worker only when needed, without interrupting it. Use for anything beyond the context you were given.",
        "parameters": {
            "type": "object",
            "properties": {"request": {"type": "string", "description": "The question or request, in plain words."}},
            "required": ["request"],
        },
    })
}

pub(crate) fn realtime_session(model: &str, voice: &str, transcribe: &str, instructions: &str) -> Value {
    json!({
        "expires_after": {"anchor": "created_at", "seconds": SECRET_TTL_SECS},
        "session": {
            "type": "realtime",
            "model": model,
            "instructions": instructions,
            "tools": [ask_worker_tool()],
            "tool_choice": "auto",
            "audio": {
                "input": {
                    "transcription": {"model": transcribe},
                    "turn_detection": {"type": "server_vad", "silence_duration_ms": 450,
                                       "create_response": true, "interrupt_response": true},
                },
                "output": {"voice": voice},
            },
        },
    })
}

/// Mint a short-lived client secret. Only `value`, `expires_at` and the
/// session id come back out; the API key never leaves this function, and an
/// upstream error is scrubbed of it before it is reported.
pub(crate) async fn mint(base: &str, key: &str, body: &Value) -> Result<Value, (u16, String)> {
    // The exact key, and any key-shaped token: OpenAI's own 401 echoes a
    // masked form ("sk-proj-****Dv0A"), and even its last four characters
    // have no business on the page.
    let scrub = |s: String| {
        let s = s.replace(key, "[redacted]");
        static KEYISH: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        KEYISH
            .get_or_init(|| regex::Regex::new(r"sk-[A-Za-z0-9_*\-]{4,}").expect("static key pattern"))
            .replace_all(&s, "[redacted]")
            .into_owned()
    };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| (0, e.to_string()))?;
    let r = client
        .post(format!("{base}/realtime/client_secrets"))
        .bearer_auth(key)
        .json(body)
        .send()
        .await
        .map_err(|e| (0, scrub(e.to_string())))?;
    let status = r.status().as_u16();
    let text = r.text().await.unwrap_or_default();
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if status >= 300 {
        let msg = v["error"]["message"].as_str().map(str::to_string).unwrap_or_else(|| text.chars().take(300).collect());
        return Err((status, scrub(msg)));
    }
    let Some(secret) = v["value"].as_str().filter(|s| !s.is_empty() && *s != key) else {
        return Err((status, "the client secret response had no value".to_string()));
    };
    Ok(json!({
        "client_secret": secret,
        "expires_at": v["expires_at"],
        "session_id": v["session"]["id"],
    }))
}

async fn realtime_session_route(State(state): State<AppState>, AxPath(worker): AxPath<String>) -> Response {
    if !valid_worker(&worker) {
        return err(StatusCode::BAD_REQUEST, "invalid worker name");
    }
    let home = crate::config::amux_home();
    let Some(key) = openai_key(&home) else {
        tracing::info!(worker = %worker, verdict = "realtime_no_key", measured = true, n_considered = 1,
            "[converse] OpenAI Realtime unavailable: {OPENAI_KEY} not set; client falls back to browser speech");
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({
            "ok": false, "fallback": "browser",
            "reason": format!("{OPENAI_KEY} is not set in ~/.amux/server.env."),
        }))).into_response();
    };
    let model = knob(&home, MODEL_KEY, DEFAULT_MODEL);
    let voice = knob(&home, VOICE_KEY, DEFAULT_VOICE);
    let transcribe = knob(&home, TRANSCRIBE_KEY, DEFAULT_TRANSCRIBE);
    let base = knob(&home, BASE_KEY, OPENAI_BASE);
    let store = state.store.clone();
    let w = worker.clone();
    let context = tokio::task::spawn_blocking(move || {
        let conn = store.read().ok();
        worker_context(conn.as_deref(), &w)
    })
    .await
    .unwrap_or_else(|_| format!("Worker: {worker}"));
    let body = realtime_session(&model, &voice, &transcribe, &instructions(&worker, &context));
    let started = std::time::Instant::now();
    match mint(&base, &key, &body).await {
        Ok(mut out) => {
            tracing::info!(worker = %worker, model = %model, voice = %voice, context_chars = context.chars().count(),
                mint_ms = started.elapsed().as_millis() as u64, verdict = "realtime_secret_minted",
                "[converse] OpenAI Realtime client secret minted (key not logged)");
            out["ok"] = json!(true);
            out["model"] = json!(model);
            out["voice"] = json!(voice);
            out["calls_url"] = json!(format!("{base}/realtime/calls"));
            out["context_chars"] = json!(context.chars().count());
            out["tool"] = json!("ask_worker");
            Json(out).into_response()
        }
        Err((status, message)) => {
            tracing::warn!(worker = %worker, upstream_status = status, verdict = "realtime_mint_failed",
                "[converse] OpenAI Realtime client secret could not be minted: {message}");
            (StatusCode::BAD_GATEWAY, Json(json!({
                "ok": false, "fallback": "browser", "upstream_status": status,
                "reason": format!("OpenAI refused the session ({status}): {message}"),
            }))).into_response()
        }
    }
}

// ---- list / read ------------------------------------------------------------

#[derive(Deserialize, Default)]
struct ListQuery {
    limit: Option<usize>,
}

struct Listing {
    rows: Vec<Value>,
    n_considered: usize,
    expired: usize,
    unreadable: usize,
}

fn list_dir(dir: &Path, worker: &str, days: u32, now: i64) -> std::io::Result<Listing> {
    let mut out = Listing { rows: vec![], n_considered: 0, expired: 0, unreadable: 0 };
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e),
    };
    for ent in rd.flatten() {
        let p = ent.path();
        if p.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        out.n_considered += 1;
        let Some(v) = read_record(&p) else {
            out.unreadable += 1;
            continue;
        };
        let v = decorate(v, worker, days, now);
        if v["expires_at"].as_i64().unwrap_or(0) <= now {
            out.expired += 1;
            continue;
        }
        out.rows.push(v);
    }
    out.rows
        .sort_by_key(|v| std::cmp::Reverse(v["started_at"].as_i64().unwrap_or(0)));
    Ok(out)
}

async fn list(AxPath(worker): AxPath<String>, Query(q): Query<ListQuery>) -> Response {
    if !valid_worker(&worker) {
        return err(StatusCode::BAD_REQUEST, "invalid worker name");
    }
    let home = crate::config::amux_home();
    let (days, days_source) = retention_days(&home);
    let dir = store_root(&home).join(&worker);
    let limit = q.limit.unwrap_or(200).clamp(1, 1000);
    let w = worker.clone();
    let now = now_ms();
    let res = tokio::task::spawn_blocking(move || list_dir(&dir, &w, days, now)).await;
    match res {
        Ok(Ok(l)) => {
            let total = l.rows.len();
            Json(json!({
                "worker": worker,
                "measured": true,
                "n_considered": l.n_considered,
                "n": total,
                "expired_pending_sweep": l.expired,
                "unreadable": l.unreadable,
                "retention_days": days,
                "retention_source": days_source,
                "conversations": l.rows.iter().take(limit).map(summary).collect::<Vec<_>>(),
            }))
            .into_response()
        }
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "worker": worker, "measured": false, "n_considered": 0,
                "why_unmeasured": format!("could not read the conversation folder: {e}"),
                "conversations": [],
            })),
        )
            .into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn get_one(AxPath((worker, id)): AxPath<(String, String)>) -> Response {
    if !valid_worker(&worker) || !valid_id(&id) {
        return err(StatusCode::NOT_FOUND, "no such conversation");
    }
    let home = crate::config::amux_home();
    let (days, _) = retention_days(&home);
    let path = store_root(&home).join(&worker).join(format!("{id}.json"));
    let now = now_ms();
    match read_record(&path).map(|v| decorate(v, &worker, days, now)) {
        Some(v) if v["expires_at"].as_i64().unwrap_or(0) > now => Json(v).into_response(),
        Some(_) => err(StatusCode::GONE, "this conversation has expired"),
        None => err(StatusCode::NOT_FOUND, "no such conversation"),
    }
}

// ---- write ------------------------------------------------------------------

fn clean_turns(v: &Value) -> Vec<Value> {
    v.as_array()
        .map(|a| {
            a.iter()
                .take(MAX_TURNS)
                .filter_map(|t| {
                    let text = t["text"].as_str()?.trim();
                    if text.is_empty() {
                        return None;
                    }
                    let role = match t["role"].as_str().unwrap_or("") {
                        "user" | "you" => "user",
                        "worker" | "assistant" => "worker",
                        _ => "system",
                    };
                    let mut turn = json!({
                        "role": role,
                        "text": text.chars().take(MAX_TURN_CHARS).collect::<String>(),
                        "t_ms": t["t_ms"].as_i64().unwrap_or(0).max(0),
                    });
                    // Utterance end to the first sound of this reply.
                    if let Some(ms) = t["first_audio_ms"].as_i64().filter(|m| (0..600_000).contains(m)) {
                        turn["first_audio_ms"] = json!(ms);
                    }
                    Some(turn)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Billed usage the browser measured: audio milliseconds each way and the
/// Realtime token counts from `response.done`. Numbers only.
fn clean_usage(v: &Value) -> Value {
    let Some(o) = v.as_object() else { return Value::Null };
    let keys = ["audio_in_ms", "audio_out_ms", "input_audio_tokens", "output_audio_tokens",
                "input_text_tokens", "output_text_tokens", "cached_tokens", "responses", "tool_calls"];
    let mut out = serde_json::Map::new();
    for k in keys {
        if let Some(n) = o.get(k).and_then(Value::as_i64).filter(|n| *n >= 0) {
            out.insert(k.to_string(), json!(n));
        }
    }
    if out.is_empty() { Value::Null } else { Value::Object(out) }
}

/// Median and worst first-audio latency over the replies that measured one.
pub(crate) fn latency_summary(turns: &[Value]) -> Value {
    let mut ms: Vec<i64> = turns.iter().filter_map(|t| t["first_audio_ms"].as_i64()).collect();
    if ms.is_empty() {
        return Value::Null;
    }
    ms.sort_unstable();
    json!({"n": ms.len(), "median_ms": ms[ms.len() / 2], "max_ms": ms[ms.len() - 1]})
}

fn short(s: Option<&str>, max: usize) -> String {
    s.unwrap_or("").trim().chars().take(max).collect()
}

/// Build the stored record from a client body, keeping fields the server owns
/// (`created_at`, `audio`) from any previous version.
fn build_record(worker: &str, id: &str, body: &Value, prev: Option<&Value>, now: i64) -> Value {
    let turns = clean_turns(&body["turns"]);
    // A client may name its own start (it knows when the mic opened), but not
    // a future one: expiry is derived from it.
    let started_at = body["started_at"]
        .as_i64()
        .or_else(|| prev.and_then(|p| p["started_at"].as_i64()))
        .unwrap_or(now)
        .min(now);
    let ended_at = body["ended_at"].as_i64().map(|e| e.clamp(started_at, now));
    let duration_ms = body["duration_ms"]
        .as_i64()
        .or_else(|| ended_at.map(|e| e - started_at))
        .unwrap_or_else(|| turns.last().and_then(|t| t["t_ms"].as_i64()).unwrap_or(0))
        .max(0);
    let mut title = short(body["title"].as_str(), 120);
    if title.is_empty() {
        title = turns
            .iter()
            .find(|t| t["role"] == "user")
            .and_then(|t| t["text"].as_str())
            .map(|s| s.chars().take(60).collect::<String>())
            .unwrap_or_else(|| "Conversation".to_string());
    }
    json!({
        "id": id,
        "worker": worker,
        "title": title,
        "engine": short(body["engine"].as_str(), 32).chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect::<String>(),
        "voice": short(body["voice"].as_str(), 120),
        "personality": short(body["personality"].as_str(), 40),
        "started_at": started_at,
        "ended_at": ended_at,
        "duration_ms": duration_ms,
        "turns": turns,
        "usage": clean_usage(&body["usage"]),
        "audio": prev.map(|p| p["audio"].clone()).unwrap_or(Value::Null),
        "rev": body["rev"].as_i64().unwrap_or(0),
        "created_at": prev.and_then(|p| p["created_at"].as_i64()).unwrap_or(now),
        "updated_at": now,
    })
}

async fn upsert(AxPath(worker): AxPath<String>, Json(body): Json<Value>) -> Response {
    if !valid_worker(&worker) {
        return err(StatusCode::BAD_REQUEST, "invalid worker name");
    }
    let id = match body["id"].as_str() {
        Some(s) if valid_id(s) => s.to_string(),
        Some(_) => return err(StatusCode::BAD_REQUEST, "id must be 6-64 of [A-Za-z0-9_-]"),
        None => format!("cv-{}", ulid::Ulid::new().to_string().to_ascii_lowercase()),
    };
    let home = crate::config::amux_home();
    let (days, _) = retention_days(&home);
    let path = store_root(&home).join(&worker).join(format!("{id}.json"));
    let now = now_ms();
    let w = worker.clone();
    let res = tokio::task::spawn_blocking(move || {
        let prev = read_record(&path);
        // A client revision older than the stored one is a late, stale save:
        // keep what is stored rather than roll it back.
        if let (Some(p), Some(rev)) = (prev.as_ref(), body["rev"].as_i64()) {
            if p["rev"].as_i64().is_some_and(|stored| rev < stored) {
                tracing::info!(id = %id, rev, stored = p["rev"].as_i64().unwrap_or(0),
                    verdict = "stale_save_ignored", "[converse] an out-of-order save was ignored");
                return Ok::<_, std::io::Error>((p.clone(), false));
            }
        }
        let rec = build_record(&w, &id, &body, prev.as_ref(), now);
        let bytes = serde_json::to_vec_pretty(&rec).map_err(std::io::Error::other)?;
        write_atomic(&path, &bytes)?;
        Ok::<_, std::io::Error>((rec, prev.is_none()))
    })
    .await;
    match res {
        Ok(Ok((rec, created))) => {
            if created {
                tracing::info!(worker = %worker, id = %rec["id"].as_str().unwrap_or(""),
                    engine = %rec["engine"].as_str().unwrap_or(""), "[converse] conversation started");
            }
            if rec["ended_at"].is_i64() {
                let u = &rec["usage"];
                let lat = latency_summary(rec["turns"].as_array().map(Vec::as_slice).unwrap_or(&[]));
                tracing::info!(worker = %worker, id = %rec["id"].as_str().unwrap_or(""),
                    engine = %rec["engine"].as_str().unwrap_or(""),
                    duration_ms = rec["duration_ms"].as_i64().unwrap_or(0),
                    audio_in_ms = u["audio_in_ms"].as_i64().unwrap_or(0),
                    audio_out_ms = u["audio_out_ms"].as_i64().unwrap_or(0),
                    input_audio_tokens = u["input_audio_tokens"].as_i64().unwrap_or(0),
                    output_audio_tokens = u["output_audio_tokens"].as_i64().unwrap_or(0),
                    first_audio_median_ms = lat["median_ms"].as_i64().unwrap_or(-1),
                    first_audio_n = lat["n"].as_i64().unwrap_or(0),
                    verdict = "conversation_ended", measured = true, n_considered = 1,
                    "[converse] conversation ended: usage and first-audio latency recorded");
            }
            let mut out = decorate(rec, &worker, days, now);
            out["created"] = json!(created);
            Json(out).into_response()
        }
        Ok(Err(e)) => {
            tracing::warn!(worker = %worker, "[converse] could not store conversation: {e}");
            err(StatusCode::INTERNAL_SERVER_ERROR, format!("could not store the conversation: {e}"))
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

fn audio_files(dir: &Path, id: &str) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    rd.flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_stem().and_then(|s| s.to_str()) == Some(id)
                && p.extension().and_then(|e| e.to_str()) != Some("json")
        })
        .collect()
}

async fn delete_one(AxPath((worker, id)): AxPath<(String, String)>) -> Response {
    if !valid_worker(&worker) || !valid_id(&id) {
        return err(StatusCode::NOT_FOUND, "no such conversation");
    }
    let dir = store_root(&crate::config::amux_home()).join(&worker);
    let key = id.clone();
    let removed = tokio::task::spawn_blocking(move || {
        let json_path = dir.join(format!("{key}.json"));
        let mut n = 0usize;
        if std::fs::remove_file(&json_path).is_ok() {
            n += 1;
        }
        for p in audio_files(&dir, &key) {
            if std::fs::remove_file(&p).is_ok() {
                n += 1;
            }
        }
        n
    })
    .await
    .unwrap_or(0);
    if removed == 0 {
        return err(StatusCode::NOT_FOUND, "no such conversation");
    }
    tracing::info!(worker = %worker, id = %id, files = removed, "[converse] conversation deleted");
    Json(json!({"ok": true, "id": id, "files_removed": removed})).into_response()
}

async fn put_audio(
    AxPath((worker, id)): AxPath<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !valid_worker(&worker) || !valid_id(&id) {
        return err(StatusCode::NOT_FOUND, "no such conversation");
    }
    if body.is_empty() {
        return err(StatusCode::BAD_REQUEST, "the recording is empty");
    }
    if body.len() > MAX_AUDIO_BYTES {
        return err(StatusCode::PAYLOAD_TOO_LARGE, format!("audio is over {} MB", MAX_AUDIO_BYTES >> 20));
    }
    let mime = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let has_worker_voice = headers
        .get("x-converse-worker-voice")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == "1");
    let home = crate::config::amux_home();
    let (days, _) = retention_days(&home);
    let dir = store_root(&home).join(&worker);
    let w = worker.clone();
    let now = now_ms();
    let res = tokio::task::spawn_blocking(move || {
        let json_path = dir.join(format!("{id}.json"));
        let Some(mut rec) = read_record(&json_path) else {
            return Err((StatusCode::NOT_FOUND, "save the conversation before its audio".to_string()));
        };
        for p in audio_files(&dir, &id) {
            let _ = std::fs::remove_file(p);
        }
        let ext = ext_for_mime(&mime);
        let file = format!("{id}.{ext}");
        write_atomic(&dir.join(&file), &body)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("could not store audio: {e}")))?;
        rec["audio"] = json!({
            "file": file,
            "mime": mime,
            "bytes": body.len(),
            "has_worker_voice": has_worker_voice,
        });
        rec["updated_at"] = json!(now);
        let bytes = serde_json::to_vec_pretty(&rec)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        write_atomic(&json_path, &bytes)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        Ok(rec)
    })
    .await;
    match res {
        Ok(Ok(rec)) => {
            tracing::info!(worker = %w, id = %rec["id"].as_str().unwrap_or(""),
                bytes = rec["audio"]["bytes"].as_u64().unwrap_or(0), "[converse] audio stored");
            Json(decorate(rec, &w, days, now)).into_response()
        }
        Ok(Err((s, m))) => err(s, m),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn get_audio(AxPath((worker, id)): AxPath<(String, String)>) -> Response {
    if !valid_worker(&worker) || !valid_id(&id) {
        return err(StatusCode::NOT_FOUND, "no such conversation");
    }
    let home = crate::config::amux_home();
    let (days, _) = retention_days(&home);
    let dir = store_root(&home).join(&worker);
    let Some(rec) = read_record(&dir.join(format!("{id}.json"))) else {
        return err(StatusCode::NOT_FOUND, "no such conversation");
    };
    if expires_at(rec["started_at"].as_i64().unwrap_or(0), days) <= now_ms() {
        return err(StatusCode::GONE, "this conversation has expired");
    }
    let Some(file) = rec["audio"]["file"].as_str().filter(|f| !f.contains('/') && !f.contains("..")) else {
        return err(StatusCode::NOT_FOUND, "this conversation has no recorded audio");
    };
    let mime = rec["audio"]["mime"].as_str().unwrap_or("application/octet-stream").to_string();
    match tokio::fs::read(dir.join(file)).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, mime),
                (header::CACHE_CONTROL, "private, max-age=3600".to_string()),
                (
                    header::CONTENT_DISPOSITION,
                    format!("inline; filename=\"{file}\""),
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => err(StatusCode::NOT_FOUND, format!("audio file missing: {e}")),
    }
}

// ---- retention sweep --------------------------------------------------------

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct SweepReport {
    pub measured: bool,
    pub n_considered: usize,
    pub removed: usize,
    pub files_removed: usize,
    pub bytes_freed: u64,
    pub kept: usize,
    pub unreadable: usize,
    pub retention_days: u32,
    pub verdict: &'static str,
    pub why_unmeasured: Option<String>,
}

/// Delete every conversation past retention under `root`. Pure filesystem, so
/// it is testable against a temp dir with an injected clock.
pub(crate) fn sweep_dir(root: &Path, days: u32, now: i64) -> SweepReport {
    let mut rep = SweepReport { retention_days: days, ..Default::default() };
    let workers = match std::fs::read_dir(root) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            rep.measured = true;
            rep.verdict = "no_store";
            return rep;
        }
        Err(e) => {
            rep.verdict = "unmeasured";
            rep.why_unmeasured = Some(format!("could not read {}: {e}", root.display()));
            return rep;
        }
    };
    rep.measured = true;
    let cutoff = std::time::SystemTime::now()
        .checked_sub(Duration::from_millis((i64::from(days) * DAY_MS) as u64));
    for wdir in workers.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
        let Ok(rd) = std::fs::read_dir(&wdir) else { continue };
        let entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        let mut live_ids = std::collections::HashSet::new();
        for p in entries.iter().filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json")) {
            rep.n_considered += 1;
            let id = p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
            let Some(rec) = read_record(p) else {
                rep.unreadable += 1;
                live_ids.insert(id);
                continue;
            };
            if expires_at(rec["started_at"].as_i64().unwrap_or(0), days) > now {
                rep.kept += 1;
                live_ids.insert(id);
                continue;
            }
            let mut freed = 0u64;
            let mut files = 0usize;
            for f in std::iter::once(p.clone()).chain(audio_files(&wdir, &id)) {
                let size = std::fs::metadata(&f).map(|m| m.len()).unwrap_or(0);
                if std::fs::remove_file(&f).is_ok() {
                    freed += size;
                    files += 1;
                }
            }
            rep.removed += 1;
            rep.files_removed += files;
            rep.bytes_freed += freed;
        }
        // Audio whose record is gone (a crash between the two writes) and is
        // itself older than retention.
        for p in entries.iter().filter(|p| {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            ext != "json" && !ext.starts_with("tmp-")
        }) {
            let id = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if live_ids.contains(id) {
                continue;
            }
            let old = match (std::fs::metadata(p).and_then(|m| m.modified()), cutoff) {
                (Ok(m), Some(c)) => m < c,
                _ => false,
            };
            if old {
                let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
                if std::fs::remove_file(p).is_ok() {
                    rep.files_removed += 1;
                    rep.bytes_freed += size;
                }
            }
        }
    }
    rep.verdict = if rep.removed > 0 || rep.files_removed > 0 { "removed" } else { "nothing_expired" };
    rep
}

/// One sweep with its log line. The `converse-retention` job and the
/// "Delete expired now" button both come through here, so both are logged.
pub async fn sweep(trigger: &'static str) -> SweepReport {
    let home = crate::config::amux_home();
    let (days, _) = retention_days(&home);
    let root = store_root(&home);
    let now = now_ms();
    let rep = tokio::task::spawn_blocking(move || sweep_dir(&root, days, now))
        .await
        .unwrap_or_else(|e| SweepReport {
            verdict: "unmeasured",
            why_unmeasured: Some(format!("sweep task failed: {e}")),
            retention_days: days,
            ..Default::default()
        });
    if rep.measured {
        tracing::info!(
            verdict = rep.verdict, measured = true, n_considered = rep.n_considered,
            removed = rep.removed, files_removed = rep.files_removed, bytes_freed = rep.bytes_freed,
            kept = rep.kept, unreadable = rep.unreadable, retention_days = days, trigger,
            "[converse] retention sweep verdict={} removed={} of {}", rep.verdict, rep.removed, rep.n_considered
        );
    } else {
        tracing::warn!(
            verdict = rep.verdict, measured = false, trigger,
            why = rep.why_unmeasured.as_deref().unwrap_or(""),
            "[converse] retention sweep could not run"
        );
    }
    rep
}

async fn sweep_now(State(_state): State<AppState>) -> Response {
    let rep = sweep("manual").await;
    let status = if rep.measured { StatusCode::OK } else { StatusCode::INTERNAL_SERVER_ERROR };
    (status, Json(json!(rep))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PR #48 review B1 class sweep: this site staged into `<stem>.tmp-<pid>`, opened
    /// following symlinks, so a link planted at that name took the write.
    #[cfg(unix)]
    #[test]
    fn a_planted_temp_link_cannot_take_this_sites_write() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "keep").unwrap();
        let target = dir.path().join("session.json");
        std::fs::write(&target, "old").unwrap();
        crate::atomic_write::plant_links(dir.path(), &[format!("session.tmp-{}", std::process::id())], &victim);
        write_atomic(&target, b"new").unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep", "the planted link was not written through");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
    }

    fn rec(dir: &Path, worker: &str, id: &str, started: i64, audio: bool) {
        let wdir = dir.join(worker);
        std::fs::create_dir_all(&wdir).unwrap();
        let mut v = json!({"id": id, "worker": worker, "started_at": started, "turns": []});
        if audio {
            std::fs::write(wdir.join(format!("{id}.webm")), b"abcd").unwrap();
            v["audio"] = json!({"file": format!("{id}.webm"), "mime": "audio/webm", "bytes": 4});
        }
        std::fs::write(wdir.join(format!("{id}.json")), v.to_string()).unwrap();
    }

    #[test]
    fn sweep_removes_only_expired_conversations_and_their_audio() {
        let tmp = tempfile::tempdir().unwrap();
        let now = 1_800_000_000_000i64;
        rec(tmp.path(), "w1", "old-conv-1", now - 31 * DAY_MS, true);
        rec(tmp.path(), "w1", "new-conv-1", now - 2 * DAY_MS, true);
        rec(tmp.path(), "w2", "edge-conv-1", now - 30 * DAY_MS + 1000, false);
        let rep = sweep_dir(tmp.path(), 30, now);
        assert!(rep.measured);
        assert_eq!(rep.n_considered, 3);
        assert_eq!(rep.removed, 1, "{rep:?}");
        assert_eq!(rep.files_removed, 2, "json and audio of the expired one");
        assert_eq!(rep.kept, 2);
        assert_eq!(rep.verdict, "removed");
        assert!(!tmp.path().join("w1/old-conv-1.json").exists());
        assert!(!tmp.path().join("w1/old-conv-1.webm").exists());
        assert!(tmp.path().join("w1/new-conv-1.webm").exists());
        // A second sweep has nothing to do and says so.
        let again = sweep_dir(tmp.path(), 30, now);
        assert_eq!(again.removed, 0);
        assert_eq!(again.verdict, "nothing_expired");
    }

    #[test]
    fn a_missing_store_is_measured_empty_not_unmeasured() {
        let tmp = tempfile::tempdir().unwrap();
        let rep = sweep_dir(&tmp.path().join("nope"), 30, now_ms());
        assert!(rep.measured);
        assert_eq!(rep.n_considered, 0);
        assert_eq!(rep.verdict, "no_store");
    }

    #[test]
    fn expiry_rounds_up_and_listing_hides_expired() {
        assert_eq!(expires_in_days(DAY_MS * 3, DAY_MS * 3 - 1), 1);
        assert_eq!(expires_in_days(DAY_MS * 3, DAY_MS), 2);
        assert_eq!(expires_in_days(DAY_MS, DAY_MS), 0);
        let tmp = tempfile::tempdir().unwrap();
        let now = 1_800_000_000_000i64;
        rec(tmp.path(), "w", "old-conv-2", now - 40 * DAY_MS, false);
        rec(tmp.path(), "w", "new-conv-2", now - DAY_MS, true);
        let l = list_dir(&tmp.path().join("w"), "w", 30, now).unwrap();
        assert_eq!(l.n_considered, 2);
        assert_eq!(l.expired, 1);
        assert_eq!(l.rows.len(), 1);
        assert_eq!(l.rows[0]["expires_in_days"], json!(29));
        assert_eq!(
            l.rows[0]["audio"]["url"],
            json!("/api/converse/w/conversations/new-conv-2/audio")
        );
    }

    #[test]
    fn records_are_cleaned_and_server_fields_are_kept() {
        let now = 1_800_000_000_000i64;
        let body = json!({
            "title": "", "engine": "browser", "started_at": now + 99_999,
            "turns": [
                {"role": "you", "text": "  hello there ", "t_ms": 0},
                {"role": "assistant", "text": "hi", "t_ms": 1200},
                {"role": "user", "text": "   ", "t_ms": 1300},
            ],
        });
        let prev = json!({"created_at": 5, "audio": {"file": "x.webm"}});
        let r = build_record("w", "abcdef", &body, Some(&prev), now);
        assert_eq!(r["started_at"], json!(now), "a future start is clamped to now");
        assert_eq!(r["title"], json!("hello there"));
        assert_eq!(r["turns"].as_array().unwrap().len(), 2);
        assert_eq!(r["turns"][0]["role"], json!("user"));
        assert_eq!(r["turns"][1]["role"], json!("worker"));
        assert_eq!(r["created_at"], json!(5));
        assert_eq!(r["audio"]["file"], json!("x.webm"));
        assert_eq!(r["duration_ms"], json!(1200));
    }

    /// A local stand-in for api.openai.com: answers the mint with a secret,
    /// or with an error that echoes the bearer token, to prove neither path
    /// lets the real key out.
    async fn stub_openai(echo_key_in_error: bool) -> String {
        use axum::routing::post as p;
        let app = axum::Router::new().route("/v1/realtime/client_secrets", p(move |headers: HeaderMap, Json(body): Json<Value>| async move {
            let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
            assert_eq!(body["session"]["type"], json!("realtime"));
            assert_eq!(body["session"]["tools"][0]["name"], json!("ask_worker"));
            if echo_key_in_error {
                (StatusCode::UNAUTHORIZED, Json(json!({"error": {"message": format!("bad credentials {auth}; Incorrect API key provided: sk-proj-********Dv0A.")}})))
            } else {
                (StatusCode::OK, Json(json!({"value": "ek_test_123", "expires_at": 1_800_000_000, "session": {"id": "sess_test"}})))
            }
        }));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        format!("http://{addr}/v1")
    }

    #[tokio::test]
    async fn the_mint_returns_the_client_secret_and_never_the_api_key() {
        let key = "sk-test-REALKEY-0123456789";
        let body = realtime_session("gpt-realtime-2.1", "marin", "gpt-4o-mini-transcribe", &instructions("w", "Worker: w"));
        let ok = mint(&stub_openai(false).await, key, &body).await.unwrap();
        assert_eq!(ok["client_secret"], json!("ek_test_123"));
        assert!(!ok.to_string().contains(key), "{ok}");
        let (status, msg) = mint(&stub_openai(true).await, key, &body).await.unwrap_err();
        assert_eq!(status, 401);
        assert!(!msg.contains(key), "upstream error must be scrubbed: {msg}");
        assert!(!msg.contains("sk-"), "a masked key echo is scrubbed too: {msg}");
        assert!(msg.contains("[redacted]"));
    }

    #[test]
    fn the_session_carries_bounded_context_and_one_tool() {
        let ctx = worker_context(None, "no-such-worker-xyz");
        assert!(ctx.contains("No amux worker by this name exists"));
        assert!(ctx.chars().count() <= CONTEXT_MAX_CHARS);
        let s = realtime_session("m", "v", "t", &instructions("no-such-worker-xyz", &ctx));
        assert_eq!(s["session"]["tools"].as_array().unwrap().len(), 1);
        assert_eq!(s["session"]["audio"]["input"]["turn_detection"]["interrupt_response"], json!(true));
        assert!(s["session"]["instructions"].as_str().unwrap().contains("ask_worker"));
        assert_eq!(latency_summary(&[json!({"first_audio_ms": 900}), json!({}), json!({"first_audio_ms": 300})]),
            json!({"n": 2, "median_ms": 900, "max_ms": 900}));
    }

    #[test]
    fn names_are_validated() {
        assert!(valid_worker("amux-frustrations"));
        assert!(valid_worker("w@chat"));
        assert!(!valid_worker("../etc"));
        assert!(!valid_worker("a/b"));
        assert!(!valid_worker(""));
        assert!(valid_id("cv-123456"));
        assert!(!valid_id("short"));
        assert!(!valid_id("has.dot.inside"));
        assert_eq!(ext_for_mime("audio/webm;codecs=opus"), "webm");
        assert_eq!(ext_for_mime("audio/mp4"), "m4a");
    }
}
