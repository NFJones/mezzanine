//! Real directory syscalls, retained positive effects and exact descendant pins.
//!
//! Each fixture grants only a private physical subtree. Tests do not dispatch
//! actor work or fabricate directory receipts; runtime integration remains a
//! separate acceptance boundary. No children, shell programs or global cwd edits.

use super::*;
use crate::security::filesystem::host_resolved_path_scopes;
use std::fs;
use std::os::unix::fs::symlink;
use std::sync::atomic::{AtomicBool, Ordering};

/// Private physical subtree with one immutable actual process/root witness.
struct Fixture {
    /// Owned data removed only after the test's filesystem handles retire.
    directory: PathBuf,
    /// Exact root allocation passed to all separately admitted directory work.
    root: NativeFilesystemRoot,
    /// Explicit read/write ceiling for this subtree only.
    scopes: PathScopes,
}
impl Fixture {
    /// Captures real process identity and physical grants without child launches.
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("mez-parent-cap-{}", rand::random::<u128>()));
        fs::create_dir(&directory).unwrap();
        let directory = fs::canonicalize(directory).unwrap();
        let root = NativeFilesystemRoot::capture(std::process::id()).unwrap();
        let grant = directory.to_str().unwrap().to_string();
        let scopes = host_resolved_path_scopes(
            root.cwd(),
            std::slice::from_ref(&grant),
            std::slice::from_ref(&grant),
            &[],
        )
        .unwrap();
        Self {
            directory,
            root,
            scopes,
        }
    }
    /// Captures one missing target under only this fixture's actual authority.
    fn file(&self, name: &str) -> NativeFileCapability {
        NativeFileCapability::capture(
            &self.root,
            &self.scopes,
            self.directory.join(name).to_str().unwrap(),
            true,
        )
        .unwrap()
    }
    /// Captures one missing directory, then retains its actual positive mkdir.
    fn create(&self, name: &str) -> NativeCreatedDirectory {
        NativeParentCapability::capture(
            &self.root,
            &self.scopes,
            self.directory.join(name).to_str().unwrap(),
            NativeIoControl::bounded_only(),
        )
        .unwrap()
        .commit(&self.root, &self.scopes)
        .unwrap()
    }
}
impl Drop for Fixture {
    /// Removes only private fixture data, never implementation cleanup targets.
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

/// A single file grant cannot create its missing parent; the separately scoped
/// directory admission must fail before mkdir, leaving no directory or file.
#[test]
fn file_only_grant_cannot_create_parent_directory() {
    let fixture = Fixture::new();
    let file = fixture
        .directory
        .join("parent/file")
        .to_str()
        .unwrap()
        .to_string();
    let read = fixture.directory.to_str().unwrap().to_string();
    let scopes = host_resolved_path_scopes(
        fixture.root.cwd(),
        std::slice::from_ref(&read),
        std::slice::from_ref(&file),
        &[],
    )
    .unwrap();
    assert!(NativeFileCapability::capture(&fixture.root, &scopes, &file, true).is_ok());
    assert!(
        NativeParentCapability::capture(
            &fixture.root,
            &scopes,
            fixture.directory.join("parent").to_str().unwrap(),
            NativeIoControl::bounded_only()
        )
        .is_err()
    );
    assert!(!fixture.directory.join("parent").exists());
}

/// Positive mkdir survives original-fence revocation before its first directory
/// observation. Attribution remains accessible and Drop must not imply rollback.
#[test]
fn committed_directory_survives_cancellation_before_observation() {
    let fixture = Fixture::new();
    let cancelled = Arc::new(AtomicBool::new(false));
    let observed = cancelled.clone();
    let control = NativeIoControl::new(move || {
        if observed.load(Ordering::SeqCst) {
            Err(MezError::conflict("original directory budget cancelled"))
        } else {
            Ok(())
        }
    });
    let admission = NativeParentCapability::capture(
        &fixture.root,
        &fixture.scopes,
        fixture.directory.join("parent").to_str().unwrap(),
        control,
    )
    .unwrap();
    let allocation = admission.commit_allocation();
    let mut created = admission.commit(&fixture.root, &fixture.scopes).unwrap();
    assert!(Arc::ptr_eq(&allocation, &created.commit_allocation()));
    cancelled.store(true, Ordering::SeqCst);
    assert!(
        created
            .observe(&fixture.root, &fixture.scopes)
            .unwrap_err()
            .message()
            .contains("budget cancelled")
    );
    assert_eq!(created.path(), fixture.directory.join("parent"));
    drop(created);
    assert!(fixture.directory.join("parent").is_dir());
    assert!(!fixture.directory.join("parent/file").exists());
}

/// Original-fence rejection before the committing syscall must leave the target
/// absent. The directory owner cannot refresh its budget to complete later.
#[test]
fn cancellation_before_directory_commit_has_no_effect() {
    let fixture = Fixture::new();
    let cancelled = Arc::new(AtomicBool::new(false));
    let observed = cancelled.clone();
    let control = NativeIoControl::new(move || {
        if observed.load(Ordering::SeqCst) {
            Err(MezError::conflict("directory cancelled"))
        } else {
            Ok(())
        }
    });
    let admission = NativeParentCapability::capture(
        &fixture.root,
        &fixture.scopes,
        fixture.directory.join("parent").to_str().unwrap(),
        control,
    )
    .unwrap();
    cancelled.store(true, Ordering::SeqCst);
    assert!(admission.commit(&fixture.root, &fixture.scopes).is_err());
    assert!(!fixture.directory.join("parent").exists());
}

/// Equal PID/cwd labels from a newly captured root are not the original root
/// allocation. Directory proposals cannot be rebound to that replacement owner.
#[test]
fn directory_admission_rejects_rebound_root_allocation() {
    let fixture = Fixture::new();
    let admission = NativeParentCapability::capture(
        &fixture.root,
        &fixture.scopes,
        fixture.directory.join("parent").to_str().unwrap(),
        NativeIoControl::bounded_only(),
    )
    .unwrap();
    let other = NativeFilesystemRoot::capture(std::process::id()).unwrap();
    assert!(admission.commit(&other, &fixture.scopes).is_err());
    assert!(!fixture.directory.join("parent").exists());
}

/// A competing directory appearing after capture must remain untouched. A
/// failed no-replace commit grants no cleanup or descendant write authority.
#[test]
fn competing_directory_creation_is_preserved() {
    let fixture = Fixture::new();
    let admission = NativeParentCapability::capture(
        &fixture.root,
        &fixture.scopes,
        fixture.directory.join("parent").to_str().unwrap(),
        NativeIoControl::bounded_only(),
    )
    .unwrap();
    fs::create_dir(fixture.directory.join("parent")).unwrap();
    fs::write(fixture.directory.join("parent/competitor"), b"keep").unwrap();
    assert!(admission.commit(&fixture.root, &fixture.scopes).is_err());
    assert_eq!(
        fs::read(fixture.directory.join("parent/competitor")).unwrap(),
        b"keep"
    );
}

/// Each nested ancestor requires its own explicit admission and held observation.
/// Only the exact positive directory chain can evolve the initial missing file;
/// that evolved capability can stage/publish without a fresh arbitrary capture.
#[test]
fn nested_parent_effects_enable_exact_missing_file_transfer() {
    let fixture = Fixture::new();
    let original = fixture.file("one/two/file");
    assert_eq!(
        original.missing_parent_paths().unwrap(),
        vec![
            fixture.directory.join("one"),
            fixture.directory.join("one/two")
        ]
    );
    assert!(
        NativeParentCapability::capture(
            &fixture.root,
            &fixture.scopes,
            fixture.directory.join("one/two").to_str().unwrap(),
            NativeIoControl::bounded_only()
        )
        .is_err()
    );
    let mut one = fixture.create("one");
    assert!(
        original
            .recapture_with_created_parents(
                &fixture.root,
                &fixture.scopes,
                std::slice::from_ref(&one)
            )
            .is_err()
    );
    one.observe(&fixture.root, &fixture.scopes).unwrap();
    let intermediate = original
        .recapture_with_created_parents(&fixture.root, &fixture.scopes, std::slice::from_ref(&one))
        .unwrap();
    assert_eq!(
        intermediate.missing_parent_paths().unwrap(),
        vec![fixture.directory.join("one/two")]
    );
    let mut two = fixture.create("one/two");
    two.observe(&fixture.root, &fixture.scopes).unwrap();
    let evolved = original
        .recapture_with_created_parents(&fixture.root, &fixture.scopes, &[one, two])
        .unwrap();
    assert!(evolved.missing_parent_paths().unwrap().is_empty());
    let mut stage = evolved
        .stage(&fixture.root, &fixture.scopes, b"exact\r\n")
        .unwrap();
    evolved
        .publish(&fixture.root, &fixture.scopes, &mut stage)
        .unwrap();
    assert_eq!(
        fs::read(fixture.directory.join("one/two/file")).unwrap(),
        b"exact\r\n"
    );
}

/// External creation without the worker's positive held directory effect cannot
/// refresh an originally missing ancestor into a new writable file capability.
#[test]
fn unrecorded_directory_cannot_refresh_missing_file_ancestry() {
    let fixture = Fixture::new();
    let original = fixture.file("parent/file");
    fs::create_dir(fixture.directory.join("parent")).unwrap();
    assert!(
        original
            .recapture_with_created_parents(&fixture.root, &fixture.scopes, &[])
            .is_err()
    );
    assert!(!fixture.directory.join("parent/file").exists());
}

/// Once observed, directory identity is pinned. A relocated directory followed
/// by an equal empty replacement must revoke descendant writes; Drop preserves
/// both directories and never removes the unrelated replacement.
#[test]
fn replaced_observed_directory_rejects_descendant_transfer() {
    let fixture = Fixture::new();
    let original = fixture.file("parent/file");
    let mut created = fixture.create("parent");
    created.observe(&fixture.root, &fixture.scopes).unwrap();
    fs::rename(
        fixture.directory.join("parent"),
        fixture.directory.join("moved"),
    )
    .unwrap();
    fs::create_dir(fixture.directory.join("parent")).unwrap();
    assert!(created.observe(&fixture.root, &fixture.scopes).is_err());
    assert!(
        original
            .recapture_with_created_parents(
                &fixture.root,
                &fixture.scopes,
                std::slice::from_ref(&created)
            )
            .is_err()
    );
    drop(created);
    assert!(fixture.directory.join("parent").is_dir());
    assert!(fixture.directory.join("moved").is_dir());
    assert!(!fixture.directory.join("parent/file").exists());
}

/// A symlink replacing a positively created but unobserved directory must not
/// grant an outside subtree. Creation remains true and both targets survive.
#[test]
fn observation_rejects_symlink_replacement_without_cleanup() {
    let fixture = Fixture::new();
    let outside = Fixture::new();
    let mut created = fixture.create("parent");
    fs::rename(
        fixture.directory.join("parent"),
        fixture.directory.join("moved"),
    )
    .unwrap();
    symlink(&outside.directory, fixture.directory.join("parent")).unwrap();
    assert!(created.observe(&fixture.root, &fixture.scopes).is_err());
    drop(created);
    assert!(fixture.directory.join("parent").is_symlink());
    assert!(fixture.directory.join("moved").is_dir());
    assert!(outside.directory.is_dir());
}

/// Fresh directory scope narrowing applies both before mkdir and to observation
/// afterwards. A failed observation must preserve the positive creation fact.
#[test]
fn narrowed_directory_authority_rejects_commit_and_observation() {
    let fixture = Fixture::new();
    let admission = NativeParentCapability::capture(
        &fixture.root,
        &fixture.scopes,
        fixture.directory.join("denied").to_str().unwrap(),
        NativeIoControl::bounded_only(),
    )
    .unwrap();
    let mut narrowed = fixture.scopes.clone();
    narrowed.write_scopes.clear();
    assert!(admission.commit(&fixture.root, &narrowed).is_err());
    assert!(!fixture.directory.join("denied").exists());
    let mut created = fixture.create("parent");
    assert!(created.observe(&fixture.root, &narrowed).is_err());
    assert!(created.path().is_dir());
}
