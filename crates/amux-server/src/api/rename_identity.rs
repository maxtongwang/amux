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
//! every successful rename appends `{old, new, inc, at}` to
//! `~/.amux/renames.jsonl`, and [`resolve_headers`] rewrites a worker header
//! that names a worker which no longer exists to the name that worker holds
//! now. Every handler that reads the headers (sends, the group boundary, board
//! ownership, the rule-10 token check) and card callbacks
//! (`board::callback_target`) then see the current name.
//!
//! ALIASES ARE BOUND TO A WORKER INCARNATION, NOT TO NAMES. Three review
//! rounds found three defects in name-graph cleanup (a released name
//! re-activating an old record, a new holder of a released target inheriting
//! its aliases, and the reverse walk dropping a live intermediate holder's
//! aliases), so names are no longer the identity:
//!
//! - A worker's incarnation is a random id in its env file
//!   ([`INCARNATION_KEY`]). It moves with the env file on rename, is assigned
//!   on the worker's first rename if it has none, and is stripped from a
//!   duplicated env file ([`strip_incarnation`]), so two workers never share
//!   one. A worker created under a reused name has a new env file and so a
//!   different (or no) incarnation.
//! - Every record carries the renamed worker's incarnation (`inc`).
//! - Resolution: a live name is never redirected. Otherwise the newest record
//!   whose `old` is the claimed name names an incarnation; its newest record
//!   names that incarnation's current name, which is returned only when the
//!   worker there now carries the SAME incarnation. No chain walk.
//! - Release (delete, retirement expiry) drops exactly the records carrying
//!   the released worker's incarnation ([`release`]). No graph traversal. If
//!   an earlier holder's rename away from the released name is still on file,
//!   a release marker is appended for that name: it ends that name's redirect
//!   (a deleted re-created `a` must not send its callbacks to the original
//!   `a`, now `b`) and touches no incarnation's records. The accepted cost:
//!   the original worker's process, if it still claims that name, is no
//!   longer resolved; it was told its new name and relaunches under it.
//! - Legacy records without `inc` are migrated at startup and before any identity
//!   read or mutation: given the
//!   current live holder's incarnation when the old chain reaches one
//!   unambiguously, dropped otherwise (`rename_records_migrated`, and a WARN
//!   with the dropped count).
//! - Known limit: if a name is renamed away by two different workers (a -> b,
//!   then a new `a` is created and renamed a -> c), a process still claiming
//!   `a` resolves to the newest one, `c`.
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

/// The env-file key holding a worker's incarnation id.
pub(crate) const INCARNATION_KEY: &str = "CC_INCARNATION";

fn record_path(home: &Path) -> PathBuf {
    home.join("renames.jsonl")
}

fn env_file(home: &Path, name: &str) -> PathBuf {
    home.join("sessions").join(format!("{name}.env"))
}

/// Serialises writers of the record file (append, release rewrite, legacy
/// migration), so a rewrite cannot drop a line appended while it ran.
static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn worker_exists(home: &Path, name: &str) -> bool {
    env_file(home, name).exists()
}

/// The incarnation recorded in an env file, if any.
fn incarnation_in(path: &Path) -> Option<String> {
    let cfg = super::session_verbs::EnvFile::load(path);
    cfg.get(INCARNATION_KEY)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(String::from)
}

/// A live worker's incarnation, if it has one.
pub(crate) fn incarnation(home: &Path, name: &str) -> Option<String> {
    incarnation_in(&env_file(home, name))
}

