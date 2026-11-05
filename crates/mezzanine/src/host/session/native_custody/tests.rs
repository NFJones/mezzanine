//! Original-root/private-context restart, without a provider or effect replay.

use super::*;
use crate::host::shell::{ResolvedShell, ShellSource};
use crate::storage::mmp_custody::ownership::RootOwnership;
use mez_agent::messaging::{Envelope, MessageScope, Recipient, SenderIdentity};
use mez_agent::transcript::{TranscriptEntry, TranscriptRole};
use mez_core::ids::{AgentId, PaneId};
use std::time::Duration;

/// Generated private context/source files and the actual original capture.
struct Fixture {
    config: PathBuf,
    payload: SessionSnapshotPayload,
    epoch: [u8; 16],
    native: AgentId,
    raw: AgentId,
}
/// Whether this actual original captures an optional message snapshot or exits
/// abruptly after durable journal/acceptance/ACK, without root/service Drop.
enum Original {
    Snapshot,
    /// Old optional checkpoint precedes an actual journaled native namespace end.
    EndedSnapshot,
    /// Exact later-slot native ACK; optionally replace the actual private epoch.
    NativeAckSnapshot {
        replace_incarnation: bool,
    },
    Registered,
    CrashRegistered(PathBuf),
    CrashNativeInput(PathBuf),
    CrashNativePreparation(PathBuf),
    CrashNativeStage(PathBuf, bool),
    /// Actual t1 or r1 commit before lower adoption and abrupt process exit.
    CrashTerminal(PathBuf, bool),
    /// Actual ended native slot d1/r1 before adoption and abrupt process exit.
    CrashRetirement(PathBuf, bool),
    /// Actual raw source loss/end/disposition at a child-only physical boundary.
    CrashRawRecipient(PathBuf, raw_retirement::CrashPoint),
}
impl Fixture {
    async fn new(guarded: bool) -> Self {
        Self::build(guarded, Original::Snapshot).await
    }
    async fn build(guarded: bool, original: Original) -> Self {
        let generated = std::env::temp_dir().join(format!(
            "mez-native-startup-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let config = match &original {
            Original::CrashRegistered(config)
            | Original::CrashNativeInput(config)
            | Original::CrashNativePreparation(config)
            | Original::CrashNativeStage(config, _)
            | Original::CrashTerminal(config, _)
            | Original::CrashRetirement(config, _)
            | Original::CrashRawRecipient(config, _) => config.clone(),
            _ => generated,
        };
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
        if matches!(
            &original,
            Original::CrashNativeInput(_)
                | Original::CrashNativePreparation(_)
                | Original::CrashNativeStage(_, _)
                | Original::CrashTerminal(_, _)
                | Original::CrashRetirement(_, _)
                | Original::CrashRawRecipient(_, _)
        ) {
            native_input::configure_original(&mut service);
        }
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
        let native = if matches!(
            &original,
            Original::CrashNativeInput(_)
                | Original::CrashNativePreparation(_)
                | Original::CrashNativeStage(_, _)
                | Original::CrashTerminal(_, _)
                | Original::CrashRetirement(_, _)
                | Original::CrashRawRecipient(_, _)
        ) {
            service
                .message_service()
                .registered_identity(&AgentId::opaque("agent-%1").unwrap())
                .unwrap()
                .clone()
        } else {
            service
                .ensure_runtime_message_identity(
                    "agent-%1",
                    PaneId::opaque("%1"),
                    "agent",
                    &["agent-harness"],
                    100,
                )
                .unwrap()
        };
        let raw = service
            .message_service_mut()
            .register_agent(None, None, "original raw", vec![]);
        service
            .message_service_mut()
            .subscribe(&native.agent_id)
            .unwrap();
        service.checkpoint_agent_session_metadata().unwrap();
        let controlled_clock = matches!(
            &original,
            Original::CrashTerminal(_, true)
                | Original::CrashRetirement(_, true)
                | Original::CrashRawRecipient(_, raw_retirement::CrashPoint::Release)
        );
        let root = if matches!(
            &original,
            Original::Snapshot | Original::EndedSnapshot | Original::NativeAckSnapshot { .. }
        ) {
            MmpCustodyRoot::open(
                root_path(&config, service.session().id.as_str()),
                service.session().id.to_string(),
                Default::default(),
                tokio::runtime::Handle::current(),
            )
        } else {
            MmpCustodyRoot::open_registered(
                root_path(&config, service.session().id.as_str()),
                service.session().id.to_string(),
                Default::default(),
                tokio::runtime::Handle::current(),
                if controlled_clock {
                    crate::storage::mmp_custody::elapsed_clock::ClockSource::controlled_for_tests(
                        99,
                    )
                } else {
                    Default::default()
                },
            )
        };
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
        let terminal_clock = match &original {
            Original::CrashRawRecipient(_, raw_retirement::CrashPoint::Release) => Some(
                crate::runtime::current_unix_millis()
                    - mez_agent::messaging::MESSAGE_TERMINAL_HANDOFF_MS,
            ),
            Original::CrashTerminal(_, release) | Original::CrashRetirement(_, release) => Some(
                crate::runtime::current_unix_millis()
                    - if *release {
                        mez_agent::messaging::MESSAGE_TERMINAL_HANDOFF_MS
                    } else {
                        0
                    },
            ),
            _ => None,
        };
        for n in 1..=2 {
            let mut original_mail = mail(&raw, &native, n);
            if n == 2 && matches!(&original, Original::CrashRawRecipient(_, _)) {
                original_mail.recipient = Recipient::Agent(raw.agent_id.clone());
            }

            let accepted_at = if let (2, Some(terminal_clock)) = (n, terminal_clock) {
                if matches!(&original, Original::CrashTerminal(_, _)) {
                    original_mail.ttl_ms = Some(1);
                }
                terminal_clock - 2
            } else if n == 2
                && matches!(&original, Original::CrashNativePreparation(_))
                && std::env::var_os(native_input::EXPIRE_PREPARATION).is_some()
            {
                let now = crate::runtime::current_unix_millis();
                original_mail.ttl_ms = Some(3000);
                fs::write(
                    config.join("expiry-deadline.json"),
                    serde_json::to_vec(&now.saturating_add(3000)).unwrap(),
                )
                .unwrap();
                now
            } else {
                100
            };
            if controlled_clock {
                root.advance_elapsed_clock_for_tests(accepted_at).unwrap();
            }
            let ticket = service
                .message_service_mut()
                .prepare_acceptance(
                    &raw.agent_id,
                    original_mail,
                    MessageScope::Session,
                    accepted_at,
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
            if n == 1 && !matches!(&original, Original::NativeAckSnapshot { .. }) {
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
        if let Original::NativeAckSnapshot {
            replace_incarnation,
        } = &original
        {
            native_ack_snapshot::commit_later_original(&mut service, &root, *replace_incarnation)
                .await;
        }
        let mut payload = SessionSnapshotPayload::from_session(service.session());
        if matches!(
            &original,
            Original::Snapshot | Original::EndedSnapshot | Original::NativeAckSnapshot { .. }
        ) {
            root.start_checkpoint(service.message_service()).unwrap();
            assert_eq!(
                root.observe().await.unwrap(),
                CustodyWorkerStatus::SnapshotAwaitingAdoption
            );
            let (checkpoint, lease) = root.adopt_checkpoint().unwrap();
            payload.message_state = Some(checkpoint);
            drop(lease);
        }
        if matches!(&original, Original::EndedSnapshot) {
            assert!(
                service
                    .message_service_mut()
                    .retire_agent_identity(&native.agent_id)
            );
            assert!(
                root.start_registration_end(service.message_service())
                    .unwrap()
            );
            assert_eq!(
                root.observe().await.unwrap(),
                CustodyWorkerStatus::RegistrationEndAwaitingAdoption
            );
            assert!(
                root.adopt_registration_end(service.message_service_mut())
                    .unwrap()
            );
        }
        let epoch = service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .native_message_incarnation
            .snapshot_identity();
        let root_dir = root_path(&config, service.session().id.as_str());
        let fixture = Self {
            config,
            payload,
            epoch,
            native: native.agent_id,
            raw: raw.agent_id,
        };
        if let Original::CrashTerminal(_, release) = &original {
            terminal::commit_then_crash(
                service,
                root,
                fixture,
                terminal_clock.unwrap(),
                *release,
                false,
            )
            .await;
        }
        if let Original::CrashRawRecipient(_, point) = &original {
            raw_retirement::commit_then_crash(service, root, fixture, *point, terminal_clock).await;
        }
        if let Original::CrashRetirement(_, release) = &original {
            terminal::commit_then_crash(
                service,
                root,
                fixture,
                terminal_clock.unwrap(),
                *release,
                true,
            )
            .await;
        }
        if matches!(
            &original,
            Original::CrashNativeInput(_)
                | Original::CrashNativePreparation(_)
                | Original::CrashNativeStage(_, _)
        ) {
            let point = match &original {
                Original::CrashNativePreparation(_) => native_input::NativeCrashPoint::Prepared,
                Original::CrashNativeStage(_, true) => native_input::NativeCrashPoint::PartialStage,
                Original::CrashNativeStage(_, false) => {
                    native_input::NativeCrashPoint::CompleteStage
                }
                _ => native_input::NativeCrashPoint::Committed,
            };
            native_input::commit_then_crash(service, root, fixture, point).await;
        }
        if matches!(&original, Original::CrashRegistered(_)) {
            finish_crash(&fixture);
        }
        drop(service);
        drop(root);
        wait_unlocked(&root_dir).await;
        fixture
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
/// Waits for and retains one actual kernel fence. Returning that same owner
/// avoids a release/reacquire gap in parallel subprocess tests: fork may briefly
/// inherit a CLOEXEC descriptor until exec, even after the original closes it.
async fn wait_for_ownership(path: &Path) -> RootOwnership {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(owner) = RootOwnership::acquire_existing(path) {
                return owner;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

/// Waits for destructor/worker retirement when no subsequent read needs a fence.
async fn wait_unlocked(path: &Path) {
    drop(wait_for_ownership(path).await);
}

/// Private context plus guarded original publication restores the exact native
/// namespace/epoch/floor, keeps raw identities absent, preserves an ACKed first
/// slot and reloads only the second original. No application dedup is claimed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_startup_restores_private_binding_and_only_unacked_custody() {
    let fixture = Fixture::new(true).await;
    let mut runtime = fixture.runtime();
    let prepared = prepare(
        &mut runtime,
        fixture.startup(),
        &fixture.config,
        Default::default(),
    )
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
            .native_continuity_is_recorded(&fixture.native)
    );
    assert!(
        runtime
            .message_service()
            .registered_identity(&fixture.raw)
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
        .receive_subscribed(&fixture.native, 103, 10)
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
    let _prepared = prepare(
        &mut runtime,
        fixture.startup(),
        &fixture.config,
        Default::default(),
    )
    .await
    .unwrap();
    assert!(
        runtime
            .message_service()
            .registered_identity(&fixture.native)
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
    for original in [Original::Snapshot, Original::Registered] {
        let fixture = Fixture::build(true, original).await;
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
        assert_originals_retained(&fixture).await;
    }
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
            checkpoint: fixture.payload.message_state.clone(),
            limits: Default::default(),
            retention_messages: 1000,
            retention_bytes: 1024 * 1024,
        },
        context,
        Default::default(),
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
/// actor. Its owned cold reload, canonical receive and exact ACK serve the
/// unACKed original; no provider service is run. External native ownership retains process cleanup.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_factory_registers_original_root_before_ready_actor() {
    let fixture = Fixture::new(true).await;
    assert_factory_redelivery(&fixture).await;
}

/// Exercises restored native delivery through the actual factory, actor and
/// canonical ACK path. A local-only configured harness admits its one new idle
/// receive turn; no provider service dispatches its offer. Reading native mail
/// through the transport test API would race this now-connected receive owner.
async fn assert_factory_redelivery(fixture: &Fixture) {
    let native = PaneProcessRetirementOwner::default();
    let mut request = fixture.request();
    request.config.layers = vec![native_input::config()];
    let mut runtime = SessionFactory::create_with_native_custody(request, native.clone(), None)
        .await
        .unwrap();
    let handle = runtime.handle().actor().clone();
    let actor = runtime.actor.take().unwrap();
    let runner = tokio::spawn(actor.run());
    let database = root_path(&fixture.config, &fixture.payload.session_id).join("custody.sqlite");
    let settled = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            handle.lifecycle_state().await.unwrap();
            let db = rusqlite::Connection::open(&database).unwrap();
            if db
                .query_row(
                    "SELECT COUNT(*) FROM records WHERE key LIKE 'k1:%'",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap()
                == 2
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    handle.shutdown().await.unwrap();
    let mut exited = runner.await.unwrap();
    if settled.is_err() {
        exited.service.terminate_all_pane_processes().unwrap();
        panic!("native original did not settle through the actual canonical ACK path");
    }
    assert!(
        exited
            .service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .project_scope
            .is_none(),
        "receiving in a restored CWD must preserve original unscoped native membership"
    );
    assert!(
        exited
            .service
            .message_service()
            .registered_identity(&fixture.native)
            .unwrap()
            .project_scope
            .is_none()
    );
    assert_eq!(exited.service.message_service().next_message_sequence(), 3);
    assert_eq!(
        exited
            .service
            .message_service()
            .subscription(&fixture.native)
            .unwrap()
            .last_sequence,
        2
    );
    assert_eq!(
        exited
            .service
            .message_service()
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        0
    );
    assert!(
        exited
            .service
            .message_service()
            .registered_identity(&fixture.raw)
            .is_none()
    );
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
    let received = exited
        .service
        .agent_turn_ledger()
        .turns()
        .iter()
        .filter(|t| t.trigger == mez_agent::AgentTurnTrigger::LocalMessage)
        .collect::<Vec<_>>();
    assert_eq!(received.len(), 1);
    assert_eq!(
        exited
            .service
            .agent_turn_contexts()
            .get(&received[0].turn_id)
            .unwrap()
            .blocks()
            .iter()
            .filter(|b| b.source == mez_agent::ContextSourceKind::PeerMessage
                && b.content.contains("original body 2"))
            .count(),
        1
    );
    let rows = exited
        .service
        .agent_transcript_store()
        .unwrap()
        .inspect(&received[0].conversation_id)
        .unwrap();
    assert_eq!(
        rows.iter()
            .filter(|r| matches!(
                mez_agent::transcript::TranscriptContextEvent::from_transcript_content(&r.content),
                Some(mez_agent::transcript::TranscriptContextEvent::NativePeerMessage { .. })
            ))
            .count(),
        1
    );
    exited.service.terminate_all_pane_processes().unwrap();
    drop(runtime);
}

/// Actual registration replaces the optional message checkpoint as the physical
/// original anchor. The real factory still folds the durable first ACK and
/// reloads only the unACKed second original through its registered actor/root.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_factory_recovers_registered_original_without_message_checkpoint() {
    let fixture = Fixture::build(true, Original::Registered).await;
    assert!(fixture.payload.message_state.is_none());
    assert_factory_redelivery(&fixture).await;
}

/// Absence of both optional snapshot and original publication cannot authorize
/// an empty replacement. Refusal precedes sockets and preserves original mail.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registered_native_recovery_refuses_missing_original_publication() {
    let fixture = Fixture::build(true, Original::Registered).await;
    let path = root_path(&fixture.config, &fixture.payload.session_id).join("registration.v1");
    fs::remove_file(&path).unwrap();
    let request = fixture.request();
    let control = request.sockets.control_path.clone();
    assert!(
        SessionFactory::create_with_native_custody(
            request,
            PaneProcessRetirementOwner::default(),
            None
        )
        .await
        .is_err()
    );
    assert!(!path.exists() && !control.exists());
    assert_originals_retained(&fixture).await;
}

/// A failed startup must retain the actual immutable originals, not merely an
/// existing database pathname. This reads both real committed bodies under an
/// actual fresh kernel fence after the refused worker has retired.
async fn assert_originals_retained(fixture: &Fixture) {
    use crate::storage::mmp_custody::MmpCustodyStore;
    use mez_agent::messaging::MessageAcceptanceDecodeLimits;
    let root = root_path(&fixture.config, &fixture.payload.session_id);
    let owner = wait_for_ownership(&root).await;
    let mut store =
        MmpCustodyStore::open_existing(&root, &fixture.payload.session_id, Default::default())
            .unwrap();
    let mut bound = owner.bind_store(&mut store).unwrap();
    for sequence in 1..=2 {
        let (source, _, _) = bound
            .store_mut()
            .read_accepted_source(
                sequence,
                MessageAcceptanceDecodeLimits {
                    payload_bytes: 1024 * 1024,
                    auxiliary_bytes: 512 * 1024,
                    recipient_count: 4096,
                    recipient_bytes: 1024 * 1024,
                },
            )
            .unwrap();
        assert_eq!(source.envelope().id, format!("original-{sequence}"));
        assert_eq!(
            source.envelope().payload,
            format!("original body {sequence}")
        );
    }
}

/// Generic construction must not silently reset native epochs when layout data
/// lacks the optional message checkpoint but original custody exists on disk.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generic_factory_refuses_existing_registered_custody_before_publication() {
    let fixture = Fixture::build(true, Original::Registered).await;
    let store = AgentTranscriptStore::under_config_root(&fixture.config);
    let (original, _) = store
        .capture_native_session_metadata(&fixture.payload.session_id)
        .unwrap();
    for missing_registration in [false, true] {
        if missing_registration {
            fs::remove_file(
                root_path(&fixture.config, &fixture.payload.session_id).join("registration.v1"),
            )
            .unwrap();
        }
        let request = fixture.request();
        let control = request.sockets.control_path.clone();
        assert!(
            SessionFactory::create_with_native_owner(
                request,
                PaneProcessRetirementOwner::default(),
                None
            )
            .await
            .is_err()
        );
        assert!(!control.exists());
        original.revalidate().unwrap();
    }
}

/// Fresh factory setup publishes original allocation and the empty private
/// context before returning a ready actor. No public option activates this path.
/// Owned shutdown still drains root and pane processes normally.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fresh_native_factory_publishes_registered_original_before_ready_actor() {
    let directory = CrashDirectory::new();
    let session = Session::new_default(shell(), Size::new(80, 24).unwrap()).unwrap();
    let session_id = session.id.to_string();
    let request = SessionFactoryRequest {
        session,
        owner_uid: rustix::process::geteuid().as_raw(),
        created_at_unix_seconds: 100,
        config: SessionRuntimeConfig {
            layers: Vec::new(),
            root: directory.0.clone(),
        },
        sockets: SessionSocketPublication {
            control_path: directory.0.join("fresh.sock"),
            publish_control: true,
            message_path: None,
            event_path: None,
            publish_registry: false,
        },
        limits: Default::default(),
        startup: SessionRuntimeStartup::Initial {
            explicit_command: Some("cat >/dev/null".into()),
            start_directory: None,
            environment: None,
        },
    };
    let native = PaneProcessRetirementOwner::default();
    let mut runtime = SessionFactory::create_with_native_custody(request, native.clone(), None)
        .await
        .unwrap();
    assert!(
        root_path(&directory.0, &session_id)
            .join("registration.v1")
            .exists()
    );
    let (_, contexts) = AgentTranscriptStore::under_config_root(&directory.0)
        .capture_native_session_metadata(&session_id)
        .unwrap();
    assert!(contexts.is_empty());
    let handle = runtime.handle().actor().clone();
    let runner = tokio::spawn(runtime.actor.take().unwrap().run());
    handle.shutdown().await.unwrap();
    let mut exited = runner.await.unwrap();
    exited.service.terminate_all_pane_processes().unwrap();
    drop(runtime);
}

/// Owns only the generated directory used for fresh/cross-process fixtures.
struct CrashDirectory(PathBuf);
impl CrashDirectory {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "mez-mmp-crash-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        )))
    }
}
impl Drop for CrashDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Cross-process expected public attribution and inert epoch bytes only. The
/// actual layout uses the established snapshot repository codec; no live token
/// or message custody checkpoint is decoded from this test handoff.
#[derive(serde::Serialize, serde::Deserialize)]
struct CrashExpectations {
    epoch: [u8; 16],
    native: String,
    raw: String,
}

/// Publishes only expected layout/public attribution, then exits without any
/// source/actor/root/store destructor. This supplies no live recovery authority.
fn finish_crash(fixture: &Fixture) -> ! {
    use std::os::unix::fs::OpenOptionsExt;
    SnapshotRepository::new(fixture.config.clone())
        .write_payload("crash-layout", &fixture.payload)
        .unwrap();
    let mut handoff = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(fixture.config.join("crash-expectations.json"))
        .unwrap();
    serde_json::to_writer(
        &mut handoff,
        &CrashExpectations {
            epoch: fixture.epoch,
            native: fixture.native.to_string(),
            raw: fixture.raw.to_string(),
        },
    )
    .unwrap();
    handoff.sync_all().unwrap();
    fs::File::open(&fixture.config).unwrap().sync_all().unwrap();
    std::process::exit(0);
}

const CRASH_ROOT: &str = "MEZ_TEST_REGISTERED_MMP_CRASH_ROOT";

/// Exact-filter child entry. With the private parent environment it exits from
/// the real original publisher before service/root destructors or checkpoint
/// capture. Normal harness discovery does no work and mutates no environment.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registered_custody_process_crash_child() {
    let Some(root) = std::env::var_os(CRASH_ROOT) else {
        return;
    };
    let _ = Fixture::build(true, Original::CrashRegistered(root.into())).await;
    panic!("registered original child must exit before orderly retirement");
}

/// An actual process exit releases the original live kernel fence without any
/// Rust/root cleanup. A new process reconstructs from the original registration,
/// journal, private context and ACKs: no message snapshot, resend or provider
/// inference supplies the retained second message or its original native epoch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_factory_recovers_after_process_crash_without_message_checkpoint() {
    let directory = CrashDirectory::new();
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "host::session::native_custody::tests::registered_custody_process_crash_child",
            "--nocapture",
        ])
        .env(CRASH_ROOT, &directory.0)
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(30), child.output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: CrashExpectations =
        serde_json::from_slice(&fs::read(directory.0.join("crash-expectations.json")).unwrap())
            .unwrap();
    let fixture = Fixture {
        config: directory.0.clone(),
        payload: SnapshotRepository::new(directory.0.clone())
            .inspect_payload("crash-layout")
            .unwrap(),
        epoch: expected.epoch,
        native: AgentId::opaque(expected.native).unwrap(),
        raw: AgentId::opaque(expected.raw).unwrap(),
    };
    assert!(fixture.payload.message_state.is_none());
    assert_factory_redelivery(&fixture).await;
}

mod native_ack_snapshot;
mod native_input;

mod registration_end;
mod terminal;

mod raw_retirement;
