//! Real atomic payload retirement and old-checkpoint cold reconstruction.

use super::*;
use mez_agent::messaging::{MessageNativeIncarnation, MessageServiceSnapshot};
use sha2::{Digest, Sha256};

type Rows = Vec<(String, Vec<u8>, Vec<u8>)>;

/// Two original sources share one cumulative positive ACK. Retiring them in
/// separate atomic operations must preserve that journal and the other source's
/// verification, then restore only the genuinely pending third occurrence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acknowledged_reclamation_shared_ack_survives_both_source_rewrites() {
    let fixture = Fixture::new(true);
    let root = fixture.root();
    root.observe().await.unwrap();
    let mut service = MessageService::with_limits(1, 4 * 1024 * 1024);
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    root.configure_payload_spill(&mut service, Default::default())
        .unwrap();
    service.subscribe(&target.agent_id).unwrap();
    let mut checkpoint = None;
    for sequence in 1..=3 {
        let ticket = service
            .prepare_acceptance(
                &sender.agent_id,
                envelope(&sender, &target, &format!("shared-{sequence}")),
                MessageScope::Session,
                100,
                4096,
                None,
            )
            .unwrap();
        root.start(&mut service, &ticket, None).unwrap();
        root.observe().await.unwrap();
        root.adopt(&mut service, None).unwrap();
        if sequence == 2 {
            root.start_checkpoint(&service).unwrap();
            root.observe().await.unwrap();
            let (snapshot, lease) = root.adopt_checkpoint().unwrap();
            checkpoint = Some(snapshot);
            drop(lease);
            assert_eq!(
                service
                    .receive_subscribed(&target.agent_id, 101, 16)
                    .unwrap()
                    .messages
                    .len(),
                2
            );
            let ack = service
                .prepare_acknowledgement(&target.agent_id, ticket.sequence(), 512 * 1024)
                .unwrap()
                .unwrap();
            root.start_acknowledgement(&mut service, &ack).unwrap();
            root.observe().await.unwrap();
            root.adopt_acknowledgement(&mut service).unwrap();
        }
    }
    let journal = rows(&fixture, "key GLOB 'k1:*'");
    assert_eq!(journal.len(), 1);
    for sequence in 1..=2 {
        root.start_acknowledged_source_reclamation(&service, sequence, 128 * 1024, 256 * 1024)
            .unwrap();
        root.observe().await.unwrap();
        let candidate = root
            .finish_retirement_inspection(&service)
            .unwrap()
            .unwrap();
        assert_eq!(candidate.sources().len(), 2);
        assert_eq!(candidate.sources()[0].shared_acknowledgements, 1);
        drop(candidate);
        assert_eq!(rows(&fixture, "key GLOB 'k1:*'"), journal);
    }
    assert_eq!(rows(&fixture, "key GLOB 'a1:*:retired'").len(), 2);
    drop((root, service));
    wait_until_unlocked(&fixture.path).await;
    let root = recover(&fixture, checkpoint.unwrap());
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::RecoveryAwaitingAdoption
    );
    let recovered = root.adopt_recovery().unwrap().into_service();
    assert_eq!(
        recovered.storage_usage().unwrap().unwrap().pending_messages,
        1
    );
    root.start_checkpoint(&recovered).unwrap();
    root.observe().await.unwrap();
    let (snapshot, lease) = root.adopt_checkpoint().unwrap();
    drop(lease);
    let sources = snapshot.storage_snapshot.unwrap().sources;
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].sequence, 3);
    drop(recovered);
    cleanup(&fixture, root).await;
}

