//! Runtime tests for actions shell protocol behavior.

use super::*;

/// Verifies that runtime shell transaction markers are generated with fresh
/// entropy for every dispatch. Identical turn/action metadata must not produce
/// reusable marker tokens.
#[test]
fn runtime_marker_for_action_uses_fresh_entropy() {
    let turn = mez_agent::AgentTurnRecord {
        turn_id: "turn-1".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        trigger: mez_agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 200,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        cooperation_mode: None,
        initial_capability: None,
        state: mez_agent::AgentTurnState::Running,
    };

    let first = runtime_marker_for_action(&turn, "a1").unwrap();
    let second = runtime_marker_for_action(&turn, "a1").unwrap();

    assert_ne!(first.as_str(), second.as_str());
    assert!(first.as_str().len() >= 64);
    assert!(second.as_str().len() >= 64);
}

/// Verifies that runtime shell transaction observation stores bounded terminal
/// text and reports truncation once the observation cap is exceeded.
#[test]
fn runtime_shell_transaction_observation_is_bounded_and_truncated() {
    let mut service = test_runtime_service();
    service.running_shell_transactions_mut_for_tests().insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "a1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "printf marker\n".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );
    let output = vec![b'x'; 300_000];

    service.record_running_shell_transaction_output("%1", &output);

    let transaction = service
        .running_shell_transactions_for_tests()
        .get("marker-1")
        .unwrap();
    assert_eq!(transaction.observed_output_bytes, 300_001);
    assert_eq!(transaction.observed_output_preview.len(), 262_144);
    assert!(transaction.observed_output_truncated);
}

/// Verifies async pane write completions are retained in the hidden trace log.
///
/// A shell transaction being recorded as running is not enough evidence that
/// the async pane worker actually wrote its wrapper bytes to the PTY. The trace
/// log should include write progress so file-action hangs can be diagnosed at
/// the delivery boundary instead of only at the transaction marker boundary.
#[test]
fn runtime_pane_input_written_traces_active_shell_transaction() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.running_shell_transactions_mut_for_tests().insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "create-1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "cat > note.txt".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    assert!(service.apply_pane_input_written_event("%1", 4096).unwrap());

    let trace = service.agent_pane_trace_log_text("%1").unwrap();
    assert!(trace.contains("pane input written bytes: 4096"), "{trace}");
    assert!(trace.contains("marker: marker-1"), "{trace}");
    assert!(trace.contains("action: create-1"), "{trace}");
}

/// Verifies model-visible shell transaction observation strips prompt styling
/// and Mezzanine wrapper echo while preserving command output.
///
/// Styled shell prompts can be much larger than the useful output for common
/// commands like `ls`. The agent context must contain the file names rather
/// than consuming its bounded observation budget with PS1 repaint bytes.
#[test]
fn runtime_shell_transaction_observation_strips_prompt_and_wrapper_noise() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.running_shell_transactions_mut_for_tests().insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "a1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "ls".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let filtered = service.visible_pane_output_bytes(
        "%1",
        b"\x1b[38;2;214;93;14m\xee\x82\xb6\x1b[48;2;214;93;14m\xef\xb0\x95 neil \x1b[0m\r\n\x1b[38;2;214;93;14m\xee\x82\xb6\x1b[48;2;214;93;14m\xef\xb0\x95 neil \x1b[0m MEZ_MARKER_TOKEN='abc'\r\n\x1b[38;2;214;93;14m\xee\x82\xb6\x1b[48;2;214;93;14m\xef\xb0\x95 neil \x1b[0m MEZ_TURN='turn-1'\r\n\x1b[1;38;2;152;151;26m\xef\x90\xb2\x1b[0m ls\r\nCargo.toml\r\nsrc\r\n\x1b]133;D;0;mez_marker=abc;mez_turn=turn-1;mez_agent=agent-%1;mez_pane=%1\x1b\\",
    );
    service.record_running_shell_transaction_output("%1", &filtered);

    let transaction = service
        .running_shell_transactions_for_tests()
        .get("marker-1")
        .unwrap();
    assert!(
        transaction.observed_output_preview.contains("src"),
        "{}",
        transaction.observed_output_preview
    );
    assert!(
        !transaction.observed_output_preview.contains("MEZ_"),
        "{}",
        transaction.observed_output_preview
    );
    assert!(
        !transaction.observed_output_preview.contains("neil"),
        "{}",
        transaction.observed_output_preview
    );
    assert!(transaction.observed_output_bytes > 0);
    assert!(!transaction.observed_output_truncated);
}

