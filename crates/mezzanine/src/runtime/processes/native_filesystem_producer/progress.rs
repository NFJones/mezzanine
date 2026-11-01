//! Bounded cumulative post-commit confirmations under original producer custody.
//!
//! The actual descriptor callback records before best-effort actor notification.
//! The journal retains every actual endpoint within the admitted operation and
//! aggregate diff budgets. Snapshots expose only the first 64 immutable receipt
//! allocations for live display. Overflow remains available after a genuine join
//! failure, independently of notification coalescing or a lost final outcome.
//! The blocking task retains only a weak recorder, so its callback cannot keep
//! its own producer/JoinHandle alive or suppress external-owner-loss fencing.

use super::*;
use crate::runtime::processes::native_patch::worker::NativePatchEndpointReceipt;
use mez_agent::{
    ActionPresentationComponentIdentity, ActionPresentationExecutionIdentity,
    ActionPresentationProgress,
};

/// Matches the existing per-action presentation component ceiling.
const MAX_PROGRESS_ENDPOINTS: usize = 64;

/// Opaque original producer and immutable cumulative receipt allocations.
#[derive(Clone)]
pub(crate) struct NativeFilesystemProgress {
    /// Actor must already retain this exact producer allocation.
    producer: NativeFilesystemProducer,
    /// Stable prefix, retained even when its notification is coalesced/dropped.
    receipts: Vec<Arc<NativePatchEndpointReceipt>>,
}

/// Worker-side recorder that cannot retain its own task's producer owner.
/// An upgrade is held only during bounded receipt capture and delivery creation.
pub(crate) struct NativeFilesystemProgressRecorder {
    /// Original allocation may disappear while the worker is blocked in I/O.
    owner: std::sync::Weak<Inner>,
}

impl NativeFilesystemProgressRecorder {
    /// Retains an actual post-commit receipt only while an external owner still
    /// exists. Owner loss has already fenced future commits; the independent
    /// worker outcome retains this effect even when no notification can exist.
    pub(crate) fn record_confirmed_progress(
        &self,
        receipt: &NativePatchEndpointReceipt,
    ) -> Result<Option<NativeFilesystemProgress>> {
        let Some(owner) = self.owner.upgrade() else {
            return Ok(None);
        };
        NativeFilesystemProducer(owner).record_confirmed_progress(receipt)
    }
}

/// Creates a display identity from an actual endpoint without command hashes.
/// Actual confirmation order distinguishes directories and both ends of a move.
pub(crate) fn endpoint_progress(
    work: &NativeFilesystemWork,
    receipt: &NativePatchEndpointReceipt,
) -> Result<ActionPresentationProgress> {
    let ordinal = receipt.confirmation_ordinal;
    let source = if receipt.is_file() {
        receipt.diff.clone()
    } else {
        format!("Created directory {}\n", receipt.path.escape_default())
    };
    let mut progress = ActionPresentationProgress::new(
        &work.owner.turn.turn_id,
        &work.owner.action.id,
        ActionPresentationExecutionIdentity::Attempt(work.identity.transaction.clone()),
        1,
        ActionPresentationComponentIdentity::confirmed_mutation(ordinal, &receipt.path),
        &source,
    );
    progress.source_truncated |= receipt.diff_truncated;
    Ok(progress)
}

impl NativeFilesystemProducer {
    /// Returns a weak callback recorder; the worker must never own its producer.
    pub(crate) fn progress_recorder(&self) -> NativeFilesystemProgressRecorder {
        NativeFilesystemProgressRecorder {
            owner: Arc::downgrade(&self.0),
        }
    }

