//! Tmux watch — notice when the live tmux server disappears, say so once, and bring the fleet back.
//!
//! # Why (2026-10-17 01:05 outage)
//!
//! A reviewer-script test run from a worker pane ran `TMUX_TMPDIR=… tmux kill-server`; tmux
//! followed `$TMUX` to the LIVE server and every worker died at once. Nothing in amux said so:
//! `reconcile` reads "no server running" as an empty fleet (correct on a fresh boot), so the
//! dashboard just showed everyone stopped until mrpopo restarted each worker by hand at 01:20.
//!
//! Deploys are not this: the tmux server is daemonized (its own process group, parent 1) and
//! self-adoption exec's only the amux process (`tests/tmux_survives_adoption.rs` pins that).
//!
//! # What it does
//!
//! Every tick it joins session discovery to amux's agent census. Two socket-owner-confirmed
//! down readings at least five seconds apart establish a loss: ONE `verdict=tmux_server_lost`
//! event with the time and worker count, a push to the owner, then a restore of every worker that was running (non-archived, not paused), each
//! resuming its recorded conversation exactly as `amux start --detach` does, and one
//! `verdict=fleet_restored` with counts and failures. Leads get a message once they are back.
//!
//! Workers a human stopped are not in the running set, so they are never restarted; a restore
//! runs at most once per [`RESTORE_GAP_SECS`]. A fresh boot with no tmux server is quiet: the
//! state that says "it was up" is persisted, so a server restart during an outage still knows.

