//! Native startup-election exclusion and replacement tests; no subprocess launch.
//!
//! Guards never replace the protected endpoint's lifetime identity lock. Fixture
//! paths are disposable and tests preserve replacement entries on guard disposal.

use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};

/// A launcher owns the election, not arbitrary duplicate descriptor lifetimes.
/// Retaining the same open-file description models fork-before-exec inheritance
/// without spawning a child. Live owners must still exclude competitors, but
/// owner disposal must explicitly release while the duplicate remains open.
#[test]
fn outbound_startup_election_releases_owner_with_retained_descriptor() {
    let root = std::env::temp_dir().join(format!(
        "mez-elect-duplicate-{:032x}",
        rand::random::<u128>()
    ));
    let first = StartupElection::acquire(&root).unwrap().unwrap();
    let duplicate = first.lock.try_clone().unwrap();
    assert!(StartupElection::acquire(&root).unwrap().is_none());
    drop(first);
    let second = StartupElection::acquire(&root)
        .unwrap()
        .expect("disposed owner must not retain election through inherited descriptors");
    assert!(StartupElection::acquire(&root).unwrap().is_none());
    drop(duplicate);
    assert!(StartupElection::acquire(&root).unwrap().is_none());
    drop(second);
    drop(StartupElection::acquire(&root).unwrap().unwrap());
    assert!(root.join(LOCK_NAME).is_file());
    std::fs::remove_dir_all(root).unwrap();
}

/// An inherited guard must neither authorize startup nor unlock its parent's
/// live election. A synthetic process mismatch exercises disposal without a fork
/// in this multithreaded test runner; a retained duplicate models the parent fd.
#[test]
fn outbound_startup_election_inherited_guard_cannot_release_live_owner() {
    let root = std::env::temp_dir().join(format!(
        "mez-elect-inherited-{:032x}",
        rand::random::<u128>()
    ));
    let mut inherited = StartupElection::acquire(&root).unwrap().unwrap();
    let parent_descriptor = inherited.lock.try_clone().unwrap();
    inherited.owner_pid = Some(std::process::id().wrapping_add(1));
    assert!(inherited.validate().is_err());
    drop(inherited);
    assert!(StartupElection::acquire(&root).unwrap().is_none());
    flock(&parent_descriptor, FlockOperation::Unlock).unwrap();
    let owner = StartupElection::acquire(&root).unwrap().unwrap();
    drop(parent_descriptor);
    assert!(StartupElection::acquire(&root).unwrap().is_none());
    drop(owner);
    std::fs::remove_dir_all(root).unwrap();
}

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