/// Verifies that transaction observation hides echoed Mezzanine-owned wrapper
/// lines for active shell transactions while preserving actual command output and the
/// OSC transaction markers that the runtime needs to observe completion.
#[test]
fn runtime_shell_transaction_wrapper_echo_is_hidden_by_default() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.running_shell_transactions_mut_for_tests().insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "a1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "ls".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let visible = service.visible_pane_output_bytes(
        "%1",
        b"MEZ_RESTORE_ERREXIT=0; case $- in *e*) MEZ_RESTORE_ERREXIT=1; set +e;; esac; MEZ_HISTORY_RESTORE=0; case \"$(set -o 2>/dev/null | awk '$1==\"history\"{print $2; exit}')\" in on) MEZ_HISTORY_RESTORE=1; set +o history 2>/dev/null || :; history -d $((HISTCMD-1)) 2>/dev/null || :;; esac\r\nMEZ_HISTORY_HISTFILE_WAS_SET=0\r\nHISTFILE=/dev/null\r\nMEZ_MARKER_TOKEN='abc'\r\nMEZ_TURN='turn-1'\r\nls\r\nprintf '\\033]133;D;%s;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\\033\\\\'\r\n\"$MEZ_STATUS\" \"$MEZ_MARKER_TOKEN\" \"$MEZ_TURN\" \"$MEZ_AGENT\" \"$MEZ_PANE\"\r\nif [ \"$MEZ_HISTORY_HISTFILE_WAS_SET\" = 1 ]; then HISTFILE=$MEZ_HISTORY_HISTFILE_SAVED; else unset HISTFILE; fi\r\nMEZ_RESTORE_HISTORY_NOW=$MEZ_HISTORY_RESTORE\r\nunset MEZ_MARKER_TOKEN MEZ_TURN MEZ_AGENT MEZ_PANE MEZ_STATUS\r\nif [ \"$MEZ_RESTORE_HISTORY_NOW\" = 1 ]; then set -o history 2>/dev/null || :; fi; if [ \"$MEZ_RESTORE_ERREXIT_NOW\" = 1 ]; then set -e; fi; unset MEZ_RESTORE_HISTORY_NOW MEZ_RESTORE_ERREXIT_NOW\r\n>\r\nfile-a\n\x1b]133;D;0;mez_marker=abc;mez_turn=turn-1;mez_agent=agent-%1;mez_pane=%1\x1b\\",
    );
    let visible_text = String::from_utf8_lossy(&visible);

    assert!(!visible_text.contains("MEZ_MARKER_TOKEN"), "{visible_text}");
    assert!(!visible_text.contains("MEZ_TURN"), "{visible_text}");
    assert!(!visible_text.contains("MEZ_STATUS"), "{visible_text}");
    assert!(
        !visible_text.contains("MEZ_RESTORE_ERREXIT"),
        "{visible_text}"
    );
    assert!(!visible_text.contains("MEZ_HISTORY"), "{visible_text}");
    assert!(!visible_text.contains("HISTFILE"), "{visible_text}");
    assert!(!visible_text.contains("history -d"), "{visible_text}");
    assert!(!visible_text.contains("case $-"), "{visible_text}");
    assert!(!visible_text.contains("\nls"), "{visible_text}");
    assert!(visible_text.contains("file-a"), "{visible_text}");
    assert!(visible.contains(&0x1b));
}

