//! Worker-only inert renewal checkpoints for the existing activation row.
//!
//! Original attempt/ancestry/preparation/expiry remain immutable. This finite
//! checkpoint can extend only a live Accepted record under an exact revision;
//! expired, Prepared and Retired records never become renewable. Exact committed
//! retry returns its existing receipt, not another write or fresh time allowance.
//! All clocks and native/actor authority must be independently supplied and
//! revalidated by a future runtime owner. No native renewal, enrollment, namespace
//! allocation, general permission or cold recovery is enabled by this module.

use super::*;

/// Fixed-size immutable proposal for one bounded same-boot renewal checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExternalSourceRenewal {
    /// Native clock capture, not UTC, PID evidence or serialized Instant.
    pub(super) renewed_at_boot_ms: u64,
    /// Half-open finite expiry relative to this checkpoint's own capture.
    pub(super) expires_at_boot_ms: u64,
}

impl ExternalSourceRenewal {
    /// Validates representation and a positive window no larger than60s. Does
    /// not read a clock, attest native continuity or establish permission.
    pub(crate) fn new(renewed_at_boot_ms: u64, expires_at_boot_ms: u64) -> Result<Self> {
        if renewed_at_boot_ms == 0
            || expires_at_boot_ms > i64::MAX as u64
            || expires_at_boot_ms
                .checked_sub(renewed_at_boot_ms)
                .is_none_or(|window| window == 0 || window > MAX_ACTIVATION_WINDOW_MS)
        {
            return Err(MezError::invalid_args(
                "external source renewal metadata unavailable",
            ));
        }
        Ok(Self {
            renewed_at_boot_ms,
            expires_at_boot_ms,
        })
    }

    /// Inert half-open clock check. Rollback before the latest captured start
    /// refuses rather than falling back to the earlier preparation window.
    pub(super) fn contains_boot_time(&self, now_boot_ms: u64) -> bool {
        self.renewed_at_boot_ms <= now_boot_ms && now_boot_ms < self.expires_at_boot_ms
    }
}

impl TokenUsageStore {
    /// Extends only the same Accepted activation while its current window is
    /// still live at the supplied independently qualified same-boot clock.
    /// Caller owns native/actor authorization, finite worker custody and final
    /// publication checks; a stored receipt itself grants none of those rights.
    /// Exact retry is idempotent, differing/stale attempts conflict, and expiry
    /// or retirement is absorbing. Never allocates an accounting namespace.
    pub(crate) fn renew_external_source_activation(
        &self,
        key: &ExternalSourceKey,
        activation: &ExternalSourceActivation,
        expected_revision: u64,
        renewal: &ExternalSourceRenewal,
        now_boot_ms: u64,
    ) -> Result<ExternalSourceActivationReceipt> {
        if expected_revision < 2 || !renewal.contains_boot_time(now_boot_ms) {
            return Err(MezError::invalid_args(
                "external source renewal revision or clock unavailable",
            ));
        }
        let revision = expected_revision
            .checked_add(1)
            .filter(|next| *next <= i64::MAX as u64)
            .ok_or_else(|| {
                MezError::invalid_state("external source activation revision exhausted")
            })?;
        let mut connection = self.open()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let owner = mapped_owner(&tx, key)?;
        let previous = load_receipt(&tx, &owner)?
            .ok_or_else(|| MezError::forbidden("external source activation was not prepared"))?;
        if previous.activation != *activation || !previous.is_accepted_at(now_boot_ms) {
            return Err(MezError::conflict(
                "external source renewal changed, expired or retired",
            ));
        }
        if previous.revision == revision && previous.renewal == Some(*renewal) {
            return Ok(previous);
        }
        let (old_start, old_end) = previous.renewal.as_ref().map_or(
            (
                activation.prepared_at_boot_ms,
                activation.expires_at_boot_ms,
            ),
            |old| (old.renewed_at_boot_ms, old.expires_at_boot_ms),
        );
        if previous.revision != expected_revision
            || renewal.renewed_at_boot_ms < old_start
            || renewal.expires_at_boot_ms <= old_end
        {
            return Err(MezError::conflict(
                "external source renewal is stale or nonmonotonic",
            ));
        }
        if tx.execute(
            "UPDATE external_usage_source_activations SET renewed_at_boot_ms=?1,renewed_until_boot_ms=?2,revision=?3
             WHERE owner=?4 AND revision=?5 AND state=1",
            params![sqlite_i64(renewal.renewed_at_boot_ms, "renewal clock")?,
                sqlite_i64(renewal.expires_at_boot_ms, "renewal expiry")?,
                sqlite_i64(revision, "activation revision")?, owner,
                sqlite_i64(expected_revision, "activation revision")?],
        )? != 1 {
            return Err(invalid_stored_activation());
        }
        tx.commit()?;
        Ok(ExternalSourceActivationReceipt {
            activation: activation.clone(),
            revision,
            state: ExternalSourceActivationState::Accepted,
            renewal: Some(*renewal),
        })
    }
}
