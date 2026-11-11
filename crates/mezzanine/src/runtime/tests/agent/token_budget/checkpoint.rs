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

/// Direct `/resume` must reacquire the original account without relying on a
/// startup checkpoint restore or making provider preparation repair the bind.
#[tokio::test]
async fn runtime_budget_ordinary_resume_retains_original_account() {
    let root;
    let mut source = Scene::new();
    let (turn, metadata) = publish(&mut source).await;
    let mut target = Scene::new();
    let store = source.store.clone();
    let transcripts = source.transcripts.clone();
    target.service.set_token_usage_store(store.clone());
    target
        .service
        .set_agent_transcript_store(transcripts.clone());
    let previous = target
        .service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .clone();
    let command = format!("/resume {}", turn.conversation_id);
    assert!(
        target
            .service
            .execute_agent_shell_resume_command("%1", &command)
            .is_err()
    );
    assert_eq!(
        target.service.agent_shell_store().get("%1"),
        Some(&previous)
    );
    root = std::mem::replace(&mut source._root, OwnedRoot::new());
    drop(source);
    target
        .service
        .execute_agent_shell_resume_command("%1", &command)
        .unwrap();
    assert_eq!(
        target
            .service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .session_id,
        turn.conversation_id
    );
    assert_eq!(
        target
            .service
            .runtime_budget_checkpoint_reference(&turn.conversation_id)
            .unwrap(),
        metadata.budget_reference
    );
    assert!(
        store
            .restore_native_budget_membership(
                &transcripts,
                metadata.budget_reference.as_ref().unwrap()
            )
            .is_err()
    );
    drop(target);
    let recovered = store
        .restore_native_budget_membership(&transcripts, metadata.budget_reference.as_ref().unwrap())
        .unwrap();
    drop(recovered);
    drop(root);
}

/// Same-pane resume can borrow only its actual native holder; failed adoption
/// must restore that exact shell/holder rather than leave the new incarnation.
#[tokio::test]
async fn runtime_budget_ordinary_resume_rolls_back_provisional_adoption() {
    let mut scene = Scene::new();
    let (turn, metadata) = publish(&mut scene).await;
    let before = scene.service.agent_shell_store().get("%1").unwrap().clone();
    let command = format!("/resume {}", turn.conversation_id);
    for checkpoint_failure in [false, true] {
        if checkpoint_failure {
            scene.transcripts.fail_next_agent_session_metadata_write();
        } else {
            scene
                .service
                .fail_next_agent_resume_after_authority_restore_for_tests();
        }
        assert!(
            scene
                .service
                .execute_agent_shell_resume_command("%1", &command)
                .is_err()
        );
        assert_eq!(scene.service.agent_shell_store().get("%1"), Some(&before));
        assert_eq!(
            scene
                .service
                .runtime_budget_checkpoint_reference(&turn.conversation_id)
                .unwrap(),
            metadata.budget_reference
        );
        assert!(
            scene
                .store
                .restore_native_budget_membership(
                    &scene.transcripts,
                    metadata.budget_reference.as_ref().unwrap()
                )
                .is_err()
        );
    }
    scene
        .service
        .execute_agent_shell_resume_command("%1", &command)
        .unwrap();
    assert_ne!(
        scene
            .service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .native_message_incarnation,
        before.native_message_incarnation
    );
    assert_eq!(
        scene
            .service
            .runtime_budget_checkpoint_reference(&turn.conversation_id)
            .unwrap(),
        metadata.budget_reference
    );
}

/// The real deferred claim/read/completion port must adopt worker-held custody;
/// an inline-only fix would leave the user-facing command unqualified.
#[tokio::test]
async fn runtime_budget_ordinary_resume_deferred_worker_retains_custody() {
    for stale in [false, true] {
        let root;
        let mut source = Scene::new();
        let (turn, metadata) = publish(&mut source).await;
        let mut target = Scene::new();
        let store = source.store.clone();
        let transcripts = source.transcripts.clone();
        target.service.set_token_usage_store(store.clone());
        target
            .service
            .set_agent_transcript_store(transcripts.clone());
        root = std::mem::replace(&mut source._root, OwnedRoot::new());
        drop(source);
        let primary = target
            .service
            .attach_primary("budget-resume", true, Size::new(120, 40).unwrap(), 120)
            .unwrap();
        target
            .service
            .execute_agent_shell_command(&primary, &format!("/resume {}", turn.conversation_id))
            .unwrap();
        let dispatch = target
            .service
            .take_pending_deferred_agent_commands()
            .pop()
            .unwrap();
        let work = target
            .service
            .claim_agent_command_work(
                &dispatch.primary_client_id,
                &dispatch.pane_id,
                &dispatch.command,
                &dispatch.input,
                dispatch.claim_generation,
                &dispatch.conversation_id,
            )
            .unwrap()
            .unwrap();
        let outcome = RuntimeSessionService::execute_deferred_agent_command(&work);
        assert!(matches!(
            outcome,
            crate::runtime::RuntimeAgentCommandAsyncOutcome::DirectResume { .. }
        ));
        // The read outcome also owns the replay conversation lock. Inspect only
        // the lifetime inode here, rather than recursively acquire that lock.
        let inode = fs::File::open(
            transcripts
                .root()
                .join(&turn.conversation_id)
                .join("budget-binding.lock"),
        )
        .unwrap();
        assert_eq!(
            rustix::fs::flock(&inode, rustix::fs::FlockOperation::NonBlockingLockExclusive),
            Err(rustix::io::Errno::WOULDBLOCK)
        );
        drop(inode);
        if stale {
            target
                .service
                .agent_shell_store_mut()
                .bind_conversation("%1", "replacement-before-adoption", 0)
                .unwrap();
            assert!(
                !target
                    .service
                    .complete_agent_command_work(&work, outcome)
                    .unwrap()
            );
            drop(work);
            assert_eq!(
                target
                    .service
                    .agent_shell_store()
                    .get("%1")
                    .unwrap()
                    .session_id,
                "replacement-before-adoption"
            );
            let recovered = store
                .restore_native_budget_membership(
                    &transcripts,
                    metadata.budget_reference.as_ref().unwrap(),
                )
                .unwrap();
            drop(recovered);
            drop(target);
            drop(root);
            continue;
        }
        assert!(
            target
                .service
                .complete_agent_command_work(&work, outcome)
                .unwrap()
        );
        drop(work);
        assert_eq!(
            target
                .service
                .runtime_budget_checkpoint_reference(&turn.conversation_id)
                .unwrap(),
            metadata.budget_reference
        );
        assert!(
            store
                .restore_native_budget_membership(
                    &transcripts,
                    metadata.budget_reference.as_ref().unwrap()
                )
                .is_err()
        );
        drop(target);
        drop(root);
    }
}

