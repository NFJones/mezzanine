//! Actual SQLite expiry commits, original fences and cold source reconstruction.
//!
//! Genuine lower acceptance, offers and expiry reservations drive the concrete
//! port. Negative controls mutate only generated physical stores; descriptive
//! frames are never imported as live expiry/ACK/native capabilities.

use super::super::ownership::RootOwnership;
use super::*;
use mez_agent::messaging::{
    Envelope, MessageAcceptanceDecodeLimits, MessageAcceptanceWriteOutcome, MessageScope,
    MessageService, MessageServiceSnapshot, MessageTerminalCommit, MessageTerminalTicket,
    MessageTerminalUncertain, MessageTerminalWriteOutcome, Recipient, SenderIdentity,
};
use std::path::PathBuf;
use std::sync::Arc;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "mez-expiry-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        )))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// One real accepted source under its original scope. The concrete writer uses
/// the actual kernel fence; no test callback substitutes for its storage fact.
struct Scene {
    service: MessageService,
    scope: MessageStorageScope,
    sender: SenderIdentity,
    target: SenderIdentity,
}
impl Scene {
    fn new() -> Self {
        Self::with_cache_payload_bytes(1024 * 1024)
    }
    fn with_cache_payload_bytes(bytes: usize) -> Self {
        let mut service = MessageService::default();
        let sender = service.register_agent(None, None, "sender", vec![]);
        let target = service.register_agent(None, None, "worker", vec![]);
        service.subscribe(&target.agent_id).unwrap();
        let scope = MessageStorageScope::default();
        service
            .enable_storage_custody(
                &scope,
                mez_agent::messaging::MessageStorageLimits {
                    cache_payload_bytes: bytes,
                    ..Default::default()
                },
            )
            .unwrap();
        service.require_native_continuity(&scope).unwrap();
        Self {
            service,
            scope,
            sender,
            target,
        }
    }
    fn accept(
        &mut self,
        store: &mut MmpCustodyStore,
        id: &str,
        recipient: Recipient,
        ttl_ms: Option<u64>,
    ) {
        let ticket = self
            .service
            .prepare_acceptance(
                &self.sender.agent_id,
                Envelope {
                    protocol: "mmp/1",
                    id: id.into(),
                    message_type: "send".into(),
                    time: "original".into(),
                    sender: self.sender.clone(),
                    recipient,
                    correlation_id: None,
                    ttl_ms,
                    content_type: "text/plain".into(),
                    payload: "original body\0\n".repeat(6000),
                    extension_fields: vec![],
                },
                MessageScope::Session,
                10,
                4096,
                None,
            )
            .unwrap();
        let plan = store.acceptance_plan(ticket.sequence()).unwrap();
        let write = self
            .service
            .begin_storage_acceptance_write(&ticket, &self.scope)
            .unwrap();
        let MessageAcceptanceWriteOutcome::Committed(commit) =
            write.persist(&mut store.acceptance_port(&plan))
        else {
            panic!("actual source must commit");
        };
        self.service.publish_acceptance(&commit).unwrap();
    }
    fn direct(&mut self, store: &mut MmpCustodyStore) {
        self.accept(
            store,
            "original-expiry",
            Recipient::Agent(self.target.agent_id.clone()),
            Some(5),
        );
    }
    fn reserve(&mut self) -> (MessageTerminalTicket, MessageTerminalWrite) {
        let ticket = self
            .service
            .prepare_storage_expiry(&self.scope, 16, 8 * 1024 * 1024)
            .unwrap()
            .unwrap();
        let write = self
            .service
            .begin_storage_terminal_write(&ticket, &self.scope)
            .unwrap();
        (ticket, write)
    }
    fn capture(
        &self,
        store: &mut MmpCustodyStore,
        fence: &RootOwnership,
    ) -> Arc<MessageServiceSnapshot> {
        let request = self.service.prepare_storage_snapshot(&self.scope).unwrap();
        let verified = request
            .write()
            .verify(&mut fence.bind_store(store).unwrap())
            .unwrap();
        Arc::new(request.finish(verified).unwrap().0)
    }
}
fn limits() -> MessageAcceptanceDecodeLimits {
    MessageAcceptanceDecodeLimits {
        payload_bytes: 1024 * 1024,
        auxiliary_bytes: 512 * 1024,
        recipient_count: 4096,
        recipient_bytes: 1024 * 1024,
    }
}
fn positive(outcome: MessageTerminalWriteOutcome) -> MessageTerminalCommit {
    match outcome {
        MessageTerminalWriteOutcome::Committed(commit) => commit,
        MessageTerminalWriteOutcome::Unknown(error) => {
            panic!("actual expiry did not commit: {}", error.error())
        }
    }
}
fn uncertain(outcome: MessageTerminalWriteOutcome) -> MessageTerminalUncertain {
    match outcome {
        MessageTerminalWriteOutcome::Unknown(error) => error,
        _ => panic!("actual refusal must remain uncertain"),
    }
}

