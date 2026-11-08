//! Actual compaction keeps original source ownership across store replacement.
//!
//! The competing archive deliberately has the same conversation, source IDs and
//! group metadata with valid revised bodies. A current-store lookup cannot
//! masquerade as the earlier prompt's source, even before compactor dispatch.

use super::*;

/// Both historical codecs retain original source before/after dispatch for
/// both triggers. Real task lifecycle supplies eligibility; the original typed
/// publication, archive and cold replay cannot use a competing installed store.
#[test]
fn compaction_uses_original_history_before_and_after_store_replacement() {
    for accounted in [false, true] {
        for observed in [false, true] {
            for after_queue in [false, true] {
                let (mut service, original, turn_id) = typed_source::saved(accounted);
                let conversation = service
                    .agent_turn_ledger()
                    .turn(&turn_id)
                    .unwrap()
                    .conversation_id
                    .clone();
                let original_rows = original.inspect(&conversation).unwrap();
                let replacement =
                    AgentTranscriptStore::new(temp_root("compaction-replacement-source"));
                let mut replacement_rows = original_rows.clone();
                // Keep the same physical IDs/group metadata with valid revised
                // bodies; all actual later task rows retain their original bytes.
                for row in &mut replacement_rows[..2] {
                    let source = if row.sequence == 1 {
                        ContextSourceKind::TranscriptAssistant
                    } else {
                        ContextSourceKind::ActionResult
                    };
                    let label = if row.sequence == 1 {
                        "same decision"
                    } else {
                        "same result"
                    };
                    let event = mez_agent::TranscriptContextEvent::execution_block_with_metadata(
                        source,
                        label,
                        "REPLACEMENT_SOURCE_MUST_NOT_BE_ADOPTED",
                        mez_agent::ContextExecutionGroupId::new("live-group-0").unwrap(),
                        row.sequence,
                        None,
                    )
                    .unwrap();
                    row.content = if let Some(saved) =
                        mez_agent::transcript::TranscriptContextRecord::from_transcript_content(
                            &row.content,
                        )
                        .unwrap()
                    {
                        let block = ContextBlock::reference_event(
                            source,
                            label,
                            "REPLACEMENT_SOURCE_MUST_NOT_BE_ADOPTED",
                        );
                        mez_agent::transcript::TranscriptContextRecord::new(
                            event,
                            saved.record().revised(&block).unwrap(),
                            None,
                        )
                        .unwrap()
                        .to_transcript_content()
                        .unwrap()
                    } else {
                        event.to_transcript_content()
                    };
                }
                replacement.append_many(&replacement_rows).unwrap();
                replacement
                    .save_compaction_epoch(
                        &conversation,
                        2,
                        "REPLACEMENT_EPOCH_MUST_NOT_BE_ADOPTED",
                    )
                    .unwrap();
                let replacement_epoch = replacement.compaction_epoch(&conversation).unwrap();
                if after_queue {
                    typed_source::queue(&mut service, &turn_id, observed);
                }
                service.set_agent_transcript_store(replacement.clone());
                if !after_queue {
                    typed_source::queue(&mut service, &turn_id, observed);
                }
                let task = service
                    .pending_agent_compaction_task_for_tests("%1")
                    .unwrap();
                assert_eq!(
                    task.source_owner.as_ref().unwrap().store.as_ref(),
                    Some(&original)
                );
                let input = crate::test_support::compaction_source::rendered_source(task).unwrap();
                assert!(input.contains("ORIGINAL_IMPORTED_PREFIX"));
                assert!(input.contains("LIVE_ORIGINAL_SOURCE"));
                assert!(!input.contains("REPLACEMENT_SOURCE_MUST_NOT_BE_ADOPTED"));
                assert!(!input.contains("REPLACEMENT_EPOCH_MUST_NOT_BE_ADOPTED"));
                typed_source::complete(&mut service, &turn_id, "ORIGINAL_SOURCE_SUMMARY");
                assert!(service.agent_turn_is_running(&turn_id));
                assert!(
                    service
                        .pending_agent_compaction_task_for_tests("%1")
                        .is_none()
                );
                let (_, cold) = AgentTranscriptStore::new(original.root())
                    .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
                    .unwrap()
                    .unwrap();
                assert_eq!(cold.revision().get(), 2);
                assert_eq!(
                    cold.context().chronology(),
                    service.agent_turn_contexts()[&turn_id].chronology()
                );
                assert_eq!(original.inspect(&conversation).unwrap(), original_rows);
                assert_eq!(
                    replacement.inspect(&conversation).unwrap(),
                    replacement_rows
                );
                assert_eq!(
                    replacement.compaction_epoch(&conversation).unwrap(),
                    replacement_epoch
                );
                assert!(
                    service
                        .terminal_compaction_handoff_content(&turn_id)
                        .unwrap()
                        .is_none()
                );
            }
        }
    }
}