/// Verifies that runtime transaction marker parsing is stateful per pane rather
/// than per PTY read chunk. Real PTY reads can split the OSC 133 transaction end
/// marker across chunks; losing that fragment leaves the agent shell action in a
/// permanent running state even though the command has already exited.
#[test]
fn runtime_shell_transaction_osc_parser_preserves_fragmented_markers() {
    let mut service = test_runtime_service();
    let size = Size::new(80, 24).unwrap();

    let (first_events, _) = service
        .terminal_osc_events_for_pane_bytes(
            "%1",
            size,
            b"file-a\n\x1b]133;D;0;mez_marker=marker-1;mez_turn=turn-1;mez_agent=agent-%1;mez",
        )
        .unwrap();
    let (second_events, _) = service
        .terminal_osc_events_for_pane_bytes("%1", size, b"_pane=%1\x1b\\")
        .unwrap();

    assert_eq!(first_events, Vec::<TerminalOscEvent>::new());
    assert_eq!(
        second_events,
        vec![TerminalOscEvent::ShellTransactionEnd {
            marker: "marker-1".to_string(),
            turn_id: "turn-1".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            exit_code: 0,
        }]
    );
}

/// Verifies that terminal-wrapped fragments of Mezzanine wrapper echo are hidden
/// even when a PTY splits the original wrapper line before the filter receives a
/// newline. The visible pane must contain command output, not implementation
/// variable fragments.
#[test]
fn runtime_shell_transaction_wrapper_echo_fragments_are_hidden_by_default() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.running_shell_transactions_mut_for_tests().insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "a1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "printf 'file-a\\n'".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let visible = service.visible_pane_output_bytes(
        "%1",
        b"Z_TURN\" \"$MEZ_AGENT\" \"$MEZ_PANE\"\r\nEZ_PANE MEZ_STATUS\r\nfile-a\n",
    );
    let visible_text = String::from_utf8_lossy(&visible);

    assert!(!visible_text.contains("Z_TURN"), "{visible_text}");
    assert!(!visible_text.contains("MEZ_AGENT"), "{visible_text}");
    assert!(!visible_text.contains("MEZ_STATUS"), "{visible_text}");
    assert!(visible_text.contains("file-a"), "{visible_text}");
}

/// Verifies that `/log-level trace` is the high-verbosity escape hatch for raw
/// shell-wrapper diagnosis. When enabled, the runtime leaves echoed wrapper
/// traffic untouched so developers can inspect exactly what was written to and
/// echoed by the pane PTY.
#[test]
fn runtime_shell_transaction_wrapper_echo_is_visible_with_trace_enabled() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Trace)
        .unwrap();
    service.running_shell_transactions_mut_for_tests().insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "a1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "ls".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let visible =
        service.visible_pane_output_bytes("%1", b"MEZ_MARKER_TOKEN='abc'\r\nls\r\nfile-a\n");
    let visible_text = String::from_utf8_lossy(&visible);

    assert!(visible_text.contains("MEZ_MARKER_TOKEN"), "{visible_text}");
    assert!(visible_text.contains("ls"), "{visible_text}");
    assert!(visible_text.contains("file-a"), "{visible_text}");
}

/// Verifies that agent command output retained for transaction observation is
/// not rendered into the user pane by default. This keeps default agent turns
/// conversational while still preserving the bytes needed for command-result
/// context.
#[test]
fn runtime_agent_shell_transaction_output_is_hidden_from_pane_by_default() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.running_shell_transactions_mut_for_tests().insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "a1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "ls".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let rendered = service.renderable_pane_output_bytes("%1", b"file-a\n");

    assert!(rendered.is_empty());
}

