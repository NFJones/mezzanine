//! Journal-qualified ordinary placement publishes under the raw append lease.
//!
//! New proposals cannot adopt previously committed implicit legacy rows. Exact
//! retained receipts may finish their own interrupted raw/map publication. Cold
//! canonical replay repairs those accepted receipts before exposing occurrence
//! identities, so a crash cannot briefly lend the archive-slot fallback ID to a
//! row whose actual producer already owns a different occurrence.

use super::{AgentTranscriptStore, AppendReceipt, MezError, Result, TranscriptAppendBatch};
use mez_agent::{ContextBlockOccurrenceId, TranscriptEntry};
use rusqlite::{OptionalExtension, params};
use std::{collections::BTreeMap, fs::File, path::PathBuf};

type StoredReceipt = (PathBuf, AppendReceipt, Vec<TranscriptEntry>);
type ReceiptRowClaims<'a> = BTreeMap<(&'a str, u64), &'a TranscriptEntry>;

/// Checks all retained raw and occurrence claims before any publication. This
/// pure check is shared by global recovery and one already-locked replay lane.
fn ordinary_receipt_claims(receipts: &[StoredReceipt]) -> Result<ReceiptRowClaims<'_>> {
    let mut rows = BTreeMap::new();
    let mut origins = BTreeMap::new();
    for (_, receipt, entries) in receipts {
        for entry in entries {
            let key = (receipt.conversation_id.as_str(), entry.sequence);
            if rows
                .insert(key, entry)
                .is_some_and(|previous| previous != entry)
            {
                return Err(MezError::conflict(
                    "transcript recovery receipts conflict at the same sequence",
                ));
            }
        }
        if let Some(accounting) = &receipt.row_accounting {
            for (sequence, origin) in accounting.origins() {
                let key = (receipt.conversation_id.as_str(), *sequence);
                if origins
                    .insert(key, *origin)
                    .is_some_and(|previous| previous != *origin)
                {
                    return Err(MezError::conflict("recovery append occurrences conflict"));
                }
            }
        }
    }
    for (_, receipt, entries) in receipts {
        for entry in entries {
            if let Some(origin) = origins.get(&(receipt.conversation_id.as_str(), entry.sequence))
                && receipt
                    .row_accounting
                    .as_ref()
                    .and_then(|owner| owner.origins().get(&entry.sequence))
                    != Some(origin)
            {
                return Err(MezError::conflict(
                    "recovery append has conflicting unaccounted ownership",
                ));
            }
        }
    }
    Ok(rows)
}

