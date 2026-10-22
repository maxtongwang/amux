//! Execution adapters: how a worker TYPE runs, behind one seam (ACW-1/3/6).
//!
//! Every worker is the same durable entity: one env file (or `_amux_workers`
//! row), one board identity, one message ledger, one event stream. What
//! differs by `worker_type` is only how a turn executes and how its output is
//! rendered. The lifecycle code (`start_session`, `stop_for_pause`,
//! `stop_session_process`, `send_text_inner_bound`, `is_running`, `peek_verb`
//! and the fleet list) asks the worker's adapter first and only runs the
//! terminal pipeline when the adapter answers [`Dispatch::Terminal`].
//!
//! The coding adapter answers `Terminal` everywhere: it IS the existing
//! tmux/herdr pipeline, unchanged. The chat adapter handles every operation
//! itself (headless provider turns). A new type implements
//! [`ExecutionAdapter`], adds itself to [`ADAPTERS`] and registers a
//! descriptor in `amux_core::worker_type::REGISTRY`; nothing in the lifecycle
//! code changes.
//!
//! Log signal (two-fix rule): every handled dispatch emits
//! `verdict="worker_exec_dispatch"` with the type and operation, so a sweep
//! can count which adapter served what and spot a chat worker that fell into
//! the terminal pipeline (it would show a tmux `not running` with no dispatch
//! line beside it).

use super::session_verbs::{parse_env, EnvFile, SendOrigin};
use super::AppState;
use amux_core::worker_type::{WorkerTypeId, REGISTRY};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

/// An adapter's answer: it handled the operation, or the worker runs on the
/// shared terminal pipeline and the caller should continue with it.
pub(crate) enum Dispatch<T> {
    Handled(T),
    Terminal,
}

#[async_trait::async_trait]
pub(crate) trait ExecutionAdapter: Send + Sync {
    /// The `amux_core::worker_type` id this adapter executes.
    fn worker_type(&self) -> &'static str;
    /// Bring the worker to a state where it accepts turns.
    async fn start(&self, state: &AppState, name: &str) -> Dispatch<(bool, String)>;
    /// Stop in-flight work. No state: delete/archive call this from paths
    /// that have none, and stopping must not depend on the DB being up.
    async fn stop(&self, name: &str) -> Dispatch<(bool, String)>;
    /// Deliver one message (owner send, peer message, steering, schedule,
    /// board dispatch: all of them converge here).
    async fn deliver(
        &self,
        state: &AppState,
        name: &str,
        text: &str,
        origin: SendOrigin,
    ) -> Dispatch<(bool, String)>;
    /// Liveness for the fleet list and every `is_running` gate.
    fn running(&self, name: &str) -> Dispatch<bool>;
    /// The `/peek` payload, same keys as the terminal shape.
    async fn peek(&self, state: &AppState, name: &str, lines: i64) -> Dispatch<Value>;
    /// Stop the in-flight turn but keep the worker and its queue (the
    /// Escape key of a terminal worker). Terminal workers take real keys.
    async fn interrupt(&self, _name: &str) -> Dispatch<(bool, String)> {
        Dispatch::Terminal
    }
}

/// The coding worker: the terminal pipeline, untouched.
struct TerminalAdapter;

#[async_trait::async_trait]
impl ExecutionAdapter for TerminalAdapter {
    fn worker_type(&self) -> &'static str {
        WorkerTypeId::CODING
    }
    async fn start(&self, _: &AppState, _: &str) -> Dispatch<(bool, String)> {
        Dispatch::Terminal
    }
    async fn stop(&self, _: &str) -> Dispatch<(bool, String)> {
        Dispatch::Terminal
    }
    async fn deliver(&self, _: &AppState, _: &str, _: &str, _: SendOrigin) -> Dispatch<(bool, String)> {
        Dispatch::Terminal
    }
    fn running(&self, _: &str) -> Dispatch<bool> {
        Dispatch::Terminal
    }
    async fn peek(&self, _: &AppState, _: &str, _: i64) -> Dispatch<Value> {
        Dispatch::Terminal
    }
}

static TERMINAL: TerminalAdapter = TerminalAdapter;
static CHAT: super::chat_worker::ChatAdapter = super::chat_worker::ChatAdapter;

/// One adapter per registered worker type. `ADAPTERS[0]` serves any id
/// without an adapter, which `WorkerTypeId::descriptor` already maps to
/// coding behaviour.
static ADAPTERS: &[&dyn ExecutionAdapter] = &[&TERMINAL, &CHAT];

pub(crate) fn adapter_for(worker_type: &WorkerTypeId) -> &'static dyn ExecutionAdapter {
    let id = worker_type.descriptor().id;
    ADAPTERS
        .iter()
        .copied()
        .find(|a| a.worker_type() == id)
        .unwrap_or(ADAPTERS[0])
}

/// The type recorded in an env map (`CC_WORKER_TYPE`; absent = coding).
pub(crate) fn worker_type_of_env(value: Option<&str>) -> WorkerTypeId {
    WorkerTypeId::parse(value.unwrap_or("")).unwrap_or_default()
}

pub(crate) fn worker_type_of(name: &str) -> WorkerTypeId {
    worker_type_of_env(parse_env(name).get("CC_WORKER_TYPE"))
}

pub(crate) fn adapter_for_session(name: &str) -> &'static dyn ExecutionAdapter {
    adapter_for_env(&parse_env(name))
}

/// [`adapter_for_session`] over an env the caller already loaded, so the send
/// layer and every lifecycle gate pick the adapter by one rule.
pub(crate) fn adapter_for_env(cfg: &EnvFile) -> &'static dyn ExecutionAdapter {
    #[cfg(test)]
    if cfg.get(fake_pane::ENV_KEY) == Some("1") {
        return &fake_pane::FAKE;
    }
    adapter_for(&worker_type_of_env(cfg.get("CC_WORKER_TYPE")))
}

