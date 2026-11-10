//! Actual private-store close/reopen controls for cold recovery qualification.

use mez_agent::messaging::{
    Envelope, MessageAcceptanceWriteOutcome, MessageAcknowledgementCommit,
    MessageAcknowledgementWriteOutcome, MessageNativeIncarnation, MessageScope, MessageService,
    MessageStorageScope, Recipient, SenderIdentity,
};

use super::*;
use crate::storage::mmp_custody::ownership::RootOwnership;

/// Owns only a generated test directory, never an existing session path.
struct Fixture(std::path::PathBuf);
impl Fixture {
    /// Chooses a unique, initially absent private directory for this test.
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "mez-mmp-recovery-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        )))
    }
}
impl Drop for Fixture {
    /// Removes only this fixture's generated directory after its owners drop.
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Uses actual lower ownership and storage ports, including a real native epoch.
struct Scene {
    service: MessageService,
    scope: MessageStorageScope,
    sender: SenderIdentity,
    target: SenderIdentity,
}
impl Scene {
    /// Creates a fresh lower owner whose opaque native epoch is never decoded.
    fn new() -> Self {
        let mut service = MessageService::default();
        let sender = service.register_agent(None, None, "sender", vec![]);
        let target = service.register_agent(None, None, "native", vec![]);
        service.subscribe(&target.agent_id).unwrap();
        let scope = MessageStorageScope::default();
        service
            .enable_storage_custody(&scope, Default::default())
            .unwrap();
        let registration = service
            .capture_sender_registration(&target.agent_id)
            .unwrap();
        service
            .bind_native_conversation(
                &registration,
                "original-conversation",
                &MessageNativeIncarnation::default(),
            )
            .unwrap();
        Self {
            service,
            scope,
            sender,
            target,
        }
    }

    /// Commits an immutable original with a body large enough to span chunks.
    fn accept(&mut self, store: &mut MmpCustodyStore, id: &str) -> u64 {
        let envelope = Envelope {
            protocol: "mmp/1",
            id: id.into(),
            message_type: "send".into(),
            time: "original".into(),
            sender: self.sender.clone(),
            recipient: Recipient::Agent(self.target.agent_id.clone()),
            correlation_id: None,
            ttl_ms: None,
            content_type: "text/plain".into(),
            payload: "x".repeat(80_000),
            extension_fields: vec![],
        };
        let ticket = self
            .service
            .prepare_acceptance(
                &self.sender.agent_id,
                envelope,
                MessageScope::Session,
                10,
                4096,
                None,
            )
            .unwrap();
        let sequence = ticket.sequence();
        let plan = store.acceptance_plan(sequence).unwrap();
        let write = self
            .service
            .begin_storage_acceptance_write(&ticket, &self.scope)
            .unwrap();
        let MessageAcceptanceWriteOutcome::Committed(commit) =
            write.persist(&mut store.acceptance_port(&plan))
        else {
            panic!("original must commit")
        };
        self.service.publish_acceptance(&commit).unwrap();
        self.service
            .release_stored_payload(sequence, &self.scope)
            .unwrap();
        sequence
    }

    /// Selects an actual own offer, then persists its source-bound transport ACK.
    /// Returning the positive result lets the caller model lost versus adopted ACKs.
    fn commit_first_ack(
        &mut self,
        store: &mut MmpCustodyStore,
        fence: &RootOwnership,
    ) -> MessageAcknowledgementCommit {
        self.commit_first_ack_at(store, fence, 20)
    }

    /// Uses the actual selected endpoint clock, including legitimate rollback
    /// before the original acceptance clock and an explicitly offered zero.
    fn commit_first_ack_at(
        &mut self,
        store: &mut MmpCustodyStore,
        fence: &RootOwnership,
        offered_at_ms: u64,
    ) -> MessageAcknowledgementCommit {
        // Hydrate through the actual original port before selecting a real offer.
        let load = self
            .service
            .begin_payload_load(1, &self.scope)
            .unwrap()
            .unwrap();
        let mut loaded = load.load(&mut fence.bind_store(store).unwrap());
        self.service.publish_payload_load(&mut loaded).unwrap();
        self.service
            .receive_subscribed(&self.target.agent_id, offered_at_ms, 1)
            .unwrap();
        let ticket = self
            .service
            .prepare_acknowledgement(&self.target.agent_id, 1, 512 * 1024)
            .unwrap()
            .unwrap();
        let write = self
            .service
            .begin_storage_acknowledgement_write(&ticket, &self.scope)
            .unwrap();
        let plan = store.acknowledgement_plan().unwrap();
        let MessageAcknowledgementWriteOutcome::Committed(commit) =
            write.persist(&mut store.acknowledgement_port(&plan))
        else {
            panic!("ACK must commit")
        };
        commit
    }

