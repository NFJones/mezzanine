//! Offline whole-wire epoch regressions using owned private Unix sockets only.
//!
//! These fixtures never bind an Iroh endpoint, create keys/profiles, spawn tasks
//! or processes, or send application operations. All futures are joined under a
//! deadline; the fixture closes its listener before removing only its own paths.

use super::*;
use crate::protocol::framing::{ProtocolFrame, ProtocolFrameCodec};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;
use tokio_util::codec::Framed;

const CONTENT_TYPE: &str = "application/vnd.mezzanine.outbound+json";
const CURRENT: &str = "mez-outbound/2";

/// Private, unique publication owned by this fixture, with no endpoint identity.
struct FakeBroker {
    root: PathBuf,
    listener: Option<tokio::net::UnixListener>,
    identity: ((u64, u64), (u64, u64)),
}

impl FakeBroker {
    /// Creates just a private directory and socket, never configuration or keys.
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("mez-wire-{:032x}", rand::random::<u128>()));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("outbound.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let root_stat = std::fs::symlink_metadata(&root).unwrap();
        let socket_stat = std::fs::symlink_metadata(path).unwrap();
        Self {
            root,
            listener: Some(listener),
            identity: (
                (root_stat.dev(), root_stat.ino()),
                (socket_stat.dev(), socket_stat.ino()),
            ),
        }
    }

    /// Accepts one scoped stream; its caller retains and disposes both halves.
    async fn accept(&self) -> Framed<tokio::net::UnixStream, ProtocolFrameCodec> {
        let (stream, _) = self.listener.as_ref().unwrap().accept().await.unwrap();
        Framed::new(stream, ProtocolFrameCodec::new(4096).unwrap())
    }

    /// Proves readiness did not replace publication, create election or identity
    /// state, or retry a connection. Successful cleanup is asserted explicitly.
    fn finish(mut self) {
        let root_stat = std::fs::symlink_metadata(&self.root).unwrap();
        let socket_stat = std::fs::symlink_metadata(self.root.join("outbound.sock")).unwrap();
        assert_eq!(
            self.identity,
            (
                (root_stat.dev(), root_stat.ino()),
                (socket_stat.dev(), socket_stat.ino())
            )
        );
        let entries: Vec<_> = std::fs::read_dir(&self.root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries, [std::ffi::OsString::from("outbound.sock")]);
        drop(self.listener.take());
        std::fs::remove_file(self.root.join("outbound.sock")).unwrap();
        std::fs::remove_dir(&self.root).unwrap();
    }
}

impl Drop for FakeBroker {
    /// Panic cleanup cannot leave a listener or detached work. Only fixture-owned
    /// paths are removed; unexpected extra state is not recursively deleted.
    fn drop(&mut self) {
        drop(self.listener.take());
        let _ = std::fs::remove_file(self.root.join("outbound.sock"));
        let _ = std::fs::remove_dir(&self.root);
    }
}

/// Builds an inert, exact-shaped readiness reply, without remote credentials.
fn reply(protocol: &str, generation: u64) -> Value {
    json!({"protocol":protocol,"handle":{"owner":"00000000000000000000000000000001","generation":generation}})
}

