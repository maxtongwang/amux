//! Durable native intent and evidence. Neither EOF nor absence of evidence
//! permits retransmission; legacy text-based receipt inference is excluded.
use super::{
    now,
    system::{Host, ProcessIdentity, Target},
    wire, ReceiptState, OBSERVATION_TIMEOUT_S,
};
use crate::db::{SharedStore, WriteOutcome};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub(super) struct Attempt {
    pub id: String,
    pub session: String,
    pub source_key: String,
    pub message_id: Option<String>,
    pub queue_id: Option<String>,
    pub card_id: Option<String>,
    pub sender: String,
    pub msg_id: String,
    pub command_uuid: String,
    pub endpoint: String,
    pub endpoint_pid: u32,
    pub endpoint_uid: u32,
    pub endpoint_start: String,
    pub endpoint_session: String,
    pub executable_sha256: String,
    pub registry_sha256: String,
    pub relay_pid: u32,
    pub relay_start: String,
    pub body_sha256: String,
    pub state: ReceiptState,
    pub created_at: f64,
    pub write_attempted_at: Option<f64>,
    pub unknown_since: Option<f64>,
    pub observation_count: u32,
}

impl Attempt {
    pub fn new(
        ctx: &super::DeliveryContext<'_>,
        target: &Target,
        relay: &ProcessIdentity,
        body: &str,
    ) -> Result<Self, &'static str> {
        let peer = ctx
            .mode
            .native_peer
            .as_ref()
            .expect("native eligibility checked");
        let conn = ctx
            .state
            .store
            .read()
            .map_err(|_| "history_lineage_unmeasured")?;
        let card_id=conn.query_row("SELECT card_id FROM cmd_history WHERE session=?1 AND (msg_id=?2 OR queue_id=?3) ORDER BY id LIMIT 1",params![ctx.name,peer.message_id,ctx.delivery],|r|r.get::<_,Option<String>>(0)).map_err(|_|"history_lineage_missing")?;
        Ok(Self {
            id: super::uuid(),
            session: ctx.name.into(),
            source_key: ctx
                .delivery
                .map(|q| format!("queue:{q}"))
                .unwrap_or_else(|| format!("message:{}", peer.message_id)),
            message_id: (!peer.message_id.is_empty()).then(|| peer.message_id.clone()),
            queue_id: ctx.delivery.map(str::to_owned),
            card_id,
            sender: peer.sender.clone(),
            msg_id: super::uuid(),
            command_uuid: super::uuid(),
            endpoint: target.socket.to_string_lossy().into(),
            endpoint_pid: target.process.pid,
            endpoint_uid: target.process.uid,
            endpoint_start: target.process.instance.clone(),
            endpoint_session: target.session.clone(),
            executable_sha256: target.process.executable_sha256.clone(),
            registry_sha256: target.registry_sha256.clone(),
            relay_pid: relay.pid,
            relay_start: relay.registry_start.clone(),
            body_sha256: wire::sha256(wire::neutralize(body).as_bytes()),
            state: ReceiptState::Reserved,
            created_at: now(),
            write_attempted_at: None,
            unknown_since: None,
            observation_count: 0,
        })
    }
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Attempt> {
    let state: String = r.get("state")?;
    Ok(Attempt {
        id: r.get("attempt_id")?,
        session: r.get("session")?,
        source_key: r.get("source_key")?,
        message_id: r.get("message_id")?,
        queue_id: r.get("queue_id")?,
        card_id: r.get("card_id")?,
        sender: r.get("sender")?,
        msg_id: r.get("msg_id")?,
        command_uuid: r.get("command_uuid")?,
        endpoint: r.get("endpoint")?,
        endpoint_pid: r.get("endpoint_pid")?,
        endpoint_uid: r.get("endpoint_uid")?,
        endpoint_start: r.get("endpoint_start")?,
        endpoint_session: r.get("endpoint_session")?,
        executable_sha256: r.get("executable_sha256")?,
        registry_sha256: r.get("registry_sha256")?,
        relay_pid: r.get("relay_pid")?,
        relay_start: r.get("relay_start")?,
        body_sha256: r.get("body_sha256")?,
        state: ReceiptState::from_str(&state).ok_or(rusqlite::Error::InvalidQuery)?,
        created_at: r.get("created_at")?,
        write_attempted_at: r.get("write_attempted_at")?,
        unknown_since: r.get("unknown_since")?,
        observation_count: r.get("observation_count")?,
    })
}

