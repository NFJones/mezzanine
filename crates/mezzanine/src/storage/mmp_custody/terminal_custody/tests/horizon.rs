//! Actual r1 commits/recovery against genuine source-owned t1 before-images.

use super::*;

/// Commits actual t1 facts through the same original physical ownership fence.
fn observe(scene: &mut Scene, store: &mut MmpCustodyStore, fence: &RootOwnership) {
    while let Some(ticket) = scene
        .service
        .prepare_storage_expiry(&scene.scope, 16, usize::MAX)
        .unwrap()
    {
        let write = scene
            .service
            .begin_storage_terminal_write(&ticket, &scene.scope)
            .unwrap();
        let plan = fence.bind_store(store).unwrap().terminal_plan().unwrap();
        let commit = {
            let mut owned = fence.bind_store(store).unwrap();
            positive(write.persist(&mut owned.terminal_port(&plan, &scene.scope)))
        };
        scene.service.publish_terminal(&commit).unwrap();
    }
}
fn select(scene: &mut Scene, now: u64) -> (MessageTerminalTicket, MessageTerminalWrite) {
    let ticket = scene
        .service
        .prepare_storage_horizon_release(&scene.scope, now, usize::MAX)
        .unwrap()
        .unwrap();
    let write = scene
        .service
        .begin_storage_terminal_write(&ticket, &scene.scope)
        .unwrap();
    (ticket, write)
}
fn record(store: &MmpCustodyStore) -> (String, Vec<u8>) {
    store
        .connection
        .query_row(
            "SELECT key,value FROM records WHERE key LIKE 'r1:%'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

/// Actual r1 can commit while the live outcome remains unconsumed. A lost reply
/// reconciles the same original bytes and generation; adoption alone releases
/// pending custody, preserving source/t1/allocator and recipient ACK state.
#[test]
fn horizon_actual_commit_reply_loss_reconciles_before_release_adoption() {
    let fixture = Fixture::new();
    let fence = RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "expiry-session").unwrap();
    let mut scene = Scene::new();
    scene.direct(&mut store);
    observe(&mut scene, &mut store, &fence);
    let (ticket, write) = select(&mut scene, 600016);
    let plan = fence
        .bind_store(&mut store)
        .unwrap()
        .terminal_plan()
        .unwrap();
    let unknown = {
        let mut owned = fence.bind_store(&mut store).unwrap();
        uncertain(write.persist(&mut LostReply(owned.terminal_port(&plan, &scene.scope))))
    };
    let before = record(&store);
    let generation = store.current_state().unwrap().generation;
    assert!(before.1.len() <= 916);
    assert_eq!(
        scene
            .service
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        1
    );
    assert_eq!(
        scene
            .service
            .delivery_outcomes_for(&scene.sender.agent_id, 10)
            .unwrap()
            .len(),
        1
    );
    assert!(scene.service.cancel_storage_terminal(&ticket).is_err());
    let write = scene.service.retry_terminal_write(unknown).unwrap();
    let commit = {
        let mut owned = fence.bind_store(&mut store).unwrap();
        positive(write.persist(&mut owned.terminal_port(&plan, &scene.scope)))
    };
    assert_eq!(record(&store), before);
    assert_eq!(store.current_state().unwrap().generation, generation);
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
    let inventory = store.inventory_custody(limits()).unwrap();
    assert_eq!(inventory.occurrences.len(), 1);
    assert_eq!(inventory.expiry_keys.len(), 1);
    assert_eq!(inventory.release_keys.len(), 1);
    assert!(inventory.acknowledgement_keys.is_empty());
    assert_eq!(
        scene
            .service
            .subscription(&scene.target.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
}

/// Cold reconstruction after physical release commit but missing adoption uses
/// actual r1 even with an unreleased checkpoint. A later saved release cannot
/// replace a missing original journal; accepted source and t1 stay intact.
#[test]
fn horizon_reopen_restores_actual_release_and_refuses_missing_journal() {
    let fixture = Fixture::new();
    let fence = RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "expiry-session").unwrap();
    let mut scene = Scene::new();
    scene.direct(&mut store);
    observe(&mut scene, &mut store, &fence);
    let stale = scene.capture(&mut store, &fence);
    let (_, write) = select(&mut scene, 600016);
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
    let (service, _input) = fence
        .bind_store(&mut store)
        .unwrap()
        .reconstruct_cold_service(stale, &scope, Default::default(), 1000, 1024 * 1024)
        .unwrap();
    assert_eq!(
        service.storage_usage().unwrap().unwrap().pending_messages,
        0
    );
    let capture = service.prepare_storage_snapshot(&scope).unwrap();
    let verified = capture
        .write()
        .verify(&mut fence.bind_store(&mut store).unwrap())
        .unwrap();
    let (snapshot, _lease) = capture.finish(verified).unwrap();
    let row = &snapshot.storage_snapshot.as_ref().unwrap().sources[0];
    assert!(!row.pending);
    assert!(row.terminal[0].unwrap().released);
    assert_eq!(row.terminal[0].unwrap().observed_at_ms, 16);
    assert!(snapshot.registered_agents.is_empty());
    snapshot.validate_storage_snapshot().unwrap();
    let (key, bytes) = record(&store);
    let original_digest = Sha256::digest(&bytes);
    let generation = store.current_state().unwrap().generation as u64;
    store
        .commit(
            generation,
            &[CustodyWrite {
                key: &key,
                previous: Some(original_digest.as_slice()),
                value: None,
            }],
        )
        .unwrap();
    assert!(
        fence
            .bind_store(&mut store)
            .unwrap()
            .qualify_recovery_checkpoint(Arc::new(snapshot), limits())
            .is_err()
    );
    let inventory = store.inventory_custody(limits()).unwrap();
    assert_eq!(inventory.occurrences.len(), 1);
    assert_eq!(inventory.expiry_keys.len(), 1);
}

/// A trigger that destroys an unrelated accepted original makes the complete
/// release transaction roll back. Same-plan retry after repair releases only
/// the actual selected outcome and preserves both immutable accepted sources.
#[test]
fn horizon_trigger_non_target_damage_rolls_back_original_release() {
    let fixture = Fixture::new();
    let fence = RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "expiry-session").unwrap();
    let mut scene = Scene::new();
    scene.direct(&mut store);
    scene.accept(
        &mut store,
        "independent",
        Recipient::Agent(scene.target.agent_id.clone()),
        None,
    );
    observe(&mut scene, &mut store, &fence);
    let (_, write) = select(&mut scene, 600016);
    let plan = fence
        .bind_store(&mut store)
        .unwrap()
        .terminal_plan()
        .unwrap();
    let generation = store.current_state().unwrap().generation;
    store.connection.execute_batch("CREATE TRIGGER damage_release AFTER INSERT ON records WHEN NEW.key LIKE 'r1:%' BEGIN DELETE FROM records WHERE key='a1:0000000000000002:commit'; END;").unwrap();
    let unknown = {
        let mut owned = fence.bind_store(&mut store).unwrap();
        uncertain(write.persist(&mut owned.terminal_port(&plan, &scene.scope)))
    };
    assert_eq!(store.current_state().unwrap().generation, generation);
    assert!(
        store
            .inventory_custody(limits())
            .unwrap()
            .release_keys
            .is_empty()
    );
    assert_eq!(
        scene
            .service
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        2
    );
    store
        .connection
        .execute_batch("DROP TRIGGER damage_release")
        .unwrap();
    let write = scene.service.retry_terminal_write(unknown).unwrap();
    let commit = {
        let mut owned = fence.bind_store(&mut store).unwrap();
        positive(write.persist(&mut owned.terminal_port(&plan, &scene.scope)))
    };
    scene.service.publish_terminal(&commit).unwrap();
    assert_eq!(
        scene
            .service
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        1
    );
    assert_eq!(
        store.inventory_custody(limits()).unwrap().occurrences.len(),
        2
    );
}

/// Checksums and a valid generic transaction cannot manufacture release
/// eligibility or original allocation identity. Each changed descriptive frame
/// refuses inventory; restoring the original actual frame restores qualification.
#[test]
fn horizon_rejects_rechecksummed_early_clock_and_foreign_allocation() {
    let fixture = Fixture::new();
    let fence = RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "expiry-session").unwrap();
    let mut scene = Scene::new();
    scene.direct(&mut store);
    observe(&mut scene, &mut store, &fence);
    let (_, write) = select(&mut scene, 600016);
    let plan = fence
        .bind_store(&mut store)
        .unwrap()
        .terminal_plan()
        .unwrap();
    let _commit = {
        let mut owned = fence.bind_store(&mut store).unwrap();
        positive(write.persist(&mut owned.terminal_port(&plan, &scene.scope)))
    };
    let (key, bytes) = record(&store);
    let original_digest = Sha256::digest(&bytes);
    for wrong_clock in [true, false] {
        let mut changed = bytes.clone();
        if wrong_clock {
            changed[40..48].copy_from_slice(&600015u64.to_le_bytes());
        } else {
            changed[8] ^= 1;
        }
        let changed_key = super::super::release::record_key(&changed);
        let changed_digest = Sha256::digest(&changed);
        let generation = store.current_state().unwrap().generation as u64;
        store
            .commit(
                generation,
                &[
                    CustodyWrite {
                        key: &key,
                        previous: Some(original_digest.as_slice()),
                        value: None,
                    },
                    CustodyWrite {
                        key: &changed_key,
                        previous: None,
                        value: Some(&changed),
                    },
                ],
            )
            .unwrap();
        store.current_state().unwrap();
        assert!(store.inventory_custody(limits()).is_err());
        let generation = store.current_state().unwrap().generation as u64;
        store
            .commit(
                generation,
                &[
                    CustodyWrite {
                        key: &changed_key,
                        previous: Some(changed_digest.as_slice()),
                        value: None,
                    },
                    CustodyWrite {
                        key: &key,
                        previous: None,
                        value: Some(&bytes),
                    },
                ],
            )
            .unwrap();
        assert_eq!(
            store
                .inventory_custody(limits())
                .unwrap()
                .release_keys
                .len(),
            1
        );
    }
}

