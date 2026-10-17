//! Owner-delegated changes: identity class x action for the owner, leads,
//! self and peers (maxtongwang/ai-config#160, piece d). mrpopo's classes are
//! in delegation_mrpopo.rs. HTTP-level, against the real router in an
//! isolated temp home (see tests/delegation_support/mod.rs).
//!
//! The table under test (dev-lead design plus the settled decisions from the
//! #18 design review):
//! - owner: any action.
//! - lead of team T (CC_ORCHESTRATOR=1 AND `leads`): create in T (never
//!   untagged; never with yolo, flags, isolated or creator); rename, stop and
//!   tags within {T} on a T worker that is not a peer lead; team rules for T;
//!   model/effort on a T worker. Tag changes touching `leads`, another team,
//!   mrpopo or a peer lead are refused even with a max_ref. Any other config
//!   field, group config, settings and guards are refused.
//! - a `leads` member without CC_ORCHESTRATOR is not a lead, and
//!   CC_ORCHESTRATOR is never a lead's or a worker's to set.
//! - self: an explicit env allowlist (model/effort, ...); owner-only keys
//!   refused, with or without a max_ref.
//! - `X@chat` gets no delegated action without process attribution.
//! - a worker acting on a peer: refused (config, rename, stop, keys, and
//!   start/reset/clear/git/worktree/archive unless owner or lead of T).
//! - the names `mrpopo` and `owner` are reserved for non-owners.
//! - secrets: never delegable, max_ref or not.

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
        if st.is_success() {
            assert_eq!(e.tags(&name), e.set(&["dev"]), "tags={tags}: created without the team tag: {v}");
        } else {
            assert_refused(st, &v, &format!("tags={tags}"));
            assert!(!e.env_path(&name).exists());
        }
    }
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
    let t = now_s() - 60.0;
    let body = scope_body("worker", DEV_A, "env", json!({"CC_ORCHESTRATOR": "1"}));
    for who in [DEV_LEAD, DEV_A] {
        e.seed_pane_prompt(who, &format!("make {DEV_A} lead"), t);
        let (st, v) = e.call(Who::Worker(who), "PUT", "/api/scope", Some(body.clone())).await;
        assert_refused(st, &v, &format!("{who} sets CC_ORCHESTRATOR"));
        let (st, v) = e.call(Who::Worker(who), "PUT", "/api/scope", Some(with_ref(body.clone(), max_ref(who, t), "Max asked"))).await;
        assert_refused_regardless_of_max_ref(st, &v, &format!("{who} sets CC_ORCHESTRATOR with max_ref"));
    }
    let (st, v) = e.call(Who::Worker(MRPOPO), "PUT", "/api/scope", Some(body)).await;
    assert_code(st, &v, "max_ref_missing", "mrpopo sets CC_ORCHESTRATOR without max_ref");
    assert!(!e.env_text(DEV_A).contains("CC_ORCHESTRATOR"), "{}", e.env_text(DEV_A));
}

// ---- lead of team T: rename, stop, tags ----------------------------------------

#[tokio::test]
async fn lead_renames_and_stops_in_its_team_only() {
    let e = setup().await;
    let lead = Who::Worker(DEV_LEAD);
    let (st, v) = e.call(lead, "POST", &format!("/api/sessions/{DEV_A}/rename"), Some(json!({"name": "review-dev-a2"}))).await;
    assert_allowed(st, &v, "lead rename in T");
    assert!(e.env_path("review-dev-a2").exists() && !e.env_path(DEV_A).exists(), "rename moved the env file");

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
    let token = amux_server::api::worker_identity::mint(e.home(), DEV_A).expect("rule 10 mints for the contract lane");
    let (st, v) = e.call(Who::Worker(DEV_LEAD), "POST", &format!("/api/sessions/{DEV_A}/rename"), Some(json!({"name": "review-dev-a2"}))).await;
    assert_allowed(st, &v, "lead rename");
    let (st, v) = e.call_socket("review-dev-a2", Some(&token), "PATCH", &cfg("review-dev-a2"), json!({"model": "sonnet"})).await;
    assert_allowed(st, &v, "renamed lane with its token");
    let (st, v) = e.call_socket("review-dev-a2", None, "PATCH", &cfg("review-dev-a2"), json!({"model": "haiku"})).await;
    assert_eq!((st.as_u16(), v["code"].clone()), (403, json!("worker_identity_refused")), "renamed lane without its token: {v}");
}

#[tokio::test]
async fn lead_sets_tags_within_its_team_and_never_widens() {
    let e = setup().await;
    let lead = Who::Worker(DEV_LEAD);

    let (st, v) = e.call(lead, "PATCH", &cfg(DEV_A), Some(json!({"tags": "dev"}))).await;
    assert_allowed(st, &v, "lead keeps a T worker inside {T}");
    let (st, v) = e.call(lead, "PATCH", &cfg(DEV_B), Some(json!({"tags": ""}))).await;
    assert_allowed(st, &v, "lead removes T");
    assert!(e.tags(DEV_B).is_empty(), "{:?}", e.tags(DEV_B));

    // Refused without a max_ref, and still refused with Max's typed
    // instruction in the lead's own pane.
    let t = now_s() - 60.0;
    for (target, tags, ctx) in [
        (DEV_A, "dev,leads", "adds leads"),
        (DEV_A, "dev,ops", "adds another team's tag"),
        (OPS_A, "ops,dev", "pulls another team's worker into T"),
        (MRPOPO, "leads,dev", "touches mrpopo"),
        (DEV_LEAD2, "dev", "touches a peer lead"),
    ] {
        e.seed_pane_prompt(DEV_LEAD, &format!("set {target} tags to {tags}"), t);
        let before = e.tags(target);
        let (st, v) = e.call(lead, "PATCH", &cfg(target), Some(json!({"tags": tags}))).await;
        assert_refused(st, &v, &format!("lead {ctx}"));
        let (st, v) = e.call(lead, "PATCH", &cfg(target), Some(with_ref(json!({"tags": tags}), max_ref(DEV_LEAD, t), "Max asked"))).await;
        assert_refused_regardless_of_max_ref(st, &v, &format!("lead {ctx} with max_ref"));
        assert_eq!(e.tags(target), before, "lead {ctx}: refused change wrote tags");
    }
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
        assert_refused(st, &v, &format!("lead sets model on {target}"));
    }

    let t = now_s() - 60.0;
    e.seed_pane_prompt(DEV_LEAD, &format!("change {DEV_A} desc dir provider yolo external email isolated send allow"), t);
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
        assert_refused(st, &v, &format!("lead sets {body}"));
        let (st, v) = e.call(lead, "PATCH", &cfg(DEV_A), Some(with_ref(body.clone(), max_ref(DEV_LEAD, t), "Max asked"))).await;
        assert_refused_regardless_of_max_ref(st, &v, &format!("lead sets {body} with max_ref"));
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

