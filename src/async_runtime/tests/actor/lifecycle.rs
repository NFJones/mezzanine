//! Async-runtime tests owned by lifecycle behavior.

use super::super::*;

/// Verifies that typed runtime events can cross the async actor boundary through
/// the same serialized request channel used by legacy compatibility requests.
/// Non-mutating event families are accepted without side effects, while later
/// tests cover mutating pane output. Keeping both paths explicit prevents the
/// event channel from silently accepting events that should have state effects.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_accepts_runtime_event_batches_in_order() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let client_id = ClientId::parse('c', "c1").unwrap();
    let mut batch = RuntimeEventBatch::new();
    batch.push(RuntimeEvent::Client(ClientEvent::Resize {
        client_id,
        size: Size::new(100, 30).unwrap(),
    }));
    batch.push(RuntimeEvent::Pane(PaneEvent::InputWritten {
        pane_id: "%1".to_string(),
        bytes: 12,
    }));

    let client = async {
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 2);
        assert_eq!(report.applied, 0);
        assert_eq!(report.families, vec!["client", "pane"]);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.commands_processed, 2);
}

/// Verifies that status-line refresh timers are owned per attached client and
/// generation checked before repainting. Runtime status fields such as uptime
/// and local datetime update periodically, but a cancelled or superseded timer
/// must not create an extra frame when an older deadline fires late.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_ignores_stale_status_refresh_timer_events() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let stale_key = RuntimeTimerKey::new(RuntimeTimerKind::StatusRefresh, primary.as_str(), 1);
        let active_key = RuntimeTimerKey::new(RuntimeTimerKind::StatusRefresh, primary.as_str(), 2);
        handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::ScheduleTimer {
                    key: stale_key.clone(),
                    delay_ms: 1000,
                },
                RuntimeSideEffect::CancelTimer {
                    key: stale_key.clone(),
                },
                RuntimeSideEffect::ScheduleTimer {
                    key: active_key.clone(),
                    delay_ms: 1000,
                },
            ])
            .await
            .unwrap();
        let scheduled = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(scheduled.len(), 3);

        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Timer(TimerEvent {
            key: stale_key,
            now_ms: 1000,
        }));
        batch.push(RuntimeEvent::Timer(TimerEvent {
            key: active_key,
            now_ms: 1000,
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 2);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 2);
        let side_effects = handle.drain_runtime_side_effects(8).await.unwrap();
        assert_eq!(side_effects.len(), 2);
        assert_eq!(
            side_effects[0],
            RuntimeSideEffect::RenderClient {
                client_id: primary.clone(),
                reason: RenderInvalidationReason::StatusLine,
            }
        );
        let RuntimeSideEffect::ScheduleTimer { key, delay_ms } = &side_effects[1] else {
            panic!("expected status refresh reschedule: {side_effects:?}");
        };
        assert_eq!(key.kind, RuntimeTimerKind::StatusRefresh);
        assert_eq!(key.owner_id, primary.to_string());
        assert_eq!(*delay_ms, 1000);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.commands_processed, 5);
}

/// Verifies timer bookkeeping follows side-effect order when one generation is
/// cancelled and then scheduled again in the same actor enqueue operation.
/// The final schedule is authoritative, so its timer event must be accepted.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_honors_cancel_then_reschedule_timer_order() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let key = RuntimeTimerKey::new(RuntimeTimerKind::StatusRefresh, primary.as_str(), 1);
        handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::CancelTimer { key: key.clone() },
                RuntimeSideEffect::ScheduleTimer {
                    key: key.clone(),
                    delay_ms: 1000,
                },
            ])
            .await
            .unwrap();
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Timer(TimerEvent { key, now_ms: 1000 }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.applied, 1);
        handle.shutdown().await.unwrap();
    };

    let ((), _) = tokio::join!(client, actor.run());
}