/// Actual authored source accepted after dispatch keeps its exact-path append
/// owner. The publication retains it once while ignoring a conflicting valid
/// occurrence at another store; neither pending owner receives an append ACK.
#[test]
fn compaction_refresh_uses_original_pending_path_after_store_replacement() {
    let (mut service, original, turn_id) = typed_source::saved(true);
    let turn = service.agent_turn_ledger().turn(&turn_id).unwrap().clone();
    let conversation = turn.conversation_id.clone();
    let source = original.inspect(&conversation).unwrap();
    typed_source::queue(&mut service, &turn_id, true);
    let replacement = AgentTranscriptStore::new(temp_root("compaction-pending-replacement"));
    replacement.append_many(&source).unwrap();
    service.persistence.enable_transcript_adapter();
    // This is a new real occurrence on the original canonical body, not text
    // imported from a later store or a reconstructed native/task completion.
    let context = service.agent_turn_contexts_mut().get_mut(&turn_id).unwrap();
    context
        .append_user_event("steering", "ORIGINAL_PENDING_TAIL")
        .unwrap();
    let event = context.chronology().last().unwrap().clone();
    let pending = TranscriptEntry {
        conversation_id: conversation.clone(),
        sequence: source.last().unwrap().sequence + 1,
        created_at_unix_seconds: 2,
        role: TranscriptRole::System,
        turn_id: turn_id.clone(),
        agent_id: turn.agent_id,
        pane_id: turn.pane_id,
        content: mez_agent::transcript::TranscriptContextRecord::new(
            mez_agent::TranscriptContextEvent::user_event(
                event.sequence().get(),
                "steering",
                "ORIGINAL_PENDING_TAIL",
            )
            .unwrap(),
            event.record(),
            None,
        )
        .unwrap()
        .to_transcript_content()
        .unwrap(),
    };
    service.persistence.queue_transcript(
        crate::runtime::RuntimeSideEffect::PersistTranscriptEntries {
            path: original.transcript_path(&conversation).unwrap(),
            store: original.clone(),
            batch: crate::storage::transcript::TranscriptAppendBatch::owned_rows(vec![
                pending.clone(),
            ])
            .unwrap(),
        },
    );
    service
        .agent_shell_store_mut()
        .record_transcript_append("%1", std::slice::from_ref(&pending))
        .unwrap();
    let mut foreign = pending.clone();
    let mut block = event.block().clone();
    block.content = "REPLACEMENT_PENDING_MUST_NOT_BE_ADOPTED".into();
    foreign.content = mez_agent::transcript::TranscriptContextRecord::new(
        mez_agent::TranscriptContextEvent::user_event(
            event.sequence().get(),
            "steering",
            &block.content,
        )
        .unwrap(),
        event.record().revised(&block).unwrap(),
        None,
    )
    .unwrap()
    .to_transcript_content()
    .unwrap();
    service.persistence.queue_transcript(
        crate::runtime::RuntimeSideEffect::PersistTranscriptEntries {
            path: replacement.transcript_path(&conversation).unwrap(),
            store: replacement.clone(),
            batch: crate::storage::transcript::TranscriptAppendBatch::owned_rows(vec![
                foreign.clone(),
            ])
            .unwrap(),
        },
    );
    service.set_agent_transcript_store(replacement.clone());
    typed_source::complete(&mut service, &turn_id, "PENDING_OWNER_SUMMARY");
    assert!(service.agent_turn_is_running(&turn_id));
    let context = &service.agent_turn_contexts()[&turn_id];
    assert_eq!(
        context
            .chronology()
            .iter()
            .filter(|candidate| *candidate == &event)
            .count(),
        1
    );
    assert!(!context.blocks().iter().any(|block| {
        block
            .content
            .contains("REPLACEMENT_PENDING_MUST_NOT_BE_ADOPTED")
    }));
    assert_eq!(
        service
            .persistence
            .pending_transcript_entries_at_path(
                &original.transcript_path(&conversation).unwrap(),
                &conversation,
            )
            .unwrap(),
        vec![pending]
    );
    assert_eq!(
        service
            .persistence
            .pending_transcript_entries_at_path(
                &replacement.transcript_path(&conversation).unwrap(),
                &conversation,
            )
            .unwrap(),
        vec![foreign]
    );
    assert_eq!(original.inspect(&conversation).unwrap(), source);
    assert_eq!(replacement.inspect(&conversation).unwrap(), source);
    let (_, cold) = AgentTranscriptStore::new(original.root())
        .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(cold.context().chronology(), context.chronology());
    assert!(
        replacement
            .compaction_epoch(&conversation)
            .unwrap()
            .is_none()
    );
}

