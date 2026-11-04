//! Actual process loss of raw endpoints, original ends and terminal positives.
//!
//! The child accepts real guarded sources and exits before selected lower
//! adoption. The parent uses the production registered-root/private-context
//! factory and actor. No raw bearer, decoded epoch or descriptive frame is used
//! to recreate an endpoint, ACK, provider task or effect.

use super::*;
use mez_agent::messaging::{DeliveryTerminalReason, MessageAcceptanceDecodeLimits};

const RAW_CRASH_ROOT: &str = "MEZ_TEST_RAW_CUSTODY_CRASH_ROOT";
const RAW_CRASH_POINT: &str = "MEZ_TEST_RAW_CUSTODY_CRASH_POINT";

/// The actual commit/adoption boundary at which the child exits without Drop.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CrashPoint {
    Accepted,
    End,
    Later,
    Terminal,
    Release,
}
impl CrashPoint {
    fn label(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::End => "end",
            Self::Later => "later",
            Self::Terminal => "terminal",
            Self::Release => "release",
        }
    }
}

/// Uses the same narrow decoder quantum as the original recovery worker.
fn limits() -> MessageAcceptanceDecodeLimits {
    MessageAcceptanceDecodeLimits {
        payload_bytes: 1024 * 1024,
        auxiliary_bytes: 512 * 1024,
        recipient_count: 4096,
        recipient_bytes: 1024 * 1024,
    }
}
/// Observes physical journals without constructing positive ownership.
fn count(db: &rusqlite::Connection, prefix: &str) -> i64 {
    db.query_row(
        "SELECT count(*) FROM records WHERE key LIKE ?1",
        [format!("{prefix}:%")],
        |row| row.get(0),
    )
    .unwrap()
}

/// Commits only real source/end/terminal operations on the original lane.
/// Later-origin mode advances the real watermark after the first raw cutoff,
/// proving startup must preserve existing end bytes instead of renewing them.
pub(super) async fn commit_then_crash(
    mut service: RuntimeSessionService,
    root: MmpCustodyRoot,
    fixture: Fixture,
    point: CrashPoint,
    first: Option<u64>,
) -> ! {
    if point == CrashPoint::Accepted {
        finish_crash(&fixture);
    }
    assert!(
        service
            .message_service_mut()
            .retire_agent_identity(&fixture.raw)
    );
    assert!(
        root.start_registration_end(service.message_service())
            .unwrap()
    );
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::RegistrationEndAwaitingAdoption
    );
    if point == CrashPoint::End {
        finish_crash(&fixture);
    }
    root.adopt_registration_end(service.message_service_mut())
        .unwrap();
    if point == CrashPoint::Later {
        let sender =
            service
                .message_service_mut()
                .register_agent(None, None, "later sender", vec![]);
        let target = service
            .message_service()
            .registered_identity(&fixture.native)
            .unwrap()
            .clone();
        let original = Envelope {
            protocol: "mmp/1",
            id: "after-original-raw-end".into(),
            message_type: "send".into(),
            time: "later".into(),
            sender: sender.clone(),
            recipient: Recipient::Agent(target.agent_id.clone()),
            correlation_id: None,
            ttl_ms: None,
            content_type: "text/plain".into(),
            payload: "later independent original".into(),
            extension_fields: vec![],
        };
        let accepted = service
            .message_service_mut()
            .prepare_acceptance(
                &sender.agent_id,
                original,
                MessageScope::Session,
                101,
                4096,
                None,
            )
            .unwrap();
        assert_eq!(accepted.sequence(), 3);
        root.start(service.message_service_mut(), &accepted, None)
            .unwrap();
        assert_eq!(
            root.observe().await.unwrap(),
            CustodyWorkerStatus::AwaitingAdoption
        );
        root.adopt(service.message_service_mut(), None).unwrap();
        assert_eq!(
            service
                .message_service()
                .receive_subscribed(&target.agent_id, 102, 1)
                .unwrap()
                .messages[0]
                .sequence,
            3
        );
        let ack = service
            .message_service_mut()
            .prepare_acknowledgement(&target.agent_id, 3, 512 * 1024)
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
        finish_crash(&fixture);
    }
    if point == CrashPoint::Release {
        root.advance_elapsed_clock_for_tests(first.unwrap())
            .unwrap();
    }
    let ticket = service
        .message_service_mut()
        .prepare_storage_raw_retirement(
            root.storage_scope(),
            first.unwrap_or_else(crate::runtime::current_unix_millis),
            usize::MAX,
        )
        .unwrap()
        .unwrap();
    root.start_terminal(service.message_service_mut(), &ticket)
        .unwrap();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalAwaitingAdoption
    );
    if point == CrashPoint::Release {
        root.adopt_terminal(service.message_service_mut()).unwrap();
        drop(ticket);
        root.advance_elapsed_clock_for_tests(crate::runtime::current_unix_millis())
            .unwrap();
        let release = service
            .message_service_mut()
            .prepare_storage_horizon_release(
                root.storage_scope(),
                crate::runtime::current_unix_millis(),
                usize::MAX,
            )
            .unwrap()
            .unwrap();
        root.start_terminal(service.message_service_mut(), &release)
            .unwrap();
        assert_eq!(
            root.observe().await.unwrap(),
            CustodyWorkerStatus::TerminalAwaitingAdoption
        );
    }
    assert!(service.message_service().has_unpublished_terminal());
    finish_crash(&fixture);
}

