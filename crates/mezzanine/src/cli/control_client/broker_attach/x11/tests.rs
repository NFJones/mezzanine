//! Generated fixture credentials and synthetic IPC qualify setup/cleanup only.
//!
//! Explicit helper inputs avoid global environment mutation or physical X work.
//! The broker never receives real credentials; uncertain settlement never retries.

use super::*;
use crate::protocol::framing::{ProtocolFrame, ProtocolFrameCodec};
use futures_util::{SinkExt, StreamExt};
use std::os::unix::fs::PermissionsExt;
use tokio_util::codec::Framed;

/// Writes an exact loopback authority selector with a synthetic cookie.
fn authority(cookie: u8) -> Vec<u8> {
    let mut bytes = 0_u16.to_be_bytes().to_vec();
    for field in [
        &[127_u8, 0, 0, 1][..],
        b"19",
        b"MIT-MAGIC-COOKIE-1",
        &[cookie; 16],
    ] {
        bytes.extend_from_slice(&(field.len() as u16).to_be_bytes());
        bytes.extend_from_slice(field);
    }
    bytes
}

/// Trusted/untrusted offers preserve original routing, takeover and fake proof.
/// Success transfers the generated lease until explicit disposal; lost setup or
/// absent publication removes private artifacts and sends no retry. Authored
/// authority files stay unchanged throughout these synthetic broker exchanges.
#[tokio::test]
async fn broker_attach_x11_prepared_offer_and_cleanup_are_exact() {
    use crate::runtime::x11::X11ForwardingMode;
    for case in ["trusted", "untrusted", "lost", "absent"] {
        let root =
            std::env::temp_dir().join(format!("mez-xattach-{:032x}", rand::random::<u128>()));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let source = root.join("source");
        let generated = root.join("generated");
        std::fs::write(&source, authority(17)).unwrap();
        std::fs::write(&generated, authority(52)).unwrap();
        for path in [&source, &generated] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let helper = root.join("xauth-fixture");
        std::fs::write(&helper, format!("#!/bin/sh\nif [ \"$5\" = generate ]; then cp '{}' \"$4\"; exit 0; fi\nif [ \"$5\" = remove ]; then : > \"$4\"; exit 0; fi\nexit 2\n", generated.display())).unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mode = if case == "trusted" {
            X11ForwardingMode::Trusted
        } else {
            X11ForwardingMode::Untrusted
        };
        let (prepared, private) =
            crate::cli::x11::prepare_broker_x11_for_tests(mode, &source, helper.as_os_str())
                .await
                .unwrap();
        assert!(private.join("authority").exists());
        let fake = base64::engine::general_purpose::STANDARD
            .encode(prepared.offer(true).fake_cookie.as_bytes());
        assert_ne!(
            fake,
            base64::engine::general_purpose::STANDARD.encode([52_u8; 16])
        );
        let control_path = root.join("outbound.sock");
        let listener = tokio::net::UnixListener::bind(&control_path).unwrap();
        std::fs::set_permissions(&control_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let name = "x0123456789abcdef.sock";
        let dedicated = tokio::net::UnixListener::bind(root.join(name)).unwrap();
        std::fs::set_permissions(root.join(name), std::fs::Permissions::from_mode(0o600)).unwrap();
        let handle = serde_json::json!({"owner":"f".repeat(32),"generation":1});
        let summary = serde_json::json!({"selected_version":3,"granted_role":"primary","session_id":"$1","lease_id":"lease-one","client_id":"c1"});
        let peer = async {
            let (stream, _) = listener.accept().await.unwrap();
            let mut peer = Framed::new(stream, ProtocolFrameCodec::new(4096).unwrap());
            peer.next().await.unwrap().unwrap();
            peer.send(ProtocolFrame::new(
                "application/vnd.mezzanine.outbound+json",
                serde_json::json!({"protocol":"mez-outbound/1","handle":handle}).to_string(),
            ))
            .await
            .unwrap();
            let frame = peer.next().await.unwrap().unwrap();
            let setup: serde_json::Value = serde_json::from_str(&frame.body).unwrap();
            assert_eq!(setup["initialize"]["idempotency_key"], "original-x11-key");
            assert_eq!(setup["initialize"]["x11_forwarding"]["mode"], mode.as_str());
            assert_eq!(setup["initialize"]["x11_forwarding"]["takeover"], true);
            assert_eq!(
                setup["initialize"]["x11_forwarding"]["fake_cookie_base64"],
                fake
            );
            assert!(setup["initialize"].get("authentication").is_none());
            if case == "lost" {
                return;
            }
            peer.next().await.unwrap().unwrap();
            peer.send(ProtocolFrame::new("application/vnd.mezzanine.outbound+json", serde_json::json!({
                "handle":handle,"session":summary,"lines":["retained"],"line_style_spans":[[]],
                "cursor":{"row":0,"column":0,"visible":false},"output_modes":{},"presentation_ids":[]
            }).to_string())).await.unwrap();
            let discovery = peer.next().await.unwrap().unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&discovery.body).unwrap()["operation"],
                "x11-discovery"
            );
            peer.send(ProtocolFrame::new("application/vnd.mezzanine.outbound+json", serde_json::json!({
                "handle":handle,"session":summary,"version":1,"socket_name":if case == "absent" { None } else { Some(name) }
            }).to_string())).await.unwrap();
            assert!(
                peer.next().await.is_none(),
                "setup or discovery must not replay"
            );
        };
        let client = async {
            let client = OutboundFrontendClient::connect(&root, std::time::Duration::from_secs(2))
                .await
                .unwrap();
            let params = initialize_params(
                "primary",
                &IrohSessionRouting::Create {
                    name: None,
                    idempotency_key: "original-x11-key".into(),
                },
                80,
                24,
                "xterm",
            )
            .unwrap();
            let policy = crate::runtime::RuntimeIrohTransportPolicy::default();
            let result = finish_prepared(
                client,
                "fixture",
                params,
                Default::default(),
                &policy,
                80,
                24,
                prepared,
                true,
            )
            .await;
            if matches!(case, "trusted" | "untrusted") {
                let attachment = result.unwrap();
                assert!(private.exists());
                let BrokerAttachment { session, x11, .. } = attachment;
                let x11 = x11.unwrap();
                assert_eq!(x11.limit, policy.x11.max_connections_per_route);
                x11.prepared
                    .run_broker_attachment(
                        x11.opener,
                        x11.limit,
                        std::time::Duration::from_millis(100),
                        policy.setup_timeout,
                        |stop| async {
                            crate::cli::x11::broker_attachment_cancelled(stop).await;
                            assert!(
                                private.exists(),
                                "credentials must survive foreground retirement"
                            );
                            drop(session);
                            Ok(())
                        },
                        std::future::ready(()),
                    )
                    .await
                    .unwrap();
            } else {
                assert!(result.is_err());
            }
            assert!(!private.exists());
        };
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(Box::pin(peer), Box::pin(client));
        })
        .await
        .unwrap();
        assert_eq!(std::fs::read(&source).unwrap(), authority(17));
        assert!(!root.join("remote/client/endpoint.key").exists());
        drop((listener, dedicated));
        std::fs::remove_dir_all(root).unwrap();
    }
}
