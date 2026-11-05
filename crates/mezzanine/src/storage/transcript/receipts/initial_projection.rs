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
        if !batch.has_initial_authority() {
            return Ok(());
        }
        let epoch = batch.initial_compaction_epoch();
        let accounting = batch.initial_row_accounting();
        let current = self.compaction_epoch_locked(batch.conversation_id())?;
        if current
            .as_ref()
            .is_some_and(|existing| epoch.is_none_or(|epoch| !existing.matches_expected(epoch)))
        {
            return Err(MezError::conflict(
                "initial transcript projection differs from existing authority",
            ));
        }
        if pending.iter().any(|(_, receipt, rows)| {
            receipt.conversation_id == batch.conversation_id()
                && receipt.has_initial_authority()
                && (receipt.compaction_epoch.as_ref() != epoch
                    || receipt.initial_row_accounting.as_ref() != accounting
                    || rows != batch.entries())
        }) {
            return Err(MezError::conflict(
                "transcript receipts claim different complete initial payloads",
            ));
        }
        let owned_partial = pending.iter().any(|(_, receipt, rows)| {
            receipt.conversation_id == batch.conversation_id()
                && receipt.compaction_epoch.as_ref() == epoch
                && receipt.initial_row_accounting.as_ref() == accounting
                && rows == batch.entries()
        });
        let current_accounting =
            self.preflight_initial_row_accounting_locked(batch.conversation_id(), accounting)?;
        let durable_initial = if accounting.is_some() {
            current_accounting
        } else {
            current.is_some()
        };
        if !durable_initial
            && !owned_partial
            && self.transcript_path(batch.conversation_id())?.exists()
        {
            return Err(MezError::conflict(
                "initial transcript projection requires a fresh target or matching receipt",
            ));
        }
        if durable_initial && !owned_partial && committed_count < batch.entries().len() as u64 {
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
        if !batch.has_initial_authority() {
            return Ok(());
        }
        let previous = self.compaction_epoch_locked(batch.conversation_id())?;
        if previous.as_ref().is_some_and(|existing| {
            batch
                .initial_compaction_epoch()
                .is_none_or(|epoch| !existing.matches_expected(epoch))
        }) {
            return Err(MezError::conflict(
                "initial transcript projection changed before publication",
            ));
        }
        let rows = batch.entries().iter().collect::<Vec<_>>();
        self.append_conversation_rows_locked(batch.conversation_id(), &rows, lease)?;
        if let Some(accounting) = batch.initial_row_accounting() {
            self.publish_initial_row_accounting_locked(
                batch.conversation_id(),
                batch.entries(),
                accounting,
            )?;
        }
        let Some(epoch) = batch.initial_compaction_epoch() else {
            return Ok(());
        };
        // The immutable batch and checked archive already match. The writer
        // validates range groups again and repairs a sidecar-before-marker
        // failure even when the exact sidecar is already present.
        let projection = if let Some(previous) = &previous {
            previous.clone()
        } else {
            let mut projection = epoch.clone();
            if matches!(projection.version, 1 | 2) {
                projection.import_legacy_summary_records()?;
            }
            projection
        };
        self.save_compaction_projection_locked(projection, None, Some(&previous))
    }

    /// Requires both exact replay bytes and the durable required-state marker
    /// before retiring initial ownership, including a metadata-only receipt.
    pub(super) fn verify_initial_projection_committed_locked(
        &self,
        batch: &TranscriptAppendBatch,
    ) -> Result<()> {
        if let Some(accounting) = batch.initial_row_accounting() {
            self.verify_initial_row_accounting_locked(
                batch.conversation_id(),
                batch.entries(),
                accounting,
            )?;
        }
        if let Some(epoch) = batch.initial_compaction_epoch()
            && (self
                .compaction_epoch_locked(batch.conversation_id())?
                .as_ref()
                .is_none_or(|current| !current.matches_expected(epoch))
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
            let payload = (
                receipt.compaction_epoch.as_ref(),
                receipt.initial_row_accounting.as_ref(),
                rows,
            );
            if receipt.has_initial_authority()
                && projections
                    .insert(receipt.conversation_id.as_str(), payload)
                    .is_some_and(|previous| previous != payload)
            {
                return Err(MezError::conflict(
                    "transcript recovery receipts claim different complete initial payloads",
                ));
            }
        }
        for (conversation_id, (epoch, accounting, _)) in projections {
            let _lease = self.acquire_conversation_lock(conversation_id)?;
            if self
                .compaction_epoch_locked(conversation_id)?
                .as_ref()
                .is_some_and(|current| epoch.is_none_or(|epoch| !current.matches_expected(epoch)))
            {
                return Err(MezError::conflict(
                    "transcript recovery initial projection conflicts with durable authority",
                ));
            }
            self.preflight_initial_row_accounting_locked(conversation_id, accounting)?;
        }
        Ok(())
    }
}
