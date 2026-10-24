// Owner message identity and the single affirmative allowlist — explicit approval requests live in delegation_requests.rs.
//! Native session and wrapped fields own approval provenance. Legacy source
//! shapes remain for history/title UX and close-only route observation.
use super::AppState;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MaxRefRefusal {
    NoSuchRow,
}

impl MaxRefRefusal {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::NoSuchRow => "max_ref_no_such_row",
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
pub(crate) struct Row {
    pub(crate) id: i64,
    pub(crate) session: String,
    pub(crate) text: String,
    pub(crate) ts: f64,
    pub(crate) source: Option<HumanSource>,
    pub(crate) incarnation: Option<String>,
    pub(crate) input_boundary: bool,
    pub(crate) native_session: Option<String>,
    pub(crate) native_prompt: bool,
    pub(crate) wrapped: bool,
    pub(crate) reserved_marker: bool,
}

/// Which human shape a row has, if any (see the module comment).
fn human_source(ctype: &str, origin: &str, delivery: &str, process_origin: &str) -> Option<HumanSource> {
    if !matches!(ctype, "user" | "chat") {
        return None;
    }
    if ctype == "user" && delivery == "pane" && origin == "pane" && matches!(process_origin, "outside" | "remote" | "remote-forwarded") {
        return Some(HumanSource::Pane);
    }
    // `origin='pane'` is the recorder's alone (the PANE shape); a direct or
    // queued row carrying it is no owner send.
    if origin != "pane" && matches!(delivery, "direct" | "queued") && matches!(process_origin, "outside" | "remote" | "remote-forwarded") {
        return Some(HumanSource::OwnerSend);
    }
    None
}

fn map_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
            let (ctype, origin, delivery, po): (String, String, String, String) = (r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?);
            Ok(Row {
                id: r.get(0)?,
                session: r.get(1)?,
                text: r.get(2)?,
                ts: r.get::<_, i64>(7)? as f64 / 1000.0,
                source: human_source(&ctype, &origin, &delivery, &po),
                incarnation: r.get(8)?,
                native_session: r.get(9)?,
                native_prompt: delivery == "approval_native_prompt",
                reserved_marker: delivery == "approval_reserved_prompt",
                wrapped: r.get::<_,Option<bool>>(10)?.unwrap_or(true),
                input_boundary: delivery == "approval_input_uncertain"
                    || (matches!(ctype.as_str(), "user" | "chat") && human_source(&ctype, &origin, &delivery, &po).is_none()),
            })
}

pub(crate) fn one_in(conn: &rusqlite::Connection, id: i64) -> rusqlite::Result<Row> {
    conn.query_row(&format!("SELECT {ROW_COLS} FROM cmd_history WHERE id=?1"),[id],map_row)
}

const ROW_COLS: &str = "id, session, text, COALESCE(delegation_message_type,type,''), COALESCE(origin,''), COALESCE(delivery,''), COALESCE(process_origin,''), ts, delegation_session_incarnation, approval_native_session, approval_wrapped";

fn rows(state: &AppState, sql_where: &str, params: &[&dyn rusqlite::ToSql]) -> Result<Vec<Row>, String> {
    let conn = state.store.read().map_err(|e| e.to_string())?;
    let sql = format!("SELECT {ROW_COLS} FROM cmd_history WHERE {sql_where} LIMIT {ROW_LIMIT}");
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let mapped = stmt
        .query_map(params, map_row)
        .map_err(|e| e.to_string())?;
    mapped.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

/// One owning allowlist: trimmed, case-insensitive whole replies only.
const AFFIRMATIVE_REPLIES: &[&str] = &["yes", "y", "approve", "ok"];

pub(crate) fn reply_is_affirmative(text: &str) -> bool {
    AFFIRMATIVE_REPLIES.contains(&text.trim().strip_suffix('.').unwrap_or(text.trim()).to_ascii_lowercase().as_str())
}

pub(crate) fn one(state: &AppState, id: i64) -> Result<Option<Row>, MaxRefRefusal> {
    rows(state, "id=?1", &[&id]).map(|r| r.into_iter().next()).map_err(|_| MaxRefRefusal::NoSuchRow)
}

/// Fast refusal, also used for a replay whose change has become a no-op.
/// The INSERT's unique index is the final authority against racing requests.
pub(crate) fn reply_spent(conn: &rusqlite::Connection, reply: i64) -> rusqlite::Result<bool> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM _amux_delegation_grants WHERE evidence_row=?1)", [reply], |r| r.get(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn approval_reply_requires_explicit_affirmative_text() {
        for yes in ["yes", "y", "approve", "ok", "YES ", "yes.", "Ok."] { assert!(reply_is_affirmative(yes)); }
        for no in ["no", "yes?", "yes please", "okay", "", "\n", "yep", "approved", "go ahead"] {
            assert!(!reply_is_affirmative(no), "{no:?}");
        }
    }
}
