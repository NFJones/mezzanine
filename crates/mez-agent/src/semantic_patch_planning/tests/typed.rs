//! Typed patch engine regressions independent of filesystem and shell execution.

use super::*;

/// Snapshot preconditions must retain captured raw CRLF bytes instead of a
/// normalized reconstruction; only final matching text may be normalized.
#[test]
fn shared_patch_plan_preserves_exact_crlf_preimage() {
    let bytes = b"old\r\ncontext\r\n";
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    let output = format!(
        "{APPLY_PATCH_READ_BEGIN_MARKER}\n{APPLY_PATCH_FILE_BEGIN_MARKER}\nPATH_B64 {}\nRESOLVED_B64 {}\nSTATUS regular\n{APPLY_PATCH_CONTENT_BEGIN_MARKER}\n{encoded}\n{APPLY_PATCH_CONTENT_END_MARKER}\n{APPLY_PATCH_FILE_END_MARKER}\n{APPLY_PATCH_READ_END_MARKER}\n",
        base64::engine::general_purpose::STANDARD.encode("file"),
        base64::engine::general_purpose::STANDARD.encode("/repo/file")
    );
    let snapshots = parse_apply_patch_snapshot_output(&output).unwrap();
    let patch = parse_mez_patch(
        "*** Begin Patch\n*** Update File: file\n@@\n-old\n+new\n context\n*** End Patch",
    )
    .unwrap();
    let plan = apply_mez_patch_to_snapshots(&patch, &snapshots).unwrap();
    assert_eq!(
        plan.changes[0].original,
        ApplyPatchOriginalState::Regular(bytes.to_vec())
    );
}

/// Supplies typed captured bytes without shell framing or filesystem access.
fn snapshots(entries: &[(&str, Option<&[u8]>)]) -> BTreeMap<String, ApplyPatchSnapshot> {
    entries
        .iter()
        .map(|(path, bytes)| {
            (
                path.to_string(),
                ApplyPatchSnapshot {
                    path: path.to_string(),
                    resolved_path: format!("/repo/{path}"),
                    state: bytes.map_or(ApplyPatchSnapshotState::Missing, |bytes| {
                        ApplyPatchSnapshotState::Regular(bytes.to_vec())
                    }),
                },
            )
        })
        .collect()
}

/// Authored move order survives path sorting and repeated-path operations
/// consume the preceding planned state, not the original captured bytes.
#[test]
fn native_plan_keeps_move_dependencies_and_intermediate_preimages() {
    use crate::native_action::{NativePatchOperation, PatchEffect};
    let operation = NativePatchOperation::parse("*** Begin Patch\n*** Update File: a\n*** Move to: z\n@@\n-old\n+middle\n*** Update File: z\n@@\n-middle\n+final\n*** End Patch", None).unwrap();
    let plan = native::plan_native_patch(
        &operation,
        &snapshots(&[("a", Some(b"old\r\n")), ("z", None)]),
    )
    .unwrap();
    assert!(plan.errors().is_empty());
    assert_eq!(plan.operations().len(), 2);
    assert!(matches!(
        plan.operations()[0].effect(),
        PatchEffect::Move { .. }
    ));
    assert_eq!(
        plan.operations()[0]
            .changes()
            .iter()
            .map(|change| change.path.as_str())
            .collect::<Vec<_>>(),
        ["z", "a"]
    );
    assert_eq!(
        plan.operations()[1].changes()[0].original,
        ApplyPatchOriginalState::Regular(b"middle\n".to_vec())
    );
    assert_eq!(
        plan.operations()[1].changes()[0].final_bytes.as_deref(),
        Some(b"final\n".as_slice())
    );
}

/// No-op matching must not normalize captured CRLF. An independent mismatch
/// retains earlier accepted changes and a typed operation-specific diagnostic.
#[test]
fn native_plan_preserves_noops_and_independent_partial_plans() {
    use crate::native_action::NativePatchOperation;
    let operation = NativePatchOperation::parse("*** Begin Patch\n*** Update File: same\n@@\n line\n*** Add File: new\n+x\n*** Update File: bad\n@@\n-missing\n+replacement\n*** End Patch", None).unwrap();
    let plan = native::plan_native_patch(
        &operation,
        &snapshots(&[
            ("same", Some(b"line\r\n")),
            ("new", None),
            ("bad", Some(b"actual\n")),
        ]),
    )
    .unwrap();
    assert!(plan.operations()[0].changes().is_empty());
    assert_eq!(
        plan.operations()[1].changes()[0].final_bytes.as_deref(),
        Some(b"x\n".as_slice())
    );
    assert_eq!(plan.errors()[0].0, 2);
}

