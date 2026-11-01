//! Root custody of one actual original archive verification worker.
//!
//! Registration precedes polling work. A stale unstarted owner is closed before
//! replacement; a started owner cannot leave this slot until actual join. The
//! immutable worker never captures the producer, avoiding self-custody cycles.
//! This slot is storage-worker ownership, not mutation or final-transfer authority.

use super::*;
use crate::runtime::agent::{NativeFilesystemArchiveAcceptance, RuntimeTranscriptPrefixReceipt};

/// Bounded original source/handle ownership, independent of observer futures.
pub(super) struct ArchiveWorkerOwner {
    /// Source remains attributable even when logical source later changes/fails.
    source: NativeFilesystemArchiveAcceptance,
    /// Actual worker must survive source replacement and monitor cancellation.
    pub(super) receipt: RuntimeTranscriptPrefixReceipt,
}

impl NativeFilesystemProducer {
    /// Retains verification work before its first poll. Another original attempt
    /// cannot borrow this root slot; a started unjoined predecessor stays owned.
    pub(crate) fn register_archive_worker(
        &self,
        source: &NativeFilesystemArchiveAcceptance,
    ) -> Result<Option<RuntimeTranscriptPrefixReceipt>> {
        if source.attempt() != self.attempt() || !source.producer_is(self) {
            return Err(MezError::conflict(
                "archive verifier belongs to another filesystem attempt",
            ));
        }
        let Some(receipt) = source.physical_receipt()? else {
            return Ok(None);
        };
        let mut owner = self
            .0
            .archive
            .lock()
            .map_err(|_| MezError::invalid_state("archive worker root owner poisoned"))?;
        if let Some(existing) = &*owner {
            if existing.receipt == receipt {
                return Ok(Some(receipt));
            }
            if !existing.receipt.close_for_replacement()? {
                return Err(MezError::conflict(
                    "original archive worker still requires actual join",
                ));
            }
        }
        *owner = Some(ArchiveWorkerOwner {
            source: source.clone(),
            receipt: receipt.clone(),
        });
        Ok(Some(receipt))
    }

    /// Returns only actual retained source/work, including a now-stale snapshot.
    /// Retirement must collect its join even when a newer source cannot use it.
    pub(crate) fn archive_worker(
        &self,
    ) -> Result<
        Option<(
            NativeFilesystemArchiveAcceptance,
            RuntimeTranscriptPrefixReceipt,
        )>,
    > {
        Ok(self
            .0
            .archive
            .lock()
            .map_err(|_| MezError::invalid_state("archive worker root owner poisoned"))?
            .as_ref()
            .map(|owner| (owner.source.clone(), owner.receipt.clone())))
    }
}
