//! Shared parsing preserves conservative scope and secret-free validation errors.
use super::*;
use secrecy::ExposeSecret;
use std::os::unix::fs::PermissionsExt;

/// Both envelopes and alias overrides preserve endpoint, proof and conservative
/// omission semantics. Invalid fields and unsafe/oversized files reject before
/// any transport or pairing effect, and errors exclude the synthetic token.
#[test]
fn shared_iroh_invitation_preserves_envelopes_alias_and_scope() {
    let root = std::env::temp_dir().join(format!(
        "mez-invitation-parser-{:032x}",
        rand::random::<u128>()
    ));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.join("invitation.json");
    let server = iroh::SecretKey::generate().public();
    let original = serde_json::json!({"format_version":1,"profile_name":"authored", "server_addr":EndpointAddr::new(server),
        "server_endpoint_id":server.to_string(),"role":"primary","token":"synthetic-private-proof","expires_at_unix_seconds":0});
    for wrapped in [false, true] {
        for scope in [None, Some("host"), Some("legacy_session")] {
            let mut value = original.clone();
            if let Some(scope) = scope {
                value["profile_scope"] = serde_json::json!(scope);
            }
            if wrapped {
                value = serde_json::json!({"result":value});
            }
            std::fs::write(&path, value.to_string()).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            let parsed = read_iroh_invitation(&path, Some("override")).unwrap();
            assert_eq!(parsed.profile_name, "override");
            assert_eq!(parsed.server_addr.id, server);
            assert_eq!(parsed.role, RemoteRoleCeiling::Primary);
            assert_eq!(parsed.token.expose_secret(), "synthetic-private-proof");
            assert_eq!(
                parsed.expires_at_unix_seconds, 0,
                "parsing does not pretend to check expiry"
            );
            assert_eq!(
                parsed.scope,
                if scope == Some("host") {
                    RemoteClientProfileScope::Host
                } else {
                    RemoteClientProfileScope::LegacySession
                }
            );
        }
    }
    for (key, value) in [
        ("format_version", serde_json::json!(2)),
        ("profile_name", serde_json::Value::Null),
        ("token", serde_json::json!("")),
        ("role", serde_json::json!("agent")),
        ("expires_at_unix_seconds", serde_json::json!("later")),
        ("profile_scope", serde_json::json!("unknown")),
        (
            "server_endpoint_id",
            serde_json::json!(iroh::SecretKey::generate().public().to_string()),
        ),
    ] {
        let mut invalid = original.clone();
        invalid[key] = value;
        std::fs::write(&path, invalid.to_string()).unwrap();
        let error = read_iroh_invitation(&path, Some("override")).err().unwrap();
        assert!(!error.message().contains("synthetic-private-proof"));
    }
    std::fs::write(&path, "x".repeat(MAX_INVITATION_BYTES as usize + 1)).unwrap();
    assert!(read_iroh_invitation(&path, None).is_err());
    std::fs::write(&path, original.to_string()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(read_iroh_invitation(&path, None).is_err());
    std::fs::remove_dir_all(root).unwrap();
}