/// Unified display is deterministic, handles Unicode and missing final
/// newlines, and truncation never alters authoritative final bytes/preimages.
#[test]
fn native_diff_is_bounded_without_losing_typed_changes() {
    let change = ApplyPatchFileChange {
        path: "note".into(),
        resolved_path: "/repo/note".into(),
        original: ApplyPatchOriginalState::Regular("old\n".as_bytes().to_vec()),
        final_bytes: Some("雪".as_bytes().to_vec()),
    };
    let complete = native::native_unified_diff(&change, usize::MAX).unwrap();
    assert!(!complete.truncated);
    assert!(
        complete
            .source
            .contains("@@ -1,1 +1,1 @@\n-old\n+雪\n\\ No newline at end of file\n")
    );
    let original = change.clone();
    let bounded = native::native_unified_diff(&change, 64).unwrap();
    assert!(bounded.source.len() <= 64);
    assert!(bounded.truncated);
    assert_eq!(change, original);
    assert_eq!(
        complete,
        native::native_unified_diff(&change, usize::MAX).unwrap()
    );
}

/// Adapter selection returns a parsed native patch without generating source,
/// while pane/remote patches retain the existing POSIX shell adapter.
#[test]
fn native_adapter_plan_does_not_materialize_shell_programs() {
    use crate::local_action::{LocalActionAdapterPlan, local_action_adapter_plan};
    let action = AgentAction {
        id: "patch".into(),
        payload: AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** Add File: a\n+x\n*** End Patch".into(),
            strip: None,
        },
    };
    assert!(matches!(
        local_action_adapter_plan(&action, true).unwrap(),
        Some(LocalActionAdapterPlan::NativePatch(_))
    ));
    assert!(matches!(
        local_action_adapter_plan(&action, false).unwrap(),
        Some(LocalActionAdapterPlan::Shell { .. })
    ));
}

/// Native planning rejects oversized raw source, unrelated snapshots and a
/// forged descriptor before exposing plans; typed missing/nonregular failures
/// remain operation-specific rather than becoming shell transport errors.
#[test]
fn native_planning_rejects_resource_and_identity_violations() {
    use crate::native_action::NativePatchOperation;
    let source = "*** Begin Patch\n*** Add File: a\n+x\n*** End Patch";
    assert!(
        NativePatchOperation::parse(&"x".repeat(native::NATIVE_PATCH_SOURCE_BYTES + 1), None)
            .is_err()
    );
    let operation = NativePatchOperation::parse(source, None).unwrap();
    assert!(native::plan_native_patch(&operation, &snapshots(&[("unrelated", None)])).is_err());
    let mut forged = operation.clone();
    forged.effects.clear();
    assert!(native::plan_native_patch(&forged, &snapshots(&[("a", None)])).is_err());
    let mut nonregular = snapshots(&[("a", None)]);
    nonregular.get_mut("a").unwrap().state = ApplyPatchSnapshotState::NonRegular;
    assert_eq!(
        native::plan_native_patch(&operation, &nonregular)
            .unwrap()
            .errors()
            .len(),
        1
    );
}

/// Creating or deleting a zero-byte file still emits a visible endpoint diff;
/// zero display budget cannot erase the authoritative file change.
#[test]
fn native_diff_preserves_empty_file_endpoint_identity() {
    let add = ApplyPatchFileChange {
        path: "empty".into(),
        resolved_path: "/repo/empty".into(),
        original: ApplyPatchOriginalState::Missing,
        final_bytes: Some(Vec::new()),
    };
    let diff = native::native_unified_diff(&add, usize::MAX).unwrap();
    assert!(
        diff.source
            .contains("--- /dev/null\n+++ b/empty\n@@ -0,0 +0,0 @@")
    );
    assert!(native::native_unified_diff(&add, 0).unwrap().truncated);
    let delete = ApplyPatchFileChange {
        original: ApplyPatchOriginalState::Regular(Vec::new()),
        final_bytes: None,
        ..add
    };
    assert!(
        native::native_unified_diff(&delete, usize::MAX)
            .unwrap()
            .source
            .contains("+++ /dev/null")
    );
}
