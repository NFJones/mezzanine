//! Construction-time resource policy for accepted in-memory delivery custody.
//!
//! Trusted callers may narrow independent budgets for bounded deployments and
//! deterministic fixtures. Policies cannot exceed the reviewed hard ceilings,
//! are not model/config/wire options, and are never restored from message data.
//! Snapshot callers must explicitly supply any nondefault destination policy.

use super::error::{MessageError, Result};

/// Independent custody budgets, separate from optional replay retention.
///
/// All values must be positive and no greater than their defaults. Constructors
/// and snapshot restoration validate every field before adopting service state.
/// Fanout charges unique payload once but keeps all frozen recipient slots
/// charged until the last exact recipient commits the shared occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingDeliveryLimits {
    /// Maximum pending immutable occurrences (hard ceiling 1000).
    pub messages: usize,
    /// Maximum unique pending payload bytes (hard ceiling 1 MiB).
    pub payload_bytes: usize,
    /// Maximum frozen recipient slots in pending occurrences (ceiling 4096).
    pub recipient_obligations: usize,
    /// Maximum pending slots per exact recipient interval (ceiling 256).
    pub per_recipient: usize,
    /// Maximum logical auxiliary bytes over shared replay and custody (16 MiB).
    /// Counts immutable envelope metadata and index/container overhead, excluding
    /// separately budgeted payload and frozen recipient records.
    pub auxiliary_bytes: usize,
}

impl Default for PendingDeliveryLimits {
    /// Returns the production ceilings without altering optional replay policy.
    fn default() -> Self {
        Self {
            messages: 1000,
            payload_bytes: 1024 * 1024,
            recipient_obligations: 4096,
            per_recipient: 256,
            auxiliary_bytes: 16 * 1024 * 1024,
        }
    }
}

impl PendingDeliveryLimits {
    /// Rejects zero or enlarged budgets before construction or restoration.
    pub(super) fn validate(self) -> Result<Self> {
        let ceiling = Self::default();
        if [
            (self.messages, ceiling.messages),
            (self.payload_bytes, ceiling.payload_bytes),
            (self.recipient_obligations, ceiling.recipient_obligations),
            (self.per_recipient, ceiling.per_recipient),
            (self.auxiliary_bytes, ceiling.auxiliary_bytes),
        ]
        .iter()
        .any(|(value, maximum)| *value == 0 || value > maximum)
        {
            return Err(MessageError::invalid_args(
                "MMP pending limits must be positive and cannot exceed production ceilings",
            ));
        }
        Ok(self)
    }
}
