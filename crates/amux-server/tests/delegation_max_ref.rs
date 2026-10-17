//! max_ref verification over HTTP (maxtongwang/ai-config#160, pieces b and d).
//!
//! Every case drives the same widening change, mrpopo adding review-dev-a to
//! `leads` through PATCH /api/sessions/review-dev-a/config, which the table
//! allows only with a valid max_ref. The cited row is seeded the way each
//! writer records it (tests/delegation_support/mod.rs):
//! - pane-typed: api/pane_prompts.rs::record (type user, origin pane,
//!   delivery pane). A Claude remote-control prompt from the owner's phone is
//!   a real UserPromptSubmit and lands the same way.
//! - owner sends: a dashboard/phone/CLI send row with #7's attribution verdict
//!   recorded by piece (b). Outside and Remote qualify; a send traced to a
//!   worker pane, or with no recorded verdict, does not.
//! - amux deliveries: `[amux ...]`-stamped relays, peer sends, and the
//!   delivery rows the pane_prompts ring / cmd_history match against.
//!
//! Every refusal must leave the tags unchanged.

mod delegation_support;

use axum::http::StatusCode;
use delegation_support::*;
use serde_json::{json, Value};

const INSTRUCTION: &str = "please add review-dev-a to leads";

fn path() -> String {
    format!("/api/sessions/{DEV_A}/config")
}

async fn widen(e: &Env, r: Option<Value>) -> (StatusCode, Value) {
    let body = match r {
        Some(r) => with_ref(json!({"tags": "dev,leads"}), r, "Max asked in my pane"),
        None => json!({"tags": "dev,leads"}),
    };
    e.call(Who::Worker(MRPOPO), "PATCH", &path(), Some(body)).await
}

#[track_caller]
fn assert_unchanged(e: &Env, ctx: &str) {
    assert_eq!(e.tags(DEV_A), e.set(&["dev"]), "{ctx}: a refused max_ref changed the tags");
}

async fn refused_with(e: &Env, r: Option<Value>, code: &str, ctx: &str) {
    let (st, v) = widen(e, r).await;
    assert_code(st, &v, code, ctx);
    assert_unchanged(e, ctx);
}

#[tokio::test]
async fn a_pane_typed_row_is_accepted() {
    let e = setup().await;
    let t = now_s() - 60.0;
    let row = e.seed_pane_prompt(MRPOPO, INSTRUCTION, t);
    let (st, v) = widen(&e, Some(max_ref(MRPOPO, t))).await;
    assert_allowed(st, &v, "pane-typed");
    assert_eq!(e.tags(DEV_A), e.set(&["dev", "leads"]));
    // The evidence the change was authorized by is the seeded row.
    let entries = e.log_entries();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0]["evidence_row"], json!(row), "{}", entries[0]);
}

#[tokio::test]
async fn a_remote_control_prompt_from_the_phone_is_accepted() {
    let e = setup().await;
    let t = now_s() - 30.0;
    // Claude remote control: typed on the owner's phone, delivered to the CLI
    // as a real UserPromptSubmit with no amux stamp, recorded as pane-typed.
    e.seed_pane_prompt(MRPOPO, "from my phone: add review-dev-a to leads now", t);
    let (st, v) = widen(&e, Some(max_ref(MRPOPO, t))).await;
    assert_allowed(st, &v, "remote-control prompt");
}

#[tokio::test]
async fn owner_sends_with_outside_or_remote_attribution_are_accepted() {
    for verdict in ["outside", "remote"] {
        let e = setup().await;
        let t = now_s() - 60.0;
        e.seed_owner_send(MRPOPO, INSTRUCTION, t, Some(verdict));
        let (st, v) = widen(&e, Some(max_ref(MRPOPO, t))).await;
        assert_allowed(st, &v, &format!("owner send, attribution {verdict}"));
    }
}

#[tokio::test]
async fn a_pane_traced_cli_send_with_matching_text_is_refused() {
    let e = setup().await;
    let t = now_s() - 60.0;
    // `amux send` run from inside a worker pane: #7 attributes it to that worker.
    e.seed_owner_send(MRPOPO, INSTRUCTION, t, Some("worker:review-dev-lead"));
    refused_with(&e, Some(max_ref(MRPOPO, t)), "max_ref_not_human_typed", "pane-traced CLI send").await;
}

#[tokio::test]
async fn an_owner_send_with_no_recorded_verdict_is_refused() {
    let e = setup().await;
    let t = now_s() - 60.0;
    e.seed_owner_send(MRPOPO, INSTRUCTION, t, None);
    refused_with(&e, Some(max_ref(MRPOPO, t)), "max_ref_not_human_typed", "no-verdict owner send").await;
}

