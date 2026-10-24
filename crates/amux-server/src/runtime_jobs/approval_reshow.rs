// Confirmation reminders — refs maxtongwang/ai-config#160.
// Durable receipts separate once-only firing/push from idempotent native notice enqueue.
use crate::api::{session_verbs, AppState};
use crate::db::WriteOutcome;
use rusqlite::params;

const JOB: &str = super::registry::ids::APPROVAL_RESHOW;
pub(crate) const CONFIRM_RESHOW_MIN: i64 = 30;
const SECONDS_PER_MINUTE: f64 = 60.0;
const TICK_SECS: u64 = 60;
const BATCH_LIMIT: i64 = 100;
// Shared by candidate discovery and reservations: ?2 is the current Unix time.
const OPEN_REQUEST: &str =
    "status='pending' AND approval_closed_at IS NULL AND resolution_row IS NULL AND expires_at>?2";

#[derive(Clone)]
pub(crate) struct Reminder {
    pub id: String,
    pub code: String,
    pub session: String,
    pub incarnation: String,
}

impl Reminder {
    pub(crate) fn notice(&self) -> String {
        format!("<amux-native-delivery kind=\"approval-reshow\" id=\"{}\">\n[amux approval] reshow {}\n</amux-native-delivery>", self.id, self.code)
    }
    fn delivery_id(&self) -> String {
        format!("approval-reshow-{}", self.id)
    }
}

#[async_trait::async_trait]
pub(crate) trait Delivery: Send + Sync {
    async fn notice(&self, state: &AppState, reminder: &Reminder) -> Result<(), String>;
    async fn push(&self, state: &AppState, reminder: &Reminder) -> Result<(), String>;
}

pub(crate) struct NativeAndWebPush;
#[async_trait::async_trait]
impl Delivery for NativeAndWebPush {
    async fn notice(&self, state: &AppState, r: &Reminder) -> Result<(), String> {
        // The common owner dispatcher requires native delivery. On this branch
        // #67 is absent: it holds this durable queue item, never pastes or keys.
        session_verbs::steer_enqueue_idempotent(
            state,
            &r.session,
            &r.notice(),
            JOB,
            "",
            &r.delivery_id(),
        )
        .await
        .map(|_| ())
        .map_err(str::to_string)
    }
    async fn push(&self, state: &AppState, r: &Reminder) -> Result<(), String> {
        let results = crate::push::send_all(
            state,
            "amux approval",
            &format!(
                "Approval {} still needs confirmation. Open mrpopo to review.",
                r.code
            ),
            &r.session,
            &r.delivery_id(),
            "/",
        )
        .await;
        crate::api::alerts::push_delivery_verdict(&results)
    }
}

/// The same open predicate protects selection and each serialized side-effect
/// reservation. A first doubt starts the clock; repeated doubt never resets it.
async fn candidates(state: &AppState, now_s: f64) -> anyhow::Result<Vec<Reminder>> {
    state.store.read_async(move |c| {
        let mut st = c.prepare(&format!("SELECT code,short_code,reply_session,reply_incarnation FROM _amux_delegation_grants WHERE {OPEN_REQUEST} AND confirm_required=1 AND confirm_needed_at<=?1 AND short_code IS NOT NULL AND reply_session IS NOT NULL AND reply_incarnation IS NOT NULL AND (confirm_reshow_at IS NULL OR confirm_reshow_notice_at IS NULL OR confirm_reshow_push_at IS NULL) ORDER BY confirm_needed_at LIMIT ?3"))?;
        let rows = st.query_map(params![now_s-CONFIRM_RESHOW_MIN as f64*SECONDS_PER_MINUTE, now_s, BATCH_LIMIT], |r| Ok(Reminder { id:r.get(0)?, code:r.get(1)?, session:r.get(2)?, incarnation:r.get(3)? }))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }).await
}

