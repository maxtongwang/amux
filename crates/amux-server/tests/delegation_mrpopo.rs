//! mrpopo, the owner agent (maxtongwang/ai-config#160, piece d; structured grants).
//!
//! Every non-secret change that was owner-only is mrpopo's to make by CITING
//! one of Max's typed rows (cite mode, the default): it applies at once as a
//! grant, logged and undoable. Each delegable class is driven: no citation ->
//! refused; another lane's citation -> applied; undo -> the files read as
//! before.
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
}

fn classes() -> Vec<Class> {
    let cfg = format!("/api/sessions/{DEV_A}/config");
    vec![
        Class {
            name: "make lead",
            method: "PUT",
            path: "/api/scope".into(),
            body: scope_body("worker", DEV_A, "env", json!({"CC_TAGS": "dev,leads", "CC_ORCHESTRATOR": "1"})),
        },
        Class {
            name: "leads rules",
            method: "PUT",
            path: "/api/scope".into(),
            body: scope_body("group", "leads", "rules", json!("Leads: report blockers first.")),
        },
        Class {
            name: "leads membership",
            method: "PATCH",
            path: cfg.clone(),
            body: json!({"tags": "dev,leads"}),
        },
        Class {
            name: "routing",
            method: "PATCH",
            path: cfg.clone(),
            body: json!({"send_allow": ""}),
        },
        Class {
            name: "yolo",
            method: "PATCH",
            path: cfg.clone(),
            body: json!({"toggle_yolo": true}),
        },
        Class {
            name: "external email",
            method: "PATCH",
            path: cfg.clone(),
            body: json!({"external_email_allowed": true}),
        },
        Class {
            name: "isolated",
            method: "PATCH",
            path: cfg.clone(),
            body: json!({"isolated": true}),
        },
        Class {
            name: "gates",
            method: "PUT",
            path: "/api/scope".into(),
            body: scope_body("worker", DEV_A, "gates", json!({"review": true})),
        },
        Class {
            name: "connectors",
            method: "PUT",
            path: "/api/scope".into(),
            body: scope_body("worker", DEV_A, "connectors", json!({"google-gmail": true})),
        },
        Class {
            name: "outbound approval",
            method: "PUT",
            path: "/api/scope".into(),
            body: scope_body("worker", DEV_A, "env", json!({"AMUX_APPROVAL_TYPES": "budget"})),
        },
        Class {
            name: "group config",
            method: "PATCH",
            // `ops`: a lead's own team's group config is the lead's (decide_lead).
            path: "/api/groups/ops/config".into(),
            body: json!({"goal": "ship delegation"}),
        },
        Class {
            name: "commit guard",
            method: "PATCH",
            path: "/api/settings/commit-guard".into(),
            body: json!({"enabled": false}),
        },
        Class {
            name: "task guard",
            method: "PATCH",
            path: "/api/settings/task-guard".into(),
            body: json!({"enabled": true}),
        },
        Class {
            name: "default model",
            method: "PATCH",
            path: "/api/settings/default-model".into(),
            body: json!({"model": "sonnet"}),
        },
        Class {
            // The real mutating route for browser-profile access is the
            // worker's scope env key (GET /api/browser/profiles is read-only).
            name: "browser profiles",
            method: "PUT",
            path: "/api/scope".into(),
            body: scope_body("worker", DEV_A, "env", json!({"AMUX_BROWSER_PROFILES_ALLOW": "review-profile"})),
        },
        Class {
            name: "alerts config (ai-config #175)",
            method: "PATCH",
            path: "/api/alert/config".into(),
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
        if (st.as_u16(), v["code"].as_str()) != (403, Some("cite_missing")) {
            failures.push(format!("{}: without a citation expected 403 cite_missing, got {st} {v}", c.name));
        }
        let row = e.seed_pane_prompt(MAX_PANE, &format!("Max asks: {}", c.name), now_s() - 30.0);
        let (st, v) = e.call(Who::Worker(MRPOPO), c.method, &c.path, Some(cited(c.body.clone(), row))).await;
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
    let row = e.seed_pane_prompt(MAX_PANE, "Max asks for all of these", now_s() - 30.0);
    let mut failures = Vec::new();
    for (name, method, path, body, code) in cases {
        let before = e.snapshot();
        let (st, v) = e.call(Who::Worker(MRPOPO), method, &path, Some(cited(body, row))).await;
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
            let row = e.seed_pane_prompt(MAX_PANE, "Max asks", now_s() - 30.0);
            let (st, v) = e.call(Who::Worker(who), c.method, &c.path, Some(cited(c.body.clone(), row))).await;
            if st.as_u16() != 403 || !e.latest_grant().is_empty() {
                failures.push(format!("{} as {who}: expected a 403 and no grant, got {st} {v}", c.name));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// mrpopo at the group and global layers: an explicit row applies there with
/// a citation; an owner-only key with no row is refused and never a grant.
#[tokio::test]
async fn mrpopo_at_the_group_and_global_layers() {
    for (level, name) in [("group", "dev"), ("global", "")] {
        let e = setup().await;
        let row = e.seed_pane_prompt(MAX_PANE, "allow external email", now_s() - 30.0);
        let (st, v) = e.call(Who::Worker(MRPOPO), "PUT", "/api/scope",
            Some(cited(scope_body(level, name, "env", json!({"AMUX_EMAIL_EXTERNAL_ALLOW": "1"})), row))).await;
        assert_allowed(st, &v, &format!("{level}-layer external email with a citation"));
        assert_eq!(e.grant_status(&e.latest_grant()), "applied", "{level}");
        let before = e.snapshot();
        let (st, v) = e.call(Who::Worker(MRPOPO), "PUT", "/api/scope",
            Some(cited(scope_body(level, name, "env", json!({"CC_SOMETHING_OWNER_ONLY": "1"})), row))).await;
        assert_code(st, &v, "delegation_not_allowed", &format!("{level}-layer key with no row"));
        assert_eq!(before, e.snapshot(), "{level}: nothing written");
    }
}
