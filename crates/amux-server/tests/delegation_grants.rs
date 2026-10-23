//! Structured grants (maxtongwang/ai-config#160), HTTP level.
//!
//! CITE MODE (default): mrpopo's widening change cites a PAIR (review B4):
//! its own ask into a lane, naming the target and the setting, and Max's
//! first typed reply there within 2 hours. It applies at once, recorded as a
//! grant: logged, fed to the leads, undoable in one step. Own-lane, delivered,
//! stale, unrelated or single-row citations are refused.
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

/// Second B1: older asks do not bind, and the reply itself is single-use
/// across actors, targets and routes, including after an owner undo.
#[tokio::test]
async fn two_asks_one_reply_funds_only_latest_once_across_actors_workers_and_routes() {
    let e = setup().await;
    let old = e.seed_row(MAX_PANE, &format!("Max, may I remove the dev tag from {DEV_A}?"), "session", DEV_LEAD, Some("direct"), now_s() - 90.0);
    let latest = e.seed_row(MAX_PANE, &format!("Max, may I remove the dev tag from {DEV_B}?"), "session", DEV_LEAD, Some("direct"), now_s() - 60.0);
    let peer = e.seed_row(MAX_PANE, &format!("Max, may I remove the dev tag from {DEV_A}?"), "session", DEV_LEAD2, Some("direct"), now_s() - 45.0);
    let reply = e.seed_pane_prompt(MAX_PANE, "yes", now_s() - 30.0);
    let pair = |ask| json!({"ask_row_id": ask, "reply_row_id": reply});
    let (st, v) = e.call(Who::Worker(DEV_LEAD), "PATCH", &cfg(DEV_A), Some(cited(json!({"tags": ""}), pair(old)))).await;
    assert_code(st, &v, "cite_ask_not_latest", "older ask cannot bind the reply");
    assert_eq!(e.latest_grant(), "");
    let (st, v) = e.call(Who::Worker(DEV_LEAD), "PATCH", &cfg(DEV_B), Some(cited(json!({"tags": ""}), pair(latest)))).await;
    assert_allowed(st, &v, "latest ask gets the single grant");
    let code = e.latest_grant();
    let (st, v) = e.call(Who::Owner, "POST", &format!("/api/delegation/grants/{code}/undo"), None).await;
    assert_allowed(st, &v, "undo does not release the reply");
    for (actor, ask, method, path, body) in [
        (DEV_LEAD, latest, "PATCH", cfg(DEV_B), json!({"tags": ""})),
        (DEV_LEAD, old, "PATCH", cfg(DEV_A), json!({"tags": ""})),
        (DEV_LEAD2, peer, "PATCH", cfg(DEV_A), json!({"tags": ""})),
        (DEV_LEAD2, peer, "PUT", "/api/scope".into(), scope_body("worker", DEV_A, "env", json!({"CC_TAGS": ""}))),
    ] {
        let before = e.config_snapshot();
        let (st, v) = e.call(Who::Worker(actor), method, &path, Some(cited(body, pair(ask)))).await;
        assert_code(st, &v, "cite_pair_spent", "the reply is spent regardless of ask/actor/worker/route");
        assert_eq!(before, e.config_snapshot());
    }
    let conn = e.store.read().unwrap();
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM _amux_delegation_grants WHERE evidence_row=?1", [reply], |r| r.get(0)).unwrap();
    assert_eq!(count, 1);
    drop(conn);
    assert_eq!(e.grant_status(&code), "undone");
}

#[tokio::test]
async fn non_affirmative_owner_replies_refuse_without_effects() {
    let e = setup().await;
    for text in ["no", "wait", "not yet", "yes?", "don't do it", "no, go ahead", "yes, don't proceed"] {
        let a = e.seed_row(MAX_PANE, &format!("Max, may I remove the dev tag from {DEV_B}?"), "session", DEV_LEAD, Some("direct"), now_s() - 60.0);
        let reply = e.seed_pane_prompt(MAX_PANE, text, now_s() - 30.0);
        let before = e.config_snapshot();
        let (st, v) = e.call(Who::Worker(DEV_LEAD), "PATCH", &cfg(DEV_B), Some(cited(json!({"tags": ""}), json!({"ask_row_id": a, "reply_row_id": reply})))).await;
        assert_code(st, &v, "cite_reply_not_affirmative", text);
        assert_eq!(before, e.config_snapshot(), "{text}");
        assert_eq!(e.latest_grant(), "", "refusal must not spend a grant");
    }
}

