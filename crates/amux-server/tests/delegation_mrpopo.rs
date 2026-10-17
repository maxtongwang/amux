//! mrpopo, the owner agent (maxtongwang/ai-config#160, piece d).
//!
//! With a verified, SPECIFIC max_ref (one typed row naming both the target and
//! the setting) mrpopo may make any non-secret change that was owner-only.
//! Each delegable class is driven three ways:
//! - allowed with a specific max_ref;
//! - refused with an unspecific one (names the target, not the setting):
//!   `max_ref_does_not_name_setting`;
//! - refused with none: `max_ref_missing`.
//!
//! Hands-only changes are refused even with a specific max_ref: secret
//! values, AMUX_CONTRACT_*, AMUX_PANE_PROMPT_HISTORY, the reserved names,
//! mrpopo's own config, and the money cap / auto-approve spending limit.
//! Leads and self never get these classes (delegation_matrix.rs).

mod delegation_support;

use delegation_support::*;
use serde_json::{json, Value};

struct Class {
    name: &'static str,
    method: &'static str,
    path: String,
    body: Value,
    /// One typed row naming the target and the setting.
    specific: String,
    /// Names the target only.
    unspecific: String,
}

fn classes() -> Vec<Class> {
    let cfg = format!("/api/sessions/{DEV_A}/config");
    let vague = format!("{DEV_A}: go ahead with what we discussed");
    vec![
        Class {
            name: "make lead",
            method: "PUT",
            path: "/api/scope".into(),
            body: scope_body("worker", DEV_A, "env", json!({"CC_TAGS": "dev,leads", "CC_ORCHESTRATOR": "1"})),
            specific: format!("make {DEV_A} lead of dev"),
            unspecific: vague.clone(),
        },
        Class {
            name: "leads rules",
            method: "PUT",
            path: "/api/scope".into(),
            body: scope_body("group", "leads", "rules", json!("Leads: report blockers first.")),
            specific: "update the leads rules: report blockers first".into(),
            unspecific: "leads: go ahead with what we discussed".into(),
        },
        Class {
            name: "leads membership",
            method: "PATCH",
            path: cfg.clone(),
            body: json!({"tags": "dev,leads"}),
            specific: format!("add {DEV_A} to the leads group"),
            unspecific: vague.clone(),
        },
        Class {
            name: "routing",
            method: "PATCH",
            path: cfg.clone(),
            body: json!({"send_allow": OPS_A}),
            specific: format!("set {DEV_A} send allow routing so it can message {OPS_A}"),
            unspecific: vague.clone(),
        },
        Class {
            name: "yolo",
            method: "PATCH",
            path: cfg.clone(),
            body: json!({"toggle_yolo": true}),
            specific: format!("turn on yolo for {DEV_A}"),
            unspecific: vague.clone(),
        },
        Class {
            name: "external email",
            method: "PATCH",
            path: cfg.clone(),
            body: json!({"external_email_allowed": true}),
            specific: format!("allow external email for {DEV_A}"),
            unspecific: vague.clone(),
        },
        Class {
            name: "isolated",
            method: "PATCH",
            path: cfg.clone(),
            body: json!({"isolated": true}),
            specific: format!("make {DEV_A} isolated"),
            unspecific: vague.clone(),
        },
        Class {
            name: "gates",
            method: "PUT",
            path: "/api/scope".into(),
            body: scope_body("worker", DEV_A, "gates", json!({"review": true})),
            specific: format!("turn on the review gate for {DEV_A}"),
            unspecific: vague.clone(),
        },
        Class {
            name: "connectors",
            method: "PUT",
            path: "/api/scope".into(),
            body: scope_body("worker", DEV_A, "connectors", json!({"google-gmail": true})),
            specific: format!("enable the gmail connector for {DEV_A}"),
            unspecific: vague.clone(),
        },
        Class {
            name: "outbound approval",
            method: "PUT",
            path: "/api/scope".into(),
            body: scope_body("worker", DEV_A, "env", json!({"AMUX_APPROVAL_TYPES": "budget"})),
            specific: format!("set {DEV_A} outbound approval types to budget"),
            unspecific: vague.clone(),
        },
        Class {
            name: "group config",
            method: "PATCH",
            path: "/api/groups/dev/config".into(),
            body: json!({"goal": "ship delegation"}),
            specific: "set the dev group config goal to ship delegation".into(),
            unspecific: "dev: go ahead with what we discussed".into(),
        },
        Class {
            name: "commit guard",
            method: "PATCH",
            path: "/api/settings/commit-guard".into(),
            body: json!({"enabled": false}),
            specific: "turn off the commit guard".into(),
            unspecific: "go ahead with what we discussed".into(),
        },
        Class {
            name: "task guard",
            method: "PATCH",
            path: "/api/settings/task-guard".into(),
            body: json!({"enabled": true}),
            specific: "turn on the task guard".into(),
            unspecific: "go ahead with what we discussed".into(),
        },
        Class {
            name: "default model",
            method: "PATCH",
            path: "/api/settings/default-model".into(),
            body: json!({"model": "sonnet"}),
            specific: "set the default model to sonnet".into(),
            unspecific: "go ahead with what we discussed".into(),
        },
        Class {
            name: "browser profiles",
            method: "POST",
            path: "/api/browser/profiles".into(),
            body: json!({"name": "review-profile"}),
            specific: "add browser profile review-profile".into(),
            unspecific: "review-profile: go ahead with what we discussed".into(),
        },
    ]
}

