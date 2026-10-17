//! Fresh persistent spawn commits durable qualification without a later prompt.
//!
//! No test-added checkpoint may hide the ordering between successful setup,
//! durable qualification and final native cleanup-capability publication.

use super::*;

mod adapter;

/// Immediately after the actual idle spawn returns, the durable checkpoint
/// already qualifies its exact relationship. A genuinely fresh runtime then
/// restores both native owners and can completely close that same owned child.
#[test]
fn runtime_persistent_close_fresh_spawn_is_qualified_without_later_checkpoint() {
    let mut fixture = CleanupFixture::new();
    let child = fixture.spawn("agent-%1");
    let pane = child.strip_prefix("agent-").unwrap();
    // Intentionally no prompt, checkpoint, or other mutation between spawn and
    // the persisted marker assertion: the spawn owner must publish this itself.
    let records = fixture
        .store
        .load_agent_session_metadata(fixture.service.session().id.as_str())
        .unwrap();
    let record = records
        .iter()
        .find(|record| record.pane_id == pane)
        .unwrap();
    assert!(record.persistent_subagent_owned);
    assert_eq!(
        record.conversation_id,
        fixture
            .service
            .persistent_subagent(&child)
            .unwrap()
            .conversation_id
    );
    restore_fresh_runtime(&mut fixture);
    assert!(fixture.service.persistent_subagent(&child).is_some());
    let native = fixture.native_identity(&child);
    let parent = parent_turn(&mut fixture);
    assert!(
        fixture
            .service
            .subagent_cleanup_child_is_owned_by_turn(&child, &parent)
    );
    assert_eq!(
        close_action(&mut fixture.service, &parent, &child).status,
        ActionStatus::Succeeded
    );
    assert!(!fixture.present(&child));
    assert_ne!(
        mez_mux::process::process_start_token_for_pid(native.0),
        Some(native.1)
    );
    assert_eq!(
        close_action(&mut fixture.service, &parent, &child).status,
        ActionStatus::Succeeded
    );
}

/// Synchronous failure of the final checkpoint occurs before proof publication. The real
/// spawn owner removes its allocated child and authority, without a completed
/// close tombstone or Local grant; the consumed nonce is never rewound/reused.
#[test]
fn runtime_persistent_close_fresh_checkpoint_failure_rolls_back_before_proof() {
    let mut fixture = CleanupFixture::without_parent_process();
    let before = fixture
        .service
        .local_cleanup_parent_state_for_tests("agent-%1");
    fixture
        .service
        .fail_next_fresh_subagent_checkpoint_for_tests();
    assert!(fixture.try_spawn("agent-%1").is_err());
    let after = fixture
        .service
        .local_cleanup_parent_state_for_tests("agent-%1");
    assert!(after.0.is_none());
    assert!(after.1 > before.1, "reservation remains consumed");
    assert_eq!(fixture.service.tracked_pane_descriptors().len(), 1);
    // The first child identity belongs only to this isolated fresh fixture.
    assert!(
        fixture
            .service
            .primary_pid_for_live_pane_process("%2")
            .is_none()
    );
    assert!(fixture.service.persistent_subagent("agent-%2").is_none());
    assert!(
        fixture
            .service
            .retired_persistent_subagent("agent-%2")
            .is_none()
    );
    let records = fixture
        .store
        .load_agent_session_metadata(fixture.service.session().id.as_str())
        .unwrap();
    assert!(
        records
            .iter()
            .all(|record| record.pane_id == "%1" && !record.persistent_subagent_owned)
    );
    let child = fixture.spawn("agent-%1");
    let current = fixture
        .service
        .local_cleanup_parent_state_for_tests("agent-%1");
    assert!(current.0.is_some_and(|generation| generation > after.1));
    let native = fixture.native_identity(&child);
    fixture.close("agent-%1");
    assert!(!fixture.present(&child));
    assert_ne!(
        mez_mux::process::process_start_token_for_pid(native.0),
        Some(native.1)
    );
}