/// Verifies alternate-screen exit pane output is promoted to a full redraw
/// invalidation instead of an ordinary pane-output update. This ensures the
/// attached-terminal client discards retained differential frame state before
/// repainting the restored primary buffer.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_uses_full_redraw_invalidation_for_alternate_screen_exit_output() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut enter = RuntimeEventBatch::new();
        enter.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"\x1b[?1049halt".to_vec(),
        }));
        let report = handle.submit_runtime_events(enter).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(
            handle.drain_render_side_effects(8).await.unwrap(),
            vec![RuntimeSideEffect::RenderClient {
                client_id: primary.clone(),
                reason: RenderInvalidationReason::PaneOutput,
            }]
        );

        let mut exit_batch = RuntimeEventBatch::new();
        exit_batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"\x1b[?1049lback".to_vec(),
        }));
        let report = handle.submit_runtime_events(exit_batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(
            handle.drain_render_side_effects(8).await.unwrap(),
            vec![RuntimeSideEffect::RenderClient {
                client_id: primary,
                reason: RenderInvalidationReason::FullRedraw,
            }]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies manual `/compact` publishes visible compaction state and queues a
/// provider-side compaction dispatch instead of blocking the actor while the
/// model request runs. This protects the terminal UI from invisible synchronous
/// compaction work.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_compact_command_queues_compaction_dispatch() {
    let mut service = test_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "async-compact-context-window".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "openai"
default_model_profile = "async-compact-test"
[providers.openai]
kind = "openai"
models = ["gpt-compact-test"]
default_model = "gpt-compact-test"
[model_profiles.async-compact-test]
provider = "openai"
model = "gpt-compact-test"
context_window_tokens = 128000
"#
            .to_string(),
        }])
        .unwrap();
    let transcript_root = std::env::temp_dir().join(format!(
        "mez-async-compact-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&transcript_root).unwrap();
    let transcript_store = AgentTranscriptStore::new(transcript_root);
    for sequence in 1..=3 {
        transcript_store
            .append(&crate::transcript::TranscriptEntry {
                conversation_id: "async-compact".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role: crate::transcript::TranscriptRole::Assistant,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: format!("compact source entry {sequence}"),
            })
            .unwrap();
    }
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "async-compact", 3)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let response = handle
            .execute_agent_shell_command(primary, "/compact".to_string())
            .await
            .unwrap();
        assert!(response.contains("state=queued"), "{response}");
        let dispatches = handle
            .drain_agent_provider_dispatch_side_effects(8)
            .await
            .unwrap();
        assert!(dispatches.iter().any(|effect| matches!(
            effect,
            RuntimeSideEffect::DispatchAgentCompaction { pane_id } if pane_id == "%1"
        )));
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    let config = exit
        .service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    assert_eq!(
        config
            .frame_context
            .panes
            .get("%1")
            .and_then(|pane| pane.agent_status.as_deref()),
        Some("compacting")
    );
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that async hook worker results are no longer accepted-only events.
/// The worker event model currently carries diagnostics rather than full hook
/// pipeline continuation state, so the actor applies completion and failure
/// events as replayable session diagnostics while preserving event ordering for
/// subscribers.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_hook_completion_events_to_event_log() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service_with_event_log())
        .build()
        .unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Hook(AsyncHookEvent::Completed {
            hook_id: "hook-1".to_string(),
            exit_code: Some(0),
            output_preview: "ok".to_string(),
        }));
        batch.push(RuntimeEvent::Hook(AsyncHookEvent::Failed {
            hook_id: "hook-2".to_string(),
            error: "worker channel closed".to_string(),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 2);
        assert_eq!(report.applied, 2);
        assert_eq!(report.side_effects, 0);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary)
        .iter()
        .map(|event| event.payload.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(events.contains(r#""async_hook":"completed""#), "{events}");
    assert!(events.contains(r#""hook_id":"hook-1""#), "{events}");
    assert!(events.contains(r#""exit_code":0"#), "{events}");
    assert!(events.contains(r#""output_preview":"ok""#), "{events}");
    assert!(events.contains(r#""async_hook":"failed""#), "{events}");
    assert!(events.contains(r#""hook_id":"hook-2""#), "{events}");
    assert!(
        events.contains(r#""error":"worker channel closed""#),
        "{events}"
    );
    assert_eq!(exit.commands_processed, 2);
}

/// Verifies that actor metrics track typed event ingress, side-effect queue
/// depth, high-water marks, drains, and worker notifications at the serialized
/// runtime boundary. These counters are the Phase 0 instrumentation surface for
/// replacing tick polling with event producers without losing visibility into
/// wakeups and retained work.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_metrics_track_event_and_side_effect_activity() {
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"metrics-output\n".to_vec(),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);

        let queued = handle.metrics().await.unwrap();
        assert_eq!(queued.commands_processed, 2);
        assert_eq!(queued.runtime_event_batches, 1);
        assert_eq!(queued.runtime_events_accepted, 1);
        assert_eq!(queued.runtime_events_applied, 1);
        assert_eq!(queued.runtime_event_batch_sizes.observations, 1);
        assert_eq!(queued.runtime_event_batch_sizes.min, Some(1));
        assert_eq!(queued.runtime_event_batch_sizes.max, Some(1));
        assert_eq!(queued.runtime_side_effects_queued, 1);
        assert_eq!(queued.runtime_side_effects_drained, 0);
        assert_eq!(queued.runtime_side_effect_enqueue_sizes.observations, 1);
        assert_eq!(queued.runtime_side_effect_enqueue_sizes.max, Some(1));
        assert_eq!(queued.pane_output_chunks, 1);
        assert_eq!(
            queued.pane_output_bytes,
            u64::try_from(b"metrics-output\n".len()).unwrap()
        );
        assert_eq!(queued.pane_output_chunk_bytes.observations, 1);
        assert_eq!(
            queued.pane_output_chunk_bytes.max,
            Some(u64::try_from(b"metrics-output\n".len()).unwrap())
        );
        assert_eq!(queued.side_effect_queue_depth, 1);
        assert_eq!(queued.side_effect_queue_high_water, 1);
        assert_eq!(queued.side_effect_queue_depth_samples.max, Some(1));
        assert_eq!(queued.side_effect_delivery_notifications, 1);

        assert_eq!(
            handle.drain_runtime_side_effects(8).await.unwrap(),
            vec![RuntimeSideEffect::RenderClient {
                client_id: primary,
                reason: RenderInvalidationReason::PaneOutput,
            }]
        );

        let drained = handle.metrics().await.unwrap();
        assert_eq!(drained.commands_processed, 4);
        assert_eq!(drained.runtime_side_effects_drained, 1);
        assert_eq!(drained.runtime_side_effect_drain_sizes.observations, 1);
        assert_eq!(drained.runtime_side_effect_drain_sizes.max, Some(1));
        assert_eq!(drained.side_effect_queue_depth, 0);
        assert_eq!(drained.side_effect_queue_high_water, 1);
        assert!(drained.side_effect_queue_depth_samples.observations >= 2);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.commands_processed, 5);
    assert_eq!(exit.metrics.runtime_event_batches, 1);
    assert_eq!(exit.metrics.runtime_side_effects_drained, 1);
    assert_eq!(exit.metrics.pane_output_chunks, 1);
    assert_eq!(
        exit.metrics.pane_output_bytes,
        u64::try_from(b"metrics-output\n".len()).unwrap()
    );
    assert_eq!(exit.metrics.runtime_event_batch_sizes.max, Some(1));
    assert_eq!(exit.metrics.runtime_side_effect_enqueue_sizes.max, Some(1));
    assert_eq!(exit.metrics.runtime_side_effect_drain_sizes.max, Some(1));
    assert_eq!(
        exit.metrics.pane_output_chunk_bytes.max,
        Some(u64::try_from(b"metrics-output\n".len()).unwrap())
    );
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that compatibility-style service methods called through the async
/// actor defer registry persistence into a single worker side effect. Primary
/// disconnect paths can request registry persistence internally, and the actor
/// must not add a duplicate registry write for the same event batch.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_defers_compatibility_registry_updates_once() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-registry-compat-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let registry = SessionRegistry::new(root.clone(), current_effective_uid());
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.set_session_registry(registry.clone());
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Client(ClientEvent::Disconnected {
            client_id: primary,
            reason: "test disconnect".to_string(),
        }));
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);

        let effects = handle.drain_persistence_side_effects(8).await.unwrap();
        let registry_effects = effects
            .iter()
            .filter(|effect| matches!(effect, RuntimeSideEffect::PersistRegistry { .. }))
            .count();
        assert_eq!(registry_effects, 1);
        handle.queue_runtime_side_effects(effects).await.unwrap();

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
        assert_eq!(registry.list().unwrap().len(), 1);
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 6);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that supervisor shutdown events are applied through typed runtime
/// ingress even when there is no active primary client. This is the async
/// supervisor path for forced daemon shutdown, failed critical services, and
/// signal handling where the multiplexer must terminate live panes and remove the
/// registry record without routing through a primary-owned control command.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_forced_shutdown_events_without_primary() {
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .detach_primary(&primary, Size::new(80, 24).unwrap())
        .unwrap();
    assert_eq!(service.lifecycle_state(), RuntimeLifecycleState::Detached);

    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Shutdown(ShutdownEvent {
            reason: "test supervisor shutdown".to_string(),
            force: true,
            failed: false,
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 0);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Killed
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(
        exit.service.lifecycle_state(),
        RuntimeLifecycleState::Killed
    );
    assert!(exit.service.session().windows().is_empty());
    assert!(
        exit.service
            .session()
            .clients()
            .iter()
            .all(|client| client.state != ClientState::Attached)
    );
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""lifecycle":"shutdown""#)
            && event.payload.contains("test supervisor shutdown")
    }));
    assert_eq!(exit.commands_processed, 2);
}

/// Verifies that non-forced supervisor shutdown requests apply through the
/// same typed runtime event ingress as forced shutdown without detaching the
/// primary client or killing panes. This covers graceful supervisor paths where
/// peer services should observe the stopping lifecycle before any later forced
/// cleanup decision.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_graceful_shutdown_events() {
    let mut service = test_service_with_event_log();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Shutdown(ShutdownEvent {
            reason: "test graceful shutdown".to_string(),
            force: false,
            failed: false,
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Stopping
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(
        exit.service.lifecycle_state(),
        RuntimeLifecycleState::Stopping
    );
    assert!(
        exit.service
            .session()
            .clients()
            .iter()
            .any(|client| client.state == ClientState::Attached)
    );
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary)
        .iter()
        .map(|event| event.payload.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(events.contains(r#""lifecycle":"stopping""#), "{events}");
    assert!(
        events.contains(r#""shutdown_reason":"test graceful shutdown""#),
        "{events}"
    );
    assert!(events.contains(r#""force":false"#), "{events}");
    assert_eq!(exit.commands_processed, 2);
}

/// Verifies that supervisor failure shutdown events are represented as failed
/// runtime state rather than being collapsed into graceful stopping or forced
/// kill semantics. This gives the Tokio supervisor a typed failure path for
/// critical-service failures while still recording a replayable lifecycle
/// diagnostic and notifying attached clients through render side effects.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_failed_shutdown_events() {
    let mut service = test_service_with_event_log();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Shutdown(ShutdownEvent {
            reason: "critical service failed".to_string(),
            force: false,
            failed: true,
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Failed
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert_eq!(
        exit.service.lifecycle_state(),
        RuntimeLifecycleState::Failed
    );
    assert_eq!(
        exit.service.session().state,
        mez_mux::session::SessionState::Failed
    );
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary)
        .iter()
        .map(|event| event.payload.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(events.contains(r#""lifecycle":"failed""#), "{events}");
    assert!(
        events.contains(r#""shutdown_reason":"critical service failed""#),
        "{events}"
    );
    assert!(events.contains(r#""force":false"#), "{events}");
    assert_eq!(exit.commands_processed, 2);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that actor-applied lifecycle events defer non-blocking configured
/// program hooks as hook-worker side effects. Blocking pre-action hooks remain
/// synchronous for now, but completed lifecycle hooks should no longer spawn
/// hook processes inside the actor.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_defers_completed_program_hooks_to_hook_worker() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-hook-deferral-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let payload_path = root.join("detach.json");
    let mut service = test_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                "[hooks.detach]\nevent = \"client_detach\"\nprogram = \"/bin/sh\"\nargs = [\"-c\", \"cat > \\\"$1\\\"\", \"hook\", \"{}\"]\non_failure = \"warn\"\n",
                payload_path.display()
            ),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Client(ClientEvent::Disconnected {
            client_id: primary,
            reason: "test disconnect".to_string(),
        }));
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert!(report.side_effects >= 1);

        let effects = handle.drain_hook_side_effects(8).await.unwrap();
        assert_eq!(effects.len(), 1);
        assert!(matches!(
            &effects[0],
            RuntimeSideEffect::RunProgramHook { plan, triggering_event_completed: true }
                if plan.hook_id == "detach"
        ));
        assert!(!payload_path.exists());
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 3);
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
            .take_running_pane_processes_for_adapter(8)
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
