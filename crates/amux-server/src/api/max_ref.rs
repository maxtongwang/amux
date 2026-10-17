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
//! SESSION RULE ([`max_ref_session_allowed`], pending dev-lead's ruling): in
//! the acting worker's OWN lane only a Remote owner send counts. A lane can
//! mint its own pane rows through `/api/sessions/<lane>/report`, and a worker
//! reaches Outside through `launchctl submit`.
//!
//! KNOWN LIMITS (ai-config#136). Workers run as the owner's macOS user. A
//! same-user process can type into the owner's pane with raw `tmux send-keys`;
//! and any lane's process can post its own UserPromptSubmit observation, which
//! yields a pane row in THAT lane this module cannot tell from the owner's.
//! Only a Remote owner send (or the Remote-verified Approve button) is proof
//! no local process can produce. Closing the rest needs an OS boundary.
use super::pane_prompts;
use super::AppState;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MaxRefRefusal {
    NoSuchRow,
    NotHumanTyped,
    Stale,
    /// The only human shape is a row in the actor's own lane, which a lane can mint.
    OwnLane,
}

impl MaxRefRefusal {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::NoSuchRow => "max_ref_no_such_row",
            Self::NotHumanTyped => "max_ref_not_human_typed",
            Self::Stale => "max_ref_stale",
            Self::OwnLane => "max_ref_own_lane",
        }
    }
}

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
    /// process_origin = remote: the tailnet forwarder from another host.
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
                remote: po == "remote",
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

/// Whether a human row in `session` may speak for `actor` (the interim rule,
/// one function so a ruling is a one-line change): a pane row or an Outside
/// owner send counts only from a lane other than the actor's; in the actor's
/// own lane only a Remote owner send counts.
pub(crate) fn max_ref_session_allowed(actor: &str, session: &str, source: HumanSource, remote: bool) -> bool {
    if actor.is_empty() {
        return true;
    }
    let own = session == actor || super::chat_worker::companion_parent(actor) == Some(session);
    !own || (source == HumanSource::OwnerSend && remote)
}

/// Whether Messages row `row_id` qualifies as the owner's own words for a
/// grant `actor` asked for: a human shape, not an amux delivery, fresh, and
/// allowed by [`max_ref_session_allowed`].
pub(crate) fn row_qualifies(state: &AppState, row_id: i64, actor: &str) -> Result<(), MaxRefRefusal> {
    row_qualifies_at(state, row_id, actor, crate::config::now_f64())
}