#[tokio::test]
async fn an_amux_stamped_relay_is_refused() {
    let e = setup().await;
    let t = now_s() - 60.0;
    e.seed_row(MRPOPO, &format!("[amux-origin: {DEV_LEAD}] {INSTRUCTION}"), "user", DEV_LEAD, Some("direct"), t);
    refused_with(&e, Some(max_ref(MRPOPO, t)), "max_ref_not_human_typed", "[amux]-stamped relay").await;
}

#[tokio::test]
async fn a_relay_whose_text_matches_exactly_is_still_refused() {
    let e = setup().await;
    let t = now_s() - 60.0;
    // A peer relayed Max's words verbatim; the text is right, the origin is not.
    e.seed_row(MRPOPO, INSTRUCTION, "user", DEV_LEAD, Some("direct"), t);
    refused_with(&e, Some(max_ref(MRPOPO, t)), "max_ref_not_human_typed", "verbatim peer relay").await;
}

#[tokio::test]
async fn amux_deliveries_are_refused() {
    // Rows amux itself delivered: a queued steering drain, a board pickup, a
    // schedule run. Each is what the pane_prompts ring / cmd_history match
    // treats as "amux delivered this", never as typed.
    for (ty, origin, delivery) in [
        ("steering", "", "queued"),
        ("user", "board", "board"),
        ("user", "scheduler", "direct"),
        ("user", "system", "direct"),
        ("user", "automation", "queued"),
    ] {
        let e = setup().await;
        let t = now_s() - 60.0;
        e.seed_row(MRPOPO, INSTRUCTION, ty, origin, Some(delivery), t);
        refused_with(&e, Some(max_ref(MRPOPO, t)), "max_ref_not_human_typed",
            &format!("amux delivery type={ty} origin={origin} delivery={delivery}")).await;
    }
}

#[tokio::test]
async fn a_missing_max_ref_is_refused() {
    let e = setup().await;
    refused_with(&e, None, "max_ref_missing", "no max_ref").await;
}

#[tokio::test]
async fn a_forged_max_ref_naming_no_row_is_refused() {
    let e = setup().await;
    refused_with(&e, Some(max_ref(MRPOPO, now_s() - 60.0)), "max_ref_no_such_row", "forged").await;
}

#[tokio::test]
async fn a_row_in_another_session_is_the_wrong_session() {
    let e = setup().await;
    let t = now_s() - 60.0;
    // Max typed it into the dev lead's pane; mrpopo cites that pane as its own authority.
    e.seed_pane_prompt(DEV_LEAD, INSTRUCTION, t);
    refused_with(&e, Some(max_ref(DEV_LEAD, t)), "max_ref_wrong_session", "row in another worker's pane").await;
}

#[tokio::test]
async fn a_stale_row_is_refused() {
    let e = setup().await;
    let t = now_s() - 90_000.0; // > MAX_REF_MAX_AGE_S (86 400 s)
    e.seed_pane_prompt(MRPOPO, INSTRUCTION, t);
    refused_with(&e, Some(max_ref(MRPOPO, t)), "max_ref_stale", "stale").await;
}

#[tokio::test]
async fn a_row_outside_the_window_is_refused() {
    let e = setup().await;
    let t = now_s() - 60.0;
    e.seed_pane_prompt(MRPOPO, INSTRUCTION, t);
    // |row.ts - max_ref.time| > MAX_REF_WINDOW_S (600 s)
    refused_with(&e, Some(max_ref(MRPOPO, t - 700.0)), "max_ref_outside_window", "outside window").await;
}

#[tokio::test]
async fn a_row_that_does_not_name_the_target_is_refused() {
    let e = setup().await;
    let t = now_s() - 60.0;
    e.seed_pane_prompt(MRPOPO, "please add review-ops-a to leads", t);
    refused_with(&e, Some(max_ref(MRPOPO, t)), "max_ref_does_not_name_target", "names another worker").await;
}

#[tokio::test]
async fn naming_the_target_is_a_word_match_not_a_substring() {
    let e = setup().await;
    let t = now_s() - 60.0;
    e.seed_pane_prompt(MRPOPO, "please add review-dev-abc to leads", t);
    refused_with(&e, Some(max_ref(MRPOPO, t)), "max_ref_does_not_name_target", "review-dev-abc is not review-dev-a").await;
}

#[tokio::test]
async fn a_malformed_max_ref_is_never_accepted() {
    let e = setup().await;
    let t = now_s() - 60.0;
    e.seed_pane_prompt(MRPOPO, INSTRUCTION, t);
    for bad in [json!("mrpopo"), json!({"session": MRPOPO}), json!({"time": t}), json!({"session": MRPOPO, "time": "yesterday"})] {
        let (st, v) = widen(&e, Some(bad.clone())).await;
        assert!(!st.is_success(), "malformed max_ref {bad} accepted: {v}");
        assert_unchanged(&e, &format!("malformed {bad}"));
    }
}
