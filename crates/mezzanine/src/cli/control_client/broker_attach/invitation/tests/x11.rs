//! Invitation X11 qualification uses explicit preparation and synthetic private IPC.
//!
//! Local helper inputs avoid DISPLAY/environment mutation and physical X work.
//! Tests retain one generated credential owner across one redemption and original
//! attachment submission, verifying cleanup on uncertain pairing/reconnect results.

use super::*;
use crate::runtime::x11::X11ForwardingMode;
use std::sync::atomic::{AtomicBool, Ordering};

/// Malformed attachment or failed local credential preparation must not consume
/// invitation proof. The callback records that role/envelope admission occurs
/// before preparation; the broker sees EOF with no pairing request.
#[tokio::test]
async fn broker_invitation_x11_rejects_before_redemption() {
    for case in ["observer", "oversized", "prepare-failed"] {
        let (home, env, paths, path) = fixture();
        let before = std::fs::read(&path).unwrap();
        let socket = paths.root().join("outbound.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let prepared = AtomicBool::new(false);
        let term = if case == "oversized" {
            "x".repeat(5 * 1024)
        } else {
            "xterm".to_string()
        };
        let peer = async {
            let (mut stream, _) = hello(&listener, 1).await;
            assert!(
                stream.next().await.is_none(),
                "rejected preparation must send no pairing"
            );
        };
        let client = try_open_with_preparation(
            &path,
            Some("alias"),
            &env,
            if case == "observer" {
                "observer"
            } else {
                "primary"
            },
            &IrohSessionRouting::Default,
            80,
            24,
            &term,
            Some((X11ForwardingMode::Untrusted, false)),
            None,
            None,
            |_| async {
                prepared.store(true, Ordering::SeqCst);
                Err(MezError::invalid_state("synthetic preparation failure"))
            },
        );
        let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(Box::pin(client), peer)
        })
        .await
        .unwrap();
        assert!(result.is_err());
        assert_eq!(prepared.load(Ordering::SeqCst), case == "prepare-failed");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!paths.root().join("remote/client/endpoint.key").exists());
        drop(listener);
        std::fs::remove_dir_all(home).unwrap();
    }
}

/// Builds a bounded binary authority record for a synthetic loopback target.
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

