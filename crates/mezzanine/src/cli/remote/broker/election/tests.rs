//! Native startup-election exclusion and replacement tests; no subprocess launch.
//!
//! Guards never replace the protected endpoint's lifetime identity lock. Fixture
//! paths are disposable and tests preserve replacement entries on guard disposal.

use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};

/// Same-root launcher attempts contend independently of runtime directories;
/// releasing the first guard permits recovery without deleting the lock file.
#[test]
fn outbound_startup_election_has_one_recoverable_owner() {
    let root = std::env::temp_dir().join(format!("mez-elect-{:032x}", rand::random::<u128>()));
    let first = StartupElection::acquire(&root).unwrap().unwrap();
    first.validate().unwrap();
    assert!(StartupElection::acquire(&root).unwrap().is_none());
    drop(first);
    let second = StartupElection::acquire(&root).unwrap().unwrap();
    second.validate().unwrap();
    drop(second);
    assert!(root.join(LOCK_NAME).is_file());
    std::fs::remove_dir_all(root).unwrap();
}

/// Root relocation/replacement and lock replacement cannot authorize startup.
/// Disposal leaves replacement entries untouched; missing roots are not recreated.
#[test]
fn outbound_startup_election_rejects_replaced_objects() {
    let root = std::env::temp_dir().join(format!("mez-elect-swap-{:032x}", rand::random::<u128>()));
    let owner = StartupElection::acquire(&root).unwrap().unwrap();
    let moved = root.with_extension("moved");
    std::fs::rename(&root, &moved).unwrap();
    assert!(owner.validate().is_err());
    assert!(!root.exists());
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(owner.validate().is_err());
    std::fs::remove_dir(&root).unwrap();
    std::fs::rename(&moved, &root).unwrap();
    owner.validate().unwrap();
    std::fs::rename(root.join(LOCK_NAME), root.join("held.lock")).unwrap();
    std::fs::write(root.join(LOCK_NAME), b"replacement").unwrap();
    std::fs::set_permissions(root.join(LOCK_NAME), std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(owner.validate().is_err());
    drop(owner);
    assert_eq!(std::fs::read(root.join(LOCK_NAME)).unwrap(), b"replacement");
    std::fs::remove_dir_all(root).unwrap();
}

/// Symlink, permissive and hard-linked lock entries reject rather than being
/// replaced. A final symlink root cannot bypass private-directory validation.
#[test]
fn outbound_startup_election_rejects_unsafe_paths() {
    let root =
        std::env::temp_dir().join(format!("mez-elect-unsafe-{:032x}", rand::random::<u128>()));
    drop(StartupElection::acquire(&root).unwrap().unwrap());
    let lock = root.join(LOCK_NAME);
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(StartupElection::acquire(&root).is_err());
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::hard_link(&lock, root.join("alias.lock")).unwrap();
    assert!(StartupElection::acquire(&root).is_err());
    std::fs::remove_file(root.join("alias.lock")).unwrap();
    std::fs::rename(&lock, root.join("original.lock")).unwrap();
    symlink("original.lock", &lock).unwrap();
    assert!(StartupElection::acquire(&root).is_err());
    let alias = root.with_extension("alias");
    symlink(&root, &alias).unwrap();
    assert!(StartupElection::acquire(&alias).is_err());
    std::fs::remove_file(alias).unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
