//! Sweep: every config-mutating route consults delegation
//! (maxtongwang/ai-config#160, pieces a and d).
//!
//! For each route two legs:
//! - REFUSED: a non-allowlisted worker (review-dev-a, a plain member) asking
//!   for a change the table does not grant it gets #8's refusal shape, and
//!   nothing is written.
//! - CONSULTED: a change only `decide()` grants, never #8's scattered rules
//!   (which refuse every worker-on-peer config and every `leads` grant),
//!   succeeds through the same route and appends exactly one delegated-change
//!   entry. A route that skipped delegation would refuse it or log nothing,
//!   so this leg is what proves the route asks.
//!
//! Membership has no dedicated `groups/membership` route on main; it is
//! written through the tags-bearing routes below (config PATCH, scope env
//! CC_TAGS, env/apply, config/apply), each swept. Routing is `send_allow`.

mod delegation_support;

use delegation_support::*;
use serde_json::{json, Value};

struct Case {
    name: &'static str,
    method: &'static str,
    path: String,
    /// What the plain worker asks for (refused).
    refused: Value,
    /// Who the consulted leg speaks as, and what it asks for (allowed).
    allowed_as: &'static str,
    allowed: Value,
    /// Whether the allowed leg needs Max's typed instruction in its pane.
    needs_ref: bool,
}

fn cases(e: &Env) -> Vec<Case> {
    let dir = e.dir.to_str().unwrap().to_string();
    vec![
        Case {
            name: "scope PUT (group rules)",
            method: "PUT",
            path: "/api/scope".into(),
            refused: scope_body("group", "dev", "rules", json!("plain worker rule")),
            allowed_as: DEV_LEAD,
            allowed: scope_body("group", "dev", "rules", json!("lead rule")),
            needs_ref: false,
        },
        Case {
            name: "scope PUT (worker env membership)",
            method: "PUT",
            path: "/api/scope".into(),
            refused: scope_body("worker", DEV_B, "env", json!({"CC_TAGS": "dev,leads"})),
            allowed_as: MRPOPO,
            allowed: scope_body("worker", DEV_B, "env", json!({"CC_TAGS": "dev,leads"})),
            needs_ref: true,
        },
        Case {
            name: "env/apply",
            method: "POST",
            path: "/api/env/apply".into(),
            refused: json!({"workers": [{"name": "review-sweep-env", "groups": ["dev"], "dir": dir}]}),
            allowed_as: DEV_LEAD,
            allowed: json!({"workers": [{"name": "review-sweep-env-l", "groups": ["dev"], "dir": dir}]}),
            needs_ref: false,
        },
        Case {
            name: "config/apply",
            method: "PUT",
            path: "/api/config/apply".into(),
            refused: json!({"workers": [{"name": "review-sweep-iac", "tags": ["dev"], "dir": dir}]}),
            allowed_as: DEV_LEAD,
            allowed: json!({"workers": [{"name": "review-sweep-iac-l", "tags": ["dev"], "dir": dir}]}),
            needs_ref: false,
        },
        Case {
            name: "PATCH /api/sessions/<w>/config",
            method: "PATCH",
            path: format!("/api/sessions/{DEV_B}/config"),
            refused: json!({"model": "sonnet"}),
            allowed_as: DEV_LEAD,
            allowed: json!({"model": "sonnet"}),
            needs_ref: false,
        },
        Case {
            name: "PATCH /api/workers/{key}/config",
            method: "PATCH",
            path: format!("/api/workers/{DEV_B}/config"),
            refused: json!({"model": "sonnet"}),
            allowed_as: DEV_LEAD,
            allowed: json!({"model": "sonnet"}),
            needs_ref: false,
        },
        Case {
            name: "PATCH /api/workers/{key}",
            method: "PATCH",
            path: format!("/api/workers/{DEV_B}"),
            refused: json!({"model": "sonnet"}),
            allowed_as: DEV_LEAD,
            allowed: json!({"model": "sonnet"}),
            needs_ref: false,
        },
        Case {
            name: "create (POST /api/sessions)",
            method: "POST",
            path: "/api/sessions".into(),
            refused: create_body(&e.dir, "review-sweep-new", json!(["dev"])),
            allowed_as: DEV_LEAD,
            allowed: create_body(&e.dir, "review-sweep-lead-new", json!(["dev"])),
            needs_ref: false,
        },
        Case {
            name: "create (POST /api/workers)",
            method: "POST",
            path: "/api/workers".into(),
            refused: json!({"name": "review-sweep-w", "cwd": "/tmp"}),
            allowed_as: DEV_LEAD,
            allowed: json!({"name": "review-sweep-wl", "cwd": "/tmp"}),
            needs_ref: false,
        },
        Case {
            // A worker renames itself; a lead's rename waits for amux #3.
            name: "rename",
            method: "POST",
            path: format!("/api/sessions/{DEV_B}/rename"),
            refused: json!({"name": "review-dev-b-renamed"}),
            allowed_as: DEV_B,
            allowed: json!({"name": "review-dev-b-renamed"}),
            needs_ref: false,
        },
        Case {
            name: "stop",
            method: "POST",
            path: format!("/api/sessions/{OPS_A}/stop"),
            refused: json!({}),
            allowed_as: OPS_LEAD,
            allowed: json!({}),
            needs_ref: false,
        },
        Case {
            name: "membership (config tags)",
            method: "PATCH",
            path: format!("/api/sessions/{OPS_A}/config"),
            refused: json!({"tags": "ops,leads"}),
            allowed_as: MRPOPO,
            allowed: json!({"tags": "ops,leads"}),
            needs_ref: true,
        },
        Case {
            name: "routing (send_allow)",
            method: "PATCH",
            path: format!("/api/sessions/{OPS_A}/config"),
            refused: json!({"send_allow": DEV_A}),
            allowed_as: MRPOPO,
            allowed: json!({"send_allow": DEV_A}),
            needs_ref: true,
        },
    ]
}