    /// Verifies real source digests under the actual kernel owner at capture.
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

/// Uses admission's ordinary ceilings; recovery may narrow its decode scratch.
fn limits() -> MessageAcceptanceDecodeLimits {
    MessageAcceptanceDecodeLimits {
        payload_bytes: 1024 * 1024,
        auxiliary_bytes: 16 * 1024 * 1024,
        recipient_count: 4096,
        recipient_bytes: 1024 * 1024,
    }
}

/// Newer durable ACKs and accepted originals must survive a stale checkpoint.
/// The original process/lock is actually dropped and reacquired; returned facts
/// preserve the original checkpoint and do not invent a live native context.
#[test]
fn reopen_qualifies_original_checkpoint_and_complete_later_history() {
    let fixture = Fixture::new();
    let fence = RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "original-session").unwrap();
    let mut scene = Scene::new();
    scene.accept(&mut store, "first");
    let checkpoint = scene.capture(&mut store, &fence);
    let native = checkpoint.storage_snapshot.as_ref().unwrap().native.clone();
    assert_eq!(scene.accept(&mut store, "later"), 2);
    let _commit = scene.commit_first_ack(&mut store, &fence);
    // Model a lost positive ACK result: the canonical service never adopts it.
    drop(scene);
    drop(store);
    drop(fence);
    let fence = RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "original-session").unwrap();
    let evidence = fence
        .bind_store(&mut store)
        .unwrap()
        .qualify_recovery_checkpoint(checkpoint.clone(), limits())
        .unwrap();
    assert!(Arc::ptr_eq(&evidence.checkpoint, &checkpoint));
    assert_eq!(evidence.history.acknowledgements.len(), 1);
    assert_eq!(evidence.history.acknowledgements[0].sequence, 1);
    assert_eq!(evidence.history.acknowledgements[0].offered_at_ms, 20);
    assert!(evidence.history.retained_metadata > evidence.history.inventory.retained_metadata);
    assert_eq!(evidence.next_sequence, 3);
    assert_eq!(evidence.history.inventory.occurrences.len(), 2);
    assert_eq!(
        evidence
            .checkpoint
            .storage_snapshot
            .as_ref()
            .unwrap()
            .sources
            .len(),
        1
    );
    assert_eq!(
        evidence
            .checkpoint
            .storage_snapshot
            .as_ref()
            .unwrap()
            .native,
        native
    );
    assert!(
        !evidence
            .checkpoint
            .storage_snapshot
            .as_ref()
            .unwrap()
            .sources[0]
            .acknowledged[0]
    );
    assert!(MessageService::from_snapshot_state(&evidence.checkpoint).is_err());
}

/// Matching owner text and byte-for-byte copied SQLite content cannot replace
/// the original physical store. The fresh kernel fence alone grants no recovery.
#[test]
fn copied_store_with_same_owner_and_allocation_labels_is_refused() {
    let original = Fixture::new();
    let fence = RootOwnership::acquire(&original.0).unwrap();
    let mut store = MmpCustodyStore::open(&original.0, "original-session").unwrap();
    let mut scene = Scene::new();
    scene.accept(&mut store, "first");
    let checkpoint = scene.capture(&mut store, &fence);
    drop(scene);
    drop(store);
    drop(fence);
    let copied = Fixture::new();
    let fence = RootOwnership::acquire(&copied.0).unwrap();
    fs::copy(
        original.0.join("custody.sqlite"),
        copied.0.join("custody.sqlite"),
    )
    .unwrap();
    let mut store = MmpCustodyStore::open(&copied.0, "original-session").unwrap();
    let error = fence
        .bind_store(&mut store)
        .unwrap()
        .qualify_recovery_checkpoint(checkpoint, limits())
        .err()
        .unwrap();
    assert!(error.to_string().contains("original allocation changed"));
}

