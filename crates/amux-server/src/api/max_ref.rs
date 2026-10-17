// Which Messages rows are the owner's own words: the row-qualification check for typed grant approvals — refs maxtongwang/ai-config#160 (piece b).
// Phrase matching and `max_ref` citations were replaced by structured grants (api/delegation_grants.rs); this keeps the human-typed verifier.
//! HUMAN-TYPED. A `cmd_history` row is the owner's own words in exactly two
//! shapes:
//! - PANE: `type='user'`, `delivery='pane'`, `origin='pane'`, written only by
//!   `pane_prompts::record` from the provider's UserPromptSubmit hook after it
//!   ruled out an amux delivery (Claude Remote Control input lands here too).
//! - OWNER SEND: `type='user'`, `delivery` `direct` or `queued`, and
//!   `process_origin` `outside` or `remote` (migration 0124), proven by process
//!   attribution (`process_identity`), not by a header the client chose.
//!
//! Every candidate then passes [`is_amux_delivery`], which refuses an `[amux`
//! stamp, provider-injected text, and any text matching what amux delivered to
//! that lane (the `pane_prompts` delivery ring and the lane's other
//! `cmd_history` rows). A relayed text that matches exactly never counts.
//!
//! TWO RULES (dev-lead, 2026-10-17), each one function:
//! - CITATION ([`cite_source_allowed`], cite mode, the default): a widening
//!   change cites one of Max's rows. It qualifies when it is human-typed, not
//!   an amux delivery, fresh, within the window of the cited time, and in a
//!   lane OTHER than the actor's own; in the actor's own lane only a
//!   non-local Remote owner send counts (a lane can mint its own pane rows
//!   through `/report`, and a worker reaches Outside or the forwarded Remote
//!   through `launchctl submit`). Accepted limit (ai-config#136): ANOTHER lane
//!   can mint a pane row the same way; in cite mode that is mitigated by the
//!   audit log, the leads feed and one-step undo, not prevented.
//! - APPROVAL ([`approval_source_allowed`], passkey mode): a pending grant is
//!   approved only by an owner send from a NON-LOCAL Remote peer (the phone or
//!   another device on the tailnet). A tailscale-serve forward over loopback
//!   (`remote-forwarded`) is not owner proof.
use super::pane_prompts;
use super::AppState;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MaxRefRefusal {
    NoSuchRow,
    NotHumanTyped,
    Stale,
    /// Human, but not an owner send from a non-local Remote peer.
    NotRemoteOwnerSend,
    /// No citation was given.
    Missing,
    /// The cited time is too far from the cited row.
    OutsideWindow,
    /// A citation in the actor's own lane that is not a non-local Remote owner send.
    OwnLane,
}

impl MaxRefRefusal {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::NoSuchRow => "max_ref_no_such_row",
            Self::NotHumanTyped => "max_ref_not_human_typed",
            Self::Stale => "max_ref_stale",
            Self::NotRemoteOwnerSend => "max_ref_not_remote_owner_send",
            Self::Missing => "cite_missing",
            Self::OutsideWindow => "max_ref_outside_window",
            Self::OwnLane => "max_ref_own_lane",
        }
    }
}

/// |row.ts - cited time| must not exceed this.
pub(crate) const CITE_WINDOW_S: f64 = 600.0;
/// now - row.ts must not exceed this.
pub(crate) const MAX_REF_MAX_AGE_S: f64 = 86_400.0;
/// Bounds every query; a lane writes a few rows a minute.
const ROW_LIMIT: i64 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
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
    /// process_origin = remote: a non-local peer (owner proof); never the forwarded verdict.
    remote: bool,
}

