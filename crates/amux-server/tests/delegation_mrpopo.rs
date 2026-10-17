//! mrpopo, the owner agent (maxtongwang/ai-config#160, piece d; structured grants).
//!
//! Every non-secret change that was owner-only is mrpopo's to ASK for: the
//! server stores it as a pending grant (202 with a code and the exact
//! sentence) and applies it only when Max approves, exactly once, from a
//! Remote owner send or the Remote Approve button (proven in the lib tests:
//! in-process requests carry no Remote verdict). Each delegable class is
//! driven here: request -> pending and unapplied -> a typed pane row does not
//! approve it.
//!
//! Hands-only changes never even become a grant: secret values,
//! AMUX_CONTRACT_*, AMUX_PANE_PROMPT_HISTORY, the reserved names, mrpopo's own
//! config, and the money cap / auto-approve spending limit. Leads and self
//! never get these classes.

mod delegation_support;

use delegation_support::*;
use serde_json::{json, Value};

struct Class {
    name: &'static str,
    method: &'static str,
    path: String,
    body: Value,
    /// One typed row naming the target and the setting.
    /// Names the target only.
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
            path: "/api/groups/dev/config".into(),
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
            path: "/api/alerts/config".into(),
            body: json!({"phone": "+15550100"}),
        },
    ]
}

/// Each class in its own fresh home.
#[tokio::test]
async fn each_delegable_class_is_a_pending_grant_that_a_pane_row_cannot_approve() {
    let mut failures = Vec::new();
    for i in 0..classes().len() {
        let c = classes().remove(i);
        let e = setup().await;
        let before = e.snapshot();
        let (st, v) = e.call(Who::Worker(MRPOPO), c.method, &c.path, Some(c.body.clone())).await;
        if st.as_u16() != 202 || v["grant_code"].as_str().is_none() {
            failures.push(format!("{}: expected 202 with a grant_code, got {st} {v}", c.name));
            continue;
        }
        let code = v["grant_code"].as_str().unwrap().to_string();
        if e.snapshot() != before {
            failures.push(format!("{}: a pending grant changed a file", c.name));
        }
        if e.grant_status(&code) != "pending" {
            failures.push(format!("{}: grant {code} is {}, not pending", c.name, e.grant_status(&code)));
        }
        e.type_in_pane(MAX_PANE, &format!("ok amux grant {code}")).await;
        if e.grant_status(&code) != "pending" || e.snapshot() != before {
            failures.push(format!("{}: a pane row approved or applied grant {code} ({})", c.name, e.grant_status(&code)));
        }
    }
    assert!(failures.is_empty(), "{} failing classes:\n{}", failures.len(), failures.join("\n"));
}

