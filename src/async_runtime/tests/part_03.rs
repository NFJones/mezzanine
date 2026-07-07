/// Verifies that project-scoped approval persistence updates runtime policy
/// immediately while deferring the project config file write to the persistence
/// worker. This covers the approval workflow's config-producing path and
/// prevents actor-owned control requests from writing `.mezzanine/config.toml`
/// inline.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_defers_project_approval_config_to_persistence_worker() {
    use crate::control::{decode_control_frame, encode_control_body};

    let root = std::env::temp_dir().join(format!(
        "mez-async-project-approval-config-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let project_config = root.join(".mezzanine/config.toml");
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 10)
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
    let approval_id = service
        .queue_blocked_approval(crate::permissions::BlockedApprovalRequest {
            id: String::new(),
            requesting_agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            parent_agent_chain: vec!["agent-%1".to_string()],
            action_kind: "shell_command".to_string(),
            action_summary: "mez-test-command --flag".to_string(),
            declared_effects: vec!["unknown command effects".to_string()],
            matched_rules: vec!["default.prompt".to_string()],
            read_scopes: Vec::new(),
            write_scopes: Vec::new(),
            cooperation_mode: None,
            created_at_unix_seconds: None,
            decided_at_unix_seconds: None,
            decided_by_client_id: None,
            state: crate::permissions::BlockedApprovalState::Pending,
            decision: None,
            redirect_instruction: None,
        })
        .unwrap();
    let deny_id = service
        .queue_blocked_approval(crate::permissions::BlockedApprovalRequest {
            id: String::new(),
            requesting_agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            parent_agent_chain: vec!["agent-%1".to_string()],
            action_kind: "shell_command".to_string(),
            action_summary: "mez-test-command --delete".to_string(),
            declared_effects: vec!["unknown command effects".to_string()],
            matched_rules: vec!["default.prompt".to_string()],
            read_scopes: Vec::new(),
            write_scopes: Vec::new(),
            cooperation_mode: None,
            created_at_unix_seconds: None,
            decided_at_unix_seconds: None,
            decided_by_client_id: None,
            state: crate::permissions::BlockedApprovalState::Pending,
            decision: None,
            redirect_instruction: None,
        })
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let input = encode_control_body(&format!(
            r#"{{"jsonrpc":"2.0","id":"allow-project","method":"approval/decide","params":{{"approval_id":"{}","decision":"approve","scope":{{"persistence":"project"}},"idempotency_key":"allow-project"}}}}"#,
            approval_id
        ));
        let result = handle
            .handle_control_input_for_connection(
                input,
                4096,
                ControlConnectionState::trusted_existing_client(primary.clone()),
            )
            .await
            .unwrap();
        let (body, _) = decode_control_frame(&result.output, 4096).unwrap();
        assert!(body.contains(r#""state":"approved""#), "{body}");
        let input = encode_control_body(&format!(
            r#"{{"jsonrpc":"2.0","id":"deny-project","method":"approval/decide","params":{{"approval_id":"{}","decision":"disapprove","scope":{{"persistence":"project"}},"idempotency_key":"deny-project"}}}}"#,
            deny_id
        ));
        let result = handle
            .handle_control_input_for_connection(
                input,
                4096,
                ControlConnectionState::trusted_existing_client(primary),
            )
            .await
            .unwrap();
        let (body, _) = decode_control_frame(&result.output, 4096).unwrap();
        assert!(body.contains(r#""state":"disapproved""#), "{body}");
        assert!(
            !project_config.exists(),
            "async actor should not write project config before persistence worker drains"
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
        assert_eq!(persistence.drained, 2);
        assert_eq!(persistence.completed, 2);
        assert_eq!(persistence.failed, 0);
        assert!(persistence.bytes_written > 0);

        let config_text = std::fs::read_to_string(&project_config).unwrap();
        assert!(config_text.contains(r#"approval_policy = "ask""#));
        assert!(config_text.contains(r#"match = "exact_sha256""#));
        assert!(config_text.contains(r#"decision = "allow""#));
        assert!(config_text.contains(r#"decision = "deny""#));
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert_eq!(
        exit.service
            .permission_policy()
            .evaluate_shell_command("mez-test-command --flag"),
        crate::permissions::RuleDecision::Allow
    );
    assert_eq!(
        exit.service
            .permission_policy()
            .evaluate_shell_command("mez-test-command --delete"),
        crate::permissions::RuleDecision::Forbid
    );
    assert!(exit.commands_processed >= 4);
    exit.service.pane_processes_mut().terminate_all().unwrap();
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that actor-owned control dispatch can route snapshot requests
/// through a configured repository. The async daemon control service uses this
/// path when serving live `mez snapshot` requests, so `snapshot/list` must not
/// fail with the repository-missing error that applies to generic control
/// dispatch.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_handles_control_requests_with_snapshot_repository() {
    use crate::control::{decode_control_frame, encode_control_body};
    use crate::snapshot::SnapshotRepository;

    let root = std::env::temp_dir().join(format!(
        "mez-async-control-snapshots-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let snapshots = SnapshotRepository::new(root.join("snapshots"));
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 10)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut connection = ControlConnectionState::trusted_existing_client(primary.clone());
        let input = encode_control_body(
            r#"{"jsonrpc":"2.0","id":"snapshot-list","method":"snapshot/list","params":{}}"#,
        );
        let result = handle
            .handle_control_input_for_connection_with_snapshots(
                input,
                4096,
                connection,
                snapshots.clone(),
            )
            .await
            .unwrap();
        connection = result.connection;
        let (body, consumed) = decode_control_frame(&result.output, 4096).unwrap();
        assert_eq!(consumed, result.output.len());
        assert!(body.contains(r#""id":"snapshot-list""#), "{body}");
        assert!(body.contains(r#""snapshots":[]"#), "{body}");
        assert!(!body.contains("runtime snapshot repository is not configured"));

        let input = encode_control_body(
            r#"{"jsonrpc":"2.0","id":"snapshot-create","method":"snapshot/create","params":{"target":{"default":true},"name":"manual","idempotency_key":"async-snapshot-create"}}"#,
        );
        let result = handle
            .handle_control_input_for_connection_with_snapshots(
                input,
                4096,
                connection,
                snapshots.clone(),
            )
            .await
            .unwrap();
        connection = result.connection;
        let (body, _) = decode_control_frame(&result.output, 4096).unwrap();
        assert!(body.contains(r#""id":"snapshot-create""#), "{body}");
        assert!(body.contains(r#""name":"manual""#), "{body}");
        let response: serde_json::Value = serde_json::from_str(&body).unwrap();
        let snapshot_id = response["result"]["snapshot"]["id"]
            .as_str()
            .unwrap()
            .to_string();

        let input = encode_control_body(&format!(
            r#"{{"jsonrpc":"2.0","id":"snapshot-resume","method":"snapshot/resume","params":{{"snapshot_id":"{}","idempotency_key":"async-snapshot-resume"}}}}"#,
            snapshot_id
        ));
        let result = handle
            .handle_control_input_for_connection_with_snapshots(
                input,
                4096,
                connection,
                snapshots.clone(),
            )
            .await
            .unwrap();
        connection = result.connection;
        let (body, _) = decode_control_frame(&result.output, 4096).unwrap();
        assert!(body.contains(r#""id":"snapshot-resume""#), "{body}");
        assert!(body.contains(r#""resumed":true"#), "{body}");
        assert!(body.contains(r#""primary_client_id""#), "{body}");

        let input = encode_control_body(&format!(
            r#"{{"jsonrpc":"2.0","id":"snapshot-delete","method":"snapshot/delete","params":{{"snapshot_id":"{}","idempotency_key":"async-snapshot-delete"}}}}"#,
            snapshot_id
        ));
        let result = handle
            .handle_control_input_for_connection_with_snapshots(input, 4096, connection, snapshots)
            .await
            .unwrap();
        let (body, _) = decode_control_frame(&result.output, 4096).unwrap();
        assert!(body.contains(r#""id":"snapshot-delete""#), "{body}");
        assert!(body.contains(r#""deleted":true"#), "{body}");
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(
        exit.commands_processed >= 8,
        "snapshot control should use a request plus completion actor command per operation: {:?}",
        exit.commands_processed
    );
    exit.service.pane_processes_mut().terminate_all().unwrap();
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that runtime `config/set` requests which target the user-private
/// config file update actor-owned runtime configuration immediately while
/// deferring the actual file replacement to the async persistence worker. This
/// prevents actor-owned control requests from performing inline config writes
/// while preserving the user-visible live reload semantics of the command.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_defers_user_config_mutation_to_persistence_worker() {
    use crate::control::{decode_control_frame, encode_control_body};

    let root = std::env::temp_dir().join(format!(
        "mez-async-user-config-mutation-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let config_root = root.join("config");
    let config_path = config_root.join("config.toml");
    std::fs::create_dir_all(&config_root).unwrap();
    std::fs::write(&config_path, "[history]\nlines = 10\n").unwrap();

    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 10)
        .unwrap();
    service.set_config_root(config_root);
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: Some(config_path.clone()),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: std::fs::read_to_string(&config_path).unwrap(),
        }])
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let config_path_json = serde_json::to_string(&config_path.to_string_lossy()).unwrap();
        let input = encode_control_body(&format!(
            r#"{{"jsonrpc":"2.0","id":"user-config-set","method":"config/set","params":{{"path":"history.lines","value":7,"persist":{{"scope":"user","path":{config_path_json}}},"idempotency_key":"user-config-set"}}}}"#
        ));
        let result = handle
            .handle_control_input_for_connection(
                input,
                4096,
                ControlConnectionState::trusted_existing_client(primary),
            )
            .await
            .unwrap();
        let (body, _) = decode_control_frame(&result.output, 4096).unwrap();
        assert!(body.contains(r#""applied":true"#), "{body}");
        assert!(body.contains(r#""persisted":true"#), "{body}");
        assert!(
            std::fs::read_to_string(&config_path)
                .unwrap()
                .contains("lines = 10"),
            "async actor should not replace the user config file before the persistence worker drains"
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
        assert!(
            std::fs::read_to_string(&config_path)
                .unwrap()
                .contains("lines = 7")
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.service.terminal_history_limit(), 7);
    assert!(exit.commands_processed >= 3);
    let _ = std::fs::remove_dir_all(root);
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
    let turn = crate::agent::AgentTurnRecord {
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        pane_id: task.pane_id.clone(),
        trigger: crate::agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 1,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        state: crate::agent::AgentTurnState::Running,
        cooperation_mode: None,
    
        initial_capability: None,};
    let action = crate::agent::AgentAction {
        id: "shell-1".to_string(),
        rationale: "run a short shell command for the user".to_string(),
        payload: crate::agent::AgentActionPayload::ShellCommand {
            summary: "Print a shell marker.".to_string(),
            command: "printf 'async timer shell\\n'".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: Some(60_000),
        },
    };
    let response_batch = crate::agent::MaapBatch {
        protocol: "maap/1".to_string(),
        rationale: "test action batch rationale".to_string(),
        thought: None,
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        actions: vec![action.clone()],
        final_turn: false,
    };
    let execution = crate::agent::AgentTurnExecution {
        request: crate::agent::ModelRequest {
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
            interaction_kind: crate::agent::ModelInteractionKind::ActionExecution,
            allowed_actions: crate::agent::AllowedActionSet::for_capability(
                crate::agent::AgentCapability::Shell,
            ),
            messages: vec![crate::agent::ModelMessage {
                role: crate::agent::ModelMessageRole::User,
                source: crate::agent::ContextSourceKind::UserInstruction,
                content: "print a marker".to_string(),
            }],
        },
        response: crate::agent::ModelResponse {
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
        action_results: vec![crate::agent::ActionResult::running(
            &turn,
            &action,
            vec!["shell command accepted for pane execution".to_string()],
            Some(r#"{"state":"pending_dispatch"}"#.to_string()),
        )],
        final_turn: false,
        terminal_state: crate::agent::AgentTurnState::Running,
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
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that the async-owned pane path keeps the pane shell alive after the
/// first agent shell command dispatch. This covers the production daemon shape:
/// a real PTY shell is claimed by the Tokio pane worker, a provider completion
/// queues a shell action, and a later pane input still reaches the same shell
/// instead of observing a process exit or supervisor shutdown.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_worker_keeps_shell_alive_after_first_agent_command() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", crate::agent::AgentLogLevel::Verbose)
        .unwrap();
    service.permission_policy_mut().set_approval_bypass(true);

    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let pane_worker_handle = handle.clone();
    let client_handle = handle.clone();
    let pane_worker_done = StdArc::new(AtomicBool::new(false));
    let pane_worker_stop = StdArc::clone(&pane_worker_done);
    let (pane_worker_stopped_tx, pane_worker_stopped_rx) = tokio::sync::oneshot::channel();

    let pane_worker = async move {
        let report = run_async_pane_process_supervisor_service(
            pane_worker_handle,
            AsyncPaneProcessSupervisorServiceConfig {
                max_polls: u64::MAX,
                take_limit: 8,
                idle_interval: Duration::from_millis(1),
                pane_service: AsyncPaneProcessServiceConfig {
                    max_polls: u64::MAX,
                    output_drain_limit: 1,
                    drain_limit: 8,
                    idle_interval: Duration::from_millis(1),
                    foreground_metadata_interval: Duration::from_secs(60),
                },
            },
            move |_, state| {
                pane_worker_stop.load(Ordering::SeqCst)
                    || matches!(state, RuntimeLifecycleState::Stopping)
            },
        )
        .await
        .unwrap();
        let _ = pane_worker_stopped_tx.send(());
        report
    };

    let client = async move {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        let start = client_handle
            .execute_agent_shell_command(primary.clone(), "print a marker".to_string())
            .await
            .unwrap();
        assert!(start.contains(r#""state":"running""#), "{start}");
        let task = client_handle
            .pending_agent_provider_tasks()
            .await
            .unwrap()
            .into_iter()
            .find(|task| task.turn_id == "turn-1")
            .expect("agent prompt should queue turn-1 provider task");
        let turn = crate::agent::AgentTurnRecord {
            turn_id: task.turn_id.clone(),
            agent_id: task.agent_id.clone(),
            pane_id: task.pane_id.clone(),
            trigger: crate::agent::AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 1,
            policy_profile: "default".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            state: crate::agent::AgentTurnState::Running,
            cooperation_mode: None,
        
            initial_capability: None,};
        let action = crate::agent::AgentAction {
            id: "shell-1".to_string(),
            rationale: "print a marker".to_string(),
            payload: crate::agent::AgentActionPayload::ShellCommand {
                summary: "Print a marker".to_string(),
                command: "printf 'AGENT_ASYNC_FIRST_COMMAND\\n'".to_string(),
                interactive: false,
                stateful: false,
                timeout_ms: Some(60_000),
            },
        };
        let batch = crate::agent::MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "test action batch rationale".to_string(),
            thought: None,
            turn_id: task.turn_id.clone(),
            agent_id: task.agent_id.clone(),
            actions: vec![action.clone()],
            final_turn: false,
        };
        let execution = crate::agent::AgentTurnExecution {
            request: crate::agent::ModelRequest {
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
                interaction_kind: crate::agent::ModelInteractionKind::ActionExecution,
                allowed_actions: crate::agent::AllowedActionSet::for_capability(
                    crate::agent::AgentCapability::Shell,
                ),
                messages: vec![crate::agent::ModelMessage {
                    role: crate::agent::ModelMessageRole::User,
                    source: crate::agent::ContextSourceKind::UserInstruction,
                    content: "print a marker".to_string(),
                }],
            },
            response: crate::agent::ModelResponse {
                provider: task.model_profile.provider.clone(),
                model: task.model_profile.model.clone(),
                raw_text: "shell command response".to_string(),
                usage: Default::default(),
            latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(batch),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![crate::agent::ActionResult::running(
                &turn,
                &action,
                vec!["shell command accepted for pane execution".to_string()],
                Some(r#"{"state":"pending_dispatch"}"#.to_string()),
            )],
            final_turn: false,
            terminal_state: crate::agent::AgentTurnState::Running,
        };
        let mut provider_batch = RuntimeEventBatch::new();
        provider_batch.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
            agent_id: AgentId::opaque(task.agent_id).unwrap(),
            turn_id: task.turn_id.clone(),
            execution: Box::new(execution),
        }));
        let provider_report = client_handle
            .submit_runtime_events(provider_batch)
            .await
            .unwrap();
        assert_eq!(provider_report.accepted, 1);
        assert_eq!(provider_report.applied, 1);

        let first_seen = wait_for_rendered_text(
            &client_handle,
            ClientViewRole::Primary,
            "AGENT_ASYNC_FIRST_COMMAND",
        )
        .await
        .unwrap();
        assert!(
            first_seen.contains("AGENT_ASYNC_FIRST_COMMAND"),
            "{first_seen}"
        );
        wait_for_shell_transaction_timer_settlement(&client_handle, "first")
            .await
            .unwrap();

        let mut next_task = None;
        for _ in 0..200 {
            if let Some(task) = client_handle
                .pending_agent_provider_tasks()
                .await
                .unwrap()
                .into_iter()
                .find(|pending| pending.turn_id == "turn-1")
            {
                next_task = Some(task);
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let next_task =
            next_task.expect("first shell transaction should queue provider continuation");
        let ready_again = client_handle
            .execute_terminal_command(
                primary.clone(),
                "mark-pane-ready --acknowledge-risk --reason async-agent-test-second-command"
                    .to_string(),
            )
            .await
            .unwrap();
        assert!(ready_again.contains("override=applied"), "{ready_again}");
        let second_turn = crate::agent::AgentTurnRecord {
            turn_id: next_task.turn_id.clone(),
            agent_id: next_task.agent_id.clone(),
            pane_id: next_task.pane_id.clone(),
            trigger: crate::agent::AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 2,
            policy_profile: "default".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            state: crate::agent::AgentTurnState::Running,
            cooperation_mode: None,
        
            initial_capability: None,};
        let second_action = crate::agent::AgentAction {
            id: "shell-2".to_string(),
            rationale: "verify the pane shell still accepts input".to_string(),
            payload: crate::agent::AgentActionPayload::ShellCommand {
                summary: "Print a second marker".to_string(),
                command: "printf 'ASYNC_PANE_STILL_ALIVE\\n'".to_string(),
                interactive: false,
                stateful: false,
                timeout_ms: Some(60_000),
            },
        };
        let second_batch = crate::agent::MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "test action batch rationale".to_string(),
            thought: None,
            turn_id: next_task.turn_id.clone(),
            agent_id: next_task.agent_id.clone(),
            actions: vec![second_action.clone()],
            final_turn: false,
        };
        let second_execution = crate::agent::AgentTurnExecution {
            request: crate::agent::ModelRequest {
                provider: next_task.model_profile.provider.clone(),
                model: next_task.model_profile.model.clone(),
                reasoning_effort: next_task
                    .model_profile
                    .provider_options
                    .get("reasoning_effort")
                    .cloned()
                    .or_else(|| next_task.model_profile.reasoning_profile.clone()),
                thinking_enabled: next_task.model_profile.thinking_enabled(),
                latency_preference: next_task.model_profile.latency_preference.clone(),
                prompt_cache_retention: next_task
                    .model_profile
                    .provider_options
                    .get("prompt_cache_retention")
                    .cloned(),
                max_output_tokens: next_task.model_profile.max_output_tokens(),
                temperature: None,
                stop: None,
                prompt_cache_session_id: None,
                prompt_cache_lineage_id: None,
                turn_id: next_task.turn_id.clone(),
                agent_id: next_task.agent_id.clone(),
                available_mcp_tools: Vec::new(),
                memory_actions_enabled: false,
                issue_actions_enabled: true,
                interaction_kind: crate::agent::ModelInteractionKind::ActionExecution,
                allowed_actions: crate::agent::AllowedActionSet::for_capability(
                    crate::agent::AgentCapability::Shell,
                ),
                messages: vec![crate::agent::ModelMessage {
                    role: crate::agent::ModelMessageRole::User,
                    source: crate::agent::ContextSourceKind::UserInstruction,
                    content: "print a second marker".to_string(),
                }],
            },
            response: crate::agent::ModelResponse {
                provider: next_task.model_profile.provider.clone(),
                model: next_task.model_profile.model.clone(),
                raw_text: "second shell command response".to_string(),
                usage: Default::default(),
            latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(second_batch),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![crate::agent::ActionResult::running(
                &second_turn,
                &second_action,
                vec!["second shell command accepted for pane execution".to_string()],
                Some(r#"{"state":"pending_dispatch"}"#.to_string()),
            )],
            final_turn: false,
            terminal_state: crate::agent::AgentTurnState::Running,
        };
        let mut second_provider_batch = RuntimeEventBatch::new();
        second_provider_batch.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
            agent_id: AgentId::opaque(next_task.agent_id).unwrap(),
            turn_id: next_task.turn_id,
            execution: Box::new(second_execution),
        }));
        let second_provider_report = client_handle
            .submit_runtime_events(second_provider_batch)
            .await
            .unwrap();
        assert_eq!(second_provider_report.accepted, 1);
        assert_eq!(second_provider_report.applied, 1);
        wait_for_shell_transaction_timer_settlement(&client_handle, "second")
            .await
            .unwrap();
        let alive_seen = wait_for_rendered_text(
            &client_handle,
            ClientViewRole::Primary,
            "ASYNC_PANE_STILL_ALIVE",
        )
        .await
        .unwrap();
        assert!(
            alive_seen.contains("ASYNC_PANE_STILL_ALIVE"),
            "{alive_seen}"
        );
        assert_eq!(
            client_handle.lifecycle_state().await.unwrap(),
            RuntimeLifecycleState::Running
        );
        pane_worker_done.store(true, Ordering::SeqCst);
        pane_worker_stopped_rx
            .await
            .expect("pane worker should stop before actor shutdown");
        assert_eq!(
            client_handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), supervisor_report, mut actor_exit) = tokio::time::timeout(
        Duration::from_secs(30),
        async { tokio::join!(client, pane_worker, actor.run()) },
    )
    .await
    .expect("async pane worker shell liveness test should not hang indefinitely");
    assert_eq!(
        actor_exit.service.lifecycle_state(),
        RuntimeLifecycleState::Running
    );
    assert!(supervisor_report.spawned_workers >= 1);
    assert_eq!(
        supervisor_report.terminal_state,
        RuntimeLifecycleState::Running
    );
    actor_exit
        .service
        .pane_processes_mut()
        .terminate_all()
        .unwrap();
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
        .set_log_level("%1", crate::agent::AgentLogLevel::Trace)
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
    let turn = crate::agent::AgentTurnRecord {
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        pane_id: task.pane_id.clone(),
        trigger: crate::agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 1,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        state: crate::agent::AgentTurnState::Running,
        cooperation_mode: None,
    
        initial_capability: None,};
    let action = crate::agent::AgentAction {
        id: "shell-1".to_string(),
        rationale: "list files".to_string(),
        payload: crate::agent::AgentActionPayload::ShellCommand {
            summary: "List files in the current directory".to_string(),
            command: "ls".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: Some(60_000),
        },
    };
    let response_batch = crate::agent::MaapBatch {
        protocol: "maap/1".to_string(),
        rationale: "test action batch rationale".to_string(),
        thought: None,
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        actions: vec![action.clone()],
        final_turn: false,
    };
    let execution = crate::agent::AgentTurnExecution {
        request: crate::agent::ModelRequest {
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
            interaction_kind: crate::agent::ModelInteractionKind::ActionExecution,
            allowed_actions: crate::agent::AllowedActionSet::for_capability(
                crate::agent::AgentCapability::Shell,
            ),
            messages: vec![crate::agent::ModelMessage {
                role: crate::agent::ModelMessageRole::User,
                source: crate::agent::ContextSourceKind::UserInstruction,
                content: "list files".to_string(),
            }],
        },
        response: crate::agent::ModelResponse {
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
        action_results: vec![crate::agent::ActionResult::running(
            &turn,
            &action,
            vec!["shell command accepted for pane execution".to_string()],
            Some(r#"{"state":"pending_dispatch"}"#.to_string()),
        )],
        final_turn: false,
        terminal_state: crate::agent::AgentTurnState::Running,
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
    let turn = crate::agent::AgentTurnRecord {
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        pane_id: task.pane_id.clone(),
        trigger: crate::agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 1,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        state: crate::agent::AgentTurnState::Running,
        cooperation_mode: None,
    
        initial_capability: None,};
    let batch_action = crate::agent::AgentAction {
        id: "fetch-listed".to_string(),
        rationale: "fetch the listed source".to_string(),
        payload: crate::agent::AgentActionPayload::FetchUrl {
            url: "https://example.com/listed".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let missing_action = crate::agent::AgentAction {
        id: "fetch-missing-result".to_string(),
        rationale: "this result no longer has a matching batch action".to_string(),
        payload: crate::agent::AgentActionPayload::FetchUrl {
            url: "https://example.com/missing".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let response_batch = crate::agent::MaapBatch {
        protocol: "maap/1".to_string(),
        rationale: "test action batch rationale".to_string(),
        thought: None,
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        actions: vec![batch_action],
        final_turn: false,
    };
    let execution = crate::agent::AgentTurnExecution {
        request: crate::agent::ModelRequest {
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
            interaction_kind: crate::agent::ModelInteractionKind::ActionExecution,
            allowed_actions: crate::agent::AllowedActionSet::for_capability(
                crate::agent::AgentCapability::NetworkFetch,
            ),
            messages: vec![crate::agent::ModelMessage {
                role: crate::agent::ModelMessageRole::User,
                source: crate::agent::ContextSourceKind::UserInstruction,
                content: "research patch behavior".to_string(),
            }],
        },
        response: crate::agent::ModelResponse {
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
        action_results: vec![crate::agent::ActionResult::running(
            &turn,
            &missing_action,
            vec!["network action accepted".to_string()],
            Some(r#"{"state":"pending_network"}"#.to_string()),
        )],
        final_turn: false,
        terminal_state: crate::agent::AgentTurnState::Running,
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
    actor_exit
        .service
        .pane_processes_mut()
        .terminate_all()
        .unwrap();
}

/// Verifies provider workers settle runtime-owned network actions pre-ingress.
///
/// Large research turns should not submit `fetch_url` actions back to the
/// single-owner runtime actor while they are still marked running. This covers
/// the async worker boundary directly with an unsupported URL, which exercises
/// the network executor without performing external HTTP during the test.
#[tokio::test(flavor = "current_thread")]
async fn async_provider_worker_executes_network_actions_before_actor_completion() {
    let turn = crate::agent::AgentTurnRecord {
        turn_id: "turn-network-worker".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        trigger: crate::agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 1,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        state: crate::agent::AgentTurnState::Running,
        cooperation_mode: None,
    
        initial_capability: None,};
    let action = crate::agent::AgentAction {
        id: "fetch-local-file".to_string(),
        rationale: "try a local file URL".to_string(),
        payload: crate::agent::AgentActionPayload::FetchUrl {
            url: "file:///tmp/provider-doc.md".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let execution = crate::agent::AgentTurnExecution {
        request: crate::agent::ModelRequest {
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
            interaction_kind: crate::agent::ModelInteractionKind::ActionExecution,
            allowed_actions: crate::agent::AllowedActionSet::for_capability(
                crate::agent::AgentCapability::NetworkFetch,
            ),
            messages: vec![crate::agent::ModelMessage {
                role: crate::agent::ModelMessageRole::User,
                source: crate::agent::ContextSourceKind::UserInstruction,
                content: "research provider docs".to_string(),
            }],
        },
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "fetch local file".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
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
        action_results: vec![crate::agent::ActionResult::running(
            &turn,
            &action,
            vec!["network action accepted for runtime execution".to_string()],
            Some(r#"{"state":"pending_runtime_network"}"#.to_string()),
        )],
        final_turn: false,
        terminal_state: crate::agent::AgentTurnState::Running,
    };

    let execution = super::client::execute_provider_worker_network_actions(&turn, execution)
        .await
        .unwrap();

    assert_eq!(
        execution.terminal_state,
        crate::agent::AgentTurnState::Failed
    );
    let result = &execution.action_results[0];
    assert_eq!(result.status, crate::agent::ActionStatus::Failed);
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

/// Waits for rendered primary-client text to contain a target string and
/// returns the most recent rendered text for assertions.
async fn wait_for_rendered_text(
    handle: &super::AsyncRuntimeSessionHandle,
    role: ClientViewRole,
    needle: &str,
) -> Result<String> {
    let mut last_text = String::new();
    for _ in 0..1000 {
        if let Some(view) = handle
            .render_client_view(
                role,
                Size::new(80, 24).unwrap(),
                TerminalClientLoopConfig::default(),
            )
            .await?
        {
            last_text = view.lines.join("\n");
            if last_text.contains(needle) {
                return Ok(last_text);
            }
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    Err(MezError::invalid_state(format!(
        "timed out waiting for rendered text {needle:?}; last render: {last_text}"
    )))
}

/// Waits until one agent shell transaction timer has been both scheduled and
/// cancelled, proving the matching runtime transaction settled.
async fn wait_for_shell_transaction_timer_settlement(
    handle: &super::AsyncRuntimeSessionHandle,
    label: &str,
) -> Result<()> {
    let mut scheduled_key = None;
    let mut cancelled_keys = Vec::new();
    for _ in 0..3000 {
        let timer_effects = handle.drain_timer_side_effects(16).await?;
        for effect in timer_effects {
            match effect {
                RuntimeSideEffect::ScheduleTimer { key, .. }
                    if key.kind == RuntimeTimerKind::ShellTransaction
                        && scheduled_key.is_none() =>
                {
                    if cancelled_keys.iter().any(|cancelled| cancelled == &key) {
                        return Ok(());
                    }
                    scheduled_key = Some(key);
                }
                RuntimeSideEffect::CancelTimer { key }
                    if key.kind == RuntimeTimerKind::ShellTransaction =>
                {
                    if scheduled_key.is_none() {
                        return Ok(());
                    }
                    if scheduled_key
                        .as_ref()
                        .is_some_and(|scheduled| scheduled == &key)
                    {
                        return Ok(());
                    }
                    cancelled_keys.push(key);
                }
                _ => {}
            }
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    Err(MezError::invalid_state(format!(
        "{label} shell transaction timer should settle before the test continues"
    )))
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
    exit.service.pane_processes_mut().terminate_all().unwrap();
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
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Reads one HTTP request from the local provider concurrency fixture.
///
/// The helper waits for the full body described by `Content-Length` so the
/// fixture can distinguish the intentionally slow and fast prompt requests by
/// their serialized model context.
async fn async_provider_concurrency_read_http_request(
    stream: &mut tokio::net::TcpStream,
) -> String {
    use tokio::io::AsyncReadExt;

    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let read = stream.read(&mut buffer).await.unwrap();
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
        if let Some(header_end) = request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|index| index + 4)
        {
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find_map(|(name, value)| {
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if request.len() >= header_end.saturating_add(content_length) {
                break;
            }
        }
    }
    String::from_utf8_lossy(&request).to_string()
}

/// Writes one OpenAI-compatible Chat Completions response for the provider
/// concurrency fixture.
///
/// The response uses structured MAAP JSON content so the runtime can complete
/// the turn without invoking privileged local actions.
async fn async_provider_concurrency_write_chat_response(
    stream: &mut tokio::net::TcpStream,
    text: &str,
) {
    use tokio::io::AsyncWriteExt;

    let content = serde_json::json!({
        "rationale": "provider concurrency fixture completed the turn",
        "thought": null,
        "actions": [
            {
                "type": "say",
                "status": "final",
                "content_type": "text/plain; charset=utf-8",
                "text": text
            }
        ]
    })
    .to_string();
    let body = serde_json::json!({
        "model": "local-chat-model",
        "choices": [
            {
                "message": {
                    "role": "assistant",
                    "content": content,
                    "tool_calls": []
                }
            }
        ],
        "usage": {
            "prompt_tokens": 3,
            "completion_tokens": 2
        }
    })
    .to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).await.unwrap();
    stream.flush().await.unwrap();
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
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap();
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
    service.set_auth_store(crate::auth::AuthStore::new(
        crate::auth::AuthPaths::under_config_root(&auth_root),
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
            crate::layout::SplitDirection::Vertical,
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

    let (handle, actor) =
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();
    let provider_handle = handle.clone();
    let client = async move {
        let pending = provider_handle.pending_agent_provider_tasks().await.unwrap();
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
    exit.service.pane_processes_mut().terminate_all().unwrap();
    tokio::time::timeout(Duration::from_secs(1), server)
        .await
        .unwrap()
        .unwrap();
    let _ = std::fs::remove_dir_all(auth_root);
}

/// Verifies async actor serializes lifecycle render and shutdown.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_serializes_lifecycle_render_and_shutdown() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();

    let client = async {
        assert_eq!(
            handle.lifecycle_state().await.unwrap(),
            RuntimeLifecycleState::Running
        );
        let view = handle
            .render_client_view(
                ClientViewRole::Primary,
                Size::new(80, 24).unwrap(),
                TerminalClientLoopConfig::default(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(view.authoritative_size, Size::new(80, 24).unwrap());
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert_eq!(exit.commands_processed, 3);
}

/// Verifies that bootstrap dispatch is driven by typed pane-output events
/// instead of a later compatibility tick. A prompt marker makes the pending
/// pane bootstrap-ready; the actor should immediately enqueue the hidden
/// bootstrap wrapper for the async pane worker and schedule its timeout timer.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_dispatches_bootstrap_after_prompt_ready_output_event() {
    let mut service = test_service();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut processes = handle
            .take_running_pane_processes_for_async_owner(8)
            .await
            .unwrap();
        assert_eq!(processes.len(), 1);
        let (pane_id, mut process) = processes.pop().unwrap();
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: pane_id.clone(),
            bytes: b"\x1b]133;A\x1b\\".to_vec(),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);

        let pane_effects = handle
            .drain_pane_io_side_effects(pane_id.as_str(), 8)
            .await
            .unwrap();
        let bootstrap_bytes = match pane_effects.as_slice() {
            [
                RuntimeSideEffect::WritePaneInput {
                    pane_id: effect_pane,
                    bytes,
                },
            ] if effect_pane == &pane_id => bytes,
            effects => panic!("expected one bootstrap pane write, got {effects:?}"),
        };
        let bootstrap_wrapper = std::str::from_utf8(bootstrap_bytes).unwrap();
        assert!(
            bootstrap_wrapper.contains("MEZ_COMMAND_B64") && bootstrap_wrapper.contains("base64"),
            "bootstrap wrapper should be queued for the pane worker"
        );

        let timer_effects = handle.drain_timer_side_effects(8).await.unwrap();
        assert!(
            timer_effects.iter().any(|effect| matches!(
                effect,
                RuntimeSideEffect::ScheduleTimer { key, .. }
                    if key.kind == RuntimeTimerKind::Bootstrap
            )),
            "bootstrap timeout should be scheduled by actor timer side effects: {timer_effects:?}"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
        let _ = process.terminate(Duration::from_millis(10));
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.service.pane_processes_mut().terminate_all().is_ok());
}

/// Verifies async attached terminal step uses runtime rendered view.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_step_uses_runtime_rendered_view() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();

    let client = async {
        let readiness = vec![
            AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Input,
                fd: 0,
                interest: TerminalFdInterest::read(),
                readable: true,
                writable: false,
                hangup: false,
                error: false,
            },
            AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Output,
                fd: 1,
                interest: TerminalFdInterest::write(),
                readable: false,
                writable: true,
                hangup: false,
                error: false,
            },
        ];
        let status = ClientStatusLine {
            kind: ClientStatusKind::Plain,
            text: "attached".to_string(),
        };
        let plan = plan_async_attached_terminal_client_step(
            &handle,
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            TerminalClientLoopConfig::default(),
            &readiness,
            Some(b"\x01\""),
            Some(&status),
        )
        .await
        .unwrap();

        assert_eq!(
            plan.actions,
            vec![crate::terminal::TerminalClientLoopAction::ExecuteMux(
                MuxAction::SplitPaneHorizontal
            )]
        );
        assert_eq!(plan.output_lines.len(), 24);
        assert_eq!(plan.output_lines[23].trim_end(), "attached");
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert_eq!(exit.commands_processed, 2);
}

/// Verifies async attached terminal step can be applied through actor.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_step_can_be_applied_through_actor() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let readiness = vec![AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Input,
            fd: 0,
            interest: TerminalFdInterest::read(),
            readable: true,
            writable: false,
            hangup: false,
            error: false,
        }];
        let (_plan, application) = plan_and_apply_async_attached_terminal_client_step(
            &handle,
            AsyncAttachedTerminalStepRequest {
                primary_client_id: primary.clone(),
                role: ClientViewRole::Primary,
                client_size: Size::new(80, 24).unwrap(),
                config: TerminalClientLoopConfig::default(),
                readiness: &readiness,
                input: Some(b"hello\n"),
                status: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(application.forwarded_bytes, 6);
        assert_eq!(
            handle.drain_pane_io_side_effects("%1", 8).await.unwrap(),
            vec![RuntimeSideEffect::WritePaneInput {
                pane_id: "%1".to_string(),
                bytes: b"hello\n".to_vec(),
            }]
        );
        let large_input = vec![b'x'; 468_586];
        let (_plan, application) = plan_and_apply_async_attached_terminal_client_step(
            &handle,
            AsyncAttachedTerminalStepRequest {
                primary_client_id: primary.clone(),
                role: ClientViewRole::Primary,
                client_size: Size::new(80, 24).unwrap(),
                config: TerminalClientLoopConfig::default(),
                readiness: &readiness,
                input: Some(&large_input),
                status: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(application.forwarded_bytes, large_input.len());
        assert_eq!(
            handle
                .drain_pane_io_side_effects("%1", usize::MAX)
                .await
                .unwrap(),
            vec![RuntimeSideEffect::WritePaneInput {
                pane_id: "%1".to_string(),
                bytes: large_input,
            }]
        );
        let split = AttachedTerminalClientStepPlan {
            actions: vec![TerminalClientLoopAction::ExecuteMux(
                MuxAction::SplitPaneVertical,
            )],
            output_lines: Vec::new(),
            output_line_style_spans: Vec::new(),
            input_hangup: false,
            output_hangup: false,
            error_roles: Vec::new(),
        };
        let split_application = handle
            .apply_attached_terminal_step_plan(primary, split)
            .await
            .unwrap();
        assert_eq!(split_application.mux_actions_applied, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());

    assert!(
        exit.commands_processed >= 5,
        "actor should process client-step, drain, split, and shutdown requests"
    );
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that pane input produced by the compatibility terminal-step path is
/// converted into actor side effects after a pane has been handed to an async
/// process owner. This protects older control-socket attach calls while the
/// production foreground path moves to fully deferred pane I/O.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_drains_service_deferred_input_after_pane_handoff() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut processes = handle
            .take_running_pane_processes_for_async_owner(8)
            .await
            .unwrap();
        assert_eq!(processes.len(), 1);
        let (pane_id, mut process) = processes.pop().unwrap();
        let step = AttachedTerminalClientStepPlan {
            actions: vec![TerminalClientLoopAction::ForwardToPane(b"hello\n".to_vec())],
            output_lines: Vec::new(),
            output_line_style_spans: Vec::new(),
            input_hangup: false,
            output_hangup: false,
            error_roles: Vec::new(),
        };
        let application = handle
            .apply_attached_terminal_step_plan_inline_pane_io(primary, step)
            .await
            .unwrap();
        assert_eq!(application.forwarded_bytes, 6);
        assert_eq!(
            handle
                .drain_pane_io_side_effects(pane_id.as_str(), 8)
                .await
                .unwrap(),
            vec![RuntimeSideEffect::WritePaneInput {
                pane_id,
                bytes: b"hello\n".to_vec(),
            }]
        );
        let _ = process.terminate(Duration::from_millis(10));
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.service.pane_processes_mut().terminate_all().is_ok());
}

/// Verifies that pane close commands produce async termination side effects
/// after pane process ownership has moved out of the manager. Without this
/// bridge, `kill-pane` would close runtime layout state while leaving the
/// worker-owned PTY process alive.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_drains_service_deferred_termination_after_pane_handoff() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut processes = handle
            .take_running_pane_processes_for_async_owner(8)
            .await
            .unwrap();
        assert_eq!(processes.len(), 1);
        let (pane_id, mut process) = processes.pop().unwrap();
        let output = handle
            .execute_terminal_command(primary, "kill-pane --force".to_string())
            .await
            .unwrap();
        assert!(output.contains("closed=true"));
        assert_eq!(
            handle
                .drain_pane_io_side_effects(pane_id.as_str(), 8)
                .await
                .unwrap(),
            vec![RuntimeSideEffect::TerminatePane {
                pane_id,
                force: true,
            }]
        );
        let _ = process.terminate(Duration::from_millis(10));
        let _ = handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.service.pane_processes_mut().terminate_all().is_ok());
}

/// Verifies that the attached terminal loop can run in deferred pane I/O mode,
/// where forwarded primary input becomes a pane side effect instead of a direct
/// synchronous manager write. This is the mode required once live pane
/// processes are owned by supervised async workers.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_loop_can_defer_pane_input_to_worker() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Input,
            fd: 0,
            interest: TerminalFdInterest::read(),
            readable: true,
            writable: false,
            hangup: false,
            error: false,
        }]],
        input_batches: vec![b"hello\n".to_vec()],
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_loop_deferred_pane_io(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.iterations, 1);
        assert_eq!(
            report.actions,
            vec![TerminalClientLoopAction::ForwardToPane(b"hello\n".to_vec())]
        );
        assert_eq!(report.output_frames, 0);
        assert_eq!(io.written_batches.len(), 0);
        assert_eq!(
            handle.drain_pane_io_side_effects("%1", 8).await.unwrap(),
            vec![RuntimeSideEffect::WritePaneInput {
                pane_id: "%1".to_string(),
                bytes: b"hello\n".to_vec(),
            }]
        );
        let _ = handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(client, actor.run());

    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that a stalled attached-terminal readiness await returns a typed
/// loop error instead of monopolizing the foreground client service forever.
/// The outer timeout is intentionally one millisecond longer than the loop
/// step bound: without the production timeout, this regression fails by hitting
/// the outer guard instead of observing the structured operation error.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_attached_terminal_loop_times_out_stalled_readiness_poll() {
    struct StalledReadinessIo;

    impl AsyncAttachedTerminalIo for StalledReadinessIo {
        fn poll_readiness<'a>(
            &'a mut self,
        ) -> super::AsyncTerminalIoFuture<'a, Vec<AttachedTerminalFdReadiness>> {
            Box::pin(std::future::pending())
        }

        fn read_input<'a>(
            &'a mut self,
            _max_bytes: usize,
        ) -> super::AsyncTerminalIoFuture<'a, Vec<u8>> {
            Box::pin(async {
                Err(crate::error::MezError::invalid_state(
                    "stalled readiness test should not read input",
                ))
            })
        }

        fn write_styled_output_with_modes<'a>(
            &'a mut self,
            _lines: &'a [String],
            _line_style_spans: &'a [Vec<crate::terminal::TerminalStyleSpan>],
            _modes: AttachedTerminalOutputModes,
        ) -> super::AsyncTerminalIoFuture<'a, usize> {
            Box::pin(async {
                Err(crate::error::MezError::invalid_state(
                    "stalled readiness test should not write output",
                ))
            })
        }
    }

    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, _actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let mut io = StalledReadinessIo;

    let result = tokio::time::timeout(
        Duration::from_millis(251),
        run_async_attached_terminal_client_loop(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            |_| Ok(None),
        ),
    )
    .await
    .expect("attached-terminal loop should return its own timeout before the test guard");
    let error = result.unwrap_err();

    assert_eq!(
        error.to_string(),
        "InvalidState: async attached terminal readiness poll timed out after 250 ms"
    );
}

/// Verifies large foreground input is drained across bounded client reads.
///
/// Host paste payloads can be larger than one attached-terminal read. The
/// client loop must keep reading subsequent chunks and queue every accepted
/// byte as ordered pane-input side effects instead of treating the first
/// viewport-sized read as the whole paste.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_loop_preserves_large_deferred_paste_across_reads() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let paste = b"large-paste-".repeat(16);
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![
            vec![AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Input,
                fd: 0,
                interest: TerminalFdInterest::read(),
                readable: true,
                writable: false,
                hangup: false,
                error: false,
            }],
            vec![AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Input,
                fd: 0,
                interest: TerminalFdInterest::read(),
                readable: true,
                writable: false,
                hangup: false,
                error: false,
            }],
            vec![AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Input,
                fd: 0,
                interest: TerminalFdInterest::read(),
                readable: true,
                writable: false,
                hangup: false,
                error: false,
            }],
        ],
        input_batches: vec![paste.clone()],
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_loop_deferred_pane_io(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 3,
                    max_input_bytes: 64,
                },
            },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.iterations, 3);
        let queued = handle.drain_pane_io_side_effects("%1", 8).await.unwrap();
        let forwarded = queued
            .into_iter()
            .filter_map(|effect| match effect {
                RuntimeSideEffect::WritePaneInput { bytes, .. } => Some(bytes),
                _ => None,
            })
            .flatten()
            .collect::<Vec<_>>();
        assert_eq!(forwarded, paste);
        let _ = handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(client, actor.run());

    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that the higher-level attached-terminal client service can use the
/// deferred pane I/O mode across its prepolled batch boundary. Foreground
/// daemon attach uses this service wrapper, so the production handoff needs the
/// service-level path as well as the single-loop path.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_service_can_defer_pane_input_to_worker() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Input,
            fd: 0,
            interest: TerminalFdInterest::read(),
            readable: true,
            writable: false,
            hangup: false,
            error: false,
        }]],
        input_batches: vec![b"service-input\n".to_vec()],
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_service_deferred_pane_io(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            AsyncAttachedTerminalClientServiceConfig { max_batches: 2 },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.batches, 2);
        assert!(
            report
                .loop_report
                .actions
                .contains(&TerminalClientLoopAction::ForwardToPane(
                    b"service-input\n".to_vec()
                ))
        );
        assert_eq!(
            handle.drain_pane_io_side_effects("%1", 8).await.unwrap(),
            vec![RuntimeSideEffect::WritePaneInput {
                pane_id: "%1".to_string(),
                bytes: b"service-input\n".to_vec(),
            }]
        );
        let _ = handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(client, actor.run());

    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that slow client-output flushing does not block foreground input
/// routing. The first service batch starts a large frame and leaves bytes
/// pending; the second batch observes user input before that frame has been
/// fully written and still forwards the payload to the primary pane worker.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_service_routes_input_while_output_is_pending() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let mut io = SlowOutputAttachedTerminalLoopIo::new(
        vec![vec![AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Input,
            fd: 0,
            interest: TerminalFdInterest::read(),
            readable: true,
            writable: false,
            hangup: false,
            error: false,
        }]],
        vec![b"hello\n".to_vec()],
        64,
    );

    let client = async {
        let report = run_async_attached_terminal_client_service_deferred_pane_io(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            AsyncAttachedTerminalClientServiceConfig { max_batches: 2 },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.batches, 2);
        assert!(report.loop_report.partial_writes > 0);
        assert!(report.loop_report.pending_output_bytes > 0);
        assert!(
            report
                .loop_report
                .actions
                .contains(&TerminalClientLoopAction::ForwardToPane(
                    b"hello\n".to_vec()
                ))
        );
        assert_eq!(
            handle.drain_pane_io_side_effects("%1", 8).await.unwrap(),
            vec![RuntimeSideEffect::WritePaneInput {
                pane_id: "%1".to_string(),
                bytes: b"hello\n".to_vec(),
            }]
        );
        assert_eq!(io.completed_frames, 0);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());

    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies async attached terminal loop renders and applies primary actions.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_loop_renders_and_applies_primary_actions() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![
            AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Input,
                fd: 0,
                interest: TerminalFdInterest::read(),
                readable: true,
                writable: false,
                hangup: false,
                error: false,
            },
            AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Output,
                fd: 1,
                interest: TerminalFdInterest::write(),
                readable: false,
                writable: true,
                hangup: false,
                error: false,
            },
        ]],
        input_batches: vec![b"\x01\x1b[C".to_vec()],
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_loop(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            |_| {
                Ok(Some(ClientStatusLine {
                    kind: ClientStatusKind::Plain,
                    text: "attached".to_string(),
                }))
            },
        )
        .await
        .unwrap();

        assert_eq!(report.iterations, 1);
        assert_eq!(
            report.actions,
            vec![TerminalClientLoopAction::ExecuteMux(MuxAction::FocusPane(
                crate::terminal::PaneFocusDirection::Right
            ))]
        );
        assert_eq!(report.output_frames, 2);
        assert_eq!(io.written_batches.len(), 2);
        assert_eq!(
            io.written_batches.last().unwrap()[23].trim_end(),
            "attached"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed > 0);
}

/// Verifies that foreground client-step errors are shown through actor-owned
/// overlay state instead of a private prompt-error acknowledgement loop. This
/// keeps the async loop non-blocking even when no acknowledgement input is
/// available in the current batch.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_loop_routes_runtime_errors_to_actor_overlay() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let wrong_primary = ClientId::new('c', 4242);
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![
            AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Input,
                fd: 0,
                interest: TerminalFdInterest::read(),
                readable: true,
                writable: false,
                hangup: false,
                error: false,
            },
            AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Output,
                fd: 1,
                interest: TerminalFdInterest::write(),
                readable: false,
                writable: true,
                hangup: false,
                error: false,
            },
        ]],
        input_batches: vec![b"hello\n".to_vec()],
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = tokio::time::timeout(
            Duration::from_millis(250),
            run_async_attached_terminal_client_loop(
                &handle,
                &mut io,
                AsyncAttachedTerminalLoopRequest {
                    role: ClientViewRole::Primary,
                    client_id: primary.clone(),
                    primary_client_id: Some(wrong_primary),
                    client_size: Size::new(80, 24).unwrap(),
                    terminal_config: TerminalClientLoopConfig::default(),
                    loop_config: AttachedTerminalClientLoopConfig {
                        max_iterations: 1,
                        max_input_bytes: 64,
                    },
                },
                |_| Ok(None),
            ),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(report.output_frames, 2);
        assert_eq!(io.written_batches.len(), 2);
        let error_frame = io.written_batches.last().unwrap();
        assert!(
            error_frame
                .iter()
                .any(|line| line.contains("operation requires the primary client")),
            "{:?}",
            error_frame
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    let overlay_view = exit
        .service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(
        overlay_view
            .lines
            .iter()
            .any(|line| line.contains("operation requires the primary client")),
        "{:?}",
        overlay_view.lines
    );
    assert!(exit.commands_processed >= 4);
}

/// Verifies async attached terminal loop runs actor owned command prompt.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_loop_runs_actor_owned_command_prompt() {
    let transcript_root = std::env::temp_dir().join(format!(
        "mez-async-command-prompt-history-{}-{:?}",
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
    let command_history_path = transcript_store.command_prompt_history_file();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![
            vec![
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Input,
                    fd: 0,
                    interest: TerminalFdInterest::read(),
                    readable: true,
                    writable: false,
                    hangup: false,
                    error: false,
                },
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Output,
                    fd: 1,
                    interest: TerminalFdInterest::write(),
                    readable: false,
                    writable: true,
                    hangup: false,
                    error: false,
                },
            ],
            vec![
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Input,
                    fd: 0,
                    interest: TerminalFdInterest::read(),
                    readable: true,
                    writable: false,
                    hangup: false,
                    error: false,
                },
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Output,
                    fd: 1,
                    interest: TerminalFdInterest::write(),
                    readable: false,
                    writable: true,
                    hangup: false,
                    error: false,
                },
            ],
            vec![
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Input,
                    fd: 0,
                    interest: TerminalFdInterest::read(),
                    readable: true,
                    writable: false,
                    hangup: false,
                    error: false,
                },
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Output,
                    fd: 1,
                    interest: TerminalFdInterest::write(),
                    readable: false,
                    writable: true,
                    hangup: false,
                    error: false,
                },
            ],
        ],
        input_batches: vec![
            b"\x01:".to_vec(),
            b"list-buffers\r".to_vec(),
            b"\x1b".to_vec(),
        ],
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_loop(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 2,
                    max_input_bytes: 64,
                },
            },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.iterations, 2);
        assert_eq!(report.output_frames, 4);
        assert_eq!(
            report.actions,
            vec![
                TerminalClientLoopAction::ExecuteMux(MuxAction::EnterCommandPrompt),
                TerminalClientLoopAction::ForwardToPane(b"list-buffers\r".to_vec())
            ]
        );
        assert_eq!(io.written_batches.len(), 4);
        assert_eq!(io.written_batches[1][23].trim_end(), "▐ :");
        assert!(
            io.written_batches[3]
                .iter()
                .any(|line| line.contains("buffers: 0"))
        );
        assert!(
            io.written_batches[3]
                .iter()
                .any(|line| line.contains("source: runtime"))
        );
        assert!(
            io.written_batches[3]
                .iter()
                .any(|line| line.contains("status: empty"))
        );
        assert!(!command_history_path.exists());
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
        assert_eq!(
            transcript_store.command_prompt_history().unwrap(),
            vec![String::from("list-buffers")]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed > 0);
    let _ = std::fs::remove_dir_all(transcript_root);
}

/// Verifies async attached terminal loop routes agent shell input non modally.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_loop_routes_agent_shell_input_non_modally() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![
            vec![
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Input,
                    fd: 0,
                    interest: TerminalFdInterest::read(),
                    readable: true,
                    writable: false,
                    hangup: false,
                    error: false,
                },
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Output,
                    fd: 1,
                    interest: TerminalFdInterest::write(),
                    readable: false,
                    writable: true,
                    hangup: false,
                    error: false,
                },
            ],
            vec![
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Input,
                    fd: 0,
                    interest: TerminalFdInterest::read(),
                    readable: true,
                    writable: false,
                    hangup: false,
                    error: false,
                },
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Output,
                    fd: 1,
                    interest: TerminalFdInterest::write(),
                    readable: false,
                    writable: true,
                    hangup: false,
                    error: false,
                },
            ],
            vec![
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Input,
                    fd: 0,
                    interest: TerminalFdInterest::read(),
                    readable: false,
                    writable: false,
                    hangup: false,
                    error: false,
                },
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Output,
                    fd: 1,
                    interest: TerminalFdInterest::write(),
                    readable: false,
                    writable: true,
                    hangup: false,
                    error: false,
                },
            ],
        ],
        input_batches: vec![b"\x01a".to_vec(), b"/status\r".to_vec()],
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_loop(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 3,
                    max_input_bytes: 64,
                },
            },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.iterations, 3);
        assert_eq!(
            report.actions,
            vec![
                TerminalClientLoopAction::ExecuteMux(MuxAction::ToggleAgentShell),
                TerminalClientLoopAction::ForwardToPane(b"/status\r".to_vec()),
            ]
        );
        assert_eq!(report.output_frames, 4);
        assert_eq!(io.written_batches.len(), 4);
        assert!(
            io.written_batches[1]
                .iter()
                .any(|line| line.trim_end() == "▐ mez>")
        );
        let status_output = io.written_batches[2].join("\n");
        assert!(
            status_output.contains("│ Permissions")
                && status_output.contains("preset read-only")
                && !status_output.contains("Quota Usage"),
            "{status_output}"
        );
        assert!(
            !io.written_batches[2]
                .iter()
                .any(|line| line.contains("agent-shell:"))
        );
        assert!(
            !io.written_batches[2]
                .iter()
                .any(|line| line.trim_end() == "▐ agent>"),
            "status display should use the pager overlay instead of pane prompt rows"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed >= 6);
}

/// Verifies that submitting pane-local agent prompt input redraws the client
/// frame in the same attached-terminal loop pass. Without this refresh, the
/// submitted prompt text stayed visible until a later agent state change caused
/// the next render, which made queued follow-up prompts feel blocked.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_loop_clears_agent_prompt_on_submit() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![
            AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Input,
                fd: 0,
                interest: TerminalFdInterest::read(),
                readable: true,
                writable: false,
                hangup: false,
                error: false,
            },
            AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Output,
                fd: 1,
                interest: TerminalFdInterest::write(),
                readable: false,
                writable: true,
                hangup: false,
                error: false,
            },
        ]],
        input_batches: vec![b"list files\r".to_vec()],
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_loop(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.iterations, 1);
        assert_eq!(
            report.actions,
            vec![TerminalClientLoopAction::ForwardToPane(
                b"list files\r".to_vec()
            )]
        );
        assert_eq!(report.output_frames, 1);
        assert_eq!(io.written_batches.len(), 1);
        let refreshed = io.written_batches.last().unwrap();
        assert!(
            refreshed
                .iter()
                .any(|line| line.trim_end().starts_with("▐ mez> thinking")),
            "{refreshed:?}"
        );
        assert!(
            !refreshed
                .iter()
                .any(|line| line.trim_end() == "▐ mez> list files"),
            "{refreshed:?}"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert_eq!(exit.service.pending_agent_provider_tasks().len(), 1);
    let pane_text = exit
        .service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("user> list files"), "{pane_text}");
    assert!(exit.commands_processed >= 4);
}

/// Verifies that leaving pane-local agent mode invalidates the attached
/// terminal's differential frame state before repainting. The agent prompt is a
/// Mezzanine-owned overlay, while the underlying shell prompt is PTY-owned; a
/// full redraw at this boundary keeps cursor placement and stale prompt rows
/// from leaking after the mode switch.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_loop_full_redraws_after_agent_prompt_exit() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let mut io = FakeResizingAttachedTerminalLoopIo {
        inner: FakeAttachedTerminalLoopIo {
            readiness_batches: vec![vec![
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Input,
                    fd: 0,
                    interest: TerminalFdInterest::read(),
                    readable: true,
                    writable: false,
                    hangup: false,
                    error: false,
                },
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Output,
                    fd: 1,
                    interest: TerminalFdInterest::write(),
                    readable: false,
                    writable: true,
                    hangup: false,
                    error: false,
                },
            ]],
            input_batches: vec![b"/exit\r".to_vec()],
            written_batches: Vec::new(),
            write_error_kinds: Vec::new(),
        },
        terminal_size_batches: Vec::new(),
        invalidated_output_frames: 0,
    };

    let client = async {
        let report = run_async_attached_terminal_client_loop(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.iterations, 1);
        assert_eq!(report.output_frames, 1);
        assert_eq!(io.invalidated_output_frames, 1);
        assert_eq!(io.inner.written_batches.len(), 1);
        assert!(
            !io.inner.written_batches[0]
                .iter()
                .any(|line| line.contains("▐ agent>")),
            "{:?}",
            io.inner.written_batches[0]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed >= 4);
}

/// Verifies async attached terminal loop renders observer without applying input.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_loop_renders_observer_without_applying_input() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![
            AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Input,
                fd: 0,
                interest: TerminalFdInterest::read(),
                readable: true,
                writable: false,
                hangup: false,
                error: false,
            },
            AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Output,
                fd: 1,
                interest: TerminalFdInterest::write(),
                readable: false,
                writable: true,
                hangup: false,
                error: false,
            },
        ]],
        input_batches: vec![b"\x1b=".to_vec()],
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_loop(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Observer,
                client_id: ClientId::new('c', 9001),
                primary_client_id: None,
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            |_| {
                Ok(Some(ClientStatusLine {
                    kind: ClientStatusKind::PendingObserver,
                    text: "observe".to_string(),
                }))
            },
        )
        .await
        .unwrap();

        assert!(report.actions.is_empty());
        assert_eq!(report.output_frames, 1);
        assert_eq!(io.input_batches.len(), 0);
        assert_eq!(io.written_batches[0][23].trim_end(), "observer: observe");
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed >= 4);
}

