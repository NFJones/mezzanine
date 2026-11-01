//! Cooperative, bounded quanta for descriptor I/O and byte verification.
//!
//! Callers inject their original cancellation/deadline check explicitly. This
//! owner grants no path or mutation authority and cannot interrupt a blocked OS
//! call. Reads, writes and hashes return to the check between 64 KiB quanta.
//! Publication callers must check before the committing syscall, then retain
//! positive completion evidence without a fallible checkpoint afterwards.

use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::FileExt;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::error::{MezError, Result};

/// Maximum bytes processed between cooperative checks, not a syscall deadline.
pub(crate) const NATIVE_IO_QUANTUM: usize = 64 * 1024;

/// Explicit reduce-only work control; cloning never renews its caller's budget.
#[derive(Clone)]
pub(crate) struct NativeIoControl {
    /// Caller-owned original budget/cancellation, never filesystem permission.
    checkpoint: Arc<dyn Fn() -> Result<()> + Send + Sync>,
}

impl NativeIoControl {
    /// Retains a caller's original check, including its unchanged deadline.
    pub(crate) fn new(checkpoint: impl Fn() -> Result<()> + Send + Sync + 'static) -> Self {
        Self {
            checkpoint: Arc::new(checkpoint),
        }
    }

    /// Selects ordinary bounded primitive access without a caller work budget.
    /// Native patch workers must instead inject their actual original fence.
    pub(crate) fn bounded_only() -> Self {
        Self::new(|| Ok(()))
    }

    /// Checks work admission only; success cannot authorize a read or commit.
    pub(crate) fn check(&self) -> Result<()> {
        (self.checkpoint)()
    }

    /// Reads at most the byte limit, checking between bounded read requests.
    /// A growing file is detected with one extra byte; interrupted reads retry
    /// through the checkpoint rather than concealing an unbounded retry loop.
    pub(crate) fn read_bounded(&self, file: &mut File, maximum: u64) -> Result<Vec<u8>> {
        let limit = maximum
            .checked_add(1)
            .ok_or_else(|| MezError::invalid_args("native read byte budget overflow"))?;
        let mut reader = file.take(limit);
        let mut bytes = Vec::new();
        let mut buffer = [0; NATIVE_IO_QUANTUM];
        loop {
            self.check()?;
            let length = match reader.read(&mut buffer) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                result => result?,
            };
            if length == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..length]);
            if bytes.len() as u64 > maximum {
                return Err(MezError::invalid_args(
                    "native snapshot exceeded byte budget during read",
                ));
            }
        }
        self.check()?;
        Ok(bytes)
    }

    /// Hashes immutable bounded input without hiding a whole-file CPU step.
    pub(crate) fn digest(&self, bytes: &[u8]) -> Result<[u8; 32]> {
        self.check()?;
        let mut digest = Sha256::new();
        for chunk in bytes.chunks(NATIVE_IO_QUANTUM) {
            self.check()?;
            digest.update(chunk);
        }
        self.check()?;
        Ok(digest.finalize().into())
    }

    /// Writes only bounded slices and checks even between interrupted retries.
    /// The caller must already own cleanup for the unpublished staging entry.
    pub(crate) fn write_all(&self, file: &mut File, bytes: &[u8]) -> Result<()> {
        let mut offset = 0;
        while offset < bytes.len() {
            self.check()?;
            let end = bytes.len().min(offset.saturating_add(NATIVE_IO_QUANTUM));
            let length = match file.write(&bytes[offset..end]) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                result => result?,
            };
            if length == 0 {
                return Err(std::io::Error::from(std::io::ErrorKind::WriteZero).into());
            }
            offset += length;
        }
        self.check()
    }

    /// Verifies the held file through bounded positional reads without making
    /// a second whole-file allocation or changing its current file position.
    pub(crate) fn verify_digest(
        &self,
        file: &File,
        length: u64,
        expected: &[u8; 32],
    ) -> Result<()> {
        self.check()?;
        let mut digest = Sha256::new();
        let mut buffer = [0; NATIVE_IO_QUANTUM];
        let mut offset = 0;
        while offset < length {
            self.check()?;
            let capacity = (length - offset).min(NATIVE_IO_QUANTUM as u64) as usize;
            let count = match file.read_at(&mut buffer[..capacity], offset) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                result => result?,
            };
            if count == 0 {
                return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
            }
            digest.update(&buffer[..count]);
            offset += count as u64;
        }
        self.check()?;
        let actual: [u8; 32] = digest.finalize().into();
        if &actual != expected {
            return Err(MezError::conflict("native staged bytes changed"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
