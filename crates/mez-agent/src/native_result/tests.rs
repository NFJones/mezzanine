//! Canonical typed-result ownership and partial-effect evidence regressions.

use super::*;
use crate::semantic_patch_planning::native::{NativePatchPlan, plan_native_patch};
use crate::semantic_patch_planning::{ApplyPatchSnapshot, ApplyPatchSnapshotState};

/// Minimal stable turn identity without filesystem or process adapters.
struct Turn;
impl AgentTurnResultIdentity for Turn {
    fn turn_id(&self) -> &str {
        "turn"
    }
    fn agent_id(&self) -> &str {
        "agent"
    }
}

/// Exact move fixture with a real pure plan, never execution evidence.
fn fixture() -> (
    AgentAction,
    NativeCommitIdentity,
    NativePatchPlan,
    NativeConfirmedEndpoint,
) {
    let patch =
        "*** Begin Patch\n*** Update File: a\n*** Move to: z\n@@\n-old\n+new\n*** End Patch";
    let action = AgentAction {
        id: "patch".into(),
        payload: AgentActionPayload::ApplyPatch {
            patch: patch.into(),
            strip: None,
        },
    };
    let identity = NativeCommitIdentity {
        turn_id: "turn".into(),
        action_id: "patch".into(),
        transaction: "transaction".into(),
        authority_generation: 7,
        operation: NativePatchOperation::parse(patch, None).unwrap(),
    };
    let snapshots = [
        ("a", ApplyPatchSnapshotState::Regular(b"old\n".to_vec())),
        ("z", ApplyPatchSnapshotState::Missing),
    ]
    .into_iter()
    .map(|(path, state)| {
        (
            path.to_string(),
            ApplyPatchSnapshot {
                path: path.into(),
                resolved_path: format!("/repo/{path}"),
                state,
            },
        )
    })
    .collect();
    let plan = plan_native_patch(&identity.operation, &snapshots).unwrap();
    let endpoint = NativeConfirmedEndpoint {
        operation_ordinal: 0,
        confirmation_ordinal: 0,
        path: "z".into(),
        kind: NativeConfirmedKind::Written,
    };
    (action, identity, plan, endpoint)
}

/// Partial move evidence survives failure and display truncation without shell
/// fields; missing source deletion cannot be relabeled confirmed success.
#[test]
fn partial_native_result_preserves_endpoints_without_shell_fields() {
    let (action, identity, plan, endpoint) = fixture();
    let display = "雪".repeat(100_000);
    let result = native_patch_result(
        &Turn,
        &action,
        &identity,
        &plan,
        NativeEffectCertainty::Partial,
        std::slice::from_ref(&endpoint),
        &display,
        Some("source deletion failed"),
    )
    .unwrap();
    assert_eq!(result.status, ActionStatus::Failed);
    result.validate_invariants().unwrap();
    let data: serde_json::Value =
        serde_json::from_str(result.structured_content_json.as_deref().unwrap()).unwrap();
    assert_eq!(data["execution_transport"], "native_runtime");
    assert_eq!(data["confirmed_endpoints"][0]["path"], "z");
    assert_eq!(data["display_truncated"], true);
    assert_eq!(data["automatic_replay"], false);
    for key in [
        "command",
        "terminal_observation",
        "exit_code",
        "signal",
        "stdout",
        "marker",
    ] {
        assert!(data.get(key).is_none());
    }
    assert!(
        native_patch_result(
            &Turn,
            &action,
            &identity,
            &plan,
            NativeEffectCertainty::Confirmed,
            &[endpoint],
            "",
            None
        )
        .is_err()
    );
}

/// Exact source/turn mismatch, wrong kinds, duplicates and source-before-
/// destination deletion fail closed; complete ordered endpoint evidence passes.
#[test]
fn native_projection_rejects_mismatched_and_inconsistent_evidence() {
    let (action, mut identity, plan, endpoint) = fixture();
    identity.turn_id = "other".into();
    assert!(
        native_patch_result(
            &Turn,
            &action,
            &identity,
            &plan,
            NativeEffectCertainty::Confirmed,
            &[],
            "",
            None
        )
        .is_err()
    );
    identity.turn_id = "turn".into();
    let mut wrong = endpoint.clone();
    wrong.kind = NativeConfirmedKind::Deleted;
    assert!(
        native_patch_result(
            &Turn,
            &action,
            &identity,
            &plan,
            NativeEffectCertainty::Partial,
            &[wrong],
            "",
            None
        )
        .is_err()
    );
    let mut deletion = endpoint.clone();
    deletion.path = "a".into();
    deletion.kind = NativeConfirmedKind::Deleted;
    assert!(
        native_patch_result(
            &Turn,
            &action,
            &identity,
            &plan,
            NativeEffectCertainty::Partial,
            &[deletion.clone()],
            "",
            None
        )
        .is_err()
    );
    deletion.confirmation_ordinal = 1;
    assert!(
        native_patch_result(
            &Turn,
            &action,
            &identity,
            &plan,
            NativeEffectCertainty::Confirmed,
            &[endpoint.clone(), deletion],
            "",
            None
        )
        .is_ok()
    );
    let mut duplicate = endpoint.clone();
    duplicate.confirmation_ordinal = 1;
    assert!(
        native_patch_result(
            &Turn,
            &action,
            &identity,
            &plan,
            NativeEffectCertainty::Partial,
            &[endpoint.clone(), duplicate],
            "",
            None
        )
        .is_err()
    );
    assert!(
        native_patch_result(
            &Turn,
            &action,
            &identity,
            &plan,
            NativeEffectCertainty::Unexecuted,
            &[endpoint],
            "",
            None
        )
        .is_err()
    );
}