/// Which human shape a row has, if any (see the module comment).
fn human_source(ctype: &str, origin: &str, delivery: &str, process_origin: &str) -> Option<HumanSource> {
    if ctype != "user" {
        return None;
    }
    if delivery == "pane" && origin == "pane" {
        return Some(HumanSource::Pane);
    }
    if matches!(delivery, "direct" | "queued") && matches!(process_origin, "outside" | "remote" | "remote-forwarded") {
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
                remote: po == super::process_identity::REMOTE_DIRECT,
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
                why_unmeasured = %e, n_considered = 0, "could not read the lane's deliveries; the row does not qualify");
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

/// THE passkey-mode approval rule: only an owner send from a non-local
/// Remote peer. One function, so a ruling is a one-line change.
pub(crate) fn approval_source_allowed(source: HumanSource, remote: bool) -> bool {
    source == HumanSource::OwnerSend && remote
}

/// Whether Messages row `row_id` qualifies as Max approving a grant: an owner
/// send, fresh, not an amux delivery, and Remote ([`approval_source_allowed`]).
pub(crate) fn row_qualifies(state: &AppState, row_id: i64) -> Result<(), MaxRefRefusal> {
    row_qualifies_at(state, row_id, crate::config::now_f64())
}

fn row_qualifies_at(state: &AppState, row_id: i64, now: f64) -> Result<(), MaxRefRefusal> {
    let row = rows(state, "id=?1", &[&row_id]).map_err(|_| MaxRefRefusal::NoSuchRow)?.into_iter().next().ok_or(MaxRefRefusal::NoSuchRow)?;
    let source = row.source.ok_or(MaxRefRefusal::NotHumanTyped)?;
    if now - row.ts > MAX_REF_MAX_AGE_S {
        return Err(MaxRefRefusal::Stale);
    }
    if is_amux_delivery(state, &row.session, row.id, &row.text, row.ts, source) {
        return Err(MaxRefRefusal::NotHumanTyped);
    }
    if !approval_source_allowed(source, row.remote) {
        return Err(MaxRefRefusal::NotRemoteOwnerSend);
    }
    Ok(())
}

/// A citation of one of Max's rows: its id, or a session and time (the
/// nearest qualifying row within [`CITE_WINDOW_S`]).
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize, PartialEq)]
pub(crate) struct Cite {
    #[serde(default)]
    pub row_id: Option<i64>,
    #[serde(default)]
    pub session: Option<String>,
    /// Unix seconds.
    #[serde(default)]
    pub time: Option<f64>,
}

/// The row a citation resolved to.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Cited {
    pub row_id: i64,
    pub session: String,
    pub excerpt: String,
}

/// THE citation rule (cite mode): another lane's human row counts; in the
/// actor's own lane only a non-local Remote owner send.
pub(crate) fn cite_source_allowed(actor: &str, session: &str, source: HumanSource, remote: bool) -> bool {
    let own = session == actor || super::chat_worker::companion_parent(actor) == Some(session);
    !own || (source == HumanSource::OwnerSend && remote)
}

fn qualify_cited(state: &AppState, row: &Row, actor: &str, now: f64) -> Result<(), MaxRefRefusal> {
    let source = row.source.ok_or(MaxRefRefusal::NotHumanTyped)?;
    if now - row.ts > MAX_REF_MAX_AGE_S {
        return Err(MaxRefRefusal::Stale);
    }
    if is_amux_delivery(state, &row.session, row.id, &row.text, row.ts, source) {
        return Err(MaxRefRefusal::NotHumanTyped);
    }
    if !cite_source_allowed(actor, &row.session, source, row.remote) {
        return Err(MaxRefRefusal::OwnLane);
    }
    Ok(())
}

/// Resolve and qualify `cite` for `actor`.
pub(crate) fn cite_row(state: &AppState, cite: Option<&Cite>, actor: &str) -> Result<Cited, MaxRefRefusal> {
    cite_row_at(state, cite, actor, crate::config::now_f64())
}