use crate::api::AppState;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const JOB: &str = super::registry::ids::TMUX_WATCH;
const TICK_SECS: u64 = 15;
/// At most one restore per ten minutes, however often the server is lost.
pub(crate) const RESTORE_GAP_SECS: f64 = 600.0;
const LOSS_CONFIRM_SECS: f64 = 5.0;
const LOSS_EPISODE_SECS: f64 = 3600.0;
const MAX_EPISODE_LOSSES: u32 = 4;

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
#[serde(default)]
pub(crate) struct WatchState {
    pub was_up: bool,
    /// When the server was last seen up: a host boot after this means the loss was a reboot.
    pub observed_at: Option<f64>,
    /// The always-alive supervisor's consecutive failures and next allowed attempt.
    pub keeper_failures: u32,
    pub keeper_next_at: Option<f64>,
    pub keeper_alerted_at: Option<f64>,
    /// Workers still to bring back after a loss, persisted so a failed start, a server restart
    /// or a second loss never drops them; retried with backoff until each is back or ineligible.
    pub pending: Vec<String>,
    pub pending_attempts: u32,
    pub pending_next_at: Option<f64>,
    pub pending_alerted_at: Option<f64>,
    /// Loss history survives successful starts; four losses without a quiet hour halt recovery.
    pub episode_losses: u32,
    pub last_loss_at: Option<f64>,
    pub episode_alerted_at: Option<f64>,
    pub loss_notice_pending: bool,
    pub storm_alerted_at: Option<f64>,
    pub restore_halted: bool,
    /// An interrupted pass may adopt completed starts without consuming a second restore slot.
    pub pending_in_progress: bool,
    /// First corroborated down reading; an intervening unreadable/up reading clears it.
    pub down_since: Option<f64>,
    /// amux workers with a live agent at the last good fleet census.
    pub workers: Vec<String>,
    pub lost_at: Option<f64>,
    pub last_restore_at: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Census {
    /// The server answered; tick filters discovery to live agents before persisting it.
    Up(Vec<String>),
    /// No tmux server is running on this socket.
    NoServer,
    /// tmux failed some other way: not evidence either way.
    Unreadable(String),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Decision {
    Quiet,
    /// `rebooted`: the host booted after the server was last seen, so this is a reboot, not a kill.
    /// `workers`: to restore (running before, not running now); `before`: the whole pre-outage set.
    Lost { workers: Vec<String>, before: Vec<String>, restore: bool, rebooted: bool },
}

/// The state machine, pure so every transition is testable without tmux.
pub(crate) fn observe(st: &mut WatchState, census: &Census, now: f64, boot_at: Option<f64>) -> Decision {
    // The host booted after the last time the server was seen: whatever tmux shows now is new,
    // so a reboot is caught even when something started a tmux server before this first look.
    let rebooted = matches!((boot_at, st.observed_at), (Some(b), Some(o)) if b > o);
    let lost = match census {
        Census::Unreadable(_) => return Decision::Quiet,
        Census::NoServer => st.was_up,
        Census::Up(_) => st.was_up && rebooted,
    };
    let before = st.workers.clone();
    match census {
        Census::Up(workers) => {
            st.was_up = true;
            st.workers = workers.clone();
            st.lost_at = None;
            st.observed_at = Some(now);
        }
        _ => st.was_up = false,
    }
    if !lost {
        return Decision::Quiet;
    }
    if !matches!(census, Census::Up(_)) {
        st.lost_at = Some(now);
    }
    // Success is not evidence that the server stopped crashing. Only a quiet hour closes
    // the episode; the fourth loss latches recovery off and produces one escalation.
    if st.last_loss_at.is_none_or(|t| now - t >= LOSS_EPISODE_SECS) {
        st.episode_losses = 0;
        st.episode_alerted_at = None;
        st.storm_alerted_at = None;
        st.restore_halted = false;
    }
    st.episode_losses = st.episode_losses.saturating_add(1);
    st.last_loss_at = Some(now);
    st.loss_notice_pending |= !rebooted && st.episode_alerted_at.is_none();
    st.restore_halted |= st.episode_losses >= MAX_EPISODE_LOSSES;
    st.pending_in_progress = false;
    st.pending_attempts = 0;
    st.pending_alerted_at = None;
    let delay = RESTORE_GAP_SECS * 2f64.powi(st.episode_losses.saturating_sub(2).min(10) as i32);
    st.pending_next_at = st.last_restore_at.map(|t| (t + delay).max(now));
    let restore = !st.restore_halted && st.pending_next_at.is_none_or(|t| now >= t);
    // Restore what was running before and is not running now.
    let running: &[String] = if let Census::Up(w) = census { w } else { &[] };
    let workers: Vec<String> = before.iter().filter(|w| !running.contains(w)).cloned().collect();
    for w in &workers {
        if !st.pending.contains(w) {
            st.pending.push(w.clone());
        }
    }
    Decision::Lost { workers, before, restore, rebooted }
}

/// Parse session discovery. NoServer is only a candidate until socket ownership corroborates it.
pub(crate) fn parse_census(success: bool, stdout: &str, stderr: &str) -> Census {
    if success {
        let mut names: Vec<String> = stdout
            .lines()
            .filter_map(|l| l.trim().strip_prefix("amux-"))
            .filter(|n| !n.is_empty())
            .map(str::to_string)
            .collect();
        names.sort();
        names.dedup();
        return Census::Up(names);
    }
    let err = stderr.trim();
    // Refusal alone is insufficient: read_census corroborates with tmux_health.
    if err.starts_with("no server running")
        || (err.starts_with("error connecting to") && (err.contains("No such file or directory") || err.contains("Connection refused")))
    {
        Census::NoServer
    } else {
        Census::Unreadable(err.chars().take(200).collect())
    }
}

/// `socket` (tests only) is an explicit `-S` path inside the test's own temp dir; `None` reads the
/// server's own tmux. `TMUX` and `TMUX_PANE` are always removed so the census can never follow a
/// pane's socket to the live server.
pub(crate) async fn read_census(socket: Option<&Path>) -> Census {
    let mut cmd = tokio::process::Command::new("tmux");
    if let Some(sock) = socket {
        cmd.arg("-S").arg(sock);
    }
    cmd.args(["list-sessions", "-F", "#{session_name}"])
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let out = tokio::time::timeout(std::time::Duration::from_secs(10), cmd.output()).await;
    match out {
        Ok(Ok(o)) => {
            let census = parse_census(o.status.success(), &String::from_utf8_lossy(&o.stdout), &String::from_utf8_lossy(&o.stderr));
            if census == Census::NoServer {
                corroborate_down(crate::backend::tmux_health::observe_socket(socket).await)
            } else {
                census
            }
        },
        Ok(Err(e)) => Census::Unreadable(format!("tmux did not run: {e}")),
        Err(_) => Census::Unreadable("tmux list-sessions timed out".into()),
    }
}

/// Reuse AMUX-4203's kernel-owner verdict, including its fail-closed unmeasured case.
fn corroborate_down(health: crate::backend::tmux_health::Observation) -> Census {
    if health.measured && health.verdict == "no_server" {
        Census::NoServer
    } else {
        Census::Unreadable(format!("socket ownership: {}", health.verdict))
    }
}

/// The shared fleet census knows whether an agent is alive; a session at a shell prompt
/// after API stop, /exit or a crash must never enter the durable was-running set.
fn worker_census(census: Census, signals: &crate::api::sessions_legacy::FleetSignals) -> Census {
    match census {
        Census::Up(workers) => {
            let n_considered = workers.len();
            let running: Vec<String> = workers.into_iter()
                .filter(|name| signals.agent_running(&format!("amux-{name}"))).collect();
            let excluded = n_considered - running.len();
            if excluded > 0 && crate::log_dedupe::first_this_bucket("tmux-watch-shells", crate::log_dedupe::hour_bucket(now_secs())) {
                tracing::info!(target: "tmux_watch", verdict = "tmux_watch_nonagents_excluded",
                    measured = true, n_considered, excluded, "retained sessions without agents excluded from recovery census");
            }
            Census::Up(running)
        },
        other => other,
    }
}

fn confirm_census(st: &mut WatchState, census: &Census, now: f64) -> Census {
    if matches!(census, Census::NoServer) {
        let first = *st.down_since.get_or_insert(now);
        if now - first < LOSS_CONFIRM_SECS {
            return Census::Unreadable("awaiting a second corroborated down reading".into());
        }
    } else {
        st.down_since = None;
    }
    census.clone()
}

fn claim_loss_notice(st: &mut WatchState, now: f64) -> bool {
    if !st.loss_notice_pending || st.episode_alerted_at.is_some() {
        return false;
    }
    st.loss_notice_pending = false;
    st.episode_alerted_at = Some(now);
    true
}

fn claim_storm_notice(st: &mut WatchState, now: f64) -> bool {
    if !st.restore_halted || st.storm_alerted_at.is_some() {
        return false;
    }
    st.storm_alerted_at = Some(now);
    true
}

/// Start one restore pass. A restart adopts this same pass; a new pass takes a new slot.
fn begin_restore(st: &mut WatchState, now: f64) {
    if !st.pending_in_progress {
        st.last_restore_at = Some(now);
        st.pending_in_progress = true;
    }
}

/// Which of the previously running workers may be brought back: the env file still exists and
/// the worker is neither archived nor paused. Returns (eligible, skipped with reasons).
pub(crate) fn eligible(home: &Path, workers: &[String]) -> (Vec<String>, Vec<(String, &'static str)>) {
    let (mut ok, mut skipped) = (Vec::new(), Vec::new());
    for name in workers {
        let env = home.join("sessions").join(format!("{name}.env"));
        if !env.is_file() {
            skipped.push((name.clone(), "no_env_file"));
            continue;
        }
        let cfg = crate::config::parse_env_file(&env);
        let on = |k: &str| crate::api::session_verbs::env_flag_on(cfg.get(k).map(String::as_str));
        if on("CC_ARCHIVED") {
            skipped.push((name.clone(), "archived"));
        } else if on("CC_PAUSED") {
            skipped.push((name.clone(), "paused"));
        } else {
            ok.push(name.clone());
        }
    }
    (ok, skipped)
}

/// Workers in the `leads` group, from their env files.
pub(crate) fn leads(home: &Path, workers: &[String]) -> Vec<String> {
    workers
        .iter()
        .filter(|name| {
            crate::config::parse_env_file(&home.join("sessions").join(format!("{name}.env")))
                .get("CC_TAGS")
                .is_some_and(|tags| tags.split(',').any(|t| t.trim().trim_matches('"').eq_ignore_ascii_case("leads")))
        })
        .cloned()
        .collect()
}

/// Workers that must always be running (Max, 2026-10-17: "Mr Popo is a special case where it
/// should always live"). `AMUX_ALWAYS_ALIVE` (comma list) overrides; an empty value turns it off.
pub(crate) fn keepers() -> Vec<String> {
    std::env::var("AMUX_ALWAYS_ALIVE")
        .unwrap_or_else(|_| "mrpopo".into())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Recovery order: always-alive workers first, then leads, then everyone else (each group sorted).
pub(crate) fn restore_order(home: &Path, workers: &[String], keep: &[String]) -> Vec<String> {
    let leads = leads(home, workers);
    let mut first: Vec<String> = keep.iter().filter(|k| workers.contains(k)).cloned().collect();
    let mut mid: Vec<String> = leads.into_iter().filter(|w| !first.contains(w)).collect();
    let mut rest: Vec<String> = workers.iter().filter(|w| !first.contains(w) && !mid.contains(w)).cloned().collect();
    mid.sort();
    rest.sort();
    first.append(&mut mid);
    first.append(&mut rest);
    first
}

/// Seconds to wait before the next always-alive attempt after `failures` consecutive failures:
/// 30s, 60s, 120s, ... capped at 10 minutes.
pub(crate) fn keeper_backoff(failures: u32) -> f64 {
    (30.0 * 2f64.powi(failures.saturating_sub(1).min(10) as i32)).min(600.0)
}

/// Is an always-alive attempt due now? Not when it is running, archived or missing, or backing off.
pub(crate) fn keeper_due(st: &WatchState, running: bool, now: f64) -> bool {
    !running && st.keeper_next_at.is_none_or(|t| now >= t)
}

pub(crate) fn record_keeper(st: &mut WatchState, started: bool, now: f64) {
    if started {
        st.keeper_failures = 0;
        st.keeper_next_at = None;
    } else {
        st.keeper_failures += 1;
        st.keeper_next_at = Some(now + keeper_backoff(st.keeper_failures));
    }
}

/// Is a pending-restore pass due now?
pub(crate) fn pending_due(st: &WatchState, now: f64) -> bool {
    !st.restore_halted && !st.pending.is_empty() && st.pending_next_at.is_none_or(|t| now >= t)
}

/// Settle one pending pass: started and no-longer-eligible workers leave the set; failures stay
/// and retries wait at least ten minutes. Episode history survives success. Returns true when done.
pub(crate) fn settle_pending(st: &mut WatchState, r: &Restore, now: f64) -> bool {
    st.pending_in_progress = false;
    st.pending.retain(|w| !r.restored.contains(w) && !r.skipped.iter().any(|(n, _)| n == w));
    if st.pending.is_empty() {
        st.pending_attempts = 0;
        st.pending_next_at = None;
        st.pending_alerted_at = None;
        return true;
    }
    st.pending_attempts += 1;
    st.pending_next_at = Some(now + RESTORE_GAP_SECS.max(keeper_backoff(st.pending_attempts)));
    false
}

/// The running set shared with boot recovery (#141): `~/.amux/running-set.json`,
/// {"updated_at", "workers":[{"name","tags"}]}, rewritten from the agent census while up.
fn worker_rows(home: &Path, workers: &[String]) -> Vec<serde_json::Value> {
    workers
        .iter()
        .map(|name| {
            let tags: Vec<String> = crate::config::parse_env_file(&home.join("sessions").join(format!("{name}.env")))
                .get("CC_TAGS")
                .map(|t| t.split(',').map(|x| x.trim().trim_matches('"').to_string()).filter(|x| !x.is_empty()).collect())
                .unwrap_or_default();
            serde_json::json!({"name": name, "tags": tags})
        })
        .collect()
}

fn write_json(path: &Path, body: &serde_json::Value) -> bool {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body.to_string()).is_ok() && std::fs::rename(&tmp, path).is_ok()
}

pub(crate) fn write_running_set(home: &Path, workers: &[String], now: f64) {
    write_json(&home.join("running-set.json"), &serde_json::json!({"updated_at": now as i64, "workers": worker_rows(home, workers)}));
}

/// On a loss, the pre-outage set is kept in `running-set.prev.json` ({"boot_epoch", "updated_at",
/// "workers":[{"name","tags"}], "restored_at"}), and never overwritten while it is unrestored:
/// the live `running-set.json` shrinks as soon as tmux is back, the snapshot does not.
pub(crate) fn snapshot_prev(home: &Path, workers: &[String], boot_at: Option<f64>, now: f64) -> bool {
    let path = home.join("running-set.prev.json");
    let unrestored = std::fs::read_to_string(&path).ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .is_some_and(|old| old["restored_at"].is_null());
    if unrestored {
        return false;
    }
    write_json(&path, &serde_json::json!({"boot_epoch": boot_at.map(|b| b as i64), "updated_at": now as i64,
        "workers": worker_rows(home, workers), "restored_at": serde_json::Value::Null}))
}

/// The unrestored pre-outage snapshot's worker names, if there is one.
pub(crate) fn read_prev(home: &Path) -> Option<Vec<String>> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(home.join("running-set.prev.json")).ok()?).ok()?;
    if !v["restored_at"].is_null() {
        return None;
    }
    Some(v["workers"].as_array()?.iter().filter_map(|w| w["name"].as_str().map(str::to_string)).collect())
}

pub(crate) fn mark_prev_restored(home: &Path, now: f64) {
    let path = home.join("running-set.prev.json");
    let Some(mut v) = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()) else { return };
    v["restored_at"] = serde_json::json!(now as i64);
    write_json(&path, &v);
}

