//! External editor integration for pane-local agent prompts.

use super::*;
use crate::runtime::{PaneSurfaceKind, RenderInvalidationReason};

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

/// Verifies an active prompt editor replaces the complete attached terminal,
/// uses the initiating client's full geometry, and excludes every Mez-owned
/// frame and prompt surface until the lease settles.
#[test]
fn runtime_prompt_editor_takes_over_complete_terminal_projection() {
    let (mut service, primary, root) = prompt_editor_fixture("prompt-editor-terminal-takeover");
    service
        .apply_attached_agent_prompt_input_for_pane(&primary, "%1", b"mez prompt must be hidden")
        .unwrap();
    let _identities = start_prompt_editor(&mut service, &primary);
    let takeover_size = Size::new(40, 16).unwrap();
    service
        .process_pane_screen_mut("%1")
        .unwrap()
        .feed(b"\x1b[2J\x1b[HEDITOR FULL SCREEN");

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    assert!(config.external_editor_takeover_active);
    assert_eq!(
        service
            .tracked_pane_descriptors()
            .into_iter()
            .find(|descriptor| descriptor.pane_id.as_str() == "%1")
            .unwrap()
            .size,
        takeover_size
    );
    assert_eq!(
        service.process_pane_screen("%1").unwrap().size(),
        takeover_size
    );

    let view = service
        .render_client_view_for_client_with_resolved_config(
            &primary,
            ClientViewRole::Primary,
            takeover_size,
            &config,
        )
        .unwrap()
        .unwrap();
    assert_eq!(view.authoritative_size, takeover_size);
    assert_eq!(view.client_size, takeover_size);
    assert_eq!(view.lines.len(), usize::from(takeover_size.rows));
    assert!(
        view.lines[0].contains("EDITOR FULL SCREEN"),
        "{:?}",
        view.lines
    );
    assert!(view.agent_prompt_region.is_none());
    assert!(!view.primary_prompt_active);
    assert!(view.lines.iter().all(|line| !line.contains("mez prompt")));

    service
        .show_primary_error_overlay(vec!["error: stale Mez overlay".to_string()])
        .unwrap();
    let (_, transition) = service
        .apply_attached_terminal_step_transition(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"editor input".to_vec(),
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    let input_effects = pane_input_effects(&transition.side_effects);
    assert_eq!(input_effects.len(), 1);
    assert_eq!(
        input_effects[0].pane_input_parts(),
        ("%1", b"editor input".as_slice(), false)
    );
    assert!(service.primary_error_status_overlay().is_some());

    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
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

/// Verifies native-mode prompt editing can launch from an unmanaged Bash pane.
///
/// Native mode deliberately leaves the user's Bash process without Mezzanine's
/// private receiver. The editor may use the ordinary POSIX-compatible wrapper
/// only after readiness and draft tracking prove that generated input cannot be
/// appended to a hidden user command.
#[test]
fn runtime_prompt_editor_native_bash_does_not_require_private_receiver() {
    let Some(bash_path) = bash_path_for_tests() else {
        eprintln!("skipping native Bash editor regression because bash is unavailable");
        return;
    };
    let root = temp_root("prompt-editor-native-bash");
    let socket_path = root.join("runtime/default.sock");
    let mut service = RuntimeServiceFixture::new()
        .control_socket(&socket_path)
        .build();
    service.session.shell = ResolvedShell::new(bash_path, ShellSource::ShellEnv).into();
    let primary = service
        .attach_primary("primary", true, Size::new(40, 16).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "native-bash-prompt-editor-test".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\nshell_mode = \"native\"\n[external_editor]\ncommand = [\"/bin/sh\", \"-c\", \"exit 0\", \"{file}\"]\nfallback = []\n"
                .to_string(),
        }])
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();

    let identities = start_prompt_editor(&mut service, &primary);

    assert!(service.external_editor_session_is_active("%1"));
    assert!(service.bash_receiver_token_for_pane("%1").is_none());
    complete_prompt_editor(&mut service, &root, &identities, b"edited in bash\n", 0);
    assert_eq!(
        service.agent_prompt_inputs_for_tests()["%1"]
            .prompt
            .buffer
            .line(),
        "edited in bash"
    );
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies a prompt-candidate native Bash pane can run the editor-owned
/// readiness probe without a private receiver and then launch the editor.
#[test]
fn runtime_prompt_editor_native_bash_resumes_after_readiness_probe() {
    let Some(bash_path) = bash_path_for_tests() else {
        eprintln!("skipping native Bash editor readiness regression because bash is unavailable");
        return;
    };
    let root = temp_root("prompt-editor-native-bash-readiness");
    let socket_path = root.join("runtime/default.sock");
    let mut service = RuntimeServiceFixture::new()
        .control_socket(&socket_path)
        .build();
    service.session.shell = ResolvedShell::new(bash_path, ShellSource::ShellEnv).into();
    let primary = service
        .attach_primary("primary", true, Size::new(40, 16).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service.set_pane_readiness("%1", PaneReadinessState::PromptCandidate);
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "native-bash-prompt-editor-readiness-test".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\nshell_mode = \"native\"\n[external_editor]\ncommand = [\"/bin/sh\", \"-c\", \"exit 0\", \"{file}\"]\nfallback = []\n"
                .to_string(),
        }])
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();

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
    assert_eq!(report.mux_actions_applied, 1);
    let (marker, turn_id) = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| {
            matches!(
                transaction.kind,
                RunningShellTransactionKind::AgentPromptEditorReadinessProbe { .. }
            )
            .then(|| (marker.clone(), transaction.turn_id.clone()))
        })
        .expect("native Bash prompt candidate should dispatch a readiness probe");

    service
        .observe_agent_shell_transaction_start("%1", &marker, &turn_id, "mez-ui", "%1")
        .unwrap();
    service
        .observe_agent_shell_transaction_end("%1", &marker, &turn_id, "mez-ui", "%1", 0)
        .unwrap();

    assert!(service.external_editor_session_is_active("%1"));
    assert!(service.bash_receiver_token_for_pane("%1").is_none());
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies a prompt-candidate pane is certified before the external editor
/// wrapper is injected and that the original binding resumes automatically.
#[test]
fn runtime_prompt_editor_binding_resumes_after_prompt_candidate_probe() {
    let (mut service, primary, root) = prompt_editor_fixture("prompt-editor-readiness");
    service.set_pane_readiness("%1", PaneReadinessState::PromptCandidate);

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

    assert_eq!(report.mux_actions_applied, 1);
    assert!(!service.external_editor_session_is_active("%1"));
    let (marker, turn_id) = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| {
            matches!(
                transaction.kind,
                RunningShellTransactionKind::AgentPromptEditorReadinessProbe { .. }
            )
            .then(|| (marker.clone(), transaction.turn_id.clone()))
        })
        .expect("prompt-candidate editor request should dispatch a readiness probe");

    service
        .observe_agent_shell_transaction_start("%1", &marker, &turn_id, "mez-ui", "%1")
        .unwrap();
    service
        .observe_agent_shell_transaction_end("%1", &marker, &turn_id, "mez-ui", "%1", 0)
        .unwrap();

    assert_eq!(service.pane_readiness_state("%1"), PaneReadinessState::Busy);
    assert!(service.external_editor_session_is_active("%1"));
    assert!(
        service
            .running_shell_transactions_for_tests()
            .values()
            .any(|transaction| matches!(
                transaction.kind,
                RunningShellTransactionKind::ExternalEditor { .. }
            ))
    );
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