/// Remains inert during normal discovery; its generated child exits inside the
/// actual original positive before Rust/root/service cleanup can acknowledge it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_recipient_process_crash_child() {
    let Some(path) = std::env::var_os(RAW_CRASH_ROOT) else {
        return;
    };
    let point = match std::env::var(RAW_CRASH_POINT).unwrap().as_str() {
        "accepted" => CrashPoint::Accepted,
        "end" => CrashPoint::End,
        "later" => CrashPoint::Later,
        "terminal" => CrashPoint::Terminal,
        "release" => CrashPoint::Release,
        other => panic!("unknown generated crash point {other}"),
    };
    let _ = Fixture::build(true, Original::CrashRawRecipient(path.into(), point)).await;
    panic!("raw child must exit before destructors");
}

/// Actual registered-root factory recovery classifies only explicit old raw
/// namespaces as unavailable. The live native context remains its original
/// epoch; raw mail is never native input and no reconnect identity is restored.
async fn recover(point: CrashPoint) {
    let directory = CrashDirectory::new();
    let output = tokio::time::timeout(Duration::from_secs(45), tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact","host::session::native_custody::tests::raw_retirement::raw_recipient_process_crash_child","--nocapture"])
        .env(RAW_CRASH_ROOT,&directory.0).env(RAW_CRASH_POINT,point.label()).kill_on_drop(true).output()).await.unwrap().unwrap();
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
    let mut request = fixture.request();
    request.config.layers = vec![native_input::config()];
    let mut runtime = if point == CrashPoint::Release {
        SessionFactory::create_with_custody_clock_for_tests(
            request,
            PaneProcessRetirementOwner::default(),
            crate::storage::mmp_custody::elapsed_clock::ClockSource::controlled_for_tests(
                crate::runtime::current_unix_millis() - 1,
            ),
        )
        .await
    } else {
        SessionFactory::create_with_native_custody(
            request,
            PaneProcessRetirementOwner::default(),
            None,
        )
        .await
    }
    .unwrap();
    let handle = runtime.handle().actor().clone();
    let runner = tokio::spawn(runtime.actor.take().unwrap().run());
    let path = root_path(&fixture.config, &fixture.payload.session_id);
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            handle.lifecycle_state().await.unwrap();
            let db = rusqlite::Connection::open(path.join("custody.sqlite")).unwrap();
            if count(&db, "d1") == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    handle.shutdown().await.unwrap();
    let mut exited = runner.await.unwrap();
    assert!(
        exited
            .service
            .message_service()
            .registered_identity(&fixture.raw)
            .is_none()
    );
    assert!(
        !exited
            .service
            .message_service()
            .has_pending_registration_end()
            .unwrap()
    );
    assert!(exited.service.pending_agent_provider_tasks().is_empty());
    assert!(!exited.service.has_pending_native_input());
    assert!(
        exited
            .service
            .agent_turn_ledger()
            .turns()
            .iter()
            .all(|turn| turn.state == mez_agent::AgentTurnState::Interrupted)
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
    assert_eq!(
        exited
            .service
            .message_service()
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        usize::from(point != CrashPoint::Release)
    );
    let db = rusqlite::Connection::open(path.join("custody.sqlite")).unwrap();
    assert_eq!(count(&db, "o1"), 1);
    assert_eq!(count(&db, "d1"), 1);
    assert_eq!(count(&db, "r1"), i64::from(point == CrashPoint::Release));
    assert_eq!(
        count(&db, "k1"),
        if point == CrashPoint::Later { 2 } else { 1 }
    );
    assert_eq!(count(&db, "c1"), 0);
    assert_eq!(count(&db, "p1"), 0);
    let bytes: Vec<u8> = db
        .query_row(
            "SELECT value FROM records WHERE key LIKE 'o1:%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let end: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(end["snapshot"]["ended_after_sequence"], 2);
    let store = crate::storage::mmp_custody::MmpCustodyStore::open_existing(
        &path,
        &fixture.payload.session_id,
        Default::default(),
    )
    .unwrap();
    let history = store.fold_custody_history(limits()).unwrap();
    assert_eq!(history.terminal_observations.len(), 1);
    assert_eq!(
        history.terminal_observations[0].reason,
        DeliveryTerminalReason::RecipientRetired
    );
    assert_eq!(history.terminal_observations[0].sequence, 2);
    assert_eq!(
        history.terminal_observations[0].released(),
        point == CrashPoint::Release
    );
    assert_eq!(
        history.inventory.sequence_watermark,
        if point == CrashPoint::Later { 3 } else { 2 }
    );
    exited.service.terminate_all_pane_processes().unwrap();
    drop(runtime);
}

/// No saved message checkpoint/end/terminal exists: actual process loss alone
/// makes the explicit old raw endpoint unavailable under sealed cold startup.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_factory_process_loss_journals_original_unavailable_endpoint() {
    recover(CrashPoint::Accepted).await;
}
/// A committed o1 whose live adoption was lost must be reconciled unchanged,
/// then authorize only its original raw source's terminal observation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_factory_recovers_end_commit_before_original_adoption() {
    recover(CrashPoint::End).await;
}
/// Later unrelated accepted/ACKed mail advances the allocator but cannot renew
/// an already committed raw namespace cutoff during cold reconstruction.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_factory_preserves_prior_end_cutoff_below_later_watermark() {
    recover(CrashPoint::Later).await;
}
/// A real raw d1 positive lost at process exit reconstructs the same reason and
/// pending obligation without requiring native or raw application reinjection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_factory_recovers_retirement_commit_before_original_adoption() {
    recover(CrashPoint::Terminal).await;
}
/// A separate raw R1 positive lost at process exit releases only that original
/// terminal fact; it does not become a recipient ACK or provider continuation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_factory_recovers_horizon_release_before_original_adoption() {
    recover(CrashPoint::Release).await;
}
