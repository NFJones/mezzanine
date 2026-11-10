//! Non-destructive preparation through the real worker and cold-open boundary.

use super::*;
use sha2::{Digest, Sha256};

type Rows = Vec<(String, Vec<u8>, Vec<u8>)>;

/// Bodyless metadata has its own exact byte ceiling, independent of payload or
/// count pressure. The real encoder measures overhead; 128 KiB fits and one
/// more byte refuses without publishing a prefix or changing production caps.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_retirement_preparation_exact_evidence_bytes() {
    let mut base = 0;
    for case in 0..3 {
        let padding = if case == 0 {
            0
        } else {
            128 * 1024 - base + case - 1
        };
        let fixture = Fixture::new(true);
        let root = fixture.root();
        root.observe().await.unwrap();
        let mut service = MessageService::with_limits(1, 4 * 1024 * 1024);
        let sender = service.register_agent(None, None, "sender", vec![]);
        let target = service.register_agent(None, None, "target", vec![]);
        root.configure_payload_spill(&mut service, Default::default())
            .unwrap();
        service.subscribe(&target.agent_id).unwrap();
        for first in [true, false] {
            let mut input = envelope(
                &sender,
                &target,
                if first { "byte-original" } else { "later" },
            );
            input.payload = "small byte payload".into();
            if first {
                input
                    .extension_fields
                    .push(("padding".into(), format!("\"{}\"", "x".repeat(padding))));
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
        root.start_retirement_preservation(&service, 1, 256 * 1024)
            .unwrap();
        root.observe().await.unwrap();
        let candidate = root
            .finish_retirement_inspection(&service)
            .unwrap()
            .unwrap();
        let size = candidate.preserved_sources().unwrap()[0].encoded().len();
        if case == 0 {
            base = size;
        } else {
            assert_eq!(size, 128 * 1024 + case - 1);
        }
        drop(candidate);
        let before = fixture.disk();
        let rows = originals(&fixture);
        root.start_durable_retirement_preparation(&service, 1, 256 * 1024, 256 * 1024)
            .unwrap();
        root.observe().await.unwrap();
        let candidate = root.finish_retirement_inspection(&service).unwrap();
        if case == 2 {
            assert!(candidate.is_none());
            assert_eq!(fixture.disk(), before);
            assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
            cleanup(&fixture, root).await;
        } else {
            assert!(candidate.is_some());
            drop(candidate);
            drop(root);
            wait_until_unlocked(&fixture.path).await;
            let root = fixture.root();
            assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
            assert_eq!(originals(&fixture), rows);
            assert_eq!(fixture.disk(), (before.0 + 2, before.1));
            cleanup(&fixture, root).await;
        }
    }
}

/// Exercise the real single-batch limit: 63 sources plus the manifest fit;
/// the valid 64-member inspection refuses publication without a partial prefix.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_retirement_preparation_exact_source_quantum() {
    for sources in [63, 64] {
        let fixture = Fixture::new(true);
        let root = fixture.root();
        root.observe().await.unwrap();
        let mut service = MessageService::with_limits(1, 4 * 1024 * 1024);
        let sender = service.register_agent(None, None, "sender", vec![]);
        let target = service.register_agent(None, None, "target", vec![]);
        root.configure_payload_spill(&mut service, Default::default())
            .unwrap();
        service.subscribe(&target.agent_id).unwrap();
        for index in 0..=sources {
            let mut input = envelope(&sender, &target, &format!("quantum-{index}"));
            // This is a record-count test, not per-recipient payload pressure.
            input.payload = "small quantum payload".into();
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
            if index + 1 == sources {
                service
                    .receive_subscribed(&target.agent_id, 101, 100)
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
        let before = fixture.disk();
        let rows = originals(&fixture);
        root.start_durable_retirement_preparation(&service, 1, 256 * 1024, 256 * 1024)
            .unwrap();
        root.observe().await.unwrap();
        let candidate = root.finish_retirement_inspection(&service).unwrap();
        if sources == 63 {
            let candidate = candidate.unwrap();
            assert_eq!(candidate.sources().len(), sources);
            assert_eq!(prepared_rows(&fixture), 64);
            drop(candidate);
            drop(root);
            wait_until_unlocked(&fixture.path).await;
            // Remove a member coherently, preserving every surviving row's
            // checksum and the original publication generation/batch digest.
            let connection = fixture.connection();
            let manifest: Vec<u8> = connection
                .query_row(
                    "SELECT value FROM records WHERE key='q1:prepare'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let removed: (String, Vec<u8>, Vec<u8>) = connection.query_row(
                "SELECT key,value,digest FROM records WHERE key GLOB 'q1:*' AND key!='q1:prepare' ORDER BY key DESC LIMIT 1", [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            ).unwrap();
            let mut partial = manifest[..manifest.len() - 8].to_vec();
            partial[104..112].copy_from_slice(&62u64.to_le_bytes());
            connection
                .execute("DELETE FROM records WHERE key=?1", [&removed.0])
                .unwrap();
            connection
                .execute(
                    "UPDATE records SET value=?1,digest=?2 WHERE key='q1:prepare'",
                    rusqlite::params![partial, Sha256::digest(&partial).as_slice()],
                )
                .unwrap();
            let damaged = fixture.disk();
            let root = fixture.root();
            assert_eq!(
                root.observe().await.unwrap(),
                CustodyWorkerStatus::Unavailable
            );
            assert_eq!(fixture.disk(), damaged);
            assert_eq!(prepared_rows(&fixture), 63);
            assert_eq!(originals(&fixture), rows);
            drop(root);
            wait_until_unlocked(&fixture.path).await;
            connection
                .execute(
                    "UPDATE records SET value=?1,digest=?2 WHERE key='q1:prepare'",
                    rusqlite::params![manifest, Sha256::digest(&manifest).as_slice()],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO records(key,value,digest) VALUES(?1,?2,?3)",
                    rusqlite::params![removed.0, removed.1, removed.2],
                )
                .unwrap();
            drop(connection);
            let root = fixture.root();
            assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
            assert_eq!(fixture.disk(), (before.0 + 2, before.1));
            assert_eq!(originals(&fixture), rows);
            cleanup(&fixture, root).await;
        } else {
            assert!(candidate.is_none());
            assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
            assert_eq!(fixture.disk(), before);
            assert_eq!(prepared_rows(&fixture), 0);
            cleanup(&fixture, root).await;
        }
    }
}

/// Refusal is quantitative and precedes writes; a healthy lane remains usable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_retirement_preparation_tiny_allowance_preserves_lane() {
    let fixture = Fixture::new(true);
    let (root, service, sequence) = retired(&fixture).await;
    let before = fixture.disk();
    let usage = service.storage_usage().unwrap();
    for (encoded, physical) in [(1, 64 * 1024), (64 * 1024, 1)] {
        root.start_durable_retirement_preparation(&service, sequence, encoded, physical)
            .unwrap();
        root.observe().await.unwrap();
        assert!(
            root.finish_retirement_inspection(&service)
                .unwrap()
                .is_none()
        );
        assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
        assert_eq!(fixture.disk(), before);
        assert_eq!(service.storage_usage().unwrap(), usage);
    }
    cleanup(&fixture, root).await;
}

/// A copied database retains equal store labels/UUID but not the original
/// physical allocation. It cannot discard the copied preparation evidence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_retirement_preparation_copied_allocation_cannot_cancel() {
    use std::os::unix::fs::DirBuilderExt;
    let fixture = Fixture::new(true);
    let (root, service, sequence) = retired(&fixture).await;
    root.start_durable_retirement_preparation(&service, sequence, 64 * 1024, 64 * 1024)
        .unwrap();
    root.observe().await.unwrap();
    drop(
        root.finish_retirement_inspection(&service)
            .unwrap()
            .unwrap(),
    );
    drop(root);
    wait_until_unlocked(&fixture.path).await;
    let copied = Fixture::new(true);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&copied.path)
        .unwrap();
    std::fs::copy(
        fixture.path.join("custody.sqlite"),
        copied.path.join("custody.sqlite"),
    )
    .unwrap();
    let before = copied.disk();
    let root = copied.root();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::Unavailable
    );
    assert_eq!(copied.disk(), before);
    assert!(prepared_rows(&copied) > 0);
    cleanup(&copied, root).await;
    let root = fixture.root();
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    assert_eq!(prepared_rows(&fixture), 0);
    cleanup(&fixture, root).await;
}

