//! Actual s1 consumed terminal history, original proof and common root ownership.

use super::*;
use sha2::Digest;

/// Obtains only a genuine original h1 result after actual canonical image
/// retirement. No DTO, transcript text or simulated store callback seals it.
async fn canonical(scene: &mut Scene) -> mez_agent::messaging::MessageTerminalHandoffCommit {
    scene.start();
    assert_eq!(
        scene.root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalHandoffAwaitingAdoption
    );
    scene
        .root
        .adopt_terminal_handoff(&scene.service)
        .unwrap()
        .unwrap()
}
fn consumed(fixture: &Fixture) -> i64 {
    fixture
        .connection()
        .query_row(
            "SELECT count(*) FROM records WHERE key LIKE 's1:%'",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

/// Retirement observation must retain actual h1/s1 and ended native registration
/// history even after replay eviction and the last original capability drop.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sender_consumption_retirement_commits_to_handoff_and_registration_history() {
    let fixture = Fixture::new(true);
    let mut scene = Scene::retired(&fixture, Probe::new(true)).await;
    let sequence = scene.source.sequence();
    let proof = canonical(&mut scene).await;
    let ticket = scene
        .service
        .prepare_storage_sender_consumption(scene.root.storage_scope(), &proof, usize::MAX)
        .unwrap();
    scene
        .root
        .start_terminal(&mut scene.service, &ticket)
        .unwrap();
    assert_eq!(
        scene.root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalAwaitingAdoption
    );
    assert_eq!(scene.root.adopt_terminal(&mut scene.service).unwrap(), 1);
    drop((ticket, proof));
    let target = scene
        .service
        .register_agent(None, None, "later-raw-recipient", vec![]);
    for index in 0..16 {
        let ticket = scene
            .service
            .prepare_acceptance(
                &scene.sender.agent_id,
                envelope(&scene.sender, &target, &format!("later-handoff-{index}")),
                MessageScope::Session,
                100,
                4096,
                None,
            )
            .unwrap();
        scene.root.start(&mut scene.service, &ticket, None).unwrap();
        assert_eq!(
            scene.root.observe().await.unwrap(),
            CustodyWorkerStatus::AwaitingAdoption
        );
        scene.root.adopt(&mut scene.service, None).unwrap();
    }
    let Scene {
        root,
        service,
        source,
        ..
    } = scene;
    drop(source);
    let before = fixture.disk();
    root.start_retirement_inspection(&service, sequence)
        .unwrap();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::RetirementInspectionAwaitingAdoption
    );
    let candidate = root
        .finish_retirement_inspection(&service)
        .unwrap()
        .unwrap();
    let source = &candidate.sources()[0];
    super::super::retirement_inspection::assert_source_evidence(&fixture, source);
    assert_eq!(source.sequence, sequence);
    assert_eq!(source.unsettled_recipients, 0);
    assert_eq!(source.native_journal_references, 1);
    assert_eq!(source.pending_native_journal_references, 0);
    assert_eq!(
        source.dependencies.records, 3,
        "d1/h1/s1 remain exact dependencies"
    );
    assert_eq!(
        source.preserved_registrations.records, 5,
        "two n1/b1 pairs plus original o1 remain preserved"
    );
    assert_ne!(
        source.dependencies.sha256,
        source.preserved_registrations.sha256
    );
    assert_eq!(fixture.disk(), before);
    drop((candidate, service, root));
    wait_until_unlocked(&fixture.path).await;
    std::fs::remove_dir_all(&fixture.path).unwrap();
    assert!(!fixture.path.exists());
}

