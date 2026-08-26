//! Async-runtime tests owned by events behavior.

use super::super::*;

/// Verifies that the async runtime event model preserves the actor-facing
/// delivery order and exposes stable event-family names. The Tokio refactor will
/// eventually route client, pane, provider, process, hook, timer, and shutdown
/// stimuli through this model, so tests need a simple invariant that catches
/// accidental reordering or ad hoc string changes before production I/O starts
/// using the channel.
#[test]
fn async_runtime_event_batch_preserves_delivery_order() {
    let client_id = ClientId::parse('c', "c1").unwrap();
    let mut batch = RuntimeEventBatch::new();
    batch.push(RuntimeEvent::Client(ClientEvent::Input {
        client_id: client_id.clone(),
        bytes: b"abc".to_vec(),
    }));
    batch.push(RuntimeEvent::Pane(PaneEvent::Output {
        pane_id: "%1".to_string(),
        bytes: b"pane-output".to_vec(),
    }));
    batch.push(RuntimeEvent::Timer(TimerEvent {
        key: RuntimeTimerKey::new(RuntimeTimerKind::ShellTransaction, "turn-1", 7),
        now_ms: 42,
    }));

    assert_eq!(batch.families(), vec!["client", "pane", "timer"]);
    assert_eq!(batch.events[0].family(), "client");
    assert_eq!(batch.events[1].family(), "pane");
    assert_eq!(batch.events[2].family(), "timer");

    let effect = RuntimeSideEffect::RenderClient {
        client_id,
        reason: RenderInvalidationReason::FullRedraw,
    };
    assert!(matches!(
        effect,
        RuntimeSideEffect::RenderClient {
            reason: RenderInvalidationReason::FullRedraw,
            ..
        }
    ));
}

/// Verifies runtime event batch prioritization keeps ready PTY output ahead of
/// timer maintenance while preserving ingress reporting order.
///
/// Timer and pane events can be collected by the same async wakeup. The actor
/// must apply interactive pane output first so render-visible bytes are not
/// delayed behind periodic provider, status, cleanup, or debounce timer work,
/// while the ingress report still describes the batch as received.
#[test]
fn async_runtime_event_batch_prioritizes_pane_output_before_timers() {
    let provider_key = RuntimeTimerKey::new(RuntimeTimerKind::ProviderPoll, "agent-provider", 1);
    let mut batch = RuntimeEventBatch::new();
    batch.push(RuntimeEvent::Timer(TimerEvent {
        key: provider_key.clone(),
        now_ms: 7,
    }));
    batch.push(RuntimeEvent::Pane(PaneEvent::Output {
        pane_id: "%1".to_string(),
        bytes: b"first".to_vec(),
    }));

    let report = batch.ingress_report();
    let prioritized = batch.prioritized_events();

    assert_eq!(report.families, vec!["timer", "pane"]);
    assert!(matches!(
        &prioritized[0],
        RuntimeEvent::Pane(PaneEvent::Output { bytes, .. }) if bytes == b"first"
    ));
    assert!(matches!(
        &prioritized[1],
        RuntimeEvent::Timer(TimerEvent { key, .. }) if key == &provider_key
    ));
}

/// Verifies runtime event batch prioritization preserves pane-output FIFO
/// ordering within the interactive priority class.
///
/// Prioritization must not reorder terminal bytes from separate PTY reads. A
/// timer can move behind output, but pane output events must keep their original
/// relative order so the terminal parser receives bytes exactly as produced.
#[test]
fn async_runtime_event_batch_preserves_pane_output_fifo_when_prioritized() {
    let mut batch = RuntimeEventBatch::new();
    batch.push(RuntimeEvent::Timer(TimerEvent {
        key: RuntimeTimerKey::new(RuntimeTimerKind::StatusRefresh, "primary", 1),
        now_ms: 7,
    }));
    batch.push(RuntimeEvent::Pane(PaneEvent::Output {
        pane_id: "%1".to_string(),
        bytes: b"first".to_vec(),
    }));
    batch.push(RuntimeEvent::Pane(PaneEvent::Output {
        pane_id: "%1".to_string(),
        bytes: b"second".to_vec(),
    }));

    let prioritized = batch.prioritized_events();
    let output_bytes: Vec<&[u8]> = prioritized
        .iter()
        .filter_map(|event| match event {
            RuntimeEvent::Pane(PaneEvent::Output { bytes, .. }) => Some(bytes.as_slice()),
            _ => None,
        })
        .collect();

    assert_eq!(
        output_bytes,
        vec![b"first".as_slice(), b"second".as_slice()]
    );
    assert!(matches!(prioritized.last(), Some(RuntimeEvent::Timer(_))));
}

/// Verifies multiple applied events trigger one global reconciliation pass,
/// while a later no-op batch does not scan global runtime state again.
///
/// Direct event application and ingress accounting remain per event, but
/// bootstrap discovery, progress repair, deferred draining, and timer
/// regeneration belong to the coherent batch boundary.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_reconciles_global_state_once_per_applied_event_batch() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let mut applied = RuntimeEventBatch::new();
        applied.push(RuntimeEvent::Client(ClientEvent::ResizeSignal {
            client_id: primary.clone(),
        }));
        applied.push(RuntimeEvent::Client(ClientEvent::OutputReady {
            client_id: primary,
        }));
        let report = handle.submit_runtime_events(applied).await.unwrap();
        assert_eq!(report.accepted, 2);
        assert_eq!(report.applied, 2);

        let metrics = handle.metrics().await.unwrap();
        assert_eq!(metrics.runtime_events_applied, 2);
        assert_eq!(metrics.runtime_event_reconciliation_passes, 1);

        let mut no_op = RuntimeEventBatch::new();
        no_op.push(RuntimeEvent::Pane(PaneEvent::InputWritten {
            pane_id: "%missing".to_string(),
            bytes: 1,
        }));
        let report = handle.submit_runtime_events(no_op).await.unwrap();
        assert_eq!(report.applied, 0);
        assert_eq!(
            handle
                .metrics()
                .await
                .unwrap()
                .runtime_event_reconciliation_passes,
            1
        );
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.runtime_event_reconciliation_passes, 1);
}

/// Verifies event delivery revisions remain pending independently for every
/// subscriber when an event arrives between an empty query and the next wait.
///
/// Remote event streams query retained events before sleeping. A shared
/// edge-triggered notification can be consumed by an unrelated waiter during
/// that gap, leaving an Iroh attachment stale until a later event. Each watch
/// receiver must instead observe the revision even when it was not awaiting at
/// publication time and another subscriber observes the same publication.
#[tokio::test(flavor = "current_thread")]
async fn event_delivery_revision_survives_query_to_wait_gap_per_subscriber() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let mut first = handle.event_delivery_watcher();
    let mut second = handle.event_delivery_watcher();
    let _ = first.borrow_and_update();
    let _ = second.borrow_and_update();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Client(ClientEvent::OutputReady {
            client_id: primary,
        }));
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.applied, 1);

        tokio::time::timeout(Duration::from_millis(100), first.changed())
            .await
            .expect("first subscriber must retain the event revision")
            .expect("event revision sender must remain open");
        tokio::time::timeout(Duration::from_millis(100), second.changed())
            .await
            .expect("second subscriber must retain the same event revision")
            .expect("event revision sender must remain open");
        assert_eq!(*first.borrow(), *second.borrow());
        assert_ne!(*first.borrow(), 0);
        handle.shutdown().await.unwrap();
    };

    let ((), _) = tokio::join!(client, actor.run());
}
