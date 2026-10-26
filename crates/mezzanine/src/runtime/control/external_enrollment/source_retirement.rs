//! Bounded source-lifetime metadata custody and absorbing retirement work.
//!
//! A permit is reserved before native preparation and retained by the original
//! worker, live source anchor and any queued/physical persistence work. Observer
//! rotation shares this same slot; it cannot mint another receipt or lose its
//! retirement fence. The actor only latches retirement and queues sealed work.
//! Retirement includes logical lease/authority expiry, not only native exit;
//! metadata never claims that a still-live producer has physically died.
//! SQLite and mutex acquisition occur solely on blocking workers. Original
//! outcomes remain cached, including errors; this owner provides neither blind
//! retries nor cold-recovery authority or a successful-durability promise.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

/// Pointer-sized enrollment reference to finite metadata and worker custody.
pub(super) struct NativeSourceSlot {
    preparation: OnceLock<source::NativeSourcePreparation>,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
    retired: AtomicBool,
    /// Never acquired on actor; serializes actual acceptance/retirement I/O.
    io: std::sync::Mutex<()>,
}

impl std::fmt::Debug for NativeSourceSlot {
    /// Excludes proposal bytes, repository paths, private replies and identities.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeSourceSlot")
            .field("reserved", &self.permit.is_some())
            .field("retired", &self.retired.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl NativeSourceSlot {
    /// Reserves at most one original source's metadata; exhaustion is optional
    /// metadata unavailability, never permission to enlarge the finite pool.
    pub(super) fn reserve(budget: Arc<tokio::sync::Semaphore>) -> Arc<Self> {
        Arc::new(Self {
            permit: budget.try_acquire_owned().ok(),
            ..Self::unreserved()
        })
    }

    /// Helpers/unsupported paths own no metadata permit or publication rights.
    pub(super) fn unreserved() -> Self {
        Self {
            preparation: OnceLock::new(),
            permit: None,
            retired: AtomicBool::new(false),
            io: std::sync::Mutex::new(()),
        }
    }

    /// Pure admission check before capturing any new activation proposal.
    pub(super) fn has_reservation(&self) -> bool {
        self.permit.is_some()
    }

    /// Reads the original preparation without storage, clocks or lock waiting.
    pub(super) fn get(&self) -> Option<&source::NativeSourcePreparation> {
        self.preparation.get()
    }

    /// Fixture-only gate proving retirement never waits for worker-only I/O.
    #[cfg(test)]
    pub(super) fn hold_io_for_tests(&self) -> std::sync::MutexGuard<'_, ()> {
        self.io.lock().unwrap()
    }

    /// Publishes original proposal once, only inside its reserved metadata slot.
    pub(super) fn set(&self, preparation: source::NativeSourcePreparation) -> bool {
        self.has_reservation() && self.preparation.set(preparation).is_ok()
    }

    /// Worker-only acceptance cannot pass a latched logical retirement. Holding
    /// the same lock through I/O means retirement either follows acceptance or
    /// prevents it; no database read or mutex wait occurs on actor.
    pub(super) fn accept(
        &self,
        store: &crate::storage::token_usage::TokenUsageStore,
    ) -> Result<()> {
        let _guard = self
            .io
            .lock()
            .map_err(|_| MezError::invalid_state("external source metadata worker failed"))?;
        if self.retired.load(Ordering::Acquire) {
            return Err(MezError::conflict("external source activation has retired"));
        }
        self.get()
            .ok_or_else(|| MezError::invalid_state("external source preparation disappeared"))?
            .accept(store)
    }

    /// Closes only old control-owner acceptance. Snapshot replacement is not a
    /// native lifetime end and must not manufacture a persistent tombstone.
    pub(super) fn invalidate_acceptance(&self) {
        self.retired.store(true, Ordering::Release);
    }

    /// Atomically latches actor-owned source retirement before proof is dropped.
    /// Only an actually admitted preparation produces one sealed persistence
    /// work item; duplicate retirement cannot queue a second owner.
    pub(super) fn retire(self: &Arc<Self>) -> Option<ExternalSourceRetirementWork> {
        let preparation = self.get()?;
        preparation.admission.get()?;
        if self.retired.swap(true, Ordering::AcqRel) {
            return None;
        }
        Some(ExternalSourceRetirementWork {
            slot: self.clone(),
            store: preparation.original_store().clone(),
        })
    }
}

/// Exact actor-sealed retirement, not a caller-provided key or general store job.
#[derive(Clone)]
pub(crate) struct ExternalSourceRetirementWork {
    slot: Arc<NativeSourceSlot>,
    store: crate::storage::token_usage::TokenUsageStore,
}

impl std::fmt::Debug for ExternalSourceRetirementWork {
    /// No native identity, proposal, token, path or callback data enters Debug.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalSourceRetirementWork")
            .finish_non_exhaustive()
    }
}

impl PartialEq for ExternalSourceRetirementWork {
    /// Cloned work is the same admitted obligation, not merely equal key text.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.slot, &other.slot)
    }
}

impl Eq for ExternalSourceRetirementWork {}

impl ExternalSourceRetirementWork {
    /// Original immutable destination for existing persistence health reporting.
    pub(crate) fn path(&self) -> &std::path::Path {
        self.store.path()
    }

    /// Runs only off actor. Retains the original exact result and never replaces
    /// a changed proposal, retries a failed mutation, or reactivates any source.
    pub(crate) fn execute(&self) -> Result<()> {
        let _guard = self
            .slot
            .io
            .lock()
            .map_err(|_| MezError::invalid_state("external source metadata worker failed"))?;
        if !self.slot.retired.load(Ordering::Acquire) {
            return Err(MezError::invalid_state(
                "external source retirement was not admitted",
            ));
        }
        self.slot
            .get()
            .ok_or_else(|| MezError::invalid_state("external source preparation disappeared"))?
            .retire()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resource-custody control only: a worker retains the one finite permit
    /// after its caller drops its reference. No synthetic slot is a native
    /// witness or actor grant. Capacity returns only after that worker exits.
    #[tokio::test(flavor = "current_thread")]
    async fn external_source_metadata_permit_tracks_physical_owner() {
        let budget = Arc::new(tokio::sync::Semaphore::new(1));
        let slot = NativeSourceSlot::reserve(budget.clone());
        assert!(slot.has_reservation());
        assert!(slot.retire().is_none(), "a permit is not an actor grant");
        let native = slot.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker = tokio::task::spawn_blocking(move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            drop(native);
        });
        started_rx.await.unwrap();
        drop(slot);
        assert_eq!(budget.available_permits(), 0);
        assert!(!NativeSourceSlot::reserve(budget.clone()).has_reservation());
        release_tx.send(()).unwrap();
        worker.await.unwrap();
        assert_eq!(budget.available_permits(), 1);
    }
}
