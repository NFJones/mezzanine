//! Async-runtime tests owned by timers behavior.

use super::super::*;

/// Verifies that timer side effects are consumed by the timer worker rather
/// than remaining as inert actor queue entries. The scheduled provider-poll
/// timer must re-enter the actor as a typed `TimerEvent`, which then produces a
/// provider-dispatch side effect through the same path used by direct timer
/// ingress.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_timer_side_effect_service_fires_scheduled_timers() {
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
    service
        .execute_agent_shell_command(&primary, "summarize the pane")
        .unwrap();
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    let expected_agent = AgentId::opaque(pending[0].agent_id.clone()).unwrap();
    let expected_turn = pending[0].turn_id.clone();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let key = RuntimeTimerKey::new(RuntimeTimerKind::ProviderPoll, "agent-provider", 1);
        let queued = handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::ScheduleTimer { key, delay_ms: 1 }])
            .await
            .unwrap();
        assert_eq!(queued, 1);

        let timer = run_async_runtime_timer_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 4,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            100,
            |polls, _| polls >= 4,
        );
        let clock = async {
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(1)).await;
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(1)).await;
        };
        let (report, ()) = tokio::join!(timer, clock);
        let report = report.unwrap();
        assert_eq!(report.drained, 1);
        assert_eq!(report.scheduled, 1);
        assert_eq!(report.fired, 1);
        assert_eq!(report.submitted_events, 1);
        assert_eq!(report.applied_events, 1);

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
        handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 3);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies that cancelled runtime timers are removed before they can emit
/// stale events. This prevents old readiness, shell transaction, or resize
/// generations from racing later actor state after a newer timer supersedes
/// them.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_timer_side_effect_service_cancels_scheduled_timers() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();

    let client = async {
        let key = RuntimeTimerKey::new(RuntimeTimerKind::CursorBlink, "primary", 9);
        let queued = handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::ScheduleTimer {
                    key: key.clone(),
                    delay_ms: 1,
                },
                RuntimeSideEffect::CancelTimer { key },
            ])
            .await
            .unwrap();
        assert_eq!(queued, 2);

        let timer = run_async_runtime_timer_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            0,
            |polls, _| polls >= 2,
        );
        let clock = async {
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(1)).await;
        };
        let (report, ()) = tokio::join!(timer, clock);
        let report = report.unwrap();
        assert_eq!(report.drained, 2);
        assert_eq!(report.scheduled, 1);
        assert_eq!(report.cancelled, 1);
        assert_eq!(report.fired, 0);
        assert_eq!(report.submitted_events, 0);
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 4);
    assert_eq!(exit.metrics.runtime_timer_schedules_queued, 1);
    assert_eq!(exit.metrics.runtime_timer_cancellations_queued, 1);
}
