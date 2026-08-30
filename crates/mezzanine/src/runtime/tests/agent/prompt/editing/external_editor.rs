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

/// Verifies failed changed drafts remain private and recoverable, observers
/// cannot inspect or mutate them, explicit apply is conflict-safe, and discard
/// remains idempotent after a successful apply consumes the recovery.
#[test]
fn runtime_prompt_editor_recovery_is_primary_authorized_and_conflict_safe() {
    let (mut service, primary, root) = prompt_editor_fixture("prompt-editor-recovery-apply");
    service
        .apply_attached_agent_prompt_input_for_pane(&primary, "%1", b"original prompt")
        .unwrap();
    let identities = start_prompt_editor(&mut service, &primary);
    let session_id = identities.2.clone();
    complete_prompt_editor(
        &mut service,
        &root,
        &identities,
        b"retained private draft\n",
        7,
    );

    assert_eq!(
        service.agent_prompt_inputs_for_tests()["%1"]
            .prompt
            .buffer
            .line(),
        "original prompt"
    );
    let observer = service
        .session
        .attach_observer_with_terminal("observer", None, 1)
        .unwrap();
    assert!(service.list_external_editor_recoveries(&observer).is_err());
    assert!(
        service
            .apply_external_editor_recovery(&observer, "%1", &session_id)
            .is_err()
    );
    assert!(
        service
            .discard_external_editor_recovery(&observer, "%1", &session_id)
            .is_err()
    );

    let listing = service.list_external_editor_recoveries(&primary).unwrap();
    assert!(listing.contains(&session_id), "{listing}");
    assert!(listing.contains("nonzero_exit"), "{listing}");
    assert!(!listing.contains("retained private draft"), "{listing}");

    service
        .apply_attached_agent_prompt_input_for_pane(&primary, "%1", b" changed")
        .unwrap();
    assert!(
        service
            .apply_external_editor_recovery(&primary, "%1", &session_id)
            .is_err()
    );
    let listing = service.list_external_editor_recoveries(&primary).unwrap();
    assert!(listing.contains("conflicted"), "{listing}");

    service
        .agent_prompt_inputs_mut_for_tests()
        .get_mut("%1")
        .unwrap()
        .prompt
        .buffer
        .set_line(String::new());
    assert!(
        service
            .apply_external_editor_recovery(&primary, "%1", &session_id)
            .is_err()
    );

    service
        .agent_prompt_inputs_mut_for_tests()
        .get_mut("%1")
        .unwrap()
        .prompt
        .buffer
        .set_line("original prompt".to_string());
    service
        .apply_external_editor_recovery(&primary, "%1", &session_id)
        .unwrap();
    assert_eq!(
        service.agent_prompt_inputs_for_tests()["%1"]
            .prompt
            .buffer
            .line(),
        "retained private draft"
    );
    assert!(
        !service
            .discard_external_editor_recovery(&primary, "%1", &session_id)
            .unwrap()
    );
    assert!(
        !root
            .join("runtime/editor-sessions")
            .join(&session_id)
            .exists()
    );
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies reopen seeds a fresh editor from retained content without applying
/// it, and even a successful reopened editor remains pending until explicit
/// apply consumes the fresh recovery.
#[test]
fn runtime_prompt_editor_reopen_never_auto_applies_recovered_content() {
    let (mut service, primary, root) = prompt_editor_fixture("prompt-editor-recovery-reopen");
    service
        .apply_attached_agent_prompt_input_for_pane(&primary, "%1", b"original prompt")
        .unwrap();
    let first = start_prompt_editor(&mut service, &primary);
    let first_session_id = first.2.clone();
    complete_prompt_editor(&mut service, &root, &first, b"retained draft", 9);

    service
        .reopen_external_editor_recovery(&primary, "%1", &first_session_id)
        .unwrap();
    let reopened = editor_transaction(&service);
    assert_ne!(reopened.2, first_session_id);
    assert_eq!(
        fs::read_to_string(
            root.join("runtime/editor-sessions")
                .join(&reopened.2)
                .join("draft.md")
        )
        .unwrap(),
        "retained draft"
    );
    assert!(
        !root
            .join("runtime/editor-sessions")
            .join(&first_session_id)
            .exists()
    );
    assert_eq!(
        service.agent_prompt_inputs_for_tests()["%1"]
            .prompt
            .buffer
            .line(),
        "original prompt"
    );

    complete_prompt_editor(&mut service, &root, &reopened, b"edited again\n", 0);
    assert_eq!(
        service.agent_prompt_inputs_for_tests()["%1"]
            .prompt
            .buffer
            .line(),
        "original prompt"
    );
    let listing = service.list_external_editor_recoveries(&primary).unwrap();
    assert!(listing.contains(&reopened.2), "{listing}");
    assert!(listing.contains("changed_unapplied"), "{listing}");

    service
        .apply_external_editor_recovery(&primary, "%1", &reopened.2)
        .unwrap();
    assert_eq!(
        service.agent_prompt_inputs_for_tests()["%1"]
            .prompt
            .buffer
            .line(),
        "edited again"
    );
    assert!(
        !root
            .join("runtime/editor-sessions")
            .join(&reopened.2)
            .exists()
    );
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies interrupted sessions are rediscovered for the same runtime session
/// after reconstruction, are never auto-applied, and can be discarded once.
#[test]
fn runtime_prompt_editor_recovery_is_discovered_after_restart_without_auto_apply() {
    let (mut service, primary, root) = prompt_editor_fixture("prompt-editor-recovery-restart");
    service
        .apply_attached_agent_prompt_input_for_pane(&primary, "%1", b"original prompt")
        .unwrap();
    let identities = start_prompt_editor(&mut service, &primary);
    let session_id = identities.2.clone();
    fs::write(
        root.join("runtime/editor-sessions")
            .join(&session_id)
            .join("draft.md"),
        b"orphaned private draft",
    )
    .unwrap();
    assert!(service.abort_external_editor_session("%1"));
    let restarted_session = (*service.session).clone();
    service.terminate_all_pane_processes().unwrap();
    drop(service);

    let mut restarted = RuntimeServiceFixture::new()
        .control_socket(root.join("runtime/default.sock"))
        .build_with_session(restarted_session);
    let listing = restarted.list_external_editor_recoveries(&primary).unwrap();
    assert!(listing.contains(&session_id), "{listing}");
    assert!(listing.contains("interrupted"), "{listing}");
    assert!(!listing.contains("orphaned private draft"), "{listing}");
    assert!(
        restarted
            .agent_prompt_inputs_for_tests()
            .get("%1")
            .is_none_or(|prompt| prompt.prompt.buffer.line().is_empty())
    );
    assert!(
        restarted
            .discard_external_editor_recovery(&primary, "%1", &session_id)
            .unwrap()
    );
    assert!(
        !restarted
            .discard_external_editor_recovery(&primary, "%1", &session_id)
            .unwrap()
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies the framed terminal-step path used by Iroh bypasses prefix,
/// keybinding, and bracketed-paste decoding while the initiating primary owns
/// an editor lease. The same request also carries authoritative resize state.
#[test]
fn runtime_prompt_editor_framed_input_forwards_exact_bytes_and_resize() {
    let (mut service, primary, root) = prompt_editor_fixture("prompt-editor-framed-input");
    service
        .apply_attached_agent_prompt_input_for_pane(&primary, "%1", b"unchanged prompt")
        .unwrap();
    let start_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "editor-start",
        "method": "terminal/step",
        "params": {
            "idempotency_key": "editor-start",
            "client_size": {"columns": 40, "rows": 16},
            "render": false,
            "input_bytes": [1, 101],
        }
    })
    .to_string();
    let start_response = service.dispatch_runtime_control_body(&start_request, &primary);
    let start_response: serde_json::Value = serde_json::from_str(&start_response).unwrap();
    assert!(start_response.get("error").is_none(), "{start_response}");
    assert_eq!(
        start_response["result"]["application"]["mux_actions_applied"],
        1
    );
    assert_eq!(
        start_response["result"]["application"]["full_redraw_required"],
        true
    );
    let identities = editor_transaction(&service);
    let input = vec![
        0x01, b'e', 0x1b, b'[', b'2', b'0', b'0', b'~', b'p', b'a', b's', b't', b'e', 0x1b, b'[',
        b'2', b'0', b'1', b'~',
    ];
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "editor-input",
        "method": "terminal/step",
        "params": {
            "idempotency_key": "editor-input",
            "client_size": {"columns": 52, "rows": 18},
            "render": false,
            "input_bytes": input,
        }
    })
    .to_string();

    let response = service.dispatch_runtime_control_body(&request, &primary);
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert!(response.get("error").is_none(), "{response}");
    assert_eq!(response["result"]["application"]["forwarded_bytes"], 19);
    assert_eq!(response["result"]["application"]["mux_actions_applied"], 0);
    assert_eq!(
        response["result"]["application"]["agent_prompt_inputs_applied"],
        0
    );
    assert_eq!(
        service.session.authoritative_size,
        Size::new(52, 18).unwrap()
    );
    assert!(service.external_editor_session_is_active("%1"));
    assert_eq!(editor_transaction(&service), identities);
    assert_eq!(
        service.agent_prompt_inputs_for_tests()["%1"]
            .prompt
            .buffer
            .line(),
        "unchanged prompt"
    );

    complete_prompt_editor(&mut service, &root, &identities, b"remote edit\n", 0);
    assert!(!service.external_editor_session_is_active("%1"));
    assert_eq!(
        service.agent_prompt_inputs_for_tests()["%1"]
            .prompt
            .buffer
            .line(),
        "remote edit"
    );
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert_eq!(service.presented_pane_surface("%1"), PaneSurfaceKind::Agent);
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}
