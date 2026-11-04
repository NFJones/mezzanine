//! Actual canonical native input writes, source-bound proofs and exact ACKs.
//! These root-boundary tests use real lower acceptance/native ownership and the
//! existing transcript archive. Actor scheduling/production activation remains
//! a separate acceptance gate; no test here claims provider execution coverage.

use super::*;
use crate::storage::transcript::AgentTranscriptStore;
use mez_agent::messaging::{
    MessageNativeIncarnation, MessageNativeInputAnchor, MessageNativeInputCommit,
};
use mez_agent::transcript::{
    TranscriptContextEvent, TranscriptEntry, TranscriptNativeInputEvent,
    TranscriptNativeInputSource, TranscriptRole,
};

/// One original accepted native source and an actual post-context projection.
struct Scene {
    root: MmpCustodyRoot,
    service: MessageService,
    target: SenderIdentity,
    anchor: MessageNativeInputAnchor,
    transcript: AgentTranscriptStore,
    row: TranscriptEntry,
}
impl Scene {
    /// Uses real registration, continuity, acceptance, selection and canonical
    /// context append before constructing its immutable transcript projection.
    async fn new(fixture: &Fixture) -> Self {
        let root = fixture.root();
        assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
        let (mut service, sender, target, _) = fixture_service();
        root.configure_payload_spill(&mut service, Default::default())
            .unwrap();
        let registration = service
            .capture_sender_registration(&target.agent_id)
            .unwrap();
        service
            .bind_native_conversation(
                &registration,
                "native-receiver",
                &MessageNativeIncarnation::default(),
            )
            .unwrap();
        root.start_native_continuity(&service, &target.agent_id)
            .unwrap();
        assert_eq!(
            root.observe().await.unwrap(),
            CustodyWorkerStatus::NativeContinuityAwaitingAdoption
        );
        assert!(root.adopt_native_continuity(&mut service).unwrap());
        service.subscribe(&target.agent_id).unwrap();
        let mut original = envelope(&sender, &target, "original-peer-message");
        original.payload = "actual peer input\0with newline\nand untrusted guidance".into();
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
        service
            .receive_subscribed(&target.agent_id, 101, 1)
            .unwrap();
        let anchor = service
            .capture_native_input_anchor(&target.agent_id, ticket.sequence())
            .unwrap();
        let store_id: Vec<u8> = fixture
            .connection()
            .query_row("SELECT store_id FROM state", [], |row| row.get(0))
            .unwrap();
        let envelope = anchor.resident_envelope().unwrap();
        let label = format!("actual peer event {}", envelope.id);
        let content = envelope.payload.clone();
        let mut context = mez_agent::AgentContext::empty();
        context
            .append_peer_message_event(label.clone(), content.clone())
            .unwrap();
        let event = TranscriptNativeInputEvent::new(
            TranscriptNativeInputSource::from_anchor(&anchor, store_id.try_into().unwrap())
                .unwrap(),
            context.event_sequence_high_water_mark(),
            label,
            content,
        )
        .unwrap();
        let transcript = AgentTranscriptStore::new(fixture.path.join("canonical"));
        let row = TranscriptEntry {
            conversation_id: "native-receiver".into(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: TranscriptRole::System,
            turn_id: "actual-receive-turn".into(),
            agent_id: target.agent_id.to_string(),
            pane_id: "pane-original".into(),
            content: TranscriptContextEvent::NativePeerMessage {
                event: Box::new(event),
            }
            .to_transcript_content(),
        };
        let mut initial = row.clone();
        initial.content = "existing canonical context".into();
        initial.turn_id = "earlier-turn".into();
        transcript.append(&initial).unwrap();
        Self {
            root,
            service,
            target,
            anchor,
            transcript,
            row,
        }
    }
    /// Starts the actual original worker; statuses below are checked only after
    /// its retained handle has actually joined.
    fn start(&self) {
        self.root
            .start_native_input(
                &self.service,
                self.anchor.clone(),
                self.transcript.clone(),
                Some(self.row.clone()),
            )
            .unwrap();
    }
    /// Adopts only actual joined native commit proof, without a transport ACK.
    async fn adopt(&self) -> MessageNativeInputCommit {
        assert_eq!(
            self.root.observe().await.unwrap(),
            CustodyWorkerStatus::NativeInputAwaitingAdoption
        );
        self.root
            .adopt_native_input(&self.service)
            .unwrap()
            .unwrap()
    }
    /// Verifies the canonical occurrence count directly in physical storage.
    fn canonical_count(&self) -> usize {
        self.transcript
            .inspect("native-receiver")
            .unwrap()
            .into_iter()
            .filter(|row| {
                matches!(
                    TranscriptContextEvent::from_transcript_content(&row.content),
                    Some(TranscriptContextEvent::NativePeerMessage { .. })
                )
            })
            .count()
    }
    /// Executes the actual shared-lane ACK journal and lower publication.
    async fn ack(&mut self, proof: &MessageNativeInputCommit) {
        let ticket = self
            .service
            .prepare_native_input_acknowledgement(proof, 512 * 1024)
            .unwrap()
            .unwrap();
        self.root
            .start_acknowledgement(&mut self.service, &ticket)
            .unwrap();
        assert_eq!(
            self.root.observe().await.unwrap(),
            CustodyWorkerStatus::AcknowledgementAwaitingAdoption
        );
        self.root.adopt_acknowledgement(&mut self.service).unwrap();
    }
}

/// Qualified absence creates no input. A genuine post-context write becomes
/// one physical canonical event before the exact original native ACK can commit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_input_commits_canonical_archive_before_actual_exact_ack() {
    let fixture = Fixture::new(true);
    let mut scene = Scene::new(&fixture).await;
    let lookup = scene
        .service
        .capture_native_input_lookup(scene.anchor.sequence(), scene.anchor.binding_index())
        .unwrap();
    scene
        .root
        .start_native_input(&scene.service, lookup, scene.transcript.clone(), None)
        .unwrap();
    assert_eq!(
        scene.root.observe().await.unwrap(),
        CustodyWorkerStatus::NativeInputAwaitingAdoption
    );
    assert!(
        scene
            .root
            .adopt_native_input(&scene.service)
            .unwrap()
            .is_none()
    );
    assert_eq!(scene.canonical_count(), 0);
    scene.start();
    let proof = scene.adopt().await;
    assert_eq!(scene.canonical_count(), 1);
    assert_eq!(proof.record().archive_sequence, 2);
    assert_eq!(
        scene
            .service
            .subscription(&scene.target.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
    let stored =
        MmpCustodyStore::open_existing(&fixture.path, "original-session", Default::default())
            .unwrap();
    assert_eq!(
        stored
            .inventory_custody(mez_agent::messaging::MessageAcceptanceDecodeLimits {
                payload_bytes: 1024 * 1024,
                auxiliary_bytes: 512 * 1024,
                recipient_count: 4096,
                recipient_bytes: 1024 * 1024,
            })
            .unwrap()
            .occurrences
            .len(),
        1
    );
    scene.ack(&proof).await;
    assert_eq!(
        scene
            .service
            .subscription(&scene.target.agent_id)
            .unwrap()
            .last_sequence,
        1
    );
    assert_eq!(scene.canonical_count(), 1);
}

/// Actual worker panic after canonical+c1 commit loses the positive reply, not
/// its immutable original preparation. Retry must not append or journal twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_input_lost_positive_reconciles_without_second_canonical_input() {
    let fixture = Fixture::new(true);
    let scene = Scene::new(&fixture).await;
    fixture
        .probe
        .panic_after_commit
        .store(true, Ordering::SeqCst);
    scene.start();
    let CustodyWorkerStatus::NativeInputRetryAt(due) = scene.root.observe().await.unwrap() else {
        panic!("lost actual native worker must join");
    };
    let before = fixture.disk();
    assert_eq!(scene.canonical_count(), 1);
    assert!(!scene.root.retry_native_input(Instant::now()).unwrap());
    assert!(scene.root.retry_native_input(due).unwrap());
    let _proof = scene.adopt().await;
    assert_eq!(fixture.disk(), before);
    assert_eq!(scene.canonical_count(), 1);
}

/// A lost positive must not let retry adopt a new preservation baseline. Even
/// a rechecksummed, semantically equal mutation of a different original row
/// refuses adoption until the exact retained non-target bytes are restored.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_input_lost_positive_keeps_original_non_target_fingerprint() {
    use sha2::{Digest, Sha256};
    let fixture = Fixture::new(true);
    let scene = Scene::new(&fixture).await;
    fixture
        .probe
        .panic_after_commit
        .store(true, Ordering::SeqCst);
    scene.start();
    let CustodyWorkerStatus::NativeInputRetryAt(due) = scene.root.observe().await.unwrap() else {
        panic!("actual lost positive must join");
    };
    let connection = fixture.connection();
    let (key, original): (String, Vec<u8>) = connection
        .query_row(
            "SELECT key,value FROM records WHERE key LIKE 'n1:%'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let mut changed = original.clone();
    changed.push(b' ');
    connection
        .execute(
            "UPDATE records SET value=?1,digest=?2 WHERE key=?3",
            rusqlite::params![changed, Sha256::digest(&changed).as_slice(), key],
        )
        .unwrap();
    scene.root.retry_native_input(due).unwrap();
    let CustodyWorkerStatus::NativeInputRetryAt(next) = scene.root.observe().await.unwrap() else {
        panic!("retry cannot resample a changed non-target row");
    };
    assert_eq!(scene.canonical_count(), 1);
    connection
        .execute(
            "UPDATE records SET value=?1,digest=?2 WHERE key=?3",
            rusqlite::params![original, Sha256::digest(&original).as_slice(), key],
        )
        .unwrap();
    scene.root.retry_native_input(next).unwrap();
    let _proof = scene.adopt().await;
    assert_eq!(scene.canonical_count(), 1);
}

/// A real SQL refusal after the actual transcript commit leaves a p1 recovery
/// owner. The next attempt requalifies that exact archive row and publishes c1
/// without repeating the input or allocating a later sequence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_input_canonical_commit_survives_missing_commit_journal_reply() {
    let fixture = Fixture::new(true);
    let scene = Scene::new(&fixture).await;
    fixture.connection().execute_batch("CREATE TRIGGER stop_native_commit BEFORE INSERT ON records WHEN NEW.key LIKE 'c1:%' BEGIN SELECT RAISE(FAIL,'injected native commit refusal'); END;").unwrap();
    scene.start();
    let CustodyWorkerStatus::NativeInputRetryAt(due) = scene.root.observe().await.unwrap() else {
        panic!("expected actual post-input journal refusal");
    };
    assert_eq!(scene.canonical_count(), 1);
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM records WHERE key LIKE 'p1:%'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    fixture
        .connection()
        .execute_batch("DROP TRIGGER stop_native_commit")
        .unwrap();
    scene.root.retry_native_input(due).unwrap();
    let proof = scene.adopt().await;
    assert_eq!(proof.record().archive_sequence, 2);
    assert_eq!(scene.canonical_count(), 1);
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM records WHERE key LIKE 'p1:%'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

/// After a real canonical commit but before c1, replacing the conversation
/// backing must not make a retry append the same source to the replacement.
/// Restoring the original physical backing permits exact reconciliation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_input_preparation_refuses_replacement_archive_and_recovers_original() {
    let fixture = Fixture::new(true);
    let scene = Scene::new(&fixture).await;
    fixture.connection().execute_batch("CREATE TRIGGER stop_native_commit BEFORE INSERT ON records WHEN NEW.key LIKE 'c1:%' BEGIN SELECT RAISE(FAIL,'injected native commit refusal'); END;").unwrap();
    scene.start();
    let CustodyWorkerStatus::NativeInputRetryAt(due) = scene.root.observe().await.unwrap() else {
        panic!("expected pending original canonical commit");
    };
    fixture
        .connection()
        .execute_batch("DROP TRIGGER stop_native_commit")
        .unwrap();
    let original = scene.transcript.root().join("native-receiver");
    let retained = scene.transcript.root().join("retained-original");
    std::fs::rename(&original, &retained).unwrap();
    let mut replacement = scene.row.clone();
    replacement.content = "replacement context must receive no old input".into();
    scene.transcript.append(&replacement).unwrap();
    let before = fixture.disk();
    scene.root.retry_native_input(due).unwrap();
    let CustodyWorkerStatus::NativeInputRetryAt(next) = scene.root.observe().await.unwrap() else {
        panic!("replacement must refuse old preparation");
    };
    assert_eq!(scene.canonical_count(), 0);
    assert_eq!(fixture.disk(), before);
    std::fs::remove_dir_all(&original).unwrap();
    std::fs::rename(&retained, &original).unwrap();
    scene.root.retry_native_input(next).unwrap();
    let _proof = scene.adopt().await;
    assert_eq!(scene.canonical_count(), 1);
}

