//! The delegated-change log and its feed (maxtongwang/ai-config#160, piece c).
//!
//! When a lead, mrpopo or a worker changes configuration on the owner's behalf
//! (an `Allow` from `api::delegation::decide` that is not the owner), the change
//! is written here: one JSON line per change, before and after, in
//! `<amux_home>/audit/delegated-changes.jsonl`. Max's words were "Automation is
//! key and losing it is not the way even for security reasons", so the answer to
//! delegating a change is that it stays visible, not that it is refused.
//!
//! - APPEND-ONLY. Opened with `O_APPEND`, mode 0600 at creation, one line per
//!   change, fsynced before `record` returns. There is no API path that edits or
//!   deletes an entry: the only route is `GET /api/delegation/log`. The writer
//!   follows `vault::audit_line`'s JSONL shape and adds what that one lacks
//!   (mode, fsync, a process lock so concurrent lines never interleave).
//! - REDACTED BEFORE IT IS WRITTEN. Any object key that names a secret
//!   (`registry::is_secret_env`, the same name test `/api/system-jobs` uses) has
//!   its value replaced with `"<redacted>"`, at every depth.
//! - READ BY THE OWNER AND VERIFIED LEADS. Other workers and invited members get
//!   403. Newest first, with `measured` / `n_considered` (diagnostic contract).
//! - FED TO THE LEADS. Each change is posted to every other `leads` member
//!   through the automated delivery seam (`steer_enqueue`, guard
//!   [`FEED_GUARD`], empty sender): stamped as amux automation, never as the
//!   owner (who sends with an empty guard) and never as a worker.
//! - REFUSALS ARE NOT CHANGES and are not written here. A refused request is
//!   already logged as a WARN with its verdict code at the refusal site; this
//!   file records only what changed, so its line count is the change count.

// `record` is called by api::delegation (piece a). Remove this allow when that
// lands; until then only the route and the tests reach the writer.

use crate::api::AppState;
use crate::config::amux_home;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

pub(crate) use super::max_ref::MaxRef;

/// One change made on the owner's behalf.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct DelegatedChange {
    pub ts: f64,
    pub actor: String,
    /// `"lead:<team>"`, `"mrpopo"` or `"self"`.
    pub class: String,
    pub action: String,
    pub target: String,
    pub before: Value,
    pub after: Value,
    pub max_ref: Option<MaxRef>,
    pub evidence_row: Option<i64>,
    pub why: Option<String>,
}

/// What a secret value is replaced with.
pub(crate) const REDACTED: &str = "<redacted>";

/// The steering guard the leads feed is queued under. A non-empty guard is what
/// marks a queue row as amux automation rather than the owner's own send.
pub(crate) const FEED_GUARD: &str = "delegation-log";


/// The log file, under the given amux home.
pub(crate) fn log_path(home: &Path) -> PathBuf {
    home.join("audit").join("delegated-changes.jsonl")
}

/// Replace the value of every secret-named key with [`REDACTED`], recursively.
/// A `{"key": <secret name>, "value": ...}` pair (the shape a single env or
/// vault write takes) has its `value` redacted too.
pub(crate) fn redact(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let names_secret = map
                .get("key")
                .and_then(Value::as_str)
                .is_some_and(crate::api::delegation::is_secret_key);
            Value::Object(
                map.iter()
                    .map(|(k, val)| {
                        let secret = crate::api::delegation::is_secret_key(k)
                            || (names_secret && k == "value");
                        (k.clone(), if secret { json!(REDACTED) } else { redact(val) })
                    })
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(redact).collect()),
        other => other.clone(),
    }
}

/// Serialises appends within this process, so two concurrent records can never
/// interleave bytes even where a platform splits a large `O_APPEND` write.
static APPEND_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn append(path: &Path, change: &DelegatedChange) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let mut line = serde_json::to_vec(change).map_err(std::io::Error::other)?;
    line.push(b'\n');
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    let _guard = APPEND_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let mut f = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(path)?;
    f.write_all(&line)?;
    f.sync_all()
}

/// Redact, append one line, then post the change to the leads. The append is
/// durable before this returns; the feed is queued on the runtime and a failed
/// delivery is a WARN, never a lost entry.
pub(crate) fn record(state: &AppState, change: DelegatedChange) -> std::io::Result<()> {
    let change = DelegatedChange { before: redact(&change.before), after: redact(&change.after), ..change };
    if let Err(e) = append(&log_path(&amux_home()), &change) {
        tracing::error!(actor = %change.actor, action = %change.action, target = %change.target, error = %e,
            verdict = "delegated_change_unrecorded", measured = true, n_considered = 1,
            "could not append a delegated change to the audit log");
        return Err(e);
    }
    tracing::info!(actor = %change.actor, class = %change.class, action = %change.action, target = %change.target,
        verdict = "delegated_change_recorded", measured = true, n_considered = 1, "recorded a delegated change");
    publish(state, &change);
    Ok(())
}

