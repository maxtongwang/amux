// One tracker issue per stable fault key, behind an interface — detectors never file twice for one fault (refs ai-config#409).
//! A detector's alert is a FAULT, not an event. Its report is therefore keyed on
//! the fault (kind plus subject, never a count or timestamp) and kept current
//! while the fault lasts:
//!
//!   - an open issue with the key exists -> [`next_state`] + `update` (last seen,
//!     current value, sightings, peak), no new issue;
//!   - the fault clears                  -> `close` with a note (cleared at, last value);
//!   - no open issue                     -> the detector opens one.
//!
//! The tracker is an interface ([`FaultTracker`]): [`GithubIssueTracker`] is the
//! production adapter (`gh`, repo [`ALERT_REPO`]), [`FakeTracker`] the in-memory
//! one every test uses. The key rides in the issue body as a stable HTML-comment
//! marker so lookup is by key, never by title. Bodies are sanitized before they
//! leave the machine: the issue repo is not the place for private paths, hosts
//! or contact data.
//!
//! Measured trigger: "N system jobs unhealthy at once" opened ~50 cards as N
//! moved 2-13 because N rode in the signature; `disk_watch` opened one card per
//! scan id for one standing condition.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

/// Repo alert issues are filed in (overridable with `AMUX_ALERT_REPO`). amux
/// itself is public, so alerts never go there.
pub const ALERT_REPO: &str = "maxtongwang/ai-config";
/// Labels every alert issue carries.
pub const ALERT_LABELS: [&str; 2] = ["kind:alert", "amux"];
/// How long a `gh issue list` result is reused. A 120s autofix tick would
/// otherwise be 720 API calls a day; our own writes update the cache in place.
const LIST_TTL_SECS: u64 = 300;
/// Cap on open alert issues fetched per listing.
const LIST_LIMIT: &str = "300";

/// Marker line that carries the fault key inside the issue body.
pub const KEY_MARKER_PREFIX: &str = "<!-- amux-fault-key: ";
const KEY_MARKER_SUFFIX: &str = " -->";
/// Separator before the state block; also the marker the parser looks for.
pub const STATE_MARK: &str =
    "\n\n--- fault state (kept current by amux; edits below are overwritten) ---\n";

/// One tracked fault as the tracker holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedFault {
    /// Tracker-native id (GitHub issue number).
    pub id: String,
    pub title: String,
    pub body: String,
}

impl TrackedFault {
    pub fn key(&self) -> Option<String> {
        key_of_body(&self.body)
    }
    pub fn state(&self) -> Option<FaultState> {
        FaultState::parse(&self.body)
    }
}

/// The seam between detectors and wherever alerts are tracked.
pub trait FaultTracker: Send + Sync {
    /// Every open alert issue.
    fn list_open(&self) -> anyhow::Result<Vec<TrackedFault>>;
    /// Open a new issue; returns its id.
    fn open(&self, title: &str, body: &str) -> anyhow::Result<String>;
    /// Replace title and body of an open issue.
    fn update(&self, id: &str, title: &str, body: &str) -> anyhow::Result<()>;
    /// Close an open issue after posting `note` on it.
    fn close(&self, id: &str, note: &str) -> anyhow::Result<()>;
}

/// What the issue says about the fault right now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FaultState {
    pub first_seen: i64,
    pub last_seen: i64,
    /// Observations recorded (each distinct `observation` id counts once).
    pub sightings: u64,
    /// Human-readable current value, e.g. the current title.
    pub current: String,
    /// Largest numeric value seen (the detector's `count`).
    pub peak: u64,
    /// Id of the last recorded observation, so re-reading one scan or one
    /// occurrence batch does not inflate `sightings`.
    pub last_observation: String,
}

impl FaultState {
    pub fn render(&self) -> String {
        format!(
            "{STATE_MARK}first_seen: {}\nlast_seen: {}\nsightings: {}\npeak: {}\ncurrent: {}\nlast_observation: {}\n",
            self.first_seen,
            self.last_seen,
            self.sightings,
            self.peak,
            one_line(&self.current),
            one_line(&self.last_observation),
        )
    }

