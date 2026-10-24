// Approval v3.1 decisions and short-code allocation — dev-lead answers 2026-10-24 (a-d).
// Pure doubt checks share one policy; ledger decisions and audit commit together.
use super::max_ref;
use rusqlite::{params, Connection};

pub(super) const SHORT_CODE_REUSE_DAYS: i64 = 7;
const SECONDS_PER_DAY: f64 = 86_400.0;
const SHORT_CODE_ATTEMPTS: usize = 128;
pub(super) const PLAIN_YES_WINDOW_MIN: i64 = 30;
const SECONDS_PER_MINUTE: f64 = 60.0;
const RESERVED_MARKER: &str = "[amux approval]";

/// Allocator's clock and entropy seams are explicit. Expiry closes at its
/// original Unix-second deadline, so delayed housekeeping cannot extend reuse.
pub(crate) fn allocate_short_code(conn: &Connection, now_s: f64, mut next: impl FnMut() -> Option<String>) -> rusqlite::Result<String> {
    conn.execute("UPDATE _amux_delegation_grants SET approval_closed_at=expires_at WHERE approval_closed_at IS NULL AND expires_at<=?1", [now_s])?;
    for _ in 0..SHORT_CODE_ATTEMPTS {
        let Some(code)=next() else { break; };
        let occupied: bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM _amux_delegation_grants WHERE short_code=?1 COLLATE NOCASE AND (approval_closed_at IS NULL OR approval_closed_at>?2))",
            params![code,now_s-SHORT_CODE_REUSE_DAYS as f64*SECONDS_PER_DAY],|r|r.get(0))?;
        if !occupied { return Ok(code); }
    }
    tracing::warn!(verdict="approval_code_exhausted",measured=true,n_considered=SHORT_CODE_ATTEMPTS,"short code allocation exhausted; no request created");
    Err(rusqlite::Error::InvalidParameterName("approval_code_exhausted".into()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DoubtReason { FirstPrompt, LatestTurn, SeveralOpen, Stale, Changed, Unverifiable }
impl DoubtReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::FirstPrompt | Self::LatestTurn => "text-between",
            Self::SeveralOpen => "several-open", Self::Stale => "stale-30min",
            Self::Changed => "changed", Self::Unverifiable => "unverifiable",
        }
    }
}

pub(super) struct PlainYesFacts {
    pub first_prompt: bool, pub latest_turn: bool, pub single_open: bool,
    pub recent: bool, pub unchanged: bool, pub verifiable: bool,
    pub confirm_required: bool,
}

/// Conditions (a-f), independently testable. Confirmation is sticky: another
/// plain yes cannot fund even if the ambiguity has subsequently disappeared.
pub(super) fn plain_yes_without_doubt(f: &PlainYesFacts) -> Result<(), DoubtReason> {
    if !f.verifiable { return Err(DoubtReason::Unverifiable); } // (f)
    if !f.first_prompt { return Err(DoubtReason::FirstPrompt); } // (a)
    if !f.latest_turn { return Err(DoubtReason::LatestTurn); } // (b)
    if !f.single_open { return Err(DoubtReason::SeveralOpen); } // (c)
    if !f.recent { return Err(DoubtReason::Stale); } // (d)
    if !f.unchanged { return Err(DoubtReason::Changed); } // (e)
    if f.confirm_required { return Err(DoubtReason::FirstPrompt); }
    Ok(())
}

/// Include exact operands, requester and incarnations. Display takes this
/// snapshot; a changed question or endpoint is also a changed request.
pub(super) fn request_snapshot(conn: &Connection, id: &str) -> rusqlite::Result<String> {
    conn.query_row("SELECT actor,actor_incarnation,change_spec,method,path,body,ask_text,target_incarnation,reply_incarnation FROM _amux_delegation_grants WHERE code=?1",[id],|r| {
        let values=(0..9).map(|i|r.get::<_,Option<String>>(i)).collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(serde_json::to_string(&values).expect("strings serialize"))
    })
}

struct Request {
    id: String, short: String, shown: Option<i64>, shown_s: Option<f64>,
    sid: Option<String>, incarnation: Option<String>, run: Option<String>, seq: Option<i64>,
    snapshot: Option<String>, confirm: bool, receipt: Option<String>, question: String, ask: Option<i64>,
}

