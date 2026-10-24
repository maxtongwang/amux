//! Shared harness for the owner-delegated changes suite (maxtongwang/ai-config#160,
//! piece d). Every `delegation_*.rs` test binary includes this with
//! `mod delegation_support;`; it is a directory module so cargo does not build
//! it as a test target of its own.
//!
//! HARD ISOLATION, for the life of each [`Env`]:
//! - AMUX_HOME and CC_HOME are a fresh temp dir; HOME is a directory inside it,
//!   so nothing reads or writes `~/.amux` or `~/.claude`.
//! - TMUX and TMUX_PANE are unset and TMUX_TMPDIR is a private directory, so
//!   the server's own tmux probes (is_running, rename, stop) resolve to an
//!   empty private socket dir, never the default socket. Run the suite under
//!   `amux-isolated`, whose guard tmux refuses the live socket outright; this
//!   harness deliberately adds no tmux shim of its own.
//! - AMUX_API and AMUX_URL point at http://127.0.0.1:1; AMUX_TEST_ISOLATED=1.
//! - Requests go through `tower::ServiceExt::oneshot` against the real router
//!   and a temp SQLite store; no listener is opened.
//!
//! Process env is global and libtest runs tests on threads, so every Env holds
//! one process-wide lock until it drops. Fixture workers are `review-*`;
//! `mrpopo` exists only inside the temp home.
#![allow(dead_code)]

use amux_server::api::{router, AppState};
use amux_server::db::{Store, WriteOutcome};
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use tower::ServiceExt;

/// The owner agent's worker name (docs: agents/owner.md). Only ever created in
/// the temp home.
pub const MRPOPO: &str = "mrpopo";
/// The session where an explicit request is shown and Max's reply is recorded.
pub const MAX_PANE: &str = MRPOPO;
pub const DEV_LEAD: &str = "review-dev-lead";
/// A second lead of team dev: a PEER lead of DEV_LEAD.
pub const DEV_LEAD2: &str = "review-dev-lead2";
pub const OPS_LEAD: &str = "review-ops-lead";
/// In `leads` but without CC_ORCHESTRATOR=1: not a lead (spec decision).
pub const LEADS_NO_ORCH: &str = "review-dev-noorch";
pub const DEV_A: &str = "review-dev-a";
pub const DEV_B: &str = "review-dev-b";
pub const OPS_A: &str = "review-ops-a";

/// The delegated-change log (spec, piece c).
pub const LOG_REL: &str = "audit/delegated-changes.jsonl";

/// Who a request speaks for.
#[derive(Clone, Copy, Debug)]
pub enum Who<'a> {
    /// No worker headers: the owner/dashboard (auth_token is None here).
    Owner,
    /// `X-Amux-Session: <name>` (in-process, so rule 10 does not apply).
    Worker(&'a str),
}