/// Leaving a pane must retire only its actor holder. A later resume can return
/// to A after B, unless actual originally issued work still holds A's inode.
#[tokio::test]
async fn runtime_budget_ordinary_resume_round_trip_retires_only_actor_holder() {
    for (held_worker, deferred) in [(false, false), (true, false), (false, true)] {
        let mut scene = Scene::new();
        let (original, metadata) = publish(&mut scene).await;
        let primary = deferred.then(|| {
            scene
                .service
                .attach_primary("budget-round-trip", true, Size::new(120, 40).unwrap(), 120)
                .unwrap()
        });
        let dispatch = if held_worker {
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
            let dispatch = scene
                .service
                .claim_configured_agent_provider_task(
                    &AgentId::opaque(turn.agent_id.clone()).unwrap(),
                    &turn.turn_id,
                )
                .unwrap()
                .unwrap();
            scene.service.stop_agent_turn_for_pane("%1").unwrap();
            Some(dispatch)
        } else {
            None
        };
        scene
            .transcripts
            .append(&TranscriptEntry {
                conversation_id: "legacy-round-trip".into(),
                sequence: 1,
                created_at_unix_seconds: 1,
                role: TranscriptRole::User,
                turn_id: "legacy-turn".into(),
                agent_id: "agent-%1".into(),
                pane_id: "%1".into(),
                content: "unbudgeted saved control".into(),
            })
            .unwrap();
        if let Some(primary) = &primary {
            deferred_resume(&mut scene.service, primary, "legacy-round-trip");
        } else {
            scene
                .service
                .execute_agent_shell_resume_command("%1", "/resume legacy-round-trip")
                .unwrap();
        }
        let command = format!("/resume {}", original.conversation_id);
        if held_worker {
            assert!(
                scene
                    .service
                    .execute_agent_shell_resume_command("%1", &command)
                    .is_err()
            );
            assert_eq!(
                scene
                    .service
                    .agent_shell_store()
                    .get("%1")
                    .unwrap()
                    .session_id,
                "legacy-round-trip"
            );
        }
        drop(dispatch);
        if let Some(primary) = &primary {
            deferred_resume(&mut scene.service, primary, &original.conversation_id);
        } else {
            scene
                .service
                .execute_agent_shell_resume_command("%1", &command)
                .unwrap();
        }
        assert_eq!(
            scene
                .service
                .runtime_budget_checkpoint_reference(&original.conversation_id)
                .unwrap(),
            metadata.budget_reference
        );
    }
}

/// Executes the actual deferred claim/read/adoption sequence, dropping its
/// immutable work only after actor completion has consumed the outcome.
fn deferred_resume(
    service: &mut RuntimeSessionService,
    primary: &mez_core::ids::ClientId,
    conversation: &str,
) {
    service
        .execute_agent_shell_command(primary, &format!("/resume {conversation}"))
        .unwrap();
    let dispatch = service
        .take_pending_deferred_agent_commands()
        .pop()
        .unwrap();
    let work = service
        .claim_agent_command_work(
            &dispatch.primary_client_id,
            &dispatch.pane_id,
            &dispatch.command,
            &dispatch.input,
            dispatch.claim_generation,
            &dispatch.conversation_id,
        )
        .unwrap()
        .unwrap();
    let outcome = RuntimeSessionService::execute_deferred_agent_command(&work);
    assert!(matches!(
        outcome,
        crate::runtime::RuntimeAgentCommandAsyncOutcome::DirectResume { .. }
    ));
    assert!(service.complete_agent_command_work(&work, outcome).unwrap());
    assert_eq!(
        service.agent_shell_store().get("%1").unwrap().session_id,
        conversation
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
