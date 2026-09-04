//! Async-runtime tests owned by provider dispatch behavior.

use super::super::*;

/// Verifies that a rewritten OpenAI input detected while registering a worker
/// lease is advisory. The claim must still dispatch and retain a fresh baseline
/// without failing the turn or forwarding an error to the service supervisor.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_contains_provider_claim_cache_chain_failure() {
    let mut service = test_service();
    let auth_root = std::env::temp_dir().join(format!(
        "mez-claim-cache-chain-auth-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    service.set_auth_store(crate::security::auth::AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&auth_root),
    ));
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "claim-cache-chain-failure".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "default"
shell_mode = "pane"
[permissions]
sandbox = "policy-only"
[providers.runtime-batch]
kind = "openai"
models = ["invalid-model"]
default_model = "invalid-model"
[model_profiles.default]
provider = "runtime-batch"
model = "invalid-model"
"#
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(160, 40).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service
        .execute_agent_shell_command(&primary, "exercise claim failure")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let task = service.pending_agent_provider_tasks().remove(0);
    let agent_id = AgentId::opaque(task.agent_id.clone()).unwrap();
    let dispatch = service
        .claim_configured_agent_provider_task(&agent_id, &task.turn_id)
        .unwrap()
        .unwrap_or_else(|| {
            panic!(
                "initial claim unavailable: {:?}",
                service
                    .pane_screen("%1")
                    .map(|screen| screen.normal_content_lines())
            )
        });
    service
        .record_claimed_agent_provider_task(&dispatch, 1, 30_000)
        .unwrap();
    let (context, _) = service
        .prepare_agent_turn_model_context(
            &dispatch.turn,
            service
                .agent_turn_contexts()
                .get(&task.turn_id)
                .unwrap()
                .clone(),
            &service.mcp_registry().prompt_summary(),
            &dispatch.model_profile,
        )
        .unwrap();
    let mut previous = context.previous_request().unwrap().clone();
    previous.messages.push(mez_agent::ModelMessage {
        role: mez_agent::ModelMessageRole::User,
        source: mez_agent::ContextSourceKind::UserInstruction,
        placement: mez_agent::ContextPlacement::ConversationAppend,
        content: "input no longer present in canonical chronology".to_string(),
    });
    service.retain_agent_provider_request_chain(&dispatch.turn, previous);
    service.clear_claimed_agent_provider_task(&task.turn_id);
    assert!(service.queue_agent_provider_task(task.turn_id.clone()));
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let client = async {
        let claimed = handle
            .claim_configured_agent_provider_task(agent_id, task.turn_id.clone())
            .await
            .expect("request assembly failures must not escape to the provider supervisor");
        assert!(claimed.is_some());
        assert!(
            handle
                .pending_agent_provider_tasks()
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            handle
                .drain_timer_side_effects(16)
                .await
                .unwrap()
                .iter()
                .any(|effect| {
                    matches!(effect, RuntimeSideEffect::ScheduleTimer { key, .. }
                if key.kind == RuntimeTimerKind::ProviderClaim)
                })
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };
    let ((), mut exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.service.agent_scheduler().snapshot().running, 1);
    assert_eq!(
        exit.service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == task.turn_id)
            .unwrap()
            .state,
        mez_agent::AgentTurnState::Running
    );
    let pane_text = exit
        .service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!pane_text.contains("provider_error"), "{pane_text}");
    exit.service
        .complete_running_agent_turn_and_start_ready(
            &dispatch.turn,
            mez_agent::AgentTurnState::Completed,
            "test_cache_warning_completed",
        )
        .unwrap();
    let next = exit
        .service
        .execute_agent_shell_command(&primary, "continue in the same conversation")
        .unwrap();
    assert!(next.contains(r#""state":"running""#), "{next}");
    let next_task = exit.service.pending_agent_provider_tasks().remove(0);
    let next_dispatch = exit
        .service
        .claim_configured_agent_provider_task(
            &AgentId::opaque(next_task.agent_id).unwrap(),
            &next_task.turn_id,
        )
        .unwrap()
        .expect("next prompt must remain dispatchable");
    assert_eq!(
        next_dispatch.turn.conversation_id,
        dispatch.turn.conversation_id
    );
    exit.service
        .record_claimed_agent_provider_task(&next_dispatch, 2, 30_000)
        .unwrap();
}

/// Verifies that render workers can drain only render invalidations while
/// leaving provider dispatches queued for provider workers. This protects the
/// side-effect queue from family-specific workers stealing unrelated work as
/// the async runtime grows more concrete worker services.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_drains_render_side_effects_without_stealing_provider_dispatches() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service
        .execute_agent_shell_command(&primary, "summarize the pane")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    let expected_agent = AgentId::opaque(pending[0].agent_id.clone()).unwrap();
    let expected_turn = pending[0].turn_id.clone();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let provider_key =
            RuntimeTimerKey::new(RuntimeTimerKind::ProviderPoll, "agent-provider", 1);
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::ScheduleTimer {
                key: provider_key.clone(),
                delay_ms: 1,
            }])
            .await
            .unwrap();
        assert_eq!(handle.drain_timer_side_effects(8).await.unwrap().len(), 1);

        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Timer(TimerEvent {
            key: provider_key,
            now_ms: 1,
        }));
        batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"render-worker-preserve\n".to_vec(),
        }));
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 2);
        assert_eq!(
            report.applied, 1,
            "pane output must publish the provider dispatch before the later poll timer is deduplicated"
        );
        assert_eq!(report.side_effects, 3);

        let render_effects = handle.drain_render_side_effects(8).await.unwrap();
        assert_eq!(
            render_effects,
            vec![RuntimeSideEffect::RenderClient {
                client_id: primary,
                reason: RenderInvalidationReason::PaneOutput,
            }]
        );
        assert_eq!(
            handle
                .drain_agent_provider_dispatch_side_effects(8)
                .await
                .unwrap(),
            vec![RuntimeSideEffect::DispatchAgentProvider {
                agent_id: expected_agent,
                turn_id: expected_turn,
            }]
        );
        let timer_effects = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(timer_effects.len(), 1);
        assert!(
            matches!(
                &timer_effects[0],
                RuntimeSideEffect::ScheduleTimer { key, .. }
                    if key.kind == RuntimeTimerKind::IdleCleanup
            ),
            "{timer_effects:?}"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.runtime_side_effects_queued, 4);
    assert_eq!(exit.metrics.runtime_side_effects_drained, 4);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies that an async provider completion which dispatches a shell command