/// One line, no control characters, so a field cannot forge a second line.
fn flat(s: &str, max: usize) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).take(max).collect()
}

/// The leads feed line: `delegated: <actor> <action> <target> (why)`.
pub(crate) fn feed_text(c: &DelegatedChange) -> String {
    let why = c.why.as_deref().map(|w| format!(" ({})", flat(w, 200))).unwrap_or_default();
    format!("delegated: {} {} {}{why}", flat(&c.actor, 64), flat(&c.action, 64), flat(&c.target, 120))
}

fn publish(state: &AppState, change: &DelegatedChange) {
    let Ok(rt) = tokio::runtime::Handle::try_current() else {
        tracing::warn!(actor = %change.actor, verdict = "delegation_feed_no_runtime", measured = false,
            "delegated change recorded but the leads feed was not queued: no async runtime");
        return;
    };
    let (state, actor, text) = (state.clone(), change.actor.clone(), feed_text(change));
    rt.spawn(async move {
        publish_feed(&state, &actor, &text).await;
    });
}

/// Queue `text` for every `leads` member except the actor. Returns how many
/// were queued.
pub(crate) async fn publish_feed(state: &AppState, actor: &str, text: &str) -> usize {
    let members = super::history::group_members(&amux_home(), super::session_verbs::LEADS_GROUP);
    let mut sent = 0usize;
    for lead in members.iter().filter(|m| m.as_str() != actor) {
        match super::session_verbs::steer_enqueue(state, lead, text, FEED_GUARD, "").await {
            Ok(_) => sent += 1,
            Err(why) => tracing::warn!(lead = %lead, why, verdict = "delegation_feed_refused", measured = true,
                n_considered = 1, "a lead did not receive a delegated-change notice"),
        }
    }
    tracing::info!(sent, verdict = "delegation_feed_published", measured = true, n_considered = members.len(),
        "posted a delegated change to the leads");
    sent
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route("/api/delegation/log", get(get_log))
}

fn forbidden(code: &'static str, error: String) -> Response {
    (StatusCode::FORBIDDEN, Json(json!({"ok": false, "code": code, "error": error}))).into_response()
}

/// `None` when the caller may read the log: the owner, or a verified worker in
/// the `leads` group.
/// The claimed worker is proven (in-process, process attribution, or its
/// valid rule-10 token), as stamped by `worker_identity::enforce`.
fn reader_proven(headers: &HeaderMap, worker: &str) -> bool {
    super::worker_identity::identity_proven(headers, worker)
}

fn read_refusal(state: &AppState, headers: &HeaderMap, uri: &Uri) -> Option<Response> {
    let refuse = |code: &'static str, who: &str, error: String| {
        tracing::warn!(who, verdict = code, measured = true, n_considered = 1, "refused a delegation log read");
        Some(forbidden(code, error))
    };
    match super::worker_identity::verified_worker(headers) {
        Err(e) => refuse("worker_identity_invalid", "unverified", format!("unusable worker identity: {e:?}")),
        // A GET skips rule 10's token check, so a worker reader must be
        // proven here: its token, or process attribution naming it.
        Ok(Some(w)) if !reader_proven(headers, &w) => refuse("delegation_log_identity_unproven", &w,
            format!("'{w}' must present its worker token or come from its own pane to read the delegated-change log")),
        Ok(Some(w)) if w == super::delegation::MRPOPO || super::delegation::is_lead(&w) => None,
        Ok(Some(w)) => refuse("delegation_log_leads_only", &w,
            format!("'{w}' is not in the 'leads' group; the delegated-change log is for the owner and leads")),
        Ok(None) if super::org::is_verified_local_member(headers) => refuse("delegation_log_leads_only", "member",
            "an invited member cannot read the delegated-change log".into()),
        Ok(None) if state.auth_token.is_some() && !super::auth::has_owner_token(state, headers, uri) => {
            refuse("delegation_log_owner_credential_missing", "no-owner-credential",
                "send the owner bearer (the dashboard does) or a lead's worker identity".into())
        }
        Ok(None) => None,
    }
}

