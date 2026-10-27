//! A worker's question goes to its lead, not to the owner.
//!
//! Max, 2026-10-26 (typed in mrpopo, 14:48:50): "yes to all 4 rule lifts",
//! lift 4 being "worker questions go to their lead first, never straight to
//! Max; the ask guard routes them that way" (ai-config agents/owner.md). Before
//! this, a worker that ended its turn on a boundary ask, or opened an
//! `AskUserQuestion` picker, filed an owner card or parked on the owner's
//! keyboard. Now that question is queued to the worker's lead as a peer
//! message stamped with the worker's name, and the worker is told to hold that
//! step until the lead answers. The lead brings the owner only the Owner-only
//! categories (OWNER_ONLY_ASKS). A lead's own
//! asks, and the owner agent's, keep the owner-ask path unchanged.
//!
//! WHO IS A LEAD comes from groups, as for lead renames and lead-made workers
//! (`session_verbs::LEADS_GROUP`): a lead is a lane in `leads`, and a worker's
//! lead also sits in one of the worker's team groups. A lane with no such
//! lead is not routed, so a box without a `leads` group behaves as before.
//! Kept in its own module so the upstream turn-end code changes only at its
//! call sites.
//!
//! Kill switch `AMUX_ASK_ROUTE_TO_LEAD`, default ON, scoped worker > group >
//! global with the process env winning (the `turn_end::enabled` resolver).

use super::session_verbs as sv;
use super::turn_end::{self, clip};
use super::AppState;
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub(crate) const ASK_ROUTE_KEY: &str = "AMUX_ASK_ROUTE_TO_LEAD";
/// Steering guard for both routed messages. Non-empty: automation, never the
/// owner's own send (see `turn_end::OWNER_ASK_GUARD`).
pub(crate) const LEAD_ASK_GUARD: &str = "lead-ask";
/// Event type of the per-question claim.
const LEAD_ASK_EVENT: &str = "turn_end.lead_ask";
/// How long one forward covers the same question from the same worker: the
/// owner-ask steer window, for the same reason (a looping lane re-asks every
/// few seconds).
const LEAD_ASK_WINDOW_S: f64 = 6.0 * 3600.0;
/// Context sent to the lead with a question, in characters.
const LEAD_CONTEXT_CHARS: usize = 1500;
/// What a lead may still bring to the owner: the Owner-only row of ai-config
/// harness/ORCHESTRATOR.md, Lead Authority.
pub(crate) const OWNER_ONLY_ASKS: &str = "outside sends, money, signatures, credentials, permanent deletes, \
     access and permission changes, or a directional bet";

/// One lane as lead resolution sees it.
#[derive(Debug, Clone)]
pub(crate) struct LaneInfo {
    pub name: String,
    pub groups: BTreeSet<String>,
    pub archived: bool,
}

fn is_owner_agent(name: &str) -> bool {
    name.trim().eq_ignore_ascii_case(sv::OWNER_AGENT)
}

/// The lead of `lane` (whose groups are `groups`) among `lanes`. None for a
/// lead, for the owner agent, and for a worker no unarchived lead shares a
/// team group with. Ties resolve to the first name, so the answer is stable.
pub(crate) fn pick_lead(lane: &str, groups: &BTreeSet<String>, lanes: &[LaneInfo]) -> Option<String> {
    if groups.contains(sv::LEADS_GROUP) || is_owner_agent(lane) {
        return None;
    }
    lanes
        .iter()
        .filter(|l| l.name != lane && !l.archived && !is_owner_agent(&l.name))
        .filter(|l| l.groups.contains(sv::LEADS_GROUP))
        .filter(|l| l.groups.iter().any(|g| g != sv::LEADS_GROUP && groups.contains(g)))
        .map(|l| l.name.clone())
        .min()
}

/// Every lane's groups and archived flag, from the session env files.
fn lanes() -> Vec<LaneInfo> {
    let Ok(rd) = std::fs::read_dir(sv::home().join("sessions")) else {
        return vec![];
    };
    rd.flatten()
        .filter_map(|e| {
            let p = e.path();
            (p.extension().and_then(|x| x.to_str()) == Some("env"))
                .then(|| p.file_stem().and_then(|s| s.to_str()).map(str::to_string))
                .flatten()
        })
        .map(|name| LaneInfo {
            groups: sv::lane_groups(&name),
            archived: sv::env_flag_on(sv::parse_env(&name).get("CC_ARCHIVED")),
            name,
        })
        .collect()
}

