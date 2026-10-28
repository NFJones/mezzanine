//! Initial fork authority publishes with its exact durable append owner.
//!
//! Projection-bearing admission journals the complete payload before touching
//! the archive, then reconciles rows and the existing crash-safe sidecar under
//! one conversation lease before acknowledging readiness. Retries may repair
//! their own partial publication, but cannot adopt unrelated existing raw
//! history or replace a different epoch. Global recovery preflights every
//! captured projection before any replay changes an archive. The receipt stays
//! authoritative until rows, sidecar and its required-state marker all settle.

use super::{AgentTranscriptStore, AppendReceipt, MezError, Result, TranscriptAppendBatch};
use mez_agent::TranscriptEntry;
use std::{collections::BTreeMap, fs::File, path::PathBuf};

/// Checked receipt identity, path and decoded rows from the existing reader.
type StoredReceipt = (PathBuf, AppendReceipt, Vec<TranscriptEntry>);

impl AgentTranscriptStore {
    /// Rejects unrelated archive adoption or different replay authority before
    /// writing a new journal. A matching retained receipt owns partial writes;
    /// an already identical durable epoch supports idempotent late admission.
    pub(super) fn check_initial_projection_admission_locked(
        &self,
        batch: &TranscriptAppendBatch,
        pending: &[StoredReceipt],
        committed_count: u64,
    ) -> Result<()> {
        let Some(epoch) = batch.initial_compaction_epoch() else {
            return Ok(());
        };
        let current = self.compaction_epoch(batch.conversation_id())?;
        if current.as_ref().is_some_and(|existing| existing != epoch) {
            return Err(MezError::conflict(
                "initial transcript projection differs from existing authority",
            ));
        }
        if pending.iter().any(|(_, receipt, rows)| {
            receipt.conversation_id == batch.conversation_id()
                && receipt.compaction_epoch.is_some()
                && (receipt.compaction_epoch.as_ref() != Some(epoch) || rows != batch.entries())
        }) {
            return Err(MezError::conflict(
                "transcript receipts claim different complete initial payloads",
            ));
        }
        let owned_partial = pending.iter().any(|(_, receipt, rows)| {
            receipt.conversation_id == batch.conversation_id()
                && receipt.compaction_epoch.as_ref() == Some(epoch)
                && rows == batch.entries()
        });
        if current.is_none()
            && !owned_partial
            && self.transcript_path(batch.conversation_id())?.exists()
        {
            return Err(MezError::conflict(
                "initial transcript projection requires a fresh target or matching receipt",
            ));
        }
        if current.is_some() && !owned_partial && committed_count < batch.entries().len() as u64 {
            return Err(MezError::conflict(
                "initial transcript replay cannot append new rows without its receipt",
            ));
        }
        Ok(())
    }

    /// Reconciles a journal-owned initial bundle without releasing its mutation
    /// lane. Ordinary batches remain journal-only until their usual worker
    /// appends them. No historical actions or terminal handoffs are executed.
    pub(super) fn reconcile_initial_projection_locked(
        &self,
        batch: &TranscriptAppendBatch,
        lease: &File,
    ) -> Result<()> {
        let Some(epoch) = batch.initial_compaction_epoch() else {
            return Ok(());
        };
        let previous = self.compaction_epoch(batch.conversation_id())?;
        if previous.as_ref().is_some_and(|existing| existing != epoch) {
            return Err(MezError::conflict(
                "initial transcript projection changed before publication",
            ));
        }
        let rows = batch.entries().iter().collect::<Vec<_>>();
        self.append_conversation_rows_locked(batch.conversation_id(), &rows, lease)?;
        // The immutable batch and checked archive already match. The writer
        // validates range groups again and repairs a sidecar-before-marker
        // failure even when the exact sidecar is already present.
        self.save_compaction_projection_locked(epoch.clone(), None, Some(&previous))
    }

    /// Requires both exact replay bytes and the durable required-state marker
    /// before retiring initial ownership, including a metadata-only receipt.
    pub(super) fn verify_initial_projection_committed_locked(
        &self,
        batch: &TranscriptAppendBatch,
    ) -> Result<()> {
        if let Some(epoch) = batch.initial_compaction_epoch()
            && (self.compaction_epoch(batch.conversation_id())?.as_ref() != Some(epoch)
                || !self.compaction_epoch_marker_is_committed(batch.conversation_id())?)
        {
            return Err(MezError::invalid_state(
                "transcript receipt initial projection is not fully committed",
            ));
        }
        Ok(())
    }

    /// Rejects mutually conflicting or superseded projection claims across the
    /// complete recovery snapshot before unrelated valid receipts are replayed.
    pub(super) fn preflight_initial_projection_receipts(
        &self,
        receipts: &[StoredReceipt],
    ) -> Result<()> {
        let mut projections = BTreeMap::new();
        for (_, receipt, rows) in receipts {
            if let Some(epoch) = receipt.compaction_epoch.as_ref()
                && projections
                    .insert(receipt.conversation_id.as_str(), (epoch, rows))
                    .is_some_and(|previous| previous != (epoch, rows))
            {
                return Err(MezError::conflict(
                    "transcript recovery receipts claim different complete initial payloads",
                ));
            }
        }
        for (conversation_id, (epoch, _)) in projections {
            let _lease = self.acquire_conversation_lock(conversation_id)?;
            if self
                .compaction_epoch(conversation_id)?
                .as_ref()
                .is_some_and(|current| current != epoch)
            {
                return Err(MezError::conflict(
                    "transcript recovery initial projection conflicts with durable authority",
                ));
            }
        }
        Ok(())
    }
}
