//! Broker attachment preparation preserves routing and early failure boundaries.
use super::*;

mod boundaries;
mod multiprocess;
mod startup;

/// An attachment caller outside the broker subtree must be able to observe and
/// reap its exact retained child after failed readiness. A harmless exited
/// executable publishes no broker, and no kill or replacement is inferred.
#[tokio::test]
async fn broker_attach_caller_can_reap_retained_startup_child() {
    let home =
        std::env::temp_dir().join(format!("mez-caller-reap-{:032x}", rand::random::<u128>()));
    let env = crate::cli::CliEnv {
        home: Some(home.clone()),
        ..Default::default()
    };
    env.config_paths().unwrap().ensure_default_config().unwrap();
    let mut child = None;
    assert!(
        crate::cli::remote::broker::launch::connect_owned(
            Path::new("/bin/true"),
            &env,
            std::time::Duration::from_millis(200),
            &mut child,
        )
        .await
        .is_err()
    );
    let child = child
        .as_mut()
        .expect("failed readiness retains the exact child");
    let status = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success());
    assert!(child.try_wait().unwrap().unwrap().success());
    assert!(
        !env.config_paths()
            .unwrap()
            .root()
            .join("outbound.sock")
            .exists()
    );
    std::fs::remove_dir_all(home).unwrap();
}

/// Setup must use the original operation key, not allocate another nonce during
/// discovery or convert explicit/default selection into fresh creation. Primary
/// clipboard negotiation and observer v1 are independent of routing intent.
#[test]
fn broker_attach_parameters_preserve_intent_key_and_role() {
    for routing in [
        IrohSessionRouting::Create {
            name: Some("fresh 雪".into()),
            idempotency_key: "original-create".into(),
        },
        IrohSessionRouting::ResolveOrCreate {
            idempotency_key: "original-resolve".into(),
        },
        IrohSessionRouting::Attach {
            target: "$1".into(),
        },
        IrohSessionRouting::Attach {
            target: "lease-one".into(),
        },
        IrohSessionRouting::Attach {
            target: "exact-name".into(),
        },
        IrohSessionRouting::Default,
    ] {
        for role in ["primary", "observer"] {
            let params = initialize_params(role, &routing, 80, 24, "xterm").unwrap();
            assert_eq!(params["session_intent"], routing.intent());
            assert_eq!(
                params.get("session_target"),
                routing.session_target().as_ref()
            );
            assert_eq!(
                params
                    .get("idempotency_key")
                    .and_then(serde_json::Value::as_str),
                routing.idempotency_key()
            );
            assert_eq!(params["requested_role"], role);
            assert_eq!(
                params["event_stream_version"],
                if role == "primary" { 2 } else { 1 }
            );
            assert!(params.get("authentication").is_none());
            assert!(params.get("x11_forwarding").is_none());
        }
    }
    assert!(initialize_params("agent", &IrohSessionRouting::Default, 80, 24, "xterm").is_err());
    assert!(initialize_params("primary", &IrohSessionRouting::Default, 0, 24, "xterm").is_err());
}
