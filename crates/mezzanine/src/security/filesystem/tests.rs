//! Native filesystem capability acceptance, using only direct OS/filesystem APIs.
//!
//! Tests inject competing mutations between capture, staging and publication.
//! They prove rejection before publication, not atomic CAS against arbitrary
//! external writers after the final check. No test changes the daemon cwd.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;

use super::capability::{NativeFileCapability, NativeFilesystemRoot};
use super::resolution::host_resolved_path_scopes;
use mez_agent::permissions::PathScopes;

/// Unique owned filesystem fixture with automatic bounded cleanup.
struct Fixture {
    /// Private temporary root, physically canonical on Linux and macOS.
    directory: PathBuf,
    /// Live local process evidence, independent of executable/shell selection.
    root: NativeFilesystemRoot,
    /// Existing authority contract authorizing only this fixture's subtree.
    scopes: PathScopes,
}

impl Fixture {
    /// Captures real local process identity without launching a child.
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("mez-native-cap-{}", rand::random::<u128>()));
        fs::create_dir(&directory).unwrap();
        let directory = fs::canonicalize(directory).unwrap();
        let root = NativeFilesystemRoot::capture(std::process::id()).unwrap();
        let path = directory.to_str().unwrap().to_string();
        let scopes = host_resolved_path_scopes(
            root.cwd(),
            std::slice::from_ref(&path),
            std::slice::from_ref(&path),
            &[],
        )
        .unwrap();
        Self {
            directory,
            root,
            scopes,
        }
    }

    /// Captures one writable file/create capability beneath the fixture.
    fn capability(&self, path: &str) -> NativeFileCapability {
        NativeFileCapability::capture(
            &self.root,
            &self.scopes,
            self.directory.join(path).to_str().unwrap(),
            true,
        )
        .unwrap()
    }
}

impl Drop for Fixture {
    /// Removes only the unique directory this fixture created.
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

/// Exact CRLF and missing-newline preimages survive capture; publication
/// preserves executable bits but suppresses setuid/setgid/sticky privilege bits.
#[test]
fn capability_updates_exact_bytes_and_safe_modes() {
    let fixture = Fixture::new();
    let path = fixture.directory.join("file");
    fs::write(&path, b"old\r\nwithout-newline").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o6755)).unwrap();
    let capability = fixture.capability("file");
    assert_eq!(
        capability.preimage(),
        Some(b"old\r\nwithout-newline".as_slice())
    );
    let mut stage = capability
        .stage(&fixture.root, &fixture.scopes, b"new\n")
        .unwrap();
    capability
        .publish(&fixture.root, &fixture.scopes, &mut stage)
        .unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"new\n");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
        0o755
    );
    assert!(
        capability
            .publish(&fixture.root, &fixture.scopes, &mut stage)
            .is_err()
    );
}

/// Adds use exclusive publication and cannot replace a competing create;
/// losing the race leaves the competitor untouched and cleans owned staging.
#[test]
fn capability_add_preserves_competing_create() {
    let fixture = Fixture::new();
    let capability = fixture.capability("new");
    assert!(capability.preimage().is_none());
    let mut stage = capability
        .stage(&fixture.root, &fixture.scopes, b"ours")
        .unwrap();
    fs::write(fixture.directory.join("new"), b"competitor").unwrap();
    assert!(
        capability
            .publish(&fixture.root, &fixture.scopes, &mut stage)
            .is_err()
    );
    drop(stage);
    assert_eq!(
        fs::read(fixture.directory.join("new")).unwrap(),
        b"competitor"
    );
    assert_eq!(fs::read_dir(&fixture.directory).unwrap().count(), 1);
}

/// Equal bytes on a replacement inode do not satisfy captured object identity,
/// and same-inode content changes do not satisfy the exact preimage check.
#[test]
fn capability_rejects_object_and_preimage_changes() {
    let fixture = Fixture::new();
    let path = fixture.directory.join("file");
    fs::write(&path, b"old").unwrap();
    let capability = fixture.capability("file");
    fs::write(&path, b"changed").unwrap();
    assert!(
        capability
            .revalidate(&fixture.root, &fixture.scopes)
            .is_err()
    );
    fs::write(&path, b"old").unwrap();
    fs::rename(&path, fixture.directory.join("held-old")).unwrap();
    fs::write(&path, b"old").unwrap();
    assert!(
        capability
            .revalidate(&fixture.root, &fixture.scopes)
            .is_err()
    );
}