fn new_incarnation() -> std::io::Result<String> {
    use std::io::Read;
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// The worker's incarnation, assigning one (written to its env file) if it
/// has none. A worker is one incarnation for the life of its env file.
pub(crate) fn ensure_incarnation(home: &Path, name: &str) -> std::io::Result<String> {
    ensure_migrated(home)?;
    assign_incarnation(home, name)
}

// Migration already holds WRITE_LOCK; do not recurse through its public boundary.
fn assign_incarnation(home: &Path, name: &str) -> std::io::Result<String> {
    let path = env_file(home, name);
    if !path.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("no worker '{name}'"),
        ));
    }
    if let Some(id) = incarnation_in(&path) {
        return Ok(id);
    }
    let id = new_incarnation()?;
    // APPENDED, so every other byte of the env file (its comments, order and
    // line endings) stays as it was, and the file keeps its mode (0600).
    let current = std::fs::read(&path)?;
    let lead = if current.is_empty() || current.ends_with(b"\n") {
        ""
    } else {
        "\n"
    };
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)?
        .write_all(format!("{lead}{INCARNATION_KEY}=\"{id}\"\n").as_bytes())?;
    // A concurrent whole-file env rewrite can lose the append; say so rather
    // than hand out an id no env file carries.
    if incarnation_in(&path).as_deref() != Some(id.as_str()) {
        return Err(std::io::Error::other(format!(
            "the incarnation for '{name}' did not persist"
        )));
    }
    tracing::info!(
        session = name,
        verdict = "worker_incarnation_assigned",
        measured = true,
        n_considered = 1,
        "a worker was given its incarnation id"
    );
    Ok(id)
}

/// Remove the incarnation from a freshly duplicated env file: the copy is a
/// different worker and must never carry the source's aliases.
pub(crate) fn strip_incarnation(path: &Path) -> std::io::Result<()> {
    let mut cfg = super::session_verbs::EnvFile::load(path);
    if cfg.get(INCARNATION_KEY).is_none() {
        return Ok(());
    }
    cfg.remove(INCARNATION_KEY);
    cfg.write(path)
}

/// One line of the record file: a rename (`new` and, once migrated, `inc`),
/// or a release marker (`released`, no `new`) saying the name `old` was
/// released by its holder, so earlier holders' renames away from it no longer
/// redirect it.
#[derive(Debug, Clone)]
struct Rec {
    old: String,
    new: Option<String>,
    inc: Option<String>,
    released: bool,
}

impl Rec {
    fn legacy(&self) -> bool {
        !self.released && self.new.is_some() && self.inc.is_none()
    }
}

fn parse(line: &str) -> Option<Rec> {
    let v = serde_json::from_str::<Value>(line.trim_end()).ok()?;
    let rec = Rec {
        old: v["old"].as_str()?.to_string(),
        new: v["new"].as_str().map(String::from),
        inc: v["inc"].as_str().filter(|s| !s.is_empty()).map(String::from),
        released: v["released"].as_bool() == Some(true),
    };
    (rec.released || rec.new.is_some()).then_some(rec)
}

/// Append one rename record for incarnation `inc`. Called after a rename has
/// fully succeeded.
pub(crate) fn record(home: &Path, old: &str, new: &str, inc: &str) -> std::io::Result<()> {
    ensure_migrated(home)?;
    if old == new
        || inc.is_empty()
        || !super::session_verbs::valid_session_name(old)
        || !super::session_verbs::valid_session_name(new)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("not a recordable rename: {old:?} -> {new:?}"),
        ));
    }
    let line = json!({"old": old, "new": new, "inc": inc, "at": chrono::Utc::now().timestamp()});
    let _w = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(record_path(home))?;
    // One write per line keeps concurrent appends from interleaving.
    f.write_all(format!("{line}\n").as_bytes())
}

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("jsonl.{}.tmp", std::process::id()));
    std::fs::write(&tmp, text)
        .and_then(|_| std::fs::rename(&tmp, path))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
}

