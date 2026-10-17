//! Qualification capture and retry across the isolated persistence adapter port.
//!
//! This executes the production adapter's immutable metadata payload against its
//! real fixture store and feeds the same typed completion/failure events back.
//! It does not claim a real async worker-thread/scheduler qualification; no live
//! session, provider or user process participates.

use super::*;
use crate::runtime::{PersistenceEvent, RuntimeSideEffect};

/// Executes exactly the store operation and completion mapping used by the
/// metadata worker, synchronously inside this isolated adapter-port fixture.
fn settle_metadata(fixture: &mut CleanupFixture, effect: &RuntimeSideEffect) {
    let RuntimeSideEffect::PersistAgentSessionMetadata {
        store,
        mezzanine_session_id,
        generation,
        records,
        ..
    } = effect
    else {
        panic!("expected metadata work");
    };
    let bytes = store
        .save_agent_session_metadata_checkpoint(mezzanine_session_id, records)
        .unwrap();
    fixture
        .service
        .apply_persistence_transition(PersistenceEvent::AgentSessionMetadataCompleted {
            mezzanine_session_id: mezzanine_session_id.clone(),
            generation: *generation,
            path: store.agent_session_metadata_checkpoint_file(mezzanine_session_id),
            records: records.len(),
            bytes,
        })
        .unwrap();
}

/// The final fresh-spawn snapshot supersedes its early unqualified snapshot in
/// the adapter queue. A real backend failure retries only the exact metadata
/// generation; native identity, child/turn count and ownership never replay.
/// Durable settlement then permits a fresh qualified restore and native close.
#[test]
fn runtime_persistent_close_adapter_qualification_retry_does_not_replay_spawn() {
    let mut fixture = CleanupFixture::new();
    let parent = idle_parent_turn(&mut fixture);
    fixture.service.use_transcript_effect_adapter();
    let child = fixture.spawn("agent-%1");
    let pane = child.strip_prefix("agent-").unwrap();
    let native = fixture.native_identity(&child);
    let owner = fixture.service.persistent_subagent(&child).unwrap().clone();
    let ledger = fixture.service.agent_turn_ledger().clone();
    let panes = fixture.service.tracked_pane_descriptors().len();
    let mut queued = fixture
        .service
        .drain_transcript_persistence_transition()
        .side_effects
        .into_iter()
        .filter(|effect| {
            matches!(
                effect,
                RuntimeSideEffect::PersistAgentSessionMetadata { .. }
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        queued.len(),
        1,
        "newest final snapshot coalesces the early one"
    );
    let effect = queued.pop().unwrap();
    let RuntimeSideEffect::PersistAgentSessionMetadata {
        store,
        mezzanine_session_id,
        generation,
        records,
        retry_attempt,
    } = &effect
    else {
        unreachable!();
    };
    let generation = *generation;
    assert_eq!(*retry_attempt, 0);
    assert!(
        records
            .iter()
            .find(|record| record.pane_id == pane)
            .unwrap()
            .persistent_subagent_owned
    );
    store.fail_next_agent_session_metadata_write();
    let error = store
        .save_agent_session_metadata_checkpoint(mezzanine_session_id, records)
        .unwrap_err();
    let failed = PersistenceEvent::AgentSessionMetadataFailed {
        effect: Box::new(effect.clone()),
        path: store.agent_session_metadata_checkpoint_file(mezzanine_session_id),
        error: error.message().to_string(),
    };
    let retry = fixture
        .service
        .apply_persistence_transition(failed.clone())
        .unwrap()
        .side_effects
        .into_iter()
        .find(|effect| {
            matches!(
                effect,
                RuntimeSideEffect::PersistAgentSessionMetadata { .. }
            )
        })
        .unwrap();
    let RuntimeSideEffect::PersistAgentSessionMetadata {
        generation: retry_generation,
        retry_attempt,
        records: retry_records,
        ..
    } = &retry
    else {
        unreachable!();
    };
    assert_eq!(*retry_generation, generation);
    assert_eq!(*retry_attempt, 1);
    assert_eq!(retry_records, records);
    fixture.assert_native_identity(&child, native);
    assert_eq!(fixture.service.persistent_subagent(&child), Some(&owner));
    assert_eq!(fixture.service.agent_turn_ledger(), &ledger);
    assert_eq!(fixture.service.tracked_pane_descriptors().len(), panes);
    assert!(
        fixture
            .service
            .subagent_cleanup_child_is_owned_by_turn(&child, &parent)
    );
    settle_metadata(&mut fixture, &retry);
    // Superseding metadata must make a later duplicate old failure inert,
    // without suppressing the actual latest immutable snapshot.
    fixture.service.checkpoint_agent_session_metadata().unwrap();
    let latest = fixture
        .service
        .drain_transcript_persistence_transition()
        .side_effects
        .into_iter()
        .find(|effect| {
            matches!(
                effect,
                RuntimeSideEffect::PersistAgentSessionMetadata { .. }
            )
        })
        .unwrap();
    assert!(
        matches!(&latest, RuntimeSideEffect::PersistAgentSessionMetadata { generation: current, .. } if *current > generation)
    );
    let stale = fixture
        .service
        .apply_persistence_transition(failed)
        .unwrap();
    assert!(!stale.side_effects.iter().any(|effect| matches!(
        effect,
        RuntimeSideEffect::PersistAgentSessionMetadata { .. }
    )));
    settle_metadata(&mut fixture, &latest);
    let saved = fixture
        .store
        .load_agent_session_metadata(fixture.service.session().id.as_str())
        .unwrap();
    assert!(
        saved
            .iter()
            .find(|record| record.pane_id == pane)
            .unwrap()
            .persistent_subagent_owned
    );
    restore_fresh_runtime(&mut fixture);
    let native = fixture.native_identity(&child);
    let parent = parent_turn(&mut fixture);
    assert_eq!(
        close_action(&mut fixture.service, &parent, &child).status,
        ActionStatus::Succeeded
    );
    assert!(!fixture.present(&child));
    assert_ne!(
        mez_mux::process::process_start_token_for_pid(native.0),
        Some(native.1)
    );
}