    /// `None` when the body carries no state block.
    pub fn parse(body: &str) -> Option<Self> {
        let block = body.split_once(STATE_MARK)?.1;
        let field = |key: &str| {
            block
                .lines()
                .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix(": ")))
        };
        Some(Self {
            first_seen: field("first_seen")?.trim().parse().ok()?,
            last_seen: field("last_seen")?.trim().parse().ok()?,
            sightings: field("sightings")?.trim().parse().ok()?,
            peak: field("peak")?.trim().parse().ok()?,
            current: field("current")?.to_string(),
            last_observation: field("last_observation").unwrap_or("").to_string(),
        })
    }
}

fn one_line(s: &str) -> String {
    s.replace('\n', " ")
}

/// The body with its state block removed.
pub fn strip_state(body: &str) -> &str {
    body.split_once(STATE_MARK).map_or(body, |(head, _)| head)
}

/// The fault key from a body's marker line.
pub fn key_of_body(body: &str) -> Option<String> {
    let rest = body.split_once(KEY_MARKER_PREFIX)?.1;
    let key = rest.split_once(KEY_MARKER_SUFFIX)?.0;
    (!key.is_empty()).then(|| key.to_string())
}

/// A first body: marker, sanitized text, first state block.
pub fn initial_body(key: &str, text: &str, title: &str, peak: u64, observation: &str, now: i64) -> String {
    let st = FaultState {
        first_seen: now,
        last_seen: now,
        sightings: 1,
        current: title.to_string(),
        peak,
        last_observation: observation.to_string(),
    };
    format!(
        "{KEY_MARKER_PREFIX}{key}{KEY_MARKER_SUFFIX}\n{}{}",
        sanitize_public(text),
        st.render()
    )
}

/// The state after seeing the fault again, or `None` when `observation` is the
/// one already recorded (a re-read, not a new sighting).
pub fn next_state(
    prev: Option<FaultState>,
    fallback_first_seen: i64,
    title: &str,
    value: u64,
    observation: &str,
    now: i64,
) -> Option<FaultState> {
    if prev
        .as_ref()
        .is_some_and(|s| !observation.is_empty() && s.last_observation == observation)
    {
        return None;
    }
    Some(FaultState {
        first_seen: prev.as_ref().map_or(fallback_first_seen, |s| s.first_seen),
        last_seen: now,
        sightings: prev.as_ref().map_or(1, |s| s.sightings) + 1,
        current: title.to_string(),
        peak: prev.as_ref().map_or(0, |s| s.peak).max(value),
        last_observation: observation.to_string(),
    })
}

/// `body` with its state block replaced by `st`.
pub fn with_state(body: &str, st: &FaultState) -> String {
    format!("{}{}", strip_state(body), st.render())
}

/// Record a repeat sighting on `fault`. Returns whether anything was written.
pub fn record_sighting(
    tracker: &dyn FaultTracker,
    fault: &TrackedFault,
    title: &str,
    value: u64,
    observation: &str,
    now: i64,
) -> anyhow::Result<Option<TrackedFault>> {
    let Some(st) = next_state(fault.state(), now, title, value, observation, now) else {
        return Ok(None);
    };
    let body = with_state(&fault.body, &st);
    let title = sanitize_public(title);
    tracker.update(&fault.id, &title, &body)?;
    tracing::info!(
        issue = %fault.id, sightings = st.sightings, current = %title,
        "fault issue updated: same fault seen again, no new issue"
    );
    Ok(Some(TrackedFault { id: fault.id.clone(), title, body }))
}

/// The note posted when a fault clears.
pub fn clear_note(fault: &TrackedFault, now: i64) -> String {
    let st = fault.state();
    let last_value = st.as_ref().map_or(fault.title.as_str(), |s| s.current.as_str());
    let last_seen = st.as_ref().map_or(now, |s| s.last_seen);
    format!(
        "Fault cleared at {}: not seen since {} (last value: {}). Closed automatically; if it \
         returns, a new issue opens.",
        local_when(now),
        local_when(last_seen),
        last_value
    )
}