/// Generated credentials precede redemption and survive through successful
/// attachment handoff. Pairing/reconnect loss cleans private artifacts without
/// another redemption, endpoint fallback or changed attachment key. Only fake
/// offer proof is sent; the authored local authority and invitation stay unchanged.
#[tokio::test]
async fn broker_invitation_x11_handoff_retains_credentials_and_original_key() {
    for case in ["success", "pair-loss", "reconnect-loss"] {
        let (home, env, paths, path) = fixture();
        let before = std::fs::read(&path).unwrap();
        let source = home.join("source-authority");
        let generated = home.join("generated-authority");
        std::fs::write(&source, authority(17)).unwrap();
        std::fs::write(&generated, authority(52)).unwrap();
        for file in [&source, &generated] {
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let helper = home.join("fake-xauth");
        std::fs::write(&helper, format!(
            "#!/bin/sh\nif [ \"$5\" = generate ]; then cp {} \"$4\"; exit 0; fi\nif [ \"$5\" = remove ]; then : > \"$4\"; exit 0; fi\nexit 2\n",
            mez_agent::shell_quote(generated.to_str().unwrap()),
        )).unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        let private = std::cell::RefCell::new(None::<PathBuf>);
        let fake = std::cell::RefCell::new(None::<String>);
        let socket = paths.root().join("outbound.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let name = "x0123456789abcdef.sock";
        let dedicated = tokio::net::UnixListener::bind(paths.root().join(name)).unwrap();
        std::fs::set_permissions(
            paths.root().join(name),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let routing = IrohSessionRouting::Create {
            name: Some("fresh".into()),
            idempotency_key: "original-x11-invitation".into(),
        };
        let peer = async {
            let (mut pairing, handle) = hello(&listener, 1).await;
            let frame = pairing.next().await.unwrap().unwrap();
            assert!(
                private
                    .borrow()
                    .as_ref()
                    .unwrap()
                    .join("authority")
                    .exists()
            );
            let request: serde_json::Value = serde_json::from_str(&frame.body).unwrap();
            assert_eq!(
                request,
                serde_json::json!({"operation":"pair","handle":handle,"path":path,"save_as":"alias"})
            );
            assert!(!frame.body.contains("synthetic-private-proof"));
            if case == "pair-loss" {
                return;
            }
            pairing
                .send(ProtocolFrame::new(
                    "application/vnd.mezzanine.outbound+json",
                    serde_json::json!({"handle":handle,"paired":true,"profile":"alias"})
                        .to_string(),
                ))
                .await
                .unwrap();
            assert!(pairing.next().await.is_none());
            if case == "reconnect-loss" {
                let (mut reconnect, _) = listener.accept().await.unwrap();
                use tokio::io::AsyncReadExt;
                let mut bytes = [0; 4096];
                assert!(reconnect.read(&mut bytes).await.unwrap() > 0);
                return;
            }
            let (mut attached, handle) = hello(&listener, 2).await;
            let frame = attached.next().await.unwrap().unwrap();
            let setup: serde_json::Value = serde_json::from_str(&frame.body).unwrap();
            assert_eq!(
                setup["initialize"]["idempotency_key"],
                "original-x11-invitation"
            );
            assert_eq!(setup["initialize"]["x11_forwarding"]["mode"], "trusted");
            assert_eq!(setup["initialize"]["x11_forwarding"]["takeover"], true);
            assert_eq!(
                setup["initialize"]["x11_forwarding"]["fake_cookie_base64"],
                fake.borrow().as_ref().unwrap().as_str()
            );
            assert!(setup["initialize"].get("authentication").is_none());
            attached.next().await.unwrap().unwrap();
            let summary = serde_json::json!({"selected_version":3,"granted_role":"primary","session_id":"$1","lease_id":"lease-one","client_id":"c1"});
            attached.send(ProtocolFrame::new("application/vnd.mezzanine.outbound+json", serde_json::json!({
                "handle":handle,"session":summary,"lines":["retained"],"line_style_spans":[[]],
                "cursor":{"row":0,"column":0,"visible":false},"output_modes":{},"presentation_ids":[]
            }).to_string())).await.unwrap();
            let frame = attached.next().await.unwrap().unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&frame.body).unwrap()["operation"],
                "x11-discovery"
            );
            attached
                .send(ProtocolFrame::new(
                    "application/vnd.mezzanine.outbound+json",
                    serde_json::json!({
                        "handle":handle,"session":summary,"version":1,"socket_name":name
                    })
                    .to_string(),
                ))
                .await
                .unwrap();
            assert!(
                attached.next().await.is_none(),
                "handoff must not replay setup"
            );
        };
        let client = async {
            let source_ref = &source;
            let helper_ref = &helper;
            let private_ref = &private;
            let fake_ref = &fake;
            let result = try_open_with_preparation(
                &path,
                Some("alias"),
                &env,
                "primary",
                &routing,
                80,
                24,
                "xterm",
                Some((X11ForwardingMode::Trusted, true)),
                None,
                None,
                |mode| async move {
                    let (prepared, directory) = crate::cli::x11::prepare_broker_x11_for_tests(
                        mode,
                        source_ref,
                        helper_ref.as_os_str(),
                    )
                    .await?;
                    *private_ref.borrow_mut() = Some(directory);
                    *fake_ref.borrow_mut() = Some(
                        base64::engine::general_purpose::STANDARD
                            .encode(prepared.offer(true).fake_cookie.as_bytes()),
                    );
                    Ok(prepared)
                },
            )
            .await;
            if case == "success" {
                let BrokerAttachment { session, x11, .. } = result.unwrap().unwrap();
                assert!(private.borrow().as_ref().unwrap().exists());
                let x11 = x11.unwrap();
                drop((session, x11.opener));
                x11.prepared.close().await.unwrap();
            } else {
                assert!(result.is_err());
            }
            assert!(!private.borrow().as_ref().unwrap().exists());
        };
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(Box::pin(peer), Box::pin(client));
        })
        .await
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(std::fs::read(&source).unwrap(), authority(17));
        assert!(!paths.root().join("remote/client/endpoint.key").exists());
        drop((listener, dedicated));
        std::fs::remove_dir_all(home).unwrap();
    }
}
