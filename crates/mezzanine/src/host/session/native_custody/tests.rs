//! Original-root/private-context restart, without a provider or effect replay.

use super::*;
use crate::host::shell::{ResolvedShell, ShellSource};
use crate::storage::mmp_custody::ownership::RootOwnership;
use mez_agent::messaging::{Envelope, MessageScope, Recipient, SenderIdentity};
use mez_agent::transcript::{TranscriptEntry, TranscriptRole};
use mez_core::ids::PaneId;
use std::time::Duration;

/// Generated private context/source files and the actual original capture.
struct Fixture {
    config: PathBuf,
    payload: SessionSnapshotPayload,
    epoch: [u8; 16],
    native: SenderIdentity,
    raw: SenderIdentity,
}
impl Fixture {
    async fn new(guarded: bool) -> Self {
        let config = std::env::temp_dir().join(format!(
            "mez-native-startup-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let session = Session::new_default(shell(), Size::new(80, 24).unwrap()).unwrap();
        let mut service = RuntimeSessionService::with_event_log(
            session,
            config.join("original.sock"),
            100,
            1024,
            4096,
        )
        .unwrap();
        service.set_config_root(config.clone());
        let transcripts = AgentTranscriptStore::under_config_root(&config);
        transcripts.initialize(100).unwrap();
        service.set_agent_transcript_store(transcripts.clone());
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        let context = service.agent_shell_store().get("%1").unwrap().clone();
        transcripts
            .append(&TranscriptEntry {
                conversation_id: context.session_id.clone(),
                sequence: 1,
                created_at_unix_seconds: 100,
                role: TranscriptRole::User,
                turn_id: "original-user".into(),
                agent_id: "agent-%1".into(),
                pane_id: "%1".into(),
                content: "original private conversation".into(),
            })
            .unwrap();
        service
            .agent_shell_store_mut()
            .record_transcript_entries("%1", 1)
            .unwrap();
        let native = service
            .ensure_runtime_message_identity(
                "agent-%1",
                PaneId::opaque("%1"),
                "agent",
                &["agent-harness"],
                100,
            )
            .unwrap();
        let raw = service
            .message_service_mut()
            .register_agent(None, None, "original raw", vec![]);
        service
            .message_service_mut()
            .subscribe(&native.agent_id)
            .unwrap();
        service.checkpoint_agent_session_metadata().unwrap();
        let root = MmpCustodyRoot::open(
            root_path(&config, service.session().id.as_str()),
            service.session().id.to_string(),
            Default::default(),
            tokio::runtime::Handle::current(),
        );
        assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
        root.configure_payload_spill(service.message_service_mut(), Default::default())
            .unwrap();
        if guarded {
            service
                .message_service_mut()
                .require_native_continuity(root.storage_scope())
                .unwrap();
        }
        service.refresh_native_message_bindings().unwrap();
        root.start_native_continuity(service.message_service(), &native.agent_id)
            .unwrap();
        assert_eq!(
            root.observe().await.unwrap(),
            CustodyWorkerStatus::NativeContinuityAwaitingAdoption
        );
        assert!(
            root.adopt_native_continuity(service.message_service_mut())
                .unwrap()
        );
        for n in 1..=2 {
            let ticket = service
                .message_service_mut()
                .prepare_acceptance(
                    &raw.agent_id,
                    mail(&raw, &native, n),
                    MessageScope::Session,
                    100,
                    4096,
                    None,
                )
                .unwrap();
            root.start(service.message_service_mut(), &ticket, None)
                .unwrap();
            assert_eq!(
                root.observe().await.unwrap(),
                CustodyWorkerStatus::AwaitingAdoption
            );
            root.adopt(service.message_service_mut(), None).unwrap();
            if n == 1 {
                assert_eq!(
                    service
                        .message_service()
                        .receive_subscribed(&native.agent_id, 101, 1)
                        .unwrap()
                        .messages
                        .len(),
                    1
                );
                let ack = service
                    .message_service_mut()
                    .prepare_acknowledgement(&native.agent_id, 1, 512 * 1024)
                    .unwrap()
                    .unwrap();
                root.start_acknowledgement(service.message_service_mut(), &ack)
                    .unwrap();
                assert_eq!(
                    root.observe().await.unwrap(),
                    CustodyWorkerStatus::AcknowledgementAwaitingAdoption
                );
                root.adopt_acknowledgement(service.message_service_mut())
                    .unwrap();
            }
            service
                .message_service_mut()
                .release_stored_payload(n, root.storage_scope())
                .unwrap();
        }
        root.start_checkpoint(service.message_service()).unwrap();
        assert_eq!(
            root.observe().await.unwrap(),
            CustodyWorkerStatus::SnapshotAwaitingAdoption
        );
        let (checkpoint, lease) = root.adopt_checkpoint().unwrap();
        let mut payload = SessionSnapshotPayload::from_session(service.session());
        payload.message_state = Some(checkpoint);
        let epoch = context.native_message_incarnation.snapshot_identity();
        let root_dir = root_path(&config, service.session().id.as_str());
        drop(lease);
        drop(service);
        drop(root);
        wait_unlocked(&root_dir).await;
        Self {
            config,
            payload,
            epoch,
            native,
            raw,
        }
    }
    fn runtime(&self) -> RuntimeSessionService {
        let mut runtime = RuntimeSessionService::with_event_log(
            restored(&self.payload),
            self.config.join("restore.sock"),
            102,
            1024,
            4096,
        )
        .unwrap();
        runtime.set_config_root(self.config.clone());
        runtime.set_agent_transcript_store(AgentTranscriptStore::under_config_root(&self.config));
        runtime
    }
    fn startup(&self) -> SessionRuntimeStartup {
        SessionRuntimeStartup::RestoredSnapshot {
            payload: Box::new(self.payload.clone()),
            restart_command: Some("cat >/dev/null".into()),
        }
    }
    fn request(&self) -> SessionFactoryRequest {
        SessionFactoryRequest {
            session: restored(&self.payload),
            owner_uid: rustix::process::geteuid().as_raw(),
            created_at_unix_seconds: 102,
            config: SessionRuntimeConfig {
                layers: Vec::new(),
                root: self.config.clone(),
            },
            sockets: SessionSocketPublication {
                control_path: self.config.join("restored.sock"),
                publish_control: true,
                message_path: Some(self.config.join("message.sock")),
                event_path: None,
                publish_registry: false,
            },
            limits: Default::default(),
            startup: self.startup(),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.config);
    }
}
fn shell() -> ResolvedShell {
    ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh)
}
fn restored(payload: &SessionSnapshotPayload) -> Session {
    Session::from_restore_input(
        shell(),
        crate::storage::snapshot::session_restore_input(payload).unwrap(),
    )
    .unwrap()
}
fn mail(raw: &SenderIdentity, native: &SenderIdentity, n: u64) -> Envelope {
    Envelope {
        protocol: "mmp/1",
        id: format!("original-{n}"),
        message_type: "send".into(),
        time: "original".into(),
        sender: raw.clone(),
        recipient: Recipient::Agent(native.agent_id.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain".into(),
        payload: format!("original body {n}"),
        extension_fields: Vec::new(),
    }
}
async fn wait_unlocked(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(owner) = RootOwnership::acquire_existing(path) {
                drop(owner);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

/// Private context plus guarded original publication restores the exact native
/// namespace/epoch/floor, keeps raw identities absent, preserves an ACKed first
/// slot and reloads only the second original. No application dedup is claimed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_startup_restores_private_binding_and_only_unacked_custody() {
    let fixture = Fixture::new(true).await;
    let mut runtime = fixture.runtime();
    let prepared = prepare(&mut runtime, fixture.startup(), &fixture.config)
        .await
        .unwrap();
    assert_eq!(
        runtime
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .native_message_incarnation
            .snapshot_identity(),
        fixture.epoch
    );
    assert!(
        runtime
            .message_service()
            .native_continuity_is_recorded(&fixture.native.agent_id)
    );
    assert!(
        runtime
            .message_service()
            .registered_identity(&fixture.raw.agent_id)
            .is_none()
    );
    assert_eq!(runtime.message_service().next_message_sequence(), 3);
    assert!(
        prepared
            .root
            .start_payload_load(runtime.message_service_mut(), 2)
            .unwrap()
    );
    assert_eq!(
        prepared.root.observe().await.unwrap(),
        CustodyWorkerStatus::PayloadLoadAwaitingAdoption
    );
    assert!(
        prepared
            .root
            .publish_payload_load(runtime.message_service_mut())
            .unwrap()
    );
    let batch = runtime
        .message_service()
        .receive_subscribed(&fixture.native.agent_id, 103, 10)
        .unwrap();
    assert_eq!(batch.messages.len(), 1);
    assert_eq!(batch.messages[0].sequence, 2);
    assert_eq!(batch.messages[0].envelope.payload, "original body 2");
}

/// Unguarded observations stay data even with matching labels and saved epoch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_startup_keeps_unguarded_observation_unavailable() {
    let fixture = Fixture::new(false).await;
    let mut runtime = fixture.runtime();
    let _prepared = prepare(&mut runtime, fixture.startup(), &fixture.config)
        .await
        .unwrap();
    assert!(
        runtime
            .message_service()
            .registered_identity(&fixture.native.agent_id)
            .is_none()
    );
    assert_ne!(
        runtime
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .native_message_incarnation
            .snapshot_identity(),
        fixture.epoch
    );
    assert_eq!(runtime.message_service().next_message_sequence(), 3);
}

/// Generic startup still cannot revive the saved cold snapshot or native epoch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generic_startup_refuses_cold_native_snapshot() {
    let fixture = Fixture::new(true).await;
    assert!(start_session(&mut fixture.runtime(), fixture.startup()).is_err());
}

/// Copying the database cannot substitute for its original physical allocation.
/// Rejection precedes sockets/processes and leaves private active metadata intact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_factory_refuses_replaced_database_before_socket_publication() {
    let fixture = Fixture::new(true).await;
    let transcripts = AgentTranscriptStore::under_config_root(&fixture.config);
    let (original, _) = transcripts
        .capture_native_session_metadata(fixture.payload.session_id.as_str())
        .unwrap();
    let path =
        root_path(&fixture.config, fixture.payload.session_id.as_str()).join("custody.sqlite");
    fs::rename(&path, path.with_extension("retained")).unwrap();
    fs::copy(path.with_extension("retained"), &path).unwrap();
    let request = fixture.request();
    let control = request.sockets.control_path.clone();
    let message = request.sockets.message_path.clone().unwrap();
    assert!(
        SessionFactory::create_with_native_custody(
            request,
            PaneProcessRetirementOwner::default(),
            None
        )
        .await
        .is_err()
    );
    assert!(!control.exists() && !message.exists());
    original.revalidate().unwrap();
}