/// Fresh replay after interruption may come from another installed store. The
/// resumed canonical chronology keeps its actual original source snapshot,
/// rather than pairing retained events with the new prompt's archive owner.
#[test]
fn interrupted_compaction_source_stays_with_original_continuation() {
    let (mut service, original, turn_id) = live_fixture_with_source_codec(200, true, true);
    let turn = service.agent_turn_ledger().turn(&turn_id).unwrap().clone();
    let source = service.agent_turn_history_source(&turn_id).unwrap().clone();
    service
        .finish_agent_turn("%1", &turn_id, AgentTurnState::Interrupted)
        .unwrap();
    let replacement = AgentTranscriptStore::new(temp_root("compaction-interrupted-replacement"));
    replacement
        .append_many(&original.inspect(&turn.conversation_id).unwrap())
        .unwrap();
    service.set_agent_transcript_store(replacement);
    let resumed = service
        .start_agent_prompt_turn("%1", "resume original chronology")
        .unwrap();
    let retained = service.agent_turn_history_source(&resumed.turn_id).unwrap();
    assert!(std::sync::Arc::ptr_eq(&source, retained));
    assert_eq!(retained.store(), Some(original));
}

/// Installing storage after an originally memory-only turn cannot retroactively
/// supply an archive owner for its compactor. Local recovery remains useful
/// without creating a transcript or replay epoch in the later store.
#[test]
fn compaction_preserves_original_missing_store_before_dispatch() {
    for observed in [false, true] {
        let (mut service, unused, turn_id) = typed_source::memory();
        let source = service.agent_turn_history_source(&turn_id).unwrap();
        assert!(source.store().is_none());
        let replacement =
            AgentTranscriptStore::new(temp_root("compaction-originally-missing-store"));
        service.set_agent_transcript_store(replacement.clone());
        typed_source::queue(&mut service, &turn_id, observed);
        let task = service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap();
        assert!(task.source_owner.as_ref().unwrap().store.is_none());
        typed_source::complete(&mut service, &turn_id, "MEMORY_ONLY_SUMMARY");
        assert!(service.agent_turn_is_running(&turn_id));
        let conversation = &service
            .agent_turn_ledger()
            .turn(&turn_id)
            .unwrap()
            .conversation_id;
        assert!(!replacement.transcript_path(conversation).unwrap().exists());
        assert!(
            replacement
                .compaction_epoch(conversation)
                .unwrap()
                .is_none()
        );
        assert!(fs::read_dir(unused.root()).unwrap().next().is_none());
    }
    // A missing store under the normal save policy is a separate case from
    // explicit NoStore. Preserve its first-turn protected root and truthful
    // no-work result, without creating an auxiliary request or archive owner.
    for observed in [false, true] {
        let (mut service, unused, turn_id) = typed_source::missing_store();
        let before = service.agent_turn_contexts()[&turn_id].clone();
        let replacement = AgentTranscriptStore::new(temp_root("missing-store-protected-root"));
        service.set_agent_transcript_store(replacement.clone());
        let profile = service.agent_turn_model_profile(&turn_id).unwrap().clone();
        let trigger = if observed {
            crate::runtime::agent_state::RuntimeActiveTurnCompactionTrigger::ObservedInputLimit {
                observed_input_tokens: 20_000,
                max_input_tokens: 20_000,
            }
        } else {
            crate::runtime::agent_state::RuntimeActiveTurnCompactionTrigger::ProviderContextLimit {
                attempt: 1,
            }
        };
        assert!(
            !service
                .queue_agent_active_turn_compaction(&turn_id, "live".into(), profile, trigger)
                .unwrap()
        );
        assert!(
            service
                .pending_agent_compaction_task_for_tests("%1")
                .is_none()
        );
        assert_eq!(service.agent_turn_contexts()[&turn_id], before);
        assert!(service.agent_turn_is_running(&turn_id));
        let conversation = &service
            .agent_turn_ledger()
            .turn(&turn_id)
            .unwrap()
            .conversation_id;
        assert!(!replacement.transcript_path(conversation).unwrap().exists());
        assert!(
            replacement
                .compaction_epoch(conversation)
                .unwrap()
                .is_none()
        );
        assert!(fs::read_dir(unused.root()).unwrap().next().is_none());
    }
}

