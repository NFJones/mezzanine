//! Lifetime exclusion for cooperating runtime custody roots.
//!
//! SQLite serializes individual transactions, but an owned custody attempt can
//! span several transactions and continue after its observer disappears. This
//! private lock fences other runtime roots until the actual owner and its
//! workers retire. It does not grant delivery authority, validate recovery, or
//! prevent direct use of the lower-level multi-connection store API. The store
//! directory remains trusted against replacement by the same user.
//! Final-owner destruction explicitly unlocks the original file description:
//! incidental fork/dup aliases must not retain an ended ownership scope. Real
//! workers retain this owner, not independently cloned lock descriptors.

use std::fs::{self, File};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rustix::fs::{FlockOperation, Mode, OFlags, flock, openat};

use super::acceptance::AcceptancePlan;
use super::acknowledgement::GuardedAckSettlement;
use super::{
    FileIdentity, MmpCustodyStore, open_private_directory, validate_directory_path,
    validate_private_path,
};
use crate::error::{MezError, Result};

/// A cooperating runtime root's exclusive, kernel-owned lifetime fence.
pub(crate) struct RootOwnership {
    /// Explicitly unlocked at final-owner destruction, then closed. Process exit
    /// also closes descriptors without depending on Rust cleanup.
    _lock: File,
    /// Original private directory held open to prevent directory-inode reuse.
    directory: File,
    /// Trusted destination pathname checked before every store binding.
    root: PathBuf,
    /// First bound store allocation and database inode; never refreshed on retry.
    bound: OnceLock<BoundStore>,
    /// Actual clock capability survives worker/store loss under this same root.
    clock: OnceLock<Result<std::sync::Arc<super::elapsed_clock::CustodyElapsedClock>>>,
    clock_source: super::elapsed_clock::ClockSource,
    /// Shared by every root clone and held result, never reset on worker reopen.
    pub(super) rewrite_reserved: std::sync::atomic::AtomicUsize,
    /// Explicit qualification is sticky for this actual owner, not all legacy
    /// roots whose canonical archives may share the parent directory.
    physical_qualified: std::sync::atomic::AtomicBool,
}

impl Drop for RootOwnership {
    /// All actual worker owners and borrowed stores have retired before this
    /// runs. Unlock the held description, never a reopened pathname; otherwise
    /// an incidental pre-exec fork/dup can extend a completed root's exclusion.
    /// Failure remains fail-closed: closing still releases this descriptor, and
    /// a new root must independently acquire the kernel lock before any work.
    fn drop(&mut self) {
        let _ = flock(&self._lock, FlockOperation::Unlock);
    }
}

/// Original database allocation captured once under this actual lock.
struct BoundStore {
    /// Persisted random allocation identity validated in the original store.
    store_id: Vec<u8>,
    /// Database file identity captured at the original open.
    database_identity: FileIdentity,
}

/// Exclusive borrow tying one store to its live, matching root ownership.
/// It is not an acceptance, noncommit, recipient or restart-recovery right.
pub(crate) struct OwnedCustodyStore<'a> {
    /// The actual kernel lock cannot be dropped while this borrow is alive.
    _owner: &'a RootOwnership,
    /// Only this worker can access the bound store during the borrow.
    store: &'a mut MmpCustodyStore,
}

