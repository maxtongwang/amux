//! The delegated-change log and leads feed (maxtongwang/ai-config#160, pieces c
//! and d): append-only JSONL at `<amux_home>/audit/delegated-changes.jsonl`,
//! `GET /api/delegation/log`, and a message to the `leads` group stamped
//! system.

mod delegation_support;

use delegation_support::*;
use serde_json::{json, Value};

#[track_caller]
fn assert_entry_shape(entry: &Value, actor: &str, class: &str, target: &str) {
    assert_eq!(entry["actor"], json!(actor), "{entry}");
    assert_eq!(entry["class"], json!(class), "{entry}");
    assert_eq!(entry["target"], json!(target), "{entry}");
    assert!(entry["action"].as_str().is_some_and(|a| !a.is_empty()), "action: {entry}");
    assert!(entry["ts"].as_f64().is_some_and(|ts| (now_s() - ts).abs() < 300.0), "ts is unix seconds, now: {entry}");
    assert!(entry.get("before").is_some() && entry.get("after").is_some(), "before/after present: {entry}");
    assert_ne!(entry["before"], entry["after"], "before and after differ for a change: {entry}");
}

#[tokio::test]
async fn each_allowed_delegated_change_appends_exactly_one_entry() {
    let e = setup().await;
    let (st, v) = e.call(Who::Worker(DEV_LEAD), "PATCH", &format!("/api/sessions/{DEV_A}/config"), Some(json!({"model": "sonnet"}))).await;
    assert_allowed(st, &v, "lead model on T worker");
    let entries = e.log_entries();
    assert_eq!(entries.len(), 1, "one allowed change, one entry: {entries:?}");
    assert_entry_shape(&entries[0], DEV_LEAD, "lead:dev", DEV_A);
    assert!(entries[0]["after"].to_string().contains("sonnet"), "after carries the new model: {}", entries[0]);

    let (st, v) = e.call(Who::Worker(DEV_A), "PATCH", &format!("/api/sessions/{DEV_A}/config"), Some(json!({"effort": "low", "model": "haiku"}))).await;
    assert_allowed(st, &v, "self model+effort");
    let entries = e.log_entries();
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert_entry_shape(&entries[1], DEV_A, "self", DEV_A);

    // mrpopo applies the exact request after Max approves in the shown session;
    // both the change and the applied grant are logged.
    let row = e.approved_request(MRPOPO,"PATCH",&format!("/api/sessions/{DEV_B}/config"),json!({"tags":"dev,leads"})).await;
    let (st, v) = e.call(Who::Worker(MRPOPO), "PATCH", &format!("/api/sessions/{DEV_B}/config"),
        Some(with_request(json!({"tags": "dev,leads", "why": "Max asked for a second lead"}), row.clone()))).await;
    assert_allowed(st, &v, "mrpopo membership with a citation");
    let code = e.latest_grant();
    assert_eq!(e.grant_status(&code), "applied");
    let entries = e.log_entries();
    assert_eq!(entries.len(), 4, "{entries:?}");
    let m = &entries[2];
    assert_entry_shape(m, MRPOPO, "mrpopo", DEV_B);
    assert_eq!(m["grant"], json!(code), "{m}");
    assert!(m["evidence_row"].as_i64().is_some(), "Max's reply is the evidence: {m}");
    assert_eq!(m["why"], json!("Max asked for a second lead"), "{m}");
    assert_eq!(entries[3]["action"], json!("grant:applied"), "{}", entries[3]);
    let id=row.as_str().unwrap();
    assert_eq!(entries[3]["after"]["cited_excerpt"], json!(format!("yes {}",e.short_code(id))), "{}", entries[3]);
    let question: String=e.store.read().unwrap().query_row(
        "SELECT ask_text FROM _amux_delegation_grants WHERE code=?1",[id],|r|r.get(0)).unwrap();
    assert_eq!(entries[3]["after"]["ask_text"],json!(question),"the audit retains the exact question");
    assert!(question.contains(DEV_B) && question.contains("dev,leads"),"the question retains the exact change: {question}");
}

#[tokio::test]
async fn refused_attempts_and_owner_changes_add_no_entries() {
    let e = setup().await;
    let (st, v) = e.call(Who::Worker(DEV_LEAD), "PATCH", &format!("/api/sessions/{OPS_A}/config"), Some(json!({"model": "sonnet"}))).await;
    assert_refused(st, &v, "lead on another team");
    let (st, v) = e.call(Who::Worker(DEV_A), "PATCH", &format!("/api/sessions/{DEV_B}/config"), Some(json!({"model": "sonnet"}))).await;
    assert_refused(st, &v, "peer");
    assert!(e.log_entries().is_empty(), "refusals are not changes: {:?}", e.log_entries());

    let (st, v) = e.call(Who::Owner, "PATCH", &format!("/api/sessions/{DEV_A}/config"), Some(json!({"model": "sonnet"}))).await;
    assert_allowed(st, &v, "owner");
    assert!(e.log_entries().is_empty(), "the owner's own changes are not delegated: {:?}", e.log_entries());
}