/// Actual h1 leaves custody pending; separate s1 commits before live release.
/// Stop permits this accepted fact's final drain. Cold fold distinguishes sender
/// consumption from horizon time and ACK, and only exact original adoption can
/// remove logical pending custody. The frame stays fixed and bodyless.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sender_consumption_root_commits_before_adoption_without_horizon_authority() {
    let fixture = Fixture::new(true);
    let mut scene = Scene::new(&fixture, Probe::new(true)).await;
    let proof = canonical(&mut scene).await;
    assert_eq!(
        scene
            .service
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        1
    );
    let before = Scene::phases(&fixture).2;
    scene.root.request_stop();
    let ticket = scene
        .service
        .prepare_storage_sender_consumption(scene.root.storage_scope(), &proof, usize::MAX)
        .unwrap();
    scene
        .root
        .start_terminal(&mut scene.service, &ticket)
        .unwrap();
    assert_eq!(
        scene.root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalAwaitingAdoption
    );
    assert_eq!(consumed(&fixture), 1);
    assert_eq!(
        scene
            .service
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        1
    );
    let bytes: Vec<u8> = fixture
        .connection()
        .query_row("SELECT value FROM records WHERE key LIKE 's1:%'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(bytes.len(), 157);
    assert_eq!(&bytes[..8], b"MMPTSC01");
    // The actually joined terminal attempt still retains its decoder/scratch
    // charge. This independent cold read decodes one source at a time while
    // no physical worker is running; adoption releases that reservation below.
    let store =
        MmpCustodyStore::open_existing(&fixture.path, "original-session", Default::default())
            .unwrap();
    let limits = mez_agent::messaging::MessageAcceptanceDecodeLimits {
        payload_bytes: 1024 * 1024,
        auxiliary_bytes: 512 * 1024,
        recipient_count: 4096,
        recipient_bytes: 1024 * 1024,
    };
    let history = store.fold_custody_history(limits).unwrap();
    assert_eq!(history.terminal_observations.len(), 1);
    assert!(history.terminal_observations[0].sender_consumed);
    assert_eq!(
        history.terminal_observations[0].release_observed_at_ms,
        None
    );
    assert!(history.acknowledgements.is_empty());
    assert_eq!(scene.root.adopt_terminal(&mut scene.service).unwrap(), 1);
    assert_eq!(
        scene
            .service
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        0
    );
    assert_eq!(Scene::phases(&fixture).2, before + 1);
    assert!(
        scene
            .service
            .prepare_storage_sender_consumption(scene.root.storage_scope(), &proof, usize::MAX)
            .is_err()
    );
}

/// Actual post-s1 worker loss retains original source/decoder and immutable
/// write escrow. After observed join, exact retry does not advance generation
/// again; native replacement does not acquire task rights or invalidate the
/// original fact's historical consumption right.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sender_consumption_root_lost_positive_reconciles_exact_source_after_replacement() {
    let fixture = Fixture::new(true);
    let mut scene = Scene::new(&fixture, Probe::new(true)).await;
    let proof = canonical(&mut scene).await;
    let probe = Probe::new(true);
    probe.panic_after_commit.store(true, Ordering::SeqCst);
    Arc::get_mut(&mut scene.root.0).unwrap().terminal_probe = Some(probe);
    scene
        .service
        .bind_native_conversation(
            &scene.registration,
            "native-sender",
            &MessageNativeIncarnation::default(),
        )
        .unwrap();
    assert!(
        !scene
            .service
            .terminal_handoff_owner_is_current(proof.anchor())
    );
    let ticket = scene
        .service
        .prepare_storage_sender_consumption(scene.root.storage_scope(), &proof, usize::MAX)
        .unwrap();
    let before = Scene::phases(&fixture).2;
    scene
        .root
        .start_terminal(&mut scene.service, &ticket)
        .unwrap();
    let CustodyWorkerStatus::TerminalRetryAt(due) = scene.root.observe().await.unwrap() else {
        panic!("actual joined result loss required");
    };
    assert_eq!(consumed(&fixture), 1);
    assert_eq!(
        scene
            .service
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        1
    );
    scene.root.request_stop();
    assert!(scene.root.retry_terminal(&mut scene.service, due).unwrap());
    assert_eq!(
        scene.root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalAwaitingAdoption
    );
    assert_eq!(Scene::phases(&fixture).2, before + 1);
    assert_eq!(scene.root.adopt_terminal(&mut scene.service).unwrap(), 1);
    assert_eq!(
        scene
            .service
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        0
    );
}