/// Release `name` held by incarnation `inc` (None for a worker that was never
/// renamed): drop exactly the records carrying `inc`, then, when an earlier
/// holder's rename away from `name` is still on file, append a release marker
/// for `name` so that name no longer redirects to that earlier holder (a
/// deleted re-created `a` must not send a's callbacks to the original `a`,
/// now `b`). The marker touches no incarnation's records, so every other
/// worker's aliases, including a live intermediate holder's, are unaffected.
/// Returns (records dropped, marker written).
pub(crate) fn release_incarnation(home: &Path, name: &str, inc: Option<&str>) -> std::io::Result<(usize, bool)> {
    ensure_migrated(home)?;
    let migrated_inc = incarnation(home, name);
    let inc = inc.or(migrated_inc.as_deref());
    let _w = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = record_path(home);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((0, false)),
        Err(e) => return Err(e),
    };
    let mut dropped = 0;
    let mut kept: String = text
        .split_inclusive('\n')
        .filter(|line| {
            let tied = inc.is_some() && parse(line).is_some_and(|r| r.inc.as_deref() == inc);
            dropped += usize::from(tied);
            !tied
        })
        .collect();
    // Only the newest line about `name` decides whether it still redirects.
    let redirects = kept
        .lines()
        .rev()
        .filter_map(parse)
        .find(|r| r.old == name)
        .is_some_and(|r| !r.released);
    if redirects {
        if !kept.is_empty() && !kept.ends_with('\n') {
            kept.push('\n');
        }
        kept.push_str(&format!(
            "{}\n",
            json!({"old": name, "released": true, "at": chrono::Utc::now().timestamp()})
        ));
    }
    if dropped > 0 || redirects {
        write_atomic(&path, &kept)?;
    }
    Ok((dropped, redirects))
}

/// A worker is releasing `name` (delete, or retirement expiry): its
/// incarnation is read from its env file, or the `.env.reaped` copy that
/// retirement leaves. Call before a delete removes the env file. Logged; a
/// failure leaves records that the incarnation check still keeps away from
/// any later holder of the name.
pub(crate) fn release(home: &Path, name: &str) {
    if ensure_migrated(home).is_err() {
        return;
    }
    let active = env_file(home, name);
    let inc = incarnation_in(&active).or_else(|| incarnation_in(&active.with_extension("env.reaped")));
    match release_incarnation(home, name, inc.as_deref()) {
        Ok((0, false)) => {}
        Ok((n, marked)) => tracing::info!(
            name,
            dropped = n,
            marked,
            verdict = "rename_records_released",
            measured = true,
            n_considered = n,
            "a worker released its name; its incarnation's rename records were dropped"
        ),
        Err(e) => {
            tracing::warn!(name, error = %e, verdict = "rename_records_release_failed", measured = true,
            n_considered = 1, "a worker released its name but its rename records could not be updated")
        }
    }
}

/// Follow mixed history to a live holder. Release markers, cycles and a
/// current record whose incarnation is no longer live are hard boundaries.
fn legacy_holder(home: &Path, newest: &HashMap<String, Rec>, records: &[Rec], start: &str) -> Option<String> {
    let mut seen = HashSet::new();
    let mut cur = start.to_string();
    loop {
        if !super::session_verbs::valid_session_name(&cur) || !seen.insert(cur.clone()) {
            return None;
        }
        let rec = newest.get(&cur)?;
        if rec.released {
            return None;
        }
        if let Some(inc) = rec.inc.as_deref() {
            let holder = records
                .iter()
                .rev()
                .find(|r| r.inc.as_deref() == Some(inc))?
                .new
                .as_deref()?;
            return (super::session_verbs::valid_session_name(holder)
                && incarnation(home, holder).as_deref() == Some(inc))
            .then(|| holder.to_string());
        }
        cur = rec.new.clone()?;
        if !super::session_verbs::valid_session_name(&cur) {
            return None;
        }
        if worker_exists(home, &cur) {
            return Some(cur);
        }
    }
}