fn lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Restores every variable it set when dropped.
struct Isolated {
    prev: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl Drop for Isolated {
    fn drop(&mut self) {
        for (k, v) in self.prev.drain(..) {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }
}

fn isolate(home: &Path) -> Isolated {
    let fake_home = home.join("home");
    let tmux_dir = home.join("tmux");
    for d in [&fake_home, &tmux_dir] {
        std::fs::create_dir_all(d).unwrap();
    }
    let dead = std::ffi::OsString::from("http://127.0.0.1:1");
    let vars: Vec<(&'static str, Option<std::ffi::OsString>)> = vec![
        ("AMUX_HOME", Some(home.as_os_str().to_owned())),
        ("CC_HOME", Some(home.as_os_str().to_owned())),
        ("HOME", Some(fake_home.into_os_string())),
        ("TMUX", None),
        ("TMUX_PANE", None),
        ("TMUX_TMPDIR", Some(tmux_dir.into_os_string())),
        ("AMUX_API", Some(dead.clone())),
        ("AMUX_URL", Some(dead)),
        ("AMUX_TEST_ISOLATED", Some("1".into())),
    ];
    let prev = vars.iter().map(|(k, _)| (*k, std::env::var_os(k))).collect();
    for (k, v) in vars {
        match v {
            Some(v) => std::env::set_var(k, v),
            None => std::env::remove_var(k),
        }
    }
    Isolated { prev }
}

/// One isolated server: temp home with the fixture fleet, a temp store, and
/// the real router.
pub struct Env {
    pub app: axum::Router,
    pub store: Arc<Store>,
    pub dir: PathBuf,
    // Drop order matters: restore env before the temp dir goes, release the
    // lock last.
    _iso: Isolated,
    home: tempfile::TempDir,
    _lock: tokio::sync::MutexGuard<'static, ()>,
}

/// The fixture fleet. Leads are `leads` members WITH CC_ORCHESTRATOR=1.
const FLEET: &[(&str, &str, bool)] = &[
    (DEV_LEAD, "leads,dev", true),
    (DEV_LEAD2, "leads,dev", true),
    (OPS_LEAD, "leads,ops", true),
    (LEADS_NO_ORCH, "leads,dev", false),
    (DEV_A, "dev", false),
    (DEV_B, "dev", false),
    (OPS_A, "ops", false),
    (MRPOPO, "leads", false),
];

pub async fn setup() -> Env {
    let guard = lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let iso = isolate(home.path());
    let sessions = home.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    for (name, tags, orch) in FLEET {
        let mut env = format!("CC_DIR=\"/tmp\"\nCC_TAGS=\"{tags}\"\nCC_INCARNATION=\"inc-{name}\"\n");
        if *orch {
            env.push_str("CC_ORCHESTRATOR=\"1\"\n");
        }
        std::fs::write(sessions.join(format!("{name}.env")), env).unwrap();
    }
    let dir = home.path().join("work");
    std::fs::create_dir_all(&dir).unwrap();
    let store = Arc::new(Store::open(&home.path().join("amux-test.db")).unwrap());
    let state = AppState {
        store: store.clone(),
        started: std::time::Instant::now(),
        build_hash: "test".into(),
        auth_token: None,
        reconciled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
    };
    Env { app: router(state), store, dir, _iso: iso, home, _lock: guard }
}

pub fn now_s() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64()
}

/// 202 with a pending grant: returns its code, checking the sentence names it.
#[track_caller]
pub fn assert_grant_pending(st: StatusCode, v: &Value, ctx: &str) -> String {
    assert_eq!(st, StatusCode::ACCEPTED, "{ctx}: expected 202 pending grant, got {st}: {v}");
    let code = v["grant_code"].as_str().unwrap_or_else(|| panic!("{ctx}: no grant_code: {v}")).to_string();
    assert!(v["sentence"].as_str().is_some_and(|s| s.starts_with(&format!("amux grant {code}: "))), "{ctx}: sentence: {v}");
    code
}

/// Apply only by explicit request ID; malformed fixtures must fail loudly.
pub fn with_request(mut body: Value, request_id: Value) -> Value {
    body["request_id"] = json!(request_id.as_str().expect("explicit request ID must be a string"));
    body
}

static PANE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl Env {
    pub async fn request(&self, actor: &str, method: &str, path: &str, body: Value) -> String {
        let (st,v) = self.request_result(actor, method, path, body).await;
        assert_eq!(st, StatusCode::ACCEPTED, "explicit request: {v}");
        let id = v["request_id"].as_str().expect("request_id").to_string();
        self.display_request(&id).await;
        id
    }

    pub async fn display_request(&self, id: &str) {
        let block = self.approval_block(id);
        self.native_report(MAX_PANE, "Stop", &block).await;
        let shown: Option<i64> = self.store.read().unwrap().query_row(
            "SELECT shown_row FROM _amux_delegation_grants WHERE code=?1", [id], |r| r.get(0)).unwrap();
        assert!(shown.is_some(), "exact completed native block must bind shown_row");
    }

    pub fn short_code(&self, id: &str) -> String {
        self.store.read().unwrap().query_row(
            "SELECT short_code FROM _amux_delegation_grants WHERE code=?1", [id], |r| r.get(0)).unwrap()
    }

    pub fn approval_block(&self, id: &str) -> String {
        let question: String = self.store.read().unwrap().query_row(
            "SELECT ask_text FROM _amux_delegation_grants WHERE code=?1", [id], |r| r.get(0)).unwrap();
        format!("Approval needed — {}\n{question}\nReply \"yes\" to approve. Anything else declines.", self.short_code(id))
    }

    /// Exercise request creation without assuming that the change is delegable.
    pub async fn request_result(&self, actor: &str, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
        self.call(Who::Worker(actor), "POST", "/api/delegation/requests",
            Some(json!({"method":method,"path":path,"body":body}))).await
    }

    pub async fn approved_request(&self, actor: &str, method: &str, path: &str, body: Value) -> Value {
        let id=self.request(actor,method,path,body).await;
        let short = self.short_code(&id);
        let (_,reply)=self.type_in_pane(MAX_PANE, &format!("yes {short}")).await;
        assert_eq!(reply["approval_context"],format!("[amux approval] approved {short}"));
        let resolved: bool = self.store.read().unwrap().query_row(
            "SELECT resolution_row IS NOT NULL AND resolution_code='request_resolved' FROM _amux_delegation_grants WHERE code=?1",
            [&id], |r| r.get(0)).unwrap();
        assert!(resolved,"native report must commit approval before any recovery read or apply");
        json!(id)
    }

    pub fn home(&self) -> &Path {
        self.home.path()
    }

    pub async fn call(&self, who: Who<'_>, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut b = Request::builder().method(method).uri(path);
        if let Who::Worker(name) = who {
            b = b.header("X-Amux-Session", name);
        }
        let req = match body {
            Some(v) => b.header(header::CONTENT_TYPE, "application/json").body(Body::from(v.to_string())).unwrap(),
            None => b.body(Body::empty()).unwrap(),
        };
        let res = self.app.clone().oneshot(req).await.unwrap();
        let st = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let v = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
        };
        (st, v)
    }