/// Verifies that direct foreground client-loop rendering schedules the
/// actor-owned cursor and status timers. Foreground attached clients still use a
/// direct render path while the refactor is in progress, and those frames must
/// seed timer-driven invalidations before the blind batch sleep can be removed.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_loop_schedules_render_timers_after_direct_flush() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Output,
            fd: 1,
            interest: TerminalFdInterest::write(),
            readable: false,
            writable: true,
            hangup: false,
            error: false,
        }]],
        input_batches: Vec::new(),
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_loop(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.output_frames, 1);
        let timers = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(timers.len(), 1);
        let RuntimeSideEffect::ScheduleTimer { key, .. } = &timers[0] else {
            panic!("expected status refresh timer: {timers:?}");
        };
        assert_eq!(key.kind, RuntimeTimerKind::StatusRefresh);
        assert_eq!(key.owner_id, primary.to_string());
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed >= 5);
}

/// Verifies async attached terminal service runs batches until hangup.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_service_runs_batches_until_hangup() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Input,
            fd: 0,
            interest: TerminalFdInterest::read(),
            readable: false,
            writable: false,
            hangup: true,
            error: false,
        }]],
        input_batches: Vec::new(),
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_service(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Observer,
                client_id: ClientId::new('c', 9002),
                primary_client_id: None,
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            AsyncAttachedTerminalClientServiceConfig { max_batches: 8 },
            |iteration| {
                Ok(Some(ClientStatusLine {
                    kind: ClientStatusKind::Plain,
                    text: format!("service-{iteration}"),
                }))
            },
        )
        .await
        .unwrap();

        assert_eq!(report.batches, 2);
        assert_eq!(report.loop_report.iterations, 2);
        assert_eq!(report.loop_report.output_frames, 1);
        assert_eq!(report.loop_report.input_hangups, 1);
        assert_eq!(io.written_batches.len(), 1);
        assert_eq!(io.written_batches[0][23].trim_end(), "service-0");
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed >= 5);
}

