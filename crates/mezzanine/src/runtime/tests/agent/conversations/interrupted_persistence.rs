//! Stop/continue persistence must retain each original occurrence exactly once.

use super::*;
use mez_agent::transcript::TranscriptContextRecord;

/// Runs original checked candidates, real append batches and their completion
/// events. Display effects keep their existing owner and are irrelevant here.
fn admit_bookkeeping(service: &mut RuntimeSessionService) {
    for work in service.claim_bookkeeping_candidates() {
        let history = work.execute();
        assert!(
            service
                .complete_bookkeeping_candidate(work, history)
                .unwrap()
        );
    }
}

fn settle_appends(service: &mut RuntimeSessionService) {
    for effect in service
        .drain_transcript_persistence_transition()
        .side_effects
    {
        match effect {
            RuntimeSideEffect::PersistTranscriptEntries { store, batch, path } => {
                let bytes = store
                    .commit_accounted_transcript_batch(&batch, batch.first_sequence())
                    .unwrap_or_else(|error| panic!("append {}..{}: {error}; rows={:?}", batch.first_sequence(), batch.entries().last().unwrap().sequence, batch.entries().iter().map(|row| (row.sequence, &row.turn_id, mez_agent::transcript::TranscriptContextRecord::from_transcript_content(&row.content).ok().flatten().map(|record| record.record().occurrence_id()))).collect::<Vec<_>>()));
                service
                    .apply_persistence_transition(
                        crate::runtime::PersistenceEvent::TranscriptCompleted {
                            conversation_id: batch.conversation_id().into(),
                            first_sequence: batch.first_sequence(),
                            batch,
                            path,
                            bytes,
                        },
                    )
                    .unwrap();
            }
            RuntimeSideEffect::PersistAgentSessionMetadata {
                store,
                mezzanine_session_id,
                records,
                ..
            } => {
                store
                    .save_agent_session_metadata_checkpoint(&mezzanine_session_id, &records)
                    .unwrap();
            }
            effect => service.queue_transcript_for_tests(effect),
        }
    }
}

/// Exercises actual stop, next-prompt admission, archive append and cold restore.
/// Both a stop before a response and a stop with grouped execution must persist
/// the new prompt, without reattributing old task roots or execution descriptors.
#[test]
fn runtime_interrupted_continuation_persists_each_occurrence_once_and_restarts() {
    for (deferred, with_execution) in [(false, false), (false, true), (true, false), (true, true)] {
        let store = AgentTranscriptStore::new(temp_root("continued-occurrence-persistence"));
        let mut service = test_runtime_service();
        service.set_agent_transcript_store(store.clone());
        if deferred {
            service.persistence.enable_transcript_adapter();
        }
        service
            .start_initial_pane_process(Some("cat >/dev/null"))
            .unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        let first = service
            .start_agent_prompt_turn("%1", "original prompt")
            .unwrap();
        let conversation = service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .session_id
            .clone();
        if with_execution {
            // Runtime hints are intentionally not exported. This gap must not
            // hide later original executions from cross-turn deduplication.
            service
                .agent_turn_contexts_mut()
                .get_mut(&first.turn_id)
                .unwrap()
                .append_reference_event(
                    ContextSourceKind::RuntimeHint,
                    "provider retry scheduled",
                    "retry hint",
                )
                .unwrap();
            service
                .agent_turn_contexts_mut()
                .get_mut(&first.turn_id)
                .unwrap()
                .append_assistant_event(
                    "settled assistant",
                    "original observation",
                    mez_agent::ContextExecutionGroupId::new("original-execution").unwrap(),
                )
                .unwrap();
        }
        service.stop_agent_turn_for_pane("%1").unwrap();
        if deferred {
            admit_bookkeeping(&mut service);
        }
        let original = if deferred {
            assert!(store.inspect(&conversation).is_err());
            service
                .persistence
                .pending_transcript_entries(&conversation)
        } else {
            store.inspect(&conversation).unwrap()
        };
        let second = service
            .start_agent_prompt_turn("%1", "continue with correction")
            .unwrap();
        if deferred {
            settle_appends(&mut service);
        }
        if with_execution {
            crate::test_support::compaction_source::complete_task(
                &mut service,
                &second.turn_id,
                "corrected task completed",
                mez_agent::ProviderApiCompatibility::OpenAiResponses,
            );
        } else {
            service.stop_agent_turn_for_pane("%1").unwrap();
        }
        if deferred {
            admit_bookkeeping(&mut service);
            settle_appends(&mut service);
        }
        let rows = store.inspect(&conversation).unwrap();
        assert_eq!(&rows[..original.len()], original);
        let mut occurrences = std::collections::BTreeSet::new();
        let mut prompts = Vec::new();
        for row in &rows {
            if let Some(record) =
                TranscriptContextRecord::from_transcript_content(&row.content).unwrap()
            {
                assert!(
                    occurrences.insert(record.record().occurrence_id()),
                    "duplicate occurrence at row {}",
                    row.sequence
                );
                if let mez_agent::TranscriptContextEvent::TaskAdmission { content, .. } =
                    record.event()
                {
                    prompts.push((row.turn_id.as_str(), content.as_str().to_owned()));
                }
            }
        }
        assert_eq!(
            prompts,
            [
                (first.turn_id.as_str(), "original prompt".into()),
                (second.turn_id.as_str(), "continue with correction".into())
            ]
        );
        assert!(store.pending_append_receipts().unwrap().is_empty());
        service.checkpoint_agent_session_metadata().unwrap();
        if deferred {
            settle_appends(&mut service);
        }
        service.terminate_all_pane_processes().unwrap();
        let mut restarted = test_runtime_service();
        restarted.session.id = service.session().id.clone();
        restarted.set_agent_transcript_store(store.clone());
        assert_eq!(
            restarted
                .restore_agent_sessions_from_transcript_store()
                .unwrap(),
            1
        );
        restarted
            .agent_context_for_pane_prompt("%1", "after restart", 0)
            .unwrap();
        fs::remove_dir_all(store.root()).unwrap();
    }
}
