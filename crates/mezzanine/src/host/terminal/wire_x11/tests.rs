//! Exact route evidence validation without local X credentials or relay work.
use super::*;

/// Explicit trusted/untrusted mode, current version and positive generation
/// must survive decoding. Invalid proof is rejected without exposing it in
/// diagnostics; unrequested authority and legacy versions remain forbidden.
#[test]
fn wire_x11_routes_preserve_exact_authority_and_redacted_errors() {
    let token = base64::engine::general_purpose::STANDARD
        .encode([51_u8; crate::runtime::x11::X11_ROUTE_TOKEN_BYTES]);
    let original = serde_json::json!({"result":{"capabilities":{"features":{"x11_forwarding":true}},
        "x11_forwarding":{"version":crate::runtime::x11::X11_FORWARDING_VERSION,
            "mode":"untrusted","generation":7,"route_token_base64":token}}});
    let route = validate_route(&original.to_string(), Some(X11ForwardingMode::Untrusted))
        .unwrap()
        .unwrap();
    assert_eq!(route.generation, 7);
    assert_eq!(route.mode, X11ForwardingMode::Untrusted);
    assert!(validate_route(&original.to_string(), None).is_err());
    assert!(validate_route(&original.to_string(), Some(X11ForwardingMode::Trusted)).is_err());
    for (field, value) in [
        ("version", serde_json::json!(1)),
        ("generation", serde_json::json!(0)),
        (
            "route_token_base64",
            serde_json::json!("private-invalid-proof"),
        ),
        ("route_token_base64", serde_json::json!("AA==")),
    ] {
        let mut invalid = original.clone();
        invalid["result"]["x11_forwarding"][field] = value;
        let error =
            validate_route(&invalid.to_string(), Some(X11ForwardingMode::Untrusted)).unwrap_err();
        assert!(!error.message().contains("private-invalid-proof"));
        assert!(!error.message().contains(&token));
    }
    let missing = r#"{"result":{"x11_forwarding":null}}"#;
    assert!(validate_route(missing, None).unwrap().is_none());
    assert_eq!(
        validate_route(missing, Some(X11ForwardingMode::Trusted))
            .unwrap_err()
            .kind(),
        crate::error::MezErrorKind::NotImplemented
    );
}