/// Verifies that an attached-terminal service wakes between batches when the
/// actor queues side effects. This keeps render/output work responsive now that
/// quiet periods no longer have a periodic foreground redraw sleep.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_attached_terminal_service_wakes_between_batches_on_side_effects() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![Vec::new(), Vec::new()],
        input_batches: Vec::new(),
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let service_handle = handle.clone();
    let notify_handle = handle.clone();
    let client = async move {
        let service = run_async_attached_terminal_client_service(
            &service_handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Observer,
                client_id: ClientId::new('c', 9022),
                primary_client_id: None,
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            AsyncAttachedTerminalClientServiceConfig { max_batches: 2 },
            |_| Ok(None),
        );
        let notifier = async {
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(10)).await;
            notify_handle
                .queue_runtime_side_effects(vec![RuntimeSideEffect::RenderClient {
                    client_id: ClientId::new('c', 9022),
                    reason: RenderInvalidationReason::CursorBlink,
                }])
                .await
                .unwrap();
        };
        let (report, ()) = tokio::time::timeout(Duration::from_millis(250), async {
            tokio::join!(service, notifier)
        })
        .await
        .unwrap();
        let report = report.unwrap();
        assert_eq!(report.batches, 2);
        assert_eq!(report.loop_report.iterations, 2);
        assert_eq!(
            service_handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 4);
}

