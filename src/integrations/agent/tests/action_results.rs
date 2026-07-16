//! Agent tests for product-owned action result behavior.
//!
//! Deterministic action-result contracts and context rendering are tested in
//! `mez-agent`; this leaf retains product action-lowering integration coverage.

use super::*;

#[test]
/// Verifies semantic file actions keep completion output available for elevated
/// action-result display.
///
/// Normal mode logs a single human-readable action line, but debug-style views
/// still need the semantic lowerings to expose their cleaned output payloads
/// after the hidden shell transaction completes.
fn semantic_file_actions_keep_displayable_completion_output_available() {
    let patch = AgentAction {
        id: "patch-1".to_string(),
        rationale: String::new(),
        payload: AgentActionPayload::ApplyPatch {
            patch: add_file_patch("note.txt", "one\ntwo\n"),
            strip: None,
        },
    };

    let patch_plan = local_action_plan(&patch).unwrap().unwrap();

    assert!(patch_plan.display_output_after_completion);
    assert_eq!(patch_plan.policy_command, "apply_patch");
    assert!(patch_plan.command.contains("base64"));
    assert!(!patch_plan.command.contains("python3"));
}