#[tokio::test]
async fn the_log_file_is_private_and_append_only() {
    let e = setup().await;
    for model in ["sonnet", "haiku"] {
        let (st, v) = e.call(Who::Worker(DEV_LEAD), "PATCH", &format!("/api/sessions/{DEV_A}/config"), Some(json!({"model": model}))).await;
        assert_allowed(st, &v, model);
    }
    let first = e.log_raw();
    assert_eq!(e.log_entries().len(), 2);
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(e.log_path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "log mode {mode:o}");
    }
    let (st, v) = e.call(Who::Worker(DEV_LEAD), "PATCH", &format!("/api/sessions/{DEV_A}/config"), Some(json!({"model": "opus"}))).await;
    assert_allowed(st, &v, "third");
    let after = e.log_raw();
    assert!(after.starts_with(&first), "earlier entries are never rewritten");
    // No API path edits or deletes entries.
    for (who, method) in [(Who::Owner, "DELETE"), (Who::Owner, "PUT"), (Who::Owner, "PATCH"), (Who::Worker(DEV_LEAD), "DELETE")] {
        let (st, v) = e.call(who, method, "/api/delegation/log", Some(json!({"entries": []}))).await;
        assert!(!st.is_success(), "{method} /api/delegation/log succeeded: {v}");
    }
    assert_eq!(e.log_raw(), after);
}

#[tokio::test]
async fn secrets_in_a_logged_change_are_redacted() {
    let e = setup().await;
    let secret = "sk-review-0123456789abcdef";
    std::fs::write(
        e.env_path(DEV_A),
        format!("CC_DIR=\"/tmp\"\nCC_TAGS=\"dev\"\nOPENAI_API_KEY=\"{secret}\"\nAMUX_WORKER_TOKEN=\"tok-review-secret\"\n"),
    )
    .unwrap();
    let (st, v) = e.call(Who::Worker(DEV_LEAD), "PATCH", &format!("/api/sessions/{DEV_A}/config"), Some(json!({"model": "sonnet"}))).await;
    assert_allowed(st, &v, "lead model on a worker holding secrets");
    assert_eq!(e.log_entries().len(), 1);
    let raw = e.log_raw();
    assert!(!raw.contains(secret) && !raw.contains("tok-review-secret"), "secret value reached the log: {raw}");
    // The redaction leaves the key visible when the snapshot carries it.
    if raw.contains("OPENAI_API_KEY") {
        assert!(raw.contains("<redacted>"), "{raw}");
    }
}

#[tokio::test]
async fn get_delegation_log_is_for_the_owner_and_leads() {
    let e = setup().await;
    for model in ["sonnet", "haiku", "opus"] {
        let (st, v) = e.call(Who::Worker(DEV_LEAD), "PATCH", &format!("/api/sessions/{DEV_A}/config"), Some(json!({"model": model}))).await;
        assert_allowed(st, &v, model);
    }
    for who in [Who::Owner, Who::Worker(DEV_LEAD), Who::Worker(OPS_LEAD)] {
        let (st, v) = e.call(who, "GET", "/api/delegation/log?limit=2", None).await;
        assert_eq!(st.as_u16(), 200, "{who:?}: {v}");
        assert_eq!(v["measured"], json!(true), "{who:?}: {v}");
        assert!(v["n_considered"].as_u64().is_some_and(|n| n >= 3), "{who:?}: {v}");
        let entries = v["entries"].as_array().unwrap_or_else(|| panic!("{who:?}: entries array: {v}"));
        assert_eq!(entries.len(), 2, "limit honored: {v}");
        assert!(entries[0]["after"].to_string().contains("opus"), "newest first: {v}");
        assert!(entries[1]["after"].to_string().contains("haiku"), "newest first: {v}");
    }
    for who in [DEV_A, OPS_A, LEADS_NO_ORCH] {
        let (st, v) = e.call(Who::Worker(who), "GET", "/api/delegation/log", None).await;
        assert_eq!(st.as_u16(), 403, "{who}: {v}");
    }
}

#[tokio::test]
async fn an_allowed_change_reaches_the_leads_group_stamped_system() {
    // The feed is queued to the other leads as amux automation (its guard and
    // empty sender are pinned in delegation_log's unit tests), never as an
    // owner send; it is posted in the background, so poll.
    let e = setup().await;
    let (st, v) = e.call(Who::Worker(DEV_LEAD), "PATCH", &format!("/api/sessions/{DEV_A}/config"), Some(json!({"model": "sonnet"}))).await;
    assert_allowed(st, &v, "lead model");
    let names = |t: &String| t.contains(DEV_A) && t.contains(DEV_LEAD);
    let mut fed = Vec::new();
    for _ in 0..40 {
        fed = e.queued_for(OPS_LEAD).into_iter().filter(names).collect();
        if !fed.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(!fed.is_empty(), "a feed message to another lead names the actor and target: {:?}", e.queued_for(OPS_LEAD));
    for lane in [DEV_A, DEV_B, OPS_A] {
        assert!(e.queued_for(lane).is_empty(), "the feed went to non-lead {lane}: {:?}", e.queued_for(lane));
    }
    let (st, v) = e.call(Who::Worker(DEV_A), "PATCH", &format!("/api/sessions/{DEV_B}/config"), Some(json!({"model": "sonnet"}))).await;
    assert_refused(st, &v, "peer");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(!e.queued_for(OPS_LEAD).iter().any(|t| t.contains(DEV_B)), "a refusal sends no feed message: {:?}", e.queued_for(OPS_LEAD));
}