fn cite_row_at(state: &AppState, cite: Option<&Cite>, actor: &str, now: f64) -> Result<Cited, MaxRefRefusal> {
    let cite = cite.ok_or(MaxRefRefusal::Missing)?;
    let cited = |row: Row| Cited { row_id: row.id, session: row.session, excerpt: row.text.chars().take(200).collect() };
    if let Some(id) = cite.row_id {
        let row = rows(state, "id=?1", &[&id]).map_err(|_| MaxRefRefusal::NoSuchRow)?.into_iter().next().ok_or(MaxRefRefusal::NoSuchRow)?;
        if cite.time.is_some_and(|t| (row.ts - t).abs() > CITE_WINDOW_S) {
            return Err(MaxRefRefusal::OutsideWindow);
        }
        qualify_cited(state, &row, actor, now)?;
        return Ok(cited(row));
    }
    let (Some(session), Some(time)) = (cite.session.as_deref().filter(|s| !s.trim().is_empty()), cite.time.filter(|t| t.is_finite())) else {
        return Err(MaxRefRefusal::Missing);
    };
    let near = rows(state, "session=?1 AND ts BETWEEN ?2 AND ?3", &[&session, &ms(time - CITE_WINDOW_S), &ms(time + CITE_WINDOW_S)])
        .map_err(|_| MaxRefRefusal::NoSuchRow)?;
    if near.is_empty() {
        return Err(MaxRefRefusal::NoSuchRow);
    }
    let mut best: Option<(Row, f64)> = None;
    let mut first_err = MaxRefRefusal::NotHumanTyped;
    for row in near {
        match qualify_cited(state, &row, actor, now) {
            Ok(()) => {
                let d = (row.ts - time).abs();
                if best.as_ref().is_none_or(|(_, bd)| d < *bd) {
                    best = Some((row, d));
                }
            }
            Err(e) => {
                if first_err == MaxRefRefusal::NotHumanTyped && e != MaxRefRefusal::NotHumanTyped {
                    first_err = e;
                }
            }
        }
    }
    best.map(|(r, _)| cited(r)).ok_or(first_err)
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

    fn remote<'a>(session: &'a str, text: &'a str, ts: f64) -> R<'a> {
        R(session, text, "user", "", "direct", Some("remote"), ts)
    }

    fn q(st: &AppState, id: i64) -> Result<(), MaxRefRefusal> {
        row_qualifies_at(st, id, NOW)
    }

    const ASK: &str = "amux grant 7K3Q";

    #[test]
    fn only_a_remote_owner_send_approves() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        let phone = put(&st, remote("test-desk", ASK, NOW));
        assert_eq!(q(&st, phone), Ok(()), "Remote owner send");
        let queued = put(&st, R("test-desk2", ASK, "user", "", "queued", Some("remote"), NOW));
        assert_eq!(q(&st, queued), Ok(()), "Remote queued owner send");
        let p = put(&st, pane("test-desk3", ASK, NOW));
        assert_eq!(q(&st, p), Err(MaxRefRefusal::NotRemoteOwnerSend), "a pane row in another lane");
        let mac = put(&st, R("test-mac", ASK, "user", "", "direct", Some("outside"), NOW));
        assert_eq!(q(&st, mac), Err(MaxRefRefusal::NotRemoteOwnerSend), "Outside (launchctl reaches it)");
        assert_eq!(q(&st, phone + 999), Err(MaxRefRefusal::NoSuchRow));
        assert!(approval_source_allowed(HumanSource::OwnerSend, true));
        assert!(!approval_source_allowed(HumanSource::Pane, true), "a pane row never approves, even flagged remote");
        assert!(!approval_source_allowed(HumanSource::OwnerSend, false));
    }

    #[test]
    fn a_stamped_or_relayed_row_is_not_human_typed() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        let a = put(&st, remote("test-relay", &format!("[amux-origin: test-peer — server-verified] {ASK}"), NOW));
        assert_eq!(q(&st, a), Err(MaxRefRefusal::NotHumanTyped));
        let b = put(&st, R("test-relay2", ASK, "session", "test-peer", "direct", Some("remote"), NOW));
        assert_eq!(q(&st, b), Err(MaxRefRefusal::NotHumanTyped));
    }

    #[test]
    fn a_remote_send_repeating_an_amux_delivery_is_not_human_typed() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        let long = "please approve it now: amux grant 7K3Q";
        put(&st, R("test-hist", long, "session", "test-peer", "direct", None, NOW - 60.0));
        let a = put(&st, remote("test-hist", long, NOW));
        assert_eq!(q(&st, a), Err(MaxRefRefusal::NotHumanTyped), "echo of a cmd_history delivery");
    }

    #[test]
    fn owner_sends_need_a_process_verdict() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        let none = put(&st, R("test-dash", ASK, "user", "", "direct", None, NOW));
        assert_eq!(q(&st, none), Err(MaxRefRefusal::NotHumanTyped), "no verdict");
        let worker = put(&st, R("test-cli", ASK, "user", "", "direct", Some("worker"), NOW));
        assert_eq!(q(&st, worker), Err(MaxRefRefusal::NotHumanTyped), "worker verdict");
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
            let id = put(&st, R(&format!("test-kind-{i}"), ASK, ty, origin, delivery, Some("remote"), NOW));
            assert!(q(&st, id).is_err(), "{ty}/{origin}/{delivery}");
        }
    }

    #[test]
    fn an_old_row_is_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        let old = put(&st, remote("test-desk", ASK, NOW - MAX_REF_MAX_AGE_S - 1.0));
        assert_eq!(q(&st, old), Err(MaxRefRefusal::Stale));
        let edge = put(&st, remote("test-desk2", ASK, NOW - MAX_REF_MAX_AGE_S));
        assert_eq!(q(&st, edge), Ok(()), "the max age itself is allowed");
    }

    #[test]
    fn codes_are_stable() {
        for (r, c) in [
            (MaxRefRefusal::NoSuchRow, "max_ref_no_such_row"),
            (MaxRefRefusal::NotHumanTyped, "max_ref_not_human_typed"),
            (MaxRefRefusal::Stale, "max_ref_stale"),
            (MaxRefRefusal::NotRemoteOwnerSend, "max_ref_not_remote_owner_send"),
        ] {
            assert_eq!(r.code(), c);
        }
    }

    /// Remote Control reaches the ledger as a pane row through the hook: a
    /// real user turn, and still never an approval (a lane can mint the same).
    #[tokio::test]
    async fn a_remote_control_prompt_through_the_hook_does_not_approve() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = crate::api::settings::test_env::set_home(tmp.path());
        let st = state(tmp.path());
        let now = crate::config::now_f64();
        pane_prompts::record(&st, "test-rc", "please go ahead: amux grant 7K3Q").await;
        let id: i64 = st.store.read().unwrap().query_row("SELECT MAX(id) FROM cmd_history WHERE session='test-rc'", [], |r| r.get(0)).unwrap();
        assert_eq!(row_qualifies_at(&st, id, now), Err(MaxRefRefusal::NotRemoteOwnerSend));
    }

    fn cite_id(id: i64) -> Cite {
        Cite { row_id: Some(id), session: None, time: None }
    }

    #[test]
    fn a_citation_of_another_lanes_typed_row_qualifies() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        let p = put(&st, pane("test-desk", "make test-alpha a lead", NOW - 30.0));
        let got = cite_row_at(&st, Some(&cite_id(p)), "test-actor", NOW).unwrap();
        assert_eq!((got.row_id, got.session.as_str()), (p, "test-desk"));
        // By session and time: the nearest qualifying row in the window.
        let by_time = Cite { row_id: None, session: Some("test-desk".into()), time: Some(NOW) };
        assert_eq!(cite_row_at(&st, Some(&by_time), "test-actor", NOW).unwrap().row_id, p);
        let mac = put(&st, R("test-mac", ASK, "user", "", "direct", Some("outside"), NOW));
        assert!(cite_row_at(&st, Some(&cite_id(mac)), "test-actor", NOW).is_ok(), "another lane's Outside send");
    }

    #[test]
    fn own_lane_delivered_stale_or_missing_citations_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        assert_eq!(cite_row_at(&st, None, "test-actor", NOW), Err(MaxRefRefusal::Missing));
        let own = put(&st, pane("test-actor", ASK, NOW));
        assert_eq!(cite_row_at(&st, Some(&cite_id(own)), "test-actor", NOW), Err(MaxRefRefusal::OwnLane), "own-lane pane row");
        let own_out = put(&st, R("test-actor", "amux x outside", "user", "", "direct", Some("outside"), NOW));
        assert_eq!(cite_row_at(&st, Some(&cite_id(own_out)), "test-actor", NOW), Err(MaxRefRefusal::OwnLane), "own-lane Outside");
        let own_fwd = put(&st, R("test-actor", "amux x forwarded", "user", "", "direct", Some("remote-forwarded"), NOW));
        assert_eq!(cite_row_at(&st, Some(&cite_id(own_fwd)), "test-actor", NOW), Err(MaxRefRefusal::OwnLane), "own-lane forwarded Remote");
        let own_remote = put(&st, R("test-actor", "amux x remote", "user", "", "direct", Some("remote"), NOW));
        assert!(cite_row_at(&st, Some(&cite_id(own_remote)), "test-actor", NOW).is_ok(), "own-lane non-local Remote");
        let stamped = put(&st, pane("test-desk", &format!("[amux-origin: test-peer] {ASK}"), NOW));
        assert_eq!(cite_row_at(&st, Some(&cite_id(stamped)), "test-actor", NOW), Err(MaxRefRefusal::NotHumanTyped), "delivered");
        let old = put(&st, pane("test-desk2", ASK, NOW - MAX_REF_MAX_AGE_S - 1.0));
        assert_eq!(cite_row_at(&st, Some(&cite_id(old)), "test-actor", NOW), Err(MaxRefRefusal::Stale));
        let far = Cite { row_id: Some(old), session: None, time: Some(NOW) };
        assert_eq!(cite_row_at(&st, Some(&far), "test-actor", NOW), Err(MaxRefRefusal::OutsideWindow));
        assert_eq!(cite_row_at(&st, Some(&cite_id(999_999)), "test-actor", NOW), Err(MaxRefRefusal::NoSuchRow));
    }

    #[test]
    fn a_forwarded_remote_send_is_never_approval() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        let fwd = put(&st, R("test-desk", ASK, "user", "", "direct", Some("remote-forwarded"), NOW));
        assert_eq!(q(&st, fwd), Err(MaxRefRefusal::NotRemoteOwnerSend));
    }

}