/// The lead `lane`'s questions go to, when routing is on for it.
pub(crate) fn lead_for(lane: &str) -> Option<String> {
    if !turn_end::enabled(lane, ASK_ROUTE_KEY) {
        return None;
    }
    let lead = pick_lead(lane, &sv::lane_groups(lane), &lanes());
    tracing::debug!(session = %lane, lead = lead.as_deref().unwrap_or(""), verdict = "lead_ask_lead_resolved",
        "ask route: resolved the lane's lead");
    lead
}

/// The message the lead receives.
pub(crate) fn lead_text(worker: &str, question: &str, boundary: Option<&str>, context: &str) -> String {
    let touches = boundary.map(|b| format!(" It touches: {b}.")).unwrap_or_default();
    format!(
        "[amux lead-ask] {worker} asked the owner: \"{}\".{touches} Worker questions go to their lead \
         first (Max, 2026-10-26), so it came to you. Answer it with native SendMessage to {worker}, never `amux send`. Decide \
         reversible internal choices yourself and report them to mrpopo; bring the owner only {OWNER_ONLY_ASKS}.\n\n\
         {worker}'s context:\n\n{}",
        clip(question, 280),
        clip(context, LEAD_CONTEXT_CHARS)
    )
}

/// The reminder the worker receives after its question was routed.
pub(crate) fn worker_text(lead: &str, question: &str) -> String {
    format!(
        "[amux lead-ask] Your last turn asked the owner: \"{}\". Worker questions go to your lead first, \
         so amux sent it to {lead}. This is not an answer or an approval: do not take that step until \
         {lead} answers. Continue other authorized work meanwhile.",
        clip(question, 240)
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Routed {
    /// Queued to the lead.
    Sent,
    /// The same question from this worker went to the lead within the window.
    AlreadySent,
}

/// Claim the question for `worker` and queue it to `lead`. `idem` names the
/// asking event (turn or tool call). Delivery is the caller's: see [`deliver`].
pub(crate) async fn route(
    state: &AppState,
    worker: &str,
    lead: &str,
    question: &str,
    boundary: Option<&str>,
    context: &str,
    idem: &str,
) -> Result<Routed, &'static str> {
    let key = turn_end::question_key(question);
    if !turn_end::claim_within(state, worker, LEAD_ASK_EVENT, &key, LEAD_ASK_WINDOW_S,
        format!("lead-ask:{worker}:{idem}"),
        json!({"question": question, "lead": lead, "boundary": boundary})).await
    {
        tracing::info!(session = %worker, lead, verdict = "lead_ask_deduped", key = %key,
            "ask route: the same question already went to the lead in the last 6h");
        return Ok(Routed::AlreadySent);
    }
    let text = lead_text(worker, question, boundary, context);
    match sv::steer_enqueue_idempotent_report(state, lead, &text, LEAD_ASK_GUARD, worker, &format!("lead-ask-{worker}-{idem}")).await {
        Ok(r) => {
            tracing::warn!(session = %worker, lead, steer_id = %r.id, verdict = "lead_ask_routed",
                boundary = boundary.unwrap_or("none"), measured = true, n_considered = 1,
                "ask route: worker question queued to its lead instead of the owner (Max, 2026-10-26)");
            Ok(Routed::Sent)
        }
        Err(e) => {
            tracing::warn!(session = %worker, lead, verdict = "lead_ask_route_refused", error = e,
                "ask route: could not queue the question to the lead; the owner-ask path takes it");
            Err(e)
        }
    }
}

/// Deliver what [`route`] queued.
pub(crate) async fn deliver(state: &AppState, lane: &str) {
    sv::steer_deliver_for_session(state, lane).await;
}

/// Turn-end boundary ask: route it and remind the worker. True when handled;
/// false sends the caller down the owner-card path.
pub(crate) async fn route_turn_end_ask(
    state: &AppState,
    worker: &str,
    lead: &str,
    question: &str,
    boundary: &str,
    context: &str,
    turn_uuid: &str,
) -> bool {
    match route(state, worker, lead, question, Some(boundary), context, turn_uuid).await {
        Ok(Routed::AlreadySent) => true,
        Ok(Routed::Sent) => {
            deliver(state, lead).await;
            let id = format!("lead-ask-reminder-{turn_uuid}");
            if sv::steer_enqueue_idempotent_report(state, worker, &worker_text(lead, question), LEAD_ASK_GUARD, "", &id)
                .await
                .is_ok()
            {
                deliver(state, worker).await;
            }
            true
        }
        Err(_) => false,
    }
}

/// `AskUserQuestion` from a worker: route it and deny the picker. None lets
/// the owner-ask intercept decide, as before.
pub(crate) async fn route_picker(state: &AppState, worker: &str, lead: &str, tool_input: &Value) -> Option<Value> {
    let (question, options) = turn_end::describe_questions(tool_input);
    if question.trim().is_empty() {
        return None;
    }
    let boundary = turn_end::boundary_of(&format!("{question} {options}")).map(turn_end::Boundary::label);
    let context = format!("AskUserQuestion picker. Options: {options}");
    let idem = turn_end::question_key(&question);
    let routed = route(state, worker, lead, &question, boundary, &context, &idem).await.ok()?;
    if routed == Routed::Sent {
        deliver(state, lead).await;
    }
    Some(json!({
        "decision": "deny",
        "reason": format!("amux: worker questions go to the lead first, so this question was sent to {lead} \
            instead of waiting on the owner. Do not take that step until {lead} answers; continue other \
            authorized work meanwhile."),
        "lead": lead,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lane(name: &str, groups: &[&str], archived: bool) -> LaneInfo {
        LaneInfo { name: name.into(), groups: groups.iter().map(|g| g.to_string()).collect(), archived }
    }
    fn groups(g: &[&str]) -> BTreeSet<String> {
        g.iter().map(|s| s.to_string()).collect()
    }
    fn fleet() -> Vec<LaneInfo> {
        vec![
            lane("mrpopo", &["leads"], false),
            lane("dev-lead", &["dev", "leads"], false),
            lane("re-ops-lead", &["re-ops", "leads"], false),
            lane("dev-fix-x", &["dev"], false),
            lane("re-ops-loan-book", &["re-ops"], false),
        ]
    }

    #[test]
    fn a_worker_routes_to_its_own_teams_lead() {
        assert_eq!(pick_lead("dev-fix-x", &groups(&["dev"]), &fleet()).as_deref(), Some("dev-lead"));
        assert_eq!(pick_lead("re-ops-loan-book", &groups(&["re-ops"]), &fleet()).as_deref(), Some("re-ops-lead"));
    }

    #[test]
    fn leads_and_the_owner_agent_are_never_routed() {
        assert_eq!(pick_lead("dev-lead", &groups(&["dev", "leads"]), &fleet()), None);
        assert_eq!(pick_lead("mrpopo", &groups(&["leads"]), &fleet()), None);
        assert_eq!(pick_lead("MrPopo", &groups(&[]), &fleet()), None);
    }

    #[test]
    fn wrong_targets_are_never_picked() {
        // No team group shared with any lead: nothing to route to.
        assert_eq!(pick_lead("loose", &groups(&["altuva"]), &fleet()), None);
        assert_eq!(pick_lead("loose", &groups(&[]), &fleet()), None);
        // Sharing only `leads` is not a team (the owner agent is in it).
        assert_eq!(pick_lead("odd", &groups(&["leads-ish"]), &fleet()), None);
        // An archived lead, a non-lead teammate and the owner agent are skipped.
        let f = vec![
            lane("dev-lead", &["dev", "leads"], true),
            lane("dev-peer", &["dev"], false),
            lane("mrpopo", &["dev", "leads"], false),
        ];
        assert_eq!(pick_lead("dev-fix-x", &groups(&["dev"]), &f), None);
        // A worker in two teams gets one stable answer.
        let two = groups(&["dev", "re-ops"]);
        assert_eq!(pick_lead("both", &two, &fleet()).as_deref(), Some("dev-lead"));
        assert_eq!(pick_lead("both", &two, &fleet()), pick_lead("both", &two, &fleet()));
    }

    #[test]
    fn the_lead_is_told_what_it_may_still_escalate() {
        let t = lead_text("dev-fix-x", "Should I pay $16 for embeddings?", Some("money"), "ctx");
        assert!(t.contains("dev-fix-x asked the owner"));
        assert!(t.contains("SendMessage to dev-fix-x"));
        assert!(t.contains(OWNER_ONLY_ASKS));
        assert!(t.contains("It touches: money."));
        let w = worker_text("dev-lead", "Should I pay $16 for embeddings?");
        assert!(w.contains("sent it to dev-lead") && w.contains("do not take that step"));
    }

    fn hermetic_state() -> (tempfile::TempDir, AppState) {
        let tmp = tempfile::tempdir().unwrap();
        let store = crate::db::Store::open(&tmp.path().join("test.db")).unwrap();
        let state = AppState {
            store: std::sync::Arc::new(store),
            started: std::time::Instant::now(),
            build_hash: "test".into(),
            auth_token: None,
            reconciled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        };
        (tmp, state)
    }

    fn write_fleet(home: &std::path::Path) {
        let dir = home.join("sessions");
        std::fs::create_dir_all(&dir).unwrap();
        for (name, tags) in [("mrpopo", "leads"), ("dev-lead", "dev,leads"), ("dev-fix-x", "dev"), ("re-ops-lead", "re-ops,leads")] {
            std::fs::write(dir.join(format!("{name}.env")), format!("CC_TAGS={tags}\n")).unwrap();
        }
    }

    fn queued(state: &AppState, session: &str) -> Vec<(String, String, String)> {
        let conn = state.store.read().unwrap();
        let mut st = conn
            .prepare("SELECT text, guard, sender FROM steering_queue WHERE session=?1 ORDER BY queued_at")
            .unwrap();
        st.query_map([session], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .flatten()
            .collect()
    }

    #[tokio::test]
    async fn a_worker_question_is_queued_to_its_lead_once_per_window() {
        let home = tempfile::tempdir().unwrap();
        let _guard = crate::api::settings::test_env::set_home(home.path());
        write_fleet(home.path());
        let (_tmp, state) = hermetic_state();
        assert_eq!(lead_for("dev-fix-x").as_deref(), Some("dev-lead"));
        assert_eq!(lead_for("dev-lead"), None, "a lead's own ask keeps the owner path");
        assert_eq!(lead_for("mrpopo"), None);

        let q = "Should I spend $16 on Gemini embeddings?";
        let r = route(&state, "dev-fix-x", "dev-lead", q, Some("money"), "ctx", "turn-1").await;
        assert_eq!(r, Ok(Routed::Sent));
        let rows = queued(&state, "dev-lead");
        assert_eq!(rows.len(), 1);
        assert!(rows[0].0.contains(q), "{}", rows[0].0);
        assert_eq!((rows[0].1.as_str(), rows[0].2.as_str()), (LEAD_ASK_GUARD, "dev-fix-x"));
        assert!(queued(&state, "re-ops-lead").is_empty() && queued(&state, "mrpopo").is_empty(),
            "only the worker's own lead receives it");

        let again = route(&state, "dev-fix-x", "dev-lead", "should i spend $16 on gemini embeddings", Some("money"), "ctx", "turn-2").await;
        assert_eq!(again, Ok(Routed::AlreadySent));
        assert_eq!(queued(&state, "dev-lead").len(), 1, "a re-ask within the window is not sent twice");
    }

    #[tokio::test]
    async fn routing_is_off_when_the_kill_switch_is_off() {
        let home = tempfile::tempdir().unwrap();
        let _guard = crate::api::settings::test_env::set_home(home.path());
        write_fleet(home.path());
        std::fs::write(home.path().join("sessions/dev-fix-x.env"), format!("CC_TAGS=dev\n{ASK_ROUTE_KEY}=0\n")).unwrap();
        assert_eq!(lead_for("dev-fix-x"), None);
    }
}
