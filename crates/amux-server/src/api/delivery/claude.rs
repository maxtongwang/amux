//! Reviewed Claude UDS profile. All terminal activity belongs to TmuxPaste;
//! this adapter never captures a pane, sends a key or retries a write.
use super::{
    ledger::{self, Attempt},
    system::{self, DiscoveryError, Host, LocalHost, Target},
    wire, DeliveryContext, DeliveryOutcome, DeliveryTransport, Eligibility, ReceiptState,
    HALF_CLOSE_DELAY, IO_TIMEOUT, MAX_CALLBACK_BYTES, MAX_SOCKET_PATH_BYTES,
};
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, OnceLock,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::time::timeout;

pub(super) struct ClaudeUdsV1;

#[async_trait::async_trait]
impl DeliveryTransport for ClaudeUdsV1 {
    fn name(&self) -> &'static str {
        "claude_uds_v1"
    }
    fn capabilities(&self, ctx: &DeliveryContext<'_>) -> Eligibility {
        if !super::enabled() {
            return Eligibility::UnavailableBeforeWrite("feature_disabled");
        }
        if !cfg!(target_os = "macos") {
            return Eligibility::UnavailableBeforeWrite("unreviewed_platform");
        }
        if ctx.payload_kind != super::PayloadKind::PeerProse {
            return Eligibility::UnavailableBeforeWrite("unsupported_payload");
        }
        let Some(peer) = ctx.mode.native_peer.as_ref() else {
            return Eligibility::UnavailableBeforeWrite("not_verified_peer_prose");
        };
        if peer.sender.is_empty()
            || peer.sender == ctx.name
            || peer.message_id.is_empty()
            || super::super::session_verbs::is_slash_command(ctx.text)
            || ctx.text.trim().is_empty()
        {
            return Eligibility::UnavailableBeforeWrite("unsupported_payload");
        }
        if super::super::session_verbs::provider_of(ctx.cfg) != "claude"
            || super::super::session_verbs::backend_of_cfg(ctx.cfg) != "tmux"
        {
            return Eligibility::UnavailableBeforeWrite("unsupported_provider_or_backend");
        }
        let meta = super::super::session_verbs::load_meta(ctx.name);
        if meta
            .get("boot_readiness_pending")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
            || meta
                .get("boot_start_scheduled")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
        {
            return Eligibility::UnavailableBeforeWrite("boot_lifecycle_pending");
        }
        Eligibility::Eligible
    }
    async fn deliver(&self, ctx: &mut DeliveryContext<'_>) -> DeliveryOutcome {
        let source = ctx
            .delivery
            .map(|q| format!("queue:{q}"))
            .unwrap_or_else(|| {
                format!(
                    "message:{}",
                    ctx.mode.native_peer.as_ref().unwrap().message_id
                )
            });
        match ctx
            .state
            .store
            .read()
            .and_then(|c| Ok(ledger::find(&c, ctx.name, &source)?))
        {
            Ok(Some(a)) => {
                return DeliveryOutcome::Native {
                    attempt_id: a.id,
                    state: a.state,
                }
            }
            Ok(None) => {}
            Err(_) => return DeliveryOutcome::Quarantined("intent_storage_unavailable"),
        }
        let Some(pids) = super::super::session_verbs::native_target_pids(ctx.name).await else {
            return DeliveryOutcome::UnavailableBeforeWrite("no_local_process_binding");
        };
        let config = ctx
            .cfg
            .get("CLAUDE_CONFIG_DIR")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from))
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".claude")
            });
        let meta = super::super::session_verbs::load_meta(ctx.name);
        let expected = meta
            .get("cc_conversation_id")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if expected.is_none() {
            return DeliveryOutcome::UnavailableBeforeWrite("no_receiver_session_binding");
        }
        let discovery = tokio::task::spawn_blocking(move || {
            system::discover(&LocalHost, &config, &pids, expected.as_deref())
        })
        .await;
        let target = match discovery {
            Ok(Ok(t)) => t,
            Ok(Err(DiscoveryError::Unavailable(why))) => {
                return DeliveryOutcome::UnavailableBeforeWrite(why)
            }
            Ok(Err(DiscoveryError::Quarantined(why))) => return DeliveryOutcome::Quarantined(why),
            Err(_) => return DeliveryOutcome::Quarantined("discovery_task_failed"),
        };
        let relay = match LocalHost.process(std::process::id()) {
            Ok(p) => p,
            Err(why) => return DeliveryOutcome::Quarantined(why),
        };
        if let Some(reason) =
            super::super::session_verbs::project_send_hold(ctx.state, ctx.name, ctx.delivery)
        {
            return DeliveryOutcome::Terminal(false, reason);
        }
        if ctx.admitted_claim
            != super::super::session_verbs::project_delivery_claim(
                ctx.state,
                ctx.name,
                ctx.delivery,
            )
        {
            return DeliveryOutcome::Terminal(
                false,
                "project_claim_changed; durable owner input remains queued".into(),
            );
        }
        let peer = ctx.mode.native_peer.as_ref().unwrap();
        let stamped;
        let body = if peer.body_stamped {
            ctx.text
        } else {
            stamped = super::super::session_verbs::origin_stamped(&peer.sender, ctx.name, ctx.text)
                .unwrap_or_else(|| ctx.text.to_string());
            &stamped
        };
        let attempt = match Attempt::new(ctx, &target, &relay, body) {
            Ok(a) => a,
            Err(why) => return DeliveryOutcome::Quarantined(why),
        };
        let before_intent =
            || super::super::session_verbs::native_prewrite_hold(ctx).map_or(Ok(()), Err);
        deliver_to(NativeSend {
            store: &ctx.state.store,
            attempt,
            host: Arc::new(LocalHost),
            target,
            body,
            sender_mode: ctx.sender_permission_class,
            before_intent: Some(&before_intent),
        })
        .await
    }
}

