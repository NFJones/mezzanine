//! Local Unix admission qualification without remote application operations.
//!
//! Pair fixtures exercise real kernel peer authentication, strict negotiation,
//! finite capacity, exact handle fencing and endpoint retention. No listener,
//! principal pairing, session creation or user terminal traffic is performed.

use super::*;
use crate::runtime::RuntimeIrohTransportPolicy;

/// Builds one isolated endpoint owner whose caller retains graceful teardown.
async fn fixture() -> (std::path::PathBuf, OutboundEndpointOwner) {
    let root = std::env::temp_dir().join(format!("mez-frontend-{:032x}", rand::random::<u128>()));
    let endpoint = OutboundEndpointOwner::bind(&root, &RuntimeIrohTransportPolicy::default())
        .await
        .unwrap();
    (root, endpoint)
}

/// Sends a versioned hello through a real Unix pair and returns both stream
/// owners. The response contains only inert handle identity, never credentials.
async fn admitted(
    admission: &OutboundFrontendAdmission,
) -> (
    AdmittedFrontend,
    Framed<tokio::net::UnixStream, ProtocolFrameCodec>,
) {
    let (server, client) = tokio::net::UnixStream::pair().unwrap();
    let client = async {
        let mut client = Framed::new(client, ProtocolFrameCodec::new(HELLO_LIMIT).unwrap());
        client
            .send(ProtocolFrame::new(
                CONTENT_TYPE,
                serde_json::json!({"protocol":PROTOCOL}).to_string(),
            ))
            .await
            .unwrap();
        let response = client.next().await.unwrap().unwrap();
        assert_eq!(response.content_type, CONTENT_TYPE);
        let value: serde_json::Value = serde_json::from_str(&response.body).unwrap();
        assert_eq!(value["protocol"], PROTOCOL);
        assert_eq!(value.as_object().unwrap().len(), 2);
        assert_eq!(value["handle"].as_object().unwrap().len(), 2);
        client
    };
    let (frontend, client) = tokio::join!(admission.admit(server), client);
    (frontend.unwrap(), client)
}

/// Independent frontend handles consume finite slots and cannot impersonate
/// siblings. Disposing one stream preserves sibling usability and permits a new
/// occurrence, whose generation is not reused. Endpoint shutdown stays fenced.
#[tokio::test]
async fn outbound_frontend_handles_are_bounded_and_exact() {
    let (root, endpoint) = fixture().await;
    let admission =
        OutboundFrontendAdmission::new(endpoint.clone(), 2, Duration::from_secs(2)).unwrap();
    let (first, first_client) = admitted(&admission).await;
    let (mut second, mut second_client) = admitted(&admission).await;
    assert_ne!(first.handle(), second.handle());
    admission.validate_handle(&first, first.handle()).unwrap();
    assert!(admission.validate_handle(&second, first.handle()).is_err());
    let (server, _client) = tokio::net::UnixStream::pair().unwrap();
    assert_eq!(
        admission.admit(server).await.err().unwrap().kind(),
        MezErrorKind::RateLimited
    );
    assert!(endpoint.clone().begin_shutdown().is_err());
    let old = first.handle().clone();
    drop(first);
    drop(first_client);
    let (third, third_client) = admitted(&admission).await;
    assert!(third.handle().generation > old.generation);
    assert!(admission.validate_handle(&third, &old).is_err());
    second_client
        .send(ProtocolFrame::new(CONTENT_TYPE, "{}"))
        .await
        .unwrap();
    assert_eq!(second.stream.next().await.unwrap().unwrap().body, "{}");
    drop(second);
    drop(second_client);
    drop(third);
    drop(third_client);
    drop(admission);
    endpoint.begin_shutdown().unwrap().finish().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

/// Unsupported versions, unknown fields and oversized input fail without
/// consuming lasting capacity. A cancelled silent handshake releases its slot;
/// authentication rejects the wrong owner UID before waiting for peer bytes.
#[tokio::test]
async fn outbound_frontend_rejection_and_cancellation_release_capacity() {
    let (root, endpoint) = fixture().await;
    let mut admission =
        OutboundFrontendAdmission::new(endpoint.clone(), 1, Duration::from_millis(100)).unwrap();
    for body in [
        serde_json::json!({"protocol":"mez-outbound/2"}).to_string(),
        serde_json::json!({"protocol":PROTOCOL,"operation":"create"}).to_string(),
        "x".repeat(HELLO_LIMIT + 1),
    ] {
        let (server, mut client) = tokio::net::UnixStream::pair().unwrap();
        use tokio::io::AsyncWriteExt;
        let bytes = crate::protocol::framing::encode_frame(&ProtocolFrame::new(CONTENT_TYPE, body));
        client.write_all(&bytes).await.unwrap();
        assert!(admission.admit(server).await.is_err());
        assert_eq!(admission.slots.available_permits(), 1);
    }
    let (server, _client) = tokio::net::UnixStream::pair().unwrap();
    let mut pending = Box::pin(admission.admit(server));
    assert!(matches!(
        futures_util::poll!(&mut pending),
        std::task::Poll::Pending
    ));
    assert_eq!(admission.slots.available_permits(), 0);
    drop(pending);
    assert_eq!(admission.slots.available_permits(), 1);
    let (server, _client) = tokio::net::UnixStream::pair().unwrap();
    assert!(admission.admit(server).await.is_err());
    assert_eq!(admission.slots.available_permits(), 1);
    admission.owner_uid = admission.owner_uid.wrapping_add(1);
    let (server, _client) = tokio::net::UnixStream::pair().unwrap();
    assert_eq!(
        admission.admit(server).await.err().unwrap().kind(),
        MezErrorKind::Forbidden
    );
    drop(admission);
    endpoint.begin_shutdown().unwrap().finish().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
