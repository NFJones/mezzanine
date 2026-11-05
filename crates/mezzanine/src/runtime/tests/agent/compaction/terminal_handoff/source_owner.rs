//! Actual compaction keeps original source ownership across store replacement.
//!
//! The competing archive deliberately has the same conversation, source IDs and
//! group metadata with valid revised bodies. A current-store lookup cannot
//! masquerade as the earlier prompt's source, even before compactor dispatch.

use super::*;

/// Both legacy/accounted imports retain the original snapshot when replacement
/// happens before or after provider dispatch. Observed-input publication uses
/// only the original store; provider-limit local summaries retain its original
/// epoch baseline and proof. Replacement bytes and authority remain untouched.
#[test]
fn compaction_uses_original_history_before_and_after_store_replacement() {
    for accounted in [false, true] {
        for observed in [false, true] {
            for after_queue in [false, true] {
                let (mut service, original, turn_id) =
                    live_fixture_with_source_codec(200, true, accounted);
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
                for row in &mut replacement_rows {
                    let event = mez_agent::TranscriptContextEvent::execution_block_with_metadata(
                        if row.sequence == 1 {
                            ContextSourceKind::TranscriptAssistant
                        } else {
                            ContextSourceKind::ActionResult
                        },
                        if row.sequence == 1 {
                            "same decision"
                        } else {
                            "same result"
                        },
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
                            if row.sequence == 1 {
                                ContextSourceKind::TranscriptAssistant
                            } else {
                                ContextSourceKind::ActionResult
                            },
                            if row.sequence == 1 {
                                "same decision"
                            } else {
                                "same result"
                            },
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
                        0,
                        "REPLACEMENT_EPOCH_MUST_NOT_BE_ADOPTED",
                    )
                    .unwrap();
                let replacement_epoch = replacement.compaction_epoch(&conversation).unwrap();
                if after_queue {
                    queue_live_compaction(&mut service, &turn_id, observed);
                }
                service.set_agent_transcript_store(replacement.clone());
                if !after_queue {
                    queue_live_compaction(&mut service, &turn_id, observed);
                }
                let task = service
                    .pending_agent_compaction_task_for_tests("%1")
                    .unwrap();
                assert_eq!(
                    task.source_owner.as_ref().unwrap().store.as_ref(),
                    Some(&original)
                );
                complete_runtime_test_compaction(&mut service, "%1", "ORIGINAL_SOURCE_SUMMARY");
                assert!(
                    service.agent_turn_is_running(&turn_id),
                    "{}",
                    service
                        .pane_screen("%1")
                        .unwrap()
                        .normal_content_lines()
                        .join("\n")
                );
                assert!(
                    service
                        .pending_agent_compaction_task_for_tests("%1")
                        .is_none()
                );
                if !observed {
                    let certificate = service
                        .terminal_compaction_handoff_content(&turn_id)
                        .unwrap()
                        .unwrap();
                    let proof: crate::storage::transcript::compaction_handoff::TerminalCompactionHandoff = serde_json::from_str(certificate.strip_prefix(crate::storage::transcript::compaction_handoff::MARKER).unwrap()).unwrap();
                    assert!(proof.baseline.is_none());
                    assert_eq!(
                        proof.replacements[0].sources,
                        original_rows
                            .iter()
                            .map(|row| row.content.clone())
                            .collect::<Vec<_>>()
                    );
                } else {
                    let epoch = original.compaction_epoch(&conversation).unwrap().unwrap();
                    assert_eq!(epoch.ranges[0].summary, "ORIGINAL_SOURCE_SUMMARY");
                    assert_eq!(epoch.ranges[0].first_sequence, 1);
                    assert_eq!(epoch.ranges[0].through_sequence, 2);
                }
                assert_eq!(
                    original.compaction_epoch(&conversation).unwrap().is_some(),
                    observed
                );
                let published = original.inspect(&conversation).unwrap();
                assert!(published.starts_with(&original_rows));
                if observed {
                    assert_eq!(published.len(), original_rows.len() + 2);
                    assert!(matches!(
                        mez_agent::TranscriptContextEvent::from_transcript_content(
                            &published[2].content
                        ),
                        Some(mez_agent::TranscriptContextEvent::McpCompactionEpoch)
                    ));
                    assert!(
                        published[3]
                            .content
                            .contains("MCP compaction re-retrieval guidance")
                    );
                } else {
                    assert_eq!(published, original_rows);
                }
                assert_eq!(
                    replacement.inspect(&conversation).unwrap(),
                    replacement_rows
                );
                assert_eq!(
                    replacement.compaction_epoch(&conversation).unwrap(),
                    replacement_epoch
                );
            }
        }
    }
}

/// Rows accepted after dispatch keep their existing exact-path queue owners.
/// Observed publication and refresh include the original pending tail while
/// ignoring conflicting equal-sequence/CID rows queued for another store. The
/// compactor neither drains nor acknowledges either pending persistence owner.
#[test]
fn compaction_refresh_uses_original_pending_path_after_store_replacement() {
    let (mut service, original, turn_id) = live_fixture_with_source_codec(200, true, true);
    let conversation = service
        .agent_turn_ledger()
        .turn(&turn_id)
        .unwrap()
        .conversation_id
        .clone();
    let source = original.inspect(&conversation).unwrap();
    queue_live_compaction(&mut service, &turn_id, true);
    let replacement = AgentTranscriptStore::new(temp_root("compaction-pending-replacement"));
    replacement.append_many(&source).unwrap();
    service.persistence.enable_transcript_adapter();
    let mut pending = source[0].clone();
    pending.sequence = 3;
    pending.role = TranscriptRole::User;
    pending.turn_id = "late-original-owner".into();
    pending.content = "ORIGINAL_PENDING_TAIL".into();
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
    foreign.content = "REPLACEMENT_PENDING_MUST_NOT_BE_ADOPTED".into();
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
    complete_runtime_test_compaction(&mut service, "%1", "PENDING_OWNER_SUMMARY");
    assert!(
        service.agent_turn_is_running(&turn_id),
        "{}",
        service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
    );
    let context = service.agent_turn_contexts().get(&turn_id).unwrap();
    assert!(
        context
            .blocks()
            .iter()
            .any(|block| block.content.contains("ORIGINAL_PENDING_TAIL"))
    );
    assert!(!context.blocks().iter().any(|block| {
        block
            .content
            .contains("REPLACEMENT_PENDING_MUST_NOT_BE_ADOPTED")
    }));
    let original_pending = service
        .persistence
        .pending_transcript_entries_at_path(
            &original.transcript_path(&conversation).unwrap(),
            &conversation,
        )
        .unwrap();
    assert_eq!(original_pending[0], pending);
    assert_eq!(original_pending.len(), 3);
    assert_eq!(original_pending[1].sequence, 4);
    assert_eq!(original_pending[2].sequence, 5);
    assert_eq!(
        service
            .persistence
            .pending_transcript_entries_at_path(
                &replacement.transcript_path(&conversation).unwrap(),
                &conversation
            )
            .unwrap(),
        vec![foreign]
    );
    assert_eq!(original.inspect(&conversation).unwrap(), source);
    assert_eq!(replacement.inspect(&conversation).unwrap(), source);
    assert!(original.compaction_epoch(&conversation).unwrap().is_some());
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
    let (mut service, _, turn_id) = live_fixture_with_source_store(200, false, false, false);
    let source = service.agent_turn_history_source(&turn_id).unwrap();
    assert!(source.store().is_none());
    let replacement = AgentTranscriptStore::new(temp_root("compaction-originally-missing-store"));
    service.set_agent_transcript_store(replacement.clone());
    queue_live_compaction(&mut service, &turn_id, false);
    let task = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    assert!(task.source_owner.as_ref().unwrap().store.is_none());
    complete_runtime_test_compaction(&mut service, "%1", "MEMORY_ONLY_SUMMARY");
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