fn open_requests(conn: &Connection, now_s: f64) -> rusqlite::Result<Vec<Request>> {
    let mut st=conn.prepare("SELECT g.code,g.short_code,g.shown_row,h.ts/1000.0,h.approval_native_session,g.reply_incarnation,h.approval_run,h.approval_sequence,g.shown_snapshot,g.confirm_required,h.text,g.ask_text,g.ask_row FROM _amux_delegation_grants g LEFT JOIN cmd_history h ON h.id=g.shown_row AND h.delivery='approval_pane_display' WHERE g.status='pending' AND g.approval_closed_at IS NULL AND g.resolution_row IS NULL AND g.short_code IS NOT NULL AND g.expires_at>?1 ORDER BY g.ask_row LIMIT ?2")?;
    let rows=st.query_map(params![now_s,super::RESOLUTION_SCAN_LIMIT as i64+1],|r|Ok(Request {
        id:r.get(0)?,short:r.get(1)?,shown:r.get(2)?,shown_s:r.get(3)?,sid:r.get(4)?,
        incarnation:r.get(5)?,run:r.get(6)?,seq:r.get(7)?,snapshot:r.get(8)?,confirm:r.get(9)?,receipt:r.get(10)?,question:r.get(11)?,ask:r.get(12)?,
    }))?;
    let rows=rows.collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.len()>super::RESOLUTION_SCAN_LIMIT { return Err(rusqlite::Error::InvalidParameterName("approval_scan_full".into())); }
    Ok(rows)
}

fn approve_reply(text: &str) -> Option<Option<String>> {
    let mut words=text.split_whitespace();
    let word=words.next()?;
    if !max_ref::reply_is_affirmative(word) { return None; }
    let code=words.next().map(|s|s.strip_suffix('.').unwrap_or(s).to_ascii_uppercase());
    if words.next().is_some() { return None; }
    Some(code)
}

pub(super) fn approve_shaped(text: &str) -> bool {
    approve_reply(text).is_some() || (super::prompt_wrapped(text)
        && text.split_once('\n').is_some_and(|(_,body)|approve_reply(body).is_some()))
}

fn context(verb: &str, code: &str) -> String { format!("[amux approval] {verb} {code}") }
fn confirm(code: &str, reason: DoubtReason) -> String {
    format!("[amux approval] confirm needed {code} (reason: {})",reason.as_str())
}