    /// Records only the actual worker's post-commit callback. Optional capacity
    /// refusal never discards the independent final outcome or gates mutation.
    pub(crate) fn record_confirmed_progress(
        &self,
        receipt: &NativePatchEndpointReceipt,
    ) -> Result<Option<NativeFilesystemProgress>> {
        let work = self
            .0
            .admission
            .lock()
            .map_err(|_| MezError::invalid_state("filesystem progress admission poisoned"))?
            .work
            .clone()
            .ok_or_else(|| MezError::conflict("filesystem progress has no issued original work"))?;
        if !receipt.matches_effect(
            work.identity
                .operation
                .effects
                .get(receipt.operation_ordinal),
        ) || receipt.operation_ordinal >= work.identity.operation.effects.len()
            || receipt.diff.len() > mez_agent::ACTION_PRESENTATION_PROGRESS_MAX_SOURCE_BYTES
            || receipt.path.len() > 4096
            || receipt.resolved_path.len() > 4096
        {
            return Err(MezError::invalid_args(
                "filesystem progress exceeds original bounds",
            ));
        }
        let mut retained = self
            .0
            .progress
            .lock()
            .map_err(|_| MezError::invalid_state("filesystem progress custody poisoned"))?;
        if let Some(existing) = retained
            .iter()
            .find(|existing| existing.confirmation_ordinal == receipt.confirmation_ordinal)
        {
            if existing.as_ref() != receipt {
                return Err(MezError::conflict("filesystem confirmed progress changed"));
            }
        } else {
            if retained.len()
                >= work.identity.operation.effects.len().saturating_mul(2)
                    + crate::security::filesystem::capability::parent::MAX_NATIVE_PARENT_DIRECTORIES
            {
                return Err(MezError::invalid_state(
                    "filesystem confirmation journal exceeds admitted endpoint count",
                ));
            }
            if !receipt.can_follow(retained.iter().map(AsRef::as_ref)) {
                return Err(MezError::conflict(
                    "filesystem confirmed progress is out of order",
                ));
            }
            let fact_bytes = self
                .0
                .progress_fact_bytes
                .load(std::sync::atomic::Ordering::Relaxed)
                .saturating_add(receipt.metadata(false).to_string().len() + 1);
            if fact_bytes > crate::runtime::processes::native_patch::worker::NATIVE_PATCH_FACT_BYTES
            {
                return Err(MezError::invalid_args(
                    "filesystem progress facts exceed admitted budget",
                ));
            }
            if retained
                .iter()
                .map(|prior| prior.diff.len())
                .sum::<usize>()
                .saturating_add(receipt.diff.len())
                > mez_agent::semantic_patch_planning::native::NATIVE_PATCH_DIFF_BYTES
            {
                return Err(MezError::invalid_args(
                    "filesystem progress aggregate diff exceeds budget",
                ));
            }
            retained.push(Arc::new(receipt.clone()));
            self.0
                .progress_fact_bytes
                .store(fact_bytes, std::sync::atomic::Ordering::Relaxed);
        }
        if retained.len() > MAX_PROGRESS_ENDPOINTS {
            return Ok(None);
        }
        Ok(Some(NativeFilesystemProgress {
            producer: self.clone(),
            receipts: retained.clone(),
        }))
    }

    /// Captures the retained prefix without another worker or callback replay.
    pub(crate) fn progress_snapshot(&self) -> Result<NativeFilesystemProgress> {
        Ok(NativeFilesystemProgress {
            producer: self.clone(),
            receipts: self
                .0
                .progress
                .lock()
                .map_err(|_| MezError::invalid_state("filesystem progress snapshot poisoned"))?
                .iter()
                .take(MAX_PROGRESS_ENDPOINTS)
                .cloned()
                .collect(),
        })
    }

    /// Admits only exact immutable data already recorded in this producer.
    /// Closing admission suppresses live paint but never deletes confirmations.
    pub(crate) fn matches_progress(&self, progress: &ActionPresentationProgress) -> bool {
        let Ok(admission) = self.0.admission.lock() else {
            return false;
        };
        if admission.closed {
            return false;
        }
        let Some(work) = &admission.work else {
            return false;
        };
        self.0.progress.lock().is_ok_and(|receipts| {
            receipts.iter().any(|receipt| {
                endpoint_progress(work, receipt).is_ok_and(|expected| expected == *progress)
            })
        })
    }
}

impl NativeFilesystemProgress {
    /// Returns original action/attempt attribution, never reconstructed IDs.
    pub(crate) fn producer(&self) -> &NativeFilesystemProducer {
        &self.producer
    }

    /// Validates every allocation against the still-owned original prefix.
    pub(crate) fn confirmed(&self) -> Result<Vec<mez_agent::ActionPresentationProgress>> {
        let work = self
            .producer
            .0
            .admission
            .lock()
            .map_err(|_| MezError::invalid_state("filesystem progress source poisoned"))?
            .work
            .clone()
            .ok_or_else(|| MezError::conflict("filesystem progress source unavailable"))?;
        let retained = self
            .producer
            .0
            .progress
            .lock()
            .map_err(|_| MezError::invalid_state("filesystem progress validation poisoned"))?;
        if self.receipts.len() > MAX_PROGRESS_ENDPOINTS
            || self.receipts.len() > retained.len()
            || !self
                .receipts
                .iter()
                .zip(retained.iter())
                .all(|(left, right)| Arc::ptr_eq(left, right))
        {
            return Err(MezError::conflict(
                "filesystem progress is not its original receipt prefix",
            ));
        }
        self.receipts
            .iter()
            .map(|receipt| endpoint_progress(&work, receipt))
            .collect()
    }
}