pub(super) fn find(
    conn: &Connection,
    session: &str,
    source: &str,
) -> rusqlite::Result<Option<Attempt>> {
    conn.query_row(
        "SELECT * FROM native_delivery_attempts WHERE session=?1 AND source_key=?2",
        params![session, source],
        row,
    )
    .optional()
}

pub(super) fn insert(conn: &Connection, a: &Attempt) -> rusqlite::Result<bool> {
    Ok(conn.execute("INSERT OR IGNORE INTO native_delivery_attempts \
        (attempt_id,session,source_key,message_id,queue_id,card_id,sender,msg_id,command_uuid,endpoint,endpoint_pid,endpoint_uid,endpoint_start,endpoint_session,executable_sha256,registry_sha256,relay_pid,relay_start,body_sha256,state,reason,created_at,updated_at) \
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,'reserved','intent_persisted',?20,?20)",
        params![a.id,a.session,a.source_key,a.message_id,a.queue_id,a.card_id,a.sender,a.msg_id,a.command_uuid,
            a.endpoint,a.endpoint_pid,a.endpoint_uid,a.endpoint_start,a.endpoint_session,a.executable_sha256,
            a.registry_sha256,a.relay_pid,a.relay_start,a.body_sha256,a.created_at])? > 0)
}

pub(super) async fn reserve(store: &SharedStore, a: &Attempt) -> anyhow::Result<bool> {
    let a = a.clone();
    Ok(store
        .write_async(move |conn| {
            let applied = insert(conn, &a)?;
            Ok(WriteOutcome {
                applied,
                events: if applied {
                    message_events(conn, ReceiptLookup::Attempts(&[a.id]))?
                } else {
                    vec![]
                },
            })
        })
        .await?
        .applied)
}

/// This commit precedes the FIRST write call, including auth bytes. A crash
/// on either side of the call is ambiguous and can never unlock a resend.
pub(super) async fn mark_write(store: &SharedStore, id: &str) -> anyhow::Result<bool> {
    let id = id.to_string();
    Ok(store
        .write_async(move |conn| {
            let applied = mark_write_conn(conn, &id, now())?;
            Ok(WriteOutcome {
                applied,
                events: if applied {
                    message_events(conn, ReceiptLookup::Attempts(&[id]))?
                } else {
                    vec![]
                },
            })
        })
        .await?
        .applied)
}

pub(super) fn mark_write_conn(conn: &Connection, id: &str, at: f64) -> rusqlite::Result<bool> {
    Ok(conn.execute("UPDATE native_delivery_attempts SET state='written_unconfirmed',reason='write_intent',write_attempted_at=?2,updated_at=?2 WHERE attempt_id=?1 AND state='reserved' AND write_attempted_at IS NULL", params![id,at])?>0)
}

pub(super) fn transition(
    conn: &Connection,
    id: &str,
    next: ReceiptState,
    reason: &str,
    at: f64,
) -> rusqlite::Result<bool> {
    let old: String = conn.query_row(
        "SELECT state FROM native_delivery_attempts WHERE attempt_id=?1",
        [id],
        |r| r.get(0),
    )?;
    let old = ReceiptState::from_str(&old).ok_or(rusqlite::Error::InvalidQuery)?;
    let receiver_lost = old == ReceiptState::Held
        && next == ReceiptState::Unknown
        && reason == "receiver_instance_changed";
    if (!old.may_advance(next) && !receiver_lost) || old == next {
        return Ok(false);
    }
    let n = conn.execute("UPDATE native_delivery_attempts SET state=?2,reason=?3,updated_at=?4,unknown_since=CASE WHEN ?2='unknown' THEN COALESCE(unknown_since,?4) ELSE unknown_since END,evidence_at=CASE WHEN ?2 IN ('held','admitted','consumed','denied','refused','expired','dropped') THEN ?4 ELSE evidence_at END WHERE attempt_id=?1 AND state=?5",
        params![id,next.as_str(),reason,at,old.as_str()])?;
    if n > 0 {
        conn.execute("INSERT OR IGNORE INTO session_events(ts,session,type,data,idem,source) SELECT ?2,session,'message.native_delivery',?3,?4,'native-delivery' FROM native_delivery_attempts WHERE attempt_id=?1",
            params![id,at,json!({"attempt_id":id,"state":next,"reason":reason,"measured":true,"n_considered":1}).to_string(),format!("native-delivery:{id}:{}",next.as_str())])?;
        tracing::info!(
            attempt_id = id,
            state = next.as_str(),
            reason,
            measured = true,
            n_considered = 1,
            verdict = "native_delivery_state",
            "native delivery evidence advanced"
        );
    }
    Ok(n > 0)
}

