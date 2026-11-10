//! Original retired sources through the actual serialized blocking-worker lane.

use super::*;

/// Test-only independent materialization from actual original storage. Both the
/// native-input and sender-handoff boundaries share this preservation oracle;
/// production inspection retains only its fixed-size size/checksum pair.
pub(super) fn assert_source_evidence(
    fixture: &Fixture,
    inspected: &crate::storage::mmp_custody::retirement_inspection::SourceRetirementInspection,
) {
    use mez_agent::messaging::{
        decode_message_acceptance, decode_message_acceptance_metadata,
        decode_message_acceptance_metadata_evidence,
    };
    use sha2::{Digest, Sha256};
    let store =
        MmpCustodyStore::open_existing(&fixture.path, "original-session", Default::default())
            .unwrap();
    let mut original = Vec::new();
    let (digest, size) = store
        .stream_accepted_occurrence(inspected.sequence, &mut original)
        .unwrap();
    assert_eq!(digest, inspected.source_sha256);
    assert_eq!(size, original.len());
    let limits = mez_agent::messaging::MessageAcceptanceDecodeLimits {
        payload_bytes: 1024 * 1024,
        auxiliary_bytes: 512 * 1024,
        recipient_count: 4096,
        recipient_bytes: 512 * 1024,
    };
    let metadata = decode_message_acceptance_metadata(original.as_slice(), limits).unwrap();
    let mut encoded = Vec::new();
    metadata
        .write_metadata_evidence(&mut encoded, &digest, size)
        .unwrap();
    assert_eq!(
        inspected.source_evidence.encoded_bytes,
        encoded.len() as u64
    );
    assert_eq!(
        inspected.source_evidence.sha256,
        <[u8; 32]>::from(Sha256::digest(&encoded))
    );
    let preserved =
        decode_message_acceptance_metadata_evidence(encoded.as_slice(), limits, size).unwrap();
    assert_eq!(preserved.source_sha256(), &digest);
    assert_eq!(preserved.original_encoded_bytes(), size);
    let actual = preserved.metadata();
    assert_eq!(actual.envelope(), metadata.envelope());
    assert_eq!(actual.audience(), metadata.audience());
    assert_eq!(actual.recipient_bindings(), metadata.recipient_bindings());
    assert_eq!(actual.sequence(), metadata.sequence());
    assert_eq!(actual.accepted_at_ms(), metadata.accepted_at_ms());
    assert_eq!(
        actual.sender_registered_after_sequence(),
        metadata.sender_registered_after_sequence()
    );
    assert_eq!(actual.native_sender(), metadata.native_sender());
    assert_eq!(
        actual.custody_clock_domain(),
        metadata.custody_clock_domain()
    );
    for index in 0..metadata.recipient_bindings().len() {
        assert_eq!(
            actual.native_recipient(index),
            metadata.native_recipient(index)
        );
        assert_eq!(
            actual.recipient_registration_namespace(index),
            metadata.recipient_registration_namespace(index)
        );
    }
    assert!(decode_message_acceptance(encoded.as_slice(), limits).is_err());
    assert!(decode_message_acceptance_metadata(encoded.as_slice(), limits).is_err());
}

async fn cleanup(fixture: &Fixture, root: MmpCustodyRoot) {
    drop(root);
    wait_until_unlocked(&fixture.path).await;
    std::fs::remove_dir_all(&fixture.path).unwrap();
    assert!(!fixture.path.exists());
}

pub(super) fn assert_prepared(
    candidate: &crate::storage::mmp_custody::retirement_inspection::SourceRetirementCandidate,
) {
    use sha2::{Digest, Sha256};
    let records = candidate
        .preserved_sources()
        .expect("charged preparation must retain actual bytes");
    assert_eq!(records.len(), candidate.sources().len());
    for (record, source) in records.iter().zip(candidate.sources()) {
        assert_eq!(record.sequence, source.sequence);
        assert_eq!(
            record.encoded().len() as u64,
            source.source_evidence.encoded_bytes
        );
        assert_eq!(
            <[u8; 32]>::from(Sha256::digest(record.encoded())),
            source.source_evidence.sha256
        );
        let limits = mez_agent::messaging::MessageAcceptanceDecodeLimits {
            payload_bytes: 1024 * 1024,
            auxiliary_bytes: 512 * 1024,
            recipient_count: 4096,
            recipient_bytes: 512 * 1024,
        };
        let evidence = mez_agent::messaging::decode_message_acceptance_metadata_evidence(
            record.encoded(),
            limits,
            64 * 1024 * 1024,
        )
        .unwrap();
        assert_eq!(evidence.source_sha256(), &source.source_sha256);
        assert_eq!(evidence.metadata().sequence(), source.sequence);
    }
}

