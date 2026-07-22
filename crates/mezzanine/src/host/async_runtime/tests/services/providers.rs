//! Async-runtime tests owned by providers behavior.

use super::super::*;

/// Verifies async agent provider service polls runtime queue.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_agent_provider_service_polls_runtime_queue() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let service_handle = handle.clone();
    let service = async move {
        let report = run_async_agent_provider_service(
            &service_handle,
            AsyncAgentProviderServiceConfig::new(1).unwrap(),
            |polls, _| polls >= 1,
        )
        .await
        .unwrap();
        let _ = service_handle.shutdown().await.unwrap();
        report
    };

    let (report, exit) = tokio::join!(service, actor.run());

    assert_eq!(report.polls, 1);
    assert_eq!(report.idle_polls, 1);
    assert_eq!(report.executions, 0);
    assert!(exit.commands_processed >= 3);
}

/// Verifies that an idle provider service performs a bounded actor-state probe
/// even when no notification arrives. This protects prompt submission on slow
/// systems from a missed side-effect notification permit: ordinary prompt work
/// still wakes the service immediately, while the bounded probe keeps queued
/// turns from staying stranded forever.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_agent_provider_service_uses_bounded_idle_probe() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();

    let client = async {
        let config = AsyncAgentProviderServiceConfig::new(1)
            .unwrap()
            .with_idle_interval(Duration::from_millis(10))
            .unwrap();
        let report = tokio::time::timeout(
            Duration::from_millis(50),
            run_async_agent_provider_service(&handle, config, |polls, _| polls >= 2),
        )
        .await
        .unwrap()
        .unwrap();
        let _ = handle.shutdown().await.unwrap();
        report
    };

    let (report, exit) = tokio::join!(client, actor.run());
    assert_eq!(report.polls, 2);
    assert_eq!(report.idle_polls, 2);
    assert_eq!(report.executions, 0);
    assert!(exit.commands_processed >= 1);
}

/// Verifies provider monitors treat actor closure during shutdown as ordinary
/// cancellation while preserving unexpected liveness failures from a running
/// actor. The lifecycle watch is the authoritative distinction between those
/// outcomes even when its final value remains `Running` after sender closure.
#[test]
fn async_agent_provider_monitor_classifies_actor_closure_by_lifecycle() {
    let (closed_tx, closed_lifecycle) = tokio::sync::watch::channel(RuntimeLifecycleState::Running);
    drop(closed_tx);
    let closed_result = crate::host::async_runtime::client::classify_provider_monitor_liveness(
        Err(MezError::invalid_state(
            "async runtime session actor is closed",
        )),
        &closed_lifecycle,
    )
    .unwrap();
    assert!(!closed_result);

    let (terminal_tx, terminal_lifecycle) =
        tokio::sync::watch::channel(RuntimeLifecycleState::Running);
    terminal_tx.send(RuntimeLifecycleState::Stopping).unwrap();
    let terminal_result = crate::host::async_runtime::client::classify_provider_monitor_liveness(
        Err(MezError::invalid_state(
            "async runtime session actor reply was dropped",
        )),
        &terminal_lifecycle,
    )
    .unwrap();
    assert!(!terminal_result);

    let (_running_tx, running_lifecycle) =
        tokio::sync::watch::channel(RuntimeLifecycleState::Running);
    let error = crate::host::async_runtime::client::classify_provider_monitor_liveness(
        Err(MezError::invalid_state("unexpected liveness failure")),
        &running_lifecycle,
    )
    .unwrap_err();
    assert_eq!(error.message(), "unexpected liveness failure");
}

/// Verifies that the provider service delegates provider-poll timer ownership
/// to the actor instead of retaining a local duplicate guard. With pending
/// provider work and no timer worker draining the queue, multiple idle provider
/// polls should leave exactly one scheduled provider-poll timer side effect.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_agent_provider_service_uses_actor_owned_provider_poll_guard() {
    let idle_interval = Duration::from_millis(25);
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
    let start = service
        .execute_agent_shell_command(&primary, "summarize the pane")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    assert_eq!(service.pending_agent_provider_tasks().len(), 1);
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let service_handle = handle.clone();
    let service = async move {
        run_async_agent_provider_service(
            &service_handle,
            AsyncAgentProviderServiceConfig::new(1)
                .unwrap()
                .with_idle_interval(idle_interval)
                .unwrap(),
            |polls, _| polls >= 2,
        )
        .await
        .unwrap()
    };
    let clock = async {
        tokio::task::yield_now().await;
        tokio::time::advance(idle_interval).await;
        tokio::task::yield_now().await;
        tokio::time::advance(idle_interval).await;
    };
    let client = async {
        let (report, ()) = tokio::join!(service, clock);
        assert_eq!(report.polls, 2);
        assert_eq!(report.idle_polls, 2);
        assert_eq!(report.executions, 0);

        let timers = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(timers.len(), 1);
        let RuntimeSideEffect::ScheduleTimer { key, delay_ms } = &timers[0] else {
            panic!("expected provider poll timer side effect, got {timers:?}");
        };
        assert_eq!(key.kind, RuntimeTimerKind::ProviderPoll);
        assert_eq!(key.owner_id, "agent-provider");
        assert_eq!(*delay_ms, idle_interval.as_millis() as u64);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.metrics.runtime_side_effects_queued >= 1);
    assert!(exit.metrics.runtime_side_effects_drained >= 1);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies that a provider-completed shell action whose pane cannot accept