/// A sealed original commit survives conversation deletion and a new current
/// native epoch. Lookup never opens a replacement archive or recreates input;
/// the real exact ACK settles only the old frozen source slot.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_input_sealed_commit_settles_original_after_context_replacement() {
    let fixture = Fixture::new(true);
    let mut scene = Scene::new(&fixture).await;
    scene.start();
    let first = scene.adopt().await;
    std::fs::remove_dir_all(scene.transcript.root().join("native-receiver")).unwrap();
    let registration = scene
        .service
        .capture_sender_registration(&scene.target.agent_id)
        .unwrap();
    scene
        .service
        .bind_native_conversation(
            &registration,
            "new-native-context",
            &MessageNativeIncarnation::default(),
        )
        .unwrap();
    let lookup = scene
        .service
        .capture_native_input_lookup(scene.anchor.sequence(), scene.anchor.binding_index())
        .unwrap();
    let missing = AgentTranscriptStore::new(fixture.path.join("must-remain-absent"));
    scene
        .root
        .start_native_input(&scene.service, lookup, missing.clone(), None)
        .unwrap();
    let recovered = scene.adopt().await;
    assert_eq!(first.record(), recovered.record());
    assert!(!missing.root().exists());
    scene.ack(&recovered).await;
    assert_eq!(
        scene
            .service
            .subscription(&scene.target.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
}

/// A typed frame's public fields do not grant source authority. A mismatching
/// acceptance hash is rejected before a preparation or transcript append.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_input_rejects_typed_frame_for_another_source() {
    let fixture = Fixture::new(true);
    let mut scene = Scene::new(&fixture).await;
    let Some(TranscriptContextEvent::NativePeerMessage { mut event }) =
        TranscriptContextEvent::from_transcript_content(&scene.row.content)
    else {
        panic!("actual native frame required");
    };
    event.source.source_sha256 = [7; 32];
    let event = TranscriptNativeInputEvent::new(
        event.source,
        event.event_sequence,
        event.label,
        event.content,
    )
    .unwrap();
    scene.row.content = TranscriptContextEvent::NativePeerMessage {
        event: Box::new(event),
    }
    .to_transcript_content();
    let before = fixture.disk();
    scene.start();
    assert!(matches!(
        scene.root.observe().await.unwrap(),
        CustodyWorkerStatus::NativeInputRetryAt(_)
    ));
    assert_eq!(scene.canonical_count(), 0);
    assert_eq!(fixture.disk(), before);
}

