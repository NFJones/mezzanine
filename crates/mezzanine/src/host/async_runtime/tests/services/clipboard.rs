//! Async-runtime tests owned by bounded host clipboard behavior.

use super::super::*;

/// Verifies a slow host clipboard helper runs outside serialized actor
/// ownership and reports one typed completion after its bounded worker exits.
///
/// A lifecycle heartbeat must complete while the helper is still sleeping. If
/// clipboard acquisition moves back into input application, this heartbeat
/// waits behind the subprocess and the regression fails.
#[tokio::test(flavor = "current_thread")]
async fn async_host_clipboard_worker_does_not_block_actor_heartbeats() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let clipboard = HostClipboard::commands(
        Vec::new(),
        vec![HostClipboardCommand::new(
            "sh",
            vec!["-c".to_string(), "sleep 0.2; printf clipboard".to_string()],
        )],
    )
    .with_read_limits(Duration::from_secs(1), 1024);

    let client = async {
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::ReadHostClipboard {
                generation: 1,
                plan: clipboard.read_plan(),
            }])
            .await
            .unwrap();
        let worker_handle = handle.clone();
        let worker = async move {
            run_async_host_clipboard_side_effect_service(
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
                .expect("actor heartbeat should not wait for clipboard acquisition")
                .unwrap()
        };

        let (report, lifecycle) = tokio::join!(worker, heartbeat);

        assert_eq!(lifecycle, RuntimeLifecycleState::Running);
        assert_eq!(report.drained, 1);
        assert_eq!(report.submitted_events, 1);
        assert_eq!(report.applied_events, 0);
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 5);
}
