//! Product witness capture and terminal publication share occurrence numbering.
//!
//! A group is closed at a consumed compaction frontier, not permanently sealed
//! against later evidence. These tests use the real compaction completion and
//! runtime result producer; they do not assert a historical provider schedule.

use super::*;

/// Ordinary runtime ownership rejects evidence without its compacted assistant.
/// Preserve that rejection, then qualify the canonical API's valid same-group
/// assistant/evidence shape with a distinct accepted summary after an exact barrier.
/// Certificates must match actual terminal rows, classify the split group as
/// unsupported before append, and keep contiguous per-group ordinals even when
/// the witness containing the later evidence is not the first witness.
#[test]
fn late_same_group_result_uses_full_chronology_ordinal_at_terminal_publication() {
    check_late_same_group_result(false);
}

/// Adapter-backed terminal cleanup must retain the same exact source map and
/// unsupported split-group disposition through candidate adoption, a partial
/// append, exact retry, stale receipt and duplicate receipt handling. None of
/// those persistence operations may replay an action or fabricate an epoch.
#[test]
fn late_same_group_result_keeps_ordinals_through_candidate_and_append_retry() {
    check_late_same_group_result(true);
}

/// Qualifies the canonical same-group shape separately from the tested ordinary
/// runtime rejection. A split group across an exact barrier uses raw authority.
fn check_late_same_group_result(adapter: bool) {
    let (mut service, store, turn_id) = live_fixture();
    compact_live(&mut service, &turn_id, "FIRST_OCCURRENCE_SUMMARY", false);
    compact_live(&mut service, &turn_id, "OTHER_GROUP_SUMMARY", false);
    let turn = service.agent_turn_ledger().turn(&turn_id).unwrap().clone();
    let mut response = runtime_say_response(&turn_id, &"late accepted result ".repeat(200), false);
    let action = &mut response.action_batch.as_mut().unwrap().actions[0];
    action.id = "late-result-in-existing-group".into();
    let result = mez_agent::ActionResult::succeeded(
        &turn,
        action,
        vec!["settled late evidence ".repeat(200)],
        None,
    );
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn_id, &turn.agent_id),
        response,
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: vec![result.clone()],
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    service
        .append_agent_execution_chronology(&turn, &execution)
        .unwrap();
    let group = service
        .agent_turn_contexts()
        .get(&turn_id)
        .unwrap()
        .chronology()
        .last()
        .unwrap()
        .execution_group_id()
        .unwrap()
        .clone();
    assert_eq!(
        service
            .commit_settled_action_results_context(&turn_id, std::slice::from_ref(&result))
            .unwrap(),
        1
    );
    compact_live(&mut service, &turn_id, "OWNED_OCCURRENCE_SUMMARY", false);
    // Action ownership survives accepted compaction, but ordinary provider
    // reacceptance must not recreate a missing assistant or replay its action.
    assert!(
        service
            .append_agent_execution_chronology(&turn, &execution)
            .is_err()
    );
    service
        .agent_turn_contexts_mut()
        .get_mut(&turn_id)
        .unwrap()
        .append_user_event("steering", "LATE_EXACT_BARRIER")
        .unwrap();
    let before_late = service.agent_turn_contexts().get(&turn_id).unwrap().clone();
    let error = service
        .commit_settled_action_results_context(&turn_id, std::slice::from_ref(&result))
        .unwrap_err();
    assert!(
        error
            .message()
            .contains("preceding owning assistant execution")
    );
    assert_eq!(
        service.agent_turn_contexts().get(&turn_id),
        Some(&before_late)
    );
    // The ordinary runtime guard above is intentional. The canonical API also
    // supports a new assistant occurrence in that group; qualify its accepted
    // witness shape without claiming an ordinary provider generated this reuse.
    service
        .agent_turn_contexts_mut()
        .get_mut(&turn_id)
        .unwrap()
        .append_assistant_event(
            "late same-group source",
            "late accepted source ".repeat(200),
            group.clone(),
        )
        .unwrap();
    assert_eq!(
        service
            .commit_settled_action_results_context(&turn_id, &[result])
            .unwrap(),
        1
    );
    compact_live(&mut service, &turn_id, "LATE_OCCURRENCE_SUMMARY", false);
    let certificate = service
        .terminal_compaction_handoff_content(&turn_id)
        .unwrap()
        .unwrap();
    let proof: crate::storage::transcript::compaction_handoff::TerminalCompactionHandoff =
        serde_json::from_str(
            certificate
                .strip_prefix(crate::storage::transcript::compaction_handoff::MARKER)
                .unwrap(),
        )
        .unwrap();
    assert!(proof.replacements.len() >= 2);
    assert_eq!(
        proof.replacements.last().unwrap().summary,
        "LATE_OCCURRENCE_SUMMARY"
    );
    let certificate_ordinals = proof
        .replacements
        .iter()
        .flat_map(|replacement| &replacement.sources)
        .filter_map(|content| {
            match mez_agent::TranscriptContextEvent::from_transcript_content(content) {
                Some(mez_agent::TranscriptContextEvent::ExecutionBlock {
                    execution_group_id: Some(owner),
                    ordinal,
                    ..
                }) if owner == group => ordinal,
                _ => None,
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(certificate_ordinals, [1, 2, 3, 4]);
    let late_ordinals = proof
        .replacements
        .last()
        .unwrap()
        .sources
        .iter()
        .map(|content| {
            match mez_agent::TranscriptContextEvent::from_transcript_content(content).unwrap() {
                mez_agent::TranscriptContextEvent::ExecutionBlock {
                    ordinal: Some(ordinal),
                    ..
                } => ordinal,
                _ => panic!("expected typed source occurrence"),
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(late_ordinals, [3, 4]);
    if adapter {
        service.use_transcript_effect_adapter();
    }
    complete_turn(&mut service, &turn_id);
    if adapter {
        let work = service.claim_bookkeeping_candidates().pop().unwrap();
        let checked = work.execute();
        assert!(
            service
                .complete_bookkeeping_candidate(work, checked)
                .unwrap()
        );
        assert!(
            !service
                .persistence
                .pending_transcript_entries(&turn.conversation_id)
                .is_empty()
        );
        let effects = service
            .drain_transcript_persistence_transition()
            .side_effects;
        let mut receipts = 0;
        for effect in effects {
            if let RuntimeSideEffect::PersistTranscriptEntries {
                store: writer,
                entries,
                path,
            } = effect
            {
                writer.fail_transcript_append_after_first();
                assert!(writer.append_many(&entries).is_err());
                let bytes = writer.append_many(&entries).unwrap();
                let receipt = crate::runtime::PersistenceEvent::TranscriptCompleted {
                    conversation_id: turn.conversation_id.clone(),
                    first_sequence: entries[0].sequence,
                    entries,
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
        }
        assert!(receipts > 0);
        assert!(
            service
                .persistence
                .pending_transcript_entries(&turn.conversation_id)
                .is_empty()
        );
    }
    let archive = store.inspect(&turn.conversation_id).unwrap();
    let expected_group = group;
    let ordinals = archive
        .iter()
        .filter_map(|row| {
            match mez_agent::TranscriptContextEvent::from_transcript_content(&row.content) {
                Some(mez_agent::TranscriptContextEvent::ExecutionBlock {
                    execution_group_id: Some(group),
                    ordinal,
                    ..
                }) if group == expected_group => ordinal,
                _ => None,
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(ordinals, [1, 2, 3, 4]);
    assert!(
        store
            .compaction_epoch(&turn.conversation_id)
            .unwrap()
            .is_none()
    );
    assert!(
        archive
            .iter()
            .any(|row| row.content.contains("noncontiguous_archive_source"))
    );
    assert!(!archive.iter().any(|row| {
        row.content
            .starts_with(crate::storage::transcript::compaction_handoff::MARKER)
    }));
    service.set_agent_transcript_store(AgentTranscriptStore::new(store.root()));
    let replay = service.runtime_agent_history_epoch_context("%1").unwrap();
    assert_eq!(
        replay
            .blocks
            .iter()
            .filter(|block| block.content == "LATE_EXACT_BARRIER")
            .count(),
        1
    );
    assert!(
        replay
            .blocks
            .iter()
            .any(|block| block.content.contains("LIVE_ORIGINAL_SOURCE"))
    );
    // Unsupported summary projection is explicit; the raw source remains
    // authoritative rather than pretending these local summaries committed.
    assert!(
        !replay
            .blocks
            .iter()
            .any(|block| block.content == "LATE_OCCURRENCE_SUMMARY")
    );
    assert_eq!(store.inspect(&turn.conversation_id).unwrap(), archive);
    service.terminate_all_pane_processes().unwrap();
    drop(service);
    std::fs::remove_dir_all(store.root()).unwrap();
}

/// Distinct accepted witnesses cannot silently substitute their source payloads
/// at a shared sequence, even when an exact duplicate would encode identically.
/// Deliberately corrupt only the private witness fixture after real acceptance;
/// certificate generation must fail before any archive/epoch/candidate mutation,
/// preserve live source and omit private payloads from its conflict diagnostic.
#[test]
fn contradictory_or_duplicate_witness_sources_fail_before_certificate_capture() {
    for changed in [true, false] {
        let (mut service, store, turn_id) = live_fixture();
        compact_live(&mut service, &turn_id, "SOURCE_GUARD_SUMMARY_A", false);
        compact_live(&mut service, &turn_id, "SOURCE_GUARD_SUMMARY_B", false);
        let turn = service.agent_turn_ledger().turn(&turn_id).unwrap().clone();
        let before_context = service.agent_turn_contexts().get(&turn_id).unwrap().clone();
        let archive_path = store.transcript_path(&turn.conversation_id).unwrap();
        let before_archive = archive_path
            .exists()
            .then(|| std::fs::read(&archive_path).unwrap());
        let before_epoch = store.compaction_epoch(&turn.conversation_id).unwrap();
        let handoff = service
            .agent
            .agent_turn_compaction_handoffs
            .get_mut(&turn_id)
            .unwrap();
        assert_eq!(handoff.witnesses.len(), 2);
        let mut substituted = handoff.witnesses[0].source.clone();
        if changed {
            let target = substituted[0].sequence().get();
            let group = substituted[0].execution_group_id().unwrap().clone();
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
            assert_eq!(forged.event_sequence_high_water_mark(), target);
            substituted[0] = forged.chronology().last().unwrap().clone();
        }
        handoff.witnesses[1].source = substituted;
        let error = service
            .terminal_compaction_handoff_content(&turn_id)
            .unwrap_err();
        assert_eq!(error.kind(), crate::MezErrorKind::Conflict);
        assert!(
            error
                .message()
                .contains("duplicate or conflicting terminal compaction source")
        );
        assert!(!error.message().contains("PRIVATE_CONTRADICTORY"));
        assert_eq!(
            service.agent_turn_contexts().get(&turn_id),
            Some(&before_context)
        );
        assert_eq!(
            archive_path
                .exists()
                .then(|| std::fs::read(&archive_path).unwrap()),
            before_archive
        );
        assert_eq!(
            store.compaction_epoch(&turn.conversation_id).unwrap(),
            before_epoch
        );
        assert!(
            !service
                .persistence
                .bookkeeping_pending(&turn.conversation_id)
        );
        service.terminate_all_pane_processes().unwrap();
        drop(service);
        std::fs::remove_dir_all(store.root()).unwrap();
    }
}
