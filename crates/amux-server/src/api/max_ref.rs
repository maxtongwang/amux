//! max_ref: prove that an owner-delegated change cites words the owner
//! actually wrote (maxtongwang/ai-config#160, piece b).
//!
//! A lead or mrpopo may widen a worker only when it cites the owner's
//! instruction: `MaxRef { session, time }`. This module finds that instruction
//! in the Messages ledger (`cmd_history`) and refuses anything amux itself put
//! there. A relay of the owner's words by a peer, a stamped delivery, or an
//! unproven send never counts, even with identical text.
//!
//! HUMAN-TYPED. A `cmd_history` row in the referenced session is the owner's
//! own words in exactly two shapes:
//! - PANE: `type='user'`, `delivery='pane'`, `origin='pane'`, written only by
//!   `pane_prompts::record` from the provider's UserPromptSubmit hook after it
//!   ruled out an amux delivery. Max's Claude Remote Control input lands here
//!   too when the provider reports it through the same hook: it is a real user
//!   turn with no amux stamp and no amux delivery behind it.
//! - OWNER SEND: `type='user'`, `delivery` `direct` or `queued`, and
//!   `process_origin` `outside` or `remote` (migration 0123): the dashboard on
//!   the Mac or the phone over the tailnet, proven by process attribution
//!   (`process_identity`), not by a header the client chose. A CLI or API send
//!   from any worker pane records no verdict, and neither does any row written
//!   before 0123, so neither ever qualifies.
//!
//! Every candidate then passes [`is_amux_delivery`], which refuses an `[amux`
//! stamp, provider-injected text, and any text matching what amux delivered to
//! that lane (the `pane_prompts` delivery ring and the lane's other
//! `cmd_history` rows: peer, relay, schedule, system, automation, and owner
//! sends without a proven verdict).
//!
//! KNOWN LIMIT (ai-config#136). Workers run as the owner's macOS user. A
//! same-user process can type into the owner's pane with raw `tmux send-keys`
//! and the hook reports it as a typed prompt. A process inside the cited lane
//! can likewise post its own UserPromptSubmit observation to
//! `/api/sessions/<own lane>/report`, which process attribution permits for
//! the lane's own session. Both yield a PANE row this module cannot tell from
//! the owner's. Closing them needs an OS boundary between the owner and the
//! workers, not a ledger check.
use super::pane_prompts;
use super::AppState;

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize, PartialEq)]
pub(crate) struct MaxRef {
    pub session: String,
    /// Unix seconds.
    pub time: f64,
}

#[derive(Clone, Debug, serde::Serialize, PartialEq)]
pub(crate) struct MaxRefEvidence {
    pub row_id: i64,
    pub session: String,
    pub ts: f64,
    /// At most [`EXCERPT_CHARS`] characters of the row's text.
    pub excerpt: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MaxRefRefusal {
    Missing,
    NoSuchRow,
    NotHumanTyped,
    WrongSession,
    Stale,
    OutsideWindow,
    DoesNotNameTarget,
    DoesNotNameSetting,
}

impl MaxRefRefusal {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Missing => "max_ref_missing",
            Self::NoSuchRow => "max_ref_no_such_row",
            Self::NotHumanTyped => "max_ref_not_human_typed",
            Self::WrongSession => "max_ref_wrong_session",
            Self::Stale => "max_ref_stale",
            Self::OutsideWindow => "max_ref_outside_window",
            Self::DoesNotNameTarget => "max_ref_does_not_name_target",
            Self::DoesNotNameSetting => "max_ref_does_not_name_setting",
        }
    }

    pub(crate) fn message(&self) -> String {
        let w = MAX_REF_WINDOW_S as i64;
        let age_h = (MAX_REF_MAX_AGE_S / 3600.0) as i64;
        match self {
            Self::Missing => "this change needs max_ref {session, time}: the owner's own instruction for it".into(),
            Self::NoSuchRow => format!("no message in that session within {w}s of max_ref.time"),
            Self::NotHumanTyped => "the cited message is not one the owner typed: it was delivered by amux (a relay, peer, stamped or automated message) or sent without process proof that it came from the owner".into(),
            Self::WrongSession => format!("the owner typed nothing in the cited session within {w}s of max_ref.time; the message at that time is in another session"),
            Self::Stale => format!("the cited message is older than {age_h}h"),
            Self::OutsideWindow => format!("the cited session has messages, but none within {w}s of max_ref.time"),
            Self::DoesNotNameTarget => "the owner's message does not name the worker, group or tag this change targets".into(),
            Self::DoesNotNameSetting => "the owner's message names the target but not the setting this change makes (for example the group being joined, the toggle, or routing)".into(),
        }
    }
}