/// Verifies a completion-time recovery persistence failure cannot consume the
/// editor lease while leaving pane readiness and prompt ownership wedged.
///
/// The interrupted-safe manifest must remain discoverable, the original prompt
/// must be restored, and a subsequent edit must be able to start immediately.
#[test]
fn runtime_prompt_editor_recovery_write_failure_restores_usable_pane_state() {
    let (mut service, primary, root) =
        prompt_editor_fixture("prompt-editor-completion-recovery-write-failure");
    service
        .apply_attached_agent_prompt_input_for_pane(&primary, "%1", b"original prompt")
        .unwrap();
    let expected = service.agent_prompt_inputs_for_tests()["%1"].clone();
    let identities = start_prompt_editor(&mut service, &primary);
    let (marker, turn_id, session_id, _) = &identities;
    fs::write(
        root.join("runtime/editor-sessions")
            .join(session_id)
            .join("draft.md"),
        b"changed but not durably settled",
    )
    .unwrap();
    service
        .observe_agent_shell_transaction_start("%1", marker, turn_id, "mez-ui", "%1")
        .unwrap();
    service.fail_next_external_editor_completion_recovery_write_for_tests();

    let error = service
        .observe_agent_shell_transaction_end("%1", marker, turn_id, "mez-ui", "%1", 0)
        .expect_err("injected completion recovery write should fail");

    assert!(error.message().contains("injected external-editor"));
    assert!(!service.external_editor_session_is_active("%1"));
    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::Ready
    );
    assert_eq!(service.agent_prompt_inputs_for_tests()["%1"], expected);
    assert_eq!(service.presented_pane_surface("%1"), PaneSurfaceKind::Agent);
    let recoveries = service.list_external_editor_recoveries(&primary).unwrap();
    assert!(recoveries.contains(session_id), "{recoveries}");
    assert!(recoveries.contains("interrupted"), "{recoveries}");

    let restarted = start_prompt_editor(&mut service, &primary);
    assert_ne!(restarted.2, *session_id);
    assert!(service.external_editor_session_is_active("%1"));
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

