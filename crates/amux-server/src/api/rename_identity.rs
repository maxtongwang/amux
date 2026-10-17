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
//! - A worker that held a name and then leaves it (deleted, or expired by
//!   retirement) drops that name's records ([`forget`]): the ones redirecting
//!   the name (an earlier holder's) and, transitively, the aliases resolving
//!   into it (its own). Without this, a new `a` created after a -> b and later
//!   deleted re-activated the old record, and a new worker created under a
//!   released `b` inherited `a`'s headers and card callbacks.
//!   The accepted cost: if the original `a`'s process (now `b`) still claims
//!   `a` after that, the claim is no longer resolved. That process was told
//!   its new name (the rename notice and a server-delivered `/rename`), and it
//!   relaunches under the new name.
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

/// Serialises writers of the record file (the append and the [`forget`]
/// rewrite), so a rewrite cannot drop a line appended while it ran.
static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
    let _w = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(record_path(home))?;
    // One write per line keeps concurrent appends from interleaving.
    f.write_all(format!("{line}\n").as_bytes())
}

/// Drop the rename records tied to the worker that is releasing `name`
/// (delete or expiry), in both directions:
///
/// - OUTGOING: records with `old == name`. They describe an EARLIER holder of
///   the name, and must not redirect it once the current holder leaves.
/// - INCOMING, transitively: records whose `new` is `name`, or is an old name
///   that already resolves into it (a -> b -> c: releasing c drops b -> c and
///   a -> b). Those aliases belonged to this worker incarnation. Left in place,
///   a later, unrelated worker created under `name` would inherit them
///   (review of 3f5ab3df, B1): a's lingering headers and card callbacks would
///   attach to the new holder.
///
/// Returns how many records were dropped. Runs under the writer lock; the
/// rewrite is atomic (temp file + rename) and keeps every other line byte for
/// byte.
pub(crate) fn forget(home: &Path, name: &str) -> std::io::Result<usize> {
    let _w = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = record_path(home);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let pair = |line: &str| -> Option<(String, String)> {
        let v = serde_json::from_str::<Value>(line.trim_end()).ok()?;
        Some((v["old"].as_str()?.to_string(), v["new"].as_str()?.to_string()))
    };
    // The redirect each old name follows today: its newest record, exactly as
    // `current_name` reads it.
    let newest: HashMap<String, String> = text.lines().filter_map(pair).collect();
    // `name` and every old name whose chain reaches it.
    let mut released = HashSet::from([name.to_string()]);
    loop {
        let before = released.len();
        for (old, new) in &newest {
            if released.contains(new) {
                released.insert(old.clone());
            }
        }
        if released.len() == before {
            break;
        }
    }
    let mut dropped = 0;
    let kept: String = text
        .split_inclusive('\n')
        .filter(|line| {
            let tied = pair(line).is_some_and(|(old, _)| released.contains(&old));
            dropped += usize::from(tied);
            !tied
        })
        .collect();
    if dropped == 0 {
        return Ok(0);
    }
    let tmp = path.with_extension(format!("jsonl.{}.tmp", std::process::id()));
    std::fs::write(&tmp, kept)
        .and_then(|_| std::fs::rename(&tmp, &path))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })?;
    Ok(dropped)
}

/// [`forget`], logged. Called by every path that releases a worker's name.
pub(crate) fn forget_logged(home: &Path, name: &str) {
    match forget(home, name) {
        Ok(0) => {}
        Ok(n) => tracing::info!(name, dropped = n, verdict = "rename_records_forgotten", measured = true,
            n_considered = n, "a worker released its name; earlier holders' rename records for it were dropped"),
        Err(e) => tracing::warn!(name, error = %e, verdict = "rename_records_forget_failed", measured = true,
            n_considered = 1, "a worker released its name but its rename records could not be dropped; \
             a stale record can redirect that name"),
    }
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

    /// Note 1 variant (review of c003491): a -> b, then a new `a` is created
    /// and released. The released name must not start resolving to `b`.
    #[test]
    fn a_released_name_stops_resolving_to_an_earlier_holder() {
        let d = home_with(&["b", "x"]);
        record(d.path(), "a", "b").unwrap();
        record(d.path(), "q", "x").unwrap();
        std::fs::write(d.path().join("sessions/a.env"), "").unwrap();
        assert_eq!(current_name(d.path(), "a"), None, "the new `a` is live");
        std::fs::remove_file(d.path().join("sessions/a.env")).unwrap();
        assert_eq!(forget(d.path(), "a").unwrap(), 1);
        assert_eq!(current_name(d.path(), "a"), None, "a released name resolves nowhere");
        assert_eq!(current_name(d.path(), "q").as_deref(), Some("x"), "other records are kept");
        assert_eq!(forget(d.path(), "a").unwrap(), 0, "idempotent");
        let none = home_with(&[]);
        assert_eq!(forget(none.path(), "a").unwrap(), 0, "no record file");
    }

    /// Review of 3f5ab3df, B1: releasing the END of a chain drops every alias
    /// that resolves into it, transitively, and nothing else.
    #[test]
    fn releasing_an_alias_target_drops_the_chain_into_it() {
        let d = home_with(&["c", "x"]);
        record(d.path(), "a", "b").unwrap();
        record(d.path(), "b", "c").unwrap();
        record(d.path(), "q", "x").unwrap();
        assert_eq!(current_name(d.path(), "a").as_deref(), Some("c"));
        std::fs::remove_file(d.path().join("sessions/c.env")).unwrap();
        assert_eq!(forget(d.path(), "c").unwrap(), 2, "b -> c and a -> b");
        std::fs::write(d.path().join("sessions/c.env"), "").unwrap();
        assert_eq!(current_name(d.path(), "a"), None, "a new c inherits nothing");
        assert_eq!(current_name(d.path(), "b"), None);
        assert_eq!(current_name(d.path(), "q").as_deref(), Some("x"), "unrelated chain kept");
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
