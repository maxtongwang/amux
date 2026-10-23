//! Owner-delegated changes: identity class x action for the owner, leads,
//! self and peers (maxtongwang/ai-config#160, piece d). mrpopo's classes are
//! in delegation_mrpopo.rs. HTTP-level, against the real router in an
//! isolated temp home (see tests/delegation_support/mod.rs).
//!
//! The table under test (dev-lead design plus the settled decisions from the
//! #18 design review):
//! - owner: any action.
//! - lead of team T (CC_ORCHESTRATOR=1 AND `leads`): create in T (never
//!   untagged; never with yolo, flags, isolated or creator); rename and stop
//!   a T worker that is not a peer lead; tag changes on non-lead T workers
//!   only through an approved request, adding or removing only the lead's own groups
//!   (no-ops refused; other targets or changed groups owner-only);
//!   team rules for T; model/effort on a T worker.
//!   Adding or removing `leads` is refused even through request creation. Any other
//!   config field, group config, settings and guards are refused.
//! - a `leads` member without CC_ORCHESTRATOR is not a lead, and
//!   CC_ORCHESTRATOR is never a lead's or a worker's to set.
//! - self: an explicit env allowlist (model/effort, ...); owner-only keys
//!   refused, directly or through request creation.
//! - `X@chat` gets no delegated action without process attribution.
//! - a worker acting on a peer: refused (config, rename, stop, keys, and
//!   start/reset/clear/git/worktree/archive unless owner or lead of T).
//! - the names `mrpopo` and `owner` are reserved for non-owners.
//! - secrets: never delegable, through request creation or not.

mod delegation_support;

use delegation_support::*;
use serde_json::json;

fn cfg(w: &str) -> String {
    format!("/api/sessions/{w}/config")
}

// ---- owner -----------------------------------------------------------------

#[tokio::test]
async fn owner_may_take_every_action() {
    let e = setup().await;
    let (st, v) = e.call(Who::Owner, "POST", "/api/sessions", Some(create_body(&e.dir, "review-owner-new", json!(["leads", "ops"])))).await;
    assert_allowed(st, &v, "owner create with leads+ops");
    assert_eq!(e.tags("review-owner-new"), e.set(&["leads", "ops"]));

    let (st, v) = e.call(Who::Owner, "PATCH", &cfg(DEV_A), Some(json!({"tags": "dev,leads"}))).await;
    assert_allowed(st, &v, "owner sets leads");
    assert_eq!(e.tags(DEV_A), e.set(&["dev", "leads"]));

    for body in [json!({"model": "sonnet"}), json!({"external_email_allowed": true}), json!({"toggle_yolo": true})] {
        let (st, v) = e.call(Who::Owner, "PATCH", &cfg(DEV_B), Some(body.clone())).await;
        assert_allowed(st, &v, &format!("owner {body}"));
    }
    let (st, v) = e.call(Who::Owner, "PUT", "/api/scope", Some(scope_body("group", "leads", "rules", json!("Owner rule.")))).await;
    assert_allowed(st, &v, "owner sets leads rules");
    let (st, v) = e.call(Who::Owner, "PUT", "/api/scope", Some(scope_body("worker", DEV_B, "env", json!({"OPENAI_API_KEY": "sk-review-owner"})))).await;
    assert_allowed(st, &v, "owner sets a secret");
    let (st, v) = e.call(Who::Owner, "PATCH", "/api/groups/dev/config", Some(json!({"goal": "owner goal"}))).await;
    assert_allowed(st, &v, "owner group config");
    let (st, v) = e.call(Who::Owner, "PATCH", "/api/settings/commit-guard", Some(json!({"enabled": true}))).await;
    assert_allowed(st, &v, "owner commit guard");
    let (st, v) = e.call(Who::Owner, "POST", &format!("/api/sessions/{OPS_A}/rename"), Some(json!({"name": "review-ops-renamed"}))).await;
    assert_allowed(st, &v, "owner renames");
    assert!(e.env_path("review-ops-renamed").exists(), "owner rename moved the env file");
}

// ---- lead of team T: create -------------------------------------------------

#[tokio::test]
async fn lead_creates_inside_its_team_only() {
    let e = setup().await;
    let lead = Who::Worker(DEV_LEAD);
    let (st, v) = e.call(lead, "POST", "/api/sessions", Some(create_body(&e.dir, "review-dev-new", json!(["dev"])))).await;
    assert_allowed(st, &v, "lead create in T");
    assert_eq!(e.tags("review-dev-new"), e.set(&["dev"]));

    for (name, tags) in [("review-x-leads", json!(["dev", "leads"])), ("review-x-ops", json!(["ops"])), ("review-x-mixed", json!("dev,ops"))] {
        let (st, v) = e.call(lead, "POST", "/api/sessions", Some(create_body(&e.dir, name, tags.clone()))).await;
        assert_refused(st, &v, &format!("lead create {tags}"));
        assert!(!e.env_path(name).exists(), "refused create wrote {name}");
    }
}