/// Invalid or exhausted rewrite reservations precede I/O. Returned preparation
/// retains the actual root lock and both charges until its last owner drops.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_rewrite_preparation_reserves_before_io_and_retains_root() {
    let fixture = Fixture::new(true);
    let (root, service, sequence) = retired(&fixture).await;
    let before = service.storage_usage().unwrap();
    let disk = fixture.disk();
    let attempts = fixture.probe.attempts.load(Ordering::SeqCst);
    for bytes in [0, 8 * 1024 * 1024 + 1, usize::MAX] {
        assert!(
            root.start_retirement_rewrite_preparation(&service, sequence, 64 * 1024, bytes)
                .is_err()
        );
        assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), attempts);
        assert_eq!(service.storage_usage().unwrap(), before);
    }
    root.start_retirement_rewrite_preparation(&service, sequence, 1, 8 * 1024 * 1024)
        .unwrap();
    root.observe().await.unwrap();
    assert!(
        root.finish_retirement_inspection(&service)
            .unwrap()
            .is_none()
    );
    assert_eq!(service.storage_usage().unwrap(), before);
    root.start_retirement_rewrite_preparation(&service, sequence, 64 * 1024, 8 * 1024 * 1024)
        .unwrap();
    root.observe().await.unwrap();
    let prepared = root
        .finish_retirement_inspection(&service)
        .unwrap()
        .unwrap();
    assert_prepared(&prepared);
    assert_eq!(prepared.reserved_rewrite_bytes(), 8 * 1024 * 1024);
    let held = service.storage_usage().unwrap();
    let attempts = fixture.probe.attempts.load(Ordering::SeqCst);
    assert!(
        root.clone()
            .start_retirement_rewrite_preparation(&service, sequence, 1024, 1)
            .is_err()
    );
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), attempts);
    assert_eq!(service.storage_usage().unwrap(), held);
    assert_eq!(fixture.disk(), disk);
    drop(root);
    assert!(RootOwnership::acquire_existing(&fixture.path).is_err());
    drop(prepared);
    wait_until_unlocked(&fixture.path).await;
    assert_eq!(service.storage_usage().unwrap(), before);
    std::fs::remove_dir_all(&fixture.path).unwrap();
    assert!(!fixture.path.exists());
}

/// An actual lost read result retains root escrow until disposition. A fresh
/// explicit preparation after exact gated reopen can reserve the full pool.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_rewrite_preparation_worker_loss_releases_only_after_join() {
    let fixture = Fixture::new(true);
    let (root, service, sequence) = retired(&fixture).await;
    let before = service.storage_usage().unwrap();
    fixture
        .probe
        .panic_after_commit
        .store(true, Ordering::SeqCst);
    root.start_retirement_rewrite_preparation(&service, sequence, 64 * 1024, 8 * 1024 * 1024)
        .unwrap();
    root.observe().await.unwrap();
    assert!(root.failure().unwrap().is_some());
    assert!(
        root.start_retirement_rewrite_preparation(&service, sequence, 1024, 1)
            .is_err()
    );
    assert!(root.finish_retirement_inspection(&service).is_err());
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::Unavailable
    );
    assert_eq!(service.storage_usage().unwrap(), before);
    root.start_retirement_rewrite_preparation(&service, sequence, 64 * 1024, 8 * 1024 * 1024)
        .unwrap();
    root.observe().await.unwrap();
    let prepared = root
        .finish_retirement_inspection(&service)
        .unwrap()
        .unwrap();
    assert_eq!(prepared.reserved_rewrite_bytes(), 8 * 1024 * 1024);
    assert_prepared(&prepared);
    drop(prepared);
    assert_eq!(service.storage_usage().unwrap(), before);
    cleanup(&fixture, root).await;
}

/// Physical qualification survives a real worker panic and exact gated reopen.
/// A later layout violation is an error, not the initial typed layout refusal;
/// removing only the fixture-owned violation permits explicit repair.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_rewrite_qualified_policy_survives_worker_loss_and_reopen() {
    use std::os::unix::fs::DirBuilderExt;

    let fixture = Fixture::new(true);
    let (root, service, sequence) = retired(&fixture).await;
    let before = service.storage_usage().unwrap();
    fixture
        .probe
        .panic_after_commit
        .store(true, Ordering::SeqCst);
    root.start_retirement_rewrite_preparation(&service, sequence, 64 * 1024, 8 * 1024 * 1024)
        .unwrap();
    root.observe().await.unwrap();
    assert!(root.finish_retirement_inspection(&service).is_err());
    root.observe().await.unwrap();
    assert_eq!(service.storage_usage().unwrap(), before);

    let nested = fixture.path.join("late-canonical");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&nested)
        .unwrap();
    root.start_retirement_rewrite_preparation(&service, sequence, 64 * 1024, 8 * 1024 * 1024)
        .unwrap();
    root.observe().await.unwrap();
    assert!(root.finish_retirement_inspection(&service).is_err());
    // The invalid qualified root cannot even reopen as an ordinary healthy
    // root. It must retain its error, with no candidate or source mutation.
    root.observe().await.unwrap();
    assert!(root.failure().unwrap().is_some());
    assert_eq!(service.storage_usage().unwrap(), before);
    assert!(nested.is_dir());
    std::fs::remove_dir(&nested).unwrap();
    cleanup(&fixture, root).await;
}