impl AgentTranscriptStore {
    /// Reads existing explicit placement and available costs without migration.
    /// Missing overrides are repairable only by an actual retained receipt.
    fn ordinary_accounting_state_locked(
        &self,
        batch: &TranscriptAppendBatch,
    ) -> Result<BTreeMap<u64, ContextBlockOccurrenceId>> {
        let Some(accounting) = batch.row_accounting() else {
            return Ok(BTreeMap::new());
        };
        accounting.validate(batch.entries())?;
        let cid = batch.conversation_id();
        if !self.root.join(cid).join("row-origins.sqlite3").exists() {
            if self.row_origins_required_locked(cid)? {
                return Err(MezError::invalid_state(
                    "required ordinary row placement is missing",
                ));
            }
            return Ok(BTreeMap::new());
        }
        let connection = self.open_row_origins_locked(cid)?;
        let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        let (costs, inheritance): (bool, bool) = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='row_costs'), EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='initial_inheritance')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if (version == 1 && costs)
            || (version >= 2 && !costs)
            || (version < 3 && inheritance)
            || (version >= 3 && !inheritance)
        {
            return Err(MezError::invalid_state(
                "ordinary row accounting schema conflicts with its version",
            ));
        }
        let mut select =
            connection.prepare_cached("SELECT origin FROM origins WHERE sequence=?1")?;
        let mut placed = BTreeMap::new();
        for (sequence, expected) in accounting.origins() {
            let current: Option<String> = select
                .query_row([sequence.to_string()], |row| row.get(0))
                .optional()?;
            if let Some(current) = current {
                let current = ContextBlockOccurrenceId::try_from(current)?;
                if current != *expected {
                    return Err(MezError::conflict(
                        "ordinary append occurrence is already owned",
                    ));
                }
                placed.insert(*sequence, current);
            }
        }
        if version >= 2 {
            self.transcript_row_records_locked(cid, accounting.origins())?;
        }
        Ok(placed)
    }

    /// Rejects overlapping owners before a journal can create recovery rights.
    pub(super) fn check_ordinary_accounting_admission_locked(
        &self,
        batch: &TranscriptAppendBatch,
        pending: &[StoredReceipt],
        committed: u64,
    ) -> Result<()> {
        let Some(accounting) = batch.row_accounting() else {
            return Ok(());
        };
        let placed = self.ordinary_accounting_state_locked(batch)?;
        let matching = pending.iter().any(|(_, receipt, rows)| {
            receipt.conversation_id == batch.conversation_id()
                && receipt.row_accounting.as_ref() == Some(accounting)
                && rows == batch.entries()
        });
        for (_, receipt, rows) in pending {
            if receipt.conversation_id != batch.conversation_id() {
                continue;
            }
            for row in rows {
                if let Some(expected) = accounting.origins().get(&row.sequence)
                    && receipt
                        .row_accounting
                        .as_ref()
                        .and_then(|owner| owner.origins().get(&row.sequence))
                        != Some(expected)
                {
                    return Err(MezError::conflict(
                        "pending append has a different occurrence owner",
                    ));
                }
            }
        }
        if !matching
            && (batch
                .entries()
                .iter()
                .any(|row| row.sequence <= committed && !placed.contains_key(&row.sequence))
                || placed.keys().any(|sequence| *sequence > committed))
        {
            return Err(MezError::conflict(
                "ordinary append requires original placement or its retained receipt",
            ));
        }
        Ok(())
    }

    /// Commits raw rows and their original placement before releasing the lease.
    /// Receipt publication precedes this operation, retaining partial failure.
    pub(super) fn reconcile_ordinary_accounting_locked(
        &self,
        batch: &TranscriptAppendBatch,
        lease: &File,
    ) -> Result<()> {
        let Some(accounting) = batch.row_accounting() else {
            return Ok(());
        };
        self.ordinary_accounting_state_locked(batch)?;
        self.ensure_custody_receipts_settled_locked(batch.conversation_id())?;
        #[cfg(test)]
        self.check_transcript_append_failure_for_tests()?;
        let rows = batch.entries().iter().collect::<Vec<_>>();
        self.append_conversation_rows_locked(batch.conversation_id(), &rows, lease)?;
        let existing =
            self.transcript_row_records_locked(batch.conversation_id(), accounting.origins())?;
        let mut connection = self.open_row_origins_locked(batch.conversation_id())?;
        let transaction = connection.transaction()?;
        for (sequence, origin) in accounting.origins() {
            transaction.execute(
                "INSERT INTO origins VALUES (?1, ?2) ON CONFLICT(sequence) DO NOTHING",
                params![sequence.to_string(), String::from(*origin)],
            )?;
        }
        for (sequence, record) in accounting.records() {
            if !existing.contains_key(sequence) {
                transaction.execute(
                    "INSERT INTO row_costs VALUES (?1, ?2)",
                    params![
                        String::from(record.occurrence_id()),
                        serde_json::to_string(record).map_err(|_| MezError::invalid_state(
                            "ordinary row cost encode failed"
                        ))?
                    ],
                )?;
            }
        }
        self.publish_row_digests_in_transaction(&transaction, batch)?;
        transaction.commit()?;
        self.require_row_origins_locked(batch.conversation_id())?;
        if batch.entries().iter().any(|entry| {
            entry.role == mez_agent::TranscriptRole::System
                && entry
                    .content
                    .starts_with(super::super::compaction_handoff::MARKER)
        }) {
            self.publish_terminal_compaction_handoffs_locked(batch.conversation_id())?;
        }
        Ok(())
    }

    /// Requires original placement and available proposal costs before receipt
    /// retirement; a valid later rendering cache is never rolled back.
    pub(in crate::storage::transcript) fn verify_ordinary_accounting_committed_locked(
        &self,
        batch: &TranscriptAppendBatch,
    ) -> Result<()> {
        let Some(accounting) = batch.row_accounting() else {
            return Ok(());
        };
        if self.ordinary_accounting_state_locked(batch)? != *accounting.origins()
            || !self.row_origins_required_locked(batch.conversation_id())?
        {
            return Err(MezError::invalid_state(
                "ordinary append occurrence publication is incomplete",
            ));
        }
        let costs =
            self.transcript_row_records_locked(batch.conversation_id(), accounting.origins())?;
        if accounting
            .records()
            .keys()
            .any(|sequence| !costs.contains_key(sequence))
        {
            return Err(MezError::invalid_state(
                "ordinary append cost publication is incomplete",
            ));
        }
        if self
            .indexed_row_origins_locked(batch.conversation_id(), batch.entries())?
            .is_some_and(|origins| origins != *accounting.origins())
        {
            return Err(MezError::invalid_state(
                "ordinary append fingerprint owner changed",
            ));
        }
        Ok(())
    }

    /// Checks every accounted proposal before recovery can mutate an unrelated
    /// archive, including equal-byte claims with conflicting original IDs.
    pub(super) fn preflight_ordinary_accounting_receipts<'a>(
        &self,
        receipts: &'a [StoredReceipt],
    ) -> Result<ReceiptRowClaims<'a>> {
        let claimed = ordinary_receipt_claims(receipts)?;
        for (_, receipt, _) in receipts {
            let batch = receipt.batch()?;
            if batch.row_accounting().is_some() {
                let _lease = self.acquire_conversation_lock(batch.conversation_id())?;
                self.ordinary_accounting_state_locked(&batch)?;
            }
        }
        Ok(claimed)
    }

    /// Writes one original ordinary proposal and retires its receipt while
    /// holding the same append lane across raw and placement publication.
    pub(crate) fn commit_accounted_transcript_batch(
        &self,
        batch: &TranscriptAppendBatch,
        generation: u64,
    ) -> Result<usize> {
        if batch.row_accounting().is_none() {
            return Err(MezError::invalid_args(
                "ordinary append is missing its occurrence proposal",
            ));
        }
        let receipt = AppendReceipt::from_batch(batch, generation)?;
        let _lease = self.acquire_conversation_lock(batch.conversation_id())?;
        let path = self.transcript_path(batch.conversation_id())?;
        let before = match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() => metadata.len(),
            Ok(_) => {
                return Err(MezError::invalid_state(
                    "ordinary transcript path is not a regular file",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error.into()),
        };
        self.accept_prepared_transcript_receipt_locked(batch, &receipt, &_lease)?;
        self.reconcile_ordinary_accounting_locked(batch, &_lease)?;
        self.settle_prepared_transcript_receipt_locked(batch, &receipt, &_lease)?;
        let after = std::fs::metadata(path)?.len();
        usize::try_from(after.checked_sub(before).ok_or_else(|| {
            MezError::invalid_state("ordinary transcript append shortened its source")
        })?)
        .map_err(|_| MezError::invalid_state("ordinary transcript byte count overflow"))
    }

    /// Repairs this conversation's unfinished ordinary accounting before
    /// canonical replay. Complete original accounting wholly inside a
    /// validated typed cut needs no replay repair; leave those receipts with the
    /// actual ordinary settlement owner rather than derive ACK from that cut.
    /// No candidate, native action or unrelated archive is published. An unsafe
    /// partial source remains a local explicit error.
    pub(in crate::storage::transcript) fn recover_accounted_appends_for_replay_locked(
        &self,
        cid: &str,
        lease: &File,
    ) -> Result<()> {
        let mut receipts = self.read_locked_append_receipts(&[cid])?;
        if !receipts
            .iter()
            .any(|(_, receipt, _)| receipt.row_accounting.is_some())
        {
            return Ok(());
        }
        let claimed = ordinary_receipt_claims(&receipts)?;
        let mut already_complete = receipts
            .iter()
            .all(|(_, receipt, _)| receipt.row_accounting.is_some());
        for (_, receipt, _) in &receipts {
            if receipt.row_accounting.is_some() {
                let batch = receipt.batch()?;
                self.ordinary_accounting_state_locked(&batch)?;
                already_complete &= self.replay_cut_covers_complete_accounting_locked(&batch)?;
            }
        }
        if already_complete {
            // Every retained claim was preflighted. The cut permits model I/O
            // omission, not physical receipt settlement or a synthetic ACK.
            // Actual off-actor settlement still validates exact archive bytes.
            return Ok(());
        }
        match self.inspect(cid) {
            Ok(rows) => {
                self.validate_restored_transcript(cid)?;
                if rows.iter().any(|row| {
                    claimed
                        .get(&(cid, row.sequence))
                        .is_some_and(|expected| **expected != *row)
                }) {
                    return Err(MezError::conflict(
                        "replay recovery receipt conflicts with durable source",
                    ));
                }
            }
            Err(error) if error.kind() == crate::MezErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        receipts.sort_by_key(|(_, receipt, _)| (receipt.first_sequence, receipt.generation));
        for (_, receipt, _) in receipts {
            if receipt.row_accounting.is_some() {
                let batch = receipt.batch()?;
                self.reconcile_ordinary_accounting_locked(&batch, lease)?;
                self.settle_prepared_transcript_receipt_locked(&batch, &receipt, lease)?;
            }
        }
        Ok(())
    }
}