pub(super) async fn record_state(
    store: &SharedStore,
    id: &str,
    state: ReceiptState,
    reason: &'static str,
) -> anyhow::Result<()> {
    let id = id.to_string();
    store
        .write_async(move |conn| {
            let applied = transition(conn, &id, state, reason, now())?;
            Ok(WriteOutcome {
                applied,
                events: if applied {
                    message_events(conn, ReceiptLookup::Attempts(&[id]))?
                } else {
                    vec![]
                },
            })
        })
        .await?;
    Ok(())
}

pub(super) async fn quarantine(
    store: &SharedStore,
    key: String,
    reason: &'static str,
) -> anyhow::Result<()> {
    store.write_async(move |conn| {
        let n=conn.execute("INSERT OR IGNORE INTO native_delivery_quarantine(endpoint_key,reason,created_at) VALUES(?1,?2,?3)",params![key,reason,now()])?;
        Ok(WriteOutcome {applied:n>0,events:vec![]})
    }).await?;
    Ok(())
}

pub(super) async fn quarantine_peer(
    store: &SharedStore,
    peer: ProcessIdentity,
    reason: &'static str,
) {
    let result=store.write_async(move |conn| {
        let known:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM native_delivery_attempts WHERE endpoint_pid=?1 AND endpoint_uid=?2 AND endpoint_start=?3 AND executable_sha256=?4)",
            params![peer.pid,peer.uid,peer.instance,peer.executable_sha256],|r|r.get(0))?;
        let n=if known {conn.execute("INSERT OR IGNORE INTO native_delivery_quarantine(endpoint_key,reason,created_at) VALUES(?1,?2,?3)",
            params![super::system::profile_key(&peer),reason,now()])?}else{0};
        Ok(WriteOutcome{applied:n>0,events:vec![]})
    }).await;
    if let Err(e) = result {
        tracing::warn!(error=%e,measured=false,n_considered=1,verdict="native_quarantine_unmeasured","callback identity anomaly could not persist quarantine");
    }
}

pub(super) fn callback(
    conn: &Connection,
    v: &Value,
    peer: &ProcessIdentity,
) -> rusqlite::Result<usize> {
    let (next, ids) = wire::callback(v).map_err(|_| rusqlite::Error::InvalidQuery)?;
    let reason = wire::callback_reason(v).map_err(|_| rusqlite::Error::InvalidQuery)?;
    let mut matched = Vec::new();
    for id in &ids {
        let a = conn
            .query_row(
                "SELECT * FROM native_delivery_attempts WHERE msg_id=?1",
                [id],
                row,
            )
            .optional()?;
        // Reject an entire coalesced frame if any ID is uncorrelated or came
        // from another instance. Do not partially accept malformed evidence.
        let a = a.ok_or(rusqlite::Error::InvalidQuery)?;
        if a.write_attempted_at.is_none()
            || a.endpoint_pid != peer.pid
            || a.endpoint_uid != peer.uid
            || a.endpoint_start != peer.instance
            || a.executable_sha256 != peer.executable_sha256
            || v["from"].as_str() != wire::address(std::path::Path::new(&a.endpoint)).as_deref()
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        matched.push(a);
    }
    let mut n = 0;
    for a in matched {
        n += usize::from(transition(conn, &a.id, next, reason, now())?);
    }
    Ok(n)
}

pub(crate) fn queue_owned(conn: &Connection, id: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM native_delivery_attempts WHERE queue_id=?1)",
        [id],
        |r| r.get(0),
    )
}

pub(crate) fn queue_blocked(conn: &Connection, id: &str) -> rusqlite::Result<bool> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM native_delivery_pending_queue p JOIN steering_queue q ON q.id=?1 WHERE p.session=q.session AND p.queued_at<=q.queued_at)", [id], |r|r.get(0))
}

pub(crate) enum QueueDisposition {
    Terminal,
    Native(ReceiptState),
    Unmeasured,
}

pub(crate) fn queue_disposition(store: &SharedStore, id: &str) -> QueueDisposition {
    let result = store.read().and_then(|c| {
        Ok(c.query_row(
            "SELECT state FROM native_delivery_attempts WHERE queue_id=?1",
            [id],
            |r| r.get::<_, String>(0),
        )
        .optional()?)
    });
    match result {
        Ok(Some(s)) => ReceiptState::from_str(&s)
            .map(QueueDisposition::Native)
            .unwrap_or(QueueDisposition::Unmeasured),
        Ok(None) => QueueDisposition::Terminal,
        Err(_) => QueueDisposition::Unmeasured,
    }
}