/// What stays in the owner's hands, served with the log so the dashboard's
/// help text has one source: the table's own hands-only list
/// (`delegation::HELD_FOR_MAX`) after secret values.
fn hands_only() -> Value {
    let mut out = vec![json!({"what": "secret values", "why": "a credential is Max's to set; delegating one would hand over what it unlocks"})];
    out.extend(super::delegation::HELD_FOR_MAX.iter().map(|(what, why)| json!({"what": what, "why": why})));
    json!(out)
}

/// `GET /api/delegation/log?limit=N`: newest first, `limit` 1..=1000 (200).
async fn get_log(
    State(state): State<AppState>,
    uri: Uri,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if let Some(r) = read_refusal(&state, &headers, &uri) {
        return r;
    }
    let limit = q.get("limit").and_then(|v| v.parse::<usize>().ok()).unwrap_or(200).clamp(1, 1000);
    let path = log_path(&amux_home());
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            let body = super::measured::unmeasured(json!({"entries": [], "limit": limit}),
                &format!("could not read the delegated-change log: {e}"));
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response();
        }
    };
    let (mut n_considered, mut unparsed) = (0usize, 0usize);
    let mut entries: Vec<Value> = Vec::new();
    for l in text.lines().rev() {
        let Ok(v) = serde_json::from_str::<Value>(l) else {
            unparsed += usize::from(!l.trim().is_empty());
            continue;
        };
        n_considered += 1;
        if entries.len() < limit {
            entries.push(v);
        }
    }
    Json(super::measured::measured(
        json!({"entries": entries, "limit": limit, "unparsed_lines": unparsed, "hands_only": hands_only()}),
        n_considered,
    ))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Store;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    /// The whole hard-isolation set, restored on drop. `set_home` holds the
    /// shared env LOCK for the guard's lifetime, so these mutations are
    /// race-free against every other home-setting test.
    struct Iso {
        _home: crate::api::settings::test_env::HomeGuard,
        prev: Vec<(&'static str, Option<std::ffi::OsString>)>,
        dir: tempfile::TempDir,
    }
    impl Drop for Iso {
        fn drop(&mut self) {
            for (k, v) in self.prev.drain(..) {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    fn iso() -> Iso {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        std::fs::create_dir_all(home.join("sessions")).unwrap();
        std::fs::create_dir_all(home.join("home")).unwrap();
        let guard = crate::api::settings::test_env::set_home(&home);
        let dead = std::ffi::OsString::from("http://127.0.0.1:1");
        let vars: [(&'static str, Option<std::ffi::OsString>); 7] = [
            ("HOME", Some(home.join("home").into_os_string())),
            ("CC_HOME", Some(home.as_os_str().to_owned())),
            ("TMUX", None),
            ("TMUX_PANE", None),
            ("AMUX_API", Some(dead.clone())),
            ("AMUX_URL", Some(dead)),
            ("AMUX_TEST_ISOLATED", Some("1".into())),
        ];
        let prev = vars
            .iter()
            .map(|(k, v)| {
                let old = std::env::var_os(k);
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
                (*k, old)
            })
            .collect();
        Iso { _home: guard, prev, dir }
    }

    impl Iso {
        fn home(&self) -> &Path {
            self.dir.path()
        }
        fn worker(&self, name: &str, tags: &str) {
            std::fs::write(self.home().join("sessions").join(format!("{name}.env")), format!("CC_TAGS={tags}\n"))
                .unwrap();
        }
        fn lines(&self) -> Vec<Value> {
            std::fs::read_to_string(log_path(self.home()))
                .unwrap_or_default()
                .lines()
                .map(|l| serde_json::from_str(l).expect("every line is one JSON change"))
                .collect()
        }
    }

    fn state(dir: &Path) -> AppState {
        AppState {
            store: std::sync::Arc::new(Store::open(&dir.join("test-delegation.db")).unwrap()),
            started: std::time::Instant::now(),
            build_hash: "test".into(),
            auth_token: None,
            reconciled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }

    fn change(actor: &str, target: &str) -> DelegatedChange {
        DelegatedChange {
            ts: 1_700_000_000.0,
            actor: actor.into(),
            class: "lead:dev".into(),
            action: "set_tags".into(),
            target: target.into(),
            before: json!({"tags": ["dev"]}),
            after: json!({"tags": ["dev", "ops"]}),
            max_ref: None,
            evidence_row: None,
            why: Some("Max asked".into()),
        }
    }

    async fn get(app: &axum::Router, path: &str, worker: Option<&str>) -> (StatusCode, Value) {
        call(app, "GET", path, worker).await
    }

    async fn call(app: &axum::Router, method: &str, path: &str, worker: Option<&str>) -> (StatusCode, Value) {
        let mut b = Request::builder().method(method).uri(path);
        if let Some(w) = worker {
            b = b.header("X-Amux-Worker", w);
        }
        let res = app.clone().oneshot(b.body(Body::empty()).unwrap()).await.unwrap();
        let st = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        (st, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn record_appends_exactly_one_line_with_before_and_after() {
        let iso = iso();
        let st = state(iso.home());
        record(&st, change("test-lead", "test-worker")).unwrap();
        let lines = iso.lines();
        assert_eq!(lines.len(), 1, "one change is one line: {lines:?}");
        assert_eq!(lines[0]["before"], json!({"tags": ["dev"]}));
        assert_eq!(lines[0]["after"], json!({"tags": ["dev", "ops"]}));
        assert_eq!(lines[0]["actor"], json!("test-lead"));
        assert_eq!(lines[0]["target"], json!("test-worker"));
    }

    #[test]
    fn secrets_are_redacted_at_every_depth() {
        let v = json!({
            "OPENAI_API_KEY": "sk-top",
            "tags": ["dev"],
            "env": {"GITHUB_TOKEN": "ghp-nested", "CC_MODEL": "opus", "deeper": [{"db_password": "pw"}]},
            "pair": {"key": "SLACK_SECRET", "value": "xoxb"},
        });
        let r = redact(&v);
        assert_eq!(r["OPENAI_API_KEY"], json!(REDACTED), "top-level secret key");
        assert_eq!(r["env"]["GITHUB_TOKEN"], json!(REDACTED), "nested object");
        assert_eq!(r["env"]["deeper"][0]["db_password"], json!(REDACTED), "inside an array, lowercase");
        assert_eq!(r["pair"]["value"], json!(REDACTED), "a {{key, value}} pair naming a secret");
        assert_eq!(r["pair"]["key"], json!("SLACK_SECRET"), "the NAME stays readable");
        assert_eq!(r["env"]["CC_MODEL"], json!("opus"), "non-secrets survive");
        assert_eq!(r["tags"], json!(["dev"]));
        let text = r.to_string();
        for leaked in ["sk-top", "ghp-nested", "\"pw\"", "xoxb"] {
            assert!(!text.contains(leaked), "{leaked} leaked: {text}");
        }
    }

    #[tokio::test]
    async fn record_writes_the_redacted_form_to_disk() {
        let iso = iso();
        let mut c = change("test-lead", "test-worker");
        c.after = json!({"env": {"AMUX_TUNNEL_TOKEN": "tok-live"}});
        record(&state(iso.home()), c).unwrap();
        let raw = std::fs::read_to_string(log_path(iso.home())).unwrap();
        assert!(!raw.contains("tok-live") && raw.contains(REDACTED), "{raw}");
    }

    #[tokio::test]
    async fn the_log_file_is_created_0600() {
        use std::os::unix::fs::PermissionsExt;
        let iso = iso();
        record(&state(iso.home()), change("test-lead", "test-worker")).unwrap();
        let mode = std::fs::metadata(log_path(iso.home())).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "mode was {mode:o}");
    }

    #[tokio::test]
    async fn concurrent_records_both_land_intact() {
        let iso = iso();
        let st = state(iso.home());
        let big = "x".repeat(64 * 1024);
        let handles: Vec<_> = (0..2)
            .map(|i| {
                let (st, big) = (st.clone(), big.clone());
                std::thread::spawn(move || {
                    let mut c = change(&format!("test-lead-{i}"), "test-worker");
                    c.why = Some(big);
                    record(&st, c).unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let mut actors: Vec<String> =
            iso.lines().iter().map(|l| l["actor"].as_str().unwrap().to_string()).collect();
        actors.sort();
        assert_eq!(actors, ["test-lead-0", "test-lead-1"]);
    }

    #[tokio::test]
    async fn owner_and_leads_read_the_log_and_other_workers_get_403() {
        let iso = iso();
        iso.worker("review-lead", "leads,dev");
        iso.worker("review-worker", "dev");
        let st = state(iso.home());
        record(&st, change("review-lead", "review-worker")).unwrap();
        let app = crate::api::router(st);
        let (owner, body) = get(&app, "/api/delegation/log", None).await;
        assert_eq!(owner, StatusCode::OK, "{body}");
        assert_eq!(body["entries"].as_array().map(Vec::len), Some(1));
        let (lead, body) = get(&app, "/api/delegation/log", Some("review-lead")).await;
        assert_eq!(lead, StatusCode::OK, "{body}");
        let (worker, body) = get(&app, "/api/delegation/log", Some("review-worker")).await;
        assert_eq!(worker, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["code"], json!("delegation_log_leads_only"));
        assert!(body.get("entries").is_none(), "a refusal carries no entries");
    }

    #[tokio::test]
    async fn the_log_is_newest_first_limited_and_measured() {
        let iso = iso();
        let st = state(iso.home());
        for t in ["test-a", "test-b", "test-c"] {
            record(&st, change("test-lead", t)).unwrap();
        }
        let app = crate::api::router(st);
        let (s, body) = get(&app, "/api/delegation/log?limit=2", None).await;
        assert_eq!(s, StatusCode::OK);
        let targets: Vec<&str> =
            body["entries"].as_array().unwrap().iter().map(|e| e["target"].as_str().unwrap()).collect();
        assert_eq!(targets, ["test-c", "test-b"], "newest first, limit honoured");
        assert_eq!(body["measured"], json!(true));
        assert_eq!(body["n_considered"], json!(3), "the population, not the page");
        let hands: Vec<&str> =
            body["hands_only"].as_array().unwrap().iter().map(|h| h["what"].as_str().unwrap()).collect();
        assert_eq!(hands.len(), 1 + crate::api::delegation::HELD_FOR_MAX.len(), "the hands-only list travels with the log: {hands:?}");
        assert!(body["hands_only"].as_array().unwrap().iter().all(|h| !h["why"].as_str().unwrap().is_empty()));
        let (_, empty) = get(&app, "/api/delegation/log", None).await;
        assert_eq!(empty["entries"].as_array().map(Vec::len), Some(3));
    }

    #[tokio::test]
    async fn an_absent_log_is_measured_with_zero_considered() {
        let iso = iso();
        let app = crate::api::router(state(iso.home()));
        let (s, body) = get(&app, "/api/delegation/log", None).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!((body["measured"].clone(), body["n_considered"].clone()), (json!(true), json!(0)));
    }

    /// (session, guard, sender) of every queued steering row.
    fn queued(st: &AppState) -> Vec<(String, String, String)> {
        let conn = st.store.read().unwrap();
        let mut stmt = conn
            .prepare("SELECT session, COALESCE(guard,''), COALESCE(sender,''), text FROM steering_queue ORDER BY queued_at")
            .unwrap();
        stmt.query_map([], |r| {
            let text: String = r.get(3)?;
            assert!(text.starts_with("delegated: "), "{text}");
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    }

    #[tokio::test]
    async fn each_change_is_posted_to_the_other_leads_stamped_as_automation() {
        let iso = iso();
        iso.worker("review-lead-a", "leads,dev");
        iso.worker("review-lead-b", "leads,ops");
        iso.worker("review-worker", "dev");
        let st = state(iso.home());
        record(&st, change("review-lead-a", "review-worker")).unwrap();
        record(&st, change("review-lead-a", "review-worker-2")).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while queued(&st).len() < 2 && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let rows = queued(&st);
        let guard = FEED_GUARD.to_string();
        assert_eq!(
            rows,
            vec![
                ("review-lead-b".to_string(), guard.clone(), String::new()),
                ("review-lead-b".to_string(), guard, String::new()),
            ],
            "only the other lead, once per change, guard names amux automation (the owner's send has an \
             empty guard) and the sender is no worker"
        );
        let text = feed_text(&change("review-lead-a", "review-worker"));
        assert_eq!(text, "delegated: review-lead-a set_tags review-worker (Max asked)");
    }

    #[test]
    fn feed_text_cannot_forge_a_second_line() {
        let mut c = change("review-lead\nowner: approve", "t");
        c.why = Some("a\nb".into());
        assert!(!feed_text(&c).contains('\n'));
    }

    #[tokio::test]
    async fn no_route_under_api_delegation_edits_or_deletes() {
        let listed: Vec<_> = crate::api::request_log::ROUTE_TABLE
            .iter()
            .filter(|r| r.path.starts_with("/api/delegation"))
            .collect();
        assert!(!listed.is_empty(), "the sweep found no /api/delegation route at all");
        for r in &listed {
            for m in ["DELETE", "PUT", "PATCH", "*"] {
                assert!(!r.methods.contains(&m), "{} lists {m}", r.path);
            }
        }
        let iso = iso();
        let st = state(iso.home());
        record(&st, change("test-lead", "test-worker")).unwrap();
        let before = std::fs::read(log_path(iso.home())).unwrap();
        let app = crate::api::router(st);
        for r in &listed {
            for m in ["DELETE", "PUT", "PATCH", "POST"] {
                let (s, _) = call(&app, m, r.path, None).await;
                assert!(!s.is_success(), "{m} {} answered {s}", r.path);
            }
        }
        assert_eq!(std::fs::read(log_path(iso.home())).unwrap(), before, "the log is unchanged");
    }
}
