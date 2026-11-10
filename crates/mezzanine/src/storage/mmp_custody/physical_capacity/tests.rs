//! Actual private roots and SQLite connections, never host disk exhaustion.

use super::*;
use std::os::unix::fs::OpenOptionsExt;

/// Owns only a directory successfully created with create-new semantics. Normal
/// completion verifies removal explicitly; unwind cleanup uses the same path.
struct Directory(std::path::PathBuf);
impl Directory {
    fn new() -> Self {
        for _ in 0..8 {
            let path = std::env::temp_dir().join(format!(
                "mez-mmp-physical-{}-{}",
                std::process::id(),
                rand::random::<u64>()
            ));
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(error) => panic!("cannot create private fixture: {error}"),
            }
        }
        panic!("private fixture allocation exhausted");
    }
    fn path(&self) -> &Path {
        &self.0
    }
    fn close(self) -> std::io::Result<()> {
        fs::remove_dir_all(&self.0)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn scene() -> (Directory, Arc<RootOwnership>, MmpCustodyStore) {
    let directory = Directory::new();
    let owner = Arc::new(RootOwnership::acquire(directory.path()).unwrap());
    let mut store = MmpCustodyStore::open(directory.path(), "physical-owner").unwrap();
    assert!(
        owner
            .bind_store(&mut store)
            .unwrap()
            .qualify_physical_capacity()
            .unwrap()
    );
    (directory, owner, store)
}

fn cleanup(directory: Directory) {
    let path = directory.path().to_owned();
    directory.close().unwrap();
    assert!(!path.exists());
}

/// Existing callers can colocate canonical archives; that is not a physically
/// qualified rewrite root. Refusing qualification must not change their old
/// authority or silently exempt those archives from the physical envelope.
#[test]
fn physical_qualification_is_explicit_and_preserves_general_root_layout() {
    let directory = Directory::new();
    let owner = Arc::new(RootOwnership::acquire(directory.path()).unwrap());
    let mut store = MmpCustodyStore::open(directory.path(), "physical-owner").unwrap();
    let canonical = directory.path().join("canonical");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&canonical)
        .unwrap();
    assert!(
        !owner
            .bind_store(&mut store)
            .unwrap()
            .qualify_physical_capacity()
            .unwrap()
    );
    owner.bind_store(&mut store).unwrap();
    fs::remove_dir(&canonical).unwrap();
    assert!(
        owner
            .bind_store(&mut store)
            .unwrap()
            .qualify_physical_capacity()
            .unwrap()
    );
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&canonical)
        .unwrap();
    assert!(owner.bind_store(&mut store).is_err());
    fs::remove_dir(&canonical).unwrap();
    owner.bind_store(&mut store).unwrap();
    drop((store, owner));
    cleanup(directory);
}

#[test]
fn physical_rewrite_reservations_share_capacity_and_retain_actual_owner() {
    let (directory, owner, mut store) = scene();
    let plan = store.acceptance_plan(0).unwrap();
    for size in [0, REWRITE_BYTES + 1, usize::MAX] {
        assert!(owner.reserve_rewrite_capacity(&plan, size).is_err());
    }
    let first = owner
        .reserve_rewrite_capacity(&plan, REWRITE_BYTES / 2)
        .unwrap();
    let second = owner
        .clone()
        .reserve_rewrite_capacity(&plan, REWRITE_BYTES / 2)
        .unwrap();
    assert!(owner.reserve_rewrite_capacity(&plan, 1).is_err());
    assert!(first.matches(&owner, &plan));
    let alias = first.clone();
    drop(first);
    assert!(owner.reserve_rewrite_capacity(&plan, 1).is_err());
    drop(alias);
    assert_eq!(
        owner.rewrite_reserved.load(Ordering::Acquire),
        REWRITE_BYTES / 2
    );
    store
        .commit(
            0,
            &[CustodyWrite {
                key: "original",
                previous: None,
                value: Some(b"kept"),
            }],
        )
        .unwrap();
    assert!(!second.matches(&owner, &store.acceptance_plan(0).unwrap()));
    let (other_dir, other_owner, other_store) = scene();
    assert!(!second.matches(&other_owner, &plan));
    drop((other_store, other_owner));
    cleanup(other_dir);
    drop((store, owner));
    assert!(RootOwnership::acquire_existing(directory.path()).is_err());
    drop(second);
    drop(RootOwnership::acquire_existing(directory.path()).unwrap());
    cleanup(directory);
}

