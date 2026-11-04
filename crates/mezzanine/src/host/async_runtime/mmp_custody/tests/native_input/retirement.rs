//! Genuine namespace ends, canonical precedence and original d1 worker custody.

use super::*;
use mez_agent::messaging::{DeliveryTerminalReason, MessageNativeInputObservation};

/// Persists the actual end before asking the original canonical-input owner.
async fn end(scene: &mut Scene) {
    scene.service.retire_agent_identity(&scene.target.agent_id);
    assert!(scene.root.start_registration_end(&scene.service).unwrap());
    assert_eq!(
        scene.root.observe().await.unwrap(),
        CustodyWorkerStatus::RegistrationEndAwaitingAdoption
    );
    assert!(
        scene
            .root
            .adopt_registration_end(&mut scene.service)
            .unwrap()
    );
}

/// Uses the same original source lookup and concrete archive port as the actor.
async fn witness(scene: &Scene) -> mez_agent::messaging::MessageNativeRetirementWitness {
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
    let MessageNativeInputObservation::RecipientEnded(witness) =
        scene.root.adopt_native_input(&scene.service).unwrap()
    else {
        panic!("actual original ended owner required");
    };
    witness
}

/// Releases the genuine application offer, rather than manufacturing absence.
fn drop_offer(scene: &mut Scene) {
    let lookup = scene
        .service
        .capture_native_input_lookup(scene.anchor.sequence(), scene.anchor.binding_index())
        .unwrap();
    drop(std::mem::replace(&mut scene.anchor, lookup));
}

/// Counts actual retirement rows without decoding or manufacturing a proof.
fn retirement_count(fixture: &Fixture) -> i64 {
    fixture
        .connection()
        .query_row(
            "SELECT count(*) FROM records WHERE key LIKE 'd1:%'",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

/// An actual application permit remains authoritative across endpoint end.
/// The observed end cannot race that owner into d1; its canonical event and
/// exact ACK subsequently win without inventing a successful delivery.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_canonical_application_owner_wins_after_actual_namespace_end() {
    let fixture = Fixture::new(true);
    let mut scene = Scene::guarded(&fixture).await;
    end(&mut scene).await;
    let ended = witness(&scene).await;
    assert!(
        scene
            .service
            .prepare_storage_native_retirement(scene.root.storage_scope(), &ended, 150, usize::MAX)
            .unwrap()
            .is_none()
    );
    drop(ended);
    scene.start();
    let proof = scene.adopt().await;
    assert_eq!(scene.canonical_count(), 1);
    scene.ack(&proof).await;
    assert_eq!(retirement_count(&fixture), 0);
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

/// Real o1/canonical absence produces one bodyless d1. A post-commit worker
/// panic preserves its first bytes and clock through Stop and actual join;
/// only original lower adoption publishes retirement, without an ACK.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_root_lost_positive_preserves_first_source_clock_and_no_ack() {
    let fixture = Fixture::new(true);
    let mut scene = Scene::guarded(&fixture).await;
    drop_offer(&mut scene);
    // The Scene's replacement lookup is itself a real independent protector.
    // Move it into the root so the returned witness owns that sole lookup.
    end(&mut scene).await;
    let Scene {
        mut root,
        mut service,
        target,
        anchor,
        transcript,
        ..
    } = scene;
    root.start_native_input(&service, anchor, transcript.clone(), None)
        .unwrap();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::NativeInputAwaitingAdoption
    );
    let MessageNativeInputObservation::RecipientEnded(ended) =
        root.adopt_native_input(&service).unwrap()
    else {
        panic!("actual ended owner required");
    };
    let probe = Probe::new(true);
    probe.panic_after_commit.store(true, Ordering::SeqCst);
    Arc::get_mut(&mut root.0).unwrap().terminal_probe = Some(probe.clone());
    let ticket = service
        .prepare_storage_native_retirement(root.storage_scope(), &ended, 90, usize::MAX)
        .unwrap()
        .unwrap();
    root.start_terminal(&mut service, &ticket).unwrap();
    let CustodyWorkerStatus::TerminalRetryAt(due) = root.observe().await.unwrap() else {
        panic!("actual joined positive loss required");
    };
    assert_eq!(retirement_count(&fixture), 1);
    let disk = fixture.disk();
    root.request_stop();
    assert!(root.retry_terminal(&mut service, due).unwrap());
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalAwaitingAdoption
    );
    assert_eq!(root.adopt_terminal(&mut service).unwrap(), 1);
    assert_eq!(fixture.disk(), disk);
    assert_eq!(probe.attempts.load(Ordering::SeqCst), 2);
    assert_eq!(
        service.storage_usage().unwrap().unwrap().pending_messages,
        1
    );
    let bytes: Vec<u8> = fixture
        .connection()
        .query_row("SELECT value FROM records WHERE key LIKE 'd1:%'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(bytes.len(), 168);
    assert_eq!(&bytes[..8], b"MMPTDR01");
    assert_eq!(u64::from_le_bytes(bytes[40..48].try_into().unwrap()), 100);
    let store =
        MmpCustodyStore::open_existing(&fixture.path, "original-session", Default::default())
            .unwrap();
    let history = store
        .fold_custody_history(mez_agent::messaging::MessageAcceptanceDecodeLimits {
            payload_bytes: 1024 * 1024,
            auxiliary_bytes: 512 * 1024,
            recipient_count: 4096,
            recipient_bytes: 1024 * 1024,
        })
        .unwrap();
    assert!(history.acknowledgements.is_empty());
    assert_eq!(
        history.terminal_observations[0].reason,
        DeliveryTerminalReason::RecipientRetired
    );
    assert_eq!(history.terminal_observations[0].observed_at_ms, 100);
    assert_eq!(
        history.terminal_observations[0].release_observed_at_ms,
        None
    );
    assert!(
        transcript
            .inspect("native-receiver")
            .unwrap()
            .iter()
            .all(|r| !matches!(
                TranscriptContextEvent::from_transcript_content(&r.content),
                Some(TranscriptContextEvent::NativePeerMessage { .. })
            ))
    );
    assert!(
        service
            .capture_sender_registration(&target.agent_id)
            .is_none()
    );
}

