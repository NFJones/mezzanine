//! Async-runtime tests owned by agent provider behavior.

use super::super::*;

/// Verifies that async provider worker failures can finish the active agent
/// turn through typed runtime event ingress. The failed event has enough
/// identity and error information to reuse the configured provider failure
/// path, including audit, prompt display, scheduler cleanup, and pending-task
/// removal, without returning an error to the daemon supervisor.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_agent_provider_failure_events() {
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
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    let task = pending[0].clone();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Failed {
            agent_id: AgentId::opaque(task.agent_id).unwrap(),
            turn_id: task.turn_id,
            kind: "invalid_state".to_string(),
            message: "provider worker failed before response".to_string(),
            provider_failure_json: None,
            provider_raw_text: None,
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.service.pending_agent_provider_tasks().is_empty());
    assert_eq!(
        exit.service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    let pane_text = exit
        .service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("provider_error"), "{pane_text}");
    assert!(
        pane_text.contains("provider worker failed before response"),
        "{pane_text}"
    );
    assert_eq!(exit.commands_processed, 2);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that async provider worker completions can apply a model-produced
/// execution through typed runtime ingress. This keeps successful provider
/// output on the same actor-owned transcript, audit, scheduler, prompt display,
/// and pane rendering path as the compatibility provider poller while allowing
/// future workers to perform network I/O outside the actor.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_agent_provider_completion_events() {
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
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    let task = pending[0].clone();
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
        id: "say-1".to_string(),
        rationale: "complete with a visible summary".to_string(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Final,
            text: "Typed completion applied.".to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    let response_batch = mez_agent::MaapBatch {
        protocol: "maap/1".to_string(),
        rationale: "test action batch rationale".to_string(),
        thought: None,
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        actions: vec![action.clone()],
        final_turn: true,
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
                mez_agent::AgentCapability::RespondOnly,
            ),
            messages: vec![mez_agent::ModelMessage {
                role: mez_agent::ModelMessageRole::User,
                source: mez_agent::ContextSourceKind::UserInstruction,
                content: "summarize the pane".to_string(),
            }],
        },
        response: mez_agent::ModelResponse {
            provider: task.model_profile.provider.clone(),
            model: task.model_profile.model.clone(),
            raw_text: "Typed completion applied.".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(response_batch),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![mez_agent::ActionResult::succeeded(
            &turn,
            &action,
            vec!["Typed completion applied.".to_string()],
            Some(
                r#"{"kind":"say","status":"final","content_type":"text/plain; charset=utf-8","text":"Typed completion applied."}"#
                    .to_string(),
            ),
        )],
        final_turn: true,
        terminal_state: mez_agent::AgentTurnState::Completed,
    };
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
            agent_id: AgentId::opaque(task.agent_id).unwrap(),
            turn_id: task.turn_id,
            execution: Box::new(execution),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.service.pending_agent_provider_tasks().is_empty());
    assert_eq!(
        exit.service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    let pane_text = exit
        .service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("Typed completion applied."),
        "{pane_text}"
    );
    assert_eq!(exit.commands_processed, 2);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that provider completions queue durable transcript entries for the