/// Static schema validation cannot know a referenced body's actual byte length.
/// The real decoder must compare it before returning any recovery observation.
#[test]
fn altered_checkpoint_metadata_is_checked_against_the_immutable_source() {
    let fixture = Fixture::new();
    let fence = RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "original-session").unwrap();
    let mut scene = Scene::new();
    scene.accept(&mut store, "first");
    let mut checkpoint = scene.capture(&mut store, &fence);
    Arc::get_mut(&mut checkpoint)
        .unwrap()
        .storage_snapshot
        .as_mut()
        .unwrap()
        .sources[0]
        .payload_bytes -= 1;
    checkpoint.validate_storage_snapshot().unwrap();
    let error = fence
        .bind_store(&mut store)
        .unwrap()
        .qualify_recovery_checkpoint(checkpoint, limits())
        .err()
        .unwrap();
    assert!(error.to_string().contains("disagrees with original source"));
}

/// Serializable positive slot bits cannot substitute for a committed ACK row.
/// A forged snapshot can be structurally coherent and still lacks original proof.
#[test]
fn positive_checkpoint_slot_without_durable_ack_is_refused() {
    let fixture = Fixture::new();
    let fence = RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "original-session").unwrap();
    let mut scene = Scene::new();
    scene.accept(&mut store, "first");
    let mut checkpoint = scene.capture(&mut store, &fence);
    let row = &mut Arc::get_mut(&mut checkpoint)
        .unwrap()
        .storage_snapshot
        .as_mut()
        .unwrap()
        .sources[0];
    row.acknowledged[0] = true;
    row.offered_at_ms[0] = Some(20);
    row.pending = false;
    checkpoint.validate_storage_snapshot().unwrap();
    let error = fence
        .bind_store(&mut store)
        .unwrap()
        .qualify_recovery_checkpoint(checkpoint, limits())
        .err()
        .unwrap();
    assert!(
        error
            .to_string()
            .contains("lacks original durable evidence")
    );
}

/// Input and read scratch already occupying the metadata budget must reduce the
/// space available to an inventory and fold, rather than receive separate caps.
#[test]
fn inventory_and_fold_share_the_callers_retained_metadata_budget() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "original-session").unwrap();
    let mut scene = Scene::new();
    scene.accept(&mut store, "first");
    let transaction = store.connection.unchecked_transaction().unwrap();
    assert!(
        store
            .fold_custody_history_in_snapshot(&transaction, limits(), 8 * 1024 * 1024)
            .is_err()
    );
    let history = store
        .fold_custody_history_in_snapshot(&transaction, limits(), 1024)
        .unwrap();
    assert!(history.inventory.retained_metadata >= 1024 + 256);
}

/// A later source not mentioned in the checkpoint is still part of the original
/// store. An unfinished projection cannot be silently omitted during recovery.
#[test]
fn incomplete_source_after_capture_withholds_the_whole_candidate() {
    let fixture = Fixture::new();
    let fence = RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "original-session").unwrap();
    let mut scene = Scene::new();
    scene.accept(&mut store, "first");
    let checkpoint = scene.capture(&mut store, &fence);
    scene.accept(&mut store, "later");
    assert_eq!(
        store
            .connection
            .execute(
                "DELETE FROM records WHERE key='a1:0000000000000002:commit'",
                []
            )
            .unwrap(),
        1
    );
    assert!(
        fence
            .bind_store(&mut store)
            .unwrap()
            .qualify_recovery_checkpoint(checkpoint, limits())
            .is_err()
    );
}

/// An apparently matching physical allocation must still cover the capture's
/// monotonic generation. Static framing does not supply that historical proof.
#[test]
fn store_below_captured_generation_is_refused() {
    let fixture = Fixture::new();
    let fence = RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "original-session").unwrap();
    let mut scene = Scene::new();
    scene.accept(&mut store, "first");
    let mut checkpoint = scene.capture(&mut store, &fence);
    Arc::get_mut(&mut checkpoint)
        .unwrap()
        .storage_snapshot
        .as_mut()
        .unwrap()
        .binding
        .as_mut()
        .unwrap()
        .generation += 1;
    checkpoint.validate_storage_snapshot().unwrap();
    let error = fence
        .bind_store(&mut store)
        .unwrap()
        .qualify_recovery_checkpoint(checkpoint, limits())
        .err()
        .unwrap();
    assert!(error.to_string().contains("history rolled back"));
}

