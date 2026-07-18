//! Runtime tests for actions patch execution behavior.

use super::*;

/// Verifies mutating semantic actions log a compact execution line and a
/// colored diff in normal agent mode.
///
/// File-change actions run through generated pane shell transactions so they
/// affect the same local, remote, or container shell that the user is operating.
/// Normal mode should still show the resulting change as a readable diff
/// instead of Mezzanine's execution machinery.
#[test]
fn runtime_semantic_mutation_logs_colored_diff_in_normal_mode() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(160, 60).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let target_rel = format!(
        "target/mez-semantic-mutation-diff-{}-{unique}/note.txt",
        std::process::id()
    );
    let target = PathBuf::from(&target_rel);
    fs::create_dir_all(target.parent().unwrap()).unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-semantic-diff","input":"create a note"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap semantic response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "patch-1".to_string(),
                    rationale: "create a file".to_string(),
                    payload: mez_agent::AgentActionPayload::ApplyPatch {
                        patch: format!(
                            "*** Begin Patch\n*** Add File: {target_rel}\n+alpha\n+beta\n*** End Patch"
                        ),
                        strip: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service.remove_pending_agent_provider_task("turn-1");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    let action_transaction = service
        .running_shell_transactions_for_tests()
        .values()
        .find(|transaction| {
            matches!(
                transaction.kind,
                RunningShellTransactionKind::AgentAction { ref action_id }
                    if action_id == "patch-1"
            )
        })
        .expect("apply_patch should dispatch through the pane shell");
    let timeout_ms = action_transaction.timeout_ms.unwrap();
    assert_eq!(timeout_ms, 30 * 1000);
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get("%1").unwrap();
    assert_eq!(pane_context.agent_status.as_deref(), Some("executing"));
    assert!(
        pane_context
            .agent_display_lines
            .iter()
            .any(|line| line.starts_with("executing (")),
        "{pane_context:?}"
    );
    let marker = service
        .running_shell_transactions_for_tests()
        .keys()
        .next()
        .cloned()
        .expect("apply_patch transaction should be running");
    let transaction = service
        .running_shell_transactions_mut_for_tests()
        .get_mut(&marker)
        .unwrap();
    transaction.command = "# __MEZ_APPLY_PATCH_WRITE_PHASE__".to_string();
    transaction.observed_output_preview = format!(
        "diff -- apply patch\n--- /dev/null\n+++ b/{target_rel}\n@@ -0,0 +1,2 @@\n+alpha\n+beta\n"
    );
    transaction.observed_output_bytes = transaction.observed_output_preview.len();
    service
        .observe_agent_shell_transaction_start("%1", &marker, "turn-1", "agent-%1", "%1")
        .unwrap();
    service
        .observe_agent_shell_transaction_end("%1", &marker, "turn-1", "agent-%1", "%1", 0)
        .unwrap();

    let context = service
        .agent_turn_contexts()
        .get("turn-1")
        .unwrap()
        .blocks
        .iter()
        .find(|block| {
            block.source == ContextSourceKind::ActionResult
                && block.content.contains("diff -- apply patch")
        })
        .map(|block| block.content.clone())
        .expect("apply_patch action result context should be recorded");
    assert!(context.contains("command: apply_patch"), "{context}");
    assert!(!context.contains("command: cat >"), "{context}");
    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let pane_text = styled_lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let collapsed_agent_wraps = pane_text.replace("\n▐ ", "");
    assert!(
        pane_text.contains("agent: apply patch: ") && collapsed_agent_wraps.contains(&target_rel),
        "{pane_text}"
    );
    assert_eq!(
        styled_lines
            .iter()
            .filter(|line| line.text.contains("agent: apply patch"))
            .count(),
        1,
        "{pane_text}"
    );
    assert!(collapsed_agent_wraps.contains("note.txt"), "{pane_text}");
    assert!(pane_text.contains("--- /dev/null"), "{pane_text}");
    assert!(
        pane_text.contains("+++ ") && pane_text.contains("note.txt"),
        "{pane_text}"
    );
    assert!(pane_text.contains("@@ -0,0 +1,2 @@"), "{pane_text}");
    assert!(pane_text.contains("            1 +alpha"), "{pane_text}");
    assert!(!pane_text.contains("$ python3 - <<'MEZ_PY'"), "{pane_text}");
    assert!(!pane_text.contains("MEZ_MARKER_TOKEN"), "{pane_text}");
    assert!(!pane_text.contains("MEZ_COMMAND_"), "{pane_text}");
    assert!(
        !pane_text.contains("MEZ_RESTORE_NOUNSET_NOW"),
        "{pane_text}"
    );
    assert!(!pane_text.contains(""), "{pane_text}");
    assert!(!pane_text.contains("∙"), "{pane_text}");
    let action_line = styled_lines
        .iter()
        .find(|line| line.text.contains("agent: apply patch"))
        .unwrap();
    assert!(!action_line.style_spans.is_empty());
    let addition_line = styled_lines
        .iter()
        .find(|line| line.text.contains("            1 +alpha"))
        .unwrap();
    assert!(
        addition_line
            .style_spans
            .iter()
            .any(|span| span.rendition.bold),
        "{addition_line:?}"
    );
    fs::remove_dir_all(target.parent().unwrap()).unwrap();
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(target.parent().unwrap());
}

