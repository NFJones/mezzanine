//! Real process exits after durable u1 or h1, without running Rust destructors.
//! Recovery uses the actual original root and its pre-handoff checkpoint; no
//! restored live sender, task/provider or fabricated canonical proof is supplied.

use super::*;
use crate::storage::mmp_custody::terminal_handoff::HandoffCrashPoint;

const CHILD_PATH: &str = "MEZ_TEST_TERMINAL_HANDOFF_CRASH_PATH";
const CHILD_PHASE: &str = "MEZ_TEST_TERMINAL_HANDOFF_CRASH_PHASE";

/// Inert in ordinary discovery; the parent selects this exact isolated child.
/// The exit occurs inside the real root worker after SQLite commit, before
/// canonical receipt retirement/adoption and without any fixture cleanup.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_handoff_process_crash_child() {
    let Some(path) = std::env::var_os(CHILD_PATH) else {
        return;
    };
    let fixture = Fixture {
        path: PathBuf::from(path),
        probe: Probe::new(true),
    };
    let mut scene = Scene::new(&fixture, Probe::new(true)).await;
    scene.root.start_checkpoint(&scene.service).unwrap();
    assert_eq!(
        scene.root.observe().await.unwrap(),
        CustodyWorkerStatus::SnapshotAwaitingAdoption
    );
    let (checkpoint, lease) = scene.root.adopt_checkpoint().unwrap();
    let path = fixture.path.join("handoff-checkpoint.json");
    std::fs::write(&path, serde_json::to_vec(&checkpoint).unwrap()).unwrap();
    std::fs::File::open(&path).unwrap().sync_all().unwrap();
    std::fs::File::open(&fixture.path)
        .unwrap()
        .sync_all()
        .unwrap();
    drop(checkpoint);
    drop(lease);
    let phase = std::env::var(CHILD_PHASE).unwrap();
    Arc::get_mut(&mut scene.root.0).unwrap().handoff_crash_after = if phase == "s1" {
        None
    } else {
        Some(if phase == "u1" {
            HandoffCrashPoint::Preparation
        } else {
            HandoffCrashPoint::Canonical
        })
    };
    scene.start();
    if phase == "s1" {
        assert_eq!(
            scene.root.observe().await.unwrap(),
            CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
        );
        let proof = scene
            .root
            .adopt_terminal_handoff(&scene.service)
            .unwrap()
            .unwrap();
        let probe = Probe::new(true);
        probe.exit_after_commit.store(true, Ordering::SeqCst);
        Arc::get_mut(&mut scene.root.0).unwrap().terminal_probe = Some(probe);
        let ticket = scene
            .service
            .prepare_storage_sender_consumption(scene.root.storage_scope(), &proof, usize::MAX)
            .unwrap();
        scene
            .root
            .start_terminal(&mut scene.service, &ticket)
            .unwrap();
    }
    let _ = scene.root.observe().await;
    panic!("actual worker must terminate process before returning");
}

/// A real s1 commit before live adoption survives process death. The earlier
/// checkpoint still says unreleased, so only original source/h1/s1 qualification
/// can reconstruct settlement. It adds no horizon clock, ACK, live sender/task,
/// raw bearer or second canonical row, and leaves no orphan handoff receipt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sender_consumption_root_recovers_s1_after_process_crash_before_adoption() {
    let fixture = Fixture::new(true);
    let output = tokio::time::timeout(Duration::from_secs(60), tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "host::async_runtime::mmp_custody::tests::terminal_handoff::crash::terminal_handoff_process_crash_child", "--quiet"])
        .env(CHILD_PATH, &fixture.path).env(CHILD_PHASE, "s1").kill_on_drop(true).output()).await.unwrap().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let count: i64 = fixture
        .connection()
        .query_row(
            "SELECT count(*) FROM records WHERE key LIKE 's1:%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    let checkpoint = serde_json::from_slice(
        &std::fs::read(fixture.path.join("handoff-checkpoint.json")).unwrap(),
    )
    .unwrap();
    let root = MmpCustodyRoot::recover(
        fixture.path.clone(),
        "original-session".into(),
        Default::default(),
        Handle::current(),
        CustodyRecoveryRequest {
            checkpoint: Some(checkpoint),
            limits: Default::default(),
            retention_messages: 16,
            retention_bytes: 4 * 1024 * 1024,
        },
    );
    let observed = root.observe().await.unwrap();
    assert_eq!(
        observed,
        CustodyWorkerStatus::RecoveryAwaitingAdoption,
        "{:?}",
        root.failure().unwrap()
    );
    let mut service = root.adopt_recovery().unwrap().into_service();
    service
        .require_native_continuity(root.storage_scope())
        .unwrap();
    assert_eq!(
        service.storage_usage().unwrap().unwrap().pending_messages,
        0
    );
    assert!(service.discover_agents().is_empty());
    assert!(
        service
            .next_terminal_handoff_lookup(None)
            .unwrap()
            .is_none()
    );
    let transcript = AgentTranscriptStore::new(fixture.path.join("canonical"));
    assert_eq!(transcript.inspect("native-sender").unwrap().len(), 2);
    assert!(
        !transcript
            .has_unsettled_append_receipts("native-sender")
            .unwrap()
    );
}

