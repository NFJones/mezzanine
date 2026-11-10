//! Positive native ACK is insufficient without exact committed canonical input.

use super::super::retirement_inspection::cleanup;
use super::*;
use mez_agent::messaging::MessageServiceSnapshot;

type Rows = Vec<(String, Vec<u8>, Vec<u8>)>;

fn rows(fixture: &Fixture, predicate: &str) -> Rows {
    let connection = fixture.connection();
    let mut statement = connection
        .prepare(&format!(
            "SELECT key,value,digest FROM records WHERE {predicate} ORDER BY key"
        ))
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// The original source leaves replay retention only through actual later
/// acceptances. Neither a fabricated absence fence nor a fabricated ACK is used.
async fn settled(
    fixture: &Fixture,
    canonical: &Fixture,
) -> (
    MmpCustodyRoot,
    MessageService,
    MessageServiceSnapshot,
    AgentTranscriptStore,
) {
    let mut scene = Scene::build_with_transcript(
        fixture,
        true,
        AgentTranscriptStore::new(canonical.path.join("canonical")),
    )
    .await;
    scene.root.start_checkpoint(&scene.service).unwrap();
    scene.root.observe().await.unwrap();
    let (checkpoint, lease) = scene.root.adopt_checkpoint().unwrap();
    drop(lease);
    scene.start();
    let proof = scene.adopt().await;
    scene.ack(&proof).await;
    assert_eq!(scene.canonical_count(), 1);
    drop(proof);
    let sender = scene.service.register_agent(None, None, "later", vec![]);
    for index in 0..16 {
        let mut input = envelope(&sender, &scene.target, &format!("later-{index}"));
        input.ttl_ms = None;
        let ticket = scene
            .service
            .prepare_acceptance(
                &sender.agent_id,
                input,
                MessageScope::Session,
                102,
                4096,
                None,
            )
            .unwrap();
        scene.root.start(&mut scene.service, &ticket, None).unwrap();
        scene.root.observe().await.unwrap();
        scene.root.adopt(&mut scene.service, None).unwrap();
    }
    let Scene {
        root,
        service,
        transcript,
        ..
    } = scene;
    (root, service, checkpoint, transcript)
}

async fn reclaim(root: &MmpCustodyRoot, service: &MessageService) -> bool {
    root.start_acknowledged_source_reclamation(service, 1, 128 * 1024, 256 * 1024)
        .unwrap();
    root.observe().await.unwrap();
    root.finish_retirement_inspection(service)
        .unwrap()
        .is_some()
}

fn cold(fixture: &Fixture, checkpoint: MessageServiceSnapshot) -> MmpCustodyRoot {
    MmpCustodyRoot::recover(
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
    )
}

/// Retiring the body preserves actual c1 and all unrelated pending sources.
/// An old pre-ACK checkpoint reconstructs only later obligations, never old mail
/// or a native endpoint. Missing c1 or a valid-checksum changed canonical clock
/// after retirement must withhold recovery.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acknowledged_native_reclamation_preserves_commit_and_cold_history() {
    for fault in 0..5 {
        let fixture = Fixture::new(true);
        let canonical = Fixture::new(true);
        let (root, service, checkpoint, transcript) = settled(&fixture, &canonical).await;
        let other = rows(&fixture, "key NOT GLOB 'a1:0000000000000001:*'");
        let committed = reclaim(&root, &service).await;
        if !committed {
            drop((service, transcript));
            cleanup(&fixture, root).await;
            std::fs::remove_dir_all(&canonical.path).unwrap();
            assert!(!canonical.path.exists());
            assert!(
                committed,
                "actual native commit and positive ACK must permit retirement"
            );
            return;
        }
        assert_eq!(
            rows(&fixture, "key NOT GLOB 'a1:0000000000000001:*'"),
            other
        );
        let retired = rows(&fixture, "key GLOB 'a1:0000000000000001:*'");
        assert_eq!(retired.len(), 1);
        assert_eq!(&retired[0].1[..8], b"MMPRET02");
        assert_eq!(transcript.inspect("native-receiver").unwrap().len(), 2);
        drop((root, service, transcript));
        wait_until_unlocked(&fixture.path).await;
        if fault == 4 {
            use sha2::{Digest, Sha256};
            let (key, value, _) = &retired[0];
            let mut value = value.clone();
            value[..8].copy_from_slice(b"MMPRET01");
            let digest = Sha256::digest(&value);
            fixture
                .connection()
                .execute(
                    "UPDATE records SET value=?1,digest=?2 WHERE key=?3",
                    rusqlite::params![value, digest.as_slice(), key],
                )
                .unwrap();
        } else if fault != 0 {
            change_commit(&fixture, fault);
        }
        let root = cold(&fixture, checkpoint);
        let status = root.observe().await.unwrap();
        if fault == 0 {
            assert_eq!(status, CustodyWorkerStatus::RecoveryAwaitingAdoption);
            let service = root.adopt_recovery().unwrap().into_service();
            assert_eq!(
                service.storage_usage().unwrap().unwrap().pending_messages,
                16
            );
            assert!(service.discover_agents().is_empty());
            root.start_checkpoint(&service).unwrap();
            root.observe().await.unwrap();
            let (snapshot, lease) = root.adopt_checkpoint().unwrap();
            drop(lease);
            let sources = snapshot.storage_snapshot.unwrap().sources;
            assert_eq!(sources.len(), 16);
            assert!(sources.iter().all(|source| source.sequence > 1));
            drop(service);
        } else {
            assert_ne!(status, CustodyWorkerStatus::RecoveryAwaitingAdoption);
            if fault != 4 {
                assert_eq!(rows(&fixture, "key GLOB 'a1:0000000000000001:*'"), retired);
            }
        }
        cleanup(&fixture, root).await;
        std::fs::remove_dir_all(&canonical.path).unwrap();
        assert!(!canonical.path.exists());
    }
}

