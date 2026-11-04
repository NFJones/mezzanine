//! Cold end evidence overrides stale optional checkpoint and private presence.

use super::*;

/// The actual original publishes an optional checkpoint and private native
/// context, then journals the genuine namespace end. Cold preparation must keep
/// its unacknowledged mail without restoring the ended allocation, even though
/// saved native labels and the private context still match its earlier n1.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registration_end_native_startup_does_not_revive_ended_checkpoint_binding() {
    let fixture = Fixture::build(true, Original::EndedSnapshot).await;
    assert!(fixture.payload.message_state.is_some());
    let mut runtime = fixture.runtime();
    let _prepared = prepare(
        &mut runtime,
        fixture.startup(),
        &fixture.config,
        Default::default(),
    )
    .await
    .unwrap();
    assert!(
        runtime
            .message_service()
            .registered_identity(&fixture.native)
            .is_none()
    );
    assert!(
        !runtime
            .message_service()
            .native_continuity_is_recorded(&fixture.native)
    );
    assert_ne!(
        runtime
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .native_message_incarnation
            .snapshot_identity(),
        fixture.epoch
    );
    let usage = runtime.message_service().storage_usage().unwrap().unwrap();
    assert_eq!(usage.pending_messages, 1);
    assert_eq!(runtime.message_service().next_message_sequence(), 3);
    let store = crate::storage::mmp_custody::MmpCustodyStore::open_existing(
        &root_path(&fixture.config, runtime.session().id.as_str()),
        runtime.session().id.as_str(),
        Default::default(),
    )
    .unwrap();
    let inventory = store
        .inventory_custody(mez_agent::messaging::MessageAcceptanceDecodeLimits {
            payload_bytes: 1024 * 1024,
            auxiliary_bytes: 512 * 1024,
            recipient_count: 4096,
            recipient_bytes: 1024 * 1024,
        })
        .unwrap();
    assert_eq!(inventory.registration_end_keys.len(), 1);
    assert_eq!(inventory.occurrences.len(), 2);
    assert!(
        inventory.expiry_keys.is_empty()
            && inventory.release_keys.is_empty()
            && inventory.consumption_keys.is_empty()
    );
}
