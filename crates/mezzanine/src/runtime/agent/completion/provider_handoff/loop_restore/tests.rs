//! Snapshot-only metadata reset contract, not runtime restoration authority.

use super::contract_restored_metadata;
use mez_agent::{AgentShellStore, AllowedActionSet};

/// Exercise the actual exact-running-turn restore and supported snapshot facade
/// with all three metadata fields nonempty. Normalizing only those fields must
/// recover the complete returned snapshot; no CID/turn/lineage/history field is
/// assigned by the reset. This is lower-store synthetic data, not an installed
/// runtime completion/controller/native proof or a general None-permission claim.
#[test]
fn restored_snapshot_resets_only_generic_metadata_fields() {
    let mut store = AgentShellStore::default();
    let mut initial = store.enter_or_resume("%metadata").unwrap().clone();
    initial.display_name = Some("synthetic parent metadata".into());
    store.restore_session("%metadata", initial).unwrap();
    store
        .capture_allowed_actions("%metadata", AllowedActionSet::capability_decision())
        .unwrap();
    store
        .capture_project_scope(
            "%metadata",
            mez_agent::messaging::ProjectMembership::from_canonical_root(std::path::PathBuf::from(
                "/synthetic/metadata-project",
            )),
        )
        .unwrap();
    store.start_turn("%metadata", "known-running-turn").unwrap();
    let before = store
        .restore_conversation_for_running_turn_with_lineage(
            "%metadata",
            "known-running-turn",
            "synthetic-restored-parent",
            mez_agent::AgentTranscriptPosition::uncompacted(17),
            Some("synthetic-parent-lineage".into()),
        )
        .unwrap()
        .clone();
    assert!(
        before.allowed_actions.is_some()
            && before.display_name.is_some()
            && before.project_scope.is_some()
    );
    let contracted = contract_restored_metadata(before.clone());
    let after = store
        .restore_session("%metadata", contracted)
        .unwrap()
        .clone();
    assert!(
        after.allowed_actions.is_none()
            && after.display_name.is_none()
            && after.project_scope.is_none()
    );
    assert_eq!(after.running_turn_id.as_deref(), Some("known-running-turn"));
    let mut normalized = after;
    normalized.allowed_actions = before.allowed_actions.clone();
    normalized.display_name = before.display_name.clone();
    normalized.project_scope = before.project_scope.clone();
    assert!(
        normalized == before,
        "only the generic metadata reset fields may differ"
    );
}