/// A partial release survives close/reopen and a wall rollback. Actual original
/// release history supplies the already observed horizon; another bounded batch
/// can settle remaining slots without restoring raw sender/recipient authority.
#[test]
fn horizon_cold_partial_fanout_preserves_actual_release_clock_on_rollback() {
    let fixture = Fixture::new();
    let fence = RootOwnership::acquire(&fixture.0).unwrap();
    let mut store = MmpCustodyStore::open(&fixture.0, "expiry-session").unwrap();
    let mut scene = Scene::new();
    for _ in 1..70 {
        scene.service.register_agent(None, None, "worker", vec![]);
    }
    scene.accept(
        &mut store,
        "fanout",
        Recipient::Role("worker".into()),
        Some(5),
    );
    observe(&mut scene, &mut store, &fence);
    let stale = scene.capture(&mut store, &fence);
    let (_, write) = select(&mut scene, 600016);
    assert_eq!(write.binding_indices().count(), 64);
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
    let (mut service, _input) = fence
        .bind_store(&mut store)
        .unwrap()
        .reconstruct_cold_service(stale, &scope, Default::default(), 1000, 1024 * 1024)
        .unwrap();
    assert_eq!(
        service.storage_usage().unwrap().unwrap().pending_messages,
        1
    );
    assert_eq!(service.next_storage_horizon_at_ms(&scope).unwrap(), Some(0));
    let ticket = service
        .prepare_storage_horizon_release(&scope, 11, usize::MAX)
        .unwrap()
        .unwrap();
    let write = service
        .begin_storage_terminal_write(&ticket, &scope)
        .unwrap();
    assert_eq!(write.observed_at_ms(), 600016);
    assert_eq!(write.binding_indices().count(), 6);
    let plan = fence
        .bind_store(&mut store)
        .unwrap()
        .terminal_plan()
        .unwrap();
    let commit = {
        let mut owned = fence.bind_store(&mut store).unwrap();
        positive(write.persist(&mut owned.terminal_port(&plan, &scope)))
    };
    assert_eq!(service.publish_terminal(&commit).unwrap(), 6);
    assert_eq!(
        service.storage_usage().unwrap().unwrap().pending_messages,
        0
    );
    assert_eq!(
        store
            .inventory_custody(limits())
            .unwrap()
            .release_keys
            .len(),
        2
    );
}