/// Unsupported private layout and insufficient physical envelope are expected
/// pre-analysis refusals, not evidence that a healthy original became corrupt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_rewrite_preparation_layout_refusal_keeps_ordinary_lane_usable() {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    for nested in [true, false] {
        let fixture = Fixture::new(true);
        let (root, service, sequence) = retired(&fixture).await;
        let before = service.storage_usage().unwrap();
        let disk = fixture.disk();
        let directory = fixture.path.join("canonical");
        let path = if nested {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&directory)
                .unwrap();
            directory.join("retained")
        } else {
            fixture.path.join("retained")
        };
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        if nested {
            std::fs::write(&path, b"original canonical data").unwrap();
        } else {
            file.set_len(16 * 1024 * 1024).unwrap();
        }
        drop(file);
        root.start_retirement_rewrite_preparation(&service, sequence, 64 * 1024, 8 * 1024 * 1024)
            .unwrap();
        root.observe().await.unwrap();
        assert!(
            root.finish_retirement_inspection(&service)
                .unwrap()
                .is_none()
        );
        assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
        assert_eq!(service.storage_usage().unwrap(), before);
        if nested {
            assert_eq!(std::fs::read(&path).unwrap(), b"original canonical data");
        } else {
            assert_eq!(std::fs::metadata(&path).unwrap().len(), 16 * 1024 * 1024);
        }
        root.start_retirement_inspection(&service, sequence)
            .unwrap();
        root.observe().await.unwrap();
        let candidate = root
            .finish_retirement_inspection(&service)
            .unwrap()
            .unwrap();
        drop(candidate);
        assert_eq!(fixture.disk(), disk);
        std::fs::remove_file(&path).unwrap();
        if nested {
            std::fs::remove_dir(&directory).unwrap();
        }
        root.start_retirement_rewrite_preparation(&service, sequence, 64 * 1024, 8 * 1024 * 1024)
            .unwrap();
        root.observe().await.unwrap();
        let candidate = root
            .finish_retirement_inspection(&service)
            .unwrap()
            .unwrap();
        assert_eq!(candidate.reserved_rewrite_bytes(), 8 * 1024 * 1024);
        drop(candidate);
        cleanup(&fixture, root).await;
    }
}

/// Reservation refusal precedes I/O. Insufficient encoded space returns no
/// partial record but leaves a healthy root usable; an exact ceiling succeeds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_preservation_worker_reserves_before_io_and_refuses_partial_output() {
    let fixture = Fixture::new(true);
    let (root, service, sequence) = retired(&fixture).await;
    let before = service.storage_usage().unwrap();
    let disk = fixture.disk();
    let attempts = fixture.probe.attempts.load(Ordering::SeqCst);
    for limit in [0, 8 * 1024 * 1024, usize::MAX] {
        assert!(
            root.start_retirement_preservation(&service, sequence, limit)
                .is_err()
        );
        assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), attempts);
        assert_eq!(service.storage_usage().unwrap(), before);
    }
    root.start_retirement_preservation(&service, sequence, 1)
        .unwrap();
    root.observe().await.unwrap();
    assert!(
        root.finish_retirement_inspection(&service)
            .unwrap()
            .is_none()
    );
    assert_eq!(service.storage_usage().unwrap(), before);
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    root.start_retirement_preservation(&service, sequence, 64 * 1024)
        .unwrap();
    root.observe().await.unwrap();
    let prepared = root
        .finish_retirement_inspection(&service)
        .unwrap()
        .unwrap();
    assert_prepared(&prepared);
    let exact = prepared.preserved_sources().unwrap()[0].encoded().len();
    let commitment = prepared.commitment();
    drop(prepared);
    assert_eq!(service.storage_usage().unwrap(), before);
    root.start_retirement_preservation(&service, sequence, exact)
        .unwrap();
    root.observe().await.unwrap();
    let prepared = root
        .finish_retirement_inspection(&service)
        .unwrap()
        .unwrap();
    assert_prepared(&prepared);
    assert_eq!(prepared.commitment(), commitment);
    drop(prepared);
    assert_eq!(service.storage_usage().unwrap(), before);
    assert_eq!(fixture.disk(), disk);
    cleanup(&fixture, root).await;
}

