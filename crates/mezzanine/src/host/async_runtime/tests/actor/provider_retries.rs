//! Async-runtime tests owned by provider retries behavior.

use super::super::*;

/// Verifies that provider-poll timer events convert pending provider work into
/// bounded dispatch side effects without executing provider I/O inside the
/// actor. The second poll before a drain proves the actor does not enqueue
/// duplicate dispatch requests for a turn that already has a dispatch side
/// effect waiting for a supervised provider worker. A queued render effect is
/// left behind for the render worker, proving provider dispatch drains do not
/// steal work from other side-effect families.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_queues_provider_dispatch_side_effects_for_provider_poll_timer() {
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
    let expected_agent = AgentId::opaque(task.agent_id.clone()).unwrap();
    let expected_turn = task.turn_id.clone();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let key = RuntimeTimerKey::new(RuntimeTimerKind::ProviderPoll, "agent-provider", 1);
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::ScheduleTimer {
                key: key.clone(),
                delay_ms: 1,
            }])
            .await
            .unwrap();
        assert_eq!(handle.drain_timer_side_effects(8).await.unwrap().len(), 1);

        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Timer(TimerEvent {
            key: key.clone(),
            now_ms: 1,
        }));
        let first = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(first.accepted, 1);
        assert_eq!(first.applied, 1);
        assert_eq!(first.side_effects, 1);

        let mut duplicate = RuntimeEventBatch::new();
        duplicate.push(RuntimeEvent::Timer(TimerEvent { key, now_ms: 2 }));
        let second = handle.submit_runtime_events(duplicate).await.unwrap();
        assert_eq!(second.accepted, 1);
        assert_eq!(second.applied, 0);
        assert_eq!(second.side_effects, 0);

        let mut render_batch = RuntimeEventBatch::new();
        render_batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"provider-side-effect-preserve\n".to_vec(),
        }));
        let render_report = handle.submit_runtime_events(render_batch).await.unwrap();
        assert_eq!(render_report.accepted, 1);
        assert_eq!(render_report.applied, 1);
        assert_eq!(render_report.side_effects, 2);

        let effects = handle
            .drain_agent_provider_dispatch_side_effects(8)
            .await
            .unwrap();
        assert_eq!(
            effects,
            vec![RuntimeSideEffect::DispatchAgentProvider {
                agent_id: expected_agent,
                turn_id: expected_turn,
            }]
        );
        assert_eq!(
            handle.drain_render_side_effects(8).await.unwrap(),
            vec![RuntimeSideEffect::RenderClient {
                client_id: primary,
                reason: RenderInvalidationReason::PaneOutput,
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
    assert_eq!(exit.metrics.runtime_event_batches, 3);
    assert_eq!(exit.metrics.runtime_side_effects_queued, 4);
    assert_eq!(exit.metrics.runtime_side_effects_drained, 4);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies that provider-poll timer events are generation checked before they
/// dispatch pending model work. Provider polling is a runtime-owned timer
/// family, so a cancelled stale deadline must not wake the provider worker or
/// enqueue duplicate provider dispatch side effects after a newer poll timer is
/// scheduled.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_ignores_stale_provider_poll_timer_events() {
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
    let expected_agent = AgentId::opaque(pending[0].agent_id.clone()).unwrap();
    let expected_turn = pending[0].turn_id.clone();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let stale_key = RuntimeTimerKey::new(RuntimeTimerKind::ProviderPoll, "agent-provider", 1);
        let active_key = RuntimeTimerKey::new(RuntimeTimerKind::ProviderPoll, "agent-provider", 2);
        handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::ScheduleTimer {
                    key: stale_key.clone(),
                    delay_ms: 1,
                },
                RuntimeSideEffect::CancelTimer {
                    key: stale_key.clone(),
                },
                RuntimeSideEffect::ScheduleTimer {
                    key: active_key.clone(),
                    delay_ms: 1,
                },
            ])
            .await
            .unwrap();
        assert_eq!(handle.drain_timer_side_effects(8).await.unwrap().len(), 3);

        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Timer(TimerEvent {
            key: stale_key,
            now_ms: 1,
        }));
        batch.push(RuntimeEvent::Timer(TimerEvent {
            key: active_key,
            now_ms: 1,
        }));
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 2);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);

        let dispatches = handle
            .drain_agent_provider_dispatch_side_effects(8)
            .await
            .unwrap();
        assert_eq!(
            dispatches,
            vec![RuntimeSideEffect::DispatchAgentProvider {
                agent_id: expected_agent,
                turn_id: expected_turn,
            }]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.metrics.runtime_side_effects_queued >= 4);
    assert!(exit.metrics.runtime_side_effects_drained >= 4);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies that retryable provider failures schedule a per-turn retry timer
