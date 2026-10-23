// Which Messages rows are the owner's own words: the row-qualification check for typed grant approvals — refs maxtongwang/ai-config#160 (piece b).
// It also verifies cite mode's (ask, reply) citation pair (review B4); grants themselves live in api/delegation_grants.rs.
//! HUMAN-TYPED. A `cmd_history` row is the owner's own words in exactly two
//! shapes:
//! - PANE: `type='user'`, `delivery='pane'`, `origin='pane'`, written only by
//!   `pane_prompts::record` from the provider's UserPromptSubmit hook after it
//!   ruled out an amux delivery (Claude Remote Control input lands here too).
//! - OWNER SEND: `type='user'`, `delivery` `direct` or `queued`, and
//!   `process_origin` `outside`, `remote` or `remote-forwarded` (migration 0124), proven by process
//!   attribution (`process_identity`), not by a header the client chose.
//!
//! Every candidate then passes [`is_amux_delivery`], which refuses an `[amux`
//! stamp, provider-injected text, and any text matching what amux delivered to
//! that lane (the `pane_prompts` delivery ring and the lane's other
//! `cmd_history` rows). A relayed text that matches exactly never counts.
//!
//! TWO RULES (dev-lead, 2026-10-17), each one function:
//! - CITATION ([`cite_pair`], cite mode, the default; ruling dev-lead
//!   2026-10-19, PR #18 review B4): a widening change cites a PAIR. (1) The
//!   ask: a message the requesting agent sent into a session (a `session` row
//!   whose origin is the agent) that names the target worker or layer AND
//!   each exact directed change, including its value and target, through
//!   [`names_changes`]. Examples: `enable yolo for worker-a`, `set flags to
//!   --dangerously-skip-permissions for worker-a`, `remove dev from worker-a`.
//!   Separate changes use separate clauses. Negated request clauses refuse;
//!   values containing negation are literal operands, never request verbs.
//!   (2) The reply: Max's own FIRST typed row in
//!   that session after the ask (human-typed, not an amux delivery), within
//!   [`ASK_REPLY_WINDOW_S`] of it and at most a day old. In the agent's own
//!   lane only a non-local Remote owner send counts as the reply
//!   ([`cite_source_allowed`]). Matching is whole words, computed; no model
//!   judges relevance. Accepted limit (ai-config#136): another lane can mint
//!   a pane row through its own `/report`; in cite mode that is mitigated by
//!   the audit log, the leads feed and one-step undo, not prevented.
//!   Dev-lead rulings 2026-10-23 (second B1 in the approval-reuse class):
//!   the ask must be that actor's latest unanswered message in the session
//!   before the reply. The reply's entire text must match
//!   [`AFFIRMATIVE_REPLIES`] (case-insensitive, outer punctuation/space trimmed;
//!   questions always refuse). A reply funds one grant globally, across asks,
//!   actors, workers, routes and approval modes, including failed/undone grants.
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
    /// A citation in the actor's own lane that is not a non-local Remote owner send.
    OwnLane,
    /// A single quoted row instead of the (ask, reply) pair.
    PairRequired,
    /// Stable wire code `cite_pair_spent`; the spent unit is the reply alone.
    PairSpent,
    AskNotFound,
    /// The ask row is not a message the requesting agent sent.
    AskNotFromActor,
    AskMissingTarget,
    AskMissingSetting,
    TargetIdentityChanged,
    AskMissingChange,
    AskNotLatest,
    ReplyNotAffirmative,
    /// The reply is in a different session from the ask.
    WrongSession,
    /// The reply is not Max's first typed message after the ask.
    ReplyNotFirst,
    /// The reply came more than [`ASK_REPLY_WINDOW_S`] after the ask.
    ReplyOutsideWindow,
}

impl MaxRefRefusal {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::NoSuchRow => "max_ref_no_such_row",
            Self::NotHumanTyped => "max_ref_not_human_typed",
            Self::Stale => "max_ref_stale",
            Self::NotRemoteOwnerSend => "max_ref_not_remote_owner_send",
            Self::Missing => "cite_missing",
            Self::OwnLane => "max_ref_own_lane",
            Self::PairRequired => "cite_pair_required",
            Self::PairSpent => "cite_pair_spent",
            Self::AskNotFound => "cite_ask_not_found",
            Self::AskNotFromActor => "cite_ask_not_from_actor",
            Self::AskMissingTarget => "cite_ask_missing_target",
            Self::AskMissingSetting => "cite_ask_missing_setting",
            Self::TargetIdentityChanged => "cite_target_identity_changed",
            Self::AskMissingChange => "cite_ask_missing_change",
            Self::AskNotLatest => "cite_ask_not_latest",
            Self::ReplyNotAffirmative => "cite_reply_not_affirmative",
            Self::WrongSession => "cite_reply_wrong_session",
            Self::ReplyNotFirst => "cite_reply_not_first",
            Self::ReplyOutsideWindow => "cite_reply_outside_window",
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
    ctype: String,
    origin: String,
    /// process_origin = remote: a non-local peer (owner proof); never the forwarded verdict.
    remote: bool,
    worker_ids: String,
}