fn row_qualifies_at(state: &AppState, row_id: i64, actor: &str, now: f64) -> Result<(), MaxRefRefusal> {
    let row = rows(state, "id=?1", &[&row_id]).map_err(|_| MaxRefRefusal::NoSuchRow)?.into_iter().next().ok_or(MaxRefRefusal::NoSuchRow)?;
    let source = row.source.ok_or(MaxRefRefusal::NotHumanTyped)?;
    if now - row.ts > MAX_REF_MAX_AGE_S {
        return Err(MaxRefRefusal::Stale);
    }
    if is_amux_delivery(state, &row.session, row.id, &row.text, row.ts, source) {
        return Err(MaxRefRefusal::NotHumanTyped);
    }
    if !max_ref_session_allowed(actor, &row.session, source, row.remote) {
        return Err(MaxRefRefusal::OwnLane);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: f64 = 1_800_000_000.0;
    const ACTOR: &str = "test-actor";

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

    fn q(st: &AppState, id: i64) -> Result<(), MaxRefRefusal> {
        row_qualifies_at(st, id, ACTOR, NOW)
    }

    const ASK: &str = "amux grant 7K3Q";

    #[test]
    fn a_typed_pane_row_in_another_lane_qualifies() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        let id = put(&st, pane("test-desk", ASK, NOW - 100.0));
        assert_eq!(q(&st, id), Ok(()));
        assert_eq!(q(&st, id + 999), Err(MaxRefRefusal::NoSuchRow));
    }

    #[test]
    fn a_stamped_or_relayed_row_is_not_human_typed() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        let a = put(&st, pane("test-relay", &format!("[amux-origin: test-peer — server-verified] {ASK}"), NOW));
        assert_eq!(q(&st, a), Err(MaxRefRefusal::NotHumanTyped));
        let b = put(&st, R("test-relay2", ASK, "session", "test-peer", "direct", None, NOW));
        assert_eq!(q(&st, b), Err(MaxRefRefusal::NotHumanTyped));
    }

    #[test]
    fn a_pane_row_matching_an_amux_delivery_is_not_human_typed() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        let long = "please approve it now: amux grant 7K3Q";
        put(&st, R("test-hist", long, "session", "test-peer", "direct", None, NOW - 60.0));
        let a = put(&st, pane("test-hist", long, NOW));
        assert_eq!(q(&st, a), Err(MaxRefRefusal::NotHumanTyped), "echo of a cmd_history delivery");
        pane_prompts::note_delivery("test-ring", long);
        let b = put(&st, pane("test-ring", long, NOW));
        assert_eq!(q(&st, b), Err(MaxRefRefusal::NotHumanTyped), "echo of the delivery ring");
    }

    #[test]
    fn owner_sends_need_a_process_verdict() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        let none = put(&st, R("test-dash", ASK, "user", "", "direct", None, NOW));
        assert_eq!(q(&st, none), Err(MaxRefRefusal::NotHumanTyped), "no verdict");
        let worker = put(&st, R("test-cli", ASK, "user", "", "direct", Some("worker"), NOW));
        assert_eq!(q(&st, worker), Err(MaxRefRefusal::NotHumanTyped), "worker verdict");
        let mac = put(&st, R("test-mac", ASK, "user", "", "direct", Some("outside"), NOW));
        assert_eq!(q(&st, mac), Ok(()), "Outside in another lane");
        let phone = put(&st, R("test-phone", ASK, "user", "", "queued", Some("remote"), NOW));
        assert_eq!(q(&st, phone), Ok(()), "Remote in another lane");
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
            let id = put(&st, R(&format!("test-kind-{i}"), ASK, ty, origin, delivery, Some("outside"), NOW));
            assert_eq!(q(&st, id), Err(MaxRefRefusal::NotHumanTyped), "{ty}/{origin}/{delivery}");
        }
    }

    #[test]
    fn an_old_row_is_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        let old = put(&st, pane("test-desk", ASK, NOW - MAX_REF_MAX_AGE_S - 1.0));
        assert_eq!(q(&st, old), Err(MaxRefRefusal::Stale));
        let edge = put(&st, pane("test-desk2", ASK, NOW - MAX_REF_MAX_AGE_S));
        assert_eq!(q(&st, edge), Ok(()), "the max age itself is allowed");
    }

    #[test]
    fn in_the_actors_own_lane_only_a_remote_owner_send_counts() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        let p = put(&st, pane(ACTOR, ASK, NOW));
        assert_eq!(q(&st, p), Err(MaxRefRefusal::OwnLane), "own-lane pane row");
        let o = put(&st, R(ACTOR, "amux grant ABCD", "user", "", "direct", Some("outside"), NOW));
        assert_eq!(q(&st, o), Err(MaxRefRefusal::OwnLane), "own-lane Outside send (launchctl reaches Outside)");
        let r = put(&st, R(ACTOR, "amux grant WXYZ", "user", "", "direct", Some("remote"), NOW));
        assert_eq!(q(&st, r), Ok(()), "own-lane Remote owner send");
        assert!(!max_ref_session_allowed("test-actor@chat", ACTOR, HumanSource::Pane, false), "own lane via its Chat tab");
    }

    #[test]
    fn codes_are_stable() {
        for (r, c) in [
            (MaxRefRefusal::NoSuchRow, "max_ref_no_such_row"),
            (MaxRefRefusal::NotHumanTyped, "max_ref_not_human_typed"),
            (MaxRefRefusal::Stale, "max_ref_stale"),
            (MaxRefRefusal::OwnLane, "max_ref_own_lane"),
        ] {
            assert_eq!(r.code(), c);
        }
    }

    /// Remote Control: a real user turn through UserPromptSubmit becomes a pane
    /// row that qualifies; stamped or delivery-matched text is never recorded.
    #[tokio::test]
    async fn a_remote_control_prompt_through_the_hook_qualifies() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = crate::api::settings::test_env::set_home(tmp.path());
        let st = state(tmp.path());
        let now = crate::config::now_f64();
        pane_prompts::record(&st, "test-rc", "please go ahead: amux grant 7K3Q").await;
        let id: i64 = st.store.read().unwrap().query_row("SELECT MAX(id) FROM cmd_history WHERE session='test-rc'", [], |r| r.get(0)).unwrap();
        assert_eq!(row_qualifies_at(&st, id, ACTOR, now), Ok(()));
    }
}