/// Cancelling the observer and Stop cannot discard staged bytes or their actual
/// reservation. The returned result owns both until its last drop.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_preservation_worker_keeps_reservation_across_cancel_and_stop() {
    let fixture = Fixture::new(true);
    let (root, service, sequence) = retired(&fixture).await;
    let before = service.storage_usage().unwrap();
    *fixture.probe.gate.0.lock().unwrap() = false;
    root.start_retirement_rewrite_preparation(&service, sequence, 64 * 1024, 8 * 1024 * 1024)
        .unwrap();
    fixture.probe.started.notified().await;
    let captured = service.storage_usage().unwrap();
    assert_ne!(captured, before);
    let observing = root.clone();
    let observer = tokio::spawn(async move { observing.observe().await });
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    assert_eq!(service.storage_usage().unwrap(), captured);
    assert!(root.start_checkpoint(&service).is_err());
    root.request_stop();
    fixture.probe.release();
    root.observe().await.unwrap();
    let prepared = root
        .finish_retirement_inspection(&service)
        .unwrap()
        .unwrap();
    assert_prepared(&prepared);
    assert_eq!(service.storage_usage().unwrap(), captured);
    assert_eq!(prepared.reserved_rewrite_bytes(), 8 * 1024 * 1024);
    assert!(
        root.start_retirement_preservation(&service, sequence, 1024)
            .is_err()
    );
    drop(prepared);
    assert_eq!(service.storage_usage().unwrap(), before);
    cleanup(&fixture, root).await;
}

/// Corrupt ACK evidence is encountered after the source has been encoded. Even
/// an insufficient output budget must not downgrade that failure to None, nor
/// can a later budget refusal clear the retained original repair gate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_preservation_worker_quarantines_encoded_bytes_on_corruption() {
    for limit in [1, 64 * 1024] {
        let fixture = Fixture::new(true);
        let (root, service, sequence) = retired(&fixture).await;
        let before = service.storage_usage().unwrap();
        let disk = fixture.disk();
        let connection = fixture.connection();
        let (key, value): (String, Vec<u8>) = connection
            .query_row(
                "SELECT key,value FROM records WHERE key LIKE 'k1:%' LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        connection
            .execute("UPDATE records SET value=x'00' WHERE key=?1", [&key])
            .unwrap();
        root.start_retirement_preservation(&service, sequence, limit)
            .unwrap();
        root.observe().await.unwrap();
        assert!(root.finish_retirement_inspection(&service).is_err());
        assert_eq!(service.storage_usage().unwrap(), before);
        assert_eq!(
            root.observe().await.unwrap(),
            CustodyWorkerStatus::Unavailable
        );
        assert!(root.start_checkpoint(&service).is_err());
        connection
            .execute(
                "UPDATE records SET value=?1 WHERE key=?2",
                rusqlite::params![value, key],
            )
            .unwrap();
        root.start_retirement_preservation(&service, sequence, 1)
            .unwrap();
        root.observe().await.unwrap();
        assert!(root.finish_retirement_inspection(&service).is_err());
        assert_eq!(
            root.observe().await.unwrap(),
            CustodyWorkerStatus::Unavailable
        );
        root.start_retirement_preservation(&service, sequence, 64 * 1024)
            .unwrap();
        root.observe().await.unwrap();
        let prepared = root
            .finish_retirement_inspection(&service)
            .unwrap()
            .unwrap();
        assert_prepared(&prepared);
        drop(prepared);
        assert_eq!(service.storage_usage().unwrap(), before);
        assert_eq!(fixture.disk(), disk);
        drop(connection);
        cleanup(&fixture, root).await;
    }
}

/// The real read thread loses its completed encoded result. Root escrow keeps
/// the reservation until actual join/disposition; reopen alone grants no output
/// and performs no replay. Only a fresh explicit read repairs the same source.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_preservation_worker_loss_retains_budget_until_disposition() {
    let fixture = Fixture::new(true);
    let (root, service, sequence) = retired(&fixture).await;
    let before = service.storage_usage().unwrap();
    let disk = fixture.disk();
    let attempts = fixture.probe.attempts.load(Ordering::SeqCst);
    fixture
        .probe
        .panic_after_commit
        .store(true, Ordering::SeqCst);
    root.start_retirement_preservation(&service, sequence, 64 * 1024)
        .unwrap();
    root.observe().await.unwrap();
    assert!(root.failure().unwrap().is_some());
    assert_ne!(service.storage_usage().unwrap(), before);
    assert!(root.finish_retirement_inspection(&service).is_err());
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::Unavailable
    );
    assert_eq!(service.storage_usage().unwrap(), before);
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), attempts + 1);
    assert!(root.start_checkpoint(&service).is_err());
    root.start_retirement_preservation(&service, sequence, 64 * 1024)
        .unwrap();
    root.observe().await.unwrap();
    let prepared = root
        .finish_retirement_inspection(&service)
        .unwrap()
        .unwrap();
    assert_prepared(&prepared);
    drop(prepared);
    assert_eq!(service.storage_usage().unwrap(), before);
    assert_eq!(fixture.disk(), disk);
    cleanup(&fixture, root).await;
}