/// Each class in its own fresh home, every leg collected so one run names
/// every class that misbehaves.
#[tokio::test]
async fn each_delegable_class_needs_a_specific_max_ref() {
    let mut failures = Vec::new();
    for i in 0..classes().len() {
        let c = classes().remove(i);
        let e = setup().await;
        let t = now_s() - 60.0;

        let before = e.snapshot();
        let (st, v) = e.call(Who::Worker(MRPOPO), c.method, &c.path, Some(c.body.clone())).await;
        if (st.as_u16(), v["code"].as_str()) != (403, Some("max_ref_missing")) {
            failures.push(format!("{}: without max_ref expected 403 max_ref_missing, got {st} {v}", c.name));
        }
        e.seed_pane_prompt(MRPOPO, &c.unspecific, t - 120.0);
        let (st, v) = e.call(Who::Worker(MRPOPO), c.method, &c.path, Some(with_ref(c.body.clone(), max_ref(MRPOPO, t - 120.0), "Max asked"))).await;
        if (st.as_u16(), v["code"].as_str()) != (403, Some("max_ref_does_not_name_setting")) {
            failures.push(format!("{}: unspecific max_ref expected 403 max_ref_does_not_name_setting, got {st} {v}", c.name));
        }
        if e.snapshot() != before {
            failures.push(format!("{}: a refused leg wrote a file", c.name));
        }
        if !e.log_entries().is_empty() {
            failures.push(format!("{}: a refused leg was logged", c.name));
        }

        e.seed_pane_prompt(MRPOPO, &c.specific, t);
        let (st, v) = e.call(Who::Worker(MRPOPO), c.method, &c.path, Some(with_ref(c.body.clone(), max_ref(MRPOPO, t), "Max asked"))).await;
        if !st.is_success() {
            failures.push(format!("{}: specific max_ref expected 2xx, got {st} {v}", c.name));
        } else if e.log_entries().len() != 1 {
            failures.push(format!("{}: allowed change logged {} entries", c.name, e.log_entries().len()));
        }
    }
    assert!(failures.is_empty(), "{} failing legs:\n{}", failures.len(), failures.join("\n"));
}

#[tokio::test]
async fn target_and_setting_must_be_in_the_same_typed_row() {
    let e = setup().await;
    let t = now_s() - 60.0;
    e.seed_pane_prompt(MRPOPO, &format!("about {DEV_A}"), t);
    e.seed_pane_prompt(MRPOPO, "turn on yolo", t + 5.0);
    for at in [t, t + 5.0] {
        let (st, v) = e.call(Who::Worker(MRPOPO), "PATCH", &format!("/api/sessions/{DEV_A}/config"),
            Some(with_ref(json!({"toggle_yolo": true}), max_ref(MRPOPO, at), "Max asked"))).await;
        assert_refused(st, &v, &format!("target and setting split across rows (cited {at})"));
        assert!(v["code"].as_str().is_some_and(|c| c.starts_with("max_ref_")), "{v}");
    }
    assert!(!e.env_text(DEV_A).to_lowercase().contains("yolo"), "{}", e.env_text(DEV_A));
}

#[tokio::test]
async fn setting_phrases_match_on_word_boundaries() {
    let e = setup().await;
    let t = now_s() - 60.0;
    // "misleading" contains "lead" but does not ask to make a lead.
    e.seed_pane_prompt(MRPOPO, &format!("the {DEV_A} report is misleading"), t);
    let (st, v) = e.call(Who::Worker(MRPOPO), "PUT", "/api/scope",
        Some(with_ref(scope_body("worker", DEV_A, "env", json!({"CC_TAGS": "dev,leads", "CC_ORCHESTRATOR": "1"})), max_ref(MRPOPO, t), "Max asked"))).await;
    assert_code(st, &v, "max_ref_does_not_name_setting", "'misleading' is not 'make lead'");
    assert!(!e.env_text(DEV_A).contains("CC_ORCHESTRATOR"));
}