pub(crate) async fn mark_candidate(store: &SharedStore, queue: &str, peer: &super::VerifiedPeer) {
    let (queue, peer) = (queue.to_string(), peer.clone());
    if let Err(e)=store.write_async(move |c|{
        let n=c.execute("UPDATE steering_queue SET native_sender=?2,native_message_id=?3,native_body_stamped=?4 WHERE id=?1 AND delivering_since IS NULL",
            params![queue,peer.sender,peer.message_id,peer.body_stamped])?;
        Ok(WriteOutcome{applied:n>0,events:vec![]})
    }).await {
        tracing::warn!(error=%e,measured=false,n_considered=1,verdict="native_candidate_unmeasured","queued message keeps its existing paste semantics");
    }
}

pub(crate) fn candidate(store: &SharedStore, queue: &str) -> Option<super::VerifiedPeer> {
    store.read().ok()?.query_row("SELECT native_sender,native_message_id,native_body_stamped FROM steering_queue WHERE id=?1 AND native_sender IS NOT NULL AND native_message_id IS NOT NULL",[queue],|r|{
        Ok(super::VerifiedPeer{sender:r.get(0)?,message_id:r.get(1)?,body_stamped:r.get(2)?})
    }).ok()
}

/// Restart does not mean cancellation. Even a reserved intent may have been
/// followed by a write whose next commit was lost; retain queue ownership.
pub(crate) fn recover(conn: &Connection) -> rusqlite::Result<usize> {
    let n=conn.execute("UPDATE native_delivery_attempts SET state='unknown',reason='server_restart',updated_at=?1,unknown_since=COALESCE(unknown_since,?1) WHERE state IN ('reserved','written_unconfirmed','held')",[now()])?;
    if n > 0 {
        tracing::warn!(
            measured = true,
            n_considered = n,
            verdict = "native_delivery_recovered_unknown",
            "pending native attempts retained without resend"
        );
    }
    Ok(n)
}

fn receipt_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let a = row(r)?;
    Ok(
        json!({"transport":"claude_uds_v1","attempt_id":a.id,"msg_id":a.msg_id,"uuid":a.command_uuid,
        "compatibility_version":super::REVIEWED_VERSION,"executable_sha256":a.executable_sha256,
        "endpoint_pid":a.endpoint_pid,"endpoint_start":a.endpoint_start,"endpoint_session":a.endpoint_session,
        "state":a.state,"reason":r.get::<_,String>("reason")?,"created_at":a.created_at,
        "updated_at":r.get::<_,f64>("updated_at")?,"evidence_at":r.get::<_,Option<f64>>("evidence_at")?,
        "unknown_deadline":a.unknown_since.map(|at|at+super::UNKNOWN_HOLD_S),
        "may_have_been_delivered":a.state==ReceiptState::Uncertain,
        "status_note":if a.state==ReceiptState::Uncertain {Some("may have been delivered")}else{None},
        "write_attempted_at":a.write_attempted_at,"admitted":a.state.admitted(),"consumed":a.state==ReceiptState::Consumed}),
    )
}

