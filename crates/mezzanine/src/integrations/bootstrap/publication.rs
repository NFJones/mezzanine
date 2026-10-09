//! Descriptor-relative bounded publication and forward recovery for owned edits.
//!
//! Read-only inspection holds root handles without lock creation/acquisition.
//! Publication/recovery explicitly acquire a private cooperating-installer lock.
//! A synced journal owns
//! exact before/after bytes before any destination is changed. Recovery finishes
//! only when every current preimage equals one of those states; foreign edits
//! leave the journal intact. This is not atomic CAS against arbitrary writers.
//! No symlink or special node is followed, and no executable is launched.
//! The explicit private path retains both cooperating lock domains
//! and migrates only compiled-authorized exact legacy intent. Identical copies
//! qualify interrupted migration; disagreements/location drift fence planning.
//! Every compiled public CLI intent uses the private owner; vendor eligibility
//! and archive migration remain independent follow-up work. Version 3 qualifies
//! fixed logical private receipt effects; version 2 retains vendor destinations.

use super::reconciliation::publication_path;
use super::root_directory::RootDirectory;
use super::state_directory::{StateDirectory, StateHome};
use crate::error::{MezError, Result};
use rustix::fs::{
    AtFlags, FlockOperation, Mode, OFlags, RenameFlags, flock, mkdirat, openat, renameat,
    renameat_with, unlinkat,
};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

const JOURNAL: &str = ".mez-bootstrap-journal";
const LOCK: &str = ".mez-bootstrap-lock";
const PRIVATE_JOURNAL: &str = "journal.json";
const PRIVATE_RECEIPT_PREFIX: &str = "@mez-bootstrap-receipt/";
const MAX_BYTES: usize = 1024 * 1024;
const MAX_JOURNAL: usize = 32 * 1024 * 1024;

/// Builds a fixed logical private receipt target, never a vendor-relative path.
/// The harness is bounded inert compiled identity, not an arbitrary filename.
pub(super) fn private_receipt_path(harness: &str) -> Result<String> {
    let path = format!("{PRIVATE_RECEIPT_PREFIX}{harness}");
    private_receipt_name(&path)?;
    Ok(path)
}

/// Resolves only the reserved private receipt namespace to a flat state name.
/// Unknown suffixes cannot traverse or collide with lock/journal/staging files.
fn private_receipt_name(path: &str) -> Result<Option<String>> {
    let Some(harness) = path.strip_prefix(PRIVATE_RECEIPT_PREFIX) else {
        return Ok(None);
    };
    if harness.is_empty()
        || harness.len() > 32
        || !harness
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'-')
    {
        return Err(MezError::invalid_args(
            "bootstrap private receipt target unavailable",
        ));
    }
    Ok(Some(format!("ownership-{harness}.json")))
}

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

/// Durable source witness; identical dual copies represent interrupted migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JournalLocation {
    Legacy,
    Private,
    Both,
}

/// Exact bytes and location of the accepted pending intent. Location is part
/// of the snapshot fence, not authority to add paths or reinterpret operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PendingRecovery {
    bytes: Vec<u8>,
    location: JournalLocation,
    pub(super) changes: Vec<Change>,
}

/// Held root, with optional exclusive ownership acquired only for publication.
pub(super) struct Publisher {
    root: PathBuf,
    directory: RootDirectory,
    _lock: Option<File>,
    state: Option<StateDirectory>,
    unbound_home: Option<StateHome>,
    /// Task-local publication barrier; no shared fault state.
    #[cfg(test)]
    pub(super) stop_after: std::cell::Cell<Option<usize>>,
    /// Task-local failure after durable private copy, before legacy removal.
    #[cfg(test)]
    pub(super) stop_after_migration_copy: std::cell::Cell<bool>,
    /// Task-local journal replacement seam after effects and before settlement.
    #[cfg(test)]
    pub(super) before_journal_removal: std::cell::RefCell<Option<Box<dyn FnOnce()>>>,
    /// Task-local drift after root materialization but before private binding.
    #[cfg(test)]
    pub(super) before_private_binding: std::cell::RefCell<Option<Box<dyn FnOnce()>>>,
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
    /// Reports private publication routing; unbound HOME still means private
    /// authority, never permission to fall through to a vendor-state target.
    pub(super) fn uses_private_state(&self) -> bool {
        self.state.is_some() || self.unbound_home.is_some()
    }

    /// Holds a no-follow root for read-only inspection. No lock, journal,
    /// artifact parent or other filesystem state is created or acquired.
    pub(super) fn inspect(root: &Path) -> Result<Self> {
        Ok(Self {
            root: root.into(),
            directory: RootDirectory::inspect(root)?,
            _lock: None,
            state: None,
            unbound_home: None,
            #[cfg(test)]
            stop_after: std::cell::Cell::new(None),
            #[cfg(test)]
            stop_after_migration_copy: std::cell::Cell::new(false),
            #[cfg(test)]
            before_journal_removal: std::cell::RefCell::new(None),
            #[cfg(test)]
            before_private_binding: std::cell::RefCell::new(None),
        })
    }

