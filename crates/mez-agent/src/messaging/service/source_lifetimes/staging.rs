//! Separately charged, bounded encoding space for original-source preservation.
//!
//! This reservation is not a disk/rewrite reserve or an I/O capability. The
//! product must retain it with the actual worker and its encoded result. It
//! cannot borrow the inspection accumulator or the independent absence loan.

use super::*;

const MAX_ENCODED_BYTES: usize = 8 * 1024 * 1024;
/// Covers up to 64 record containers, their ordering and encoder framing.
const STAGING_FRAMING_BYTES: usize = 64 * 1024;

/// One exact inspection's encoding allowance, never source or cleanup authority.
pub struct MessageSourcePreservationReservation {
    inspection: Arc<()>,
    encoded_bytes: usize,
    _lease: MessageStorageSnapshotLease,
}

impl MessageSourcePreservationReservation {
    /// Total encoded storage funded before I/O, not a larger admission limit.
    pub fn encoded_bytes_limit(&self) -> usize {
        self.encoded_bytes
    }

    /// Matches the actual issuing inspection/service, never saved labels.
    pub fn belongs_to(&self, input: &MessageSourceRetirementInspection) -> bool {
        Arc::ptr_eq(&self.inspection, &input.identity)
    }
}

impl MessageService {
    /// Reserves encoding plus framing from ordinary combined metadata space.
    /// Zero, excessive, foreign and over-budget requests fail before any I/O or
    /// encoding. All real owners must share this reservation, not copy its loan.
    /// This does not establish settlement or reserve physical cleanup space.
    pub fn reserve_source_preservation_staging(
        &self,
        input: &MessageSourceRetirementInspection,
        encoded_bytes: usize,
    ) -> Result<MessageSourcePreservationReservation> {
        if !self.source_retirement_inspection_is_original(input)
            || encoded_bytes == 0
            || encoded_bytes > MAX_ENCODED_BYTES
        {
            return Err(unavailable());
        }
        let policy = self.storage_custody.as_ref().ok_or_else(unavailable)?;
        let lease = checkpoint::reserve_checkpoint(
            policy.checkpoint_bytes.clone(),
            encoded_bytes + STAGING_FRAMING_BYTES,
            self.storage_metadata_base_usage()?,
            policy.limits.metadata_bytes,
        )?;
        Ok(MessageSourcePreservationReservation {
            inspection: input.identity.clone(),
            encoded_bytes,
            _lease: lease,
        })
    }
}