/// Verifies that `/log-level verbose` opts the pane back into agent command
/// output without enabling raw wrapper traffic. Verbose remains the shell-view
/// level for commands and their output; trace remains reserved for wrapper
/// internals and full diagnostic payloads.
#[test]
fn runtime_agent_shell_transaction_output_is_visible_with_verbose_enabled() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Verbose)
        .unwrap();
    service.running_shell_transactions_mut_for_tests().insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "a1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "ls".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let rendered = service.renderable_pane_output_bytes("%1", b"file-a\n");

    assert_eq!(rendered, b"file-a\n");
}

/// Verifies exiting agent mode after interrupting a live shell transaction
/// closes the nested agent subshell with a line command.
///
/// Immediate EOF can be consumed by an interrupted transaction wrapper's read
/// loop, leaving the user inside the child shell after agent mode hides. After
/// a live transaction is interrupted, the runtime should queue Ctrl+C followed
/// by `exit` so the command is read by the shell after the wrapper unwinds.
#[test]
fn runtime_agent_shell_exit_after_shell_transaction_uses_command_exit() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let mut process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    service.enter_agent_subshell(pane_id.clone());
    let started = service
        .start_agent_prompt_turn(&pane_id, "search the file")
        .unwrap();
    service.running_shell_transactions_mut_for_tests().insert(
        "marker-grep".to_string(),
        RunningShellTransactionRef {
            turn_id: started.turn_id.clone(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "shell-grep".to_string(),
            },
            pane_id: pane_id.clone(),
            command: "grep -n needle file.txt".to_string(),
            started_at_unix_ms: 1_000,
            timeout_ms: Some(10 * 60 * 1000),
            pending_input_payload: Some(b"payload\n".to_vec()),
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-exit","method":"agent/shell/command","params":{"idempotency_key":"agent-exit-live-shell","input":"/exit"}}"#,
        &primary,
    );

    assert!(response.contains(r#""visibility":"hidden""#), "{response}");
    let exit_inputs = service.drain_pane_io_transition().side_effects;
    assert_eq!(exit_inputs.len(), 2);
    assert_eq!(exit_inputs[0].pane_input_parts().0, pane_id);
    assert_eq!(exit_inputs[0].pane_input_parts().1, b"\x03");
    assert_eq!(exit_inputs[1].pane_input_parts().0, pane_id);
    assert_eq!(exit_inputs[1].pane_input_parts().1, b"exit\n");
    assert!(!service.agent_subshell_is_active(&pane_id));
    assert!(!service.agent_subshell_command_exit_is_pending_for_tests(&pane_id));
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies list items keep their marker and first content words on the same
/// rendered row instead of flushing a marker-only line before the paragraph
/// text arrives. CommonMark emits `Paragraph` inside list items, so the
/// renderer must not treat the freshly written list prefix as a completed
/// block.
#[test]
fn runtime_agent_markdown_lists_keep_content_on_marker_row() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(64, 20).unwrap(), 120)
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(64, 20).unwrap(), 120).unwrap(),
    );
    let markdown = "1. first numbered item\n2. second numbered item\n\n- bullet item";

    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            markdown,
            mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
        )
        .unwrap();

    let pane_lines = service.pane_screen("%1").unwrap().normal_content_lines();
    let pane_text = pane_lines.join("\n");

    assert!(
        pane_text.contains("▐ mez> 1. first numbered item"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("▐      2. second numbered item"),
        "{pane_text}"
    );
    assert!(pane_text.contains("▐      • bullet item"), "{pane_text}");
    assert!(
        !pane_lines.iter().any(|line| line.trim_end() == "▐ mez> 1."
            || line.trim_end() == "▐      2."
            || line.trim_end() == "▐      •"),
        "{pane_text}"
    );
}

