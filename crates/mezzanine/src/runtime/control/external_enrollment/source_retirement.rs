//! Bounded source-lifetime metadata custody and absorbing retirement work.
//!
//! A permit is reserved before native preparation and retained by the original
//! worker, live source anchor and any queued/physical persistence work. Observer
//! rotation shares this same slot; it cannot mint another receipt or lose its
//! retirement fence. The actor only latches retirement and queues sealed work.
//! Retirement includes logical lease/authority expiry, not only native exit;
//! metadata never claims that a still-live producer has physically died.
//! SQLite and mutex acquisition occur solely on blocking workers. Original
//! outcomes remain cached, including errors. After failure, a read may establish
//! the exact committed tombstone or prove the original predecessor uncommitted.
//! Only the latter allows one separately cached recovery CAS, never blind replay.
//! Missing/changed/unavailable evidence preserves the original failure. This
//! owner provides neither unbounded retries nor cold-recovery authority. Required
//! end replies use a separate actor-fenced publication over this same exact work.
//! A separate one-per-source physical renewal worker shares this I/O fence and
//! retains immutable outcomes through paced admission and final actor fencing.

use super::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Pointer-sized enrollment reference to finite metadata and worker custody.
pub(super) struct NativeSourceSlot {
    preparation: OnceLock<source::NativeSourcePreparation>,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
    retired: AtomicBool,
    /// One required reply worker per original source, across registry resets.
    end_publication: Arc<tokio::sync::Semaphore>,
    /// One physical renewal attempt across clones and old control generations.
    renewal_worker: Arc<tokio::sync::Semaphore>,
    /// Monotonic scheduling only, never a persisted native lifetime clock.
    renewal_clock: Instant,
    next_renewal_ms: AtomicU64,
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
            end_publication: Arc::new(tokio::sync::Semaphore::new(1)),
            renewal_worker: Arc::new(tokio::sync::Semaphore::new(1)),
            renewal_clock: Instant::now(),
            next_renewal_ms: AtomicU64::new(30_000),
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

    /// Nonblocking pacing across observer rotation and registry changes. A new
    /// tick is a distinct qualified intent, never replay of an earlier attempt.
    pub(super) fn admit_renewal_tick(&self) -> bool {
        let Ok(now) = u64::try_from(self.renewal_clock.elapsed().as_millis()) else {
            return false;
        };
        let due = self.next_renewal_ms.load(Ordering::Acquire);
        let Some(next) = now.checked_add(30_000) else {
            return false;
        };
        now >= due
            && self
                .next_renewal_ms
                .compare_exchange(due, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }

    /// Current configuration must still name the captured repository. This is
    /// only a pure destination check; the worker separately validates stored facts.
    pub(super) fn renewal_store_matches(
        &self,
        store: &crate::storage::token_usage::TokenUsageStore,
    ) -> bool {
        self.get()
            .is_some_and(|preparation| preparation.original_store().path() == store.path())
    }

    /// Pure final-publication fence; does not wait for I/O or renew any clock.
    pub(super) fn renewal_is_open(&self) -> bool {
        !self.retired.load(Ordering::Acquire)
    }

    /// Changes only an injectable scheduling hint, never native proof/time.
    #[cfg(test)]
    pub(super) fn make_renewal_due_for_tests(&self) {
        self.next_renewal_ms.store(0, Ordering::Release);
    }

    /// Reserves one worker only for an actually admitted original preparation.
    /// The physical permit stays with every clone until its final owner drops.
    pub(super) fn reserve_renewal(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.get()?.admission.get()?;
        if self.retired.load(Ordering::Acquire) {
            return None;
        }
        self.renewal_worker.clone().try_acquire_owned().ok()
    }

    /// Runs only off actor under the same I/O lock as acceptance and retirement.
    /// A queued attempt cannot pass control invalidation or logical retirement;
    /// native proof and clock qualification happen afresh inside this boundary.
    pub(super) fn renew_native(
        &self,
        origin: &Arc<UnixOriginProcess>,
        ancestry: &UnixAncestryWitness,
        deadline: Instant,
    ) -> Result<()> {
        let _guard = self
            .io
            .lock()
            .map_err(|_| MezError::invalid_state("external source metadata worker failed"))?;
        if self.retired.load(Ordering::Acquire) || Instant::now() >= deadline {
            return Err(MezError::conflict(
                "external source renewal owner expired or retired",
            ));
        }
        self.get()
            .ok_or_else(|| MezError::invalid_state("external source preparation disappeared"))?
            .renew_native(origin, ancestry, deadline)?;
        if self.retired.load(Ordering::Acquire) || Instant::now() >= deadline {
            return Err(MezError::conflict(
                "external source renewal owner expired or retired",
            ));
        }
        Ok(())
    }

    /// Closes only old control-owner acceptance. Snapshot replacement is not a
    /// native lifetime end and must not manufacture a persistent tombstone.
    pub(super) fn invalidate_acceptance(&self) {
        self.retired.store(true, Ordering::Release);
    }

    /// Clones only an already retired admitted obligation for a required reply.
    /// The anchor owner additionally proves logical source retirement; control
    /// invalidation alone must never call this to manufacture an end intent.
    pub(super) fn retired_work(self: &Arc<Self>) -> Option<ExternalSourceRetirementWork> {
        let preparation = self.get()?;
        preparation.admission.get()?;
        self.retired
            .load(Ordering::Acquire)
            .then(|| ExternalSourceRetirementWork {
                slot: self.clone(),
                store: preparation.original_store().clone(),
            })
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
    /// Physical reply custody outlives numeric actor reservations and clones.
    pub(super) fn reserve_publication(&self) -> Result<tokio::sync::OwnedSemaphorePermit> {
        self.slot
            .end_publication
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                MezError::new(
                    crate::error::MezErrorKind::RateLimited,
                    "external source end already pending",
                )
            })
    }

    /// Verifies current exact tombstone evidence after the existing bounded
    /// retirement operation; a historical cached success alone is not a reply.
    pub(super) fn execute_for_publication(&self) -> Result<()> {
        self.execute()?;
        let _guard = self
            .slot
            .io
            .lock()
            .map_err(|_| MezError::invalid_state("external source metadata worker failed"))?;
        self.slot
            .get()
            .ok_or_else(|| MezError::invalid_state("external source preparation disappeared"))?
            .reconcile_retirement()
    }

    /// Original immutable destination for existing persistence health reporting.
    pub(crate) fn path(&self) -> &std::path::Path {
        self.store.path()
    }

    /// Runs only off actor. An exact committed tombstone reconciles a lost reply;
    /// a positively unchanged predecessor permits at most one recovery CAS.
    /// Both outcomes stay cached; unknown/changed state cannot authorize writes
    /// or replace the original failure. Native authority always remains closed.
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
        let preparation = self
            .slot
            .get()
            .ok_or_else(|| MezError::invalid_state("external source preparation disappeared"))?;
        preparation
            .retire()
            .or_else(|original| preparation.recover_retirement().map_err(|_| original))
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
