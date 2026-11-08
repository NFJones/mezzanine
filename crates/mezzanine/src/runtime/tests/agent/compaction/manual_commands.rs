//! Manual command admission, steering, cancellation and conversation binding.

use super::*;

/// Verifies overlapping compaction attempts are rejected before they can start
/// another model request for the same pane.
#[test]
fn runtime_agent_shell_compact_rejects_overlapping_pane_compaction() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.mark_agent_compacting_for_tests("%1", 1);

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compact-overlap","method":"agent/shell/command","params":{"idempotency_key":"compact-overlap","input":"/compact"}}"#,
        &primary,
    );

    assert!(response.contains("cannot mutate pane state"), "{response}");
}

/// Verifies manual compaction owns the pane: plain text is retained for the
/// post-compaction epoch and conversation replacement is rejected meanwhile.
#[test]
fn runtime_manual_compaction_queues_steering_and_blocks_conversation_mutation() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.mark_agent_compacting_for_tests("%1", 1);
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();

    let prompt = service
        .execute_agent_shell_command(&primary, "first steering prompt")
        .unwrap();
    assert!(prompt.contains("\"command\":\"compacting\""), "{prompt}");
    assert!(service.pending_agent_provider_tasks().is_empty());
    service
        .execute_agent_shell_command(&primary, "second steering prompt")
        .unwrap();

    let blocked = service
        .execute_agent_shell_command(&primary, "/new")
        .unwrap();
    assert!(blocked.contains("cannot mutate pane state"), "{blocked}");
    assert_eq!(
        service.agent_shell_store().get("%1").unwrap().session_id,
        conversation_id
    );
    let stopped = service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    assert!(stopped.contains("compaction_cancelled=true"), "{stopped}");
    assert!(!service.agent_is_compacting("%1"));
    let resumed = service.take_pending_agent_prompt_history_for_tests();
    assert_eq!(resumed.len(), 1);
    assert_eq!(
        resumed[0].prompt,
        "first steering prompt\n\nsecond steering prompt"
    );
    assert!(service.agent_command_is_active("%1"));
    assert!(service.take_agent_compaction_steering("%1").is_empty());
    assert_eq!(resumed[0].steering_receipts.len(), 2);
    let ids = resumed[0]
        .steering_receipts
        .iter()
        .map(|receipt| receipt.id.clone())
        .collect::<Vec<_>>();
    assert_ne!(ids[0], ids[1]);
    assert!(service.claim_agent_prompt_history_preparation(&resumed[0]));
    let history = execute_runtime_agent_prompt_history_work(resumed[0].history_work.clone());
    assert!(
        service
            .complete_agent_prompt_history_preparation(&resumed[0], history)
            .unwrap()
    );
    assert!(
        !service
            .complete_agent_prompt_history_preparation(
                &resumed[0],
                execute_runtime_agent_prompt_history_work(resumed[0].history_work.clone())
            )
            .unwrap()
    );
    let receipts = service.steering_receipts_for_tests("turn-1");
    assert_eq!(
        receipts
            .iter()
            .map(|receipt| receipt.id.clone())
            .collect::<Vec<_>>(),
        ids
    );
    assert_eq!(receipts[0].sequence, receipts[1].sequence);
    assert!(receipts[0].sequence > 0);
    let event = service
        .agent_turn_contexts()
        .get("turn-1")
        .unwrap()
        .chronology()
        .iter()
        .find(|event| event.sequence().get() == receipts[0].sequence)
        .unwrap();
    assert_eq!(
        event.block().content,
        "first steering prompt\n\nsecond steering prompt"
    );
}

/// Verifies cancelling a real queued compaction releases manual steering on
/// the unchanged replay epoch and ignores a late result from that generation.
#[test]
fn runtime_manual_compaction_cancellation_ignores_late_provider_result() {
    // Literal historical reference needs no invented execution completion.
    // The shared fixture admits the actual manual canonical operation.
    let (mut service, store) = manual_model::queued("manual-cancel", "cancel source".into());
    let archive = store.inspect("manual-cancel").unwrap();
    let primary = service.session().layout_owner_client_id().unwrap().clone();
    let generation = service
        .pending_agent_compaction_task_generation("%1")
        .expect("queued compaction generation");
    let steering = service
        .execute_agent_shell_command(&primary, "continue after cancellation")
        .unwrap();
    assert!(
        steering.contains("\"command\":\"compacting\""),
        "{steering}"
    );

    let stopped = service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    assert!(stopped.contains("compaction_cancelled=true"), "{stopped}");
    assert!(!service.agent_is_compacting("%1"));
    let resumed = service.take_pending_agent_prompt_history_for_tests();
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0].prompt, "continue after cancellation");

    let late = service
        .apply_agent_compaction_transition(crate::runtime::AgentCompactionEvent::Completed {
            pane_id: "%1".to_string(),
            task_generation: generation,
            response: Box::new(runtime_test_compaction_response("must be ignored")),
        })
        .unwrap();
    assert!(!late.applied);
    assert!(!service.agent_is_compacting("%1"));
    assert_eq!(
        service.take_pending_agent_prompt_history_for_tests().len(),
        0
    );
    assert_eq!(service.agent_turn_ledger().turns().len(), 0);
    assert_eq!(store.inspect("manual-cancel").unwrap(), archive);
    assert!(store.compaction_epoch("manual-cancel").unwrap().is_none());
}

/// Verifies prompts retained for one conversation are discarded when the pane
/// is rebound, rather than being resumed in a later conversation's compaction.
#[test]
fn runtime_manual_compaction_steering_does_not_cross_conversation_rebind() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let original_conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    service
        .queue_agent_compaction_steering(
            "%1",
            primary.clone(),
            original_conversation.clone(),
            service.agent_compaction_epoch("%1"),
            "private old-conversation instruction".to_string(),
            "private old-conversation instruction".to_string(),
        )
        .unwrap();

    let rebound = service
        .execute_agent_shell_command(&primary, "/new")
        .unwrap();
    assert!(rebound.contains("\"command\":\"new\""), "{rebound}");
    assert_ne!(
        service.agent_shell_store().get("%1").unwrap().session_id,
        original_conversation
    );
    assert!(service.take_agent_compaction_steering("%1").is_empty());

    service.mark_agent_compacting_for_tests("%1", 1);
    service.cancel_current_agent_compaction_task("%1");
    assert!(!service.resume_agent_compaction_steering("%1").unwrap());
    assert!(
        service
            .take_pending_agent_prompt_history_for_tests()
            .is_empty()
    );
}