/// Verifies that a quiet attached-terminal service does not advance from an
/// idle timeout after its initial frame. This protects the foreground path from
/// reintroducing a periodic redraw clock that consumes CPU while the terminal is
/// idle.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_attached_terminal_service_has_no_idle_batch_timer() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let write_count = StdArc::new(AtomicUsize::new(0));
    let write_notify = StdArc::new(tokio::sync::Notify::new());
    let mut io = IdleAsyncAttachedTerminalLoopIo::new(write_count.clone(), write_notify.clone());

    let client = async {
        let service = run_async_attached_terminal_client_service(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Observer,
                client_id: ClientId::new('c', 9023),
                primary_client_id: None,
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            AsyncAttachedTerminalClientServiceConfig { max_batches: 2 },
            |_| Ok(None),
        );
        tokio::pin!(service);
        tokio::select! {
            _ = write_notify.notified() => {}
            result = &mut service => panic!("attached terminal service completed before idling: {result:?}"),
        }
        let advance = async {
            tokio::time::advance(Duration::from_millis(250)).await;
        };
        let (result, ()) = tokio::join!(
            tokio::time::timeout(Duration::from_millis(200), &mut service),
            advance
        );
        assert!(result.is_err());
        assert_eq!(write_count.load(Ordering::SeqCst), 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 4);
}