/// Cancellation failure rolls back the whole deletion. Another healthy root
/// can still accept mail; after storage repair the original cancels once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_retirement_preparation_cancel_refusal_then_recovery() {
    let fixture = Fixture::new(true);
    let (root, service, sequence) = retired(&fixture).await;
    root.start_durable_retirement_preparation(&service, sequence, 64 * 1024, 64 * 1024)
        .unwrap();
    root.observe().await.unwrap();
    drop(
        root.finish_retirement_inspection(&service)
            .unwrap()
            .unwrap(),
    );
    drop(root);
    wait_until_unlocked(&fixture.path).await;
    let rows = originals(&fixture);
    let before = fixture.disk();
    let connection = fixture.connection();
    connection.execute_batch("CREATE TRIGGER refuse_cancel BEFORE DELETE ON records WHEN OLD.key GLOB 'q1:*' BEGIN SELECT RAISE(ABORT,'fixture cancellation refusal'); END;").unwrap();
    let root = fixture.root();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::Unavailable
    );
    assert_eq!(fixture.disk(), before);
    assert_eq!(originals(&fixture), rows);
    drop(root);
    wait_until_unlocked(&fixture.path).await;
    let healthy = Fixture::new(true);
    let (healthy_root, _, _) = retired(&healthy).await;
    cleanup(&healthy, healthy_root).await;
    connection
        .execute_batch("DROP TRIGGER refuse_cancel")
        .unwrap();
    drop(connection);
    let root = fixture.root();
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    assert_eq!(fixture.disk(), (before.0 + 1, before.1 - 2));
    assert_eq!(originals(&fixture), rows);
    cleanup(&fixture, root).await;
}