#[tokio::test]
async fn multiple_settings_in_one_body_reserve_one_immutable_grant() {
    let e = setup().await;
    let pair = e.cite(&format!("Max, may I enable yolo for {DEV_A} and enable auto continue for {DEV_A}?"));
    let body = json!({"toggle_yolo": true, "toggle_auto_continue": true});
    let (st, v) = e.call(Who::Worker(MRPOPO), "PATCH", &cfg(DEV_A), Some(cited(body.clone(), pair.clone()))).await;
    assert_allowed(st, &v, "one body with two settings");
    let conn = e.store.read().unwrap();
    let (count, stored): (i64, String) = conn.query_row("SELECT COUNT(*), body FROM _amux_delegation_grants", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(count, 1);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&stored).unwrap(), body);
    drop(conn);
    let (st, v) = e.call(Who::Worker(MRPOPO), "PATCH", &cfg(DEV_B), Some(cited(body, pair))).await;
    assert_code(st, &v, "cite_pair_spent", "body cannot be replayed on another target");
}

#[tokio::test]
async fn batch_targets_cannot_launder_one_reply_through_declarative_apply() {
    let e = setup().await;
    let pair = e.cite(&format!("Max, may I add leads to {DEV_A} and add leads to {DEV_B}?"));
    for (method, path, workers) in [
        ("PUT", "/api/config/apply", json!([{"name": DEV_A, "tags": ["dev", "leads"]}, {"name": DEV_B, "tags": ["dev", "leads"]}])),
        ("POST", "/api/env/apply", json!([{"name": DEV_A, "groups": ["dev", "leads"]}, {"name": DEV_B, "groups": ["dev", "leads"]}])),
    ] {
        let before = e.config_snapshot();
        let (st, v) = e.call(Who::Worker(MRPOPO), method, path, Some(json!({"workers": workers, "cite": pair.clone()}))).await;
        assert_refused(st, &v, "batch targets have no cite-mode bypass");
        assert_eq!(before, e.config_snapshot());
        assert_eq!(e.latest_grant(), "");
    }
}