/// shell input fails the agent turn instead of failing the async runtime actor
/// request. This reproduces the daemon-exit class where the model's first shell
/// command reaches runtime dispatch before a pane process is available.
#[tokio::test(flavor = "current_thread")]
async fn async_provider_completed_shell_dispatch_error_fails_turn_without_exiting_runtime() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", mez_agent::AgentLogLevel::Trace)
        .unwrap();
    service.permission_policy_mut().set_approval_bypass(true);
    let start = service
        .execute_agent_shell_command(&primary, "list files")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let task = service
        .pending_agent_provider_tasks()
        .into_iter()
        .find(|task| task.turn_id == "turn-1")
        .expect("agent prompt should queue turn-1 provider task");
    let turn = mez_agent::AgentTurnRecord {
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        pane_id: task.pane_id.clone(),
        trigger: mez_agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 1,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        state: mez_agent::AgentTurnState::Running,
        cooperation_mode: None,

        initial_capability: None,
    };
    let action = mez_agent::AgentAction {
        id: "shell-1".to_string(),
        rationale: "list files".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "List files in the current directory".to_string(),
            command: "ls".to_string(),
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
                content: "list files".to_string(),
            }],
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
    let client = async move {
        let mut provider_batch = RuntimeEventBatch::new();
        provider_batch.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
            agent_id: AgentId::opaque(task.agent_id).unwrap(),
            turn_id: task.turn_id.clone(),
            execution: Box::new(execution),
        }));
        let report = handle.submit_runtime_events(provider_batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(
            handle.lifecycle_state().await.unwrap(),
            RuntimeLifecycleState::Running
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), actor_exit) = tokio::join!(client, actor.run());
    assert_eq!(
        actor_exit.service.lifecycle_state(),
        RuntimeLifecycleState::Running
    );
    assert!(actor_exit.service.pending_agent_provider_tasks().is_empty());
    assert!(!actor_exit.service.agent_turn_is_running("turn-1"));
    assert_eq!(
        actor_exit
            .service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    let pane_text = actor_exit
        .service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("shell command failed before execution"),
        "{pane_text}"
    );
    assert!(pane_text.contains("pane process not found"), "{pane_text}");
}

