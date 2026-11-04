//! Bounded physical original publication and fail-closed cold seed controls.

use super::*;
use crate::storage::mmp_custody::ownership::RootOwnership;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;

/// Owns only a generated private registration/store directory.
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "mez-mmp-registration-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        )))
    }
    fn open(&self) -> (RootOwnership, MmpCustodyStore) {
        let owner = RootOwnership::acquire(&self.0).unwrap();
        let store = MmpCustodyStore::open(&self.0, "original-session").unwrap();
        (owner, store)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Only actual empty fenced setup publishes the origin. An exact lost result
/// reconciles without replacing its inode or claiming an accepted occurrence.
/// The constant-size cold seed contains no source, cursor or native authority.
#[test]
fn original_registration_publishes_once_and_recovers_bodyless_seed() {
    let fixture = Fixture::new();
    let (owner, mut store) = fixture.open();
    let mut bound = owner.bind_store(&mut store).unwrap();
    bound.publish_registration().unwrap();
    let before = fs::metadata(fixture.0.join(NAME)).unwrap();
    bound.publish_registration().unwrap();
    let after = fs::metadata(fixture.0.join(NAME)).unwrap();
    assert_eq!(before.ino(), after.ino());
    assert_eq!(after.nlink(), 1);
    assert!(after.len() <= MAX_BYTES as u64);
    assert!(!fixture.0.join(STAGE).exists());
    let seed = bound
        .registered_recovery_checkpoint(1000, 1024 * 1024)
        .unwrap();
    seed.validate_storage_snapshot().unwrap();
    assert_eq!(seed.schema_version, 10);
    assert_eq!(seed.next_sequence, 1);
    let data = seed.storage_snapshot.unwrap();
    assert!(data.sources.is_empty() && data.native.is_empty());
    assert_eq!(data.binding.unwrap().generation, 0);
    assert!(store.acceptance_plan(0).unwrap().is_empty_original());
}

/// Nonempty history cannot be retroactively labelled as an original publication.
/// A colliding stage is also retained, never deleted as though this writer owned it.
#[test]
fn registration_refuses_history_and_unowned_publication_stages() {
    let fixture = Fixture::new();
    let (owner, mut store) = fixture.open();
    store
        .commit(
            0,
            &[CustodyWrite {
                key: "original",
                previous: None,
                value: Some(b"retained"),
            }],
        )
        .unwrap();
    assert!(
        owner
            .bind_store(&mut store)
            .unwrap()
            .publish_registration()
            .is_err()
    );
    assert!(!fixture.0.join(NAME).exists());
    assert_eq!(store.read("original").unwrap().unwrap(), b"retained");
    let fixture = Fixture::new();
    let (owner, mut store) = fixture.open();
    owner
        .bind_store(&mut store)
        .unwrap()
        .publish_registration()
        .unwrap();
    let original = fs::read(fixture.0.join(NAME)).unwrap();
    fs::write(fixture.0.join(STAGE), b"unowned stage").unwrap();
    assert!(
        owner
            .bind_store(&mut store)
            .unwrap()
            .publish_registration()
            .is_err()
    );
    assert_eq!(fs::read(fixture.0.join(STAGE)).unwrap(), b"unowned stage");
    assert_eq!(fs::read(fixture.0.join(NAME)).unwrap(), original);
    assert!(
        owner
            .bind_store(&mut store)
            .unwrap()
            .registered_recovery_checkpoint(1000, 1024 * 1024)
            .is_err()
    );
}

/// Checksums alone cannot substitute another scope or physical allocation, and
/// malformed/future framing cannot be silently initialized as a fresh original.
/// Every refusal leaves the exact registration and store position unchanged.
#[test]
fn original_registration_refuses_corrupt_and_rechecksummed_foreign_facts() {
    for fault in [
        "truncated",
        "future",
        "checksum",
        "owner",
        "allocation",
        "directory",
        "database",
        "floor",
        "oversized",
    ] {
        let fixture = Fixture::new();
        let (owner, mut store) = fixture.open();
        owner
            .bind_store(&mut store)
            .unwrap()
            .publish_registration()
            .unwrap();
        let mut bytes = fs::read(fixture.0.join(NAME)).unwrap();
        match fault {
            "truncated" => {
                bytes.pop();
            }
            "future" => bytes[7] = b'2',
            "checksum" => bytes[8] ^= 1,
            "owner" => bytes[HEADER] ^= 1,
            "allocation" => bytes[8] ^= 1,
            "directory" => bytes[48] ^= 1,
            "database" => bytes[64] ^= 1,
            "floor" => bytes[32] = 1,
            "oversized" => bytes.resize(MAX_BYTES + 1, b'x'),
            _ => unreachable!(),
        }
        if !matches!(fault, "truncated" | "checksum" | "oversized") {
            let end = bytes.len() - 32;
            let digest = Sha256::digest(&bytes[..end]);
            bytes[end..].copy_from_slice(&digest);
        }
        fs::write(fixture.0.join(NAME), &bytes).unwrap();
        assert!(
            owner
                .bind_store(&mut store)
                .unwrap()
                .registered_recovery_checkpoint(1000, 1024 * 1024)
                .is_err(),
            "{fault}"
        );
        assert!(
            owner
                .bind_store(&mut store)
                .unwrap()
                .publish_registration()
                .is_err(),
            "{fault}"
        );
        assert_eq!(fs::read(fixture.0.join(NAME)).unwrap(), bytes);
        assert!(store.acceptance_plan(0).unwrap().is_empty_original());
    }
}

/// Missing, shared-link, public and symlink registrations cannot create a cold
/// input. Existing-only reading never recreates the absent publication.
#[test]
fn original_registration_requires_its_private_single_link_file() {
    for fault in ["missing", "hardlink", "permissions", "symlink"] {
        let fixture = Fixture::new();
        let (owner, mut store) = fixture.open();
        owner
            .bind_store(&mut store)
            .unwrap()
            .publish_registration()
            .unwrap();
        let path = fixture.0.join(NAME);
        match fault {
            "missing" => fs::remove_file(&path).unwrap(),
            "hardlink" => fs::hard_link(&path, fixture.0.join("extra-link")).unwrap(),
            "permissions" => fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap(),
            "symlink" => {
                fs::rename(&path, fixture.0.join("retained")).unwrap();
                symlink(fixture.0.join("retained"), &path).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            owner
                .bind_store(&mut store)
                .unwrap()
                .registered_recovery_checkpoint(1000, 1024 * 1024)
                .is_err(),
            "{fault}"
        );
        if fault == "missing" {
            assert!(!path.exists());
        }
    }
}

/// Copying every byte, including the registration and lock, cannot preserve the
/// original kernel directory/database allocations. A fresh owner of the copy
/// must refuse before any source or native context could be reconstructed.
#[test]
fn copied_root_registration_cannot_qualify_recovery() {
    let original = Fixture::new();
    let (owner, mut store) = original.open();
    owner
        .bind_store(&mut store)
        .unwrap()
        .publish_registration()
        .unwrap();
    drop(store);
    drop(owner);
    let copied = Fixture::new();
    fs::create_dir(&copied.0).unwrap();
    fs::set_permissions(&copied.0, fs::Permissions::from_mode(0o700)).unwrap();
    for name in ["owner.lock", "custody.sqlite", NAME] {
        fs::copy(original.0.join(name), copied.0.join(name)).unwrap();
    }
    let owner = RootOwnership::acquire_existing(&copied.0).unwrap();
    let mut store =
        MmpCustodyStore::open_existing(&copied.0, "original-session", Default::default()).unwrap();
    assert!(
        owner
            .bind_store(&mut store)
            .unwrap()
            .registered_recovery_checkpoint(1000, 1024 * 1024)
            .is_err()
    );
}
