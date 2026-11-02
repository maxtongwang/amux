//! Everything an owner scopes to a worker (memory and rules at global, group
//! and worker level) reaches that worker's launch, reaches nobody else, and
//! reaches a RUNNING worker again when it changes, deletions included.
//!
//! Found with real Claude and Codex workers on a side server: global memory
//! reached no model at all; a live rules change was refused by Claude as
//! "pasted content"; memory changes and a rules deletion never reached a
//! running worker; and `codex resume` kept the thread's original developer
//! instructions after an edit. These cells pin the composition and delivery
//! seams those fixes run through; the model-facing half is in the PR's matrix.
//!
//! One test function on purpose: `AMUX_HOME` is process-global, and cargo runs
//! a binary's tests in parallel, so sibling tests would race on it.

use amux_server::api::session_verbs::{
    codex_resume_context_prompt, compose_worker_context, worker_rules_args, worker_rules_file,
    SCOPED_CONTEXT_MARKER,
};
use amux_server::api::{router, AppState};
use amux_server::db::Store;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use tower::ServiceExt;

fn write(p: &Path, body: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

async fn call(app: &axum::Router, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 22).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn file_of(name: &str) -> String {
    fs::read_to_string(worker_rules_file(name)).unwrap_or_default()
}

#[tokio::test]
async fn scoped_memory_and_rules_reach_only_their_worker_and_follow_every_change() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    // A private tmux socket dir: the live-notice path asks tmux whether a
    // worker is running, and that question must never reach the fleet's server.
    let tmux = tempfile::tempdir().unwrap();
    std::env::set_var("AMUX_HOME", h);
    std::env::set_var("TMUX_TMPDIR", tmux.path());
    std::env::remove_var("TMUX");

    // Two workers in DIFFERENT groups sharing ONE directory (the shape where
    // Claude's per-directory memory used to leak), a Codex worker, and an
    // isolated worker in the same group as the first.
    write(&h.join("sessions/wa.env"), "CC_DIR=/tmp/shared\nCC_TAGS=\"alpha\"\n");
    write(&h.join("sessions/wb.env"), "CC_DIR=/tmp/shared\nCC_TAGS=\"beta\"\n");
    write(&h.join("sessions/wx.env"), "CC_DIR=/tmp/cx\nCC_PROVIDER=codex\nCC_TAGS=\"alpha\"\n");
    write(&h.join("sessions/wi.env"), "CC_DIR=/tmp/iso\nCC_TAGS=\"alpha\"\nCC_ISOLATED=1\n");
    write(&h.join("memory/_global.md"), "GLOBAL-MEM-1\n");
    write(&h.join("memory/tags/alpha.md"), "ALPHA-MEM-1\n");
    write(&h.join("memory/tags/beta.md"), "BETA-MEM-1\n");
    write(&h.join("memory/tags/alpha.rules.md"), "ALPHA-RULE-1\n");
    write(&h.join("memory/wa.md"), "WA-MEM-1\n");
    write(&h.join("memory/wa.rules.md"), "WA-RULE-1\n");
    write(&h.join("memory/wb.md"), "WB-MEM-1\n");
    write(&h.join("memory/wi.md"), "WI-MEM-1\n");

    // ---- 1. Launch composition: every layer, in precedence order. ----------
    let wa = compose_worker_context("wa", false);
    for want in ["GLOBAL-MEM-1", "ALPHA-MEM-1", "WA-MEM-1", "ALPHA-RULE-1", "WA-RULE-1"] {
        assert!(wa.contains(want), "{want} missing from wa's launch context:\n{wa}");
    }
    let pos = |t: &str| wa.find(t).unwrap();
    assert!(
        pos("GLOBAL-MEM-1") < pos("ALPHA-MEM-1") && pos("ALPHA-MEM-1") < pos("WA-MEM-1"),
        "memory must compose global > group > worker:\n{wa}"
    );
    assert!(pos("ALPHA-RULE-1") < pos("WA-RULE-1"), "rules compose group before worker");
    assert!(
        wa.contains(SCOPED_CONTEXT_MARKER) && wa.contains(&worker_rules_file("wa").display().to_string()),
        "the launch context must vouch for the re-read file and its marker:\n{wa}"
    );

    // ---- 2. Isolation: nothing crosses groups, workers, or into isolated. --
    let wb = compose_worker_context("wb", false);
    assert!(wb.contains("BETA-MEM-1") && wb.contains("WB-MEM-1"), "premise: wb gets its own:\n{wb}");
    for leak in ["ALPHA-MEM-1", "ALPHA-RULE-1", "WA-MEM-1", "WA-RULE-1"] {
        assert!(!wb.contains(leak), "{leak} leaked into a worker outside its scope:\n{wb}");
    }
    assert!(
        worker_rules_args("wi", "claude", true).is_empty(),
        "an isolated worker's launch carries nothing"
    );
    assert!(!worker_rules_file("wi").exists(), "and no context file exists for it");

    // ---- 3. Both providers carry the same block, and the file matches. ----
    let claude = worker_rules_args("wa", "claude", false);
    assert_eq!(claude.first().map(String::as_str), Some("--append-system-prompt-file"));
    assert_eq!(file_of("wa"), wa, "Claude's system prompt file is the composed block");
    let codex = worker_rules_args("wx", "codex", false);
    assert_eq!(codex.first().map(String::as_str), Some("-c"));
    assert!(
        codex[1].starts_with("developer_instructions=") && codex[1].contains("GLOBAL-MEM-1")
            && codex[1].contains("ALPHA-RULE-1"),
        "Codex developer instructions carry global memory and group rules: {}",
        codex[1]
    );
    assert!(file_of("wx").contains(SCOPED_CONTEXT_MARKER), "Codex gets the re-read file too");

    // ---- 4. Owner writes through the real router. -------------------------
    let store = std::sync::Arc::new(Store::open(&h.join("scoped-context-test.db")).unwrap());
    let app = router(AppState {
        store: store.clone(),
        started: std::time::Instant::now(),
        build_hash: "test".into(),
        auth_token: None,
        reconciled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
    });

    // A group memory edit rewrites every member's file, says who it reached,
    // and names the isolated member it cannot reach.
    let (st, body) = call(&app, "PUT", "/api/scope",
        json!({"level":"group","name":"alpha","capability":"memory","value":{"text":"ALPHA-MEM-2"}})).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["affected_workers"], json!(["wa", "wi", "wx"]), "{body}");
    assert_eq!(body["isolated_not_reached"], json!(["wi"]), "{body}");
    assert!(body["takes_effect"].as_str().unwrap_or("").contains("re-read"), "{body}");
    assert!(file_of("wa").contains("ALPHA-MEM-2") && !file_of("wa").contains("ALPHA-MEM-1"),
        "the member's file must hold the edit and drop the old text:\n{}", file_of("wa"));
    assert!(file_of("wx").contains("ALPHA-MEM-2"), "Codex member refreshed too");
    assert!(!file_of("wb").contains("ALPHA-MEM-2"), "a non-member is untouched");
    assert!(!worker_rules_file("wi").exists(), "an isolated member never gets a file");

    // Deleting a worker's rules removes them from what it re-reads. Before the
    // fix an emptied rules layer sent nothing and changed no file.
    let (st, body) = call(&app, "PUT", "/api/scope",
        json!({"level":"worker","name":"wa","capability":"rules","value":{"text":""}})).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert!(!file_of("wa").contains("WA-RULE-1"), "a deleted rule must leave the file:\n{}", file_of("wa"));
    assert!(file_of("wa").contains("ALPHA-RULE-1"), "the group rule still applies");

    // Global memory through the Memory tab's endpoint reaches every
    // non-isolated worker's file.
    let (st, body) = call(&app, "POST", "/api/memory/global", json!({"content":"GLOBAL-MEM-2"})).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    for w in ["wa", "wb", "wx"] {
        assert!(file_of(w).contains("GLOBAL-MEM-2"), "{w} did not get the global edit:\n{}", file_of(w));
    }

    // Worker memory from the peek Memory tab: saved, refreshed, and an
    // isolated worker's save says it never reaches the model.
    let (st, body) = call(&app, "POST", "/api/sessions/wa/memory", json!({"content":"WA-MEM-2"})).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert!(file_of("wa").contains("WA-MEM-2") && !file_of("wa").contains("WA-MEM-1"));
    let (_, body) = call(&app, "POST", "/api/sessions/wi/memory", json!({"content":"WI-MEM-2"})).await;
    assert_eq!(body["isolated_not_reached"], json!(["wi"]), "{body}");

    // hq#165: real scope PUTs persist and queue the reread notice at both
    // restored dashboard levels. Fake only tmux liveness; never contact the fleet.
    let bin = tempfile::tempdir().unwrap();
    let fake_tmux = bin.path().join("tmux");
    fs::write(&fake_tmux, "#!/bin/sh\n[ \"$1\" = has-session ]\n").unwrap();
    fs::set_permissions(&fake_tmux, fs::Permissions::from_mode(0o755)).unwrap();
    let old_path = std::env::var_os("PATH");
    std::env::set_var("PATH", bin.path());
    for (level, name, text, expected, path) in [
        ("global", "", "GLOBAL-RULE-2", vec!["wa", "wb", "wx"], "memory/_rules.md"),
        ("group", "alpha", "ALPHA-RULE-2", vec!["wa", "wx"], "memory/tags/alpha.rules.md"),
    ] {
        let (st, body) = call(&app, "PUT", "/api/scope",
            json!({"level":level,"name":name,"capability":"rules","value":{"text":text}})).await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(body["delivered_live"], json!(expected), "{body}");
        assert_eq!(body["isolated_not_reached"], json!(["wi"]), "{body}");
        assert_eq!(fs::read_to_string(h.join(path)).unwrap().trim(), text);
        let conn = store.read().unwrap();
        let mut query = conn.prepare(
            "SELECT session, text FROM steering_queue WHERE guard='scoped-context' ORDER BY session"
        ).unwrap();
        let notices: Vec<(String, String)> = query.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap().map(Result::unwrap).collect();
        assert_eq!(notices.iter().map(|(w, _)| w.as_str()).collect::<Vec<_>>(), expected);
        for (worker, notice) in notices {
            assert!(notice.starts_with(SCOPED_CONTEXT_MARKER), "{notice}");
            assert!(notice.contains(&format!("rules ({level} scope")), "{notice}");
            assert!(notice.contains(&worker_rules_file(&worker).display().to_string()), "{notice}");
            assert!(file_of(&worker).contains(text));
        }
        assert!(!worker_rules_file("wi").exists());
        drop(query);
        drop(conn);
        store.write_async(|conn| {
            conn.execute("DELETE FROM steering_queue WHERE guard='scoped-context'", [])?;
            Ok(amux_server::db::WriteOutcome { applied: true, events: vec![] })
        }).await.unwrap();
    }
    if let Some(path) = old_path { std::env::set_var("PATH", path); }
    else { std::env::remove_var("PATH"); }

    // ---- 5. Codex resume re-asserts a changed context, and only then. -----
    let mut meta = serde_json::Map::new();
    let block = compose_worker_context("wx", false);
    assert!(codex_resume_context_prompt("wx", &block, false, &mut meta).is_none(), "fresh: nothing");
    assert!(
        codex_resume_context_prompt("wx", &block, true, &mut meta).is_none(),
        "resume with the same context adds no turn"
    );
    write(&h.join("memory/wx.md"), "WX-MEM-2\n");
    let changed = compose_worker_context("wx", false);
    let prompt = codex_resume_context_prompt("wx", &changed, true, &mut meta)
        .expect("a resume after a scope change must carry the re-read notice");
    assert!(prompt.starts_with(SCOPED_CONTEXT_MARKER) && prompt.contains(&worker_rules_file("wx").display().to_string()));
    let mut unknown = serde_json::Map::new();
    assert!(
        codex_resume_context_prompt("wx", &changed, true, &mut unknown).is_none(),
        "with no recorded digest nothing is claimed"
    );
}