pub(crate) fn read_running_set(home: &Path) -> Option<Vec<String>> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(home.join("running-set.json")).ok()?).ok()?;
    Some(v["workers"].as_array()?.iter().filter_map(|w| w["name"].as_str().map(str::to_string)).collect())
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct Restore {
    pub restored: Vec<String>,
    /// (name, error)
    pub failed: Vec<(String, String)>,
    pub skipped: Vec<(String, &'static str)>,
}

/// Start each eligible worker in turn; one failure never stops the rest.
pub(crate) async fn restore_fleet<F, Fut>(home: &Path, workers: &[String], mut start: F) -> Restore
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = (bool, String)>,
{
    let (ok, skipped) = eligible(home, workers);
    let mut out = Restore { skipped, ..Default::default() };
    for name in restore_order(home, &ok, &keepers()) {
        let (started, msg) = start(name.clone()).await;
        if started { out.restored.push(name) } else { out.failed.push((name, msg)) }
    }
    out
}

fn state_path(home: &Path) -> PathBuf {
    home.join("tmux-watch.json")
}

pub(crate) fn load_state(home: &Path) -> WatchState {
    std::fs::read_to_string(state_path(home))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Persist the watch state; a failed write is logged (the pending restore is only durable once
/// this returns true).
pub(crate) fn save_state(home: &Path, st: &WatchState) -> bool {
    let path = state_path(home);
    let tmp = path.with_extension("json.tmp");
    let ok = serde_json::to_vec(st).is_ok_and(|body| std::fs::write(&tmp, body).is_ok() && std::fs::rename(&tmp, &path).is_ok());
    if !ok {
        tracing::error!(target: "tmux_watch", path = %path.display(), pending = st.pending.len(), verdict = "tmux_watch_state_unsaved",
            "could not persist the tmux watch state; a pending restore is not durable until it saves");
    }
    ok
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

async fn start(state: &AppState, name: &str) -> (bool, String) {
    // Same path as `amux start --detach`: resumes the recorded conversation.
    crate::api::session_verbs::start_session(state, name, "", false).await
}

async fn push_owner(state: &AppState, title: &str, body: &str, tag: &str) {
    let pushed = crate::push::send_all(state, title, body, "amux", tag, "/").await;
    if let Err(why) = crate::api::alerts::push_delivery_verdict(&pushed) {
        tracing::warn!(target: "tmux_watch", verdict = "tmux_watch_push_undelivered", %why, tag, "an owner push was not delivered");
    }
}

/// Always-alive supervisor: start each keeper that is not running, before anything else, unless
/// it is archived or has no env file. Returns the keepers it started, so the fleet restore does
/// not start them twice.
async fn keep_alive(state: &AppState, home: &Path, st: &mut WatchState, census: &Census, now: f64) -> Vec<String> {
    let mut started = Vec::new();
    if st.restore_halted || (!st.pending.is_empty() && !pending_due(st, now)) {
        return started;
    }
    let running: &[String] = match census {
        Census::Up(w) => w,
        Census::NoServer => &[],
        Census::Unreadable(_) => return started,
    };
    for keeper in keepers() {
        let env = home.join("sessions").join(format!("{keeper}.env"));
        if !env.is_file() {
            continue;
        }
        let cfg = crate::config::parse_env_file(&env);
        let on = |k: &str| crate::api::session_verbs::env_flag_on(cfg.get(k).map(String::as_str));
        if on("CC_ARCHIVED") || on("CC_PAUSED") || cfg.get("CC_REVIEW_HELD").map(String::as_str) == Some("1") {
            continue;
        }
        // A tmux session can outlive the agent (a pane left at its shell prompt), so liveness is
        // the process-aware check, not the session name.
        let alive = running.contains(&keeper) && crate::api::session_verbs::is_running(&keeper).await;
        if !keeper_due(st, alive, now) {
            continue;
        }
        let (ok, msg) = start(state, &keeper).await;
        record_keeper(st, ok, now);
        if ok {
            started.push(keeper.clone());
            tracing::warn!(target: "tmux_watch", session = %keeper, measured = true, n_considered = 1, verdict = "keeper_restarted",
                "always-alive worker was not running; restarted it on its recorded conversation");
        } else {
            tracing::error!(target: "tmux_watch", session = %keeper, measured = true, n_considered = 1, verdict = "keeper_start_failed",
                failures = st.keeper_failures, retry_in_s = keeper_backoff(st.keeper_failures), detail = %msg,
                "always-alive worker could not be restarted");
            if st.keeper_failures >= 3 && st.keeper_alerted_at.is_none_or(|t| now - t >= 3600.0) {
                st.keeper_alerted_at = Some(now);
                push_owner(state, &format!("amux: {keeper} will not start"),
                    &format!("{keeper} has failed to start {} times in a row ({msg}). amux keeps retrying every 10 minutes.", st.keeper_failures),
                    "keeper-unrecoverable").await;
            }
        }
    }
    started
}

/// Observe and persist the pre-start state before any worker start can change tmux's census.
fn begin_tick(home: &Path, census: &Census, now: f64, boot_at: Option<f64>) -> (WatchState, Decision, WatchState, Census) {
    let mut st = load_state(home);
    let before = st.clone();
    let census = confirm_census(&mut st, census, now);
    let decision = observe(&mut st, &census, now, boot_at);
    if let Census::Up(workers) = &census {
        write_running_set(home, workers, now);
    }
    // A failed save is logged by save_state; the pending set is only durable on success.
    if st != before {
        save_state(home, &st);
    }
    (st, decision, before, census)
}

async fn tick(state: AppState) {
    let home = crate::config::amux_home();
    let mut census = read_census(None).await;
    if matches!(census, Census::Up(_)) {
        let discovery = census.clone();
        let store = state.store.clone();
        census = tokio::task::spawn_blocking(move || {
            match store.read() {
                Ok(conn) => worker_census(census, &crate::api::sessions_legacy::FleetSignals::load(&conn)),
                Err(error) => Census::Unreadable(format!("worker census unavailable: {error}")),
            }
        }).await.unwrap_or_else(|error| Census::Unreadable(format!("worker census failed: {error}")));
        // Do not replace the last good running set with an empty/partial census if tmux
        // disappeared between discovery and the shared agent-process probe.
        if matches!(census, Census::Up(_)) && read_census(None).await != discovery {
            census = Census::Unreadable("session discovery changed during the agent census".into());
        }
    }
    let now = now_secs();
    // Persist the loss (its pending set) BEFORE any start can change what tmux shows, so an
    // interruption after the keeper's start can never consume it.
    let (mut st, decision, before, census) = begin_tick(&home, &census, now, crate::runtime_jobs::heartbeat::machine_boot_at());
    if matches!(census, Census::Unreadable(_)) {
        if let Census::Unreadable(ref why) = census {
            if crate::log_dedupe::first_this_bucket("tmux-watch-unreadable", crate::log_dedupe::hour_bucket(now)) {
                tracing::warn!(target: "tmux_watch", verdict = "tmux_watch_census_unreadable", %why,
                    "tmux watch has no confirmed reading; preserving the last good census");
            }
        }
        return;
    }
    if pending_due(&st, now) {
        begin_restore(&mut st, now);
        if !save_state(&home, &st) { return; }
    }
    // Mr Popo first, before anything else, subject to the same storm gate.
    let kept = keep_alive(&state, &home, &mut st, &census, now).await;
    if st != before {
        save_state(&home, &st);
    }
    let mut loss = None;
    if let Decision::Lost { workers, before, restore, rebooted } = decision {
        let boot_at = crate::runtime_jobs::heartbeat::machine_boot_at();
        if !snapshot_prev(&home, &before, boot_at, now) {
            tracing::warn!(target: "tmux_watch", verdict = "running_set_prev_kept", "an unrestored pre-outage snapshot already exists; keeping it");
        }
        let at = chrono::Utc::now().to_rfc3339();
        let verdict = if rebooted { "host_rebooted" } else { "tmux_server_lost" };
        tracing::error!(target: "tmux_watch", measured = true, n_considered = workers.len(), verdict,
            lost_at = %at, workers = workers.len(), restore,
            "the tmux server is gone ({verdict}): {} worker(s) were running in it", workers.len());
        if !restore {
            tracing::warn!(target: "tmux_watch", measured = true, n_considered = workers.len(), verdict = "fleet_restore_rate_limited",
                losses = st.episode_losses, next_at = ?st.pending_next_at, halted = st.restore_halted,
                "fleet restore delayed or halted by the loss episode policy");
        }
        loss = Some((at, rebooted, workers.len()));
    }
    // Notices are driven from durable state as well as fresh loss edges: a crash between
    // recording the loss and reaching this point must not erase the owner's escalation.
    if claim_loss_notice(&mut st, now) && save_state(&home, &st) {
        push_owner(&state, "amux: every worker stopped",
            &format!("The tmux server was lost with {} worker(s) recorded. Recovery is bounded to one pass per ten minutes, with backoff after repeated losses.", st.pending.len()),
            "tmux-server-lost").await;
    }
    if claim_storm_notice(&mut st, now) && save_state(&home, &st) {
        tracing::error!(target: "tmux_watch", verdict = "fleet_restore_halted", losses = st.episode_losses,
            "repeated tmux losses halted automatic starts; pending evidence retained");
        push_owner(&state, "amux: tmux recovery halted",
            "The tmux server keeps dying. Automatic starts have stopped; pending workers and loss evidence are preserved. Inspect GET /api/debug/tmux.",
            "tmux-recovery-halted").await;
    }
    // The pending set (persisted) is retried on every readable tick, not only at the loss, so a
    // failed start, a restart mid-restore or a second loss never leaves a worker down for good.
    if !matches!(census, Census::Unreadable(_)) {
        drive_pending(&state, &home, &mut st, &kept, now, loss).await;
    }
}

/// One pending-restore pass when due. State is saved BEFORE any start so a crash mid-pass
/// re-runs it (a start is idempotent: a running worker reads "already running").
async fn drive_pending(state: &AppState, home: &Path, st: &mut WatchState, kept: &[String], now: f64,
    loss: Option<(String, bool, usize)>) {
    let keep = keepers();
    st.pending.retain(|w| !kept.contains(w) && !keep.contains(w));
    if !pending_due(st, now) {
        save_state(home, st);
        return;
    }
    begin_restore(st, now);
    if !save_state(home, st) { return; }
    let todo = st.pending.clone();
    // Bounded fixture pause from 5fd4e6128; zero in production.
    let step_delay_ms = crate::config::env_i64("AMUX_TMUX_WATCH_RESTORE_STEP_DELAY_MS", 0).clamp(0, 10_000) as u64;
    let r = restore_fleet(home, &todo, |name| {
        let s = state.clone();
        async move {
            let result = start(&s, &name).await;
            if step_delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(step_delay_ms)).await;
            }
            result
        }
    })
    .await;
    let done = settle_pending(st, &r, now);
    save_state(home, st);
    let failures = r.failed.iter().map(|(n, e)| format!("{n}: {e}")).collect::<Vec<_>>().join("; ");
    let skipped = r.skipped.iter().map(|(n, why)| format!("{n} ({why})")).collect::<Vec<_>>().join(", ");
    tracing::warn!(target: "tmux_watch", measured = true, n_considered = todo.len(),
        verdict = if done { "fleet_restored" } else { "fleet_restore_incomplete" },
        restored = r.restored.len(), failed = r.failed.len(), skipped = r.skipped.len(), still_pending = st.pending.len(),
        order = %r.restored.join(","), %failures, skipped_workers = %skipped, retry_in_s = st.pending_next_at.map(|t| t - now),
        "fleet restore pass: {} restored, {} failed, {} skipped, {} still pending", r.restored.len(), r.failed.len(), r.skipped.len(), st.pending.len());
    if done {
        mark_prev_restored(home, now);
    } else if st.pending_attempts >= 3 && st.pending_alerted_at.is_none() {
        st.pending_alerted_at = Some(now);
        save_state(home, st);
        push_owner(state, "amux: workers are not coming back",
            &format!("{} worker(s) still down after {} restore attempts ({failures}). amux keeps retrying every 10 minutes.", st.pending.len(), st.pending_attempts),
            "fleet-restore-stuck").await;
    }
    if r.restored.is_empty() {
        return;
    }
    let (at, rebooted, total) = loss.unwrap_or_else(|| (chrono::Utc::now().to_rfc3339(), false, todo.len()));
    let note = format!(
        "[amux] The tmux server was lost at {at}{}; amux restarted {} of {} worker(s){}{}{}. Check your team's workers.",
        if rebooted { " (host reboot)" } else { "" },
        r.restored.len() + kept.len(), total + kept.len(),
        if failures.is_empty() { String::new() } else { format!("; failed: {failures}") },
        if skipped.is_empty() { String::new() } else { format!("; not restarted: {skipped}") },
        if st.pending.is_empty() { String::new() } else { format!("; {} still pending, retrying", st.pending.len()) },
    );
    for lead in leads(home, &r.restored) {
        let (sent, why) = crate::api::session_verbs::send_text(state, &lead, &note, true, crate::api::session_verbs::SendOrigin::Automation).await;
        if !sent {
            tracing::warn!(target: "tmux_watch", session = %lead, %why, verdict = "fleet_restored_lead_unnotified", "a lead was not told about the restore");
        }
    }
}

/// A lead may restore one named worker of its own team: the caller carries `leads`, and the two
/// share at least one other tag (the team).
pub(crate) fn lead_may_restore(home: &Path, lead: &str, target: &str) -> bool {
    let tags = |n: &str| -> std::collections::BTreeSet<String> {
        crate::config::parse_env_file(&home.join("sessions").join(format!("{n}.env")))
            .get("CC_TAGS")
            .map(|t| t.split(',').map(|x| x.trim().trim_matches('"').to_lowercase()).filter(|x| !x.is_empty()).collect())
            .unwrap_or_default()
    };
    let (mine, theirs) = (tags(lead), tags(target));
    mine.contains("leads") && mine.iter().any(|t| t != "leads" && theirs.contains(t))
}

/// `POST /api/fleet/restore {"only": NAME?, "dry_run": bool}`: start the recorded running set in
/// recovery order (always-alive, leads, rest). Idempotent. The owner may restore anything; a lead
/// only one named worker of its own team. An explicit `only` restores that worker even when it is
/// not in the record (`note`), never an archived or paused one. Used by boot recovery (#141).
/// Response (stable shape): {ok, measured, n_considered, order:[name], restored:[name],
/// failed:[{name, error}], skipped:[{name, why}]}; `dry_run` omits restored and failed.
pub(crate) async fn restore_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
    headers: axum::http::HeaderMap,
    body: Option<axum::Json<serde_json::Value>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let body = body.map(|b| b.0).unwrap_or_default();
    let home = crate::config::amux_home();
    let only = body["only"].as_str().filter(|s| !s.is_empty()).map(str::to_string);
    let worker = crate::api::alerts::hdr_worker(&headers);
    if !worker.is_empty() && !only.as_deref().is_some_and(|o| lead_may_restore(&home, &worker, o)) {
        return (axum::http::StatusCode::FORBIDDEN, axum::Json(serde_json::json!({"ok": false,
            "error": format!("fleet restore is owner-only; a lead may restore one named worker of its own team (--only), and {worker} may not restore {}", only.as_deref().unwrap_or("the fleet"))}))).into_response();
    }
    // A bare restore prefers the unrestored pre-outage snapshot: the live record shrinks as soon
    // as tmux is back (the keeper starts mrpopo first).
    let from_prev = only.is_none() && read_prev(&home).is_some();
    let recorded = read_prev(&home).or_else(|| read_running_set(&home));
    let mut note = None;
    let mut workers = match (&only, recorded) {
        // An explicit name is the caller's choice: restored even when not in the record
        // (a worker someone stopped on purpose), still never archived or paused.
        (Some(o), rec) => {
            if !rec.as_ref().is_some_and(|r| r.contains(o)) {
                note = Some("explicit --only, not in record");
            }
            vec![o.clone()]
        }
        (None, Some(rec)) => rec,
        (None, None) => {
            return axum::Json(serde_json::json!({"ok": false, "measured": false, "n_considered": 0, "error": "no running-set.json recorded yet"})).into_response();
        }
    };
    workers.dedup();
    let (ok, skipped) = eligible(&home, &workers);
    let order = restore_order(&home, &ok, &keepers());
    let skipped_json = |s: &[(String, &'static str)]| s.iter().map(|(n, w)| serde_json::json!({"name": n, "why": w})).collect::<Vec<_>>();
    if body["dry_run"].as_bool().unwrap_or(false) {
        return axum::Json(serde_json::json!({"ok": true, "dry_run": true, "measured": true, "n_considered": workers.len(),
            "order": order, "skipped": skipped_json(&skipped), "note": note})).into_response();
    }
    let r = restore_fleet(&home, &workers, |name| {
        let st = state.clone();
        async move { start(&st, &name).await }
    })
    .await;
    if from_prev && r.failed.is_empty() {
        mark_prev_restored(&home, now_secs());
    }
    tracing::info!(target: "tmux_watch", verdict = "fleet_restore_requested", measured = true, n_considered = workers.len(),
        restored = r.restored.len(), failed = r.failed.len(), skipped = r.skipped.len(), "fleet restore requested by the owner");
    axum::Json(serde_json::json!({"ok": r.failed.is_empty(), "measured": true, "n_considered": workers.len(), "order": order,
        "restored": r.restored,
        "failed": r.failed.iter().map(|(n, e)| serde_json::json!({"name": n, "error": e})).collect::<Vec<_>>(),
        "skipped": skipped_json(&r.skipped), "note": note, "source": if from_prev { "running-set.prev.json" } else { "running-set.json" }})).into_response()
}

pub fn spawn(state: AppState) -> super::PeriodicTask {
    super::spawn_periodic(JOB, TICK_SECS, move || {
        let st = state.clone();
        async move { tick(st).await }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn up(names: &[&str]) -> Census {
        Census::Up(names.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn a_fresh_boot_without_a_server_is_quiet() {
        let mut st = WatchState::default();
        assert_eq!(observe(&mut st, &Census::NoServer, 100.0, None), Decision::Quiet);
        assert!(!st.was_up);
    }

    #[test]
    fn the_first_no_server_after_up_is_one_loss_with_the_running_workers() {
        let mut st = WatchState::default();
        observe(&mut st, &up(&["a", "b"]), 100.0, None);
        assert_eq!(
            observe(&mut st, &Census::NoServer, 110.0, None),
            Decision::Lost { workers: vec!["a".into(), "b".into()], before: vec!["a".into(), "b".into()], restore: true, rebooted: false }
        );
        assert_eq!(observe(&mut st, &Census::NoServer, 125.0, None), Decision::Quiet, "one event, not one per tick");
        assert_eq!(st.lost_at, Some(110.0));
    }

    #[test]
    fn an_unreadable_answer_neither_alarms_nor_forgets() {
        let mut st = WatchState::default();
        observe(&mut st, &up(&["a"]), 100.0, None);
        assert_eq!(observe(&mut st, &Census::Unreadable("timed out".into()), 110.0, None), Decision::Quiet);
        assert!(st.was_up);
        assert!(matches!(observe(&mut st, &Census::NoServer, 120.0, None), Decision::Lost { .. }));
    }

    #[test]
    fn restores_are_at_most_one_per_ten_minutes() {
        let mut st = WatchState::default();
        observe(&mut st, &up(&["a"]), 0.0, None);
        observe(&mut st, &Census::NoServer, 10.0, None);
        assert!(pending_due(&st, 10.0));
        begin_restore(&mut st, 10.0);
        assert!(settle_pending(&mut st, &Restore { restored: vec!["a".into()], ..Default::default() }, 12.0));
        observe(&mut st, &up(&["a"]), 20.0, None);
        assert!(matches!(observe(&mut st, &Census::NoServer, 40.0, None), Decision::Lost { restore: false, .. }));
        assert_eq!(st.pending_next_at, Some(610.0));
        assert!(!pending_due(&st, 609.0));
        assert!(pending_due(&st, 610.0));
        // Failure retries also consume a restore slot; no 30-second retry loophole.
        begin_restore(&mut st, 610.0);
        assert!(!settle_pending(&mut st, &Restore::default(), 612.0));
        assert!(!pending_due(&st, 1211.0));
        assert!(pending_due(&st, 1212.0));
    }

    #[test]
    fn successful_restores_preserve_exponential_episode_backoff_across_restart() {
        let d = tempfile::tempdir().unwrap();
        let mut st = WatchState::default();
        for (loss, start, expected_losses) in [(10.0, 10.0, 1), (40.0, 610.0, 2), (640.0, 1810.0, 3)] {
            observe(&mut st, &up(&["a"]), loss - 1.0, None);
            observe(&mut st, &Census::NoServer, loss, None);
            if expected_losses > 1 {
                assert!(!pending_due(&st, start - 1.0));
            }
            assert!(pending_due(&st, start));
            begin_restore(&mut st, start);
            settle_pending(&mut st, &Restore { restored: vec!["a".into()], ..Default::default() }, start + 1.0);
            assert!(save_state(d.path(), &st));
            st = load_state(d.path());
            assert_eq!(st.episode_losses, expected_losses, "success/restart must preserve storm history");
            assert_eq!(st.last_restore_at, Some(start));
        }
    }

    #[test]
    fn a_fourth_loss_halts_starts_and_alerts_once_until_a_quiet_hour() {
        let d = tempfile::tempdir().unwrap();
        let mut st = WatchState::default();
        let mut notices = 0;
        let mut escalations = 0;
        for n in 0..6 {
            let now = 10.0 + f64::from(n) * 30.0;
            observe(&mut st, &up(&["a"]), now - 1.0, None);
            observe(&mut st, &Census::NoServer, now, None);
            notices += u32::from(claim_loss_notice(&mut st, now));
            escalations += u32::from(claim_storm_notice(&mut st, now));
            assert!(save_state(d.path(), &st));
            st = load_state(d.path());
            if n >= MAX_EPISODE_LOSSES - 1 {
                assert!(st.restore_halted);
                assert!(!pending_due(&st, now + 86_400.0), "waiting alone cannot unlatch the stop");
                assert_eq!(st.pending, vec!["a"], "retain pending evidence when halted");
            }
        }
        assert_eq!((notices, escalations), (1, 1), "one loss notice and one stop escalation per episode, including restarts");
        observe(&mut st, &up(&["a"]), 4000.0, None);
        observe(&mut st, &Census::NoServer, 4010.0, None);
        assert_eq!(st.episode_losses, 1);
        assert!(!st.restore_halted);
        assert!(claim_loss_notice(&mut st, 4010.0));
        assert!(!claim_storm_notice(&mut st, 4010.0));
    }

    #[test]
    fn a_restart_before_notification_preserves_one_loss_notice_and_one_halt_alert() {
        let d = tempfile::tempdir().unwrap();
        let mut st = WatchState::default();
        for n in 0..MAX_EPISODE_LOSSES {
            observe(&mut st, &up(&["a"]), f64::from(n) * 20.0, None);
            observe(&mut st, &Census::NoServer, f64::from(n) * 20.0 + 10.0, None);
        }
        assert!(save_state(d.path(), &st));
        st = load_state(d.path());
        assert_eq!(observe(&mut st, &Census::NoServer, 100.0, None), Decision::Quiet);
        assert!(claim_loss_notice(&mut st, 100.0));
        assert!(claim_storm_notice(&mut st, 100.0));
        assert!(save_state(d.path(), &st));
        st = load_state(d.path());
        assert!(!claim_loss_notice(&mut st, 115.0));
        assert!(!claim_storm_notice(&mut st, 115.0));
    }

    #[test]
    fn restarting_mid_pass_adopts_without_resetting_the_restore_slot() {
        let d = tempfile::tempdir().unwrap();
        let mut st = WatchState { pending: vec!["a".into()], ..Default::default() };
        begin_restore(&mut st, 10.0);
        assert!(save_state(d.path(), &st));
        st = load_state(d.path());
        assert!(pending_due(&st, 25.0));
        begin_restore(&mut st, 25.0);
        assert_eq!(st.last_restore_at, Some(10.0));
    }

    #[test]
    fn a_worker_stopped_before_the_loss_is_not_in_the_restore() {
        let mut st = WatchState::default();
        observe(&mut st, &up(&["a", "b"]), 0.0, None);
        observe(&mut st, &up(&["a"]), 15.0, None); // b stopped by a person
        assert_eq!(observe(&mut st, &Census::NoServer, 30.0, None), Decision::Lost { workers: vec!["a".into()], before: vec!["a".into()], restore: true, rebooted: false });
    }

    #[test]
    fn a_stopped_worker_with_a_live_shell_is_excluded_from_the_recorded_census() {
        let d = tempfile::tempdir().unwrap();
        let mut signals = crate::api::sessions_legacy::tests::signals();
        signals.running.extend(["amux-a".into(), "amux-stopped".into()]);
        signals.shell_only.insert("amux-stopped".into());
        let census = worker_census(up(&["a", "stopped"]), &signals);
        let (st, _, _, _) = begin_tick(d.path(), &census, 0.0, None);
        assert_eq!(st.workers, vec!["a"]);
        assert_eq!(read_running_set(d.path()), Some(vec!["a".into()]));
        begin_tick(d.path(), &Census::NoServer, 10.0, None);
        let (st, _, _, _) = begin_tick(d.path(), &Census::NoServer, 15.0, None);
        assert_eq!(st.pending, vec!["a"], "a surviving shell is never a restore candidate");
    }

    fn health(verdict: &str, measured: bool) -> crate::backend::tmux_health::Observation {
        crate::backend::tmux_health::Observation {
            measured, n_considered: 0, why_unmeasured: None,
            socket_path: "/private/fixture/socket".into(), responding_pid: None,
            probe_error: Some("Connection refused".into()), owners: vec![], verdict: verdict.into(),
        }
    }

    #[test]
    fn a_single_refusal_is_not_a_loss_and_two_agreeing_readings_five_seconds_apart_are() {
        let d = tempfile::tempdir().unwrap();
        begin_tick(d.path(), &up(&["a"]), 0.0, None);
        let down = corroborate_down(health("no_server", true));
        let (st, decision, _, census) = begin_tick(d.path(), &down, 10.0, None);
        assert_eq!(decision, Decision::Quiet);
        assert!(matches!(census, Census::Unreadable(_)));
        assert!(st.was_up && st.pending.is_empty());
        let (_, decision, _, _) = begin_tick(d.path(), &down, 14.99, None);
        assert_eq!(decision, Decision::Quiet, "less than five seconds cannot confirm");
        let (st, decision, _, census) = begin_tick(d.path(), &down, 15.0, None);
        assert_eq!(census, Census::NoServer);
        assert!(matches!(decision, Decision::Lost { .. }));
        assert_eq!(st.pending, vec!["a"]);
    }

    #[test]
    fn live_owner_or_unmeasured_health_never_confirms_loss_and_breaks_the_debounce() {
        for (verdict, measured) in [("live_server_unreachable", true), ("multiple_socket_owners", true), ("no_server", false), ("unmeasured", false), ("ok", true)] {
            let d = tempfile::tempdir().unwrap();
            begin_tick(d.path(), &up(&["a"]), 0.0, None);
            begin_tick(d.path(), &Census::NoServer, 10.0, None);
            let census = corroborate_down(health(verdict, measured));
            assert!(matches!(census, Census::Unreadable(_)), "{verdict}/{measured}");
            let (st, decision, _, _) = begin_tick(d.path(), &census, 15.0, None);
            assert_eq!(decision, Decision::Quiet);
            assert_eq!(st.down_since, None);
            let (_, decision, _, _) = begin_tick(d.path(), &Census::NoServer, 20.0, None);
            assert_eq!(decision, Decision::Quiet, "two down readings separated by an uncertain reading disagree");
        }
    }

    #[test]
    fn an_up_reading_breaks_the_down_confirmation_sequence() {
        let d = tempfile::tempdir().unwrap();
        begin_tick(d.path(), &up(&["a"]), 0.0, None);
        begin_tick(d.path(), &Census::NoServer, 10.0, None);
        begin_tick(d.path(), &up(&["a"]), 15.0, None);
        let (_, decision, _, _) = begin_tick(d.path(), &Census::NoServer, 20.0, None);
        assert_eq!(decision, Decision::Quiet);
    }

    #[test]
    fn census_reads_tmux_answers() {
        assert_eq!(parse_census(true, "amux-b\nmine\namux-a\n", ""), up(&["a", "b"]));
        assert_eq!(parse_census(false, "", "no server running on /tmp/tmux-501/default\n"), Census::NoServer);
        assert_eq!(parse_census(false, "", "error connecting to /tmp/tmux-501/default (No such file or directory)\n"), Census::NoServer);
        assert_eq!(parse_census(false, "", "error connecting to /tmp/x/probe.sock (Connection refused)\n"), Census::NoServer);
        assert!(matches!(parse_census(false, "", "lost server\n"), Census::Unreadable(_)));
    }

    #[test]
    fn only_live_unpaused_workers_are_eligible_and_leads_are_found() {
        let d = tempfile::tempdir().unwrap();
        let s = d.path().join("sessions");
        std::fs::create_dir_all(&s).unwrap();
        std::fs::write(s.join("a.env"), "CC_TAGS=\"dev,leads\"\n").unwrap();
        std::fs::write(s.join("b.env"), "CC_ARCHIVED=1\n").unwrap();
        std::fs::write(s.join("c.env"), "CC_PAUSED=true\n").unwrap();
        std::fs::write(s.join("e.env"), "CC_TAGS=dev\n").unwrap();
        let all: Vec<String> = ["a", "b", "c", "d", "e"].iter().map(|s| s.to_string()).collect();
        let (ok, skipped) = eligible(d.path(), &all);
        assert_eq!(ok, vec!["a".to_string(), "e".to_string()]);
        assert_eq!(skipped, vec![("b".into(), "archived"), ("c".into(), "paused"), ("d".into(), "no_env_file")]);
        assert_eq!(leads(d.path(), &ok), vec!["a".to_string()]);
    }

    #[tokio::test]
    async fn the_restore_starts_each_eligible_worker_and_counts_failures() {
        let d = tempfile::tempdir().unwrap();
        let s = d.path().join("sessions");
        std::fs::create_dir_all(&s).unwrap();
        for n in ["a", "b", "c"] {
            std::fs::write(s.join(format!("{n}.env")), "").unwrap();
        }
        std::fs::write(s.join("p.env"), "CC_PAUSED=1\n").unwrap();
        let started = std::sync::Mutex::new(Vec::new());
        let all: Vec<String> = ["a", "b", "c", "p"].iter().map(|s| s.to_string()).collect();
        let r = restore_fleet(d.path(), &all, |name| {
            started.lock().unwrap().push(name.clone());
            async move { if name == "b" { (false, "tmux refused".into()) } else { (true, "started".into()) } }
        })
        .await;
        assert_eq!(*started.lock().unwrap(), vec!["a", "b", "c"], "a paused worker is never started");
        assert_eq!(r.restored, vec!["a".to_string(), "c".to_string()], "one failure does not stop the rest");
        assert_eq!(r.failed, vec![("b".to_string(), "tmux refused".to_string())]);
        assert_eq!(r.skipped, vec![("p".to_string(), "paused")]);
    }

    #[test]
    fn a_host_boot_after_the_last_observation_is_a_reboot_not_a_kill() {
        let mut st = WatchState::default();
        observe(&mut st, &up(&["a"]), 100.0, Some(10.0));
        assert!(matches!(observe(&mut st, &Census::NoServer, 500.0, Some(400.0)), Decision::Lost { rebooted: true, restore: true, .. }));
        let mut st = WatchState::default();
        observe(&mut st, &up(&["a"]), 100.0, Some(10.0));
        assert!(matches!(observe(&mut st, &Census::NoServer, 110.0, Some(10.0)), Decision::Lost { rebooted: false, .. }),
            "the same boot means the server was killed");
    }

    #[test]
    fn a_reboot_is_caught_even_when_tmux_is_already_up_at_the_first_look() {
        let mut st = WatchState::default();
        observe(&mut st, &up(&["mrpopo", "a", "b"]), 100.0, Some(10.0));
        // After the reboot something (a login, the keeper) already started mrpopo.
        assert_eq!(
            observe(&mut st, &up(&["mrpopo"]), 900.0, Some(500.0)),
            Decision::Lost { workers: vec!["a".into(), "b".into()], before: vec!["mrpopo".into(), "a".into(), "b".into()], restore: true, rebooted: true }
        );
        assert_eq!(observe(&mut st, &up(&["mrpopo"]), 915.0, Some(500.0)), Decision::Quiet, "one event per boot");
    }

    #[test]
    fn the_pre_outage_snapshot_is_kept_until_a_restore_completes() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("sessions")).unwrap();
        assert!(snapshot_prev(d.path(), &["mrpopo".into(), "a".into(), "b".into()], Some(500.0), 600.0));
        write_running_set(d.path(), &["mrpopo".into()], 610.0); // tmux back, the live record shrinks
        assert!(!snapshot_prev(d.path(), &["mrpopo".into()], Some(500.0), 620.0), "an unrestored snapshot is never overwritten");
        assert_eq!(read_prev(d.path()), Some(vec!["mrpopo".to_string(), "a".to_string(), "b".to_string()]));
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(d.path().join("running-set.prev.json")).unwrap()).unwrap();
        assert_eq!((v["boot_epoch"].clone(), v["updated_at"].clone()), (serde_json::json!(500), serde_json::json!(600)));
        mark_prev_restored(d.path(), 700.0);
        assert_eq!(read_prev(d.path()), None);
        assert!(snapshot_prev(d.path(), &["x".into()], None, 800.0), "a restored snapshot may be replaced by the next loss");
    }

    #[test]
    fn a_loss_is_pending_work_that_survives_a_restart_and_retries_until_done() {
        let d = tempfile::tempdir().unwrap();
        let mut st = WatchState::default();
        observe(&mut st, &up(&["a", "b"]), 0.0, None);
        observe(&mut st, &Census::NoServer, 10.0, None);
        save_state(d.path(), &st);
        // The server restarts here, before any start was attempted.
        let mut st = load_state(d.path());
        assert_eq!(st.pending, vec!["a".to_string(), "b".to_string()]);
        assert!(pending_due(&st, 25.0), "the restored process still owes the restore");
        let r = Restore { restored: vec!["a".into()], failed: vec![("b".into(), "busy".into())], skipped: vec![] };
        assert!(!settle_pending(&mut st, &r, 25.0));
        assert_eq!(st.pending, vec!["b".to_string()]);
        assert!(!pending_due(&st, 624.0) && pending_due(&st, 625.0), "a failed start waits at least ten minutes");
        let r = Restore { restored: vec![], failed: vec![], skipped: vec![("b".into(), "paused")] };
        assert!(settle_pending(&mut st, &r, 630.0), "a worker that became ineligible leaves the set");
        assert_eq!((st.pending_attempts, st.pending_next_at), (0, None));
    }

    #[test]
    fn a_loss_saved_before_the_keeper_survives_a_crash_after_mrpopo_starts() {
        // The boundary review round 2 named: the loss is saved, the keeper starts mrpopo, amux
        // dies before anything else, and on restart tmux shows only mrpopo on the same boot.
        let d = tempfile::tempdir().unwrap();
        let (_, observed, _, _) = begin_tick(d.path(), &up(&["a", "mrpopo"]), 0.0, Some(-10.0));
        assert_eq!(observed, Decision::Quiet);
        begin_tick(d.path(), &Census::NoServer, 5.0, Some(-10.0));
        let (_, loss, _, _) = begin_tick(d.path(), &Census::NoServer, 10.0, Some(-10.0));
        assert!(matches!(loss, Decision::Lost { .. }));
        // Simulated crash before keep_alive: discard the returned in-memory state.
        let mut st = load_state(d.path());
        assert_eq!(st.pending, vec!["a", "mrpopo"], "the production pre-start path persisted the loss");
        assert_eq!(observe(&mut st, &up(&["mrpopo"]), 25.0, Some(-10.0)), Decision::Quiet);
        assert!(st.pending.contains(&"a".to_string()), "a is still owed a restore: {:?}", st.pending);
        assert!(pending_due(&st, 25.0));
    }

    #[test]
    fn tick_persists_the_loss_before_the_keeper_can_start() {
        let source = include_str!("tmux_watch.rs");
        let tick = source.split("async fn tick(state: AppState) {").nth(1).expect("tick source")
            .split("\nasync fn drive_pending(").next().expect("tick end");
        let begin = tick.find("begin_tick(&home, &census, now,").expect("production tick calls begin_tick");
        let keeper = tick.find("keep_alive(&state, &home,").expect("production tick calls keep_alive");
        assert!(begin < keeper, "production tick must persist the loss before starting a keeper");
    }

    #[test]
    fn an_unwritable_state_file_is_reported_not_assumed_durable() {
        let d = tempfile::tempdir().unwrap();
        let missing = d.path().join("no-such-dir");
        assert!(!save_state(&missing, &WatchState::default()));
    }

    #[test]
    fn a_second_loss_inside_the_gap_backs_off_instead_of_looping() {
        let mut st = WatchState::default();
        observe(&mut st, &up(&["a"]), 0.0, None);
        observe(&mut st, &Census::NoServer, 10.0, None);
        begin_restore(&mut st, 10.0);
        settle_pending(&mut st, &Restore { restored: vec!["a".into()], ..Default::default() }, 12.0);
        observe(&mut st, &up(&["a"]), 20.0, None);
        observe(&mut st, &Census::NoServer, 40.0, None);
        assert_eq!(st.pending, vec!["a"]);
        assert_eq!(st.pending_next_at, Some(610.0));
        assert!(!pending_due(&st, 70.0), "a successful previous pass cannot reopen the 30-second loop");
        observe(&mut st, &up(&["a"]), 75.0, None);
        observe(&mut st, &Census::NoServer, 80.0, None);
        assert_eq!(st.pending_next_at, Some(1210.0), "another loss doubles the delay even before the retry");
    }

    #[test]
    fn recovery_order_is_keepers_then_leads_then_the_rest() {
        let d = tempfile::tempdir().unwrap();
        let s = d.path().join("sessions");
        std::fs::create_dir_all(&s).unwrap();
        for (n, tags) in [("zeta-lead", "dev,leads"), ("alpha-lead", "leads"), ("mrpopo", "leads"), ("b", "dev"), ("a", "dev")] {
            std::fs::write(s.join(format!("{n}.env")), format!("CC_TAGS=\"{tags}\"\n")).unwrap();
        }
        let all: Vec<String> = ["b", "zeta-lead", "a", "mrpopo", "alpha-lead"].iter().map(|s| s.to_string()).collect();
        assert_eq!(restore_order(d.path(), &all, &["mrpopo".into()]), vec!["mrpopo", "alpha-lead", "zeta-lead", "a", "b"]);
        assert_eq!(restore_order(d.path(), &["a".into()], &["mrpopo".into()]), vec!["a"], "a keeper not in the set is not added here");
    }

    #[test]
    fn the_keeper_backs_off_and_resets_on_success() {
        assert_eq!([1, 2, 3, 4, 6, 20].map(keeper_backoff), [30.0, 60.0, 120.0, 240.0, 600.0, 600.0]);
        let mut st = WatchState::default();
        assert!(keeper_due(&st, false, 0.0));
        assert!(!keeper_due(&st, true, 0.0), "a running keeper is left alone");
        record_keeper(&mut st, false, 0.0);
        assert!(!keeper_due(&st, false, 29.0) && keeper_due(&st, false, 30.0));
        record_keeper(&mut st, false, 30.0);
        assert_eq!(st.keeper_next_at, Some(90.0));
        record_keeper(&mut st, true, 95.0);
        assert_eq!((st.keeper_failures, st.keeper_next_at), (0, None));
    }

    #[test]
    fn the_running_set_records_names_and_tags_for_boot_recovery() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("sessions")).unwrap();
        std::fs::write(d.path().join("sessions").join("mrpopo.env"), "CC_TAGS=leads\n").unwrap();
        write_running_set(d.path(), &["mrpopo".into(), "x".into()], 1234.0);
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(d.path().join("running-set.json")).unwrap()).unwrap();
        assert_eq!(v, serde_json::json!({"updated_at": 1234, "workers": [{"name": "mrpopo", "tags": ["leads"]}, {"name": "x", "tags": []}]}));
        assert_eq!(read_running_set(d.path()), Some(vec!["mrpopo".to_string(), "x".to_string()]));
    }

    async fn call(app: &axum::Router, worker: Option<&str>, body: serde_json::Value) -> (u16, serde_json::Value) {
        use tower::ServiceExt;
        let mut req = axum::http::Request::post("/api/fleet/restore").header("content-type", "application/json");
        if let Some(w) = worker {
            req = req.header("X-Amux-Worker", w);
        }
        let resp = app.clone().oneshot(req.body(axum::body::Body::from(body.to_string())).unwrap()).await.unwrap();
        let code = resp.status().as_u16();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        (code, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
    }

    #[tokio::test]
    async fn the_restore_endpoint_is_owner_only_and_dry_runs_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::api::settings::test_env::set_home(dir.path());
        let s = dir.path().join("sessions");
        std::fs::create_dir_all(&s).unwrap();
        for (n, body) in [("mrpopo", "CC_TAGS=leads\n"), ("dev-lead", "CC_TAGS=\"dev,leads\"\n"), ("w1", ""), ("old", "CC_ARCHIVED=1\n")] {
            std::fs::write(s.join(format!("{n}.env")), body).unwrap();
        }
        let app = axum::Router::new()
            .route("/api/fleet/restore", axum::routing::post(restore_handler))
            .with_state(crate::api::standing_approvals::tests::test_state());
        let (code, v) = call(&app, None, serde_json::json!({"dry_run": true})).await;
        assert_eq!((code, v["measured"].clone()), (200, serde_json::json!(false)), "no recorded set yet is unmeasured, not empty: {v}");
        write_running_set(dir.path(), &["w1".into(), "old".into(), "dev-lead".into(), "mrpopo".into()], 1.0);
        let (code, v) = call(&app, Some("w1"), serde_json::json!({"dry_run": true})).await;
        assert_eq!(code, 403, "a worker cannot restore the fleet: {v}");
        let (code, v) = call(&app, None, serde_json::json!({"dry_run": true})).await;
        assert_eq!(code, 200, "{v}");
        assert_eq!(v["order"], serde_json::json!(["mrpopo", "dev-lead", "w1"]));
        assert_eq!(v["skipped"], serde_json::json!([{"name": "old", "why": "archived"}]));
        snapshot_prev(dir.path(), &["w1".into(), "dev-lead".into()], None, 2.0);
        let (_, v) = call(&app, None, serde_json::json!({"dry_run": true})).await;
        assert_eq!(v["order"], serde_json::json!(["dev-lead", "w1"]), "a bare restore prefers the unrestored snapshot: {v}");
        mark_prev_restored(dir.path(), 3.0);
        let (_, v) = call(&app, None, serde_json::json!({"dry_run": true, "only": "w1"})).await;
        assert_eq!(v["order"], serde_json::json!(["w1"]));
        std::fs::write(s.join("stopped.env"), "CC_TAGS=dev\n").unwrap();
        let (_, v) = call(&app, None, serde_json::json!({"dry_run": true, "only": "stopped"})).await;
        assert_eq!((v["order"].clone(), v["note"].clone()), (serde_json::json!(["stopped"]), serde_json::json!("explicit --only, not in record")),
            "an explicitly named worker is restored even when it was stopped on purpose: {v}");
        let (_, v) = call(&app, None, serde_json::json!({"dry_run": true, "only": "old"})).await;
        assert_eq!(v["order"], serde_json::json!([]), "never an archived one, even by name: {v}");
        let (code, _) = call(&app, Some("dev-lead"), serde_json::json!({"dry_run": true, "only": "stopped"})).await;
        assert_eq!(code, 200, "a lead may restore a worker of its own team by name");
        let (code, _) = call(&app, Some("dev-lead"), serde_json::json!({"dry_run": true})).await;
        assert_eq!(code, 403, "a lead may not restore the whole fleet");
        std::fs::write(s.join("other.env"), "CC_TAGS=re-ops\n").unwrap();
        let (code, _) = call(&app, Some("dev-lead"), serde_json::json!({"dry_run": true, "only": "other"})).await;
        assert_eq!(code, 403, "nor a worker of another team");
    }

    #[test]
    fn state_survives_a_server_restart() {
        let d = tempfile::tempdir().unwrap();
        let mut st = WatchState::default();
        observe(&mut st, &up(&["a"]), 5.0, None);
        save_state(d.path(), &st);
        let mut again = load_state(d.path());
        assert!(matches!(observe(&mut again, &Census::NoServer, 9.0, None), Decision::Lost { .. }),
            "a restart during an outage still knows the server was up");
    }

    /// The real tmux on a private `-S` socket in this test's temp dir, never the live server: a
    /// running server reads Up, and once its only session ends (tmux exits on its own; nothing
    /// here sends kill-server or kill-session) it reads NoServer. Every tmux spawn removes TMUX and
    /// TMUX_PANE and replaces TMUX_TMPDIR (2026-10-17 stop-order).
    #[tokio::test]
    async fn the_census_reads_a_private_tmux_server() {
        if std::process::Command::new("tmux").arg("-V").output().is_err() {
            eprintln!("skip: tmux not installed");
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let sock = d.path().join("probe.sock");
        let tmux = |args: &[&str]| {
            std::process::Command::new("tmux")
                .arg("-S").arg(&sock).args(args)
                .env_remove("TMUX").env_remove("TMUX_PANE").env("TMUX_TMPDIR", d.path())
                .status().unwrap()
        };
        assert_eq!(read_census(Some(&sock)).await, Census::NoServer, "nothing runs on the private socket yet");
        assert!(tmux(&["new-session", "-d", "-s", "amux-probe", "sleep 3"]).success());
        assert_eq!(read_census(Some(&sock)).await, up(&["probe"]));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut last = read_census(Some(&sock)).await;
        while last != Census::NoServer && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            last = read_census(Some(&sock)).await;
        }
        assert_eq!(last, Census::NoServer, "the server exits with its last session");
    }
}