/// Row removal cannot lower the allocator floor. This uses the real batch port
/// to remove a fixture's complete original, without claiming that production
/// settled reclaim or its receipt-retention policy has been implemented.
#[test]
fn recovery_allocator_uses_persistent_watermark_when_no_source_survives() {
    let fixture = Fixture::new();
    let fence = RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "original-session").unwrap();
    let mut scene = Scene::new();
    let checkpoint = scene.capture(&mut store, &fence);
    scene.accept(&mut store, "later");
    let generation = store.inventory_custody(limits()).unwrap().generation;
    let records = {
        let mut statement = store
            .connection
            .prepare("SELECT key FROM records ORDER BY key")
            .unwrap();
        let keys = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap();
        keys.map(|key| {
            let key = key.unwrap();
            let digest = <[u8; 32]>::from(Sha256::digest(store.read(&key).unwrap().unwrap()));
            (key, digest)
        })
        .collect::<Vec<_>>()
    };
    let writes = records
        .iter()
        .map(|(key, digest)| CustodyWrite {
            key,
            previous: Some(digest),
            value: None,
        })
        .collect::<Vec<_>>();
    store.commit(generation, &writes).unwrap();
    drop(scene);
    drop(store);
    drop(fence);
    let fence = RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "original-session").unwrap();
    let evidence = fence
        .bind_store(&mut store)
        .unwrap()
        .qualify_recovery_checkpoint(checkpoint, limits())
        .unwrap();
    assert!(evidence.history.inventory.occurrences.is_empty());
    assert_eq!(evidence.history.observed_next_sequence, Some(1));
    assert_eq!(evidence.next_sequence, 2);
}

/// A large descriptive checkpoint cannot reserve a separate metadata budget
/// from inventory/folding. Refuse before any original-source decode is issued.
#[test]
fn oversized_checkpoint_and_decode_scratch_are_preflighted_together() {
    let fixture = Fixture::new();
    let fence = RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "original-session").unwrap();
    let mut scene = Scene::new();
    scene.accept(&mut store, "first");
    let mut checkpoint = scene.capture(&mut store, &fence);
    Arc::get_mut(&mut checkpoint)
        .unwrap()
        .storage_snapshot
        .as_mut()
        .unwrap()
        .sources[0]
        .delivery
        .message_id = "x".repeat(800_000);
    checkpoint.validate_storage_snapshot().unwrap();
    let error = fence
        .bind_store(&mut store)
        .unwrap()
        .qualify_recovery_checkpoint(checkpoint, limits())
        .err()
        .unwrap();
    assert!(error.to_string().contains("inventory metadata capacity"));
}

/// A genuinely published positive slot remains valid after reopening when its
/// original durable ACK exists. Qualification must not reject all positive bits
/// merely because fabricated bits require refusal in the negative control.
#[test]
fn committed_checkpoint_ack_is_qualified_by_its_original_durable_row() {
    let fixture = Fixture::new();
    let fence = RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "original-session").unwrap();
    let mut scene = Scene::new();
    scene.accept(&mut store, "first");
    let commit = scene.commit_first_ack(&mut store, &fence);
    scene.service.publish_acknowledgement(&commit).unwrap();
    let checkpoint = scene.capture(&mut store, &fence);
    assert!(checkpoint.storage_snapshot.as_ref().unwrap().sources[0].acknowledged[0]);
    drop(scene);
    drop(store);
    drop(fence);
    let fence = RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "original-session").unwrap();
    let evidence = fence
        .bind_store(&mut store)
        .unwrap()
        .qualify_recovery_checkpoint(checkpoint, limits())
        .unwrap();
    assert_eq!(evidence.history.acknowledgements.len(), 1);
}

/// Snapshot terminal bits cannot authorize custody loss before the terminal
/// journal is implemented. Keep a structurally coherent, unreleased outcome
/// unavailable even when every immutable original still verifies.
#[test]
fn snapshot_terminal_bits_without_a_durable_journal_are_refused() {
    let fixture = Fixture::new();
    let fence = RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "original-session").unwrap();
    let mut scene = Scene::new();
    scene.accept(&mut store, "first");
    let mut checkpoint = scene.capture(&mut store, &fence);
    Arc::get_mut(&mut checkpoint)
        .unwrap()
        .storage_snapshot
        .as_mut()
        .unwrap()
        .sources[0]
        .terminal[0] = Some(mez_agent::messaging::MessageDeliveryTerminalSnapshot {
        reason: mez_agent::messaging::DeliveryTerminalReason::RecipientRetired,
        observed_at_ms: 20,
        released: false,
    });
    checkpoint.validate_storage_snapshot().unwrap();
    let error = fence
        .bind_store(&mut store)
        .unwrap()
        .qualify_recovery_checkpoint(checkpoint, limits())
        .err()
        .unwrap();
    assert!(
        error
            .to_string()
            .contains("terminal outcome needs durable recovery evidence")
    );
}

