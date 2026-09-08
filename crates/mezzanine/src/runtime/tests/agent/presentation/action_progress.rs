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
                    rationale: "exercise executor progress".to_string(),

                    actions: vec![action.clone()],
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

/// Verifies a row-only pane resize rebases retained action-progress state so a
/// later revision cannot restore the previous screen height.
#[test]
fn runtime_row_only_resize_rebases_running_action_progress() {
    let (mut service, turn) =
        running_action_progress_fixture(shell_action(), "marker-rows", "sleep 1");
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    assert!(
        service
            .apply_action_presentation_progress(transaction_progress(
                &turn.turn_id,
                "shell-1",
                "marker-rows",
                1,
                ActionPresentationComponentIdentity::ShellOutput,
                "progress before row resize",
            ))
            .unwrap()
    );
    let old_size = service.agent_pane_screen("%1").unwrap().size();

    service
        .resize_attached_primary_terminal(&primary, Size::new(80, 30).unwrap())
        .unwrap();

    let resized_size = service.agent_pane_screen("%1").unwrap().size();
    assert_eq!(resized_size.columns, old_size.columns);
    assert!(
        resized_size.rows > old_size.rows,
        "{old_size:?} -> {resized_size:?}"
    );
    assert!(
        service
            .apply_action_presentation_progress(transaction_progress(
                &turn.turn_id,
                "shell-1",
                "marker-rows",
                2,
                ActionPresentationComponentIdentity::ShellOutput,
                "progress after row resize",
            ))
            .unwrap()
    );
    assert_eq!(
        service.agent_pane_screen("%1").unwrap().size(),
        resized_size
    );
    assert!(
        service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .iter()
            .any(|line| line.contains("progress after row resize"))
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies live shell progress wraps to the configured agent column cap and
/// retains only the newest physical rows after wrapping.
#[test]
fn runtime_shell_action_progress_honors_configured_column_cap() {
    let (mut service, turn) =
        running_action_progress_fixture(shell_action(), "marker-cap", "sleep 1");
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[terminal]\nagent_wrap_column_cap = 16\nshell_output_preview_lines = 3\n"
                .to_string(),
        }])
        .unwrap();

    assert!(
        service
            .apply_action_presentation_progress(transaction_progress(
                &turn.turn_id,
                "shell-1",
                "marker-cap",
                1,
                ActionPresentationComponentIdentity::ShellOutput,
                "discarddiscard alpha beta gamma 0123456789abcdefghij",
            ))
            .unwrap()
    );

    let visible = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    let progress_rows = visible
        .iter()
        .filter(|line| {
            line.contains("gamma")
                || line.contains("efghij")
                || line.chars().any(|character| character.is_ascii_digit())
        })
        .collect::<Vec<_>>();
    assert_eq!(progress_rows.len(), 3, "{visible:?}");
    assert!(
        progress_rows
            .iter()
            .all(|line| unicode_width::UnicodeWidthStr::width(line.as_str()) <= 16),
        "{visible:?}"
    );
    assert!(
        visible.iter().all(|line| !line.contains("discarddiscard")),
        "{visible:?}"
    );
}

/// Verifies debug-visible provisional read output uses the same configured
/// cap for whitespace wrapping and hard splitting of unbroken tokens.
#[test]
fn runtime_provisional_read_progress_honors_configured_column_cap() {
    let (mut service, turn) = running_action_progress_fixture(
        patch_action(),
        "read-cap",
        "# __MEZ_APPLY_PATCH_READ_PHASE__",
    );
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[terminal]\nagent_wrap_column_cap = 16\n".to_string(),
        }])
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Debug)
        .unwrap();

    assert!(
        service
            .apply_action_presentation_progress(transaction_progress(
                &turn.turn_id,
                "patch-1",
                "read-cap",
                1,
                ActionPresentationComponentIdentity::ProvisionalReadBody,
                "read alpha beta 0123456789abcdefghij",
            ))
            .unwrap()
    );

    let visible = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    let progress_rows = visible
        .iter()
        .filter(|line| {
            line.contains("read")
                || line.contains("alpha")
                || line.contains("beta")
                || line.chars().any(|character| character.is_ascii_digit())
        })
        .collect::<Vec<_>>();
    assert!(progress_rows.len() >= 3, "{visible:?}");
    assert!(
        progress_rows
            .iter()
            .all(|line| unicode_width::UnicodeWidthStr::width(line.as_str()) <= 16),
        "{visible:?}"
    );
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

/// Verifies retiring executor progress preserves a full pane's viewport origin.
///
/// Progress rows may scroll durable content while they are visible. Retirement
/// must clear that exact suffix from the installed composite rather than
/// rebuilding from the older baseline and moving every visible row.
#[test]
fn runtime_action_progress_retirement_preserves_bottom_viewport_origin() {
    let (mut service, turn) =
        running_action_progress_fixture(shell_action(), "marker-bottom", "sleep 1");
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let mut screen = TerminalScreen::new(Size::new(60, 5).unwrap(), 40).unwrap();
    screen.feed(b"durable-zero\r\ndurable-one\r\ndurable-two\r\ndurable-three\r\ndurable-four");
    service.set_agent_pane_screen("%1", conversation_id, screen);

    assert!(
        service
            .apply_action_presentation_progress(transaction_progress(
                &turn.turn_id,
                "shell-1",
                "marker-bottom",
                1,
                ActionPresentationComponentIdentity::ShellOutput,
                "progress-one\nprogress-two\nprogress-three",
            ))
            .unwrap()
    );
    let projected = service.agent_pane_screen("%1").unwrap();
    let history_len = projected.history().len();
    assert!(
        projected
            .visible_lines()
            .iter()
            .any(|line| line.contains("progress-three"))
    );

    assert_eq!(
        service
            .retire_action_presentation_progress_for_action(&turn.turn_id, "shell-1")
            .unwrap(),
        1
    );

    let retired = service.agent_pane_screen("%1").unwrap();
    assert_eq!(retired.history().len(), history_len);
    assert_eq!(retired.visible_lines()[0], "durable-four");
    assert!(
        retired
            .visible_lines()
            .iter()
            .all(|line| !line.contains("progress-"))
    );
    assert_eq!(retired.cursor_state().row, 1);
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
    let promoted_screen = service.agent_pane_screen("%1").unwrap().clone();
    let promoted_lineage = service
        .agent_pane_screen_lineage("%1", &turn.conversation_id)
        .unwrap();

    assert_eq!(
        service
            .retire_action_presentation_progress_for_action(&turn.turn_id, &action.id)
            .unwrap(),
        1
    );
    assert_eq!(service.agent_pane_screen("%1").unwrap(), &promoted_screen);
    assert_eq!(
        service.agent_pane_screen_lineage("%1", &turn.conversation_id),
        Some(promoted_lineage)
    );
    assert_eq!(
        service.action_presentation_progress_counts_for_tests("%1"),
        (0, 0)
    );
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
