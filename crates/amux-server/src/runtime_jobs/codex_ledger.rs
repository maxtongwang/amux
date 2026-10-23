//! Codex turns in the token ledger (AMUX-4583).
//!
//! Nine lanes on this fleet run codex, and every token they spent was invisible
//! to `/api/usage`, `/api/observability` and the Cost tab: `translate_codex`
//! discards the usage on `turn.completed`, and `~/.codex/sessions` was read only
//! to decide whether a lane looked busy. A dashboard that shows the fleet's
//! spend while silently omitting a provider is the confident-zero shape the
//! token ledger itself was built to end.
//!
//! Codex writes the same thing Claude does, in a different place and shape:
//! `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`, one `event_msg` per
//! `token_count` carrying `info.last_token_usage` (the delta for that turn) and
//! `info.total_token_usage` (cumulative). Cumulative differences suppress repeated
//! events; prefix replay retains model/counter state across incremental passes.
//! Native ordinals (or byte boundaries when absent) keep replays idempotent.
//!
//! Attribution prefers a validated native CLI session ID plus its recorded cwd.
//! Directory-only fallback is allowed only when one worker owns the directory.
//! Project workers share a checkout, so its path alone cannot identify a turn.
use crate::db::SharedStore;
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// One codex turn's usage, as the ledger stores it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CodexTurn {
    pub ts: i64,
    /// The event's position in its own file: stable, so re-reading a file
    /// cannot bill the same turn twice.
    pub ordinal: i64,
    pub model: String,
    /// `[input, cache_read, cache_write, output]`, matching `token_ledger`.
    pub tokens: [i64; 4],
    pub compaction: Option<String>,
}

/// `~/.codex/sessions`, from the OS home. Codex writes here, NOT into amux's
/// own home; `codex_rollout_files` records what pointing at the wrong one cost.
fn codex_sessions_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".codex/sessions")
}

/// Every `rollout-*.jsonl` under `root`, depth-bounded like the transcript walk.
fn rollout_files(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth > 3 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, depth + 1, out);
            } else if p
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.starts_with("rollout-") && s.ends_with(".jsonl"))
            {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, 0, &mut out);
    out
}

/// The conversation key for a rollout file.
///
/// Prefixed `codex:` on purpose. Both providers name conversations with a
/// UUID, and the ledger's cursor table is keyed by that string alone, so an
/// unprefixed collision would let one provider's cursor skip the other's file.
/// It also makes a codex row identifiable in the ledger without a join.
pub(crate) fn conversation_key(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    // rollout-2026-09-09T14-12-21-<uuid>
    let id = stem
        .rsplit_once("-")
        .map(|(_, tail)| tail.to_string())
        .unwrap_or_default();
    let id = if id.len() >= 12 { id } else { stem.clone() };
    format!("codex:{id}")
}

/// The `cwd` a rollout file was opened in, from its `session_meta` first line.
fn rollout_identity(path: &Path) -> Option<serde_json::Value> {
    let f = std::fs::File::open(path).ok()?;
    let mut first = String::new();
    BufReader::new(f).read_line(&mut first).ok()?;
    let v: serde_json::Value = serde_json::from_str(&first).ok()?;
    v.get("payload").cloned()
}

