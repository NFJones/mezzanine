//! Runtime executor-owned action presentation progress regressions.
//!
//! These tests keep provisional executor observations outside canonical action
//! results and durable presentation until an exact settlement promotes them.

use super::*;
use mez_agent::{
    ActionPresentationComponentIdentity, ActionPresentationExecutionIdentity,
    ActionPresentationProgress,
};

/// Installs one running action and an exact managed-shell transaction marker.
///
/// The fixture deliberately bypasses process I/O: these tests exercise the
/// actor-owned identity and compositor boundary after an executor has already
/// produced a typed cumulative progress snapshot.
fn running_action_progress_fixture(
    action: mez_agent::AgentAction,
    marker: &str,
    transaction_command: &str,
) -> (RuntimeSessionService, mez_agent::AgentTurnRecord) {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "exercise executor progress")
        .unwrap();
    service.remove_pending_agent_provider_task(&started.turn_id);
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    service.agent_turn_executions_mut().insert(
        turn.turn_id.clone(),
        mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "running action progress fixture".to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "exercise executor progress".to_string(),
                    thought: None,
                    turn_id: turn.turn_id.clone(),
                    agent_id: turn.agent_id.clone(),
                    actions: vec![action.clone()],
                    final_turn: false,
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![mez_agent::ActionResult::running(
                &turn,
                &action,
                Vec::new(),
                None,
            )],
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        },
    );
    service.register_running_shell_transaction(
        marker.to_string(),
        RunningShellTransactionRef {
            turn_id: turn.turn_id.clone(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: action.id.clone(),
            },
            pane_id: "%1".to_string(),
            command: transaction_command.to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
        false,
    );
    (service, turn)
}

fn shell_action() -> mez_agent::AgentAction {
    mez_agent::AgentAction {
        id: "shell-1".to_string(),
        rationale: "observe a running shell command".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Observe a running shell command".to_string(),
            command: "sleep 1".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    }
}

fn patch_action() -> mez_agent::AgentAction {
    mez_agent::AgentAction {
        id: "patch-1".to_string(),
        rationale: "update one file".to_string(),
        payload: mez_agent::AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** Add File: note.txt\n+new\n*** End Patch".to_string(),
            strip: None,
        },
    }
}

fn transaction_progress(
    turn_id: &str,
    action_id: &str,
    marker: &str,
    revision: u64,
    component: ActionPresentationComponentIdentity,
    source: &str,
) -> ActionPresentationProgress {
    ActionPresentationProgress::new(
        turn_id,
        action_id,
        ActionPresentationExecutionIdentity::Transaction(marker.to_string()),
        revision,
        component,
        source,
    )
}

/// Verifies exact execution identity and monotonically newer component revisions
/// fence executor progress before it can replace the pane. The first accepted
/// source composes over an existing shell-preview baseline, while a stale marker,
/// stale revision, and update after an unrelated screen lineage all leave that
/// baseline and the intervening pane content unchanged.
#[test]
fn runtime_action_progress_fences_identity_revision_and_screen_lineage() {
    let (mut service, turn) =
        running_action_progress_fixture(shell_action(), "marker-1", "sleep 1");
    service
        .append_agent_command_preview_to_terminal_buffer("%1", "sleep 1")
        .unwrap();
    service
        .update_agent_shell_output_preview(
            "%1",
            crate::runtime::render::RuntimeAgentShellPreviewOwner {
                turn_id: turn.turn_id.clone(),
                action_id: "shell-1".to_string(),
                marker: "marker-1".to_string(),
            },
            1,
            &["shell-preview-baseline".to_string()],
        )
        .unwrap();

    assert!(
        service
            .apply_action_presentation_progress(transaction_progress(
                &turn.turn_id,
                "shell-1",
                "marker-1",
                2,
                ActionPresentationComponentIdentity::ShellOutput,
                "executor-progress-new",
            ))
            .unwrap()
    );
    let composed = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(composed.contains("shell-preview-baseline"), "{composed}");
    assert!(composed.contains("executor-progress-new"), "{composed}");

    assert!(
        !service
            .apply_action_presentation_progress(transaction_progress(
                &turn.turn_id,
                "shell-1",
                "marker-stale",
                3,
                ActionPresentationComponentIdentity::ShellOutput,
                "wrong-marker-output",
            ))
            .unwrap()
    );
    assert!(
        !service
            .apply_action_presentation_progress(transaction_progress(
                &turn.turn_id,
                "shell-1",
                "marker-1",
                1,
                ActionPresentationComponentIdentity::ShellOutput,
                "stale-revision-output",
            ))
            .unwrap()
    );

    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let mut replacement = service.agent_pane_screen("%1").unwrap().clone();
    replacement.feed(b"\r\nintervening-lineage\r\n");
    service.set_agent_pane_screen("%1".to_string(), conversation_id, replacement);
    let before_stale_lineage = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        !service
            .apply_action_presentation_progress(transaction_progress(
                &turn.turn_id,
                "shell-1",
                "marker-1",
                3,
                ActionPresentationComponentIdentity::ShellOutput,
                "stale-lineage-output",
            ))
            .unwrap()
    );
    let after_stale_lineage = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(after_stale_lineage, before_stale_lineage);
    assert!(after_stale_lineage.contains("intervening-lineage"));
    assert!(!after_stale_lineage.contains("stale-lineage-output"));
}