/// persistence worker when a transcript store is configured. The actor assigns
/// transcript sequence numbers and records the pane reference immediately, while
/// filesystem writes happen only after the persistence worker drains the typed
/// transcript side effect.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_defers_agent_transcript_entries_to_persistence_worker() {
    let transcript_root = std::env::temp_dir().join(format!(
        "mez-async-transcript-defer-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&transcript_root);
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    let mut service = test_service();
    service.set_agent_transcript_store(transcript_store.clone());
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
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let transcript_path = transcript_store.transcript_path(&conversation_id).unwrap();
    let start = service
        .execute_agent_shell_command(&primary, "summarize the pane")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    let task = pending[0].clone();
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
        id: "say-1".to_string(),
        rationale: "complete with a visible summary".to_string(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Final,
            text: "Typed transcript completion.".to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    let response_batch = mez_agent::MaapBatch {
        protocol: "maap/1".to_string(),
        rationale: "test action batch rationale".to_string(),
        thought: None,
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        actions: vec![action.clone()],
        final_turn: true,
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
                mez_agent::AgentCapability::RespondOnly,
            ),
            messages: vec![mez_agent::ModelMessage {
                role: mez_agent::ModelMessageRole::User,
                source: mez_agent::ContextSourceKind::UserInstruction,
                content: "summarize the pane".to_string(),
            }],
        },
        response: mez_agent::ModelResponse {
            provider: task.model_profile.provider.clone(),
            model: task.model_profile.model.clone(),
            raw_text: "Typed transcript completion.".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(response_batch),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![mez_agent::ActionResult::succeeded(
            &turn,
            &action,
            vec!["Typed transcript completion.".to_string()],
            Some(
                r#"{"kind":"say","status":"final","content_type":"text/plain; charset=utf-8","text":"Typed transcript completion."}"#
                    .to_string(),
            ),
        )],
        final_turn: true,
        terminal_state: mez_agent::AgentTurnState::Completed,
    };
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
            agent_id: AgentId::opaque(task.agent_id).unwrap(),
            turn_id: task.turn_id,
            execution: Box::new(execution),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert!(report.side_effects >= 2);
        assert!(!transcript_path.exists());

        let persistence = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(persistence.drained, 1);
        assert_eq!(persistence.completed, 1);
        assert_eq!(persistence.failed, 0);
        assert!(persistence.bytes_written > 0);

        let entries = transcript_store.inspect(&conversation_id).unwrap();
        assert!(!entries.is_empty());
        assert!(
            entries
                .iter()
                .any(|entry| { entry.content.contains("Typed transcript completion.") })
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.service.pending_agent_provider_tasks().is_empty());
    assert!(exit.commands_processed >= 4);
    exit.service.pane_processes_mut().terminate_all().unwrap();
    let _ = std::fs::remove_dir_all(transcript_root);
}

/// Verifies that submitted agent prompts are appended to the shared prompt
/// history through the persistence worker instead of being written while actor
/// state is applying the prompt. This keeps the hot input path non-blocking
/// while preserving the global prompt-history UX across sessions.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_defers_agent_prompt_history_to_persistence_worker() {
    let transcript_root = std::env::temp_dir().join(format!(
        "mez-async-prompt-history-defer-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&transcript_root);
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    let mut service = test_service();
    service.set_agent_transcript_store(transcript_store.clone());
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
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let prompt_history_path = transcript_store.prompt_history_file();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let response = handle
            .execute_agent_shell_command(primary, "remember this prompt".to_string())
            .await
            .unwrap();
        assert!(response.contains(r#""state":"running""#), "{response}");
        assert!(!prompt_history_path.exists());
        assert!(
            transcript_store
                .prompt_history(&conversation_id)
                .unwrap()
                .is_empty()
        );

        let persistence = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(persistence.drained, 1);
        assert_eq!(persistence.completed, 1);
        assert_eq!(persistence.failed, 0);
        assert!(persistence.bytes_written > 0);

        assert_eq!(
            transcript_store.prompt_history(&conversation_id).unwrap(),
            vec![String::from("remember this prompt")]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 4);
    exit.service.pane_processes_mut().terminate_all().unwrap();
    let _ = std::fs::remove_dir_all(transcript_root);
}

/// Verifies that the async actor defers `/init` scaffold creation to the
/// persistence worker. The command path records the user-visible mutation
/// immediately, but the project instruction file is created by the async
/// persistence worker so the actor does not perform the potentially slow file
/// write inline.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_defers_agent_init_scaffold_to_persistence_worker() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-agent-init-defer-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let scaffold = root.join("AGENTS.md");
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let started = service
        .start_initial_pane_process(Some("sleep 30"))
        .unwrap();
    service
        .apply_pane_foreground_process_event(
            started.pane_id.clone(),
            "sleep",
            started.primary_pid,
            Some(root.to_string_lossy().to_string()),
        )
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let response = handle
            .execute_agent_shell_command(primary, "/init".to_string())
            .await
            .unwrap();
        assert!(response.contains(r#""kind":"mutated""#), "{response}");
        assert!(response.contains(r#""command":"init""#), "{response}");
        assert!(response.contains("created=true"), "{response}");
        assert!(
            !scaffold.exists(),
            "async actor should not create AGENTS.md before persistence worker drains"
        );

        let persistence = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(persistence.drained, 1);
        assert_eq!(persistence.completed, 1);
        assert_eq!(persistence.failed, 0);
        assert!(persistence.bytes_written > 0);

        let text = std::fs::read_to_string(&scaffold).unwrap();
        assert!(text.contains("# Repository Guidelines"), "{text}");
        assert!(
            text.contains("## Build, Test, and Development Commands"),
            "{text}"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 4);
    exit.service.pane_processes_mut().terminate_all().unwrap();
    let _ = std::fs::remove_dir_all(root);
}
