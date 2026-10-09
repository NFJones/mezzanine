//! Private native bootstrap storage, separate from vendor configuration trees.
//!
//! HOME and the managed Mez boundary remain owned/non-other-writable; .config
//! is an OS-authorized routing ancestor with held incarnation, not a privacy
//! eligibility gate. Bootstrap/namespace directories and files stay user-private.
//! Existing modes/ownership are never normalized. Read-only inspection captures
//! exact directory/absence witnesses without mkdir, locking, or publication.
//! Mutating methods require nonwaiting cooperating ownership and revalidate held
//! state spelling/incarnation; additions use atomic no-replace publication.
//!
//! One namespace per effective UID and physical vendor-root device/inode shares
//! a lock across harnesses and path aliases. Namespace identity is coordination,
//! not artifact or journal authority: the installer must independently qualify
//! root identity, harness, compiled intent, receipts and every effect. File names
//! are internal bounded flat identifiers; no stored name supplies vendor paths.
//! CLI admission selects the private Publisher, which owns exact journal
//! migration; receipt/archive activation and vendor eligibility remain outside
//! this storage primitive.
//! It follows the repository's Unix owner/mode privacy contract, not ACL or
//! executable attestation. Preimage checks plus rename are not external-writer
//! CAS. Partial directory creation may remain after failure; no tree sweeping.

use super::root_directory::{CreatedDirectories, RootDirectory, walk};
use crate::error::{MezError, Result};
use rustix::fs::{
    AtFlags, FlockOperation, Mode, OFlags, RenameFlags, flock, openat, renameat, renameat_with,
    unlinkat,
};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

const LOCK: &str = "lock";
const MAX_BYTES: usize = 32 * 1024 * 1024;
const SUFFIX: [&str; 3] = [".config", "mezzanine", "bootstrap"];

/// Read-only HOME/config-base witness captured before a vendor root exists.
/// It owns no namespace/key and cannot read or publish another root's state.
pub(super) struct StateHome {
    home: PathBuf,
    anchor: File,
    chain: Vec<File>,
    path: PathBuf,
    base: RootDirectory,
}

impl StateHome {
    /// Captures exact protected HOME/base incarnations or first absence without
    /// creating state, deriving a speculative key, or acquiring writer ownership.
    pub(super) fn inspect(home: &Path) -> Result<Self> {
        let anchor = open_home(home)?;
        let chain = validate_chain(&anchor, None)?;
        let path = SUFFIX
            .iter()
            .fold(home.to_path_buf(), |path, part| path.join(part));
        let owner = Self {
            home: home.into(),
            anchor,
            chain,
            base: RootDirectory::inspect(&path)?,
            path,
        };
        owner.validate()?;
        Ok(owner)
    }

    /// Revalidates every originally existing protected prefix and the captured
    /// base/absence witness; appeared state cannot be implicitly adopted.
    pub(super) fn validate(&self) -> Result<()> {
        revalidate_chain(&self.home, &self.anchor, &self.chain, None)?;
        self.base.validate(&self.path)
    }

