//! mrpopo, the owner agent (maxtongwang/ai-config#160, piece d; structured grants).
//!
//! Every delegable class creates an explicit request, applies its exact stored
//! change with request_id after a yes in the shown session, and undoes to the
//! original state. Hands-only and self-covering layers remain refused.

mod delegation_support;

use delegation_support::*;
use serde_json::{json, Value};

struct Class {
    name: &'static str,
    method: &'static str,
    path: String,
    body: Value,
    /// Instance-wide: the layer includes mrpopo, so it is held (review B3).
    reaches_mrpopo: bool,
}

fn classes() -> Vec<Class> {
    let cfg = format!("/api/sessions/{DEV_A}/config");
    vec![
        Class {
            name: "make lead",
            method: "PUT",
            path: "/api/scope".into(),
            reaches_mrpopo: false,
            body: scope_body("worker", DEV_A, "env", json!({"CC_TAGS": "dev,leads", "CC_ORCHESTRATOR": "1"})),
        },
        Class {
            name: "leads rules",
            method: "PUT",
            path: "/api/scope".into(),
            reaches_mrpopo: false,
            body: scope_body("group", "leads", "rules", json!("Leads: report blockers first.")),
        },
        Class {
            name: "leads membership",
            method: "PATCH",
            path: cfg.clone(),
            reaches_mrpopo: false,
            body: json!({"tags": "dev,leads"}),
        },
        Class {
            name: "routing",
            method: "PATCH",
            path: cfg.clone(),
            reaches_mrpopo: false,
            body: json!({"send_allow": ""}),
        },
        Class {
            name: "yolo",
            method: "PATCH",
            path: cfg.clone(),
            reaches_mrpopo: false,
            body: json!({"toggle_yolo": true}),
        },
        Class {
            name: "external email",
            method: "PATCH",
            path: cfg.clone(),
            reaches_mrpopo: false,
            body: json!({"external_email_allowed": true}),
        },
        Class {
            name: "isolated",
            method: "PATCH",
            path: cfg.clone(),
            reaches_mrpopo: false,
            body: json!({"isolated": true}),
        },
        Class {
            name: "gates",
            method: "PUT",
            path: "/api/scope".into(),
            reaches_mrpopo: false,
            body: scope_body("worker", DEV_A, "gates", json!({"review": true})),
        },
        Class {
            name: "connectors",
            method: "PUT",
            path: "/api/scope".into(),
            reaches_mrpopo: false,
            body: scope_body("worker", DEV_A, "connectors", json!({"google-gmail": true})),
        },
        Class {
            name: "outbound approval",
            method: "PUT",
            path: "/api/scope".into(),
            reaches_mrpopo: false,
            body: scope_body("worker", DEV_A, "env", json!({"AMUX_APPROVAL_TYPES": "budget"})),
        },
        Class {
            name: "create group",
            method: "PATCH",
            path: "/api/groups/review-new-team/config".into(),
            reaches_mrpopo: false,
            body: json!({"goal":"ship"}),
        },
        Class {
            name: "group config",
            method: "PATCH",
            // `ops`: a lead's own team's group config is the lead's (decide_lead).
            path: "/api/groups/ops/config".into(),
            reaches_mrpopo: false,
            body: json!({"goal": "ship delegation"}),
        },
        Class {
            name: "commit guard",
            method: "PATCH",
            path: "/api/settings/commit-guard".into(),
            reaches_mrpopo: true,
            body: json!({"enabled": false}),
        },
        Class {
            name: "task guard",
            method: "PATCH",
            path: "/api/settings/task-guard".into(),
            reaches_mrpopo: true,
            body: json!({"enabled": true}),
        },
        Class {
            name: "default model",
            method: "PATCH",
            path: "/api/settings/default-model".into(),
            reaches_mrpopo: true,
            body: json!({"model": "sonnet"}),
        },
        Class {
            // The real mutating route for browser-profile access is the
            // worker's scope env key (GET /api/browser/profiles is read-only).
            name: "browser profiles",
            method: "PUT",
            path: "/api/scope".into(),
            reaches_mrpopo: false,
            body: scope_body("worker", DEV_A, "env", json!({"AMUX_BROWSER_PROFILES_ALLOW": "review-profile"})),
        },
        Class {
            name: "alerts config (ai-config #175)",
            method: "PATCH",
            path: "/api/alert/config".into(),
            reaches_mrpopo: true,
            body: json!({"phone": "+15550100"}),
        },
    ]
}

