//! Creation-only permission contracts for positive bootstrap fixtures.
//!
//! A developer's inherited umask may permit group writes. Positive fixtures
//! must still create owned HOME anchors and legacy state with the permissions
//! required by real admission. These helpers never normalize an existing node,
//! change process umask, or participate in production publication. Negative
//! ownership/mode tests continue constructing their deliberately unsafe nodes.

use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

/// Creates a new owner-only fixture directory, refusing an existing entry.
pub(crate) fn create_private_directory(path: impl AsRef<Path>) -> io::Result<()> {
    std::fs::DirBuilder::new().mode(0o700).create(path)
}

/// Creates a new owner-only synthetic state file without adopting an existing
/// node. Subsequent deliberate byte edits use the already-created fixture file.
pub(crate) fn create_private_file(path: impl AsRef<Path>, bytes: &[u8]) -> io::Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?
        .write_all(bytes)
}

/// Fixture helpers create only new nodes: existing unsafe directories, files,
/// and links must remain unchanged rather than being silently repaired into
/// positive admission evidence. Synthetic files are private at initial creation.
#[test]
fn bootstrap_fixture_creation_never_adopts_existing_nodes() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    let root = std::env::temp_dir().join(format!(
        "mez-bootstrap-fixture-modes-{}",
        crate::storage::token_usage::new_token_usage_event_id()
    ));
    create_private_directory(&root).unwrap();
    assert_eq!(std::fs::metadata(&root).unwrap().mode() & 0o077, 0);
    let file = root.join("state");
    create_private_file(&file, b"original").unwrap();
    assert_eq!(std::fs::metadata(&file).unwrap().mode() & 0o077, 0);
    assert!(create_private_file(&file, b"replacement").is_err());
    let link = root.join("link");
    symlink(&file, &link).unwrap();
    assert!(create_private_file(&link, b"replacement").is_err());
    assert_eq!(std::fs::read(&file).unwrap(), b"original");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(create_private_directory(&root).is_err());
    assert_eq!(std::fs::metadata(&root).unwrap().mode() & 0o777, 0o777);
    std::fs::remove_dir_all(root).unwrap();
}

/// Positive production CLI fixtures must work under common permissive and
/// restrictive umasks. Change umask only in a freshly forked child before exec,
/// never in the parallel parent test process; each child uses unique roots and
/// preserves the real install/repeat/repair/dry-run assertions.
#[test]
fn bootstrap_positive_fixture_is_independent_of_inherited_umask() {
    use std::os::unix::process::CommandExt;
    for mask in [0o002, 0o022, 0o077] {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "cli::bootstrap::tests::bootstrap_default_install_and_explicit_dry_run_are_distinct",
            "--quiet",
        ]);
        // SAFETY: umask is async-signal-safe and this closure accesses no shared
        // Rust state or locks. Only the exec child's file-creation mask changes.
        unsafe {
            command.pre_exec(move || {
                libc::umask(mask);
                Ok(())
            });
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "umask={mask:o}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed; 0 failed"),
            "the exact fixture must execute, not silently match zero tests: umask={mask:o}: {output:?}"
        );
    }
}