/// A lead cannot make a worker outside every team: the team tag is applied,
/// or the create is refused. Either way no untagged worker exists after.
#[tokio::test]
async fn a_lead_never_creates_an_untagged_worker() {
    let e = setup().await;
    for (i, tags) in [json!([]), serde_json::Value::Null, json!("")].into_iter().enumerate() {
        let name = format!("review-untagged-{i}");
        let (st, v) = e.call(Who::Worker(DEV_LEAD), "POST", "/api/sessions", Some(create_body(&e.dir, &name, tags.clone()))).await;
        // Settled: an untagged lead create gets the lead's team tag, stored.
        assert_allowed(st, &v, &format!("untagged lead create tags={tags}"));
        assert_eq!(e.tags(&name), e.set(&["dev"]), "tags={tags}: the team tag must be stored: {v}");
    }
    // The store create route stores it too.
    let (st, v) = e.call(Who::Worker(DEV_LEAD), "POST", "/api/workers", Some(json!({"name": "review-untagged-store", "cwd": "/tmp"}))).await;
    assert_allowed(st, &v, "untagged lead store create");
    assert_eq!(e.tags("review-untagged-store"), e.set(&["dev"]), "store create must store the team tag: {v}");
}

#[tokio::test]
async fn a_lead_create_with_permission_flags_is_refused() {
    let e = setup().await;
    for (i, (k, val)) in [
        ("yolo", json!(true)),
        ("flags", json!("--dangerously-skip-permissions")),
        ("isolated", json!(true)),
        ("creator", json!("owner")),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("review-flagged-{i}");
        let mut body = create_body(&e.dir, &name, json!(["dev"]));
        body[k] = val.clone();
        let (st, v) = e.call(Who::Worker(DEV_LEAD), "POST", "/api/sessions", Some(body)).await;
        assert_refused(st, &v, &format!("lead create with {k}={val}"));
        assert!(!e.env_path(&name).exists(), "refused create wrote {name}");
    }
}

#[tokio::test]
async fn a_leads_member_without_orchestrator_is_not_a_lead() {
    let e = setup().await;
    let (st, v) = e.call(Who::Worker(LEADS_NO_ORCH), "POST", "/api/sessions", Some(create_body(&e.dir, "review-noorch-new", json!(["dev"])))).await;
    assert_refused(st, &v, "leads member without CC_ORCHESTRATOR creates");
    assert!(!e.env_path("review-noorch-new").exists());
    let (st, v) = e.call(Who::Worker(LEADS_NO_ORCH), "PATCH", &cfg(DEV_A), Some(json!({"model": "sonnet"}))).await;
    assert_refused(st, &v, "leads member without CC_ORCHESTRATOR sets a T worker's model");
}

#[tokio::test]
async fn cc_orchestrator_is_never_a_leads_or_a_workers_to_set() {
    let e = setup().await;
    let body = scope_body("worker", DEV_A, "env", json!({"CC_ORCHESTRATOR": "1"}));
    for who in [DEV_LEAD, DEV_A] {
        let (st, v) = e.call(Who::Worker(who), "PUT", "/api/scope", Some(body.clone())).await;
        assert_permission_refused(st, &v, &format!("{who} sets CC_ORCHESTRATOR"));
        let (st, v) = e.request_result(who, "PUT", "/api/scope", body.clone(), MAX_PANE).await;
        assert_permission_refused(st, &v, &format!("{who} requests CC_ORCHESTRATOR"));
    }
    let (st, v) = e.call(Who::Worker(MRPOPO), "PUT", "/api/scope", Some(body)).await;
    assert_code(st, &v, "request_id_required", "mrpopo sets CC_ORCHESTRATOR with no citation of Max");
    assert!(!e.env_text(DEV_A).contains("CC_ORCHESTRATOR"), "{}", e.env_text(DEV_A));
}

// ---- lead of team T: rename, stop, tags ----------------------------------------