fn rows(fixture: &Fixture, predicate: &str) -> Rows {
    let connection = fixture.connection();
    let mut query = connection
        .prepare(&format!(
            "SELECT key,value,digest FROM records WHERE {predicate} ORDER BY key"
        ))
        .unwrap();
    query
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// Capture an actual checkpoint while the first original is still pending,
/// then ACK it and evict its replay owner by accepting another original.
async fn setup(
    fixture: &Fixture,
    native: bool,
    large: bool,
) -> (MmpCustodyRoot, MessageService, MessageServiceSnapshot) {
    let root = fixture.root();
    root.observe().await.unwrap();
    let mut service = MessageService::with_limits(1, 4 * 1024 * 1024);
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    root.configure_payload_spill(&mut service, Default::default())
        .unwrap();
    if native {
        let registration = service
            .capture_sender_registration(&target.agent_id)
            .unwrap();
        service
            .bind_native_conversation(
                &registration,
                "native-target",
                &MessageNativeIncarnation::default(),
            )
            .unwrap();
    }
    service.subscribe(&target.agent_id).unwrap();
    let mut checkpoint = None;
    for first in [true, false] {
        let mut input = envelope(
            &sender,
            &target,
            if first {
                "retire-original"
            } else {
                "pending-later"
            },
        );
        if first && large {
            input.payload = "\0".repeat(1024 * 1024);
        }
        let ticket = service
            .prepare_acceptance(
                &sender.agent_id,
                input,
                MessageScope::Session,
                100,
                4096,
                None,
            )
            .unwrap();
        root.start(&mut service, &ticket, None).unwrap();
        root.observe().await.unwrap();
        root.adopt(&mut service, None).unwrap();
        if first {
            root.start_checkpoint(&service).unwrap();
            root.observe().await.unwrap();
            let (snapshot, lease) = root.adopt_checkpoint().unwrap();
            checkpoint = Some(snapshot);
            drop(lease);
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
    (root, service, checkpoint.unwrap())
}

fn recover(fixture: &Fixture, checkpoint: MessageServiceSnapshot) -> MmpCustodyRoot {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acknowledged_reclamation_removes_payload_and_recovers_old_checkpoint() {
    let fixture = Fixture::new(true);
    let (root, service, checkpoint) = setup(&fixture, false, false).await;
    let journal = rows(&fixture, "key NOT GLOB 'a1:0000000000000001:*'");
    let before = fixture.disk();
    let usage = service.storage_usage().unwrap();
    root.start_acknowledged_source_reclamation(&service, 1, 128 * 1024, 256 * 1024)
        .unwrap();
    root.observe().await.unwrap();
    let candidate = root
        .finish_retirement_inspection(&service)
        .unwrap()
        .unwrap();
    assert_eq!(candidate.sources()[0].acknowledged_recipients, 1);
    let original = rows(&fixture, "key GLOB 'a1:0000000000000001:*'");
    assert_eq!(original.len(), 1);
    assert!(original[0].0.ends_with(":retired"));
    assert!(original[0].1.len() < 4096);
    assert_eq!(
        rows(&fixture, "key NOT GLOB 'a1:0000000000000001:*'"),
        journal
    );
    assert_eq!(fixture.disk().0, before.0 + 1);
    drop(candidate);
    assert_eq!(service.storage_usage().unwrap(), usage);
    let disk = fixture.disk();
    root.start_acknowledged_source_reclamation(&service, 1, 128 * 1024, 256 * 1024)
        .unwrap();
    root.observe().await.unwrap();
    assert!(
        root.finish_retirement_inspection(&service)
            .unwrap()
            .is_none()
    );
    assert_eq!(fixture.disk(), disk);
    drop((root, service));
    wait_until_unlocked(&fixture.path).await;
    let root = recover(&fixture, checkpoint);
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::RecoveryAwaitingAdoption
    );
    let recovered = root.adopt_recovery().unwrap().into_service();
    assert_eq!(
        recovered.storage_usage().unwrap().unwrap().pending_messages,
        1
    );
    root.start_checkpoint(&recovered).unwrap();
    root.observe().await.unwrap();
    let (snapshot, lease) = root.adopt_checkpoint().unwrap();
    drop(lease);
    let stored = snapshot.storage_snapshot.unwrap();
    assert_eq!(stored.sources.len(), 1);
    assert_eq!(stored.sources[0].sequence, 2);
    assert_eq!(fixture.disk(), disk);
    drop(recovered);
    cleanup(&fixture, root).await;
}

/// Exact SQL rollback preserves every original after a mid-delete refusal;
/// removing the fixture trigger permits explicit repair without another send.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acknowledged_reclamation_delete_refusal_rolls_back_and_recovers() {
    let fixture = Fixture::new(true);
    let (root, service, _) = setup(&fixture, false, false).await;
    let before = rows(&fixture, "1");
    let disk = fixture.disk();
    let connection = fixture.connection();
    connection.execute_batch("CREATE TRIGGER refuse_retire BEFORE DELETE ON records WHEN OLD.key='a1:0000000000000001:chunk:00000001' BEGIN SELECT RAISE(ABORT,'fixture retirement refusal'); END;").unwrap();
    root.start_acknowledged_source_reclamation(&service, 1, 128 * 1024, 256 * 1024)
        .unwrap();
    root.observe().await.unwrap();
    assert!(root.finish_retirement_inspection(&service).is_err());
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::Unavailable
    );
    assert_eq!(rows(&fixture, "1"), before);
    assert_eq!(fixture.disk(), disk);
    connection
        .execute_batch("DROP TRIGGER refuse_retire")
        .unwrap();
    root.start_acknowledged_source_reclamation(&service, 1, 128 * 1024, 256 * 1024)
        .unwrap();
    root.observe().await.unwrap();
    drop(
        root.finish_retirement_inspection(&service)
            .unwrap()
            .unwrap(),
    );
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    drop(connection);
    cleanup(&fixture, root).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acknowledged_reclamation_native_and_large_sources_refuse() {
    for (native, large) in [(true, false), (false, true)] {
        let fixture = Fixture::new(true);
        let (root, service, _) = setup(&fixture, native, large).await;
        let before = rows(&fixture, "1");
        let disk = fixture.disk();
        root.start_acknowledged_source_reclamation(&service, 1, 128 * 1024, 256 * 1024)
            .unwrap();
        root.observe().await.unwrap();
        assert!(
            root.finish_retirement_inspection(&service)
                .unwrap()
                .is_none()
        );
        assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
        assert_eq!(rows(&fixture, "1"), before);
        assert_eq!(fixture.disk(), disk);
        cleanup(&fixture, root).await;
    }
}

/// The real worker panics after atomic commit. Its original live-position
/// reopen cannot succeed; cold recovery uses the committed retired form once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acknowledged_reclamation_commit_before_worker_loss_is_cold_safe() {
    let fixture = Fixture::new(true);
    let (root, service, checkpoint) = setup(&fixture, false, false).await;
    fixture
        .probe
        .panic_after_commit
        .store(true, Ordering::SeqCst);
    root.start_acknowledged_source_reclamation(&service, 1, 128 * 1024, 256 * 1024)
        .unwrap();
    root.observe().await.unwrap();
    assert!(root.finish_retirement_inspection(&service).is_err());
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::Unavailable
    );
    assert_eq!(rows(&fixture, "key GLOB 'a1:0000000000000001:*'").len(), 1);
    drop((root, service));
    wait_until_unlocked(&fixture.path).await;
    let root = recover(&fixture, checkpoint);
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::RecoveryAwaitingAdoption
    );
    drop(root.adopt_recovery().unwrap().into_service());
    cleanup(&fixture, root).await;
}