    /// Captures private HOME/base state without writing either tree. Existing
    /// vendor roots bind immediately; absent roots retain a key-free witness
    /// until publication materializes the actual root. All compiled CLI intents
    /// share this owner; vendor-directory policy migration remains separate.
    pub(super) fn inspect_private(root: &Path, home: &Path) -> Result<Self> {
        let mut publisher = Self::inspect(root)?;
        if publisher.directory.is_missing() {
            publisher.unbound_home = Some(StateHome::inspect(home)?);
        } else {
            publisher.state = Some(StateDirectory::inspect(home, publisher.directory.file()?)?);
        }
        Ok(publisher)
    }

    /// Acquires a private lock for an existing root; recovery never creates an
    /// absent tree merely to discover that no accepted intent is present.
    pub(super) fn open(root: &Path) -> Result<Self> {
        let mut publisher = Self::inspect(root)?;
        if publisher.directory.is_missing() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "bootstrap recovery/publication root is absent",
            )
            .into());
        }
        publisher.acquire_lock()?;
        Ok(publisher)
    }

    /// Converts an inspected root to one bounded cooperating writer. The held
    /// root must still match its spelling before and after lock acquisition;
    /// an absent root is materialized only along its captured native suffix.
    /// no destination/journal work is authorized until this returns successfully.
    pub(super) fn acquire_lock(&mut self) -> Result<()> {
        self.validate_root()?;
        if self._lock.is_some() {
            return self.require_lock();
        }
        let created = self.directory.materialize_witnessed(&self.root)?;
        #[cfg(test)]
        if let Some(barrier) = self.before_private_binding.borrow_mut().take() {
            barrier();
        }
        if let Some(home) = &mut self.unbound_home {
            home.accept_created(&created)?;
            self.state = Some(home.bind(self.directory.file()?)?);
            self.unbound_home = None;
        }
        if let Some(state) = &mut self.state {
            state.acquire()?;
        }
        let lock = File::from(
            openat(
                self.directory.file()?,
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
            || metadata.len() != 0
        {
            return Err(MezError::forbidden(
                "bootstrap lock must be private and owned",
            ));
        }
        flock(&lock, FlockOperation::NonBlockingLockExclusive).map_err(|error| {
            MezError::conflict(format!("bootstrap installer lock unavailable: {error}"))
        })?;
        self._lock = Some(lock);
        self.require_lock()
    }

    /// Read-only holders cannot accidentally publish/recover through the shared
    /// descriptor helpers. Mutation requires explicitly acquired writer ownership.
    fn require_lock(&self) -> Result<()> {
        if self._lock.is_none() {
            return Err(MezError::invalid_state(
                "bootstrap publication requires installer lock",
            ));
        }
        self.validate_root()?;
        if let Some(state) = &self.state {
            state.require_ownership()?;
        }
        let held = self
            ._lock
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("bootstrap installer lock unavailable"))?;
        let current = File::from(
            openat(
                self.directory.file()?,
                LOCK,
                OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(std::io::Error::from)?,
        );
        let held = held.metadata()?;
        let current = current.metadata()?;
        if !current.is_file()
            || current.nlink() != 1
            || current.uid() != rustix::process::geteuid().as_raw()
            || current.mode() & 0o077 != 0
            || current.len() != 0
            || held.nlink() != 1
            || held.dev() != current.dev()
            || held.ino() != current.ino()
        {
            return Err(MezError::conflict("bootstrap installer lock changed"));
        }
        Ok(())
    }

    /// Reads both possible durable locations without mutation. Identical copies
    /// are a supported interrupted migration; disagreement never picks a winner.
    fn journal_bytes(&self) -> Result<Option<(Vec<u8>, JournalLocation)>> {
        self.validate_root()?;
        if self.directory.is_missing() {
            return Ok(None);
        }
        let legacy = read_at(self.directory.file()?, JOURNAL, MAX_JOURNAL)?;
        let private = self
            .state
            .as_ref()
            .map(|state| state.read(PRIVATE_JOURNAL, MAX_JOURNAL))
            .transpose()?
            .flatten();
        match (legacy, private) {
            (None, None) => Ok(None),
            (Some(bytes), None) => Ok(Some((bytes, JournalLocation::Legacy))),
            (None, Some(bytes)) => Ok(Some((bytes, JournalLocation::Private))),
            (Some(legacy), Some(private)) if legacy == private => {
                Ok(Some((private, JournalLocation::Both)))
            }
            _ => Err(MezError::conflict(
                "bootstrap journal copies disagree; no mutation",
            )),
        }
    }

    /// Revalidates exact accepted bytes/location, not merely valid JSON intent.
    fn require_journal(&self, bytes: &[u8], location: JournalLocation) -> Result<()> {
        if self
            .journal_bytes()?
            .as_ref()
            .map(|(current, source)| (current.as_slice(), *source))
            != Some((bytes, location))
        {
            return Err(MezError::conflict(
                "bootstrap accepted journal changed; no publication",
            ));
        }
        Ok(())
    }

    /// Migrates only an already compiled-authorized exact snapshot under both
    /// writer domains. The private copy is durable before removing legacy;
    /// identical dual copies remain recoverable after interruption at that edge.
    fn migrate_journal(&self, pending: &PendingRecovery) -> Result<JournalLocation> {
        self.require_lock()?;
        self.require_journal(&pending.bytes, pending.location)?;
        let Some(state) = &self.state else {
            return Ok(pending.location);
        };
        if pending.location == JournalLocation::Legacy {
            state.publish(PRIVATE_JOURNAL, None, Some(&pending.bytes), MAX_JOURNAL)?;
            #[cfg(test)]
            if self.stop_after_migration_copy.replace(false) {
                return Err(MezError::invalid_state(
                    "injected bootstrap private copy interruption",
                ));
            }
        }
        if pending.location != JournalLocation::Private {
            self.require_lock()?;
            self.require_journal(&pending.bytes, JournalLocation::Both)?;
            unlinkat(self.directory.file()?, JOURNAL, AtFlags::empty())
                .map_err(std::io::Error::from)?;
            self.directory.file()?.sync_all()?;
        }
        Ok(JournalLocation::Private)
    }

    /// Revalidates held root spelling before publishing through its descriptors.
    fn validate_root(&self) -> Result<()> {
        self.directory.validate(&self.root)?;
        if let Some(home) = &self.unbound_home {
            home.validate()?;
        }
        Ok(())
    }

    /// Resolves a relative parent with no-follow handles, optionally creating it.
    fn parent(&self, path: &str, create: bool) -> Result<Option<(File, String)>> {
        publication_path(path)?;
        self.validate_root()?;
        if self.directory.is_missing() {
            return Ok(None);
        }
        let mut directory = self.directory.file()?.try_clone()?;
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
        if let Some(name) = private_receipt_name(path)? {
            self.validate_root()?;
            if let Some(state) = &self.state {
                return state.read(&name, MAX_BYTES);
            }
            if self.unbound_home.is_some() {
                return Ok(None);
            }
            return Err(MezError::invalid_state(
                "bootstrap private receipt requires private owner",
            ));
        }
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
        if changes.is_empty() || changes.len() > 17 {
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
    fn finish(&self, changes: &[Change], bytes: &[u8], location: JournalLocation) -> Result<()> {
        self.require_lock()?;
        self.require_journal(bytes, location)?;
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
            if let Some(name) = private_receipt_name(&change.path)? {
                self.require_lock()?;
                let state = self.state.as_ref().ok_or_else(|| {
                    MezError::invalid_state("bootstrap private receipt owner unavailable")
                })?;
                state.publish(
                    &name,
                    change.before.as_deref(),
                    change.after.as_deref(),
                    MAX_BYTES,
                )?;
            } else {
                let Some((parent, name)) = self.parent(&change.path, change.after.is_some())?
                else {
                    return Err(MezError::conflict(
                        "bootstrap destination parent unavailable",
                    ));
                };
                if read_at(&parent, &name, MAX_BYTES)? != change.before {
                    return Err(MezError::conflict(
                        "bootstrap preimage changed before commit",
                    ));
                }
                self.require_lock()?;
                match &change.after {
                    Some(bytes) => self.write_at(&parent, &name, bytes, change.before.is_some())?,
                    None => {
                        unlinkat(&parent, name.as_str(), AtFlags::empty())
                            .map_err(std::io::Error::from)?;
                        parent.sync_all()?;
                    }
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
        #[cfg(test)]
        if let Some(barrier) = self.before_journal_removal.borrow_mut().take() {
            barrier();
        }
        self.require_lock()?;
        self.require_journal(bytes, location)?;
        if let Some(state) = &self.state {
            state.publish(PRIVATE_JOURNAL, Some(bytes), None, MAX_JOURNAL)?;
        } else {
            unlinkat(self.directory.file()?, JOURNAL, AtFlags::empty())
                .map_err(std::io::Error::from)?;
            self.directory.file()?.sync_all()?;
        }
        Ok(())
    }

    /// Recovers only an explicitly accepted durable journal, without new planning.
    pub(super) fn recover_authorized(
        &self,
        authorize: impl FnOnce(u32, &serde_json::Value, &[Change]) -> Result<()>,
    ) -> Result<bool> {
        self.require_lock()?;
        let Some(pending) = self.inspect_pending_authorized(authorize)? else {
            return Ok(false);
        };
        let location = self.migrate_journal(&pending)?;
        self.finish(&pending.changes, &pending.bytes, location)?;
        Ok(true)
    }

    /// Read-only recovery preview verifies the same accepted root/version,
    /// compiled intent and actual before/after states as recovery. It neither
    /// creates a missing root nor acquires a writer lock or finishes effects.
    pub(super) fn inspect_recovery_authorized(
        &self,
        authorize: impl FnOnce(u32, &serde_json::Value, &[Change]) -> Result<()>,
    ) -> Result<Option<Vec<Change>>> {
        Ok(self
            .inspect_pending_authorized(authorize)?
            .map(|pending| pending.changes))
    }

    /// Captures exact journal bytes only after compiled intent and physical
    /// before/after validation. No filesystem writes or lock acquisition occur.
    pub(super) fn inspect_pending_authorized(
        &self,
        authorize: impl FnOnce(u32, &serde_json::Value, &[Change]) -> Result<()>,
    ) -> Result<Option<PendingRecovery>> {
        self.validate_root()?;
        if self.directory.is_missing() {
            return Ok(None);
        }
        let Some((bytes, location)) = self.journal_bytes()? else {
            return Ok(None);
        };
        let journal: Journal = serde_json::from_slice(&bytes).map_err(|_| {
            MezError::invalid_state("bootstrap journal invalid; manual review required")
        })?;
        let root = self.directory.file()?.metadata()?;
        self.validate_root()?;
        if !matches!(journal.version, 2 | 3)
            || journal.root_device != root.dev()
            || journal.root_inode != root.ino()
        {
            return Err(MezError::invalid_state(
                "bootstrap journal version/root mismatch",
            ));
        }
        if journal.version == 2
            && (journal.changes.len() > 16
                || journal
                    .changes
                    .iter()
                    .any(|change| change.path.starts_with(PRIVATE_RECEIPT_PREFIX)))
        {
            return Err(MezError::forbidden(
                "bootstrap version-2 journal cannot authorize private receipt targets",
            ));
        }
        if journal.version == 3 && !self.uses_private_state() {
            return Err(MezError::forbidden(
                "bootstrap version-3 journal requires private owner",
            ));
        }
        authorize(journal.version, &journal.intent, &journal.changes)?;
        self.validate_changes(&journal.changes, true)?;
        Ok(Some(PendingRecovery {
            bytes,
            location,
            changes: journal.changes,
        }))
    }

    /// Reauthorizes the identical inspected journal under writer ownership,
    /// then finishes its original operation without releasing the lock.
    pub(super) fn recover_pending_authorized(
        &self,
        pending: &PendingRecovery,
        authorize: impl FnOnce(u32, &serde_json::Value, &[Change]) -> Result<()>,
    ) -> Result<()> {
        self.require_lock()?;
        if self.inspect_pending_authorized(authorize)?.as_ref() != Some(pending) {
            return Err(MezError::conflict(
                "bootstrap inspected recovery changed; no publication",
            ));
        }
        let location = self.migrate_journal(pending)?;
        self.finish(&pending.changes, &pending.bytes, location)
    }

    /// Commits an already planned transaction; never silently recovers old intent.
    pub(super) fn require_no_pending_journal(&self) -> Result<()> {
        self.validate_root()?;
        if self.directory.is_missing() {
            return Ok(());
        }
        if self.journal_bytes()?.is_some() {
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
        let root = self.directory.file()?.metadata()?;
        let bytes = serde_json::to_vec(&Journal {
            version: if self.uses_private_state() { 3 } else { 2 },
            root_device: root.dev(),
            root_inode: root.ino(),
            intent,
            changes: changes.clone(),
        })
        .map_err(|error| MezError::invalid_state(format!("bootstrap journal encoding: {error}")))?;
        if bytes.len() > MAX_JOURNAL {
            return Err(MezError::invalid_args("bootstrap journal limit"));
        }
        let location = if let Some(state) = &self.state {
            state.publish(PRIVATE_JOURNAL, None, Some(&bytes), MAX_JOURNAL)?;
            JournalLocation::Private
        } else {
            self.write_at(self.directory.file()?, JOURNAL, &bytes, false)?;
            JournalLocation::Legacy
        };
        self.finish(&changes, &bytes, location)
    }

    /// Test-only raw transaction fixture; production always supplies manifest intent.
    #[cfg(test)]
    pub(super) fn apply(&self, changes: Vec<Change>) -> Result<()> {
        self.apply_authorized(changes, serde_json::Value::Null)
    }

    /// Test-only publication recovery isolates filesystem behavior from manifest admission.
    #[cfg(test)]
    pub(super) fn recover(&self) -> Result<bool> {
        self.recover_authorized(|_, intent, _| {
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
