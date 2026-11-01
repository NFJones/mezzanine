//! Real worker parent commits, truthful partial facts and pre-mutation capacity.
//!
//! These use actual mkdir/stage/publication syscalls with the explicit test-only
//! authority bridge. Actor/provider/journal integration has separate fixtures.

use super::*;

/// Nested parent effects precede the file and stay distinct from file line deltas.
/// A second initially missing file reuses the exact held created ancestor rather
/// than recapturing an arbitrary newly existing directory or creating it twice.
#[test]
fn nested_parents_and_shared_original_snapshots_commit_in_order() {
    let fixture = Fixture::new();
    let raw = fixture.run("*** Begin Patch\n*** Add File: $ROOT/one/two/a\n+first\n*** Add File: $ROOT/one/two/b\n+second\n*** End Patch");
    assert_eq!(
        raw.result.certainty,
        NativeEffectCertainty::Confirmed,
        "{:?}",
        raw.result.diagnostic
    );
    assert_eq!(raw.receipts.len(), 4);
    assert_eq!(
        raw.receipts
            .iter()
            .map(|receipt| receipt.confirmation_ordinal)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    assert!(raw.receipts[..2].iter().all(|receipt| !receipt.is_file()
        && receipt.diff.is_empty()
        && receipt.added_lines == 0
        && receipt.removed_lines == 0));
    assert_eq!(raw.receipts[0].endpoint_ordinal, 2);
    assert_eq!(raw.receipts[1].endpoint_ordinal, 3);
    assert_eq!(raw.receipts[2].endpoint_ordinal, 0);
    assert_eq!(raw.receipts[3].operation_ordinal, 1);
    assert_eq!(
        fs::read(fixture.directory.join("one/two/a")).unwrap(),
        b"first\n"
    );
    assert_eq!(
        fs::read(fixture.directory.join("one/two/b")).unwrap(),
        b"second\n"
    );
    assert_eq!(raw.completed_operations, vec![0, 1]);
}

/// Cancellation acknowledged after actual mkdir cannot erase that positive
/// directory effect. Observation/file publication stops and no rollback occurs.
#[test]
fn acknowledged_cancellation_after_mkdir_preserves_directory_fact() {
    let fixture = Fixture::new();
    let request =
        fixture.request("*** Begin Patch\n*** Add File: $ROOT/parent/file\n+new\n*** End Patch");
    let fence = request.fence.clone();
    let mut status = None;
    let raw = execute_native_patch(
        request,
        |_, _, _| Ok(fixture.scopes.clone()),
        |receipt| {
            if !receipt.is_file() {
                status = Some(fence.cancel());
            }
        },
    );
    assert_eq!(
        status,
        Some(crate::runtime::processes::native_patch::commit::NativePatchFenceStatus::Fenced)
    );
    assert_eq!(raw.result.certainty, NativeEffectCertainty::Partial);
    assert_eq!(raw.receipts.len(), 1);
    assert!(!raw.receipts[0].is_file());
    assert!(raw.result.confirmed_effects.is_empty());
    assert!(raw.completed_operations.is_empty());
    assert!(fixture.directory.join("parent").is_dir());
    assert!(!fixture.directory.join("parent/file").exists());
}

/// A later exact file lease denial preserves the earlier directory and removes
/// only its unpublished staging file. Directory creation is never a file success.
#[test]
fn denied_file_commit_keeps_directory_and_cleans_owned_stage() {
    let fixture = Fixture::new();
    let raw = execute_native_patch(
        fixture.request("*** Begin Patch\n*** Add File: $ROOT/parent/file\n+new\n*** End Patch"),
        |_, phase, path| {
            if phase == NativePatchPhase::Commit && path.ends_with("/file") {
                Err(MezError::forbidden("file lease narrowed"))
            } else {
                Ok(fixture.scopes.clone())
            }
        },
        |_| {},
    );
    assert_eq!(raw.result.certainty, NativeEffectCertainty::Partial);
    assert_eq!(raw.receipts.len(), 1);
    assert!(!raw.receipts[0].is_file());
    assert!(
        fs::read_dir(fixture.directory.join("parent"))
            .unwrap()
            .next()
            .is_none()
    );
}

/// A newly broader actor snapshot cannot widen the original action's file-only
/// write ceiling to create ancestors. Rejection occurs before the first mkdir.
#[test]
fn file_only_original_write_scope_cannot_create_missing_parents() {
    let fixture = Fixture::new();
    let mut request =
        fixture.request("*** Begin Patch\n*** Add File: $ROOT/parent/file\n+new\n*** End Patch");
    request.scopes = host_resolved_path_scopes(
        request.root.cwd(),
        &fixture.scopes.read_scopes,
        &[fixture
            .directory
            .join("parent/file")
            .to_str()
            .unwrap()
            .to_owned()],
        &[],
    )
    .unwrap();
    let raw = execute_native_patch(request, |_, _, _| Ok(fixture.scopes.clone()), |_| {});
    assert_eq!(raw.result.certainty, NativeEffectCertainty::Unexecuted);
    assert!(raw.receipts.is_empty());
    assert!(!fixture.directory.join("parent").exists());
}

/// A dependent move confirms its directories and destination before deleting
/// source. Cancellation after destination publication preserves both endpoints
/// truthfully without falsely completing the move or rolling anything back.
#[test]
fn move_with_new_parents_retains_destination_before_cancelled_source_delete() {
    let fixture = Fixture::new();
    fs::write(fixture.directory.join("source"), b"old\n").unwrap();
    let request = fixture.request("*** Begin Patch\n*** Update File: $ROOT/source\n*** Move to: $ROOT/one/two/destination\n@@\n-old\n+new\n*** End Patch");
    let fence = request.fence.clone();
    let raw = execute_native_patch(
        request,
        |_, _, _| Ok(fixture.scopes.clone()),
        |receipt| {
            if receipt.kind == NativePatchEndpointKind::Created {
                fence.cancel();
            }
        },
    );
    assert_eq!(raw.result.certainty, NativeEffectCertainty::Partial);
    assert_eq!(raw.receipts.len(), 3);
    assert_eq!(raw.receipts[2].kind, NativePatchEndpointKind::Created);
    assert!(raw.result.confirmed_effects.is_empty());
    assert_eq!(
        fs::read(fixture.directory.join("source")).unwrap(),
        b"old\n"
    );
    assert_eq!(
        fs::read(fixture.directory.join("one/two/destination")).unwrap(),
        b"new\n"
    );
}

/// A competing unrecorded second ancestor cannot be adopted after the first
/// positive directory. Both directories survive, but only our actual creation
/// is attributed and no file is published into the competitor's subtree.
#[test]
fn unrecorded_competing_parent_stops_after_retaining_earlier_directory() {
    let fixture = Fixture::new();
    let raw = execute_native_patch(
        fixture.request("*** Begin Patch\n*** Add File: $ROOT/one/two/file\n+new\n*** End Patch"),
        |_, _, _| Ok(fixture.scopes.clone()),
        |_| {
            fs::create_dir(fixture.directory.join("one/two")).unwrap();
            fs::write(fixture.directory.join("one/two/competitor"), b"keep").unwrap();
        },
    );
    assert_eq!(raw.result.certainty, NativeEffectCertainty::Partial);
    assert_eq!(raw.receipts.len(), 1);
    assert_eq!(
        fs::read(fixture.directory.join("one/two/competitor")).unwrap(),
        b"keep"
    );
    assert!(!fixture.directory.join("one/two/file").exists());
}

/// Replacing an observed ancestor before a later directory commit must revoke
/// the actual held parent. Its original directory and unrelated replacement
/// survive; a matching pathname cannot refresh the capture into write authority.
#[test]
fn observed_parent_replacement_before_next_mkdir_is_rejected() {
    let fixture = Fixture::new();
    let raw = execute_native_patch(
        fixture.request("*** Begin Patch\n*** Add File: $ROOT/one/two/file\n+new\n*** End Patch"),
        |_, phase, path| {
            if phase == NativePatchPhase::Commit && path.ends_with("/one/two") {
                fs::rename(
                    fixture.directory.join("one"),
                    fixture.directory.join("moved"),
                )
                .unwrap();
                fs::create_dir(fixture.directory.join("one")).unwrap();
            }
            Ok(fixture.scopes.clone())
        },
        |_| {},
    );
    assert_eq!(raw.result.certainty, NativeEffectCertainty::Partial);
    assert_eq!(raw.receipts.len(), 1);
    assert!(fixture.directory.join("moved").is_dir());
    assert!(fixture.directory.join("one").is_dir());
    assert!(!fixture.directory.join("one/two").exists());
}

/// Parent count admission covers the complete plan before any side effect,
/// including an otherwise valid earlier independent file operation.
#[test]
fn excess_parent_count_is_rejected_before_earlier_file_commit() {
    let fixture = Fixture::new();
    let nested = "p/"
        .repeat(crate::security::filesystem::capability::parent::MAX_NATIVE_PARENT_DIRECTORIES + 1);
    let raw = fixture.run(&format!("*** Begin Patch\n*** Add File: $ROOT/earlier\n+x\n*** Add File: $ROOT/{nested}file\n+y\n*** End Patch"));
    assert_eq!(raw.result.certainty, NativeEffectCertainty::Unexecuted);
    assert!(raw.receipts.is_empty());
    assert!(!fixture.directory.join("earlier").exists());
    assert!(!fixture.directory.join("p").exists());
}

/// Oversized encoded authoritative facts must be refused before mkdir or any
/// earlier file write. This exercises the actual source/plan/physical captures,
/// not a standalone JSON length calculation after irreversible effects.
#[test]
fn encoded_fact_capacity_is_checked_before_any_mutation() {
    let fixture = Fixture::new();
    // Escaping stresses encoded capacity with few held captures on either OS.
    let parent = std::iter::repeat_n("\"".repeat(200), 19)
        .collect::<Vec<_>>()
        .join("/");
    let mut source = "*** Begin Patch\n*** Add File: $ROOT/earlier\n+x\n".to_string();
    for index in 0..24 {
        source.push_str(&format!(
            "*** Add File: $ROOT/{parent}/{index:04}{}\n+x\n",
            "f".repeat(100)
        ));
    }
    source.push_str("*** End Patch");
    let raw = fixture.run(&source);
    assert_eq!(
        raw.result.certainty,
        NativeEffectCertainty::Unexecuted,
        "{:?}",
        raw.result.diagnostic
    );
    assert!(
        raw.result
            .diagnostic
            .as_deref()
            .unwrap()
            .contains("encoded effect facts"),
        "{:?}",
        raw.result.diagnostic
    );
    assert!(raw.receipts.is_empty());
    assert!(!fixture.directory.join("earlier").exists());
    assert!(!fixture.directory.join(parent).exists());
}

/// Optional diff text has a separate escaping allowance. A JSON-expensive
/// preview cannot consume authoritative fact capacity or block the actual file
/// deletion when all complete endpoint facts fit the admitted result envelope.
#[test]
fn escaped_diff_does_not_consume_authoritative_fact_capacity() {
    let fixture = Fixture::new();
    fs::write(fixture.directory.join("file"), vec![1u8; 400_000]).unwrap();
    let raw = fixture.run("*** Begin Patch\n*** Delete File: $ROOT/file\n*** End Patch");
    assert_eq!(
        raw.result.certainty,
        NativeEffectCertainty::Confirmed,
        "{:?}",
        raw.result.diagnostic
    );
    assert!(!fixture.directory.join("file").exists());
    assert!(raw.receipts[0].metadata(true).to_string().len() > NATIVE_PATCH_FACT_BYTES);
    assert!(raw.receipts[0].metadata(true).to_string().len() < NATIVE_PATCH_RESULT_BYTES);
}