/// Drops an actual concrete store reply after its real commit; no mock success.
struct LostReply<S>(S);
impl<S: MessageTerminalStore> MessageTerminalStore for LostReply<S> {
    fn commit(&mut self, write: &MessageTerminalWrite) -> mez_agent::messaging::Result<()> {
        self.0.commit(write)?;
        Err(MessageError::invalid_state(
            "injected actual postcommit reply loss",
        ))
    }
}

/// A real postcommit lost reply leaves physical t1 but no live outcome. Retry
/// preserves exact bytes/generation and only the actual positive allows adoption.
/// Descriptor validation retains metadata without reserving another body.
#[test]
fn expiry_real_commit_lost_reply_reconciles_before_outcome_publication() {
    let fixture = Fixture::new();
    let fence = RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "expiry-session").unwrap();
    let mut scene = Scene::new();
    scene.direct(&mut store);
    let (ticket, write) = scene.reserve();
    assert_eq!(
        scene
            .service
            .payload_observer()
            .unwrap()
            .snapshot()
            .unwrap()
            .reserved_payload_bytes,
        0
    );
    let plan = fence
        .bind_store(&mut store)
        .unwrap()
        .terminal_plan()
        .unwrap();
    let unknown = {
        let mut owned = fence.bind_store(&mut store).unwrap();
        uncertain(write.persist(&mut LostReply(owned.terminal_port(&plan, &scene.scope))))
    };
    assert!(
        scene
            .service
            .delivery_outcomes_for(&scene.sender.agent_id, 10)
            .unwrap()
            .is_empty()
    );
    assert!(scene.service.cancel_storage_terminal(&ticket).is_err());
    let generation = store.current_state().unwrap().generation;
    let original_rows = store
        .connection
        .query_row(
            "SELECT COUNT(*) FROM records WHERE key LIKE 't1:%'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(original_rows, 1);
    let write = scene.service.retry_terminal_write(unknown).unwrap();
    let commit = {
        let mut owned = fence.bind_store(&mut store).unwrap();
        positive(write.persist(&mut owned.terminal_port(&plan, &scene.scope)))
    };
    assert_eq!(store.current_state().unwrap().generation, generation);
    assert_eq!(
        scene
            .service
            .payload_observer()
            .unwrap()
            .snapshot()
            .unwrap()
            .reserved_payload_bytes,
        0
    );
    assert_eq!(scene.service.publish_terminal(&commit).unwrap(), 1);
    let outcomes = scene
        .service
        .delivery_outcomes_for(&scene.sender.agent_id, 10)
        .unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].observed_at_ms, 16);
    assert_eq!(
        scene
            .service
            .subscription(&scene.target.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
    assert!(
        scene
            .service
            .acknowledge_delivery_outcome(&scene.sender.agent_id, &outcomes[0])
            .is_err()
    );
    let inventory = store.inventory_custody(limits()).unwrap();
    assert_eq!(inventory.expiry_keys.len(), 1);
    assert_eq!(inventory.occurrences.len(), 1);
    let size: i64 = store
        .connection
        .query_row(
            "SELECT length(value) FROM records WHERE key LIKE 't1:%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(size <= 340, "expiry stores no source payload");
}

/// Real close/reopen after t1 commit but before lower adoption must recover the
/// first observation from actual history even with a stale pre-expiry snapshot.
/// This is file recovery, not a claim of terminal actor/subprocess coverage.
#[test]
fn expiry_reopen_restores_actual_first_observation_and_rejects_forged_release() {
    let fixture = Fixture::new();
    let fence = RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "expiry-session").unwrap();
    let mut scene = Scene::new();
    scene.direct(&mut store);
    let stale = scene.capture(&mut store, &fence);
    let (_, write) = scene.reserve();
    let plan = fence
        .bind_store(&mut store)
        .unwrap()
        .terminal_plan()
        .unwrap();
    let commit = {
        let mut owned = fence.bind_store(&mut store).unwrap();
        positive(write.persist(&mut owned.terminal_port(&plan, &scene.scope)))
    };
    drop(commit);
    drop(scene);
    drop(store);
    drop(fence);
    let fence = RootOwnership::acquire_existing(&fixture.0).unwrap();
    let mut store =
        MmpCustodyStore::open_existing(&fixture.0, "expiry-session", Default::default()).unwrap();
    let scope = MessageStorageScope::default();
    let (service, lease) = fence
        .bind_store(&mut store)
        .unwrap()
        .reconstruct_cold_service(stale, &scope, Default::default(), 1000, 1024 * 1024)
        .unwrap();
    assert_eq!(
        service.storage_usage().unwrap().unwrap().pending_messages,
        1
    );
    assert_eq!(
        service.storage_usage().unwrap().unwrap().cache,
        mez_agent::messaging::MessagePayloadUsage::default()
    );
    let capture = service.prepare_storage_snapshot(&scope).unwrap();
    let verified = capture
        .write()
        .verify(&mut fence.bind_store(&mut store).unwrap())
        .unwrap();
    let (snapshot, later_lease) = capture.finish(verified).unwrap();
    let terminal = snapshot.storage_snapshot.as_ref().unwrap().sources[0].terminal[0].unwrap();
    assert_eq!(
        terminal.reason,
        mez_agent::messaging::DeliveryTerminalReason::Expired
    );
    assert_eq!(terminal.observed_at_ms, 16);
    assert!(!terminal.released);
    assert!(
        snapshot.registered_agents.is_empty(),
        "source bytes cannot restore raw identities"
    );
    let mut forged = snapshot;
    forged.storage_snapshot.as_mut().unwrap().sources[0].terminal[0]
        .as_mut()
        .unwrap()
        .released = true;
    forged.storage_snapshot.as_mut().unwrap().sources[0].pending = false;
    // The negative control is structurally valid: only original journal
    // qualification can reject this fabricated durable release claim.
    forged.validate_storage_snapshot().unwrap();
    assert!(
        fence
            .bind_store(&mut store)
            .unwrap()
            .qualify_recovery_checkpoint(Arc::new(forged), limits())
            .is_err()
    );
    assert_eq!(
        store.inventory_custody(limits()).unwrap().occurrences.len(),
        1
    );
    drop(later_lease);
    drop(lease);
}

/// SQL effects cannot remove unrelated accepted evidence while writing t1.
/// The actual transaction rolls back, preserving the original attempt for a
/// same-plan retry after storage is repaired; no error fabricates terminality.
#[test]
fn expiry_trigger_non_target_mutation_rolls_back_and_retries_original() {
    let fixture = Fixture::new();
    let fence = RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "expiry-session").unwrap();
    let mut scene = Scene::new();
    scene.direct(&mut store);
    scene.accept(
        &mut store,
        "unrelated",
        Recipient::Agent(scene.target.agent_id.clone()),
        None,
    );
    let (ticket, write) = scene.reserve();
    let plan = fence
        .bind_store(&mut store)
        .unwrap()
        .terminal_plan()
        .unwrap();
    let generation = store.current_state().unwrap().generation;
    store.connection.execute_batch("CREATE TRIGGER expiry_damage AFTER INSERT ON records WHEN NEW.key LIKE 't1:%' BEGIN DELETE FROM records WHERE key='a1:0000000000000002:commit'; END;").unwrap();
    let unknown = {
        let mut owned = fence.bind_store(&mut store).unwrap();
        uncertain(write.persist(&mut owned.terminal_port(&plan, &scene.scope)))
    };
    assert!(scene.service.cancel_storage_terminal(&ticket).is_err());
    assert_eq!(store.current_state().unwrap().generation, generation);
    assert_eq!(
        store.inventory_custody(limits()).unwrap().occurrences.len(),
        2
    );
    assert_eq!(
        store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM records WHERE key LIKE 't1:%'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    store
        .connection
        .execute_batch("DROP TRIGGER expiry_damage;")
        .unwrap();
    let write = scene.service.retry_terminal_write(unknown).unwrap();
    let commit = {
        let mut owned = fence.bind_store(&mut store).unwrap();
        positive(write.persist(&mut owned.terminal_port(&plan, &scene.scope)))
    };
    assert_eq!(scene.service.publish_terminal(&commit).unwrap(), 1);
    assert_eq!(
        store.inventory_custody(limits()).unwrap().occurrences.len(),
        2
    );
}

/// Correct framing/checksums and unchanged accepted source bytes cannot replace
/// the actual allocation wrapper. A changed stored root binding fails closed;
/// restoring only generated corruption makes the same original valid again.
#[test]
fn expiry_record_requires_original_allocation_even_with_valid_key_and_checksum() {
    let fixture = Fixture::new();
    let fence = RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "expiry-session").unwrap();
    let mut scene = Scene::new();
    scene.direct(&mut store);
    let (_, write) = scene.reserve();
    let plan = fence
        .bind_store(&mut store)
        .unwrap()
        .terminal_plan()
        .unwrap();
    let _commit = {
        let mut owned = fence.bind_store(&mut store).unwrap();
        positive(write.persist(&mut owned.terminal_port(&plan, &scene.scope)))
    };
    let (key, bytes): (String, Vec<u8>) = store
        .connection
        .query_row(
            "SELECT key,value FROM records WHERE key LIKE 't1:%'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let mut changed = bytes.clone();
    changed[8] ^= 1;
    let changed_key = record_key(&changed);
    store
        .connection
        .execute(
            "UPDATE records SET key=?1,value=?2,digest=?3 WHERE key=?4",
            params![
                changed_key,
                changed,
                Sha256::digest(&changed).as_slice(),
                key
            ],
        )
        .unwrap();
    assert!(store.inventory_custody(limits()).is_err());
    store
        .connection
        .execute(
            "UPDATE records SET key=?1,value=?2,digest=?3 WHERE key=?4",
            params![key, bytes, Sha256::digest(&bytes).as_slice(), changed_key],
        )
        .unwrap();
    assert_eq!(
        store.inventory_custody(limits()).unwrap().expiry_keys.len(),
        1
    );
}

/// Actual expiry and ACK of different fanout slots may coexist. The journal
/// fences its exact slot from ACK/native commit without claiming the other
/// recipient's offered commit or cursor as an expiry outcome.
#[test]
fn expiry_actual_slot_guard_preserves_independent_fanout_ack() {
    let fixture = Fixture::new();
    let fence = RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "expiry-session").unwrap();
    let mut scene = Scene::new();
    let other = scene.service.register_agent(None, None, "worker", vec![]);
    scene.service.subscribe(&other.agent_id).unwrap();
    scene.accept(
        &mut store,
        "fanout",
        Recipient::Role("worker".into()),
        Some(5),
    );
    assert_eq!(
        scene
            .service
            .receive_subscribed(&other.agent_id, 11, 1)
            .unwrap()
            .messages
            .len(),
        1
    );
    let ack = scene
        .service
        .prepare_acknowledgement(&other.agent_id, 1, 512 * 1024)
        .unwrap()
        .unwrap();
    let (_, write) = scene.reserve();
    let selected = write.binding_indices().collect::<Vec<_>>();
    assert_eq!(selected.len(), 1);
    let protected = 1 - selected[0];
    let plan = fence
        .bind_store(&mut store)
        .unwrap()
        .terminal_plan()
        .unwrap();
    let commit = {
        let mut owned = fence.bind_store(&mut store).unwrap();
        positive(write.persist(&mut owned.terminal_port(&plan, &scene.scope)))
    };
    let transaction = store.connection.unchecked_transaction().unwrap();
    assert!(
        store
            .ensure_slot_not_terminal_in_snapshot(&transaction, 1, selected[0])
            .is_err()
    );
    store
        .ensure_slot_not_terminal_in_snapshot(&transaction, 1, protected)
        .unwrap();
    store.finish_transaction(transaction).unwrap();
    let ack_plan = store.acknowledgement_plan().unwrap();
    let ack_write = scene
        .service
        .begin_storage_acknowledgement_write(&ack, &scene.scope)
        .unwrap();
    let mez_agent::messaging::MessageAcknowledgementWriteOutcome::Committed(ack_commit) =
        ack_write.persist(&mut store.acknowledgement_port(&ack_plan))
    else {
        panic!("actual unselected ACK must commit");
    };
    scene.service.publish_acknowledgement(&ack_commit).unwrap();
    scene.service.publish_terminal(&commit).unwrap();
    let history = store.fold_custody_history(limits()).unwrap();
    assert_eq!(history.acknowledgements.len(), 1);
    assert_eq!(history.acknowledgements[0].binding_index, protected);
    assert_eq!(history.terminal_observations.len(), 1);
    assert_eq!(history.terminal_observations[0].binding_index, selected[0]);
    assert_eq!(
        scene
            .service
            .subscription(&other.agent_id)
            .unwrap()
            .last_sequence,
        1
    );
}

