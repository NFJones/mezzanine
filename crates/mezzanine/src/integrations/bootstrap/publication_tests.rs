//! Owned publication recovery and filesystem admission regressions.

use super::publication::*;
use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{PermissionsExt, symlink};

/// Uses a unique explicit private root, independent of process cwd/environment.
fn root() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "mez-bootstrap-{}",
        crate::storage::token_usage::new_token_usage_event_id()
    ));
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

/// A read-only holder cannot publish or recover by accidentally using shared
/// methods. It neither creates a lock nor unlocks another cooperating holder.
#[test]
fn bootstrap_publication_inspection_cannot_mutate_or_release_foreign_lock() {
    let root = root();
    let inspected = Publisher::inspect(&root).unwrap();
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    assert!(
        inspected
            .apply(vec![Change {
                path: "owned".into(),
                before: None,
                after: Some(b"owned".to_vec())
            }])
            .is_err()
    );
    assert!(inspected.recover().is_err());
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    let holder = Publisher::open(&root).unwrap();
    drop(inspected);
    assert!(Publisher::open(&root).is_err());
    drop(holder);
    assert!(Publisher::open(&root).is_ok());
    fs::remove_dir_all(root).unwrap();
}

/// A crash after one destination leaves exact recovery intent. Recovery must
/// refuse foreign edits before changing any remaining destination, then finish
/// the original accepted operation after the conflict is explicitly repaired.
#[test]
fn bootstrap_publication_recovers_partial_commit_without_foreign_overwrite() {
    let root = root();
    let publisher = Publisher::open(&root).unwrap();
    publisher.stop_after.set(Some(1));
    let changes = vec![
        Change {
            path: "first".into(),
            before: None,
            after: Some(b"one".to_vec()),
        },
        Change {
            path: "nested/second".into(),
            before: None,
            after: Some(b"two".to_vec()),
        },
    ];
    assert!(publisher.apply(changes).is_err());
    assert_eq!(fs::read(root.join("first")).unwrap(), b"one");
    assert!(root.join(".mez-bootstrap-journal").is_file());
    fs::write(root.join("first"), b"foreign").unwrap();
    assert!(publisher.recover().is_err());
    assert!(!root.join("nested").exists());
    fs::write(root.join("first"), b"one").unwrap();
    assert!(publisher.recover().unwrap());
    assert_eq!(fs::read(root.join("nested/second")).unwrap(), b"two");
    assert!(!publisher.recover().unwrap());
    drop(publisher);
    fs::remove_dir_all(root).unwrap();
}

/// No-follow reads reject symlink ancestors, symlink files, FIFOs, hard links
/// and competing installer locks promptly. Exact preimages protect authored
/// changes; additions never replace an existing destination.
#[test]
fn bootstrap_publication_refuses_unsafe_nodes_and_conflicting_preimages() {
    let root = root();
    let publisher = Publisher::open(&root).unwrap();
    assert!(Publisher::open(&root).is_err());
    fs::write(root.join("ordinary"), b"authored").unwrap();
    symlink(root.join("ordinary"), root.join("linked")).unwrap();
    assert!(publisher.read("linked").is_err());
    symlink(&root, root.join("alias")).unwrap();
    assert!(publisher.read("alias/ordinary").is_err());
    let fifo = CString::new(root.join("pipe").as_os_str().as_bytes()).unwrap();
    // SAFETY: fifo is a NUL-terminated path in our unique private directory.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert!(publisher.read("pipe").is_err());
    fs::hard_link(root.join("ordinary"), root.join("hardlink")).unwrap();
    assert!(publisher.read("hardlink").is_err());
    fs::remove_file(root.join("hardlink")).unwrap();
    assert!(
        publisher
            .apply(vec![Change {
                path: "ordinary".into(),
                before: None,
                after: Some(b"replacement".to_vec())
            }])
            .is_err()
    );
    assert_eq!(fs::read(root.join("ordinary")).unwrap(), b"authored");
    assert!(!root.join(".mez-bootstrap-journal").exists());
    drop(publisher);
    fs::remove_dir_all(root).unwrap();
}