/// Parse `token_count` deltas after `offset`.
///
/// Returns the new byte offset and one turn per event that reported any tokens.
/// A zero-token event is skipped rather than stored: codex emits one on turns
/// that spent nothing, and a ledger row of four zeros is noise a reader has to
/// explain.
pub(crate) fn parse_codex_from(path: &Path, offset: u64) -> (u64, Vec<CodexTurn>) {
    let Ok(f) = std::fs::File::open(path) else {
        return (offset, vec![]);
    };
    // Replay the prefix for cumulative/model state; emit only new records.
    // This is intentionally read-only: no historical response-ID rewrite.
    let mut new_off = 0;
    let mut cumulative: Option<[i64; 4]> = None;
    let mut seen_ordinals = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut model = String::new();
    let mut compaction = None;
    let mut compaction_offset = None;
    let mut reader = BufReader::new(f);
    loop {
        let mut bytes = Vec::new();
        let Ok(consumed) = reader.read_until(b'\n', &mut bytes) else {
            break;
        };
        if consumed == 0 {
            break;
        }
        let terminated = bytes.last() == Some(&b'\n');
        if terminated {
            bytes.pop();
        }
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
        let e = match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(e) => e,
            Err(_) if !terminated => break, // leave an unfinished JSON line for the next pass
            Err(_) => {
                new_off += consumed as u64;
                continue;
            }
        };
        new_off += consumed as u64;
        if e["type"] == "compacted"
            || e.pointer("/payload/type")
                .and_then(serde_json::Value::as_str)
                == Some("context_compacted")
        {
            compaction_offset = Some(new_off - consumed as u64);
            compaction=Some(serde_json::json!({"measured":true,"source":"codex_context_compacted","timestamp":e["timestamp"]}).to_string());
        }
        // The model is named by turn context events, not by `session_meta`.
        // Remember the most recent one: it is what the turns below were spent on.
        for key in ["/payload/model", "/model", "/payload/turn_context/model"] {
            if let Some(m) = e.pointer(key).and_then(serde_json::Value::as_str) {
                if !m.trim().is_empty() {
                    model = m.trim().to_string();
                }
            }
        }
        if e.pointer("/payload/type")
            .and_then(serde_json::Value::as_str)
            != Some("token_count")
        {
            continue;
        }
        let usage = |v: &serde_json::Value| -> [i64; 4] {
            let n = |k: &str| v[k].as_i64().unwrap_or(0).max(0);
            [
                n("input_tokens"),
                n("cached_input_tokens"),
                n("cache_write_input_tokens"),
                n("output_tokens"),
            ]
        };
        let total = e
            .pointer("/payload/info/total_token_usage")
            .filter(|v| v.is_object())
            .map(usage);
        let last = e
            .pointer("/payload/info/last_token_usage")
            .filter(|v| v.is_object())
            .map(usage);
        let raw = if let Some(total) = total {
            let previous = cumulative.unwrap_or([0; 4]);
            cumulative = Some(total);
            if total == previous {
                continue;
            }
            if total.iter().zip(previous).any(|(n, p)| *n < p) {
                tracing::warn!(
                    verdict = "codex_cumulative_reset",
                    measured = true,
                    n_considered = 1,
                    "cumulative token counters reset; starting a new counter epoch"
                );
                total
            } else {
                std::array::from_fn(|i| total[i] - previous[i])
            }
        } else if let Some(last) = last {
            tracing::debug!(
                verdict = "codex_cumulative_unavailable",
                measured = false,
                "using last usage; cumulative reconciliation unavailable"
            );
            last
        } else {
            continue;
        };
        if new_off <= offset {
            compaction = None;
            compaction_offset = None;
            continue;
        }
        let tokens = [(raw[0] - raw[1]).max(0), raw[1], raw[2], raw[3]];
        if tokens.iter().all(|t| *t == 0) {
            continue;
        }
        let Some(ts) = e
            .get("timestamp")
            .and_then(serde_json::Value::as_str)
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.timestamp())
        else {
            continue;
        };
        let ordinal = e
            .get("ordinal")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(new_off as i64);
        if !seen_ordinals.insert(ordinal) {
            continue;
        }
        out.push(CodexTurn {
            ts,
            ordinal,
            model: model.clone(),
            tokens,
            compaction: compaction.take(),
        });
        compaction_offset = None;
    }
    if let Some(boundary) = compaction_offset.filter(|b| *b >= offset) {
        new_off = new_off.min(boundary);
    }
    (new_off, out)
}

/// cwd -> the single lane that works there. A directory two lanes share is
/// dropped rather than guessed at.
pub(crate) fn unambiguous_owners(workdirs: &BTreeMap<String, String>) -> HashMap<String, String> {
    let mut by_dir: HashMap<String, Vec<String>> = HashMap::new();
    for (lane, dir) in workdirs {
        let dir = dir.trim().trim_end_matches('/').to_string();
        if dir.is_empty() {
            continue;
        }
        by_dir.entry(dir).or_default().push(lane.clone());
    }
    by_dir
        .into_iter()
        .filter(|(_, lanes)| lanes.len() == 1)
        .map(|(dir, lanes)| (dir, lanes[0].clone()))
        .collect()
}

/// Prefer durable validated workspace identity over the configured parent clone.
/// Retirement keeps both the workspace record and .env.reaped as provenance.
pub(crate) fn workspace_workdirs(
    home: &Path,
    mut dirs: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    if let Ok(entries) = std::fs::read_dir(home.join("workspaces")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if !crate::api::session_verbs::valid_session_name(name) {
                continue;
            }
            let Some(w) = crate::fanout_workspace::load(home, name) else {
                continue;
            };
            let env = home.join("sessions").join(format!("{name}.env"));
            let env = if env.exists() {
                env
            } else {
                env.with_extension("env.reaped")
            };
            let settings = crate::config::parse_env_file(&env);
            let project_owned = settings.get("CC_PROJECT").is_some_and(|project| {
                crate::project_execution::checkout::belongs_to(home, project, &w)
            });
            let worker_owned = w.path
                == crate::fanout_workspace::expected_path(&w.repo, name).to_string_lossy()
                && w.branch == format!("amux/fanout/{name}");
            if !(project_owned || worker_owned)
                || !Path::new(&w.repo).is_absolute()
                || w.base.len() != 40
                || !w.base.bytes().all(|b| b.is_ascii_hexdigit())
                || !settings.get("CC_DIR").is_some_and(|repo| {
                    crate::fanout_workspace::same_repository(repo, &w.repo)
                })
            {
                continue;
            }
            dirs.insert(name.into(), w.path);
        }
    }
    dirs
}

