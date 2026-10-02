//! Accepted command projection parity across runtime admission and persistence.
//!
//! Command previews remain presentation only until normal dispatch authorization;
//! accepted source is retained once without a full-redraw or duplicate pane row.

use super::*;

/// Verifies exact streamed rationale and command rows become the authoritative
/// shell-action presentation without restoring or appending the preview again.
///
/// A current projection for one ready, running shell action already has final
/// wrapping and styling. Completion must preserve that screen, persist both
/// semantic sources once, dispatch the command, and request only incremental
/// pane output so the attached client never performs a full-display clear.
#[tokio::test]
async fn runtime_streaming_command_completion_promotes_without_full_redraw() {
    for log_level in [AgentLogLevel::Normal, AgentLogLevel::Verbose] {
        let mut service = test_runtime_service();
        let transcript_store = AgentTranscriptStore::new(temp_root("streaming-command-promotion"));
        service.set_agent_transcript_store(transcript_store.clone());
        let primary = service
            .attach_primary("primary", true, Size::new(48, 12).unwrap(), 120)
            .unwrap();
        service.start_initial_pane_process(None).unwrap();
        service.permission_policy_mut().set_approval_bypass(true);
        mark_test_pane_ready(&mut service, "%1");
        let conversation_id = service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap()
            .session_id
            .clone();
        service
            .agent_shell_store_mut()
            .set_log_level("%1", log_level)
            .unwrap();
        let started = service
            .start_agent_prompt_turn("%1", "print alpha beta")
            .unwrap();
        let turn = service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == started.turn_id)
            .cloned()
            .unwrap();
        service.remove_pending_agent_provider_task(&turn.turn_id);

        let rationale = "Run the requested print command";
        let summary = "Print the requested output";
        let command = "printf 'alpha beta\\n'";
        for event in [
            mez_agent::StreamingSayEvent::RationaleStarted,
            mez_agent::StreamingSayEvent::RationaleTextDelta {
                text: rationale.to_string(),
            },
            mez_agent::StreamingSayEvent::RationaleTextComplete,
            mez_agent::StreamingSayEvent::ShellCommandSummaryStarted { action_index: 0 },
            mez_agent::StreamingSayEvent::ShellCommandSummaryTextDelta {
                action_index: 0,
                text: summary.to_string(),
            },
            mez_agent::StreamingSayEvent::ShellCommandSummaryTextComplete { action_index: 0 },
            mez_agent::StreamingSayEvent::ShellCommandStarted { action_index: 0 },
            mez_agent::StreamingSayEvent::ShellCommandTextDelta {
                action_index: 0,
                text: command.to_string(),
            },
            mez_agent::StreamingSayEvent::ShellCommandTextComplete { action_index: 0 },
        ] {
            service
                .apply_agent_streaming_say_event_to_terminal_buffer("%1", &turn.turn_id, &event)
                .unwrap();
        }
        let work = service
            .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
            .unwrap()
            .expect("complete rationale and command should project");
        let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
        assert!(
            service
                .apply_agent_streaming_say_projection_result(projection)
                .unwrap()
        );
        let projected_screen = service.agent_pane_screen("%1").unwrap().clone();
        let projected_command_rows = projected_screen
            .normal_content_lines()
            .into_iter()
            .filter(|line| line.contains("printf") || line.contains("alpha beta"))
            .count();

        let action = mez_agent::AgentAction {
            id: "shell-streamed".to_string(),

            payload: mez_agent::AgentActionPayload::ShellCommand {
                summary: summary.to_string(),
                command: command.to_string(),
                interactive: false,
                stateful: false,
                timeout_ms: None,
            },
        };
        let mut request = runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id);
        request.allowed_actions =
            mez_agent::AllowedActionSet::for_capability(mez_agent::AgentCapability::Shell);
        let execution = mez_agent::AgentTurnExecution {
            request,
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: String::new(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    rationale: rationale.to_string(),

                    actions: vec![action.clone()],
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![mez_agent::ActionResult::running(
                &turn,
                &action,
                vec!["shell action accepted".to_string()],
                None,
            )],
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        };

        let transition = service
            .apply_agent_provider_completed_transition(
                &AgentId::opaque(turn.agent_id.clone()).unwrap(),
                &turn.turn_id,
                execution,
            )
            .await
            .unwrap();

        assert!(transition.applied);
        assert!(transition.side_effects.iter().any(|effect| matches!(
            effect,
            RuntimeSideEffect::RenderClient {
                reason: RenderInvalidationReason::PaneOutput,
                ..
            }
        )));
        assert!(transition.side_effects.iter().all(|effect| !matches!(
            effect,
            RuntimeSideEffect::RenderClient {
                reason: RenderInvalidationReason::FullRedraw,
                ..
            }
        )));
        if log_level == AgentLogLevel::Normal {
            assert_eq!(service.agent_pane_screen("%1").unwrap(), &projected_screen);
        }
        let settled_text = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        for source in [rationale, summary, command] {
            assert_eq!(
                settled_text.matches(source).count(),
                1,
                "{log_level:?}: {settled_text}"
            );
        }
        let settled_command_rows = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .into_iter()
            .filter(|line| line.contains("printf") || line.contains("alpha beta"))
            .count();
        assert_eq!(settled_command_rows, projected_command_rows);
        assert!(
            service
                .running_shell_transactions_for_tests()
                .values()
                .any(|transaction| {
                    matches!(
                        &transaction.kind,
                        RunningShellTransactionKind::AgentAction { action_id }
                            if action_id == "shell-streamed"
                    )
                })
        );

        let entries = transcript_store
            .inspect_presentation(&conversation_id)
            .unwrap();
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.source_text.as_deref() == Some(rationale))
                .count(),
            1,
            "{entries:?}"
        );
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.source_text.as_deref() == Some(summary))
                .count(),
            1,
            "{entries:?}"
        );
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.source_text.as_deref() == Some(command))
                .count(),
            1,
            "{entries:?}"
        );
        let sources = entries
            .iter()
            .filter_map(|entry| entry.source_text.as_deref())
            .filter(|source| [rationale, summary, command].contains(source))
            .collect::<Vec<_>>();
        assert_eq!(sources, [rationale, summary, command]);
        service.terminate_all_pane_processes().unwrap();
        drop(primary);
    }
}

