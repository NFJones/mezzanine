//! Finite actor metadata pressure through actual provider preparation. Synthetic
//! retired slots do not pretend to be thousands of real SQLite publications.

use super::*;

/// Capacity reclamation occurs after the target shell is provisionally bound.
/// A failed checkpoint must still recover the prior account's actual holder.
#[tokio::test]
async fn runtime_budget_reclamation_failed_resume_preserves_original_source_holder() {
    let mut scene = Scene::new();
    let first = scene.start();
    let work = scene
        .service
        .prepare_agent_provider_work(&first.turn_id)
        .unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
    scene
        .service
        .apply_agent_provider_preparation(outcome)
        .unwrap();
    scene.service.stop_agent_turn_for_pane("%1").unwrap();
    let first_reference = scene
        .service
        .runtime_budget_checkpoint_reference(&first.conversation_id)
        .unwrap()
        .unwrap();
    let mut other = Scene::new();
    other.service.set_token_usage_store(scene.store.clone());
    other
        .service
        .set_agent_transcript_store(scene.transcripts.clone());
    let second = other.start();
    let work = other
        .service
        .prepare_agent_provider_work(&second.turn_id)
        .unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
    other
        .service
        .apply_agent_provider_preparation(outcome)
        .unwrap();
    other.service.stop_agent_turn_for_pane("%1").unwrap();
    let second_reference = other
        .service
        .runtime_budget_checkpoint_reference(&second.conversation_id)
        .unwrap()
        .unwrap();
    drop(other);
    scene.service.checkpoint_agent_session_metadata().unwrap();
    let previous = scene.service.agent_shell_store().get("%1").unwrap().clone();
    scene
        .service
        .fill_runtime_budget_captures_for_tests(&first.conversation_id, false);
    scene.transcripts.fail_next_agent_session_metadata_write();
    let error = scene
        .service
        .execute_agent_shell_resume_command("%1", &format!("/resume {}", second.conversation_id))
        .unwrap_err();
    assert!(
        error
            .message()
            .contains("injected agent session metadata write failure"),
        "{error}"
    );
    assert_eq!(scene.service.runtime_budget_capture_count_for_tests(), 1);
    assert_eq!(scene.service.agent_shell_store().get("%1"), Some(&previous));
    assert_eq!(
        scene
            .service
            .runtime_budget_checkpoint_reference(&first.conversation_id)
            .unwrap()
            .as_ref(),
        Some(&first_reference)
    );
    assert!(
        scene
            .store
            .restore_native_budget_membership(&scene.transcripts, &first_reference)
            .is_err()
    );
    let released = scene
        .store
        .restore_native_budget_membership(&scene.transcripts, &second_reference)
        .unwrap();
    drop(released);
}

/// A completed actor capture can be reclaimed while issued work still owns the
/// original inode, but an outstanding preparation must keep its exact entry.
#[tokio::test]
async fn runtime_budget_reclamation_preserves_actual_worker_and_preparation_owners() {
    for preparation in [false, true] {
        let mut scene = Scene::new();
        let original = scene.start();
        let work = scene
            .service
            .prepare_agent_provider_work(&original.turn_id)
            .unwrap();
        let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
        scene
            .service
            .apply_agent_provider_preparation(outcome)
            .unwrap();
        let reference = scene
            .service
            .runtime_budget_checkpoint_reference(&original.conversation_id)
            .unwrap()
            .unwrap();
        let dispatch = (!preparation).then(|| {
            scene
                .service
                .claim_configured_agent_provider_task(
                    &AgentId::opaque(original.agent_id.clone()).unwrap(),
                    &original.turn_id,
                )
                .unwrap()
                .unwrap()
        });
        let pending = if preparation {
            scene
                .service
                .prepare_runtime_budget_membership(&original.turn_id)
                .unwrap()
        } else {
            None
        };
        scene.service.stop_agent_turn_for_pane("%1").unwrap();
        scene
            .service
            .fill_runtime_budget_captures_for_tests(&original.conversation_id, false);
        scene
            .service
            .agent_shell_store_mut()
            .start_new_conversation("%1")
            .unwrap();
        let next = scene.start();
        let work = scene
            .service
            .prepare_agent_provider_work(&next.turn_id)
            .unwrap();
        assert_eq!(
            scene.service.runtime_budget_capture_count_for_tests(),
            if preparation { 2 } else { 1 }
        );
        assert!(
            scene
                .store
                .restore_native_budget_membership(&scene.transcripts, &reference)
                .is_err()
        );
        let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
        scene
            .service
            .apply_agent_provider_preparation(outcome)
            .unwrap();
        if let Some(pending) = pending {
            // Actual late bookkeeping may settle, but its old pane cannot be
            // made ready after a new conversation owns this slot.
            let outcome = pending.execute();
            scene
                .service
                .apply_runtime_budget_membership(outcome)
                .unwrap();
            assert_eq!(
                scene
                    .service
                    .agent_shell_store()
                    .get("%1")
                    .unwrap()
                    .session_id,
                next.conversation_id
            );
        }
        drop(dispatch);
        if !preparation {
            let recovered = scene
                .store
                .restore_native_budget_membership(&scene.transcripts, &reference)
                .unwrap();
            drop(recovered);
        }
    }
}

/// Completed detached captures must not turn the fixed bound into a lifetime
/// conversation limit. Unresolved proposals must remain retained and refuse.
#[tokio::test]
async fn runtime_budget_reclamation_reuses_detached_completed_capacity_only() {
    for unresolved in [false, true] {
        let mut scene = Scene::new();
        let original = scene.start();
        let work = scene
            .service
            .prepare_agent_provider_work(&original.turn_id)
            .unwrap();
        let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
        scene
            .service
            .apply_agent_provider_preparation(outcome)
            .unwrap();
        scene.service.stop_agent_turn_for_pane("%1").unwrap();
        let before = scene
            .store
            .budget_membership(&original.conversation_id)
            .unwrap()
            .unwrap();
        scene
            .service
            .fill_runtime_budget_captures_for_tests(&original.conversation_id, unresolved);
        assert_eq!(scene.service.runtime_budget_capture_count_for_tests(), 4096);
        scene
            .service
            .agent_shell_store_mut()
            .start_new_conversation("%1")
            .unwrap();
        let next = scene.start();
        let prepared = scene.service.prepare_agent_provider_work(&next.turn_id);
        if unresolved {
            assert!(prepared.is_err());
            assert_eq!(scene.service.runtime_budget_capture_count_for_tests(), 4096);
        } else {
            let outcome =
                RuntimeSessionService::execute_agent_provider_preparation(prepared.unwrap()).await;
            scene
                .service
                .apply_agent_provider_preparation(outcome)
                .unwrap();
            assert_eq!(scene.service.runtime_budget_capture_count_for_tests(), 1);
            let after = scene
                .store
                .budget_membership(&next.conversation_id)
                .unwrap()
                .unwrap();
            assert_ne!(before.0.root, after.0.root);
        }
        assert_eq!(
            scene
                .store
                .budget_membership(&original.conversation_id)
                .unwrap()
                .unwrap(),
            before
        );
    }
}