async fn retired(fixture: &Fixture) -> (MmpCustodyRoot, MessageService, u64) {
    let root = fixture.root();
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    let mut service = MessageService::with_limits(1, 4 * 1024 * 1024);
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    root.configure_payload_spill(&mut service, Default::default())
        .unwrap();
    service.subscribe(&target.agent_id).unwrap();
    for id in ["retired", "later"] {
        let ticket = service
            .prepare_acceptance(
                &sender.agent_id,
                envelope(&sender, &target, id),
                MessageScope::Session,
                100,
                4096,
                None,
            )
            .unwrap();
        root.start(&mut service, &ticket, None).unwrap();
        fixture.probe.started.notified().await;
        assert_eq!(
            root.observe().await.unwrap(),
            CustodyWorkerStatus::AwaitingAdoption
        );
        root.adopt(&mut service, None).unwrap();
        if id == "retired" {
            service
                .receive_subscribed(&target.agent_id, 101, 16)
                .unwrap();
            let ack = service
                .prepare_acknowledgement(&target.agent_id, ticket.sequence(), 512 * 1024)
                .unwrap()
                .unwrap();
            root.start_acknowledgement(&mut service, &ack).unwrap();
            assert_eq!(
                root.observe().await.unwrap(),
                CustodyWorkerStatus::AcknowledgementAwaitingAdoption
            );
            root.adopt_acknowledgement(&mut service).unwrap();
        }
    }
    (root, service, 1)
}

/// Aborting an observer cannot free the lane or the charged lower capture.
/// Stop denies new analysis but permits this original result to settle once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_inspection_worker_retains_capture_across_observer_cancel_and_stop() {
    let fixture = Fixture::new(true);
    let (root, service, sequence) = retired(&fixture).await;
    let before = service.storage_usage().unwrap();
    let disk = fixture.disk();
    *fixture.probe.gate.0.lock().unwrap() = false;
    root.start_retirement_inspection(&service, sequence)
        .unwrap();
    fixture.probe.started.notified().await;
    let observing = root.clone();
    let observer = tokio::spawn(async move { observing.observe().await });
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    assert!(
        root.start_retirement_inspection(&service, sequence)
            .is_err()
    );
    assert!(root.start_checkpoint(&service).is_err());
    tokio::task::yield_now().await; // unrelated async work remains schedulable
    root.request_stop();
    fixture.probe.release();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::RetirementInspectionAwaitingAdoption
    );
    let candidate = root
        .finish_retirement_inspection(&service)
        .unwrap()
        .unwrap();
    assert_eq!(candidate.sources().len(), 1);
    assert_eq!(candidate.sources()[0].sequence, sequence);
    assert_eq!(candidate.sources()[0].unsettled_recipients, 0);
    assert!(candidate.is_original(&service));
    assert_ne!(service.storage_usage().unwrap(), before);
    assert!(root.finish_retirement_inspection(&service).is_err());
    assert!(
        root.start_retirement_inspection(&service, sequence)
            .is_err()
    );
    drop(candidate);
    assert_eq!(service.storage_usage().unwrap(), before);
    assert_eq!(fixture.disk(), disk);
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    cleanup(&fixture, root).await;
}

/// Stale adoption is refused even for matching labels. Invalid/live seeds and
/// foreign services do not start a physical worker or consume its idle lane.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_inspection_worker_fences_live_seeds_and_replaced_lower_service() {
    let fixture = Fixture::new(true);
    let (root, service, sequence) = retired(&fixture).await;
    let attempts = fixture.probe.attempts.load(Ordering::SeqCst);
    for seed in [0, sequence + 1, sequence + 2] {
        assert!(root.start_retirement_inspection(&service, seed).is_err());
    }
    let foreign = MessageService::default();
    assert!(
        root.start_retirement_inspection(&foreign, sequence)
            .is_err()
    );
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), attempts);
    root.start_retirement_inspection(&service, sequence)
        .unwrap();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::RetirementInspectionAwaitingAdoption
    );
    assert!(root.finish_retirement_inspection(&foreign).is_err());
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    root.start_retirement_inspection(&service, sequence)
        .unwrap();
    root.observe().await.unwrap();
    root.finish_retirement_inspection(&service).unwrap();
    cleanup(&fixture, root).await;
}

/// A failed scan returns the connection without an automatic rescan. Restoring
/// the original source permits an explicit fresh read; no send/ACK is replayed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_inspection_worker_contains_read_refusal_and_recovers_original_source() {
    let fixture = Fixture::new(true);
    let (root, service, sequence) = retired(&fixture).await;
    let connection = fixture.connection();
    let key = "a1:0000000000000001:chunk:00000000";
    let bytes: Vec<u8> = connection
        .query_row("SELECT value FROM records WHERE key=?1", [key], |r| {
            r.get(0)
        })
        .unwrap();
    connection
        .execute("UPDATE records SET value=X'00' WHERE key=?1", [key])
        .unwrap();
    let disk = fixture.disk();
    root.start_retirement_inspection(&service, sequence)
        .unwrap();
    root.observe().await.unwrap();
    assert!(root.failure().unwrap().is_some());
    assert!(root.finish_retirement_inspection(&service).is_err());
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::Unavailable
    );
    assert!(root.start_checkpoint(&service).is_err());
    connection
        .execute(
            "UPDATE records SET value=?1 WHERE key=?2",
            rusqlite::params![bytes, key],
        )
        .unwrap();
    root.start_retirement_inspection(&service, sequence)
        .unwrap();
    root.observe().await.unwrap();
    assert_eq!(
        root.finish_retirement_inspection(&service)
            .unwrap()
            .unwrap()
            .sources()
            .len(),
        1
    );
    assert_eq!(fixture.disk(), disk);
    drop(connection);
    cleanup(&fixture, root).await;
}