impl OwnedCustodyStore<'_> {
    /// Establishes the dedicated flat-root envelope before a rewrite result can
    /// be issued. False is expected initial layout/capacity refusal; errors are
    /// owner/path/pager/I/O failures. Success makes subsequent boundaries enforce
    /// this policy through worker loss, with no downgrade to expected refusal.
    pub(crate) fn qualify_physical_capacity(&mut self) -> Result<bool> {
        self.revalidate()?;
        super::physical_capacity::validate_connection(self.store)?;
        if !super::physical_capacity::qualification_paths(
            &self._owner.root,
            &self._owner.rewrite_reserved,
        )? {
            self.revalidate()?;
            return Ok(false);
        }
        self._owner
            .physical_qualified
            .store(true, std::sync::atomic::Ordering::Release);
        self.revalidate()?;
        Ok(true)
    }

    /// Establishes actual suspend-inclusive time only on an empty original.
    /// The live root retains both success and failure through every later worker.
    pub(crate) fn initialize_elapsed_clock(&mut self) -> Result<()> {
        self.revalidate()?;
        let reader = self
            ._owner
            .clock
            .get_or_init(|| {
                self.store
                    .qualify_elapsed_clock(self._owner.clock_source.capture()?)
            })
            .as_ref()
            .map_err(Clone::clone)?
            .clone();
        self.store.attach_elapsed_clock(reader)?;
        self.revalidate()
    }

    /// Checks an exact original under this live owner before considering a
    /// yield. True is only a snapshot observation, not a transferable receipt;
    /// subsequent work must retain the owner and fence the same store position.
    pub(crate) fn acceptance_never_written(&mut self, plan: &AcceptancePlan) -> Result<bool> {
        self.revalidate()?;
        let result = self.store.acceptance_never_written(plan)?;
        self.revalidate()?;
        Ok(result)
    }

    /// Rechecks the exact committed ACK batch and suspended original in one
    /// store snapshot under this same live root, then returns its fenced plan.
    pub(crate) fn verify_guarded_ack_settlement(
        &mut self,
        original: &AcceptancePlan,
        settlement: &GuardedAckSettlement,
    ) -> Result<AcceptancePlan> {
        self.revalidate()?;
        if !settlement.matches(self.store, original) {
            return Err(MezError::conflict("MMP guarded ACK original changed"));
        }
        let transaction = self
            .store
            .connection
            .unchecked_transaction()
            .map_err(super::database_error)?;
        let state = self.store.checked_state(&transaction)?;
        if u64::try_from(state.generation).ok() != Some(settlement.generation())
            || state.batch != settlement.batch()
            || !self.store.acceptance_never_written_in_snapshot(
                &transaction,
                original,
                settlement.generation(),
            )?
        {
            return Err(MezError::conflict("MMP guarded ACK settlement changed"));
        }
        self.store.finish_transaction(transaction)?;
        self.revalidate()?;
        Ok(original.at_generation(settlement.generation()))
    }

    /// Borrows the already-bound store for this synchronous worker phase.
    pub(crate) fn store_mut(&mut self) -> &mut MmpCustodyStore {
        self.store
    }

    /// Rechecks the live owner and original store binding at a worker boundary.
    pub(crate) fn revalidate(&mut self) -> Result<()> {
        let _bound = self._owner.bind_store(self.store)?;
        Ok(())
    }
}

impl RootOwnership {
    /// Retains a deterministic native-clock stand-in in this fixture's actual
    /// kernel owner. Saved metadata never calls or constructs this test seam.
    #[cfg(test)]
    pub(crate) fn retain_elapsed_clock_for_tests(
        &self,
        clock: std::sync::Arc<super::elapsed_clock::CustodyElapsedClock>,
    ) {
        assert!(self.clock.set(Ok(clock)).is_ok());
    }

    /// Acquires a nonblocking fence before a runtime root opens its store.
    /// Rejects unsafe lock paths and an already-owned destination.
    pub(crate) fn acquire(root: &Path) -> Result<Self> {
        Self::acquire_policy(root, true, Default::default())
    }

    /// Acquires an already existing private root/lock without creating a new
    /// destination. Missing original custody remains unavailable during recovery.
    pub(crate) fn acquire_existing(root: &Path) -> Result<Self> {
        Self::acquire_policy(root, false, Default::default())
    }

    /// Makes filesystem allocation authority explicit before touching a path.
    pub(crate) fn acquire_with_clock_source(
        root: &Path,
        create: bool,
        clock_source: super::elapsed_clock::ClockSource,
    ) -> Result<Self> {
        Self::acquire_policy(root, create, clock_source)
    }

