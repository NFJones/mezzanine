//! Explicit broker-process qualification against a synthetic pinned QUIC peer.
//!
//! The real executable owns local IPC and the paired endpoint. The synthetic
//! peer checks original invocation identity and returns allowlisted settlement;
//! it does not establish real host authorization or physical terminal behavior.

use super::*;
use crate::protocol::framing::{ProtocolFrame, ProtocolFrameCodec};
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio_util::codec::Framed;

/// Even an X11 first attachment must elect the shared owner for a supported
/// pinned profile. Failed readiness retains exact-child evidence and is terminal,
/// rather than granting direct identity acquisition. The harmless executable
/// never becomes ready, so no credential generation or remote setup occurs.
#[tokio::test]
async fn broker_attach_x11_first_owner_failure_retains_child_without_direct_fallback() {
    let home = std::env::temp_dir().join(format!("mez-xfirst-{:032x}", rand::random::<u128>()));
    let env = crate::cli::CliEnv {
        home: Some(home.clone()),
        ..Default::default()
    };
    let paths = env.config_paths().unwrap();
    paths.ensure_default_config().unwrap();
    std::fs::write(
        paths.default_primary_file(),
        format!(
            "version = {}\n[transport.iroh]\nsetup_timeout_ms = 200\n",
            crate::config::CURRENT_CONFIG_SCHEMA_VERSION,
        ),
    )
    .unwrap();
    RemoteClientProfileStore::under_config_root(paths.root())
        .save(&RemoteClientProfile {
            name: "fixture".into(),
            server_addr: EndpointAddr::new(iroh::SecretKey::generate().public())
                .with_ip_addr("127.0.0.1:43210".parse().unwrap()),
            role: RemoteRoleCeiling::Primary,
            scope: RemoteClientProfileScope::Host,
            device_credential: SecretString::from("synthetic-first-owner-proof".to_string()),
        })
        .unwrap();
    let mut child = None;
    let result = try_open_inner(
        &crate::cli::ControlTargetSelection::IrohProfile("fixture".into()),
        &env,
        "primary",
        &IrohSessionRouting::Create {
            name: Some("first-x11".into()),
            idempotency_key: "original-first-x11".into(),
        },
        80,
        24,
        "xterm",
        Some((crate::runtime::x11::X11ForwardingMode::Untrusted, false)),
        Some(&mut child),
        Some(Path::new("/bin/true")),
    )
    .await;
    let retained = child.is_some();
    if let Some(child) = child.as_mut() {
        assert!(
            tokio::time::timeout(Duration::from_secs(5), child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
    }
    std::fs::remove_dir_all(home).unwrap();
    assert!(
        result.is_err(),
        "attempted first-owner startup must not authorize direct fallback"
    );
    assert!(
        retained,
        "X11 must use elected shared-owner startup rather than direct identity ownership"
    );
}

/// Two setups beginning with no broker must retain one actual child, distinct
/// session identities and original Create keys. Dropping either attachment must
/// preserve the shared process. Explicit fixture cleanup reaps that exact child;
/// production callers do not infer termination authority from readiness failure.
#[tokio::test]
#[ignore = "requires explicit MEZ_BROKER_EXECUTABLE for broker process qualification"]
async fn broker_attach_actual_first_owner_retains_child_and_sibling() {
    let executable = PathBuf::from(
        std::env::var_os("MEZ_BROKER_EXECUTABLE")
            .expect("explicit trusted broker executable required"),
    );
    assert!(executable.is_absolute());
    let home =
        std::env::temp_dir().join(format!("mez-first-owner-{:032x}", rand::random::<u128>()));
    let env = crate::cli::CliEnv {
        home: Some(home.clone()),
        ..Default::default()
    };
    let paths = env.config_paths().unwrap();
    paths.ensure_default_config().unwrap();
    std::fs::write(
        paths.default_primary_file(),
        format!(
            "version = {}\n[transport.iroh]\ncompression_codecs = [\"none\"]\n",
            crate::config::CURRENT_CONFIG_SCHEMA_VERSION,
        ),
    )
    .unwrap();
    let server = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .alpns(vec![crate::runtime::MEZZANINE_IROH_ALPN.to_vec()])
        .relay_mode(iroh::RelayMode::Disabled)
        .clear_address_lookup()
        .bind()
        .await
        .unwrap();
    RemoteClientProfileStore::under_config_root(paths.root())
        .save(&RemoteClientProfile {
            name: "fixture".into(),
            server_addr: server.addr(),
            role: RemoteRoleCeiling::Primary,
            scope: RemoteClientProfileScope::Host,
            device_credential: SecretString::from("synthetic-first-owner-proof".to_string()),
        })
        .unwrap();
    let mut child = None;
    let mut sibling_child = None;
    let target = crate::cli::ControlTargetSelection::IrohProfile("fixture".into());
    let work = async {
        let first = IrohSessionRouting::Create {
            name: Some("first".into()),
            idempotency_key: "original-first".into(),
        };
        let second = IrohSessionRouting::Create {
            name: Some("second".into()),
            idempotency_key: "original-second".into(),
        };
        let first = try_open_inner(
            &target,
            &env,
            "primary",
            &first,
            80,
            24,
            "xterm",
            None,
            Some(&mut child),
            Some(&executable),
        )
        .await?
        .ok_or_else(|| MezError::invalid_state("fixture first setup bypassed broker"))?;
        let second = try_open_inner(
            &target,
            &env,
            "primary",
            &second,
            80,
            24,
            "xterm",
            None,
            Some(&mut sibling_child),
            Some(&executable),
        )
        .await?
        .ok_or_else(|| MezError::invalid_state("fixture second setup bypassed broker"))?;
        assert!(sibling_child.is_none());
        let first_id = serde_json::to_value(first.session.summary()).unwrap();
        let second_id = serde_json::to_value(second.session.summary()).unwrap();
        assert_ne!(first_id["session_id"], second_id["session_id"]);
        assert_ne!(first_id["lease_id"], second_id["lease_id"]);
        drop(first);
        let (session, lines) = second
            .session
            .snapshot(80, 24, Duration::from_secs(2))
            .await?;
        assert_eq!(lines, ["second retained snapshot"]);
        drop(session);
        assert!(
            child
                .as_mut()
                .expect("elected caller must retain child")
                .try_wait()?
                .is_none()
        );
        Ok::<(), MezError>(())
    };
    // Drive peers concurrently with setup: acceptance alone cannot flush replies.
    let peer = async {
        let first = server.accept().await.unwrap().await.unwrap();
        let first_peer = serve_peer(first, server.id(), 1);
        let second_peer = async {
            let second = server.accept().await.unwrap().await.unwrap();
            serve_peer(second, server.id(), 2).await
        };
        let (first, second) = tokio::join!(first_peer, second_peer);
        first?;
        second
    };
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(Box::pin(work), Box::pin(peer))
    })
    .await;
    let shutdown = if let Some(child) = child.as_mut() {
        tokio::time::timeout(Duration::from_secs(10), child.shutdown_for_tests()).await
    } else {
        Ok(Err(MezError::invalid_state(
            "fixture startup retained no child",
        )))
    };
    if let Some(child) = child.as_mut()
        && child.try_wait().unwrap().is_none()
    {
        tokio::time::timeout(Duration::from_secs(5), child.terminate_for_tests())
            .await
            .unwrap()
            .unwrap();
    }
    let (work, peer) = result.expect("fixture workflow must finish");
    work.unwrap();
    peer.unwrap();
    assert!(
        shutdown
            .expect("fixture graceful shutdown must finish")
            .unwrap()
            .success()
    );
    assert!(!paths.root().join("outbound.sock").exists());
    drop(RemoteClientIdentity::load_or_create(paths.root()).unwrap());
    server.close().await;
    std::fs::remove_dir_all(home).unwrap();
}

