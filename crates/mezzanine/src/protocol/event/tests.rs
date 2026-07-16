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
        EventVisibility::PrimaryOnly,
        "{\"approval\":\"pending\"}",
    )
    .unwrap();

    let events = log.replay_for(&EventAudience::Primary);

    assert_eq!(events.len(), 2);
    assert_eq!(events[0].session_id.as_deref(), Some("$1"));
    assert!(events[0].time.contains('T'));
    assert!(events[0].time.ends_with('Z'));
    assert!(!events[0].time.starts_with("event:"));
}

/// Verifies that approved observers only see session-view events at or after
/// their approval marker.
#[test]
fn approved_observer_replay_starts_at_visibility_marker() {
    let mut log = EventLog::new(10, 1024).unwrap();
    log.append(
        EventKind::PaneChanged,
        Some("$1".to_string()),
        EventVisibility::SessionView,
        "before",
    )
    .unwrap();
    let marker = log
        .append(
            EventKind::ObserverDecided,
            Some("$1".to_string()),
            EventVisibility::PrimaryOnly,
            "approved",
        )
        .unwrap();
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

/// Verifies that pending observers receive only request-local events and do not
/// receive the session id before approval.
#[test]
fn pending_observer_receives_only_request_local_status_without_session() {
    let mut log = EventLog::new(10, 1024).unwrap();
    log.append(
        EventKind::ObserverRequested,
        Some("$1".to_string()),
        EventVisibility::PendingObserverRequest("o1".to_string()),
        "{\"state\":\"pending\"}",
    )
    .unwrap();
    log.append(
        EventKind::PaneChanged,
        Some("$1".to_string()),
        EventVisibility::SessionView,
        "secret view",
    )
    .unwrap();

    let events = log.replay_for(&EventAudience::PendingObserver {
        observer_request_id: "o1".to_string(),
    });

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].session_id, None);
    assert_eq!(events[0].payload, "{\"state\":\"pending\"}");
}

/// Verifies that retention limits discard the oldest events when the log grows
/// beyond its configured capacity.
#[test]
fn event_log_retains_bounded_events() {
    let mut log = EventLog::new(2, 1024).unwrap();
    log.append(
        EventKind::Diagnostic,
        None,
        EventVisibility::PrimaryOnly,
        "one",
    )
    .unwrap();
    log.append(
        EventKind::Diagnostic,
        None,
        EventVisibility::PrimaryOnly,
        "two",
    )
    .unwrap();
    log.append(
        EventKind::Diagnostic,
        None,
        EventVisibility::PrimaryOnly,
        "three",
    )
    .unwrap();

    let events = log.replay_for(&EventAudience::Primary);

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
            EventVisibility::PrimaryOnly,
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
        EventVisibility::PrimaryOnly,
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
        kind: EventKind::ObserverRequested,
        session_id: None,
        payload: r#"{"observer_request_id":"obs1","state":"pending"}"#.to_string(),
    });

    assert!(notification.contains(r#""session_id":null"#));
    assert!(notification.contains(r#""object":{"observer_request_id":"obs1","state":"pending"}"#));
}