fn local_when(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "an unknown time".into())
}

/// Strip what must not reach a shared tracker: home-directory paths, e-mail
/// addresses, IPv4 addresses, and tailnet/LAN hostnames.
pub fn sanitize_public(text: &str) -> String {
    static RULES: OnceLock<Vec<(regex::Regex, &'static str)>> = OnceLock::new();
    let rules = RULES.get_or_init(|| {
        [
            (r"/(?:Users|home)/[A-Za-z0-9._-]+", "~"),
            (r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}", "<email>"),
            (r"\b\d{1,3}(?:\.\d{1,3}){3}\b", "<ip>"),
            (r"\b[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.(?:ts\.net|local|lan|internal)\b", "<host>"),
        ]
        .into_iter()
        .map(|(p, r)| (regex::Regex::new(p).expect("static sanitize pattern"), r))
        .collect()
    });
    rules
        .iter()
        .fold(text.to_string(), |acc, (re, rep)| re.replace_all(&acc, *rep).into_owned())
}

// ---------------------------------------------------------------------------
// Production adapter: GitHub issues through the `gh` CLI.
// ---------------------------------------------------------------------------

/// Files alert issues in [`ALERT_REPO`] with `gh`. Never falls back to another
/// tracker: when `gh` fails the caller gets the error and files nothing.
pub struct GithubIssueTracker {
    repo: String,
    cache: Mutex<Option<(std::time::Instant, Vec<TrackedFault>)>>,
    labels_ready: OnceLock<()>,
}

impl GithubIssueTracker {
    pub fn from_env() -> Self {
        Self {
            repo: std::env::var("AMUX_ALERT_REPO")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| ALERT_REPO.to_string()),
            cache: Mutex::new(None),
            labels_ready: OnceLock::new(),
        }
    }

    /// Run `gh` with separate arguments (never a shell) and optional stdin.
    fn gh(args: &[&str], stdin: Option<&str>) -> anyhow::Result<String> {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let mut child = Command::new("gh")
            .args(args)
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| anyhow::anyhow!("gh could not start: {e}"))?;
        if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
            pipe.write_all(text.as_bytes())?;
        }
        let out = child.wait_with_output()?;
        if !out.status.success() {
            anyhow::bail!(
                "gh {} failed ({}): {}",
                args.first().copied().unwrap_or(""),
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn ensure_labels(&self) -> anyhow::Result<()> {
        if self.labels_ready.get().is_some() {
            return Ok(());
        }
        for label in ALERT_LABELS {
            Self::gh(&["label", "create", label, "--repo", &self.repo, "--force"], None)?;
        }
        let _ = self.labels_ready.set(());
        Ok(())
    }

    fn cached(&self) -> Option<Vec<TrackedFault>> {
        let g = self.cache.lock().ok()?;
        let (at, list) = g.as_ref()?;
        (at.elapsed().as_secs() < LIST_TTL_SECS).then(|| list.clone())
    }

    fn mutate_cache(&self, f: impl FnOnce(&mut Vec<TrackedFault>)) {
        if let Ok(mut g) = self.cache.lock() {
            if let Some((_, list)) = g.as_mut() {
                f(list);
            }
        }
    }
}

/// Parse `gh issue list --json number,title,body`.
pub fn parse_issue_list(json: &str) -> anyhow::Result<Vec<TrackedFault>> {
    let v: serde_json::Value = serde_json::from_str(json)?;
    let arr = v.as_array().ok_or_else(|| anyhow::anyhow!("gh issue list: not an array"))?;
    Ok(arr
        .iter()
        .filter_map(|i| {
            Some(TrackedFault {
                id: i.get("number")?.as_u64()?.to_string(),
                title: i.get("title")?.as_str()?.to_string(),
                body: i.get("body").and_then(|b| b.as_str()).unwrap_or("").to_string(),
            })
        })
        .collect())
}

impl FaultTracker for GithubIssueTracker {
    fn list_open(&self) -> anyhow::Result<Vec<TrackedFault>> {
        if let Some(list) = self.cached() {
            return Ok(list);
        }
        let out = Self::gh(
            &[
                "issue", "list", "--repo", &self.repo,
                "--label", ALERT_LABELS[0], "--label", ALERT_LABELS[1],
                "--state", "open", "--limit", LIST_LIMIT,
                "--json", "number,title,body",
            ],
            None,
        )?;
        let list = parse_issue_list(&out)?;
        if let Ok(mut g) = self.cache.lock() {
            *g = Some((std::time::Instant::now(), list.clone()));
        }
        Ok(list)
    }

    fn open(&self, title: &str, body: &str) -> anyhow::Result<String> {
        self.ensure_labels()?;
        let out = Self::gh(
            &[
                "issue", "create", "--repo", &self.repo, "--title", title,
                "--label", ALERT_LABELS[0], "--label", ALERT_LABELS[1],
                "--body-file", "-",
            ],
            Some(body),
        )?;
        let id = out
            .trim()
            .rsplit('/')
            .next()
            .filter(|n| n.parse::<u64>().is_ok())
            .ok_or_else(|| anyhow::anyhow!("gh issue create returned no issue url: {out:?}"))?
            .to_string();
        let fault = TrackedFault { id: id.clone(), title: title.to_string(), body: body.to_string() };
        self.mutate_cache(|l| l.push(fault));
        Ok(id)
    }

    fn update(&self, id: &str, title: &str, body: &str) -> anyhow::Result<()> {
        Self::gh(
            &["issue", "edit", id, "--repo", &self.repo, "--title", title, "--body-file", "-"],
            Some(body),
        )?;
        let (t, b) = (title.to_string(), body.to_string());
        self.mutate_cache(|l| {
            if let Some(f) = l.iter_mut().find(|f| f.id == id) {
                f.title = t;
                f.body = b;
            }
        });
        Ok(())
    }

    fn close(&self, id: &str, note: &str) -> anyhow::Result<()> {
        Self::gh(&["issue", "comment", id, "--repo", &self.repo, "--body-file", "-"], Some(note))?;
        Self::gh(&["issue", "close", id, "--repo", &self.repo, "--reason", "completed"], None)?;
        self.mutate_cache(|l| l.retain(|f| f.id != id));
        Ok(())
    }
}

/// The production tracker, shared by every detector.
pub fn production() -> Arc<dyn FaultTracker> {
    static T: OnceLock<Arc<GithubIssueTracker>> = OnceLock::new();
    T.get_or_init(|| Arc::new(GithubIssueTracker::from_env())).clone()
}

/// The tracker for this process's store. Tests get an in-memory fake per
/// database file, so parallel tests never share issues and a "restarted"
/// state on the same file sees the same issues, as GitHub would show.
pub fn tracker_for(state: &crate::api::AppState) -> Arc<dyn FaultTracker> {
    #[cfg(test)]
    {
        FakeTracker::for_db(state.store.db_path())
    }
    #[cfg(not(test))]
    {
        let _ = state;
        production()
    }
}

// ---------------------------------------------------------------------------
// Test double.
// ---------------------------------------------------------------------------

/// In-memory tracker. Records every call so tests can assert on what a
/// detector did and did not open.
#[derive(Default)]
pub struct FakeTracker {
    inner: Mutex<FakeInner>,
}

#[derive(Default)]
struct FakeInner {
    next: u64,
    open: BTreeMap<u64, TrackedFault>,
    closed: Vec<(TrackedFault, String)>,
    opened_total: u64,
    fail: bool,
}

impl FakeTracker {
    #[cfg(test)]
    pub fn for_db(path: &std::path::Path) -> Arc<dyn FaultTracker> {
        Self::typed_for_db(path)
    }

    #[cfg(test)]
    pub fn typed_for_db(path: &std::path::Path) -> Arc<FakeTracker> {
        static REG: OnceLock<Mutex<BTreeMap<std::path::PathBuf, Arc<FakeTracker>>>> = OnceLock::new();
        let mut g = REG.get_or_init(Default::default).lock().unwrap();
        g.entry(path.to_path_buf()).or_default().clone()
    }

    pub fn open_issues(&self) -> Vec<TrackedFault> {
        self.inner.lock().unwrap().open.values().cloned().collect()
    }
    pub fn closed_issues(&self) -> Vec<(TrackedFault, String)> {
        self.inner.lock().unwrap().closed.clone()
    }
    /// Total issues ever opened (open + closed).
    pub fn opened_total(&self) -> u64 {
        self.inner.lock().unwrap().opened_total
    }
    /// Make every call fail, as an unreachable GitHub would.
    pub fn set_failing(&self, fail: bool) {
        self.inner.lock().unwrap().fail = fail;
    }
    fn check(&self) -> anyhow::Result<()> {
        if self.inner.lock().unwrap().fail {
            anyhow::bail!("fake tracker unavailable");
        }
        Ok(())
    }
}

impl FaultTracker for FakeTracker {
    fn list_open(&self) -> anyhow::Result<Vec<TrackedFault>> {
        self.check()?;
        Ok(self.open_issues())
    }
    fn open(&self, title: &str, body: &str) -> anyhow::Result<String> {
        self.check()?;
        let mut g = self.inner.lock().unwrap();
        g.next += 1;
        g.opened_total += 1;
        let id = g.next;
        g.open.insert(id, TrackedFault { id: id.to_string(), title: title.into(), body: body.into() });
        Ok(id.to_string())
    }
    fn update(&self, id: &str, title: &str, body: &str) -> anyhow::Result<()> {
        self.check()?;
        let mut g = self.inner.lock().unwrap();
        let f = g
            .open
            .get_mut(&id.parse::<u64>()?)
            .ok_or_else(|| anyhow::anyhow!("no open issue {id}"))?;
        f.title = title.into();
        f.body = body.into();
        Ok(())
    }
    fn close(&self, id: &str, note: &str) -> anyhow::Result<()> {
        self.check()?;
        let mut g = self.inner.lock().unwrap();
        let f = g
            .open
            .remove(&id.parse::<u64>()?)
            .ok_or_else(|| anyhow::anyhow!("no open issue {id}"))?;
        g.closed.push((f, note.to_string()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_round_trips_with_key_and_one_state_block() {
        let b = initial_body("silent|system-job|ROLLUP", "evidence\n", "6 jobs", 6, "t1", 100);
        assert_eq!(key_of_body(&b).as_deref(), Some("silent|system-job|ROLLUP"));
        let st = FaultState::parse(&b).expect("parses");
        assert_eq!((st.first_seen, st.sightings, st.peak), (100, 1, 6));
        let again = with_state(&b, &next_state(Some(st), 0, "7 jobs", 7, "t2", 200).unwrap());
        assert_eq!(again.matches(STATE_MARK).count(), 1);
        assert_eq!(FaultState::parse(&again).unwrap().peak, 7);
    }

    #[test]
    fn a_re_read_observation_is_not_a_new_sighting() {
        let b = initial_body("k", "e", "t", 1, "scan-1", 100);
        assert!(next_state(FaultState::parse(&b), 0, "t", 1, "scan-1", 200).is_none());
    }

    #[test]
    fn private_paths_hosts_and_contacts_are_stripped() {
        let s = sanitize_public("see /Users/mwang/.amux/x on mac.tail1234.ts.net 10.0.0.5 mail a@b.com");
        assert!(!s.contains("mwang") && !s.contains("ts.net") && !s.contains("10.0.0.5") && !s.contains("a@b.com"), "{s}");
        assert!(s.contains("~/.amux/x"));
    }

    #[test]
    fn gh_issue_list_json_parses() {
        let l = parse_issue_list(r#"[{"number":7,"title":"t","body":"b"},{"number":"bad"}]"#).unwrap();
        assert_eq!(l, vec![TrackedFault { id: "7".into(), title: "t".into(), body: "b".into() }]);
    }
}
