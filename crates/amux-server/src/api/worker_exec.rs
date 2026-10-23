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

    static FRAMES: Mutex<Vec<(std::path::PathBuf, String, String, &'static str)>> = Mutex::new(Vec::new());

    pub(crate) fn set_frame(name: &str, raw: &str) {
        let home = crate::config::amux_home();
        let mut frames = FRAMES.lock().unwrap();
        let origin = frames.iter().find(|(h, n, _, _)| h == &home && n == name).map_or("owner", |f| f.3);
        frames.retain(|(h, n, _, _)| h != &home || n != name);
        frames.push((home, name.into(), raw.into(), origin));
    }

    pub(crate) fn frame(name: &str) -> Option<String> {
        let home = crate::config::amux_home();
        FRAMES.lock().unwrap().iter().find(|(h,n,_,_)| h == &home && n == name).map(|(_,_,raw,_)| raw.clone())
    }

    /// Framed panes emulate terminal I/O only. Draft admission and submission
    /// verification must run through the production terminal send pipeline.
    pub(crate) fn terminal_output(args: &[&str]) -> Option<std::process::Output> {
        use std::os::unix::process::ExitStatusExt;
        // Match only a registered fake pane's canonical exact target. A bare
        // or prefix target must never turn real terminal I/O into fake success.
        let home = crate::config::amux_home();
        let name = FRAMES.lock().unwrap().iter().find_map(|(h, name, _, _)| {
            let pt = crate::backend::tmux::pane_target(&format!("amux-{name}"));
            (h == &home && args.windows(2).any(|pair|
                pair[0].strip_prefix('-') == Some("t") && pair[1] == pt.as_str()))
                .then(|| name.clone())
        })?;
        let name = name.as_str();
        let raw = frame(name)?;
        let stdout = match args[0] {
            "capture-pane" => raw,
            "send-keys" => {
                if args.contains(&"-l") {
                    let text = args.last().unwrap();
                    set_frame(name, &format!("Claude Code\n❯ {text}\n────────────────────\n⏵⏵ bypass permissions on (shift+tab to cycle)"));
                    let home = crate::config::amux_home();
                    let origin = FRAMES.lock().unwrap().iter().find(|(h,n,_,_)| h == &home && n == name).unwrap().3;
                    TYPED.lock().unwrap().push((name.into(), (*text).into(), origin));
                } else {
                    match *args.last().unwrap() {
                        "C-u" | "Enter" => set_frame(name, "Claude Code\n❯ \n────────────────────\n⏵⏵ bypass permissions on (shift+tab to cycle)"),
                        "Escape" => {},
                        key => panic!("unimplemented framed-pane key: {key}"),
                    }
                }
                String::new()
            }
            command => panic!("unimplemented framed-pane command: {command}"),
        };
        Some(std::process::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: stdout.into_bytes(),
            stderr: vec![],
        })
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
            let origin = match origin {
                SendOrigin::Owner => "owner",
                SendOrigin::Automation => "amux",
            };
            let home = crate::config::amux_home();
            if let Some(f) = FRAMES.lock().unwrap().iter_mut().find(|(h,n,_,_)| h == &home && n == name) {
                f.3 = origin;
                return Dispatch::Terminal;
            }
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