mod horizon;

/// A genuine offered delivery fills the narrowed cache and stays externally
/// pinned across t1/r1 commits, an actual postcommit lost reply and exact retry.
/// Metadata verification must hash the escaped original without allocating or
/// reserving another body; the retained pin remains visible and unchanged.
#[test]
fn terminal_settlement_streams_original_with_entire_cache_externally_pinned() {
    let fixture = Fixture::new();
    let fence = RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "expiry-session").unwrap();
    let mut scene = Scene::with_cache_payload_bytes(90_000);
    scene.direct(&mut store);
    let delivery = scene
        .service
        .receive_subscribed(&scene.target.agent_id, 11, 1)
        .unwrap();
    assert_eq!(delivery.messages.len(), 1);
    let observer = scene.service.payload_observer().unwrap();
    let pinned = observer.snapshot().unwrap();
    assert_eq!(pinned.live_payload_bytes, 90_000);
    assert_eq!(pinned.reserved_payload_bytes, 0);
    store.source_decode_probe.maximum_payload.set(0);
    store.source_decode_probe.maximum_streamed_payload.set(0);
    let (ticket, write) = scene.reserve();
    let plan = fence
        .bind_store(&mut store)
        .unwrap()
        .terminal_plan()
        .unwrap();
    let unknown = {
        let mut owned = fence.bind_store(&mut store).unwrap();
        uncertain(write.persist(&mut LostReply(owned.terminal_port(&plan, &scene.scope))))
    };
    assert_eq!(observer.snapshot().unwrap(), pinned);
    let generation = store.current_state().unwrap().generation;
    assert!(scene.service.cancel_storage_terminal(&ticket).is_err());
    let write = scene.service.retry_terminal_write(unknown).unwrap();
    let commit = {
        let mut owned = fence.bind_store(&mut store).unwrap();
        positive(write.persist(&mut owned.terminal_port(&plan, &scene.scope)))
    };
    assert_eq!(store.current_state().unwrap().generation, generation);
    assert_eq!(scene.service.publish_terminal(&commit).unwrap(), 1);
    assert_eq!(observer.snapshot().unwrap(), pinned);
    let ticket = scene
        .service
        .prepare_storage_horizon_release(&scene.scope, 600016, usize::MAX)
        .unwrap()
        .unwrap();
    let write = scene
        .service
        .begin_storage_terminal_write(&ticket, &scene.scope)
        .unwrap();
    let plan = fence
        .bind_store(&mut store)
        .unwrap()
        .terminal_plan()
        .unwrap();
    let commit = {
        let mut owned = fence.bind_store(&mut store).unwrap();
        positive(write.persist(&mut owned.terminal_port(&plan, &scene.scope)))
    };
    assert_eq!(scene.service.publish_terminal(&commit).unwrap(), 1);
    assert_eq!(
        scene
            .service
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        0
    );
    assert_eq!(observer.snapshot().unwrap(), pinned);
    assert_eq!(store.source_decode_probe.maximum_payload.get(), 0);
    assert_eq!(
        store.source_decode_probe.maximum_streamed_payload.get(),
        90_000
    );
    assert_eq!(delivery.messages[0].envelope.payload.len(), 90_000);
}