fn originals(fixture: &Fixture) -> Rows {
    let connection = fixture.connection();
    let mut query = connection
        .prepare("SELECT key,value,digest FROM records WHERE key NOT GLOB 'q1:*' ORDER BY key")
        .unwrap();
    query
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn prepared_rows(fixture: &Fixture) -> i64 {
    fixture
        .connection()
        .query_row(
            "SELECT count(*) FROM records WHERE key GLOB 'q1:*'",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

/// Successful publication gates further work. Cold disposition advances only
/// the metadata generation; original rows, allocator and charged ownership stay
/// intact. A second cold open is inert, including after a lost cancellation ACK.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_retirement_preparation_cold_cancellation_preserves_originals() {
    let fixture = Fixture::new(true);
    let (root, service, sequence) = retired(&fixture).await;
    let before = fixture.disk();
    let rows = originals(&fixture);
    let usage = service.storage_usage().unwrap();
    root.start_durable_retirement_preparation(&service, sequence, 64 * 1024, 64 * 1024)
        .unwrap();
    root.observe().await.unwrap();
    let candidate = root
        .finish_retirement_inspection(&service)
        .unwrap()
        .unwrap();
    assert_prepared(&candidate);
    assert_eq!(
        prepared_rows(&fixture),
        candidate.sources().len() as i64 + 1
    );
    assert_eq!(fixture.disk().0, before.0 + 1);
    assert_eq!(originals(&fixture), rows);
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::Unavailable
    );
    assert!(
        root.start_retirement_inspection(&service, sequence)
            .is_err()
    );
    drop(root);
    assert!(RootOwnership::acquire_existing(&fixture.path).is_err());
    drop(candidate);
    wait_until_unlocked(&fixture.path).await;
    assert_eq!(service.storage_usage().unwrap(), usage);
    let root = fixture.root();
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    assert_eq!(prepared_rows(&fixture), 0);
    assert_eq!(originals(&fixture), rows);
    assert_eq!(fixture.disk(), (before.0 + 2, before.1));
    drop(root);
    wait_until_unlocked(&fixture.path).await;
    let root = fixture.root();
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    assert_eq!(fixture.disk(), (before.0 + 2, before.1));
    cleanup(&fixture, root).await;
}

/// The actual worker loses its return after the SQL commit. No blind replay or
/// weaker live-root reopen succeeds; a new cold owner validates cancellation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_retirement_preparation_commit_before_worker_loss() {
    let fixture = Fixture::new(true);
    let (root, service, sequence) = retired(&fixture).await;
    let rows = originals(&fixture);
    fixture
        .probe
        .panic_after_commit
        .store(true, Ordering::SeqCst);
    root.start_durable_retirement_preparation(&service, sequence, 64 * 1024, 64 * 1024)
        .unwrap();
    root.observe().await.unwrap();
    assert!(prepared_rows(&fixture) > 0);
    assert!(root.finish_retirement_inspection(&service).is_err());
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::Unavailable
    );
    assert_eq!(originals(&fixture), rows);
    drop(root);
    wait_until_unlocked(&fixture.path).await;
    let root = fixture.root();
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    assert_eq!(prepared_rows(&fixture), 0);
    assert_eq!(originals(&fixture), rows);
    cleanup(&fixture, root).await;
}