/// A real thread panic is joined before the sole exact-position reopen. It
/// neither repeats the scan nor changes original acceptance/ACK evidence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_inspection_worker_loss_reopens_once_without_replaying_analysis() {
    for changed_position in [false, true] {
        let fixture = Fixture::new(true);
        let (root, service, sequence) = retired(&fixture).await;
        let attempts = fixture.probe.attempts.load(Ordering::SeqCst);
        let disk = fixture.disk();
        fixture
            .probe
            .panic_after_commit
            .store(true, Ordering::SeqCst);
        root.start_retirement_inspection(&service, sequence)
            .unwrap();
        assert_eq!(
            root.observe().await.unwrap(),
            CustodyWorkerStatus::RetirementInspectionAwaitingAdoption
        );
        assert!(root.failure().unwrap().is_some());
        assert!(
            root.start_retirement_inspection(&service, sequence)
                .is_err()
        );
        if changed_position {
            fixture
                .connection()
                .execute("UPDATE state SET generation=generation+1", [])
                .unwrap();
        }
        assert!(root.finish_retirement_inspection(&service).is_err());
        assert_eq!(
            root.observe().await.unwrap(),
            CustodyWorkerStatus::Unavailable
        );
        assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), attempts + 1);
        if !changed_position {
            assert_eq!(fixture.disk(), disk);
            root.start_retirement_inspection(&service, sequence)
                .unwrap();
            root.observe().await.unwrap();
            root.finish_retirement_inspection(&service).unwrap();
        }
        cleanup(&fixture, root).await;
    }
}

/// A valid shared ACK can depend on a replay-retained source. This is a candidate
/// refusal, not corruption: the validated connection remains usable, then the
/// same original can be inspected after a third accepted source evicts it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_inspection_worker_valid_dependency_refusal_keeps_lane_usable() {
    let fixture = Fixture::new(true);
    let root = fixture.root();
    root.observe().await.unwrap();
    let mut service = MessageService::with_limits(1, 4 * 1024 * 1024);
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    root.configure_payload_spill(&mut service, Default::default())
        .unwrap();
    service.subscribe(&target.agent_id).unwrap();
    for id in 0..2 {
        let ticket = service
            .prepare_acceptance(
                &sender.agent_id,
                envelope(&sender, &target, &format!("shared-{id}")),
                MessageScope::Session,
                100,
                4096,
                None,
            )
            .unwrap();
        root.start(&mut service, &ticket, None).unwrap();
        root.observe().await.unwrap();
        root.adopt(&mut service, None).unwrap();
        if id == 1 {
            service
                .receive_subscribed(&target.agent_id, 101, 16)
                .unwrap();
            let ack = service
                .prepare_acknowledgement(&target.agent_id, ticket.sequence(), 512 * 1024)
                .unwrap()
                .unwrap();
            root.start_acknowledgement(&mut service, &ack).unwrap();
            root.observe().await.unwrap();
            root.adopt_acknowledgement(&mut service).unwrap();
        }
    }
    root.start_retirement_inspection(&service, 1).unwrap();
    root.observe().await.unwrap();
    assert!(
        root.finish_retirement_inspection(&service)
            .unwrap()
            .is_none()
    );
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &target, "shared-2"),
            MessageScope::Session,
            100,
            4096,
            None,
        )
        .unwrap();
    root.start(&mut service, &ticket, None).unwrap();
    root.observe().await.unwrap();
    root.adopt(&mut service, None).unwrap();
    let disk = fixture.disk();
    root.start_retirement_inspection(&service, 1).unwrap();
    root.observe().await.unwrap();
    let candidate = root
        .finish_retirement_inspection(&service)
        .unwrap()
        .unwrap();
    assert_eq!(candidate.sources().len(), 2);
    assert!(
        candidate
            .sources()
            .iter()
            .all(|s| s.unsettled_recipients == 0)
    );
    let encoded_bytes = candidate
        .sources()
        .iter()
        .map(|source| source.source_evidence.encoded_bytes as usize)
        .sum::<usize>();
    let commitment = candidate.commitment();
    drop(candidate);
    let before = service.storage_usage().unwrap();
    // The first record fits; the second exceeds the shared allowance by one.
    root.start_retirement_preservation(&service, 1, encoded_bytes - 1)
        .unwrap();
    root.observe().await.unwrap();
    assert!(
        root.finish_retirement_inspection(&service)
            .unwrap()
            .is_none()
    );
    assert_eq!(service.storage_usage().unwrap(), before);
    root.start_retirement_preservation(&service, 1, encoded_bytes)
        .unwrap();
    root.observe().await.unwrap();
    let prepared = root
        .finish_retirement_inspection(&service)
        .unwrap()
        .unwrap();
    assert_prepared(&prepared);
    assert_eq!(prepared.commitment(), commitment);
    drop(prepared);
    assert_eq!(service.storage_usage().unwrap(), before);
    assert_eq!(fixture.disk(), disk);
    cleanup(&fixture, root).await;
}