/// Cold reconstruction must consume the complete qualified history after a real
/// close/reopen, including an ACK whose positive result was never published and
/// an original accepted after capture. No raw or native endpoint is revived.
#[test]
fn reopened_store_reconstructs_cold_pending_and_lost_ack_without_endpoint_authority() {
    let fixture = Fixture::new();
    let fence = RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "original-session").unwrap();
    let mut scene = Scene::new();
    scene.accept(&mut store, "first");
    let checkpoint = scene.capture(&mut store, &fence);
    scene.accept(&mut store, "later");
    let _lost_commit = scene.commit_first_ack(&mut store, &fence);
    let original_scope = scene.scope.clone();
    let original_target = scene.target.agent_id.clone();
    let original_sender = scene.sender.agent_id.clone();
    drop(scene);
    drop(store);
    drop(fence);
    let fence = RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "original-session").unwrap();
    let scope = MessageStorageScope::default();
    let (mut service, _input_lease) = fence
        .bind_store(&mut store)
        .unwrap()
        .reconstruct_cold_service(checkpoint, &scope, Default::default(), 1000, 1024 * 1024)
        .unwrap();
    let usage = service.storage_usage().unwrap().unwrap();
    assert_eq!(usage.pending_messages, 1);
    assert_eq!(usage.pending_payload_bytes, 80_000);
    assert_eq!(
        usage.cache,
        mez_agent::messaging::MessagePayloadUsage::default()
    );
    assert!(service.discover_agents().is_empty());
    assert!(
        service
            .receive_subscribed(&original_target, 30, 10)
            .is_err()
    );
    assert!(service.begin_payload_load(2, &original_scope).is_err());
    let capture = service.prepare_storage_snapshot(&scope).unwrap();
    let verified = capture
        .write()
        .verify(&mut fence.bind_store(&mut store).unwrap())
        .unwrap();
    let (captured, _lease) = capture.finish(verified).unwrap();
    let rows = &captured.storage_snapshot.as_ref().unwrap().sources;
    assert_eq!(captured.next_sequence, 3);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].acknowledged, vec![true]);
    assert_eq!(rows[0].offered_at_ms, vec![Some(20)]);
    assert!(!rows[0].pending);
    assert_eq!(rows[1].delivery.message_id, "later");
    assert_eq!(rows[1].offered_at_ms, vec![None]);
    assert!(rows[1].pending);
    let write = service.begin_payload_load(2, &scope).unwrap().unwrap();
    let mut loaded = write.load(&mut fence.bind_store(&mut store).unwrap());
    assert!(loaded.error().is_none());
    assert!(service.publish_payload_load(&mut loaded).unwrap());
    assert_eq!(
        service
            .storage_usage()
            .unwrap()
            .unwrap()
            .cache
            .live_payload_bytes,
        80_000
    );
    service.release_stored_payload(2, &scope).unwrap();
    let sender = service.register_agent(None, None, "fresh sender", vec![]);
    let target = service.register_agent(None, None, "fresh recipient", vec![]);
    assert_ne!(sender.agent_id, original_sender);
    assert_ne!(target.agent_id, original_target);
    service.subscribe(&target.agent_id).unwrap();
    assert!(
        service
            .receive_subscribed(&target.agent_id, 30, 10)
            .unwrap()
            .messages
            .is_empty()
    );
    let envelope = Envelope {
        protocol: "mmp/1",
        id: "fresh".into(),
        message_type: "send".into(),
        time: "fresh".into(),
        sender: sender.clone(),
        recipient: Recipient::Agent(target.agent_id.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain".into(),
        payload: "fresh".into(),
        extension_fields: vec![],
    };
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope,
            MessageScope::Session,
            30,
            4096,
            None,
        )
        .unwrap();
    assert_eq!(ticket.sequence(), 3);
    service.cancel_acceptance(&ticket).unwrap();
}