    /// Advances absence only for exact shared ancestors created by the held
    /// vendor-root materializer. Every new protected prefix needs its sealed
    /// creation descriptor, while all prior incarnations remain immutable.
    /// Candidate adoption occurs only after complete new witness validation.
    pub(super) fn accept_created(&mut self, created: &CreatedDirectories) -> Result<()> {
        let current = revalidate_chain(&self.home, &self.anchor, &self.chain, None)?;
        if current.len() == self.chain.len() {
            return self.validate();
        }
        let mut spelling = self.home.clone();
        for (index, file) in current.iter().enumerate() {
            spelling.push(SUFFIX[index]);
            if index >= self.chain.len() && !created.matches(&spelling, file)? {
                return Err(MezError::conflict(
                    "bootstrap private state ancestor appeared without creation proof",
                ));
            }
        }
        let candidate = Self {
            home: self.home.clone(),
            anchor: self.anchor.try_clone()?,
            chain: current,
            path: self.path.clone(),
            base: RootDirectory::inspect(&self.path)?,
        };
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    /// Binds a namespace from an actual held vendor directory only after HOME
    /// revalidation. Failed binding leaves this witness reusable but fail-closed;
    /// the returned namespace independently retains the complete exact chain.
    pub(super) fn bind(&self, vendor: &File) -> Result<StateDirectory> {
        self.validate()?;
        let namespace = namespace(vendor)?;
        let chain = revalidate_chain(&self.home, &self.anchor, &self.chain, Some(&namespace))?;
        self.base.validate(&self.path)?;
        let path = self.path.join(namespace);
        let owner = StateDirectory {
            home: self.home.clone(),
            anchor: self.anchor.try_clone()?,
            chain,
            directory: RootDirectory::inspect(&path)?,
            path,
            lock: None,
        };
        owner.validate()?;
        Ok(owner)
    }
}

/// Inspected private namespace with optional acquired cooperating-writer lock.
/// HOME and namespace descriptors fence relocation/replacement independently.
pub(super) struct StateDirectory {
    home: PathBuf,
    anchor: File,
    chain: Vec<File>,
    path: PathBuf,
    directory: RootDirectory,
    lock: Option<File>,
}

/// Validates the Unix owner/mode contract without changing either property.
/// Config ancestors may be readable, but managed state directories may not.
fn validate_directory(file: &File, private: bool) -> Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & if private { 0o077 } else { 0o022 } != 0
    {
        return Err(MezError::forbidden(
            "bootstrap private state directory ownership/mode unavailable",
        ));
    }
    Ok(())
}

/// Captures an existing absolute HOME through the shared no-follow walker.
/// Missing anchors and unsafe paths are not converted into alternate stores.
fn open_home(home: &Path) -> Result<File> {
    let (anchor, _, missing) = walk(home)?;
    if !missing.is_empty() {
        return Err(MezError::invalid_args(
            "bootstrap private state HOME is absent",
        ));
    }
    validate_directory(&anchor, false)?;
    Ok(anchor)
}

/// Traverses only the fixed managed suffix and derived namespace, qualifying
/// every existing parent. NOENT alone denotes absence; no symlink is followed.
fn validate_chain(anchor: &File, namespace: Option<&str>) -> Result<Vec<File>> {
    let mut directory = anchor.try_clone()?;
    let mut chain = Vec::new();
    for (index, part) in SUFFIX.into_iter().chain(namespace).enumerate() {
        let child = match openat(
            &directory,
            part,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(child) => File::from(child),
            Err(rustix::io::Errno::NOENT) => return Ok(chain),
            Err(error) => return Err(std::io::Error::from(error).into()),
        };
        // .config is an OS-selected routing ancestor, not private data. Its
        // exact descriptor is still captured/revalidated; owned non-writable
        // Mez directories and private bootstrap/namespace leaves remain required.
        if index != 0 {
            validate_directory(&child, index >= 2)?;
        }
        chain.push(child.try_clone()?);
        directory = child;
    }
    Ok(chain)
}

/// Reopens and compares protected HOME and every retained prefix descriptor.
/// Optional actual namespace extension never substitutes an existing ancestor;
/// callers separately check their exact base/final absence witness.
fn revalidate_chain(
    home: &Path,
    anchor: &File,
    captured: &[File],
    namespace: Option<&str>,
) -> Result<Vec<File>> {
    let current = open_home(home)?;
    let held = anchor.metadata()?;
    let observed = current.metadata()?;
    if held.dev() != observed.dev() || held.ino() != observed.ino() {
        return Err(MezError::conflict("bootstrap private state HOME replaced"));
    }
    let current = validate_chain(&current, namespace)?;
    if current.len() < captured.len() {
        return Err(MezError::conflict(
            "bootstrap private state ancestor disappeared",
        ));
    }
    for (held, observed) in captured.iter().zip(&current) {
        let held = held.metadata()?;
        let observed = observed.metadata()?;
        if held.dev() != observed.dev() || held.ino() != observed.ino() {
            return Err(MezError::conflict(
                "bootstrap private state ancestor replaced",
            ));
        }
    }
    Ok(current)
}

