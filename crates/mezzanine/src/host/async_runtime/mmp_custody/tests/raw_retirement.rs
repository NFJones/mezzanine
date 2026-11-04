//! Actual raw source/end/terminal ownership, SQL faults and socket shutdown.

use super::*;
use mez_agent::messaging::{DeliveryTerminalReason, MessageAcceptanceDecodeLimits};

/// One actual guarded raw original; endpoint end is still a separate transition.
struct Scene {
    root: MmpCustodyRoot,
    service: MessageService,
    target: SenderIdentity,
}
impl Scene {
    async fn new(fixture: &Fixture, probe: Option<Arc<Probe>>) -> Self {
        let mut root = fixture.root();
        assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
        Arc::get_mut(&mut root.0).unwrap().terminal_probe = probe;
        let (mut service, sender, target, _) = fixture_service();
        root.configure_payload_spill(&mut service, Default::default())
            .unwrap();
        service
            .require_native_continuity(root.storage_scope())
            .unwrap();
        let mut original = envelope(&sender, &target, "actual-raw-original");
        original.ttl_ms = None;
        let ticket = service
            .prepare_acceptance(
                &sender.agent_id,
                original,
                MessageScope::Session,
                100,
                4096,
                None,
            )
            .unwrap();
        root.start(&mut service, &ticket, None).unwrap();
        assert_eq!(
            root.observe().await.unwrap(),
            CustodyWorkerStatus::AwaitingAdoption
        );
        root.adopt(&mut service, None).unwrap();
        Self {
            root,
            service,
            target,
        }
    }
    /// Persists only the actual original namespace, before any terminal I/O.
    async fn end(&mut self) {
        self.service.retire_agent_identity(&self.target.agent_id);
        assert!(
            self.service
                .prepare_storage_raw_retirement(self.root.storage_scope(), 150, usize::MAX)
                .unwrap()
                .is_none()
        );
        assert!(self.root.start_registration_end(&self.service).unwrap());
        assert_eq!(
            self.root.observe().await.unwrap(),
            CustodyWorkerStatus::RegistrationEndAwaitingAdoption
        );
        self.root.adopt_registration_end(&mut self.service).unwrap();
    }
}
/// Narrow actual decoder quantum shared by root and cold forensic controls.
fn limits() -> MessageAcceptanceDecodeLimits {
    MessageAcceptanceDecodeLimits {
        payload_bytes: 1024 * 1024,
        auxiliary_bytes: 512 * 1024,
        recipient_count: 4096,
        recipient_bytes: 1024 * 1024,
    }
}
/// Counts bounded private journals; no result supplies a positive capability.
fn count(fixture: &Fixture, prefix: &str) -> i64 {
    fixture
        .connection()
        .query_row(
            "SELECT count(*) FROM records WHERE key LIKE ?1",
            [format!("{prefix}:%")],
            |r| r.get(0),
        )
        .unwrap()
}