/// Accepted command intent is independent of shell readiness and execution state.
/// Project rationale, summary, and command separately as the stream arrives,
/// then validate the same source with pending or successful execution. Neither
/// acceptance nor later output-tail replacement may erase or replay those rows.
#[tokio::test]
async fn runtime_streaming_command_intent_survives_validation_and_tail_settlement() {
    for (ready, rows, sibling) in [
        (false, 24, false),
        (true, 24, false),
        (false, 4, false),
        (true, 4, false),
        (true, 24, true),
        (false, 4, true),
    ] {
        for status in [
            ActionStatus::Running,
            ActionStatus::Succeeded,
            ActionStatus::Blocked,
            ActionStatus::Failed,
        ] {
            let mut service = test_runtime_service();
            let store = AgentTranscriptStore::new(temp_root("command-intent-settlement"));
            service.set_agent_transcript_store(store.clone());
            service
                .attach_primary("primary", true, Size::new(72, rows).unwrap(), 200)
                .unwrap();
            service.start_initial_pane_process(None).unwrap();
            if ready {
                mark_test_pane_ready(&mut service, "%1");
            }
            let conversation_id = service
                .agent_shell_store_mut()
                .enter_or_resume("%1")
                .unwrap()
                .session_id
                .clone();
            let started = service
                .start_agent_prompt_turn("%1", "inspect command logs")
                .unwrap();
            let turn = service
                .agent_turn_ledger()
                .turns()
                .iter()
                .find(|turn| turn.turn_id == started.turn_id)
                .cloned()
                .unwrap();
            service.remove_pending_agent_provider_task(&turn.turn_id);
            set_agent_pane_screen_for_test(
                &mut service,
                "%1",
                TerminalScreen::new(Size::new(72, rows).unwrap(), 200).unwrap(),
            );
            let rationale = "Keep the reasoning above the command";
            let summary = "Inspect the retained command log";
            let command = "printf retained-command";
            for event in [
                mez_agent::StreamingSayEvent::RationaleStarted,
                mez_agent::StreamingSayEvent::RationaleTextDelta {
                    text: rationale.to_string(),
                },
                mez_agent::StreamingSayEvent::RationaleTextComplete,
                mez_agent::StreamingSayEvent::ShellCommandSummaryStarted { action_index: 0 },
                mez_agent::StreamingSayEvent::ShellCommandSummaryTextDelta {
                    action_index: 0,
                    text: summary.to_string(),
                },
                mez_agent::StreamingSayEvent::ShellCommandSummaryTextComplete { action_index: 0 },
                mez_agent::StreamingSayEvent::ShellCommandStarted { action_index: 0 },
                mez_agent::StreamingSayEvent::ShellCommandTextDelta {
                    action_index: 0,
                    text: command.to_string(),
                },
                mez_agent::StreamingSayEvent::ShellCommandTextComplete { action_index: 0 },
                mez_agent::StreamingSayEvent::ActionComplete { action_index: 0 },
            ] {
                service
                    .ingest_provider_log(
                        "%1",
                        &turn.turn_id,
                        crate::runtime::RuntimeProviderLogInput::Progress(&event),
                    )
                    .unwrap();
                if let Some(work) = service
                    .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
                    .unwrap()
                {
                    let projection =
                        RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
                    service
                        .apply_agent_streaming_say_projection_result(projection)
                        .unwrap();
                }
            }
            let projected = service.agent_pane_screen("%1").unwrap().clone();
            let text = projected.normal_content_lines().join("\n");
            for source in [rationale, summary, command] {
                assert_eq!(text.matches(source).count(), 1, "{text}");
            }
            if sibling {
                for event in [
                    mez_agent::StreamingSayEvent::ShellCommandStarted { action_index: 1 },
                    mez_agent::StreamingSayEvent::ShellCommandTextDelta {
                        action_index: 1,
                        text: "printf later-command".to_string(),
                    },
                    mez_agent::StreamingSayEvent::ShellCommandTextComplete { action_index: 1 },
                    mez_agent::StreamingSayEvent::ActionComplete { action_index: 1 },
                ] {
                    service
                        .ingest_provider_log(
                            "%1",
                            &turn.turn_id,
                            crate::runtime::RuntimeProviderLogInput::Progress(&event),
                        )
                        .unwrap();
                    if let Some(work) = service
                        .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
                        .unwrap()
                    {
                        let projection =
                            RuntimeSessionService::build_agent_streaming_say_projection(work)
                                .unwrap();
                        service
                            .apply_agent_streaming_say_projection_result(projection)
                            .unwrap();
                    }
                }
                assert!(
                    service.agent_pane_screen("%1").unwrap() == &projected,
                    "later unvalidated source must not replace the visible prefix"
                );
            }
            let action = mez_agent::AgentAction {
                id: "retained-command".to_string(),
                payload: mez_agent::AgentActionPayload::ShellCommand {
                    summary: summary.to_string(),
                    command: command.to_string(),
                    interactive: false,
                    stateful: false,
                    timeout_ms: None,
                },
            };
            let mut result = mez_agent::ActionResult::running(&turn, &action, Vec::new(), None);
            result.status = status;
            let mut actions = vec![action];
            let mut action_results = vec![result];
            if sibling {
                let action = mez_agent::AgentAction {
                    id: "later-command".to_string(),
                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: String::new(),
                        command: "printf later-command".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                };
                action_results.push(mez_agent::ActionResult::running(
                    &turn,
                    &action,
                    Vec::new(),
                    None,
                ));
                actions.push(action);
            }
            let execution = mez_agent::AgentTurnExecution {
                request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
                response: mez_agent::ModelResponse {
                    provider: "runtime-batch".to_string(),
                    model: "test".to_string(),
                    raw_text: String::new(),
                    usage: Default::default(),
                    latest_request_usage: None,
                    quota_usage: Default::default(),
                    provider_transcript_events: Vec::new(),
                    action_batch: Some(mez_agent::MaapBatch {
                        rationale: rationale.to_string(),
                        actions,
                    }),
                },
                latest_response_usage: Default::default(),
                routing_token_usage_by_model: std::collections::BTreeMap::new(),
                action_results,
                final_turn: false,
                terminal_state: AgentTurnState::Running,
            };
            service
                .ingest_provider_log(
                    "%1",
                    &turn.turn_id,
                    crate::runtime::RuntimeProviderLogInput::Validated(&execution),
                )
                .unwrap();
            assert!(
                service.agent_pane_screen("%1").unwrap() == &projected,
                "validated intent must not disappear: ready={ready}, status={status:?}, rows={rows}; before={text}; after={}",
                service
                    .agent_pane_screen("%1")
                    .unwrap()
                    .normal_content_lines()
                    .join("\n")
            );
            let owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
                turn_id: turn.turn_id.clone(),
                action_id: "retained-command".to_string(),
                marker: "retained-command-attempt".to_string(),
            };
            for (revision, lines) in [
                (1, vec!["first output".to_string()]),
                (
                    2,
                    (0..5)
                        .map(|index| format!("growing output {index}"))
                        .collect(),
                ),
                (3, vec!["new output".to_string()]),
            ] {
                service
                    .update_agent_shell_output_preview("%1", owner.clone(), revision, &lines)
                    .unwrap();
                let text = service
                    .agent_pane_screen("%1")
                    .unwrap()
                    .normal_content_lines()
                    .join("\n");
                for source in [rationale, summary, command] {
                    assert_eq!(text.matches(source).count(), 1, "{text}");
                }
            }
            assert!(service.settle_agent_shell_output_preview("%1", &owner));
            service
                .append_agent_status_text_to_terminal_buffer("%1", "command settled")
                .unwrap();
            service
                .ingest_provider_log(
                    "%1",
                    &turn.turn_id,
                    crate::runtime::RuntimeProviderLogInput::Settled(&execution),
                )
                .unwrap();
            let text = service
                .agent_pane_screen("%1")
                .unwrap()
                .normal_content_lines()
                .join("\n");
            for source in [rationale, summary, command] {
                assert_eq!(text.matches(source).count(), 1, "{text}");
                assert_eq!(
                    store
                        .inspect_presentation(&conversation_id)
                        .unwrap()
                        .iter()
                        .filter(|entry| entry.source_text.as_deref() == Some(source))
                        .count(),
                    1
                );
            }
            let entries = store.inspect_presentation(&conversation_id).unwrap();
            set_agent_pane_screen_for_test(
                &mut service,
                "%1",
                TerminalScreen::new(Size::new(72, rows).unwrap(), 200).unwrap(),
            );
            service
                .replay_agent_presentation_entries_to_terminal_buffer("%1", &entries)
                .unwrap();
            let replayed = service
                .agent_pane_screen("%1")
                .unwrap()
                .normal_content_lines()
                .join("\n");
            let positions = [rationale, summary, command].map(|source| {
                assert_eq!(replayed.matches(source).count(), 1, "{replayed}");
                replayed.find(source).unwrap()
            });
            assert!(
                positions.windows(2).all(|pair| pair[0] < pair[1]),
                "{replayed}"
            );
            assert!(!replayed.contains("growing output"), "{replayed}");
            assert!(!replayed.contains("new output"), "{replayed}");
            service.terminate_all_pane_processes().unwrap();
        }
    }
}