/// Link retargeting and directory relocation cannot redirect a held capability
/// into another authorized or unauthorized location before publication.
#[test]
fn capability_rejects_symlink_retarget_and_ancestor_swap() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.directory.join("parent")).unwrap();
    fs::write(fixture.directory.join("parent/file"), b"old").unwrap();
    fs::write(fixture.directory.join("other"), b"other").unwrap();
    symlink("parent/file", fixture.directory.join("link")).unwrap();
    let link = fixture.capability("link");
    fs::remove_file(fixture.directory.join("link")).unwrap();
    symlink("other", fixture.directory.join("link")).unwrap();
    assert!(link.revalidate(&fixture.root, &fixture.scopes).is_err());
    let file = fixture.capability("parent/file");
    let mut stage = file.stage(&fixture.root, &fixture.scopes, b"new").unwrap();
    fs::rename(
        fixture.directory.join("parent"),
        fixture.directory.join("relocated"),
    )
    .unwrap();
    fs::create_dir(fixture.directory.join("parent")).unwrap();
    fs::write(fixture.directory.join("parent/file"), b"replacement").unwrap();
    assert!(
        file.publish(&fixture.root, &fixture.scopes, &mut stage)
            .is_err()
    );
    assert_eq!(
        fs::read(fixture.directory.join("relocated/file")).unwrap(),
        b"old"
    );
    assert_eq!(
        fs::read(fixture.directory.join("parent/file")).unwrap(),
        b"replacement"
    );
}

/// Current actor authority is rechecked even after staging; planning/read-only
/// scopes cannot create staging files or publish a previously captured grant.
#[test]
fn capability_requires_current_read_and_write_authority() {
    let fixture = Fixture::new();
    fs::write(fixture.directory.join("file"), b"old").unwrap();
    let capability = fixture.capability("file");
    let mut stage = capability
        .stage(&fixture.root, &fixture.scopes, b"new")
        .unwrap();
    let mut read_only = fixture.scopes.clone();
    read_only.write_scopes.clear();
    assert!(capability.stage(&fixture.root, &read_only, b"new").is_err());
    assert!(
        capability
            .publish(&fixture.root, &read_only, &mut stage)
            .is_err()
    );
    let mut no_read = fixture.scopes.clone();
    no_read.read_scopes.clear();
    assert!(
        NativeFileCapability::capture(
            &fixture.root,
            &no_read,
            capability.path().to_str().unwrap(),
            true
        )
        .is_err()
    );
    assert_eq!(fs::read(fixture.directory.join("file")).unwrap(), b"old");
}

