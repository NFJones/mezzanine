//! Transactional disk projection of the existing MMP custody owner.
//!
//! This adapter persists opaque canonical records and exact batch receipts; it
//! never resolves recipients, acknowledges mail, or creates a second mailbox.
//! Calls are synchronous and must run in the owned blocking worker. Runtime
//! acceptance integration is a separate step and is not provided by this module.
//!
//! Each transaction compares a store generation and every replaced row digest.
//! The batch digest and new generation commit with the rows, permitting exact
//! reconciliation after a lost reply. Only the most recent unacknowledged batch
//! can be reconciled: the actor must not submit later work before settling it.
//! DELETE journaling and EXTRA synchronization avoid unbounded WAL growth.
//! Database pages and logical rows are bounded independently; the eventual
//! runtime must additionally reserve all inflight/recipient/cache resources and
//! qualify total physical footprint. No hardware power-loss guarantee is made.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use crate::error::{MezError, Result};

pub(crate) mod acceptance;
pub(crate) mod acknowledgement;
mod inventory;
pub(crate) mod ownership;

/// Version of this private row/receipt store, independent of message snapshots.
const SCHEMA_VERSION: i64 = 1;
/// Maximum records in one off-actor transaction.
const MAX_BATCH_RECORDS: usize = 64;
/// Maximum measured key/preimage/value bytes plus framing in one batch.
const MAX_BATCH_BYTES: usize = 256 * 1024;
/// Explicit row framing charge, separate from actual SQLite page allocation.
const ROW_METADATA_BYTES: usize = 64;

/// Trusted storage policy; production ceilings may only be narrowed by callers.
#[derive(Clone, Copy)]
pub(crate) struct CustodyStoreLimits {
    /// Canonical stored rows; recipient obligations are owned by the caller.
    pub records: usize,
    /// Canonical key, value, checksum and framing bytes, including metadata.
    pub logical_bytes: usize,
    /// Independent 4096-byte main database page ceiling, not a sidecar bound.
    pub database_pages: usize,
}

impl Default for CustodyStoreLimits {
    /// Supplies finite store ceilings without enlarging lower memory custody.
    fn default() -> Self {
        Self {
            records: 10_000,
            logical_bytes: 64 * 1024 * 1024,
            database_pages: 16_384,
        }
    }
}

/// One generation-fenced batch of canonical row replacements or removals.
#[derive(Debug)]
pub(crate) struct CustodyWrite<'a> {
    /// Canonical occurrence key, not a filesystem path.
    pub key: &'a str,
    /// Expected prior SHA256; absence requires a previously absent key.
    pub previous: Option<&'a [u8]>,
    /// New canonical bytes; absence removes an exactly matched settled row.
    pub value: Option<&'a [u8]>,
}

/// Exact committed batch evidence, including idempotent lost-reply recovery.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CustodyCommit {
    /// Monotonic store generation after the batch committed.
    generation: u64,
    /// Digest of the complete original batch, including expected preimages.
    digest: Vec<u8>,
    /// Persisted random allocation identity; equal session text is insufficient.
    store_id: Vec<u8>,
}

impl CustodyCommit {
    /// Returns the monotonic generation bound to this exact committed batch.
    pub fn generation(&self) -> u64 {
        self.generation
    }
    /// Returns the persisted allocation identity, independent of owner labels.
    pub fn store_id(&self) -> &[u8] {
        &self.store_id
    }
    /// Returns the digest of all rows, expected preimages and batch order.
    pub fn digest(&self) -> &[u8] {
        &self.digest
    }
}

/// Private session store; only the runtime custody owner may construct batches.
pub(crate) struct MmpCustodyStore {
    /// Sole connection owned by a serialized blocking-worker pipeline.
    connection: Connection,
    /// Destination policy is never loaded from untrusted message data.
    limits: CustodyStoreLimits,
    /// Random private allocation identity loaded from the same transaction.
    store_id: Vec<u8>,
}

impl MmpCustodyStore {
    /// Opens a private store for an exact trusted session owner.
    /// Existing different-owner, future-schema, symlink or malformed stores fail
    /// closed. This function never chooses an owner from envelope content.
    pub fn open(root: &Path, owner: &str) -> Result<Self> {
        Self::open_with_limits(root, owner, CustodyStoreLimits::default())
    }

