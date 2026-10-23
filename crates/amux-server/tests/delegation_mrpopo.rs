//! mrpopo, the owner agent (maxtongwang/ai-config#160, piece d; structured grants).
//!
//! Every non-secret change that was owner-only is mrpopo's to make by CITING
//! a pair (cite mode, the default; review B4): its ask naming the target and
//! the setting, and Max's first typed reply. It applies at once as a grant,
//! logged and undoable. Each delegable class is driven: no citation ->
//! refused; a citation pair -> applied; undo -> the files read as before.
//! A class that reaches mrpopo itself (an instance-wide setting, the global
//! layer, a group it is in) is held for Max (review B3).
//!
//! Hands-only changes are refused even with a citation and never become a
//! grant: secret values, AMUX_CONTRACT_*, AMUX_PANE_PROMPT_HISTORY, the money
//! cap / auto-approve spending limit, the approval mode, the reserved names
//! and mrpopo's own config. Leads and self never get these classes.

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

/// An ask that names the class, its route and body (so its setting words),
/// and every target spelling the classes use.
fn ask_for(c: &Class) -> String {
    if c.name == "leads membership" {
        return format!("Max, may I add the leads tag to {DEV_A}?");
    }
    format!("Max, may I {} ({} {}) for {DEV_A}, ops, leads and the fleet?", c.name, c.path, c.body)
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
        let want = if c.reaches_mrpopo { "held_for_max" } else { "cite_missing" };
        if (st.as_u16(), v["code"].as_str()) != (403, Some(want)) {
            failures.push(format!("{}: without a citation expected 403 {want}, got {st} {v}", c.name));
        }
        let row = e.cite(&ask_for(&c));
        let (st, v) = e.call(Who::Worker(MRPOPO), c.method, &c.path, Some(cited(c.body.clone(), row))).await;
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

/// Hands-only: refused outright (403) even with a citation, never a grant.
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
    let row = e.cite(&format!("Max, may I change these for {DEV_A}, {MRPOPO} and the fleet: token, contract, history, money cap, approval mode, names, model, yolo, tags, rules?"));
    let mut failures = Vec::new();
    for (name, method, path, body, code) in cases {
        let before = e.snapshot();
        let (st, v) = e.call(Who::Worker(MRPOPO), method, &path, Some(cited(body, row.clone()))).await;
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

/// The classes above are mrpopo's alone: a lead or self citing Max is refused,
/// and no grant is created for them.
#[tokio::test]
async fn leads_and_self_never_get_mrpopos_classes() {
    let mut failures = Vec::new();
    for c in classes() {
        for who in [DEV_LEAD, DEV_A] {
            let e = setup().await;
            let row = e.cite(&ask_for(&c));
            let (st, v) = e.call(Who::Worker(who), c.method, &c.path, Some(cited(c.body.clone(), row))).await;
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
    let row = e.cite("Max, may I allow external email for dev?");
    let (st, v) = e.call(Who::Worker(MRPOPO), "PUT", "/api/scope",
        Some(cited(scope_body("group", "dev", "env", json!({"AMUX_EMAIL_EXTERNAL_ALLOW": "1"})), row))).await;
    assert_allowed(st, &v, "group-layer external email with a citation");
    assert_eq!(e.grant_status(&e.latest_grant()), "applied");
    let before = e.snapshot();
    let row = e.cite("Max, may I set an owner-only key for dev?");
    let (st, v) = e.call(Who::Worker(MRPOPO), "PUT", "/api/scope",
        Some(cited(scope_body("group", "dev", "env", json!({"CC_SOMETHING_OWNER_ONLY": "1"})), row))).await;
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
    let cases: Vec<(&str, Value, &str)> = vec![
        ("mrpopo: CC_FLAGS on the leads group", scope_body("group", "leads", "env", json!({"CC_FLAGS": "--dangerously-skip-permissions"})),
            "Max, may I set yolo flags for leads?"),
        ("mrpopo: CC_SEND_ALLOW=* globally", scope_body("global", "", "env", json!({"CC_SEND_ALLOW": "*"})),
            "Max, may I open routing for the fleet?"),
        ("mrpopo: external email globally", scope_body("global", "", "env", json!({"AMUX_EMAIL_EXTERNAL_ALLOW": "1"})),
            "Max, may I allow external email for the fleet?"),
        ("mrpopo: connectors on the leads group", scope_body("group", "leads", "connectors", json!({"google-gmail": true})),
            "Max, may I enable the gmail connector for leads?"),
    ];
    for (name, body, ask) in cases {
        let before = (e.snapshot(), effective(name));
        let row = e.cite(ask);
        let (st, v) = e.call(Who::Worker(MRPOPO), "PUT", "/api/scope", Some(cited(body, row))).await;
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
    let row = e.cite("Max, may I update the rules for leads?");
    let (st, v) = e.call(Who::Worker(MRPOPO), "PUT", "/api/scope", Some(cited(scope_body("group", "leads", "rules", json!("Leads: report blockers first.")), row))).await;
    assert_allowed(st, &v, "mrpopo sets the leads rules with a citation");
}
