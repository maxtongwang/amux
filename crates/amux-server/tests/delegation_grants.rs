//! Structured grants (maxtongwang/ai-config#160): mrpopo's widening change is
//! stored as a pending grant and applied exactly once, only on Max's approval.
//! Approval is a typed `amux grant <code>` in a qualifying row, or the owner's
//! Approve over a Remote-verified connection.
//!
//! Run under `amux-isolated`; the harness (delegation_support) isolates HOME,
//! AMUX_HOME, CC_HOME and tmux.

mod delegation_support;

use delegation_support::*;
use serde_json::json;

fn cfg(w: &str) -> String {
    format!("/api/sessions/{w}/config")
}

async fn ask_yolo(e: &Env) -> String {
    let (st, v) = e.call(Who::Worker(MRPOPO), "PATCH", &cfg(DEV_A), Some(json!({"toggle_yolo": true, "why": "Max asked"}))).await;
    assert_grant_pending(st, &v, "mrpopo asks for yolo on a worker")
}

fn yolo_on(e: &Env) -> bool {
    let t = e.env_text(DEV_A);
    t.contains("dangerously") || t.contains("CC_AUTO_CONTINUE=\"1\"")
}

#[tokio::test]
async fn a_grant_is_created_logged_and_posted_but_not_applied() {
    let e = setup().await;
    let before = e.snapshot();
    let code = ask_yolo(&e).await;
    assert_eq!(e.grant_status(&code), "pending");
    assert_eq!(before, e.snapshot(), "a pending grant changes nothing");
    let entries = e.log_entries();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0]["action"], json!("grant:requested"));
    assert_eq!(entries[0]["grant"], json!(code));
    assert!(entries[0]["after"]["sentence"].as_str().is_some_and(|s| s.contains(&format!("amux grant {code}"))), "{}", entries[0]);
    // The leads feed carries the exact sentence (queued asynchronously).
    let mut seen = false;
    for _ in 0..40 {
        if e.queued_for(DEV_LEAD).iter().any(|t| t.contains(&format!("amux grant {code}"))) {
            seen = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(seen, "the leads feed names the grant: {:?}", e.queued_for(DEV_LEAD));
}

#[tokio::test]
async fn a_typed_code_in_another_lane_applies_exactly_once() {
    let e = setup().await;
    let code = ask_yolo(&e).await;
    let (st, v) = e.type_in_pane(MAX_PANE, &format!("go ahead: amux grant {code}")).await;
    assert_allowed(st, &v, "Max types the code in his own pane");
    assert_eq!(e.grant_status(&code), "applied");
    assert!(yolo_on(&e), "the stored change applied: {}", e.env_text(DEV_A));
    let after_first = e.snapshot();
    // Replay: typing it again (or approving it) does nothing more.
    let (st, v) = e.type_in_pane(MAX_PANE, &format!("again amux grant {code}")).await;
    assert_allowed(st, &v, "typing again is just a message");
    assert_eq!(after_first, e.snapshot(), "a replay does not apply twice");
    let applied = e.log_entries().iter().filter(|m| m["action"] == json!("grant:applied")).count();
    assert_eq!(applied, 1, "{:?}", e.log_entries());
}

#[tokio::test]
async fn an_own_lane_pane_row_does_not_approve() {
    let e = setup().await;
    let code = ask_yolo(&e).await;
    let (st, v) = e.type_in_pane(MRPOPO, &format!("amux grant {code}")).await;
    assert_allowed(st, &v, "the report itself is accepted");
    assert_eq!(e.grant_status(&code), "pending", "mrpopo's own lane cannot approve its grant");
    assert!(!yolo_on(&e));
}

#[tokio::test]
async fn a_wrong_or_expired_code_does_not_apply() {
    let e = setup().await;
    let code = ask_yolo(&e).await;
    let wrong = if code == "ZZZZ" { "YYYY" } else { "ZZZZ" };
    e.type_in_pane(MAX_PANE, &format!("amux grant {wrong}")).await;
    assert_eq!(e.grant_status(&code), "pending");
    e.expire_grant(&code);
    e.type_in_pane(MAX_PANE, &format!("amux grant {code}")).await;
    assert_eq!(e.grant_status(&code), "expired");
    assert!(!yolo_on(&e), "an expired grant never applies");
    assert!(e.log_entries().iter().any(|m| m["action"] == json!("grant:expired")), "{:?}", e.log_entries());
}

#[tokio::test]
async fn the_owner_approve_route_requires_remote_and_deny_works() {
    let e = setup().await;
    let code = ask_yolo(&e).await;
    // In-process owner with no process verdict: refused. A client-supplied
    // verdict header is stripped by attribution, so it is refused too.
    let (st, v) = e.call(Who::Owner, "POST", &format!("/api/delegation/grants/{code}/approve"), None).await;
    assert_code(st, &v, "grant_approve_requires_remote", "owner without Remote");
    // A worker never approves.
    let (st, v) = e.call(Who::Worker(DEV_LEAD), "POST", &format!("/api/delegation/grants/{code}/approve"), None).await;
    assert_code(st, &v, "grant_owner_only", "a lead approving");
    assert_eq!(e.grant_status(&code), "pending");
    // Deny: owner only, final.
    let (st, v) = e.call(Who::Worker(MRPOPO), "POST", &format!("/api/delegation/grants/{code}/deny"), None).await;
    assert_code(st, &v, "grant_owner_only", "mrpopo denying");
    let (st, v) = e.call(Who::Owner, "POST", &format!("/api/delegation/grants/{code}/deny"), None).await;
    assert_allowed(st, &v, "owner denies");
    assert_eq!(e.grant_status(&code), "denied");
    e.type_in_pane(MAX_PANE, &format!("amux grant {code}")).await;
    assert_eq!(e.grant_status(&code), "denied", "a denied grant never applies");
    assert!(!yolo_on(&e));
    assert!(e.log_entries().iter().any(|m| m["action"] == json!("grant:denied")), "{:?}", e.log_entries());
}

#[tokio::test]
async fn pending_grants_are_listed_for_the_owner() {
    let e = setup().await;
    let code = ask_yolo(&e).await;
    let (st, v) = e.call(Who::Owner, "GET", "/api/delegation/grants", None).await;
    assert_allowed(st, &v, "owner lists grants");
    assert_eq!(v["measured"], json!(true));
    assert!(v["grants"].as_array().unwrap().iter().any(|g| g["code"] == json!(code) && g["status"] == json!("pending")), "{v}");
    let (st, v) = e.call(Who::Worker(DEV_A), "GET", "/api/delegation/grants", None).await;
    assert_refused(st, &v, "a plain worker cannot list grants");
}

/// ai-config #175: the owner's alert channels are owner-only, or mrpopo
/// through a grant; a lead or a worker is refused.
#[tokio::test]
async fn alerts_config_goes_through_the_table() {
    let e = setup().await;
    let body = json!({"phone": "+15550100"});
    for who in [DEV_LEAD, DEV_A] {
        let (st, v) = e.call(Who::Worker(who), "PATCH", "/api/alerts/config", Some(body.clone())).await;
        assert_refused(st, &v, &format!("{who} sets alerts config"));
    }
    let (st, v) = e.call(Who::Worker(MRPOPO), "PATCH", "/api/alerts/config", Some(body.clone())).await;
    assert_grant_pending(st, &v, "mrpopo asks for an alerts change");
    let (st, v) = e.call(Who::Owner, "PATCH", "/api/alerts/config", Some(body)).await;
    assert_allowed(st, &v, "owner sets alerts config");
}