#[tokio::test]
async fn lead_renames_and_stops_in_its_team_only() {
    let e = setup().await;
    let lead = Who::Worker(DEV_LEAD);
    // amux #3 moves the worker's token and alias with a rename, so a lead
    // renames a non-lead worker in its team (the amux #40 rule).
    let (st, v) = e.call(lead, "POST", &format!("/api/sessions/{DEV_A}/rename"), Some(json!({"name": "review-dev-a2"}))).await;
    assert_allowed(st, &v, "lead rename in T");
    assert!(!e.env_path(DEV_A).exists() && e.env_path("review-dev-a2").exists(), "the rename moved the env file");
    let (st, v) = e.call(lead, "POST", "/api/sessions/review-dev-a2/rename", Some(json!({"name": "mrpopo"}))).await;
    assert_code(st, &v, "reserved_name", "lead renames a T worker to mrpopo");
    assert!(e.env_path("review-dev-a2").exists(), "a refused rename moved nothing");

    let (st, v) = e.call(lead, "POST", &format!("/api/sessions/{DEV_B}/stop"), Some(json!({}))).await;
    assert_allowed(st, &v, "lead stop in T");

    for target in [OPS_A, DEV_LEAD2] {
        let (st, v) = e.call(lead, "POST", &format!("/api/sessions/{target}/rename"), Some(json!({"name": "review-renamed-x"}))).await;
        assert_refused(st, &v, &format!("lead renames {target}"));
        assert!(e.env_path(target).exists() && !e.env_path("review-renamed-x").exists());
        let (st, v) = e.call(lead, "POST", &format!("/api/sessions/{target}/stop"), Some(json!({}))).await;
        assert_refused(st, &v, &format!("lead stops {target}"));
    }
}

#[tokio::test]
async fn a_lead_renamed_workers_token_still_validates() {
    let e = setup().await;
    // DEV_A is a rule-10 contract lane with a minted token.
    std::fs::write(e.env_path(DEV_A), "CC_DIR=\"/tmp\"\nCC_TAGS=\"dev\"\nAMUX_CONTRACT_DONE=1\n").unwrap();
    let token = amux_server::api::worker_identity::mint(e.home(), DEV_A).expect("mint").expect("rule 10 mints for the contract lane");
    // amux #3 carries the token across a rename, so the token-after-rename
    // hole (#8 review N2) stays shut: the renamed lane validates with its
    // token and fails without.
    let (st, v) = e.call(Who::Worker(DEV_LEAD), "POST", &format!("/api/sessions/{DEV_A}/rename"), Some(json!({"name": "review-dev-a2"}))).await;
    assert_allowed(st, &v, "lead renames a T contract lane");
    let renamed = "review-dev-a2";
    let (st, v) = e.call_socket(renamed, Some(&token), "PATCH", &cfg(renamed), json!({"model": "sonnet"})).await;
    assert_allowed(st, &v, "the renamed lane validates with its token");
    let (st, v) = e.call_socket(renamed, None, "PATCH", &cfg(renamed), json!({"model": "haiku"})).await;
    assert_eq!((st.as_u16(), v["code"].clone()), (403, json!("worker_identity_refused")), "renamed lane without its token: {v}");
}

