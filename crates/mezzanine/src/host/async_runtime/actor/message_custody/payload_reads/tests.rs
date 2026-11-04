//! Actual-root deferred reads: cancellation, allocation pressure and rebinding.

use super::*;
use crate::host::async_runtime::{AsyncRuntimeActorConfig, mmp_custody::CustodyWorkerStatus};
use crate::storage::mmp_custody::CustodyStoreLimits;
use mez_agent::messaging::{
    Envelope, MessagePayloadObserver, MessageScope, MessageStorageLimits, Recipient, SenderIdentity,
};
use std::path::PathBuf;

/// One cooperating owner drives actual store setup before entering actor events.
/// No forged result, parallel connection, fake join or provider execution is used.
struct Scene {
    actor: AsyncRuntimeSessionActor,
    root: MmpCustodyRoot,
    sender: SenderIdentity,
    first: SenderIdentity,
    second: SenderIdentity,
    usage: MessagePayloadObserver,
    path: PathBuf,
}

impl Scene {
    /// Creates a real empty private root and a cache smaller than two bodies.
    async fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "mez-payload-read-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let root = MmpCustodyRoot::open(
            path.clone(),
            "read-session".into(),
            CustodyStoreLimits::default(),
            tokio::runtime::Handle::current(),
        );
        assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
        let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
        let messages = service.message_service_mut();
        let sender = messages.register_agent(None, None, "sender", vec![]);
        let first = messages.register_agent(None, None, "first", vec![]);
        let second = messages.register_agent(None, None, "second", vec![]);
        for target in [&first, &second] {
            messages.subscribe(&target.agent_id).unwrap();
        }
        let (_, mut actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        actor.register_message_custody(root.clone()).unwrap();
        actor
            .enable_message_payload_spill(MessageStorageLimits {
                cache_messages: 4,
                cache_payload_bytes: 64,
                ..Default::default()
            })
            .unwrap();
        let usage = actor.service.message_service().payload_observer().unwrap();
        Self {
            actor,
            root,
            sender,
            first,
            second,
            usage,
            path,
        }
    }

    /// Seeds an original using the same real lower/root write and adoption as
    /// the actor; the setup driver never overlaps an issued actor continuation.
    async fn seed(&mut self, target: SenderIdentity, id: &str, bytes: usize) {
        self.seed_with_ttl(target, id, bytes, None).await;
    }

    /// Persists the original acceptance-time TTL before any read can be queued.
    async fn seed_with_ttl(
        &mut self,
        target: SenderIdentity,
        id: &str,
        bytes: usize,
        ttl_ms: Option<u64>,
    ) {
        let messages = self.actor.service.message_service_mut();
        let envelope = Envelope {
            protocol: "mmp/1",
            id: id.into(),
            message_type: "send".into(),
            time: "original".into(),
            sender: self.sender.clone(),
            recipient: Recipient::Agent(target.agent_id),
            correlation_id: None,
            ttl_ms,
            content_type: "text/plain".into(),
            payload: "x".repeat(bytes),
            extension_fields: vec![],
        };
        let ticket = messages
            .prepare_acceptance(
                &self.sender.agent_id,
                envelope,
                MessageScope::Session,
                10,
                4096,
                None,
            )
            .unwrap();
        self.root.start(messages, &ticket, None).unwrap();
        assert_eq!(
            self.root.observe().await.unwrap(),
            CustodyWorkerStatus::AwaitingAdoption
        );
        self.root.adopt(messages, None).unwrap();
    }

    /// Observes a real registered job then applies that exact root event.
    async fn settle(&mut self) {
        let event = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            self.actor.message_custody_wakeup(),
        )
        .await
        .unwrap();
        self.actor.apply_message_custody_event(event);
    }
}
impl Drop for Scene {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Exhausting only captured metadata must not put an unissued native capture
/// ahead of an original positive ACK. That ACK uses the actual idle root and
/// reserved worker headroom; releasing the held captures then lets the actor
/// journal the current native interval without a message-sequence allocation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refused_native_capture_allows_actual_ack_to_release_custody() {
    let mut scene = Scene::new().await;
    scene
        .seed(scene.first.clone(), "original-before-native", 40)
        .await;
    scene.settle().await; // Actual idle observation clears registration wake.
    let batch = scene
        .actor
        .service
        .message_service()
        .fanout_ready_for(&scene.first.agent_id, 20, 1)
        .unwrap()
        .unwrap();
    scene
        .actor
        .service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let native = scene
        .actor
        .service
        .ensure_runtime_message_identity(
            "agent-%1",
            mez_core::ids::PaneId::opaque("%1"),
            "agent",
            &["agent-harness"],
            21,
        )
        .unwrap();
    scene
        .actor
        .service
        .refresh_native_message_bindings()
        .unwrap();
    let mut held = Vec::new();
    while let Ok(request) = scene
        .actor
        .service
        .message_service()
        .prepare_native_continuity(scene.root.storage_scope(), &native.agent_id)
    {
        held.push(request);
        assert!(
            held.len() < 10_000,
            "capture leases must enforce finite metadata"
        );
    }
    assert!(!held.is_empty());
    scene.actor.poll_message_custody();
    let owner = scene.actor.message_custody.as_ref().unwrap();
    assert!(owner.pending.is_none());
    assert!(matches!(owner.wake, Some(Wake::NativeCaptureRetry(_))));
    assert_eq!(
        scene.root.observe().await.unwrap(),
        CustodyWorkerStatus::Idle
    );
    let (reply, receive) = oneshot::channel();
    scene.actor.acknowledge_message_fanout(batch, reply);
    assert!(matches!(
        scene.actor.message_custody.as_ref().unwrap().pending,
        Some(Pending::Acknowledgement(_))
    ));
    scene.settle().await;
    assert_eq!(receive.await.unwrap().unwrap().last_sequence, 1);
    assert!(
        !scene
            .actor
            .service
            .message_service()
            .native_continuity_is_recorded(&native.agent_id)
    );
    drop(held);
    scene.settle().await; // Actual idle observation after ACK adoption.
    scene.actor.poll_message_custody();
    scene.settle().await;
    assert!(
        scene
            .actor
            .service
            .message_service()
            .native_continuity_is_recorded(&native.agent_id)
    );
    assert_eq!(
        scene
            .actor
            .service
            .message_service()
            .next_message_sequence(),
        2
    );
}

