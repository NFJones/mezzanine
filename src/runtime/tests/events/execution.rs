//! Runtime tests for events execution behavior.

use super::*;

/// Verifies action execution rows keep secondary count metadata visually quiet.
///
/// Multi-target action previews need to tell users there are additional
/// targets without letting that bookkeeping compete with the action verb or
/// the primary path argument.
#[test]
fn runtime_multi_target_action_line_mutes_secondary_count() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let action = mez_agent::AgentAction {
        id: "patch-many".to_string(),
        rationale: String::new(),
        payload: mez_agent::AgentActionPayload::ApplyPatch {
            patch: concat!(
                "*** Begin Patch\n",
                "*** Update File: src/runtime/render.rs\n",
                "@@\n-old\n+new\n",
                "*** Update File: src/agent/maap.rs\n",
                "@@\n-old\n+new\n",
                "*** Update File: src/terminal/screen.rs\n",
                "@@\n-old\n+new\n",
                "*** End Patch"
            )
            .to_string(),
            strip: None,
        },
    };

    let emitted = service
        .append_agent_action_execution_text_to_terminal_buffer("%1", &action)
        .unwrap();
    assert!(emitted);

    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let action_line = styled_lines
        .iter()
        .find(|line| line.text.contains("agent: apply patch:"))
        .unwrap();
    assert!(
        action_line
            .text
            .contains("agent: apply patch: src/agent/maap.rs (+2 more)"),
        "{action_line:?}"
    );
    let theme = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .ui_theme;
    let metadata_column = display_column_for_fragment(&action_line.text, "(+2 more)");
    let metadata_rendition = styled_line_rendition_at(action_line, metadata_column);
    assert_eq!(
        metadata_rendition.foreground,
        Some(theme.colors.agent_transcript_status.foreground)
    );
    assert!(metadata_rendition.dim);
}

/// Verifies `/loop --limit` rejects a non-positive per-command loop limit.
///
/// A zero iteration budget would prevent `/loop` from running even the first
/// work turn, so the command must fail validation before mutating pane loop
/// state.
#[test]
fn runtime_agent_loop_limit_option_rejects_zero() {
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-loop-limit-zero"));
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store);
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 100).unwrap(),
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let error = service
        .execute_agent_shell_loop_command("%1", "/loop --limit 0 review this document")
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("/loop --limit requires a positive integer"),
        "{error}"
    );
    assert!(!service.agent_loops_by_pane.contains_key("%1"));
}