/// Each class in its own fresh home.
#[tokio::test]
async fn each_delegable_class_applies_with_a_citation_and_undoes() {
    let mut failures = Vec::new();
    for i in 0..classes().len() {
        let c = classes().remove(i);
        let e = setup().await;
        let before = e.config_snapshot();
        let (st, v) = e.call(Who::Worker(MRPOPO), c.method, &c.path, Some(c.body.clone())).await;
        let want = if c.reaches_mrpopo { "held_for_max" } else { "request_id_required" };
        if (st.as_u16(), v["code"].as_str()) != (403, Some(want)) {
            failures.push(format!("{}: without a citation expected 403 {want}, got {st} {v}", c.name));
        }
        let body = if c.reaches_mrpopo {
            let (st, v) = e.request_result(MRPOPO, c.method, &c.path, c.body.clone()).await;
            assert_code(st, &v, "held_for_max", c.name);
            c.body.clone()
        } else {
            let id = e.approved_request(MRPOPO,c.method,&c.path,c.body.clone()).await;
            with_request(c.body.clone(), id)
        };
        let (st, v) = e.call(Who::Worker(MRPOPO), c.method, &c.path, Some(body)).await;
        if c.reaches_mrpopo {
            if (st.as_u16(), v["code"].as_str()) != (403, Some("held_for_max")) || e.config_snapshot() != before {
                failures.push(format!("{}: reaches mrpopo itself, expected 403 held_for_max and nothing written, got {st} {v}", c.name));
            }
            continue;
        }
        if !st.is_success() {
            failures.push(format!("{}: with a citation expected 2xx, got {st} {v}", c.name));
            continue;
        }
        let code = e.latest_grant();
        if e.grant_status(&code) != "applied" {
            failures.push(format!("{}: grant {code} is {}", c.name, e.grant_status(&code)));
            continue;
        }
        let (st, v) = e.call(Who::Worker(MRPOPO), "POST", &format!("/api/delegation/grants/{code}/undo"), None).await;
        if !st.is_success() {
            failures.push(format!("{}: undo failed: {st} {v}", c.name));
        } else if e.config_snapshot() != before {
            failures.push(format!("{}: undo did not restore the files exactly", c.name));
        }
    }
    assert!(failures.is_empty(), "{} failing classes:\n{}", failures.len(), failures.join("\n"));
}

