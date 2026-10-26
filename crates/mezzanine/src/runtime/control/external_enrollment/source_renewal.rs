//! Sealed native renewal worker custody, not maintenance admission or publication.
//!
//! One original source slot supplies a physical permit and its retirement/I/O
//! fence. Clones share one cached result, including ambiguity/failure; executing
//! again never replenishes a mutation allowance. Workers independently verify
//! original-kind writer/ancestry/boot-clock evidence and use the captured store.
//! No actor path currently constructs this work. Future maintenance admission
//! must additionally select the current binding/registry and pace finite work;
//! this module does not restore observer rights or publish cold-recovery success.

use super::*;

/// Shared physical attempt, with no credential, source path or identity in Debug.
struct RenewalAttempt {
    slot: Arc<source_retirement::NativeSourceSlot>,
    origin: Arc<UnixOriginProcess>,
    ancestry: Arc<UnixAncestryWitness>,
    deadline: Instant,
    outcome: OnceLock<Result<()>>,
    /// Retained until the LAST clone/physical worker retires, not numeric cleanup.
    _permit: tokio::sync::OwnedSemaphorePermit,
}

/// Original-source worker; no caller-provided native PID/key/token/store exists.
#[derive(Clone)]
pub(super) struct ExternalSourceRenewalWork(Arc<RenewalAttempt>);

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
    /// payload-selected identity/store. Future actor admission must select a
    /// current binding/registry and pace work before calling this internal seam.
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
            _permit: permit,
        })))
    }
}

impl ExternalSourceRenewalWork {
    /// Worker-only one-shot execution. Cache both success and failure across
    /// clones; a lost result never causes a second physical CAS. Deadlines are
    /// cooperative checks, not blocked-syscall preemption or native exit proof.
    /// A cached outcome is historical, never fresh actor publication authority.
    pub(super) fn execute(&self) -> Result<()> {
        self.0
            .outcome
            .get_or_init(|| {
                self.0
                    .slot
                    .renew_native(&self.0.origin, &self.0.ancestry, self.0.deadline)
            })
            .clone()
    }

    /// Exercises deadline refusal without sleeping or changing global clocks.
    #[cfg(test)]
    pub(super) fn expire_for_tests(&mut self) {
        Arc::get_mut(&mut self.0).unwrap().deadline = Instant::now();
    }
}