/// Which human shape a row has, if any (see the module comment).
fn human_source(ctype: &str, origin: &str, delivery: &str, process_origin: &str) -> Option<HumanSource> {
    if ctype != "user" {
        return None;
    }
    if delivery == "pane" && origin == "pane" {
        return Some(HumanSource::Pane);
    }
    // `origin='pane'` is the recorder's alone (the PANE shape); a direct or
    // queued row carrying it is no owner send.
    if origin != "pane" && matches!(delivery, "direct" | "queued") && matches!(process_origin, "outside" | "remote" | "remote-forwarded") {
        return Some(HumanSource::OwnerSend);
    }
    None
}

const ROW_COLS: &str = "id, session, text, COALESCE(type,''), COALESCE(origin,''), COALESCE(delivery,''), COALESCE(process_origin,''), ts, COALESCE(cite_worker_ids,'{}')";

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
                ctype,
                origin,
                worker_ids: r.get(8)?,
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

/// A citation (ruling, dev-lead 2026-10-19, review B4): the PAIR of
/// (1) `ask_row_id`, the asking message the requesting agent sent into a
/// session, naming the target and the setting in plain words, and
/// (2) `reply_row_id`, Max's own FIRST typed message in that same session
/// after it, within [`ASK_REPLY_WINDOW_S`]. A single quoted row (`row_id`, or
/// `session` and `time`) no longer qualifies.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize, PartialEq, Default)]
pub(crate) struct Cite {
    #[serde(default)]
    pub ask_row_id: Option<i64>,
    #[serde(default)]
    pub reply_row_id: Option<i64>,
    /// The retired single-row forms, kept only to answer them precisely.
    #[serde(default)]
    pub row_id: Option<i64>,
    #[serde(default)]
    pub session: Option<String>,
}

/// The pair a citation resolved to. `row_id`/`excerpt` are the reply's.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Cited {
    pub row_id: i64,
    pub session: String,
    pub excerpt: String,
    pub ask_row_id: i64,
    pub ask_text: String,
    pub reply_text: String,
    pub target_incarnation: Option<String>,
}

/// What the ask must name: the target (any one spelling) and EACH setting
/// (any one spelling per setting). Built by `delegation::cite_terms`.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CiteTerms {
    pub target: Vec<String>,
    pub settings: Vec<Vec<String>>,
    /// Worker name at request time; the ask must carry the same incarnation.
    pub worker: Option<String>,
    /// Each changed tag with its required direction (add/remove).
    pub tag_changes: Vec<(String, String)>,
    /// Each exact directed request; any spelling per request, all requests required.
    pub changes: Vec<Vec<ChangePhrase>>,
}

/// The reply must come within this long after the ask.
pub(crate) const ASK_REPLY_WINDOW_S: f64 = 7_200.0;

/// Entire accepted reply phrases, never substring/keyword matches. The
/// combined phrase retains the existing owner fixture. Additional prose,
/// negations, and questions fail closed rather than guessing owner intent.
const AFFIRMATIVE_REPLIES: &[&str] = &[
    "yes", "y", "yep", "approve", "approved", "go ahead", "do it", "ok to proceed", "yes, go ahead",
];

fn reply_is_affirmative(text: &str) -> bool {
    if text.contains('?') { return false; }
    let normalized = text.trim_matches(|c: char| c.is_whitespace() || c.is_ascii_punctuation())
        .to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ");
    AFFIRMATIVE_REPLIES.contains(&normalized.as_str())
}

/// THE citation rule (cite mode): another lane's human row counts; in the
/// actor's own lane only a non-local Remote owner send.
pub(crate) fn cite_source_allowed(actor: &str, session: &str, source: HumanSource, remote: bool) -> bool {
    let own = session == actor || super::chat_worker::companion_parent(actor) == Some(session);
    !own || (source == HumanSource::OwnerSend && remote)
}

/// Max's own typed row in its lane: human-typed, not an amux delivery.
fn typed_by_max(state: &AppState, row: &Row) -> Option<HumanSource> {
    let source = row.source?;
    (!is_amux_delivery(state, &row.session, row.id, &row.text, row.ts, source)).then_some(source)
}

/// `name` appears in `text` as a whole name: case-insensitive, and not part
/// of a longer name (no letter, digit, `-`, `_` or `.` on either side).
pub(crate) fn names_exactly(text: &str, name: &str) -> bool {
    let (t, n) = (text.to_lowercase(), name.trim().to_lowercase());
    if n.is_empty() {
        return false;
    }
    let part = |c: char| c.is_alphanumeric() || matches!(c, '-' | '_' | '.');
    t.match_indices(&n).any(|(i, _)| {
        let before = t[..i].chars().next_back();
        let after = t[i + n.len()..].chars().next();
        !before.is_some_and(part) && !after.is_some_and(part)
    })
}

/// A worker name containing a tag/setting word is not a mention of that
/// setting. Mask exact target names before checking the requested settings.
fn setting_text(text: &str, targets: &[String]) -> String {
    let mut text = text.to_lowercase();
    for target in targets {
        let target = target.to_lowercase();
        if target.is_empty() { continue; }
        let part = |c: char| c.is_alphanumeric() || matches!(c, '-' | '_' | '.');
        let spans: Vec<_> = text.match_indices(&target).filter(|(i, _)| {
            !text[..*i].chars().next_back().is_some_and(part)
                && !text[*i + target.len()..].chars().next().is_some_and(part)
        }).map(|(i, _)| i..i + target.len()).collect();
        for span in spans.into_iter().rev() { text.replace_range(span, " "); }
    }
    text
}