/// Verifies that the attached-terminal service treats queued render work as
/// level-triggered state instead of relying only on a retained notify permit.
///
/// A background service can consume the one stored side-effect notification
/// before the foreground client reaches its idle wait. The render invalidation
/// itself remains queued in the actor, and the client must drain it before
/// awaiting fresh input so a quiet terminal cannot strand a repaint forever.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_attached_terminal_service_drains_stranded_render_effect_before_waiting() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let client_id = ClientId::new('c', 9024);
    let write_count = StdArc::new(AtomicUsize::new(0));
    let write_notify = StdArc::new(tokio::sync::Notify::new());
    let mut io = IdleAsyncAttachedTerminalLoopIo::new(write_count.clone(), write_notify.clone());

    let client = async {
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::RenderClient {
                client_id: client_id.clone(),
                reason: RenderInvalidationReason::PaneOutput,
            }])
            .await
            .unwrap();
        handle.wait_for_runtime_side_effects().await;

        let report = tokio::time::timeout(
            Duration::from_millis(250),
            run_async_attached_terminal_client_service(
                &handle,
                &mut io,
                AsyncAttachedTerminalLoopRequest {
                    role: ClientViewRole::Observer,
                    client_id,
                    primary_client_id: None,
                    client_size: Size::new(80, 24).unwrap(),
                    terminal_config: TerminalClientLoopConfig::default(),
                    loop_config: AttachedTerminalClientLoopConfig {
                        max_iterations: 1,
                        max_input_bytes: 64,
                    },
                },
                AsyncAttachedTerminalClientServiceConfig { max_batches: 2 },
                |_| Ok(None),
            ),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(report.batches, 2);
        assert_eq!(report.loop_report.output_frames, 2);
        assert_eq!(write_count.load(Ordering::SeqCst), 2);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 7);
}