/// Both crash boundaries leave the original terminal obligation pending. Cold
/// reconstruction and historical lookup finish the original image/journal,
/// retire its receipt and yield canonical proof without a live sender/task.
/// h1 alone never releases the outcome, and no duplicate archive row appears.
/// The recovered original is then installed into a fresh actor: historical
/// lookup and actual S1 settle it without creating any sender task or provider.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_handoff_root_recovers_u1_and_orphan_h1_after_actual_process_crash() {
    for phase in ["u1", "h1"] {
        let fixture = Fixture::new(true);
        let output = tokio::time::timeout(Duration::from_secs(60), tokio::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "host::async_runtime::mmp_custody::tests::terminal_handoff::crash::terminal_handoff_process_crash_child", "--quiet"])
            .env(CHILD_PATH, &fixture.path).env(CHILD_PHASE, phase).kill_on_drop(true).output()).await.unwrap().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let phases = Scene::phases(&fixture);
        assert_eq!(
            (phases.0, phases.1),
            if phase == "u1" { (1, 0) } else { (0, 1) }
        );
        assert!(
            fixture
                .path
                .join("canonical/.append-receipts")
                .read_dir()
                .unwrap()
                .next()
                .is_some()
        );
        let checkpoint = serde_json::from_slice(
            &std::fs::read(fixture.path.join("handoff-checkpoint.json")).unwrap(),
        )
        .unwrap();
        let root = MmpCustodyRoot::recover(
            fixture.path.clone(),
            "original-session".into(),
            Default::default(),
            Handle::current(),
            CustodyRecoveryRequest {
                checkpoint: Some(checkpoint),
                limits: Default::default(),
                retention_messages: 16,
                retention_bytes: 4 * 1024 * 1024,
            },
        );
        let observed = root.observe().await.unwrap();
        assert_eq!(
            observed,
            CustodyWorkerStatus::RecoveryAwaitingAdoption,
            "phase={phase}, failure={:?}",
            root.failure().unwrap()
        );
        let mut service = root.adopt_recovery().unwrap().into_service();
        service
            .require_native_continuity(root.storage_scope())
            .unwrap();
        assert!(service.discover_agents().is_empty());
        assert_eq!(
            service.storage_usage().unwrap().unwrap().pending_messages,
            1
        );
        let source = service.next_terminal_handoff_lookup(None).unwrap().unwrap();
        assert!(!source.is_live());
        let transcript = AgentTranscriptStore::new(fixture.path.join("canonical"));
        root.start_terminal_handoff(&mut service, source.clone(), transcript.clone(), None)
            .unwrap();
        assert_eq!(
            root.observe().await.unwrap(),
            CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
        );
        let proof = root.adopt_terminal_handoff(&service).unwrap().unwrap();
        assert_eq!(proof.record().archive_observed, phase == "u1");
        assert_eq!(Scene::phases(&fixture).0, 0);
        assert_eq!(Scene::phases(&fixture).1, 1);
        assert!(
            !transcript
                .has_unsettled_append_receipts("native-sender")
                .unwrap()
        );
        assert_eq!(transcript.inspect("native-sender").unwrap().len(), 2);
        assert_eq!(
            service.storage_usage().unwrap().unwrap().pending_messages,
            1
        );
        assert!(service.discover_agents().is_empty());
        assert!(!source.terminal().released);
        drop(proof);
        drop(source);
        let mut runtime = crate::test_support::runtime::RuntimeServiceFixture::new().build();
        *runtime.message_service_mut() = service;
        runtime.set_agent_transcript_store(transcript.clone());
        let turns = runtime.agent_turn_ledger().turns().len();
        let (_, mut actor) =
            crate::host::async_runtime::AsyncRuntimeSessionActor::new(runtime, Default::default())
                .unwrap();
        actor.register_message_custody(root).unwrap();
        actor.poll_message_custody_for_tests();
        assert_eq!(
            actor.observe_message_custody_for_tests().await.unwrap(),
            CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
        );
        assert!(actor.service.has_pending_terminal_handoff());
        actor.poll_message_custody_for_tests();
        assert_eq!(
            actor.observe_message_custody_for_tests().await.unwrap(),
            CustodyWorkerStatus::TerminalAwaitingAdoption
        );
        assert!(!actor.service.has_pending_terminal_handoff());
        assert_eq!(
            actor
                .service
                .message_service()
                .storage_usage()
                .unwrap()
                .unwrap()
                .pending_messages,
            0
        );
        assert_eq!(actor.service.agent_turn_ledger().turns().len(), turns);
        assert!(actor.service.pending_agent_provider_tasks().is_empty());
        assert!(actor.service.message_service().discover_agents().is_empty());
        assert_eq!(transcript.inspect("native-sender").unwrap().len(), 2);
        assert!(
            !transcript
                .has_unsettled_append_receipts("native-sender")
                .unwrap()
        );
        assert_eq!(
            fixture
                .connection()
                .query_row(
                    "SELECT count(*) FROM records WHERE key LIKE 's1:%'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
    }
}
