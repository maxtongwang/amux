//! STUB for piece (c) of maxtongwang/ai-config#160 (feat/delegation-log).
//! Exact interface signatures only, so piece (a) compiles before (c) lands.
//! It writes nothing. It is deleted when (c)'s branch is merged.
use super::AppState;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct DelegatedChange {
    pub ts: f64,
    pub actor: String,
    pub class: String,
    pub action: String,
    pub target: String,
    pub before: serde_json::Value,
    pub after: serde_json::Value,
    pub max_ref: Option<crate::api::max_ref::MaxRef>,
    pub evidence_row: Option<i64>,
    pub why: Option<String>,
}

pub(crate) fn record(_state: &AppState, change: DelegatedChange) -> std::io::Result<()> {
    tracing::info!(actor = %change.actor, class = %change.class, action = %change.action, target = %change.target,
        verdict = "delegated_change_unlogged_stub", "delegation_log stub: piece (c) not merged yet");
    Ok(())
}

pub(crate) fn redact(v: &serde_json::Value) -> serde_json::Value {
    v.clone()
}