    /// Like [`Env::call`], over a socket (ConnectInfo set) so rule 10 judges
    /// the worker claim, presenting `token` as X-Amux-Worker-Token.
    pub async fn call_socket(&self, worker: &str, token: Option<&str>, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
        let mut b = Request::builder().method(method).uri(path)
            .header(header::CONTENT_TYPE, "application/json").header("X-Amux-Session", worker);
        if let Some(t) = token {
            b = b.header("X-Amux-Worker-Token", t);
        }
        let mut req = b.body(Body::from(body.to_string())).unwrap();
        req.extensions_mut().insert(axum::extract::ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 9))));
        let res = self.app.clone().oneshot(req).await.unwrap();
        let st = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        (st, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    /// Every config file a change could write (worker, group and global env,
    /// server.env, memory and rules), path -> content. Equal snapshots before
    /// and after a refused request mean it wrote no file.
    pub fn snapshot(&self) -> std::collections::BTreeMap<String, String> {
        let mut out = std::collections::BTreeMap::new();
        let mut stack = vec![self.home().join("sessions"), self.home().join("env"), self.home().join("memory")];
        for f in ["amux.env", "server.env"] {
            let p = self.home().join(f);
            out.insert(f.to_string(), std::fs::read_to_string(&p).unwrap_or_default());
        }
        while let Some(d) = stack.pop() {
            for entry in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    let rel = p.strip_prefix(self.home()).unwrap().to_string_lossy().into_owned();
                    out.insert(rel, std::fs::read_to_string(&p).unwrap_or_default());
                }
            }
        }
        out
    }

    /// [`Self::snapshot`] as configuration: each env file read as its
    /// key/value pairs, without the worker identity amux #3 keeps
    /// (`CC_INCARNATION`, `.incarnation.lock`), which every env writer
    /// preserves and undo leaves alone. Undo is exact on this view.
    pub fn config_snapshot(&self) -> std::collections::BTreeMap<String, String> {
        self.snapshot()
            .into_iter()
            .filter(|(f, _)| !f.rsplit('/').next().unwrap_or("").starts_with('.'))
            .map(|(f, text)| {
                if !f.ends_with(".env") {
                    return (f, text);
                }
                let pairs: Vec<String> = text
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && !l.starts_with('#'))
                    .filter_map(|l| l.split_once('='))
                    .filter(|(k, _)| *k != "CC_INCARNATION")
                    .map(|(k, v)| format!("{k}={}", v.trim_matches('"')))
                    .collect();
                (f, pairs.join("\n"))
            })
            .collect()
    }

    pub fn env_path(&self, name: &str) -> PathBuf {
        self.home().join("sessions").join(format!("{name}.env"))
    }

    pub fn env_text(&self, name: &str) -> String {
        std::fs::read_to_string(self.env_path(name)).unwrap_or_default()
    }

    /// The worker's groups as its env file stores them.
    pub fn tags(&self, name: &str) -> BTreeSet<String> {
        let text = self.env_text(name);
        let line = text.lines().rev().find_map(|l| l.trim().strip_prefix("CC_TAGS=")).unwrap_or("");
        line.trim_matches('"').split(',').map(|t| t.trim().to_lowercase()).filter(|t| !t.is_empty()).collect()
    }

    pub fn set(&self, xs: &[&str]) -> BTreeSet<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    /// Insert one Messages row; `ts_s` is unix seconds (the table stores ms).
    pub fn seed_row(&self, session: &str, text: &str, ty: &str, origin: &str, delivery: Option<&str>, ts_s: f64) -> i64 {
        let (session, text, ty, origin) = (session.to_string(), text.to_string(), ty.to_string(), origin.to_string());
        let identity = std::fs::read_to_string(self.env_path(&session)).ok().and_then(|text| {
            text.lines().find_map(|line| line.strip_prefix("CC_INCARNATION=").map(|v|v.trim_matches('"').to_string()))
        });
        let delivery = delivery.map(str::to_string);
        let id = Arc::new(std::sync::atomic::AtomicI64::new(0));
        let out = id.clone();
        self.store
            .write(move |conn| {
                conn.execute(
                    "INSERT INTO cmd_history (text, type, session, ts, origin, delivery, delegation_session_incarnation) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    rusqlite::params![text, ty, session, (ts_s * 1000.0) as i64, origin, delivery, identity],
                )?;
                out.store(conn.last_insert_rowid(), std::sync::atomic::Ordering::SeqCst);
                Ok(WriteOutcome { applied: true, events: vec![] })
            })
            .unwrap();
        id.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Every row's (session, origin, type, text) since `after_id`.
    pub fn rows_after(&self, after_id: i64) -> Vec<(String, String, String, String)> {
        let conn = self.store.read().unwrap();
        let mut st = conn
            .prepare("SELECT session, origin, type, text FROM cmd_history WHERE id > ?1 ORDER BY id")
            .unwrap();
        st.query_map([after_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .flatten()
            .collect()
    }

    /// Native lifecycle payloads go through the real report route, with the
    /// provider session and launch incarnation used by production approval.
    pub async fn native_report(&self, lane: &str, event: &str, text: &str) -> (StatusCode, Value) {
        let now = now_s();
        let dir = self.home().join("status-events").join(lane);
        std::fs::create_dir_all(&dir).unwrap();
        let run = format!("review-run-{lane}");
        if !dir.join("current.json").exists() {
            std::fs::write(dir.join("current.json"), json!({"run_id": run, "provider": "claude", "started": now - 100.0,
                "incarnation":format!("inc-{lane}")}).to_string()).unwrap();
        }
        let seq = PANE_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut report = json!({"native_status": true, "approval_live": true, "event": event,
            "state": if event == "Stop" { "idle" } else { "active" }, "run_id": run, "provider": "claude",
            "sequence": seq, "event_ts": now, "session_id": format!("native-{lane}"), "turn_id": ""});
        if event == "Stop" {
            report["assistant_message"] = json!(text);
        } else {
            report["prompt"] = json!(text);
            report["prompt_truncated"] = json!(false);
        }
        let reply = self.call(Who::Worker(lane), "POST", &format!("/api/sessions/{lane}/report"), Some(report)).await;
        assert!(reply.0.is_success() && reply.1["applied"] == true,
            "native hook report must be applied before it can record owner input: {reply:?}");
        reply
    }

    pub async fn type_in_pane(&self, lane: &str, text: &str) -> (StatusCode, Value) {
        self.native_report(lane, "UserPromptSubmit", text).await
    }

    /// A stored grant's status, or "" when there is no such grant.
    pub fn grant_status(&self, code: &str) -> String {
        let conn = self.store.read().unwrap();
        conn.query_row("SELECT status FROM _amux_delegation_grants WHERE code=?1", [code], |r| r.get(0)).unwrap_or_default()
    }

    /// The newest grant's code ("" when none).
    pub fn latest_grant(&self) -> String {
        let conn = self.store.read().unwrap();
        conn.query_row("SELECT code FROM _amux_delegation_grants ORDER BY created_at DESC LIMIT 1", [], |r| r.get(0)).unwrap_or_default()
    }

    /// Switch the delegation approval mode in this home's global layer.
    pub fn set_mode(&self, mode: &str) {
        std::fs::write(self.home().join("amux.env"), format!("AMUX_DELEGATION_APPROVAL=\"{mode}\"\n")).unwrap();
    }

    /// Move a grant's expiry into the past.
    pub fn expire_grant(&self, code: &str) {
        let code = code.to_string();
        self.store.write(move |conn| {
            conn.execute("UPDATE _amux_delegation_grants SET expires_at=1 WHERE code=?1", [code])?;
            Ok(WriteOutcome { applied: true, events: vec![] })
        }).unwrap();
    }

    /// Steering-queue texts for `lane` (the leads feed lands here).
    pub fn queued_for(&self, lane: &str) -> Vec<String> {
        let conn = self.store.read().unwrap();
        let mut st = conn.prepare("SELECT text FROM steering_queue WHERE session=?1").unwrap();
        st.query_map([lane], |r| r.get(0)).unwrap().flatten().collect()
    }

    pub fn max_row_id(&self) -> i64 {
        let conn = self.store.read().unwrap();
        conn.query_row("SELECT COALESCE(MAX(id), 0) FROM cmd_history", [], |r| r.get(0)).unwrap()
    }

    pub fn log_path(&self) -> PathBuf {
        self.home().join(LOG_REL)
    }

    /// The delegated-change log's entries, oldest first; empty when absent.
    /// A line that is not JSON fails the test rather than being skipped.
    pub fn log_entries(&self) -> Vec<Value> {
        let text = std::fs::read_to_string(self.log_path()).unwrap_or_default();
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("log line is not JSON ({e}): {l}")))
            .collect()
    }

    pub fn log_raw(&self) -> String {
        std::fs::read_to_string(self.log_path()).unwrap_or_default()
    }
}