/// Live originals are rejected before I/O; insufficient funded evidence or
/// rewrite bytes leaves every original row and the healthy lane unchanged.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acknowledged_reclamation_budget_and_live_refusals_preserve_originals() {
    let fixture = Fixture::new(true);
    let (root, service, _) = setup(&fixture, false, false).await;
    let before = rows(&fixture, "1");
    let disk = fixture.disk();
    let attempts = fixture.probe.attempts.load(Ordering::SeqCst);
    assert!(
        root.start_acknowledged_source_reclamation(&service, 2, 128 * 1024, 256 * 1024)
            .is_err()
    );
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), attempts);
    for (evidence, rewrite) in [(1, 256 * 1024), (128 * 1024, 1)] {
        let usage = service.storage_usage().unwrap();
        root.start_acknowledged_source_reclamation(&service, 1, evidence, rewrite)
            .unwrap();
        root.observe().await.unwrap();
        assert!(
            root.finish_retirement_inspection(&service)
                .unwrap()
                .is_none()
        );
        assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
        assert_eq!(rows(&fixture, "1"), before);
        assert_eq!(fixture.disk(), disk);
        assert_eq!(service.storage_usage().unwrap(), usage);
    }
    cleanup(&fixture, root).await;
}

/// A trigger changes settlement evidence after preflight. Post-write inspection
/// must still roll back payload deletion, even when the replacement row itself
/// is valid and no SQLite operation reported an error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acknowledged_reclamation_trigger_cannot_remove_ack_inside_commit() {
    let fixture = Fixture::new(true);
    let (root, service, _) = setup(&fixture, false, false).await;
    let before = rows(&fixture, "1");
    let disk = fixture.disk();
    let connection = fixture.connection();
    connection.execute_batch("CREATE TRIGGER steal_ack AFTER INSERT ON records WHEN NEW.key='a1:0000000000000001:retired' BEGIN DELETE FROM records WHERE key GLOB 'k1:*'; END;").unwrap();
    root.start_acknowledged_source_reclamation(&service, 1, 128 * 1024, 256 * 1024)
        .unwrap();
    root.observe().await.unwrap();
    assert!(root.finish_retirement_inspection(&service).is_err());
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::Unavailable
    );
    assert_eq!(rows(&fixture, "1"), before);
    assert_eq!(fixture.disk(), disk);
    connection.execute_batch("DROP TRIGGER steal_ack").unwrap();
    root.start_acknowledged_source_reclamation(&service, 1, 128 * 1024, 256 * 1024)
        .unwrap();
    root.observe().await.unwrap();
    drop(
        root.finish_retirement_inspection(&service)
            .unwrap()
            .unwrap(),
    );
    drop(connection);
    cleanup(&fixture, root).await;
}