/// Supplies bounded correlated session/view frames on one exact connection.
/// No additional initialize or mutation is accepted after original setup.
async fn serve_peer(
    connection: iroh::endpoint::Connection,
    server: iroh::EndpointId,
    index: u64,
) -> Result<()> {
    let (send, recv) = connection
        .accept_bi()
        .await
        .map_err(|_| MezError::invalid_state("fixture stream unavailable"))?;
    let policy = crate::runtime::IrohCompressionPolicy::new(
        crate::runtime::RuntimeIrohCompressionCodec::None,
        512,
        3,
        1024 * 1024 + 1024,
    )?;
    let mut bridge = IrohCompressionBridge::spawn(recv, send, policy, 1024 * 1024)?;
    let mut stream = Framed::new(bridge.stream_mut(), ProtocolFrameCodec::new(1024 * 1024)?);
    let initialize = stream.next().await.unwrap()?;
    let initialize: serde_json::Value = serde_json::from_str(&initialize.body).unwrap();
    let name = if index == 1 { "first" } else { "second" };
    assert_eq!(initialize["method"], "control/initialize");
    assert_eq!(
        initialize["params"]["idempotency_key"],
        format!("original-{name}")
    );
    assert_eq!(
        initialize["params"]["authentication"]["token"],
        "synthetic-first-owner-proof"
    );
    stream.send(ProtocolFrame::new(crate::control::CONTROL_CONTENT_TYPE, serde_json::json!({
        "jsonrpc":"2.0","id":initialize["id"],"result":{
            "selected_version":3,"granted_role":"primary","host":{"endpoint_id":server.to_string()},
            "client":{"id":format!("c{index}")},"session":{"id":format!("${index}")},
            "lease":{"lease_id":format!("lease-{index}"),"session_id":format!("${index}"),"state":"active"},
            "capabilities":{"features":{"client_clipboard_write":true}},"x11_forwarding":null
        }
    }).to_string())).await?;
    let mut events = connection
        .open_uni()
        .await
        .map_err(|_| MezError::invalid_state("fixture events unavailable"))?;
    events
        .write_all(crate::runtime::MEZZANINE_IROH_EVENT_STREAM_V2_PREFACE)
        .await
        .map_err(|_| MezError::invalid_state("fixture event preface write unavailable"))?;
    while let Some(frame) = stream.next().await {
        let request: serde_json::Value = serde_json::from_str(&frame?.body).unwrap();
        assert_eq!(request["method"], "terminal/view");
        let size = &request["params"]["client_size"];
        stream.send(ProtocolFrame::new(crate::control::CONTROL_CONTENT_TYPE, serde_json::json!({
            "jsonrpc":"2.0","id":request["id"],"result":{"view":{
                "role":"primary","client_size":size,"lines":[format!("{name} retained snapshot")],
                "line_style_spans":[[]],"cursor":{"row":0,"column":0,"visible":false},"output_modes":{}
            },"presentation_ids":[]}
        }).to_string())).await?;
    }
    drop(stream);
    drop(events);
    drop(bridge);
    Ok(())
}