/// SQL trigger damage during s1 cannot remove original h1/source evidence or
/// release the live outcome. Rollback retains the same first plan and actual
/// canonical proof; removing the generated fault allows exact original retry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sender_consumption_root_preserves_non_target_rows_and_retries_original_proof() {
    let fixture = Fixture::new(true);
    let mut scene = Scene::new(&fixture, Probe::new(true)).await;
    let proof = canonical(&mut scene).await;
    let before = Scene::phases(&fixture);
    fixture.connection().execute_batch("CREATE TRIGGER damage_consumption AFTER INSERT ON records WHEN NEW.key LIKE 's1:%' BEGIN DELETE FROM records WHERE key LIKE 'h1:%'; END;").unwrap();
    let ticket = scene
        .service
        .prepare_storage_sender_consumption(scene.root.storage_scope(), &proof, usize::MAX)
        .unwrap();
    scene
        .root
        .start_terminal(&mut scene.service, &ticket)
        .unwrap();
    let CustodyWorkerStatus::TerminalRetryAt(due) = scene.root.observe().await.unwrap() else {
        panic!("actual rolled back s1 required");
    };
    assert_eq!(consumed(&fixture), 0);
    assert_eq!(Scene::phases(&fixture), before);
    assert_eq!(
        scene
            .service
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        1
    );
    fixture
        .connection()
        .execute_batch("DROP TRIGGER damage_consumption")
        .unwrap();
    assert!(scene.root.retry_terminal(&mut scene.service, due).unwrap());
    assert_eq!(
        scene.root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalAwaitingAdoption
    );
    assert_eq!(scene.root.adopt_terminal(&mut scene.service).unwrap(), 1);
    assert_eq!(consumed(&fixture), 1);
}

/// An authentic in-memory proof cannot authorize s1 against missing or altered
/// original h1 evidence. A different canonical position/hash is not a retry
/// baseline. Restoring the exact original journal permits only that first write,
/// preserving the original terminal fact until actual commit and adoption.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sender_consumption_root_requires_actual_unchanged_canonical_journal() {
    for variant in 0..3 {
        let fixture = Fixture::new(true);
        let mut scene = Scene::new(&fixture, Probe::new(true)).await;
        let proof = canonical(&mut scene).await;
        let (key, bytes): (String, Vec<u8>) = fixture
            .connection()
            .query_row(
                "SELECT key,value FROM records WHERE key LIKE 'h1:%'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        if variant == 0 {
            fixture
                .connection()
                .execute("DELETE FROM records WHERE key=?1", [&key])
                .unwrap();
        } else {
            let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            if variant == 1 {
                value["transcript"]["archive_sequence"] = serde_json::json!(99);
            } else {
                value["transcript"]["canonical_sha256"] = serde_json::to_value([9u8; 32]).unwrap();
            }
            let changed = serde_json::to_vec(&value).unwrap();
            let hash: [u8; 32] = sha2::Sha256::digest(&changed).into();
            fixture
                .connection()
                .execute(
                    "UPDATE records SET value=?1,digest=?2 WHERE key=?3",
                    rusqlite::params![changed, hash.as_slice(), &key],
                )
                .unwrap();
        }
        let ticket = scene
            .service
            .prepare_storage_sender_consumption(scene.root.storage_scope(), &proof, usize::MAX)
            .unwrap();
        scene
            .root
            .start_terminal(&mut scene.service, &ticket)
            .unwrap();
        let CustodyWorkerStatus::TerminalRetryAt(due) = scene.root.observe().await.unwrap() else {
            panic!("actual original h1 must qualify");
        };
        assert_eq!(consumed(&fixture), 0);
        assert_eq!(
            scene
                .service
                .storage_usage()
                .unwrap()
                .unwrap()
                .pending_messages,
            1
        );
        let hash: [u8; 32] = sha2::Sha256::digest(&bytes).into();
        fixture.connection().execute("INSERT INTO records(key,value,digest) VALUES(?1,?2,?3) ON CONFLICT(key) DO UPDATE SET value=excluded.value,digest=excluded.digest", rusqlite::params![key, bytes, hash.as_slice()]).unwrap();
        assert!(scene.root.retry_terminal(&mut scene.service, due).unwrap());
        assert_eq!(
            scene.root.observe().await.unwrap(),
            CustodyWorkerStatus::TerminalAwaitingAdoption
        );
        assert_eq!(scene.root.adopt_terminal(&mut scene.service).unwrap(), 1);
        assert_eq!(consumed(&fixture), 1);
    }
}