/// |row.ts - r.time| must not exceed this.
pub(crate) const MAX_REF_WINDOW_S: f64 = 600.0;
/// now - row.ts must not exceed this.
pub(crate) const MAX_REF_MAX_AGE_S: f64 = 86_400.0;
const EXCERPT_CHARS: usize = 200;
/// Bounds every query; a lane writes a few rows a minute.
const ROW_LIMIT: i64 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HumanSource {
    Pane,
    OwnerSend,
}

#[derive(Debug, Clone)]
struct Row {
    id: i64,
    session: String,
    text: String,
    ts: f64,
    source: Option<HumanSource>,
}

/// Which human shape a row has, if any (see the module comment).
fn human_source(ctype: &str, origin: &str, delivery: &str, process_origin: &str) -> Option<HumanSource> {
    if ctype != "user" {
        return None;
    }
    if delivery == "pane" && origin == "pane" {
        return Some(HumanSource::Pane);
    }
    if matches!(delivery, "direct" | "queued") && matches!(process_origin, "outside" | "remote") {
        return Some(HumanSource::OwnerSend);
    }
    None
}

const ROW_COLS: &str = "id, session, text, COALESCE(type,''), COALESCE(origin,''), COALESCE(delivery,''), COALESCE(process_origin,''), ts";

fn rows(state: &AppState, sql_where: &str, params: &[&dyn rusqlite::ToSql]) -> Result<Vec<Row>, String> {
    let conn = state.store.read().map_err(|e| e.to_string())?;
    let sql = format!("SELECT {ROW_COLS} FROM cmd_history WHERE {sql_where} LIMIT {ROW_LIMIT}");
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let mapped = stmt
        .query_map(params, |r| {
            let (ctype, origin, delivery, po): (String, String, String, String) = (r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?);
            Ok(Row {
                id: r.get(0)?,
                session: r.get(1)?,
                text: r.get(2)?,
                ts: r.get::<_, i64>(7)? as f64 / 1000.0,
                source: human_source(&ctype, &origin, &delivery, &po),
            })
        })
        .map_err(|e| e.to_string())?;
    mapped.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

fn ms(s: f64) -> i64 {
    (s * 1000.0) as i64
}

/// Did amux, rather than the owner, put this text in front of the lane?
///
/// True for an `[amux` stamp or provider-injected text, and for any text
/// matching a delivery to `session` near `ts`: the lane's other `cmd_history`
/// rows that are not themselves human-typed, plus, for a PANE row, the
/// `pane_prompts` delivery ring. (An owner send's own delivery is in the ring,
/// so the ring is not consulted for it; its authorship is proven by process.)
/// Fails closed: a ledger that cannot be read counts as an amux delivery.
///
/// TODO(maxtongwang/amux#10): when the server stamp registry and per-message
/// `msg_id` rows land, also return true for a text whose stamp is in the
/// registry or whose row carries an amux `msg_id`. They plug in here and
/// nowhere else.
pub(crate) fn is_amux_delivery(state: &AppState, session: &str, row_id: i64, text: &str, ts: f64, source: HumanSource) -> bool {
    let w = pane_prompts::DELIVERY_WINDOW_S;
    let near = match rows(
        state,
        "session=?1 AND id<>?2 AND ts BETWEEN ?3 AND ?4 ORDER BY ts DESC",
        &[&session, &row_id, &ms(ts - w), &ms(ts + w)],
    ) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(session, row_id, error = %e, verdict = "max_ref_ledger_unreadable", measured = false,
                why_unmeasured = %e, n_considered = 0, "max_ref could not read the lane's deliveries; refused");
            return true;
        }
    };
    let mut delivered: Vec<String> = near
        .into_iter()
        .filter(|r| r.source.is_none())
        .map(|r| pane_prompts::normalize(&r.text))
        .collect();
    if source == HumanSource::Pane {
        delivered.extend(pane_prompts::recent_deliveries(session));
    }
    pane_prompts::classify(text, &delivered) != pane_prompts::PaneVerdict::Record
}

