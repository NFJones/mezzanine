//! Durable persistent-host process ownership.
//!
//! The host owns configuration-scoped leases, generations, trust, identity,
//! and checkpoints even when runtime socket roots differ. This module fences
//! all of those resources with one nonblocking lifetime lock under the durable
//! host-state root. Runtime-root locks remain startup-election conveniences and
//! are never authoritative for durable state ownership.

use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use rustix::fs::{FlockOperation, Mode, OFlags, flock, open};

use crate::error::{MezError, Result};
use crate::storage::lease::default_host_state_directory;

const HOST_OWNER_LOCK_FILE_NAME: &str = "owner.lock";

/// Exclusive lifetime ownership of one durable persistent-host state root.
#[derive(Debug)]
pub(crate) struct HostOwnershipGuard {
    state_root: PathBuf,
    lock: fs::File,
}

impl HostOwnershipGuard {
    /// Acquires exclusive nonblocking ownership below the configuration root.
    ///
    /// The returned guard must remain alive until every host listener and
    /// durable-state worker has stopped. A live owner returns a conflict before
    /// the caller may initialize identity, advance generations, or recover
    /// sessions.
    pub(crate) fn acquire(config_root: &Path, owner_uid: u32) -> Result<Self> {
        ensure_private_directory(config_root, owner_uid)?;
        let state_root = default_host_state_directory(config_root);
        ensure_private_directory(&state_root, owner_uid)?;
        let path = state_root.join(HOST_OWNER_LOCK_FILE_NAME);
        let descriptor = open(
            &path,
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(std::io::Error::from)?;
        let mut lock = fs::File::from(descriptor);
        validate_private_lock(&path, owner_uid, &lock.metadata()?)?;
        match flock(&lock, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(error) if error == rustix::io::Errno::WOULDBLOCK => {
                return Err(MezError::conflict(
                    "another persistent host already owns this durable host state",
                ));
            }
            Err(error) => return Err(std::io::Error::from(error).into()),
        }
        write_owner_diagnostics(&mut lock)?;
        Ok(Self { state_root, lock })
    }

    /// Verifies this guard belongs to the configuration root being served.
    pub(crate) fn validate_config_root(&self, config_root: &Path) -> Result<()> {
        if self.state_root == default_host_state_directory(config_root) {
            Ok(())
        } else {
            Err(MezError::invalid_args(
                "persistent host ownership guard does not match the configured durable state root",
            ))
        }
    }
}

impl Drop for HostOwnershipGuard {
    fn drop(&mut self) {
        let _ = flock(&self.lock, FlockOperation::Unlock);
    }
}

fn ensure_private_directory(path: &Path, owner_uid: u32) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    builder.mode(0o700);
    builder.create(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != owner_uid
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(MezError::forbidden(
            "durable host state directory must be private and owned by the current user",
        ));
    }
    Ok(())
}

fn validate_private_lock(path: &Path, owner_uid: u32, metadata: &fs::Metadata) -> Result<()> {
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != owner_uid
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(MezError::forbidden(format!(
            "durable host ownership lock {} must be a private regular file owned by the current user",
            path.display()
        )));
    }
    Ok(())
}

fn write_owner_diagnostics(lock: &mut fs::File) -> Result<()> {
    lock.set_len(0)?;
    lock.seek(SeekFrom::Start(0))?;
    writeln!(lock, "pid={}", std::process::id())?;
    lock.flush()?;
    Ok(())
}
