//! Atomic descriptive accounting import under the original conversation lease.
//!
//! Only the existing epoch file changes. Raw history, required-state markers,
//! receipt ownership and catalog authority remain with their original writers.
//! The prior modification time is retained: descriptive cache work is not new
//! conversation activity, including for a genuine summary-only archive.
//! A failed pre-rename import leaves the complete old authority available for
//! retry; the caller never exposes newly allocated origins before persistence.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use super::AgentCompactionEpoch;
use crate::{MezError, Result};

/// Replaces one validated epoch while its caller holds the mutation lease.
/// The explicit hook supports the store's existing pre-rename failure owner.
pub(in crate::storage::transcript) fn replace_summary_record_epoch(
    path: &Path,
    epoch: &AgentCompactionEpoch,
    before_rename: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let directory = path
        .parent()
        .ok_or_else(|| MezError::invalid_state("compaction epoch directory is unavailable"))?;
    let bytes = serde_json::to_vec(epoch).map_err(|error| {
        MezError::invalid_state(format!("compaction accounting encode failed: {error}"))
    })?;
    if bytes.len().saturating_add(1) as u64
        > super::super::store::ACCOUNTED_COMPACTION_EPOCH_MAX_BYTES
    {
        return Err(MezError::invalid_state(
            "compaction accounting exceeds sidecar size limit",
        ));
    }
    let temporary = directory.join(".compaction-epoch-accounting.json.tmp");
    let modified = fs::metadata(path)?.modified()?;
    let result = (|| {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        super::super::fs::set_private_file_permissions(&temporary)?;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.set_modified(modified)?;
        file.sync_all()?;
        before_rename()?;
        fs::rename(&temporary, path)?;
        super::super::fs::set_private_file_permissions(path)?;
        fs::File::open(directory)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
