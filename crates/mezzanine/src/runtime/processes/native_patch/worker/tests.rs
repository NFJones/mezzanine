//! Real-filesystem worker regressions without process or pane adapters.
//!
//! Tests supply explicit authority callbacks in place of the pending actor
//! bridge. They exercise actual capabilities, staging and commit receipts;
//! they do not certify the assembled dispatcher or native process association.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use super::*;
use crate::runtime::processes::native_cancellation::NativeActionCancellation;
use crate::security::filesystem::host_resolved_path_scopes;

/// Owned, physically canonical fixture with grants restricted to its subtree.
struct Fixture {
    /// Private directory removed only by this fixture.
    directory: PathBuf,
    /// Explicit grant, separate from current process filesystem credentials.
    scopes: PathScopes,
}

impl Fixture {
    /// Captures no shell context and launches no child to prepare the fixture.
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("mez-native-worker-{}", rand::random::<u128>()));
        fs::create_dir(&directory).unwrap();
        let directory = fs::canonicalize(directory).unwrap();
        let root = NativeFilesystemRoot::capture(std::process::id()).unwrap();
        let scope = directory.to_str().unwrap().to_string();
        let scopes = host_resolved_path_scopes(
            root.cwd(),
            std::slice::from_ref(&scope),
            std::slice::from_ref(&scope),
            &[],
        )
        .unwrap();
        Self { directory, scopes }
    }

    /// Binds exact parsed semantics to fresh action-local cancellation/custody.
    fn request(&self, source: &str) -> NativePatchWorkerRequest {
        let source = source.replace("$ROOT", self.directory.to_str().unwrap());
        NativePatchWorkerRequest {
            identity: NativeCommitIdentity {
                turn_id: "turn-worker".into(),
                action_id: "patch-worker".into(),
                transaction: "transaction-worker".into(),
                authority_generation: 1,
                operation: NativePatchOperation::parse(&source, None).unwrap(),
            },
            root: NativeFilesystemRoot::capture(std::process::id()).unwrap(),
            scopes: self.scopes.clone(),
            fence: NativePatchCommitFence::new(NativeActionCancellation::new(), None).unwrap(),
            coordinator: NativePatchWriteCoordinator::default(),
        }
    }

    /// Executes actual filesystem operations under this fixture's current grant.
    fn run(&self, source: &str) -> NativePatchWorkerOutcome {
        execute_native_patch(
            self.request(source),
            |_, _, _| Ok(self.scopes.clone()),
            |_| {},
        )
    }

    /// Checks that cleanup retained no exclusive transaction staging files.
    fn assert_no_stages(&self) {
        assert!(fs::read_dir(&self.directory).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".mez-native-stage-")
        }));
    }
}

