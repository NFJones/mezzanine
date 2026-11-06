//! Full-field row fingerprints in the original append accounting transaction.
//!
//! These are selected-row lookup metadata, not source payloads, an append
//! journal, a replay cursor or a settlement capability. A future bounded reader
//! must separately qualify its archive cut before using them to omit old I/O.
//! Legacy absence yields no proof; malformed or conflicting present state is an
//! error. Actual edit/resequence owners invalidate obsolete fingerprints.

use super::{AgentTranscriptStore, FILE_NAME, TranscriptRowOrigins};
use crate::{MezError, Result};
use mez_agent::{ContextBlockOccurrenceId, TranscriptEntry};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

impl AgentTranscriptStore {
    /// Writes full-row images with the same original occurrence/cost transaction.
    /// The caller has reconciled the accepted receipt's raw rows under the held
    /// conversation lease. Existing index claims must agree; any error rolls back
    /// this transaction and leaves the original receipt available for recovery.
    pub(in crate::storage::transcript) fn publish_row_digests_in_transaction(
        &self,
        transaction: &Transaction<'_>,
        batch: &super::super::TranscriptAppendBatch,
    ) -> Result<()> {
        let accounting = batch
            .row_accounting()
            .ok_or_else(|| invalid("missing original append accounting"))?;
        accounting.validate(batch.entries())?;
        let version: u32 = transaction.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version < 4 {
            if version == 3 {
                let initial: bool = transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM initial_inheritance WHERE singleton=1)",
                    [],
                    |row| row.get(0),
                )?;
                if !initial {
                    return Err(invalid(
                        "required inherited accounting is missing before migration",
                    ));
                }
            }
            if version == 1 {
                transaction.execute_batch(
                    "CREATE TABLE row_costs (origin TEXT PRIMARY KEY, record TEXT NOT NULL);",
                )?;
            }
            if version < 3 {
                transaction.execute_batch("CREATE TABLE initial_inheritance (singleton INTEGER PRIMARY KEY CHECK(singleton=1), payload TEXT NOT NULL);")?;
            }
            transaction.execute_batch(
                "CREATE TABLE row_digests (sequence TEXT PRIMARY KEY, origin TEXT NOT NULL, digest TEXT NOT NULL);
                 CREATE TABLE row_digest_meta (singleton INTEGER PRIMARY KEY CHECK(singleton=1), initial_required INTEGER NOT NULL CHECK(initial_required IN (0, 1)));
                 PRAGMA user_version=4;",
            )?;
            transaction.execute(
                "INSERT INTO row_digest_meta VALUES (1, ?1)",
                [u32::from(version == 3)],
            )?;
        }
        for row in batch.entries() {
            let origin = accounting
                .origins()
                .get(&row.sequence)
                .ok_or_else(|| invalid("missing original row occurrence"))?;
            let placed: String = transaction.query_row(
                "SELECT origin FROM origins WHERE sequence=?1",
                [row.sequence.to_string()],
                |value| value.get(0),
            )?;
            if ContextBlockOccurrenceId::try_from(placed)? != *origin {
                return Err(invalid("fingerprint publication changed row occurrence"));
            }
            let digest = self.transcript_entry_revision(row)?;
            if indexed_row(transaction, row.sequence)?
                .is_some_and(|previous| previous != (*origin, digest.clone()))
            {
                return Err(invalid(
                    "fingerprint publication conflicts with original row",
                ));
            }
            transaction.execute(
                "INSERT INTO row_digests VALUES (?1, ?2, ?3) ON CONFLICT(sequence) DO NOTHING",
                params![row.sequence.to_string(), String::from(*origin), digest],
            )?;
        }
        Ok(())
    }

    /// Checks only supplied complete row images and returns their original IDs.
    /// The caller holds this conversation's lease. Legacy/missing index entries
    /// yield `None`, never guessed proof or automatic migration; every present
    /// entry is still validated so an earlier miss cannot hide later corruption.
    /// This lookup does not read the archive, prove a cursor or acknowledge an
    /// append. Its result is usable only with independently checked source.
    pub(in crate::storage::transcript) fn indexed_row_origins_locked(
        &self,
        conversation: &str,
        rows: &[TranscriptEntry],
    ) -> Result<Option<TranscriptRowOrigins>> {
        mez_agent::transcript::validate_conversation_id(conversation)?;
        if rows.is_empty() {
            return Ok(Some(TranscriptRowOrigins::new()));
        }
        if !self.root.join(conversation).join(FILE_NAME).exists() {
            if self.row_origins_required_locked(conversation)? {
                return Err(invalid("required occurrence database is missing"));
            }
            return Ok(None);
        }
        let connection = self.open_row_origins_locked(conversation)?;
        let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version < 4 {
            return Ok(None);
        }
        let mut found = TranscriptRowOrigins::new();
        let mut complete = true;
        for row in rows {
            if row.conversation_id != conversation {
                return Err(invalid("foreign selected row"));
            }
            let digest = self.transcript_entry_revision(row)?;
            match indexed_row(&connection, row.sequence)? {
                Some((origin, expected)) => {
                    if digest != expected {
                        return Err(invalid("selected row image changed"));
                    }
                    if found.insert(row.sequence, origin).is_some() {
                        return Err(invalid("duplicate selected row"));
                    }
                }
                None => complete = false,
            }
        }
        Ok(complete.then_some(found))
    }
}

/// Invalidates descriptive images only under an actual edit/copy/resequence
/// transaction. An absent old-schema index remains absent; this never creates
/// fresh source proof or adopts changed bytes as an append commitment.
pub(in crate::storage::transcript) fn forget_row_digests(
    connection: &Connection,
    sequence: Option<u64>,
) -> Result<()> {
    let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version >= 4 {
        if let Some(sequence) = sequence {
            connection.execute(
                "DELETE FROM row_digests WHERE sequence=?1",
                [sequence.to_string()],
            )?;
        } else {
            connection.execute("DELETE FROM row_digests", [])?;
        }
    }
    Ok(())
}

/// An index row must still describe the actual occurrence placement. Canonical
/// lower-case SHA-256 framing is checked before comparison or future selection.
fn indexed_row(
    connection: &Connection,
    sequence: u64,
) -> Result<Option<(ContextBlockOccurrenceId, String)>> {
    let value: Option<(String, String, Option<String>)> = connection.query_row(
        "SELECT i.origin, i.digest, o.origin FROM row_digests i LEFT JOIN origins o ON o.sequence=i.sequence WHERE i.sequence=?1",
        [sequence.to_string()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).optional()?;
    value
        .map(|(origin, digest, placed)| {
            let origin = ContextBlockOccurrenceId::try_from(origin)?;
            if placed.as_deref() != Some(String::from(origin).as_str())
                || digest.len() != 64
                || !digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(invalid("malformed fingerprint or occurrence placement"));
            }
            Ok((origin, digest))
        })
        .transpose()
}

fn invalid(reason: &str) -> MezError {
    MezError::invalid_state(format!("transcript row fingerprint: {reason}"))
}