/// Case-insensitive whole-word PHRASE match after whitespace normalization:
/// `external  email` matches "turn on External\nemail". A word character is
/// alphanumeric, `-` or `_`, so `lead` does not match `dev-lead` or
/// `misleading`, and `dev` does not match `devops`. Used for targets and
/// setting terms alike.
pub(crate) fn names_phrase(text: &str, phrase: &str) -> bool {
    let target = pane_prompts::normalize(phrase).to_lowercase();
    if target.is_empty() {
        return false;
    }
    let hay = pane_prompts::normalize(text).to_lowercase();
    let word = |c: char| c.is_alphanumeric() || c == '-' || c == '_';
    hay.match_indices(&target).any(|(i, m)| {
        let before = hay[..i].chars().next_back();
        let after = hay[i + m.len()..].chars().next();
        !before.is_some_and(word) && !after.is_some_and(word)
    })
}

/// Verify that `r` cites a message the owner typed, in `r.session`, within
/// [`MAX_REF_WINDOW_S`] of `r.time`, no older than [`MAX_REF_MAX_AGE_S`] at
/// `now`, naming at least one of `targets` (non-empty required here). The
/// nearest qualifying row wins. Prefer [`verify_max_ref_terms`] for a change
/// that has a setting: this form authorizes on the target name alone.
pub(crate) fn verify_max_ref(state: &AppState, r: Option<&MaxRef>, targets: &[&str], now: f64) -> Result<MaxRefEvidence, MaxRefRefusal> {
    if targets.iter().all(|t| t.trim().is_empty()) {
        return logged(r, Err(MaxRefRefusal::DoesNotNameTarget));
    }
    logged(r, verify(state, r, targets, None, now))
}

/// [`verify_max_ref`] plus the setting: ONE human-typed row must name at least
/// one of `targets` AND contain at least one of `terms` (both phrase matches,
/// see [`names_phrase`]); matches are never combined across rows. An empty
/// `targets` is a fleet-level change with no target requirement. An empty
/// `terms` is a caller bug and is refused as [`MaxRefRefusal::DoesNotNameSetting`].
pub(crate) fn verify_max_ref_terms(state: &AppState, r: Option<&MaxRef>, targets: &[&str], terms: &[&str], now: f64) -> Result<MaxRefEvidence, MaxRefRefusal> {
    if terms.iter().all(|t| t.trim().is_empty()) {
        tracing::warn!(verdict = "max_ref_no_terms", measured = true, n_considered = 0,
            "verify_max_ref_terms called with no setting terms; refused (caller bug)");
        return logged(r, Err(MaxRefRefusal::DoesNotNameSetting));
    }
    logged(r, verify(state, r, targets, Some(terms), now))
}

fn logged(r: Option<&MaxRef>, out: Result<MaxRefEvidence, MaxRefRefusal>) -> Result<MaxRefEvidence, MaxRefRefusal> {
    match &out {
        Ok(ev) => tracing::info!(session = %ev.session, row_id = ev.row_id, verdict = "max_ref_accepted",
            measured = true, n_considered = 1, "max_ref cites a message the owner typed"),
        Err(refusal) => tracing::info!(session = r.map(|r| r.session.as_str()).unwrap_or(""), code = refusal.code(),
            verdict = "max_ref_refused", measured = true, n_considered = 1, "{}", refusal.message()),
    }
    out
}

