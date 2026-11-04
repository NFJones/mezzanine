//! Qualified ended-owner observations after original canonical input lookup.
//!
//! Data binds the frozen recipient to guarded lifetime provenance and an actual
//! namespace end. Only the trusted lookup port can create the opaque witness;
//! its presence is not a terminal disposition. Existing input anchors keep their
//! shared slot protection, so a later terminal reservation must still exclude
//! any earlier application owner before consuming this observation.

use super::*;

/// Bounded original journal observations, never a live or source capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageNativeRetirementRecord {
    /// Actual original private allocation observed by the trusted port.
    pub store_id: [u8; 16],
    /// Guarded original namespace/epoch provenance with availability false.
    pub native: MessageNativeCheckpoint,
    /// First known guarded native publication, strictly before the end.
    pub native_publication: u64,
    /// Explicit original namespace end, never inferred from latest presence.
    pub end: MessageRegistrationEndSnapshot,
    /// Actual immutable end publication generation.
    pub end_publication: u64,
    /// Exact immutable lifetime row bytes, not caller-selected replacement data.
    pub lifetime_sha256: [u8; 32],
    /// Exact immutable namespace-end row bytes.
    pub end_sha256: [u8; 32],
}

impl MessageNativeRetirementRecord {
    /// Validates descriptive evidence against this actual source's frozen slot.
    /// Only the trusted original store can qualify the publication observations.
    fn validate_for(&self, anchor: &MessageNativeInputAnchor) -> Result<()> {
        native_binding::validate_native_checkpoint(std::slice::from_ref(&self.native))?;
        self.end.validate()?;
        if self.store_id == [0; 16]
            || self.native.admission_available
            || self.native.delivery_available
            || self.native.agent_id != anchor.recipient().as_str()
            || self.native.owner != *anchor.native_owner()
            || self.native.registration_namespace != self.end.registration_namespace
            || self.end.agent_id != self.native.agent_id
            || self.end.registered_after_sequence != anchor.registered_after_sequence()
            || anchor.sequence() <= self.end.registered_after_sequence
            || anchor.sequence() > self.end.ended_after_sequence
            || self.native_publication == 0
            || self.native_publication >= self.end_publication
            || self.lifetime_sha256 == [0; 32]
            || self.end_sha256 == [0; 32]
        {
            return Err(unavailable());
        }
        Ok(())
    }
}

/// Actual original source plus trusted canonical absence and end qualification.
/// Decoding its descriptive record cannot reconstruct this allocation.
#[derive(Clone, Debug)]
pub struct MessageNativeRetirementWitness {
    pub(in crate::messaging::service) anchor: MessageNativeInputAnchor,
    record: Arc<MessageNativeRetirementRecord>,
}

impl MessageNativeRetirementWitness {
    /// Borrows its exact original source; this supplies no fresh native offer.
    pub fn anchor(&self) -> &MessageNativeInputAnchor {
        &self.anchor
    }
    /// Borrows actual original end observations, never a live token constructor.
    pub fn record(&self) -> &MessageNativeRetirementRecord {
        &self.record
    }
}

/// One actual original canonical lookup, with positive input taking precedence.
#[derive(Debug)]
pub enum MessageNativeInputObservation {
    /// Actual canonical input exists and retains its original ACK authority.
    Committed(MessageNativeInputCommit),
    /// Canonical absence; missing end provenance remains explicitly unknown.
    Absent,
    /// Canonical absence plus original end evidence, still no terminal effect.
    RecipientEnded(MessageNativeRetirementWitness),
}

impl MessageNativeInputObservation {
    /// Extracts only a canonical positive for callers interested in input commit.
    /// Other observations leave their original accepted slot unsettled.
    pub fn into_commit(self) -> Option<MessageNativeInputCommit> {
        match self {
            Self::Committed(commit) => Some(commit),
            Self::Absent | Self::RecipientEnded(_) => None,
        }
    }
}

impl MessageNativeInputAnchor {
    /// Lookup owns the only protection block reference; an offered source or
    /// another retained application anchor must settle before retirement.
    pub(in crate::messaging::service) fn retirement_is_exclusive(&self) -> bool {
        self.0.offered_at_ms.is_none() && Arc::strong_count(&self.0._protection) == 1
    }
    /// Compares the actual captured allocation, independently of equal metadata.
    pub fn same_source(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Checks canonical input first and qualifies an original end only on actual
    /// absence. A positive always wins; no end data can replace its ACK proof.
    pub fn observe(
        self,
        store: &mut impl MessageNativeInputStore,
    ) -> Result<MessageNativeInputObservation> {
        if let Some(commit) = self.clone().verify(store)? {
            return Ok(MessageNativeInputObservation::Committed(commit));
        }
        let Some(record) = store.retired_owner(&self)? else {
            return Ok(MessageNativeInputObservation::Absent);
        };
        record.validate_for(&self)?;
        // Clones of one source share one observation allocation and lease.
        // Requalification must still inspect canonical input first, and may
        // never substitute a different immutable lifetime/end row image.
        let candidate = Arc::new(record);
        let record = self.0.retirement.get_or_init(|| candidate.clone());
        if record.as_ref() != candidate.as_ref() {
            return Err(unavailable());
        }
        let record = record.clone();
        Ok(MessageNativeInputObservation::RecipientEnded(
            MessageNativeRetirementWitness {
                anchor: self,
                record,
            },
        ))
    }
}
