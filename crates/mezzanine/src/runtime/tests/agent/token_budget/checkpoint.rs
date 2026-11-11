//! Actual active-checkpoint capture and runtime restore, with exact stores and
//! kernel lifetime custody. No provider or historical action is replayed.

use super::*;

/// Adapter capture queues an immutable obligation, not a durable ACK. Failed
/// writes retry that same reference/generation without republishing membership.
#[tokio::test]
async fn runtime_budget_checkpoint_adapter_retry_preserves_original_reference() {
    use crate::runtime::{PersistenceEvent, RuntimeSideEffect};
    let mut scene = Scene::new();
    scene.service.use_transcript_effect_adapter();
    let turn = scene.start();
    let work = scene
        .service
        .prepare_agent_provider_work(&turn.turn_id)
        .unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
    scene
        .service
        .apply_agent_provider_preparation(outcome)
        .unwrap();
    let effect = scene
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
    let RuntimeSideEffect::PersistAgentSessionMetadata {
        store,
        mezzanine_session_id,
        generation,
        records,
        ..
    } = &effect
    else {
        unreachable!()
    };
    let reference = records[0].budget_reference.clone().unwrap();
    assert_eq!(reference.conversation, turn.conversation_id);
    let prior = store
        .load_agent_session_metadata(mezzanine_session_id)
        .unwrap();
    assert!(prior.iter().all(|record| record.budget_reference.is_none()));
    store.fail_next_agent_session_metadata_write();
    assert!(
        store
            .save_agent_session_metadata_checkpoint(mezzanine_session_id, records)
            .is_err()
    );
    let retry = scene
        .service
        .apply_persistence_transition(PersistenceEvent::AgentSessionMetadataFailed {
            effect: Box::new(effect.clone()),
            path: store.agent_session_metadata_checkpoint_file(mezzanine_session_id),
            error: "fixture refusal".into(),
        })
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
        records: retry_records,
        retry_attempt,
        ..
    } = retry
    else {
        unreachable!()
    };
    assert_eq!(retry_generation, *generation);
    assert_eq!(retry_attempt, 1);
    assert_eq!(retry_records, *records);
    store
        .save_agent_session_metadata_checkpoint(mezzanine_session_id, &retry_records)
        .unwrap();
    assert_eq!(
        store
            .load_agent_session_metadata(mezzanine_session_id)
            .unwrap()[0]
            .budget_reference
            .as_ref(),
        Some(&reference)
    );
    assert_eq!(
        scene
            .store
            .budget_membership(&turn.conversation_id)
            .unwrap()
            .unwrap()
            .0
            .operation,
        reference.operation.as_str()
    );
}

async fn publish(
    scene: &mut Scene,
) -> (AgentTurnRecord, mez_agent::transcript::AgentSessionMetadata) {
    let turn = scene.start();
    let work = scene
        .service
        .prepare_agent_provider_work(&turn.turn_id)
        .unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
    scene
        .service
        .apply_agent_provider_preparation(outcome)
        .unwrap();
    scene.service.stop_agent_turn_for_pane("%1").unwrap();
    scene.service.checkpoint_agent_session_metadata().unwrap();
    let metadata = scene
        .transcripts
        .load_agent_session_metadata(scene.service.session().id.as_str())
        .unwrap()
        .remove(0);
    assert!(metadata.budget_reference.is_some());
    (turn, metadata)
}

/// A genuine checkpoint captures the original account. A second runtime cannot
/// replace its own live pane until the old holder retires; then normal hydration
/// keeps that same account, but never restores the old native shell identity.
#[tokio::test]
async fn runtime_budget_checkpoint_restore_requires_retirement_and_preserves_account() {
    // Declared before both runtimes so unwind retires their holders before the
    // transferred original store root can be removed.
    let root;
    let mut source = Scene::new();
    let (turn, mut metadata) = publish(&mut source).await;
    let member = source
        .store
        .budget_membership(&turn.conversation_id)
        .unwrap()
        .unwrap()
        .0;
    let original_native = metadata.native_message_incarnation;
    let mut target = Scene::new();
    let previous = target
        .service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .clone();
    metadata.mezzanine_session_id = target.service.session().id.as_str().into();
    target.service.set_token_usage_store(source.store.clone());
    target
        .service
        .set_agent_transcript_store(source.transcripts.clone());
    source
        .transcripts
        .save_agent_session_metadata_checkpoint(&metadata.mezzanine_session_id, &[metadata.clone()])
        .unwrap();
    assert!(
        target
            .service
            .restore_agent_sessions_from_transcript_store()
            .is_err()
    );
    assert_eq!(
        target.service.agent_shell_store().get("%1").unwrap(),
        &previous
    );
    let store = source.store.clone();
    let transcripts = source.transcripts.clone();
    // Move the exact owned root out so the original service can retire while
    // the durable source survives until the target service is destroyed.
    root = std::mem::replace(&mut source._root, OwnedRoot::new());
    drop(source);
    assert_eq!(
        target
            .service
            .restore_agent_sessions_from_transcript_store()
            .unwrap(),
        1
    );
    let restored = target.service.agent_shell_store().get("%1").unwrap();
    assert_eq!(restored.session_id, turn.conversation_id);
    assert_ne!(
        Some(restored.native_message_incarnation.snapshot_identity()),
        original_native
    );
    assert_eq!(
        store
            .budget_membership(&turn.conversation_id)
            .unwrap()
            .unwrap()
            .0,
        member
    );
    let checkpoint = transcripts
        .load_agent_session_metadata(target.service.session().id.as_str())
        .unwrap()
        .remove(0);
    assert_eq!(checkpoint.budget_reference, metadata.budget_reference);
    // Native custody is retained even while production assembly remains off.
    assert!(
        store
            .restore_native_budget_membership(
                &transcripts,
                checkpoint.budget_reference.as_ref().unwrap()
            )
            .is_err()
    );
    drop(target);
    let recovered = store
        .restore_native_budget_membership(
            &transcripts,
            checkpoint.budget_reference.as_ref().unwrap(),
        )
        .unwrap();
    drop(recovered);
    drop(root);
}

