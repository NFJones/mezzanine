//! Real same-user Unix lifecycle transport and exact acknowledgment fixtures.
//!
//! Synthetic capabilities never authorize a production pane. These fixtures
//! verify one restricted request per call, bounded strict reply parsing, and
//! retention of exact pending work after ambiguous delivery or peer failures.

use super::*;
use crate::integrations::bootstrap::pi::Observation;

/// Unique owned socket directory, removed after both ends settle.
struct Fixture {
    root: PathBuf,
    listener: tokio::net::UnixListener,
    owner: LifecycleOwner,
    transport: CapabilityTransport,
}

impl Fixture {
    /// Creates a same-user endpoint and synthetic, explicitly supplied capability.
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "mez-pi-transport-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        std::fs::create_dir(&root).unwrap();
        let socket = root.join("control.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let owner = LifecycleOwner::new("bound").unwrap();
        let transport =
            CapabilityTransport::new(&socket, SecretString::from("x".repeat(43)), 7, &owner)
                .unwrap();
        Self {
            root,
            listener,
            owner,
            transport,
        }
    }
}

impl Drop for Fixture {
    /// Removes only this fixture's owned directory; no user socket cleanup.
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

/// Reads one strict request and verifies that credentials cannot widen its RPC.
async fn request(
    listener: &tokio::net::UnixListener,
) -> (tokio::net::UnixStream, serde_json::Value) {
    let (mut stream, _) = listener.accept().await.unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let count = stream.read(&mut buffer).await.unwrap();
        assert!(count > 0 && bytes.len() + count <= MAX_BODY + 8192);
        bytes.extend_from_slice(&buffer[..count]);
        if let Some((frame, consumed)) =
            crate::protocol::framing::decode_frame_incremental(&bytes, MAX_BODY).unwrap()
        {
            assert_eq!(consumed, bytes.len());
            let parsed = crate::control::parse_json_rpc_request(&frame.body).unwrap();
            crate::control::validate_control_method_params_schema(&parsed).unwrap();
            assert_ne!(parsed.method, "control/initialize");
            let value: serde_json::Value = serde_json::from_str(&frame.body).unwrap();
            assert_eq!(value["params"]["generation"], 7);
            assert_eq!(value["params"]["external_session_id"], "bound");
            return (stream, value);
        }
    }
}

/// Encodes synthetic peer response evidence, never raw vendor content.
fn reply(result: serde_json::Value) -> Vec<u8> {
    crate::control::encode_control_body(
        &serde_json::json!({
            "jsonrpc":"2.0", "id":"pi-lifecycle", "result":result,
        })
        .to_string(),
    )
}

/// Typed register/renew evidence and fragmented presentation replies settle only
/// the exact owner head. Retirement uses its restricted method, never initialize.
#[tokio::test(flavor = "current_thread")]
async fn pi_transport_acknowledges_restricted_lifecycle_operations() {
    let mut fixture = Fixture::new();
    for method in ["agent/external/register", "agent/external/renew"] {
        let server = async {
            let (mut stream, value) = request(&fixture.listener).await;
            assert_eq!(value["method"], method);
            if method.ends_with("register") {
                assert_eq!(value["params"]["display_name"], "Pi");
            }
            stream
                .write_all(&reply(
                    serde_json::json!({"agent_id":"external-one", "generation":7,
                "expires_at_unix_seconds":100, "registered":true}),
                ))
                .await
                .unwrap();
        };
        let client = async {
            if method.ends_with("register") {
                fixture.transport.register("Pi").await
            } else {
                fixture.transport.renew().await
            }
        };
        let (ack, ()) = tokio::join!(client, server);
        assert_eq!(
            ack.unwrap(),
            LeaseAck {
                agent_id: "external-one".into(),
                expires_at: 100
            }
        );
    }
    fixture
        .owner
        .observe(1, "bound", Observation::Running)
        .unwrap();
    let head = fixture.owner.pending().unwrap().clone();
    let server = async {
        let (mut stream, value) = request(&fixture.listener).await;
        assert_eq!(value["method"], "agent/external/presentation");
        assert_eq!(value["params"]["state"], "running");
        assert_eq!(value["params"]["sequence"], head.sequence);
        let bytes = reply(serde_json::json!({"sequence":head.sequence,"changed":false}));
        for chunk in bytes.chunks(13) {
            stream.write_all(chunk).await.unwrap();
            tokio::task::yield_now().await;
        }
    };
    let (result, ()) = tokio::join!(fixture.transport.deliver_next(&mut fixture.owner), server);
    assert!(result.unwrap());
    assert!(fixture.owner.pending().is_none());
    fixture
        .owner
        .observe(1, "bound", Observation::SessionShutdown { reason: "quit" })
        .unwrap();
    let server = async {
        let (mut stream, value) = request(&fixture.listener).await;
        assert_eq!(value["method"], "agent/external/deregister");
        stream
            .write_all(&reply(serde_json::json!({"retired":true,"changed":false})))
            .await
            .unwrap();
    };
    let (result, ()) = tokio::join!(fixture.transport.deliver_next(&mut fixture.owner), server);
    assert!(result.unwrap());
    assert!(fixture.owner.pending().is_none());
}