/// Verifies truncated `apply_patch` read snapshots surface a transport
/// diagnostic instead of falling through to snapshot parsing.
///
/// The read phase carries base64-framed file snapshots that Rust must parse
/// before it can generate the write phase. If the retained PTY observation is
/// truncated or transport-incomplete, parsing the partial payload produces a
/// misleading "missing snapshot marker" error. The runtime should dispatch a
/// model-visible write-phase failure that names the capture boundary directly.
#[test]
fn runtime_apply_patch_read_phase_truncation_dispatches_specific_error_plan() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(160, 60).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-apply-patch-truncated-read","input":"create a note"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap semantic response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "patch-1".to_string(),
                    rationale: "create a file".to_string(),
                    payload: mez_agent::AgentActionPayload::ApplyPatch {
                        patch: "*** Begin Patch\n*** Add File: target/truncated-read-note.txt\n+alpha\n*** End Patch"
                            .to_string(),
                        strip: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service.remove_pending_agent_provider_task("turn-1");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    let marker = service
        .running_shell_transactions_for_tests()
        .keys()
        .next()
        .cloned()
        .expect("apply_patch read transaction should be running");
    let transaction = service
        .running_shell_transactions_mut_for_tests()
        .get_mut(&marker)
        .unwrap();
    transaction.observed_output_preview = "partial apply_patch read transport".to_string();
    transaction.observed_output_bytes = transaction.observed_output_preview.len();
    transaction.observed_output_truncated = true;
    service
        .observe_agent_shell_transaction_start("%1", &marker, "turn-1", "agent-%1", "%1")
        .unwrap();
    service
        .observe_agent_shell_transaction_end("%1", &marker, "turn-1", "agent-%1", "%1", 0)
        .unwrap();

    let write_transaction = service
        .running_shell_transactions_for_tests()
        .values()
        .find(|transaction| {
            matches!(
                transaction.kind,
                RunningShellTransactionKind::AgentAction { ref action_id }
                    if action_id == "patch-1"
            )
        })
        .expect("truncated read should dispatch an apply_patch error write phase");
    assert!(
        write_transaction.command.contains(
            "apply_patch read phase output was truncated or transport-incomplete before Rust could build the write phase"
        ),
        "{}",
        write_transaction.command
    );
    assert!(
        !write_transaction
            .command
            .contains("read phase did not emit a snapshot"),
        "{}",
        write_transaction.command
    );
    let write_marker = service
        .running_shell_transactions_for_tests()
        .keys()
        .find(|candidate| *candidate != &marker)
        .cloned()
        .expect("write-phase error transaction should have a marker");
    service
        .observe_agent_shell_transaction_start("%1", &write_marker, "turn-1", "agent-%1", "%1")
        .unwrap();
    service
        .observe_agent_shell_transaction_end("%1", &write_marker, "turn-1", "agent-%1", "%1", 1)
        .unwrap();
    assert!(!service.agent_turn_executions().contains_key("turn-1"));
    let context = service.agent_turn_contexts().get("turn-1").unwrap();
    assert!(context.blocks.iter().all(|block| {
        block.source != ContextSourceKind::RuntimeHint || block.label != "action failure feedback"
    }));
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a pane-shell `apply_patch` action does not switch execution mode after dispatch.
///
/// Shell-backed `apply_patch` runs as a read transaction followed by a verified
/// Verifies full internal `apply_patch` read transport survives preview truncation.
///
/// The shell-backed read phase feeds a bounded pane preview for display and a
/// full internal transport buffer for Rust write planning. Large read outputs
/// may truncate the preview, but a complete internal snapshot should still let
/// the runtime build the verified write phase instead of surfacing the preview
/// truncation diagnostic.
#[test]
fn runtime_apply_patch_uses_full_read_transport_when_preview_truncates() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(160, 60).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-apply-patch-retained-read","input":"create a note"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap semantic response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "patch-1".to_string(),
                    rationale: "create a file".to_string(),
                    payload: mez_agent::AgentActionPayload::ApplyPatch {
                        patch: "*** Begin Patch\n*** Add File: target/truncated-read-note.txt\n+alpha\n*** End Patch"
                            .to_string(),
                        strip: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service.remove_pending_agent_provider_task("turn-1");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    let marker = service
        .running_shell_transactions_for_tests()
        .keys()
        .next()
        .cloned()
        .expect("apply_patch read transaction should be running");
    let snapshot = concat!(
        "__MEZ_SHELL_OUTPUT_BASE64_BEGIN__\n",
        "X19NRVpfQVBQTFlfUEFUQ0hfUkVBRF9CRUdJTl9fCl9fTUVaX0FQUExZX1BBVENIX0ZJTEVfQkVHSU5fXwpQQVRIX0I2\n",
        "NCBkR0Z5WjJWMEwzUnlkVzVqWVhSbFpDMXlaV0ZrTFc1dmRHVXVkSGgwClJFU09MVkVEX0I2NCBMMmh2YldVdmJtVnBi\n",
        "QzlFYjJOMWJXVnVkSE12Y21Wd2IzTXZiV1Y2ZW1GdWFXNWxMM1JoY21kbGRDOTBjblZ1WTJGMFpXUXRjbVZoWkMxdWIz\n",
        "UmxMblI0ZEE9PQpTVEFUVVMgbWlzc2luZwpfX01FWl9BUFBMWV9QQVRDSF9GSUxFX0VORF9fCl9fTUVaX0FQUExZX1BB\n",
        "VENIX1JFQURfRU5EX18K\n",
        "__MEZ_SHELL_OUTPUT_BASE64_END__\n",
    );
    let transaction = service
        .running_shell_transactions_mut_for_tests()
        .get_mut(&marker)
        .unwrap();
    transaction.observed_output_preview = "partial apply_patch read transport".to_string();
    transaction.observed_output_bytes = snapshot.len();
    transaction.observed_output_truncated = true;
    let state_key = RuntimeSessionService::apply_patch_batch_state_key("turn-1", "patch-1");
    service.append_apply_patch_batch_transport(&state_key, snapshot.as_bytes());
    service
        .observe_agent_shell_transaction_start("%1", &marker, "turn-1", "agent-%1", "%1")
        .unwrap();
    service
        .observe_agent_shell_transaction_end("%1", &marker, "turn-1", "agent-%1", "%1", 0)
        .unwrap();

    let write_transaction = service
        .running_shell_transactions_for_tests()
        .values()
        .find(|transaction| {
            matches!(
                transaction.kind,
                RunningShellTransactionKind::AgentAction { ref action_id }
                    if action_id == "patch-1"
            )
        })
        .expect("complete internal read transport should dispatch apply_patch write phase");
    assert!(
        write_transaction
            .command
            .contains("__MEZ_APPLY_PATCH_WRITE_PHASE__"),
        "{}",
        write_transaction.command
    );
    assert!(
        !write_transaction.command.contains(
            "apply_patch read phase output was truncated or transport-incomplete before Rust could build the write phase"
        ),
        "{}",
        write_transaction.command
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies `/loop` schedules another work iteration after a completed turn
/// that emitted an `apply_patch` action.
///
/// The spec keys loop continuation to emitted patch actions, not to the final
/// settled action-result type. Multi-phase `apply_patch` work leaves the turn
/// running until shell settlement, so this regression confirms the controller
/// still observes the original semantic patch action and queues iteration two.
#[test]
fn runtime_agent_loop_continues_after_apply_patch_iteration() {
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-loop-apply-patch"));
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    let old_session = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: old_session.clone(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::User,
            turn_id: "parent-turn".to_string(),
            agent_id: "agent".to_string(),
            pane_id: "%1".to_string(),
            content: "create a note".to_string(),
        })
        .unwrap();
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let target_rel = format!(
        "target/mez-loop-apply-patch-{}-{unique}/note.txt",
        std::process::id()
    );
    let target = PathBuf::from(&target_rel);
    fs::create_dir_all(target.parent().unwrap()).unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-loop","method":"agent/shell/command","params":{"idempotency_key":"agent-loop","input":"/loop create a note"}}"#,
        &primary,
    );
    assert!(start.contains(r#""kind":"mutated""#), "{start}");
    assert!(start.contains("state=running"), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap semantic response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "patch-1".to_string(),
                    rationale: "create a file".to_string(),
                    payload: mez_agent::AgentActionPayload::ApplyPatch {
                        patch: format!(
                            "*** Begin Patch\n*** Add File: {target_rel}\n+alpha\n*** End Patch"
                        ),
                        strip: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service.remove_pending_agent_provider_task("turn-1");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    for _ in 0..300 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        if service.running_shell_transactions_for_tests().is_empty() {
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(service.running_shell_transactions_for_tests().is_empty());
    let completion_provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "done".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(runtime_complete_batch("turn-1")),
            provider_transcript_events: Vec::new(),
        },
    };
    let completions = service
        .poll_agent_provider_tasks_with_provider(&completion_provider, 1)
        .unwrap();
    assert_eq!(completions.len(), 1);
    assert_eq!(completions[0].terminal_state, AgentTurnState::Completed);
    assert_eq!(service.agent_loop_state("%1").unwrap().iteration, 2);
    assert_eq!(service.agent_loop_turn("turn-2").unwrap().iteration, 2);
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == "turn-2")
            .map(|turn| turn.state),
        Some(AgentTurnState::Running)
    );
    fs::remove_dir_all(target.parent().unwrap()).unwrap();
}

/// Verifies display-only `say` actions can show raw Mezzanine patch examples.
///
/// When a user asks to see a patch, the patch text is ordinary assistant
/// output and must not be parsed as markdown structure, executed as a semantic
/// mutation, or collapsed into a no-output placeholder.
#[test]
fn runtime_agent_markdown_say_displays_raw_mez_patch_examples() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(96, 24).unwrap(), 120)
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(96, 24).unwrap(), 120).unwrap(),
    );
    let patch = "*** Begin Patch\n*** Update File: docs/example.md\n@@\n-old\n+new\n*** End Patch";

    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            patch,
            mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
        )
        .unwrap();

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("mez> *** Begin Patch"), "{pane_text}");
    assert!(
        pane_text.contains("     *** Update File: docs/example.md"),
        "{pane_text}"
    );
    assert!(pane_text.contains("     +new"), "{pane_text}");
    assert!(!pane_text.contains("[mez: no output]"), "{pane_text}");
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies native local execution bypasses pane readiness before starting a
/// host-side child process.
///
/// Native mode does not send input to the pane shell, so alternate-screen TUIs
/// and other non-ready pane states must not block model-authored local actions,
/// Verifies pre-execution `apply_patch` transport failures are model
/// correctable.
///
/// A pane input write timeout means the runtime could not deliver the generated
/// write command, not that the user request is impossible. The model should
/// receive bounded correction feedback so it can retry with a smaller or
/// different file action instead of failing through immediately.
#[test]
fn runtime_apply_patch_pane_input_failure_queues_model_self_correction() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "write the file")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.remove_pending_agent_provider_task(&turn.turn_id);

    let action = mez_agent::AgentAction {
        id: "patch-transport".to_string(),
        rationale: "write a source file".to_string(),
        payload: mez_agent::AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** Add File: src/generated.rs\n+content\n*** End Patch"
                .to_string(),
            strip: None,
        },
    };
    let failed = mez_agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::Failed,
        "pane_input_write_failed",
        "pane input write failed while sending shell action",
    )
    .unwrap();
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "write transport failure".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![action],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![failed],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };

    let queued = service
        .queue_agent_failure_feedback_for_correction(
            &turn,
            &mut execution,
            "apply_patch_transport_failed",
        )
        .unwrap();

    assert!(queued);
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == turn.turn_id)
    );
    assert_eq!(
        service
            .agent_failure_feedback_attempts_for_tests()
            .values()
            .copied()
            .collect::<Vec<_>>(),
        vec![1]
    );
    let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result patch-transport apply_patch failed]")
            && block.content.contains("pane_input_write_failed")
    }));
    assert!(context.blocks.iter().all(|block| {
        block.source != ContextSourceKind::RuntimeHint || block.label != "action failure feedback"
    }));
    service.terminate_all_pane_processes().unwrap();
}
