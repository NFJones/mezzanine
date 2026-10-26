//! Pre-admission durable history uncertainty; no namespace or native authority.
//!
//! Optional warm publication can fail, but cannot leave cold recovery believing
//! that an omitted retired instance never existed. Each native warm proposal
//! first stores its own gap intent; only full actor history at that exact intent
//! can clear it. Newer proposals fence delayed older repairs transactionally.

use super::*;

/// Original source slot/fence and one immutable pre-admission metadata outcome.
pub(super) struct WarmHistoryPlan {
    slot: Arc<source_retirement::NativeSourceSlot>,
    previous: Arc<crate::storage::token_usage::ExternalSourceObserverFence>,
    predecessor: ([u8; 32], u64),
    pub(super) nonce: [u8; 32],
    outcome: OnceLock<Result<()>>,
}

impl std::fmt::Debug for WarmHistoryPlan {
    /// No native identity, metadata selectors or operation bytes in diagnostics.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WarmHistoryPlan").finish_non_exhaustive()
    }
}

impl WarmHistoryPlan {
    /// Capture only from an existing actual actor-owned source, never a payload.
    pub(super) fn new(
        slot: Arc<source_retirement::NativeSourceSlot>,
        previous: Arc<crate::storage::token_usage::ExternalSourceObserverFence>,
        predecessor: ([u8; 32], u64),
    ) -> Arc<Self> {
        let mut nonce = [0; 32];
        rand::rng().fill_bytes(&mut nonce);
        Arc::new(Self {
            slot,
            previous,
            predecessor,
            nonce,
            outcome: OnceLock::new(),
        })
    }

    /// Actor-side cached fact only; a supplied Ok cannot create this outcome.
    pub(super) fn was_recorded(&self) -> bool {
        self.outcome.get().is_some_and(Result::is_ok)
    }

    /// Historical success belongs only to the captured actor predecessor. A
    /// future numeric request guess cannot acquire a later predecessor, nor can
    /// coincident generation reuse substitute a different binding digest.
    pub(super) fn recorded_for(
        &self,
        slot: &Arc<source_retirement::NativeSourceSlot>,
        digest: [u8; 32],
        generation: u64,
    ) -> bool {
        Arc::ptr_eq(&self.slot, slot)
            && self.predecessor == (digest, generation)
            && self.was_recorded()
    }

    /// Off actor, after this work's real current native ancestry was captured.
    pub(super) fn execute(&self, work: &ExternalEnrollmentWork) -> Result<()> {
        self.outcome
            .get_or_init(|| {
                self.slot.mark_observer_history(
                    &work.origin,
                    work.ancestry
                        .get()
                        .ok_or_else(|| MezError::conflict("source history ancestry unavailable"))?,
                    work.deadline,
                    &self.previous,
                    self.nonce,
                )
            })
            .clone()
    }
}
