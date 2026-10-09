//! Configuration-root startup election, distinct from endpoint lifetime ownership.
//!
//! One short-lived launcher guard uses a protected nonblocking flock below the
//! canonical configuration root, independent of frontend runtime directories.
//! The held root and lock objects are revalidated without unlinking or stealing
//! ownership. This component starts no processes or network operations. The
//! persistent endpoint key's existing exclusive lifetime lock remains mandatory.
//! Successful acquisition belongs to this process, not fork-inherited descriptor
//! lifetimes. Only that process explicitly unlocks on disposal; unacquired or
//! inherited guards cannot release an active owner's open-file-description lock.

use std::fs::File;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use rustix::fs::{AtFlags, FlockOperation, Mode, OFlags, flock, open, openat, statat};

use crate::error::{MezError, Result};

const LOCK_NAME: &str = "outbound.startup.lock";

/// One elected launcher, retaining exact root and lock objects until disposal.
/// Releasing this guard permits another launch attempt, never another live key owner.
pub(super) struct StartupElection {
    root_path: PathBuf,
    root: File,
    lock: File,
    uid: u32,
    owner_pid: Option<u32>,
}

impl StartupElection {
    /// Attempts election without waiting. Returns None for a genuinely held lock;
    /// unsafe root/lock objects fail before they may authorize process startup.
    /// Parent provisioning is the caller's responsibility; final symlinks reject.
    pub(super) fn acquire(config_root: &Path) -> Result<Option<Self>> {
        let uid = crate::runtime::current_effective_uid();
        crate::runtime::ensure_private_socket_directory(config_root, uid)?;
        let root_path = std::fs::canonicalize(config_root)?;
        let root: File = open(
            &root_path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(std::io::Error::from)?
        .into();
        validate_root(&root_path, &root, uid)?;
        let lock: File = openat(
            &root,
            LOCK_NAME,
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(std::io::Error::from)?
        .into();
        let mut guard = Self {
            root_path,
            root,
            lock,
            uid,
            owner_pid: None,
        };
        guard.validate()?;
        match flock(&guard.lock, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {
                guard.owner_pid = Some(std::process::id());
                guard.validate()?;
                Ok(Some(guard))
            }
            Err(error) if error == rustix::io::Errno::WOULDBLOCK => Ok(None),
            Err(error) => Err(std::io::Error::from(error).into()),
        }
    }

    /// Revalidates current private-root policy and named lock identity before
    /// startup effects. This is not an atomic capability against same-user renames.
    pub(super) fn validate(&self) -> Result<()> {
        if self.owner_pid.is_some_and(|pid| pid != std::process::id()) {
            return Err(MezError::forbidden(
                "outbound startup election belongs to another process",
            ));
        }
        validate_root(&self.root_path, &self.root, self.uid)?;
        let held = self.lock.metadata()?;
        if !held.is_file()
            || held.uid() != self.uid
            || held.mode() & 0o077 != 0
            || held.nlink() != 1
        {
            return Err(MezError::forbidden(
                "outbound startup lock must remain private and regular",
            ));
        }
        let named = statat(&self.root, LOCK_NAME, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(std::io::Error::from)?;
        if named.st_dev != held.dev() || named.st_ino != held.ino() {
            return Err(MezError::conflict("outbound startup lock changed"));
        }
        Ok(())
    }

    /// Returns the retained root after validating launcher ownership. The
    /// descriptor is borrowed only for fixed-name diagnostic publication.
    pub(super) fn launch_root(&self) -> Result<(&Path, &File)> {
        self.validate()?;
        Ok((&self.root_path, &self.root))
    }

    /// Retains an inherited-style descriptor only for startup release fixtures.
    /// Production launchers cannot borrow the lock through this test-only seam.
    #[cfg(test)]
    pub(super) fn fixture_duplicate_lock(&self) -> Result<File> {
        self.validate()?;
        Ok(self.lock.try_clone()?)
    }
}

impl Drop for StartupElection {
    /// Best-effort explicit release of only this process's successful acquisition.
    /// The held descriptor, never a reopened path, targets the original lock even
    /// after replacement. Surviving duplicates cannot extend normal ownership;
    /// inherited guard disposal cannot unlock its parent. Unexpected host unlock
    /// errors retain normal descriptor-close fallback, not unlink or lock stealing.
    fn drop(&mut self) {
        if self.owner_pid == Some(std::process::id()) {
            let _ = flock(&self.lock, FlockOperation::Unlock);
        }
    }
}

/// Checks retained directory identity through read-only metadata observation.
fn validate_root(path: &Path, root: &File, uid: u32) -> Result<()> {
    let held = root.metadata()?;
    let current = std::fs::symlink_metadata(path)?;
    if !current.is_dir()
        || current.file_type().is_symlink()
        || current.uid() != uid
        || current.mode() & 0o077 != 0
    {
        return Err(MezError::forbidden(
            "outbound startup root must remain private",
        ));
    }
    if held.dev() != current.dev() || held.ino() != current.ino() {
        return Err(MezError::conflict("outbound startup root changed"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
