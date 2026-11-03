//! Lifetime exclusion for cooperating runtime custody roots.
//!
//! SQLite serializes individual transactions, but an owned custody attempt can
//! span several transactions and continue after its observer disappears. This
//! private lock fences other runtime roots until the actual owner and its
//! workers retire. It does not grant delivery authority, validate recovery, or
//! prevent direct use of the lower-level multi-connection store API. The store
//! directory remains trusted against replacement by the same user.

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
    /// Closing the final owner releases the lock, including on process exit.
    _lock: File,
    /// Original private directory held open to prevent directory-inode reuse.
    directory: File,
    /// Trusted destination pathname checked before every store binding.
    root: PathBuf,
    /// First bound store allocation and database inode; never refreshed on retry.
    bound: OnceLock<BoundStore>,
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
    /// Acquires a nonblocking fence before a runtime root opens its store.
    /// Rejects unsafe lock paths and an already-owned destination.
    pub(crate) fn acquire(root: &Path) -> Result<Self> {
        if !root.exists() {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true).mode(0o700);
            builder.create(root)?;
        }
        let directory = open_private_directory(root)?;
        let descriptor = openat(
            &directory,
            "owner.lock",
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
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
        };
        owner.validate_root()?;
        Ok(owner)
    }

    /// Rejects a replaced directory or lock pathname before opening a store.
    pub(crate) fn validate_root(&self) -> Result<()> {
        validate_directory_path(&self.root, &self.directory)?;
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
        Ok(OwnedCustodyStore {
            _owner: self,
            store,
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
