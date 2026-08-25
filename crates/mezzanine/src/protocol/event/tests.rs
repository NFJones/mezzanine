//! Tests for retained event replay visibility and notification encoding.

use super::{
    EventAudience, EventKind, EventLog, EventVisibility, VisibleEvent, encode_event_notification,
};

/// Verifies that the primary audience receives all retained events including
/// primary-only payloads.
#[test]
fn event_log_replays_primary_events() {
    let mut log = EventLog::new(10, 1024).unwrap();
    log.append(
        EventKind::WindowChanged,
        Some("$1".to_string()),
        EventVisibility::SessionView,
        "{\"window\":\"@1\"}",
    )
    .unwrap();
    log.append(
        EventKind::ApprovalChanged,
        Some("$1".to_string()),
        EventVisibility::AllPrimaries,
        "{\"approval\":\"pending\"}",
    )
    .unwrap();

    let events = log.replay_for(&EventAudience::AllPrimaries);

    assert_eq!(events.len(), 2);
    assert_eq!(events[0].session_id.as_deref(), Some("$1"));
    assert!(events[0].time.contains('T'));
    assert!(events[0].time.ends_with('Z'));
    assert!(!events[0].time.starts_with("event:"));
}

/// Verifies shared primary events fan out to every exact primary while
/// client-private events remain visible only to their owning client.
#[test]
fn exact_primary_audiences_receive_shared_and_private_events() {
    let first = mez_core::ids::ClientId::parse('c', "c1".to_string()).unwrap();
    let second = mez_core::ids::ClientId::parse('c', "c2".to_string()).unwrap();
    let mut log = EventLog::new(10, 1024).unwrap();
    log.append(
        EventKind::ClientAttached,
        Some("$1".to_string()),
        EventVisibility::AllPrimaries,
        "shared",
    )
    .unwrap();
    log.append(
        EventKind::Diagnostic,
        Some("$1".to_string()),
        EventVisibility::PrimaryClient(first.clone()),
        "private-first",
    )
    .unwrap();
    log.append(
        EventKind::PaneChanged,
        Some("$1".to_string()),
        EventVisibility::SessionView,
        "session-view",
    )
    .unwrap();

    let first_events = log.replay_for(&EventAudience::PrimaryClient(first));
    let second_events = log.replay_for(&EventAudience::PrimaryClient(second));
    let session_events = log.replay_for(&EventAudience::SessionView);

    assert_eq!(
        first_events
            .iter()
            .map(|event| event.payload.as_str())
            .collect::<Vec<_>>(),
        vec!["shared", "private-first", "session-view"]
    );
    assert_eq!(
        second_events
            .iter()
            .map(|event| event.payload.as_str())
            .collect::<Vec<_>>(),
        vec!["shared", "session-view"]
    );
    assert_eq!(
        session_events
            .iter()
            .map(|event| event.payload.as_str())
            .collect::<Vec<_>>(),
        vec!["session-view"]
    );
}

/// Verifies that observers only see session-view events at or after their
/// attachment marker.
#[test]
fn observer_replay_starts_at_attachment_marker() {
    let mut log = EventLog::new(10, 1024).unwrap();
    log.append(
        EventKind::PaneChanged,
        Some("$1".to_string()),
        EventVisibility::SessionView,
        "before",
    )
    .unwrap();
    let marker = log.latest_event_id() + 1;
    log.append(
        EventKind::PaneChanged,
        Some("$1".to_string()),
        EventVisibility::SessionView,
        "after",
    )
    .unwrap();

    let events = log.replay_for(&EventAudience::ApprovedObserver {
        visible_from_event_id: marker,
    });

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].payload, "after");
}

/// Verifies that retention limits discard the oldest events when the log grows
/// beyond its configured capacity.
#[test]
fn event_log_retains_bounded_events() {
    let mut log = EventLog::new(2, 1024).unwrap();
    log.append(
        EventKind::Diagnostic,
        None,
        EventVisibility::AllPrimaries,
        "one",
    )
    .unwrap();
    log.append(
        EventKind::Diagnostic,
        None,
        EventVisibility::AllPrimaries,
        "two",
    )
    .unwrap();
    log.append(
        EventKind::Diagnostic,
        None,
        EventVisibility::AllPrimaries,
        "three",
    )
    .unwrap();

    let events = log.replay_for(&EventAudience::AllPrimaries);

    assert_eq!(log.len(), 2);
    assert_eq!(events[0].payload, "two");
    assert_eq!(events[1].payload, "three");
}

/// Verifies that oversized event payloads are rejected before they enter the
/// retained log.
#[test]
fn oversized_payload_is_rejected() {
    let mut log = EventLog::new(2, 4).unwrap();

    let error = log
        .append(
            EventKind::Diagnostic,
            None,
            EventVisibility::AllPrimaries,
            "too long",
        )
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies cursor-based replay filters by event id and then applies audience
/// visibility.
#[test]
fn event_log_replays_visible_events_after_cursor() {
    let mut log = EventLog::new(10, 1024).unwrap();
    log.append(
        EventKind::PaneChanged,
        Some("$1".to_string()),
        EventVisibility::SessionView,
        "one",
    )
    .unwrap();
    let cursor = log.latest_event_id();
    log.append(
        EventKind::PaneChanged,
        Some("$1".to_string()),
        EventVisibility::SessionView,
        "two",
    )
    .unwrap();
    log.append(
        EventKind::ApprovalChanged,
        Some("$1".to_string()),
        EventVisibility::AllPrimaries,
        "secret",
    )
    .unwrap();

    let events = log.replay_after_for(
        &EventAudience::ApprovedObserver {
            visible_from_event_id: cursor,
        },
        cursor,
        10,
    );

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].payload, "two");
}

/// Verifies that event notifications use the JSON-RPC event namespace and wrap
/// plain payload text as an object.
#[test]
fn event_notifications_use_json_rpc_event_namespace() {
    let notification = encode_event_notification(&VisibleEvent {
        id: 7,
        time: "event:7".to_string(),
        kind: EventKind::PaneChanged,
        session_id: Some("$1".to_string()),
        payload: "line\nnext".to_string(),
    });

    assert!(notification.contains(r#""method":"event/pane_changed""#));
    assert!(notification.contains(r#""event_id":7"#));
    assert!(notification.contains(r#""time":"event:7""#));
    assert!(notification.contains(r#""event_type":"pane_changed""#));
    assert!(notification.contains(r#""object":{"payload":"line\nnext"}"#));
}

/// Verifies that object payload strings are embedded directly in event
/// notifications.
#[test]
fn event_notifications_embed_object_payloads() {
    let notification = encode_event_notification(&VisibleEvent {
        id: 8,
        time: "event:8".to_string(),
        kind: EventKind::ClientAttached,
        session_id: Some("$1".to_string()),
        payload: r#"{"client_id":"c2","role":"observer"}"#.to_string(),
    });

    assert!(notification.contains(r#""session_id":"$1""#));
    assert!(notification.contains(r#""object":{"client_id":"c2","role":"observer"}"#));
}