/// Mutations preserve row checksum and the actual original source. A changed
/// offer remains syntactically valid and within the no-TTL source's lifetime.
fn change_commit(fixture: &Fixture, fault: usize) {
    let committed = rows(fixture, "key GLOB 'c1:*'");
    assert_eq!(committed.len(), 1);
    let (key, bytes, _) = &committed[0];
    let connection = fixture.connection();
    match fault {
        1 => assert_eq!(
            connection
                .execute("DELETE FROM records WHERE key=?1", [key])
                .unwrap(),
            1
        ),
        2 => {
            use sha2::{Digest, Sha256};
            let mut value: serde_json::Value = serde_json::from_slice(bytes).unwrap();
            let clock = value["source"]["offered_at_ms"].as_u64().unwrap();
            value["source"]["offered_at_ms"] = (clock + 1).into();
            let bytes = serde_json::to_vec(&value).unwrap();
            let digest = Sha256::digest(&bytes);
            assert_eq!(
                connection
                    .execute(
                        "UPDATE records SET value=?1,digest=?2 WHERE key=?3",
                        rusqlite::params![bytes, digest.as_slice(), key]
                    )
                    .unwrap(),
                1
            );
        }
        3 => assert_eq!(
            connection
                .execute(
                    "UPDATE records SET key=?1 WHERE key=?2",
                    rusqlite::params![key.replacen("c1:", "p1:", 1), key]
                )
                .unwrap(),
            1
        ),
        _ => panic!("unknown fixture fault"),
    }
}

/// A generic positive ACK cannot hide a missing/pending canonical journal or a
/// different canonical offer clock. Every refused rewrite preserves all rows.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acknowledged_native_reclamation_refuses_missing_pending_or_changed_commit() {
    for fault in 1..=3 {
        let fixture = Fixture::new(true);
        let canonical = Fixture::new(true);
        let (root, service, _, transcript) = settled(&fixture, &canonical).await;
        change_commit(&fixture, fault);
        let before = rows(&fixture, "1");
        let committed = reclaim(&root, &service).await;
        let after = rows(&fixture, "1");
        drop((service, transcript));
        cleanup(&fixture, root).await;
        std::fs::remove_dir_all(&canonical.path).unwrap();
        assert!(!canonical.path.exists());
        assert!(!committed);
        assert_eq!(before, after);
    }
}

/// A lost actual postcommit worker result cannot cause live mutation replay.
/// Cold verification adopts only history and leaves the canonical input once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acknowledged_native_reclamation_cold_recovers_after_committed_worker_loss() {
    let fixture = Fixture::new(true);
    let canonical = Fixture::new(true);
    let (root, service, checkpoint, transcript) = settled(&fixture, &canonical).await;
    let before = rows(&fixture, "key NOT GLOB 'a1:0000000000000001:*'");
    fixture
        .probe
        .panic_after_commit
        .store(true, Ordering::SeqCst);
    root.start_acknowledged_source_reclamation(&service, 1, 128 * 1024, 256 * 1024)
        .unwrap();
    root.observe().await.unwrap();
    assert!(root.failure().unwrap().is_some());
    assert!(root.finish_retirement_inspection(&service).is_err());
    assert_eq!(
        rows(&fixture, "key NOT GLOB 'a1:0000000000000001:*'"),
        before
    );
    assert_eq!(
        rows(&fixture, "key GLOB 'a1:0000000000000001:retired'").len(),
        1
    );
    drop((root, service));
    wait_until_unlocked(&fixture.path).await;
    let root = cold(&fixture, checkpoint);
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::RecoveryAwaitingAdoption
    );
    let service = root.adopt_recovery().unwrap().into_service();
    assert_eq!(
        service.storage_usage().unwrap().unwrap().pending_messages,
        16
    );
    assert_eq!(transcript.inspect("native-receiver").unwrap().len(), 2);
    drop((service, transcript));
    cleanup(&fixture, root).await;
    std::fs::remove_dir_all(&canonical.path).unwrap();
    assert!(!canonical.path.exists());
}
