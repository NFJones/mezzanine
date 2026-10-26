//! Current-observer metadata custody over the original native source slot.
//!
//! Each scope contains only a bounded predecessor and its own committed receipt,
//! never a parent chain or a private credential. The actor installs a pending
//! scope only with admitted current-binding work. The blocking worker seals its
//! exact SQL result while holding the original slot's I/O lock, before fallible
//! postchecks. Retirement resolves that SAME scope under that lock: it cannot
//! miss a committed change or learn an unrelated newer database fence. Neither
//! scope construction nor a metadata receipt grants native or cold authority.

use super::*;
use crate::storage::token_usage::{ExternalSourceObserverChange, ExternalSourceObserverFence};

/// One bounded observer operation, independent of physical work/admission IDs.
#[derive(Debug)]
pub(super) struct ObserverScope {
    previous: Arc<ExternalSourceObserverFence>,
    committed: OnceLock<Arc<ExternalSourceObserverFence>>,
}

impl ObserverScope {
    /// Captures an immutable known predecessor without retaining older scopes.
    pub(super) fn new(previous: Arc<ExternalSourceObserverFence>) -> Arc<Self> {
        Arc::new(Self {
            previous,
            committed: OnceLock::new(),
        })
    }

    /// Pure bounded inspection. For retirement call only under source I/O
    /// ownership after its logical fence prevents any later pending mutation.
    pub(super) fn current(&self) -> Arc<ExternalSourceObserverFence> {
        self.committed.get().unwrap_or(&self.previous).clone()
    }

    /// Only the actual SQL worker records this, before fallible postchecks.
    pub(super) fn has_committed(&self) -> bool {
        self.committed.get().is_some()
    }

    /// Records only this operation's actual SQL receipt before lock release.
    pub(super) fn committed(&self, fence: ExternalSourceObserverFence) -> Result<()> {
        self.committed
            .set(Arc::new(fence))
            .map_err(|_| MezError::conflict("source observer receipt already recorded"))
    }
}

/// Secret-free immutable CAS proposal plus its exact operation-local receipt.
#[derive(Debug)]
pub(super) struct ObserverMutation {
    pub(super) change: ExternalSourceObserverChange,
    pub(super) scope: Arc<ObserverScope>,
    pub(super) history_nonce: Option<[u8; 32]>,
}

impl RuntimeSessionService {
    /// Selects only the current exact anchor already checked by the slot owner.
    pub(super) fn current_external_source_observer_scope(
        &self,
        digest: [u8; 32],
    ) -> Option<Arc<ObserverScope>> {
        self.current_external_source_renewal_slot(digest)?;
        self.control.external_sources().observer_scope(digest)
    }

    /// Pure actor cache handoff, never storage/native proof. The caller already
    /// owns the actual current binding and finite physical/admission permits.
    pub(super) fn install_external_source_observer_scope(
        &mut self,
        digest: [u8; 32],
        previous: &Arc<ObserverScope>,
        next: Arc<ObserverScope>,
    ) -> bool {
        self.control
            .external_sources_mut()
            .install_observer_scope(digest, previous, next)
    }
}
