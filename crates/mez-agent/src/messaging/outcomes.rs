//! Payload-free terminal delivery facts and their private snapshot projection.
//!
//! These records describe an unsatisfied delivery, never successful task work,
//! approval, death inferred from presence, or permission to replay effects.

use serde::{Deserialize, Serialize};

use super::types::{MessageRecipientBindingSnapshot, MessageSequence};

/// Ten-minute native handoff window after first observing a terminal/expiry
/// fact. Repeated maintenance and clock rollback never renew this opportunity.
/// Local source owners may protect already retained outcomes until handoff.
pub(super) const TERMINAL_HANDOFF_MS: u64 = 600_000;

/// Authoritatively observed reason an accepted recipient cannot commit mail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryTerminalReason {
    /// The original occurrence's strict acceptance-relative TTL elapsed.
    Expired,
    /// Its exact registered recipient interval ended or was replaced.
    RecipientRetired,
}

/// Exact, payload-free handoff receipt for the original authenticated sender.
/// Consumption acknowledges this evidence only, not recipient delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageDeliveryOutcome {
    /// Immutable accepted occurrence sequence, not a new message.
    pub sequence: MessageSequence,
    /// Original accepted envelope ID for originating-action correlation.
    pub message_id: String,
    /// Frozen recipient interval that did not acknowledge this occurrence.
    pub recipient: MessageRecipientBindingSnapshot,
    /// Typed local fact, never peer-supplied lifecycle authority.
    pub reason: DeliveryTerminalReason,
    /// First service observation of this terminal state; retries never renew it.
    pub observed_at_ms: u64,
}

/// Fixed-size state held in a pre-reserved slot, independently of replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageDeliveryTerminalSnapshot {
    /// First terminal reason; later events cannot replace it.
    pub reason: DeliveryTerminalReason,
    /// First observation, at or after acceptance.
    pub observed_at_ms: u64,
    /// The exact sender consumed it, or its finite handoff horizon elapsed.
    /// This is explicitly not an application delivery acknowledgement.
    pub released: bool,
}