/// Cancelling an unissued read discards only its small continuation. It neither
/// claims a physical worker nor evicts/pins a second payload for an absent caller.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_queued_reader_does_not_issue_or_allocate_payload() {
    let mut scene = Scene::new().await;
    scene.seed(scene.first.clone(), "cold", 40).await;
    scene.seed(scene.second.clone(), "hot", 40).await;
    let before = scene.usage.snapshot().unwrap();
    let (reply, receive) = oneshot::channel();
    scene
        .actor
        .message_fanout_request(scene.first.agent_id.clone(), 11, 64, reply);
    drop(receive);
    scene.actor.poll_message_custody();
    assert_eq!(
        scene.root.observe().await.unwrap(),
        CustodyWorkerStatus::Idle
    );
    assert!(
        scene
            .actor
            .message_custody
            .as_ref()
            .unwrap()
            .payload_reads
            .is_empty()
    );
    assert_eq!(scene.usage.snapshot().unwrap(), before);
}

/// A deferred request remains bound to its exact registration even if public
/// ID/floors now identify another recipient. The replacement's fresh mailbox is
/// provably readable, but an old queued observer receives none of its new data.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deferred_reader_cannot_cross_registration_replacement() {
    let mut scene = Scene::new().await;
    scene.seed(scene.first.clone(), "old-cold", 40).await;
    scene.seed(scene.second.clone(), "other-hot", 40).await;
    let (reply, receive) = oneshot::channel();
    scene
        .actor
        .message_fanout_request(scene.first.agent_id.clone(), 11, 64, reply);
    let messages = scene.actor.service.message_service_mut();
    assert!(messages.retire_agent_identity(&scene.first.agent_id));
    messages
        .ensure_agent_identity(scene.first.clone(), 12)
        .unwrap();
    messages.subscribe(&scene.first.agent_id).unwrap();
    scene
        .seed(scene.first.clone(), "replacement-private", 8)
        .await;
    scene.actor.poll_message_custody();
    assert!(receive.await.unwrap().unwrap().is_none());
    let (reply, current) = oneshot::channel();
    scene
        .actor
        .message_fanout_request(scene.first.agent_id.clone(), 13, 64, reply);
    let current = current.await.unwrap().unwrap().unwrap();
    assert_eq!(
        current.batch.batch.messages[0].envelope.id,
        "replacement-private"
    );
}

