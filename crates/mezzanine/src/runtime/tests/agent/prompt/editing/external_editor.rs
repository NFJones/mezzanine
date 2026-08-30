//! External editor integration for pane-local agent prompts.

use super::*;
use crate::runtime::PaneSurfaceKind;

/// Returns the live editor transaction identities created by the prompt binding.
fn editor_transaction(service: &RuntimeSessionService) -> (String, String, String, String) {
    service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| match &transaction.kind {
            RunningShellTransactionKind::ExternalEditor {
                session_id,
                completion_nonce,
            } => Some((
                marker.clone(),
                transaction.turn_id.clone(),
                session_id.clone(),
                completion_nonce.clone(),
            )),
            _ => None,
        })
        .expect("prompt binding should create an editor transaction")
}

/// Builds a visible editable prompt backed by a live pane process.
fn prompt_editor_fixture(name: &str) -> (RuntimeSessionService, mez_core::ids::ClientId, PathBuf) {
    let root = temp_root(name);
    let socket_path = root.join("runtime/default.sock");
    let mut service = RuntimeServiceFixture::new()
        .control_socket(&socket_path)
        .build();
    let primary = service
        .attach_primary("primary", true, Size::new(40, 16).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "prompt-editor-test".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\nshell_mode = \"pane\"\n[external_editor]\ncommand = [\"/bin/sh\", \"-c\", \"exit 0\", \"{file}\"]\nfallback = []\n"
                .to_string(),
        }])
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    (service, primary, root)
}