#[tokio::test]
async fn lead_changes_tags_only_through_a_cited_grant_and_never_makes_a_lead() {
    let e = setup().await;
    let lead = Who::Worker(DEV_LEAD);

    let before = e.tags(DEV_A);
    let (st, v) = e.call(lead, "PATCH", &cfg(DEV_A), Some(json!({"tags": "dev"}))).await;
    assert_code(st, &v, "delegation_not_allowed", "lead no-op tag change");
    assert_eq!(e.tags(DEV_A), before, "refused no-op changed tags");
    let before = e.tags(DEV_B);
    let (st, v) = e.call(lead, "PATCH", &cfg(DEV_B), Some(json!({"tags": ""}))).await;
    assert_code(st, &v, "request_id_required", "lead removes T without a citation");
    assert_eq!(e.tags(DEV_B), before, "uncited removal changed tags");
    assert_eq!(e.latest_grant(), "", "refused tag changes record no grant");

    let id = e.approved_request(DEV_LEAD,"PATCH",&cfg(DEV_B),json!({"tags":""})).await;
    let (st, v) = e.call(lead, "PATCH", &cfg(DEV_B), Some(with_request(json!({"tags":""}),id))).await;
    assert_allowed(st, &v, "lead removes T through a cited grant");
    let grant = e.latest_grant();
    assert_eq!(e.grant_status(&grant), "applied", "cited tag removal is an applied grant");
    assert!(e.tags(DEV_B).is_empty(), "{:?}", e.tags(DEV_B));

    // Both the target and changed tags must belong to the lead's teams.
    // Other-team tags and other-team/privileged targets stay owner-only.
    // Changing `leads` wins over these boundaries; no-op wins over them too.
    for (target, tags, ctx, code) in [
        (DEV_A, "dev,leads", "adds leads", "worker_group_membership_refused"),
        (DEV_A, "dev,ops", "adds another team's tag", "peer_config_owner_only"),
        (OPS_A, "ops,dev", "pulls another team's worker into T", "peer_config_owner_only"),
        (MRPOPO, "leads,dev", "touches mrpopo", "peer_config_owner_only"),
        (DEV_LEAD2, "dev,leads,ops", "touches a peer lead", "peer_config_owner_only"),
        (DEV_LEAD2, "dev", "removes leads from a peer lead", "worker_group_membership_refused"),
        (OPS_A, "ops,leads", "adds leads to another team's worker", "worker_group_membership_refused"),
        (MRPOPO, "dev", "removes leads from mrpopo", "worker_group_membership_refused"),
        (OPS_A, "ops", "leaves another team's worker unchanged", "delegation_not_allowed"),
        (MRPOPO, "leads", "leaves mrpopo unchanged", "delegation_not_allowed"),
        (DEV_LEAD2, "dev,leads", "leaves a peer lead unchanged", "delegation_not_allowed"),
    ] {
        let before = e.tags(target);
        let (st, v) = e.call(lead, "PATCH", &cfg(target), Some(json!({"tags": tags}))).await;
        assert_code(st, &v, code, &format!("lead {ctx}"));
        let (st, v) = e.request_result(DEV_LEAD, "PATCH", &cfg(target), json!({"tags": tags}), MAX_PANE).await;
        assert_code(st, &v, code, &format!("lead requests to {ctx}"));
        assert_eq!(e.tags(target), before, "lead {ctx}: refused change wrote tags");
        assert_eq!(e.latest_grant(), grant, "lead {ctx}: refused change recorded a grant");
    }

    // An approved request cannot be repurposed across target/group boundaries.
    // The exact original request remains applicable; rejected drafts create none.
    let id = e.approved_request(DEV_LEAD, "PATCH", &cfg(DEV_A), json!({"tags":""})).await;
    let pending = id.as_str().unwrap();
    for (target, tags, code) in [
        (OPS_A, "ops,dev", "peer_config_owner_only"),
        (MRPOPO, "leads,dev", "peer_config_owner_only"),
        (DEV_LEAD2, "dev,leads,ops", "peer_config_owner_only"),
        (DEV_A, "dev,leads", "worker_group_membership_refused"),
        (DEV_A, "dev,ops", "peer_config_owner_only"),
    ] {
        let before = e.tags(target);
        let (st, v) = e.request_result(DEV_LEAD, "PATCH", &cfg(target), json!({"tags": tags}), MAX_PANE).await;
        assert_code(st, &v, code, &format!("lead requests forbidden tags on {target}"));
        let (st, v) = e.call(lead, "PATCH", &cfg(target), Some(with_request(json!({"tags": tags}), id.clone()))).await;
        assert_code(st, &v, "request_change_mismatch", &format!("lead repurposes approval for {target}"));
        assert_eq!(e.tags(target), before, "refused request changed {target}'s tags");
        assert_eq!(e.latest_grant(), pending, "refused change created a new request");
        assert_eq!(e.grant_status(pending), "pending", "refused change consumed the approval");
    }
    let (st, v) = e.call(lead, "PATCH", &cfg(DEV_A), Some(with_request(json!({"tags":""}), id.clone()))).await;
    assert_allowed(st, &v, "original approval still applies to exactly its own-team change");
    assert!(e.tags(DEV_A).is_empty());
    assert_eq!(e.grant_status(pending), "applied");
}

// ---- lead of team T: rules, config ------------------------------------------

#[tokio::test]
async fn lead_sets_its_team_rules_only() {
    let e = setup().await;
    let lead = Who::Worker(DEV_LEAD);
    let (st, v) = e.call(lead, "PUT", "/api/scope", Some(scope_body("group", "dev", "rules", json!("Dev team: run the suite.")))).await;
    assert_allowed(st, &v, "lead sets tag:dev rules");
    let stored = std::fs::read_to_string(e.home().join("memory/tags/dev.rules.md")).unwrap_or_default();
    assert!(stored.contains("Dev team: run the suite."), "rules stored: {stored:?}");

    let (st, v) = e.call(lead, "PUT", "/api/scope", Some(scope_body("group", "ops", "rules", json!("Ops rule from dev.")))).await;
    assert_refused(st, &v, "lead sets another team's rules");
    let (st, v) = e.call(lead, "PUT", "/api/scope", Some(scope_body("group", "leads", "rules", json!("Leads rule from a lead.")))).await;
    assert_refused(st, &v, "lead sets leads rules");
    assert!(!e.home().join("memory/tags/ops.rules.md").exists());
    assert!(!e.home().join("memory/tags/leads.rules.md").exists());
}