/// instead of immediately requeueing provider work. This keeps retry/backoff as
/// an explicit actor-owned timer transition rather than another provider-poll
/// fallback that can wake too early.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_schedules_provider_retry_timer_for_retryable_failure() {
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
    let expected_agent = AgentId::opaque(pending[0].agent_id.clone()).unwrap();
    let expected_turn = pending[0].turn_id.clone();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut failure = RuntimeEventBatch::new();
        failure.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Failed {
            agent_id: expected_agent.clone(),
            turn_id: expected_turn.clone(),
            kind: "invalid_state".to_string(),
            message: "provider HTTP request failed: rate limited".to_string(),
            provider_failure_json: Some(r#"{"status_code":429}"#.to_string()),
            provider_raw_text: None,
        }));
        let failure_report = handle.submit_runtime_events(failure).await.unwrap();
        assert_eq!(failure_report.accepted, 1);
        assert_eq!(failure_report.applied, 1);
        assert!(
            handle
                .pending_agent_provider_tasks()
                .await
                .unwrap()
                .is_empty(),
            "retryable failure should wait for the retry timer before requeueing provider work"
        );
        assert!(
            handle
                .drain_agent_provider_dispatch_side_effects(8)
                .await
                .unwrap()
                .is_empty(),
            "retryable failure must not dispatch provider work before the retry timer fires"
        );

        let timer_effects = handle.drain_timer_side_effects(8).await.unwrap();
        let (retry_key, retry_delay_ms) = timer_effects
            .iter()
            .find_map(|effect| match effect {
                RuntimeSideEffect::ScheduleTimer { key, delay_ms }
                    if key.kind == RuntimeTimerKind::ProviderRetry =>
                {
                    Some((key.clone(), *delay_ms))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected provider retry timer, got {timer_effects:?}"));
        assert_eq!(retry_key.owner_id, expected_turn);
        assert_eq!(retry_key.generation, 1);
        assert_eq!(retry_delay_ms, 1_000);

        let mut retry = RuntimeEventBatch::new();
        retry.push(RuntimeEvent::Timer(TimerEvent {
            key: retry_key,
            now_ms: 1_000,
        }));
        let retry_report = handle.submit_runtime_events(retry).await.unwrap();
        assert_eq!(retry_report.accepted, 1);
        assert_eq!(retry_report.applied, 1);

        let dispatches = handle
            .drain_agent_provider_dispatch_side_effects(8)
            .await
            .unwrap();
        assert_eq!(
            dispatches,
            vec![RuntimeSideEffect::DispatchAgentProvider {
                agent_id: expected_agent,
                turn_id: expected_turn,
            }]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.metrics.runtime_side_effects_queued >= 2);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies rate-limited provider failures receive the full bounded exponential
/// retry policy before the runtime records a terminal turn failure.
///
/// The backlog regression was a 429 response that failed the turn after the
/// earlier shorter retry budget. This test drives five consecutive rate-limit
/// failures through the actor-owned retry timer path, checks the backoff delay
/// for every scheduled retry, and verifies the sixth failure exhausts the
/// policy instead of scheduling an extra retry.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_retries_rate_limits_five_times_with_exponential_backoff() {
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
    let expected_agent = AgentId::opaque(pending[0].agent_id.clone()).unwrap();
    let expected_turn = pending[0].turn_id.clone();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let expected_delays = [1_000, 2_000, 4_000, 8_000, 16_000];
        for (index, expected_delay_ms) in expected_delays.into_iter().enumerate() {
            let attempt = index + 1;
            let mut failure = RuntimeEventBatch::new();
            failure.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Failed {
                agent_id: expected_agent.clone(),
                turn_id: expected_turn.clone(),
                kind: "invalid_state".to_string(),
                message: "provider HTTP request failed: rate limited".to_string(),
                provider_failure_json: Some(r#"{"status_code":429}"#.to_string()),
                provider_raw_text: None,
            }));
            let failure_report = handle.submit_runtime_events(failure).await.unwrap();
            assert_eq!(failure_report.accepted, 1);
            assert_eq!(failure_report.applied, 1);

            let timer_effects = handle.drain_timer_side_effects(8).await.unwrap();
            let (retry_key, retry_delay_ms) = timer_effects
                .iter()
                .find_map(|effect| match effect {
                    RuntimeSideEffect::ScheduleTimer { key, delay_ms }
                        if key.kind == RuntimeTimerKind::ProviderRetry =>
                    {
                        Some((key.clone(), *delay_ms))
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("expected provider retry timer, got {timer_effects:?}"));
            assert_eq!(retry_key.owner_id, expected_turn);
            assert_eq!(retry_key.generation, attempt as u64);
            assert_eq!(retry_delay_ms, expected_delay_ms);

            let mut retry = RuntimeEventBatch::new();
            retry.push(RuntimeEvent::Timer(TimerEvent {
                key: retry_key,
                now_ms: expected_delay_ms,
            }));
            let retry_report = handle.submit_runtime_events(retry).await.unwrap();
            assert_eq!(retry_report.accepted, 1);
            assert_eq!(retry_report.applied, 1);

            let dispatches = handle
                .drain_agent_provider_dispatch_side_effects(8)
                .await
                .unwrap();
            assert_eq!(
                dispatches,
                vec![RuntimeSideEffect::DispatchAgentProvider {
                    agent_id: expected_agent.clone(),
                    turn_id: expected_turn.clone(),
                }]
            );
        }

        let mut exhausted = RuntimeEventBatch::new();
        exhausted.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Failed {
            agent_id: expected_agent,
            turn_id: expected_turn,
            kind: "invalid_state".to_string(),
            message: "provider HTTP request failed: rate limited".to_string(),
            provider_failure_json: Some(r#"{"status_code":429}"#.to_string()),
            provider_raw_text: None,
        }));
        let exhausted_report = handle.submit_runtime_events(exhausted).await.unwrap();
        assert_eq!(exhausted_report.accepted, 1);
        assert_eq!(exhausted_report.applied, 1);
        assert!(
            handle.drain_timer_side_effects(8).await.unwrap().is_empty(),
            "exhausted retry budget must not schedule another timer"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.metrics.runtime_side_effects_queued >= 10);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies provider output-limit failures use the retry timer path after
/// mutating only active-turn retry guidance for the first retry.
///
/// This protects OpenAI `response.incomplete/max_output_tokens` events from
/// becoming terminal turn failures or context-compaction triggers before the
/// later retry-budget escalation stage.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_recovers_output_limit_failure_before_provider_retry() {
    let mut service = test_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "async-output-limit-recovery".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "openai"
default_model_profile = "default"
[providers.openai]
kind = "openai"
models = ["gpt-test"]
default_model = "gpt-test"
[model_profiles.default]
provider = "openai"
model = "gpt-test"
max_output_tokens = 4096
"#
            .to_string(),
        }])
        .unwrap();
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
        .execute_agent_shell_command(&primary, "continue the implementation")
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
        let mut failure = RuntimeEventBatch::new();
        failure.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Failed {
            agent_id: expected_agent.clone(),
            turn_id: expected_turn.clone(),
            kind: "invalid_state".to_string(),
            message: "OpenAI stream returned an incomplete response: max_output_tokens".to_string(),
            provider_failure_json: Some(
                r#"{"incomplete_details":{"reason":"max_output_tokens"}}"#.to_string(),
            ),
            provider_raw_text: None,
        }));
        let failure_report = handle.submit_runtime_events(failure).await.unwrap();
        assert_eq!(failure_report.accepted, 1);
        assert_eq!(failure_report.applied, 1);
        assert!(
            handle
                .pending_agent_provider_tasks()
                .await
                .unwrap()
                .is_empty()
        );

        let timer_effects = handle.drain_timer_side_effects(8).await.unwrap();
        let retry_key = timer_effects
            .iter()
            .find_map(|effect| match effect {
                RuntimeSideEffect::ScheduleTimer { key, delay_ms }
                    if key.kind == RuntimeTimerKind::ProviderRetry =>
                {
                    assert_eq!(*delay_ms, 1_000);
                    Some(key.clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected provider retry timer, got {timer_effects:?}"));
        assert_eq!(retry_key.owner_id, expected_turn);

        let mut retry = RuntimeEventBatch::new();
        retry.push(RuntimeEvent::Timer(TimerEvent {
            key: retry_key,
            now_ms: 1_000,
        }));
        let retry_report = handle.submit_runtime_events(retry).await.unwrap();
        assert_eq!(retry_report.accepted, 1);
        assert_eq!(retry_report.applied, 1);

        let pending = handle.pending_agent_provider_tasks().await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].model_profile.max_output_tokens(), Some(4096));
        let dispatches = handle
            .drain_agent_provider_dispatch_side_effects(8)
            .await
            .unwrap();
        assert_eq!(
            dispatches,
            vec![RuntimeSideEffect::DispatchAgentProvider {
                agent_id: expected_agent,
                turn_id: expected_turn,
            }]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    let pane_text = exit
        .service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("provider response hit output limit; retrying with shorter-response"),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("provider rejected context as too large"),
        "{pane_text}"
    );
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies exhausted output-limit retries queue automatic compaction instead
/// of failing the running turn.
///
/// OpenAI Responses can finish with `response.incomplete` and
/// `incomplete_details.reason=max_output_tokens` after spending the output
/// budget on reasoning. The actor first uses the bounded provider retry path.
/// Once that budget is exhausted, it should queue model-backed conversation
/// compaction so the same task can resume from compacted context rather than
/// recording a terminal MAAP failure.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_queues_compaction_after_output_limit_retry_exhaustion() {
    let mut service = test_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "async-output-limit-auto-compaction".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "openai"
default_model_profile = "default"
[providers.openai]
kind = "openai"
models = ["gpt-test"]
default_model = "gpt-test"
[model_profiles.default]
provider = "openai"
model = "gpt-test"
max_output_tokens = 4096
context_window_tokens = 128000
"#
            .to_string(),
        }])
        .unwrap();
    let transcript_root = std::env::temp_dir().join(format!(
        "mez-async-output-limit-auto-compact-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&transcript_root).unwrap();
    let transcript_store = AgentTranscriptStore::new(transcript_root);
    for sequence in 1..=6 {
        transcript_store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: "async-output-limit-auto".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role: mez_agent::transcript::TranscriptRole::Assistant,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: format!("durable prior output-limit entry {sequence}"),
            })
            .unwrap();
    }
    service.set_agent_transcript_store(transcript_store);
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
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "async-output-limit-auto", 6)
        .unwrap();
    let start = service
        .execute_agent_shell_command(&primary, "continue the implementation")
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
        let retry_delays = [1_000, 2_000, 4_000, 8_000, 16_000];
        for (index, expected_delay_ms) in retry_delays.into_iter().enumerate() {
            let mut failure = RuntimeEventBatch::new();
            failure.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Failed {
                agent_id: expected_agent.clone(),
                turn_id: expected_turn.clone(),
                kind: "invalid_state".to_string(),
                message: "OpenAI stream returned an incomplete response: max_output_tokens"
                    .to_string(),
                provider_failure_json: Some(
                    r#"{"incomplete_details":{"reason":"max_output_tokens"}}"#.to_string(),
                ),
                provider_raw_text: None,
            }));
            let failure_report = handle.submit_runtime_events(failure).await.unwrap();
            assert_eq!(failure_report.accepted, 1);
            assert_eq!(failure_report.applied, 1);

            let timer_effects = handle.drain_timer_side_effects(8).await.unwrap();
            let retry_key = timer_effects
                .iter()
                .find_map(|effect| match effect {
                    RuntimeSideEffect::ScheduleTimer { key, delay_ms }
                        if key.kind == RuntimeTimerKind::ProviderRetry =>
                    {
                        assert_eq!(*delay_ms, expected_delay_ms);
                        Some(key.clone())
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("expected provider retry timer, got {timer_effects:?}"));
            assert_eq!(retry_key.owner_id, expected_turn);
            assert_eq!(retry_key.generation, (index + 1) as u64);

            let mut retry = RuntimeEventBatch::new();
            retry.push(RuntimeEvent::Timer(TimerEvent {
                key: retry_key,
                now_ms: expected_delay_ms,
            }));
            let retry_report = handle.submit_runtime_events(retry).await.unwrap();
            assert_eq!(retry_report.accepted, 1);
            assert_eq!(retry_report.applied, 1);

            let dispatches = handle
                .drain_agent_provider_dispatch_side_effects(8)
                .await
                .unwrap();
            assert!(dispatches.iter().any(|effect| matches!(
                effect,
                RuntimeSideEffect::DispatchAgentProvider { turn_id, .. }
                    if turn_id == &expected_turn
            )));
        }

        let mut exhausted = RuntimeEventBatch::new();
        exhausted.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Failed {
            agent_id: expected_agent,
            turn_id: expected_turn.clone(),
            kind: "invalid_state".to_string(),
            message: "OpenAI stream returned an incomplete response: max_output_tokens".to_string(),
            provider_failure_json: Some(
                r#"{"incomplete_details":{"reason":"max_output_tokens"}}"#.to_string(),
            ),
            provider_raw_text: None,
        }));
        let exhausted_report = handle.submit_runtime_events(exhausted).await.unwrap();
        assert_eq!(exhausted_report.accepted, 1);
        assert_eq!(exhausted_report.applied, 1);
        assert!(
            handle.drain_timer_side_effects(8).await.unwrap().is_empty(),
            "exhausted output-limit recovery should not schedule another retry timer"
        );

        let dispatches = handle
            .drain_agent_provider_dispatch_side_effects(8)
            .await
            .unwrap();
        assert!(dispatches.iter().any(|effect| matches!(
            effect,
            RuntimeSideEffect::DispatchAgentCompaction { pane_id } if pane_id == "%1"
        )));
        assert!(
            handle
                .pending_agent_provider_tasks()
                .await
                .unwrap()
                .is_empty(),
            "provider work should wait until compaction completion refreshes context"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    let pane_text = exit
        .service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("provider output-limit retries exhausted; compacting conversation"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("agent: turn turn-"), "{pane_text}");
    assert_eq!(
        exit.service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        Some(expected_turn.as_str())
    );
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies idle cleanup does not fail turns whose only progress path is an
/// actor-owned provider retry timer.
///
/// Retry timers are tracked by the async actor instead of
/// `RuntimeSessionService`. If an idle-cleanup timer fires while the turn is
/// waiting for retry backoff, service-level progress reconciliation must treat
/// that actor-owned timer as valid progress rather than failing the running
/// turn as unreachable.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_idle_cleanup_preserves_turn_waiting_for_provider_retry_timer() {
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
    let expected_agent = AgentId::opaque(pending[0].agent_id.clone()).unwrap();
    let expected_turn = pending[0].turn_id.clone();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut failure = RuntimeEventBatch::new();
        failure.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Failed {
            agent_id: expected_agent.clone(),
            turn_id: expected_turn.clone(),
            kind: "invalid_state".to_string(),
            message: "provider HTTP request failed: rate limited".to_string(),
            provider_failure_json: Some(r#"{"status_code":429}"#.to_string()),
            provider_raw_text: None,
        }));
        let failure_report = handle.submit_runtime_events(failure).await.unwrap();
        assert_eq!(failure_report.accepted, 1);
        assert_eq!(failure_report.applied, 1);

        let timer_effects = handle.drain_timer_side_effects(8).await.unwrap();
        let retry_key = timer_effects
            .iter()
            .find_map(|effect| match effect {
                RuntimeSideEffect::ScheduleTimer { key, delay_ms }
                    if key.kind == RuntimeTimerKind::ProviderRetry =>
                {
                    assert_eq!(*delay_ms, 1_000);
                    Some(key.clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected provider retry timer, got {timer_effects:?}"));
        assert_eq!(retry_key.owner_id, expected_turn);

        let idle_key = RuntimeTimerKey::new(RuntimeTimerKind::IdleCleanup, "session", 42);
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::ScheduleTimer {
                key: idle_key.clone(),
                delay_ms: 1,
            }])
            .await
            .unwrap();
        let idle_effects = handle.drain_timer_side_effects(8).await.unwrap();
        assert!(
            idle_effects.iter().any(|effect| matches!(
                effect,
                RuntimeSideEffect::ScheduleTimer { key, .. } if key == &idle_key
            )),
            "{idle_effects:?}"
        );

        let mut idle = RuntimeEventBatch::new();
        idle.push(RuntimeEvent::Timer(TimerEvent {
            key: idle_key,
            now_ms: 500,
        }));
        let idle_report = handle.submit_runtime_events(idle).await.unwrap();
        assert_eq!(idle_report.accepted, 1);
        assert!(
            handle
                .pending_agent_provider_tasks()
                .await
                .unwrap()
                .is_empty(),
            "idle cleanup must not requeue provider work before retry backoff"
        );
        assert!(
            handle
                .drain_agent_provider_dispatch_side_effects(8)
                .await
                .unwrap()
                .is_empty(),
            "idle cleanup must not dispatch provider work before retry backoff"
        );

        let mut retry = RuntimeEventBatch::new();
        retry.push(RuntimeEvent::Timer(TimerEvent {
            key: retry_key,
            now_ms: 1_000,
        }));
        let retry_report = handle.submit_runtime_events(retry).await.unwrap();
        assert_eq!(retry_report.accepted, 1);
        assert_eq!(retry_report.applied, 1);

        let dispatches = handle
            .drain_agent_provider_dispatch_side_effects(8)
            .await
            .unwrap();
        assert_eq!(
            dispatches,
            vec![RuntimeSideEffect::DispatchAgentProvider {
                agent_id: expected_agent,
                turn_id: expected_turn,
            }]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    let pane_text = exit
        .service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        !pane_text.contains("runtime found no remaining progress path"),
        "{pane_text}"
    );
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies OpenAI-style provider/controller failures that explicitly invite
/// retry follow the same bounded retry timer path as transport and rate-limit
/// failures.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_schedules_provider_retry_timer_for_controller_retry_hint() {
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
    let expected_turn = pending[0].turn_id.clone();
    let expected_agent = AgentId::opaque(pending[0].agent_id.clone()).unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let retry_message = "An error occurred while processing your request. You can retry your request, or contact us through our help center at help.openai.com if the error persists. Please include the request ID b331baf5-b254-46d7-8d3f-58b563ce7ee8 in your message.";
        let mut failure = RuntimeEventBatch::new();
        failure.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Failed {
            agent_id: expected_agent,
            turn_id: expected_turn.clone(),
            kind: "invalid_state".to_string(),
            message: retry_message.to_string(),
            provider_failure_json: Some(
                serde_json::json!({
                    "error": {
                        "message": retry_message,
                        "type": "server_error"
                    }
                })
                .to_string(),
            ),
            provider_raw_text: None,
        }));
        let failure_report = handle.submit_runtime_events(failure).await.unwrap();
        assert_eq!(failure_report.accepted, 1);
        assert_eq!(failure_report.applied, 1);

        let timer_effects = handle.drain_timer_side_effects(8).await.unwrap();
        let retry_key = timer_effects
            .iter()
            .find_map(|effect| match effect {
                RuntimeSideEffect::ScheduleTimer { key, delay_ms }
                    if key.kind == RuntimeTimerKind::ProviderRetry =>
                {
                    assert_eq!(*delay_ms, 1_000);
                    Some(key.clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected provider retry timer, got {timer_effects:?}"));
        assert_eq!(retry_key.owner_id, expected_turn);
        assert_eq!(retry_key.generation, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.metrics.runtime_side_effects_queued >= 1);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies that non-retryable provider failures still fail the active turn and
/// do not schedule delayed provider retry timers. Authentication and validation
/// failures must be visible immediately instead of being hidden behind backoff.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_fails_non_retryable_provider_failures_without_retry_timer() {
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
    let expected_agent = AgentId::opaque(pending[0].agent_id.clone()).unwrap();
    let expected_turn = pending[0].turn_id.clone();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut failure = RuntimeEventBatch::new();
        failure.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Failed {
            agent_id: expected_agent,
            turn_id: expected_turn,
            kind: "invalid_state".to_string(),
            message: "OpenAI provider returned 401 Unauthorized: invalid token".to_string(),
            provider_failure_json: Some(r#"{"status_code":401}"#.to_string()),
            provider_raw_text: None,
        }));
        let failure_report = handle.submit_runtime_events(failure).await.unwrap();
        assert_eq!(failure_report.accepted, 1);
        assert_eq!(failure_report.applied, 1);
        assert!(
            handle
                .pending_agent_provider_tasks()
                .await
                .unwrap()
                .is_empty()
        );
        let timer_effects = handle.drain_timer_side_effects(8).await.unwrap();
        assert!(
            !timer_effects.iter().any(|effect| matches!(
                effect,
                RuntimeSideEffect::ScheduleTimer { key, .. }
                    if key.kind == RuntimeTimerKind::ProviderRetry
            )),
            "{timer_effects:?}"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(
        exit.service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref())
            .is_none()
    );
    assert_eq!(exit.service.agent_scheduler().snapshot().running, 0);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies model-correctable file-action failures enqueue the next provider
/// dispatch immediately through the async actor.
///
/// The runtime service stores the retry context, but the async actor owns
/// side-effect dispatch. A failed provider completion that queues action
/// failure feedback must therefore emit a fresh provider-dispatch side effect
/// instead of waiting for an unrelated timer path before the model can
/// self-correct.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_dispatches_provider_retry_after_file_action_failure_feedback() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service
        .execute_agent_shell_command(&primary, "write then inspect")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let task = service
        .pending_agent_provider_tasks()
        .into_iter()
        .next()
        .expect("prompt should queue a provider task");
    let turn = mez_agent::AgentTurnRecord {
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        pane_id: task.pane_id.clone(),
        trigger: mez_agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 2,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        state: mez_agent::AgentTurnState::Running,
        cooperation_mode: None,

        initial_capability: None,
    };
    let write_action = mez_agent::AgentAction {
        id: "patch-fail".to_string(),
        rationale: "write a source file".to_string(),
        payload: mez_agent::AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** Add File: src/generated.rs\n+content\n*** End Patch"
                .to_string(),
            strip: None,
        },
    };
    let read_action = mez_agent::AgentAction {
        id: "read-unsent".to_string(),
        rationale: "read the source file".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Read the source file".to_string(),
            command: "sed -n '1,120p' src/generated.rs".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    let mut failed = mez_agent::ActionResult::failed(
        &turn,
        &write_action,
        mez_agent::ActionStatus::Failed,
        "pane_input_write_failed",
        "pane input write failed while sending shell action",
    )
    .unwrap();
    failed.structured_content_json = Some(
        serde_json::json!({
            "kind": "apply_patch",
            "terminal_observation": {
                "state": "pane-input-write-failed"
            }
        })
        .to_string(),
    );
    let pending = mez_agent::ActionResult::running(
        &turn,
        &read_action,
        vec!["local action accepted for pane execution".to_string()],
        Some(r#"{"state":"pending_dispatch"}"#.to_string()),
    );
    let batch = mez_agent::MaapBatch {
        protocol: "maap/1".to_string(),
        rationale: "test action batch rationale".to_string(),
        thought: None,
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        actions: vec![write_action, read_action],
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
                content: "write then inspect".to_string(),
            }],
        },
        response: mez_agent::ModelResponse {
            provider: task.model_profile.provider.clone(),
            model: task.model_profile.model.clone(),
            raw_text: "failed file action response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(batch),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![failed, pending],
        final_turn: false,
        terminal_state: mez_agent::AgentTurnState::Failed,
    };
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut provider_batch = RuntimeEventBatch::new();
        provider_batch.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
            agent_id: AgentId::opaque(task.agent_id.clone()).unwrap(),
            turn_id: task.turn_id.clone(),
            execution: Box::new(execution),
        }));
        let report = handle.submit_runtime_events(provider_batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        let dispatches = handle
            .drain_agent_provider_dispatch_side_effects(8)
            .await
            .unwrap();
        assert!(dispatches.iter().any(|effect| matches!(
            effect,
            RuntimeSideEffect::DispatchAgentProvider { turn_id, .. }
                if turn_id == &task.turn_id
        )));
        let pending = handle.pending_agent_provider_tasks().await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].turn_id, task.turn_id);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    let pane_text = exit
        .service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: action failed; asking model to recover"),
        "{pane_text}"
    );
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies transient provider-authored overload failures still enter the
/// runtime retry scheduler even when the provider surfaces them as 400-class
/// invalid-state errors.
///
/// Some OpenAI Responses-compatible backends report overload conditions through
/// provider-authored error payloads instead of 429 or 5xx statuses. This
/// regression proves the actor still classifies those failures as retryable
/// transport so the existing exponential backoff path owns recovery.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_retries_provider_overload_message_without_rate_limit_status() {
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
    assert!(
        start.contains(r#"state":"running""#) || start.contains(r#""state":"running""#),
        "{start}"
    );
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    let expected_agent = AgentId::opaque(pending[0].agent_id.clone()).unwrap();
    let expected_turn = pending[0].turn_id.clone();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut failure = RuntimeEventBatch::new();
        failure.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Failed {
            agent_id: expected_agent.clone(),
            turn_id: expected_turn.clone(),
            kind: "invalid_state".to_string(),
            message:
                "OpenAI Responses API returned status 400: API overloaded, please try again later."
                    .to_string(),
            provider_failure_json: Some(
                r#"{"status_code":400,"error":{"message":"API overloaded, please try again later."}}"#
                    .to_string(),
            ),
            provider_raw_text: None,
        }));
        let failure_report = handle.submit_runtime_events(failure).await.unwrap();
        assert_eq!(failure_report.accepted, 1);
        assert_eq!(failure_report.applied, 1);

        let timer_effects = handle.drain_timer_side_effects(8).await.unwrap();
        let (retry_key, retry_delay_ms) = timer_effects
            .iter()
            .find_map(|effect| match effect {
                RuntimeSideEffect::ScheduleTimer { key, delay_ms }
                    if key.kind == RuntimeTimerKind::ProviderRetry =>
                {
                    Some((key.clone(), *delay_ms))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected provider retry timer, got {timer_effects:?}"));
        assert_eq!(retry_key.owner_id, expected_turn);
        assert_eq!(retry_key.generation, 1);
        assert_eq!(retry_delay_ms, 1_000);

        let mut retry = RuntimeEventBatch::new();
        retry.push(RuntimeEvent::Timer(TimerEvent {
            key: retry_key,
            now_ms: retry_delay_ms,
        }));
        let retry_report = handle.submit_runtime_events(retry).await.unwrap();
        assert_eq!(retry_report.accepted, 1);
        assert_eq!(retry_report.applied, 1);

        let dispatches = handle
            .drain_agent_provider_dispatch_side_effects(8)
            .await
            .unwrap();
        assert_eq!(
            dispatches,
            vec![RuntimeSideEffect::DispatchAgentProvider {
                agent_id: expected_agent,
                turn_id: expected_turn,
            }]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.metrics.runtime_side_effects_queued >= 2);
    exit.service.terminate_all_pane_processes().unwrap();
}
