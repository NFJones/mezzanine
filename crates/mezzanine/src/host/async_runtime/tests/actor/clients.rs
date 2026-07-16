//! Async-runtime tests owned by clients behavior.

use super::super::*;

/// Verifies that a primary-client resize delivered through typed async runtime
/// events mutates authoritative terminal geometry through the actor instead of
/// the compatibility resize request. Resize events are high-frequency terminal
/// stimuli, so this guards the migration invariant that stale/non-primary
/// events are harmless while active primary events use the established pane
/// geometry and render-invalidation path.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_primary_client_resize_events() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Client(ClientEvent::Resize {
            client_id: primary,
            size: Size::new(100, 30).unwrap(),
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

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(
        exit.service.session().authoritative_size,
        Size::new(100, 30).unwrap()
    );
    assert_eq!(exit.commands_processed, 2);
}

/// Verifies that primary-client input delivered as a typed runtime event uses
/// the normal terminal planner and applies the resulting client step through
/// the serialized actor. This protects mux key handling during the migration
/// away from compatibility client-loop requests: the input bytes are external
/// stimuli, while split-pane state mutation remains actor-owned.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_primary_client_input_events() {
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
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Client(ClientEvent::Input {
            client_id: primary,
            bytes: b"\x01%".to_vec(),
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
    let pane_count = exit
        .service
        .session()
        .active_window()
        .map(|window| window.panes().len())
        .unwrap_or_default();
    assert_eq!(pane_count, 2);
    assert_eq!(exit.commands_processed, 2);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies that primary-client disconnects can be applied from async runtime
/// event ingress without a nested terminal loop owning detach behavior. This
/// covers fd hangup and attached-client task shutdown paths where the actor
/// must record a normal primary detach plus a diagnostic reason while leaving
/// stale observer or non-primary events non-mutating.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_primary_client_disconnect_events() {
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Client(ClientEvent::Disconnected {
            client_id: primary,
            reason: "terminal input hangup".to_string(),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 0);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Detached
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(
        exit.service.lifecycle_state(),
        RuntimeLifecycleState::Detached
    );
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
        .replay_for(&EventAudience::Primary)
        .iter()
        .map(|event| event.payload.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        events.contains(r#""client_disconnect":"primary""#),
        "{events}"
    );
    assert!(
        events.contains(r#""reason":"terminal input hangup""#),
        "{events}"
    );
    assert_eq!(exit.commands_processed, 2);
}