pub(super) enum ReceiptLookup<'a> {
    Attempts(&'a [String]),
    NativeMessages(&'a [String]),
}

/// Publish the same committed Message invalidation used by normal history.
/// Batch coalesced callbacks; receipt updates must not depend on manual reloads.
pub(super) fn message_events(
    conn: &Connection,
    lookup: ReceiptLookup<'_>,
) -> rusqlite::Result<Vec<crate::db::PendingEvent>> {
    let (column, ids) = match lookup {
        ReceiptLookup::Attempts(ids) => ("attempt_id", ids),
        ReceiptLookup::NativeMessages(ids) => ("msg_id", ids),
    };
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let placeholders = vec!["?"; ids.len()].join(",");
    let sql = format!("SELECT DISTINCT h.id FROM cmd_history h JOIN native_delivery_attempts n ON n.session=h.session AND (n.message_id=h.msg_id OR n.queue_id=h.queue_id) WHERE n.{column} IN ({placeholders})");
    let mut stmt = conn.prepare(&sql)?;
    let events = stmt
        .query_map(rusqlite::params_from_iter(ids), |r| {
            Ok(crate::db::PendingEvent {
                entity_type: amux_core::revision::EntityType::Message,
                entity_id: format!("MSG-{}", r.get::<_, i64>(0)?),
                mutation: amux_core::revision::MutationKind::Updated,
                payload: None,
            })
        })?
        .collect();
    events
}

pub(crate) fn receipt(
    conn: &Connection,
    session: &str,
    message: &str,
    queue: Option<&str>,
) -> rusqlite::Result<Option<Value>> {
    conn.query_row("SELECT * FROM native_delivery_attempts WHERE session=?1 AND ((message_id=?2 AND ?2<>'') OR queue_id=?3) ORDER BY created_at DESC LIMIT 1",params![session,message,queue],receipt_row).optional()
}

/// Batch the current history page's exact IDs; no per-message queries or scan
/// of every attempt belonging to the session is needed by the dashboard.
pub(crate) fn attach_receipts(conn: &Connection, values: &mut [Value]) -> rusqlite::Result<()> {
    for chunk in values.chunks_mut(super::RECEIPT_BATCH_ROWS) {
        let mut clauses = Vec::new();
        let mut bindings = Vec::<String>::new();
        for value in chunk.iter() {
            if let (Some(session), Some(message)) = (
                value["session"].as_str(),
                value["msg_id"].as_str().filter(|s| !s.is_empty()),
            ) {
                clauses.push("(session=? AND message_id=?)");
                bindings.push(session.into());
                bindings.push(message.into());
            }
            if let Some(queue) = value["queue_id"].as_str().filter(|s| !s.is_empty()) {
                clauses.push("queue_id=?");
                bindings.push(queue.into());
            }
        }
        if clauses.is_empty() {
            continue;
        }
        let sql = format!(
            "SELECT * FROM native_delivery_attempts WHERE {} ORDER BY created_at",
            clauses.join(" OR ")
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut by_message = std::collections::HashMap::new();
        let mut by_queue = std::collections::HashMap::new();
        let rows = stmt.query_map(rusqlite::params_from_iter(bindings), |r| {
            Ok((
                r.get::<_, String>("session")?,
                r.get::<_, Option<String>>("message_id")?,
                r.get::<_, Option<String>>("queue_id")?,
                receipt_row(r)?,
            ))
        })?;
        for result in rows {
            let (session, message, queue, receipt) = result?;
            if let Some(message) = message {
                by_message.insert((session, message), receipt.clone());
            }
            if let Some(queue) = queue {
                by_queue.insert(queue, receipt);
            }
        }
        for value in chunk {
            let receipt = by_message
                .get(&(
                    value["session"].as_str().unwrap_or("").to_string(),
                    value["msg_id"].as_str().unwrap_or("").to_string(),
                ))
                .or_else(|| value["queue_id"].as_str().and_then(|q| by_queue.get(q)));
            if let Some(receipt) = receipt {
                let state = receipt["state"]
                    .as_str()
                    .and_then(ReceiptState::from_str)
                    .ok_or(rusqlite::Error::InvalidQuery)?;
                value["delivered"] = json!(if state.admitted() {
                    "delivered"
                } else if state.terminal() {
                    "not delivered"
                } else {
                    "unknown"
                });
                value["delivered_source"] =
                    json!("native delivery ledger — queue admission is separate from consumption");
                value["delivery_receipt"] = receipt.clone();
            }
        }
    }
    Ok(())
}

pub(crate) struct HistorySendOutcome {
    pub row_id: i64,
    pub ok: bool,
    pub message: String,
    pub queue_id: Option<String>,
}

/// The HTTP response can finish after a callback/reconciliation transaction.
/// Its older send observation must not clear already committed evidence.
pub(crate) fn record_history_outcome(
    conn: &Connection,
    send: &HistorySendOutcome,
) -> rusqlite::Result<()> {
    let (submitted, _) = super::super::session_verbs::submission_verdict(send.ok, &send.message);
    let at = (now() * super::MS_PER_SECOND) as i64;
    conn.execute("UPDATE cmd_history SET delivery=?2,submit_verdict=CASE WHEN delivered_at IS NULL THEN ?3 ELSE submit_verdict END,queue_id=COALESCE(queue_id,?4),queued_at=COALESCE(queued_at,?5),delivered_at=COALESCE(delivered_at,?6) WHERE id=?1",
        params![send.row_id,if send.queue_id.is_some(){"queued"}else{"direct"},
            super::super::session_verbs::submit_verdict_of(&send.message),send.queue_id,
            send.queue_id.as_ref().map(|_|at),if submitted==Some(true){Some(at)}else{None}])?;
    Ok(())
}

/// Positive-only, exact-ID receiver evidence. A matching body, display name,
/// or timestamp alone cannot settle another send, including identical text.
pub(super) fn artifact_matches(a: &Attempt, record: &Value) -> bool {
    let origin = &record["origin"];
    if a.write_attempted_at.is_none()
        || record["type"] != "user"
        || record["sessionId"]
            .as_str()
            .or_else(|| record["session_id"].as_str())
            != Some(a.endpoint_session.as_str())
        || record["uuid"] != a.command_uuid
        || origin["kind"] != "peer"
        || origin["verifiedPeerPid"].as_u64() != Some(a.relay_pid as u64)
        || origin
            .get("msg_id")
            .is_some_and(|id| id.as_str() != Some(a.msg_id.as_str()))
    {
        return false;
    }
    let exact = |body: &str| wire::sha256(body.as_bytes()) == a.body_sha256;
    origin["body"].as_str().is_some_and(exact)
        || origin["members"].as_array().is_some_and(|members| {
            !members.is_empty()
                && members.iter().all(|m| m["body"].is_string())
                && members
                    .iter()
                    .any(|m| m["body"].as_str().is_some_and(exact))
        })
}

pub(super) trait Clock: Send + Sync {
    fn now(&self) -> f64;
}
struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> f64 {
        now()
    }
}

pub(super) trait ArtifactReader: Send + Sync {
    fn read(&self, attempt: &Attempt) -> Result<Vec<Value>, &'static str>;
}
struct ReceiverArtifacts;
impl ArtifactReader for ReceiverArtifacts {
    fn read(&self, attempt: &Attempt) -> Result<Vec<Value>, &'static str> {
        let path = super::super::session_verbs::session_jsonl_path(&attempt.session)
            .ok_or("no_receiver_artifact")?;
        Ok(super::super::session_verbs::iter_jsonl_tail(
            &path,
            super::ARTIFACT_TAIL_BYTES,
        ))
    }
}

