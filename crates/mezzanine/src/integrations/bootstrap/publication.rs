//! Descriptor-relative bounded publication and forward recovery for owned edits.
//!
//! Read-only inspection holds root handles without lock creation/acquisition.
//! Publication/recovery explicitly acquire a private cooperating-installer lock.
//! A synced journal owns
//! exact before/after bytes before any destination is changed. Recovery finishes
//! only when every current preimage equals one of those states; foreign edits
//! leave the journal intact. This is not atomic CAS against arbitrary writers.
//! No symlink or special node is followed, and no executable is launched.

use super::reconciliation::publication_path;
use crate::error::{MezError, Result};
use rustix::fs::{
    AtFlags, FlockOperation, Mode, OFlags, RenameFlags, flock, mkdirat, openat, renameat,
    renameat_with, unlinkat,
};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

const JOURNAL: &str = ".mez-bootstrap-journal";
const LOCK: &str = ".mez-bootstrap-lock";
const MAX_BYTES: usize = 1024 * 1024;
const MAX_JOURNAL: usize = 32 * 1024 * 1024;

/// One exact replacement, retained in the journal before publication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Change {
    pub(super) path: String,
    pub(super) before: Option<Vec<u8>>,
    pub(super) after: Option<Vec<u8>>,
}

/// Versioned recovery intent; unknown versions are not interpreted.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u32,
    root_device: u64,
    root_inode: u64,
    intent: serde_json::Value,
    changes: Vec<Change>,
}

/// Held root, with optional exclusive ownership acquired only for publication.
pub(super) struct Publisher {
    root: PathBuf,
    pub(super) directory: File,
    _lock: Option<File>,
    /// Task-local publication barrier; no shared fault state.
    #[cfg(test)]
    pub(super) stop_after: std::cell::Cell<Option<usize>>,
}

/// Opens every absolute directory component without following symlinks.
fn open_directory(path: &Path) -> Result<File> {
    if !path.is_absolute() {
        return Err(MezError::invalid_args("bootstrap root must be absolute"));
    }
    let mut directory = File::open("/")?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                directory = File::from(
                    openat(
                        &directory,
                        name,
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(),
                    )
                    .map_err(std::io::Error::from)?,
                )
            }
            _ => return Err(MezError::forbidden("bootstrap root traversal rejected")),
        }
    }
    let metadata = directory.metadata()?;
    if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o022 != 0 {
        return Err(MezError::forbidden(
            "bootstrap root must be owned and not writable by other users",
        ));
    }
    Ok(directory)
}

/// Reads a regular single-link owned file promptly, bounded before and during I/O.
fn read_at(directory: &File, name: &str, limit: usize) -> Result<Option<Vec<u8>>> {
    let descriptor = match openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(descriptor) => descriptor,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => return Err(std::io::Error::from(error).into()),
    };
    let mut file = File::from(descriptor);
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o022 != 0
        || metadata.len() > limit as u64
    {
        return Err(MezError::forbidden(
            "bootstrap destination must be bounded, owned and single-link regular file",
        ));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(MezError::invalid_args("bootstrap file exceeds limit"));
    }
    Ok(Some(bytes))
}

impl Publisher {
    /// Holds a no-follow root for read-only inspection. No lock, journal,
    /// artifact parent or other filesystem state is created or acquired.
    pub(super) fn inspect(root: &Path) -> Result<Self> {
        Ok(Self {
            root: root.into(),
            directory: open_directory(root)?,
            _lock: None,
            #[cfg(test)]
            stop_after: std::cell::Cell::new(None),
        })
    }

    /// Acquires a private lock without waiting indefinitely or changing vendor files.
    pub(super) fn open(root: &Path) -> Result<Self> {
        let mut publisher = Self::inspect(root)?;
        publisher.acquire_lock()?;
        Ok(publisher)
    }