/// Native events have already passed launch nonce, provider and sequence checks.
/// Keep contradictory claims as None so a cwd fallback cannot override them.
///
/// Incremental (Mac hog RCA 20261014-112244, AMUX-5999): the full scan ran
/// json_extract over every native_status row (384k on the live DB, over 200s)
/// on each ledger pass. Pairs already seen are cached per database with the
/// highest id read, and each pass reads only newer rows.
type OwnerPairs = (i64, std::collections::BTreeSet<(String, String)>);
static NATIVE_OWNER_CACHE: std::sync::Mutex<Option<(String, OwnerPairs)>> = std::sync::Mutex::new(None);

fn native_owners(c: &rusqlite::Connection) -> rusqlite::Result<HashMap<String, Option<String>>> {
    let db = c.path().unwrap_or("").to_string();
    let max_id: i64 = c.query_row("SELECT coalesce(max(id),0) FROM session_events", [], |r| r.get(0))?;
    let (mut since, mut pairs) = {
        let g = NATIVE_OWNER_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        match g.as_ref() {
            Some((path, (seen, p))) if !db.is_empty() && *path == db && *seen <= max_id => (*seen, p.clone()),
            _ => (0, Default::default()),
        }
    };
    let started = std::time::Instant::now();
    let mut q = c.prepare("SELECT DISTINCT session,json_extract(data,'$.session_id') FROM session_events WHERE id > ?1 AND id <= ?2 AND type='session.native_status' AND json_valid(data) AND json_extract(data,'$.provider')='codex' AND json_extract(data,'$.event')='SessionStart' AND json_extract(data,'$.session_id')<>''")?;
    let rows = q.query_map([since, max_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for row in rows {
        pairs.insert(row?);
    }
    let ms = started.elapsed().as_millis() as u64;
    if ms > 2000 {
        tracing::warn!(target: "codex_ledger", verdict = "native_owners_slow", elapsed_ms = ms, rows_from = since,
            measured = true, n_considered = pairs.len(), "native owner scan was slow");
    }
    since = max_id;
    *NATIVE_OWNER_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some((db, (since, pairs.clone())));
    let mut owners = HashMap::<String, Option<String>>::new();
    for (worker, id) in pairs {
        owners
            .entry(id)
            .and_modify(|prior| {
                if prior.as_deref() != Some(&worker) {
                    *prior = None;
                }
            })
            .or_insert(Some(worker));
    }
    Ok(owners)
}

fn rollout_owner(
    path: &Path,
    native: &HashMap<String, Option<String>>,
    workdirs: &BTreeMap<String, String>,
    directories: &HashMap<String, String>,
) -> String {
    let Some(meta) = rollout_identity(path) else {
        return String::new();
    };
    let cwd = meta["cwd"].as_str().unwrap_or("").trim_end_matches('/');
    let id = meta["id"]
        .as_str()
        .or_else(|| meta["session_id"].as_str())
        .unwrap_or("");
    if let Some(owner) = native.get(id) {
        return owner
            .as_ref()
            .filter(|worker| {
                workdirs
                    .get(*worker)
                    .is_some_and(|dir| dir.trim_end_matches('/') == cwd)
            })
            .cloned()
            .unwrap_or_default();
    }
    directories.get(cwd).cloned().unwrap_or_default()
}

/// Read through the existing session index once; write only bounded exact row IDs.
fn ownership_repairs(
    c: &rusqlite::Connection,
    owners: &[(String, String)],
) -> rusqlite::Result<Vec<(i64, String)>> {
    if owners.is_empty() {
        return Ok(vec![]);
    }
    let mut unique = HashMap::<String, Option<String>>::new();
    for (conversation, owner) in owners {
        unique
            .entry(conversation.clone())
            .and_modify(|prior| {
                if prior.as_deref() != Some(owner.as_str()) {
                    *prior = None;
                }
            })
            .or_insert_with(|| Some(owner.clone()));
    }
    let owners: HashMap<_, _> = unique
        .into_iter()
        .filter_map(|(key, value)| value.map(|v| (key, v)))
        .collect();
    if owners.is_empty() {
        return Ok(vec![]);
    }

    let conversations = serde_json::to_string(&owners.keys().collect::<Vec<_>>()).unwrap();
    let mut q=c.prepare("SELECT id,conversation FROM token_ledger INDEXED BY idx_ledger_session WHERE session='' AND task='' AND conversation IN (SELECT value FROM json_each(?1)) ORDER BY id LIMIT 1000")?;
    let rows = q.query_map([conversations], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
    })?;
    rows.map(|r| r.map(|(id, conversation)| (id, owners[&conversation].clone())))
        .collect()
}

/// One pass over the codex rollouts. Returns how many ledger rows were written.
pub async fn index_once(store: &SharedStore, home: &Path) -> anyhow::Result<usize> {
    index_once_at(
        store,
        home,
        &codex_sessions_dir(),
        &workspace_workdirs(home, crate::api::session_verbs::all_session_workdirs()),
    )
    .await
}

/// The pass with its roots injected, so a test drives a temp tree and a fixed
/// lane map instead of this machine's real ones.
pub async fn index_once_at(
    store: &SharedStore,
    home: &Path,
    sessions: &Path,
    workdirs: &BTreeMap<String, String>,
) -> anyhow::Result<usize> {
    use super::token_ledger::{self, LedgerFileBatch, LedgerRow};
    if !sessions.is_dir() {
        return Ok(0);
    }
    let table = token_ledger::prices(home);
    let owners = unambiguous_owners(workdirs);
    let native = {
        let conn = store.read()?;
        native_owners(&conn)?
    };
    let cursors = token_ledger::read_cursors(store)?;

    let mut batches: Vec<LedgerFileBatch> = Vec::new();
    let mut repairs = Vec::new();
    for path in rollout_files(sessions) {
        let conversation = conversation_key(&path);
        let Ok(meta) = path.metadata() else { continue };
        let size = meta.len();
        let offset = cursors.get(&conversation).copied().unwrap_or(0);
        let lane = rollout_owner(&path, &native, workdirs, &owners);
        let identity = rollout_identity(&path);
        let parent = identity
            .as_ref()
            .and_then(|v| v.pointer("/source/subagent/thread_spawn/parent_thread_id"))
            .and_then(serde_json::Value::as_str)
            .map(|id| format!("codex:{}", id.rsplit('-').next().unwrap_or(id)));
        let started = super::token_ledger::transcript_started_at(&path);
        if !lane.is_empty() {
            repairs.push((conversation.clone(), lane.clone()));
        }
        if offset == size {
            continue;
        }
        let offset = if offset > size { 0 } else { offset };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let (new_off, turns) = parse_codex_from(&path, offset);
        if turns.is_empty() && new_off == offset {
            continue;
        }
        let rows = turns
            .into_iter()
            .map(|t| {
                let cost = token_ledger::turn_cost_usd(&table, &t.model, t.tokens);
                LedgerRow {
                    ts: t.ts,
                    session: lane.clone(),
                    conversation: conversation.clone(),
                    model: t.model,
                    tokens: t.tokens,
                    cost,
                    request_id: None,
                    parent_conversation: parent.clone(),
                    parent_started_at: started,
                    compaction: t.compaction,
                    message_id: Some(format!("ord:{}", t.ordinal)),
                }
            })
            .collect();
        batches.push(LedgerFileBatch {
            conversation,
            offset: new_off,
            mtime,
            rows,
        });
    }

    token_ledger::warn_unpriced("codex", &table, &batches);
    let inserted = token_ledger::commit_ledger_batch(store, batches, table).await?;
    let repairs = {
        let c = store.read()?;
        ownership_repairs(&c, &repairs)?
    };
    let repaired = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = repaired.clone();
    if !repairs.is_empty() {
        store
            .write_async(move |c| {
                let mut n = 0;
                for (id, lane) in repairs {
                    n += c.execute(
                        "UPDATE token_ledger SET session=?2 WHERE id=?1 AND session='' AND task=''",
                        rusqlite::params![id, lane],
                    )?;
                }
                count.store(n, std::sync::atomic::Ordering::Relaxed);
                if n > 0 {
                    tracing::info!(
                        measured = true,
                        n_considered = n,
                        verdict = "codex_workspace_usage_recovered",
                        "unowned exact-match rollout rows assigned from durable workspace identity"
                    );
                }
                Ok(crate::db::WriteOutcome {
                    applied: n > 0,
                    events: vec![],
                })
            })
            .await?;
    }
    let recovered = repaired.load(std::sync::atomic::Ordering::Relaxed);
    if recovered > 0 {
        tracing::info!(verdict="codex_usage_owner_recovered", rows=recovered,
            "bound previously unowned usage to validated CLI sessions");
    }
    if inserted > 0 || recovered > 0 {
        token_ledger::attribute_tasks(store).await?;
    }
    Ok(inserted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> SharedStore {
        let dir = tempfile::tempdir().unwrap();
        let st = crate::db::Store::open(&dir.path().join("codex-ledger-test.db")).unwrap();
        std::mem::forget(dir);
        std::sync::Arc::new(st)
    }

    /// One `token_count` event in the shape codex actually writes (copied from a
    /// live rollout on this box, trimmed).
    fn token_count(
        ordinal: i64,
        ts: &str,
        input: i64,
        cached: i64,
        cache_write: i64,
        output: i64,
    ) -> String {
        format!(
            r#"{{"timestamp":"{ts}","ordinal":{ordinal},"type":"event_msg","payload":{{"type":"token_count","info":{{"last_token_usage":{{"input_tokens":{input},"cached_input_tokens":{cached},"cache_write_input_tokens":{cache_write},"output_tokens":{output},"reasoning_output_tokens":{reasoning},"total_tokens":{total}}},"model_context_window":258400}}}}}}"#,
            reasoning = output / 3,
            total = input + output,
        )
    }

    fn session_meta(cwd: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-09-15T10:00:00.000Z","ordinal":0,"type":"session_meta","payload":{{"session_id":"019a4580-abd6-7153-993c-217e6941ea1d","cwd":"{cwd}","model_provider":"openai"}}}}"#
        )
    }

    fn rollout(dir: &Path, cwd: &str, body: &str) -> PathBuf {
        let day = dir.join("2026/09/15");
        std::fs::create_dir_all(&day).unwrap();
        let p = day.join("rollout-2026-09-15T10-00-00-019a4580-abd6-7153-993c-217e6941ea1d.jsonl");
        std::fs::write(&p, format!("{}\n{}", session_meta(cwd), body)).unwrap();
        p
    }

    /// The mapping this whole file rests on: codex reports the input total WITH
    /// the cached part inside it, and the reasoning tokens inside the output.
    /// Getting either wrong inflates a lane's bill with tokens nobody spent.
    #[test]
    fn a_turn_splits_cached_input_out_and_never_counts_reasoning_twice() {
        let dir = tempfile::tempdir().unwrap();
        let body = format!(
            "{}\n{}\n{}\n",
            r#"{"timestamp":"2026-09-15T10:00:01.000Z","ordinal":1,"type":"turn_context","payload":{"model":"gpt-5-codex"}}"#,
            token_count(2, "2026-09-15T10:00:02.000Z", 58880, 57344, 0, 390),
            token_count(3, "2026-09-15T10:00:03.000Z", 0, 0, 0, 0),
        );
        let p = rollout(dir.path(), "/Users/ethan/Dev/amux", &body);

        let (off, turns) = parse_codex_from(&p, 0);
        assert_eq!(
            turns.len(),
            1,
            "the zero-token event is not a ledger row: {turns:?}"
        );
        let t = &turns[0];
        assert_eq!(t.tokens, [1536, 57344, 0, 390],
            "fresh input is input_tokens minus the cached part, and output is not summed with reasoning");
        assert_eq!(
            t.model, "gpt-5-codex",
            "the model comes from the turn context, not session_meta"
        );
        assert_eq!(t.ordinal, 2);
        assert_eq!(t.ts, 1789466402);
        assert_eq!(
            off,
            std::fs::metadata(&p).unwrap().len(),
            "the cursor lands on the end of the file"
        );

        // Resuming from that cursor finds nothing: the same turn cannot be
        // billed twice by a second pass.
        let (off2, again) = parse_codex_from(&p, off);
        assert!(again.is_empty(), "{again:?}");
        assert_eq!(off2, off);
    }

    /// A directory two lanes share attributes to NEITHER: the ledger is what
    /// the cost view bills, and a wrong owner is worse than an unowned row.
    #[test]
    fn a_shared_working_directory_is_left_unattributed() {
        let mut dirs = BTreeMap::new();
        dirs.insert("alpha".to_string(), "/Users/ethan/Dev/amux".to_string());
        dirs.insert("beta".to_string(), "/Users/ethan/Dev/amux/".to_string()); // same dir, trailing slash
        dirs.insert("solo".to_string(), "/Users/ethan/Dev/solo".to_string());
        dirs.insert("nowhere".to_string(), String::new());
        let owners = unambiguous_owners(&dirs);
        assert_eq!(
            owners.get("/Users/ethan/Dev/solo"),
            Some(&"solo".to_string())
        );
        assert!(
            !owners.contains_key("/Users/ethan/Dev/amux"),
            "two lanes, no owner: {owners:?}"
        );
        assert!(!owners.contains_key(""), "an empty workdir owns nothing");
    }

    /// End to end: a rollout becomes ledger rows charged to the lane that works
    /// in that directory, and a second pass adds nothing.
    #[tokio::test]
    async fn a_rollout_becomes_ledger_rows_for_the_lane_that_works_there() {
        let home = tempfile::tempdir().unwrap();
        let sessions = tempfile::tempdir().unwrap();
        let body = format!(
            "{}\n{}\n{}\n",
            r#"{"timestamp":"2026-09-15T10:00:01.000Z","ordinal":1,"type":"turn_context","payload":{"model":"gpt-5-codex"}}"#,
            token_count(2, "2026-09-15T10:00:02.000Z", 1000, 400, 0, 50),
            token_count(4, "2026-09-15T10:00:04.000Z", 2000, 0, 0, 70),
        );
        rollout(sessions.path(), "/Users/ethan/Dev/amux", &body);
        let mut workdirs = BTreeMap::new();
        workdirs.insert(
            "amux-research".to_string(),
            "/Users/ethan/Dev/amux".to_string(),
        );

        let st = store();
        let n = index_once_at(&st, home.path(), sessions.path(), &workdirs)
            .await
            .unwrap();
        assert_eq!(n, 2);
        {
            let conn = st.read().unwrap();
            let rows: Vec<(String, String, String, i64, i64, i64, String)> = conn
                .prepare(
                    "SELECT session, conversation, model, input, cache_read, output, message_id \
                          FROM token_ledger ORDER BY ts",
                )
                .unwrap()
                .query_map([], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                })
                .unwrap()
                .flatten()
                .collect();
            assert_eq!(rows.len(), 2, "{rows:?}");
            assert_eq!(
                rows[0].0, "amux-research",
                "charged to the lane working in that cwd"
            );
            assert!(
                rows[0].1.starts_with("codex:"),
                "the conversation says which provider it came from: {}",
                rows[0].1
            );
            assert_eq!(rows[0].2, "gpt-5-codex");
            assert_eq!((rows[0].3, rows[0].4, rows[0].5), (600, 400, 50));
            assert_eq!(rows[0].6, "ord:2");
            assert_eq!(rows[1].6, "ord:4");
        }

        // Idempotent: the cursor holds, and re-running bills nothing again.
        let again = index_once_at(&st, home.path(), sessions.path(), &workdirs)
            .await
            .unwrap();
        assert_eq!(again, 0, "a second pass must not re-bill the same turns");
        let conn = st.read().unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM token_ledger", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
    }

    /// A rollout in a directory no lane owns still counts toward fleet totals,
    /// unowned. Dropping it would recreate the invisible-spend hole this closes.
    #[tokio::test]
    async fn an_unknown_working_directory_still_reaches_the_ledger_unowned() {
        let home = tempfile::tempdir().unwrap();
        let sessions = tempfile::tempdir().unwrap();
        rollout(
            sessions.path(),
            "/somewhere/nobody/owns",
            &format!(
                "{}\n",
                token_count(2, "2026-09-15T10:00:02.000Z", 500, 0, 0, 10)
            ),
        );
        let st = store();
        let n = index_once_at(&st, home.path(), sessions.path(), &BTreeMap::new())
            .await
            .unwrap();
        assert_eq!(n, 1);
        let conn = st.read().unwrap();
        let (session, input): (String, i64) = conn
            .query_row("SELECT session, input FROM token_ledger", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(session, "", "unowned, not dropped and not guessed");
        assert_eq!(input, 500);
    }
    #[test]
    fn native_owners_reads_only_new_rows_and_keeps_earlier_claims() {
        let dir = tempfile::tempdir().unwrap();
        let c = rusqlite::Connection::open(dir.path().join("o.db")).unwrap();
        c.execute_batch("CREATE TABLE session_events(id INTEGER PRIMARY KEY, ts REAL, session TEXT, type TEXT, data TEXT, source TEXT);").unwrap();
        let add = |w: &str, sid: &str| {
            c.execute("INSERT INTO session_events(ts,session,type,data) VALUES(1,?1,'session.native_status',?2)",
                rusqlite::params![w, serde_json::json!({"provider":"codex","event":"SessionStart","session_id":sid}).to_string()]).unwrap();
        };
        add("a", "s1");
        assert_eq!(native_owners(&c).unwrap().get("s1"), Some(&Some("a".to_string())));
        add("b", "s2");
        let o = native_owners(&c).unwrap();
        assert_eq!(o.get("s1"), Some(&Some("a".to_string())), "earlier claim survives the incremental pass");
        assert_eq!(o.get("s2"), Some(&Some("b".to_string())), "new claim is read");
        add("c", "s1");
        assert_eq!(native_owners(&c).unwrap().get("s1"), Some(&None), "a contradiction still reads as unowned");
    }
    #[tokio::test]
    async fn shared_checkout_usage_uses_native_identity_and_recovers_unowned_rows() {
        let home = tempfile::tempdir().unwrap();
        let sessions = tempfile::tempdir().unwrap();
        let st = store();
        let dirs = BTreeMap::from([
            ("first".into(), "/project/checkout".into()),
            ("second".into(), "/project/checkout".into()),
        ]);
        let p = rollout(
            sessions.path(),
            "/project/checkout",
            &format!(
                "{}\n",
                token_count(2, "2026-09-15T10:00:02.000Z", 500, 0, 0, 10)
            ),
        );
        index_once_at(&st, home.path(), sessions.path(), &dirs)
            .await
            .unwrap();
        st.write(|c| {
            c.execute("INSERT INTO session_events(ts,session,type,data,source) VALUES(1,'first','session.native_status',?1,'native-hook')",
                [serde_json::json!({"provider":"codex","event":"SessionStart","session_id":"019a4580-abd6-7153-993c-217e6941ea1d"}).to_string()])?;
            Ok(crate::db::WriteOutcome { applied: true, events: vec![] })
        }).unwrap();
        index_once_at(&st, home.path(), sessions.path(), &dirs)
            .await
            .unwrap();
        let c = st.read().unwrap();
        let (owner, count, tokens): (String, i64, i64) = c
            .query_row(
                "SELECT session,count(*),sum(input+output) FROM token_ledger",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((owner, count, tokens), ("first".into(), 1, 510));
        let mut native = native_owners(&c).unwrap();
        let directories = unambiguous_owners(&dirs);
        assert_eq!(rollout_owner(&p, &native, &dirs, &directories), "first");
        let wrong_dirs = BTreeMap::from([("first".into(), "/foreign".into())]);
        assert_eq!(rollout_owner(&p, &native, &wrong_dirs, &directories), "");
        native.insert("019a4580-abd6-7153-993c-217e6941ea1d".into(), None);
        assert_eq!(rollout_owner(&p, &native, &dirs, &directories), "");
    }

    #[tokio::test]
    async fn project_workspace_owners_survive_retirement_and_recover_exact_unowned_rows() {
        let home = tempfile::tempdir().unwrap();
        let sessions = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("sessions")).unwrap();
        let mut parent_dirs = BTreeMap::new();
        for (name, suffix) in [("active", "env"), ("retired", "env.reaped")] {
            let w = crate::fanout_workspace::Workspace {
                repo: "/parent/clone".into(),
                path: crate::fanout_workspace::expected_path("/parent/clone", name)
                    .to_string_lossy()
                    .into(),
                branch: format!("amux/fanout/{name}"),
                base: "a".repeat(40),
            };
            crate::fanout_workspace::save(home.path(), name, &w).unwrap();
            std::fs::write(
                home.path()
                    .join("sessions")
                    .join(format!("{name}.{suffix}")),
                "CC_DIR=/parent/clone\n",
            )
            .unwrap();
            parent_dirs.insert(name.into(), w.repo);
        }
        let dirs = workspace_workdirs(home.path(), parent_dirs);
        assert_eq!(dirs.len(), 2);
        assert_ne!(dirs["active"], dirs["retired"]);
        assert!(!unambiguous_owners(&dirs).contains_key("/parent/clone"));
        let st = store();
        rollout(
            sessions.path(),
            &dirs["retired"],
            &format!(
                "{}\n",
                token_count(2, "2026-09-15T10:00:02.000Z", 500, 0, 0, 10)
            ),
        );
        assert_eq!(
            index_once_at(&st, home.path(), sessions.path(), &BTreeMap::new())
                .await
                .unwrap(),
            1
        );
        let mut ambiguous = dirs.clone();
        ambiguous.insert("other".into(), dirs["retired"].clone());
        index_once_at(&st, home.path(), sessions.path(), &ambiguous)
            .await
            .unwrap();
        assert_eq!(
            st.read()
                .unwrap()
                .query_row("SELECT session FROM token_ledger", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            ""
        );
        index_once_at(&st, home.path(), sessions.path(), &dirs)
            .await
            .unwrap();
        index_once_at(&st, home.path(), sessions.path(), &dirs)
            .await
            .unwrap();
        let c = st.read().unwrap();
        let row: (String, String, i64) = c
            .query_row("SELECT session,task,count(*) FROM token_ledger", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert_eq!(
            row,
            ("retired".into(), "".into(), 1),
            "ownership repair neither double bills nor invents a claim window"
        );
        std::fs::write(home.path().join("sessions/active.env"), "CC_DIR=/foreign\n").unwrap();
        assert!(!workspace_workdirs(home.path(), BTreeMap::new()).contains_key("active"));
    }

    #[tokio::test]
    async fn codex_ownership_repair_skips_unchanged_rows_and_admits_new_owners() {
        let st = store();
        st.write(|c| {
            for (conversation, session) in [("one", ""), ("two", ""), ("foreign", "fixed")] {
                c.execute(
                    "INSERT INTO token_ledger(ts,session,conversation,input) VALUES(1,?1,?2,10)",
                    rusqlite::params![session, conversation],
                )?;
            }
            let first = vec![
                ("one".into(), "owner".into()),
                ("foreign".into(), "wrong".into()),
            ];
            let rows = ownership_repairs(c, &first)?;
            assert_eq!(rows.len(), 1);
            for (id, owner) in rows {
                c.execute(
                    "UPDATE token_ledger SET session=?2 WHERE id=?1",
                    rusqlite::params![id, owner],
                )?;
            }
            assert!(
                ownership_repairs(c, &first)?.is_empty(),
                "unchanged pass does not enter writer"
            );
            let later = vec![
                ("one".into(), "owner".into()),
                ("two".into(), "retired-owner".into()),
            ];
            let rows = ownership_repairs(c, &later)?;
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].1, "retired-owner");
            assert!(
                ownership_repairs(
                    c,
                    &[
                        ("two".into(), "first".into()),
                        ("two".into(), "second".into())
                    ]
                )?
                .is_empty(),
                "ambiguous conversation ownership refused"
            );
            Ok(crate::db::WriteOutcome {
                applied: true,
                events: vec![],
            })
        })
        .unwrap();
    }
}

#[cfg(test)]
mod cumulative_accounting_tests {
    use super::*;
    use serde_json::json;
    fn count(ordinal: Option<i64>, input: i64, cached: i64, out: i64) -> String {
        let mut event = json!({"type":"event_msg","timestamp":"2026-10-23T10:00:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":input,"cached_input_tokens":cached,"output_tokens":out},"last_token_usage":{"input_tokens":999,"output_tokens":999}}}});
        if let Some(n) = ordinal {
            event["ordinal"] = json!(n);
        }
        event.to_string() + "\n"
    }
    #[test]
    fn cumulative_deltas_ignore_repeated_events_and_carry_model_across_passes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        let prefix = format!(
            "{}\n{}{}",
            json!({"type":"turn_context","payload":{"model":"gpt-fixture"}}),
            count(Some(1), 100, 60, 10),
            count(Some(2), 100, 60, 10)
        );
        std::fs::write(&path, &prefix).unwrap();
        let (off, rows) = parse_codex_from(&path, 0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tokens, [40, 60, 0, 10]);
        std::fs::write(
            &path,
            format!(
                "{prefix}{}{}",
                count(Some(3), 250, 160, 30),
                count(Some(4), 250, 160, 30)
            ),
        )
        .unwrap();
        let (end, rows) = parse_codex_from(&path, off);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model, "gpt-fixture");
        assert_eq!(rows[0].tokens, [50, 100, 0, 20]);
        assert!(parse_codex_from(&path, end).1.is_empty());
        let (_, all) = parse_codex_from(&path, 0);
        assert_eq!(
            all.iter()
                .map(|r| r.tokens.iter().sum::<i64>())
                .sum::<i64>(),
            280
        );
    }
    #[test]
    fn missing_ordinals_use_stable_byte_keys_and_incomplete_lines_wait() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        let first = count(None, 100, 10, 2);
        let second = count(None, 200, 20, 4);
        std::fs::write(&path, format!("{first}{}", &second[..10])).unwrap();
        let (off, rows) = parse_codex_from(&path, 0);
        assert_eq!(off, first.len() as u64);
        assert_eq!(rows.len(), 1);
        std::fs::write(&path, format!("{first}{second}")).unwrap();
        let (_, again) = parse_codex_from(&path, off);
        assert_eq!(again.len(), 1);
        assert_ne!(rows[0].ordinal, again[0].ordinal);
        assert_eq!(again[0].model, "");
    }
}
