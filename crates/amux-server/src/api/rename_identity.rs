//! A renamed worker keeps its identity (2026-10-16).
//!
//! `rename_session` moves a worker's tmux session and files to the new name,
//! but the worker's RUNNING process keeps the environment it launched with:
//! `AMUX_WORKER`, `AMUX_SESSION` and `AMUX_STATUS_WORKER` still say the old
//! name, and the CLI and hooks send that as `X-Amux-Worker` /
//! `X-Amux-Session`. Every request the worker made after a rename therefore
//! claimed a name that no longer existed: it had no groups, so its sends were
//! refused at the group boundary, and its cards and callbacks addressed a dead
//! lane. Live: `amux-setup-complete` (renamed from `claude-0b`) sending to
//! `dev-lead` was refused as `claude-0b [{}] -> dev-lead [{"dev"}]`.
//!
//! The fix is one resolver at the request boundary, not a patch per reader:
//! every successful rename appends `{old, new, at}` to `~/.amux/renames.jsonl`,
//! and [`resolve_headers`] rewrites a worker header that names a worker which
//! no longer exists to the name that worker holds now, following the chain
//! (a -> b -> c). Every handler that reads the headers (sends, the group
//! boundary, board ownership, callbacks, the rule-10 token check) then sees the
//! current name without knowing renames exist.
//!
//! Rules:
//! - A name a live worker holds (its env file exists) is never redirected, so
//!   a new worker created under an old name is not hijacked by the alias.
//! - The newest record for a name wins; a chain is followed until it reaches a
//!   live worker. A chain that ends nowhere live leaves the header unchanged.
//! - A cycle (a -> b -> a with neither live) is detected and logged
//!   (`rename_alias_cycle`), and the header is left unchanged.
//! - Known limit: if a name is renamed away twice by two different workers
//!   (a -> b, then a new `a` is created and renamed a -> c), a process still
//!   claiming `a` resolves to the newest holder, `c`.
use axum::extract::Request;
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::Response;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// The headers a worker's identity is read from, in every module.
const IDENTITY_HEADERS: [&str; 2] = ["x-amux-worker", "x-amux-session"];

fn record_path(home: &Path) -> PathBuf {
    home.join("renames.jsonl")
}

fn worker_exists(home: &Path, name: &str) -> bool {
    home.join("sessions").join(format!("{name}.env")).exists()
}

/// Append one rename record. Called after a rename has fully succeeded.
pub(crate) fn record(home: &Path, old: &str, new: &str) -> std::io::Result<()> {
    if old == new
        || !super::session_verbs::valid_session_name(old)
        || !super::session_verbs::valid_session_name(new)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("not a recordable rename: {old:?} -> {new:?}"),
        ));
    }
    let line = json!({"old": old, "new": new, "at": chrono::Utc::now().timestamp()});
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(record_path(home))?;
    // One write per line keeps concurrent appends from interleaving.
    f.write_all(format!("{line}\n").as_bytes())
}

/// old -> new, the newest record per old name (later lines win).
fn load_records(home: &Path) -> HashMap<String, String> {
    let Ok(text) = std::fs::read_to_string(record_path(home)) else {
        return HashMap::new();
    };
    let mut map = HashMap::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if let (Some(old), Some(new)) = (v["old"].as_str(), v["new"].as_str()) {
            map.insert(old.to_string(), new.to_string());
        }
    }
    map
}

/// The name the worker once called `name` holds now, or None when `name` is
/// held by a live worker, was never renamed, or its chain reaches no live
/// worker. The live check runs first, so the common case reads no record.
pub(crate) fn current_name(home: &Path, name: &str) -> Option<String> {
    if !super::session_verbs::valid_session_name(name) || worker_exists(home, name) {
        return None;
    }
    let records = load_records(home);
    let mut seen = HashSet::from([name.to_string()]);
    let mut cur = name.to_string();
    while let Some(next) = records.get(&cur) {
        if !seen.insert(next.clone()) {
            tracing::warn!(name, at = %next, verdict = "rename_alias_cycle", measured = true,
                n_considered = seen.len(),
                "rename records form a cycle with no live worker; the identity is left unchanged");
            return None;
        }
        if worker_exists(home, next) {
            return Some(next.clone());
        }
        cur = next.clone();
    }
    None
}

/// Middleware: rewrite a renamed worker's identity headers to its current name.
pub async fn resolve_headers(mut req: Request, next: Next) -> Response {
    let home = super::session_verbs::home();
    for header in IDENTITY_HEADERS {
        let Some(claimed) = req
            .headers()
            .get(header)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
        else {
            continue;
        };
        let Some(current) = current_name(&home, &claimed) else { continue };
        let Ok(value) = HeaderValue::from_str(&current) else { continue };
        tracing::info!(from = %claimed, to = %current, header, path = %req.uri().path(),
            verdict = "renamed_identity_resolved", measured = true, n_considered = 1,
            "a request claimed a renamed worker's old name; attributed to its current name");
        req.headers_mut().insert(header, value);
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home_with(live: &[&str]) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("sessions")).unwrap();
        for w in live {
            std::fs::write(d.path().join(format!("sessions/{w}.env")), "CC_TAGS=dev\n").unwrap();
        }
        d
    }

    #[test]
    fn a_chain_resolves_to_the_live_holder() {
        let d = home_with(&["c"]);
        record(d.path(), "a", "b").unwrap();
        record(d.path(), "b", "c").unwrap();
        assert_eq!(current_name(d.path(), "a").as_deref(), Some("c"));
        assert_eq!(current_name(d.path(), "b").as_deref(), Some("c"));
        assert_eq!(current_name(d.path(), "c"), None, "a live name is never redirected");
    }

    #[test]
    fn a_live_worker_holding_an_old_name_wins_over_the_alias() {
        let d = home_with(&["b", "a"]);
        record(d.path(), "a", "b").unwrap();
        assert_eq!(current_name(d.path(), "a"), None, "a new worker named `a` keeps its name");
    }

    #[test]
    fn a_cycle_or_a_dead_end_leaves_the_name_unchanged() {
        let d = home_with(&[]);
        record(d.path(), "a", "b").unwrap();
        record(d.path(), "b", "a").unwrap();
        assert_eq!(current_name(d.path(), "a"), None);
        assert_eq!(current_name(d.path(), "never-renamed"), None);
        // Renaming back makes the original name live again: the other resolves to it.
        std::fs::write(d.path().join("sessions/a.env"), "").unwrap();
        assert_eq!(current_name(d.path(), "b").as_deref(), Some("a"));
    }

    #[test]
    fn invalid_names_are_neither_recorded_nor_resolved() {
        let d = home_with(&["ok"]);
        assert!(record(d.path(), "../x", "ok").is_err());
        assert!(record(d.path(), "same", "same").is_err());
        assert_eq!(current_name(d.path(), "../sessions/ok"), None);
        assert_eq!(current_name(d.path(), "member:someone@example.com"), None);
        std::fs::write(record_path(d.path()), "not json\n{\"old\":\"z\"}\n").unwrap();
        assert_eq!(current_name(d.path(), "z"), None, "malformed lines are skipped");
    }
}