/// Verifies that a bash-backed pane shell survives the first agent shell
/// transaction after the command is displayed. The user-visible failure mode
/// was the primary pane exiting immediately after an agent command preview, so
/// this test waits through transaction settlement and repeated process polls.
#[test]
fn runtime_bash_agent_shell_transaction_keeps_parent_shell_alive() {
    let Some(bash_path) = bash_path_for_tests() else {
        eprintln!("skipping bash parent-shell regression because bash is unavailable");
        return;
    };
    let mut service = RuntimeSessionService::with_event_log(
        Session::new_default(
            ResolvedShell::new(bash_path, ShellSource::ShellEnv),
            Size::new(80, 24).unwrap(),
        ),
        PathBuf::from("/tmp/mez-1000/default.sock"),
        100,
        10,
        1024,
    )
    .unwrap();
    *service.host_clipboard_mut_for_tests() = HostClipboard::disabled();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
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
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-bash-survival","input":"run a bash command"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap shell response".to_string(),
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
                    id: "shell-1".to_string(),
                    rationale: "exercise bash shell survival".to_string(),
                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: "Run a failing bash command and keep the parent shell available"
                            .to_string(),
                        command: "printf 'agent-bash-command-ran\\n'; false".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
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
    assert!(
        service.running_shell_transactions_for_tests().is_empty(),
        "agent transaction should have completed before checking parent shell liveness"
    );
    let pane_exits = service.poll_pane_processes().unwrap();
    assert!(pane_exits.is_empty(), "{pane_exits:?}");
    assert!(service.pane_processes().contains_pane("%1"));
    for _ in 0..10 {
        let exits = service.poll_pane_processes().unwrap();
        assert!(exits.is_empty(), "{exits:?}");
        assert!(service.pane_processes().contains_pane("%1"));
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!pane_text.contains("MEZ_MARKER_TOKEN"), "{pane_text}");
    assert!(!pane_text.contains("MEZ_HISTORY_"), "{pane_text}");
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies that the bash-backed pane shell also survives an agent shell
/// transaction when strict interactive options are already enabled. Some users
/// set `errexit` and `nounset` in shell startup files, so the transaction
/// prologue must temporarily disable and later restore both without letting a
/// failed agent command close the pane or the enclosing Mez session.
#[test]
fn runtime_bash_agent_shell_transaction_preserves_strict_parent_shell_options() {
    let Some(bash_path) = bash_path_for_tests() else {
        eprintln!("skipping bash strict-option regression because bash is unavailable");
        return;
    };
    let mut service = RuntimeSessionService::with_event_log(
        Session::new_default(
            ResolvedShell::new(bash_path, ShellSource::ShellEnv),
            Size::new(80, 24).unwrap(),
        ),
        PathBuf::from("/tmp/mez-1000/default.sock"),
        100,
        10,
        1024,
    )
    .unwrap();
    *service.host_clipboard_mut_for_tests() = HostClipboard::disabled();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .write_input_to_pane(&primary, Some("%1"), b"set -eu\n")
        .unwrap();
    for _ in 0..20 {
        let _ = service.poll_pane_outputs(4096).unwrap();
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-bash-strict-survival","input":"run a bash command"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap shell response".to_string(),
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
                    id: "shell-1".to_string(),
                    rationale: "exercise bash strict shell survival".to_string(),
                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: "Run a failing bash command and keep strict shell options intact"
                            .to_string(),
                        command: "printf 'agent-bash-strict-command-ran\\n'; false".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
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
    let pane_exits = service.poll_pane_processes().unwrap();
    assert!(pane_exits.is_empty(), "{pane_exits:?}");
    assert!(service.pane_processes().contains_pane("%1"));
    if !service.pending_agent_provider_tasks().is_empty() {
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
    }

    service
        .write_input_to_pane(&primary, Some("%1"), b"case $- in *e*u*|*u*e*) printf 'STRICT_OPTIONS_STILL_SET\\n';; *) printf 'STRICT_OPTIONS_LOST:%s\\n' \"$-\";; esac\n")
        .unwrap();
    let mut pane_text = String::new();
    for _ in 0..150 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        pane_text = service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        if pane_text.contains("STRICT_OPTIONS_STILL_SET") {
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(
        pane_text.contains("STRICT_OPTIONS_STILL_SET"),
        "{pane_text}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies mismatched shell-transaction markers fail the live action promptly.
///
/// A terminal OSC marker can be malformed, delayed, or spoofed. The runtime must
/// validate marker metadata against the retained transaction state and fail the
/// action instead of leaving the turn to wait for a later timeout.
#[test]
fn runtime_shell_transaction_metadata_mismatch_fails_live_action() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-marker-mismatch","input":"run a command"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.remove_pending_agent_provider_task("turn-1");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "shell".to_string(),
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
                    id: "shell-1".to_string(),
                    rationale: "run a shell command".to_string(),
                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: "Run a command".to_string(),
                        command: "true".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    let marker = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| match &transaction.kind {
            RunningShellTransactionKind::AgentAction { action_id } if action_id == "shell-1" => {
                Some(marker.clone())
            }
            _ => None,
        })
        .unwrap();

    let observed = service
        .observe_agent_shell_transaction_end("%2", &marker, "turn-1", "agent-%1", "%1", 0)
        .unwrap();

    assert_eq!(observed, 1);
    assert!(
        !service
            .running_shell_transactions_for_tests()
            .contains_key(&marker)
    );
    assert!(!service.shell_transaction_requires_start_marker_for_tests(&marker));
    assert!(!service.shell_transaction_started_for_tests(&marker));
    assert!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .any(|turn| turn.turn_id == "turn-1" && turn.state == AgentTurnState::Failed)
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text
            .contains("shell transaction marker metadata does not match runtime dispatch state"),
        "{pane_text}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a duplicate start marker fails the live shell action.
///
/// The wrapper start marker is the handoff boundary for deferred command
/// payloads. Seeing it twice for one marker means the in-band control stream is
/// no longer well framed, so the action should fail instead of waiting for a
/// later timeout.
#[test]
fn runtime_shell_transaction_duplicate_start_marker_fails_live_action() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let (pane_id, marker) =
        dispatch_protocol_test_shell_action(&mut service, &primary, "shell-duplicate-start");

    service
        .observe_agent_shell_transaction_start(&pane_id, &marker, "turn-1", "agent-%1", &pane_id)
        .unwrap();
    assert!(service.shell_transaction_started_for_tests(&marker));
    let observed = service
        .observe_agent_shell_transaction_start(&pane_id, &marker, "turn-1", "agent-%1", &pane_id)
        .unwrap();

    assert_eq!(observed, 1);
    assert!(
        !service
            .running_shell_transactions_for_tests()
            .contains_key(&marker)
    );
    assert!(!service.shell_transaction_requires_start_marker_for_tests(&marker));
    assert!(!service.shell_transaction_started_for_tests(&marker));
    assert!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .any(|turn| turn.turn_id == "turn-1" && turn.state == AgentTurnState::Failed)
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("shell transaction emitted a duplicate start marker"),
        "{pane_text}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies an end marker before the start marker fails the live shell action.
///
/// Runtime-dispatched wrappers must emit a start marker before any end marker.
/// An end marker first means the parser missed a control boundary or command
/// output spoofed the frame, either of which should fail fast with diagnostics.
#[test]
fn runtime_shell_transaction_end_before_start_marker_fails_live_action() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let (pane_id, marker) =
        dispatch_protocol_test_shell_action(&mut service, &primary, "shell-end-before-start");

    let observed = service
        .observe_agent_shell_transaction_end(&pane_id, &marker, "turn-1", "agent-%1", &pane_id, 0)
        .unwrap();

    assert_eq!(observed, 1);
    assert!(
        !service
            .running_shell_transactions_for_tests()
            .contains_key(&marker)
    );
    assert!(!service.shell_transaction_requires_start_marker_for_tests(&marker));
    assert!(!service.shell_transaction_started_for_tests(&marker));
    assert!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .any(|turn| turn.turn_id == "turn-1" && turn.state == AgentTurnState::Failed)
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("shell transaction end marker arrived before the start marker"),
        "{pane_text}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies shell transaction payload bytes are deferred until the wrapper
/// receiver emits its start marker.
///
/// Large generated file-action scripts must not be sent as part of the initial
/// shell wrapper. Waiting for the start marker proves the shell has reached the
/// read loop that treats following bytes as payload data instead of shell
/// source.
#[test]
fn runtime_shell_transaction_start_streams_deferred_payload() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let mut process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();
    mark_test_pane_ready(&mut service, &pane_id);
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-stream-payload","input":"run command"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.remove_pending_agent_provider_task("turn-1");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "shell action".to_string(),
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
                    id: "shell-stream".to_string(),
                    rationale: "run payload command".to_string(),
                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: "Run payload command".to_string(),
                        command: "printf '%s\\n' payload-marker".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    let deferred_wrapper = service.drain_pane_io_transition().side_effects;
    assert_eq!(deferred_wrapper.len(), 1);
    let wrapper_text = String::from_utf8_lossy(deferred_wrapper[0].pane_input_parts().1);
    assert!(wrapper_text.contains("__mez_tx_"), "{wrapper_text}");
    assert!(!wrapper_text.contains("payload-marker"), "{wrapper_text}");
    let (marker, transaction) = service
        .running_shell_transactions_for_tests()
        .iter()
        .find(|(_, transaction)| {
            matches!(
                transaction.kind,
                RunningShellTransactionKind::AgentAction { ref action_id }
                    if action_id == "shell-stream"
            )
        })
        .map(|(marker, transaction)| (marker.clone(), transaction.clone()))
        .unwrap();
    assert!(transaction.pending_input_payload.is_some());

    service
        .observe_agent_shell_transaction_start(&pane_id, &marker, "turn-1", "agent-%1", &pane_id)
        .unwrap();

    let deferred_payload = service.drain_pane_io_transition().side_effects;
    assert_eq!(deferred_payload.len(), 1);
    let payload_text = String::from_utf8_lossy(deferred_payload[0].pane_input_parts().1);
    let encoded = payload_text
        .lines()
        .take_while(|line| !line.starts_with("__MEZ_COMMAND_PAYLOAD_END_"))
        .collect::<String>();
    let decoded = String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(encoded.as_bytes())
            .unwrap(),
    )
    .unwrap();
    assert!(decoded.contains("payload-marker"), "{decoded}");
    assert!(
        service
            .running_shell_transactions_for_tests()
            .get(&marker)
            .unwrap()
            .pending_input_payload
            .is_none()
    );
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies pending payload handoff uses a short start-marker deadline.
///
/// Non-stateful shell actions wait for an OSC start marker before sending the
/// encoded command body. If that marker is lost or the wrapper never reaches
/// the receiver loop, the transaction should time out quickly instead of
/// occupying the pane until the full command timeout expires.
#[test]
fn runtime_shell_transaction_pending_payload_uses_short_start_timer() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = "%1".to_string();
    let mut process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();
    service.running_shell_transactions_mut_for_tests().insert(
        "marker-start".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "shell-1".to_string(),
            },
            pane_id: pane_id.clone(),
            command: "grep -n needle file.txt".to_string(),
            started_at_unix_ms: 1_000,
            timeout_ms: Some(10 * 60 * 1000),
            pending_input_payload: Some(b"payload\n".to_vec()),
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let timer = service
        .running_shell_transaction_timers()
        .into_iter()
        .find(|timer| timer.marker == "marker-start")
        .unwrap();

    assert_eq!(timer.timeout_ms, 30_000);

    service
        .observe_agent_shell_transaction_start(
            &pane_id,
            "marker-start",
            "turn-1",
            "agent-%1",
            &pane_id,
        )
        .unwrap();
    let timer = service
        .running_shell_transaction_timers()
        .into_iter()
        .find(|timer| timer.marker == "marker-start")
        .unwrap();
    assert_eq!(timer.timeout_ms, 10 * 60 * 1000);
    let _ = process.terminate(Duration::from_millis(10));
}
