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

    // mrpopo's membership change is a grant: requested (logged), then applied
    // once Max types its code in another lane (approved, the change, applied).
    let (st, v) = e.call(Who::Worker(MRPOPO), "PATCH", &format!("/api/sessions/{DEV_B}/config"),
        Some(with_ref(json!({"tags": "dev,leads"}), json!(null), "Max asked for a second lead"))).await;
    let code = assert_grant_pending(st, &v, "mrpopo membership");
    let entries = e.log_entries();
    assert_eq!(entries.len(), 3, "{entries:?}");
    assert_eq!(entries[2]["action"], json!("grant:requested"), "{}", entries[2]);
    assert_eq!(entries[2]["grant"], json!(code), "{}", entries[2]);
    assert_eq!(entries[2]["why"], json!("Max asked for a second lead"), "{}", entries[2]);
    assert_entry_shape(&entries[2], MRPOPO, "mrpopo", DEV_B);
    // The approved/applied entries are proven in the lib tests (Remote only).
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
    let e = setup().await;
    let mark = e.max_row_id();
    let (st, v) = e.call(Who::Worker(DEV_LEAD), "PATCH", &format!("/api/sessions/{DEV_A}/config"), Some(json!({"model": "sonnet"}))).await;
    assert_allowed(st, &v, "lead model");
    let rows = e.rows_after(mark);
    let feed: Vec<_> = rows.iter().filter(|(_, _, _, text)| text.contains(DEV_A) && text.contains(DEV_LEAD)).collect();
    assert!(!feed.is_empty(), "a feed message names the actor and target: {rows:?}");
    for (session, origin, _, _) in &feed {
        assert!([DEV_LEAD, OPS_LEAD, LEADS_NO_ORCH, MRPOPO].contains(&session.as_str()),
            "feed went to a leads member, not {session}: {rows:?}");
        assert_eq!(origin, "system", "feed is stamped system, never the owner: {rows:?}");
    }
    let (st, v) = e.call(Who::Worker(DEV_A), "PATCH", &format!("/api/sessions/{DEV_B}/config"), Some(json!({"model": "sonnet"}))).await;
    assert_refused(st, &v, "peer");
    let after_refusal = e.rows_after(mark);
    assert_eq!(after_refusal.len(), rows.len(), "a refusal sends no feed message: {after_refusal:?}");
}