/// Individually valid release frames can still violate a cross-record slot
/// invariant. Neither ordinary refusal nor a lost worker may downgrade that
/// integrity failure to Idle; only explicit inspection after repair can do so.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_inspection_worker_semantic_release_overlap_stays_gated() {
    use sha2::{Digest, Sha256};
    let fixture = Fixture::new(true);
    let root = fixture.root();
    root.observe().await.unwrap();
    let mut service = MessageService::with_limits(1, 4 * 1024 * 1024);
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    root.configure_payload_spill(&mut service, Default::default())
        .unwrap();
    let mut original = envelope(&sender, &target, "terminal-original");
    original.ttl_ms = Some(5);
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            original,
            MessageScope::Session,
            10,
            4096,
            None,
        )
        .unwrap();
    root.start(&mut service, &ticket, None).unwrap();
    root.observe().await.unwrap();
    root.adopt(&mut service, None).unwrap();
    drop(ticket);
    for horizon in [false, true] {
        let ticket = if horizon {
            service.prepare_storage_horizon_release(root.storage_scope(), 600016, usize::MAX)
        } else {
            service.prepare_storage_expiry(root.storage_scope(), 16, usize::MAX)
        }
        .unwrap()
        .unwrap();
        root.start_terminal(&mut service, &ticket).unwrap();
        root.observe().await.unwrap();
        root.adopt_terminal(&mut service).unwrap();
    }
    // Subscribe after A so B's later ACK does not share A's terminal source.
    service.subscribe(&target.agent_id).unwrap();
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &target, "later"),
            MessageScope::Session,
            600017,
            4096,
            None,
        )
        .unwrap();
    root.start(&mut service, &ticket, None).unwrap();
    root.observe().await.unwrap();
    root.adopt(&mut service, None).unwrap();
    service
        .receive_subscribed(&target.agent_id, 600018, 16)
        .unwrap();
    let ack = service
        .prepare_acknowledgement(&target.agent_id, 2, 512 * 1024)
        .unwrap()
        .unwrap();
    root.start_acknowledgement(&mut service, &ack).unwrap();
    root.observe().await.unwrap();
    root.adopt_acknowledgement(&mut service).unwrap();
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &target, "independent-later"),
            MessageScope::Session,
            600019,
            4096,
            None,
        )
        .unwrap();
    root.start(&mut service, &ticket, None).unwrap();
    root.observe().await.unwrap();
    root.adopt(&mut service, None).unwrap();
    root.start_retirement_inspection(&service, 2).unwrap();
    root.observe().await.unwrap();
    assert_eq!(
        root.finish_retirement_inspection(&service)
            .unwrap()
            .unwrap()
            .sources()
            .len(),
        1
    );
    service
        .receive_subscribed(&target.agent_id, 600020, 16)
        .unwrap();
    let ack = service
        .prepare_acknowledgement(&target.agent_id, 3, 512 * 1024)
        .unwrap()
        .unwrap();
    root.start_acknowledgement(&mut service, &ack).unwrap();
    root.observe().await.unwrap();
    root.adopt_acknowledgement(&mut service).unwrap();
    root.start_retirement_inspection(&service, 1).unwrap();
    root.observe().await.unwrap();
    let original_digest = root
        .finish_retirement_inspection(&service)
        .unwrap()
        .unwrap()
        .sources()[0]
        .source_sha256;
    let connection = fixture.connection();
    let mut bytes: Vec<u8> = connection
        .query_row(
            "SELECT value FROM records WHERE key LIKE 'r1:%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    bytes[40..48].copy_from_slice(&600017u64.to_le_bytes());
    let mut key_hash = Sha256::new();
    key_hash.update(b"mezzanine-mmp-terminal-release-v1\0");
    key_hash.update(&bytes);
    let key = format!(
        "r1:{}",
        key_hash
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    connection
        .execute(
            "INSERT INTO records(key,value,digest) VALUES(?1,?2,?3)",
            rusqlite::params![key, bytes, Sha256::digest(&bytes).as_slice()],
        )
        .unwrap();
    // Prove this is not a malformed-row/checksum test: the ordinary strict
    // inventory accepts each original frame; the slot accumulator catches overlap.
    {
        let mut state = root.0.state.lock().unwrap();
        let State::Idle(opened) = &mut *state else {
            panic!("idle original store");
        };
        let input = service
            .prepare_source_retirement_inspection(root.storage_scope())
            .unwrap();
        assert_eq!(
            opened
                .store
                .inventory_custody(input.decode_limits())
                .unwrap()
                .release_keys
                .len(),
            2
        );
    }
    let disk = fixture.disk();
    for lose_worker in [false, true] {
        fixture
            .probe
            .panic_after_commit
            .store(lose_worker, Ordering::SeqCst);
        root.start_retirement_inspection(&service, 1).unwrap();
        root.observe().await.unwrap();
        assert!(root.finish_retirement_inspection(&service).is_err());
        assert_eq!(
            root.observe().await.unwrap(),
            CustodyWorkerStatus::Unavailable
        );
        assert!(root.start_checkpoint(&service).is_err());
        assert!(root.start(&mut service, &ticket, None).is_err());
        let attempts = fixture.probe.attempts.load(Ordering::SeqCst);
        assert!(
            root.start_retirement_inspection(&service, 2).is_err(),
            "an unrelated retired source cannot prove repair of A"
        );
        assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), attempts);
        assert_eq!(
            root.observe().await.unwrap(),
            CustodyWorkerStatus::Unavailable
        );
        assert_eq!(fixture.disk(), disk);
    }
    connection
        .execute("DELETE FROM records WHERE key=?1", [&key])
        .unwrap();
    // Construct a valid cumulative ACK with A's expiry exclusion and B/C's
    // actual positive decisions. C remains replay-live. The resulting None is
    // incomplete repair coverage, not permission to erase the retained A gate.
    let read_ack = |endpoint: u64| -> Vec<u8> {
        connection
            .query_row(
                "SELECT value FROM records WHERE key LIKE 'k1:%' AND substr(value,17,8)=?1",
                [endpoint.to_le_bytes().to_vec()],
                |row| row.get(0),
            )
            .unwrap()
    };
    let second = read_ack(2);
    let third = read_ack(3);
    let entries = 48 + u32::from_le_bytes(third[40..44].try_into().unwrap()) as usize;
    let mut partial = third[..entries].to_vec();
    partial[8..16].copy_from_slice(&0u64.to_le_bytes());
    partial[44..48].copy_from_slice(&3u32.to_le_bytes());
    partial.extend_from_slice(&1u64.to_le_bytes());
    partial.extend_from_slice(&0u32.to_le_bytes());
    partial.push(0);
    partial.extend_from_slice(&0u64.to_le_bytes());
    partial.extend_from_slice(&original_digest);
    partial.extend_from_slice(&second[entries..]);
    partial.extend_from_slice(&third[entries..]);
    let mut hash = Sha256::new();
    hash.update(b"mezzanine-mmp-ack-record-v1\0");
    hash.update(&partial);
    let partial_key = format!(
        "k1:{}",
        hash.finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    connection
        .execute(
            "INSERT INTO records(key,value,digest) VALUES(?1,?2,?3)",
            rusqlite::params![partial_key, partial, Sha256::digest(&partial).as_slice()],
        )
        .unwrap();
    root.start_retirement_inspection(&service, 1).unwrap();
    root.observe().await.unwrap();
    {
        let state = root.0.state.lock().unwrap();
        let State::InspectedRetirement {
            result: Ok(written),
            ..
        } = &*state
        else {
            panic!("joined original result required");
        };
        assert!(
            matches!(written.candidate, Ok(None)),
            "valid live dependency must be typed None"
        );
    }
    assert!(root.finish_retirement_inspection(&service).is_err());
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::Unavailable
    );
    assert!(root.start_checkpoint(&service).is_err());
    connection
        .execute("DELETE FROM records WHERE key=?1", [&partial_key])
        .unwrap();
    root.start_retirement_inspection(&service, 1).unwrap();
    root.observe().await.unwrap();
    let candidate = root
        .finish_retirement_inspection(&service)
        .unwrap()
        .unwrap();
    assert_eq!(candidate.sources()[0].unsettled_recipients, 0);
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    drop(candidate);
    drop(connection);
    cleanup(&fixture, root).await;
}

/// Last-root loss transfers the real read handle to existing fallback custody.
/// The OS root lock cannot be reacquired until the gated read actually exits.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_inspection_worker_root_drop_keeps_actual_worker_owned() {
    let fixture = Fixture::new(true);
    let (root, service, sequence) = retired(&fixture).await;
    let before = service.storage_usage().unwrap();
    *fixture.probe.gate.0.lock().unwrap() = false;
    root.start_retirement_preservation(&service, sequence, 64 * 1024)
        .unwrap();
    fixture.probe.started.notified().await;
    drop(root);
    assert!(RootOwnership::acquire_existing(&fixture.path).is_err());
    assert_ne!(service.storage_usage().unwrap(), before);
    fixture.probe.release();
    wait_until_unlocked(&fixture.path).await;
    assert_eq!(service.storage_usage().unwrap(), before);
    std::fs::remove_dir_all(&fixture.path).unwrap();
    assert!(!fixture.path.exists());
}
