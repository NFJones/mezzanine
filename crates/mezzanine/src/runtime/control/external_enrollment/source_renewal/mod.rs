//! Sealed native renewal work with paced actor admission and completion fencing.
//!
//! One original source slot supplies a physical permit and its retirement/I/O
//! fence. Clones share one cached result, including ambiguity/failure; executing
//! again never replenishes a mutation allowance. Workers independently verify
//! original-kind writer/ancestry/boot-clock evidence and use the captured store.
//! Existing idle maintenance selects current ordinary bindings; the existing
//! persistence lane executes work and returns an opaque observed-join receipt.
//! Final actor checks precede metadata health publication, never new observer
//! rights or cold-recovery success.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
mod admission;

/// Shared physical attempt, with no credential, source path or identity in Debug.
struct RenewalAttempt {
    slot: Arc<source_retirement::NativeSourceSlot>,
    origin: Arc<UnixOriginProcess>,
    ancestry: Arc<UnixAncestryWitness>,
    deadline: Instant,
    outcome: OnceLock<Result<()>>,
    /// Absorbing even when initialization unwinds before caching its outcome.
    started: AtomicBool,
    /// Deterministic owned-worker join fault, before any native/store operation.
    #[cfg(test)]
    panic_before_io: AtomicBool,
    scope: OnceLock<admission::RenewalActorScope>,
    /// Set only after the real blocking worker join was observed.
    completion: OnceLock<bool>,
    /// Retained until the LAST clone/physical worker retires, not numeric cleanup.
    _permit: tokio::sync::OwnedSemaphorePermit,
}

/// Original-source worker; no caller-provided native PID/key/token/store exists.
#[derive(Clone)]
pub(crate) struct ExternalSourceRenewalWork(Arc<RenewalAttempt>);

impl PartialEq for ExternalSourceRenewalWork {
    /// Equality identifies the admitted physical attempt, not native selectors.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for ExternalSourceRenewalWork {}

impl std::fmt::Debug for ExternalSourceRenewalWork {
    /// Deliberately excludes private native evidence and original destination.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExternalSourceRenewalWork")
            .finish_non_exhaustive()
    }
}

impl ExternalEnrollmentWork {
    /// Reuses a genuine initial-admission fixture without fabricating native proof.
    #[cfg(test)]
    pub(super) fn prepare_source_renewal(&self) -> Option<ExternalSourceRenewalWork> {
        if self.curated.is_some() || self.helper.is_some() {
            return None;
        }
        let ancestry = self.ancestry.get()?.clone();
        self.source_preparation
            .prepare_renewal(self.origin.clone(), ancestry)
    }
}

impl source_retirement::NativeSourceSlot {
    /// Captures one actual original-source slot with native evidence, never a
    /// payload-selected identity/store. Actor admission selects a current
    /// binding/registry and applies pacing before enqueueing this internal work.
    /// The worker independently rejects foreign evidence and stale native proof.
    pub(super) fn prepare_renewal(
        self: &Arc<Self>,
        origin: Arc<UnixOriginProcess>,
        ancestry: Arc<UnixAncestryWitness>,
    ) -> Option<ExternalSourceRenewalWork> {
        let permit = self.reserve_renewal()?;
        Some(ExternalSourceRenewalWork(Arc::new(RenewalAttempt {
            slot: self.clone(),
            origin,
            ancestry,
            deadline: Instant::now().checked_add(ADMISSION_BUDGET)?,
            outcome: OnceLock::new(),
            started: AtomicBool::new(false),
            #[cfg(test)]
            panic_before_io: AtomicBool::new(false),
            scope: OnceLock::new(),
            completion: OnceLock::new(),
            _permit: permit,
        })))
    }
}

impl ExternalSourceRenewalWork {
    /// Existing persistence-lane executor observes the actual blocking join.
    /// Caller-supplied completion events cannot manufacture this private receipt.
    pub(crate) async fn execute_persistence(&self) -> bool {
        let native = self.clone();
        let success = match tokio::task::spawn_blocking(move || native.execute_once()).await {
            Ok((false, _)) => return false, // Another execution owns the result.
            Ok((true, result)) => result.is_ok(),
            Err(_) => false, // Exact owned task joined; panic data stays private.
        };
        let _ = self.0.completion.set(success);
        success
    }

    /// Worker-only one-shot execution. Cache both success and failure across
    /// clones; a lost result never causes a second physical CAS. Deadlines are
    /// cooperative checks, not blocked-syscall preemption or native exit proof.
    /// A cached outcome is historical, never fresh actor publication authority.
    #[cfg(test)]
    pub(super) fn execute(&self) -> Result<()> {
        self.execute_once().1
    }

    /// Returns a terminal result only for the sole execution or an existing
    /// immutable cache. Busy/unknown attempts never create a join receipt.
    fn execute_once(&self) -> (bool, Result<()>) {
        if let Some(outcome) = self.0.outcome.get() {
            return (true, outcome.clone());
        }
        if self.0.started.swap(true, Ordering::AcqRel) {
            return (
                false,
                Err(MezError::conflict(
                    "external source renewal outcome pending or unavailable",
                )),
            );
        }
        #[cfg(test)]
        if self.0.panic_before_io.swap(false, Ordering::AcqRel) {
            panic!("owned renewal worker fixture panic");
        }
        let result = self
            .0
            .slot
            .renew_native(&self.0.origin, &self.0.ancestry, self.0.deadline);
        let _ = self.0.outcome.set(result.clone());
        (true, result)
    }

    /// Injects an owned-task failure without private payloads or external effects.
    #[cfg(test)]
    pub(super) fn panic_before_io_for_tests(&self) {
        self.0.panic_before_io.store(true, Ordering::Release);
    }

    /// Inspects only a cached fixture error; never executes or retries work.
    #[cfg(test)]
    pub(super) fn cached_failure_for_tests(&self) -> Option<String> {
        self.0
            .outcome
            .get()?
            .as_ref()
            .err()
            .map(|error| error.message().to_owned())
    }

    /// Exercises deadline refusal without sleeping or changing global clocks.
    #[cfg(test)]
    pub(super) fn expire_for_tests(&mut self) {
        Arc::get_mut(&mut self.0).unwrap().deadline = Instant::now();
    }
}