/// Hydration failure after genuine native preflight releases only its temporary
/// holder, preserves the old pane, and never acknowledges the incomplete history.
#[tokio::test]
async fn runtime_budget_checkpoint_failed_hydration_releases_preflight_custody() {
    qualify_failed_hydration(false).await;
}

/// The late failure occurs after the replacement shell and presentation install,
/// not only during history prevalidation. The old binding must still survive.
#[tokio::test]
async fn runtime_budget_checkpoint_post_install_failure_rolls_back_binding() {
    qualify_failed_hydration(true).await;
}

async fn qualify_failed_hydration(after_install: bool) {
    let root;
    let mut source = Scene::new();
    let (_, mut metadata) = publish(&mut source).await;
    let mut target = Scene::new();
    let previous = target
        .service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .clone();
    metadata.mezzanine_session_id = target.service.session().id.as_str().into();
    if after_install {
        target.service.fail_budget_restore_after_install_for_tests();
    } else {
        metadata.transcript_entries = u64::MAX;
    }
    target.service.set_token_usage_store(source.store.clone());
    target
        .service
        .set_agent_transcript_store(source.transcripts.clone());
    source
        .transcripts
        .save_agent_session_metadata_checkpoint(&metadata.mezzanine_session_id, &[metadata.clone()])
        .unwrap();
    let store = source.store.clone();
    let transcripts = source.transcripts.clone();
    root = std::mem::replace(&mut source._root, OwnedRoot::new());
    drop(source);
    assert!(
        target
            .service
            .restore_agent_sessions_from_transcript_store()
            .is_err()
    );
    assert_eq!(
        target.service.agent_shell_store().get("%1").unwrap(),
        &previous
    );
    assert_eq!(
        transcripts
            .load_agent_session_metadata(&metadata.mezzanine_session_id)
            .unwrap(),
        vec![metadata.clone()]
    );
    let original = store
        .restore_native_budget_membership(&transcripts, metadata.budget_reference.as_ref().unwrap())
        .unwrap();
    drop(original);
    drop(target);
    drop(root);
}

/// A missing checkpoint reference cannot turn already-budgeted history into a
/// legacy unlimited account. Refusal leaves the previous pane and disk row exact.
#[tokio::test]
async fn runtime_budget_checkpoint_writers_refuse_provenance_loss() {
    let mut scene = Scene::new();
    let (_, mut metadata) = publish(&mut scene).await;
    let id = metadata.mezzanine_session_id.clone();
    let before = fs::read(
        scene
            .transcripts
            .agent_session_metadata_checkpoint_file(&id),
    )
    .unwrap();
    metadata.budget_reference = None;
    assert!(
        scene
            .transcripts
            .save_agent_session_metadata_checkpoint(&id, &[metadata.clone()])
            .is_err()
    );
    assert!(
        scene
            .transcripts
            .save_agent_session_metadata(&id, &[metadata])
            .is_err()
    );
    assert_eq!(
        fs::read(
            scene
                .transcripts
                .agent_session_metadata_checkpoint_file(&id)
        )
        .unwrap(),
        before
    );
}

/// Corrupt input bypasses the writer deliberately; restore still refuses it.
#[tokio::test]
async fn runtime_budget_checkpoint_missing_reference_preserves_previous_binding() {
    let mut source = Scene::new();
    let (_, mut metadata) = publish(&mut source).await;
    let mut target = Scene::new();
    metadata.mezzanine_session_id = target.service.session().id.as_str().into();
    let previous = target
        .service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .clone();
    target.service.set_token_usage_store(source.store.clone());
    target
        .service
        .set_agent_transcript_store(source.transcripts.clone());
    source
        .transcripts
        .save_agent_session_metadata_checkpoint(&metadata.mezzanine_session_id, &[metadata.clone()])
        .unwrap();
    // Deliberately corrupt the exact task-owned checkpoint behind both writer
    // fences; the reader must still refuse before touching the previous pane.
    let path = source
        .transcripts
        .agent_session_metadata_checkpoint_file(&metadata.mezzanine_session_id);
    let mut envelope: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let row = envelope["rows"][0].as_str().unwrap();
    let (prefix, _) = row.rsplit_once('\t').unwrap();
    envelope["rows"][0] = format!("{prefix}\tnull").into();
    fs::write(path, serde_json::to_vec(&envelope).unwrap()).unwrap();
    metadata.budget_reference = None;
    assert!(
        target
            .service
            .restore_agent_sessions_from_transcript_store()
            .is_err()
    );
    assert_eq!(
        target.service.agent_shell_store().get("%1").unwrap(),
        &previous
    );
    assert_eq!(
        source
            .transcripts
            .load_agent_session_metadata(&metadata.mezzanine_session_id)
            .unwrap(),
        vec![metadata]
    );
    drop(target);
}