async fn stamp(
    state: &AppState,
    id: &str,
    column: &'static str,
    now_s: f64,
) -> anyhow::Result<bool> {
    let id = id.to_string();
    Ok(state.store.write_async(move |c| {
        // Column names are internal constants, never external input.
        let n=c.execute(&format!("UPDATE _amux_delegation_grants SET {column}=?2 WHERE code=?1 AND {column} IS NULL AND {OPEN_REQUEST}"),params![id,now_s])?;
        Ok(WriteOutcome { applied:n==1, events:vec![] })
    }).await?.applied)
}

pub(crate) async fn tick_with(
    state: &AppState,
    now_s: f64,
    delivery: &dyn Delivery,
) -> anyhow::Result<()> {
    if super::fleet_isolation_reason(JOB).is_some() {
        return Ok(());
    }
    let reminders = candidates(state, now_s).await?;
    for r in reminders {
        if session_verbs::session_is_isolated(&r.session) {
            continue;
        }
        if !crate::api::delegation_requests::is_owner_session(&r.session)
            || crate::api::delegation_requests::incarnation(&r.session).as_deref()
                != Some(r.incarnation.as_str())
        {
            tracing::warn!(
                request_id = r.id,
                verdict = "approval_reshow_identity_held",
                measured = true,
                n_considered = 1,
                "reminder retains its original owner incarnation"
            );
            continue;
        }
        if stamp(state, &r.id, "confirm_reshow_at", now_s).await? {
            tracing::info!(
                request_id = r.id,
                short_code = r.code,
                verdict = "approval_reshow_fired",
                measured = true,
                n_considered = 1,
                "confirmation reminder fired once"
            );
        }
        // Enqueue under a stable ID before stamping: restart after a lost ACK
        // adopts the same queue/history receipt rather than duplicating it.
        let (open,notice_pending):(bool,bool)=state.store.read_async({ let id=r.id.clone(); move |c| Ok(c.query_row(&format!("SELECT {OPEN_REQUEST},confirm_reshow_notice_at IS NULL FROM _amux_delegation_grants WHERE code=?1"),params![id,now_s],|row|Ok((row.get(0)?,row.get(1)?)))?) }).await?;
        if !open {
            continue;
        }
        if notice_pending {
            match delivery.notice(state, &r).await {
                Ok(()) => {
                    stamp(state, &r.id, "confirm_reshow_notice_at", now_s).await?;
                }
                Err(error) => {
                    tracing::warn!(request_id=r.id, %error, verdict="approval_reshow_notice_held", measured=true, n_considered=1, "native reminder enqueue retained for recovery")
                }
            }
        }
        // Reserve BEFORE calling Web Push: an ambiguous network result or
        // crash must never page twice. A NULL result means unknown, not sent.
        if stamp(state, &r.id, "confirm_reshow_push_at", now_s).await? {
            let result = delivery.push(state, &r).await;
            let outcome = match &result {
                Ok(()) => "sent".to_string(),
                Err(e) => format!("failed: {e}"),
            };
            tracing::info!(request_id=r.id, %outcome, verdict="approval_reshow_push", measured=true, n_considered=1, "once-only Web Push attempt completed");
            let id = r.id.clone();
            state.store.write_async(move |c| {
                c.execute("UPDATE _amux_delegation_grants SET confirm_reshow_push_result=?2 WHERE code=?1",params![id,outcome])?;
                Ok(WriteOutcome { applied:true, events:vec![] })
            }).await?;
        }
    }
    Ok(())
}

pub fn spawn(state: AppState) -> super::PeriodicTask {
    super::spawn_periodic(JOB, TICK_SECS, move || {
        let state = state.clone();
        async move {
            if let Err(error) = tick_with(&state, crate::config::now_f64(), &NativeAndWebPush).await
            {
                tracing::warn!(%error,verdict="approval_reshow_failed",measured=false,n_considered=0,"confirmation reminder tick failed; durable work retained");
            }
        }
    })
}