/// Conflicting same-target pending rows are a preparation failure, never an
/// empty successful replay. Capturing work is still actor-local; the worker
/// propagates the retained diagnostic before archive reads/publication, and
/// an unrelated conversation remains writable while this source is refused.
#[test]
fn history_preparation_retains_original_pending_capture_failure() {
    let (mut service, store, turn_id) = live_fixture_with_source_codec(200, true, true);
    let conversation = service
        .agent_turn_ledger()
        .turn(&turn_id)
        .unwrap()
        .conversation_id
        .clone();
    let original = store.inspect(&conversation).unwrap();
    service.persistence.enable_transcript_adapter();
    for body in ["first accepted body", "conflicting accepted body"] {
        let mut row = original[0].clone();
        row.sequence = 3;
        row.role = TranscriptRole::User;
        row.content = body.into();
        service.persistence.queue_transcript(
            crate::runtime::RuntimeSideEffect::PersistTranscriptEntries {
                path: store.transcript_path(&conversation).unwrap(),
                store: store.clone(),
                batch: crate::storage::transcript::TranscriptAppendBatch::owned_rows(vec![row])
                    .unwrap(),
            },
        );
    }
    let work = service.prepare_runtime_agent_prompt_history_work("%1");
    let error = crate::runtime::execute_runtime_agent_prompt_history_work(work).unwrap_err();
    assert!(
        error
            .message()
            .contains("pending sequence has conflicting entries"),
        "{error}"
    );
    assert_eq!(store.inspect(&conversation).unwrap(), original);
    assert!(store.compaction_epoch(&conversation).unwrap().is_none());
    let mut other = original[0].clone();
    other.conversation_id = "unaffected-source".into();
    other.sequence = 1;
    other.role = TranscriptRole::User;
    other.content = "unaffected".into();
    store.append(&other).unwrap();
    assert_eq!(store.inspect("unaffected-source").unwrap(), vec![other]);
}