/// Invokes the semantic binding and returns its correlated transaction identities.
fn start_prompt_editor(
    service: &mut RuntimeSessionService,
    primary: &mez_core::ids::ClientId,
) -> (String, String, String, String) {
    let report = service
        .apply_attached_terminal_step_plan(
            primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ExecuteMux(
                    MuxAction::EditAgentPrompt,
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(report.mux_actions_applied, 1);
    assert!(report.full_redraw_required);
    assert_eq!(service.agent_prompt_reserved_rows_for_pane("%1", 40, 14), 0);
    assert_eq!(
        service.presented_pane_surface("%1"),
        PaneSurfaceKind::Process
    );
    editor_transaction(service)
}

/// Completes the correlated shell transaction after replacing its private draft.
fn complete_prompt_editor(
    service: &mut RuntimeSessionService,
    root: &Path,
    identities: &(String, String, String, String),
    draft: &[u8],
    exit_code: i32,
) {
    let (marker, turn_id, session_id, _) = identities;
    let draft_path = root
        .join("runtime/editor-sessions")
        .join(session_id)
        .join("draft.md");
    fs::write(draft_path, draft).unwrap();
    service
        .observe_agent_shell_transaction_start("%1", marker, turn_id, "mez-ui", "%1")
        .unwrap();
    service
        .observe_agent_shell_transaction_end("%1", marker, turn_id, "mez-ui", "%1", exit_code)
        .unwrap();
}

/// Verifies a successful changed draft returns to the in-pane prompt without
/// submission or history mutation and places the cursor at the end.
#[test]
fn runtime_prompt_editor_binding_restores_changed_text_without_submitting() {
    let (mut service, primary, root) = prompt_editor_fixture("prompt-editor-changed");
    service
        .apply_attached_agent_prompt_input_for_pane(&primary, "%1", b"before\nsecond")
        .unwrap();
    let identities = start_prompt_editor(&mut service, &primary);
    complete_prompt_editor(&mut service, &root, &identities, b"after\nchanged\n", 0);

    let prompt = service.agent_prompt_inputs_for_tests().get("%1").unwrap();
    assert_eq!(prompt.prompt.buffer.line(), "after\nchanged");
    assert_eq!(prompt.prompt.buffer.cursor(), "after\nchanged".len());
    assert!(prompt.prompt.buffer.history().is_empty());
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert_eq!(service.presented_pane_surface("%1"), PaneSurfaceKind::Agent);
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies empty successful output is a valid empty prompt and still does not submit.
#[test]
fn runtime_prompt_editor_accepts_empty_output_without_submitting() {
    let (mut service, primary, root) = prompt_editor_fixture("prompt-editor-empty");
    service
        .apply_attached_agent_prompt_input_for_pane(&primary, "%1", b"erase me")
        .unwrap();
    let identities = start_prompt_editor(&mut service, &primary);
    complete_prompt_editor(&mut service, &root, &identities, b"", 0);

    let prompt = service.agent_prompt_inputs_for_tests().get("%1").unwrap();
    assert_eq!(prompt.prompt.buffer.line(), "");
    assert!(prompt.prompt.buffer.history().is_empty());
    assert!(service.pending_agent_provider_tasks().is_empty());
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies nonzero editor exit restores every prompt field exactly, including
/// decoder, display, selector metadata, and pending Ctrl+C confirmation.
#[test]
fn runtime_prompt_editor_nonzero_exit_restores_exact_snapshot() {
    let (mut service, primary, root) = prompt_editor_fixture("prompt-editor-nonzero");
    service
        .apply_attached_agent_prompt_input_for_pane(&primary, "%1", b"keep me")
        .unwrap();
    {
        let state = service
            .agent_prompt_inputs_mut_for_tests()
            .get_mut("%1")
            .unwrap();
        state.display_lines = vec!["status retained".to_string()];
        state.pending_ctrl_c_exit_at_unix_ms = Some(42);
        let _ = state.decoder.decode(b"\x1b[").unwrap();
    }
    let expected = service.agent_prompt_inputs_for_tests()["%1"].clone();
    let identities = start_prompt_editor(&mut service, &primary);
    complete_prompt_editor(&mut service, &root, &identities, b"discard me", 7);

    assert_eq!(service.agent_prompt_inputs_for_tests()["%1"], expected);
    assert!(service.pending_agent_provider_tasks().is_empty());
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies an editor-added final newline that normalizes to the original text
/// preserves the complete prompt snapshot rather than resetting transient state.
#[test]
fn runtime_prompt_editor_normalized_unchanged_output_restores_exact_snapshot() {
    let (mut service, primary, root) = prompt_editor_fixture("prompt-editor-unchanged");
    service
        .apply_attached_agent_prompt_input_for_pane(&primary, "%1", b"unchanged")
        .unwrap();
    {
        let state = service
            .agent_prompt_inputs_mut_for_tests()
            .get_mut("%1")
            .unwrap();
        state.display_lines = vec!["status retained".to_string()];
        state.pending_ctrl_c_exit_at_unix_ms = Some(42);
        let _ = state.decoder.decode(b"\x1b[").unwrap();
    }
    let expected = service.agent_prompt_inputs_for_tests()["%1"].clone();
    let identities = start_prompt_editor(&mut service, &primary);
    complete_prompt_editor(&mut service, &root, &identities, b"unchanged\n", 0);

    assert_eq!(service.agent_prompt_inputs_for_tests()["%1"], expected);
    assert!(service.pending_agent_provider_tasks().is_empty());
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies invalid UTF-8 editor output cannot replace prompt text and instead
/// restores every captured prompt field without submitting a turn.
#[test]
fn runtime_prompt_editor_invalid_utf8_restores_exact_snapshot() {
    let (mut service, primary, root) = prompt_editor_fixture("prompt-editor-invalid-utf8");
    service
        .apply_attached_agent_prompt_input_for_pane(&primary, "%1", b"keep valid text")
        .unwrap();
    let expected = service.agent_prompt_inputs_for_tests()["%1"].clone();
    let identities = start_prompt_editor(&mut service, &primary);
    complete_prompt_editor(&mut service, &root, &identities, &[0xff, 0xfe], 0);

    assert_eq!(service.agent_prompt_inputs_for_tests()["%1"], expected);
    assert!(service.pending_agent_provider_tasks().is_empty());
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies the semantic binding is rejected when no visible agent prompt owns
/// the focused pane and never opens an editor transaction or inserts text.
#[test]
fn runtime_prompt_editor_binding_rejects_inactive_prompt() {
    let root = temp_root("prompt-editor-inactive");
    let socket_path = root.join("runtime/default.sock");
    let mut service = RuntimeServiceFixture::new()
        .control_socket(&socket_path)
        .build();
    let primary = service
        .attach_primary("primary", true, Size::new(40, 16).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    mark_test_pane_ready(&mut service, "%1");

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ExecuteMux(
                    MuxAction::EditAgentPrompt,
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.mux_actions_applied, 0);
    assert!(report.full_redraw_required);
    assert!(service.running_shell_transactions_for_tests().is_empty());
    assert!(!service.external_editor_session_is_active("%1"));
    assert!(
        service
            .primary_error_status_overlay()
            .is_some_and(|message| message.contains("agent prompt is not active"))
    );
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}