/// An unchanged historical hello could admit setup and an unconditional view,
/// then reject the newly added conditional field. This is a schema reproduction,
/// not proof that the unavailable incident broker actually had that version.
#[test]
fn outbound_wire_epoch_old_schema_explains_late_conditional_rejection() {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct OldHello {
        protocol: String,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct OldView {
        handle: Value,
        columns: u16,
        rows: u16,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct OldSetup {
        handle: Value,
        profile: String,
        initialize: Value,
    }
    let hello: OldHello = serde_json::from_value(json!({"protocol":"mez-outbound/1"})).unwrap();
    assert_eq!(hello.protocol, "mez-outbound/1");
    let handle = reply("mez-outbound/1", 1)["handle"].clone();
    let setup = json!({"handle":handle,"profile":"synthetic","initialize":{
        "client_name":"fixture","requested_version":3,"requested_role":"observer","session_intent":"host_only"
    }});
    let setup: OldSetup = serde_json::from_value(setup).unwrap();
    assert_eq!(setup.profile, "synthetic");
    let _: crate::host::outbound_frontend::FrontendHandle =
        serde_json::from_value(setup.handle).unwrap();
    crate::control::initialize_params_from_json(&setup.initialize.to_string()).unwrap();
    let mut view = json!({"handle":handle,"columns":80,"rows":24});
    let old: OldView = serde_json::from_value(view.clone()).unwrap();
    assert_eq!((old.handle, old.columns, old.rows), (handle, 80, 24));
    view["if_view_identity"] = json!("a".repeat(64));
    assert!(serde_json::from_value::<OldView>(view).is_err());
}

/// Old/future epochs, missing contract evidence, malformed/extra fields and
/// invalid exact handles must fail before setup, pairing, terminal or profile
/// bytes. EOF models an old strict broker closing on an unsupported hello.
/// Startup must not launch, elect, replace discovery or retry these failures.
#[tokio::test]
async fn outbound_wire_epoch_incompatible_readiness_never_launches_or_sends_setup() {
    let mut invalid_owner = reply(CURRENT, 1);
    invalid_owner["handle"]["owner"] = json!("untrusted-secret-sentinel");
    let mut extra = reply(CURRENT, 1);
    extra["untrusted-secret-sentinel"] = json!(true);
    let mut missing_handle_field = reply(CURRENT, 1);
    missing_handle_field["handle"]
        .as_object_mut()
        .unwrap()
        .remove("generation");
    let cases = [
        Some(ProtocolFrame::new(
            CONTENT_TYPE,
            reply("mez-outbound/1", 1).to_string(),
        )),
        Some(ProtocolFrame::new(
            CONTENT_TYPE,
            reply("mez-outbound/3", 1).to_string(),
        )),
        Some(ProtocolFrame::new(
            CONTENT_TYPE,
            json!({"handle":reply(CURRENT, 1)["handle"]}).to_string(),
        )),
        Some(ProtocolFrame::new(
            CONTENT_TYPE,
            missing_handle_field.to_string(),
        )),
        Some(ProtocolFrame::new(CONTENT_TYPE, extra.to_string())),
        Some(ProtocolFrame::new(
            CONTENT_TYPE,
            reply(CURRENT, 0).to_string(),
        )),
        Some(ProtocolFrame::new(CONTENT_TYPE, invalid_owner.to_string())),
        Some(ProtocolFrame::new(
            CONTENT_TYPE,
            "untrusted-secret-sentinel",
        )),
        Some(ProtocolFrame::new(
            "text/plain",
            reply(CURRENT, 1).to_string(),
        )),
        None,
    ];
    for response in cases {
        let fixture = FakeBroker::new();
        let launches = AtomicUsize::new(0);
        let peer = async {
            let mut peer = fixture.accept().await;
            let request = peer.next().await.unwrap().unwrap();
            assert_eq!(request.content_type, CONTENT_TYPE);
            if let Some(response) = response {
                peer.send(response).await.unwrap();
                assert!(
                    peer.next().await.is_none(),
                    "no application operation may follow rejected readiness"
                );
            }
        };
        let client = async {
            connect_with_launcher(&fixture.root, Duration::from_secs(2), |_| {
                launches.fetch_add(1, Ordering::SeqCst);
                Err(MezError::invalid_state("unexpected fixture launch"))
            })
            .await
            .err()
        };
        let (error, ()) =
            tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(client, peer) })
                .await
                .unwrap();
        let error = error.expect("incompatible broker must not establish readiness");
        assert!(
            error.io_kind().is_none(),
            "connected failure must not authorize startup: {error}"
        );
        assert!(!error.message().contains("untrusted-secret-sentinel"));
        assert!(
            error.message().contains("owner"),
            "owner-aware recovery guidance required: {error}"
        );
        assert_eq!(launches.load(Ordering::SeqCst), 0);
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                fixture.listener.as_ref().unwrap().accept()
            )
            .await
            .is_err()
        );
        fixture.finish();
    }
}

/// Concurrent current-epoch frontends retain distinct exact streams. Rejecting
/// another candidate must not invalidate retained handles or replace discovery;
/// this readiness-only fixture does not claim application stream usability.
/// Successful readiness itself emits no setup or application requests.
#[tokio::test]
async fn outbound_wire_epoch_current_concurrent_reuse_preserves_siblings() {
    let fixture = FakeBroker::new();
    let peer = async {
        let mut streams = Vec::new();
        for generation in 1..=3 {
            let mut peer = fixture.accept().await;
            let request = peer.next().await.unwrap().unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&request.body).unwrap(),
                json!({"protocol":CURRENT})
            );
            peer.send(ProtocolFrame::new(
                CONTENT_TYPE,
                reply(
                    if generation == 3 {
                        "mez-outbound/1"
                    } else {
                        CURRENT
                    },
                    generation,
                )
                .to_string(),
            ))
            .await
            .unwrap();
            if generation == 3 {
                assert!(peer.next().await.is_none());
            } else {
                streams.push(peer);
            }
        }
        for peer in &mut streams {
            assert!(peer.next().await.is_none());
        }
    };
    let clients = async {
        let (first, second) = tokio::join!(
            OutboundFrontendClient::connect(&fixture.root, Duration::from_secs(2)),
            OutboundFrontendClient::connect(&fixture.root, Duration::from_secs(2))
        );
        let first = first.unwrap();
        let second = second.unwrap();
        assert_ne!(first.handle().unwrap(), second.handle().unwrap());
        assert!(
            OutboundFrontendClient::connect(&fixture.root, Duration::from_secs(2))
                .await
                .is_err()
        );
        // Public client ownership remains valid after the candidate's rejection.
        first.handle().unwrap();
        second.handle().unwrap();
        drop((first, second));
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(clients, peer)
    })
    .await
    .unwrap();
    fixture.finish();
}
