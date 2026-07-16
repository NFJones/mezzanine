//! Control idempotency tests.

use super::*;

/// Verifies idempotency conflicts on reused key with different params.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn idempotency_conflicts_on_reused_key_with_different_params() {
    let (mut session, primary) = test_session();
    let mut cache = ControlIdempotencyCache::default();

    let first = dispatch_control_request_cached(
        r#"{"jsonrpc":"2.0","id":1,"method":"window/create","params":{"name":"one","idempotency_key":"same"}}"#,
        &mut session,
        &primary,
        &mut cache,
    );
    let second = dispatch_control_request_cached(
        r#"{"jsonrpc":"2.0","id":2,"method":"window/create","params":{"name":"two","idempotency_key":"same"}}"#,
        &mut session,
        &primary,
        &mut cache,
    );

    assert!(first.contains(r#""window_id":"@2""#));
    assert!(second.contains(r#""mezzanine_code":"conflict""#));
    assert_eq!(session.windows().len(), 2);
}

/// Verifies that completed JSON-RPC idempotency responses are retained within
/// explicit entry and byte budgets. Primary attach loops create many control
/// frames, so this protects the runtime from keeping arbitrarily many old
/// responses while preserving retry behavior for recent requests.
#[test]
fn idempotency_cache_evicts_oldest_entries_within_limits() {
    let mut cache = ControlIdempotencyCache::with_limits(2, 1024);

    cache.remember_response("client:a", "window/create", None, r#"{"ok":1}"#);
    cache.remember_response("client:b", "window/create", None, r#"{"ok":2}"#);
    cache.remember_response("client:c", "window/create", None, r#"{"ok":3}"#);

    assert_eq!(cache.len(), 2);
    assert!(
        cache
            .cached_response("client:a", "window/create", &None)
            .unwrap()
            .is_none()
    );
    assert!(
        cache
            .cached_response("client:b", "window/create", &None)
            .unwrap()
            .is_some()
    );
    assert!(
        cache
            .cached_response("client:c", "window/create", &None)
            .unwrap()
            .is_some()
    );
    assert!(cache.retained_bytes() <= 1024);
}

/// Verifies that oversized idempotency responses are not retained. A single
/// rendered response must not consume the entire cache because callers can
/// still reissue the operation when the bounded replay window cannot store it.
#[test]
fn idempotency_cache_skips_entries_larger_than_byte_limit() {
    let mut cache = ControlIdempotencyCache::with_limits(8, 16);

    cache.remember_response("client:large", "terminal/step", None, "x".repeat(64));

    assert!(cache.is_empty());
    assert_eq!(cache.retained_bytes(), 0);
}

/// Verifies connection idempotency replays completed error responses.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn connection_idempotency_replays_completed_error_responses() {
    let (mut session, _primary) = test_session();
    let mut connection = ControlConnectionState::new(true, true);
    let mut cache = ControlIdempotencyCache::default();
    let mut input = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );
    let invalid = encode_control_body(
        r#"{"jsonrpc":"2.0","id":1,"method":"pane/resize","params":{"target":{"pane_id":"%1"},"size":{"mode":"cells","columns":"wide","rows":10},"idempotency_key":"bad-resize"}}"#,
    );
    input.extend_from_slice(&invalid);
    input.extend_from_slice(&invalid);

    let (output, _) =
        handle_control_frames(&input, 4096, &mut session, &mut connection, &mut cache).unwrap();
    let (_init_body, init_len) = decode_control_frame(&output, 4096).unwrap();
    let (first_body, first_len) = decode_control_frame(&output[init_len..], 4096).unwrap();
    let (second_body, _) = decode_control_frame(&output[init_len + first_len..], 4096).unwrap();

    assert_eq!(first_body, second_body);
    assert!(first_body.contains(r#""error""#), "{first_body}");
    assert!(first_body.contains(r#""code":-32602"#), "{first_body}");
    assert!(
        first_body.contains(r#""mezzanine_code":"invalid_params""#),
        "{first_body}"
    );
    assert_eq!(cache.len(), 1);
}