/// Verifies stopping a `/loop`-owned turn clears the pane loop controller
/// state before the turn finishes interrupted.
///
/// Early stop previously bypassed the normal loop follow-up cleanup, leaving
/// stale `agent_loops_by_pane` and `agent_loop_turns` entries that blocked the
/// next `/loop` command in the same pane.
#[test]
fn runtime_agent_loop_stop_clears_interrupted_loop_state() {
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-loop-stop"));
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 100).unwrap(),
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
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
            content: "review this document".to_string(),
        })
        .unwrap();
    service
        .agent_shell_store_mut()
        .record_transcript_entries("%1", 1)
        .unwrap();

    let outcome = service
        .execute_agent_shell_loop_command("%1", "/loop --fork review this document")
        .unwrap();

    assert!(matches!(
        outcome,
        crate::runtime::AgentShellCommandOutcome::Mutated { .. }
    ));
    let loop_session = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    assert_ne!(loop_session, old_session);
    assert!(service.agent_loops_by_pane.contains_key("%1"));
    assert!(service.agent_loop_turns.contains_key("turn-1"));

    let stopped = service.stop_agent_turn_for_pane("%1").unwrap();

    assert_eq!(stopped.turn_id, "turn-1");
    assert!(!service.agent_loops_by_pane.contains_key("%1"));
    assert!(!service.agent_loop_turns.contains_key("turn-1"));
    let session = service.agent_shell_store().get("%1").unwrap();
    assert_eq!(session.session_id, old_session);
    assert!(!session.ephemeral);
    assert!(
        session
            .ephemeral_transcript_source_conversation_id
            .is_none()
    );
    assert_eq!(session.ephemeral_transcript_source_entries, 0);
    assert!(transcript_store.summary(&loop_session).unwrap().is_none());
    assert_eq!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == "turn-1")
            .map(|turn| turn.state),
        Some(AgentTurnState::Interrupted)
    );

    let restarted = service
        .execute_agent_shell_loop_command("%1", "/loop review this document")
        .unwrap();

    assert!(matches!(
        restarted,
        crate::runtime::AgentShellCommandOutcome::Mutated { .. }
    ));
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies that bootstrap parsing uses the hidden transaction capture rather
/// than the visible pane screen. Bootstrap traffic is normally hidden from the
/// terminal buffer, so parsing only screen history leaves the pane marked as
/// bootstrap-pending and causes a tick-time bootstrap loop.
#[test]
fn runtime_bootstrap_completion_uses_hidden_transaction_output_and_clears_pending() {
    let mut service = test_runtime_service();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_pane_readiness("%1", PaneReadinessState::Busy);
    let marker = "bootstrap-marker";
    let turn_id = "bootstrap-%1-test";
    let output = "env\tos\tLinux\n\
env\tarch\tx86_64\n\
env\tkernel_version\t6.8.0-generic\n\
env\thost\tmyhost\n\
env\tuser\tme\n\
env\tshell_path\t/bin/sh\n\
env\tshell_class\tposix-sh\n\
env\tshell_version\t/bin/sh\n\
env\tpath\t/usr/local/bin:/usr/bin:/bin\n\
env\tcwd\t/home/me/project\n\
env\tproject_root\t/home/me/project\n\
env\tgit_repo\t1\n\
bootstrap\tcomplete\t1714500000\n\
tool\tsed\t1\t/usr/bin/sed\tGNU sed 4.9\tcommand -v sed\t0\t/usr/bin/sed --version\t0\t1714500000\n";
    service.running_shell_transactions.insert(
        marker.to_string(),
        RunningShellTransactionRef {
            turn_id: turn_id.to_string(),
            kind: RunningShellTransactionKind::Bootstrap,
            pane_id: "%1".to_string(),
            command: "bootstrap".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: output.len(),
            observed_output_preview: output.to_string(),
            observed_output_truncated: false,
        },
    );

    let observed = service
        .observe_agent_shell_transaction_end("%1", marker, turn_id, "agent-%1", "%1", 0)
        .unwrap();

    assert_eq!(observed, 1);
    assert!(
        !service.pane_bootstrap_pending.contains("%1"),
        "bootstrap pending should be cleared after one completed attempt"
    );
    let signature = service.pane_environment_signatures.get("%1").unwrap();
    assert_eq!(signature.working_directory, "/home/me/project");
    assert_eq!(signature.project_root.as_deref(), Some("/home/me/project"));
    assert!(
        service
            .tool_discovery_cache
            .get(signature)
            .is_some_and(|inventory| inventory.sed)
    );
    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::Ready
    );
    service.maybe_bootstrap_ready_panes().unwrap();
    assert!(
        service
            .running_shell_transactions
            .values()
            .all(|transaction| transaction.kind != RunningShellTransactionKind::Bootstrap)
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies failure-feedback accounting is per failed action, not per batch.
///
/// A single model response may contain multiple correctable action failures.
/// Each failed action should receive its own bounded retry counter so one bad
/// action does not amortize away another action's correction opportunity.
#[test]
fn runtime_action_failure_retry_budget_is_per_failed_action() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-per-action-retry","input":"research docs"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.pending_agent_provider_tasks.remove("turn-1");
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .cloned()
        .unwrap();
    let first_action = mez_agent::AgentAction {
        id: "fetch-first".to_string(),
        rationale: "try first source".to_string(),
        payload: mez_agent::AgentActionPayload::FetchUrl {
            url: "https://example.test/first".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let second_action = mez_agent::AgentAction {
        id: "fetch-second".to_string(),
        rationale: "try second source".to_string(),
        payload: mez_agent::AgentActionPayload::FetchUrl {
            url: "https://example.test/second".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let first_result = mez_agent::ActionResult::failed(
        &turn,
        &first_action,
        ActionStatus::Failed,
        "network_http_error",
        "network request returned HTTP 404 for first source",
    )
    .unwrap();
    let second_result = mez_agent::ActionResult::failed(
        &turn,
        &second_action,
        ActionStatus::Failed,
        "network_http_error",
        "network request returned HTTP 404 for second source",
    )
    .unwrap();
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture("turn-1"),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "two failed network fetches".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![first_action, second_action],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![first_result, second_result],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };

    let queued = service
        .queue_agent_failure_feedback_for_correction(
            &turn,
            &mut execution,
            "network_research_failed_actions",
        )
        .unwrap();

    assert!(queued);
    let mut attempt_values = service
        .agent_turn_failure_feedback_attempts
        .values()
        .copied()
        .collect::<Vec<_>>();
    attempt_values.sort_unstable();
    assert_eq!(attempt_values, vec![1, 1]);
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies that intentionally terminal model actions do not use the automatic
/// failure-feedback path. Cancellations and denials represent user or policy
/// boundaries rather than correctable execution evidence, so they must end the
/// turn without queuing another provider request.
#[test]
fn runtime_cancelled_action_does_not_queue_failure_feedback() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-cancel-no-feedback","input":"stop"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.pending_agent_provider_tasks.remove("turn-1");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "abort".to_string(),
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
                    id: "abort-1".to_string(),
                    rationale: "abort the turn".to_string(),
                    payload: mez_agent::AgentActionPayload::Abort {
                        reason: "cannot continue".to_string(),
                    },
                }],
                final_turn: true,
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

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert!(service.agent_turn_failure_feedback_attempts.is_empty());
    assert!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .any(|turn| turn.turn_id == "turn-1" && turn.state == AgentTurnState::Failed)
    );
    service.terminate_all_pane_processes().unwrap();
}
