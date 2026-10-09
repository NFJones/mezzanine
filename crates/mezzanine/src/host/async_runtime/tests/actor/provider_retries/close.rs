//! Async close-rejection settlement and immediate corrective provider dispatch.
//!
//! Completion carries a running close action, not a fabricated rejection. The
//! actor executes its ownership check and must schedule the existing bounded
//! continuation before any unrelated timer; a changed-course completion then
//! retires the same turn without altering pane resources.

use super::*;

/// A valid unknown child close runs through production actor settlement, retains
/// opaque rejected evidence and promptly dispatches one corrective request. The
/// correction finishes locally without replaying close or inventing success.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_unavailable_close_dispatches_correction_immediately() {
    let mut service = test_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "close-feedback".into(),
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
        .execute_agent_shell_command(&primary, "close or continue locally")
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
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
        &turn,
        context,
    )
    .unwrap();
    let action = mez_agent::AgentAction {
        id: "unknown-child".into(),
        payload: mez_agent::AgentActionPayload::CloseAgent {
            agent_id: "agent-%999".into(),
        },
    };
    let execution = mez_agent::AgentTurnExecution {
        request,
        response: mez_agent::ModelResponse {
            provider: task.model_profile.provider.clone(),
            model: task.model_profile.model.clone(),
            raw_text: "close unknown child".into(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "close child".into(),
                actions: vec![action.clone()],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: vec![mez_agent::ActionResult::running(
            &turn,
            &action,
            vec![],
            None,
        )],
        final_turn: false,
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
        assert_eq!(dispatches.iter().filter(|effect| matches!(effect, RuntimeSideEffect::DispatchAgentProvider { turn_id, .. } if turn_id == &turn.turn_id)).count(), 1);
        assert_eq!(
            handle.pending_agent_provider_tasks().await.unwrap().len(),
            1
        );
        let say = mez_agent::AgentAction {
            id: "finish-local".into(),
            payload: mez_agent::AgentActionPayload::Say {
                status: mez_agent::SayStatus::Final,
                content_type: "text/plain".into(),
                text: "continued locally".into(),
            },
        };
        let mut final_execution = execution;
        final_execution
            .response
            .action_batch
            .as_mut()
            .unwrap()
            .actions = vec![say.clone()];
        final_execution.response.raw_text = "continued locally".into();
        final_execution.action_results = vec![mez_agent::ActionResult::succeeded(
            &turn,
            &say,
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