/// Lowercase, `-`/`_`/`:` as spaces, whitespace collapsed.
fn plain(s: &str) -> String {
    s.to_lowercase().chars().map(|c| if matches!(c, '-' | '_' | ':') { ' ' } else { c }).collect::<String>()
        .split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `phrase` appears in `text` as whole words (a plural `s` allowed), with
/// `-`, `_` and spaces interchangeable: "auto-continue" names auto continue.
pub(crate) fn names_phrase(text: &str, phrase: &str) -> bool {
    let (t, p) = (plain(text), plain(phrase));
    if p.is_empty() {
        return false;
    }
    t.match_indices(&p).any(|(i, _)| {
        let before = t[..i].chars().next_back();
        let rest = &t[i + p.len()..];
        let rest = rest.strip_prefix("es").or_else(|| rest.strip_prefix('s')).filter(|r| !r.starts_with(char::is_alphanumeric)).unwrap_or(rest);
        !before.is_some_and(char::is_alphanumeric) && !rest.starts_with(char::is_alphanumeric)
    })
}

/// Server-derived identities at ASK recording time. Missing/legacy bindings
/// fail closed at citation time; a name is never rebound to its new holder.
pub(crate) fn worker_bindings(text: &str) -> String {
    let home = crate::config::amux_home();
    let mut ids = std::collections::BTreeMap::new();
    // Look up only names in this message, never scan the fleet or mint an id.
    // Creation/legacy migration owns identity assignment. Missing ids fail closed.
    let text = text.to_lowercase();
    for name in text.split(|c: char| !(c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))) {
        let name = name.trim_end_matches('.');
        if !super::session_verbs::valid_session_name(name) { continue; }
        if let Some(id) = super::rename_identity::incarnation(&home, name) {
            ids.insert(name.to_string(), id);
        }
    }
    serde_json::to_string(&ids).expect("string map serializes")
}

/// A readable request plus its literal operand. Grammar/name casing is flexible;
/// the value's bytes (including whitespace and case) must match exactly.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ChangePhrase {
    pub text: String,
    literal: Option<String>,
}

impl From<String> for ChangePhrase {
    fn from(text: String) -> Self { Self { text, literal: None } }
}

impl ChangePhrase {
    fn with_value(text: String, literal: String) -> Self { Self { text, literal: Some(literal) } }

    fn pattern(&self) -> String {
        let grammar = |s: &str| regex::escape(s).replace(' ', r"\s+");
        match self.literal.as_deref().filter(|value| !value.is_empty()) {
            Some(value) => self.text.split(value).map(grammar).collect::<Vec<_>>()
                .join(&format!("(?-i:{})", regex::escape(value))),
            None => grammar(&self.text),
        }
    }
}

/// One grammar for EVERY cite action. Each alternative is an entire directed
/// request including its target and exact value, never independent keywords.
/// Semicolons/newlines/punctuation bound negation scope; conjunctions do not
/// erase a negation. Negated clauses cannot authorize a change.
/// This is deliberately bounded syntax, not a natural-language consent model.
pub(crate) fn names_changes(text: &str, changes: &[Vec<ChangePhrase>]) -> bool {
    let clauses = regex::Regex::new(r"[;?!\n]").expect("clause grammar");
    let negation = regex::Regex::new(r"(?i)\b(?:no|not|never|without|except|unless|leave|avoid|cannot|(?:do|does|did|ca|could|wo|would|should|is|are|was|were|has|have|had|must|need|sha)n['’]?t)\b").expect("negation grammar");
    !changes.is_empty() && changes.iter().all(|alternatives| {
        if alternatives.is_empty() { return false; }
        let choices = alternatives.iter().map(ChangePhrase::pattern).collect::<Vec<_>>().join("|");
        let pattern = format!(r"(?i)(?:^|[^\p{{L}}\p{{N}}_.-])(?P<change>{choices})(?:$|[^\p{{L}}\p{{N}}_.-])");
        regex::Regex::new(&pattern).is_ok_and(|request| request.captures_iter(text).any(|captures| {
            // Inspect around the request itself: delimiter characters matched
            // for whole-name boundaries must remain in the clause slices.
            let matched = captures.name("change").expect("request capture");
            let before = clauses.split(&text[..matched.start()]).last().unwrap_or("");
            let after = clauses.split(&text[matched.end()..]).next().unwrap_or("");
            !negation.is_match(before) && !negation.is_match(after)
        }))
    })
}