#[tokio::test]
async fn every_config_mutating_route_refuses_a_non_allowlisted_worker() {
    let e = setup().await;
    let mut failures = Vec::new();
    for c in cases(&e) {
        let before = e.snapshot();
        let (st, v) = e.call(Who::Worker(DEV_A), c.method, &c.path, Some(c.refused.clone())).await;
        let shaped = st.as_u16() == 403 && v["ok"] == json!(false) && v["code"].as_str().is_some_and(|code| !code.is_empty());
        if !shaped {
            failures.push(format!("{}: plain worker expected #8's 403 refusal, got {st} {v}", c.name));
        }
        if e.snapshot() != before {
            failures.push(format!("{}: refused request wrote a file", c.name));
        }
    }
    assert!(e.log_entries().is_empty(), "refusals log nothing: {:?}", e.log_entries());
    assert!(failures.is_empty(), "routes that did not refuse:\n{}", failures.join("\n"));
}

/// Each case runs in its own fresh home so one allowed change cannot set up or
/// mask the next.
#[tokio::test]
async fn every_config_mutating_route_consults_delegation() {
    let n = { let e = setup().await; cases(&e).len() };
    let mut failures = Vec::new();
    for i in 0..n {
        let e = setup().await;
        let c = cases(&e).remove(i);
        if c.path.starts_with("/api/workers/") && c.method == "PATCH" && !c.path.ends_with("/config") {
            // The store row the modern route edits.
            let (st, v) = e.call(Who::Owner, "POST", "/api/workers", Some(json!({"name": DEV_B, "cwd": "/tmp"}))).await;
            assert!(st.is_success(), "fixture: owner registers {DEV_B}'s store row: {st} {v}");
        }
        let body = if c.needs_ref {
            let t = now_s() - 60.0;
            let target = c.path.split('/').nth(3).filter(|s| s.starts_with("review-")).unwrap_or(DEV_B);
            e.seed_pane_prompt(c.allowed_as, &format!("add {target} to the leads group and set its send allow routing"), t);
            with_ref(c.allowed.clone(), max_ref(c.allowed_as, t), "Max asked")
        } else {
            c.allowed.clone()
        };
        let (st, v) = e.call(Who::Worker(c.allowed_as), c.method, &c.path, Some(body)).await;
        if !st.is_success() {
            failures.push(format!("{} as {}: {st} {v}", c.name, c.allowed_as));
            continue;
        }
        if e.log_entries().len() != 1 {
            failures.push(format!("{}: allowed delegated change logged {} entries", c.name, e.log_entries().len()));
        }
    }
    assert!(failures.is_empty(), "routes that did not consult delegation:\n{}", failures.join("\n"));
}