/// Derives an opaque shared namespace from actual held vendor-root metadata.
/// Path aliases coordinate together; different root incarnations never collide
/// by spelling alone. The caller still owns vendor-root path revalidation.
fn namespace(vendor: &File) -> Result<String> {
    let metadata = vendor.metadata()?;
    if !metadata.is_dir() {
        return Err(MezError::invalid_args(
            "bootstrap state vendor root must be directory",
        ));
    }
    let mut hash = Sha256::new();
    hash.update(b"mezzanine.bootstrap.root.v1\0");
    hash.update(rustix::process::geteuid().as_raw().to_be_bytes());
    hash.update(metadata.dev().to_be_bytes());
    hash.update(metadata.ino().to_be_bytes());
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Restricts internal state files to finite flat names outside staging/lock
/// namespaces. No absolute, traversal, control, or Unicode spelling is admitted.
/// Reserved ASCII aliases reject even on case-insensitive macOS volumes.
fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 192
        || name.eq_ignore_ascii_case(LOCK)
        || !name.as_bytes()[0].is_ascii_alphanumeric()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        || name.contains("..")
    {
        return Err(MezError::invalid_args(
            "bootstrap private state filename unavailable",
        ));
    }
    Ok(())
}

/// Checks finite regular single-link user-private file evidence before I/O.
fn validate_file(file: &File, limit: usize) -> Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
        || metadata.len() > limit as u64
    {
        return Err(MezError::forbidden(
            "bootstrap private state file must be bounded, private, owned and single-link regular",
        ));
    }
    Ok(())
}

impl StateDirectory {
    /// Exposes only the isolated fixture path for source-level recovery tests.
    /// Production callers use descriptor operations, not this pathname.
    #[cfg(test)]
    pub(super) fn fixture_path(&self) -> &Path {
        &self.path
    }

    /// Requires currently valid acquired writer ownership, including live
    /// lock-entry and every captured directory incarnation. Inspection alone
    /// never supplies publication authority to an integrating publisher.
    pub(super) fn require_ownership(&self) -> Result<()> {
        if self.lock.is_none() {
            return Err(MezError::invalid_state(
                "bootstrap private state publication requires lock",
            ));
        }
        self.validate()
    }

    /// Inspects a fixed HOME-relative namespace for a held existing vendor root.
    /// Does not create HOME/config/state directories, files or writer ownership.
    pub(super) fn inspect(home: &Path, vendor: &File) -> Result<Self> {
        StateHome::inspect(home)?.bind(vendor)
    }