/// A verified disk original still cannot enlarge a narrowed destination's
/// custody policy. The whole reconstruction is withheld and the store remains
/// unchanged, so retry under a sufficient policy recovers both originals.
#[test]
fn cold_reconstruction_capacity_refusal_preserves_original_store_for_retry() {
    let fixture = Fixture::new();
    let fence = RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "original-session").unwrap();
    let mut scene = Scene::new();
    scene.accept(&mut store, "first");
    let checkpoint = scene.capture(&mut store, &fence);
    scene.accept(&mut store, "later");
    drop(scene);
    let before = store.acceptance_plan(0).unwrap();
    let scope = MessageStorageScope::default();
    let limits = mez_agent::messaging::MessageStorageLimits {
        messages: 1,
        ..Default::default()
    };
    assert!(
        fence
            .bind_store(&mut store)
            .unwrap()
            .reconstruct_cold_service(checkpoint.clone(), &scope, limits, 1000, 1024 * 1024)
            .is_err()
    );
    assert!(before.same_position(&store.acceptance_plan(0).unwrap()));
    let (recovered, _input_lease) = fence
        .bind_store(&mut store)
        .unwrap()
        .reconstruct_cold_service(checkpoint, &scope, Default::default(), 1000, 1024 * 1024)
        .unwrap();
    assert_eq!(
        recovered.storage_usage().unwrap().unwrap().pending_messages,
        2
    );
    assert!(before.same_position(&store.acceptance_plan(0).unwrap()));
}

/// A narrow metadata policy must constrain the storage inventory's first
/// allocation, not just reject an already over-budget lower service afterward.
#[test]
fn cold_reconstruction_metadata_preflight_honors_narrowed_destination() {
    let fixture = Fixture::new();
    let fence = RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "original-session").unwrap();
    let mut scene = Scene::new();
    scene.accept(&mut store, "first");
    let checkpoint = scene.capture(&mut store, &fence);
    drop(scene);
    let limits = mez_agent::messaging::MessageStorageLimits {
        // Worker scratch, native proof, ACK and cache headroom all remain
        // inside this genuinely narrowed destination, not the full 8 MiB.
        metadata_bytes: 5 * 1024 * 1024,
        ..Default::default()
    };
    let (service, _input_lease) = fence
        .bind_store(&mut store)
        .unwrap()
        .reconstruct_cold_service(
            checkpoint,
            &MessageStorageScope::default(),
            limits,
            1000,
            1024 * 1024,
        )
        .unwrap();
    assert!(service.storage_usage().unwrap().unwrap().metadata_bytes <= limits.metadata_bytes);
}

/// A genuine offer may predate acceptance on a rolled-back endpoint clock.
/// Persist real ACKs at times9 and0 for an original accepted at10, actually drop
/// and reacquire its kernel/store owner, and reconstruct without inventing a new
/// offer time or rejecting legitimate original durable evidence.
#[test]
fn cold_reconstruction_preserves_genuine_ack_offer_clocks_across_rollback() {
    for offered_at_ms in [9, 0] {
        let fixture = Fixture::new();
        let fence = RootOwnership::acquire(&fixture.0).unwrap();
        let mut store = MmpCustodyStore::open(&fixture.0, "original-session").unwrap();
        let mut scene = Scene::new();
        scene.accept(&mut store, "first");
        let checkpoint = scene.capture(&mut store, &fence);
        let _lost_commit = scene.commit_first_ack_at(&mut store, &fence, offered_at_ms);
        drop(scene);
        drop(store);
        drop(fence);
        let fence = RootOwnership::acquire(&fixture.0).unwrap();
        let mut store = MmpCustodyStore::open(&fixture.0, "original-session").unwrap();
        let scope = MessageStorageScope::default();
        let (service, _input_lease) = fence
            .bind_store(&mut store)
            .unwrap()
            .reconstruct_cold_service(checkpoint, &scope, Default::default(), 1000, 1024 * 1024)
            .unwrap();
        assert_eq!(
            service.storage_usage().unwrap().unwrap().pending_messages,
            0
        );
        let request = service.prepare_storage_snapshot(&scope).unwrap();
        let verified = request
            .write()
            .verify(&mut fence.bind_store(&mut store).unwrap())
            .unwrap();
        let (snapshot, _lease) = request.finish(verified).unwrap();
        let row = &snapshot.storage_snapshot.as_ref().unwrap().sources[0];
        assert_eq!(row.accepted_at_ms, 10);
        assert_eq!(row.offered_at_ms, vec![Some(offered_at_ms)]);
        assert_eq!(row.acknowledged, vec![true]);
    }
}