#[tokio::test]
async fn lead_sets_model_and_effort_on_a_team_worker_and_nothing_else() {
    let e = setup().await;
    let lead = Who::Worker(DEV_LEAD);
    let (st, v) = e.call(lead, "PATCH", &cfg(DEV_A), Some(json!({"model": "sonnet"}))).await;
    assert_allowed(st, &v, "lead sets model on T worker");
    let (st, v) = e.call(lead, "PATCH", &cfg(DEV_A), Some(json!({"model": "sonnet", "effort": "high"}))).await;
    assert_allowed(st, &v, "lead sets model+effort on T worker");

    for target in [OPS_A, DEV_LEAD2, MRPOPO] {
        let (st, v) = e.call(lead, "PATCH", &cfg(target), Some(json!({"model": "sonnet"}))).await;
        assert_permission_refused(st, &v, &format!("lead sets model on {target}"));
    }

    for body in [
        json!({"desc": "rewritten by the lead"}),
        json!({"dir": "/tmp"}),
        json!({"provider": "codex"}),
        json!({"task_summary": "x"}),
        json!({"toggle_yolo": true}),
        json!({"external_email_allowed": true}),
        json!({"isolated": true}),
        json!({"send_allow": OPS_A}),
    ] {
        let before = e.env_text(DEV_A);
        let (st, v) = e.call(lead, "PATCH", &cfg(DEV_A), Some(body.clone())).await;
        assert_permission_refused(st, &v, &format!("lead sets {body}"));
        let (st, v) = e.request_result(DEV_LEAD, "PATCH", &cfg(DEV_A), body.clone(), MAX_PANE).await;
        assert_permission_refused(st, &v, &format!("lead requests {body}"));
        assert_eq!(e.env_text(DEV_A), before, "refused {body} changed the env file");
    }
}

/// PATCH /api/workers/{id} judges every field, not only the row's group.
#[tokio::test]
async fn patch_worker_row_judges_every_field() {
    let e = setup().await;
    let (st, v) = e.call(Who::Owner, "POST", "/api/workers", Some(json!({"name": DEV_A, "cwd": "/tmp"}))).await;
    assert!(st.is_success(), "fixture: owner registers {DEV_A}'s store row: {st} {v}");
    let path = format!("/api/workers/{DEV_A}");
    let (st, v) = e.call(Who::Worker(DEV_LEAD), "PATCH", &path, Some(json!({"model": "sonnet"}))).await;
    assert_allowed(st, &v, "lead sets a T worker row's model");
    for body in [json!({"cwd": "/elsewhere"}), json!({"provider": "codex"}), json!({"backend": "herdr"}), json!({"display_name": "review-dev-a-row"})] {
        let (st, v) = e.call(Who::Worker(DEV_LEAD), "PATCH", &path, Some(body.clone())).await;
        assert_refused(st, &v, &format!("lead row {body}"));
    }
    for body in [json!({"display_name": "owner"}), json!({"display_name": "mrpopo"})] {
        let (st, v) = e.call(Who::Worker(DEV_A), "PATCH", &path, Some(body.clone())).await;
        assert_refused(st, &v, &format!("self row rename to a reserved name {body}"));
    }
}