/// A real SQL refusal rolls back manifest and evidence together. Even this
/// optional operation cannot declare a retained failure healthy without proof.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_retirement_preparation_sql_refusal_has_no_prefix() {
    let fixture = Fixture::new(true);
    let (root, service, sequence) = retired(&fixture).await;
    let before = fixture.disk();
    let rows = originals(&fixture);
    let connection = fixture.connection();
    connection.execute_batch("CREATE TRIGGER refuse_prepare BEFORE INSERT ON records WHEN NEW.key GLOB 'q1:*' BEGIN SELECT RAISE(ABORT,'fixture preparation refusal'); END;").unwrap();
    root.start_durable_retirement_preparation(&service, sequence, 64 * 1024, 64 * 1024)
        .unwrap();
    root.observe().await.unwrap();
    assert!(root.finish_retirement_inspection(&service).is_err());
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::Unavailable
    );
    assert_eq!(prepared_rows(&fixture), 0);
    assert_eq!(fixture.disk(), before);
    assert_eq!(originals(&fixture), rows);
    connection
        .execute_batch("DROP TRIGGER refuse_prepare")
        .unwrap();
    root.start_retirement_inspection(&service, sequence)
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

/// Valid SQL checksums do not authorize deletion: foreign format, changed body,
/// missing manifest and unexpected generation each retain every staged row and
/// fail only this root. No invalid prefix is treated as an empty store.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_retirement_preparation_cold_tamper_refuses_without_cleanup() {
    for fault in 0..5 {
        let fixture = Fixture::new(true);
        let (root, service, sequence) = retired(&fixture).await;
        root.start_durable_retirement_preparation(&service, sequence, 64 * 1024, 64 * 1024)
            .unwrap();
        root.observe().await.unwrap();
        drop(
            root.finish_retirement_inspection(&service)
                .unwrap()
                .unwrap(),
        );
        drop(root);
        wait_until_unlocked(&fixture.path).await;
        let connection = fixture.connection();
        match fault {
            0 | 4 => {
                let mut value: Vec<u8> = connection
                    .query_row(
                        "SELECT value FROM records WHERE key='q1:prepare'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                if fault == 0 {
                    value[7] = b'9';
                } else {
                    value[72] ^= 1; // descriptive commitment, valid row checksum
                }
                connection
                    .execute(
                        "UPDATE records SET value=?1,digest=?2 WHERE key='q1:prepare'",
                        rusqlite::params![value, Sha256::digest(&value).as_slice()],
                    )
                    .unwrap();
            }
            1 => {
                let value = b"different evidence";
                connection.execute("UPDATE records SET value=?1,digest=?2 WHERE key GLOB 'q1:*' AND key!='q1:prepare'", rusqlite::params![value.as_slice(), Sha256::digest(value).as_slice()]).unwrap();
            }
            2 => {
                connection
                    .execute("DELETE FROM records WHERE key='q1:prepare'", [])
                    .unwrap();
            }
            _ => {
                connection
                    .execute("UPDATE state SET generation=generation+1", [])
                    .unwrap();
            }
        }
        let rows = originals(&fixture);
        let staged = prepared_rows(&fixture);
        let before = fixture.disk();
        let root = fixture.root();
        assert_eq!(
            root.observe().await.unwrap(),
            CustodyWorkerStatus::Unavailable
        );
        assert!(root.failure().unwrap().is_some());
        assert_eq!(prepared_rows(&fixture), staged);
        assert_eq!(originals(&fixture), rows);
        assert_eq!(fixture.disk(), before);
        drop(connection);
        cleanup(&fixture, root).await;
    }
}