/// Count a handled dispatch (see module doc).
pub(crate) fn note_dispatch(name: &str, op: &'static str, worker_type: &str) {
    tracing::debug!(
        session = %name, op, worker_type, measured = true, n_considered = 1,
        verdict = "worker_exec_dispatch",
        "worker operation served by its type's execution adapter"
    );
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/worker-types", get(list_types))
        .route("/api/sessions/{name}/chat", get(super::chat_worker::history_route))
        .route(
            "/api/sessions/{name}/chat/stream",
            get(super::chat_worker::stream_route),
        )
        .route(
            "/api/sessions/{name}/chat/interrupt",
            post(super::chat_worker::interrupt_route),
        )
}

/// The registry, so the create/edit UI renders options and requirements from
/// data rather than from a hardcoded list.
async fn list_types() -> Response {
    Json(json!({
        "default": WorkerTypeId::CODING,
        "types": REGISTRY,
    }))
    .into_response()
}

/// A test-only worker whose "pane" is an in-memory log: it reads as running
/// and records every delivery with its origin, so a test can assert what amux
/// typed into a worker through the real send layer without a tmux server.
/// Selected per worker by `CC_TEST_FAKE_PANE=1` in its env file, so it follows
/// the worker through a rename and touches no other test's workers.
#[cfg(test)]
pub(crate) mod fake_pane {
    use super::*;
    use std::sync::Mutex;

    pub(crate) const ENV_KEY: &str = "CC_TEST_FAKE_PANE";
    pub(crate) struct FakePane;
    pub(crate) static FAKE: FakePane = FakePane;
    static TYPED: Mutex<Vec<(String, String, &'static str)>> = Mutex::new(Vec::new());

    /// Opt-in terminal transport: exercise the real send choreography without
    /// a process, provider HOME or socket. Other fake workers keep the adapter.
    #[derive(Default)]
    pub(crate) struct Composer {
        pub text: String,
        pub restore_before_type: Option<String>,
        pub race_during_type: Option<String>,
        pub keys: Vec<String>,
        pub(crate) reads_after_clear: Option<usize>,
    }
    static COMPOSERS: std::sync::LazyLock<Mutex<std::collections::HashMap<String, Composer>>> =
        std::sync::LazyLock::new(Default::default);
    const PLACEHOLDER: &str = "\x1b[2mTry a suggestion\x1b[0m";

    pub(crate) fn composer(name: &str, pane: Composer) {
        COMPOSERS.lock().unwrap().insert(name.into(), pane);
    }

    pub(crate) fn keys(name: &str) -> Vec<String> {
        COMPOSERS.lock().unwrap().get(name).unwrap().keys.clone()
    }

    pub(crate) fn capture(name: &str) -> Option<String> {
        let mut panes = COMPOSERS.lock().unwrap();
        let pane = panes.get_mut(name)?;
        if let Some(reads) = &mut pane.reads_after_clear {
            *reads += 1;
            if *reads == 2 {
                if let Some(draft) = pane.restore_before_type.take() {
                    pane.text.insert_str(0, &draft);
                }
            }
        }
        let text = if pane.text.is_empty() { PLACEHOLDER } else { &pane.text };
        Some(format!("──── lane ────\n❯ {text}\n──────\n⏵⏵ bypass permissions on\n"))
    }

    pub(crate) fn key(name: &str, key: &str) -> bool {
        let mut panes = COMPOSERS.lock().unwrap();
        let Some(pane) = panes.get_mut(name) else { return false };
        pane.keys.push(key.into());
        if key == "C-u" {
            pane.text.clear();
            pane.reads_after_clear = Some(0);
        } else if key == "Enter" {
            pane.text.clear();
        }
        true
    }

    pub(crate) fn literal(name: &str, text: &str) -> bool {
        let mut panes = COMPOSERS.lock().unwrap();
        let Some(pane) = panes.get_mut(name) else { return false };
        if let Some(draft) = pane.race_during_type.take() {
            pane.text.push_str(&draft);
        }
        pane.text.push_str(text);
        TYPED.lock().unwrap().push((name.into(), text.into(), "terminal"));
        true
    }

    /// Every `(text, origin)` delivered to `name` so far.
    pub(crate) fn typed(name: &str) -> Vec<(String, &'static str)> {
        TYPED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(n, _, _)| n == name)
            .map(|(_, t, o)| (t.clone(), *o))
            .collect()
    }

    #[async_trait::async_trait]
    impl ExecutionAdapter for FakePane {
        fn worker_type(&self) -> &'static str {
            "test-fake-pane"
        }
        async fn start(&self, _: &AppState, _: &str) -> Dispatch<(bool, String)> {
            Dispatch::Handled((true, "fake pane".into()))
        }
        async fn stop(&self, _: &str) -> Dispatch<(bool, String)> {
            Dispatch::Handled((true, "fake pane".into()))
        }
        async fn deliver(&self, _: &AppState, name: &str, text: &str, origin: SendOrigin) -> Dispatch<(bool, String)> {
            if COMPOSERS.lock().unwrap().contains_key(name) {
                return Dispatch::Terminal;
            }
            let origin = match origin {
                SendOrigin::Owner => "owner",
                SendOrigin::Automation => "amux",
            };
            TYPED
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((name.to_string(), text.to_string(), origin));
            Dispatch::Handled((true, "sent".into()))
        }
        fn running(&self, _: &str) -> Dispatch<bool> {
            Dispatch::Handled(true)
        }
        async fn peek(&self, _: &AppState, name: &str, _: i64) -> Dispatch<Value> {
            Dispatch::Handled(json!({"name": name, "output": ""}))
        }
    }
}