/// Another team's group config, provider env and the settings guards are
/// owner-only for leads and workers (mrpopo's path is in
/// delegation_mrpopo.rs); a lead sets its own team's group config.
#[tokio::test]
async fn group_config_settings_and_guards_are_owner_only_for_leads_and_workers() {
    let e = setup().await;
    let routes = [
        ("PATCH", "/api/groups/ops/config", json!({"goal": "lead goal"})),
        ("PATCH", "/api/settings/env", json!({"OPENAI_API_KEY": "sk-review-lead"})),
        ("PATCH", "/api/settings/commit-guard", json!({"enabled": false})),
        ("PATCH", "/api/settings/task-guard", json!({"enabled": true})),
        ("PATCH", "/api/settings/default-model", json!({"model": "sonnet"})),
    ];
    for who in [DEV_LEAD, DEV_A] {
        for (method, path, body) in &routes {
            let before = e.snapshot();
            let (st, v) = e.call(Who::Worker(who), method, path, Some(body.clone())).await;
            assert_refused(st, &v, &format!("{who} {method} {path}"));
            assert_eq!(e.snapshot(), before, "{who} {method} {path}: refused request wrote a file");
        }
    }
    let (st, v) = e.call(Who::Owner, "GET", "/api/groups/ops/config", None).await;
    assert_eq!(v["goal"], json!(""), "no refused write reached group config: {st} {v}");
    let (st, v) = e.call(Who::Worker(DEV_A), "PATCH", "/api/groups/dev/config", Some(json!({"goal": "worker goal"}))).await;
    assert_refused(st, &v, "a worker sets its team's group config");
    let (st, v) = e.call(Who::Worker(DEV_LEAD), "PATCH", "/api/groups/dev/config", Some(json!({"goal": "lead goal"}))).await;
    assert_allowed(st, &v, "a lead sets its own team's group config");
    let (_, v) = e.call(Who::Owner, "GET", "/api/groups/dev/config", None).await;
    assert_eq!(v["goal"], json!("lead goal"), "{v}");
}

// ---- self ---------------------------------------------------------------------

#[tokio::test]
async fn self_sets_model_and_effort_but_not_owner_only_keys() {
    let e = setup().await;
    let me = Who::Worker(DEV_A);
    let (st, v) = e.call(me, "PATCH", &cfg(DEV_A), Some(json!({"model": "sonnet"}))).await;
    assert_allowed(st, &v, "self model");
    let (st, v) = e.call(me, "PATCH", &cfg(DEV_A), Some(json!({"model": "sonnet", "effort": "low"}))).await;
    assert_allowed(st, &v, "self model+effort");

    for body in [json!({"external_email_allowed": true}), json!({"toggle_yolo": true}), json!({"isolated": true}), json!({"tags": "dev,leads"})] {
        let before = e.env_text(DEV_A);
        let (st, v) = e.call(me, "PATCH", &cfg(DEV_A), Some(body.clone())).await;
        assert_permission_refused(st, &v, &format!("self {body}"));
        let (st, v) = e.request_result(DEV_A, "PATCH", &cfg(DEV_A), body.clone(), MAX_PANE).await;
        assert_permission_refused(st, &v, &format!("self requests {body}"));
        assert_eq!(e.env_text(DEV_A), before, "refused self {body} changed the env file");
    }
}

/// Self env keys follow an explicit allowlist; these are outside it.
#[tokio::test]
async fn self_env_keys_outside_the_allowlist_are_refused() {
    let e = setup().await;
    for key in ["CC_VERIFY", "CC_DEPLOY_CHECK", "CC_WORKTREE_AUTO_MERGE", "AMUX_YOLO_AUTO_YES", "CC_PROJECT_LEAD", "AMUX_PANE_PROMPT_HISTORY"] {
        let before = e.env_text(DEV_A);
        let (st, v) = e.call(Who::Worker(DEV_A), "PUT", "/api/scope", Some(scope_body("worker", DEV_A, "env", json!({ key: "1" })))).await;
        assert_refused(st, &v, &format!("self sets {key}"));
        assert_eq!(e.env_text(DEV_A), before, "self {key} changed the env file");
    }
}

// ---- reserved names, chat identities ------------------------------------------

#[tokio::test]
async fn reserved_names_are_refused_for_non_owners() {
    let e = setup().await;
    for reserved in ["owner", "mrpopo", "Owner"] {
        for who in [DEV_LEAD, MRPOPO] {
            let (st, v) = e.call(Who::Worker(who), "POST", "/api/sessions",
                Some(create_body(&e.dir, reserved, json!(["dev"])))).await;
            assert_refused(st, &v, &format!("{who} creates {reserved}"));
            let (st, v) = e.call(Who::Worker(who), "POST", "/api/workers", Some(json!({"name": reserved, "cwd": "/tmp"}))).await;
            assert_refused(st, &v, &format!("{who} creates row {reserved}"));
        }
        let (st, v) = e.call(Who::Worker(DEV_LEAD), "POST", &format!("/api/sessions/{DEV_A}/rename"), Some(json!({"name": reserved}))).await;
        assert_refused(st, &v, &format!("lead renames {DEV_A} to {reserved}"));
        let (st, v) = e.call(Who::Worker(DEV_A), "PATCH", &cfg(DEV_A), Some(json!({"rename": reserved}))).await;
        assert_refused(st, &v, &format!("self renames to {reserved}"));
    }
    assert!(e.env_path(DEV_A).exists() && !e.env_path("owner").exists());
}

