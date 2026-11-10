//! Conservative physical envelope for one cooperating owned custody root.
//!
//! The main SQLite file remains capped at 64 MiB. DELETE journaling, no cache
//! spilling and memory-only temporary tables bound the supported pager's journal:
//! each original page has one page-number/checksum preimage, with sector-aligned
//! headers. An 80 MiB allowance deliberately exceeds that bound. We charge the
//! whole allowance even when no journal exists, plus all other private files and
//! the complete rewrite pool even before leases are issued. This is not an OS
//! disk quota or a promise of free blocks; ENOSPC must still retain original
//! effects and evidence.
//!
//! Physical qualification is explicit and sticky for the actual root owner;
//! preexisting unqualified roots keep their general directory layout contract.
//! Only the runtime root fence participates. Direct lower-store connections and
//! hostile same-user filesystem mutation are outside this cooperating protocol.
//! No temporary file, cleanup intent, deletion permission or retry is created.

use super::acceptance::AcceptancePlan;
use super::ownership::RootOwnership;
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

const PHYSICAL_BYTES: u64 = 160 * 1024 * 1024;
const JOURNAL_BYTES: u64 = 80 * 1024 * 1024;
const REWRITE_BYTES: usize = 8 * 1024 * 1024;
/// At most 128 independently allocated leases, even for one-byte requests.
const MIN_RESERVATION_CHARGE: usize = 64 * 1024;
const MAX_FILES: usize = 128;

/// Actual original owner and exact pre-write position, not a disposal grant.
pub(crate) struct RewriteCapacityReservation {
    owner: Arc<RootOwnership>,
    position: AcceptancePlan,
    bytes: usize,
    charge: usize,
}

impl RewriteCapacityReservation {
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    /// Must be combined with actual root/store revalidation before any I/O.
    pub(crate) fn matches(&self, owner: &RootOwnership, position: &AcceptancePlan) -> bool {
        std::ptr::eq(self.owner.as_ref(), owner) && self.position.same_position(position)
    }
}

impl Drop for RewriteCapacityReservation {
    fn drop(&mut self) {
        self.owner
            .rewrite_reserved
            .fetch_sub(self.charge, Ordering::AcqRel);
    }
}

impl RootOwnership {
    /// Reserves bounded headroom before issuance without filesystem I/O. The
    /// worker must subsequently validate the actual root envelope. Every root
    /// clone uses the same counter; each lease retains the real OS owner lock.
    pub(crate) fn reserve_rewrite_capacity(
        self: &Arc<Self>,
        position: &AcceptancePlan,
        bytes: usize,
    ) -> Result<Arc<RewriteCapacityReservation>> {
        if bytes == 0 || bytes > REWRITE_BYTES {
            return Err(unavailable());
        }
        let charge = bytes.max(MIN_RESERVATION_CHARGE);
        let mut held = self.rewrite_reserved.load(Ordering::Acquire);
        loop {
            let next = held
                .checked_add(charge)
                .filter(|total| *total <= REWRITE_BYTES)
                .ok_or_else(unavailable)?;
            match self.rewrite_reserved.compare_exchange_weak(
                held,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(current) => held = current,
            }
        }
        Ok(Arc::new(RewriteCapacityReservation {
            owner: self.clone(),
            position: position.clone(),
            bytes,
            charge,
        }))
    }
}

/// Checks paths before SQLite open/recovery and at every owned binding. Unknown
/// files are preserved and charged, never deleted or interpreted as intents.
/// Sparse lengths and allocated blocks are both charged; symlinks, hardlinks,
/// nested directories, special files and unbounded entry sets fail closed.
pub(super) fn validate_paths(root: &Path, reserved: &AtomicUsize) -> Result<()> {
    if !qualification_paths(root, reserved)? {
        return Err(unavailable());
    }
    Ok(())
}

/// False is an expected first-qualification layout/capacity refusal, not a
/// source-integrity verdict. Existing qualified owners use validate_paths and
/// cannot downgrade a later broken envelope. Unsafe paths/I/O remain errors.
pub(super) fn qualification_paths(root: &Path, reserved: &AtomicUsize) -> Result<bool> {
    let main_bytes = (CustodyStoreLimits::default().database_pages as u64) * 4096;
    if reserved.load(Ordering::Acquire) > REWRITE_BYTES {
        return Err(unavailable());
    }
    let directory = fs::symlink_metadata(root)?;
    let directory_bytes = directory.len().max(
        directory
            .blocks()
            .checked_mul(512)
            .ok_or_else(unavailable)?,
    );
    let mut total = main_bytes
        .checked_add(JOURNAL_BYTES)
        .and_then(|n| n.checked_add(REWRITE_BYTES as u64))
        .and_then(|n| n.checked_add(directory_bytes))
        .ok_or_else(unavailable)?;
    let mut count = 0;
    let mut fits = total <= PHYSICAL_BYTES;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        count += 1;
        if count > MAX_FILES {
            return Ok(false);
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        validate_private_path(&path, metadata.is_dir())?;
        if metadata.is_dir() {
            fits = false;
            continue;
        }
        let bytes = metadata
            .len()
            .max(metadata.blocks().checked_mul(512).ok_or_else(unavailable)?);
        match entry.file_name().to_str() {
            Some("custody.sqlite") => {
                if bytes > main_bytes {
                    return Err(unavailable());
                }
            }
            Some("custody.sqlite-journal") => {
                if bytes > JOURNAL_BYTES {
                    return Err(unavailable());
                }
            }
            Some("custody.sqlite-wal" | "custody.sqlite-shm") => return Err(unavailable()),
            _ => total = total.checked_add(bytes).ok_or_else(unavailable)?,
        }
        if total > PHYSICAL_BYTES {
            fits = false;
        }
    }
    Ok(fits)
}

/// Reject changed connection assumptions rather than silently resetting policy.
/// Public row writes use this owned binding before and after worker phases.
pub(super) fn validate_connection(store: &MmpCustodyStore) -> Result<()> {
    let connection = &store.connection;
    let mode: String = connection
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .map_err(database_error)?;
    let scalar = |pragma: &str| -> Result<i64> {
        connection
            .query_row(pragma, [], |r| r.get(0))
            .map_err(database_error)
    };
    if mode != "delete"
        || scalar("PRAGMA page_size")? != 4096
        || scalar("PRAGMA max_page_count")? != store.limits.database_pages as i64
        || scalar("PRAGMA cache_spill")? != 0
        || scalar("PRAGMA temp_store")? != 2
        || scalar("PRAGMA synchronous")? != 3
    {
        return Err(unavailable());
    }
    Ok(())
}

fn unavailable() -> MezError {
    MezError::invalid_state("MMP custody physical capacity unavailable")
}

#[cfg(test)]
mod tests;