/// Reply construction happens before another reader can evict newly loaded
/// data. A batch held in its response channel still counts against the cache;
/// after its real stored ACK/drop, a different recipient's deferred read proceeds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loaded_reply_is_pinned_before_another_recipient_loads() {
    let mut scene = Scene::new().await;
    scene.seed(scene.first.clone(), "first-cold", 40).await;
    scene
        .seed(scene.second.clone(), "second-cold-next", 40)
        .await;
    let (reply, first) = oneshot::channel();
    scene
        .actor
        .message_fanout_request(scene.first.agent_id.clone(), 11, 64, reply);
    scene.actor.poll_message_custody();
    scene.settle().await;
    let (reply, mut second) = oneshot::channel();
    scene
        .actor
        .message_fanout_request(scene.second.agent_id.clone(), 12, 64, reply);
    scene.actor.poll_message_custody();
    assert!(matches!(
        second.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    assert_eq!(
        scene.root.observe().await.unwrap(),
        CustodyWorkerStatus::Idle
    );
    let usage = scene.usage.snapshot().unwrap();
    assert_eq!(
        (usage.live_payload_bytes, usage.reserved_payload_bytes),
        (40, 0)
    );
    let first = first.await.unwrap().unwrap().unwrap();
    assert_eq!(first.batch.batch.messages[0].envelope.id, "first-cold");
    let (reply, acknowledged) = oneshot::channel();
    scene.actor.acknowledge_message_fanout(first.batch, reply);
    scene.settle().await;
    assert_eq!(acknowledged.await.unwrap().unwrap().last_sequence, 1);
    // The actual ACK publication supplies the capacity-change observation.
    scene.actor.poll_message_custody();
    scene.settle().await;
    let second = second.await.unwrap().unwrap().unwrap();
    assert_eq!(
        second.batch.batch.messages[0].envelope.id,
        "second-cold-next"
    );
    assert_eq!(scene.usage.snapshot().unwrap().live_payload_bytes, 40);
}

/// Bounded overload retains at most 64 payload-free continuations; the next
/// caller gets no batch and a bounded retry wake, without issuing a read. Once
/// queued observers leave, the same owner can immediately accept fresh demand.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reader_queue_refuses_overflow_and_releases_cancelled_descriptors() {
    let mut scene = Scene::new().await;
    scene.seed(scene.first.clone(), "cold", 40).await;
    scene.seed(scene.second.clone(), "hot", 40).await;
    let before = scene.usage.snapshot().unwrap();
    let mut receivers = Vec::new();
    for _ in 0..MESSAGE_PAYLOAD_DEMAND_RECIPIENTS {
        let (reply, receive) = oneshot::channel();
        scene
            .actor
            .message_fanout_request(scene.first.agent_id.clone(), 11, 64, reply);
        receivers.push(receive);
    }
    let (reply, overflow) = oneshot::channel();
    scene
        .actor
        .message_fanout_request(scene.first.agent_id.clone(), 11, 64, reply);
    assert!(overflow.await.unwrap().unwrap().is_none());
    assert_eq!(
        scene
            .actor
            .message_custody
            .as_ref()
            .unwrap()
            .payload_reads
            .len(),
        64
    );
    assert_eq!(scene.usage.snapshot().unwrap(), before);
    drop(receivers);
    // A real capacity wake clears the bounded overflow deadline.
    scene
        .actor
        .apply_message_custody_event(CustodyEvent::PayloadCapacityReady);
    scene.actor.poll_message_custody();
    assert!(
        scene
            .actor
            .message_custody
            .as_ref()
            .unwrap()
            .payload_reads
            .is_empty()
    );
    let (reply, _receive) = oneshot::channel();
    scene
        .actor
        .message_fanout_request(scene.first.agent_id.clone(), 11, 64, reply);
    assert_eq!(
        scene
            .actor
            .message_custody
            .as_ref()
            .unwrap()
            .payload_reads
            .len(),
        1
    );
}

