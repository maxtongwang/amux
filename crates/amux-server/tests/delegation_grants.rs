//! Structured grants (maxtongwang/ai-config#160), HTTP level.
//!
//! CITE MODE (default): mrpopo's widening change cites one of Max's typed
//! rows in another lane and applies at once, recorded as a grant: logged, fed
//! to the leads, undoable in one step. Own-lane, delivered, stale or missing
//! citations are refused.
//! PASSKEY MODE (`AMUX_DELEGATION_APPROVAL=passkey`, owner-only): the change is
//! a pending grant; only the approval proof (a non-local Remote owner) applies
//! it. In-process requests carry no Remote verdict, so the approving path is
//! proven in the lib tests (api/delegation_grants.rs, with #7's resolver).
//!
//! Run under `amux-isolated`.

mod delegation_support;

use delegation_support::*;
use serde_json::json;

fn cfg(w: &str) -> String {
    format!("/api/sessions/{w}/config")
}

fn yolo_on(e: &Env) -> bool {
    let t = e.env_text(DEV_A);
    t.contains("dangerously") || t.contains("CC_AUTO_CONTINUE=\"1\"")
}

#[tokio::test]
async fn cite_mode_applies_at_once_logs_feeds_and_undoes_exactly() {
    let e = setup().await;
    let before = e.config_snapshot();
    let row = e.seed_pane_prompt(MAX_PANE, &format!("turn on yolo for {DEV_A}"), now_s() - 30.0);
    let (st, v) = e.call(Who::Worker(MRPOPO), "PATCH", &cfg(DEV_A), Some(cited(json!({"toggle_yolo": true, "why": "Max asked"}), row))).await;
    assert_allowed(st, &v, "cited change applies");
    let code = e.latest_grant();
    assert_eq!(e.grant_status(&code), "applied");
    assert!(yolo_on(&e), "{}", e.env_text(DEV_A));
    let actions: Vec<String> = e.log_entries().iter().filter_map(|m| m["action"].as_str().map(str::to_string)).collect();
    assert!(actions.contains(&"grant:applied".to_string()), "{actions:?}");
    let mut fed = false;
    for _ in 0..40 {
        if e.queued_for(DEV_LEAD).iter().any(|t| t.contains(&format!("amux grant {code}")) && t.contains("turn on yolo")) {
            fed = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(fed, "the leads feed carries the sentence and the cited excerpt: {:?}", e.queued_for(DEV_LEAD));
    // Undo: a plain worker may not; the owner may; the state is restored exactly.
    let (st, v) = e.call(Who::Worker(DEV_A), "POST", &format!("/api/delegation/grants/{code}/undo"), None).await;
    assert_refused(st, &v, "a worker undoing");
    let (st, v) = e.call(Who::Owner, "POST", &format!("/api/delegation/grants/{code}/undo"), None).await;
    assert_allowed(st, &v, "owner undoes");
    assert_eq!(e.grant_status(&code), "undone");
    assert_eq!(before, e.config_snapshot(), "undo restores exactly");
    assert!(e.log_entries().iter().any(|m| m["action"] == json!("grant:undone")), "{:?}", e.log_entries());
}

#[tokio::test]
async fn cite_mode_refuses_own_lane_delivered_stale_and_missing_citations() {
    let e = setup().await;
    let now = now_s();
    let own = e.seed_pane_prompt(MRPOPO, &format!("turn on yolo for {DEV_A}"), now - 30.0);
    let delivered = e.seed_pane_prompt(MAX_PANE, &format!("[amux-origin: {DEV_LEAD}] turn on yolo for {DEV_A}"), now - 30.0);
    let stale = e.seed_pane_prompt(MAX_PANE, &format!("turn on yolo for {DEV_A}"), now - 90_000.0);
    let before = e.snapshot();
    for (body, code) in [
        (json!({"toggle_yolo": true}), "cite_missing"),
        (cited(json!({"toggle_yolo": true}), own), "max_ref_own_lane"),
        (cited(json!({"toggle_yolo": true}), delivered), "max_ref_not_human_typed"),
        (cited(json!({"toggle_yolo": true}), stale), "max_ref_stale"),
    ] {
        let (st, v) = e.call(Who::Worker(MRPOPO), "PATCH", &cfg(DEV_A), Some(body)).await;
        assert_code(st, &v, code, code);
    }
    assert_eq!(before, e.snapshot());
    assert_eq!(e.latest_grant(), "", "a refused citation records no grant");
}

#[tokio::test]
async fn a_lead_or_worker_citing_max_gets_nothing() {
    let e = setup().await;
    let row = e.seed_pane_prompt(MAX_PANE, &format!("turn on yolo for {DEV_A}"), now_s() - 30.0);
    for who in [DEV_LEAD, DEV_A, OPS_A] {
        let (st, v) = e.call(Who::Worker(who), "PATCH", &cfg(DEV_A), Some(cited(json!({"toggle_yolo": true}), row))).await;
        assert_refused(st, &v, &format!("{who} citing Max"));
    }
    assert!(!yolo_on(&e));
}

#[tokio::test]
async fn passkey_mode_creates_pending_grants_that_local_requests_cannot_approve() {
    let e = setup().await;
    e.set_mode("passkey");
    let row = e.seed_pane_prompt(MAX_PANE, &format!("turn on yolo for {DEV_A}"), now_s() - 30.0);
    let (st, v) = e.call(Who::Worker(MRPOPO), "PATCH", &cfg(DEV_A), Some(cited(json!({"toggle_yolo": true}), row))).await;
    let code = assert_grant_pending(st, &v, "passkey mode: pending even with a citation");
    assert!(!yolo_on(&e));
    // A pane row naming the code, and a local owner Approve, are not proof.
    e.type_in_pane(MAX_PANE, &format!("amux grant {code}")).await;
    assert_eq!(e.grant_status(&code), "pending");
    let (st, v) = e.call(Who::Owner, "POST", &format!("/api/delegation/grants/{code}/approve"), None).await;
    assert_code(st, &v, "grant_approve_requires_remote", "a local owner approve");
    let (st, v) = e.call(Who::Worker(DEV_LEAD), "POST", &format!("/api/delegation/grants/{code}/approve"), None).await;
    assert_code(st, &v, "grant_owner_only", "a lead approving");
    // Deny: owner only, final.
    let (st, v) = e.call(Who::Worker(MRPOPO), "POST", &format!("/api/delegation/grants/{code}/deny"), None).await;
    assert_code(st, &v, "grant_owner_only", "mrpopo denying");
    let (st, v) = e.call(Who::Owner, "POST", &format!("/api/delegation/grants/{code}/deny"), None).await;
    assert_allowed(st, &v, "owner denies");
    assert_eq!(e.grant_status(&code), "denied");
    assert!(!yolo_on(&e));
}

/// The mode setting is owner-only: hands-only for every worker, mrpopo included.
#[tokio::test]
async fn the_mode_setting_is_owner_only() {
    let e = setup().await;
    let row = e.seed_pane_prompt(MAX_PANE, "switch delegation to cite mode", now_s() - 30.0);
    for who in [MRPOPO, DEV_LEAD, DEV_A] {
        for (level, name) in [("global", ""), ("worker", DEV_A)] {
            let body = cited(scope_body(level, name, "env", json!({"AMUX_DELEGATION_APPROVAL": "cite"})), row);
            let (st, v) = e.call(Who::Worker(who), "PUT", "/api/scope", Some(body)).await;
            assert_refused(st, &v, &format!("{who} switching the mode at {level}"));
        }
    }
    let (st, v) = e.call(Who::Owner, "PUT", "/api/scope", Some(scope_body("global", "", "env", json!({"AMUX_DELEGATION_APPROVAL": "passkey"})))).await;
    assert_allowed(st, &v, "the owner switches the mode");
}

#[tokio::test]
async fn whoami_answers_in_process() {
    let e = setup().await;
    let (st, v) = e.call(Who::Owner, "GET", "/api/delegation/whoami", None).await;
    assert_allowed(st, &v, "whoami");
    assert_eq!((v["process_origin"].clone(), v["forwarded"].clone(), v["approval_proof"].clone()), (json!(null), json!(false), json!(false)), "{v}");
    assert_eq!(v["mode"], json!("cite"));
}

/// ai-config #175: the owner's alert channels are owner-only, or mrpopo
/// citing Max; a lead or a worker is refused.
#[tokio::test]
async fn alerts_config_goes_through_the_table() {
    let e = setup().await;
    let body = json!({"phone": "+15550100"});
    for who in [DEV_LEAD, DEV_A] {
        let (st, v) = e.call(Who::Worker(who), "PATCH", "/api/alert/config", Some(body.clone())).await;
        assert_refused(st, &v, &format!("{who} sets alerts config"));
    }
    let (st, v) = e.call(Who::Worker(MRPOPO), "PATCH", "/api/alert/config", Some(body.clone())).await;
    assert_code(st, &v, "cite_missing", "mrpopo without a citation");
    let row = e.seed_pane_prompt(MAX_PANE, "set my alert phone to +15550100", now_s() - 30.0);
    let (st, v) = e.call(Who::Worker(MRPOPO), "PATCH", "/api/alert/config", Some(cited(body.clone(), row))).await;
    assert_allowed(st, &v, "mrpopo citing Max");
    let (st, v) = e.call(Who::Owner, "PATCH", "/api/alert/config", Some(body)).await;
    assert_allowed(st, &v, "owner sets alerts config");
}
