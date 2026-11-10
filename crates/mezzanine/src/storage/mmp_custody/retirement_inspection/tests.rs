//! Read-only original retirement observations over real lower writes and SQLite.

use super::super::ownership::RootOwnership;
use super::*;
use mez_agent::messaging::{
    Envelope, MessageAcceptanceWriteOutcome, MessageAcknowledgementWriteOutcome, MessageScope,
    MessageService, Recipient, SenderIdentity,
};

/// A uniquely created disposable root. Cleanup verifies this exact path only.
struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        use std::os::unix::fs::DirBuilderExt;
        let path = std::env::temp_dir().join(format!(
            "mez-retirement-inspection-{}-{:016x}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let removed = std::fs::remove_dir_all(&self.0);
        if !std::thread::panicking() {
            removed.unwrap();
            assert!(!self.0.exists());
        }
    }
}

fn accept(
    store: &mut MmpCustodyStore,
    service: &mut MessageService,
    scope: &MessageStorageScope,
    sender: &SenderIdentity,
    target: &SenderIdentity,
    id: &str,
    ttl_ms: Option<u64>,
) -> u64 {
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            Envelope {
                protocol: "mmp/1",
                id: id.into(),
                message_type: "send".into(),
                time: "original".into(),
                sender: sender.clone(),
                recipient: Recipient::Agent(target.agent_id.clone()),
                correlation_id: None,
                ttl_ms,
                content_type: "text/plain".into(),
                payload: "source\0\n".repeat(16_000),
                extension_fields: vec![],
            },
            MessageScope::Session,
            10,
            4096,
            None,
        )
        .unwrap();
    let sequence = ticket.sequence();
    let plan = store.acceptance_plan(sequence).unwrap();
    let write = service
        .begin_storage_acceptance_write(&ticket, scope)
        .unwrap();
    let MessageAcceptanceWriteOutcome::Committed(commit) =
        write.persist(&mut store.acceptance_port(&plan))
    else {
        panic!("actual original must commit");
    };
    service.publish_acceptance(&commit).unwrap();
    sequence
}

fn ack(
    store: &mut MmpCustodyStore,
    service: &mut MessageService,
    scope: &MessageStorageScope,
    target: &SenderIdentity,
    sequence: u64,
) {
    service
        .receive_subscribed(&target.agent_id, 11, 16)
        .unwrap();
    let ticket = service
        .prepare_acknowledgement(&target.agent_id, sequence, 512 * 1024)
        .unwrap()
        .unwrap();
    let write = service
        .begin_storage_acknowledgement_write(&ticket, scope)
        .unwrap();
    let plan = store.acknowledgement_plan().unwrap();
    let MessageAcknowledgementWriteOutcome::Committed(commit) =
        write.persist(&mut store.acknowledgement_port(&plan))
    else {
        panic!("actual ACK must commit");
    };
    service.publish_acknowledgement(&commit).unwrap();
}

/// An actual shared k1 binds both originals. Empty runtime indexes cannot turn
/// that row into an independently deletable ACK for the first original. Missing
/// disk ACK evidence remains unsettled even if lower custody already released.
#[test]
fn retired_source_inspection_preserves_shared_ack_and_missing_settlement() {
    for shared in [false, true] {
        let fixture = Fixture::new();
        let owner = RootOwnership::acquire(&fixture.0).unwrap();
        let mut store = MmpCustodyStore::open(&fixture.0, "original-session").unwrap();
        let mut service = MessageService::with_limits(1, 1024 * 1024);
        let sender = service.register_agent(None, None, "sender", vec![]);
        let target = service.register_agent(None, None, "recipient", vec![]);
        let scope = MessageStorageScope::default();
        service
            .enable_storage_custody(&scope, Default::default())
            .unwrap();
        service.subscribe(&target.agent_id).unwrap();
        let first = accept(
            &mut store,
            &mut service,
            &scope,
            &sender,
            &target,
            "first",
            None,
        );
        if !shared {
            ack(&mut store, &mut service, &scope, &target, first);
        }
        let second = accept(
            &mut store,
            &mut service,
            &scope,
            &sender,
            &target,
            "second",
            None,
        );
        if shared {
            ack(&mut store, &mut service, &scope, &target, second);
        }
        let input = service
            .prepare_source_retirement_inspection(&scope)
            .unwrap();
        assert!(service.source_retirement_inspection_is_original(&input));
        assert!(input.has_no_live_source(first));
        assert!(!input.has_no_live_source(second));
        let before = image(&store);
        let result = owner
            .bind_store(&mut store)
            .unwrap()
            .inspect_source_retirement(&input, &scope, first)
            .unwrap();
        assert_eq!(result.sequence, first);
        assert_eq!(result.recipient_count, 1);
        assert_eq!(result.unsettled_recipients, 0);
        assert_eq!(result.shared_acknowledgements, usize::from(shared));
        assert_eq!(result.native_journal_references, 0);
        assert_eq!(result.position.store_id, store.store_id);
        assert_eq!(result.position.sequence_watermark, second);
        assert_ne!(result.source_sha256, [0; 32]);
        assert_eq!(image(&store), before);
        for sequence in [0, second, second + 1] {
            assert!(
                owner
                    .bind_store(&mut store)
                    .unwrap()
                    .inspect_source_retirement(&input, &scope, sequence)
                    .is_err()
            );
        }
        assert!(
            owner
                .bind_store(&mut store)
                .unwrap()
                .inspect_source_retirement(&input, &MessageStorageScope::default(), first)
                .is_err()
        );
        // A missing original durable ACK is not repaired from live state or a
        // cursor. The inspection only reports the now-unsettled original slot.
        store
            .connection
            .execute("DELETE FROM records WHERE key LIKE 'k1:%'", [])
            .unwrap();
        let result = owner
            .bind_store(&mut store)
            .unwrap()
            .inspect_source_retirement(&input, &scope, first)
            .unwrap();
        assert_eq!(result.unsettled_recipients, 1);
        let key = "a1:0000000000000001:chunk:00000000";
        store
            .connection
            .execute("UPDATE records SET value=X'00' WHERE key=?1", [key])
            .unwrap();
        let before = image(&store);
        assert!(
            owner
                .bind_store(&mut store)
                .unwrap()
                .inspect_source_retirement(&input, &scope, first)
                .is_err()
        );
        assert_eq!(image(&store), before, "corrupt sources stay untouched");
    }
}

