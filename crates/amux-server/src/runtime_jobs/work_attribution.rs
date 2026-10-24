//! Response-time work provenance shared by every provider. Historical resolution
//! uses bounded recorded intervals, never the worker's current card/config.
use crate::db::{SharedStore, WriteOutcome};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Historical claims expire after a day without further evidence.
const MAX_WINDOW_S: i64 = 24 * 3600;
const BACKFILL_BATCH: i64 = 2_000;
pub(crate) const EXTERNAL_REFS_KEY: &str = "AMUX_WORK_REFERENCES";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkKind {
    Card,
    Pr,
    Issue,
    Deal,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct WorkLink {
    pub kind: WorkKind,
    pub id: String,
    pub source: String,
    pub effective_at: i64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct Attribution {
    pub worker_id: Option<String>,
    pub primary: Option<WorkLink>,
    pub attempt: Option<i64>,
    pub generation: Option<i64>,
    pub reason: String,
    pub source: String,
    pub effective_at: i64,
    #[serde(default)]
    pub valid_until: Option<i64>,
}
impl Attribution {
    pub fn card(&self) -> &str {
        self.primary
            .as_ref()
            .filter(|p| p.kind == WorkKind::Card)
            .map(|p| p.id.as_str())
            .unwrap_or("")
    }
}

#[derive(Clone)]
struct Worker {
    id: String,
    name: String,
    aliases: Vec<String>,
    environment: Value,
}
#[derive(Clone)]
struct Window {
    name: String,
    from: i64,
    to: i64,
    actual_to: i64,
    attribution: Attribution,
}

pub(crate) struct Resolver {
    workers: Vec<Worker>,
    windows: HashMap<String, Vec<Window>>,
    snapshots: HashMap<String, Vec<Attribution>>,
}

fn read_json(text: String) -> Value {
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

impl Resolver {
    pub fn load(conn: &Connection, now: i64) -> rusqlite::Result<Self> {
        let mut st =
            conn.prepare("SELECT id,display_name,name_aliases,environment FROM usage_workers")?;
        let workers = st
            .query_map([], |r| {
                Ok(Worker {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    aliases: serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_default(),
                    environment: read_json(r.get(3)?),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut windows: HashMap<String, Vec<Window>> = HashMap::new();
        let mut push = |name: String,
                        card: String,
                        from: i64,
                        to: i64,
                        attempt: Option<i64>,
                        generation: Option<i64>,
                        source: &str| {
            if name.trim().is_empty() || card.trim().is_empty() || to <= from {
                return;
            }
            let attribution = Attribution {
                primary: Some(WorkLink {
                    kind: WorkKind::Card,
                    id: card,
                    source: source.into(),
                    effective_at: from,
                }),
                attempt,
                generation,
                reason: "attributed".into(),
                source: source.into(),
                effective_at: from,
                ..Default::default()
            };
            windows.entry(name.clone()).or_default().push(Window {
                name,
                from,
                to: to.min(from + MAX_WINDOW_S),
                actual_to: to,
                attribution,
            });
        };
        let mut attempts_from: HashMap<String, i64> = HashMap::new();
        if conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='task_attempts')",
            [],
            |r| r.get::<_, bool>(0),
        )? {
            let mut st = conn.prepare("SELECT worker,card,started_at,COALESCE(ended_at,?1),attempt,generation FROM task_attempts")?;
            for r in st.query_map([now + 1], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            })? {
                let (name, card, from, to, attempt, generation) = r?;
                attempts_from
                    .entry(name.clone())
                    .and_modify(|t| *t = (*t).min(from))
                    .or_insert(from);
                let matches: Vec<_> = workers
                    .iter()
                    .filter(|w| w.id == name || w.name == name || w.aliases.contains(&name))
                    .collect();
                if let [worker] = matches.as_slice() {
                    for alias in std::iter::once(&worker.id)
                        .chain(std::iter::once(&worker.name))
                        .chain(worker.aliases.iter())
                    {
                        attempts_from
                            .entry(alias.clone())
                            .and_modify(|t| *t = (*t).min(from))
                            .or_insert(from);
                    }
                }
                push(
                    name,
                    card,
                    from,
                    to,
                    Some(attempt),
                    Some(generation),
                    "task_attempts",
                );
            }
        }
        for (card, name, from, to) in
            crate::project_execution::outputs::usage_windows(conn, now, MAX_WINDOW_S)?
        {
            push(
                name,
                card,
                from,
                to,
                None,
                None,
                "project_execution_history",
            );
        }
        let mut st = conn.prepare("SELECT session,CAST(ts AS INTEGER),data FROM session_events WHERE type='task.claimed' AND session<>'' AND NOT EXISTS(SELECT 1 FROM issues i WHERE i.project_group IS NOT NULL AND i.id=CASE WHEN json_valid(session_events.data) THEN json_extract(session_events.data,'$.issue') END) ORDER BY session,ts,id")?;
        let claims = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    read_json(r.get(2)?),
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (i, (name, from, data)) in claims.iter().enumerate() {
            let to = claims[i + 1..]
                .iter()
                .take_while(|(n, _, _)| n == name)
                .find(|(_, t, _)| t > from)
                .map(|(_, t, _)| *t)
                .unwrap_or(now + 1)
                .min(attempts_from.get(name).copied().unwrap_or(i64::MAX));
            if let Some(card) = data["issue"].as_str() {
                push(
                    name.clone(),
                    card.into(),
                    *from,
                    to,
                    None,
                    data["generation"].as_i64(),
                    "task.claimed",
                );
            }
        }
        let mut snapshots: HashMap<String, Vec<Attribution>> = HashMap::new();
        let mut st = conn.prepare(
            "SELECT conversation,attribution FROM usage_work_snapshots ORDER BY effective_at",
        )?;
        for row in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (conv, text) = row?;
            if let Ok(a) = serde_json::from_str::<Attribution>(&text) {
                if let Some(tail) = conv.rsplit('-').next().filter(|s| s.len() == 12) {
                    snapshots
                        .entry(format!("codex:{tail}"))
                        .or_default()
                        .push(a.clone());
                }
                snapshots.entry(conv).or_default().push(a);
            }
        }
        Ok(Self {
            workers,
            windows,
            snapshots,
        })
    }

    pub fn resolve_child(
        &self,
        name: &str,
        parent: &str,
        started: Option<i64>,
        ts: i64,
    ) -> Attribution {
        let Some(start) = started.filter(|start| *start <= ts && ts < *start + MAX_WINDOW_S) else {
            return Attribution {
                worker_id: self.identity(name).map(|w| w.id.clone()),
                reason: "child_start_unknown_or_expired".into(),
                source: "child_transcript".into(),
                effective_at: ts,
                ..Default::default()
            };
        };
        let mut a = self.resolve(name, parent, None, start);
        a.source = format!("inherited_parent_at_start:{}", a.source);
        a.effective_at = start;
        a
    }

    fn identity(&self, name: &str) -> Option<&Worker> {
        let mut found = self
            .workers
            .iter()
            .filter(|w| w.id == name || w.name == name || w.aliases.iter().any(|a| a == name));
        let worker = found.next()?;
        found.next().is_none().then_some(worker)
    }

    /// `parent` is inherited only from the transcript directory, not guessed
    /// from a title. Snapshots with no primary link deliberately clear old work.
    pub fn resolve(
        &self,
        name: &str,
        conversation: &str,
        parent: Option<&str>,
        ts: i64,
    ) -> Attribution {
        let mut worker = self.identity(name);
        let mut out = Attribution {
            worker_id: worker.map(|w| w.id.clone()),
            reason: if worker.is_none() {
                "no_worker_identity"
            } else {
                "no_work_reference"
            }
            .into(),
            source: "historical_intervals".into(),
            effective_at: ts,
            ..Default::default()
        };
        if self
            .workers
            .iter()
            .filter(|w| w.id == name || w.name == name || w.aliases.iter().any(|a| a == name))
            .count()
            > 1
        {
            out.reason = "conflicting_worker_identity".into();
            return out;
        }
        let key = parent.unwrap_or(conversation);
        let snapshots = self.snapshots.get(key).or_else(|| {
            key.strip_prefix("codex:")
                .and_then(|k| self.snapshots.get(k))
        });
        let snapshot = snapshots.and_then(|s| s.iter().rev().find(|a| a.effective_at <= ts));
        if let Some(a) = snapshot.filter(|a| ts < a.effective_at + MAX_WINDOW_S) {
            if snapshots
                .into_iter()
                .flatten()
                .filter(|s| s.effective_at == a.effective_at)
                .any(|s| {
                    s.worker_id != a.worker_id
                        || s.primary != a.primary
                        || s.generation != a.generation
                        || s.reason != a.reason
                })
            {
                out.reason = "conflicting_turn_snapshots".into();
                return out;
            }
        }
        if let Some(a) = snapshot.filter(|a| ts < a.effective_at + MAX_WINDOW_S) {
            if out.worker_id.is_some() && a.worker_id != out.worker_id {
                out.reason = "conflicting_worker_identity".into();
                return out;
            }
            out.worker_id = a.worker_id.clone();
            if out.worker_id.is_some() {
                out.reason = "no_work_reference".into();
            }
            if a.reason == "isolated" || a.primary.as_ref().is_none_or(|p| p.kind != WorkKind::Card)
            {
                return a.clone();
            }
            // Card snapshots must still fall inside their historical attempt.
        }
        if worker.is_none() {
            worker = out
                .worker_id
                .as_ref()
                .and_then(|id| self.workers.iter().find(|w| &w.id == id));
        }
        let names: Vec<&str> = worker
            .map(|w| {
                std::iter::once(w.id.as_str())
                    .chain(std::iter::once(w.name.as_str()))
                    .chain(w.aliases.iter().map(String::as_str))
                    .collect()
            })
            .unwrap_or_else(|| vec![name]);
        // Fresh native dispatch evidence may renew a genuinely active long
        // attempt; the historical-only fallback still expires after 24h.
        if let Some(a) =
            snapshot.filter(|a| ts < a.effective_at + MAX_WINDOW_S && a.primary.is_some())
        {
            let active: Vec<_> = names
                .iter()
                .filter_map(|n| self.windows.get(*n))
                .flatten()
                .filter(|w| w.from <= ts && ts < w.actual_to)
                .collect();
            if a.attempt.is_none() && a.valid_until.is_some_and(|end| ts < end) && active.is_empty()
            {
                return a.clone();
            }
            if active.iter().any(|w| {
                w.attribution.card() == a.card() && w.attribution.generation == a.generation
            }) && active.iter().all(|w| {
                w.attribution.card() == a.card()
                    && (w.attribution.generation.is_none()
                        || a.generation.is_none()
                        || w.attribution.generation == a.generation)
            }) {
                return a.clone();
            }
        }
        let candidates: Vec<_> = names
            .iter()
            .filter_map(|n| self.windows.get(*n))
            .flatten()
            .filter(|w| w.from <= ts && ts < w.to && !w.name.is_empty())
            .collect();
        if let Some(first) = candidates.first() {
            if candidates.iter().any(|w| {
                w.attribution.card() != first.attribution.card()
                    || (w.attribution.generation.is_some()
                        && first.attribution.generation.is_some()
                        && w.attribution.generation != first.attribution.generation)
            }) {
                out.reason = "conflicting_work_references".into();
                return out;
            }
            if let Some(a) = snapshot.filter(|a| {
                ts < a.effective_at + MAX_WINDOW_S
                    && a.card() == first.attribution.card()
                    && a.generation == first.attribution.generation
                    && a.worker_id == out.worker_id
            }) {
                return a.clone();
            }
            let mut a = first.attribution.clone();
            a.worker_id = out.worker_id;
            return a;
        }
        out
    }
}

/// Called only after native launch/sequence validation. External references
/// come from structured worker config, never prompt prose or a transcript title.
#[cfg(test)]
fn capture(conn: &Connection, name: &str, event: &Value, isolated: bool) -> rusqlite::Result<()> {
    capture_native(
        conn,
        name,
        event,
        &NativeContext {
            isolated,
            ..Default::default()
        },
    )
}

/// Only the native boundary reads config; historical resolution reads retained
/// observations. Legacy identity reuses CC_INCARNATION, never a mutable name.
#[derive(Default)]
pub(crate) struct NativeContext {
    pub incarnation: Option<String>,
    pub historical_aliases: Vec<String>,
    pub external_references: Option<String>,
    pub isolated: bool,
}

pub(crate) fn capture_native(
    conn: &Connection,
    name: &str,
    event: &Value,
    context: &NativeContext,
) -> rusqlite::Result<()> {
    let isolated = context.isolated;
    if !matches!(
        event["event"].as_str(),
        Some("SessionStart" | "UserPromptSubmit")
    ) {
        return Ok(());
    }
    let Some(conversation) = event["session_id"].as_str().filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    let ts = event["event_ts"].as_f64().unwrap_or(0.0) as i64;
    let mut resolver = Resolver::load(conn, ts)?;
    if resolver
        .identity(name)
        .is_none_or(|w| w.id.starts_with("incarnation:"))
    {
        if let Some(inc) = context.incarnation.as_deref() {
            let id = format!("incarnation:{inc}");
            let prior = resolver.workers.iter().find(|w| w.id == id);
            let mut aliases = prior.map(|w| w.aliases.clone()).unwrap_or_default();
            if let Some(previous) = prior.filter(|w| w.name != name) {
                if !aliases.contains(&previous.name) {
                    aliases.push(previous.name.clone());
                }
            }
            for alias in &context.historical_aliases {
                if alias != name && !aliases.contains(alias) {
                    aliases.push(alias.clone());
                }
            }
            let environment = serde_json::json!({(EXTERNAL_REFS_KEY):context.external_references});
            conn.execute("INSERT INTO usage_worker_identities(id,display_name,name_aliases,environment,first_seen,last_seen) VALUES(?1,?2,?3,?4,?5,?5) ON CONFLICT(id) DO UPDATE SET display_name=excluded.display_name,name_aliases=excluded.name_aliases,environment=excluded.environment,last_seen=excluded.last_seen",params![id,name,serde_json::to_string(&aliases).unwrap(),environment.to_string(),ts])?;
            resolver = Resolver::load(conn, ts)?;
        }
    }

    let mut a = resolver.resolve(name, conversation, None, ts);
    // A fresh turn is a fresh decision, not an inheritance from the prior turn.
    let mut without_snapshots = resolver;
    without_snapshots.snapshots.clear();
    if !isolated {
        a = without_snapshots.resolve(name, conversation, None, ts);
    }
    if !isolated && a.primary.is_none() && a.reason == "no_work_reference" {
        if let Some(worker) = without_snapshots.identity(name) {
            let mut st=conn.prepare("SELECT id,lease_generation,lease_expires_at FROM issues WHERE status='doing' AND lease_owner IN (?1,?2) AND lease_acquired_at<=?3 AND lease_expires_at>?3")?;
            let active = st
                .query_map(params![name, worker.id, ts], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if let [(card, generation, expires)] = active.as_slice() {
                a.primary = Some(WorkLink {
                    kind: WorkKind::Card,
                    id: card.clone(),
                    source: "validated_card_lease".into(),
                    effective_at: ts,
                });
                a.generation = Some(*generation);
                a.reason = "attributed".into();
                a.attempt = std::iter::once(&worker.id)
                    .chain(std::iter::once(&worker.name))
                    .chain(worker.aliases.iter())
                    .filter_map(|n| without_snapshots.windows.get(n))
                    .flatten()
                    .find(|w| {
                        w.attribution.card() == card
                            && w.attribution.generation == Some(*generation)
                    })
                    .and_then(|w| w.attribution.attempt);
                if a.attempt.is_none() {
                    a.valid_until = Some(*expires);
                }
            } else if active.len() > 1 {
                a.reason = "conflicting_work_references".into();
            }
        }
    }
    if a.worker_id.is_none() {
        if let Some(inc) = context.incarnation.as_deref() {
            a.worker_id = Some(format!("incarnation:{inc}"));
            a.primary = None;
            a.attempt = None;
            a.generation = None;
            a.reason = "conflicting_worker_identity".into();
        }
    }
    if isolated {
        a.primary = None;
        a.attempt = None;
        a.generation = None;
        a.reason = "isolated".into();
        a.valid_until = None;
    }
    if !isolated && a.primary.is_none() && a.reason == "no_work_reference" {
        if let Some(worker) = without_snapshots.identity(name) {
            if let Some(raw) = worker.environment[EXTERNAL_REFS_KEY].as_str() {
                match serde_json::from_str::<Vec<WorkLink>>(raw) {
                    Ok(links) if links.len() == 1 && valid_external(&links[0], ts) => {
                        let mut link = links[0].clone();
                        link.source = "worker_environment".into();
                        link.effective_at = ts;
                        a.primary = Some(link);
                        a.reason = "attributed".into();
                    }
                    _ => a.reason = "invalid_or_conflicting_work_references".into(),
                }
            }
        }
    }
    a.source = "native_turn_snapshot".into();
    a.effective_at = ts;
    if a.worker_id.is_none() {
        a.primary = None;
        a.attempt = None;
        a.generation = None;
        if !isolated && !a.reason.starts_with("conflicting") {
            a.reason = "no_worker_identity".into();
        }
        tracing::warn!(
            verdict = "usage_attribution_identity_gap",
            session = name,
            measured = true,
            n_considered = 1,
            "snapshot retained without a unique durable worker identity"
        );
    }
    let id = a.worker_id.as_deref().unwrap_or("");
    conn.execute("INSERT OR REPLACE INTO usage_work_snapshots(worker_id,conversation,effective_at,turn_id,attribution) VALUES(?1,?2,?3,?4,?5)",params![id,conversation,ts,event["turn_id"].as_str().unwrap_or(""),serde_json::to_string(&a).unwrap()])?;
    Ok(())
}

fn valid_external(link: &WorkLink, now: i64) -> bool {
    link.kind != WorkKind::Card
        && !link.id.trim().is_empty()
        && link.id.trim() == link.id
        && link.id.len() <= 512
        && !link.id.chars().any(char::is_control)
        && link.effective_at <= now
        && link.effective_at >= now - MAX_WINDOW_S
}

/// Bounded, resumable backfill from recorded evidence only. No live state reads.
pub(crate) async fn backfill(store: &SharedStore) -> anyhow::Result<()> {
    let mut after = 0;
    let resolver = {
        let c = store.read()?;
        Resolver::load(&c, chrono::Utc::now().timestamp())?
    };
    let mut total = 0usize;
    let mut gaps = 0usize;
    loop {
        let rows = {
            let c = store.read()?;
            let mut st=c.prepare("SELECT id,session,conversation,parent_conversation,ts FROM token_ledger WHERE (attribution IS NULL OR (json_extract(attribution,'$.primary') IS NULL AND json_extract(attribution,'$.source')='historical_intervals')) AND id>?1 ORDER BY id LIMIT ?2")?;
            let rows = st
                .query_map(params![after, BACKFILL_BATCH], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, i64>(4)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        if rows.is_empty() {
            break;
        }
        after = rows.last().unwrap().0;
        let updates: Vec<_> = rows
            .into_iter()
            .map(|(id, name, conv, parent, ts)| {
                (id, resolver.resolve(&name, &conv, parent.as_deref(), ts))
            })
            .collect();
        total += updates.len();
        gaps += updates.iter().filter(|(_, a)| a.primary.is_none()).count();
        store.write_async(move |conn| {
            for (id,a) in &updates {
                conn.execute("UPDATE token_ledger SET worker_id=?2,attribution=?3,task=?4 WHERE id=?1 AND attribution IS NOT ?3 AND (attribution IS NULL OR json_extract(attribution,'$.source')='historical_intervals')", params![id,a.worker_id,serde_json::to_string(a).unwrap(),a.card()])?;
            }
            Ok(WriteOutcome {applied:true,events:vec![]})
        }).await?;
    }
    if gaps > 0 {
        tracing::warn!(
            verdict = "usage_attribution_coverage_gap",
            measured = true,
            n_considered = total,
            unattributed = gaps,
            "responses have no validated work reference"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> Connection {
        let c = crate::db::migrate::test_memdb();
        crate::db::attempts::ensure_table(&c).unwrap();
        c.execute("INSERT INTO _amux_workers(id,display_name,name_aliases,created_at,updated_at) VALUES('wrk_a','renamed','[\"old\"]','2026-01-01','2026-01-01')",[]).unwrap();
        c
    }
    fn attempt(c: &Connection, card: &str, start: i64, end: i64, generation: i64) {
        c.execute("INSERT INTO task_attempts(card,attempt,worker,generation,started_at,ended_at) VALUES(?1,1,'old',?2,?3,?4)",params![card,generation,start,end]).unwrap();
    }
    fn event(ts: i64) -> Value {
        json!({"event":"UserPromptSubmit","session_id":"conv","event_ts":ts,"turn_id":format!("turn-{ts}")})
    }
    #[test]
    fn rename_resume_transitions_and_day_gaps_use_historical_intervals() {
        let c = fixture();
        attempt(&c, "A", 100, 200, 1);
        attempt(&c, "B", 200, 100_000, 2);
        let resolver = Resolver::load(&c, 200_000).unwrap();
        let first = resolver.resolve("renamed", "conv", None, 199);
        assert_eq!(first.worker_id.as_deref(), Some("wrk_a"));
        assert_eq!(first.card(), "A");
        assert_eq!(first.attempt, Some(1));
        assert_eq!(first.generation, Some(1));
        assert_eq!(resolver.resolve("old", "conv", None, 200).card(), "B");
        assert_eq!(
            resolver.resolve_child("old", "conv", Some(150), 250).card(),
            "A"
        );
        assert!(resolver
            .resolve_child("old", "conv", None, 250)
            .primary
            .is_none());
        assert_eq!(
            resolver.resolve("old", "conv", None, 90_000).reason,
            "no_work_reference"
        );
        assert!(resolver.resolve("old", "conv", None, 99).primary.is_none());
        c.execute("INSERT INTO issues(id,title,created,updated,status,lease_owner,lease_generation,lease_acquired_at,lease_expires_at) VALUES('B','Long work',1,1,'doing','renamed',2,200,110000)",[]).unwrap();
        capture(&c, "renamed", &event(90_000), false).unwrap();
        let renewed = Resolver::load(&c, 120_000).unwrap();
        assert_eq!(renewed.resolve("old", "conv", None, 90_001).card(), "B");
        assert!(renewed
            .resolve("old", "conv", None, 100_000)
            .primary
            .is_none());
        assert_eq!(
            resolver.resolve("", "unknown", None, 150).reason,
            "no_worker_identity"
        );
    }
    #[test]
    fn overlapping_attempts_and_same_time_claims_stay_conflicting() {
        let c = fixture();
        attempt(&c, "A", 100, 300, 1);
        attempt(&c, "B", 200, 400, 2);
        let a = Resolver::load(&c, 500)
            .unwrap()
            .resolve("old", "conv", None, 250);
        assert!(a.primary.is_none());
        assert_eq!(a.reason, "conflicting_work_references");
        c.execute("DELETE FROM task_attempts", []).unwrap();
        for card in ["A", "B"] {
            c.execute("INSERT INTO session_events(ts,session,type,data) VALUES(100,'old','task.claimed',?1)",[json!({"issue":card}).to_string()]).unwrap();
        }
        let a = Resolver::load(&c, 500)
            .unwrap()
            .resolve("old", "conv", None, 150);
        assert_eq!(a.reason, "conflicting_work_references");
    }
    #[test]
    fn a_closed_attempt_cannot_be_reopened_by_its_older_claim() {
        let c = fixture();
        attempt(&c, "A", 100, 200, 1);
        c.execute("INSERT INTO session_events(ts,session,type,data) VALUES(90,'old','task.claimed','{\"issue\":\"A\"}')",[]).unwrap();
        let r = Resolver::load(&c, 500).unwrap();
        assert_eq!(r.resolve("old", "conv", None, 95).card(), "A");
        assert_eq!(r.resolve("old", "conv", None, 199).card(), "A");
        assert!(r.resolve("old", "conv", None, 200).primary.is_none());
    }
    #[test]
    fn native_snapshots_rotate_external_work_and_preserve_isolation_for_children() {
        let c = fixture();
        let links = json!([{"kind":"pr","id":"org/repo#258","source":"owner","effective_at":100}]);
        c.execute(
            "UPDATE _amux_workers SET environment=?1 WHERE id='wrk_a'",
            [json!({(EXTERNAL_REFS_KEY):links.to_string()}).to_string()],
        )
        .unwrap();
        capture(&c, "renamed", &event(100), false).unwrap();
        let r = Resolver::load(&c, 150).unwrap();
        let a = r.resolve("old", "agent-child", Some("conv"), 110);
        assert_eq!(a.primary.unwrap().kind, WorkKind::Pr);
        assert_eq!(a.source, "native_turn_snapshot");
        // Today's external state must never backfill a response before capture.
        assert!(r.resolve("old", "conv", None, 99).primary.is_none());
        c.execute(
            "UPDATE _amux_workers SET environment='{}' WHERE id='wrk_a'",
            [],
        )
        .unwrap();
        capture(&c, "renamed", &event(120), false).unwrap();
        let r = Resolver::load(&c, 150).unwrap();
        assert!(r.resolve("old", "conv", None, 121).primary.is_none());
        capture(&c, "renamed", &event(130), true).unwrap();
        let a = Resolver::load(&c, 150)
            .unwrap()
            .resolve("old", "agent-child", Some("conv"), 140);
        assert_eq!(a.reason, "isolated");
        assert!(a.primary.is_none());
        assert_eq!(a.generation, None);
    }
    #[test]
    fn malformed_multiple_and_future_external_references_are_not_attributed() {
        for raw in ["not-json".to_string(),json!([{"kind":"issue","id":"repo#1","source":"owner","effective_at":101}]).to_string(),json!([{"kind":"deal","id":"D1","source":"owner","effective_at":100},{"kind":"pr","id":"repo#2","source":"owner","effective_at":100}]).to_string()] {
            let c=fixture();c.execute("UPDATE _amux_workers SET environment=?1",[json!({(EXTERNAL_REFS_KEY):raw}).to_string()]).unwrap();
            capture(&c,"renamed",&event(100),false).unwrap();
            let a=Resolver::load(&c,150).unwrap().resolve("old","conv",None,110);
            assert_eq!(a.reason,"invalid_or_conflicting_work_references");assert!(a.primary.is_none());
        }
    }
    #[test]
    fn unknown_worker_snapshot_records_no_worker_identity() {
        let c = fixture();
        capture(&c, "ghost", &event(100), false).unwrap();
        capture(&c, "ghost", &event(110), true).unwrap();
        let reasons: Vec<(String, String)> = c
            .prepare("SELECT worker_id,attribution FROM usage_work_snapshots ORDER BY effective_at")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| {
                let (id, raw): (String, String) = r.unwrap();
                let v: Value = serde_json::from_str(&raw).unwrap();
                (id, v["reason"].as_str().unwrap_or_default().to_string())
            })
            .collect();
        assert_eq!(
            reasons,
            vec![
                (String::new(), "no_worker_identity".to_string()),
                (String::new(), "isolated".to_string()),
            ],
            "a turn with no durable worker identity must say so, and isolation must win"
        );
    }
    #[test]
    fn snapshot_ownership_cannot_be_adopted_by_another_worker() {
        let c = fixture();
        capture(&c, "renamed", &event(100), false).unwrap();
        c.execute("INSERT INTO _amux_workers(id,display_name,created_at,updated_at) VALUES('wrk_b','other','2026-01-01','2026-01-01')",[]).unwrap();
        let a = Resolver::load(&c, 150)
            .unwrap()
            .resolve("other", "conv", None, 110);
        assert!(a.primary.is_none());
        assert_eq!(a.reason, "conflicting_worker_identity");
    }
}

#[cfg(test)]
mod legacy_identity_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn existing_incarnation_survives_rename_without_a_rust_worker_row() {
        let c = crate::db::migrate::test_memdb();
        let event = |ts| json!({"event":"UserPromptSubmit","session_id":"legacy-conv","event_ts":ts,"turn_id":format!("turn-{ts}")});
        let context = NativeContext {
            incarnation: Some("existing-id".into()),
            historical_aliases: vec!["original".into()],
            external_references: Some(
                json!([{"kind":"deal","id":"deal-1","source":"owner","effective_at":100}])
                    .to_string(),
            ),
            ..Default::default()
        };
        capture_native(&c, "first-name", &event(100), &context).unwrap();
        capture_native(&c, "renamed", &event(120), &context).unwrap();
        let r = Resolver::load(&c, 200).unwrap();
        let a = r.resolve("renamed", "legacy-conv", None, 130);
        assert_eq!(a.worker_id.as_deref(), Some("incarnation:existing-id"));
        assert_eq!(a.primary.unwrap().kind, WorkKind::Deal);
        let aliases: String = c
            .query_row(
                "SELECT name_aliases FROM usage_worker_identities",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(aliases.contains("first-name"));
        assert!(aliases.contains("original"));
        assert!(r
            .resolve("original", "legacy-conv", None, 99)
            .primary
            .is_none());
    }
    #[test]
    fn isolated_snapshot_blocks_attribution_even_without_any_identity_record() {
        let c = crate::db::migrate::test_memdb();
        crate::db::attempts::ensure_table(&c).unwrap();
        c.execute("INSERT INTO task_attempts(card,attempt,worker,generation,started_at,ended_at) VALUES('stale',1,'legacy',1,90,200)",[]).unwrap();
        capture_native(
            &c,
            "legacy",
            &json!({"event":"UserPromptSubmit","session_id":"conv","event_ts":100}),
            &NativeContext {
                isolated: true,
                ..Default::default()
            },
        )
        .unwrap();
        let a = Resolver::load(&c, 200)
            .unwrap()
            .resolve("legacy", "conv", None, 110);
        assert_eq!(a.reason, "isolated");
        assert!(a.primary.is_none());
    }
}