/// Metadata scans enforce the destination's payload-size bound without a body
/// allocation; actual construction uses its reserved payload quantum. Observe
/// both real decoder inputs and completed bodies independently:
/// a narrow-cache refusal cannot first allocate an oversized original, and retry
/// under a sufficient policy must still recover the unchanged original store.
#[test]
fn cold_reconstruction_narrow_cache_refuses_before_oversized_inventory_decode() {
    let fixture = Fixture::new();
    let fence = RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "original-session").unwrap();
    let mut scene = Scene::new();
    scene.accept(&mut store, "first");
    let checkpoint = scene.capture(&mut store, &fence);
    let _committed = scene.commit_first_ack(&mut store, &fence);
    drop(scene);
    store.source_decode_probe.maximum_limit.set(0);
    store.source_decode_probe.maximum_payload.set(0);
    store.source_decode_probe.maximum_streamed_limit.set(0);
    store.source_decode_probe.maximum_streamed_payload.set(0);
    let before = store.acceptance_plan(0).unwrap();
    let scope = MessageStorageScope::default();
    let limits = mez_agent::messaging::MessageStorageLimits {
        cache_payload_bytes: 32 * 1024,
        ..Default::default()
    };
    assert!(
        fence
            .bind_store(&mut store)
            .unwrap()
            .reconstruct_cold_service(checkpoint.clone(), &scope, limits, 1000, 1024 * 1024,)
            .is_err()
    );
    assert_eq!(
        store.source_decode_probe.maximum_payload.get(),
        0,
        "an oversized original must not complete decoding before refusal"
    );
    assert_eq!(
        store.source_decode_probe.maximum_streamed_limit.get(),
        limits.cache_payload_bytes
    );
    assert_eq!(
        store.source_decode_probe.maximum_limit.get(),
        0,
        "full body reader ran before capacity refusal"
    );
    assert_eq!(
        store.source_decode_probe.maximum_streamed_payload.get(),
        0,
        "oversized descriptor completed validation before refusal"
    );
    assert!(before.same_position(&store.acceptance_plan(0).unwrap()));
    let (service, _input_lease) = fence
        .bind_store(&mut store)
        .unwrap()
        .reconstruct_cold_service(checkpoint, &scope, Default::default(), 1000, 1024 * 1024)
        .unwrap();
    assert_eq!(store.source_decode_probe.maximum_payload.get(), 80_000);
    assert_eq!(
        service.storage_usage().unwrap().unwrap().cache,
        mez_agent::messaging::MessagePayloadUsage::default()
    );
    assert!(before.same_position(&store.acceptance_plan(0).unwrap()));
}

/// Real accepted originals, durable ACK evidence and an older schema10 source
/// checkpoint all validate through the streaming descriptor. Proof qualification
/// must not allocate any payload; actual subsequent payload loading is the
/// positive control demonstrating that the full decoder probe remains effective.
#[test]
fn original_source_and_ack_qualification_stream_payload_without_materializing_it() {
    let fixture = Fixture::new();
    let owner = RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "original-session").unwrap();
    let mut scene = Scene::new();
    scene.accept(&mut store, "first");
    scene.accept(&mut store, "second");
    let checkpoint = scene.capture(&mut store, &owner);
    let _committed_ack = scene.commit_first_ack(&mut store, &owner);
    store.source_decode_probe.maximum_payload.set(0);
    store.source_decode_probe.maximum_streamed_payload.set(0);
    let evidence = owner
        .bind_store(&mut store)
        .unwrap()
        .qualify_recovery_checkpoint(checkpoint, limits())
        .unwrap();
    assert_eq!(evidence.history.inventory.occurrences.len(), 2);
    assert_eq!(evidence.history.acknowledgements.len(), 1);
    assert_eq!(
        store.source_decode_probe.maximum_payload.get(),
        0,
        "metadata validation materialized a body"
    );
    assert_eq!(
        store.source_decode_probe.maximum_streamed_payload.get(),
        80_000
    );
    let (source, _, _) = store.read_accepted_source(1, limits()).unwrap();
    assert_eq!(source.envelope().payload.len(), 80_000);
    assert_eq!(store.source_decode_probe.maximum_payload.get(), 80_000);
}