/// Cancellation after physical issuance still joins/publishes the original
/// worker and returns its lane. It creates no delivery offer for an absent
/// observer; a later live reader obtains the same original sequence normally.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_issued_reader_joins_without_creating_an_offer() {
    let mut scene = Scene::new().await;
    scene.seed(scene.first.clone(), "cold", 40).await;
    scene.seed(scene.second.clone(), "hot", 40).await;
    let (reply, receive) = oneshot::channel();
    scene
        .actor
        .message_fanout_request(scene.first.agent_id.clone(), 11, 64, reply);
    scene.actor.poll_message_custody();
    let usage = scene.usage.snapshot().unwrap();
    assert_eq!(usage.live_payload_bytes + usage.reserved_payload_bytes, 40);
    drop(receive);
    scene.settle().await;
    assert_eq!(
        scene.root.observe().await.unwrap(),
        CustodyWorkerStatus::Idle
    );
    assert!(
        scene
            .actor
            .service
            .message_service_mut()
            .prepare_acknowledgement(&scene.first.agent_id, 1, 512 * 1024)
            .is_err()
    );
    let (reply, receive) = oneshot::channel();
    scene
        .actor
        .message_fanout_request(scene.first.agent_id.clone(), 12, 64, reply);
    assert_eq!(
        receive
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .batch
            .batch
            .messages[0]
            .envelope
            .id,
        "cold"
    );
}

/// The original wall-clock selection plus monotonic elapsed I/O time controls
/// TTL at completion. Advancing only the retained clock origin models a long
/// physical read without sleeping; qualified loaded bytes cannot renew expiry
/// or create an offer after the original acceptance-time deadline.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn payload_read_completion_does_not_renew_original_ttl() {
    let mut scene = Scene::new().await;
    scene
        .seed_with_ttl(scene.first.clone(), "expiring-cold", 40, Some(100))
        .await;
    scene.seed(scene.second.clone(), "hot", 40).await;
    let (reply, receive) = oneshot::channel();
    scene
        .actor
        .message_fanout_request(scene.first.agent_id.clone(), 11, 64, reply);
    scene.actor.poll_message_custody();
    let Some(Pending::PayloadLoad(read)) = scene
        .actor
        .message_custody
        .as_mut()
        .unwrap()
        .pending
        .as_mut()
    else {
        panic!("original actual load must be issued")
    };
    read.queued_at = Some(Instant::now() - std::time::Duration::from_secs(1));
    scene.settle().await;
    assert!(receive.await.unwrap().unwrap().is_none());
    assert_eq!(
        scene.root.observe().await.unwrap(),
        CustodyWorkerStatus::Idle
    );
    assert!(
        scene
            .actor
            .service
            .message_service_mut()
            .prepare_acknowledgement(&scene.first.agent_id, 1, 512 * 1024)
            .is_err()
    );
    assert_eq!(
        scene
            .actor
            .service
            .message_service()
            .subscription(&scene.first.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
}