/// All action builders use the same directed setting/value forms. Strings
/// accept literal text or JSON quoting; objects/arrays use compact JSON.
pub(crate) fn setting_requests(settings: &[String], targets: &[String], value: &serde_json::Value) -> Vec<ChangePhrase> {
    let literal = value.as_str().filter(|s| !s.is_empty()).map(str::to_string).unwrap_or_else(|| value.to_string());
    let mut forms = Vec::new();
    for setting in settings {
        for target in targets {
            for setting in [setting.clone(), format!("the {setting}")] {
                for (relation, prefix) in ["for", "on"].into_iter().flat_map(|relation|
                    ["", "the ", "worker ", "group "].into_iter().map(move |prefix| (relation, prefix))) {
                    let tail = format!("{relation} {prefix}{target}");
                    forms.push(ChangePhrase::with_value(format!("set {setting} to {literal} {tail}"), literal.clone()));
                    forms.push(ChangePhrase::with_value(format!("set {setting} {tail} to {literal}"), literal.clone()));
                    if value.is_string() {
                        forms.push(ChangePhrase::with_value(format!("set {setting} to {value} {tail}"), value.to_string()));
                        forms.push(ChangePhrase::with_value(format!("set {setting} {tail} to {value}"), value.to_string()));
                    }
                    if let Some(enabled) = value.as_bool() {
                        for verb in if enabled { ["enable", "turn on", "allow"] } else { ["disable", "turn off", "disallow"] } {
                            forms.push(format!("{verb} {setting} {tail}").into());
                        }
                    }
                    if value.is_null() { forms.push(format!("remove {setting} {tail}").into()); }
                    if value.as_str() == Some("") { forms.push(format!("clear {setting} {tail}").into()); }
                }
            }
        }
    }
    forms
}

fn tag_requests(targets: &[String], tag: &str, direction: &str) -> Vec<ChangePhrase> {
    let preposition = if direction == "add" { "to" } else { "from" };
    targets.iter().flat_map(|target| ["", "the "].into_iter().flat_map(move |article| {
        ["", " tag", " tags"].into_iter().flat_map(move |noun| {
            ["", "worker "].into_iter().map(move |worker| format!("{direction} {article}{tag}{noun} {preposition} {worker}{target}").into())
        })
    })).collect()
}

/// Tags use the same shared matcher and negation rule as every setting.
pub(crate) fn tag_change_requests(targets: &[String], changes: &[(String, String)]) -> Vec<Vec<ChangePhrase>> {
    changes.iter().map(|(tag, direction)| tag_requests(targets, tag, direction)).collect()
}

/// Resolve and verify a citation pair for `actor` asking for `terms`.
pub(crate) fn cite_pair(state: &AppState, cite: Option<&Cite>, actor: &str, terms: &CiteTerms) -> Result<Cited, MaxRefRefusal> {
    cite_pair_at(state, cite, actor, terms, crate::config::now_f64())
}

fn one(state: &AppState, id: i64) -> Result<Option<Row>, MaxRefRefusal> {
    rows(state, "id=?1", &[&id]).map(|r| r.into_iter().next()).map_err(|_| MaxRefRefusal::NoSuchRow)
}

/// Fast refusal, also used for a replay whose change has become a no-op.
/// The INSERT's unique index is the final authority against racing requests.
pub(crate) fn reply_spent(conn: &rusqlite::Connection, reply: i64) -> rusqlite::Result<bool> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM _amux_delegation_grants WHERE evidence_row=?1)", [reply], |r| r.get(0))
}

/// A missing ask cannot bypass the global reply-consumption check.
pub(crate) fn pair_unspent(state: &AppState, cite: &Cite) -> Result<(), MaxRefRefusal> {
    let Some(reply) = cite.reply_row_id else { return Ok(()) };
    let conn = state.store.read().map_err(|_| MaxRefRefusal::NoSuchRow)?;
    let spent = reply_spent(&conn, reply).map_err(|_| MaxRefRefusal::NoSuchRow)?;
    if spent { return Err(MaxRefRefusal::PairSpent); }
    Ok(())
}

