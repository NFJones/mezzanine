//! Full-field row fingerprints in the original append accounting transaction.
//!
//! These are selected-row lookup metadata, not source payloads, an append
//! journal, a replay cursor or a settlement capability. The bounded reader must
//! separately qualify its manifest-bound cut before using them to omit old I/O.
//! Legacy absence yields no proof; malformed or conflicting present state is an
//! error. Actual edit/resequence owners invalidate obsolete fingerprints.

use super::{AgentTranscriptStore, FILE_NAME, TranscriptRowOrigins};
use crate::{MezError, Result};
use mez_agent::{ContextBlockOccurrenceId, TranscriptEntry};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

/// Descriptive exact index reference, bound to a separately qualified archive cut.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::storage::transcript) struct IndexedRowImage {
    pub(in crate::storage::transcript) sequence: u64,
    pub(in crate::storage::transcript) origin: ContextBlockOccurrenceId,
    pub(in crate::storage::transcript) digest: String,
}

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
        let origins = if let Some(accounting) = batch.row_accounting() {
            accounting.validate(batch.entries())?;
            accounting.origins()
        } else if let Some(accounting) = batch.initial_row_accounting() {
            accounting.validate(batch.entries())?;
            accounting.origins()
        } else {
            return Err(invalid("missing original append accounting"));
        };
        self.publish_owned_row_images_in_transaction(
            transaction,
            &batch.entries().iter().collect::<Vec<_>>(),
            origins,
        )
    }

    /// The caller owns either an accepted append receipt or the existing saved
    /// fork generation with independently checked original row fingerprints.
    /// This shared SQL primitive never creates that ownership or an append ACK.
    fn publish_owned_row_images_in_transaction(
        &self,
        transaction: &Transaction<'_>,
        rows: &[&TranscriptEntry],
        origins: &TranscriptRowOrigins,
    ) -> Result<()> {
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
        for row in rows {
            let origin = origins
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
        transaction.execute_batch(
            "CREATE INDEX IF NOT EXISTS row_digests_origin ON row_digests(origin);
             CREATE INDEX IF NOT EXISTS origins_origin ON origins(origin);",
        )?;
        self.publish_execution_sources_in_transaction(transaction, rows)?;
        Ok(())
    }

    /// Rebinds only genuine source images to the fork generation's actual
    /// copied bytes. Source and target leases, original fingerprint validation
    /// and generation-safe rollback belong to the enclosing fork transaction.
    /// Missing legacy images remain absent; no original append is acknowledged.
    pub(in crate::storage::transcript) fn install_copied_row_images_locked(
        &self,
        conversation: &str,
        rows: &[TranscriptEntry],
        originals: &TranscriptRowOrigins,
    ) -> Result<()> {
        if rows.iter().any(|row| row.conversation_id != conversation) {
            return Err(invalid("copied row image has a foreign target"));
        }
        let selected = rows
            .iter()
            .filter(|row| originals.contains_key(&row.sequence))
            .collect::<Vec<_>>();
        if selected.len() != originals.len() {
            return Err(invalid("copied row image lost its original placement"));
        }
        if selected.is_empty() {
            return Ok(());
        }
        let mut connection = self.open_row_origins_locked(conversation)?;
        let transaction = connection.transaction()?;
        self.publish_owned_row_images_in_transaction(&transaction, &selected, originals)?;
        transaction.commit()?;
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
        let found = self.available_indexed_row_origins_locked(conversation, rows)?;
        Ok((found.len() == rows.len()).then_some(found))
    }

    /// Returns only actually indexed selected rows, checking every present claim.
    /// A caller may advance only over the contiguous proven prefix of its own
    /// already validated row window; missing entries do not create membership.
    pub(in crate::storage::transcript) fn available_indexed_row_origins_locked(
        &self,
        conversation: &str,
        rows: &[TranscriptEntry],
    ) -> Result<TranscriptRowOrigins> {
        mez_agent::transcript::validate_conversation_id(conversation)?;
        if rows.is_empty() {
            return Ok(TranscriptRowOrigins::new());
        }
        if !self.root.join(conversation).join(FILE_NAME).exists() {
            if self.row_origins_required_locked(conversation)? {
                return Err(invalid("required occurrence database is missing"));
            }
            return Ok(TranscriptRowOrigins::new());
        }
        let connection = self.open_row_origins_locked(conversation)?;
        let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version < 4 {
            return Ok(TranscriptRowOrigins::new());
        }
        let mut found = TranscriptRowOrigins::new();
        for row in rows {
            if row.conversation_id != conversation {
                return Err(invalid("foreign selected row"));
            }
            let digest = self.transcript_entry_revision(row)?;
            if let Some((origin, expected)) = indexed_row(&connection, row.sequence)? {
                if super::authored_origin(row)?.is_some_and(|authored| authored != origin) {
                    return Err(invalid("indexed image changed its authored occurrence"));
                }
                if digest != expected {
                    return Err(invalid("selected row image changed"));
                }
                if found.insert(row.sequence, origin).is_some() {
                    return Err(invalid("duplicate selected row"));
                }
            }
        }
        Ok(found)
    }

    /// Builds only the reconstructible SQL access path during a publication
    /// operation, without inventing or migrating any row-image source claims.
    pub(in crate::storage::transcript) fn ensure_row_digest_lookup_index_locked(
        &self,
        conversation: &str,
    ) -> Result<()> {
        if !self.root.join(conversation).join(FILE_NAME).exists() {
            return Ok(());
        }
        let connection = self.open_row_origins_locked(conversation)?;
        let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version >= 4 {
            connection.execute_batch(
                "CREATE INDEX IF NOT EXISTS row_digests_origin ON row_digests(origin);
                 CREATE INDEX IF NOT EXISTS origins_origin ON origins(origin);",
            )?;
        }
        Ok(())
    }

    /// Resolves one required row reference without decoding earlier archive data.
    pub(in crate::storage::transcript) fn indexed_row_reference_locked(
        &self,
        conversation: &str,
        sequence: u64,
    ) -> Result<Option<IndexedRowImage>> {
        mez_agent::transcript::validate_conversation_id(conversation)?;
        if !self.root.join(conversation).join(FILE_NAME).exists() {
            return Err(invalid("required image database is missing"));
        }
        let connection = self.open_row_origins_locked(conversation)?;
        let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version < 4 {
            return Ok(None);
        }
        Ok(
            indexed_row(&connection, sequence)?.map(|(origin, digest)| IndexedRowImage {
                sequence,
                origin,
                digest,
            }),
        )
    }

    /// Finds original indexed placement for duplicate/retirement qualification.
    /// Multiple placements of one occurrence are corruption, never permission to
    /// choose whichever row happens to fit a later replay window. A surviving
    /// original placement within the cut requires its image even when that image
    /// is missing from the lookup table. Later unindexed placement is no proof.
    pub(in crate::storage::transcript) fn indexed_occurrence_reference_locked(
        &self,
        conversation: &str,
        origin: ContextBlockOccurrenceId,
        required_through: u64,
    ) -> Result<Option<IndexedRowImage>> {
        mez_agent::transcript::validate_conversation_id(conversation)?;
        if !self.root.join(conversation).join(FILE_NAME).exists() {
            return Err(invalid("required image database is missing"));
        }
        let connection = self.open_row_origins_locked(conversation)?;
        let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version < 4 {
            return Ok(None);
        }
        let mut select = connection.prepare_cached(
            "SELECT sequence FROM row_digests WHERE origin=?1
             UNION SELECT sequence FROM origins WHERE origin=?1 LIMIT 2",
        )?;
        let sequences = select
            .query_map([String::from(origin)], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if sequences.len() > 1 {
            return Err(invalid("one occurrence has multiple original placements"));
        }
        Ok(sequences
            .first()
            .map(|sequence| {
                let parsed = sequence
                    .parse::<u64>()
                    .map_err(|_| invalid("invalid indexed sequence"))?;
                if parsed == 0 || parsed.to_string() != *sequence {
                    return Err(invalid("noncanonical original sequence"));
                }
                let Some((actual, digest)) = indexed_row(&connection, parsed)? else {
                    if parsed <= required_through {
                        return Err(invalid("required archived occurrence image disappeared"));
                    }
                    return Ok(None);
                };
                if actual != origin {
                    return Err(invalid("indexed occurrence changed"));
                }
                Ok(Some(IndexedRowImage {
                    sequence: parsed,
                    origin,
                    digest,
                }))
            })
            .transpose()?
            .flatten())
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
        if version >= 5 {
            if let Some(sequence) = sequence {
                connection.execute(
                    "DELETE FROM execution_sources WHERE sequence=?1",
                    [sequence.to_string()],
                )?;
            } else {
                connection.execute("DELETE FROM execution_sources", [])?;
            }
        }
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
pub(super) fn indexed_row(
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
