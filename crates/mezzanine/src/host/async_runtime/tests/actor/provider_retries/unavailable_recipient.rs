//! Async unavailable-recipient single/mixed settlement and immediate dispatch.
//!
//! The initial typed completion carries planned Running send-to-unavailable and
//! list_agents actions plus a Succeeded final say. Production actor settlement
//! must execute the message, abandon only the unstarted read-only discovery,
//! and enqueue the existing corrective provider dispatch immediately, without a
//! timer or poll rescue. A changed-course completion then retires the same turn.
//! The single-action matrix additionally supplies both planned-final flags and
//! chat compatibility shapes. These are offline planned completions, not live
//! SDK/parser captures. Heap-pinning the shared large fixture keeps wrapper
//! futures off the default test stack without changing runtime stack policy.

use super::*;

/// A mixed final batch with one unavailable recipient dispatches exactly one
/// immediate corrective provider task through production actor completion.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_unavailable_recipient_mixed_batch_dispatches_correction_immediately() {
    Box::pin(qualify_unavailable_recipient(
        false,
        false,
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
    ))
    .await;
}

/// The reopened report has ONE dispatched action and final-planned=true. Drive
/// the actual send/error/settlement/dispatch owners with exactly one planned
/// send, project audience, both final flags and DeepSeek-compatible versus
/// ordinary chat request shaping. A changed-course response completes the SAME
/// turn after one immediate correction, with no timer, extra input or resend.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_unavailable_recipient_single_action_final_and_nonfinal_correct_immediately() {
    for api in [
        mez_agent::ProviderApiCompatibility::DeepSeekChatCompletions,
        mez_agent::ProviderApiCompatibility::OpenAiChatCompletions,
    ] {
        for final_planned in [false, true] {
            Box::pin(qualify_unavailable_recipient(true, final_planned, api)).await;
        }
    }
}

/// Executes one isolated production actor workflow; only the original planned
/// completion is supplied. The runtime itself produces unavailable feedback.
async fn qualify_unavailable_recipient(
    single: bool,
    final_planned: bool,
    api: mez_agent::ProviderApiCompatibility,
) {
    let mut service = test_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "unavailable-feedback".into(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\nshell_mode=\"native\"\n[permissions]\nsandbox=\"policy-only\"\n"
                .into(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "send or continue locally")
        .unwrap();
    let task = service.pending_agent_provider_tasks().remove(0);
    let turn = service
        .agent_turn_ledger()
        .turn(&task.turn_id)
        .unwrap()
        .clone();
    let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    let request = crate::integrations::agent::context::assemble_model_request(
        &task.model_profile,
        api,
        &turn,
        context,
    )
    .unwrap();
    let send = mez_agent::AgentAction {
        id: "msg-unavailable".into(),
        payload: mez_agent::AgentActionPayload::SendMessage {
            expect_reply: false,
            recipient: "agent:agent-nowhere".into(),
            scope: Some(if single { "project" } else { "session" }.into()),
            content_type: "text/plain; charset=utf-8".into(),
            payload: "handoff".into(),
            correlation_id: None,
        },
    };
    let discovery = mez_agent::AgentAction {
        id: "discover-1".into(),
        payload: mez_agent::AgentActionPayload::ListAgents {
            agent_type: None,
            scope: None,
        },
    };
    let say = mez_agent::AgentAction {
        id: "say-final".into(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Final,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
            text: "continued locally".into(),
        },
    };
    let execution = mez_agent::AgentTurnExecution {
        request,
        response: mez_agent::ModelResponse {
            provider: task.model_profile.provider.clone(),
            model: task.model_profile.model.clone(),
            raw_text: "send or continue locally".into(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "send or continue locally".into(),
                actions: if single {
                    vec![send.clone()]
                } else {
                    vec![send.clone(), discovery.clone(), say.clone()]
                },
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: if single {
            vec![mez_agent::ActionResult::running(
                &turn,
                &send,
                Vec::new(),
                None,
            )]
        } else {
            vec![
                mez_agent::ActionResult::running(&turn, &send, Vec::new(), None),
                mez_agent::ActionResult::running(&turn, &discovery, Vec::new(), None),
                mez_agent::ActionResult::succeeded(
                    &turn,
                    &say,
                    vec!["continued locally".into()],
                    None,
                ),
            ]
        },
        final_turn: final_planned,
        terminal_state: mez_agent::AgentTurnState::Running,
    };
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let client = async {
        handle
            .record_claimed_agent_provider_task_for_tests(turn.turn_id.clone(), 1)
            .await
            .unwrap();
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
            agent_id: AgentId::opaque(turn.agent_id.clone()).unwrap(),
            turn_id: turn.turn_id.clone(),
            claim_generation: 1,
            execution: Box::new(execution.clone()),
        }));
        assert_eq!(
            handle.submit_runtime_events(batch).await.unwrap().applied,
            1
        );
        let dispatches = handle
            .drain_agent_provider_dispatch_side_effects(8)
            .await
            .unwrap();
        assert_eq!(
            dispatches
                .iter()
                .filter(|effect| matches!(effect, RuntimeSideEffect::DispatchAgentProvider { turn_id, .. } if turn_id == &turn.turn_id))
                .count(),
            1,
            "unavailable recipient must dispatch correction immediately: single={single} final={final_planned} api={api:?}"
        );
        assert_eq!(
            handle.pending_agent_provider_tasks().await.unwrap().len(),
            1
        );
        let finish = mez_agent::AgentAction {
            id: "finish-local-after-correction".into(),
            payload: mez_agent::AgentActionPayload::Say {
                status: mez_agent::SayStatus::Final,
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
                text: "continued locally".into(),
            },
        };
        let mut final_execution = execution;
        final_execution
            .response
            .action_batch
            .as_mut()
            .unwrap()
            .actions = vec![finish.clone()];
        final_execution.response.raw_text = "continued locally".into();
        final_execution.action_results = vec![mez_agent::ActionResult::succeeded(
            &turn,
            &finish,
            vec!["continued locally".into()],
            None,
        )];
        final_execution.final_turn = true;
        final_execution.terminal_state = mez_agent::AgentTurnState::Completed;
        handle
            .record_claimed_agent_provider_task_for_tests(turn.turn_id.clone(), 2)
            .await
            .unwrap();
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
            agent_id: AgentId::opaque(turn.agent_id.clone()).unwrap(),
            turn_id: turn.turn_id.clone(),
            claim_generation: 2,
            execution: Box::new(final_execution),
        }));
        assert_eq!(
            handle.submit_runtime_events(batch).await.unwrap().applied,
            1
        );
        assert!(
            handle
                .pending_agent_provider_tasks()
                .await
                .unwrap()
                .is_empty()
        );
        handle.shutdown().await.unwrap();
    };
    let ((), mut exit) = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(client, actor.run())
    })
    .await
    .unwrap();
    assert_eq!(
        exit.service
            .agent_turn_ledger()
            .turn(&turn.turn_id)
            .unwrap()
            .state,
        mez_agent::AgentTurnState::Completed
    );
    assert_eq!(exit.service.session().windows().len(), 1);
    exit.service.terminate_all_pane_processes().unwrap();
}
