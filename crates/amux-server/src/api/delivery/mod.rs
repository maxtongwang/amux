//! Message delivery transports, independent of worker execution types.
//! Only proven pre-write unavailability permits trying the next adapter.

mod claude;
pub(crate) mod ledger;
mod system;
#[cfg(test)]
mod tests;
mod wire;

use super::session_verbs::{EnvFile, SendMode};
use super::AppState;
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub(super) const FEATURE_ENV: &str = "AMUX_NATIVE_CLAUDE_DELIVERY";
pub(super) const IO_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const HALF_CLOSE_DELAY: Duration = Duration::from_millis(150);
pub(super) const OBSERVATION_TIMEOUT_S: f64 = 600.0;
pub(super) const MAX_WIRE_BYTES: usize = 1_048_576;
pub(super) const MAX_REGISTRY_BYTES: usize = 65_536;
pub(super) const MAX_AUTH_FILE_BYTES: usize = 4096;
pub(super) const AUTH_TOKEN_HEX_CHARS: usize = 32;
pub(super) const MAX_SENDER_NAME_CHARS: usize = 64;
pub(super) const RELAY_SUFFIX_HEX_CHARS: usize = 8;
#[cfg(target_os = "macos")]
pub(super) const EXECUTABLE_HASH_BUFFER_BYTES: usize = 65_536;
#[cfg(target_os = "macos")]
pub(super) const MAX_PROCESS_START_BYTES: u64 = 256;
pub(super) const MS_PER_SECOND: f64 = 1000.0;
pub(super) const MAX_CALLBACK_BYTES: usize = 65_536;
pub(super) const MAX_SOCKET_PATH_BYTES: usize = 103;
pub(super) const MAX_RELAY_ADDRESS_BYTES: usize = 204;
pub(super) const MAX_CALLBACK_IDS: usize = 100;
pub(super) const CALLBACK_CONNECTION_LIMIT: usize = 16;
pub(super) const ACCEPT_MAX_FAILURES: usize = 3;
pub(super) const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(100);
#[cfg(target_os = "macos")]
pub(super) const PROCESS_TIMEOUT: Duration = Duration::from_secs(1);
pub(super) const RECONCILE_BATCH_SIZE: usize = 16;
pub(super) const RECEIPT_BATCH_ROWS: usize = 128;
pub(super) const RECONCILE_INTERVAL_S: f64 = 30.0;
pub(super) const ARTIFACT_TAIL_BYTES: u64 = 4 * 1024 * 1024;
pub(super) const REVIEWED_VERSION: &str = "2.1.303";
pub(super) const REVIEWED_SHA256: &str =
    "d2da3b003dc08b88b7230469014fe3e1dc6fe94bc8f648e89b34175d30269370";
pub(super) const WIRE_VERSION: u64 = 1;
pub(super) const REQUIRED_FEATURES: &[&str] = &["notify_idle", "artifact_yield"];

pub(crate) fn enabled() -> bool {
    std::env::var(FEATURE_ENV).is_ok_and(|v| v == "1")
}

/// Created only after the server's identity and group/lifecycle gates. Never
/// construct this by parsing a stamp, native display name or request body.
#[derive(Clone, Debug)]
pub(crate) struct VerifiedPeer {
    pub sender: String,
    pub message_id: String,
    pub body_stamped: bool,
}

pub(crate) struct DeliveryContext<'a> {
    pub state: &'a AppState,
    pub name: &'a str,
    pub text: &'a str,
    pub cfg: &'a EnvFile,
    pub mode: &'a SendMode,
    pub admitted_claim: Option<(String, i64, String)>,
    pub payload_kind: PayloadKind,
    /// Unknown unless independently observed on the actual sender. A relay
    /// cannot assert parity by copying the receiving process's mode.
    pub sender_permission_class: Option<&'a str>,
    pub delivery: Option<&'a str>,
    pub queue_id: &'a mut Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PayloadKind {
    PeerProse,
    TerminalInput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReceiptState {
    Reserved,
    WrittenUnconfirmed,
    Held,
    Admitted,
    Consumed,
    Denied,
    Refused,
    Expired,
    Dropped,
    Unknown,
}