/// Minimum charges bound independent reservation metadata as well as bytes.
#[test]
fn physical_rewrite_tiny_leases_have_a_finite_container_bound() {
    let (directory, owner, store) = scene();
    let plan = store.acceptance_plan(0).unwrap();
    let leases = (0..128)
        .map(|_| owner.reserve_rewrite_capacity(&plan, 1).unwrap())
        .collect::<Vec<_>>();
    assert!(owner.reserve_rewrite_capacity(&plan, 1).is_err());
    assert_eq!(
        owner.rewrite_reserved.load(Ordering::Acquire),
        REWRITE_BYTES
    );
    drop(leases);
    assert_eq!(owner.rewrite_reserved.load(Ordering::Acquire), 0);
    drop((store, owner));
    cleanup(directory);
}

/// Sparse lengths still consume the conservative physical envelope.
#[test]
fn physical_envelope_refuses_extra_bytes_without_modifying_originals() {
    let (directory, owner, mut store) = scene();
    let path = directory.path().join("retained-private-evidence");
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    file.set_len(16 * 1024 * 1024).unwrap();
    assert!(owner.bind_store(&mut store).is_err());
    assert_eq!(file.metadata().unwrap().len(), 16 * 1024 * 1024);
    assert_eq!(store.acceptance_plan(0).unwrap().generation(), 0);
    drop(file);
    fs::remove_file(&path).unwrap();
    owner.bind_store(&mut store).unwrap();
    store
        .commit(
            0,
            &[CustodyWrite {
                key: "original",
                previous: None,
                value: Some(b"kept"),
            }],
        )
        .unwrap();
    owner.bind_store(&mut store).unwrap();
    assert_eq!(
        store.read("original").unwrap().as_deref(),
        Some(b"kept".as_slice())
    );
    drop((store, owner));
    cleanup(directory);
}

#[test]
fn physical_envelope_rejects_unsafe_paths_and_bounded_entry_exhaustion() {
    let (directory, owner, mut store) = scene();
    let path = directory.path().join("not-a-file");
    fs::create_dir(&path).unwrap();
    assert!(owner.bind_store(&mut store).is_err());
    fs::remove_dir(&path).unwrap();
    std::os::unix::fs::symlink("custody.sqlite", &path).unwrap();
    assert!(owner.bind_store(&mut store).is_err());
    fs::remove_file(&path).unwrap();
    fs::hard_link(directory.path().join("custody.sqlite"), &path).unwrap();
    assert!(owner.bind_store(&mut store).is_err());
    fs::remove_file(&path).unwrap();
    let paths = (0..MAX_FILES)
        .map(|index| directory.path().join(format!("private-{index}")))
        .collect::<Vec<_>>();
    for path in &paths {
        drop(
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(path)
                .unwrap(),
        );
    }
    assert!(owner.bind_store(&mut store).is_err());
    for path in paths {
        fs::remove_file(path).unwrap();
    }
    owner.bind_store(&mut store).unwrap();
    drop((store, owner));
    cleanup(directory);
}

#[test]
fn physical_envelope_rejects_changed_pager_policy_without_silent_reset() {
    for pragma in [
        "PRAGMA journal_mode=WAL",
        "PRAGMA cache_spill=ON",
        "PRAGMA temp_store=FILE",
        "PRAGMA synchronous=NORMAL",
        "PRAGMA max_page_count=32768",
    ] {
        let (directory, owner, mut store) = scene();
        store.connection.execute_batch(pragma).unwrap();
        assert!(owner.bind_store(&mut store).is_err(), "{pragma}");
        assert_eq!(store.acceptance_plan(0).unwrap().generation(), 0);
        drop((store, owner));
        cleanup(directory);
    }
}

/// Observe the actual uncommitted rollback journal after touching many original
/// pages with the supported pager, rather than testing only a size formula.
#[test]
fn physical_envelope_accounts_for_actual_multi_page_rollback_journal() {
    let (directory, owner, mut store) = scene();
    let original = vec![b'a'; 64 * 1024];
    for generation in 0..12 {
        let key = format!("original-{generation}");
        store
            .commit(
                generation,
                &[CustodyWrite {
                    key: &key,
                    previous: None,
                    value: Some(&original),
                }],
            )
            .unwrap();
    }
    let transaction = store.connection.unchecked_transaction().unwrap();
    transaction
        .execute("UPDATE records SET value=zeroblob(length(value))", [])
        .unwrap();
    let journal = fs::metadata(directory.path().join("custody.sqlite-journal")).unwrap();
    assert!(journal.len() > 64 * 1024 && journal.len() <= JOURNAL_BYTES);
    validate_paths(directory.path(), &owner.rewrite_reserved).unwrap();
    transaction.rollback().unwrap();
    assert_eq!(
        store.read("original-0").unwrap().as_deref(),
        Some(original.as_slice())
    );
    owner.bind_store(&mut store).unwrap();
    drop((store, owner));
    cleanup(directory);
}