/// Verifies a runner-manifest construction failure occurs before private
/// recovery artifacts are materialized, avoiding a false interrupted edit.
#[test]
fn runtime_prompt_editor_manifest_failure_leaves_no_recovery_artifacts() {
    let (mut service, primary, root) = prompt_editor_fixture("prompt-editor-manifest-failure");
    let oversized_argument = "x".repeat(300 * 1024);
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "oversized-editor-manifest".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                "[agents]\nshell_mode = \"pane\"\n[external_editor]\ncommand = [\"/bin/sh\", \"{oversized_argument}\", \"{{file}}\"]\nfallback = []\n"
            ),
        }])
        .unwrap();

    let error = service
        .start_external_editor_session(
            &primary,
            "%1",
            crate::runtime::ExternalEditTarget::AgentPrompt,
            String::new(),
            String::new(),
            true,
        )
        .expect_err("oversized runner manifest should be rejected");

    assert!(
        error.message().contains("runner manifest exceeds"),
        "{error:?}"
    );
    let sessions = root.join("runtime/editor-sessions");
    assert!(
        !sessions.exists() || fs::read_dir(&sessions).unwrap().next().is_none(),
        "pre-lease failure left recovery artifacts under {}",
        sessions.display()
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
    assert!(service.abort_external_editor_session("%1").unwrap());
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

/// Verifies an explicit editor abort restores the captured prompt and terminal
/// geometry while retaining the private draft as interrupted recovery.
///
/// Abort settlement must also request a full redraw and remain idempotent so
/// timeout, write-failure, detach, and teardown callers share one safe path.
#[test]
fn runtime_prompt_editor_abort_settles_prompt_geometry_and_redraw() {
    let (mut service, primary, root) = prompt_editor_fixture("prompt-editor-abort-settlement");
    service
        .apply_attached_agent_prompt_input_for_pane(&primary, "%1", b"original prompt")
        .unwrap();
    let expected = service.agent_prompt_inputs_for_tests()["%1"].clone();
    let identities = start_prompt_editor(&mut service, &primary);
    let session_id = identities.2.clone();
    fs::write(
        root.join("runtime/editor-sessions")
            .join(&session_id)
            .join("draft.md"),
        b"interrupted draft",
    )
    .unwrap();
    service
        .resize_attached_primary_terminal(&primary, Size::new(52, 18).unwrap())
        .unwrap();

    assert!(service.abort_external_editor_session("%1").unwrap());

    assert!(!service.external_editor_session_is_active("%1"));
    assert_eq!(service.agent_prompt_inputs_for_tests()["%1"], expected);
    assert_eq!(service.presented_pane_surface("%1"), PaneSurfaceKind::Agent);
    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::PromptCandidate
    );
    assert_eq!(
        service
            .tracked_pane_descriptors()
            .into_iter()
            .find(|descriptor| descriptor.pane_id.as_str() == "%1")
            .unwrap()
            .size,
        service
            .pane_process_size_for(service.session().active_window().unwrap(), "%1")
            .unwrap()
    );
    let effects = service.drain_deferred_effects_transition().side_effects;
    assert!(effects.iter().any(|effect| matches!(
        effect,
        RuntimeSideEffect::RenderClient {
            reason: RenderInvalidationReason::FullRedraw,
            ..
        }
    )));
    let recoveries = service.list_external_editor_recoveries(&primary).unwrap();
    assert!(recoveries.contains(&session_id), "{recoveries}");
    assert!(recoveries.contains("interrupted"), "{recoveries}");
    assert!(!service.abort_external_editor_session("%1").unwrap());

    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies detaching an editor's initiating primary aborts obsolete client
/// ownership while preserving an interrupted draft for a replacement primary.
///
/// Reattach may occur before the old editor process reports its exit. That late
/// completion must remain identity-fenced and cannot apply or discard recovery.
#[test]
fn runtime_prompt_editor_detach_preserves_recovery_for_replacement_primary() {
    let (mut service, primary, root) = prompt_editor_fixture("prompt-editor-detach-recovery");
    service
        .apply_attached_agent_prompt_input_for_pane(&primary, "%1", b"original prompt")
        .unwrap();
    let identities = start_prompt_editor(&mut service, &primary);
    let (marker, turn_id, session_id, _) = &identities;
    fs::write(
        root.join("runtime/editor-sessions")
            .join(session_id)
            .join("draft.md"),
        b"draft retained across detach",
    )
    .unwrap();

    service
        .detach_primary(&primary, Size::new(40, 16).unwrap())
        .unwrap();

    assert!(!service.external_editor_session_is_active("%1"));
    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::PromptCandidate
    );
    let replacement = service
        .attach_primary("replacement", true, Size::new(40, 16).unwrap(), 122)
        .unwrap();
    let before_exit = service
        .list_external_editor_recoveries(&replacement)
        .unwrap();
    assert!(before_exit.contains(session_id), "{before_exit}");
    assert!(before_exit.contains("interrupted"), "{before_exit}");

    service
        .observe_agent_shell_transaction_start("%1", marker, turn_id, "mez-ui", "%1")
        .unwrap();
    assert_eq!(
        service
            .observe_agent_shell_transaction_end("%1", marker, turn_id, "mez-ui", "%1", 0)
            .unwrap(),
        0
    );
    let after_exit = service
        .list_external_editor_recoveries(&replacement)
        .unwrap();
    assert!(after_exit.contains(session_id), "{after_exit}");
    assert!(after_exit.contains("interrupted"), "{after_exit}");

    service
        .apply_external_editor_recovery(&replacement, "%1", session_id)
        .unwrap();
    assert_eq!(
        service.agent_prompt_inputs_for_tests()["%1"]
            .prompt
            .buffer
            .line(),
        "draft retained across detach"
    );
    service.terminate_all_pane_processes().unwrap();
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

/// Verifies local raw input resolves the sending primary's pane before
/// inspecting an editor lease owned by another primary on a different pane.
#[test]
fn runtime_external_editor_local_input_uses_sending_client_pane() {
    let (mut service, editor_primary, root) =
        prompt_editor_fixture("prompt-editor-multi-primary-local-input");
    let sending_primary = service
        .attach_primary("sending-primary", true, Size::new(40, 16).unwrap(), 121)
        .unwrap();
    assert!(
        service
            .apply_attached_mux_action(&editor_primary, MuxAction::SplitPaneVertical)
            .unwrap()
    );
    service.agent_shell_store_mut().request_exit("%1").unwrap();
    mark_test_pane_ready(&mut service, "%2");
    service
        .start_external_editor_session(
            &editor_primary,
            "%2",
            crate::runtime::ExternalEditTarget::AgentPrompt,
            String::new(),
            String::new(),
            true,
        )
        .unwrap();
    assert_eq!(
        service
            .session
            .active_pane_for(&sending_primary)
            .unwrap()
            .id
            .as_str(),
        "%1"
    );
    assert_eq!(service.active_pane_id().unwrap(), "%2");

    let transition = service
        .apply_client_input_transition(&sending_primary, b"local-client-input")
        .unwrap();

    assert!(transition.applied);
    assert_eq!(transition.side_effects.len(), 1);
    assert!(matches!(
        &transition.side_effects[0],
        RuntimeSideEffect::WritePaneInput { pane_id, bytes }
            if pane_id == "%1" && bytes == b"local-client-input"
    ));
    assert!(service.external_editor_session_is_active("%2"));
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies control `terminal/step` input resolves the sending primary's pane
/// before inspecting an editor lease owned by another primary.
#[test]
fn runtime_external_editor_control_input_uses_sending_client_pane() {
    let (mut service, editor_primary, root) =
        prompt_editor_fixture("prompt-editor-multi-primary-control-input");
    let sending_primary = service
        .attach_primary("sending-primary", true, Size::new(40, 16).unwrap(), 121)
        .unwrap();
    assert!(
        service
            .apply_attached_mux_action(&editor_primary, MuxAction::SplitPaneVertical)
            .unwrap()
    );
    service.agent_shell_store_mut().request_exit("%1").unwrap();
    mark_test_pane_ready(&mut service, "%2");
    service
        .start_external_editor_session(
            &editor_primary,
            "%2",
            crate::runtime::ExternalEditTarget::AgentPrompt,
            String::new(),
            String::new(),
            true,
        )
        .unwrap();
    assert_eq!(
        service
            .session
            .active_pane_for(&sending_primary)
            .unwrap()
            .id
            .as_str(),
        "%1"
    );
    assert_eq!(service.active_pane_id().unwrap(), "%2");
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "multi-primary-editor-input",
        "method": "terminal/step",
        "params": {
            "idempotency_key": "multi-primary-editor-input",
            "client_size": {"columns": 40, "rows": 16},
            "render": false,
            "input_bytes": b"control-client-input",
        }
    })
    .to_string();

    let response = service.dispatch_runtime_control_body(&request, &sending_primary);
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();

    assert!(response.get("error").is_none(), "{response}");
    assert_eq!(
        response["result"]["application"]["forwarded_bytes"],
        b"control-client-input".len()
    );
    assert_eq!(service.active_pane_id().unwrap(), "%1");
    assert!(service.external_editor_session_is_active("%2"));
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Builds the persistent stores and project identity used by durable editor tests.
fn durable_editor_stores(
    service: &mut RuntimeSessionService,
    root: &Path,
) -> (
    crate::storage::issues::IssueStore,
    crate::storage::memory::PersistentMemoryStore,
    String,
) {
    let config_root = root.join("config");
    fs::create_dir_all(&config_root).unwrap();
    service.set_config_root(config_root.clone());
    let project = crate::storage::issues::project_key_for_working_directory(
        service
            .pane_current_working_directory("%1")
            .unwrap_or_else(|| config_root.clone()),
    );
    (
        crate::storage::issues::IssueStore::under_config_root(&config_root),
        crate::storage::memory::PersistentMemoryStore::under_config_root(&config_root),
        project,
    )
}

/// Verifies issue body and notes editing exports only the selected prose field,
/// applies through the full-record CAS boundary, and preserves structured data.
#[test]
fn runtime_issue_external_editor_applies_only_selected_text_field() {
    let (mut service, primary, root) = prompt_editor_fixture("issue-editor-success");
    let (issues, _memories, project) = durable_editor_stores(&mut service, &root);
    let issue = issues
        .add_issue(
            project.clone(),
            mez_agent::issues::IssueKind::Task,
            "Structured title".to_string(),
            Some("body before".to_string()),
            Some("notes retained".to_string()),
            10,
        )
        .unwrap();

    let response = service
        .execute_agent_shell_command(&primary, &format!("/issue edit {} body", issue.id))
        .unwrap();
    assert!(response.contains("editor_started=true"), "{response}");
    let identities = editor_transaction(&service);
    assert_eq!(
        service.process_pane_screen("%1").unwrap().size(),
        Size::new(40, 16).unwrap()
    );
    assert_eq!(
        fs::read_to_string(
            root.join("runtime/editor-sessions")
                .join(&identities.2)
                .join("draft.md")
        )
        .unwrap(),
        "body before"
    );
    let _ = service.drain_deferred_effects_transition();

    complete_prompt_editor(&mut service, &root, &identities, b"body after\n", 0);
    assert_eq!(
        service.process_pane_screen("%1").unwrap().size(),
        service
            .pane_presentation_size_for(service.session().active_window().unwrap(), "%1")
            .unwrap()
    );
    assert_eq!(
        service
            .tracked_pane_descriptors()
            .into_iter()
            .find(|descriptor| descriptor.pane_id.as_str() == "%1")
            .unwrap()
            .size,
        service
            .pane_process_size_for(service.session().active_window().unwrap(), "%1")
            .unwrap()
    );
    let effects = service.drain_deferred_effects_transition().side_effects;
    assert!(effects.iter().any(|effect| matches!(
        effect,
        RuntimeSideEffect::RenderClient {
            reason: RenderInvalidationReason::FullRedraw,
            ..
        }
    )));
    let updated = issues
        .get_issue(project, issue.id)
        .unwrap()
        .expect("edited issue should remain present");
    assert_eq!(updated.body.as_deref(), Some("body after\n"));
    assert_eq!(updated.notes.as_deref(), Some("notes retained"));
    assert_eq!(updated.title, "Structured title");
    assert!(
        service
            .list_external_editor_recoveries(&primary)
            .unwrap()
            .contains("No retained")
    );
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies an issue changed after editor launch is never overwritten and its
/// private draft remains explicitly recoverable until the primary discards it.
#[test]
fn runtime_issue_external_editor_retains_stale_conflict() {
    let (mut service, primary, root) = prompt_editor_fixture("issue-editor-conflict");
    let (issues, _memories, project) = durable_editor_stores(&mut service, &root);
    let issue = issues
        .add_issue(
            project.clone(),
            mez_agent::issues::IssueKind::Task,
            "Concurrent issue".to_string(),
            None,
            Some("notes before".to_string()),
            10,
        )
        .unwrap();
    service
        .start_issue_external_edit(
            &primary,
            "%1",
            &issue.id,
            crate::storage::issues::IssueTextField::Notes,
        )
        .unwrap();
    let identities = editor_transaction(&service);
    issues
        .update_issue(
            project.clone(),
            issue.id.clone(),
            mez_agent::issues::IssueUpdate {
                priority: Some(90),
                ..mez_agent::issues::IssueUpdate::default()
            },
            10,
        )
        .unwrap();

    complete_prompt_editor(&mut service, &root, &identities, b"notes from editor", 0);
    let current = issues
        .get_issue(project, issue.id)
        .unwrap()
        .expect("conflicted issue should remain present");
    assert_eq!(current.notes.as_deref(), Some("notes before"));
    assert_eq!(current.priority, 90);
    let listing = service.list_external_editor_recoveries(&primary).unwrap();
    assert!(listing.contains(&identities.2), "{listing}");
    assert!(listing.contains("conflicted"), "{listing}");
    assert!(
        service
            .apply_external_editor_recovery(&primary, "%1", &identities.2)
            .is_err()
    );
    assert!(
        service
            .discard_external_editor_recovery(&primary, "%1", &identities.2)
            .unwrap()
    );
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies persistent-memory content editing applies through CAS without
/// changing metadata or leaking structured fields into the editor draft.
#[test]
fn runtime_memory_external_editor_applies_only_content() {
    let (mut service, primary, root) = prompt_editor_fixture("memory-editor-success");
    let (_issues, memories, _project) = durable_editor_stores(&mut service, &root);
    let record = MemoryRecord::new_with_defaults(
        "external-memory",
        mez_agent::memory::MemoryScope::Global,
        10,
        10,
        mez_agent::memory::MemorySource::User,
        75,
        "memory before",
    );
    let memory_id = record.id.clone();
    memories.upsert(record).unwrap();

    let response = service
        .execute_agent_shell_command(&primary, &format!("/memory edit {memory_id}"))
        .unwrap();
    assert!(response.contains("editor_started=true"), "{response}");
    let identities = editor_transaction(&service);
    assert_eq!(
        fs::read_to_string(
            root.join("runtime/editor-sessions")
                .join(&identities.2)
                .join("draft.md")
        )
        .unwrap(),
        "memory before"
    );
    complete_prompt_editor(&mut service, &root, &identities, b"memory after", 0);

    let updated = memories.inspect(&memory_id).unwrap();
    assert_eq!(updated.content, "memory after");
    assert_eq!(updated.priority, 75);
    assert_eq!(updated.source, mez_agent::memory::MemorySource::User);
    assert!(
        service
            .list_external_editor_recoveries(&primary)
            .unwrap()
            .contains("No retained")
    );
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies deleting a memory during editing cannot recreate it and the changed
/// draft remains primary-authorized recovery data until explicit discard.
#[test]
fn runtime_memory_external_editor_retains_deleted_conflict() {
    let (mut service, primary, root) = prompt_editor_fixture("memory-editor-deleted");
    let (_issues, memories, _project) = durable_editor_stores(&mut service, &root);
    let record = MemoryRecord::new_with_defaults(
        "deleted-memory",
        mez_agent::memory::MemoryScope::Global,
        10,
        10,
        mez_agent::memory::MemorySource::User,
        50,
        "memory before",
    );
    let memory_id = record.id.clone();
    memories.upsert(record).unwrap();
    service
        .start_memory_external_edit(&primary, "%1", &memory_id)
        .unwrap();
    let identities = editor_transaction(&service);
    assert!(memories.delete(&memory_id).unwrap());

    complete_prompt_editor(&mut service, &root, &identities, b"must not recreate", 0);
    assert!(memories.inspect(&memory_id).is_err());
    let listing = service.list_external_editor_recoveries(&primary).unwrap();
    assert!(listing.contains(&identities.2), "{listing}");
    assert!(listing.contains("conflicted"), "{listing}");
    let observer = service
        .session
        .attach_observer_with_terminal("observer", None, 1)
        .unwrap();
    assert!(
        service
            .apply_external_editor_recovery(&observer, "%1", &identities.2)
            .is_err()
    );
    assert!(
        service
            .discard_external_editor_recovery(&primary, "%1", &identities.2)
            .unwrap()
    );
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies a valid nonzero memory draft remains unapplied until the primary
/// explicitly applies the recovery through the original CAS token.
#[test]
fn runtime_memory_external_editor_recovery_applies_explicitly() {
    let (mut service, primary, root) = prompt_editor_fixture("memory-editor-recovery-apply");
    let (_issues, memories, _project) = durable_editor_stores(&mut service, &root);
    let record = MemoryRecord::new_with_defaults(
        "recover-memory",
        mez_agent::memory::MemoryScope::Global,
        10,
        10,
        mez_agent::memory::MemorySource::User,
        60,
        "memory before",
    );
    let memory_id = record.id.clone();
    memories.upsert(record).unwrap();
    service
        .start_memory_external_edit(&primary, "%1", &memory_id)
        .unwrap();
    let identities = editor_transaction(&service);

    complete_prompt_editor(&mut service, &root, &identities, b"recovered memory", 7);
    assert_eq!(
        memories.inspect(&memory_id).unwrap().content,
        "memory before"
    );
    let listing = service.list_external_editor_recoveries(&primary).unwrap();
    assert!(listing.contains(&identities.2), "{listing}");
    assert!(listing.contains("nonzero_exit"), "{listing}");

    service
        .apply_external_editor_recovery(&primary, "%1", &identities.2)
        .unwrap();
    assert_eq!(
        memories.inspect(&memory_id).unwrap().content,
        "recovered memory"
    );
    assert!(
        !root
            .join("runtime/editor-sessions")
            .join(&identities.2)
            .exists()
    );
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies reopening retained issue prose seeds a fresh editor without
/// applying on close; only a later explicit recovery apply mutates the issue.
#[test]
fn runtime_issue_external_editor_reopen_remains_explicit_apply() {
    let (mut service, primary, root) = prompt_editor_fixture("issue-editor-recovery-reopen");
    let (issues, _memories, project) = durable_editor_stores(&mut service, &root);
    let issue = issues
        .add_issue(
            project.clone(),
            mez_agent::issues::IssueKind::Task,
            "Recover issue prose".to_string(),
            Some("body before".to_string()),
            None,
            10,
        )
        .unwrap();
    service
        .start_issue_external_edit(
            &primary,
            "%1",
            &issue.id,
            crate::storage::issues::IssueTextField::Body,
        )
        .unwrap();
    let first = editor_transaction(&service);
    complete_prompt_editor(&mut service, &root, &first, b"retained issue body", 9);

    service
        .reopen_external_editor_recovery(&primary, "%1", &first.2)
        .unwrap();
    let reopened = editor_transaction(&service);
    assert_ne!(reopened.2, first.2);
    assert_eq!(
        fs::read_to_string(
            root.join("runtime/editor-sessions")
                .join(&reopened.2)
                .join("draft.md")
        )
        .unwrap(),
        "retained issue body"
    );
    complete_prompt_editor(&mut service, &root, &reopened, b"edited after reopen", 0);
    assert_eq!(
        issues
            .get_issue(project.clone(), issue.id.clone())
            .unwrap()
            .unwrap()
            .body
            .as_deref(),
        Some("body before")
    );
    let listing = service.list_external_editor_recoveries(&primary).unwrap();
    assert!(listing.contains(&reopened.2), "{listing}");
    assert!(listing.contains("changed_unapplied"), "{listing}");

    service
        .apply_external_editor_recovery(&primary, "%1", &reopened.2)
        .unwrap();
    assert_eq!(
        issues
            .get_issue(project, issue.id)
            .unwrap()
            .unwrap()
            .body
            .as_deref(),
        Some("edited after reopen")
    );
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies a context-document edit exports only source content and commits
/// through the document CAS boundary without changing inclusion metadata.
#[test]
fn runtime_context_document_external_editor_applies_content_only() {
    let (mut service, primary, root) = prompt_editor_fixture("context-document-editor-success");
    let (_issues, _memories, project) = durable_editor_stores(&mut service, &root);
    let store = crate::storage::context_documents::ContextDocumentStore::under_config_root(
        root.join("config"),
    );
    let document = store
        .create(
            crate::storage::context_documents::ContextDocumentScope::Project { root: project },
            "Editable Runbook".to_string(),
            "document before".to_string(),
            true,
            10,
        )
        .unwrap();

    let response = service
        .execute_agent_shell_command(&primary, &format!("/context-doc edit {}", document.id))
        .unwrap();
    assert!(response.contains("editor_started=true"), "{response}");
    let identities = editor_transaction(&service);
    assert_eq!(
        fs::read_to_string(
            root.join("runtime/editor-sessions")
                .join(&identities.2)
                .join("draft.md")
        )
        .unwrap(),
        "document before"
    );
    complete_prompt_editor(&mut service, &root, &identities, b"document after", 0);

    let updated = store.inspect(&document.id).unwrap().unwrap();
    assert_eq!(updated.content, "document after");
    assert_eq!(updated.title, "Editable Runbook");
    assert!(updated.enabled);
    assert!(
        service
            .list_external_editor_recoveries(&primary)
            .unwrap()
            .contains("No retained")
    );
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies a concurrent inclusion change cannot be overwritten by document
/// prose completion and leaves a primary-authorized conflicted recovery.
#[test]
fn runtime_context_document_external_editor_retains_stale_conflict() {
    let (mut service, primary, root) = prompt_editor_fixture("context-document-editor-conflict");
    let (_issues, _memories, _project) = durable_editor_stores(&mut service, &root);
    let store = crate::storage::context_documents::ContextDocumentStore::under_config_root(
        root.join("config"),
    );
    let document = store
        .create(
            crate::storage::context_documents::ContextDocumentScope::Global,
            "Concurrent Runbook".to_string(),
            "document before".to_string(),
            true,
            10,
        )
        .unwrap();
    service
        .start_context_document_external_edit(&primary, "%1", &document.id)
        .unwrap();
    let identities = editor_transaction(&service);
    store.set_enabled(&document.id, false, 10).unwrap();

    complete_prompt_editor(&mut service, &root, &identities, b"must not overwrite", 0);
    let current = store.inspect(&document.id).unwrap().unwrap();
    assert_eq!(current.content, "document before");
    assert!(!current.enabled);
    let listing = service.list_external_editor_recoveries(&primary).unwrap();
    assert!(listing.contains(&identities.2), "{listing}");
    assert!(listing.contains("conflicted"), "{listing}");
    assert!(
        service
            .apply_external_editor_recovery(&primary, "%1", &identities.2)
            .is_err()
    );
    assert!(
        service
            .discard_external_editor_recovery(&primary, "%1", &identities.2)
            .unwrap()
    );
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies typed context-document commands keep inclusion separate from prose
/// editing and do not expose disabled documents to future-turn selection.
#[test]
fn runtime_context_document_commands_manage_explicit_inclusion_lifecycle() {
    let (mut service, primary, root) = prompt_editor_fixture("context-document-commands");
    let (_issues, _memories, project) = durable_editor_stores(&mut service, &root);
    let store = crate::storage::context_documents::ContextDocumentStore::under_config_root(
        root.join("config"),
    );

    let created = service
        .execute_agent_shell_command(
            &primary,
            "/context-doc create --scope project --title 'Project Runbook'",
        )
        .unwrap();
    assert!(created.contains("enabled=false"), "{created}");
    let document = store.list().unwrap().into_iter().next().unwrap();
    assert_eq!(
        document.scope,
        crate::storage::context_documents::ContextDocumentScope::Project {
            root: project.clone()
        }
    );
    assert!(!document.enabled);
    assert!(
        store
            .select_enabled_for_project(&project)
            .unwrap()
            .documents
            .is_empty()
    );

    let revision = store.revision(&document).unwrap();
    store
        .compare_and_swap_content(
            &document.id,
            &revision,
            "project context".to_string(),
            document.updated_at_unix_seconds,
        )
        .unwrap();
    let enabled = service
        .execute_agent_shell_command(&primary, &format!("/context-doc enable {}", document.id))
        .unwrap();
    assert!(enabled.contains("enabled=true"), "{enabled}");
    assert_eq!(
        store
            .select_enabled_for_project(&project)
            .unwrap()
            .documents
            .len(),
        1
    );

    let listed = service
        .execute_agent_shell_command(&primary, "/context-doc list")
        .unwrap();
    assert!(listed.contains(&document.id), "{listed}");
    assert!(!listed.contains("project context"), "{listed}");
    let shown = service
        .execute_agent_shell_command(&primary, &format!("/context-doc show {}", document.id))
        .unwrap();
    assert!(shown.contains("project context"), "{shown}");

    let disabled = service
        .execute_agent_shell_command(&primary, &format!("/context-doc disable {}", document.id))
        .unwrap();
    assert!(disabled.contains("enabled=false"), "{disabled}");
    let deleted = service
        .execute_agent_shell_command(&primary, &format!("/context-doc delete {}", document.id))
        .unwrap();
    assert!(deleted.contains("deleted=true"), "{deleted}");
    assert!(store.inspect(&document.id).unwrap().is_none());
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies deleting a context source while its editor is open cannot recreate
/// it and keeps the changed draft behind primary-authorized recovery controls.
#[test]
fn runtime_context_document_external_editor_retains_deleted_conflict() {
    let (mut service, primary, root) = prompt_editor_fixture("context-document-editor-deleted");
    let (_issues, _memories, _project) = durable_editor_stores(&mut service, &root);
    let store = crate::storage::context_documents::ContextDocumentStore::under_config_root(
        root.join("config"),
    );
    let document = store
        .create(
            crate::storage::context_documents::ContextDocumentScope::Global,
            "Deleted Runbook".to_string(),
            "document before".to_string(),
            true,
            10,
        )
        .unwrap();
    service
        .start_context_document_external_edit(&primary, "%1", &document.id)
        .unwrap();
    let identities = editor_transaction(&service);
    assert!(store.delete(&document.id).unwrap());

    complete_prompt_editor(&mut service, &root, &identities, b"must not recreate", 0);
    assert!(store.inspect(&document.id).unwrap().is_none());
    let listing = service.list_external_editor_recoveries(&primary).unwrap();
    assert!(listing.contains(&identities.2), "{listing}");
    assert!(listing.contains("conflicted"), "{listing}");
    let observer = service
        .session
        .attach_observer_with_terminal("observer", None, 1)
        .unwrap();
    assert!(
        service
            .apply_external_editor_recovery(&observer, "%1", &identities.2)
            .is_err()
    );
    assert!(
        service
            .discard_external_editor_recovery(&primary, "%1", &identities.2)
            .unwrap()
    );
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(root);
}