/// SQL side effects on non-target custody rows invalidate the entire native
/// preparation transaction. Accepted originals remain byte-for-byte intact and
/// no canonical input is written before the preparation is actually durable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_input_preparation_rolls_back_unrelated_trigger_effects() {
    let fixture = Fixture::new(true);
    let scene = Scene::new(&fixture).await;
    fixture.connection().execute_batch("CREATE TRIGGER corrupt_original AFTER INSERT ON records WHEN NEW.key LIKE 'p1:%' BEGIN DELETE FROM records WHERE key LIKE 'a1:%:stage'; END;").unwrap();
    let before = fixture.disk();
    scene.start();
    let CustodyWorkerStatus::NativeInputRetryAt(due) = scene.root.observe().await.unwrap() else {
        panic!("unrelated mutation must reject preparation");
    };
    assert_eq!(fixture.disk(), before);
    assert_eq!(scene.canonical_count(), 0);
    fixture
        .connection()
        .execute_batch("DROP TRIGGER corrupt_original")
        .unwrap();
    scene.root.retry_native_input(due).unwrap();
    let _proof = scene.adopt().await;
    assert_eq!(scene.canonical_count(), 1);
}

/// A cold accepted payload is not required to qualify an already committed
/// canonical input. Lookup streams its original fingerprint, never runs an
/// extra unreserved source decoder or installs payload bytes in the cache.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_input_committed_lookup_needs_no_payload_cache_or_source_decode() {
    let fixture = Fixture::new(true);
    let mut scene = Scene::new(&fixture).await;
    scene.start();
    let first = scene.adopt().await;
    scene
        .service
        .release_stored_payload(scene.anchor.sequence(), scene.root.storage_scope())
        .unwrap();
    assert!(scene.anchor.resident_envelope().is_err());
    let lookup = scene
        .service
        .capture_native_input_lookup(scene.anchor.sequence(), scene.anchor.binding_index())
        .unwrap();
    scene
        .root
        .start_native_input(&scene.service, lookup, scene.transcript.clone(), None)
        .unwrap();
    let recovered = scene.adopt().await;
    assert_eq!(first.record(), recovered.record());
    assert_eq!(
        scene.service.storage_usage().unwrap().unwrap().cache,
        mez_agent::messaging::MessagePayloadUsage::default()
    );
    {
        let state = scene.root.0.state.lock().unwrap();
        let State::Idle(opened) = &*state else {
            panic!("actual native result was adopted");
        };
        assert_eq!(opened.store.native_input_source_decode_bytes(), 0);
    }
    scene.ack(&recovered).await;
}