/// Settle legacy ownership once, before any lifecycle mutation. The version
/// marker is published last: interruption leaves an idempotently retryable
/// rewrite, never a marker claiming unfinished migration succeeded.
pub(crate) fn ensure_migrated(home: &Path) -> std::io::Result<()> {
    let result = (|| {
        let _w = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let marker = home.join("renames.version");
        match std::fs::read_to_string(&marker) {
            Ok(v) if v == "1\n" => return Ok(()),
            Ok(_) => return Err(std::io::Error::other("unsupported rename records version")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        std::fs::create_dir_all(home)?;
        let path = record_path(home);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };
        let records: Vec<Rec> = text.lines().filter_map(parse).collect();
        let legacy: Vec<&Rec> = records.iter().filter(|r| r.legacy()).collect();
        let newest: HashMap<String, Rec> = records.iter().map(|r| (r.old.clone(), r.clone())).collect();
        let (mut migrated, mut dropped) = (0usize, 0usize);
        let mut out = String::with_capacity(text.len());
        for line in text.split_inclusive('\n') {
            let Some(rec) = parse(line) else {
                out.push_str(line);
                continue;
            };
            if !rec.legacy() {
                out.push_str(line);
                continue;
            }
            let inc = match legacy_holder(home, &newest, &records, &rec.old) {
                Some(holder) => Some(assign_incarnation(home, &holder)?),
                None => None,
            };
            match inc {
                Some(inc) => {
                    let mut v: Value = serde_json::from_str(line.trim_end()).unwrap_or_else(|_| json!({}));
                    v["inc"] = json!(inc);
                    out.push_str(&format!("{v}\n"));
                    migrated += 1;
                }
                None => dropped += 1,
            }
        }
        if !legacy.is_empty() {
            write_atomic(&path, &out)?;
            tracing::info!(
                migrated,
                dropped,
                verdict = "rename_records_migrated",
                measured = true,
                n_considered = legacy.len(),
                "legacy rename records were bound to their live holder's incarnation"
            );
            if dropped > 0 {
                tracing::warn!(
                    dropped,
                    verdict = "rename_records_legacy_dropped",
                    measured = true,
                    n_considered = legacy.len(),
                    "legacy rename records reached no live holder unambiguously and were dropped"
                );
            }
        }
        write_atomic(&marker, "1\n")
    })();
    if let Err(ref e) = result {
        tracing::warn!(error = %e, verdict = "rename_records_migration_failed", measured = true,
            n_considered = 1, "rename identity operation blocked until legacy migration succeeds");
    }
    result
}

fn load(home: &Path) -> Vec<Rec> {
    std::fs::read_to_string(record_path(home))
        .unwrap_or_default()
        .lines()
        .filter_map(parse)
        .filter(|r| !r.legacy())
        .collect()
}

/// The name the worker once called `name` holds now, or None when `name` is
/// held by a live worker, was never renamed, or the worker it named is gone
/// (a worker now under its last name is a different incarnation). The live
/// check runs first, so the common case reads no record.
pub(crate) fn current_name(home: &Path, name: &str) -> Option<String> {
    ensure_migrated(home).ok()?;
    if !super::session_verbs::valid_session_name(name) || worker_exists(home, name) {
        return None;
    }
    let records = load(home);
    // The newest line about the claimed name: a release marker ends it.
    let inc = records.iter().rev().find(|r| r.old == name)?.inc.clone()?;
    let now = records
        .iter()
        .rev()
        .find(|r| r.inc.as_deref() == Some(inc.as_str()))?
        .new
        .clone()?;
    if now == name || !worker_exists(home, &now) {
        return None;
    }
    if incarnation(home, &now).as_deref() != Some(inc.as_str()) {
        tracing::info!(claimed = name, holder = %now, verdict = "rename_alias_other_incarnation", measured = true,
            n_considered = 1, "a renamed worker's last name is held by a different worker; the alias is not followed");
        return None;
    }
    Some(now)
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
pub(crate) mod tests {
    use super::*;

    /// The number of records of incarnation `inc` on disk.
    pub(crate) fn records_of(home: &Path, inc: &str) -> usize {
        std::fs::read_to_string(record_path(home))
            .unwrap_or_default()
            .lines()
            .filter_map(parse)
            .filter(|r| r.inc.as_deref() == Some(inc))
            .count()
    }

    fn home_with(live: &[&str]) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("sessions")).unwrap();
        for w in live {
            live_worker(d.path(), w);
        }
        d
    }

    fn live_worker(home: &Path, name: &str) {
        std::fs::write(env_file(home, name), "CC_TAGS=dev\n").unwrap();
    }

    /// Simulate one rename the way rename_session does it: the env file moves
    /// (keeping the incarnation), then the record is written.
    fn rename(home: &Path, old: &str, new: &str) -> String {
        let inc = ensure_incarnation(home, old).unwrap();
        std::fs::rename(env_file(home, old), env_file(home, new)).unwrap();
        record(home, old, new, &inc).unwrap();
        inc
    }

    async fn assert_consumers(home: &Path, expected: &str) {
        use axum::{body::Body, http::Request, routing::get, Router};
        use tower::ServiceExt;
        let _home = crate::api::settings::test_env::set_home(home);
        let app = Router::new()
            .route(
                "/who",
                get(|h: axum::http::HeaderMap| async move {
                    format!(
                        "{}:{}",
                        h["x-amux-worker"].to_str().unwrap(),
                        h["x-amux-session"].to_str().unwrap()
                    )
                }),
            )
            .layer(axum::middleware::from_fn(resolve_headers));
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/who")
                    .header("x-amux-worker", "review-a")
                    .header("x-amux-session", "review-a")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let header = String::from_utf8(body.to_vec()).unwrap();
        let callback = crate::api::board::callback_target(home, "review-a".into());
        assert_eq!(
            (header, callback),
            (format!("{expected}:{expected}"), expected.to_string())
        );
    }

    fn legacy_fixture() -> tempfile::TempDir {
        let d = home_with(&["review-b"]);
        std::fs::write(record_path(d.path()), "{\"old\":\"review-a\",\"new\":\"review-b\"}\n").unwrap();
        d
    }

    #[tokio::test]
    async fn release_before_first_legacy_read_does_not_rebind() {
        let d = legacy_fixture();
        release(d.path(), "review-b");
        std::fs::remove_file(env_file(d.path(), "review-b")).unwrap();
        live_worker(d.path(), "review-b");
        assert_consumers(d.path(), "review-a").await;
    }

    #[tokio::test]
    async fn rename_before_first_legacy_read_keeps_alias() {
        let d = legacy_fixture();
        // The real rename entry point must migrate BEFORE moving the env.
        // ensure_incarnation is also an identity mutation boundary.
        rename(d.path(), "review-b", "review-c");
        assert_consumers(d.path(), "review-c").await;
    }

    #[tokio::test]
    async fn retirement_before_first_legacy_read_does_not_rebind() {
        let d = legacy_fixture();
        // Retirement releases the .env.reaped copy after expiry.
        std::fs::rename(
            env_file(d.path(), "review-b"),
            env_file(d.path(), "review-b").with_extension("env.reaped"),
        )
        .unwrap();
        release(d.path(), "review-b");
        live_worker(d.path(), "review-b");
        assert_consumers(d.path(), "review-a").await;
    }

    #[test]
    fn legacy_migration_is_idempotent() {
        let d = legacy_fixture();
        // A prior partial upgrade already wrote a current edge after a legacy
        // one. Migration must follow both formats to the same live incarnation.
        std::fs::rename(env_file(d.path(), "review-b"), env_file(d.path(), "review-c")).unwrap();
        std::fs::write(env_file(d.path(), "review-c"), "CC_INCARNATION=mixed-inc\n").unwrap();
        std::fs::OpenOptions::new().append(true).open(record_path(d.path())).unwrap()
            .write_all(b"{\"old\":\"review-b\",\"new\":\"review-c\",\"inc\":\"mixed-inc\"}\n").unwrap();
        ensure_migrated(d.path()).unwrap();
        assert_eq!(current_name(d.path(), "review-a").as_deref(), Some("review-c"));
        assert_eq!(current_name(d.path(), "review-b").as_deref(), Some("review-c"));
        let once = std::fs::read(record_path(d.path())).unwrap();
        let env = std::fs::read(env_file(d.path(), "review-c")).unwrap();
        let marker = d.path().join("renames.version");
        assert_eq!(std::fs::read_to_string(&marker).unwrap_or_default(), "1\n");
        let version_time = std::fs::metadata(&marker).unwrap().modified().unwrap();
        ensure_migrated(d.path()).unwrap();
        assert_eq!(std::fs::metadata(&marker).unwrap().modified().unwrap(), version_time);
        assert_eq!(std::fs::read(record_path(d.path())).unwrap(), once);
        assert_eq!(std::fs::read(env_file(d.path(), "review-c")).unwrap(), env);
    }

    #[test]
    fn a_chain_resolves_to_the_live_holder() {
        let d = home_with(&["a"]);
        rename(d.path(), "a", "b");
        rename(d.path(), "b", "c");
        assert_eq!(current_name(d.path(), "a").as_deref(), Some("c"));
        assert_eq!(current_name(d.path(), "b").as_deref(), Some("c"));
        assert_eq!(current_name(d.path(), "c"), None, "a live name is never redirected");
    }

    /// Name reuse at every hop: a new worker under any old name keeps it, and
    /// the original worker's other old names still reach it.
    #[test]
    fn a_new_worker_under_any_old_name_keeps_it() {
        let d = home_with(&["a"]);
        rename(d.path(), "a", "b");
        rename(d.path(), "b", "c");
        live_worker(d.path(), "a");
        assert_eq!(current_name(d.path(), "a"), None, "the new a keeps its name");
        assert_eq!(current_name(d.path(), "b").as_deref(), Some("c"));
        live_worker(d.path(), "b");
        assert_eq!(current_name(d.path(), "b"), None, "the new b keeps its name");
        std::fs::remove_file(env_file(d.path(), "a")).unwrap();
        assert_eq!(current_name(d.path(), "a").as_deref(), Some("c"), "a still reaches c past the live new b");
    }

    /// The original review case: a -> b, b is gone, a different worker takes
    /// b. Even with the records still on disk (no release ran), a must not
    /// attach to the new b: it is another incarnation.
    #[test]
    fn a_new_holder_of_the_last_name_is_another_incarnation() {
        let d = home_with(&["a"]);
        let inc = rename(d.path(), "a", "b");
        std::fs::remove_file(env_file(d.path(), "b")).unwrap();
        live_worker(d.path(), "b");
        assert_eq!(records_of(d.path(), &inc), 1, "the record is still there");
        assert_eq!(current_name(d.path(), "a"), None);
    }

    /// Codex's case on 99054a99: a -> b -> c, then q is renamed to the freed b,
    /// then c is released. Only c's incarnation's records go; q keeps b.
    #[test]
    fn releasing_a_worker_drops_only_its_incarnation() {
        let d = home_with(&["a", "q"]);
        let x = rename(d.path(), "a", "b");
        rename(d.path(), "b", "c");
        let y = rename(d.path(), "q", "b");
        assert_eq!(current_name(d.path(), "q").as_deref(), Some("b"));
        assert_eq!(current_name(d.path(), "a").as_deref(), Some("c"));
        release(d.path(), "c");
        std::fs::remove_file(env_file(d.path(), "c")).unwrap();
        assert_eq!(records_of(d.path(), &x), 0, "c's incarnation's records are gone");
        assert_eq!(records_of(d.path(), &y), 1, "q's record is kept");
        assert_eq!(current_name(d.path(), "q").as_deref(), Some("b"), "q still resolves to the live b");
        assert_eq!(current_name(d.path(), "a"), None);
        live_worker(d.path(), "c");
        assert_eq!(current_name(d.path(), "a"), None, "a new c inherits nothing");
    }

    /// A reused name released while the original worker lives on elsewhere:
    /// the reused name stops redirecting (the deleted re-created worker's
    /// callbacks must not reach the original), and the original's chain and
    /// every other alias are untouched.
    #[test]
    fn releasing_a_reused_name_ends_its_redirect_and_nothing_else() {
        let d = home_with(&["a", "q"]);
        rename(d.path(), "a", "b");
        rename(d.path(), "b", "c");
        // A new worker q takes the freed b, then is deleted.
        let y = rename(d.path(), "q", "b");
        release(d.path(), "b");
        std::fs::remove_file(env_file(d.path(), "b")).unwrap();
        assert_eq!(records_of(d.path(), &y), 0);
        assert_eq!(current_name(d.path(), "b"), None, "the released b no longer redirects");
        assert_eq!(current_name(d.path(), "q"), None);
        assert_eq!(current_name(d.path(), "a").as_deref(), Some("c"), "the original chain is untouched");
        // A never-renamed worker re-created under a and deleted.
        live_worker(d.path(), "a");
        release(d.path(), "a");
        std::fs::remove_file(env_file(d.path(), "a")).unwrap();
        assert_eq!(current_name(d.path(), "a"), None, "the deleted a's callbacks do not reach c");
        release(d.path(), "a");
        let markers = std::fs::read_to_string(record_path(d.path()))
            .unwrap()
            .lines()
            .filter_map(parse)
            .filter(|r| r.released && r.old == "a")
            .count();
        assert_eq!(markers, 1, "one marker; releasing an already released name adds none");
    }

    #[test]
    fn a_duplicate_does_not_carry_the_incarnation() {
        let d = home_with(&["a"]);
        rename(d.path(), "a", "b");
        std::fs::copy(env_file(d.path(), "b"), env_file(d.path(), "dup")).unwrap();
        strip_incarnation(&env_file(d.path(), "dup")).unwrap();
        assert_eq!(incarnation(d.path(), "dup"), None);
        assert!(incarnation(d.path(), "b").is_some());
    }

    /// Legacy records (no `inc`) are bound to their live holder's incarnation
    /// when the old chain reaches one, and dropped otherwise.
    #[test]
    fn legacy_records_migrate_to_the_live_holders_incarnation() {
        let d = home_with(&["c"]);
        std::fs::write(
            record_path(d.path()),
            "{\"old\":\"a\",\"new\":\"b\"}\n{\"old\":\"b\",\"new\":\"c\"}\n{\"old\":\"m\",\"new\":\"gone\"}\nnot json\n",
        )
        .unwrap();
        assert_eq!(current_name(d.path(), "a").as_deref(), Some("c"));
        let inc = incarnation(d.path(), "c").expect("the holder got an incarnation");
        assert_eq!(records_of(d.path(), &inc), 2, "a -> b and b -> c bound to c");
        assert_eq!(current_name(d.path(), "m"), None, "a dead-end legacy record is dropped");
        let text = std::fs::read_to_string(record_path(d.path())).unwrap();
        assert!(!text.contains("\"gone\""), "{text}");
        assert!(text.contains("not json"), "unparseable lines are left alone");
        // The holder is later replaced: the migrated aliases do not follow.
        std::fs::remove_file(env_file(d.path(), "c")).unwrap();
        live_worker(d.path(), "c");
        assert_eq!(current_name(d.path(), "a"), None);
    }

    #[test]
    fn invalid_names_are_neither_recorded_nor_resolved() {
        let d = home_with(&["ok"]);
        assert!(record(d.path(), "../x", "ok", "i").is_err());
        assert!(record(d.path(), "same", "same", "i").is_err());
        assert!(record(d.path(), "a", "ok", "").is_err());
        assert_eq!(current_name(d.path(), "../sessions/ok"), None);
        assert_eq!(current_name(d.path(), "member:someone@example.com"), None);
        assert_eq!(current_name(d.path(), "never-renamed"), None);
    }
}