fn endpoint_key(t: &Target) -> String {
    system::profile_key(&t.process)
}

pub(super) struct NativeSend<'a> {
    pub store: &'a crate::db::SharedStore,
    pub attempt: Attempt,
    pub host: Arc<dyn Host>,
    pub target: Target,
    pub body: &'a str,
    pub sender_mode: Option<&'a str>,
    pub before_intent: Option<&'a (dyn Fn() -> Result<(), String> + Send + Sync)>,
}

pub(super) async fn deliver_to(request: NativeSend<'_>) -> DeliveryOutcome {
    let NativeSend {
        store,
        attempt: a,
        host,
        target,
        body,
        sender_mode,
        before_intent,
    } = request;
    let key = endpoint_key(&target);
    let quarantined = store.read().and_then(|c| {
        Ok(c.query_row(
            "SELECT EXISTS(SELECT 1 FROM native_delivery_quarantine WHERE endpoint_key=?1)",
            [&key],
            |r| r.get::<_, bool>(0),
        )?)
    });
    match quarantined {
        Ok(false) => {}
        _ => return DeliveryOutcome::Quarantined("endpoint_quarantined_or_unmeasured"),
    }
    let result = prepare(store, host.clone(), &target).await;
    let (relay, token) = match result {
        Ok(v) => v,
        Err(DiscoveryError::Unavailable(why)) => {
            return DeliveryOutcome::UnavailableBeforeWrite(why)
        }
        Err(DiscoveryError::Quarantined(why)) => {
            let _ = ledger::quarantine(store, key, why).await;
            return DeliveryOutcome::Quarantined(why);
        }
    };
    let Some(from) = wire::address(&relay.path) else {
        return DeliveryOutcome::UnavailableBeforeWrite("relay_address");
    };
    if from.len() > super::MAX_RELAY_ADDRESS_BYTES {
        return DeliveryOutcome::UnavailableBeforeWrite("reply_address_cap");
    }
    let bytes = match wire::encode(&wire::Frame {
        from: &from,
        sender: &a.sender,
        mode: sender_mode,
        body,
        msg_id: &a.msg_id,
        command_uuid: &a.command_uuid,
        session_id: &target.session,
        token: token.as_deref(),
    }) {
        Ok(b) => b,
        Err("payload_byte_cap") => {
            return DeliveryOutcome::UnavailableBeforeWrite("payload_byte_cap")
        }
        Err(why) => return DeliveryOutcome::Quarantined(why),
    };
    let mut stream = match timeout(IO_TIMEOUT, UnixStream::connect(&target.socket)).await {
        Ok(Ok(s)) => s,
        Ok(Err(e))
            if matches!(
                e.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) =>
        {
            return DeliveryOutcome::UnavailableBeforeWrite("connect_refused_before_write")
        }
        Err(_) => return DeliveryOutcome::UnavailableBeforeWrite("connect_timeout_before_write"),
        _ => return DeliveryOutcome::Quarantined("connect_anomaly"),
    };
    // Kernel identity is checked on the connected descriptor; filesystem
    // ownership alone cannot attest the process listening at that path.
    let validation = (|| {
        if system::peer(&stream)? != (target.process.pid, host.uid()) {
            return Err("connected_endpoint_mismatch");
        }
        system::revalidate(host.as_ref(), &target)?;
        if !relay.alive.load(Ordering::Acquire) {
            return Err("reply_consumer_unavailable");
        }
        Ok(())
    })();
    if let Err(why) = validation {
        let _ = ledger::quarantine(store, key, why).await;
        return DeliveryOutcome::Quarantined(why);
    }
    if let Some(gate) = before_intent {
        if let Err(reason) = gate() {
            return DeliveryOutcome::Terminal(false, reason);
        }
    }
    match ledger::reserve(store, &a).await {
        Ok(true) => {}
        Ok(false) => {
            let existing = store
                .read()
                .ok()
                .and_then(|c| ledger::find(&c, &a.session, &a.source_key).ok().flatten());
            return match existing {
                Some(a) => DeliveryOutcome::Native {
                    attempt_id: a.id,
                    state: a.state,
                },
                None => DeliveryOutcome::Quarantined("intent_conflict_unmeasured"),
            };
        }
        Err(_) => return DeliveryOutcome::Quarantined("intent_commit_failed"),
    }
    // Repeat immediately before write intent: the process/record may have
    // changed while waiting for the single SQLite writer.
    if let Err(why) = system::revalidate(host.as_ref(), &target) {
        let _ = ledger::record_state(store, &a.id, ReceiptState::Unknown, why).await;
        let _ = ledger::quarantine(store, key, why).await;
        return DeliveryOutcome::Native {
            attempt_id: a.id,
            state: ReceiptState::Unknown,
        };
    }
    if !matches!(ledger::mark_write(store, &a.id).await, Ok(true)) {
        return DeliveryOutcome::Native {
            attempt_id: a.id,
            state: ReceiptState::Unknown,
        };
    }
    tracing::info!(session=%a.session,attempt_id=%a.id,endpoint_pid=a.endpoint_pid,
        transport="claude_uds_v1",compatibility_version=super::REVIEWED_VERSION,measured=true,n_considered=1,verdict="native_delivery_write_intent",
        "durable intent committed before native write");
    let write = timeout(IO_TIMEOUT, async {
        stream.write_all(&bytes).await?;
        tokio::time::sleep(HALF_CLOSE_DELAY).await;
        stream.shutdown().await?;
        let mut byte = [0u8; 1];
        // There is no positive ACK on this stream. Unexpected bytes are an
        // anomaly, and EOF remains unconfirmed, never delivered.
        match stream.read(&mut byte).await? {
            0 => Ok::<_, std::io::Error>(()),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unexpected response",
            )),
        }
    })
    .await;
    if matches!(&write, Ok(Err(e)) if e.kind()==std::io::ErrorKind::InvalidData) {
        let _ = ledger::quarantine(store, key, "unexpected_send_stream_response").await;
    }
    if !matches!(write, Ok(Ok(()))) {
        let _ = ledger::record_state(
            store,
            &a.id,
            ReceiptState::Unknown,
            "write_or_close_uncertain",
        )
        .await;
        tracing::warn!(attempt_id=%a.id,measured=true,n_considered=1,verdict="native_delivery_write_uncertain","no native resend or paste permitted");
    }
    // A callback can win the race while write_all/close is finishing. Read
    // its durable state instead of overwriting held/denied with uncertainty.
    let state = store
        .read()
        .ok()
        .and_then(|c| ledger::find(&c, &a.session, &a.source_key).ok().flatten())
        .map(|a| a.state)
        .unwrap_or(ReceiptState::Unknown);
    DeliveryOutcome::Native {
        attempt_id: a.id,
        state,
    }
}

