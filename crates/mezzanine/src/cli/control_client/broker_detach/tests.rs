//! Administrative detach rejects incomplete authority/selection before broker work.
//!
//! Disposable protected profiles have no usable remote route. Invalid inputs must
//! neither create an endpoint key nor start a broker or choose another session.

use super::*;

/// Missing selectors, malformed IDs, observer ceilings and missing discovery
/// fail closed. Local and legacy options remain separate, and the original key
/// is not replaced by any implicit creation or reconnect attempt.
#[tokio::test]
async fn broker_detach_rejects_incomplete_selection_before_endpoint_acquisition() {
    for case in [
        "missing-session",
        "missing-client",
        "bad-session",
        "bad-client",
        "ceiling",
        "no-broker",
        "veto",
    ] {
        let home = std::env::temp_dir().join(format!(
            "mez-detach-boundary-{:032x}",
            rand::random::<u128>()
        ));
        let env = crate::cli::CliEnv {
            home: Some(home.clone()),
            ..Default::default()
        };
        let paths = env.config_paths().unwrap();
        paths.ensure_default_config().unwrap();
        RemoteClientProfileStore::under_config_root(paths.root())
            .save(&RemoteClientProfile {
                name: "paired".into(),
                server_addr: EndpointAddr::new(iroh::SecretKey::generate().public()),
                role: if case == "ceiling" {
                    RemoteRoleCeiling::Observer
                } else {
                    RemoteRoleCeiling::Primary
                },
                scope: RemoteClientProfileScope::Host,
                device_credential: SecretString::from("synthetic-only-proof".to_string()),
            })
            .unwrap();
        if case == "veto" {
            std::fs::write(
                paths.default_primary_file(),
                format!(
                    "version = {}\n[transport.iroh]\noutbound_enabled = false\n",
                    crate::config::CURRENT_CONFIG_SCHEMA_VERSION
                ),
            )
            .unwrap();
        }
        let session = match case {
            "missing-session" => None,
            "bad-session" => Some("wrong"),
            _ => Some("$1"),
        };
        let client = match case {
            "missing-client" => None,
            "bad-client" => Some("wrong"),
            _ => Some("c2"),
        };
        let error = try_detach(
            &crate::cli::ControlTargetSelection::IrohProfile("paired".into()),
            &env,
            session,
            client,
            "original-key",
        )
        .await
        .unwrap_err();
        assert!(!error.message().contains("synthetic-only-proof"));
        if case == "no-broker" {
            assert!(error.message().contains("active outbound broker"));
        }
        if case == "ceiling" {
            assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
        }
        assert!(!paths.root().join("remote/client/endpoint.key").exists());
        assert!(!paths.root().join("outbound.startup.lock").exists());
        std::fs::remove_dir_all(home).unwrap();
    }
}
