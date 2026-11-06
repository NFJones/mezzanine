//! Durable placement of legacy raw-row occurrences, separate from source bytes.
//!
//! Unaccounted rows initially import from their original conversation/sequence.
//! Saved copies and resequencing persist that origin instead of deriving it from
//! the new position. The conversation mutation lease owns this SQLite projection;
//! it is user state, not a rebuildable catalog. Missing required state fails the
//! affected operation. Typed context records retain their own authoritative ID.
//!
//! A resequence writes its old/new byte digests and proposed mapping before the
//! raw-file rename. Recovery chooses only an exact old or new image, then retires
//! the intent transactionally. No text matching or guessed row alignment is used.

use super::{AgentTranscriptStore, encoding::encode_transcript_entry};
use crate::{MezError, Result};
use mez_agent::{
    ContextBlockOccurrenceId, TranscriptEntry, TranscriptRole, transcript::TranscriptContextRecord,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, io::Read, path::Path};

mod costs;
pub(crate) use costs::TranscriptRowRecords;
mod images;
pub(super) use images::forget_row_digests;

/// Exact archive sequence to original legacy occurrence for one frozen read.
pub(crate) type TranscriptRowOrigins = BTreeMap<u64, ContextBlockOccurrenceId>;

/// Private, versioned conversation payload; archives must retain it with history.
const FILE_NAME: &str = "row-origins.sqlite3";

impl AgentTranscriptStore {
    /// Establishes the namespace before acknowledging a new raw producer. A
    /// later saved fork can then read the source without changing its payloads.
    pub(super) fn ensure_row_origins_locked(&self, conversation: &str) -> Result<()> {
        self.open_row_origins_locked(conversation)?;
        self.require_row_origins_locked(conversation)?;
        Ok(())
    }