/// Even an internally valid remaining inventory cannot prove preservation:
/// removing an entire other occurrence or its independent valid ACK must roll
/// back this transaction, leaving the original pending delivery intact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acknowledged_reclamation_collateral_pending_source_deletion_rolls_back() {
    collateral_deletion(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acknowledged_reclamation_collateral_unrelated_journal_deletion_rolls_back() {
    collateral_deletion(true).await;
}

async fn collateral_deletion(journal: bool) {
    let fixture = Fixture::new(true);
    let (root, mut service, _) = setup(&fixture, false, false).await;
    let prior_acknowledgements = rows(&fixture, "key GLOB 'k1:*'");
    let sender = service.register_agent(None, None, "independent-sender", vec![]);
    let target = service.register_agent(None, None, "independent-target", vec![]);
    service.subscribe(&target.agent_id).unwrap();
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &target, "independent-acked"),
            MessageScope::Session,
            102,
            4096,
            None,
        )
        .unwrap();
    assert_eq!(ticket.sequence(), 3);
    root.start(&mut service, &ticket, None).unwrap();
    root.observe().await.unwrap();
    root.adopt(&mut service, None).unwrap();
    service
        .receive_subscribed(&target.agent_id, 103, 16)
        .unwrap();
    let ack = service
        .prepare_acknowledgement(&target.agent_id, 3, 512 * 1024)
        .unwrap()
        .unwrap();
    root.start_acknowledgement(&mut service, &ack).unwrap();
    root.observe().await.unwrap();
    root.adopt_acknowledgement(&mut service).unwrap();
    let acknowledgements = rows(&fixture, "key GLOB 'k1:*'");
    assert_eq!(acknowledgements.len(), 2);
    // Find the independently generated new journal without assuming its hash.
    let independent = acknowledgements
        .iter()
        .find(|(key, _, _)| !prior_acknowledgements.iter().any(|(old, _, _)| old == key))
        .unwrap();
    let before = rows(&fixture, "1");
    let disk = fixture.disk();
    let connection = fixture.connection();
    let deletion = if journal {
        format!("DELETE FROM records WHERE key='{}';", independent.0)
    } else {
        "DELETE FROM records WHERE key GLOB 'a1:0000000000000002:*';".into()
    };
    connection.execute_batch(&format!("CREATE TRIGGER collateral AFTER INSERT ON records WHEN NEW.key='a1:0000000000000001:retired' BEGIN {deletion} END;")).unwrap();
    root.start_acknowledged_source_reclamation(&service, 1, 128 * 1024, 256 * 1024)
        .unwrap();
    root.observe().await.unwrap();
    let refused = root.finish_retirement_inspection(&service).is_err();
    let preserved = rows(&fixture, "1") == before && fixture.disk() == disk;
    connection.execute_batch("DROP TRIGGER collateral").unwrap();
    drop(connection);
    // Settle task-created resources before reporting the desired-behavior RED.
    drop(service);
    cleanup(&fixture, root).await;
    assert!(refused, "collateral transaction must refuse");
    assert!(
        preserved,
        "all original and journal rows must survive rollback"
    );
}

