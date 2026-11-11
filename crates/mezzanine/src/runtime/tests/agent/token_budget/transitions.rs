//! Actual human conversation commands choose independent accounting identity;
//! copied history must not become source spend or delegated ownership.

use super::*;

/// A post-capture setup failure may discard the new proposal only after actual
/// target teardown. Uncertain teardown retains its exact unresolved capture.
#[tokio::test]
async fn runtime_budget_human_fork_failure_owns_capture_until_proven_teardown() {
    for fault in [5, 6] {
        let mut scene = Scene::new();
        let primary = scene
            .service
            .attach_primary("primary", true, Size::new(120, 30).unwrap(), 120)
            .unwrap();
        scene
            .service
            .start_initial_pane_process(Some("cat >/dev/null"))
            .unwrap();
        mark_test_pane_ready(&mut scene.service, "%1");
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
            .execute_agent_shell_command(&primary, "/fork budget-failed-fork")
            .unwrap();
        let dispatch = scene
            .service
            .take_pending_deferred_agent_commands()
            .pop()
            .unwrap();
        let work = scene
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
        let crate::runtime::RuntimeAgentCommandAsyncOutcome::SavedFork(read) =
            RuntimeSessionService::execute_deferred_agent_command(&work)
        else {
            panic!("actual fork worker must publish its original receipt");
        };
        RuntimeSessionService::fail_saved_fork_adoption_for_tests(&read, fault);
        let adoption = scene.service.adopt_saved_fork_command(&work, &read);
        let outcome = RuntimeSessionService::finish_saved_fork_worker(&read, adoption);
        let crate::runtime::RuntimeAgentCommandAsyncOutcome::Failed { message, .. } = &outcome
        else {
            panic!("post-capture fault must remain a real failure");
        };
        assert!(
            message.contains("injected fork failure after budget capture"),
            "{message}"
        );
        scene
            .service
            .complete_agent_command_work(&work, outcome)
            .unwrap();
        let target_live = scene
            .service
            .agent_shell_store()
            .sessions()
            .any(|session| session.session_id == "budget-failed-fork");
        assert_eq!(target_live, fault == 6);
        assert_eq!(
            scene.service.runtime_budget_capture_count_for_tests(),
            if fault == 6 { 2 } else { 1 }
        );
        assert!(
            scene
                .store
                .budget_membership("budget-failed-fork")
                .unwrap()
                .is_none()
        );
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

/// Genuine new starts an independent account, but must release only the old
/// actor holder so an ordinary return to its saved account remains possible.
#[tokio::test]
async fn runtime_budget_human_new_can_return_to_original_saved_account() {
    let mut scene = Scene::new();
    let primary = scene
        .service
        .attach_primary("primary", true, Size::new(120, 30).unwrap(), 120)
        .unwrap();
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
    let before = scene
        .store
        .budget_membership(&original.conversation_id)
        .unwrap()
        .unwrap();
    scene.service.stop_agent_turn_for_pane("%1").unwrap();
    let response = scene
        .service
        .execute_agent_shell_command(&primary, "/new")
        .unwrap();
    assert!(response.contains("new"), "{response}");
    assert_ne!(
        scene
            .service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .session_id,
        original.conversation_id
    );
    let next = scene.start();
    let work = scene
        .service
        .prepare_agent_provider_work(&next.turn_id)
        .unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
    scene
        .service
        .apply_agent_provider_preparation(outcome)
        .unwrap();
    let after = scene
        .store
        .budget_membership(&next.conversation_id)
        .unwrap()
        .unwrap();
    assert_ne!(after.0.root, before.0.root);
    scene.service.stop_agent_turn_for_pane("%1").unwrap();
    scene
        .service
        .execute_agent_shell_resume_command("%1", &format!("/resume {}", original.conversation_id))
        .unwrap();
    assert_eq!(
        scene
            .service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .session_id,
        original.conversation_id
    );
    assert_eq!(
        scene
            .store
            .budget_membership(&original.conversation_id)
            .unwrap()
            .unwrap(),
        before
    );
}

#[tokio::test]
async fn runtime_budget_human_saved_fork_starts_independent_account() {
    let mut scene = Scene::new();
    let primary = scene
        .service
        .attach_primary("primary", true, Size::new(120, 30).unwrap(), 120)
        .unwrap();
    scene
        .service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    mark_test_pane_ready(&mut scene.service, "%1");
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
    let before = scene
        .store
        .budget_membership(&original.conversation_id)
        .unwrap()
        .unwrap();
    // Synthetic incurred usage through the real repository port qualifies
    // ownership conservation, not an actual provider exchange.
    use crate::storage::token_usage::budget::receipts::{
        BudgetAttempt, BudgetAttemptPurpose, BudgetReceipt,
    };
    use mez_agent::token_budget::{BudgetAttemptId, BudgetReceiptId, UsageSnapshot};
    let source_attempt = BudgetAttempt {
        namespace: before.0.namespace.clone(),
        id: BudgetAttemptId::new("source-spend").unwrap(),
        root: before.0.root.clone(),
        conversation: before.0.conversation.clone(),
        model: mez_agent::ModelTokenUsageKey::new("offline", "source"),
        purpose: BudgetAttemptPurpose::Execution,
        reporting_origin: None,
    };
    scene
        .store
        .register_budget_attempt(&source_attempt)
        .unwrap();
    scene
        .store
        .commit_budget_receipt(&BudgetReceipt {
            id: BudgetReceiptId::new("source-spend-receipt").unwrap(),
            attempt: source_attempt,
            snapshot: UsageSnapshot {
                input: Some(17),
                output: Some(3),
                terminal: true,
            },
            reporting: None,
        })
        .unwrap();
    let spent = scene.store.budget_account(&before.0.root).unwrap();
    scene.service.stop_agent_turn_for_pane("%1").unwrap();
    scene
        .service
        .execute_agent_shell_command(&primary, "/fork budget-human-fork")
        .unwrap();
    let response = scene
        .service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .unwrap();
    assert!(response.contains("forked=true"), "{response}");
    let fork = scene
        .service
        .agent_shell_store()
        .sessions()
        .find(|session| session.session_id == "budget-human-fork")
        .unwrap()
        .clone();
    mark_test_pane_ready(&mut scene.service, &fork.pane_id);
    let started = scene
        .service
        .start_agent_prompt_turn(&fork.pane_id, "qualify independent human fork")
        .unwrap();
    let work = scene
        .service
        .prepare_agent_provider_work(&started.turn_id)
        .unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
    scene
        .service
        .apply_agent_provider_preparation(outcome)
        .unwrap();
    let after = scene
        .store
        .budget_membership(&fork.session_id)
        .unwrap()
        .unwrap();
    assert_eq!(after.1, BudgetMembershipState::Active);
    assert_ne!(after.0.root, before.0.root);
    assert_eq!(after.0.parent, None);
    assert_eq!(scene.store.budget_account(&before.0.root).unwrap(), spent);
    assert_eq!(
        scene
            .store
            .budget_account(&after.0.root)
            .unwrap()
            .known
            .input,
        0
    );
    assert_eq!(
        scene
            .store
            .budget_membership(&original.conversation_id)
            .unwrap()
            .unwrap(),
        before
    );
}