    /// Opens identity state under the caller's mutation lease. Validation alone
    /// never migrates metadata: unrelated admission/rollback may still fail.
    /// Callers exposing IDs or acknowledging raw source explicitly require the
    /// marker after the database is durable; interrupted marker writes retry it.
    pub(super) fn open_row_origins_locked(&self, conversation: &str) -> Result<Connection> {
        let directory = self.row_origins_directory_locked(conversation)?;
        let path = directory.join(FILE_NAME);
        if !path.exists() {
            if self.row_origins_required_locked(conversation)? {
                return Err(MezError::invalid_state(
                    "required transcript row origins are missing",
                ));
            }
            let temporary = directory.join(".row-origins.sqlite3.new");
            if temporary.exists() {
                fs::remove_file(&temporary)?;
            }
            fs::File::create(&temporary)?;
            super::fs::set_private_file_permissions(&temporary)?;
            let connection = Connection::open(&temporary)?;
            connection.execute_batch(
                "PRAGMA synchronous=FULL;
                 BEGIN IMMEDIATE;
                 PRAGMA user_version=2;
                 CREATE TABLE origin_meta (singleton INTEGER PRIMARY KEY CHECK(singleton=1), namespace TEXT NOT NULL);
                 CREATE TABLE origins (sequence TEXT PRIMARY KEY, origin TEXT NOT NULL);
                 CREATE TABLE row_costs (origin TEXT PRIMARY KEY, record TEXT NOT NULL);
                 CREATE TABLE rewrite_intent (singleton INTEGER PRIMARY KEY CHECK(singleton=1), old_digest BLOB NOT NULL, new_digest BLOB NOT NULL, namespace TEXT NOT NULL);
                 CREATE TABLE rewrite_origins (sequence TEXT PRIMARY KEY, origin TEXT NOT NULL);",
            )?;
            connection.execute("INSERT INTO origin_meta VALUES (1, ?1)", [conversation])?;
            connection.execute_batch("COMMIT;")?;
            drop(connection);
            fs::rename(&temporary, &path)?;
            fs::File::open(&directory)?.sync_all()?;
        }
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if !matches!(version, 1..=4) {
            return Err(MezError::invalid_state(
                "unsupported transcript row origins version",
            ));
        }
        let (images, image_meta): (bool, bool) = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='row_digests'), EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='row_digest_meta')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if images != (version == 4) || image_meta != (version == 4) {
            return Err(MezError::invalid_state(
                "transcript row fingerprint schema conflicts with its version",
            ));
        }
        if version == 4 {
            let required: u32 = connection.query_row(
                "SELECT initial_required FROM row_digest_meta WHERE singleton=1",
                [],
                |row| row.get(0),
            )?;
            let inherited: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM initial_inheritance WHERE singleton=1)",
                [],
                |row| row.get(0),
            )?;
            if required > 1 || inherited != (required == 1) {
                return Err(MezError::invalid_state(
                    "transcript inherited accounting requirement is inconsistent",
                ));
            }
        }
        connection.execute_batch("PRAGMA synchronous=FULL;")?;
        let namespace: String = connection.query_row(
            "SELECT namespace FROM origin_meta WHERE singleton=1",
            [],
            |row| row.get(0),
        )?;
        ContextBlockOccurrenceId::for_legacy_archive_row(&namespace, 1, 0)?;
        Ok(connection)
    }

    /// Resolves only the checked logical window. Unmapped rows use the durable
    /// current namespace; persisted overrides describe survivors and saved copies.
    pub(super) fn transcript_row_origins_locked(
        &self,
        conversation: &str,
        rows: &[TranscriptEntry],
    ) -> Result<TranscriptRowOrigins> {
        if rows.is_empty() {
            return Ok(BTreeMap::new());
        }
        let connection = self.open_row_origins_locked(conversation)?;
        let namespace: String = connection.query_row(
            "SELECT namespace FROM origin_meta WHERE singleton=1",
            [],
            |row| row.get(0),
        )?;
        let mut select =
            connection.prepare_cached("SELECT origin FROM origins WHERE sequence=?1")?;
        let mut origins = BTreeMap::new();
        for row in rows {
            if row.conversation_id != conversation {
                return Err(MezError::invalid_state(
                    "transcript row origin conversation mismatch",
                ));
            }
            let stored: Option<String> = select
                .query_row([row.sequence.to_string()], |value| value.get(0))
                .optional()?;
            let origin = match (stored, authored_origin(row)?) {
                (Some(origin), authored) => {
                    let origin = ContextBlockOccurrenceId::try_from(origin)?;
                    if authored.is_some_and(|authored| authored != origin) {
                        return Err(MezError::conflict("transcript authored row origin changed"));
                    }
                    origin
                }
                (None, Some(origin)) => origin,
                (None, None) => {
                    ContextBlockOccurrenceId::for_legacy_archive_row(&namespace, row.sequence, 0)?
                }
            };
            origins.insert(row.sequence, origin);
        }
        self.require_row_origins_locked(conversation)?;
        Ok(origins)
    }

    /// A covered carrier still belongs to its original authored occurrence.
    /// Validate the selected claim even when no carrier row is rendered; never
    /// manufacture an entry to reuse the ordinary projection API.
    pub(super) fn validate_stable_carrier_origin_locked(
        &self,
        conversation: &str,
        sequence: u64,
        expected: ContextBlockOccurrenceId,
    ) -> Result<()> {
        let connection = self.open_row_origins_locked(conversation)?;
        let stored: Option<String> = connection
            .query_row(
                "SELECT origin FROM origins WHERE sequence=?1",
                [sequence.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(stored) = stored
            && ContextBlockOccurrenceId::try_from(stored)? != expected
        {
            return Err(MezError::conflict("stable snapshot carrier origin changed"));
        }
        self.require_row_origins_locked(conversation)
    }

    /// Pins the original occurrence before a semantic edit replaces its source.
    /// Losing/revising a cache cannot lose identity, and a replacement typed
    /// record cannot claim a different occurrence at this existing row position.
    pub(super) fn preserve_edited_row_origin_locked(
        &self,
        original: &TranscriptEntry,
        updated: &TranscriptEntry,
    ) -> Result<()> {
        let origins = self.transcript_row_origins_locked(
            &original.conversation_id,
            std::slice::from_ref(original),
        )?;
        let origin = origins
            .get(&original.sequence)
            .copied()
            .ok_or_else(|| MezError::invalid_state("edited transcript row origin is missing"))?;
        if authored_origin(updated)?.is_some_and(|authored| authored != origin) {
            return Err(MezError::conflict(
                "transcript edit cannot replace row occurrence",
            ));
        }
        let mut connection = self.open_row_origins_locked(&original.conversation_id)?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO origins VALUES (?1, ?2) ON CONFLICT(sequence) DO UPDATE SET origin=excluded.origin",
            params![original.sequence.to_string(), String::from(origin)],
        )?;
        forget_row_digests(&transaction, Some(original.sequence))?;
        transaction.commit()?;
        Ok(())
    }

    /// Installs a complete inherited mapping while the unpublished fork target
    /// remains under its existing compensation owner. Future rows use a fresh
    /// namespace, so they cannot reuse an inherited or deleted occurrence.
    pub(super) fn install_fork_row_origins_locked(
        &self,
        conversation: &str,
        origins: &TranscriptRowOrigins,
    ) -> Result<()> {
        let mut connection = self.open_row_origins_locked(conversation)?;
        let transaction = connection.transaction()?;
        transaction.execute("DELETE FROM origins", [])?;
        forget_row_digests(&transaction, None)?;
        insert_origins(&transaction, "INSERT INTO origins VALUES (?1, ?2)", origins)?;
        transaction.execute(
            "UPDATE origin_meta SET namespace=?1 WHERE singleton=1",
            [String::from(ContextBlockOccurrenceId::allocate())],
        )?;
        transaction.commit()?;
        self.require_row_origins_locked(conversation)?;
        Ok(())
    }

    /// Stages an exact resequence alongside the already synced raw temporary
    /// image. A failure leaves an intent recoverable under the next owner lease.
    pub(super) fn prepare_row_origins_rewrite_locked(
        &self,
        conversation: &str,
        old_path: &Path,
        rows: &[TranscriptEntry],
        origins: &TranscriptRowOrigins,
    ) -> Result<()> {
        if rows.len() != origins.len()
            || rows.iter().any(|row| !origins.contains_key(&row.sequence))
        {
            return Err(MezError::invalid_state(
                "incomplete transcript resequence origins",
            ));
        }
        let mut digest = Sha256::new();
        for row in rows {
            digest.update(encode_transcript_entry(row)?.as_bytes());
            digest.update(b"\n");
        }
        let new_digest = digest.finalize().to_vec();
        let old_digest = file_digest(old_path)?;
        let mut connection = self.open_row_origins_locked(conversation)?;
        let transaction = connection.transaction()?;
        transaction.execute("DELETE FROM rewrite_origins", [])?;
        insert_origins(
            &transaction,
            "INSERT INTO rewrite_origins VALUES (?1, ?2)",
            origins,
        )?;
        transaction.execute(
            "INSERT INTO rewrite_intent VALUES (1, ?1, ?2, ?3)",
            params![
                old_digest,
                new_digest,
                String::from(ContextBlockOccurrenceId::allocate())
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Reconciles an interrupted rename without creating identity state for an
    /// untouched conversation. Unexpected images refuse only this conversation;
    /// they cannot be repaired by reassigning IDs or silently deleting the intent.
    pub(super) fn recover_row_origins_rewrite_locked(&self, conversation: &str) -> Result<()> {
        let path = self.root.join(conversation).join(FILE_NAME);
        if !path.exists() {
            if self.row_origins_required_locked(conversation)? {
                return Err(MezError::invalid_state(
                    "required transcript row origins are missing",
                ));
            }
            return Ok(());
        }
        let mut connection = self.open_row_origins_locked(conversation)?;
        let intent: Option<(Vec<u8>, Vec<u8>, String)> = connection
            .query_row(
                "SELECT old_digest, new_digest, namespace FROM rewrite_intent WHERE singleton=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((old, new, namespace)) = intent else {
            return Ok(());
        };
        ContextBlockOccurrenceId::for_legacy_archive_row(&namespace, 1, 0)?;
        let actual = file_digest(&self.existing_transcript_path_for(conversation)?)?;
        if actual != old && actual != new {
            return Err(MezError::conflict(
                "transcript row origins rewrite source changed",
            ));
        }
        let transaction = connection.transaction()?;
        if actual == new {
            transaction.execute_batch(
                "DELETE FROM origins; INSERT INTO origins SELECT * FROM rewrite_origins;",
            )?;
            transaction.execute(
                "UPDATE origin_meta SET namespace=?1 WHERE singleton=1",
                [namespace],
            )?;
            let version: u32 =
                transaction.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            if matches!(version, 2..=4) {
                transaction.execute(
                    "DELETE FROM row_costs WHERE origin NOT IN (SELECT origin FROM origins)",
                    [],
                )?;
            }
            forget_row_digests(&transaction, None)?;
        }
        transaction.execute_batch("DELETE FROM rewrite_origins; DELETE FROM rewrite_intent;")?;
        transaction.commit()?;
        Ok(())
    }
}

/// Writes structurally indexed IDs without an integer narrowing or text match.
fn insert_origins(
    connection: &Connection,
    sql: &str,
    origins: &TranscriptRowOrigins,
) -> Result<()> {
    let mut insert = connection.prepare_cached(sql)?;
    for (sequence, origin) in origins {
        insert.execute(params![sequence.to_string(), String::from(*origin)])?;
    }
    Ok(())
}

/// Only actual system records own typed metadata. A literal user marker remains
/// ordinary source and cannot replace the row's imported identity or role.
fn authored_origin(row: &TranscriptEntry) -> Result<Option<ContextBlockOccurrenceId>> {
    if row.role != TranscriptRole::System {
        return Ok(None);
    }
    Ok(
        TranscriptContextRecord::from_transcript_content(&row.content)?
            .map(|record| record.record().occurrence_id()),
    )
}

/// Hashes exact physical bytes only for a mutation/recovery proof, not on every
/// replay or as a canonical occurrence/content-addressed identity.
fn file_digest(path: &Path) -> Result<Vec<u8>> {
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let size = file.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        digest.update(&buffer[..size]);
    }
    Ok(digest.finalize().to_vec())
}

#[cfg(test)]
mod tests;
