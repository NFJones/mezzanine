//! Async-runtime tests owned by persistence behavior.

use super::super::*;

/// Verifies that registry persistence side effects are coalesced before queue
/// capacity is checked. Registry writes describe the latest discoverable
/// session state, so a burst only needs the newest pending update for that
/// session rather than a queue entry per intermediate state.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_coalesces_registry_persistence_before_capacity_check() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-registry-coalesce-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let registry = SessionRegistry::new(root.clone(), current_effective_uid());
    let mut service = test_service();
    service.set_session_registry(registry.clone());
    let update = service.registry_update_plan();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .config(AsyncRuntimeActorConfig {
            side_effect_buffer: 2,
            ..AsyncRuntimeActorConfig::default()
        })
        .build()
        .unwrap();

    let client = async {
        let queued = handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::PersistRegistry {
                    registry: registry.clone(),
                    update: update.clone(),
                },
                RuntimeSideEffect::PersistRegistry {
                    registry: registry.clone(),
                    update: update.clone(),
                },
                RuntimeSideEffect::PersistRegistry { registry, update },
            ])
            .await
            .unwrap();
        assert_eq!(queued, 3);

        let effects = handle.drain_persistence_side_effects(8).await.unwrap();
        assert_eq!(effects.len(), 1);
        assert!(
            matches!(effects[0], RuntimeSideEffect::PersistRegistry { .. }),
            "{effects:?}"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.runtime_side_effects_queued, 1);
    assert_eq!(exit.metrics.runtime_side_effects_drained, 1);
    assert_eq!(exit.metrics.render_invalidations_coalesced, 2);
    assert_eq!(exit.metrics.side_effect_queue_high_water, 1);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that raw pane-output bursts do not enqueue registry persistence.