    /// Revalidates HOME incarnation, every protected ancestor, and the exact
    /// namespace/first-missing-entry witness before using held descriptors.
    fn validate(&self) -> Result<()> {
        self.current_chain()?;
        if let Some(held) = &self.lock {
            validate_file(held, 0)?;
            let current = File::from(
                openat(
                    self.directory.file()?,
                    LOCK,
                    OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(std::io::Error::from)?,
            );
            validate_file(&current, 0)?;
            let held = held.metadata()?;
            let current = current.metadata()?;
            if held.dev() != current.dev() || held.ino() != current.ino() {
                return Err(MezError::conflict("bootstrap private state lock replaced"));
            }
        }
        Ok(())
    }

    /// Reopens the protected chain and compares every captured incarnation.
    /// An owned suffix may extend it only after materialization has updated the
    /// independent absence witness; replacing any existing prefix still fails.
    fn current_chain(&self) -> Result<Vec<File>> {
        let namespace = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                MezError::invalid_state("bootstrap private state namespace unavailable")
            })?;
        let current = revalidate_chain(&self.home, &self.anchor, &self.chain, Some(namespace))?;
        self.directory.validate(&self.path)?;
        Ok(current)
    }

    /// Materializes only the captured missing suffix and acquires an exclusive
    /// nonwaiting private lock. Creation conflicts do not adopt appeared state.
    pub(super) fn acquire(&mut self) -> Result<()> {
        self.validate()?;
        if self.lock.is_some() {
            return Ok(());
        }
        self.directory.materialize(&self.path)?;
        self.chain = self.current_chain()?;
        self.validate()?;
        let lock = File::from(
            openat(
                self.directory.file()?,
                LOCK,
                OFlags::RDWR
                    | OFlags::CREATE
                    | OFlags::NOFOLLOW
                    | OFlags::NONBLOCK
                    | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(std::io::Error::from)?,
        );
        validate_file(&lock, 0)?;
        flock(&lock, FlockOperation::NonBlockingLockExclusive).map_err(|error| {
            MezError::conflict(format!("bootstrap private state lock unavailable: {error}"))
        })?;
        self.lock = Some(lock);
        self.validate()
    }

    /// Reads a bounded private regular file promptly without creating state.
    /// Caller-selected limits can only narrow the global finite storage ceiling.
    pub(super) fn read(&self, name: &str, limit: usize) -> Result<Option<Vec<u8>>> {
        validate_name(name)?;
        if limit > MAX_BYTES {
            return Err(MezError::invalid_args("bootstrap private state byte limit"));
        }
        self.validate()?;
        if self.directory.is_missing() {
            return Ok(None);
        }
        let descriptor = match openat(
            self.directory.file()?,
            name,
            OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(descriptor) => descriptor,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(error) => return Err(std::io::Error::from(error).into()),
        };
        let mut file = File::from(descriptor);
        validate_file(&file, limit)?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > limit {
            return Err(MezError::invalid_args(
                "bootstrap private state grew beyond byte limit",
            ));
        }
        self.validate()?;
        Ok(Some(bytes))
    }

    /// Publishes one exact private state transition under acquired ownership.
    /// New bytes are synced before same-directory publication; additions use
    /// NOREPLACE, existing bytes/deletions require exact inspected preimages.
    /// Content admission/compiled authority belong to the installer, not here.
    pub(super) fn publish(
        &self,
        name: &str,
        before: Option<&[u8]>,
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<()> {
        if self.lock.is_none() {
            return Err(MezError::invalid_state(
                "bootstrap private state publication requires lock",
            ));
        }
        if limit > MAX_BYTES
            || before.is_some_and(|bytes| bytes.len() > limit)
            || after.is_some_and(|bytes| bytes.len() > limit)
        {
            return Err(MezError::invalid_args("bootstrap private state byte limit"));
        }
        if self.read(name, limit)?.as_deref() != before {
            return Err(MezError::conflict(
                "bootstrap private state preimage changed",
            ));
        }
        if before == after {
            return Ok(());
        }
        let parent = self.directory.file()?;
        let Some(bytes) = after else {
            self.validate()?;
            unlinkat(parent, name, AtFlags::empty()).map_err(std::io::Error::from)?;
            parent.sync_all()?;
            return Ok(());
        };
        let stage = format!(
            ".stage-{}",
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
        let result = (|| -> Result<()> {
            validate_file(&file, limit)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            self.validate()?;
            if self.read(name, limit)?.as_deref() != before {
                return Err(MezError::conflict(
                    "bootstrap private state preimage changed before commit",
                ));
            }
            if before.is_some() {
                renameat(parent, stage.as_str(), parent, name).map_err(std::io::Error::from)?;
            } else {
                renameat_with(parent, stage.as_str(), parent, name, RenameFlags::NOREPLACE)
                    .map_err(std::io::Error::from)?;
            }
            parent.sync_all()?;
            Ok(())
        })();
        let _ = unlinkat(parent, stage.as_str(), AtFlags::empty());
        result
    }
}

impl Drop for StateDirectory {
    /// Explicit unlock avoids a transient inherited open description retaining
    /// ownership after its writer drops; inspection holders never unlock peers.
    fn drop(&mut self) {
        if let Some(lock) = &self.lock {
            let _ = flock(lock, FlockOperation::Unlock);
        }
    }
}

#[cfg(test)]
mod tests;