    /// Converts an inspected root to one bounded cooperating writer. The held
    /// root must still match its spelling before and after lock acquisition;
    /// no destination/journal work is authorized until this returns successfully.
    pub(super) fn acquire_lock(&mut self) -> Result<()> {
        self.validate_root()?;
        if self._lock.is_some() {
            return Ok(());
        }
        let lock = File::from(
            openat(
                &self.directory,
                LOCK,
                OFlags::RDWR
                    | OFlags::CREATE
                    | OFlags::NOFOLLOW
                    | OFlags::CLOEXEC
                    | OFlags::NONBLOCK,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(std::io::Error::from)?,
        );
        let metadata = lock.metadata()?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
        {
            return Err(MezError::forbidden(
                "bootstrap lock must be private and owned",
            ));
        }
        flock(&lock, FlockOperation::NonBlockingLockExclusive).map_err(|error| {
            MezError::conflict(format!("bootstrap installer lock unavailable: {error}"))
        })?;
        self._lock = Some(lock);
        self.validate_root()
    }

    /// Read-only holders cannot accidentally publish/recover through the shared
    /// descriptor helpers. Mutation requires explicitly acquired writer ownership.
    fn require_lock(&self) -> Result<()> {
        if self._lock.is_none() {
            return Err(MezError::invalid_state(
                "bootstrap publication requires installer lock",
            ));
        }
        self.validate_root()
    }

    /// Revalidates held root spelling before publishing through its descriptors.
    fn validate_root(&self) -> Result<()> {
        let current = open_directory(&self.root)?.metadata()?;
        let held = self.directory.metadata()?;
        if current.dev() != held.dev() || current.ino() != held.ino() {
            return Err(MezError::conflict(
                "bootstrap root replaced; recovery required",
            ));
        }
        Ok(())
    }

    /// Resolves a relative parent with no-follow handles, optionally creating it.
    fn parent(&self, path: &str, create: bool) -> Result<Option<(File, String)>> {
        publication_path(path)?;
        self.validate_root()?;
        let mut directory = self.directory.try_clone()?;
        let parts = path.split('/').collect::<Vec<_>>();
        for part in &parts[..parts.len() - 1] {
            let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            let child = match openat(&directory, *part, flags, Mode::empty()) {
                Ok(child) => child,
                Err(rustix::io::Errno::NOENT) if create => {
                    match mkdirat(&directory, *part, Mode::RUSR | Mode::WUSR | Mode::XUSR) {
                        Ok(()) => directory.sync_all()?,
                        Err(rustix::io::Errno::EXIST) => {}
                        Err(error) => return Err(std::io::Error::from(error).into()),
                    }
                    openat(&directory, *part, flags, Mode::empty()).map_err(std::io::Error::from)?
                }
                Err(rustix::io::Errno::NOENT) => return Ok(None),
                Err(error) => return Err(std::io::Error::from(error).into()),
            };
            directory = File::from(child);
            let metadata = directory.metadata()?;
            if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o022 != 0
            {
                return Err(MezError::forbidden(
                    "bootstrap parent must be owned and not writable by others",
                ));
            }
        }
        Ok(Some((directory, parts[parts.len() - 1].into())))
    }

    /// Reads one safe destination without creating missing parents.
    pub(super) fn read(&self, path: &str) -> Result<Option<Vec<u8>>> {
        let Some((parent, name)) = self.parent(path, false)? else {
            return Ok(None);
        };
        read_at(&parent, &name, MAX_BYTES)
    }

    /// Publishes bytes using exclusive staging; additions use atomic no-replace rename.
    fn write_at(&self, parent: &File, name: &str, bytes: &[u8], replace: bool) -> Result<()> {
        let stage = format!(
            ".mez-bootstrap-stage-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        );
        let mut file = File::from(
            openat(
                parent,
                stage.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(std::io::Error::from)?,
        );
        let publication = (|| -> Result<()> {
            file.write_all(bytes)?;
            file.sync_all()?;
            if replace {
                renameat(parent, stage.as_str(), parent, name).map_err(std::io::Error::from)?;
            } else {
                renameat_with(parent, stage.as_str(), parent, name, RenameFlags::NOREPLACE)
                    .map_err(std::io::Error::from)?;
            }
            parent.sync_all()?;
            Ok(())
        })();
        let _ = unlinkat(parent, stage.as_str(), AtFlags::empty());
        publication
    }

    /// Verifies all destinations before journal publication or recovery progress.
    fn validate_changes(&self, changes: &[Change], recovering: bool) -> Result<()> {
        if changes.is_empty() || changes.len() > 16 {
            return Err(MezError::invalid_args("bootstrap transaction entry limit"));
        }
        let mut paths = std::collections::BTreeSet::new();
        for change in changes {
            if !paths.insert(&change.path)
                || change.before.as_ref().is_some_and(|b| b.len() > MAX_BYTES)
                || change.after.as_ref().is_some_and(|b| b.len() > MAX_BYTES)
            {
                return Err(MezError::invalid_args("bootstrap transaction bounds"));
            }
            let current = self.read(&change.path)?;
            if current != change.before && !(recovering && current == change.after) {
                return Err(MezError::conflict(
                    "bootstrap preimage changed; journal retained, no overwrite",
                ));
            }
        }
        Ok(())
    }

    /// Completes forward publication, retaining the journal on any partial failure.
    fn finish(&self, changes: &[Change]) -> Result<()> {
        self.validate_changes(changes, true)?;
        for (index, change) in changes.iter().enumerate() {
            #[cfg(not(test))]
            let _ = index;
            let current = self.read(&change.path)?;
            if current == change.after {
                continue;
            }
            if current != change.before {
                return Err(MezError::conflict(
                    "bootstrap preimage changed during publication",
                ));
            }
            let Some((parent, name)) = self.parent(&change.path, change.after.is_some())? else {
                return Err(MezError::conflict(
                    "bootstrap destination parent unavailable",
                ));
            };
            if read_at(&parent, &name, MAX_BYTES)? != change.before {
                return Err(MezError::conflict(
                    "bootstrap preimage changed before commit",
                ));
            }
            match &change.after {
                Some(bytes) => self.write_at(&parent, &name, bytes, change.before.is_some())?,
                None => {
                    unlinkat(&parent, name.as_str(), AtFlags::empty())
                        .map_err(std::io::Error::from)?;
                    parent.sync_all()?;
                }
            }
            #[cfg(test)]
            if self.stop_after.get() == Some(index + 1) {
                self.stop_after.set(None);
                return Err(MezError::invalid_state(
                    "injected bootstrap publication interruption",
                ));
            }
        }
        unlinkat(&self.directory, JOURNAL, AtFlags::empty()).map_err(std::io::Error::from)?;
        self.directory.sync_all()?;
        Ok(())
    }

    /// Recovers only an explicitly accepted durable journal, without new planning.
    pub(super) fn recover_authorized(
        &self,
        authorize: impl FnOnce(&serde_json::Value, &[Change]) -> Result<()>,
    ) -> Result<bool> {
        self.require_lock()?;
        let Some(bytes) = read_at(&self.directory, JOURNAL, MAX_JOURNAL)? else {
            return Ok(false);
        };
        let journal: Journal = serde_json::from_slice(&bytes).map_err(|_| {
            MezError::invalid_state("bootstrap journal invalid; manual review required")
        })?;
        let root = self.directory.metadata()?;
        self.validate_root()?;
        if journal.version != 2
            || journal.root_device != root.dev()
            || journal.root_inode != root.ino()
        {
            return Err(MezError::invalid_state(
                "bootstrap journal version/root mismatch",
            ));
        }
        authorize(&journal.intent, &journal.changes)?;
        self.finish(&journal.changes)?;
        Ok(true)
    }

    /// Commits an already planned transaction; never silently recovers old intent.
    pub(super) fn require_no_pending_journal(&self) -> Result<()> {
        if read_at(&self.directory, JOURNAL, MAX_JOURNAL)?.is_some() {
            return Err(MezError::conflict("bootstrap recovery pending"));
        }
        Ok(())
    }

    /// Commits an already planned transaction; never silently recovers old intent.
    pub(super) fn apply_authorized(
        &self,
        changes: Vec<Change>,
        intent: serde_json::Value,
    ) -> Result<()> {
        self.require_lock()?;
        self.require_no_pending_journal()?;
        self.validate_changes(&changes, false)?;
        let root = self.directory.metadata()?;
        let bytes = serde_json::to_vec(&Journal {
            version: 2,
            root_device: root.dev(),
            root_inode: root.ino(),
            intent,
            changes: changes.clone(),
        })
        .map_err(|error| MezError::invalid_state(format!("bootstrap journal encoding: {error}")))?;
        if bytes.len() > MAX_JOURNAL {
            return Err(MezError::invalid_args("bootstrap journal limit"));
        }
        self.write_at(&self.directory, JOURNAL, &bytes, false)?;
        self.finish(&changes)
    }

    /// Test-only raw transaction fixture; production always supplies manifest intent.
    #[cfg(test)]
    pub(super) fn apply(&self, changes: Vec<Change>) -> Result<()> {
        self.apply_authorized(changes, serde_json::Value::Null)
    }

    /// Test-only publication recovery isolates filesystem behavior from manifest admission.
    #[cfg(test)]
    pub(super) fn recover(&self) -> Result<bool> {
        self.recover_authorized(|intent, _| {
            if intent.is_null() {
                Ok(())
            } else {
                Err(MezError::forbidden(
                    "raw recovery fixture cannot authorize manifest intent",
                ))
            }
        })
    }
}

impl Drop for Publisher {
    /// Releases advisory ownership even if a concurrent fork inherited a copy
    /// of the open description. Descriptor closure alone can leave it locked.
    fn drop(&mut self) {
        if let Some(lock) = &self._lock {
            let _ = flock(lock, FlockOperation::Unlock);
        }
    }
}