#[tokio::test]
async fn cite_mode_applies_at_once_logs_feeds_and_undoes_exactly() {
    let e = setup().await;
    let before = e.config_snapshot();
    let row = e.cite(&format!("Max, may I turn on yolo for {DEV_A}?"));
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
    assert!(fed, "the leads feed carries the sentence and the ask: {:?}", e.queued_for(DEV_LEAD));
    assert!(e.queued_for(DEV_LEAD).iter().any(|t| t.contains("Max replied: \"yes, go ahead\"")), "and Max's reply: {:?}", e.queued_for(DEV_LEAD));
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
    let ask = format!("Max, may I turn on yolo for {DEV_A}?");
    let pair = |lane: &str, reply: &str, at: f64| {
        let a = e.seed_row(lane, &ask, "session", MRPOPO, Some("direct"), at - 30.0);
        json!({"ask_row_id": a, "reply_row_id": e.seed_pane_prompt(lane, reply, at)})
    };
    let own = pair(MRPOPO, "yes", now - 30.0);
    let delivered = pair(DEV_B, &format!("[amux-origin: {DEV_LEAD}] yes"), now - 30.0);
    let stale = pair(OPS_A, "yes", now - 90_000.0);
    let lunch = e.seed_pane_prompt(MAX_PANE, "what time is lunch?", now - 20.0);
    let a = e.seed_row(MAX_PANE, "lunch at noon?", "session", MRPOPO, Some("direct"), now - 15.0);
    let unrelated = json!({"ask_row_id": a, "reply_row_id": e.seed_pane_prompt(MAX_PANE, "ok", now - 10.0)});
    let before = e.snapshot();
    for (body, code) in [
        (json!({"toggle_yolo": true}), "cite_missing"),
        (json!({"toggle_yolo": true, "cite": {"row_id": lunch}}), "cite_pair_required"),
        (cited(json!({"toggle_yolo": true}), unrelated), "cite_ask_missing_target"),
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
    let row = e.cite(&format!("Max, may I turn on yolo for {DEV_A}?"));
    for who in [DEV_LEAD, DEV_A, OPS_A] {
        let (st, v) = e.call(Who::Worker(who), "PATCH", &cfg(DEV_A), Some(cited(json!({"toggle_yolo": true}), row.clone()))).await;
        assert_refused(st, &v, &format!("{who} citing Max"));
    }
    assert!(!yolo_on(&e));
}

#[tokio::test]
async fn passkey_mode_creates_pending_grants_that_local_requests_cannot_approve() {
    let e = setup().await;
    e.set_mode("passkey");
    let row = e.cite(&format!("Max, may I turn on yolo for {DEV_A}?"));
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
    let row = e.cite("Max, may I switch the delegation approval mode to cite globally?");
    for who in [MRPOPO, DEV_LEAD, DEV_A] {
        for (level, name) in [("global", ""), ("worker", DEV_A)] {
            let body = cited(scope_body(level, name, "env", json!({"AMUX_DELEGATION_APPROVAL": "cite"})), row.clone());
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

/// ai-config #175: the owner's alert channels are owner-only. A lead or a
/// worker is refused, and so is mrpopo since review B3: an instance-wide
/// setting is the global layer, which includes mrpopo itself.
#[tokio::test]
async fn alerts_config_goes_through_the_table() {
    let e = setup().await;
    let body = json!({"phone": "+15550100"});
    for who in [DEV_LEAD, DEV_A] {
        let (st, v) = e.call(Who::Worker(who), "PATCH", "/api/alert/config", Some(body.clone())).await;
        assert_refused(st, &v, &format!("{who} sets alerts config"));
    }
    let row = e.cite("Max, may I set your alert phone for the fleet to +15550100?");
    let before = e.snapshot();
    for b in [body.clone(), cited(body.clone(), row)] {
        let (st, v) = e.call(Who::Worker(MRPOPO), "PATCH", "/api/alert/config", Some(b)).await;
        assert_code(st, &v, "held_for_max", "mrpopo, an instance-wide setting");
    }
    assert_eq!(before, e.snapshot(), "nothing written");
    let (st, v) = e.call(Who::Owner, "PATCH", "/api/alert/config", Some(body)).await;
    assert_allowed(st, &v, "owner sets alerts config");
}


/// PR #18 R3 B1: an immediate replay, an owner reversal and another target
/// all preserve the original grant and cannot spend the same pair again.
#[tokio::test]
async fn citation_pair_is_spent_after_one_change_including_owner_reversal() {
    let e = setup().await;
    let a = e.seed_row(MAX_PANE, &format!("Max, {DEV_A} is busy; may I remove the dev tag from {DEV_B}?"),
        "session", DEV_LEAD, Some("direct"), now_s() - 60.0);
    let pair = json!({"ask_row_id": a, "reply_row_id": e.seed_pane_prompt(MAX_PANE, "yes", now_s() - 30.0)});
    let body = cited(json!({"tags": ""}), pair);
    let (st, v) = e.call(Who::Worker(DEV_LEAD), "PATCH", &cfg(DEV_B), Some(body.clone())).await;
    assert_allowed(st, &v, "first use");
    let grant = e.latest_grant();
    for target in [DEV_B, DEV_A] {
        let (st, v) = e.call(Who::Worker(DEV_LEAD), "PATCH", &cfg(target), Some(body.clone())).await;
        assert_code(st, &v, "cite_pair_spent", "same pair replay");
    }
    let (st, v) = e.call(Who::Owner, "PATCH", &cfg(DEV_B), Some(json!({"tags":"dev"}))).await;
    assert_allowed(st, &v, "owner restores membership");
    let (st, v) = e.call(Who::Worker(DEV_LEAD), "PATCH", &cfg(DEV_B), Some(body)).await;
    assert_code(st, &v, "cite_pair_spent", "owner reversal cannot be undone by replay");
    assert_eq!(e.tags(DEV_B), ["dev".to_string()].into_iter().collect());
    assert_eq!(e.latest_grant(), grant);
    let conn = e.store.read().unwrap();
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM _amux_delegation_grants WHERE ask_row=?1 AND cite_spent_at IS NOT NULL", [a], |r| r.get(0)).unwrap();
    assert_eq!(count, 1, "one persisted spent marker and one grant");
}

#[tokio::test]
async fn mrpopo_cannot_reuse_a_pair_on_config_scope_or_after_undo() {
    let e = setup().await;
    let pair = e.cite(&format!("Max, may I enable yolo for {DEV_A}?"));
    let body = cited(json!({"toggle_yolo":true}), pair.clone());
    let (st, v) = e.call(Who::Worker(MRPOPO), "PATCH", &cfg(DEV_A), Some(body.clone())).await;
    assert_allowed(st, &v, "first mrpopo change");
    let code = e.latest_grant();
    let (st, v) = e.call(Who::Owner, "POST", &format!("/api/delegation/grants/{code}/undo"), None).await;
    assert_allowed(st, &v, "undo");
    for (method, path, body) in [
        ("PATCH", cfg(DEV_A), body),
        ("PUT", "/api/scope".into(), cited(scope_body("worker", DEV_B, "env", json!({"CC_FLAGS":"--dangerously-skip-permissions"})), pair)),
    ] {
        let (st, v) = e.call(Who::Worker(MRPOPO), method, &path, Some(body)).await;
        assert_code(st, &v, "cite_pair_spent", "undo/route change never frees citation");
    }
    assert!(!yolo_on(&e));
}

#[tokio::test]
async fn citation_requires_the_actual_tag_direction_and_target() {
    let e = setup().await;
    for (ask, code) in [
        (format!("Max, may I change {DEV_B}?"), "cite_ask_missing_setting"),
        (format!("Max, may I add the dev tag to {DEV_B}?"), "cite_ask_missing_change"),
        (format!("Max, {DEV_B} is busy; may I remove the dev tag from {DEV_A}?"), "cite_ask_missing_change"),
        (format!("Max, {DEV_B} has the dev tag."), "cite_ask_missing_change"),
    ] {
        let a = e.seed_row(MAX_PANE, &ask, "session", DEV_LEAD, Some("direct"), now_s() - 60.0);
        let pair = json!({"ask_row_id":a,"reply_row_id":e.seed_pane_prompt(MAX_PANE,"yes",now_s()-30.0)});
        let before = e.snapshot();
        let (st, v) = e.call(Who::Worker(DEV_LEAD), "PATCH", &cfg(DEV_B), Some(cited(json!({"tags":""}),pair))).await;
        assert_code(st, &v, code, &ask);
        let error = v["error"].as_str().expect("refusal explains the required ask");
        assert!(error.contains(&format!("remove dev from {DEV_B}")), "{v}");
        assert_eq!(e.snapshot(), before, "refused citation writes nothing");
    }
    assert_eq!(e.latest_grant(), "");
}

/// The same name is insufficient both before first cite use and after a
/// passkey grant is pending. No tmux rename is needed: move the registry
/// files (the production identity substrate) preserving their incarnations.
#[tokio::test]
async fn citations_and_pending_grants_do_not_follow_reused_worker_names() {
    for passkey in [false, true] {
        let e = setup().await;
        let pair = e.cite(&format!("Max, may I enable yolo for {DEV_A}?"));
        let mut pending = String::new();
        if passkey {
            e.set_mode("passkey");
            let (st,v) = e.call(Who::Worker(MRPOPO), "PATCH", &cfg(DEV_A), Some(json!({"toggle_yolo":true}))).await;
            pending = assert_grant_pending(st,&v,"pending for original worker");
            let conn = e.store.read().unwrap();
            let inc: String = conn.query_row("SELECT target_incarnation FROM _amux_delegation_grants WHERE code=?1",[&pending],|r|r.get(0)).unwrap();
            assert_eq!(inc,format!("inc-{DEV_A}"));
        }
        let sessions = e.home().join("sessions");
        std::fs::rename(sessions.join(format!("{DEV_A}.env")), sessions.join("review-renamed.env")).unwrap();
        std::fs::rename(sessions.join(format!("{DEV_B}.env")), sessions.join(format!("{DEV_A}.env"))).unwrap();
        if passkey {
            // The public approval route requires non-local proof. The same
            // identity guard is checked again by claim_applying; exercised in lib tests.
            let conn = e.store.read().unwrap();
            let status: String = conn.query_row("SELECT status FROM _amux_delegation_grants WHERE code=?1",[pending],|r|r.get(0)).unwrap();
            assert_eq!(status,"pending");
            e.set_mode("cite");
        }
        let before = e.config_snapshot();
        let (st,v) = e.call(Who::Worker(MRPOPO),"PATCH",&cfg(DEV_A),Some(cited(json!({"toggle_yolo":true}),pair))).await;
        assert_code(st,&v,"cite_target_identity_changed","old ask cannot approve new holder of the name");
        assert_eq!(e.config_snapshot(),before);
        assert!(!yolo_on(&e));
    }
}

#[tokio::test]
async fn tags_arrays_and_strings_store_the_same_canonical_membership_on_both_routes() {
    let e = setup().await;
    for tags in [json!([" OPS ","dev, DEV", ""]), json!(" OPS ,dev,DEV,")] {
        for scope in [false,true] {
            let (method,path,body) = if scope { ("PUT","/api/scope".to_string(),scope_body("worker",DEV_A,"env",json!({"CC_TAGS":tags}))) }
                else { ("PATCH",cfg(DEV_A),json!({"tags":tags})) };
            let (st,v) = e.call(Who::Owner,method,&path,Some(body)).await;
            assert_allowed(st,&v,"canonical tags");
            assert!(e.env_text(DEV_A).lines().any(|l| l == "CC_TAGS=dev,ops" || l == "CC_TAGS=\"dev,ops\""), "{}",e.env_text(DEV_A));
            assert_eq!(e.tags(DEV_A), ["dev".into(),"ops".into()].into_iter().collect());
        }
    }
    for scope in [false,true] {
        let bad = json!(["dev", 7]);
        let (method,path,body) = if scope { ("PUT","/api/scope".to_string(),scope_body("worker",DEV_A,"env",json!({"CC_TAGS":bad}))) }
            else { ("PATCH",cfg(DEV_A),json!({"tags":bad})) };
        let before = e.snapshot();
        let (st,v) = e.call(Who::Owner,method,&path,Some(body)).await;
        assert_eq!(st.as_u16(),400,"{v}");
        assert_eq!(v["code"],"tags_invalid");
        assert_eq!(e.snapshot(),before);
    }
}

#[tokio::test]
async fn citation_settings_reject_passing_target_mentions_values_and_negations() {
    let e = setup().await;
    // Seed the citing actor's latest unanswered ask, immediately followed by
    // the owner reply, so identity/ordering cannot mask the change guard.
    let cite = |actor: &str, ask: &str| {
        let ask = e.seed_row(MAX_PANE, ask, "session", actor, Some("direct"), now_s() - 60.0);
        let reply = e.seed_pane_prompt(MAX_PANE, "yes, go ahead", now_s() - 30.0);
        json!({"ask_row_id": ask, "reply_row_id": reply})
    };
    for ask in [
        format!("Max, may I enable yolo for {DEV_B} and leave {DEV_A} alone?"),
        format!("Max, may I disable yolo for {DEV_A}?"),
        format!("Max, do not enable yolo for {DEV_A}?"),
        format!("Max, don't enable yolo for {DEV_A}?"),
        format!("Max, no yolo for {DEV_A}?"),
        format!("Max, may I enable yolo for not {DEV_A}?"),
    ] {
        let pair = cite(MRPOPO, &ask);
        let before = e.snapshot();
        let (st, v) = e.call(Who::Worker(MRPOPO), "PATCH", &cfg(DEV_A), Some(cited(json!({"toggle_yolo":true}),pair))).await;
        assert_code(st, &v, "cite_ask_missing_change", &ask);
        assert_eq!(e.snapshot(), before, "refusal writes nothing");
    }
    for request in [format!("Max, don't remove dev from {DEV_B}"), format!("Max, do not remove dev from {DEV_B}"), format!("Max, remove dev from {DEV_B}, not {DEV_B}")] {
        for suffix in ["", "?"] {
            let ask = format!("{request}{suffix}");
            let pair = cite(DEV_LEAD, &ask);
            let before = e.snapshot();
            let (st, v) = e.call(Who::Worker(DEV_LEAD), "PATCH", &cfg(DEV_B), Some(cited(json!({"tags":""}),pair))).await;
            assert_code(st, &v, "cite_ask_missing_change", &ask);
            assert_eq!(e.snapshot(), before, "{ask}: refusal writes nothing");
            assert!(e.tags(DEV_B).contains("dev"));
        }
    }
    // The same actor/target/direction with a question mark is valid once the
    // negation is removed: question form alone does not refuse an ask.
    let pair = cite(DEV_LEAD, &format!("Max, remove dev from {DEV_B}?"));
    let (st, v) = e.call(Who::Worker(DEV_LEAD), "PATCH", &cfg(DEV_B), Some(cited(json!({"tags":""}),pair))).await;
    assert_allowed(st, &v, "unnegated tag question");
    assert!(!e.tags(DEV_B).contains("dev"));
    let pair = cite(MRPOPO, &format!("Max, may I enable yolo for {DEV_A}?"));
    let (st,v) = e.call(Who::Worker(MRPOPO), "PATCH", &cfg(DEV_A), Some(cited(json!({"toggle_yolo":true}),pair))).await;
    assert_allowed(st, &v, "explicit direction on the actual target");
}
