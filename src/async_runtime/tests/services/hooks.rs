//! Async-runtime tests owned by hooks behavior.

use super::super::*;

/// Verifies that program hook side effects are executed by the async hook
/// worker and reported back through typed actor events. This keeps lifecycle
/// hook process latency out of the actor while preserving ordered runtime
/// application of hook results.
#[tokio::test(flavor = "current_thread")]
async fn async_hook_side_effect_service_executes_program_hooks() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-hook-complete-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let payload_path = root.join("payload.json");
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();

    let client = async {
        let queued = handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::RunProgramHook {
                plan: Box::new(HookExecutionPlan {
                    hook_id: "async-hook".to_string(),
                    event: HookEvent::ClientDetach,
                    run_in_focused_shell: false,
                    target_pane_id: None,
                    blocks_on_shell_availability: false,
                    program: Some("/bin/sh".to_string()),
                    args: vec![
                        "-c".to_string(),
                        "cat > \"$1\"".to_string(),
                        "hook".to_string(),
                        payload_path.display().to_string(),
                    ],
                    shell_command: None,
                    event_payload_json: r#"{"client_id":"primary"}"#.to_string(),
                    timeout_ms: 1_000,
                    on_failure: HookOnFailure::Warn,
                }),
                triggering_event_completed: true,
            }])
            .await
            .unwrap();
        assert_eq!(queued, 1);

        let report = run_async_hook_side_effect_service(
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
        assert_eq!(report.completed, 1);
        assert_eq!(report.failed, 0);
        assert_eq!(report.submitted_events, 1);
        assert_eq!(report.applied_events, 1);
        assert_eq!(
            std::fs::read_to_string(&payload_path).unwrap(),
            r#"{"client_id":"primary"}"#
        );
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}