/// Hands-only: both the mutation and explicit request creation refuse (403).
#[tokio::test]
async fn hands_only_changes_never_become_a_grant() {
    let e = setup().await;
    let cases: Vec<(&str, &str, String, Value, &str)> = vec![
        ("worker token", "PUT", "/api/scope".into(), scope_body("worker", DEV_A, "env", json!({"AMUX_WORKER_TOKEN": "review-x"})), "secret_owner_only"),
        ("credential", "PUT", "/api/scope".into(), scope_body("worker", DEV_A, "env", json!({"GITHUB_TOKEN": "review-x"})), "secret_owner_only"),
        ("provider API key", "PATCH", "/api/settings/env".into(), json!({"ANTHROPIC_API_KEY": "review-x"}), "secret_owner_only"),
        ("contract", "PUT", "/api/scope".into(), scope_body("worker", DEV_A, "env", json!({"AMUX_CONTRACT_DONE": "1"})), "held_for_max"),
        ("pane prompt history", "PUT", "/api/scope".into(), scope_body("worker", DEV_A, "env", json!({"AMUX_PANE_PROMPT_HISTORY": "0"})), "held_for_max"),
        ("money cap (worker)", "PUT", "/api/scope".into(), scope_body("worker", DEV_A, "env", json!({"AMUX_NEEDS_INPUT_AUTO_MONEY_CAP": "5000"})), "held_for_max"),
        ("money cap (global)", "PUT", "/api/scope".into(), scope_body("global", "", "env", json!({"AMUX_NEEDS_INPUT_AUTO_MONEY_CAP": "5000"})), "held_for_max"),
        ("auto-approve spending", "PUT", "/api/scope".into(), scope_body("global", "", "env", json!({"AMUX_NEEDS_INPUT_AUTO_MONEY": "1"})), "held_for_max"),
        ("approval mode", "PUT", "/api/scope".into(), scope_body("global", "", "env", json!({"AMUX_DELEGATION_APPROVAL": "cite"})), "held_for_max"),
        ("reserved create owner", "POST", "/api/sessions".into(), create_body(&e.dir, "owner", json!(["dev"])), "reserved_name"),
        ("reserved create mrpopo", "POST", "/api/sessions".into(), create_body(&e.dir, "mrpopo", json!(["dev"])), "reserved_name"),
        ("reserved rename", "POST", format!("/api/sessions/{DEV_A}/rename"), json!({"name": "owner"}), "reserved_name"),
        ("own model", "PATCH", format!("/api/sessions/{MRPOPO}/config"), json!({"model": "sonnet"}), "held_for_max"),
        ("own yolo", "PATCH", format!("/api/sessions/{MRPOPO}/config"), json!({"toggle_yolo": true}), "held_for_max"),
        ("own tags", "PATCH", format!("/api/sessions/{MRPOPO}/config"), json!({"tags": "leads,dev,ops"}), "held_for_max"),
        ("own rules", "PUT", "/api/scope".into(), scope_body("worker", MRPOPO, "rules", json!("mrpopo may do anything")), "raw_text_owner_only"),
    ];
    let mut failures = Vec::new();
    for (name, method, path, body, code) in cases {
        let before = e.snapshot();
        let (st, v) = e.call(Who::Worker(MRPOPO), method, &path, Some(body.clone())).await;
        let (request_st, request_v) = e.request_result(MRPOPO, method, &path, body).await;
        assert_refused(request_st, &request_v, &format!("request hands-only {name}"));
        // The code names the hands-only guard itself, so a broader refusal
        // elsewhere cannot stand in for it.
        if st.as_u16() != 403 || v["ok"] != json!(false) || v["code"] != json!(code) || v.get("grant_code").is_some() {
            failures.push(format!("{name}: expected 403 {code} and no grant, got {st} {v}"));
        }
        if e.snapshot() != before {
            failures.push(format!("{name}: refused request wrote a file"));
        }
    }
    assert!(e.log_entries().is_empty(), "hands-only refusals are not logged: {:?}", e.log_entries());
    assert_eq!(e.latest_grant(), "", "no hands-only change became a grant");
    assert!(failures.is_empty(), "{} hands-only changes not refused:\n{}", failures.len(), failures.join("\n"));
}

