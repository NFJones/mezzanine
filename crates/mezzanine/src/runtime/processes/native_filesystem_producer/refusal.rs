//! Positive no-dispatch custody, separate from native execution/join evidence.
//!
//! The actor supplies original queued work and its captured rejection. Capture
//! requires closed, settled admission and an actually unstarted execution slot.
//! Semantic empty outcome data can feed canonical settlement, but never enters
//! Execution::Joined. Immutable proof and historical transfer keep only original
//! metadata/identity, not a strong producer or enrollment back-reference.

use super::*;
use mez_agent::native_action::{NativeEffectCertainty, NativePatchOutcome, NativePatchPhase};

/// Immutable positive no-dispatch source, not a native worker result receipt.
#[derive(Clone)]
pub(crate) struct NativeFilesystemRefusal(Arc<RefusalSource>);
/// Exact original metadata retained before any canonical projection can fail.
struct RefusalSource {
    /// Identity without owning the cell that retains this producer.
    attempt: NativeFilesystemAttempt,
    /// Original producer identity without a strong producer reference.
    producer: NativeFilesystemProducerIdentity,
    /// Actual original queued work allocation.
    work: Arc<NativeFilesystemWork>,
    /// Semantic unexecuted data, explicitly not a worker/join observation.
    outcome: Arc<NativePatchWorkerOutcome>,
    /// Captured actor rejection, or explicit closed-before-dispatch cancellation.
    cause: MezError,
    /// Only absence of an actor rejection denotes cancellation of queued work.
    cancelled: bool,
}
impl std::fmt::Debug for NativeFilesystemRefusal {
    /// Does not print source bytes, diagnostics or allocation addresses.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeFilesystemRefusal")
    }
}
impl NativeFilesystemRefusal {
    /// Compares the actual immutable proof allocation, not matching labels.
    pub(crate) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    /// Checks original source and producer identity; grants no dispatch rights.
    pub(crate) fn belongs_to(&self, producer: &NativeFilesystemProducer) -> bool {
        self.0.attempt == *producer.attempt() && self.0.producer == producer.identity()
    }
    /// Original frozen work, independent of later display/current execution.
    pub(crate) fn work(&self) -> &Arc<NativeFilesystemWork> {
        &self.0.work
    }
    /// Empty semantic outcome; this never certifies an execution join.
    pub(crate) fn outcome(&self) -> &Arc<NativePatchWorkerOutcome> {
        &self.0.outcome
    }
    /// Actual first rejection remains available after required settlement fails.
    pub(crate) fn cause(&self) -> &MezError {
        &self.0.cause
    }
    /// Distinguishes queued cancellation from a positively recorded rejection.
    pub(crate) fn cancelled(&self) -> bool {
        self.0.cancelled
    }
}
impl NativeFilesystemProducer {
    /// Captures positive unissued custody synchronously under execution/admission
    /// locks. Busy execution stays pending; issued/claiming/open work cannot mint
    /// refusal. The actor independently validates original Queued/Rejected state.
    pub(crate) fn capture_refusal(
        &self,
        work: Arc<NativeFilesystemWork>,
        rejection: Option<MezError>,
    ) -> Result<Option<NativeFilesystemRefusal>> {
        let Ok(execution) = self.0.execution.try_lock() else {
            return Ok(None);
        };
        let admission =
            self.0.admission.lock().map_err(|_| {
                MezError::invalid_state("native filesystem admission lock poisoned")
            })?;
        if !matches!(*execution, Execution::Unstarted)
            || !admission.closed
            || admission.claiming
            || admission.work.is_some()
            || admission.dispatch.is_some()
        {
            return Err(MezError::conflict(
                "filesystem refusal lacks positive no-dispatch custody",
            ));
        }
        let mut retained = self
            .0
            .refusal
            .lock()
            .map_err(|_| MezError::invalid_state("filesystem refusal custody poisoned"))?;
        if let Some(original) = &*retained {
            return if Arc::ptr_eq(original.work(), &work) && original.belongs_to(self) {
                Ok(Some(original.clone()))
            } else {
                Err(MezError::conflict("filesystem refusal source changed"))
            };
        }
        let cancelled = rejection.is_none();
        let cause = rejection
            .unwrap_or_else(|| MezError::conflict("Native patch cancelled before dispatch."));
        let outcome = Arc::new(NativePatchWorkerOutcome {
            result: NativePatchOutcome {
                identity: work.identity.clone(),
                phase: NativePatchPhase::Settle,
                certainty: NativeEffectCertainty::Unexecuted,
                confirmed_effects: Vec::new(),
                diagnostic: Some(cause.message().to_owned()),
            },
            receipts: Vec::new(),
            completed_operations: Vec::new(),
        });
        self.validate_confirmed_outcome(&outcome)?;
        let refusal = NativeFilesystemRefusal(Arc::new(RefusalSource {
            attempt: self.attempt().non_owning(),
            producer: self.identity(),
            work,
            outcome,
            cause,
            cancelled,
        }));
        *retained = Some(refusal.clone());
        Ok(Some(refusal))
    }
    /// Reads retained positive refusal data without claiming execution/join.
    pub(crate) fn refusal(&self) -> Result<Option<NativeFilesystemRefusal>> {
        Ok(self
            .0
            .refusal
            .lock()
            .map_err(|_| MezError::invalid_state("filesystem refusal custody poisoned"))?
            .clone())
    }
    /// Recovers the historical exact no-dispatch handoff after a lost reply.
    pub(crate) fn refusal_acknowledgement(
        &self,
    ) -> Result<Option<crate::runtime::agent::NativeFilesystemRefusalTransfer>> {
        Ok(self
            .0
            .refusal_transfer
            .lock()
            .map_err(|_| MezError::invalid_state("filesystem refusal transfer custody poisoned"))?
            .clone())
    }
    /// Stores opaque actor-issued acceptance before original owner removal.
    pub(crate) fn retain_refusal_acknowledgement(
        &self,
        ack: crate::runtime::agent::NativeFilesystemRefusalTransfer,
    ) -> Result<()> {
        if !ack.belongs_to(self) {
            return Err(MezError::conflict(
                "filesystem refusal transfer producer changed",
            ));
        }
        let mut accepted =
            self.0.refusal_transfer.lock().map_err(|_| {
                MezError::invalid_state("filesystem refusal transfer custody poisoned")
            })?;
        if accepted.is_some() {
            return Err(MezError::conflict(
                "filesystem refusal transfer already accepted",
            ));
        }
        *accepted = Some(ack);
        Ok(())
    }
}