pub fn is_success(st: StatusCode) -> bool {
    st.is_success()
}

/// Allowed: a 2xx answer.
#[track_caller]
pub fn assert_allowed(st: StatusCode, v: &Value, ctx: &str) {
    assert!(st.is_success(), "{ctx}: expected allowed (2xx), got {st}: {v}");
}

/// #8's refusal shape: 403 with `ok: false` and a non-empty string `code`.
#[track_caller]
pub fn assert_refused(st: StatusCode, v: &Value, ctx: &str) {
    assert_eq!(st, StatusCode::FORBIDDEN, "{ctx}: expected 403, got {st}: {v}");
    assert_eq!(v["ok"], json!(false), "{ctx}: refusal carries ok:false: {v}");
    assert!(v["code"].as_str().is_some_and(|c| !c.is_empty()), "{ctx}: refusal carries a code: {v}");
}

/// Refused with exactly `code`.
#[track_caller]
pub fn assert_code(st: StatusCode, v: &Value, code: &str, ctx: &str) {
    assert_refused(st, v, ctx);
    assert_eq!(v["code"], json!(code), "{ctx}: {v}");
}

/// A permission-table denial, rather than a missing/invalid approval fixture.
#[track_caller]
pub fn assert_permission_refused(st: StatusCode, v: &Value, ctx: &str) {
    assert_refused(st, v, ctx);
    let code = v["code"].as_str().unwrap();
    assert!(!["max_ref_", "request_", "grant_", "cite_"].iter().any(|prefix| code.starts_with(prefix)),
        "{ctx}: require the permission-table refusal, not an approval error: {v}");
}

pub fn create_body(dir: &Path, name: &str, tags: Value) -> Value {
    json!({"name": name, "dir": dir.to_str().unwrap(), "tags": tags, "start": false})
}

pub fn scope_body(level: &str, name: &str, capability: &str, value: Value) -> Value {
    json!({"level": level, "name": name, "capability": capability, "value": value})
}