struct ReplyInbox {
    path: PathBuf,
    alive: Arc<AtomicBool>,
}
type Inboxes = Mutex<HashMap<PathBuf, Arc<ReplyInbox>>>;
fn inboxes() -> &'static Inboxes {
    static INBOXES: OnceLock<Inboxes> = OnceLock::new();
    INBOXES.get_or_init(Default::default)
}

async fn prepare(
    store: &crate::db::SharedStore,
    host: Arc<dyn Host>,
    target: &Target,
) -> Result<(Arc<ReplyInbox>, Option<String>), DiscoveryError> {
    system::revalidate(host.as_ref(), target).map_err(DiscoveryError::Quarantined)?;
    let token = system::auth_token(host.as_ref(), target).map_err(DiscoveryError::Quarantined)?;
    let parent = target
        .socket
        .parent()
        .ok_or(DiscoveryError::Quarantined("inbox_parent"))?;
    system::vet_directory(parent, host.uid()).map_err(DiscoveryError::Quarantined)?;
    // Same vetted directory is explicitly permitted by the reviewed peer
    // protocol; no Claude registry row is published for the amux relay.
    let mut map = inboxes()
        .lock()
        .map_err(|_| DiscoveryError::Quarantined("inbox_registry_poisoned"))?;
    if let Some(inbox) = map.get(parent).filter(|i| i.alive.load(Ordering::Acquire)) {
        return Ok((inbox.clone(), token));
    }
    let nonce = super::uuid();
    let path = parent.join(format!(
        "{}-{}.sock",
        std::process::id(),
        &nonce[nonce.len() - super::RELAY_SUFFIX_HEX_CHARS..]
    ));
    if path.as_os_str().len() > MAX_SOCKET_PATH_BYTES {
        return Err(DiscoveryError::Unavailable("reply_path_cap"));
    }
    let listener = UnixListener::bind(&path)
        .map_err(|_| DiscoveryError::Unavailable("reply_bind_unavailable"))?;
    let cleanup = SocketCleanup(path.clone());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .map_err(|_| DiscoveryError::Quarantined("reply_permissions"))?;
    let inbox = Arc::new(ReplyInbox {
        path,
        alive: Arc::new(AtomicBool::new(true)),
    });
    map.insert(parent.into(), inbox.clone());
    let alive = inbox.alive.clone();
    let store = store.clone();
    tokio::spawn(async move {
        let _guard = ConsumerGuard { alive, cleanup };
        let slots = Arc::new(tokio::sync::Semaphore::new(
            super::CALLBACK_CONNECTION_LIMIT,
        ));
        let mut failures = 0;
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(s) => {
                    failures = 0;
                    s
                }
                Err(_) => {
                    failures += 1;
                    tracing::warn!(
                        measured = true,
                        n_considered = 1,
                        failures,
                        verdict = "native_reply_accept_failed",
                        "native reply consumer retrying"
                    );
                    if failures >= super::ACCEPT_MAX_FAILURES {
                        break;
                    }
                    tokio::time::sleep(super::ACCEPT_RETRY_DELAY).await;
                    continue;
                }
            };
            let Ok(permit) = slots.clone().try_acquire_owned() else {
                tracing::warn!(
                    measured = true,
                    n_considered = 1,
                    verdict = "native_reply_capacity",
                    "callback connection refused; receipts stay unconfirmed"
                );
                continue;
            };
            let (host, store) = (host.clone(), store.clone());
            tokio::spawn(async move {
                let _permit = permit;
                receive_callback(stream, host, store).await;
            });
        }
    });
    Ok((inbox, token))
}
struct SocketCleanup(PathBuf);
impl Drop for SocketCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
struct ConsumerGuard {
    alive: Arc<AtomicBool>,
    cleanup: SocketCleanup,
}
impl Drop for ConsumerGuard {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
        tracing::warn!(path=%self.cleanup.0.display(),measured=true,n_considered=1,verdict="native_reply_consumer_stopped","new native writes require a fresh reply consumer");
    }
}