fn verify(state: &AppState, r: Option<&MaxRef>, targets: &[&str], terms: Option<&[&str]>, now: f64) -> Result<MaxRefEvidence, MaxRefRefusal> {
    let Some(r) = r.filter(|r| !r.session.trim().is_empty() && r.time.is_finite()) else {
        return Err(MaxRefRefusal::Missing);
    };
    let (lo, hi) = (ms(r.time - MAX_REF_WINDOW_S), ms(r.time + MAX_REF_WINDOW_S));
    // A ledger that cannot be read proves nothing: refuse as unfound.
    let in_window = rows(state, "session=?1 AND ts BETWEEN ?2 AND ?3", &[&r.session, &lo, &hi]).map_err(|_| MaxRefRefusal::NoSuchRow)?;
    if in_window.is_empty() {
        let elsewhere = rows(state, "session<>?1 AND ts BETWEEN ?2 AND ?3", &[&r.session, &lo, &hi]).map_err(|_| MaxRefRefusal::NoSuchRow)?;
        if elsewhere.iter().any(|row| row.source.is_some()) {
            return Err(MaxRefRefusal::WrongSession);
        }
        let any = rows(state, "session=?1", &[&r.session]).map_err(|_| MaxRefRefusal::NoSuchRow)?;
        return Err(if any.is_empty() { MaxRefRefusal::NoSuchRow } else { MaxRefRefusal::OutsideWindow });
    }
    let fresh: Vec<Row> = in_window.into_iter().filter(|row| now - row.ts <= MAX_REF_MAX_AGE_S).collect();
    if fresh.is_empty() {
        return Err(MaxRefRefusal::Stale);
    }
    let mut typed: Vec<Row> = fresh
        .into_iter()
        .filter(|row| row.source.is_some_and(|src| !is_amux_delivery(state, &row.session, row.id, &row.text, row.ts, src)))
        .collect();
    if typed.is_empty() {
        return Err(MaxRefRefusal::NotHumanTyped);
    }
    // Each filter narrows the SAME rows, so one row satisfies both.
    if !targets.is_empty() {
        typed.retain(|row| targets.iter().any(|t| names_phrase(&row.text, t)));
        if typed.is_empty() {
            return Err(MaxRefRefusal::DoesNotNameTarget);
        }
    }
    if let Some(terms) = terms {
        typed.retain(|row| terms.iter().any(|t| names_phrase(&row.text, t)));
    }
    typed.sort_by(|a, b| (a.ts - r.time).abs().total_cmp(&(b.ts - r.time).abs()));
    let row = typed.into_iter().next().ok_or(MaxRefRefusal::DoesNotNameSetting)?;
    Ok(MaxRefEvidence { row_id: row.id, session: row.session, ts: row.ts, excerpt: row.text.chars().take(EXCERPT_CHARS).collect() })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: f64 = 1_800_000_000.0;

    fn state(dir: &std::path::Path) -> AppState {
        AppState {
            store: std::sync::Arc::new(crate::db::Store::open(&dir.join("test.db")).unwrap()),
            started: std::time::Instant::now(),
            build_hash: "test".into(),
            auth_token: None,
            reconciled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }

    /// One `cmd_history` row: (session, text, type, origin, delivery, process_origin, ts seconds).
    struct R<'a>(&'a str, &'a str, &'a str, &'a str, &'a str, Option<&'a str>, f64);

    fn put(st: &AppState, r: R<'_>) -> i64 {
        let (session, text, ty, origin, delivery, po, ts) =
            (r.0.to_string(), r.1.to_string(), r.2.to_string(), r.3.to_string(), r.4.to_string(), r.5.map(str::to_string), ms(r.6));
        let id = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
        let idw = id.clone();
        st.store
            .write(move |c| {
                crate::api::session_verbs::ensure_fleet_tables(c)?;
                c.execute(
                    "INSERT INTO cmd_history(session,text,type,origin,delivery,process_origin,ts) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                    rusqlite::params![session, text, ty, origin, delivery, po, ts],
                )?;
                idw.store(c.last_insert_rowid(), std::sync::atomic::Ordering::SeqCst);
                Ok(crate::db::WriteOutcome { applied: true, events: vec![] })
            })
            .unwrap();
        id.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn pane<'a>(session: &'a str, text: &'a str, ts: f64) -> R<'a> {
        R(session, text, "user", "pane", "pane", None, ts)
    }

    fn mref(session: &str, time: f64) -> MaxRef {
        MaxRef { session: session.into(), time }
    }

    const ASK: &str = "test-lead: create a worker test-alpha in the dev group";

    #[test]
    fn a_typed_pane_row_is_accepted_with_evidence() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        let id = put(&st, pane("test-lead", ASK, NOW - 100.0));
        let ev = verify_max_ref(&st, Some(&mref("test-lead", NOW - 60.0)), &["test-alpha"], NOW).unwrap();
        assert_eq!(ev, MaxRefEvidence { row_id: id, session: "test-lead".into(), ts: NOW - 100.0, excerpt: ASK.into() });
        let long = "test-alpha ".repeat(40);
        put(&st, pane("test-long", &long, NOW));
        let ev = verify_max_ref(&st, Some(&mref("test-long", NOW)), &["test-alpha"], NOW).unwrap();
        assert_eq!(ev.excerpt.chars().count(), 200, "excerpt is capped");
        // Two qualifying rows: the one nearest max_ref.time is the evidence.
        put(&st, pane("test-two", "test-alpha first", NOW - 300.0));
        let near = put(&st, pane("test-two", "test-alpha second", NOW - 20.0));
        put(&st, pane("test-two", "test-alpha third", NOW - 500.0));
        let ev = verify_max_ref(&st, Some(&mref("test-two", NOW)), &["test-alpha"], NOW).unwrap();
        assert_eq!(ev.row_id, near);
    }

    #[test]
    fn a_missing_or_malformed_ref_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        put(&st, pane("test-lead", ASK, NOW));
        assert_eq!(verify_max_ref(&st, None, &["test-alpha"], NOW), Err(MaxRefRefusal::Missing));
        assert_eq!(verify_max_ref(&st, Some(&mref(" ", NOW)), &["test-alpha"], NOW), Err(MaxRefRefusal::Missing));
        assert_eq!(verify_max_ref(&st, Some(&mref("test-lead", f64::NAN)), &["test-alpha"], NOW), Err(MaxRefRefusal::Missing));
    }

    #[test]
    fn a_session_with_no_messages_is_no_such_row() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        put(&st, R("test-other", "unrelated peer note", "session", "test-peer", "direct", None, NOW));
        assert_eq!(verify_max_ref(&st, Some(&mref("test-lead", NOW)), &["test-alpha"], NOW), Err(MaxRefRefusal::NoSuchRow));
    }

    #[test]
    fn a_stamped_relay_with_the_same_text_is_not_human_typed() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        // A pane row is written only after pane_prompts ruled out a delivery,
        // so these are rows a forged or older path left: each must still fail.
        put(&st, pane("test-relay", &format!("[amux-origin: test-peer — server-verified] {ASK}"), NOW));
        assert_eq!(verify_max_ref(&st, Some(&mref("test-relay", NOW)), &["test-alpha"], NOW), Err(MaxRefRefusal::NotHumanTyped));
        // The relay recorded as a peer row is not human either.
        put(&st, R("test-relay2", ASK, "session", "test-peer", "direct", None, NOW));
        assert_eq!(verify_max_ref(&st, Some(&mref("test-relay2", NOW)), &["test-alpha"], NOW), Err(MaxRefRefusal::NotHumanTyped));
    }

    #[test]
    fn a_pane_row_matching_an_amux_delivery_is_not_human_typed() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        // cmd_history: a peer delivered the same words a minute earlier.
        put(&st, R("test-hist", ASK, "session", "test-peer", "direct", None, NOW - 60.0));
        put(&st, pane("test-hist", ASK, NOW));
        assert_eq!(verify_max_ref(&st, Some(&mref("test-hist", NOW)), &["test-alpha"], NOW), Err(MaxRefRefusal::NotHumanTyped));
        // The ring: a raw send that wrote no history row.
        pane_prompts::note_delivery("test-ring", ASK);
        put(&st, pane("test-ring", ASK, NOW));
        assert_eq!(verify_max_ref(&st, Some(&mref("test-ring", NOW)), &["test-alpha"], NOW), Err(MaxRefRefusal::NotHumanTyped));
    }

    #[test]
    fn a_dashboard_send_without_process_proof_and_its_echo_are_not_human_typed() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        // No verdict: an old row, or a CLI send with the owner token.
        put(&st, R("test-dash", ASK, "user", "", "direct", None, NOW - 30.0));
        assert_eq!(verify_max_ref(&st, Some(&mref("test-dash", NOW - 30.0)), &["test-alpha"], NOW), Err(MaxRefRefusal::NotHumanTyped));
        // A pane row with the same text is that send's echo, not new typing.
        put(&st, pane("test-dash", ASK, NOW));
        assert_eq!(verify_max_ref(&st, Some(&mref("test-dash", NOW)), &["test-alpha"], NOW), Err(MaxRefRefusal::NotHumanTyped));
        // A verdict of `worker` is never stored by the recorder, and is not
        // accepted if a row carries it anyway.
        put(&st, R("test-cli", ASK, "user", "", "direct", Some("worker"), NOW));
        assert_eq!(verify_max_ref(&st, Some(&mref("test-cli", NOW)), &["test-alpha"], NOW), Err(MaxRefRefusal::NotHumanTyped));
    }

    #[test]
    fn owner_sends_proven_outside_or_remote_are_accepted_unless_stamped() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        let a = put(&st, R("test-mac", ASK, "user", "", "direct", Some("outside"), NOW));
        assert_eq!(verify_max_ref(&st, Some(&mref("test-mac", NOW)), &["test-alpha"], NOW).map(|e| e.row_id), Ok(a));
        let b = put(&st, R("test-phone", ASK, "user", "", "queued", Some("remote"), NOW));
        assert_eq!(verify_max_ref(&st, Some(&mref("test-phone", NOW)), &["test-alpha"], NOW).map(|e| e.row_id), Ok(b));
        // The owner's own queued send is in the delivery ring once drained;
        // that must not refuse it.
        pane_prompts::note_delivery("test-phone", ASK);
        assert!(verify_max_ref(&st, Some(&mref("test-phone", NOW)), &["test-alpha"], NOW).is_ok());
        put(&st, R("test-mac2", &format!("[amux steering] {ASK}"), "user", "", "direct", Some("outside"), NOW));
        assert_eq!(verify_max_ref(&st, Some(&mref("test-mac2", NOW)), &["test-alpha"], NOW), Err(MaxRefRefusal::NotHumanTyped));
        put(&st, R("test-mac3", ASK, "session", "test-peer", "direct", None, NOW - 10.0));
        put(&st, R("test-mac3", ASK, "user", "", "direct", Some("outside"), NOW));
        assert_eq!(verify_max_ref(&st, Some(&mref("test-mac3", NOW)), &["test-alpha"], NOW), Err(MaxRefRefusal::NotHumanTyped),
            "an owner send repeating a peer's delivery is refused");
    }

    #[test]
    fn peer_automation_and_system_rows_are_not_human_typed() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        for (i, (ty, origin, delivery)) in [
            ("session", "test-peer", "direct"),
            ("schedule", "schedule:test-1", "direct"),
            ("steering", "", "queued"),
            ("system", "amux", "direct"),
            ("user", "", "board"),
            ("user", "pane", "direct"),
            ("user", "", "pane"),
        ]
        .into_iter()
        .enumerate()
        {
            let lane = format!("test-kind-{i}");
            put(&st, R(&lane, ASK, ty, origin, delivery, Some("outside"), NOW));
            assert_eq!(verify_max_ref(&st, Some(&mref(&lane, NOW)), &["test-alpha"], NOW), Err(MaxRefRefusal::NotHumanTyped), "{ty}/{origin}/{delivery}");
        }
    }

    #[test]
    fn a_typed_row_in_another_session_is_wrong_session() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        put(&st, pane("test-elsewhere", ASK, NOW));
        put(&st, pane("test-lead", "earlier unrelated words", NOW - 5_000.0));
        assert_eq!(verify_max_ref(&st, Some(&mref("test-lead", NOW)), &["test-alpha"], NOW), Err(MaxRefRefusal::WrongSession));
    }

    #[test]
    fn a_row_older_than_the_max_age_is_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        put(&st, pane("test-lead", ASK, NOW - MAX_REF_MAX_AGE_S - 1.0));
        assert_eq!(verify_max_ref(&st, Some(&mref("test-lead", NOW - MAX_REF_MAX_AGE_S)), &["test-alpha"], NOW), Err(MaxRefRefusal::Stale));
        put(&st, pane("test-edge", ASK, NOW - MAX_REF_MAX_AGE_S));
        assert!(verify_max_ref(&st, Some(&mref("test-edge", NOW - MAX_REF_MAX_AGE_S)), &["test-alpha"], NOW).is_ok(), "the max age itself is allowed");
    }

    #[test]
    fn a_row_beyond_the_window_is_outside_window() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        put(&st, pane("test-lead", ASK, NOW - MAX_REF_WINDOW_S - 1.0));
        assert_eq!(verify_max_ref(&st, Some(&mref("test-lead", NOW)), &["test-alpha"], NOW), Err(MaxRefRefusal::OutsideWindow));
        put(&st, pane("test-edge", ASK, NOW - MAX_REF_WINDOW_S));
        assert!(verify_max_ref(&st, Some(&mref("test-edge", NOW)), &["test-alpha"], NOW).is_ok(), "the window edge is allowed");
    }

    #[test]
    fn a_typed_row_that_names_no_target_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        put(&st, pane("test-lead", ASK, NOW));
        assert_eq!(verify_max_ref(&st, Some(&mref("test-lead", NOW)), &["test-beta"], NOW), Err(MaxRefRefusal::DoesNotNameTarget));
        assert_eq!(verify_max_ref(&st, Some(&mref("test-lead", NOW)), &[], NOW), Err(MaxRefRefusal::DoesNotNameTarget));
        assert_eq!(verify_max_ref(&st, Some(&mref("test-lead", NOW)), &[""], NOW), Err(MaxRefRefusal::DoesNotNameTarget));
        assert!(verify_max_ref(&st, Some(&mref("test-lead", NOW)), &["test-beta", "DEV"], NOW).is_ok(), "any one target, any case");
    }

    #[test]
    fn a_target_inside_a_longer_word_does_not_match() {
        assert!(names_phrase("Give test-alpha the tag", "test-alpha"));
        assert!(names_phrase("(Test-Alpha), please", "test-alpha"));
        assert!(names_phrase("dev.", "dev"));
        assert!(!names_phrase("give test-alphas the tag", "test-alpha"));
        assert!(!names_phrase("the devops group", "dev"));
        assert!(!names_phrase("ask dev-lead", "dev"));
        assert!(!names_phrase("ask dev-lead", "lead"));
        assert!(!names_phrase("my_dev group", "dev"));
        assert!(!names_phrase("redev", "dev"));
        assert!(names_phrase("dev then devops", "dev"), "a later whole word still counts");
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        put(&st, pane("test-lead", "spin up test-alphas for the demo", NOW));
        assert_eq!(verify_max_ref(&st, Some(&mref("test-lead", NOW)), &["test-alpha"], NOW), Err(MaxRefRefusal::DoesNotNameTarget));
    }

    /// Remote Control: the owner types on his phone into a Claude session and
    /// the provider reports a real user turn through UserPromptSubmit. The
    /// recorder makes it a pane row and the verifier accepts it; stamped or
    /// delivery-matched text through the same path does not.
    #[tokio::test]
    async fn a_remote_control_prompt_through_the_hook_is_accepted() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = crate::api::settings::test_env::set_home(tmp.path());
        let st = state(tmp.path());
        let now = crate::config::now_f64();
        pane_prompts::record(&st, "test-rc", ASK).await;
        let ev = verify_max_ref(&st, Some(&mref("test-rc", now)), &["test-alpha"], now).unwrap();
        assert_eq!(ev.excerpt, ASK);
        // Stamped: not recorded, so there is nothing typed to cite.
        pane_prompts::record(&st, "test-rc2", &format!("[amux-origin: test-peer] {ASK}")).await;
        // Delivery-matched: amux typed it, so not recorded either.
        pane_prompts::note_delivery("test-rc3", ASK);
        pane_prompts::record(&st, "test-rc3", ASK).await;
        for lane in ["test-rc2", "test-rc3"] {
            assert_eq!(verify_max_ref(&st, Some(&mref(lane, now)), &["test-alpha"], now), Err(MaxRefRefusal::NoSuchRow), "{lane}");
        }
    }

    #[test]
    fn one_row_must_name_both_the_target_and_the_setting() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        let ok = put(&st, pane("test-both", "add test-alpha to the leads group", NOW));
        assert_eq!(verify_max_ref_terms(&st, Some(&mref("test-both", NOW)), &["test-alpha"], &["leads"], NOW).map(|e| e.row_id), Ok(ok));
        // Target in one row, setting in another: never combined.
        put(&st, pane("test-split", "test-alpha is doing well", NOW - 30.0));
        put(&st, pane("test-split", "the leads group needs one more", NOW));
        assert_eq!(verify_max_ref_terms(&st, Some(&mref("test-split", NOW)), &["test-alpha"], &["leads"], NOW), Err(MaxRefRefusal::DoesNotNameSetting));
        // Target only.
        put(&st, pane("test-target", "test-alpha looks fine", NOW));
        assert_eq!(verify_max_ref_terms(&st, Some(&mref("test-target", NOW)), &["test-alpha"], &["yolo"], NOW), Err(MaxRefRefusal::DoesNotNameSetting));
        // Setting only, with a target required.
        put(&st, pane("test-term", "turn on yolo for everyone", NOW));
        assert_eq!(verify_max_ref_terms(&st, Some(&mref("test-term", NOW)), &["test-alpha"], &["yolo"], NOW), Err(MaxRefRefusal::DoesNotNameTarget));
        // A fleet-level change: no target, the setting suffices.
        assert!(verify_max_ref_terms(&st, Some(&mref("test-term", NOW)), &[], &["yolo"], NOW).is_ok());
        // Empty terms: a caller bug, refused.
        assert_eq!(verify_max_ref_terms(&st, Some(&mref("test-both", NOW)), &["test-alpha"], &[], NOW), Err(MaxRefRefusal::DoesNotNameSetting));
        assert_eq!(verify_max_ref_terms(&st, Some(&mref("test-both", NOW)), &["test-alpha"], &[" "], NOW), Err(MaxRefRefusal::DoesNotNameSetting));
        // Phrases match across whitespace and on word boundaries only.
        put(&st, pane("test-phrase", "test-alpha: enable External\n  Email please", NOW));
        assert!(verify_max_ref_terms(&st, Some(&mref("test-phrase", NOW)), &["test-alpha"], &["external email"], NOW).is_ok());
        put(&st, pane("test-mislead", "test-alpha was misleading", NOW));
        assert_eq!(verify_max_ref_terms(&st, Some(&mref("test-mislead", NOW)), &["test-alpha"], &["lead"], NOW), Err(MaxRefRefusal::DoesNotNameSetting));
        assert!(!names_phrase("misleading", "lead"));
        assert!(!names_phrase("make dev-x a lead", "make lead"), "a phrase is contiguous");
        assert!(names_phrase("make  dev-x\ta lead", "dev-x a lead"));
    }

    #[test]
    fn codes_are_stable() {
        let all = [
            MaxRefRefusal::Missing,
            MaxRefRefusal::NoSuchRow,
            MaxRefRefusal::NotHumanTyped,
            MaxRefRefusal::WrongSession,
            MaxRefRefusal::Stale,
            MaxRefRefusal::OutsideWindow,
            MaxRefRefusal::DoesNotNameTarget,
            MaxRefRefusal::DoesNotNameSetting,
        ];
        let codes: Vec<&str> = all.iter().map(MaxRefRefusal::code).collect();
        assert_eq!(codes, [
            "max_ref_missing", "max_ref_no_such_row", "max_ref_not_human_typed", "max_ref_wrong_session",
            "max_ref_stale", "max_ref_outside_window", "max_ref_does_not_name_target",
            "max_ref_does_not_name_setting",
        ]);
        assert!(all.iter().all(|r| !r.message().is_empty()));
    }
}