    /// Opens under narrowed trusted row/page budgets, rejecting over-cap state.
    /// Filesystem permissions follow the existing private SQLite conventions;
    /// same-user concurrent path replacement is not a filesystem sandbox.
    pub fn open_with_limits(root: &Path, owner: &str, limits: CustodyStoreLimits) -> Result<Self> {
        validate_identifier(owner)?;
        let ceiling = CustodyStoreLimits::default();
        if limits.records == 0
            || limits.records > ceiling.records
            || limits.logical_bytes == 0
            || limits.logical_bytes > ceiling.logical_bytes
            || limits.database_pages < 4
            || limits.database_pages > ceiling.database_pages
        {
            return Err(MezError::invalid_args("invalid MMP custody storage policy"));
        }
        if !root.exists() {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true).mode(0o700);
            builder.create(root).map_err(MezError::from)?;
        }
        validate_private_path(root, true)?;
        let path = root.join("custody.sqlite");
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.into()),
        }
        validate_private_path(&path, false)?;
        for suffix in ["-journal", "-wal", "-shm"] {
            let candidate = root.join(format!("custody.sqlite{suffix}"));
            match fs::symlink_metadata(&candidate) {
                Ok(_) => {
                    validate_private_path(&candidate, false)?;
                    if suffix != "-journal" {
                        return Err(MezError::invalid_state(
                            "unexpected MMP custody WAL sidecar",
                        ));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
        let mut connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(database_error)?;
        connection
            .busy_timeout(Duration::from_secs(1))
            .map_err(database_error)?;
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(database_error)?;
        if version != 0 && version != SCHEMA_VERSION {
            return Err(MezError::invalid_state(
                "unsupported MMP custody store schema",
            ));
        }
        connection.execute_batch("PRAGMA page_size=4096; PRAGMA journal_mode=DELETE; PRAGMA synchronous=EXTRA; PRAGMA temp_store=MEMORY; PRAGMA cache_spill=OFF; PRAGMA cache_size=-1024;")
            .map_err(database_error)?;
        let page_size: i64 = connection
            .query_row("PRAGMA page_size", [], |r| r.get(0))
            .map_err(database_error)?;
        let pages: i64 = connection
            .query_row("PRAGMA page_count", [], |r| r.get(0))
            .map_err(database_error)?;
        if page_size != 4096 || pages > limits.database_pages as i64 {
            return Err(MezError::invalid_state(
                "MMP custody database exceeds page policy",
            ));
        }
        let configured: i64 = connection
            .query_row(
                &format!("PRAGMA max_page_count={}", limits.database_pages),
                [],
                |r| r.get(0),
            )
            .map_err(database_error)?;
        if configured != limits.database_pages as i64 {
            return Err(MezError::invalid_state(
                "MMP custody page cap was not installed",
            ));
        }
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;
        let current_version: i64 = transaction
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(database_error)?;
        if current_version == 0 {
            let tables: i64 = transaction
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table'",
                    [],
                    |r| r.get(0),
                )
                .map_err(database_error)?;
            if tables != 0 {
                return Err(MezError::invalid_state(
                    "unversioned nonempty MMP custody store",
                ));
            }
            transaction.execute_batch("CREATE TABLE state (singleton INTEGER PRIMARY KEY CHECK(singleton=1), owner TEXT NOT NULL, store_id BLOB NOT NULL CHECK(length(store_id)=16), generation INTEGER NOT NULL CHECK(generation>=0), batch BLOB NOT NULL); CREATE TABLE records (key TEXT PRIMARY KEY, value BLOB NOT NULL, digest BLOB NOT NULL CHECK(length(digest)=32)); PRAGMA user_version=1;").map_err(database_error)?;
            transaction
                .execute(
                    "INSERT INTO state VALUES(1,?1,?2,0,X'')",
                    params![owner, rand::random::<[u8; 16]>().as_slice()],
                )
                .map_err(database_error)?;
        } else if current_version != SCHEMA_VERSION {
            return Err(MezError::invalid_state(
                "MMP custody schema changed during open",
            ));
        }
        let stored_owner: String = transaction
            .query_row("SELECT owner FROM state WHERE singleton=1", [], |r| {
                r.get(0)
            })
            .map_err(database_error)?;
        if stored_owner != owner {
            return Err(MezError::conflict("MMP custody session owner mismatch"));
        }
        let store_id: Vec<u8> = transaction
            .query_row("SELECT store_id FROM state WHERE singleton=1", [], |r| {
                r.get(0)
            })
            .map_err(database_error)?;
        if store_id.len() != 16 {
            return Err(MezError::invalid_state(
                "invalid MMP custody store identity",
            ));
        }
        validate_totals(&transaction, limits)?;
        transaction.commit().map_err(database_error)?;
        super::shared_sqlite::integrity_check(&connection)?;
        Ok(Self {
            connection,
            limits,
            store_id,
        })
    }

    /// Commits exact canonical rows before returning a durable receipt.
    /// A retry of the immediately preceding identical batch returns its original
    /// receipt. Conflicting generations/preimages/capacity change no row. An I/O
    /// error may be ambiguous; reconcile the same batch before later admission.
    pub fn commit(
        &mut self,
        generation: u64,
        writes: &[CustodyWrite<'_>],
    ) -> Result<CustodyCommit> {
        let digest = batch_digest(writes)?;
        let expected = i64::try_from(generation)
            .map_err(|_| MezError::invalid_args("MMP custody generation overflow"))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;
        let (actual, previous): (i64, Vec<u8>) = transaction
            .query_row(
                "SELECT generation,batch FROM state WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(database_error)?;
        if expected.checked_add(1) == Some(actual) && previous == digest {
            // A historical label alone cannot discharge the caller's pending
            // original. Validate actual bounded postimages under this same
            // transaction; missing/corrupt custody must never be reconstructed.
            for write in writes {
                if read_record(&transaction, write.key)?.as_deref() != write.value {
                    return Err(MezError::invalid_state(
                        "MMP custody committed postimage no longer matches",
                    ));
                }
            }
            return Ok(CustodyCommit {
                generation: actual as u64,
                digest,
                store_id: self.store_id.clone(),
            });
        }
        if actual != expected {
            return Err(MezError::conflict("stale MMP custody generation"));
        }
        let next = actual
            .checked_add(1)
            .ok_or_else(|| MezError::invalid_state("MMP custody generation exhausted"))?;
        for write in writes {
            let stored =
                read_record(&transaction, write.key)?.map(|value| Sha256::digest(&value).to_vec());
            if stored.as_deref() != write.previous {
                return Err(MezError::conflict("MMP custody record preimage mismatch"));
            }
            if let Some(value) = write.value {
                let hash = Sha256::digest(value);
                transaction.execute("INSERT INTO records(key,value,digest) VALUES(?1,?2,?3) ON CONFLICT(key) DO UPDATE SET value=excluded.value,digest=excluded.digest", params![write.key,value,hash.as_slice()]).map_err(database_error)?;
            } else {
                transaction
                    .execute("DELETE FROM records WHERE key=?1", [write.key])
                    .map_err(database_error)?;
            }
        }
        validate_totals(&transaction, self.limits)?;
        transaction
            .execute(
                "UPDATE state SET generation=?1,batch=?2 WHERE singleton=1",
                params![next, &digest],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)?;
        Ok(CustodyCommit {
            generation: next as u64,
            digest,
            store_id: self.store_id.clone(),
        })
    }

    /// Reads and checksum-validates one bounded canonical record after restart.
    /// Corruption is an error, never empty recovery; this grants no live identity.
    pub fn read(&self, key: &str) -> Result<Option<Vec<u8>>> {
        validate_identifier(key)?;
        read_record(&self.connection, key)
    }
}

/// Nullable SQL fields represent invalid framing without loading oversized blobs.
type FramedRecord = (Option<Vec<u8>>, Option<Vec<u8>>);

/// Loads bounded bytes and framing under one SQLite statement snapshot. An
/// oversized/corrupt value is never copied into application memory or treated
/// as missing; commit preimage checks use the same verification as reads.
fn read_record(connection: &Connection, key: &str) -> Result<Option<Vec<u8>>> {
    let row: Option<FramedRecord> = connection.query_row(
        "SELECT CASE WHEN length(value)<=?2 THEN value ELSE NULL END, CASE WHEN length(digest)=32 THEN digest ELSE NULL END FROM records WHERE key=?1",
        params![key, MAX_BATCH_BYTES as i64], |row| Ok((row.get(0)?,row.get(1)?))
    ).optional().map_err(database_error)?;
    let Some((value, digest)) = row else {
        return Ok(None);
    };
    let (Some(value), Some(digest)) = (value, digest) else {
        return Err(MezError::invalid_state(
            "invalid MMP custody record framing",
        ));
    };
    if Sha256::digest(&value).as_slice() != digest {
        return Err(MezError::invalid_state(
            "MMP custody record checksum mismatch",
        ));
    }
    Ok(Some(value))
}

/// Rejects unbounded/ambiguous row identities before allocation or SQL work.
fn validate_identifier(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 256 || !value.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(MezError::invalid_args("invalid MMP custody identifier"));
    }
    Ok(())
}

/// Rejects foreign, linked, nonprivate or special files before SQLite opens them.
/// The trusted private parent remains the protection against path replacement.
fn validate_private_path(path: &Path, directory: bool) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.permissions().mode() & 0o077 != 0
        || if directory {
            !metadata.is_dir()
        } else {
            !metadata.is_file() || metadata.nlink() != 1
        }
    {
        return Err(MezError::invalid_state(
            "MMP custody requires private owned regular paths",
        ));
    }
    Ok(())
}

/// Hashes a finite, unambiguously framed batch including every expected digest.
fn batch_digest(writes: &[CustodyWrite<'_>]) -> Result<Vec<u8>> {
    if writes.is_empty() || writes.len() > MAX_BATCH_RECORDS {
        return Err(MezError::invalid_args("MMP custody batch record limit"));
    }
    let mut bytes = 0usize;
    let mut keys = HashSet::new();
    let mut hash = Sha256::new();
    hash.update(b"mezzanine-mmp-custody-batch-v1");
    for write in writes {
        validate_identifier(write.key)?;
        if !keys.insert(write.key)
            || write.previous.is_some_and(|digest| digest.len() != 32)
            || (write.previous.is_none() && write.value.is_none())
        {
            return Err(MezError::invalid_args(
                "invalid MMP custody batch preimage/key",
            ));
        }
        bytes = bytes
            .saturating_add(write.key.len())
            .saturating_add(write.previous.map_or(0, <[u8]>::len))
            .saturating_add(write.value.map_or(0, <[u8]>::len))
            .saturating_add(ROW_METADATA_BYTES);
        if bytes > MAX_BATCH_BYTES {
            return Err(MezError::invalid_args("MMP custody batch byte limit"));
        }
        hash.update((write.key.len() as u64).to_le_bytes());
        hash.update(write.key.as_bytes());
        for field in [write.previous, write.value] {
            hash.update([u8::from(field.is_some())]);
            if let Some(value) = field {
                hash.update((value.len() as u64).to_le_bytes());
                hash.update(value);
            }
        }
    }
    Ok(hash.finalize().to_vec())
}

/// Checks the whole canonical logical projection before publishing any batch.
fn validate_totals(connection: &Connection, limits: CustodyStoreLimits) -> Result<()> {
    let (count,bytes): (i64,i64) = connection.query_row("SELECT COUNT(*), COALESCE(SUM(length(CAST(key AS BLOB))+length(value)+length(digest)+64),0) FROM records", [], |r| Ok((r.get(0)?,r.get(1)?))).map_err(database_error)?;
    if count < 0
        || bytes < 0
        || count > limits.records as i64
        || bytes > limits.logical_bytes as i64
    {
        return Err(MezError::invalid_state(
            "MMP custody storage capacity; batch not accepted",
        ));
    }
    Ok(())
}

/// Preserves storage context and distinguishes contention from data corruption.
fn database_error(error: rusqlite::Error) -> MezError {
    if let rusqlite::Error::SqliteFailure(code, _) = &error
        && matches!(
            code.code,
            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
        )
    {
        return MezError::conflict(format!("MMP custody database busy: {error}"));
    }
    MezError::invalid_state(format!("MMP custody database: {error}"))
}

#[cfg(test)]
mod tests;