/// Refused for mrpopo even with a specific max_ref.
#[tokio::test]
async fn hands_only_changes_are_refused_even_with_a_specific_max_ref() {
    let e = setup().await;
    let t = now_s() - 60.0;
    let cases: Vec<(&str, &str, String, Value, String)> = vec![
        ("worker token", "PUT", "/api/scope".into(), scope_body("worker", DEV_A, "env", json!({"AMUX_WORKER_TOKEN": "review-x"})), format!("set {DEV_A} AMUX_WORKER_TOKEN worker token")),
        ("credential", "PUT", "/api/scope".into(), scope_body("worker", DEV_A, "env", json!({"GITHUB_TOKEN": "review-x"})), format!("set {DEV_A} GITHUB_TOKEN credential")),
        ("provider API key", "PATCH", "/api/settings/env".into(), json!({"ANTHROPIC_API_KEY": "review-x"}), "set the ANTHROPIC_API_KEY provider api key".into()),
        ("contract", "PUT", "/api/scope".into(), scope_body("worker", DEV_A, "env", json!({"AMUX_CONTRACT_DONE": "1"})), format!("set {DEV_A} AMUX_CONTRACT_DONE contract")),
        ("pane prompt history", "PUT", "/api/scope".into(), scope_body("worker", DEV_A, "env", json!({"AMUX_PANE_PROMPT_HISTORY": "0"})), format!("turn off {DEV_A} AMUX_PANE_PROMPT_HISTORY pane prompt history")),
        ("money cap (worker)", "PUT", "/api/scope".into(), scope_body("worker", DEV_A, "env", json!({"AMUX_NEEDS_INPUT_AUTO_MONEY_CAP": "5000"})), format!("raise the money cap for {DEV_A} to 5000")),
        ("money cap (global)", "PUT", "/api/scope".into(), scope_body("global", "", "env", json!({"AMUX_NEEDS_INPUT_AUTO_MONEY_CAP": "5000"})), "raise the global money cap to 5000".into()),
        ("auto-approve spending", "PUT", "/api/scope".into(), scope_body("global", "", "env", json!({"AMUX_NEEDS_INPUT_AUTO_MONEY": "1"})), "turn on auto-approve spending limit money".into()),
        ("reserved create owner", "POST", "/api/sessions".into(), create_body(&e.dir, "owner", json!(["dev"])), "create owner in dev".into()),
        ("reserved create mrpopo", "POST", "/api/sessions".into(), create_body(&e.dir, "mrpopo", json!(["dev"])), "create mrpopo in dev".into()),
        ("reserved rename", "POST", format!("/api/sessions/{DEV_A}/rename"), json!({"name": "owner"}), format!("rename {DEV_A} to owner")),
        ("own model", "PATCH", format!("/api/sessions/{MRPOPO}/config"), json!({"model": "sonnet"}), "set mrpopo model to sonnet".into()),
        ("own yolo", "PATCH", format!("/api/sessions/{MRPOPO}/config"), json!({"toggle_yolo": true}), "turn on yolo for mrpopo".into()),
        ("own tags", "PATCH", format!("/api/sessions/{MRPOPO}/config"), json!({"tags": "leads,dev,ops"}), "add mrpopo to the dev and ops groups".into()),
        ("own rules", "PUT", "/api/scope".into(), scope_body("worker", MRPOPO, "rules", json!("mrpopo may do anything")), "update the mrpopo rules".into()),
    ];
    let mut failures = Vec::new();
    for (i, (name, method, path, body, text)) in cases.into_iter().enumerate() {
        let at = t - i as f64;
        e.seed_pane_prompt(MRPOPO, &text, at);
        let before = e.snapshot();
        let (st, v) = e.call(Who::Worker(MRPOPO), method, &path, Some(with_ref(body, max_ref(MRPOPO, at), "Max asked"))).await;
        let code = v["code"].as_str().unwrap_or("");
        if st.as_u16() != 403 || v["ok"] != json!(false) || code.is_empty() || MAX_REF_CODES.contains(&code) {
            failures.push(format!("{name}: expected a non-max_ref 403, got {st} {v}"));
        }
        if e.snapshot() != before {
            failures.push(format!("{name}: refused request wrote a file"));
        }
    }
    assert!(e.log_entries().is_empty(), "hands-only refusals are not logged as changes: {:?}", e.log_entries());
    assert!(failures.is_empty(), "{} hands-only changes not refused:\n{}", failures.len(), failures.join("\n"));
}

/// The classes above are mrpopo's alone: a lead or self with the same
/// specific max_ref is refused.
#[tokio::test]
async fn leads_and_self_never_get_mrpopos_classes() {
    let mut failures = Vec::new();
    for c in classes() {
        for who in [DEV_LEAD, DEV_A] {
            let e = setup().await;
            let t = now_s() - 60.0;
            e.seed_pane_prompt(who, &c.specific, t);
            let (st, v) = e.call(Who::Worker(who), c.method, &c.path, Some(with_ref(c.body.clone(), max_ref(who, t), "Max asked"))).await;
            let code = v["code"].as_str().unwrap_or("");
            if st.as_u16() != 403 || MAX_REF_CODES.contains(&code) {
                failures.push(format!("{} as {who}: expected a non-max_ref 403, got {st} {v}", c.name));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