/// In-flight work stays nonterminal and unknown effects cannot assert success;
/// a true planned no-op can complete without mutation endpoint confirmations.
#[test]
fn native_projection_distinguishes_inflight_unknown_and_confirmed_noop() {
    let (mut action, mut identity, plan, _) = fixture();
    let inflight = native_patch_result(
        &Turn,
        &action,
        &identity,
        &plan,
        NativeEffectCertainty::InFlight,
        &[],
        "",
        None,
    )
    .unwrap();
    assert_eq!(inflight.status, ActionStatus::Running);
    assert!(!inflight.is_terminal());
    assert_eq!(
        native_patch_result(
            &Turn,
            &action,
            &identity,
            &plan,
            NativeEffectCertainty::Unknown,
            &[],
            "",
            None
        )
        .unwrap()
        .status,
        ActionStatus::Failed
    );
    let patch = "*** Begin Patch\n*** Update File: a\n@@\n old\n*** End Patch";
    action.payload = AgentActionPayload::ApplyPatch {
        patch: patch.into(),
        strip: None,
    };
    identity.operation = NativePatchOperation::parse(patch, None).unwrap();
    let snapshots = std::collections::BTreeMap::from([(
        "a".into(),
        ApplyPatchSnapshot {
            path: "a".into(),
            resolved_path: "/repo/a".into(),
            state: ApplyPatchSnapshotState::Regular(b"old\n".to_vec()),
        },
    )]);
    let noop = plan_native_patch(&identity.operation, &snapshots).unwrap();
    let unplanned = NativeConfirmedEndpoint {
        operation_ordinal: 0,
        confirmation_ordinal: 0,
        path: "a".into(),
        kind: NativeConfirmedKind::Written,
    };
    assert!(
        native_patch_result(
            &Turn,
            &action,
            &identity,
            &noop,
            NativeEffectCertainty::Confirmed,
            &[unplanned],
            "",
            None
        )
        .is_err()
    );
    assert_eq!(
        native_patch_result(
            &Turn,
            &action,
            &identity,
            &noop,
            NativeEffectCertainty::Confirmed,
            &[],
            "",
            None
        )
        .unwrap()
        .status,
        ActionStatus::Succeeded
    );
}

/// Pruning a mutable execution/display copy cannot strip the authoritative
/// read-only plan or turn an unexecuted move into a positively planned no-op.
#[test]
fn stripped_execution_copy_cannot_weaken_authoritative_plan() {
    let (action, identity, plan, _) = fixture();
    let mut execution_copy = plan.operations()[0].changes().to_vec();
    execution_copy.clear();
    assert!(execution_copy.is_empty());
    assert_eq!(plan.operations()[0].changes().len(), 2);
    assert!(
        native_patch_result(
            &Turn,
            &action,
            &identity,
            &plan,
            NativeEffectCertainty::Confirmed,
            &[],
            "",
            None
        )
        .is_err()
    );
}

/// Repeated-path updates depend on intermediate preimages. Increasing worker
/// confirmation numbers cannot authorize reversing authored operation order.
#[test]
fn reversed_repeated_path_confirmations_are_rejected() {
    let (mut action, mut identity, _, _) = fixture();
    let patch = "*** Begin Patch\n*** Update File: a\n@@\n-old\n+middle\n*** Update File: a\n@@\n-middle\n+final\n*** End Patch";
    action.payload = AgentActionPayload::ApplyPatch {
        patch: patch.into(),
        strip: None,
    };
    identity.operation = NativePatchOperation::parse(patch, None).unwrap();
    let snapshots = std::collections::BTreeMap::from([(
        "a".into(),
        ApplyPatchSnapshot {
            path: "a".into(),
            resolved_path: "/repo/a".into(),
            state: ApplyPatchSnapshotState::Regular(b"old\n".to_vec()),
        },
    )]);
    let plan = plan_native_patch(&identity.operation, &snapshots).unwrap();
    let reversed = [
        NativeConfirmedEndpoint {
            operation_ordinal: 1,
            confirmation_ordinal: 0,
            path: "a".into(),
            kind: NativeConfirmedKind::Written,
        },
        NativeConfirmedEndpoint {
            operation_ordinal: 0,
            confirmation_ordinal: 1,
            path: "a".into(),
            kind: NativeConfirmedKind::Written,
        },
    ];
    assert!(
        native_patch_result(
            &Turn,
            &action,
            &identity,
            &plan,
            NativeEffectCertainty::Confirmed,
            &reversed,
            "",
            None
        )
        .is_err()
    );
    let ordered = [
        NativeConfirmedEndpoint {
            operation_ordinal: 0,
            confirmation_ordinal: 0,
            path: "a".into(),
            kind: NativeConfirmedKind::Written,
        },
        NativeConfirmedEndpoint {
            operation_ordinal: 1,
            confirmation_ordinal: 1,
            path: "a".into(),
            kind: NativeConfirmedKind::Written,
        },
    ];
    assert!(
        native_patch_result(
            &Turn,
            &action,
            &identity,
            &plan,
            NativeEffectCertainty::Confirmed,
            &ordered,
            "",
            None
        )
        .is_ok()
    );
}