/// A trigger changing another custody row during c1 publication must roll back
/// c1 and retain p1 even though canonical input has already committed. Retrying
/// after removing the trigger seals the same input exactly once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_input_commit_rolls_back_unrelated_trigger_effects() {
    let fixture = Fixture::new(true);
    let scene = Scene::new(&fixture).await;
    fixture.connection().execute_batch("CREATE TRIGGER corrupt_original AFTER INSERT ON records WHEN NEW.key LIKE 'c1:%' BEGIN DELETE FROM records WHERE key LIKE 'a1:%:stage'; END;").unwrap();
    scene.start();
    let CustodyWorkerStatus::NativeInputRetryAt(due) = scene.root.observe().await.unwrap() else {
        panic!("unrelated mutation must reject commit publication");
    };
    assert_eq!(scene.canonical_count(), 1);
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM records WHERE key LIKE 'p1:%'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    fixture
        .connection()
        .execute_batch("DROP TRIGGER corrupt_original")
        .unwrap();
    scene.root.retry_native_input(due).unwrap();
    let _proof = scene.adopt().await;
    assert_eq!(scene.canonical_count(), 1);
}

/// Aborting observation and dropping the final root cannot detach native input
/// I/O or release its original kernel fence while the blocking worker runs.
/// The fallback drain completes the real write but grants no lower ACK/provider
/// continuation after its requesting actor has gone away.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_input_final_root_drop_drains_actual_worker_before_releasing_fence() {
    let fixture = Fixture::new(true);
    let scene = Scene::new(&fixture).await;
    *fixture.probe.gate.0.lock().unwrap() = false;
    let before = fixture.probe.attempts.load(Ordering::SeqCst);
    scene.start();
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture.probe.attempts.load(Ordering::SeqCst) == before {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let observer_root = scene.root.clone();
    let observer = tokio::spawn(async move { observer_root.observe().await });
    tokio::task::yield_now().await;
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    let Scene {
        root,
        service,
        target,
        anchor: _,
        transcript,
        row: _,
    } = scene;
    drop(root);
    assert!(RootOwnership::acquire_existing(&fixture.path).is_err());
    assert_eq!(tokio::spawn(async { 17 }).await.unwrap(), 17);
    fixture.probe.release();
    wait_until_unlocked(&fixture.path).await;
    let rows = transcript.inspect("native-receiver").unwrap();
    assert_eq!(rows.len(), 2);
    assert!(matches!(
        TranscriptContextEvent::from_transcript_content(&rows[1].content),
        Some(TranscriptContextEvent::NativePeerMessage { .. })
    ));
    assert_eq!(
        service
            .subscription(&target.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
}

/// Fault injection and assertions access only this generated private database.
impl Fixture {
    fn connection(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.path.join("custody.sqlite")).unwrap()
    }
}