#[tokio::test]
async fn a_chat_identity_gets_no_delegated_action_without_attribution() {
    let e = setup().await;
    let chat = Who::Worker("review-dev-lead@chat");
    let (st, v) = e.call(chat, "PATCH", &cfg(DEV_A), Some(json!({"model": "sonnet"}))).await;
    assert_refused(st, &v, "lead@chat model on T worker");
    let (st, v) = e.call(chat, "POST", "/api/sessions", Some(create_body(&e.dir, "review-chat-new", json!(["dev"])))).await;
    assert_refused(st, &v, "lead@chat create");
    let (st, v) = e.call(chat, "PUT", "/api/scope", Some(scope_body("group", "dev", "rules", json!("chat rule")))).await;
    assert_refused(st, &v, "lead@chat team rules");
    assert!(!e.env_path("review-chat-new").exists());
}

// ---- a worker on a peer --------------------------------------------------------

#[tokio::test]
async fn a_worker_acting_on_a_peer_is_refused() {
    let e = setup().await;
    let me = Who::Worker(DEV_A);
    let before = e.env_text(DEV_B);
    let (st, v) = e.call(me, "PATCH", &cfg(DEV_B), Some(json!({"model": "sonnet"}))).await;
    assert_refused(st, &v, "peer model");
    let (st, v) = e.call(me, "POST", &format!("/api/sessions/{DEV_B}/rename"), Some(json!({"name": "review-dev-b2"}))).await;
    assert_refused(st, &v, "peer rename");
    let (st, v) = e.call(me, "POST", &format!("/api/sessions/{DEV_B}/stop"), Some(json!({}))).await;
    assert_refused(st, &v, "peer stop");
    let (st, v) = e.call(me, "POST", &format!("/api/sessions/{DEV_B}/keys"), Some(json!({"keys": "Enter"}))).await;
    assert_refused(st, &v, "peer keys");
    let (st, v) = e.call(Who::Worker(DEV_LEAD), "POST", &format!("/api/sessions/{DEV_B}/keys"), Some(json!({"keys": "Enter"}))).await;
    assert_refused(st, &v, "lead keys into a T worker");
    let (st, v) = e.call(me, "PUT", "/api/scope", Some(scope_body("group", "dev", "rules", json!("peer rule")))).await;
    assert_refused(st, &v, "plain worker team rules");
    assert_eq!(e.env_text(DEV_B), before);
    assert!(e.env_path(DEV_B).exists());
}

/// start/reset/clear/git/worktree/archive on a peer: owner or lead of T only.
#[tokio::test]
async fn lifecycle_verbs_on_a_peer_are_the_owners_or_its_leads() {
    let e = setup().await;
    for verb in ["start", "reset", "clear", "git", "worktree", "archive"] {
        let path = format!("/api/sessions/{DEV_B}/{verb}");
        for who in [DEV_A, OPS_LEAD, LEADS_NO_ORCH] {
            let (st, v) = e.call(Who::Worker(who), "POST", &path, Some(json!({}))).await;
            assert_refused(st, &v, &format!("{who} {verb} on {DEV_B}"));
        }
        // The lead of T and the owner are not refused by the delegation table
        // (the verb itself may still fail in a home with no tmux). Archive by
        // any agent stays behind the separate human-only archive rule
        // (AMUX_ALLOW_AGENT_SESSION_DELETE), which the table does not lift;
        // this harness's owner carries no dashboard UI token, so it too.
        for who in [Who::Worker(DEV_LEAD), Who::Owner] {
            let (st, v) = e.call(who, "POST", &path, Some(json!({}))).await;
            if verb == "archive" {
                assert_eq!(st.as_u16(), 403, "{v}");
                assert!(v.get("code").is_none() && v["error"].as_str().unwrap_or("").contains("AMUX_ALLOW_AGENT_SESSION_DELETE"),
                    "{who:?} archive is refused only by the human-only archive rule: {v}");
                continue;
            }
            assert_ne!(st.as_u16(), 403, "{who:?} {verb} on {DEV_B} was refused: {v}");
        }
    }
}

// ---- secrets ------------------------------------------------------------------