/// A separately issued r1 requires the actual retired before-image and retains
/// its reason through cold fold. D1 alone leaves custody pending until adoption
/// of this release; neither phase supplies a delivery ACK.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_root_horizon_releases_only_actual_retired_before_image() {
    let fixture = Fixture::new(true);
    let mut scene = Scene::guarded(&fixture).await;
    drop_offer(&mut scene);
    end(&mut scene).await;
    let Scene {
        root,
        mut service,
        anchor,
        transcript,
        ..
    } = scene;
    root.start_native_input(&service, anchor, transcript, None)
        .unwrap();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::NativeInputAwaitingAdoption
    );
    let MessageNativeInputObservation::RecipientEnded(ended) =
        root.adopt_native_input(&service).unwrap()
    else {
        panic!("actual end required");
    };
    let ticket = service
        .prepare_storage_native_retirement(root.storage_scope(), &ended, 90, usize::MAX)
        .unwrap()
        .unwrap();
    root.start_terminal(&mut service, &ticket).unwrap();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalAwaitingAdoption
    );
    assert_eq!(root.adopt_terminal(&mut service).unwrap(), 1);
    drop(ticket);
    drop(ended);
    let release = service
        .prepare_storage_horizon_release(root.storage_scope(), 600100, usize::MAX)
        .unwrap()
        .unwrap();
    root.start_terminal(&mut service, &release).unwrap();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalAwaitingAdoption
    );
    assert_eq!(
        service.storage_usage().unwrap().unwrap().pending_messages,
        1
    );
    assert_eq!(root.adopt_terminal(&mut service).unwrap(), 1);
    assert_eq!(
        service.storage_usage().unwrap().unwrap().pending_messages,
        0
    );
    let store =
        MmpCustodyStore::open_existing(&fixture.path, "original-session", Default::default())
            .unwrap();
    let history = store
        .fold_custody_history(mez_agent::messaging::MessageAcceptanceDecodeLimits {
            payload_bytes: 1024 * 1024,
            auxiliary_bytes: 512 * 1024,
            recipient_count: 4096,
            recipient_bytes: 1024 * 1024,
        })
        .unwrap();
    assert_eq!(
        history.terminal_observations[0].reason,
        DeliveryTerminalReason::RecipientRetired
    );
    assert_eq!(
        history.terminal_observations[0].release_observed_at_ms,
        Some(600100)
    );
    assert!(history.acknowledgements.is_empty());
}

/// A real SQL effect attempts to erase provenance while inserting d1. The
/// original transaction must roll back its entire batch, retaining the same
/// first plan and source. Removing the fault permits only that exact retry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_root_rolls_back_provenance_damage_and_retries_original_plan() {
    let fixture = Fixture::new(true);
    let mut scene = Scene::guarded(&fixture).await;
    drop_offer(&mut scene);
    end(&mut scene).await;
    let Scene {
        root,
        mut service,
        anchor,
        transcript,
        ..
    } = scene;
    root.start_native_input(&service, anchor, transcript, None)
        .unwrap();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::NativeInputAwaitingAdoption
    );
    let MessageNativeInputObservation::RecipientEnded(ended) =
        root.adopt_native_input(&service).unwrap()
    else {
        panic!("actual end required");
    };
    let before = fixture.disk();
    fixture.connection().execute_batch("CREATE TRIGGER damage_retirement AFTER INSERT ON records WHEN NEW.key LIKE 'd1:%' BEGIN DELETE FROM records WHERE key LIKE 'b1:%'; END;").unwrap();
    let ticket = service
        .prepare_storage_native_retirement(root.storage_scope(), &ended, 150, usize::MAX)
        .unwrap()
        .unwrap();
    root.start_terminal(&mut service, &ticket).unwrap();
    let CustodyWorkerStatus::TerminalRetryAt(due) = root.observe().await.unwrap() else {
        panic!("rolled-back original write must retain retry");
    };
    assert_eq!(fixture.disk(), before);
    assert_eq!(retirement_count(&fixture), 0);
    assert_eq!(
        service.storage_usage().unwrap().unwrap().pending_messages,
        1
    );
    fixture
        .connection()
        .execute_batch("DROP TRIGGER damage_retirement")
        .unwrap();
    assert!(root.retry_terminal(&mut service, due).unwrap());
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalAwaitingAdoption
    );
    assert_eq!(root.adopt_terminal(&mut service).unwrap(), 1);
    assert_eq!(retirement_count(&fixture), 1);
}