/// Equal public/private data in a second provisional runtime cannot consume the
/// first runtime's actual qualified epochs. The correct adopter remains usable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_result_requires_its_original_provisional_runtime() {
    let fixture = Fixture::new(true).await;
    let mut first = fixture.runtime();
    let context = first.prepare_native_custody_context().unwrap();
    let mut other = fixture.runtime();
    let _other_context = other.prepare_native_custody_context().unwrap();
    let root = MmpCustodyRoot::recover_native(
        root_path(&fixture.config, fixture.payload.session_id.as_str()),
        fixture.payload.session_id.clone(),
        Default::default(),
        tokio::runtime::Handle::current(),
        CustodyRecoveryRequest {
            checkpoint: fixture.payload.message_state.clone().unwrap(),
            limits: Default::default(),
            retention_messages: 1000,
            retention_bytes: 1024 * 1024,
        },
        context,
    );
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::RecoveryAwaitingAdoption
    );
    let result = root.adopt_recovery().unwrap();
    assert!(result.install_native_context(&mut other).is_err());
    result.install_native_context(&mut first).unwrap();
    assert_eq!(
        first
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .native_message_incarnation
            .snapshot_identity(),
        fixture.epoch
    );
}

/// The real factory installs and registers original custody before returning its
/// actor. A genuine actor-owned payload reload serves the unACKed original; no
/// provider service is run. External native ownership retains process cleanup.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_factory_registers_original_root_before_ready_actor() {
    let fixture = Fixture::new(true).await;
    let native = PaneProcessRetirementOwner::default();
    let mut runtime =
        SessionFactory::create_with_native_custody(fixture.request(), native.clone(), None)
            .await
            .unwrap();
    let handle = runtime.handle().actor().clone();
    let actor = runtime.actor.take().unwrap();
    let runner = tokio::spawn(actor.run());
    let body = handle
        .message_fanout_ready_for(fixture.native.agent_id.clone(), 103, 10)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(body.batch.batch.messages[0].sequence, 2);
    handle.shutdown().await.unwrap();
    let mut exited = runner.await.unwrap();
    assert_eq!(
        exited
            .service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .native_message_incarnation
            .snapshot_identity(),
        fixture.epoch
    );
    exited.service.terminate_all_pane_processes().unwrap();
    drop(runtime);
}
