//! Exact setup-frame budget includes escaping and maximum future-handle overhead.
use super::*;

/// A pre-pair envelope at the maximum admitted size must fit every subsequent
/// valid generation. One additional byte or JSON-escaped expansion rejects;
/// validation never mutates the original initialization or its operation key.
#[test]
fn outbound_session_setup_budget_matches_serialized_envelope() {
    let handle = FrontendHandle {
        owner: "f".repeat(32),
        generation: u64::MAX,
    };
    let alias = "alias\"\\雪";
    let mut initialize = serde_json::json!({
        "client_name":"fixture", "requested_version":3,
        "requested_role":"primary", "session_intent":"create",
        "idempotency_key":"original-create", "event_stream_version":2,
        "client":{"name":"fixture","interactive":true,
            "terminal":{"columns":80,"rows":24,"term":"x"}}
    });
    let budget = Duration::from_secs(1);
    let (base, _) = encode_setup(&handle, alias, &initialize, 80, 24, budget).unwrap();
    let term_length = HELLO_LIMIT - base.len() + 1;
    initialize["client"]["terminal"]["term"] = serde_json::json!("x".repeat(term_length));
    let original = initialize.clone();
    OutboundFrontendClient::validate_session_setup(alias, &initialize, 80, 24, budget).unwrap();
    for generation in [1, u64::MAX] {
        let actual = FrontendHandle {
            generation,
            ..handle.clone()
        };
        let (body, params) = encode_setup(&actual, alias, &initialize, 80, 24, budget).unwrap();
        assert!(body.len() <= HELLO_LIMIT);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["initialize"],
            original
        );
        assert_eq!(params.requested_role, RequestedRole::Primary);
    }
    assert_eq!(
        encode_setup(&handle, alias, &initialize, 80, 24, budget)
            .unwrap()
            .0
            .len(),
        HELLO_LIMIT
    );
    initialize["client"]["terminal"]["term"] = serde_json::json!("x".repeat(term_length + 1));
    assert!(
        OutboundFrontendClient::validate_session_setup(alias, &initialize, 80, 24, budget).is_err()
    );
    initialize["client"]["terminal"]["term"] = serde_json::json!("\"".repeat(term_length));
    assert!(
        OutboundFrontendClient::validate_session_setup(alias, &initialize, 80, 24, budget).is_err()
    );
    assert_eq!(original["idempotency_key"], "original-create");
}
