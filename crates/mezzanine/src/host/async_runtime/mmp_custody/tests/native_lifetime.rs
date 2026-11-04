//! Guarded native provenance at the actual actor/root binding boundary.
//!
//! Genuine shell/session tokens and an accepted original precede replacement
//! and exact rollback. The original actor performs n1/history publication and
//! real worker-loss recovery. Decoded lifetime tuples never restore a live
//! binding or declare a suspended parent retired.

use super::*;

/// Actual native fixture with guarded registration committed by the actor.
async fn scene(
    fixture: &Fixture,
) -> (
    crate::host::async_runtime::AsyncRuntimeSessionActor,
    MmpCustodyRoot,
    mez_agent::AgentTurnRecord,
    SenderIdentity,
) {
    let (_, mut actor, root, turn, target) = super::native_actor::actor_fixture(fixture).await;
    actor
        .enable_message_payload_spill(Default::default())
        .unwrap();
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::NativeContinuityAwaitingAdoption
    );
    (actor, root, turn, target)
}

/// Same reserved decoder policy used by actual storage-only recovery.
fn limits() -> mez_agent::messaging::MessageAcceptanceDecodeLimits {
    mez_agent::messaging::MessageAcceptanceDecodeLimits {
        payload_bytes: 1024 * 1024,
        auxiliary_bytes: 512 * 1024,
        recipient_count: 4096,
        recipient_bytes: 1024 * 1024,
    }
}

/// Latest n1 can move to another real shell binding and back under exact
/// rollback. Both first guarded tuple publications survive, while the accepted
/// original, frozen native sender and lack of terminal/ACK facts remain intact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_lifetime_actor_retains_original_across_binding_replacement_and_rollback() {
    let fixture = Fixture::new(true);
    let (mut actor, _, turn, target) = scene(&fixture).await;
    let sender = mez_core::ids::AgentId::opaque(turn.agent_id.clone()).unwrap();
    let original = super::native_actor::source(&actor, &turn, &target);
    let (reply, response) = tokio::sync::oneshot::channel();
    actor
        .start_tracked_native_message(
            &turn,
            original,
            MessageScope::Session,
            crate::runtime::current_unix_millis(),
            reply,
        )
        .unwrap();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::AwaitingAdoption
    );
    assert!(response.await.unwrap().unwrap().accepted);
    let store =
        MmpCustodyStore::open_existing(&fixture.path, "original-session", Default::default())
            .unwrap();
    let before = store.inventory_custody(limits()).unwrap();
    let source_digest = before.occurrences[0].digest;
    let first = store
        .read_native_continuity(sender.as_str())
        .unwrap()
        .unwrap();
    assert_eq!(before.native_lifetime_keys.len(), 1);
    actor
        .service
        .finish_agent_turn(
            &turn.pane_id,
            &turn.turn_id,
            mez_agent::AgentTurnState::Interrupted,
        )
        .unwrap();
    let original_shell = actor
        .service
        .agent_shell_store()
        .get(&turn.pane_id)
        .unwrap()
        .clone();
    actor
        .service
        .agent_shell_store_mut()
        .bind_conversation(&turn.pane_id, "other-actual-conversation", 0)
        .unwrap();
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::NativeContinuityAwaitingAdoption
    );
    let replacement = store
        .read_native_continuity(sender.as_str())
        .unwrap()
        .unwrap();
    assert_ne!(
        replacement.snapshot.native.owner,
        first.snapshot.native.owner
    );
    assert_eq!(
        store
            .inventory_custody(limits())
            .unwrap()
            .native_lifetime_keys
            .len(),
        2
    );
    actor
        .service
        .agent_shell_store_mut()
        .restore_session(&turn.pane_id, original_shell)
        .unwrap();
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::NativeContinuityAwaitingAdoption
    );
    let restored = store
        .read_native_continuity(sender.as_str())
        .unwrap()
        .unwrap();
    assert_eq!(restored.snapshot.native.owner, first.snapshot.native.owner);
    let after = store.inventory_custody(limits()).unwrap();
    assert_eq!(after.native_lifetime_keys.len(), 2);
    assert_eq!(after.occurrences.len(), 1);
    assert_eq!(after.occurrences[0].digest, source_digest);
    assert!(
        after.expiry_keys.is_empty()
            && after.release_keys.is_empty()
            && after.consumption_keys.is_empty()
            && after.acknowledgement_keys.is_empty()
    );
    let histories = after
        .native_lifetime_keys
        .iter()
        .map(|key| store.read_native_lifetime(key).unwrap().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        histories
            .iter()
            .find(|h| h.native.owner == first.snapshot.native.owner)
            .unwrap()
            .publication_generation,
        first.publication_generation().unwrap()
    );
    assert_eq!(
        histories
            .iter()
            .find(|h| h.native.owner == replacement.snapshot.native.owner)
            .unwrap()
            .publication_generation,
        replacement.publication_generation().unwrap()
    );
    assert!(
        histories
            .iter()
            .all(|h| !h.native.admission_available && !h.native.delivery_available)
    );
    assert_eq!(
        actor
            .service
            .message_service()
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        1
    );
}

/// Actual post-commit worker loss must retain the first native/history plan outside
/// the worker. After real backoff and join, history and n1 reconcile together,
/// without a second generation or an early live continuity marker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_lifetime_actor_reconciles_lost_guarded_publication_after_actual_join() {
    let fixture = Fixture::new(true);
    let (_, mut actor, _, turn, _) = super::native_actor::actor_fixture(&fixture).await;
    actor
        .enable_message_payload_spill(Default::default())
        .unwrap();
    fixture
        .probe
        .panic_after_commit
        .store(true, Ordering::SeqCst);
    let sender = mez_core::ids::AgentId::opaque(turn.agent_id.clone()).unwrap();
    actor.poll_message_custody_for_tests();
    assert!(matches!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::NativeContinuityRetryAt(_)
    ));
    let before = fixture.disk();
    assert_eq!(before, (1, 2));
    assert!(
        !actor
            .service
            .message_service()
            .native_continuity_is_recorded(&sender)
    );
    assert!(
        actor
            .service
            .message_service()
            .storage_usage()
            .unwrap()
            .unwrap()
            .metadata_bytes
            >= 2 * 1024 * 1024
    );
    tokio::time::sleep(Duration::from_millis(1100)).await;
    actor.poll_message_custody_for_tests();
    assert_eq!(
        actor.observe_message_custody_for_tests().await.unwrap(),
        CustodyWorkerStatus::NativeContinuityAwaitingAdoption
    );
    assert_eq!(fixture.disk(), before);
    assert!(
        actor
            .service
            .message_service()
            .native_continuity_is_recorded(&sender)
    );
    let store =
        MmpCustodyStore::open_existing(&fixture.path, "original-session", Default::default())
            .unwrap();
    let inventory = store.inventory_custody(limits()).unwrap();
    assert_eq!(inventory.native_lifetime_keys.len(), 1);
    assert!(inventory.occurrences.is_empty());
}