/// also queues a runtime-owned shell transaction timer side effect. Without
/// this timer handoff, shell action timeouts still depend on the compatibility
/// tick loop instead of the dedicated Tokio timer worker. The first timer is
/// the short payload-receiver start deadline because the command body is still
/// waiting for the shell wrapper start marker.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_queues_shell_transaction_timer_after_provider_completion() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let ready = service
        .execute_terminal_command(&primary, "mark-pane-ready --acknowledge-risk --reason test")
        .unwrap();
    assert!(ready.contains("override=applied"), "{ready}");
    let start = service
        .execute_agent_shell_command(&primary, "print a marker")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    let task = pending[0].clone();
    let turn = mez_agent::AgentTurnRecord {
        turn_id: task.turn_id.clone(),
        conversation_id: "conversation-1".to_string(),
        agent_id: task.agent_id.clone(),
        pane_id: task.pane_id.clone(),
        trigger: mez_agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 1,
        deadline_at_unix_millis: 0,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        state: mez_agent::AgentTurnState::Running,
        cooperation_mode: None,

        initial_capability: None,
    };
    let action = mez_agent::AgentAction {
        id: "shell-1".to_string(),
        rationale: "run a short shell command for the user".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Print a shell marker.".to_string(),
            command: "printf 'async timer shell\\n'".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: Some(60_000),
        },
    };
    let response_batch = mez_agent::MaapBatch {
        protocol: "maap/1".to_string(),
        rationale: "test action batch rationale".to_string(),
        thought: None,
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        actions: vec![action.clone()],
        final_turn: false,
    };
    let execution = mez_agent::AgentTurnExecution {
        request: mez_agent::ModelRequest {
            provider: task.model_profile.provider.clone(),
            model: task.model_profile.model.clone(),
            reasoning_effort: task
                .model_profile
                .provider_options
                .get("reasoning_effort")
                .cloned()
                .or_else(|| task.model_profile.reasoning_profile.clone()),
            thinking_enabled: task.model_profile.thinking_enabled(),
            latency_preference: task.model_profile.latency_preference.clone(),
            prompt_cache_retention: task
                .model_profile
                .provider_options
                .get("prompt_cache_retention")
                .cloned(),
            max_output_tokens: task.model_profile.max_output_tokens(),
            temperature: None,
            stop: None,
            prompt_cache_session_id: None,
            prompt_cache_lineage_id: None,
            turn_id: task.turn_id.clone(),
            agent_id: task.agent_id.clone(),
            available_mcp_tools: Vec::new(),
            memory_actions_enabled: false,
            issue_actions_enabled: true,
            interaction_kind: mez_agent::ModelInteractionKind::ActionExecution,
            allowed_actions: mez_agent::AllowedActionSet::for_capability(
                mez_agent::AgentCapability::Shell,
            ),
            messages: vec![mez_agent::ModelMessage {
                role: mez_agent::ModelMessageRole::User,
                source: mez_agent::ContextSourceKind::UserInstruction,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                content: "print a marker".to_string(),
            }]
            .into(),
        },
        response: mez_agent::ModelResponse {
            provider: task.model_profile.provider.clone(),
            model: task.model_profile.model.clone(),
            raw_text: "shell command response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(response_batch),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![mez_agent::ActionResult::running(
            &turn,
            &action,
            vec!["shell command accepted for pane execution".to_string()],
            Some(r#"{"state":"pending_dispatch"}"#.to_string()),
        )],
        final_turn: false,
        terminal_state: mez_agent::AgentTurnState::Running,
    };
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
            agent_id: AgentId::opaque(task.agent_id).unwrap(),
            turn_id: task.turn_id.clone(),
            execution: Box::new(execution),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 2);
        let timers = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(timers.len(), 1);
        let RuntimeSideEffect::ScheduleTimer { key, delay_ms } = &timers[0] else {
            panic!("expected shell transaction timer side effect, got {timers:?}");
        };
        assert_eq!(key.kind, RuntimeTimerKind::ShellTransaction);
        assert!((0..=30 * 1000).contains(delay_ms), "{delay_ms}");
        assert!(!key.owner_id.is_empty());
        let scheduled_key = key.clone();
        let mut output_batch = RuntimeEventBatch::new();
        output_batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"shell output before marker\n".to_vec(),
        }));
        let output_report = handle.submit_runtime_events(output_batch).await.unwrap();
        assert_eq!(output_report.accepted, 1);
        assert_eq!(output_report.applied, 1);
        assert_eq!(output_report.side_effects, 2);
        let idle_timers = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(idle_timers.len(), 1);
        let RuntimeSideEffect::ScheduleTimer { key, delay_ms } = &idle_timers[0] else {
            panic!("expected idle cleanup timer side effect, got {idle_timers:?}");
        };
        assert_eq!(key.kind, RuntimeTimerKind::IdleCleanup);
        assert!(*delay_ms > 0, "{delay_ms}");

        let marker_output = format!(
            "\x1b]133;D;0;mez_marker={};mez_turn={};mez_agent=agent-%1;mez_pane=%1\x1b\\",
            scheduled_key.owner_id.as_str(),
            task.turn_id
        );
        let mut marker_batch = RuntimeEventBatch::new();
        marker_batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: marker_output.into_bytes(),
        }));
        let marker_report = handle.submit_runtime_events(marker_batch).await.unwrap();
        assert_eq!(marker_report.accepted, 1);
        assert_eq!(marker_report.applied, 1);
        assert!(marker_report.side_effects >= 2, "{marker_report:?}");
        let timer_cancellations = handle.drain_timer_side_effects(8).await.unwrap();
        assert!(
            timer_cancellations
                .iter()
                .any(|effect| matches!(effect, RuntimeSideEffect::CancelTimer { key } if key == &scheduled_key)),
            "shell transaction timer should be cancelled after marker: {timer_cancellations:?}"
        );
        assert!(
            timer_cancellations.iter().all(|effect| match effect {
                RuntimeSideEffect::CancelTimer { .. } => true,
                RuntimeSideEffect::ScheduleTimer { key, .. } => {
                    key.kind == RuntimeTimerKind::Bootstrap
                }
                _ => false,
            }),
            "only cancellation and bootstrap timer effects should be queued: {timer_cancellations:?}"
        );
        assert!(
            handle.shutdown().await.unwrap() == RuntimeLifecycleState::Running,
            "actor should shut down from running state"
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(
        exit.service.running_shell_transaction_timers().iter().all(
            |timer| timer.kind != crate::runtime::RuntimeShellTransactionTimerKind::AgentAction
        ),
        "agent shell transaction timer should be settled"
    );
    exit.service.terminate_all_pane_processes().unwrap();
}
