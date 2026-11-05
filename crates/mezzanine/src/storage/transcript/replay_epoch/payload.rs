//! Immutable content-addressed source, bounded reads and ordered durability.
//! Manifest metadata owns occurrence identity; deduplicating equal bytes never
//! collapses original events. Existing payloads must match before reuse. Paths
//! are derived exclusively from checked lowercase SHA-256, never stored paths.

use super::{SOURCE_LIMIT, invalid};
use crate::{MezError, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::Path,
};

/// Private immutable content reference, independent of canonical occurrence ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Payload {
    pub(super) sha256: String,
    pub(super) bytes: usize,
}

impl Payload {
    /// Captures the exact UTF-8 payload that publication will make durable.
    pub(super) fn new(bytes: &[u8]) -> Self {
        Self {
            sha256: digest(bytes),
            bytes: bytes.len(),
        }
    }

    /// Rejects malformed references before allocating memory or joining paths.
    fn validate_format(&self) -> Result<()> {
        if self.bytes > SOURCE_LIMIT
            || self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(invalid("invalid payload reference"));
        }
        Ok(())
    }

    /// Checks both exact length and digest before exposing source bytes.
    pub(super) fn validate(&self, bytes: &[u8]) -> Result<()> {
        self.validate_format()?;
        if bytes.len() != self.bytes || digest(bytes) != self.sha256 {
            return Err(invalid("required payload integrity check failed"));
        }
        Ok(())
    }
}

/// Computes the canonical lowercase digest used by immutable payload names.
pub(super) fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Reads at most the accepted bound plus one byte, including racing file growth.
pub(super) fn bounded_read(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > limit {
        return Err(invalid(
            "required file exceeds size limit or is not regular",
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("required file grew beyond size limit"));
    }
    Ok(bytes)
}

/// Resolves a single checked reference, never a lifetime archive scan.
pub(super) fn read(directory: &Path, reference: &Payload) -> Result<String> {
    reference.validate_format()?;
    let bytes = bounded_read(
        &directory.join("epoch-payloads").join(&reference.sha256),
        reference.bytes as u64,
    )
    .map_err(|error| error.with_context("required replay epoch payload unavailable"))?;
    reference.validate(&bytes)?;
    String::from_utf8(bytes).map_err(|_| invalid("required payload is not UTF-8"))
}

/// Publishes/reuses exact bytes then syncs both file and directory. A retry of a
/// prior uncertain write checks existing content instead of blessing corruption.
pub(super) fn publish(
    directory: &Path,
    hash: &str,
    bytes: &[u8],
    before_sync: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let reference = Payload {
        sha256: hash.to_owned(),
        bytes: bytes.len(),
    };
    reference.validate(bytes)?;
    let payloads = directory.join("epoch-payloads");
    match fs::DirBuilder::new().mode(0o700).create(&payloads) {
        Ok(()) => sync_directory(directory)?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let path = payloads.join(hash);
    if path.try_exists()? {
        reference.validate(&bounded_read(&path, reference.bytes as u64)?)?;
        before_sync()?;
        fs::File::open(&path)?.sync_all()?;
    } else {
        let temporary = payloads.join(format!(".{hash}.tmp"));
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(bytes)?;
            before_sync()?;
            file.sync_all()?;
            fs::rename(&temporary, &path)?;
            Ok::<_, MezError>(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result?;
    }
    sync_directory(&payloads)
}

/// Persists namespace changes on both supported Unix targets.
pub(super) fn sync_directory(directory: &Path) -> Result<()> {
    fs::File::open(directory)?.sync_all().map_err(Into::into)
}