/// Verifies that bursty render invalidations are coalesced behind the
/// configured foreground render rate and still produce one trailing frame.
///
/// This protects slow remote clients from being flooded by intermediate frames
/// while preserving the final visible state after an output burst settles.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_attached_terminal_service_rate_limits_bursty_render_invalidations() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let write_count = StdArc::new(AtomicUsize::new(0));
    let write_notify = StdArc::new(tokio::sync::Notify::new());

    let client = async {
        let service_handle = handle.clone();
        let service_primary = primary.clone();
        let service_write_notify = write_notify.clone();
        let service_write_count = write_count.clone();
        let service_task = tokio::spawn(async move {
            let mut io =
                IdleAsyncAttachedTerminalLoopIo::new(service_write_count, service_write_notify);
            run_async_attached_terminal_client_service(
                &service_handle,
                &mut io,
                AsyncAttachedTerminalLoopRequest {
                    role: ClientViewRole::Primary,
                    client_id: service_primary.clone(),
                    primary_client_id: Some(service_primary),
                    client_size: Size::new(80, 24).unwrap(),
                    terminal_config: TerminalClientLoopConfig::default(),
                    loop_config: AttachedTerminalClientLoopConfig {
                        max_iterations: 1,
                        max_input_bytes: 64,
                    },
                },
                AsyncAttachedTerminalClientServiceConfig { max_batches: 2 },
                |_| Ok(None),
            )
            .await
        });

        write_notify.notified().await;
        assert_eq!(write_count.load(Ordering::SeqCst), 1);

        for _ in 0..3 {
            handle
                .queue_runtime_side_effects(vec![RuntimeSideEffect::RenderClient {
                    client_id: primary.clone(),
                    reason: RenderInvalidationReason::PaneOutput,
                }])
                .await
                .unwrap();
        }
        tokio::task::yield_now().await;
        assert_eq!(write_count.load(Ordering::SeqCst), 1);

        tokio::time::advance(Duration::from_millis(199)).await;
        tokio::task::yield_now().await;
        assert_eq!(write_count.load(Ordering::SeqCst), 1);

        tokio::time::advance(Duration::from_millis(1)).await;
        for _ in 0..8 {
            if write_count.load(Ordering::SeqCst) == 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(write_count.load(Ordering::SeqCst), 2);

        let report = tokio::time::timeout(Duration::from_millis(1), service_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(report.batches, 2);
        assert_eq!(report.loop_report.output_frames, 2);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 7);
}

/// Verifies that the foreground attached-terminal service polls terminal
/// dimensions while otherwise idle.
///
/// Some hosting terminals can change cell dimensions without producing an
/// input or runtime event that wakes the render service. The idle resize poll
/// should notice that size change, invalidate retained diff state, and repaint
/// exactly once instead of waiting for user interaction.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_service_polls_terminal_size_while_idle() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let write_count = StdArc::new(AtomicUsize::new(0));
    let write_notify = StdArc::new(tokio::sync::Notify::new());
    let invalidate_count = StdArc::new(AtomicUsize::new(0));

    let client = async {
        let service_handle = handle.clone();
        let service_primary = primary.clone();
        let service_write_count = write_count.clone();
        let service_write_notify = write_notify.clone();
        let service_invalidate_count = invalidate_count.clone();
        let service_task = tokio::spawn(async move {
            let mut io = InvalidatingIdleAsyncAttachedTerminalLoopIo::new(
                service_write_count,
                service_write_notify,
                service_invalidate_count,
            )
            .with_terminal_size_batches(vec![None, Some(Size::new(100, 30).unwrap())]);
            run_async_attached_terminal_client_service(
                &service_handle,
                &mut io,
                AsyncAttachedTerminalLoopRequest {
                    role: ClientViewRole::Primary,
                    client_id: service_primary.clone(),
                    primary_client_id: Some(service_primary),
                    client_size: Size::new(80, 24).unwrap(),
                    terminal_config: TerminalClientLoopConfig::default(),
                    loop_config: AttachedTerminalClientLoopConfig {
                        max_iterations: 1,
                        max_input_bytes: 64,
                    },
                },
                AsyncAttachedTerminalClientServiceConfig { max_batches: 2 },
                |_| Ok(None),
            )
            .await
        });

        write_notify.notified().await;
        assert_eq!(write_count.load(Ordering::SeqCst), 1);

        let deadline = Instant::now() + Duration::from_millis(500);
        while write_count.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(write_count.load(Ordering::SeqCst), 2);
        assert_eq!(invalidate_count.load(Ordering::SeqCst), 1);

        let report = tokio::time::timeout(Duration::from_millis(1), service_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(report.terminal_resizes, 1);
        assert_eq!(report.loop_report.output_frames, 2);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 5);
    assert_eq!(
        exit.service.session().authoritative_size,
        Size::new(100, 30).unwrap()
    );
}

/// Verifies that resize render invalidations interrupt an already pending
/// ordinary render-rate wait. Slow remote terminals can leave pane-output
/// refreshes coalesced behind the frame cadence, but a hosting terminal resize
/// changes the visible geometry and must immediately discard retained diff
/// state before repainting instead of waiting for the next pane-output tick.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_attached_terminal_service_resize_bypasses_pending_render_rate_limit() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let write_count = StdArc::new(AtomicUsize::new(0));
    let write_notify = StdArc::new(tokio::sync::Notify::new());
    let invalidate_count = StdArc::new(AtomicUsize::new(0));

    let client = async {
        let service_handle = handle.clone();
        let service_primary = primary.clone();
        let service_write_count = write_count.clone();
        let service_write_notify = write_notify.clone();
        let service_invalidate_count = invalidate_count.clone();
        let service_task = tokio::spawn(async move {
            let mut io = InvalidatingIdleAsyncAttachedTerminalLoopIo::new(
                service_write_count,
                service_write_notify,
                service_invalidate_count,
            );
            run_async_attached_terminal_client_service(
                &service_handle,
                &mut io,
                AsyncAttachedTerminalLoopRequest {
                    role: ClientViewRole::Primary,
                    client_id: service_primary.clone(),
                    primary_client_id: Some(service_primary),
                    client_size: Size::new(80, 24).unwrap(),
                    terminal_config: TerminalClientLoopConfig::default(),
                    loop_config: AttachedTerminalClientLoopConfig {
                        max_iterations: 1,
                        max_input_bytes: 64,
                    },
                },
                AsyncAttachedTerminalClientServiceConfig { max_batches: 2 },
                |_| Ok(None),
            )
            .await
        });

        write_notify.notified().await;
        assert_eq!(write_count.load(Ordering::SeqCst), 1);

        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::RenderClient {
                client_id: primary.clone(),
                reason: RenderInvalidationReason::PaneOutput,
            }])
            .await
            .unwrap();
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_millis(50)).await;
        assert_eq!(write_count.load(Ordering::SeqCst), 1);

        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::RenderClient {
                client_id: primary.clone(),
                reason: RenderInvalidationReason::Resize,
            }])
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_millis(1), write_notify.notified())
            .await
            .unwrap();
        assert_eq!(write_count.load(Ordering::SeqCst), 2);
        assert_eq!(invalidate_count.load(Ordering::SeqCst), 1);

        let report = tokio::time::timeout(Duration::from_millis(1), service_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(report.batches, 2);
        assert_eq!(report.loop_report.output_frames, 2);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 7);
}