/// Group config, provider env and the settings guards are owner-only for
/// leads and workers (mrpopo's max_ref path is in delegation_mrpopo.rs).
#[tokio::test]
async fn group_config_settings_and_guards_are_owner_only_for_leads_and_workers() {
    let e = setup().await;
    let routes = [
        ("PATCH", "/api/groups/dev/config", json!({"goal": "lead goal"})),
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
    let (st, v) = e.call(Who::Owner, "GET", "/api/groups/dev/config", None).await;
    assert_eq!(v["goal"], json!(""), "no refused write reached group config: {st} {v}");
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

    let t = now_s() - 60.0;
    e.seed_pane_prompt(DEV_A, &format!("{DEV_A}: you may turn on yolo, external email, isolated and leads"), t);
    for body in [json!({"external_email_allowed": true}), json!({"toggle_yolo": true}), json!({"isolated": true}), json!({"tags": "dev,leads"})] {
        let before = e.env_text(DEV_A);
        let (st, v) = e.call(me, "PATCH", &cfg(DEV_A), Some(body.clone())).await;
        assert_refused(st, &v, &format!("self {body}"));
        let (st, v) = e.call(me, "PATCH", &cfg(DEV_A), Some(with_ref(body.clone(), max_ref(DEV_A, t), "Max asked"))).await;
        assert_refused_regardless_of_max_ref(st, &v, &format!("self {body} with max_ref"));
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
    let t = now_s() - 60.0;
    for who in [DEV_LEAD, MRPOPO] {
        e.seed_pane_prompt(who, &format!("create owner and mrpopo in dev, rename {DEV_A} to owner"), t);
    }
    for reserved in ["owner", "mrpopo", "Owner"] {
        for who in [DEV_LEAD, MRPOPO] {
            let (st, v) = e.call(Who::Worker(who), "POST", "/api/sessions",
                Some(with_ref(create_body(&e.dir, reserved, json!(["dev"])), max_ref(who, t), "Max asked"))).await;
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
        // The lead of T and the owner are not refused by authorization (the
        // verb itself may still fail in a home with no tmux).
        for who in [Who::Worker(DEV_LEAD), Who::Owner] {
            let (st, v) = e.call(who, "POST", &path, Some(json!({}))).await;
            assert_ne!(st.as_u16(), 403, "{who:?} {verb} on {DEV_B} was refused: {v}");
        }
    }
}

// ---- secrets ------------------------------------------------------------------

/// Secret keys are the owner's whoever asks and whatever max_ref they cite.
#[tokio::test]
async fn secrets_are_never_delegable() {
    let e = setup().await;
    let t = now_s() - 60.0;
    for who in [DEV_LEAD, MRPOPO, DEV_A] {
        e.seed_pane_prompt(who, &format!("set the secret env token key on {DEV_A} and dev"), t);
    }
    let keys = ["AMUX_WORKER_TOKEN", "AMUX_CONTRACT_DONE", "AMUX_CONTRACT_RULES", "OPENAI_API_KEY", "GITHUB_TOKEN", "STRIPE_SECRET_KEY", "DB_PASSWORD"];
    for who in [DEV_LEAD, MRPOPO, DEV_A] {
        for key in keys {
            for (level, name) in [("worker", DEV_A), ("group", "dev")] {
                let body = scope_body(level, name, "env", json!({ key: "review-secret-value" }));
                for b in [body.clone(), with_ref(body.clone(), max_ref(who, t), "Max asked")] {
                    let (st, v) = e.call(Who::Worker(who), "PUT", "/api/scope", Some(b)).await;
                    assert_refused_regardless_of_max_ref(st, &v, &format!("{who} sets {level}:{name} {key}"));
                }
            }
            let (st, v) = e.call(Who::Worker(who), "PATCH", &cfg(DEV_A),
                Some(with_ref(json!({ key: "review-secret-value" }), max_ref(who, t), "Max asked"))).await;
            assert!(!st.is_success() || !e.env_text(DEV_A).contains("review-secret-value"),
                "{who} wrote {key} through config PATCH: {st} {v}");
        }
        let (st, v) = e.call(Who::Worker(who), "PATCH", "/api/settings/env",
            Some(with_ref(json!({"OPENAI_API_KEY": "review-secret-value"}), max_ref(who, t), "Max asked"))).await;
        assert_refused_regardless_of_max_ref(st, &v, &format!("{who} sets a provider API key"));
    }
    let snap = e.snapshot();
    assert!(snap.values().all(|text| !text.contains("review-secret-value")), "a secret value was written: {snap:?}");
}