/// A physically retired source is never a missing-payload fallback. Missing ACK,
/// future-version framing, changed physical binding and coexisting chunks each
/// withhold the entire cold service and preserve the damaged evidence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acknowledged_reclamation_cold_damage_refuses_without_empty_mail() {
    for case in 0..4 {
        let fixture = Fixture::new(true);
        let (root, service, checkpoint) = setup(&fixture, false, false).await;
        root.start_acknowledged_source_reclamation(&service, 1, 128 * 1024, 256 * 1024)
            .unwrap();
        root.observe().await.unwrap();
        drop(
            root.finish_retirement_inspection(&service)
                .unwrap()
                .unwrap(),
        );
        drop((root, service));
        wait_until_unlocked(&fixture.path).await;
        let connection = fixture.connection();
        if case == 0 {
            connection
                .execute("DELETE FROM records WHERE key GLOB 'k1:*'", [])
                .unwrap();
        } else if case == 3 {
            let value = b"retained unexpected chunk";
            connection.execute("INSERT INTO records(key,value,digest) VALUES('a1:0000000000000001:chunk:00000000',?1,?2)", rusqlite::params![value.as_slice(), Sha256::digest(value).as_slice()]).unwrap();
        } else {
            let mut value: Vec<u8> = connection
                .query_row(
                    "SELECT value FROM records WHERE key='a1:0000000000000001:retired'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            value[if case == 1 { 7 } else { 40 }] ^= 1;
            connection
                .execute(
                    "UPDATE records SET value=?1,digest=?2 WHERE key='a1:0000000000000001:retired'",
                    rusqlite::params![&value, Sha256::digest(&value).as_slice()],
                )
                .unwrap();
        }
        drop(connection);
        let damaged = rows(&fixture, "1");
        let root = recover(&fixture, checkpoint);
        root.observe().await.unwrap();
        assert!(root.adopt_recovery().is_err());
        assert!(matches!(
            root.observe().await.unwrap(),
            CustodyWorkerStatus::RecoveryRetryAt(_) | CustodyWorkerStatus::Unavailable
        ));
        assert_eq!(rows(&fixture, "1"), damaged);
        cleanup(&fixture, root).await;
    }
}