/// Verifies that a newer rate-limited render supersedes stale pending output.
///
/// Slow clients can leave bytes from an older frame pending. During rapid pane
/// output, the attached client should wait for the next render tick and write
/// the latest frame instead of streaming obsolete pending bytes immediately.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_attached_terminal_service_does_not_flush_stale_pending_output_before_render_tick() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let write_count = StdArc::new(AtomicUsize::new(0));
    let write_notify = StdArc::new(tokio::sync::Notify::new());
    let pending_output_bytes = StdArc::new(AtomicUsize::new(0));
    let stale_flushes = StdArc::new(AtomicUsize::new(0));

    let client = async {
        let service_handle = handle.clone();
        let service_primary = primary.clone();
        let service_write_count = write_count.clone();
        let service_write_notify = write_notify.clone();
        let service_pending_output_bytes = pending_output_bytes.clone();
        let service_stale_flushes = stale_flushes.clone();
        let service_task = tokio::spawn(async move {
            let mut io = SupersedablePendingOutputIo::new(
                service_write_count,
                service_write_notify,
                service_pending_output_bytes,
                service_stale_flushes,
            );
            run_async_attached_terminal_client_service(
                &service_handle,
                &mut io,
                AsyncAttachedTerminalLoopRequest {
                    role: ClientViewRole::Primary,
                    client_id: service_primary.clone(),
                    primary_client_id: Some(service_primary),
                    client_size: Size::new(80, 24).unwrap(),
                    terminal_config: TerminalClientLoopConfig::default(),
                    loop_config: AttachedTerminalClientLoopConfig {
                        max_iterations: 1,
                        max_input_bytes: 64,
                    },
                },
                AsyncAttachedTerminalClientServiceConfig { max_batches: 2 },
                |_| Ok(None),
            )
            .await
        });

        write_notify.notified().await;
        assert_eq!(write_count.load(Ordering::SeqCst), 1);
        pending_output_bytes.store(1024, Ordering::SeqCst);

        for _ in 0..3 {
            handle
                .queue_runtime_side_effects(vec![RuntimeSideEffect::RenderClient {
                    client_id: primary.clone(),
                    reason: RenderInvalidationReason::PaneOutput,
                }])
                .await
                .unwrap();
        }

        tokio::task::yield_now().await;
        assert_eq!(write_count.load(Ordering::SeqCst), 1);
        assert_eq!(stale_flushes.load(Ordering::SeqCst), 0);

        tokio::time::advance(Duration::from_millis(199)).await;
        tokio::task::yield_now().await;
        assert_eq!(write_count.load(Ordering::SeqCst), 1);
        assert_eq!(stale_flushes.load(Ordering::SeqCst), 0);

        tokio::time::advance(Duration::from_millis(1)).await;
        for _ in 0..8 {
            if write_count.load(Ordering::SeqCst) == 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(write_count.load(Ordering::SeqCst), 2);
        assert_eq!(stale_flushes.load(Ordering::SeqCst), 0);
        assert_eq!(pending_output_bytes.load(Ordering::SeqCst), 0);

        let report = tokio::time::timeout(Duration::from_millis(1), service_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(report.batches, 2);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 7);
}

/// Verifies that output writability for an unsuperseded partial frame only
/// flushes retained bytes and does not ask the actor to compose another frame.
///
/// Slow foreground clients can leave encoded bytes pending after the latest
/// rendered frame. When no render invalidation is queued, the service should
/// treat writable output as flush readiness rather than as a redraw trigger.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_attached_terminal_service_flushes_idle_pending_output_without_redraw() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let write_count = StdArc::new(AtomicUsize::new(0));
    let write_notify = StdArc::new(tokio::sync::Notify::new());
    let pending_output_bytes = StdArc::new(AtomicUsize::new(0));
    let flushes = StdArc::new(AtomicUsize::new(0));

    let client = async {
        let service_handle = handle.clone();
        let service_primary = primary.clone();
        let service_write_count = write_count.clone();
        let service_write_notify = write_notify.clone();
        let service_pending_output_bytes = pending_output_bytes.clone();
        let service_flushes = flushes.clone();
        let service_task = tokio::spawn(async move {
            let mut io = SupersedablePendingOutputIo::new(
                service_write_count,
                service_write_notify,
                service_pending_output_bytes,
                service_flushes,
            );
            run_async_attached_terminal_client_service(
                &service_handle,
                &mut io,
                AsyncAttachedTerminalLoopRequest {
                    role: ClientViewRole::Primary,
                    client_id: service_primary.clone(),
                    primary_client_id: Some(service_primary),
                    client_size: Size::new(80, 24).unwrap(),
                    terminal_config: TerminalClientLoopConfig::default(),
                    loop_config: AttachedTerminalClientLoopConfig {
                        max_iterations: 1,
                        max_input_bytes: 64,
                    },
                },
                AsyncAttachedTerminalClientServiceConfig { max_batches: 2 },
                |_| Ok(None),
            )
            .await
        });

        write_notify.notified().await;
        assert_eq!(write_count.load(Ordering::SeqCst), 1);
        pending_output_bytes.store(1024, Ordering::SeqCst);
        tokio::task::yield_now().await;

        let report = tokio::time::timeout(Duration::from_millis(1), service_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(report.batches, 2);
        assert_eq!(report.loop_report.output_frames, 1);
        assert_eq!(report.loop_report.partial_writes, 1);
        assert_eq!(write_count.load(Ordering::SeqCst), 1);
        assert_eq!(flushes.load(Ordering::SeqCst), 1);
        assert_eq!(pending_output_bytes.load(Ordering::SeqCst), 1024);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.render_client_frame_requests, 1);
    assert!(exit.commands_processed >= 5);
}

/// Verifies that a closed foreground terminal output endpoint is treated as a
/// normal hangup instead of bubbling a `BrokenPipe` I/O error to the top-level
/// CLI error handler during clean primary shutdown or terminal teardown.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_service_treats_broken_pipe_as_output_hangup() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Output,
            fd: 1,
            interest: TerminalFdInterest::write(),
            readable: false,
            writable: true,
            hangup: false,
            error: false,
        }]],
        input_batches: Vec::new(),
        written_batches: Vec::new(),
        write_error_kinds: vec![std::io::ErrorKind::BrokenPipe],
    };

    let client = async {
        let report = run_async_attached_terminal_client_service(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Observer,
                client_id: ClientId::new('c', 9003),
                primary_client_id: None,
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            AsyncAttachedTerminalClientServiceConfig { max_batches: 4 },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.batches, 1);
        assert_eq!(report.loop_report.output_hangups, 1);
        assert_eq!(report.loop_report.output_frames, 0);
        assert!(io.written_batches.is_empty());
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed >= 5);
}

/// Verifies that the long-lived attached-terminal service treats an observed
/// terminal size change as authoritative for the primary client. This covers
/// the runtime path used by foreground sessions after a hosting terminal resize:
/// the client loop observes the new size, updates session geometry through the
/// actor, and subsequent rendering uses the resized authoritative dimensions.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_service_propagates_primary_terminal_resize() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let mut io = FakeResizingAttachedTerminalLoopIo {
        inner: FakeAttachedTerminalLoopIo {
            readiness_batches: vec![Vec::new()],
            input_batches: Vec::new(),
            written_batches: Vec::new(),
            write_error_kinds: Vec::new(),
        },
        terminal_size_batches: vec![Some(Size::new(100, 30).unwrap())],
        invalidated_output_frames: 0,
    };

    let client = async {
        let report = run_async_attached_terminal_client_service(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            AsyncAttachedTerminalClientServiceConfig { max_batches: 1 },
            |_| Ok(None),
        )
        .await
        .unwrap();
        let view = handle
            .render_client_view(
                ClientViewRole::Primary,
                Size::new(100, 30).unwrap(),
                TerminalClientLoopConfig::default(),
            )
            .await
            .unwrap()
            .unwrap();

        assert_eq!(report.terminal_resizes, 1);
        assert_eq!(view.authoritative_size, Size::new(100, 30).unwrap());
        assert_eq!(view.client_size, Size::new(100, 30).unwrap());
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed >= 5);
}