/// Secret keys remain owner-only for both direct mutations and approval requests.
#[tokio::test]
async fn secrets_are_never_delegable() {
    let e = setup().await;
    let keys = ["AMUX_WORKER_TOKEN", "AMUX_CONTRACT_DONE", "AMUX_CONTRACT_RULES", "OPENAI_API_KEY", "GITHUB_TOKEN", "STRIPE_SECRET_KEY", "DB_PASSWORD"];
    for who in [DEV_LEAD, MRPOPO, DEV_A] {
        for key in keys {
            for (level, name) in [("worker", DEV_A), ("group", "dev")] {
                let body = scope_body(level, name, "env", json!({ key: "review-secret-value" }));
                let (st, v) = e.call(Who::Worker(who), "PUT", "/api/scope", Some(body.clone())).await;
                assert_permission_refused(st, &v, &format!("{who} sets {level}:{name} {key}"));
                let (st, v) = e.request_result(who, "PUT", "/api/scope", body, MAX_PANE).await;
                assert_permission_refused(st, &v, &format!("{who} requests {level}:{name} {key}"));
            }
            let (st, v) = e.call(Who::Worker(who), "PATCH", &cfg(DEV_A),
                Some(json!({ key: "review-secret-value" }))).await;
            assert!(!st.is_success() || !e.env_text(DEV_A).contains("review-secret-value"),
                "{who} wrote {key} through config PATCH: {st} {v}");
        }
        let (st, v) = e.call(Who::Worker(who), "PATCH", "/api/settings/env",
            Some(json!({"OPENAI_API_KEY": "review-secret-value"}))).await;
        assert_permission_refused(st, &v, &format!("{who} sets a provider API key"));
    }
    let snap = e.snapshot();
    assert!(snap.values().all(|text| !text.contains("review-secret-value")), "a secret value was written: {snap:?}");
}

/// Review B1: `tags` beside another key is no longer a tag change that lets
/// the other key through. On BOTH config routes a lead's mixed body is
/// refused with nothing written, each board-automation field alone is
/// refused, mrpopo's cited mixed body is judged key by key, and the owner's
/// mixed body is two settings.
#[tokio::test]
async fn a_mixed_config_body_is_judged_key_by_key_on_both_routes() {
    let e = setup().await;
    let board = ["auto_drain_backlog", "board_auto_pickup", "board_auto_continue", "board_standing_orders", "board_decompose", "board_force_adherence"];
    let mut extras: Vec<&str> = vec!["toggle_auto_continue", "toggle_yolo"];
    extras.extend(board);
    let mut tried = 0;
    for route in [format!("/api/sessions/{DEV_A}/config"), format!("/api/workers/{DEV_A}/config")] {
        for k in &extras {
            for body in [json!({"tags": "dev", *k: true}), json!({"rename": "review-dev-a2", *k: true})] {
                let before = e.snapshot();
                let (st, v) = e.call(Who::Worker(DEV_LEAD), "PATCH", &route, Some(body.clone())).await;
                assert_code(st, &v, "peer_config_owner_only", &format!("lead {route} {body}"));
                assert_eq!(before, e.snapshot(), "{route} {body}: nothing written");
                tried += 1;
            }
        }
        for k in board {
            let before = e.snapshot();
            let (st, v) = e.call(Who::Worker(DEV_LEAD), "PATCH", &route, Some(json!({k: true}))).await;
            assert_code(st, &v, "peer_config_owner_only", &format!("lead {route} {k} alone"));
            assert_eq!(before, e.snapshot(), "{route} {k}: nothing written");
        }
    }
    assert_eq!(tried, 32, "every mixed body was sent on both routes");
    assert!(!e.env_text(DEV_A).contains("dangerously"), "no YOLO: {}", e.env_text(DEV_A));
    // An approved membership request cannot carry an extra auto-continue setting.
    let id = e.approved_request(MRPOPO, "PATCH", &cfg(DEV_A), json!({"tags":"dev,ops"})).await;
    let before = e.snapshot();
    let (st, v) = e.call(Who::Worker(MRPOPO), "PATCH", &format!("/api/sessions/{DEV_A}/config"),
        Some(with_request(json!({"tags": "dev,ops", "toggle_auto_continue": true}), id.clone()))).await;
    assert_code(st, &v, "request_change_mismatch", "mrpopo's approval cannot carry a bundled setting");
    assert_eq!(before, e.snapshot(), "nothing written");
    assert_eq!(e.latest_grant(), id.as_str().unwrap(), "no new grant");
    assert_eq!(e.grant_status(id.as_str().unwrap()), "pending", "mixed body did not consume the approval");
    // The owner: a mixed body is two settings, refused as malformed.
    let (st, v) = e.call(Who::Owner, "PATCH", &format!("/api/sessions/{DEV_A}/config"), Some(json!({"tags": "dev", "toggle_auto_continue": true}))).await;
    assert_eq!((st.as_u16(), v["code"].clone()), (400, json!("one_setting_per_patch")), "{v}");
    assert_eq!(before, e.snapshot(), "nothing written");
}
