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