impl Drop for Fixture {
    /// Removes only the unique subtree positively created by this fixture.
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

/// A real move publishes the destination before deleting its source, then a
/// later update consumes the exact published object and intermediate bytes.
/// CRLF is retained as the initial preimage while changed text follows the
/// shared matcher's documented output normalization.
#[test]
fn move_and_later_update_keep_ordered_endpoint_custody() {
    let fixture = Fixture::new();
    fs::write(fixture.directory.join("a"), b"old\r\n").unwrap();
    let outcome = fixture.run("*** Begin Patch\n*** Update File: $ROOT/a\n*** Move to: $ROOT/z\n@@\n-old\n+middle\n*** Update File: $ROOT/z\n@@\n-middle\n+final\n*** End Patch");
    assert_eq!(outcome.result.certainty, NativeEffectCertainty::Confirmed);
    assert!(outcome.result.diagnostic.is_none());
    assert_eq!(outcome.completed_operations, [0, 1]);
    assert_eq!(
        outcome
            .receipts
            .iter()
            .map(|r| (r.operation_ordinal, r.endpoint_ordinal, r.kind))
            .collect::<Vec<_>>(),
        [
            (0, 0, NativePatchEndpointKind::Created),
            (0, 1, NativePatchEndpointKind::Deleted),
            (1, 0, NativePatchEndpointKind::Updated)
        ]
    );
    assert!(!fixture.directory.join("a").exists());
    assert_eq!(fs::read(fixture.directory.join("z")).unwrap(), b"final\n");
    fixture.assert_no_stages();
}

/// Ordered create/update/delete/create operations on the same spelling must
/// retain successive exact object proofs, including a positively missing entry
/// after deletion. No command generation or replay reconstructs the outcome.
#[test]
fn repeated_path_recreation_consumes_actual_intermediate_states() {
    let fixture = Fixture::new();
    let outcome = fixture.run("*** Begin Patch\n*** Add File: $ROOT/a\n+first\n*** Update File: $ROOT/a\n@@\n-first\n+second\n*** Delete File: $ROOT/a\n*** Add File: $ROOT/a\n+last\n*** End Patch");
    assert_eq!(outcome.result.certainty, NativeEffectCertainty::Confirmed);
    assert_eq!(outcome.receipts.len(), 4);
    assert_eq!(outcome.completed_operations, [0, 1, 2, 3]);
    assert_eq!(fs::read(fixture.directory.join("a")).unwrap(), b"last\n");
    fixture.assert_no_stages();
}

/// Losing authorization for a move's source deletion leaves its positively
/// committed destination and original source intact. Endpoint evidence must
/// survive without falsely reporting the whole move as a confirmed effect.
#[test]
fn failed_move_source_delete_preserves_destination_and_partial_receipt() {
    let fixture = Fixture::new();
    let source = fixture.directory.join("a");
    fs::write(&source, b"old\n").unwrap();
    let request = fixture.request("*** Begin Patch\n*** Update File: $ROOT/a\n*** Move to: $ROOT/z\n@@\n-old\n+new\n*** End Patch");
    let outcome = execute_native_patch(
        request,
        |_, phase, path| {
            if phase == NativePatchPhase::Commit && std::path::Path::new(path) == source.as_path() {
                return Err(MezError::forbidden("source deletion revoked"));
            }
            Ok(fixture.scopes.clone())
        },
        |_| {},
    );
    assert_eq!(outcome.result.certainty, NativeEffectCertainty::Partial);
    assert_eq!(outcome.receipts.len(), 1);
    assert!(outcome.result.confirmed_effects.is_empty());
    assert!(outcome.completed_operations.is_empty());
    assert_eq!(fs::read(source).unwrap(), b"old\n");
    assert_eq!(fs::read(fixture.directory.join("z")).unwrap(), b"new\n");
    fixture.assert_no_stages();
}

/// Replacing a captured source with a same-byte different inode before staging
/// cannot refresh away the original snapshot identity. The substituted entry
/// remains untouched and no confirmed mutation is invented.
#[test]
fn equal_byte_source_replacement_rejects_initial_object_substitution() {
    let fixture = Fixture::new();
    let path = fixture.directory.join("a");
    fs::write(&path, b"old\n").unwrap();
    let request =
        fixture.request("*** Begin Patch\n*** Update File: $ROOT/a\n@@\n-old\n+new\n*** End Patch");
    let mut swapped = false;
    let outcome = execute_native_patch(
        request,
        |_, phase, _| {
            if phase == NativePatchPhase::Stage && !swapped {
                fs::rename(&path, fixture.directory.join("original")).unwrap();
                fs::write(&path, b"old\n").unwrap();
                swapped = true;
            }
            Ok(fixture.scopes.clone())
        },
        |_| {},
    );
    assert_eq!(outcome.result.certainty, NativeEffectCertainty::Unexecuted);
    assert!(outcome.receipts.is_empty());
    assert_eq!(fs::read(path).unwrap(), b"old\n");
    fixture.assert_no_stages();
}

/// A later same-byte substitution of an already published file cannot become
/// the preimage owner of the next operation. The first commit receipt remains
/// attributable while the replacement entry is never overwritten.
#[test]
fn equal_byte_published_replacement_rejects_later_object_substitution() {
    let fixture = Fixture::new();
    let path = fixture.directory.join("a");
    let request=fixture.request("*** Begin Patch\n*** Add File: $ROOT/a\n+first\n*** Update File: $ROOT/a\n@@\n-first\n+last\n*** End Patch");
    let outcome = execute_native_patch(
        request,
        |_, _, _| Ok(fixture.scopes.clone()),
        |_| {
            fs::rename(&path, fixture.directory.join("published")).unwrap();
            fs::write(&path, b"first\n").unwrap();
        },
    );
    assert_eq!(outcome.result.certainty, NativeEffectCertainty::Partial);
    assert_eq!(outcome.receipts.len(), 1);
    assert_eq!(fs::read(path).unwrap(), b"first\n");
    fixture.assert_no_stages();
}

/// Cancellation arriving through the real shared fence after staging but
/// before publication leaves the target absent and cleans the exact owned
/// stage. An acknowledged fence is never converted into a successful create.
#[test]
fn cancellation_before_publication_cleans_stage_without_committing() {
    let fixture = Fixture::new();
    let request = fixture.request("*** Begin Patch\n*** Add File: $ROOT/a\n+new\n*** End Patch");
    let fence = request.fence.clone();
    let outcome = execute_native_patch(
        request,
        |_, phase, _| {
            if phase == NativePatchPhase::Commit {
                fence.cancel();
            }
            Ok(fixture.scopes.clone())
        },
        |_| {},
    );
    assert_eq!(outcome.result.certainty, NativeEffectCertainty::Unexecuted);
    assert!(!fixture.directory.join("a").exists());
    assert!(outcome.receipts.is_empty());
    fixture.assert_no_stages();
}

/// A current actor grant that narrows to no writable paths cannot inherit the
/// original admission's broader scope. Staged bytes remain private and cleanup
/// runs even when authorization changes immediately before commit.
#[test]
fn narrowed_current_scopes_reject_publication_and_clean_stage() {
    let fixture = Fixture::new();
    let request = fixture.request("*** Begin Patch\n*** Add File: $ROOT/a\n+new\n*** End Patch");
    let outcome = execute_native_patch(
        request,
        |_, phase, _| {
            Ok(if phase == NativePatchPhase::Commit {
                host_resolved_path_scopes(
                    std::path::Path::new(&fixture.scopes.current_directory),
                    &[],
                    &[],
                    &[],
                )
                .unwrap()
            } else {
                fixture.scopes.clone()
            })
        },
        |_| {},
    );
    assert_eq!(outcome.result.certainty, NativeEffectCertainty::Unexecuted);
    assert!(!fixture.directory.join("a").exists());
    assert!(outcome.receipts.is_empty());
    fixture.assert_no_stages();
}

/// Presentation failure after a positive commit cannot erase its receipt or
/// trigger the next file's effect. A private panic payload becomes a bounded
/// generic failure, while already committed bytes remain in place.
#[test]
fn progress_panic_retains_confirmed_receipt_and_stops_later_effects() {
    let fixture = Fixture::new();
    let request=fixture.request("*** Begin Patch\n*** Add File: $ROOT/a\n+first\n*** Add File: $ROOT/b\n+later\n*** End Patch");
    let outcome = execute_native_patch(
        request,
        |_, _, _| Ok(fixture.scopes.clone()),
        |_| panic!("private progress canary"),
    );
    assert_eq!(outcome.result.certainty, NativeEffectCertainty::Partial);
    assert_eq!(outcome.receipts.len(), 1);
    assert!(fixture.directory.join("a").exists());
    assert!(!fixture.directory.join("b").exists());
    assert!(
        !outcome
            .result
            .diagnostic
            .unwrap()
            .contains("private progress canary")
    );
    fixture.assert_no_stages();
}

/// A semantic no-op revalidates current ownership and retains exact original
/// CRLF bytes. It records a completed operation without fabricating a mutation
/// receipt or treating normalization alone as a file change.
#[test]
fn no_op_preserves_raw_crlf_and_has_no_mutation_receipt() {
    let fixture = Fixture::new();
    let path = fixture.directory.join("a");
    fs::write(&path, b"old\r\n").unwrap();
    let outcome =
        fixture.run("*** Begin Patch\n*** Update File: $ROOT/a\n@@\n-old\n+old\n*** End Patch");
    assert_eq!(outcome.result.certainty, NativeEffectCertainty::Confirmed);
    assert_eq!(outcome.completed_operations, [0]);
    assert!(outcome.receipts.is_empty());
    assert!(outcome.result.confirmed_effects.is_empty());
    assert_eq!(fs::read(path).unwrap(), b"old\r\n");
}

/// Atomic replacement preserves ordinary permissions of an existing entry;
/// privileged mode bits, ownership, ACL and xattr preservation are not implied
/// by the capability's explicit metadata contract.
#[test]
fn update_preserves_ordinary_file_mode() {
    let fixture = Fixture::new();
    let path = fixture.directory.join("a");
    fs::write(&path, b"old\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    let outcome =
        fixture.run("*** Begin Patch\n*** Update File: $ROOT/a\n@@\n-old\n+new\n*** End Patch");
    assert_eq!(outcome.result.certainty, NativeEffectCertainty::Confirmed);
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o640
    );
}

/// Aggregate diff truncation never truncates authoritative final bytes or
/// removes the later file's typed receipt. Each published endpoint survives
/// even after the shared display budget has been exhausted.
#[test]
fn aggregate_diff_budget_keeps_full_files_and_all_commit_receipts() {
    let fixture = Fixture::new();
    let text = "x".repeat(200_000);
    let outcome=fixture.run(&format!("*** Begin Patch\n*** Add File: $ROOT/a\n+{text}\n*** Add File: $ROOT/b\n+{text}\n*** End Patch"));
    assert_eq!(outcome.result.certainty, NativeEffectCertainty::Confirmed);
    assert_eq!(outcome.receipts.len(), 2);
    assert!(
        outcome.receipts.iter().map(|r| r.diff.len()).sum::<usize>() <= NATIVE_PATCH_DIFF_BYTES
    );
    assert!(outcome.receipts.iter().any(|r| r.diff_truncated));
    for path in ["a", "b"] {
        assert_eq!(
            fs::read(fixture.directory.join(path)).unwrap().len(),
            text.len() + 1
        );
    }
}

/// A directory substitution cannot transfer ownership merely by moving
/// the exact published inode into the replacement parent. Later operations
/// must retain the original parent ancestry as well as bytes and leaf identity.
#[test]
fn published_inode_cannot_transfer_into_a_replacement_parent() {
    let fixture = Fixture::new();
    let parent = fixture.directory.join("nested");
    fs::create_dir(&parent).unwrap();
    let request=fixture.request("*** Begin Patch\n*** Add File: $ROOT/nested/a\n+first\n*** Update File: $ROOT/nested/a\n@@\n-first\n+last\n*** End Patch");
    let mut substituted = false;
    let outcome = execute_native_patch(
        request,
        |_, _, _| Ok(fixture.scopes.clone()),
        |_| {
            if !substituted {
                let original = fixture.directory.join("original-parent");
                fs::rename(&parent, &original).unwrap();
                fs::create_dir(&parent).unwrap();
                fs::rename(original.join("a"), parent.join("a")).unwrap();
                substituted = true;
            }
        },
    );
    assert_eq!(outcome.result.certainty, NativeEffectCertainty::Partial);
    assert_eq!(outcome.receipts.len(), 1);
    assert_eq!(fs::read(parent.join("a")).unwrap(), b"first\n");
    fixture.assert_no_stages();
}