async fn receive_callback(
    mut stream: UnixStream,
    host: Arc<dyn Host>,
    store: crate::db::SharedStore,
) {
    let result = async {
        let (pid, uid) = system::peer(&stream)?;
        if uid != host.uid() {
            return Err("callback_uid");
        }
        let identity = host.process(pid)?;
        let mut bytes = Vec::new();
        (&mut stream)
            .take(MAX_CALLBACK_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "callback_read")?;
        if bytes.len() > MAX_CALLBACK_BYTES {
            return Err("callback_cap");
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| "callback_utf8")?;
        let mut frames = Vec::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let v = serde_json::from_str::<serde_json::Value>(line).map_err(|_| "callback_json")?;
            wire::callback(&v)?;
            frames.push(v);
        }
        if frames.is_empty() {
            return Err("callback_empty");
        }
        if host.process(pid)? != identity {
            return Err("callback_process_changed");
        }
        store
            .write_async(move |conn| {
                let mut n = 0;
                for frame in frames {
                    n += ledger::callback(conn, &frame, &identity)?;
                }
                Ok(crate::db::WriteOutcome {
                    applied: n > 0,
                    events: vec![],
                })
            })
            .await
            .map_err(|_| "callback_identity_or_correlation")?;
        Ok::<_, &'static str>(())
    };
    let reason = match timeout(IO_TIMEOUT, result).await {
        Ok(Ok(())) => return,
        Ok(Err(r)) => r,
        Err(_) => "callback_timeout",
    };
    if let Ok((pid, uid)) = system::peer(&stream) {
        if uid == host.uid() {
            if let Ok(peer) = host.process(pid) {
                ledger::quarantine_peer(&store, peer, reason).await;
            }
        }
    }
    tracing::warn!(
        reason,
        measured = true,
        n_considered = 1,
        verdict = "native_callback_rejected",
        "uncorrelated or malformed callback did not advance receipts"
    );
}
