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
    Arc::get_mut(&mut scene.root.0).unwrap().handoff_crash_after =
        Some(if std::env::var(CHILD_PHASE).unwrap() == "u1" {
            HandoffCrashPoint::Preparation
        } else {
            HandoffCrashPoint::Canonical
        });
    scene.start();
    let _ = scene.root.observe().await;
    panic!("actual worker must terminate process before returning");
}

/// Both crash boundaries leave the original terminal obligation pending. Cold
/// reconstruction and historical lookup finish the original image/journal,
/// retire its receipt and yield canonical proof without a live sender/task.
/// h1 alone never releases the outcome, and no duplicate archive row appears.
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
    }
}