/// The existing timer continues reconciliation with the dark flag off.
/// Bounded batches and a durable observation cursor prevent an old uncertain
/// send from starving newer receipts or hashing every binary on every tick.
pub(crate) async fn reconcile(state: &super::super::AppState) {
    reconcile_with(
        state,
        std::sync::Arc::new(super::system::LocalHost),
        &SystemClock,
        std::sync::Arc::new(ReceiverArtifacts),
    )
    .await;
}

pub(super) async fn reconcile_with(
    state: &super::super::AppState,
    host: std::sync::Arc<dyn Host>,
    clock: &dyn Clock,
    artifacts: std::sync::Arc<dyn ArtifactReader>,
) {
    let store = state.store.clone();
    let at = clock.now();
    let result=store.read_async(move |conn| {
        let mut stmt=conn.prepare("SELECT * FROM native_delivery_attempts WHERE next_observation_at<=?1 AND ((state IN ('reserved','written_unconfirmed','held','unknown') OR (state='admitted' AND (finalized_at IS NULL OR created_at+?3>?1))) OR (state IN ('consumed','denied','refused','expired','dropped','uncertain') AND finalized_at IS NULL)) ORDER BY next_observation_at,created_at LIMIT ?2")?;
        let rows=stmt.query_map(params![at,super::RECONCILE_BATCH_SIZE,super::RECONCILE_MAX_AGE_S],row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }).await;
    let attempts = match result {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(error=%e,measured=false,n_considered=0,verdict="native_reconcile_unmeasured","native receipts retained");
            return;
        }
    };
    for a in attempts {
        let (probe, reader, host) = (a.clone(), artifacts.clone(), host.clone());
        let evidence = tokio::task::spawn_blocking(move || {
            if probe.state.terminal()
                || (probe.state == ReceiptState::Unknown
                    && probe
                        .unknown_since
                        .is_some_and(|since| at >= since + super::UNKNOWN_HOLD_S))
                || (probe.state == ReceiptState::Admitted
                    && at >= probe.created_at + super::RECONCILE_MAX_AGE_S)
            {
                return Ok(false);
            }
            let receiver = host.process(probe.endpoint_pid)?;
            if receiver.uid != probe.endpoint_uid
                || receiver.instance != probe.endpoint_start
                || receiver.executable_sha256 != probe.executable_sha256
            {
                return Err("receiver_instance_changed");
            }
            let records = reader.read(&probe)?;
            let positive = records.iter().any(|r| artifact_matches(&probe, r));
            if !positive {
                return Ok::<_, &'static str>(false);
            }
            let live_relay = host
                .process(probe.relay_pid)
                .ok()
                .is_some_and(|p| p.registry_start == probe.relay_start);
            let captured_relay = records.iter().any(|r| {
                artifact_matches(&probe, r)
                    && r["origin"]["verifiedPeerProcStart"].as_str()
                        == Some(probe.relay_start.as_str())
            });
            if !live_relay && !captured_relay {
                return Err("relay_start_evidence_mismatch");
            }
            Ok(true)
        })
        .await;
        let positive = matches!(evidence, Ok(Ok(true)));
        let receiver_lost = matches!(evidence, Ok(Err("receiver_instance_changed")));
        if let Ok(Err(reason)) = evidence {
            tracing::info!(attempt_id=%a.id,reason,measured=false,n_considered=1,verdict="native_artifact_unmeasured","absence cannot confirm or cancel native delivery");
        }
        let id = a.id.clone();
        let result=store.write_async(move |conn| {
            let mut changed=false;
            if a.state == ReceiptState::Unknown && a.unknown_since.is_some_and(|since| at >= since + super::UNKNOWN_HOLD_S) {
                changed|=transition(conn,&id,ReceiptState::Uncertain,"unknown_hold_deadline",at)?;
            }
            else if positive {changed|=transition(conn,&id,ReceiptState::Consumed,"exact_receiver_artifact",at)?;}
            else if receiver_lost {changed|=transition(conn,&id,ReceiptState::Unknown,"receiver_instance_changed",at)?;}
            else if at-a.created_at>OBSERVATION_TIMEOUT_S && matches!(a.state,ReceiptState::Reserved|ReceiptState::WrittenUnconfirmed) {
                changed|=transition(conn,&id,ReceiptState::Unknown,"observation_deadline",at)?;
            }
            changed|=finalize(conn,&id)?;
            conn.execute("UPDATE native_delivery_attempts SET next_observation_at=?2,observation_count=observation_count+1 WHERE attempt_id=?1",params![id,next_observation(&a,at)])?;
            Ok(WriteOutcome{applied:changed,events:if changed {message_events(conn,ReceiptLookup::Attempts(&[id]))?}else{vec![]}})
        }).await;
        if let Err(e) = result {
            tracing::warn!(error=%e,attempt_id=%a.id,measured=false,n_considered=1,verdict="native_reconcile_failed","native attempt retained");
        }
    }
}

