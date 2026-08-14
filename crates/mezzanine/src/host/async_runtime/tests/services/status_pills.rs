//! Async-runtime tests owned by command-backed status-pill behavior.

use super::super::*;
use crate::host::async_runtime::run_async_status_pill_side_effect_service;

/// Verifies slow status-pill helpers run concurrently outside serialized actor
/// ownership while actor heartbeats remain responsive.
#[tokio::test(flavor = "current_thread")]
async fn async_status_pill_worker_does_not_block_actor_heartbeats() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let plans = ["first", "second"].map(|name| {
        crate::runtime::RuntimeStatusPillRefreshPlan::for_tests(
            name,
            1,
            "sleep 0.5; printf ready",
            1_000,
            32,
        )
    });

    let client = async {
        handle
            .queue_runtime_side_effects(
                plans
                    .into_iter()
                    .map(|plan| RuntimeSideEffect::RefreshStatusPill { plan })
                    .collect(),
            )
            .await
            .unwrap();
        let worker_handle = handle.clone();
        let worker = async move {
            run_async_status_pill_side_effect_service(
                &worker_handle,
                AsyncRuntimeSideEffectServiceConfig {
                    max_polls: 2,
                    drain_limit: 8,
                    idle_interval: Duration::from_millis(1),
                },
                |polls, _| polls >= 2,
            )
            .await
            .unwrap()
        };
        let heartbeat = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            tokio::time::timeout(Duration::from_millis(50), handle.lifecycle_state())
                .await
                .expect("actor heartbeat should not wait for status-pill helpers")
                .unwrap()
        };

        let started = Instant::now();
        let (report, lifecycle) = tokio::join!(worker, heartbeat);

        assert_eq!(lifecycle, RuntimeLifecycleState::Running);
        assert_eq!(report.drained, 2);
        assert_eq!(report.submitted_events, 2);
        assert_eq!(report.applied_events, 0);
        assert!(started.elapsed() < Duration::from_millis(750));
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 5);
}

/// Verifies an actor render schedules one refresh, the worker applies its
/// typed completion, and only the changed cached value invalidates status.
#[tokio::test(flavor = "current_thread")]
async fn async_status_pill_render_completion_updates_cached_status() {
    let mut service = test_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "async-status-pill".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r##"[frames.window]
right_status = "#{pill.used}"
[frames.window.pills.used]
label = "USED"
command = "printf ready"
interval_seconds = 60
initial = "boot"
timeout_ms = 1000
"##
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let initial = handle
            .render_client_view(
                ClientViewRole::Primary,
                Size::new(80, 24).unwrap(),
                TerminalClientLoopConfig::default(),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(initial.lines.iter().any(|line| line.contains("USED boot")));

        let report = run_async_status_pill_side_effect_service(
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
        assert_eq!(report.drained, 1);
        assert_eq!(report.submitted_events, 1);
        assert_eq!(report.applied_events, 1);
        assert_eq!(
            handle.drain_render_side_effects(8).await.unwrap(),
            vec![RuntimeSideEffect::RenderClient {
                client_id: primary,
                reason: RenderInvalidationReason::StatusLine,
            }]
        );

        let updated = handle
            .render_client_view(
                ClientViewRole::Primary,
                Size::new(80, 24).unwrap(),
                TerminalClientLoopConfig::default(),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(updated.lines.iter().any(|line| line.contains("USED ready")));
        assert!(
            handle
                .drain_status_pill_side_effects(8)
                .await
                .unwrap()
                .is_empty()
        );
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 7);
}