fn cite_pair_at(state: &AppState, cite: Option<&Cite>, actor: &str, terms: &CiteTerms, now: f64) -> Result<Cited, MaxRefRefusal> {
    let cite = cite.ok_or(MaxRefRefusal::Missing)?;
    pair_unspent(state, cite)?;
    let (Some(ask_id), Some(reply_id)) = (cite.ask_row_id, cite.reply_row_id) else {
        return Err(if cite.row_id.is_some() || cite.session.is_some() || cite.ask_row_id.is_some() || cite.reply_row_id.is_some() {
            MaxRefRefusal::PairRequired
        } else {
            MaxRefRefusal::Missing
        });
    };
    // (1) The ask: a message the actor itself sent into the session.
    let ask = one(state, ask_id)?.ok_or(MaxRefRefusal::AskNotFound)?;
    if ask.ctype != "session" || ask.origin != actor {
        return Err(MaxRefRefusal::AskNotFromActor);
    }
    if !terms.target.iter().any(|n| names_exactly(&ask.text, n)) {
        return Err(MaxRefRefusal::AskMissingTarget);
    }
    let settings_text = setting_text(&ask.text, &terms.target);
    if !terms.settings.iter().all(|alts| alts.iter().any(|p| names_phrase(&settings_text, p))) {
        return Err(MaxRefRefusal::AskMissingSetting);
    }
    if !names_changes(&ask.text, &terms.changes) {
        return Err(MaxRefRefusal::AskMissingChange);
    }
    let target_incarnation = if let Some(worker) = &terms.worker {
        let ids: std::collections::BTreeMap<String, String> = serde_json::from_str(&ask.worker_ids).unwrap_or_default();
        let current = super::rename_identity::incarnation(&crate::config::amux_home(), worker);
        if current.is_none() || ids.get(worker) != current.as_ref() {
            return Err(MaxRefRefusal::TargetIdentityChanged);
        }
        current
    } else { None };
    // (2) The reply: Max's own typed row in the same session.
    let reply = one(state, reply_id)?.ok_or(MaxRefRefusal::NoSuchRow)?;
    if reply.session != ask.session {
        return Err(MaxRefRefusal::WrongSession);
    }
    let source = typed_by_max(state, &reply).ok_or(MaxRefRefusal::NotHumanTyped)?;
    if !cite_source_allowed(actor, &reply.session, source, reply.remote) {
        return Err(MaxRefRefusal::OwnLane);
    }
    if now - reply.ts > MAX_REF_MAX_AGE_S {
        return Err(MaxRefRefusal::Stale);
    }
    if reply.id <= ask.id || reply.ts < ask.ts {
        return Err(MaxRefRefusal::ReplyNotFirst);
    }
    if reply.ts - ask.ts > ASK_REPLY_WINDOW_S {
        return Err(MaxRefRefusal::ReplyOutsideWindow);
    }
    // FIRST: no typed row of Max's sits between the ask and the reply. A
    // bounded read that comes back full cannot prove that, so it refuses.
    let between = rows(state, "session=?1 AND id>?2 AND id<?3 ORDER BY id", &[&ask.session, &ask.id, &reply.id])
        .map_err(|_| MaxRefRefusal::ReplyNotFirst)?;
    if between.len() as i64 >= ROW_LIMIT || between.iter().any(|r| typed_by_max(state, r).is_some()) {
        return Err(MaxRefRefusal::ReplyNotFirst);
    }
    // Every later session message from the actor supersedes its prior ask;
    // no attempt to infer which settings an intervening message concerns.
    if between.iter().any(|r| r.ctype == "session" && r.origin == actor) {
        return Err(MaxRefRefusal::AskNotLatest);
    }
    if !reply_is_affirmative(&reply.text) {
        return Err(MaxRefRefusal::ReplyNotAffirmative);
    }
    Ok(Cited {
        row_id: reply.id,
        session: reply.session,
        excerpt: reply.text.chars().take(200).collect(),
        ask_row_id: ask.id,
        ask_text: ask.text.chars().take(2000).collect(),
        reply_text: reply.text.chars().take(2000).collect(),
        target_incarnation,
    })
}