fn next_observation(a: &Attempt, at: f64) -> f64 {
    let delay = (super::RECONCILE_INTERVAL_S * 2f64.powi(a.observation_count.min(4) as i32))
        .min(super::RECONCILE_MAX_INTERVAL_S);
    let next = at + delay;
    if a.state == ReceiptState::Unknown {
        return a
            .unknown_since
            .map_or(next, |since| next.min(since + super::UNKNOWN_HOLD_S));
    }
    next
}

/// Abandon ordering only; this never cancels a Claude write or permits replay.
pub(super) fn abandon(conn: &Connection, session: &str, id: &str) -> rusqlite::Result<bool> {
    let attempt = conn
        .query_row(
            "SELECT * FROM native_delivery_attempts WHERE session=?1 AND attempt_id=?2",
            params![session, id],
            row,
        )
        .optional()?;
    let Some(a) = attempt else { return Ok(false) };
    if !matches!(
        a.state,
        ReceiptState::Reserved
            | ReceiptState::WrittenUnconfirmed
            | ReceiptState::Unknown
            | ReceiptState::Uncertain
    ) {
        return Ok(false);
    }
    let changed = transition(conn, id, ReceiptState::Uncertain, "owner_abandoned", now())?;
    Ok(finalize(conn, id)? || changed || a.state == ReceiptState::Uncertain)
}