    fn acquire_policy(
        root: &Path,
        create: bool,
        clock_source: super::elapsed_clock::ClockSource,
    ) -> Result<Self> {
        if create && !root.exists() {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true).mode(0o700);
            builder.create(root)?;
        }
        let directory = open_private_directory(root)?;
        let descriptor = openat(
            &directory,
            "owner.lock",
            OFlags::RDWR
                | OFlags::NOFOLLOW
                | OFlags::NONBLOCK
                | OFlags::CLOEXEC
                | if create {
                    OFlags::CREATE
                } else {
                    OFlags::empty()
                },
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(std::io::Error::from)?;
        let lock = File::from(descriptor);
        let owned = lock.metadata()?;
        if !owned.is_file()
            || owned.nlink() != 1
            || owned.uid() != rustix::process::geteuid().as_raw()
            || owned.mode() & 0o077 != 0
        {
            return Err(MezError::invalid_state(
                "MMP custody owner lock must be a private owned single-link file",
            ));
        }
        match flock(&lock, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => (),
            Err(error) if error == rustix::io::Errno::WOULDBLOCK => {
                return Err(MezError::conflict("MMP custody root already owned"));
            }
            Err(error) => return Err(std::io::Error::from(error).into()),
        }
        let owner = Self {
            _lock: lock,
            directory,
            root: root.to_owned(),
            bound: OnceLock::new(),
            clock: OnceLock::new(),
            clock_source,
            rewrite_reserved: std::sync::atomic::AtomicUsize::new(0),
            physical_qualified: std::sync::atomic::AtomicBool::new(false),
        };
        owner.validate_root()?;
        Ok(owner)
    }

    /// Rejects a replaced directory or lock pathname before opening a store.
    pub(crate) fn validate_root(&self) -> Result<()> {
        validate_directory_path(&self.root, &self.directory)?;
        if self
            .physical_qualified
            .load(std::sync::atomic::Ordering::Acquire)
        {
            super::physical_capacity::validate_paths(&self.root, &self.rewrite_reserved)?;
        }
        let path = self.root.join("owner.lock");
        validate_private_path(&path, false)?;
        let held = self._lock.metadata()?;
        let current = fs::symlink_metadata(&path)?;
        if !held.is_file()
            || held.nlink() != 1
            || held.uid() != rustix::process::geteuid().as_raw()
            || held.permissions().mode() & 0o077 != 0
            || FileIdentity::from_metadata(&held) != FileIdentity::from_metadata(&current)
        {
            return Err(MezError::conflict("MMP custody owner lock path changed"));
        }
        Ok(())
    }

    /// Binds one exact private store to this live lock without trusting labels.
    /// A replaced directory, lock, database inode or persisted allocation fails.
    pub(crate) fn bind_store<'a>(
        &'a self,
        store: &'a mut MmpCustodyStore,
    ) -> Result<OwnedCustodyStore<'a>> {
        self.validate_root()?;
        let held = self.directory.metadata()?;
        let opened = store.directory.metadata()?;
        if FileIdentity::from_metadata(&held) != FileIdentity::from_metadata(&opened) {
            return Err(MezError::conflict("MMP custody store directory changed"));
        }
        let database = self.root.join("custody.sqlite");
        validate_private_path(&database, false)?;
        if FileIdentity::from_metadata(&fs::symlink_metadata(&database)?) != store.database_identity
        {
            return Err(MezError::conflict("MMP custody database path changed"));
        }
        store.checked_state(&store.connection)?;
        if self
            .physical_qualified
            .load(std::sync::atomic::Ordering::Acquire)
        {
            super::physical_capacity::validate_connection(store)?;
        }
        let _ = self.bound.set(BoundStore {
            store_id: store.store_id.clone(),
            database_identity: store.database_identity,
        });
        let bound = self
            .bound
            .get()
            .ok_or_else(|| MezError::invalid_state("MMP custody binding unavailable"))?;
        if bound.store_id != store.store_id || bound.database_identity != store.database_identity {
            return Err(MezError::conflict(
                "MMP custody original store allocation changed",
            ));
        }
        if store.clock_anchor.is_some() && store.elapsed_clock.is_none() {
            let reader = self
                .clock
                .get_or_init(|| store.qualify_elapsed_clock(self.clock_source.capture()?))
                .as_ref()
                .map_err(Clone::clone)?
                .clone();
            store.attach_elapsed_clock(reader)?;
        }
        Ok(OwnedCustodyStore {
            _owner: self,
            store,
        })
    }
}