/// Verifies malformed provider-completion action state fails only the affected
/// agent turn instead of failing the async runtime actor request.
///
/// Provider completions arrive after the provider claim has already been
/// cleared. If applying the completion discovers an impossible internal action
/// state, such as a running network result whose action is missing from the
/// returned batch, the runtime must settle the turn as failed and keep the
/// daemon usable for other panes.
#[tokio::test(flavor = "current_thread")]
async fn async_provider_completion_application_error_fails_turn_without_exiting_runtime() {
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
    let start = service
        .execute_agent_shell_command(&primary, "research patch behavior")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let task = service
        .pending_agent_provider_tasks()
        .into_iter()
        .find(|task| task.turn_id == "turn-1")
        .expect("agent prompt should queue turn-1 provider task");
    let turn = mez_agent::AgentTurnRecord {
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        pane_id: task.pane_id.clone(),
        trigger: mez_agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 1,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        state: mez_agent::AgentTurnState::Running,
        cooperation_mode: None,

        initial_capability: None,
    };
    let batch_action = mez_agent::AgentAction {
        id: "fetch-listed".to_string(),
        rationale: "fetch the listed source".to_string(),
        payload: mez_agent::AgentActionPayload::FetchUrl {
            url: "https://example.com/listed".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let missing_action = mez_agent::AgentAction {
        id: "fetch-missing-result".to_string(),
        rationale: "this result no longer has a matching batch action".to_string(),
        payload: mez_agent::AgentActionPayload::FetchUrl {
            url: "https://example.com/missing".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let response_batch = mez_agent::MaapBatch {
        protocol: "maap/1".to_string(),
        rationale: "test action batch rationale".to_string(),
        thought: None,
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        actions: vec![batch_action],
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
                mez_agent::AgentCapability::NetworkFetch,
            ),
            messages: vec![mez_agent::ModelMessage {
                role: mez_agent::ModelMessageRole::User,
                source: mez_agent::ContextSourceKind::UserInstruction,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                content: "research patch behavior".to_string(),
            }],
        },
        response: mez_agent::ModelResponse {
            provider: task.model_profile.provider.clone(),
            model: task.model_profile.model.clone(),
            raw_text: "network completion response".to_string(),
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
            &missing_action,
            vec!["network action accepted".to_string()],
            Some(r#"{"state":"pending_network"}"#.to_string()),
        )],
        final_turn: false,
        terminal_state: mez_agent::AgentTurnState::Running,
    };
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let client = async move {
        let mut provider_batch = RuntimeEventBatch::new();
        provider_batch.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
            agent_id: AgentId::opaque(task.agent_id).unwrap(),
            turn_id: task.turn_id.clone(),
            execution: Box::new(execution),
        }));
        let report = handle.submit_runtime_events(provider_batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(
            handle.lifecycle_state().await.unwrap(),
            RuntimeLifecycleState::Running
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut actor_exit) = tokio::join!(client, actor.run());
    assert_eq!(
        actor_exit.service.lifecycle_state(),
        RuntimeLifecycleState::Running
    );
    assert!(actor_exit.service.pending_agent_provider_tasks().is_empty());
    assert!(!actor_exit.service.agent_turn_is_running("turn-1"));
    assert_eq!(
        actor_exit
            .service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    let pane_text = actor_exit
        .service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("Failed after"), "{pane_text}");
    actor_exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies provider workers settle runtime-owned network actions pre-ingress.
///
/// Large research turns should not submit `fetch_url` actions back to the
/// single-owner runtime actor while they are still marked running. This covers
/// the async worker boundary directly with an unsupported URL, which exercises
/// the network executor without performing external HTTP during the test.
#[tokio::test(flavor = "current_thread")]
async fn async_provider_worker_executes_network_actions_before_actor_completion() {
    let turn = mez_agent::AgentTurnRecord {
        turn_id: "turn-network-worker".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        trigger: mez_agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 1,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        state: mez_agent::AgentTurnState::Running,
        cooperation_mode: None,

        initial_capability: None,
    };
    let action = mez_agent::AgentAction {
        id: "fetch-local-file".to_string(),
        rationale: "try a local file URL".to_string(),
        payload: mez_agent::AgentActionPayload::FetchUrl {
            url: "file:///tmp/provider-doc.md".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let execution = mez_agent::AgentTurnExecution {
        request: mez_agent::ModelRequest {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            reasoning_effort: None,
            thinking_enabled: None,
            latency_preference: None,
            prompt_cache_retention: None,
            max_output_tokens: None,
            temperature: None,
            stop: None,
            prompt_cache_session_id: None,
            prompt_cache_lineage_id: None,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            available_mcp_tools: Vec::new(),
            memory_actions_enabled: false,
            issue_actions_enabled: true,
            interaction_kind: mez_agent::ModelInteractionKind::ActionExecution,
            allowed_actions: mez_agent::AllowedActionSet::for_capability(
                mez_agent::AgentCapability::NetworkFetch,
            ),
            messages: vec![mez_agent::ModelMessage {
                role: mez_agent::ModelMessageRole::User,
                source: mez_agent::ContextSourceKind::UserInstruction,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                content: "research provider docs".to_string(),
            }],
        },
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "fetch local file".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "fetch one provider documentation source".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![action.clone()],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![mez_agent::ActionResult::running(
            &turn,
            &action,
            vec!["network action accepted for runtime execution".to_string()],
            Some(r#"{"state":"pending_runtime_network"}"#.to_string()),
        )],
        final_turn: false,
        terminal_state: mez_agent::AgentTurnState::Running,
    };

    let execution = crate::host::async_runtime::client::execute_provider_worker_network_actions(
        &turn, execution,
    )
    .await
    .unwrap();

    assert_eq!(execution.terminal_state, mez_agent::AgentTurnState::Failed);
    let result = &execution.action_results[0];
    assert_eq!(result.status, mez_agent::ActionStatus::Failed);
    let error = result.error.as_ref().unwrap();
    assert_eq!(error.code, "unsupported_url_scheme");
    assert!(
        error
            .message
            .contains("fetch_url supports only http:// or https:// URLs"),
        "{}",
        error.message
    );
    assert!(
        result
            .structured_content_json
            .as_deref()
            .unwrap()
            .contains(r#""kind":"fetch_url""#)
    );
}

/// Verifies that configured provider task failures stay scoped to the pending
/// agent turn when the async provider service polls the runtime queue. This
/// protects the interactive prompt UX from crashing the daemon when the default
/// provider cannot run, such as a fresh OpenAI setup with no attached auth
/// store.
#[tokio::test(flavor = "current_thread")]
async fn async_agent_provider_service_keeps_running_after_prompt_provider_failure() {
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
    let start = service
        .execute_agent_shell_command(&primary, "summarize the pane")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    assert_eq!(service.pending_agent_provider_tasks().len(), 1);
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let service_handle = handle.clone();
    let timer_handle = handle.clone();
    let shutdown_handle = handle.clone();
    let service = async move {
        run_async_agent_provider_service(
            &service_handle,
            AsyncAgentProviderServiceConfig::new(1).unwrap(),
            |polls, _| polls >= 4,
        )
        .await
        .unwrap()
    };
    let timer = async move {
        run_async_runtime_timer_side_effect_service(
            &timer_handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 8,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            0,
            |polls, _| polls >= 8,
        )
        .await
        .unwrap()
    };
    let client = async move {
        let (report, timer_report) = tokio::join!(service, timer);
        let _ = shutdown_handle.shutdown().await.unwrap();
        (report, timer_report)
    };

    let ((report, timer_report), mut exit) = tokio::join!(client, actor.run());

    assert!(report.polls >= 2, "{report:?}");
    assert!(report.idle_polls >= 1, "{report:?}");
    assert_eq!(report.executions, 0);
    assert!(timer_report.fired >= 1, "{timer_report:?}");
    assert!(exit.service.pending_agent_provider_tasks().is_empty());
    assert_eq!(
        exit.service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    assert!(exit.commands_processed >= 4);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies that the async provider service wakes from runtime notifications
/// when a prompt queues provider work after an initially idle poll. This ties
/// agent prompt latency to notification delivery once the actor observes a user
/// prompt.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_agent_provider_service_wakes_when_prompt_queues_work() {
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
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let provider_handle = handle.clone();
    let provider = async move {
        run_async_agent_provider_service(
            &provider_handle,
            AsyncAgentProviderServiceConfig::new(1).unwrap(),
            |polls, _| polls >= 2,
        )
        .await
        .unwrap()
    };
    let (prompt_done_tx, prompt_done_rx) = tokio::sync::oneshot::channel();
    let prompt_handle = handle.clone();
    let prompt = async move {
        tokio::task::yield_now().await;
        let start = prompt_handle
            .execute_agent_shell_command(primary, "summarize the pane".to_string())
            .await
            .unwrap();
        assert!(start.contains(r#""state":"running""#), "{start}");
        let _ = prompt_done_tx.send(());
    };
    let watchdog = async move {
        let _ = prompt_done_rx.await;
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
        panic!("provider service did not process prompt work after actor notification");
    };
    let provider_or_watchdog = async move {
        tokio::select! {
            report = provider => report,
            _ = watchdog => unreachable!("provider wakeup watchdog panicked"),
        }
    };
    let client = async {
        let (report, ()) = tokio::join!(provider_or_watchdog, prompt);
        assert!(report.polls >= 2, "{report:?}");
        assert!(report.idle_polls >= 1, "{report:?}");
        assert_eq!(report.executions, 0);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
        report
    };

    let (report, mut exit) = tokio::join!(client, actor.run());
    assert_eq!(report.terminal_state, RuntimeLifecycleState::Running);
    assert!(exit.service.pending_agent_provider_tasks().is_empty());
    assert_eq!(
        exit.service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies that one slow provider request does not block another pane's
/// provider request from being claimed and executed.
///
/// The mock provider intentionally withholds the slow response until it has
/// observed the fast request. A serialized provider service would wait forever
/// on the first request and the test timeout would fail before the second pane
/// could reach the server.
#[tokio::test(flavor = "current_thread")]
async fn async_agent_provider_service_does_not_serialize_provider_requests_across_panes() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let fast_request_seen = StdArc::new(AtomicBool::new(false));
    let fast_request_notify = StdArc::new(tokio::sync::Notify::new());
    let request_count = StdArc::new(AtomicUsize::new(0));
    let server_seen = request_count.clone();
    let server_fast_seen = fast_request_seen.clone();
    let server_notify = fast_request_notify.clone();
    let server = tokio::spawn(async move {
        let mut handlers = Vec::new();
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request_count = server_seen.clone();
            let fast_request_seen = server_fast_seen.clone();
            let fast_request_notify = server_notify.clone();
            handlers.push(tokio::spawn(async move {
                let request = async_provider_concurrency_read_http_request(&mut stream).await;
                request_count.fetch_add(1, Ordering::SeqCst);
                if request.contains("fast-unblocked-provider") {
                    fast_request_seen.store(true, Ordering::SeqCst);
                    fast_request_notify.notify_waiters();
                    async_provider_concurrency_write_chat_response(
                        &mut stream,
                        "fast pane completed",
                    )
                    .await;
                } else if request.contains("slow-gated-provider") {
                    while !fast_request_seen.load(Ordering::SeqCst) {
                        fast_request_notify.notified().await;
                    }
                    async_provider_concurrency_write_chat_response(
                        &mut stream,
                        "slow pane completed",
                    )
                    .await;
                } else {
                    panic!("unexpected provider request: {request}");
                }
            }));
        }
        for handler in handlers {
            handler.await.unwrap();
        }
    });

    let mut service = test_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                "[agents]\n\
                 default_provider = \"local-chat\"\n\
                 default_model_profile = \"default\"\n\
                 max_concurrent_agents = 4\n\
                 \n\
                 [providers.local-chat]\n\
                 kind = \"openai-compatible\"\n\
                 base_url = \"http://{address}/v1\"\n\
                 models = [\"local-chat-model\"]\n\
                 default_model = \"local-chat-model\"\n\
                 \n\
                 [providers.local-chat.options]\n\
                 maap_output = \"structured_json\"\n\
                 structured_output = \"json_schema\"\n\
                 \n\
                 [model_profiles.default]\n\
                 provider = \"local-chat\"\n\
                 model = \"local-chat-model\"\n"
            ),
        }])
        .unwrap();
    let auth_root = std::env::temp_dir().join(format!(
        "mez-async-provider-concurrency-auth-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&auth_root);
    service.set_auth_store(crate::security::auth::AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&auth_root),
    ));
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "slow-gated-provider")
        .unwrap();
    let second_pane = service
        .split_pane_with_process(
            &primary,
            mez_mux::layout::SplitDirection::Vertical,
            Some("cat >/dev/null"),
        )
        .unwrap()
        .pane_id;
    service
        .agent_shell_store_mut()
        .enter_or_resume(second_pane.as_str())
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "fast-unblocked-provider")
        .unwrap();
    assert_eq!(service.pending_agent_provider_tasks().len(), 2);

    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let provider_handle = handle.clone();
    let client = async move {
        let pending = provider_handle
            .pending_agent_provider_tasks()
            .await
            .unwrap();
        assert_eq!(pending.len(), 2);
        let side_effects = pending
            .into_iter()
            .map(|task| RuntimeSideEffect::DispatchAgentProvider {
                agent_id: AgentId::opaque(task.agent_id).unwrap(),
                turn_id: task.turn_id,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            provider_handle
                .queue_runtime_side_effects(side_effects)
                .await
                .unwrap(),
            2
        );
        let report = tokio::time::timeout(
            Duration::from_secs(2),
            run_async_agent_provider_service(
                &provider_handle,
                AsyncAgentProviderServiceConfig::new(1)
                    .unwrap()
                    .with_idle_interval(Duration::from_millis(5))
                    .unwrap(),
                |polls, _| polls >= 8,
            ),
        )
        .await
        .expect("provider service should not block behind the slow request")
        .unwrap();
        assert_eq!(report.executions, 2, "{report:?}");
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
        report
    };

    let (report, mut exit) = tokio::join!(client, actor.run());
    assert_eq!(request_count.load(Ordering::SeqCst), 2);
    assert!(fast_request_seen.load(Ordering::SeqCst));
    assert!(exit.service.pending_agent_provider_tasks().is_empty());
    assert_eq!(exit.service.agent_scheduler().snapshot().running, 0);
    assert_eq!(report.executions, 2);
    exit.service.terminate_all_pane_processes().unwrap();
    tokio::time::timeout(Duration::from_secs(1), server)
        .await
        .unwrap()
        .unwrap();
    let _ = std::fs::remove_dir_all(auth_root);
}