/// A special FIFO is rejected from metadata before opening/reading it, and a
/// non-directory ancestor is never mislabeled as a missing create target.
#[test]
fn capability_rejects_special_nodes_promptly() {
    let fixture = Fixture::new();
    rustix::fs::mknodat(
        rustix::fs::CWD,
        fixture.directory.join("fifo"),
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        0,
    )
    .unwrap();
    let started = std::time::Instant::now();
    assert!(
        NativeFileCapability::capture(
            &fixture.root,
            &fixture.scopes,
            fixture.directory.join("fifo").to_str().unwrap(),
            true
        )
        .is_err()
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    fs::write(fixture.directory.join("file"), b"old").unwrap();
    assert!(
        super::resolution::resolve_host_path(
            fixture.root.cwd(),
            fixture.directory.join("file/child").to_str().unwrap()
        )
        .is_err()
    );
}

/// Atomic replacement affects only the selected hard-link entry; a subsequent
/// deletion uses its newly captured exact identity without modifying siblings.
#[test]
fn capability_preserves_hardlink_siblings_and_deletes_verified_entries() {
    let fixture = Fixture::new();
    fs::write(fixture.directory.join("file"), b"old").unwrap();
    fs::hard_link(
        fixture.directory.join("file"),
        fixture.directory.join("sibling"),
    )
    .unwrap();
    let capability = fixture.capability("file");
    let mut stage = capability
        .stage(&fixture.root, &fixture.scopes, b"new")
        .unwrap();
    capability
        .publish(&fixture.root, &fixture.scopes, &mut stage)
        .unwrap();
    assert_eq!(fs::read(fixture.directory.join("sibling")).unwrap(), b"old");
    fixture
        .capability("file")
        .delete(&fixture.root, &fixture.scopes)
        .unwrap();
    assert!(!fixture.directory.join("file").exists());
    assert_eq!(fs::read(fixture.directory.join("sibling")).unwrap(), b"old");
}

/// Scope escapes via symlinks, loops and unresolved traversal through absent
/// directories fail closed while ordinary inside links remain supported.
#[test]
fn capability_rejects_scope_escape_and_resolution_loops() {
    let fixture = Fixture::new();
    let outside = Fixture::new();
    fs::write(outside.directory.join("file"), b"outside").unwrap();
    symlink(
        outside.directory.join("file"),
        fixture.directory.join("escape"),
    )
    .unwrap();
    assert!(
        NativeFileCapability::capture(
            &fixture.root,
            &fixture.scopes,
            fixture.directory.join("escape").to_str().unwrap(),
            true
        )
        .is_err()
    );
    symlink("loop", fixture.directory.join("loop")).unwrap();
    assert!(
        super::resolution::resolve_host_path(
            fixture.root.cwd(),
            fixture.directory.join("loop").to_str().unwrap()
        )
        .is_err()
    );
    assert!(
        super::resolution::resolve_host_path(
            fixture.root.cwd(),
            fixture.directory.join("missing/../file").to_str().unwrap()
        )
        .is_err()
    );
    assert_eq!(
        fs::read(outside.directory.join("file")).unwrap(),
        b"outside"
    );
}

/// Missing directories are explicit independent effects; a file-only grant
/// cannot implicitly create ancestors, and each creation requires current scope.
#[test]
fn capability_parent_creation_is_explicit_and_scoped() {
    let fixture = Fixture::new();
    let capability = fixture.capability("parent/file");
    assert!(
        capability
            .stage(&fixture.root, &fixture.scopes, b"new")
            .is_err()
    );
    let parent = fixture.directory.join("parent");
    NativeFileCapability::create_parent_directory(
        &fixture.root,
        &fixture.scopes,
        parent.to_str().unwrap(),
    )
    .unwrap();
    assert_eq!(
        fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let capability = fixture.capability("parent/file");
    let mut stage = capability
        .stage(&fixture.root, &fixture.scopes, b"new")
        .unwrap();
    capability
        .publish(&fixture.root, &fixture.scopes, &mut stage)
        .unwrap();
    assert_eq!(fs::read(parent.join("file")).unwrap(), b"new");
    let mut read_only = fixture.scopes.clone();
    read_only.write_scopes.clear();
    assert!(
        NativeFileCapability::create_parent_directory(
            &fixture.root,
            &read_only,
            fixture.directory.join("denied").to_str().unwrap()
        )
        .is_err()
    );
    assert!(!fixture.directory.join("denied").exists());
}

/// Staging bytes and entry identity are checked before publication. Cleanup
/// removes only the held entry and leaves a competitor replacing its name intact.
#[test]
fn capability_rejects_staging_tampering_and_preserves_replacement() {
    let fixture = Fixture::new();
    fs::write(fixture.directory.join("file"), b"old").unwrap();
    let capability = fixture.capability("file");
    let mut stage = capability
        .stage(&fixture.root, &fixture.scopes, b"new")
        .unwrap();
    let path = fs::read_dir(&fixture.directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with(".mez-native-stage-")
        })
        .unwrap();
    fs::write(&path, b"bad").unwrap();
    assert!(
        capability
            .publish(&fixture.root, &fixture.scopes, &mut stage)
            .is_err()
    );
    fs::rename(&path, fixture.directory.join("held-stage")).unwrap();
    fs::write(&path, b"competitor").unwrap();
    assert!(
        capability
            .publish(&fixture.root, &fixture.scopes, &mut stage)
            .is_err()
    );
    drop(stage);
    assert_eq!(fs::read(&path).unwrap(), b"competitor");
    assert_eq!(fs::read(fixture.directory.join("file")).unwrap(), b"old");
}

/// Unchanged inode/content cannot hide privilege-bit or permission broadening
/// on the stage. Changed target permissions also revoke the captured preimage.
#[test]
fn capability_rejects_mode_only_staging_and_target_tampering() {
    let fixture = Fixture::new();
    let target = fixture.directory.join("file");
    fs::write(&target, b"old").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    for mode in [0o4755, 0o644] {
        let capability = fixture.capability("file");
        let mut stage = capability
            .stage(&fixture.root, &fixture.scopes, b"new")
            .unwrap();
        let path = fs::read_dir(&fixture.directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with(".mez-native-stage-")
            })
            .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        assert!(
            capability
                .publish(&fixture.root, &fixture.scopes, &mut stage)
                .is_err()
        );
        assert_eq!(fs::read(&target).unwrap(), b"old");
        drop(stage);
        assert!(!path.exists());
    }
    let capability = fixture.capability("file");
    let mut stage = capability
        .stage(&fixture.root, &fixture.scopes, b"new")
        .unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o400)).unwrap();
    assert!(
        capability
            .publish(&fixture.root, &fixture.scopes, &mut stage)
            .is_err()
    );
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o400
    );
}