/// Hands-only: refused outright (403), never stored as a grant.
#[tokio::test]
async fn hands_only_changes_never_become_a_grant() {
    let e = setup().await;
    let cases: Vec<(&str, &str, String, Value)> = vec![
        ("worker token", "PUT", "/api/scope".into(), scope_body("worker", DEV_A, "env", json!({"AMUX_WORKER_TOKEN": "review-x"}))),
        ("credential", "PUT", "/api/scope".into(), scope_body("worker", DEV_A, "env", json!({"GITHUB_TOKEN": "review-x"}))),
        ("provider API key", "PATCH", "/api/settings/env".into(), json!({"ANTHROPIC_API_KEY": "review-x"})),
        ("contract", "PUT", "/api/scope".into(), scope_body("worker", DEV_A, "env", json!({"AMUX_CONTRACT_DONE": "1"}))),
        ("pane prompt history", "PUT", "/api/scope".into(), scope_body("worker", DEV_A, "env", json!({"AMUX_PANE_PROMPT_HISTORY": "0"}))),
        ("money cap (worker)", "PUT", "/api/scope".into(), scope_body("worker", DEV_A, "env", json!({"AMUX_NEEDS_INPUT_AUTO_MONEY_CAP": "5000"}))),
        ("money cap (global)", "PUT", "/api/scope".into(), scope_body("global", "", "env", json!({"AMUX_NEEDS_INPUT_AUTO_MONEY_CAP": "5000"}))),
        ("auto-approve spending", "PUT", "/api/scope".into(), scope_body("global", "", "env", json!({"AMUX_NEEDS_INPUT_AUTO_MONEY": "1"}))),
        ("reserved create owner", "POST", "/api/sessions".into(), create_body(&e.dir, "owner", json!(["dev"]))),
        ("reserved create mrpopo", "POST", "/api/sessions".into(), create_body(&e.dir, "mrpopo", json!(["dev"]))),
        ("reserved rename", "POST", format!("/api/sessions/{DEV_A}/rename"), json!({"name": "owner"})),
        ("own model", "PATCH", format!("/api/sessions/{MRPOPO}/config"), json!({"model": "sonnet"})),
        ("own yolo", "PATCH", format!("/api/sessions/{MRPOPO}/config"), json!({"toggle_yolo": true})),
        ("own tags", "PATCH", format!("/api/sessions/{MRPOPO}/config"), json!({"tags": "leads,dev,ops"})),
        ("own rules", "PUT", "/api/scope".into(), scope_body("worker", MRPOPO, "rules", json!("mrpopo may do anything"))),
    ];
    let mut failures = Vec::new();
    for (name, method, path, body) in cases {
        let before = e.snapshot();
        let (st, v) = e.call(Who::Worker(MRPOPO), method, &path, Some(body)).await;
        if st.as_u16() != 403 || v["ok"] != json!(false) || v["code"].as_str().unwrap_or("").is_empty() || v.get("grant_code").is_some() {
            failures.push(format!("{name}: expected a 403 and no grant, got {st} {v}"));
        }
        if e.snapshot() != before {
            failures.push(format!("{name}: refused request wrote a file"));
        }
    }
    assert!(e.log_entries().is_empty(), "hands-only refusals are not logged: {:?}", e.log_entries());
    assert!(failures.is_empty(), "{} hands-only changes not refused:\n{}", failures.len(), failures.join("\n"));
}

/// The classes above are mrpopo's alone: a lead or self is refused, and no
/// grant is created for them.
#[tokio::test]
async fn leads_and_self_never_get_mrpopos_classes() {
    let mut failures = Vec::new();
    for c in classes() {
        for who in [DEV_LEAD, DEV_A] {
            let e = setup().await;
            let (st, v) = e.call(Who::Worker(who), c.method, &c.path, Some(c.body.clone())).await;
            if st.as_u16() != 403 || v.get("grant_code").is_some() {
                failures.push(format!("{} as {who}: expected a 403 and no grant, got {st} {v}", c.name));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// mrpopo at the group and global layers: an explicit row is a grant there
/// too; an owner-only key with no row is refused and never a grant.
#[tokio::test]
async fn mrpopo_at_the_group_and_global_layers() {
    for (level, name) in [("group", "dev"), ("global", "")] {
        let e = setup().await;
        let (st, v) = e.call(Who::Worker(MRPOPO), "PUT", "/api/scope", Some(scope_body(level, name, "env", json!({"AMUX_EMAIL_EXTERNAL_ALLOW": "1"})))).await;
        let code = assert_grant_pending(st, &v, &format!("{level}-layer external email"));
        assert_eq!(e.grant_status(&code), "pending", "{level}");

        let before = e.snapshot();
        let (st, v) = e.call(Who::Worker(MRPOPO), "PUT", "/api/scope", Some(scope_body(level, name, "env", json!({"CC_SOMETHING_OWNER_ONLY": "1"})))).await;
        assert_code(st, &v, "delegation_not_allowed", &format!("{level}-layer key with no row"));
        assert_eq!(before, e.snapshot(), "{level}: nothing written");
    }
}