/// Verifies ordinary read bodies remain hidden in normal logging mode even
/// when an exact managed read-phase marker and running action accept their
/// typed source. Acceptance may retain bounded reconciliation state, but it
/// must not bypass the same visibility gate used by final result rendering.
#[test]
fn runtime_action_progress_hidden_read_body_does_not_render_in_normal_mode() {
    let (mut service, turn) = running_action_progress_fixture(
        patch_action(),
        "read-marker",
        "# __MEZ_APPLY_PATCH_READ_PHASE__",
    );
    assert!(
        service
            .apply_action_presentation_progress(transaction_progress(
                &turn.turn_id,
                "patch-1",
                "read-marker",
                1,
                ActionPresentationComponentIdentity::ProvisionalReadBody,
                "hidden-read-body",
            ))
            .unwrap()
    );
    let pane_text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!pane_text.contains("hidden-read-body"), "{pane_text}");
    assert_eq!(
        service.action_presentation_progress_counts_for_tests("%1"),
        (1, 0)
    );
}

/// Verifies a failed provisional component rolls back to its exact baseline,
/// while a successful result whose canonical content exactly matches the
/// retained cumulative source promotes that source and tells final-result
/// presentation to suppress replay. Neither path mutates the canonical result.
#[test]
fn runtime_action_progress_reconciles_provisional_results_without_replay() {
    let action = shell_action();
    let (mut service, turn) =
        running_action_progress_fixture(action.clone(), "marker-1", "sleep 1");
    let failed_progress = transaction_progress(
        &turn.turn_id,
        &action.id,
        "marker-1",
        1,
        ActionPresentationComponentIdentity::ShellOutput,
        "provisional-failure-output",
    );
    assert!(
        service
            .apply_action_presentation_progress(failed_progress.clone())
            .unwrap()
    );
    let failed = mez_agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::Failed,
        "test_failure",
        "executor failed",
    )
    .unwrap();
    assert!(
        !service
            .reconcile_action_presentation_progress(&failed_progress, &failed)
            .unwrap()
    );
    let rolled_back = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!rolled_back.contains("provisional-failure-output"));

    let successful_progress = transaction_progress(
        &turn.turn_id,
        &action.id,
        "marker-1",
        2,
        ActionPresentationComponentIdentity::ShellOutput,
        "exact-success-output",
    );
    assert!(
        service
            .apply_action_presentation_progress(successful_progress.clone())
            .unwrap()
    );
    let succeeded = mez_agent::ActionResult::succeeded(
        &turn,
        &action,
        vec!["exact-success-output".to_string()],
        None,
    );
    let canonical_before = succeeded.clone();
    assert!(
        service
            .reconcile_action_presentation_progress(&successful_progress, &succeeded)
            .unwrap()
    );
    assert_eq!(succeeded, canonical_before);
    let promoted = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(promoted.matches("exact-success-output").count(), 1);
}

/// Verifies an executor-confirmed semantic mutation uses the readable static
/// diff renderer, promotes exactly once, and remains in the durable baseline
/// when a later action settlement fails. Repeated promotion and failure
/// reconciliation must not duplicate the already-confirmed diff section.
#[test]
fn runtime_action_progress_confirmed_diff_survives_failure_without_duplicates() {
    let action = patch_action();
    let (mut service, turn) = running_action_progress_fixture(
        action.clone(),
        "write-marker",
        "# __MEZ_APPLY_PATCH_WRITE_PHASE__",
    );
    let diff = "diff --git a/note.txt b/note.txt\n--- a/note.txt\n+++ b/note.txt\n@@ -1 +1 @@\n-old\n+confirmed-new\n";
    let progress = transaction_progress(
        &turn.turn_id,
        &action.id,
        "write-marker",
        1,
        ActionPresentationComponentIdentity::confirmed_mutation(0, "note.txt"),
        diff,
    );
    assert!(
        service
            .apply_action_presentation_progress(progress.clone())
            .unwrap()
    );
    assert!(
        service
            .promote_confirmed_action_presentation_progress(&progress)
            .unwrap()
    );
    assert!(
        !service
            .promote_confirmed_action_presentation_progress(&progress)
            .unwrap()
    );
    let failed = mez_agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::Failed,
        "later_failure",
        "a later patch section failed",
    )
    .unwrap();
    assert!(
        !service
            .reconcile_action_presentation_progress(&progress, &failed)
            .unwrap()
    );
    let pane_text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(pane_text.matches("confirmed-new").count(), 1, "{pane_text}");
    assert_eq!(
        service.action_presentation_progress_counts_for_tests("%1"),
        (0, 1)
    );
}
