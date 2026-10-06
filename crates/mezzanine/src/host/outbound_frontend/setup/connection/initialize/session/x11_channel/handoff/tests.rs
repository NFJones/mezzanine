//! Dedicated Unix handoff qualification; no listener or physical X server effects.
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Provides inert validated-session-shaped evidence for exact comparison. The
/// handoff cannot use these synthetic labels to acquire any remote authority.
fn identity() -> (FrontendHandle, SessionSummary) {
    (
        FrontendHandle {
            owner: "f".repeat(32),
            generation: 1,
        },
        serde_json::from_value(
            serde_json::json!({"selected_version":3,"granted_role":"primary",
            "session_id":"$1","lease_id":"lease-one","client_id":"c1"}),
        )
        .unwrap(),
    )
}

/// Readiness must precede the raw byte transition on the same dedicated stream.
/// The closed reply preserves exact identities without token fields; raw bytes
/// afterwards are delivered without framing loss or any control-channel traffic.
#[tokio::test]
async fn outbound_x11_handoff_preserves_exact_owner_and_raw_transition() {
    let (handle, session) = identity();
    let (server, client) = tokio::net::UnixStream::pair().unwrap();
    let peer =
        async {
            let mut framed = Framed::new(client, ProtocolFrameCodec::new(HELLO_LIMIT).unwrap());
            framed.send(ProtocolFrame::new(CONTENT_TYPE, serde_json::json!({
            "protocol":"mez-outbound-x11/1","handle":handle,"session":session,"occurrence":7
        }).to_string())).await.unwrap();
            let reply = framed.next().await.unwrap().unwrap();
            let reply: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
            assert_eq!(
                reply,
                serde_json::json!({"protocol":"mez-outbound-x11/1","handle":handle,
            "session":session,"occurrence":7,"ready":true})
            );
            assert!(framed.read_buffer().is_empty());
            let mut raw = framed.into_inner();
            raw.write_all(b"raw-tail").await.unwrap();
            raw.shutdown().await.unwrap();
        };
    let admitted = async {
        let mut raw = authenticate_frontend(
            server,
            crate::runtime::current_effective_uid(),
            &handle,
            &session,
            7,
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        let mut bytes = Vec::new();
        raw.read_to_end(&mut bytes).await.unwrap();
        assert_eq!(bytes, b"raw-tail");
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(peer, admitted);
    })
    .await
    .unwrap();
}

/// Wrong UID, MIME, protocol, handle, session, occurrence and extra credential
/// fields must produce no ready reply. Coalesced premature bytes are rejected,
/// not silently lost when the framed stream becomes raw; silent peers time out.
#[tokio::test]
async fn outbound_x11_handoff_rejects_foreign_and_premature_streams() {
    for case in [
        "uid",
        "type",
        "protocol",
        "handle",
        "session",
        "occurrence",
        "extra",
        "pipeline",
        "silent",
    ] {
        let (handle, session) = identity();
        let (server, mut client) = tokio::net::UnixStream::pair().unwrap();
        let mut body = serde_json::json!({"protocol":"mez-outbound-x11/1","handle":handle,
            "session":session,"occurrence":7});
        match case {
            "protocol" => body["protocol"] = serde_json::json!("wrong"),
            "handle" => body["handle"]["generation"] = serde_json::json!(2),
            "session" => body["session"]["client_id"] = serde_json::json!("c2"),
            "occurrence" => body["occurrence"] = serde_json::json!(8),
            "extra" => body["token"] = serde_json::json!("private-proof"),
            _ => {}
        }
        if case != "silent" {
            let mut bytes = crate::protocol::framing::encode_frame(&ProtocolFrame::new(
                if case == "type" {
                    "application/json"
                } else {
                    CONTENT_TYPE
                },
                body.to_string(),
            ));
            if case == "pipeline" {
                bytes.extend_from_slice(b"premature-raw");
            }
            client.write_all(&bytes).await.unwrap();
        }
        let uid = crate::runtime::current_effective_uid();
        let result = authenticate_frontend(
            server,
            if case == "uid" {
                uid.wrapping_add(1)
            } else {
                uid
            },
            &handle,
            &session,
            7,
            Duration::from_millis(100),
        )
        .await;
        assert!(result.is_err(), "{case}");
        let mut bytes = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(1), client.read_to_end(&mut bytes))
            .await
            .unwrap();
        if let Err(error) = read {
            assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset);
        }
        assert!(bytes.is_empty(), "{case} must emit no readiness");
    }
}