pub(super) fn resolve(conn: &Connection, row: &max_ref::Row, owner_input: bool) -> rusqlite::Result<()> {
    let done:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM _amux_delegation_resolutions WHERE reply_row=?1)",[row.id],|r|r.get(0))?;
    if done { return Ok(()); }
    let now_s=crate::config::now_f64();
    let (run,seq):(Option<String>,Option<i64>)=conn.query_row("SELECT approval_run,approval_sequence FROM cmd_history WHERE id=?1",[row.id],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let native=run.is_some();
    let ignored=row.reserved_marker || row.text.contains(RESERVED_MARKER);
    let requests=open_requests(conn,now_s)?;
    let open_total:i64=conn.query_row("SELECT COUNT(*) FROM _amux_delegation_grants WHERE status='pending' AND approval_closed_at IS NULL AND resolution_row IS NULL AND ask_row IS NOT NULL AND expires_at>?1",[now_s],|r|r.get(0))?;
    let latest:i64=conn.query_row("SELECT COALESCE(MAX(id),0) FROM cmd_history WHERE session=?1 AND id<?2 AND (COALESCE(delegation_message_type,type) IN ('user','chat') OR type='assistant' OR delivery='approval_output' OR (type='session' AND origin=session AND delivery<>'approval_request')) AND (COALESCE(delegation_message_type,type) NOT IN ('user','chat') OR (delivery<>'approval_reserved_prompt' AND instr(COALESCE(text,''),'[amux approval]')=0))",params![row.session,row.id],|r|r.get(0))?;
    let mut intent=approve_reply(&row.text);
    if row.wrapped && intent.is_none() {
        // A recognized envelope can carry a reply, but can never prove its author.
        intent=row.text.split_once('\n').and_then(|(_,body)|approve_reply(body));
    }
    let mut lines=Vec::new();
    let mut funded=None;
    let mut verdict="request_no_candidate";
    let mut note="no open displayed request".to_string();
    if ignored {
        lines.push("[amux approval] ignored reserved marker in prompt".to_string());
        verdict="request_reserved_marker_ignored";
    } else if native && intent.is_some() {
        let coded=intent.as_ref().and_then(|v|v.as_deref());
        let candidates:Vec<_>=requests.iter().filter(|q|coded.is_none_or(|c|q.short==c)).collect();
        if let Some(code)=coded.filter(|_|candidates.is_empty()) {
            lines.push(context("no open approval",code));
            verdict="request_no_open_code";
        } else {
            for q in candidates {
                let ordered=q.run.is_some() && q.run==run && q.seq.zip(seq).is_some_and(|(stop,submit)|stop<submit)
                    && q.shown.is_some_and(|shown|shown<row.id);
                let exact_receipt=q.receipt.as_deref().is_some_and(|text|super::receipt_matches(text,&q.short,&q.question,q.confirm));
                let verifiable=exact_receipt && row.ts<=now_s && now_s-row.ts<=max_ref::MAX_REF_MAX_AGE_S && owner_input && super::is_owner_session(&row.session)
                    && row.incarnation.is_some() && row.incarnation==super::incarnation(&row.session)
                    && row.incarnation==q.incarnation && row.native_session.is_some() && row.native_session==q.sid && ordered;
                let first_prompt=match q.shown {
                    Some(shown)=>!conn.query_row("SELECT EXISTS(SELECT 1 FROM cmd_history WHERE session=?1 AND id>?2 AND id<?3 AND COALESCE(delegation_message_type,type) IN ('user','chat') AND delivery<>'approval_reserved_prompt' AND instr(COALESCE(text,''),'[amux approval]')=0)",params![row.session,shown,row.id],|r|r.get::<_,bool>(0))?,
                    None=>false,
                };
                let facts=PlainYesFacts { first_prompt,latest_turn:q.shown==Some(latest),single_open:open_total==1,
                    recent:q.shown_s.is_some_and(|shown|row.ts>=shown && row.ts-shown<=PLAIN_YES_WINDOW_MIN as f64*SECONDS_PER_MINUTE),
                    unchanged:q.snapshot.as_deref()==Some(request_snapshot(conn,&q.id)?.as_str()),verifiable,confirm_required:q.confirm };
                let decision=if coded.is_some() {
                    if verifiable && facts.unchanged { Ok(()) } else { Err(if !verifiable { DoubtReason::Unverifiable } else { DoubtReason::Changed }) }
                } else { plain_yes_without_doubt(&facts) };
                match decision {
                    Ok(())=> {
                        conn.execute("UPDATE _amux_delegation_grants SET resolution_row=?2,resolution_code='request_resolved',resolution_note='native owner approval',approval_closed_at=?3 WHERE code=?1 AND approval_closed_at IS NULL",params![q.id,row.id,now_s])?;
                        funded=Some(q.id.clone());verdict="request_resolved";
                        lines.push(context("approved",&q.short));
                    },
                    Err(reason)=> {
                        conn.execute("UPDATE _amux_delegation_grants SET confirm_required=1,resolution_code='request_confirm_needed',resolution_note=?2 WHERE code=?1",params![q.id,reason.as_str()])?;
                        verdict="request_confirm_needed";note=reason.as_str().into();
                        lines.push(confirm(&q.short,reason));
                    },
                }
            }
        }
    } else {
        verdict=if owner_input { "request_reply_not_affirmative" } else { "request_reply_not_owner_input" };
        let latest_ask=requests.iter().filter_map(|q|q.ask).max();
        for q in requests.iter().filter(|q|q.shown==Some(latest) || (native && q.shown.is_none() && q.ask==latest_ask && q.ask.is_some_and(|ask|ask<row.id))) {
            conn.execute("UPDATE _amux_delegation_grants SET resolution_code=?2,resolution_note='declined by following input',approval_closed_at=?3 WHERE code=?1",params![q.id,verdict,now_s])?;
            if native { lines.push(context("declined",&q.short)); }
        }
    }
    let output=(!lines.is_empty()).then(||lines.join("\n"));
    conn.execute("INSERT INTO _amux_delegation_resolutions(reply_row,request_id,session,code,note,created_at,approval_context) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![row.id,funded,row.session,verdict,note,now_s,output])?;
    tracing::info!(reply_row=row.id,session=row.session,request_id=?funded,code=verdict,reason=note,verdict="delegation_request_resolution",measured=true,n_considered=requests.len(),"native approval decision committed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn clear() -> PlainYesFacts {
        PlainYesFacts {first_prompt:true,latest_turn:true,single_open:true,recent:true,unchanged:true,verifiable:true,confirm_required:false}
    }
    #[test] fn no_doubt_accepts_plain_yes() { assert_eq!(plain_yes_without_doubt(&clear()),Ok(())); }
    #[test] fn condition_a_first_prompt_alone() { let mut f=clear();f.first_prompt=false;assert_eq!(plain_yes_without_doubt(&f),Err(DoubtReason::FirstPrompt)); }
    #[test] fn condition_b_latest_turn_alone() { let mut f=clear();f.latest_turn=false;assert_eq!(plain_yes_without_doubt(&f),Err(DoubtReason::LatestTurn)); }
    #[test] fn condition_c_several_open_alone() { let mut f=clear();f.single_open=false;assert_eq!(plain_yes_without_doubt(&f),Err(DoubtReason::SeveralOpen)); }
    #[test] fn condition_d_stale_alone() { let mut f=clear();f.recent=false;assert_eq!(plain_yes_without_doubt(&f),Err(DoubtReason::Stale)); }
    #[test] fn condition_e_changed_alone() { let mut f=clear();f.unchanged=false;assert_eq!(plain_yes_without_doubt(&f),Err(DoubtReason::Changed)); }
    #[test] fn condition_f_unverifiable_alone() { let mut f=clear();f.verifiable=false;assert_eq!(plain_yes_without_doubt(&f),Err(DoubtReason::Unverifiable)); }
    #[test] fn confirmation_stays_required() { let mut f=clear();f.confirm_required=true;assert!(plain_yes_without_doubt(&f).is_err()); }
    #[test] fn decision_lines_are_byte_exact() {
        assert_eq!(context("approved","A7K2"),"[amux approval] approved A7K2");
        assert_eq!(context("declined","A7K2"),"[amux approval] declined A7K2");
        assert_eq!(context("no open approval","A7K2"),"[amux approval] no open approval A7K2");
        for (reason,name) in [(DoubtReason::FirstPrompt,"text-between"),(DoubtReason::LatestTurn,"text-between"),
            (DoubtReason::SeveralOpen,"several-open"),(DoubtReason::Stale,"stale-30min"),
            (DoubtReason::Changed,"changed"),(DoubtReason::Unverifiable,"unverifiable")] {
            assert_eq!(confirm("A7K2",reason),format!("[amux approval] confirm needed A7K2 (reason: {name})"));
        }
    }
    #[test] fn code_retries_retention_and_exhaustion_use_clock_seam() {
        let tmp=tempfile::tempdir().unwrap();
        let store=crate::db::Store::open(&tmp.path().join("codes.db")).unwrap();
        store.write(|conn| {
            let now=1_000_000.0;
            conn.execute("INSERT INTO _amux_delegation_grants(code,short_code,actor,class,action,target,method,path,body,sentence,created_at,expires_at,status) VALUES('dlg_one','A7K2','a','a','a','a','PATCH','/a','{}','a',1,2000000,'pending')",[])?;
            let mut codes=["a7k2","B7K2"].into_iter();
            assert_eq!(allocate_short_code(conn,now,||codes.next().map(str::to_string))?,"B7K2");
            assert!(allocate_short_code(conn,now,||Some("A7K2".into())).is_err());
            conn.execute("UPDATE _amux_delegation_grants SET approval_closed_at=?1",[now])?;
            assert!(allocate_short_code(conn,now+7.0*86_400.0-1.0,||Some("A7K2".into())).is_err());
            assert_eq!(allocate_short_code(conn,now+7.0*86_400.0,||Some("A7K2".into()))?,"A7K2");
            Ok(crate::db::WriteOutcome {applied:true,events:vec![]})
        }).unwrap();
    }
}