#[cfg(test)]
fn test_tag_change(text: &str, target: &str, tag: &str, direction: &str) -> bool {
    names_changes(text, &tag_change_requests(&[target.into()], &[(tag.into(), direction.into())]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delegation_tag_change_names_target_tag_and_direction_with_optional_tag_noun() {
        for ask in [
            "Max, may I add leads to review-dev-b?",
            "Max, may I add the leads tag to worker review-dev-b?",
            "Max, may I add leads tags to review-dev-b?",
        ] {
            assert!(test_tag_change(ask, "review-dev-b", "leads", "add"), "{ask}");
        }
        for ask in [
            "remove leads from review-dev-b",
            "remove the leads tag from review-dev-b",
        ] {
            assert!(test_tag_change(ask, "review-dev-b", "leads", "remove"), "{ask}");
        }
        for ask in [
            "remove leads from review-dev-b",
            "add ops to review-dev-b",
            "review-dev-b is busy; add leads to review-dev-a",
            "add leads to review-dev-b-other",
            "review-dev-b has leads",
            "add review-dev-b to leads",
            "don't add leads to review-dev-b",
            "do not add leads to review-dev-b and add leads to review-dev-b",
            "add leads to review-dev-b and leave review-dev-b alone",
        ] {
            assert!(!test_tag_change(ask, "review-dev-b", "leads", "add"), "{ask}");
        }
    }

    #[test]
    fn citation_change_grammar_preserves_negation_clause_boundaries() {
        let changes = vec![setting_requests(&["yolo".into()], &["test-alpha".into()], &serde_json::json!(true))];
        for good in [
            "Max, may I enable yolo for test-alpha? Reply yes or no.",
            "Do not change the other worker; enable yolo for test-alpha?",
            "I want to enable yolo for test-alpha?",
            "May I enable yolo for test-alpha! No other worker should change.",
        ] {
            assert!(names_changes(good, &changes), "{good}");
        }
        for bad in [
            "I don't want to enable yolo for test-alpha?",
            "I can't enable yolo for test-alpha?",
            "I shouldn't enable yolo for test-alpha?",
            "enable yolo for test-alpha, no yolo",
            "don't enable yolo for test-alpha and enable yolo for test-alpha",
        ] {
            assert!(!names_changes(bad, &changes), "{bad}");
        }
    }

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
            (MaxRefRefusal::PairRequired, "cite_pair_required"),
            (MaxRefRefusal::AskMissingTarget, "cite_ask_missing_target"),
            (MaxRefRefusal::AskMissingSetting, "cite_ask_missing_setting"),
            (MaxRefRefusal::ReplyNotFirst, "cite_reply_not_first"),
            (MaxRefRefusal::ReplyOutsideWindow, "cite_reply_outside_window"),
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

    const DESK: &str = "test-desk";
    const ACTOR: &str = "test-actor";
    const YOLO_ASK: &str = "Max, may I turn on yolo for test-alpha? Reply yes or no.";

    fn terms() -> CiteTerms {
        CiteTerms { target: vec!["test-alpha".into()], settings: vec![vec!["yolo".into(), "CC_FLAGS".into()]], changes: vec![setting_requests(&["yolo".into(), "CC_FLAGS".into()], &["test-alpha".into()], &serde_json::json!(true))], ..Default::default() }
    }

    /// The ask: a message `from` sent into `session` (a peer send's row shape).
    fn ask(st: &AppState, session: &str, from: &str, text: &str, ts: f64) -> i64 {
        put(st, R(session, text, "session", from, "direct", None, ts))
    }

    fn pair(a: i64, r: i64) -> Cite {
        Cite { ask_row_id: Some(a), reply_row_id: Some(r), ..Default::default() }
    }

    fn check(st: &AppState, c: &Cite) -> Result<Cited, MaxRefRefusal> {
        cite_pair_at(st, Some(c), ACTOR, &terms(), NOW)
    }

    #[test]
    fn citation_reply_requires_explicit_affirmative_text() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        for (i, text) in ["no", "wait", "not yet", "yes?", "yes?\"", "don't", "don't do it",
            "no, go ahead", "yes, don't proceed", "yes but wait", "yesterday", "approval", "ok"].iter().enumerate() {
            let lane = format!("test-negative-{i}");
            let a = ask(&st, &lane, ACTOR, YOLO_ASK, NOW - 60.0);
            let r = put(&st, pane(&lane, text, NOW - 30.0));
            assert_eq!(check(&st, &pair(a, r)).unwrap_err().code(), "cite_reply_not_affirmative", "{text:?}");
            let later = put(&st, pane(&lane, "yes", NOW - 10.0));
            assert_eq!(check(&st, &pair(a, later)).unwrap_err().code(), "cite_reply_not_first", "{text:?} already answered the ask; a later yes cannot replace it");
        }
        for (i, text) in ["yes", "Y", "Yep!", "approve", "APPROVED.", "go ahead", "do it", "ok to proceed", "yes, go ahead", "  \"YES!\"  "].iter().enumerate() {
            let lane = format!("test-positive-{i}");
            let a = ask(&st, &lane, ACTOR, YOLO_ASK, NOW - 60.0);
            let r = put(&st, pane(&lane, text, NOW - 30.0));
            assert!(check(&st, &pair(a, r)).is_ok(), "{text:?}");
        }
        assert!(!reply_is_affirmative(""), "empty replies never approve");
    }

    #[test]
    fn citation_reply_binds_only_the_actors_latest_unanswered_ask() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        let first = ask(&st, DESK, ACTOR, YOLO_ASK, NOW - 90.0);
        let latest = ask(&st, DESK, ACTOR, YOLO_ASK, NOW - 60.0);
        // Other actors and other sessions cannot replace this actor's ask.
        ask(&st, DESK, "test-peer", YOLO_ASK, NOW - 50.0);
        ask(&st, "test-other-desk", ACTOR, YOLO_ASK, NOW - 40.0);
        let reply = put(&st, pane(DESK, "yes", NOW - 30.0));
        assert_eq!(check(&st, &pair(first, reply)).unwrap_err().code(), "cite_ask_not_latest");
        assert!(check(&st, &pair(latest, reply)).is_ok());
        let later = put(&st, pane(DESK, "yes!", NOW - 20.0));
        assert_eq!(check(&st, &pair(latest, later)).unwrap_err().code(), "cite_reply_not_first", "an already answered ask cannot use a later reply");
        assert_eq!(check(&st, &pair(latest, put(&st, pane("test-other-desk", "yes", NOW - 10.0)))).unwrap_err().code(), "cite_reply_wrong_session");
    }

    /// Review B4 ruling: the ask names target and setting, the reply is Max's
    /// first typed message after it in that session, within 2 hours.
    #[test]
    fn a_correct_pair_passes_and_carries_both_texts() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        let a = ask(&st, DESK, ACTOR, YOLO_ASK, NOW - 600.0);
        let r = put(&st, pane(DESK, "yes", NOW - 500.0));
        let got = check(&st, &pair(a, r)).unwrap();
        assert_eq!((got.row_id, got.ask_row_id, got.session.as_str()), (r, a, DESK));
        assert_eq!((got.ask_text.as_str(), got.reply_text.as_str()), (YOLO_ASK, "yes"));
        // An owner send from the Mac counts as Max's typed reply too.
        let a2 = ask(&st, "test-desk2", ACTOR, "turn on CC_FLAGS for test-alpha?", NOW - 100.0);
        let r2 = put(&st, R("test-desk2", "go ahead", "user", "", "direct", Some("outside"), NOW - 50.0));
        assert!(check(&st, &pair(a2, r2)).is_ok());
    }

    #[test]
    fn citation_bindings_only_read_existing_identities_without_minting_on_mentions() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = crate::api::settings::test_env::set_home(tmp.path());
        std::fs::create_dir_all(tmp.path().join("sessions")).unwrap();
        let path = tmp.path().join("sessions/test-alpha.env");
        let legacy = b"CC_TAGS=dev\n";
        std::fs::write(&path,legacy).unwrap();
        assert_eq!(worker_bindings("test-alpha is busy"), "{}");
        assert_eq!(std::fs::read(&path).unwrap(), legacy);
        assert_eq!(worker_bindings(YOLO_ASK), "{}", "legacy identity is never minted by ask capture");
        assert_eq!(std::fs::read(&path).unwrap(), legacy);
        std::fs::write(&path,"CC_TAGS=dev\nCC_INCARNATION=existing-worker\n").unwrap();
        let before = std::fs::read(&path).unwrap();
        let ids: std::collections::BTreeMap<String,String> = serde_json::from_str(&worker_bindings("May I enable yolo for Test-Alpha?")).unwrap();
        assert_eq!(ids.get("test-alpha").map(String::as_str), Some("existing-worker"));
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn citation_identity_is_captured_at_ask_time_and_never_rebound() {
        let tmp = tempfile::tempdir().unwrap();
        let _home = crate::api::settings::test_env::set_home(tmp.path());
        std::fs::create_dir_all(tmp.path().join("sessions")).unwrap();
        let target = tmp.path().join("sessions/test-alpha.env");
        std::fs::write(&target,"CC_TAGS=dev\nCC_INCARNATION=original-worker\n").unwrap();
        let st = state(tmp.path());
        let ids = worker_bindings(YOLO_ASK);
        let parsed: std::collections::BTreeMap<String,String> = serde_json::from_str(&ids).unwrap();
        assert_eq!(parsed.get("test-alpha").map(String::as_str),Some("original-worker"));
        assert_eq!(worker_bindings("test-alpha2 has yolo"),"{}","whole names only");
        let a = ask(&st,DESK,ACTOR,YOLO_ASK,NOW-60.0);
        let r = put(&st,pane(DESK,"yes",NOW-30.0));
        let mut t = terms();
        t.worker = Some("test-alpha".into());
        assert_eq!(cite_pair_at(&st,Some(&pair(a,r)),ACTOR,&t,NOW).map(|_|()),Err(MaxRefRefusal::TargetIdentityChanged),"legacy ask has no binding");
        st.store.write(move |c| {
            c.execute("UPDATE cmd_history SET cite_worker_ids=?1 WHERE id=?2",rusqlite::params![ids,a])?;
            Ok(crate::db::WriteOutcome{applied:true,events:vec![]})
        }).unwrap();
        assert!(cite_pair_at(&st,Some(&pair(a,r)),ACTOR,&t,NOW).is_ok());
        std::fs::rename(&target,tmp.path().join("sessions/renamed.env")).unwrap();
        std::fs::write(&target,"CC_TAGS=dev\nCC_INCARNATION=new-holder\n").unwrap();
        assert_eq!(cite_pair_at(&st,Some(&pair(a,r)),ACTOR,&t,NOW).map(|_|()),Err(MaxRefRefusal::TargetIdentityChanged));
    }

    #[test]
    fn an_ask_missing_the_target_or_the_setting_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        for (i, (text, want)) in [
            ("Max, may I turn on yolo for test-beta?", MaxRefRefusal::AskMissingTarget),
            ("Max, may I turn on yolo for test-alpha2?", MaxRefRefusal::AskMissingTarget),
            ("Max, may I change test-alpha's settings?", MaxRefRefusal::AskMissingSetting),
            ("Max, may I turn on yolomode for test-alpha?", MaxRefRefusal::AskMissingSetting),
        ].into_iter().enumerate() {
            let lane = format!("test-lane-{i}");
            let a = ask(&st, &lane, ACTOR, text, NOW - 60.0);
            let r = put(&st, pane(&lane, "yes", NOW - 30.0));
            assert_eq!(check(&st, &pair(a, r)).map(|_| ()), Err(want), "{text}");
        }
    }

    #[test]
    fn an_owner_reply_that_is_not_the_first_after_the_ask_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        let a = ask(&st, DESK, ACTOR, YOLO_ASK, NOW - 600.0);
        let first = put(&st, pane(DESK, "yes", NOW - 500.0));
        let later = put(&st, pane(DESK, "yes!", NOW - 400.0));
        assert_eq!(check(&st, &pair(a, later)).map(|_| ()), Err(MaxRefRefusal::ReplyNotFirst));
        assert!(check(&st, &pair(a, first)).is_ok(), "the first typed reply is the one that counts");
        // A reply BEFORE the ask is not an answer to it.
        let early = put(&st, pane("test-desk3", "yes", NOW - 900.0));
        let a3 = ask(&st, "test-desk3", ACTOR, YOLO_ASK, NOW - 800.0);
        assert_eq!(check(&st, &pair(a3, early)).map(|_| ()), Err(MaxRefRefusal::ReplyNotFirst));
        // An amux delivery between them is not a reply of Max's.
        let a4 = ask(&st, "test-desk4", ACTOR, YOLO_ASK, NOW - 300.0);
        put(&st, pane("test-desk4", "[amux-origin: test-peer] status update", NOW - 200.0));
        let r4 = put(&st, pane("test-desk4", "yes", NOW - 100.0));
        assert!(check(&st, &pair(a4, r4)).is_ok());
    }

    #[test]
    fn a_reply_outside_the_two_hour_window_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        let a = ask(&st, DESK, ACTOR, YOLO_ASK, NOW - ASK_REPLY_WINDOW_S - 120.0);
        let r = put(&st, pane(DESK, "yes", NOW - 60.0));
        assert_eq!(check(&st, &pair(a, r)).map(|_| ()), Err(MaxRefRefusal::ReplyOutsideWindow));
        // An old "ok" stays stale whatever it answers.
        let a2 = ask(&st, "test-desk2", ACTOR, YOLO_ASK, NOW - MAX_REF_MAX_AGE_S - 200.0);
        let r2 = put(&st, pane("test-desk2", "ok", NOW - MAX_REF_MAX_AGE_S - 100.0));
        assert_eq!(check(&st, &pair(a2, r2)).map(|_| ()), Err(MaxRefRefusal::Stale));
    }

    #[test]
    fn a_citation_of_an_unrelated_message_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        let lunch = put(&st, pane(DESK, "what time is lunch?", NOW - 60.0));
        // The retired single-row forms.
        for c in [Cite { row_id: Some(lunch), ..Default::default() }, Cite { session: Some(DESK.into()), ..Default::default() },
            Cite { reply_row_id: Some(lunch), ..Default::default() }] {
            assert_eq!(check(&st, &c).map(|_| ()), Err(MaxRefRefusal::PairRequired), "{c:?}");
        }
        assert_eq!(cite_pair_at(&st, None, ACTOR, &terms(), NOW).map(|_| ()), Err(MaxRefRefusal::Missing));
        // Max's own row, or another agent's message, is not the actor's ask.
        let r = put(&st, pane(DESK, "yes", NOW - 30.0));
        assert_eq!(check(&st, &pair(lunch, r)).map(|_| ()), Err(MaxRefRefusal::AskNotFromActor));
        let peer = ask(&st, DESK, "test-peer", YOLO_ASK, NOW - 50.0);
        assert_eq!(check(&st, &pair(peer, r)).map(|_| ()), Err(MaxRefRefusal::AskNotFromActor));
        // An unrelated ask of the actor's.
        let chat = ask(&st, "test-desk5", ACTOR, "lunch at noon?", NOW - 50.0);
        let r5 = put(&st, pane("test-desk5", "yes", NOW - 30.0));
        assert_eq!(check(&st, &pair(chat, r5)).map(|_| ()), Err(MaxRefRefusal::AskMissingTarget));
        assert_eq!(check(&st, &pair(999_998, r5)).map(|_| ()), Err(MaxRefRefusal::AskNotFound));
    }

    #[test]
    fn the_reply_must_be_maxs_typed_row_in_the_asks_session() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        let a = ask(&st, DESK, ACTOR, YOLO_ASK, NOW - 600.0);
        let elsewhere = put(&st, pane("test-other", "yes", NOW - 500.0));
        assert_eq!(check(&st, &pair(a, elsewhere)).map(|_| ()), Err(MaxRefRefusal::WrongSession));
        let stamped = put(&st, pane(DESK, "[amux-origin: test-peer] yes", NOW - 400.0));
        assert_eq!(check(&st, &pair(a, stamped)).map(|_| ()), Err(MaxRefRefusal::NotHumanTyped));
        let peer_row = put(&st, R(DESK, "yes", "session", "test-peer", "direct", None, NOW - 300.0));
        assert_eq!(check(&st, &pair(a, peer_row)).map(|_| ()), Err(MaxRefRefusal::NotHumanTyped));
        assert_eq!(check(&st, &pair(a, 999_997)).map(|_| ()), Err(MaxRefRefusal::NoSuchRow));
        // In the actor's own lane only a non-local Remote owner send counts.
        let own = ask(&st, ACTOR, ACTOR, YOLO_ASK, NOW - 300.0);
        let own_pane = put(&st, pane(ACTOR, "yes", NOW - 200.0));
        assert_eq!(check(&st, &pair(own, own_pane)).map(|_| ()), Err(MaxRefRefusal::OwnLane));
        let own2 = ask(&st, ACTOR, ACTOR, YOLO_ASK, NOW - 150.0);
        let own_remote = put(&st, remote(ACTOR, "yes", NOW - 100.0));
        assert!(check(&st, &pair(own2, own_remote)).is_ok(), "own lane, non-local Remote");
    }

    #[test]
    fn names_and_phrases_match_whole_words_only() {
        assert!(names_exactly("for Test-Alpha?", "test-alpha"));
        assert!(!names_exactly("for test-alpha2", "test-alpha") && !names_exactly("for xtest-alpha", "test-alpha"));
        assert!(!names_exactly("for test-alpha.b", "test-alpha"));
        assert!(names_phrase("turn on auto-continue", "auto continue") && names_phrase("set CC_FLAGS", "cc_flags"));
        assert!(names_phrase("the gates", "gate") && names_phrase("two connectors", "connector"));
        assert!(!names_phrase("yolomode", "yolo") && !names_phrase("leader", "lead"));
    }

    #[test]
    fn a_forwarded_remote_send_is_never_approval() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        let fwd = put(&st, R("test-desk", ASK, "user", "", "direct", Some("remote-forwarded"), NOW));
        assert_eq!(q(&st, fwd), Err(MaxRefRefusal::NotRemoteOwnerSend));
    }

}