/// A payload read retains and revalidates the actual original kernel owner.
/// Returned bytes remain descriptive until the lower original publishes them.
impl mez_agent::messaging::MessagePayloadLoadStore for OwnedCustodyStore<'_> {
    fn load(
        &mut self,
        request: &mez_agent::messaging::MessagePayloadLoadWrite,
    ) -> mez_agent::messaging::Result<mez_agent::messaging::MessageAcceptanceSource> {
        let read = (|| -> Result<_> {
            self.revalidate()?;
            let (source, digest, _) = self
                .store
                .read_accepted_source(request.sequence(), request.decode_limits())?;
            self.revalidate()?;
            if digest != request.source_digest() {
                return Err(MezError::invalid_state(
                    "MMP original stored payload fingerprint changed",
                ));
            }
            Ok(source)
        })();
        read.map_err(|error| {
            mez_agent::messaging::MessageError::invalid_state(format!(
                "MMP owned source read: {error}"
            ))
        })
    }
}

impl mez_agent::messaging::MessageStorageSnapshotStore for OwnedCustodyStore<'_> {
    /// Verifies original references under the live kernel fence before and after I/O.
    fn verify(
        &mut self,
        request: &mez_agent::messaging::MessageStorageSnapshotWrite,
    ) -> mez_agent::messaging::Result<mez_agent::messaging::MessageStorageBinding> {
        let result = (|| {
            self.revalidate()?;
            let binding = self.store.storage_snapshot_binding(request)?;
            self.revalidate()?;
            Ok(binding)
        })();
        result.map_err(|e: MezError| {
            mez_agent::messaging::MessageError::invalid_state(format!(
                "MMP checkpoint original verification: {e}"
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::CustodyWrite;
    use super::*;
    use sha2::Digest;
    use std::io::{BufRead, BufReader, Write};
    use std::path::PathBuf;
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;

    const CHILD_ROOT: &str = "MEZ_TEST_MMP_OWNER_CHILD_ROOT";
    const READY: &str = "MMP_OWNER_CHILD_READY";

    /// Generated private directory removed after each ownership fixture.
    struct Directory(PathBuf);

    impl Directory {
        /// Chooses a fresh path without touching any user session directory.
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "mez-mmp-owner-binding-{}-{}",
                std::process::id(),
                rand::random::<u64>()
            )))
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// An incidental fork/dup alias is not a worker owner. It must not retain an
    /// ended root's exclusion, whereas a worker's actual Arc must retain it.
    #[test]
    fn final_root_owner_releases_lock_despite_incidental_descriptor_alias() {
        let fixture = Directory::new();
        let owner = std::sync::Arc::new(RootOwnership::acquire(&fixture.0).unwrap());
        let worker_owner = owner.clone();
        // try_clone shares the same open-file description as a pre-exec fork,
        // removing timing and unrelated child scheduling from this regression.
        let incidental_alias = owner._lock.try_clone().unwrap();
        drop(owner);
        assert!(RootOwnership::acquire_existing(&fixture.0).is_err());
        worker_owner.validate_root().unwrap();
        drop(worker_owner);

        let replacement = RootOwnership::acquire_existing(&fixture.0)
            .expect("an incidental descriptor must not retain a completed root's lock");
        // Closing an old alias cannot unlock the independently opened successor.
        drop(incidental_alias);
        assert!(RootOwnership::acquire_existing(&fixture.0).is_err());
        replacement.validate_root().unwrap();
        drop(replacement);
        assert!(RootOwnership::acquire_existing(&fixture.0).is_ok());
    }

    /// An absent original qualifies only at its exact bound store position.
    /// An unrelated committed write makes the original plan stale, and an
    /// equal owner label on another allocation cannot recreate the observation.
    #[test]
    fn untouched_acceptance_requires_the_live_original_store_and_position() {
        let fixture = Directory::new();
        let owner = RootOwnership::acquire(&fixture.0).unwrap();
        let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
        let original = store.acceptance_plan(1).unwrap();
        assert!(
            owner
                .bind_store(&mut store)
                .unwrap()
                .acceptance_never_written(&original)
                .unwrap()
        );
        store
            .commit(
                0,
                &[CustodyWrite {
                    key: "fixture",
                    previous: None,
                    value: Some(b"later"),
                }],
            )
            .unwrap();
        assert!(
            !owner
                .bind_store(&mut store)
                .unwrap()
                .acceptance_never_written(&original)
                .unwrap()
        );
        let other_fixture = Directory::new();
        let other_owner = RootOwnership::acquire(&other_fixture.0).unwrap();
        let mut other = MmpCustodyStore::open(&other_fixture.0, "session").unwrap();
        assert!(
            other_owner
                .bind_store(&mut other)
                .unwrap()
                .acceptance_never_written(&original)
                .is_err()
        );
    }

    /// A removed stage still advances the durable watermark, so an empty
    /// namespace cannot qualify a reused sequence after intervening history.
    #[test]
    fn removed_stage_never_qualifies_an_old_sequence_as_unwritten() {
        let fixture = Directory::new();
        let owner = RootOwnership::acquire(&fixture.0).unwrap();
        let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
        let key = "a1:0000000000000002:stage";
        let first = store
            .commit(
                0,
                &[CustodyWrite {
                    key,
                    previous: None,
                    value: Some(b"stage"),
                }],
            )
            .unwrap();
        let digest = sha2::Sha256::digest(b"stage");
        store
            .commit(
                first.generation(),
                &[CustodyWrite {
                    key,
                    previous: Some(&digest),
                    value: None,
                }],
            )
            .unwrap();
        let reused = store.acceptance_plan(1).unwrap();
        assert!(
            !owner
                .bind_store(&mut store)
                .unwrap()
                .acceptance_never_written(&reused)
                .unwrap()
        );
        let fresh = store.acceptance_plan(3).unwrap();
        assert!(
            owner
                .bind_store(&mut store)
                .unwrap()
                .acceptance_never_written(&fresh)
                .unwrap()
        );
    }

    /// A migrated prototype store has no proof of removed old reservations.
    /// Even an empty namespace under its real lock cannot authorize a yield.
    #[test]
    fn migrated_untrusted_watermark_cannot_qualify_unwritten_acceptance() {
        let fixture = Directory::new();
        let owner = RootOwnership::acquire(&fixture.0).unwrap();
        drop(MmpCustodyStore::open(&fixture.0, "session").unwrap());
        let connection = rusqlite::Connection::open(fixture.0.join("custody.sqlite")).unwrap();
        connection.execute_batch("BEGIN IMMEDIATE; ALTER TABLE state RENAME TO state_v2; CREATE TABLE state (singleton INTEGER PRIMARY KEY CHECK(singleton=1), owner TEXT NOT NULL, store_id BLOB NOT NULL CHECK(length(store_id)=16), generation INTEGER NOT NULL CHECK(generation>=0), batch BLOB NOT NULL); INSERT INTO state SELECT singleton,owner,store_id,generation,batch FROM state_v2; DROP TABLE state_v2; PRAGMA user_version=1; COMMIT;").unwrap();
        drop(connection);
        let mut migrated = MmpCustodyStore::open(&fixture.0, "session").unwrap();
        let plan = migrated.acceptance_plan(1).unwrap();
        assert!(
            !owner
                .bind_store(&mut migrated)
                .unwrap()
                .acceptance_never_written(&plan)
                .unwrap()
        );
    }

    /// A live lock binds one directory, database inode and persisted allocation.
    /// Equal session labels or copied allocation bytes cannot bind a different
    /// database. Restoring the actual original file restores the binding.
    #[test]
    fn store_binding_rejects_foreign_and_replaced_database_inodes() {
        let fixture = Directory::new();
        let owner = RootOwnership::acquire(&fixture.0).unwrap();
        let mut original = MmpCustodyStore::open(&fixture.0, "session").unwrap();
        let original_id = original.store_id.clone();
        assert!(owner.bind_store(&mut original).is_ok());

        let foreign = Directory::new();
        let mut other = MmpCustodyStore::open(&foreign.0, "session").unwrap();
        assert!(owner.bind_store(&mut other).is_err());
        drop(other);
        drop(original);

        let database = fixture.0.join("custody.sqlite");
        let saved = fixture.0.join("original.saved");
        let replaced = fixture.0.join("replacement.saved");
        fs::rename(&database, &saved).unwrap();
        let replacement = MmpCustodyStore::open(&fixture.0, "session").unwrap();
        replacement
            .connection
            .execute("UPDATE state SET store_id=?1", [&original_id])
            .unwrap();
        drop(replacement);
        let mut replacement = MmpCustodyStore::open(&fixture.0, "session").unwrap();
        assert_eq!(replacement.store_id, original_id);
        assert!(owner.bind_store(&mut replacement).is_err());
        drop(replacement);
        fs::rename(&database, &replaced).unwrap();
        fs::rename(&saved, &database).unwrap();
        let mut restored = MmpCustodyStore::open(&fixture.0, "session").unwrap();
        assert!(owner.bind_store(&mut restored).is_ok());

        let mut changed = original_id.clone();
        changed[0] ^= 1;
        restored
            .connection
            .execute("UPDATE state SET store_id=?1", [&changed])
            .unwrap();
        drop(restored);
        let mut changed_store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
        assert!(owner.bind_store(&mut changed_store).is_err());
        changed_store
            .connection
            .execute("UPDATE state SET store_id=?1", [&original_id])
            .unwrap();
        drop(changed_store);
        let mut restored = MmpCustodyStore::open(&fixture.0, "session").unwrap();
        assert!(owner.bind_store(&mut restored).is_ok());
    }

    /// A lock file renamed away and replaced creates a different kernel lock.
    /// The old guard must refuse new store borrows until its own lock pathname
    /// is restored; equal directory and allocation identities are insufficient.
    #[test]
    fn replaced_lock_path_invalidates_original_binding_until_restored() {
        let fixture = Directory::new();
        let owner = RootOwnership::acquire(&fixture.0).unwrap();
        let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
        assert!(owner.bind_store(&mut store).is_ok());
        let lock = fixture.0.join("owner.lock");
        let saved = fixture.0.join("owner.saved");
        let replacement = fixture.0.join("owner.replaced");
        fs::rename(&lock, &saved).unwrap();
        let other = RootOwnership::acquire(&fixture.0).unwrap();
        assert!(owner.bind_store(&mut store).is_err());
        assert!(other.bind_store(&mut store).is_ok());
        drop(other);
        fs::rename(&lock, &replacement).unwrap();
        fs::rename(&saved, &lock).unwrap();
        assert!(owner.bind_store(&mut store).is_ok());
    }

    /// Runs only when explicitly invoked as a child by the process-exit test.
    /// It holds the actual OS lock until the parent forcibly terminates it.
    #[test]
    fn lock_child() {
        let Some(root) = std::env::var_os(CHILD_ROOT) else {
            return;
        };
        let _owner = RootOwnership::acquire(Path::new(&root)).unwrap();
        println!("{READY}");
        std::io::stdout().flush().unwrap();
        loop {
            std::thread::park();
        }
    }

    /// A generated child and its private destination are always retired even
    /// when an assertion fails, so the test cannot leave a process holding a lock.
    struct ChildFixture {
        child: Child,
        root: PathBuf,
    }

    impl Drop for ChildFixture {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    /// An independently spawned process owns the same real filesystem lock,
    /// blocks another owner, and releases it when killed without Rust cleanup.
    /// This checks the crash boundary rather than merely dropping a local guard.
    #[test]
    fn process_exit_releases_actual_owner_lock() {
        let root = std::env::temp_dir().join(format!(
            "mez-mmp-owner-process-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "storage::mmp_custody::ownership::tests::lock_child",
                "--nocapture",
            ])
            .env(CHILD_ROOT, &root)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let output = child.stdout.take().unwrap();
        let mut fixture = ChildFixture { child, root };
        let (send, receive) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                match line {
                    Ok(line) if line.contains(READY) => {
                        let _ = send.send(());
                        return;
                    }
                    Ok(_) => (),
                    Err(_) => break,
                }
            }
        });
        assert!(receive.recv_timeout(Duration::from_secs(10)).is_ok());
        assert!(RootOwnership::acquire(&fixture.root).is_err());
        fixture.child.kill().unwrap();
        fixture.child.wait().unwrap();
        reader.join().unwrap();
        let _replacement = RootOwnership::acquire(&fixture.root).unwrap();
    }
}