pub(crate) async fn abandon_action(
    state: &super::super::AppState,
    session: &str,
    headers: &axum::http::HeaderMap,
    body: &Value,
) -> axum::response::Response {
    use axum::{
        http::{StatusCode, Uri},
        response::IntoResponse,
        Json,
    };
    let is_owner = super::super::session_verbs::worker_origin(headers).is_empty()
        && !super::super::org::is_verified_local_member(headers)
        && (state.auth_token.is_none()
            || super::super::auth::has_owner_token(state, headers, &Uri::from_static("/")));
    if !is_owner {
        tracing::warn!(
            session,
            verdict = "native_abandon_owner_only",
            measured = true,
            n_considered = 1,
            "only the owner may abandon native ordering"
        );
        return (StatusCode::FORBIDDEN, Json(json!({"ok":false,"code":"native_abandon_owner_only","error":"Abandon requires direct owner input and the owner credential."}))).into_response();
    }
    let Some(id) = body["attempt_id"]
        .as_str()
        .filter(|id| super::valid_uuid(id))
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok":false,"code":"invalid_attempt_id"})),
        )
            .into_response();
    };
    let (session, id) = (session.to_string(), id.to_string());
    let (target, attempt) = (session.clone(), id.clone());
    let result = state
        .store
        .write_async(move |conn| {
            let applied = abandon(conn, &target, &attempt)?;
            Ok(WriteOutcome {
                applied,
                events: if applied {
                    message_events(conn, ReceiptLookup::Attempts(&[attempt]))?
                } else {
                    vec![]
                },
            })
        })
        .await;
    match result {
        Ok(reply) if reply.applied => {
            let receipt = state.store.read().and_then(|c| Ok(c.query_row(
                "SELECT * FROM native_delivery_attempts WHERE session=?1 AND attempt_id=?2",params![session,id],receipt_row)?));
            match receipt {
                Ok(receipt) => Json(json!({"ok":true,"delivery_receipt":receipt})).into_response(),
                Err(_) => (StatusCode::INTERNAL_SERVER_ERROR,Json(json!({"ok":false,"code":"native_receipt_unmeasured"}))).into_response(),
            }
        }
        Ok(_) => (StatusCode::CONFLICT,Json(json!({"ok":false,"code":"native_attempt_not_abandonable","error":"No ambiguous attempt on this worker; held approvals and confirmed receipts cannot be abandoned."}))).into_response(),
        Err(e) => {
            tracing::warn!(error=%e,verdict="native_abandon_failed",measured=false,n_considered=1,"native attempt retained");
            (StatusCode::INTERNAL_SERVER_ERROR,Json(json!({"ok":false,"code":"native_abandon_failed"}))).into_response()
        }
    }
}

pub(super) fn finalize(conn: &Connection, id: &str) -> rusqlite::Result<bool> {
    let a = conn.query_row(
        "SELECT * FROM native_delivery_attempts WHERE attempt_id=?1",
        [id],
        row,
    )?;
    if a.state.terminal() && !a.state.admitted() {
        let at = now();
        let mut n=conn.execute("UPDATE cmd_history SET submit_verdict=?3 WHERE session=?1 AND ((msg_id=?2 AND ?2<>'') OR queue_id=?4) AND delivered_at IS NULL AND COALESCE(submit_verdict,'')<>?3",
            params![a.session,a.message_id,a.state.as_str(),a.queue_id])?;
        if let Some(queue) = &a.queue_id {
            n += conn.execute("DELETE FROM steering_queue WHERE id=?1", [queue])?;
            n+=conn.execute("UPDATE schedule_runs SET status='refused',submission=?2,note=COALESCE(note,'') || ' · native disposition: ' || ?2 WHERE queue_id=?1 AND status='queued'",params![queue,a.state.as_str()])?;
        }
        n+=conn.execute("UPDATE native_delivery_attempts SET finalized_at=?2 WHERE attempt_id=?1 AND finalized_at IS NULL",params![id,at])?;
        return Ok(n > 0);
    }
    if !a.state.admitted() {
        return Ok(false);
    }
    let at = now();
    let mut n=conn.execute("UPDATE cmd_history SET submit_verdict='confirmed',delivered_at=COALESCE(delivered_at,?3),reconciled_at=?3 WHERE session=?1 AND ((msg_id=?2 AND ?2<>'') OR queue_id=?4) AND delivered_at IS NULL",
        params![a.session,a.message_id,(at*super::MS_PER_SECOND) as i64,a.queue_id])?;
    if let Some(queue) = &a.queue_id {
        let text: Option<String> = conn
            .query_row(
                "SELECT text FROM steering_queue WHERE id=?1",
                [queue],
                |r| r.get(0),
            )
            .optional()?;
        let text = text.map(|t| super::super::session_verbs::redact_secrets(&t));
        n+=conn.execute("INSERT OR IGNORE INTO steering_history(id,session,text,queued_at,delivered_at,outcome,guard,sender) SELECT id,session,?3,queued_at,?2,'sent (native queue admission confirmed)',guard,sender FROM steering_queue WHERE id=?1",params![queue,at,text])?;
        n += conn.execute("DELETE FROM steering_queue WHERE id=?1", [queue])?;
        n+=conn.execute("UPDATE schedule_runs SET status='delivered',submission=?2,note=COALESCE(note,'') || ' · native queue admission confirmed' WHERE queue_id=?1 AND status='queued'",
            params![queue,a.state.as_str()])?;
    }
    n += conn.execute("UPDATE native_delivery_attempts SET finalized_at=?2 WHERE attempt_id=?1 AND finalized_at IS NULL", params![id,at])?;
    Ok(n > 0)
}