/// Pane output changes the terminal screen and event stream, but it does not
/// change the discoverable session registry record; persisting after every PTY
/// read can overflow the bounded side-effect queue during high-volume output.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_does_not_persist_registry_for_pane_output_bursts() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-registry-output-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let registry = SessionRegistry::new(root.clone(), current_effective_uid());
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    service.set_session_registry(registry);
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .config(AsyncRuntimeActorConfig {
            side_effect_buffer: 2,
            ..AsyncRuntimeActorConfig::default()
        })
        .build()
        .unwrap();

    let client = async {
        for index in 0..128 {
            let mut batch = RuntimeEventBatch::new();
            batch.push(RuntimeEvent::Pane(PaneEvent::Output {
                pane_id: "%1".to_string(),
                bytes: format!("burst-output-{index}\n").into_bytes(),
            }));
            let report = handle.submit_runtime_events(batch).await.unwrap();
            assert_eq!(report.accepted, 1);
            assert_eq!(report.applied, 1);
            assert_eq!(report.side_effects, 1);
        }

        let persistence = handle.drain_persistence_side_effects(8).await.unwrap();
        assert!(
            persistence.is_empty(),
            "pane output should not queue registry persistence: {persistence:?}"
        );
        let render = handle.drain_render_side_effects(8).await.unwrap();
        assert_eq!(
            render,
            vec![RuntimeSideEffect::RenderClient {
                client_id: primary,
                reason: RenderInvalidationReason::PaneOutput,
            }]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.side_effect_queue_high_water, 1);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that actor-applied runtime events refresh the session registry
/// by queuing a persistence-worker side effect rather than writing from inside
/// the actor. Daemon discovery must see sessions whose state changes through
/// typed events, and persistence-completion diagnostics must not recursively
/// enqueue another registry write.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_persists_registry_after_applied_runtime_events() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-registry-event-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let registry = SessionRegistry::new(root.clone(), current_effective_uid());
    let mut service = test_service();
    service.set_session_registry(registry.clone());
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Process(ProcessEvent::Spawned {
            pane_id: "%1".to_string(),
            pid: Some(42),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);

        let persistence = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 3,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |_, _| false,
        )
        .await
        .unwrap();
        assert_eq!(persistence.drained, 1);
        assert_eq!(persistence.completed, 1);
        assert_eq!(persistence.failed, 0);
        assert_eq!(persistence.submitted_events, 1);
        assert_eq!(persistence.applied_events, 1);
        assert_eq!(registry.list().unwrap().len(), 1);
        assert!(
            handle
                .drain_persistence_side_effects(8)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 3);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that audit records created through actor-owned runtime commands are
/// queued for the persistence worker instead of written from inside the actor.
/// The command still mutates policy state immediately, while the audit JSONL
/// append is drained through the target-specific persistence path.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_defers_audit_writes_to_persistence_worker() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-audit-defer-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let audit_path = root.join("audit.jsonl");
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_audit_log(crate::audit::AuditLog::new(crate::audit::AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: true,
        required: true,
    }));
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let response = handle
            .execute_agent_shell_command(primary, "/approval full-access".to_string())
            .await
            .unwrap();
        assert!(response.contains("changed=true"), "{response}");
        assert!(!audit_path.exists());

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
        assert_eq!(persistence.submitted_events, 1);
        assert_eq!(persistence.applied_events, 1);

        let audit = std::fs::read_to_string(&audit_path).unwrap();
        assert!(audit.contains(r#""event_type":"permission""#), "{audit}");
        assert!(
            audit.contains(r#""permission_id":"permissions.approval_policy""#),
            "{audit}"
        );
        assert!(audit.contains(r#""hash":"#), "{audit}");
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""worker":"async-persistence""#)
            && event.payload.contains(r#""target":"audit_log""#)
            && event.payload.contains(r#""state":"completed""#)
    }));
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that file-backed pane pipes start and append output through the
/// persistence worker in the async actor path. This keeps both `pipe-pane -o`
/// setup and later pane-output application from blocking on file I/O while
/// preserving the existing user behavior.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_defers_file_pane_pipe_writes_to_persistence_worker() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-pane-pipe-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("pane.log");
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let started = handle
            .execute_terminal_command(primary, format!("pipe-pane -o {}", path.display()))
            .await
            .unwrap();
        assert!(started.contains("pipe=started"), "{started}");
        assert!(
            !path.exists(),
            "async actor should not create file-backed pipe output before persistence worker drains"
        );

        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"pipe-async\n".to_vec(),
        }));
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert!(report.side_effects >= 1);

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
        assert_eq!(persistence.submitted_events, 1);
        assert_eq!(persistence.applied_events, 1);
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("pipe-async")
        );
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""worker":"async-persistence""#)
            && event.payload.contains(r#""target":"pane_pipe""#)
            && event.payload.contains(r#""state":"completed""#)
    }));
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that file-backed pane pipe persistence failures stop the active
/// pipe through the actor. A failed async append otherwise leaves runtime state
/// believing that pane output is still being captured even though subsequent
/// writes will continue to fail.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_stops_file_pane_pipe_after_persistence_failure() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-pane-pipe-failed-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("pane.log");
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let started = service
        .execute_terminal_command(&primary, &format!("pipe-pane -o {}", path.display()))
        .unwrap();
    assert!(started.contains("pipe=started"), "{started}");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"pipe-fail\n".to_vec(),
        }));
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert!(report.side_effects >= 1);

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
        assert_eq!(persistence.completed, 0);
        assert_eq!(persistence.failed, 1);
        assert_eq!(persistence.submitted_events, 1);
        assert_eq!(persistence.applied_events, 1);
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""worker":"async-persistence""#)
            && event.payload.contains(r#""target":"pane_pipe""#)
            && event.payload.contains(r#""state":"failed""#)
    }));
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""pipe":"stopped""#)
            && event.payload.contains(r#""reason":"persistence-failed""#)
    }));
    assert_eq!(exit.service.active_pane_pipe_display(), "active_pipes=0");
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that command-backed pane pipes are checked by actor-owned timers
/// after accepted pane output. The command writer can fail after `write_output`
/// has already accepted bytes into its bounded queue; the timer makes that
/// asynchronous failure visible and stops the active pipe without requiring a
/// later pane-output write or an explicit `pipe-pane --stop`.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_stops_command_pane_pipe_after_health_timer() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-command-pane-pipe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let script = root.join("pipe-command.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nhead -c 1 >/dev/null\nsleep 0.02\nexit 7\n",
    )
    .unwrap();
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let started = service
        .execute_terminal_command(&primary, &format!("pipe-pane /bin/sh {}", script.display()))
        .unwrap();
    assert!(started.contains("pipe=started"), "{started}");
    assert!(started.contains("mode=command"), "{started}");
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"pipe-command-health\n".to_vec(),
        }));
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert!(report.side_effects >= 1);
        tokio::time::sleep(Duration::from_millis(80)).await;

        let timers = run_async_runtime_timer_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 4,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            1_000,
            |polls, _| polls >= 4,
        )
        .await
        .unwrap();
        assert_eq!(timers.drained, 1);
        assert_eq!(timers.fired, 1);
        assert_eq!(timers.submitted_events, 1);
        assert_eq!(timers.applied_events, 1);
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""pipe":"stopped""#)
            && event.payload.contains(r#""mode":"command""#)
            && event.payload.contains(r#""reason":"command-failed""#)
    }));
    assert_eq!(exit.service.active_pane_pipe_display(), "active_pipes=0");
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that actor-owned command-backed pane pipes receive a health timer
/// as soon as the terminal command starts the pipe and reschedule that health
/// check while the pipe command is still active. This protects command pipe
/// lifecycle cleanup from depending on unrelated pane output and keeps quick
/// exits or deferred startup failures discoverable through timer ingress.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_actor_schedules_command_pane_pipe_health_after_start() {
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let started = handle
            .execute_terminal_command(primary.clone(), "pipe-pane cat >/dev/null".to_string())
            .await
            .unwrap();
        assert!(started.contains("pipe=started"), "{started}");
        assert!(started.contains("mode=command"), "{started}");

        let effects = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(effects.len(), 1, "{effects:?}");
        let first_key = match &effects[0] {
            RuntimeSideEffect::ScheduleTimer { key, delay_ms } => {
                assert_eq!(key.kind, RuntimeTimerKind::PanePipeHealth);
                assert_eq!(key.owner_id, "%1");
                assert_eq!(*delay_ms, 50);
                key.clone()
            }
            other => panic!("expected pane-pipe health timer, got {other:?}"),
        };

        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Timer(TimerEvent {
            key: first_key.clone(),
            now_ms: 1_060,
        }));
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 0);
        assert_eq!(report.side_effects, 1);

        let effects = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(effects.len(), 1, "{effects:?}");
        let second_key = match &effects[0] {
            RuntimeSideEffect::ScheduleTimer { key, delay_ms } => {
                assert_eq!(key.kind, RuntimeTimerKind::PanePipeHealth);
                assert_eq!(key.owner_id, "%1");
                assert_eq!(*delay_ms, 50);
                key.clone()
            }
            other => panic!("expected rescheduled pane-pipe health timer, got {other:?}"),
        };
        assert!(second_key.generation > first_key.generation);

        let stopped = handle
            .execute_terminal_command(primary, "pipe-pane --stop".to_string())
            .await
            .unwrap();
        assert!(stopped.contains("pipe=stopped"), "{stopped}");
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.service.active_pane_pipe_display(), "active_pipes=0");
    assert!(exit.commands_processed >= 4);
}

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
        .queue_blocked_approval(mez_agent::permissions::BlockedApprovalRequest {
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
            state: mez_agent::permissions::BlockedApprovalState::Pending,
            decision: None,
            redirect_instruction: None,
        })
        .unwrap();
    let deny_id = service
        .queue_blocked_approval(mez_agent::permissions::BlockedApprovalRequest {
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
            state: mez_agent::permissions::BlockedApprovalState::Pending,
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
        mez_agent::RuleDecision::Allow
    );
    assert_eq!(
        exit.service
            .permission_policy()
            .evaluate_shell_command("mez-test-command --delete"),
        mez_agent::RuleDecision::Forbid
    );
    assert!(exit.commands_processed >= 4);
    exit.service.terminate_all_pane_processes().unwrap();
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