/// Releases the actual blocking lane even if a socket control assertion fails.
struct ReleaseGate(Arc<Probe>);
impl Drop for ReleaseGate {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// Uses genuine socket-owned hello registrations and framed raw acceptance.
/// The target has never committed or ACKed its message when this returns.
async fn raw_actor_scene(
    fixture: &Fixture,
) -> (
    crate::host::async_runtime::AsyncRuntimeSessionHandle,
    tokio::task::JoinHandle<crate::host::async_runtime::AsyncRuntimeActorExit>,
    mez_core::ids::AgentId,
) {
    use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
    let root = fixture.root();
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    let service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    let (handle, mut actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    actor.register_message_custody(root).unwrap();
    actor
        .enable_message_payload_spill(Default::default())
        .unwrap();
    let runner = tokio::spawn(actor.run());
    let (target, _target_owner, _target_lease) = raw_actor::hello(&handle).await;
    let target = target.agent_id.unwrap();
    let (sender, owner, _lease) = raw_actor::hello(&handle).await;
    let frame = crate::protocol::message::encode_mmp_body(&serde_json::json!({
        "protocol":"mmp/1", "type":"send", "id":"actual-raw-lease-end", "time":"original", "scope":"session",
        "sender":{"agent_id":sender.agent_id.as_ref().unwrap().as_str()}, "recipient":{"agent_id":target.as_str()},
        "ttl_ms":null,"correlation_id":null,"content_type":"text/plain; charset=utf-8","payload":"uncommitted raw original"
    }).to_string());
    let result = handle
        .handle_message_input_with_owner(frame, 4096, sender, 100, Some(owner))
        .await
        .unwrap();
    let response = raw_actor::body(&result.output);
    assert_eq!(
        response["type"], "ack",
        "actual socket response: {response}"
    );
    // Consume the real acceptance probe's stored notification before reclosing.
    fixture.probe.started.notified().await;
    (handle, runner, target)
}

/// Actual socket cleanup journals the end before raw d1 can use the common
/// lane. A blocked end keeps control healthy; release causes exactly one raw
/// terminal observation, with no recipient commit or replacement socket owner.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_retirement_actor_waits_for_real_socket_end_before_terminal_write() {
    let fixture = Fixture::new(true);
    let (handle, runner, target) = raw_actor_scene(&fixture).await;
    let _gate = ReleaseGate(fixture.probe.clone());
    *fixture.probe.gate.0.lock().unwrap() = false;
    handle
        .retire_message_connection(target.clone())
        .await
        .unwrap();
    fixture.probe.started.notified().await;
    assert_eq!(count(&fixture, "d1"), 0);
    assert_eq!(count(&fixture, "o1"), 0);
    assert!(handle.lifecycle_state().await.is_ok());
    fixture.probe.release();
    tokio::time::timeout(Duration::from_secs(5), async {
        while count(&fixture, "d1") == 0 {
            handle.lifecycle_state().await.unwrap();
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    handle.shutdown().await.unwrap();
    let exited = runner.await.unwrap();
    assert!(
        exited
            .service
            .message_service()
            .registered_identity(&target)
            .is_none()
    );
    assert_eq!(count(&fixture, "d1"), 1);
    assert_eq!(count(&fixture, "o1"), 1);
    assert_eq!(count(&fixture, "k1"), 0);
    assert_eq!(
        exited
            .service
            .message_service()
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        1
    );
}

/// Stop retires its actual raw target only after original work has settled.
/// The resulting o1 must really join/adopt before the shutdown reply completes;
/// fresh d1 stays withheld, leaving the accepted original for cold recovery.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_retirement_shutdown_drains_new_end_before_reply_and_root_release() {
    let fixture = Fixture::new(true);
    let (handle, runner, _) = raw_actor_scene(&fixture).await;
    let _gate = ReleaseGate(fixture.probe.clone());
    *fixture.probe.gate.0.lock().unwrap() = false;
    let shutdown = tokio::spawn(async move { handle.shutdown().await });
    fixture.probe.started.notified().await;
    assert!(!shutdown.is_finished());
    assert!(!runner.is_finished());
    assert!(RootOwnership::acquire_existing(&fixture.path).is_err());
    fixture.probe.release();
    shutdown.await.unwrap().unwrap();
    let exited = runner.await.unwrap();
    assert_eq!(count(&fixture, "o1"), 1);
    assert_eq!(count(&fixture, "d1"), 0);
    assert!(
        !exited
            .service
            .message_service()
            .has_pending_registration_end()
            .unwrap()
    );
    assert_eq!(
        exited
            .service
            .message_service()
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        1
    );
}

/// Real end adoption allows a raw d1 without b1 or native input. A lost positive
/// retains first bytes through actual join/Stop; cold fold preserves retirement
/// while the accepted source remains pending and no recipient ACK is invented.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_retirement_root_reconciles_original_after_lost_positive_and_stop() {
    let fixture = Fixture::new(true);
    let probe = Probe::new(true);
    let mut scene = Scene::new(&fixture, Some(probe.clone())).await;
    scene.end().await;
    assert_eq!(count(&fixture, "b1"), 0);
    probe.panic_after_commit.store(true, Ordering::SeqCst);
    let ticket = scene
        .service
        .prepare_storage_raw_retirement(scene.root.storage_scope(), 90, usize::MAX)
        .unwrap()
        .unwrap();
    scene
        .root
        .start_terminal(&mut scene.service, &ticket)
        .unwrap();
    let CustodyWorkerStatus::TerminalRetryAt(due) = scene.root.observe().await.unwrap() else {
        panic!("actual joined positive loss required");
    };
    let disk = fixture.disk();
    assert_eq!(count(&fixture, "d1"), 1);
    scene.root.request_stop();
    assert!(scene.root.retry_terminal(&mut scene.service, due).unwrap());
    assert_eq!(
        scene.root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalAwaitingAdoption
    );
    assert_eq!(scene.root.adopt_terminal(&mut scene.service).unwrap(), 1);
    assert_eq!(fixture.disk(), disk);
    let bytes: Vec<u8> = fixture
        .connection()
        .query_row("SELECT value FROM records WHERE key LIKE 'd1:%'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(&bytes[..8], b"MMPTDR02");
    assert_eq!(bytes.len(), 168);
    let store =
        MmpCustodyStore::open_existing(&fixture.path, "original-session", Default::default())
            .unwrap();
    let history = store.fold_custody_history(limits()).unwrap();
    assert_eq!(
        history.terminal_observations[0].reason,
        DeliveryTerminalReason::RecipientRetired
    );
    assert_eq!(history.terminal_observations[0].observed_at_ms, 100);
    assert_eq!(
        history.terminal_observations[0].release_observed_at_ms,
        None
    );
    assert!(history.acknowledgements.is_empty());
    assert_eq!(
        scene
            .service
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        1
    );
}

/// A SQL trigger attempts to erase the original end while writing raw d1.
/// Revalidation must roll back the whole transaction, and removing the fault
/// permits only the same original plan to retry before publishing its outcome.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_retirement_root_rolls_back_end_damage_before_original_retry() {
    let fixture = Fixture::new(true);
    let mut scene = Scene::new(&fixture, None).await;
    scene.end().await;
    let before = fixture.disk();
    fixture.connection().execute_batch("CREATE TRIGGER damage_raw_retirement AFTER INSERT ON records WHEN NEW.key LIKE 'd1:%' BEGIN DELETE FROM records WHERE key LIKE 'o1:%'; END;").unwrap();
    let ticket = scene
        .service
        .prepare_storage_raw_retirement(scene.root.storage_scope(), 150, usize::MAX)
        .unwrap()
        .unwrap();
    scene
        .root
        .start_terminal(&mut scene.service, &ticket)
        .unwrap();
    let CustodyWorkerStatus::TerminalRetryAt(due) = scene.root.observe().await.unwrap() else {
        panic!("actual rollback required");
    };
    assert_eq!(fixture.disk(), before);
    assert_eq!(count(&fixture, "d1"), 0);
    fixture
        .connection()
        .execute_batch("DROP TRIGGER damage_raw_retirement")
        .unwrap();
    assert!(scene.root.retry_terminal(&mut scene.service, due).unwrap());
    assert_eq!(
        scene.root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalAwaitingAdoption
    );
    assert_eq!(scene.root.adopt_terminal(&mut scene.service).unwrap(), 1);
}