/// Verifies that a rapid sequence of foreground terminal-size changes
/// reschedules resize debounce work to the newest generation. Slow remote
/// clients can deliver resize signals close together, so the service should
/// cancel older debounce timers instead of letting each intermediate size force
/// a separate delayed full repaint.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_service_coalesces_resize_storm_timers() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let mut io = FakeResizingAttachedTerminalLoopIo {
        inner: FakeAttachedTerminalLoopIo {
            readiness_batches: vec![Vec::new(), Vec::new()],
            input_batches: Vec::new(),
            written_batches: Vec::new(),
            write_error_kinds: Vec::new(),
        },
        terminal_size_batches: vec![
            Some(Size::new(100, 30).unwrap()),
            Some(Size::new(120, 35).unwrap()),
            Some(Size::new(130, 40).unwrap()),
        ],
        invalidated_output_frames: 0,
    };

    let client = async {
        let report = run_async_attached_terminal_client_service(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig {
                    resize_debounce_ms: 25,
                    ..TerminalClientLoopConfig::default()
                },
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            AsyncAttachedTerminalClientServiceConfig { max_batches: 3 },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.terminal_resizes, 3);
        let timer_effects = handle.drain_timer_side_effects(8).await.unwrap();
        let resize_timer_effects = timer_effects
            .into_iter()
            .filter(|effect| match effect {
                RuntimeSideEffect::ScheduleTimer { key, .. }
                | RuntimeSideEffect::CancelTimer { key } => {
                    key.kind == RuntimeTimerKind::ResizeDebounce
                }
                _ => false,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            resize_timer_effects,
            vec![
                RuntimeSideEffect::ScheduleTimer {
                    key: RuntimeTimerKey::new(
                        RuntimeTimerKind::ResizeDebounce,
                        primary.as_str(),
                        1,
                    ),
                    delay_ms: 200,
                },
                RuntimeSideEffect::CancelTimer {
                    key: RuntimeTimerKey::new(
                        RuntimeTimerKind::ResizeDebounce,
                        primary.as_str(),
                        1,
                    ),
                },
                RuntimeSideEffect::ScheduleTimer {
                    key: RuntimeTimerKey::new(
                        RuntimeTimerKind::ResizeDebounce,
                        primary.as_str(),
                        2,
                    ),
                    delay_ms: 200,
                },
                RuntimeSideEffect::CancelTimer {
                    key: RuntimeTimerKey::new(
                        RuntimeTimerKind::ResizeDebounce,
                        primary.as_str(),
                        2,
                    ),
                },
                RuntimeSideEffect::ScheduleTimer {
                    key: RuntimeTimerKey::new(
                        RuntimeTimerKind::ResizeDebounce,
                        primary.as_str(),
                        3,
                    ),
                    delay_ms: 200,
                },
            ]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed >= 13);
}

/// Verifies that resize handling immediately invalidates retained foreground
/// output state and also queues a resize debounce timer. The immediate
/// invalidation gives the resized terminal a full refresh right away, while the
/// actor-owned timer still coalesces follow-up resize work without a blind
/// compatibility client-loop deadline.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_service_schedules_resize_debounce_timer() {
    let mut service = test_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[terminal]\nresize_debounce_ms = 1\n".to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let mut io = FakeResizingAttachedTerminalLoopIo {
        inner: FakeAttachedTerminalLoopIo {
            readiness_batches: vec![Vec::new(), Vec::new()],
            input_batches: Vec::new(),
            written_batches: Vec::new(),
            write_error_kinds: Vec::new(),
        },
        terminal_size_batches: vec![Some(Size::new(100, 30).unwrap()), None],
        invalidated_output_frames: 0,
    };

    let client = async {
        let report = run_async_attached_terminal_client_service(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            AsyncAttachedTerminalClientServiceConfig { max_batches: 1 },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.terminal_resizes, 1);
        assert_eq!(io.invalidated_output_frames, 1);
        let timers = handle.drain_timer_side_effects(8).await.unwrap();
        let resize_timer = timers
            .iter()
            .find(|effect| {
                matches!(
                    effect,
                    RuntimeSideEffect::ScheduleTimer { key, .. }
                        if key.kind == RuntimeTimerKind::ResizeDebounce
                )
            })
            .unwrap_or_else(|| panic!("expected resize debounce timer: {timers:?}"));
        let RuntimeSideEffect::ScheduleTimer { key, delay_ms } = resize_timer else {
            panic!("expected resize debounce timer: {timers:?}");
        };
        assert_eq!(key.kind, RuntimeTimerKind::ResizeDebounce);
        assert_eq!(key.owner_id, primary.to_string());
        assert_eq!(*delay_ms, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed >= 6);
}

/// Verifies async attached terminal service can be supervised by name.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_service_can_be_supervised_by_name() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let actor_handle = handle.clone();
    let io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Input,
            fd: 0,
            interest: TerminalFdInterest::read(),
            readable: false,
            writable: false,
            hangup: true,
            error: false,
        }]],
        input_batches: Vec::new(),
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };
    let service = build_async_attached_terminal_client_service(
        "attached-terminal-primary",
        handle,
        io,
        AsyncAttachedTerminalLoopRequest {
            role: ClientViewRole::Observer,
            client_id: ClientId::new('c', 9004),
            primary_client_id: None,
            client_size: Size::new(80, 24).unwrap(),
            terminal_config: TerminalClientLoopConfig::default(),
            loop_config: AttachedTerminalClientLoopConfig {
                max_iterations: 1,
                max_input_bytes: 64,
            },
        },
        AsyncAttachedTerminalClientServiceConfig { max_batches: 4 },
        |_| Ok(None),
    )
    .unwrap();

    let actor_task = tokio::spawn(actor.run());
    let report = supervise_async_runtime_services(vec![service], std::future::pending())
        .await
        .unwrap();
    assert!(!report.shutdown_requested);
    assert_eq!(
        report.services,
        vec![AsyncRuntimeServiceReport {
            name: "attached-terminal-primary".to_string(),
            exit: AsyncRuntimeServiceExit::completed(2),
        }]
    );
    assert_eq!(
        actor_handle.shutdown().await.unwrap(),
        RuntimeLifecycleState::Running
    );
    let exit = actor_task.await.unwrap();
    assert!(exit.commands_processed >= 4);
}

/// Verifies async attached terminal service exits cleanly after primary detach.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_service_exits_cleanly_after_primary_detach() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .detach_primary(&primary, Size::new(80, 24).unwrap())
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let mut io = FakeAttachedTerminalLoopIo::default();

    let client = async {
        let report = run_async_attached_terminal_client_service(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            AsyncAttachedTerminalClientServiceConfig { max_batches: 4 },
            |_| Ok(None),
        )
        .await
        .unwrap();
        assert_eq!(report.batches, 0);
        assert!(report.stopped_by_lifecycle);
        assert_eq!(report.terminal_state, RuntimeLifecycleState::Detached);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Detached
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed > 0);
}

/// Verifies async actor rejects requests after shutdown.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_rejects_requests_after_shutdown() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let retained_handle = handle.clone();

    let client = async {
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), _exit) = tokio::join!(client, actor.run());
    let error = retained_handle.lifecycle_state().await.unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
}

/// Verifies async control connection authorizes and round trips control frame.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_control_connection_authorizes_and_round_trips_control_frame() {
    use crate::control::{decode_control_frame, encode_control_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let (mut client_stream, mut server_stream) = UnixStream::pair().unwrap();
    let input = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );

    let client = async {
        client_stream.write_all(&input).await.unwrap();
        let mut output = vec![0; 4096];
        let read = client_stream.read(&mut output).await.unwrap();
        output.truncate(read);
        let (body, consumed) = decode_control_frame(&output, 4096).unwrap();
        assert_eq!(consumed, output.len());
        assert!(body.contains(r#""control/initialize""#));
    };
    let server = async {
        let mut connection = ControlConnectionState::new(true, true);
        let served = serve_async_runtime_control_connection(
            &mut server_stream,
            &handle,
            &mut connection,
            AsyncRuntimeControlConnectionConfig::new(4096, current_effective_uid()).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(served, input.len());
        assert!(connection.initialized());
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), exit) = tokio::join!(client, server, actor.run());

    assert_eq!(exit.commands_processed, 2);
}

/// Verifies async control connection loop preserves initialized caller.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_control_connection_loop_preserves_initialized_caller() {
    use crate::control::{decode_control_frame, encode_control_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let (mut client_stream, mut server_stream) = UnixStream::pair().unwrap();
    let initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );
    let get_session =
        encode_control_body(r#"{"jsonrpc":"2.0","id":"get","method":"session/get","params":{}}"#);

    let client = async {
        client_stream.write_all(&initialize).await.unwrap();
        let mut first = vec![0; 4096];
        let read = client_stream.read(&mut first).await.unwrap();
        first.truncate(read);
        let (body, _) = decode_control_frame(&first, 4096).unwrap();
        assert!(body.contains(r#""control/initialize""#));

        client_stream.write_all(&get_session).await.unwrap();
        let mut second = vec![0; 4096];
        let read = client_stream.read(&mut second).await.unwrap();
        second.truncate(read);
        let (body, _) = decode_control_frame(&second, 4096).unwrap();
        assert!(body.contains(r#""session_id""#));
        assert!(body.contains(r#""windows""#));
    };
    let server = async {
        let mut connection = ControlConnectionState::new(true, true);
        let served = serve_async_runtime_control_connection_loop(
            &mut server_stream,
            &handle,
            &mut connection,
            AsyncRuntimeControlConnectionConfig::new(4096, current_effective_uid()).unwrap(),
            |served, _state| served >= 2,
        )
        .await
        .unwrap();
        assert_eq!(served, 2);
        assert!(connection.initialized());
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), exit) = tokio::join!(client, server, actor.run());

    assert!(exit.commands_processed >= 3);
}

/// Verifies async control listener serves stateful connection until client closes.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_control_listener_serves_stateful_connection_until_client_closes() {
    use crate::control::{decode_control_frame, encode_control_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};

    let path = std::env::temp_dir().join(format!(
        "mez-async-control-listener-{}-{}.sock",
        std::process::id(),
        "stateful"
    ));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );
    let get_session =
        encode_control_body(r#"{"jsonrpc":"2.0","id":"get","method":"session/get","params":{}}"#);

    let client = async {
        let mut stream = UnixStream::connect(&path).await.unwrap();
        stream.write_all(&initialize).await.unwrap();
        let mut first = vec![0; 4096];
        let read = stream.read(&mut first).await.unwrap();
        first.truncate(read);
        let (body, _) = decode_control_frame(&first, 4096).unwrap();
        assert!(body.contains(r#""control/initialize""#));

        stream.write_all(&get_session).await.unwrap();
        let mut second = vec![0; 4096];
        let read = stream.read(&mut second).await.unwrap();
        second.truncate(read);
        let (body, _) = decode_control_frame(&second, 4096).unwrap();
        assert!(body.contains(r#""session_id""#));
    };
    let server = async {
        let served = serve_async_runtime_control_listener(
            &listener,
            &handle,
            AsyncRuntimeControlConnectionConfig::new(4096, current_effective_uid()).unwrap(),
            |served, _state| served >= 1,
        )
        .await
        .unwrap();
        assert_eq!(served, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), _exit) = tokio::join!(client, server, actor.run());
    let _ = std::fs::remove_file(&path);
}