/// Actual t1 and r1 commits distinguish expiry observation from disposition
/// release. The original lower source retires only after release plus replay
/// eviction; missing r1 subsequently remains an unsettled durable outcome.
#[test]
fn retired_source_inspection_requires_actual_terminal_release() {
    use mez_agent::messaging::MessageTerminalWriteOutcome;

    let fixture = Fixture::new();
    let owner = RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "original-session").unwrap();
    let mut service = MessageService::with_limits(1, 1024 * 1024);
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "recipient", vec![]);
    let scope = MessageStorageScope::default();
    service
        .enable_storage_custody(&scope, Default::default())
        .unwrap();
    let sequence = accept(
        &mut store,
        &mut service,
        &scope,
        &sender,
        &target,
        "expired",
        Some(5),
    );
    for release in [false, true] {
        let ticket = if release {
            service.prepare_storage_horizon_release(&scope, 600_016, usize::MAX)
        } else {
            service.prepare_storage_expiry(&scope, 16, usize::MAX)
        }
        .unwrap()
        .unwrap();
        let write = service
            .begin_storage_terminal_write(&ticket, &scope)
            .unwrap();
        let plan = owner
            .bind_store(&mut store)
            .unwrap()
            .terminal_plan()
            .unwrap();
        let MessageTerminalWriteOutcome::Committed(commit) = ({
            let mut owned = owner.bind_store(&mut store).unwrap();
            write.persist(&mut owned.terminal_port(&plan, &scope))
        }) else {
            panic!("actual terminal record must commit");
        };
        service.publish_terminal(&commit).unwrap();
    }
    accept(
        &mut store,
        &mut service,
        &scope,
        &sender,
        &target,
        "later",
        None,
    );
    let input = service
        .prepare_source_retirement_inspection(&scope)
        .unwrap();
    assert!(input.has_no_live_source(sequence));
    let before = image(&store);
    let result = owner
        .bind_store(&mut store)
        .unwrap()
        .inspect_source_retirement(&input, &scope, sequence)
        .unwrap();
    assert_eq!(result.unsettled_recipients, 0);
    assert_eq!(result.shared_acknowledgements, 0);
    assert_eq!(image(&store), before);
    store
        .connection
        .execute("DELETE FROM records WHERE key LIKE 'r1:%'", [])
        .unwrap();
    let result = owner
        .bind_store(&mut store)
        .unwrap()
        .inspect_source_retirement(&input, &scope, sequence)
        .unwrap();
    assert_eq!(result.unsettled_recipients, 1);
}

/// Independent SQL oracle includes source/journal bytes and allocator/clock
/// state. This test-only materialization is not used by production inspection.
fn image(store: &MmpCustodyStore) -> Vec<(String, Vec<u8>, Vec<u8>)> {
    let mut statement = store
        .connection
        .prepare("SELECT key,value,digest FROM records ORDER BY key")
        .unwrap();
    let mut rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let state = store
        .connection
        .query_row(
            "SELECT generation,batch,sequence_watermark,elapsed_clock FROM state",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, Option<Vec<u8>>>(3)?,
                ))
            },
        )
        .unwrap();
    rows.push((format!("state:{}", state.0), state.1, state.2));
    rows.push(("clock".into(), state.3.unwrap_or_default(), vec![]));
    rows
}