impl ReceiptState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::WrittenUnconfirmed => "written_unconfirmed",
            Self::Held => "held",
            Self::Admitted => "admitted",
            Self::Consumed => "consumed",
            Self::Denied => "denied",
            Self::Refused => "refused",
            Self::Expired => "expired",
            Self::Dropped => "dropped",
            Self::Unknown => "unknown",
        }
    }
    pub fn from_str(s: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(s.into())).ok()
    }
    pub fn admitted(self) -> bool {
        matches!(self, Self::Admitted | Self::Consumed)
    }
    pub fn terminal(self) -> bool {
        matches!(
            self,
            Self::Consumed | Self::Denied | Self::Refused | Self::Expired | Self::Dropped
        )
    }
    pub fn may_advance(self, next: Self) -> bool {
        if self == next {
            return true;
        }
        if self.terminal() {
            return false;
        }
        match next {
            Self::Reserved => false,
            Self::WrittenUnconfirmed => self == Self::Reserved,
            Self::Held => matches!(
                self,
                Self::Reserved | Self::WrittenUnconfirmed | Self::Unknown
            ),
            // The reviewed wire emits delivered only for a held release.
            // Its preceding held callback may have been lost or reordered.
            Self::Admitted => matches!(
                self,
                Self::Reserved | Self::WrittenUnconfirmed | Self::Held | Self::Unknown
            ),
            Self::Consumed => true,
            Self::Unknown => matches!(self, Self::Reserved | Self::WrittenUnconfirmed),
            _ => true,
        }
    }
    // The legacy public send signature remains (bool,String). Only this
    // boundary serializes native outcomes; drains read the typed ledger.
    pub fn result(self, attempt: &str) -> (bool, String) {
        (
            true,
            format!("native delivery: {}; attempt={attempt}", self.as_str()),
        )
    }
    pub fn from_result(s: &str) -> Option<Self> {
        Self::from_str(s.strip_prefix("native delivery: ")?.split(';').next()?)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Eligibility {
    Eligible,
    UnavailableBeforeWrite(&'static str),
}

pub(crate) enum DeliveryOutcome {
    UnavailableBeforeWrite(&'static str),
    Native {
        attempt_id: String,
        state: ReceiptState,
    },
    Quarantined(&'static str),
    Terminal(bool, String),
}

#[async_trait::async_trait]
pub(crate) trait DeliveryTransport: Send + Sync {
    fn name(&self) -> &'static str;
    fn capabilities(&self, ctx: &DeliveryContext<'_>) -> Eligibility;
    async fn deliver(&self, ctx: &mut DeliveryContext<'_>) -> DeliveryOutcome;
}

pub(super) struct TmuxPaste;
#[async_trait::async_trait]
impl DeliveryTransport for TmuxPaste {
    fn name(&self) -> &'static str {
        "tmux_paste"
    }
    fn capabilities(&self, _: &DeliveryContext<'_>) -> Eligibility {
        Eligibility::Eligible
    }
    async fn deliver(&self, ctx: &mut DeliveryContext<'_>) -> DeliveryOutcome {
        let (ok, message) = super::session_verbs::send_text_paste_bound(ctx).await;
        DeliveryOutcome::Terminal(ok, message)
    }
}

static NATIVE: claude::ClaudeUdsV1 = claude::ClaudeUdsV1;
static PASTE: TmuxPaste = TmuxPaste;
static TRANSPORTS: &[&dyn DeliveryTransport] = &[&NATIVE, &PASTE];

pub(crate) async fn dispatch(ctx: &mut DeliveryContext<'_>) -> (bool, String) {
    resolve(ctx, TRANSPORTS).await
}

async fn resolve(
    ctx: &mut DeliveryContext<'_>,
    adapters: &[&dyn DeliveryTransport],
) -> (bool, String) {
    for adapter in adapters {
        let outcome = match adapter.capabilities(ctx) {
            Eligibility::Eligible => adapter.deliver(ctx).await,
            Eligibility::UnavailableBeforeWrite(why) => {
                DeliveryOutcome::UnavailableBeforeWrite(why)
            }
        };
        match outcome {
            DeliveryOutcome::UnavailableBeforeWrite(reason) => {
                if enabled() {
                    tracing::info!(
                        session = ctx.name,
                        transport = adapter.name(),
                        reason,
                        measured = true,
                        n_considered = 1,
                        verdict = "delivery_unavailable_before_write",
                        "selecting next transport before any write"
                    );
                }
            }
            DeliveryOutcome::Native { attempt_id, state } => return state.result(&attempt_id),
            DeliveryOutcome::Quarantined(reason) => {
                tracing::warn!(
                    session = ctx.name,
                    reason,
                    measured = true,
                    n_considered = 1,
                    verdict = "native_delivery_quarantined",
                    "delivery stopped; identity/policy failures never permit paste fallback"
                );
                return (false, format!("native delivery quarantined: {reason}"));
            }
            DeliveryOutcome::Terminal(ok, message) => return (ok, message),
        }
    }
    (false, "no eligible delivery transport".into())
}

pub(super) fn uuid() -> String {
    let mut b = ulid::Ulid::new().to_bytes();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex::encode(b);
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

pub(super) fn valid_uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

pub(super) fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