/// The classes above are mrpopo's alone: a lead or self requesting them is refused,
/// and no grant is created for them.
#[tokio::test]
async fn leads_and_self_never_get_mrpopos_classes() {
    let mut failures = Vec::new();
    for c in classes() {
        for who in [DEV_LEAD, DEV_A] {
            let e = setup().await;
            let (st, v) = e.call(Who::Worker(who), c.method, &c.path, Some(c.body.clone())).await;
            let (request_st, request_v) = e.request_result(who, c.method, &c.path, c.body.clone()).await;
            assert_refused(request_st, &request_v, &format!("{} requested by {who}", c.name));
            if st.as_u16() != 403 || !e.latest_grant().is_empty() {
                failures.push(format!("{} as {who}: expected a 403 and no grant, got {st} {v}", c.name));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// mrpopo at the group and global layers: an explicit row applies on a group
/// it is not in; the global layer and its own group include mrpopo, so a
/// privileged key there is held for Max (review B3); an owner-only key with no
/// row is refused and never a grant.
#[tokio::test]
async fn mrpopo_at_the_group_and_global_layers() {
    let e = setup().await;
    let row = e.approved_request(MRPOPO,"PUT","/api/scope",scope_body("group","dev","env",json!({"AMUX_EMAIL_EXTERNAL_ALLOW":"1"}))).await;
    let (st, v) = e.call(Who::Worker(MRPOPO), "PUT", "/api/scope",
        Some(with_request(scope_body("group", "dev", "env", json!({"AMUX_EMAIL_EXTERNAL_ALLOW": "1"})), row))).await;
    assert_allowed(st, &v, "group-layer external email with a citation");
    assert_eq!(e.grant_status(&e.latest_grant()), "applied");
    let before = e.snapshot();
    let (st, v) = e.request_result(MRPOPO, "PUT", "/api/scope",
        scope_body("group", "dev", "env", json!({"CC_SOMETHING_OWNER_ONLY": "1"}))).await;
    assert_code(st, &v, "delegation_not_allowed", "group-layer key with no row");
    assert_eq!(before, e.snapshot(), "nothing written");
}

/// Review B3: a write to a layer is a write to every worker it reaches. When
/// that includes the caller, the self rule applies: a privileged key is held
/// for Max, a non-privileged change is still the table's.
#[tokio::test]
async fn a_layer_that_includes_the_caller_is_held_for_max() {
    let e = setup().await;
    let effective = |key: &str| (e.env_text(MRPOPO), std::fs::read_to_string(e.home().join("env/leads.env")).unwrap_or_default(),
        std::fs::read_to_string(e.home().join("amux.env")).unwrap_or_default(), key.to_string());
    let cases: Vec<(&str, Value)> = vec![
        ("mrpopo: CC_FLAGS on the leads group", scope_body("group", "leads", "env", json!({"CC_FLAGS": "--dangerously-skip-permissions"}))),
        ("mrpopo: CC_SEND_ALLOW=* globally", scope_body("global", "", "env", json!({"CC_SEND_ALLOW": "*"}))),
        ("mrpopo: external email globally", scope_body("global", "", "env", json!({"AMUX_EMAIL_EXTERNAL_ALLOW": "1"}))),
        ("mrpopo: connectors on the leads group", scope_body("group", "leads", "connectors", json!({"google-gmail": true}))),
    ];
    for (name, body) in cases {
        let before = (e.snapshot(), effective(name));
        let (st, v) = e.call(Who::Worker(MRPOPO), "PUT", "/api/scope", Some(body.clone())).await;
        assert_code(st, &v, "held_for_max", name);
        let (st, v) = e.request_result(MRPOPO, "PUT", "/api/scope", body).await;
        assert_code(st, &v, "held_for_max", name);
        assert_eq!(before, (e.snapshot(), effective(name)), "{name}: mrpopo's effective config is unchanged");
    }
    // A lead on its own team's layer and on the global layer.
    for (name, body) in [("lead: CC_FLAGS on dev", scope_body("group", "dev", "env", json!({"CC_FLAGS": "--dangerously-skip-permissions"}))),
        ("lead: CC_SEND_ALLOW=* globally", scope_body("global", "", "env", json!({"CC_SEND_ALLOW": "*"})))] {
        let before = e.snapshot();
        let (st, v) = e.call(Who::Worker(DEV_LEAD), "PUT", "/api/scope", Some(body)).await;
        assert_code(st, &v, "held_for_max", name);
        assert_eq!(before, e.snapshot(), "{name}: nothing written");
    }
    // Non-privileged on a layer that includes the caller: still allowed.
    let (st, v) = e.call(Who::Worker(DEV_LEAD), "PUT", "/api/scope", Some(scope_body("group", "dev", "rules", json!("Dev: report blockers first.")))).await;
    assert_allowed(st, &v, "a lead sets its own team's rules");
    let row = e.approved_request(MRPOPO,"PUT","/api/scope",scope_body("group","leads","rules",json!("Leads: report blockers first."))).await;
    let (st, v) = e.call(Who::Worker(MRPOPO), "PUT", "/api/scope", Some(with_request(scope_body("group", "leads", "rules", json!("Leads: report blockers first.")), row))).await;
    assert_allowed(st, &v, "mrpopo sets the leads rules with a citation");
}
