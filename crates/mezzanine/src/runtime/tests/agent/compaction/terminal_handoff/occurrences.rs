//! Original execution occurrences survive rewrites and terminal append retries.
//!
//! A current group stays raw behind its exact task root. A later result keeps
//! that group's full chronology ordinal across another typed head publication;
//! no selective summary/certificate is accepted as a second live authority.
//! The historical certificate corruption guard is qualified separately as an
//! explicit private descriptor test, not fabricated runtime summary acceptance.

use super::*;

#[test]
fn late_same_group_result_uses_full_chronology_ordinal_at_terminal_publication() {
    check_late_same_group_result(false);
}

#[test]
fn late_same_group_result_keeps_ordinals_through_candidate_and_append_retry() {
    check_late_same_group_result(true);
}

/// Drive the original chronology/result owners in controlled observation order.
/// Both actions are inert say fixtures; neither an actual external effect nor
/// its completion/termination evidence is invented by this controlled adapter.
fn check_late_same_group_result(adapter: bool) {
    let (mut service, store, turn_id) = typed_source::pending();
    let turn = service.agent_turn_ledger().turn(&turn_id).unwrap().clone();
    let mut response = runtime_say_response(&turn_id, "original two-result execution", false);
    let batch = response.action_batch.as_mut().unwrap();
    batch.actions[0].id = "first-original-result".into();
    let mut second_action = batch.actions[0].clone();
    second_action.id = "late-original-result".into();
    batch.actions.push(second_action);
    let first = mez_agent::ActionResult::succeeded(
        &turn,
        &batch.actions[0],
        vec!["first original evidence".into()],
        None,
    );
    let late = mez_agent::ActionResult::succeeded(
        &turn,
        &batch.actions[1],
        vec!["late original evidence".into()],
        None,
    );
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn_id, &turn.agent_id),
        response,
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: Vec::new(),
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    service
        .append_agent_execution_chronology(&turn, &execution)
        .unwrap();
    let group = service.agent_turn_contexts()[&turn_id]
        .chronology()
        .last()
        .unwrap()
        .execution_group_id()
        .unwrap()
        .clone();
    assert_eq!(
        service
            .commit_settled_action_results_context(&turn_id, &[first])
            .unwrap(),
        1
    );
    let original_group = service.agent_turn_contexts()[&turn_id]
        .chronology()
        .iter()
        .filter(|event| event.execution_group_id() == Some(&group))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(original_group.len(), 2);
    typed_source::queue(&mut service, &turn_id, true);
    typed_source::complete(&mut service, &turn_id, "FIRST_OCCURRENCE_SUMMARY");
    assert!(original_group.iter().all(|event| {
        service.agent_turn_contexts()[&turn_id]
            .chronology()
            .contains(event)
    }));
    service
        .agent_turn_contexts_mut()
        .get_mut(&turn_id)
        .unwrap()
        .append_user_event("steering", "LATE_EXACT_BARRIER")
        .unwrap();
    assert_eq!(
        service
            .commit_settled_action_results_context(&turn_id, std::slice::from_ref(&late))
            .unwrap(),
        1
    );
    let late_event = service.agent_turn_contexts()[&turn_id]
        .chronology()
        .last()
        .unwrap()
        .clone();
    assert_eq!(late_event.execution_group_id(), Some(&group));
    let before_replay = service.agent_turn_contexts()[&turn_id].clone();
    service
        .append_agent_execution_chronology(&turn, &execution)
        .unwrap();
    assert_eq!(
        service
            .commit_settled_action_results_context(&turn_id, &[late])
            .unwrap(),
        0
    );
    assert_eq!(service.agent_turn_contexts()[&turn_id], before_replay);
    typed_source::queue(&mut service, &turn_id, true);
    typed_source::complete(&mut service, &turn_id, "LATE_OCCURRENCE_SUMMARY");
    let live = service.agent_turn_contexts()[&turn_id].clone();
    let occurrences =
        RuntimeSessionService::terminal_execution_occurrences(live.chronology()).unwrap();
    let ordinals = occurrences
        .values()
        .filter_map(|event| match event {
            mez_agent::TranscriptContextEvent::ExecutionBlock {
                execution_group_id: Some(owner),
                ordinal,
                ..
            } if owner == &group => *ordinal,
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(ordinals, [1, 2, 3]);
    assert!(
        original_group
            .iter()
            .all(|event| live.chronology().contains(event))
    );
    assert!(live.chronology().contains(&late_event));
    assert!(
        service
            .terminal_compaction_handoff_content(&turn_id)
            .unwrap()
            .is_none()
    );
    let (_, accepted) = store
        .load_replay_epoch(&turn.conversation_id, mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(accepted.revision().get(), 2);
    service.stop_agent_turn_for_pane("%1").unwrap();
    if adapter {
        typed_source::admit_bookkeeping(&mut service);
        let mut receipts = 0usize;
        for effect in service
            .drain_transcript_persistence_transition()
            .side_effects
        {
            match effect {
                RuntimeSideEffect::PersistTranscriptEntries {
                    store: writer,
                    batch,
                    path,
                } => {
                    writer.fail_transcript_append_after_first();
                    assert!(writer.commit_accounted_transcript_batch(&batch, 1).is_err());
                    let bytes = writer.commit_accounted_transcript_batch(&batch, 1).unwrap();
                    let receipt = crate::runtime::PersistenceEvent::TranscriptCompleted {
                        conversation_id: batch.conversation_id().into(),
                        first_sequence: batch.first_sequence(),
                        batch,
                        path,
                        bytes,
                    };
                    let mut stale = receipt.clone();
                    if let crate::runtime::PersistenceEvent::TranscriptCompleted {
                        first_sequence,
                        ..
                    } = &mut stale
                    {
                        *first_sequence += 100;
                    }
                    assert!(!service.apply_persistence_transition(stale).unwrap().applied);
                    assert!(
                        service
                            .apply_persistence_transition(receipt.clone())
                            .unwrap()
                            .applied
                    );
                    assert!(
                        !service
                            .apply_persistence_transition(receipt)
                            .unwrap()
                            .applied
                    );
                    receipts += 1;
                }
                effect => service.queue_transcript_for_tests(effect),
            }
        }
        assert!(receipts > 0);
    } else {
        typed_source::settle(&mut service);
    }
    assert!(
        service
            .persistence
            .pending_transcript_entries(&turn.conversation_id)
            .is_empty()
    );
    let archive = store.inspect(&turn.conversation_id).unwrap();
    let ordinals = archive
        .iter()
        .filter_map(|row| {
            match mez_agent::TranscriptContextEvent::from_transcript_content(&row.content) {
                Some(mez_agent::TranscriptContextEvent::ExecutionBlock {
                    execution_group_id: Some(owner),
                    ordinal,
                    ..
                }) if owner == group => ordinal,
                _ => None,
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(ordinals, [1, 2, 3]);
    assert!(!archive.iter().any(|row| {
        row.content
            .starts_with(crate::storage::transcript::compaction_handoff::MARKER)
    }));
    service.set_agent_transcript_store(AgentTranscriptStore::new(store.root()));
    let replay = service
        .runtime_agent_history_epoch_context("%1")
        .unwrap()
        .replay_owner()
        .unwrap()
        .unwrap();
    assert_eq!(replay.head(), accepted.head());
    assert_eq!(replay.revision(), accepted.revision());
    assert_eq!(
        replay
            .context()
            .chronology()
            .iter()
            .filter(|event| event.record() == late_event.record())
            .count(),
        1
    );
    assert_eq!(
        replay
            .context()
            .chronology()
            .iter()
            .find(|event| event.record() == late_event.record())
            .unwrap(),
        &late_event
    );
    assert_eq!(
        replay
            .context()
            .blocks()
            .iter()
            .filter(|block| block.content == "LATE_EXACT_BARRIER")
            .count(),
        1
    );
    assert!(
        !replay
            .context()
            .blocks()
            .iter()
            .any(|block| block.content.contains("LIVE_ORIGINAL_SOURCE")
                || block.content == "FIRST_OCCURRENCE_SUMMARY")
    );
    assert_eq!(store.inspect(&turn.conversation_id).unwrap(), archive);
}

/// Genuine current publication produces no legacy certificate. The retained
/// historical capture guard still refuses duplicate/conflicting private witness
/// descriptors before touching canonical source or any archive/epoch bytes.
#[test]
fn contradictory_or_duplicate_witness_sources_fail_before_certificate_capture() {
    for changed in [true, false] {
        let (mut service, store, turn_id) = typed_source::pending();
        let turn = service.agent_turn_ledger().turn(&turn_id).unwrap().clone();
        let source = service.agent_turn_contexts()[&turn_id]
            .chronology()
            .iter()
            .filter(|event| event.execution_group_id().is_some())
            .cloned()
            .collect::<Vec<_>>();
        assert!(!source.is_empty());
        typed_source::queue(&mut service, &turn_id, true);
        typed_source::complete(&mut service, &turn_id, "SOURCE_GUARD_SUMMARY");
        assert!(
            service
                .terminal_compaction_handoff_content(&turn_id)
                .unwrap()
                .is_none()
        );
        let before_context = service.agent_turn_contexts()[&turn_id].clone();
        let archive = store.inspect(&turn.conversation_id).unwrap();
        let path = store
            .root()
            .join(&turn.conversation_id)
            .join("compaction-epoch.json");
        let manifest = fs::read(&path).unwrap();
        let witness = crate::runtime::agent_state::RuntimeCompactionWitness {
            anchor: source[0].sequence().get(),
            source: source.clone(),
            archived_sources: Default::default(),
            summary: "historical private summary".into(),
            summary_record: before_context.chronology()[0].record(),
        };
        let mut duplicate = witness.clone();
        if changed {
            let target = source[0].sequence().get();
            let group = source[0].execution_group_id().unwrap().clone();
            let mut forged = mez_agent::AgentContext::new_durable(vec![ContextBlock::user_event(
                "seed", "seed",
            )])
            .unwrap();
            while forged.event_sequence_high_water_mark() < target {
                forged
                    .append_assistant_event(
                        "PRIVATE_CONTRADICTORY_LABEL",
                        "PRIVATE_CONTRADICTORY_SOURCE",
                        group.clone(),
                    )
                    .unwrap();
            }
            duplicate.source[0] = forged.chronology().last().unwrap().clone();
        }
        let handoff = crate::runtime::agent_state::RuntimeTurnCompactionHandoff {
            baseline: None,
            witnesses: vec![witness, duplicate],
        };
        let error = RuntimeSessionService::terminal_compaction_handoff_from_source(
            &before_context,
            &handoff,
        )
        .unwrap_err();
        assert_eq!(error.kind(), crate::MezErrorKind::Conflict);
        assert!(
            error
                .message()
                .contains("duplicate or conflicting terminal compaction source"),
            "{error}"
        );
        assert!(!error.message().contains("PRIVATE_CONTRADICTORY"));
        assert_eq!(service.agent_turn_contexts()[&turn_id], before_context);
        assert_eq!(store.inspect(&turn.conversation_id).unwrap(), archive);
        assert_eq!(fs::read(&path).unwrap(), manifest);
        assert!(service.take_replay_epoch_attempt_for_tests().is_none());
    }
}