/// Error/ambiguous ownership, malformed framing and missing typed acknowledgment
/// preserve the original delivery and never echo arbitrary peer secrets.
#[tokio::test(flavor = "current_thread")]
async fn pi_transport_rejects_invalid_replies_without_consuming_work() {
    let mut fixture = Fixture::new();
    fixture
        .owner
        .observe(1, "bound", Observation::Running)
        .unwrap();
    let original = fixture.owner.pending().unwrap().clone();
    let cases = [
        crate::control::encode_control_body(
            r#"{"jsonrpc":"2.0","id":"other","result":{"sequence":1,"changed":true}}"#,
        ),
        crate::control::encode_control_body(
            r#"{"jsonrpc":"2.0","id":"pi-lifecycle","result":{"sequence":1,"changed":true},"error":{"message":"PRIVATE"}}"#,
        ),
        reply(serde_json::json!({"sequence":2,"changed":true})),
        reply(serde_json::json!({"sequence":1,"changed":"PRIVATE"})),
        b"Content-Length: invalid\r\n\r\n".to_vec(),
        b"Content-Length: 65537\r\n\r\n".to_vec(),
        vec![b'x'; 8193],
        Vec::new(),
    ];
    for bytes in cases {
        let server = async {
            let (mut stream, _) = request(&fixture.listener).await;
            let _ = stream.write_all(&bytes).await;
        };
        let (result, ()) = tokio::join!(fixture.transport.deliver_next(&mut fixture.owner), server);
        let error = result.unwrap_err();
        assert_eq!(error.message(), "Pi lifecycle delivery unavailable");
        assert_eq!(fixture.owner.pending(), Some(&original));
    }
}

/// A lost reply after request delivery leaves exact work retained; an explicitly
/// caller-selected retry sends the same sequence. The transport never retries.
#[tokio::test(flavor = "current_thread")]
async fn pi_transport_lost_reply_retains_identity_and_stalls_have_one_deadline() {
    let mut fixture = Fixture::new();
    fixture
        .owner
        .observe(1, "bound", Observation::Running)
        .unwrap();
    let original = fixture.owner.pending().unwrap().clone();
    let server = async {
        let (_stream, value) = request(&fixture.listener).await;
        assert_eq!(value["params"]["sequence"], original.sequence);
        tokio::time::sleep(DEADLINE + Duration::from_millis(100)).await;
    };
    let (result, ()) = tokio::join!(fixture.transport.deliver_next(&mut fixture.owner), server);
    assert!(result.is_err());
    assert_eq!(fixture.owner.pending(), Some(&original));
    let server = async {
        let (mut stream, value) = request(&fixture.listener).await;
        assert_eq!(value["params"]["sequence"], original.sequence);
        stream
            .write_all(&reply(
                serde_json::json!({"sequence":original.sequence,"changed":false}),
            ))
            .await
            .unwrap();
    };
    let (result, ()) = tokio::join!(fixture.transport.deliver_next(&mut fixture.owner), server);
    assert!(result.unwrap());
    assert!(fixture.owner.pending().is_none());
}

/// Same-session foreign reducer instances and invalid capabilities fail before
/// connecting; returned diagnostics cannot disclose tokens or supplied paths.
#[tokio::test(flavor = "current_thread")]
async fn pi_transport_rejects_foreign_owner_and_invalid_lease_evidence() {
    let fixture = Fixture::new();
    let mut foreign = LifecycleOwner::new("bound").unwrap();
    foreign.observe(1, "bound", Observation::Running).unwrap();
    assert!(fixture.transport.deliver_next(&mut foreign).await.is_err());
    assert!(foreign.pending().is_some());
    for result in [
        serde_json::json!({"agent_id":"external-one","generation":8,"expires_at_unix_seconds":100,"registered":true}),
        serde_json::json!({"agent_id":"PRIVATE\n","generation":7,"expires_at_unix_seconds":100,"registered":true}),
        serde_json::json!({"agent_id":"external-one","generation":7,"expires_at_unix_seconds":0,"registered":true}),
        serde_json::json!({"agent_id":"external-one","generation":7,"expires_at_unix_seconds":100,"registered":false}),
    ] {
        let server = async {
            let (mut stream, _) = request(&fixture.listener).await;
            stream.write_all(&reply(result)).await.unwrap();
        };
        let (result, ()) = tokio::join!(fixture.transport.register("Pi"), server);
        assert_eq!(
            result.unwrap_err().message(),
            "Pi lifecycle delivery unavailable"
        );
    }
    let result = CapabilityTransport::new(
        Path::new("PRIVATE"),
        SecretString::from("SECRET"),
        0,
        &foreign,
    );
    assert_eq!(
        result.err().unwrap().message(),
        "Pi lifecycle delivery unavailable"
    );
}
