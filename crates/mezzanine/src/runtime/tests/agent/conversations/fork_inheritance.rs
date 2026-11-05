//! Actual fork children inherit authoritative context and independent storage.
//!
//! These tests cross real resume, spawn, initial model-context construction and
//! deferred publication boundaries. Durable acknowledgments use actual receipt
//! admission and settlement; no provider requests or historical actions execute.

use super::fork_fixtures::{CONVERSATION, resumed_source, row};
use super::*;
use crate::runtime::PersistenceEvent;
use crate::storage::transcript::{AgentCompactionRange, TranscriptAppendBatch};
use std::fs;

/// Builds the same restricted child request for successful and failed admission.
fn fork_request(skip: bool) -> SubagentSpawnRequest {
    SubagentSpawnRequest {
        parent_agent_id: "agent-%1".into(),
        requested_role: "explorer".into(),
        placement: "new-pane".into(),
        cooperation_mode: CooperationMode::ExploreOnly,
        cooperation_mode_defaulted: false,
        read_scopes: vec![],
        read_scopes_defaulted: false,
        write_scopes: vec![],
        write_scopes_defaulted: false,
        session_mode: mez_agent::SubagentSessionMode::Fork,
        initial_model_size: None,
        initial_reasoning_effort: None,
        task_prompt: "continue the frozen parent context".into(),
        explicit_user_approval: false,
        skip_initial_turn: skip,
    }
}

/// Allocates an ordinary fork through its actual runtime entry point and returns
/// the child's pane, conversation and optional initial turn for context checks.
fn spawn_child(
    service: &mut RuntimeSessionService,
    primary: &mez_core::ids::ClientId,
    skip: bool,
) -> (String, String, Option<String>) {
    service.start_initial_pane_process(Some("cat")).unwrap();
    let response = service
        .spawn_runtime_subagent(
            primary,
            fork_request(skip),
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(&response).unwrap();
    let pane = value["pane"]["pane_id"].as_str().unwrap().to_string();
    let cid = service
        .agent_shell_store()
        .get(&pane)
        .unwrap()
        .session_id
        .clone();
    let turn = value["turn"]["id"].as_str().map(str::to_string);
    (pane, cid, turn)
}

/// A real fork drops the prefix-covered raw row and resequences its retained
/// tail, including two equal independent bodies. The actual initial child
/// request and cold replay must retain every original occurrence and local
/// estimate, rather than deriving identity from the new CID/row positions.
#[test]
fn runtime_fork_inheritance_preserves_original_raw_records_through_initial_receipt() {
    let (mut service, store, primary) = resumed_source(7, 1);
    for sequence in [6, 7] {
        let row = store
            .inspect(CONVERSATION)
            .unwrap()
            .into_iter()
            .find(|row| row.sequence == sequence)
            .unwrap();
        store
            .compare_and_swap_entry_content(
                CONVERSATION,
                sequence,
                "%9",
                &store.transcript_entry_revision(&row).unwrap(),
                "equal inherited 雪 code {\"rows\":[1,2,3]}".into(),
            )
            .unwrap();
    }
    let parent = service
        .agent_context_for_pane_prompt("%1", "inspect parent", 0)
        .unwrap();
    let originals = parent
        .chronology()
        .iter()
        .filter(|event| event.block().source == ContextSourceKind::TranscriptAssistant)
        .map(|event| event.record())
        .collect::<Vec<_>>();
    assert_eq!(originals.len(), 6);
    assert_ne!(originals[4].occurrence_id(), originals[5].occurrence_id());
    let raw = fs::read(store.transcript_path(CONVERSATION).unwrap()).unwrap();
    let (pane, cid, turn) = spawn_child(&mut service, &primary, false);
    let turn = turn.unwrap();
    let child = service.agent_turn_contexts().get(&turn).unwrap();
    let inherited = child
        .chronology()
        .iter()
        .filter(|event| event.block().source == ContextSourceKind::TranscriptAssistant)
        .map(|event| event.record())
        .collect::<Vec<_>>();
    assert_eq!(inherited.len(), originals.len());
    let expected = child
        .chronology()
        .iter()
        .filter(|event| event.block().source == ContextSourceKind::TranscriptAssistant)
        .zip(
            parent
                .chronology()
                .iter()
                .filter(|event| event.block().source == ContextSourceKind::TranscriptAssistant),
        )
        .map(|(child, original)| {
            assert_eq!(child.block().content, original.block().content);
            original.record().restored(child.block()).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        inherited, expected,
        "actual initial child request lost original records"
    );
    assert_eq!(store.inspect(&cid).unwrap().len(), 6);
    let snapshot = store
        .conversation_replay_snapshot(&cid, &[], |_| {
            Some((
                crate::storage::transcript::ConversationTranscriptRead::All,
                true,
            ))
        })
        .unwrap();
    assert_eq!(
        snapshot.row_records.values().copied().collect::<Vec<_>>(),
        expected
    );
    service.set_agent_transcript_store(AgentTranscriptStore::new(store.root()));
    let cold = service
        .agent_context_for_pane_prompt(&pane, "inspect cold child", 0)
        .unwrap();
    let restored = cold
        .chronology()
        .iter()
        .filter(|event| event.block().source == ContextSourceKind::TranscriptAssistant)
        .map(|event| event.record())
        .collect::<Vec<_>>();
    assert_eq!(restored, expected);
    assert_eq!(
        fs::read(store.transcript_path(CONVERSATION).unwrap()).unwrap(),
        raw
    );
    service.terminate_all_pane_processes().unwrap();
}

/// A parent without compaction authority still requires complete initial raw
/// accounting. The summary-free path must use the same batch/receipt contract
/// and preserve duplicate occurrences through actual initial and cold contexts.
#[test]
fn runtime_fork_inheritance_without_summary_preserves_raw_record_ownership() {
    let store = AgentTranscriptStore::new(temp_root("plain-inherited-records"));
    for sequence in 1..=3 {
        let mut entry = row(1);
        entry.sequence = sequence;
        entry.content = "equal plain inherited source 雪".into();
        store.append(&entry).unwrap();
    }
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", CONVERSATION, 3)
        .unwrap();
    let parent = service
        .agent_context_for_pane_prompt("%1", "inspect plain parent", 0)
        .unwrap();
    let originals = parent
        .chronology()
        .iter()
        .filter(|event| event.block().source == ContextSourceKind::TranscriptAssistant)
        .map(|event| event.record().occurrence_id())
        .collect::<Vec<_>>();
    assert_eq!(originals.len(), 3);
    assert_ne!(originals[0], originals[1]);
    let (pane, cid, turn) = spawn_child(&mut service, &primary, false);
    let context = service
        .agent_turn_contexts()
        .get(turn.as_ref().unwrap())
        .unwrap();
    let records = context
        .chronology()
        .iter()
        .filter(|event| event.block().source == ContextSourceKind::TranscriptAssistant)
        .map(|event| event.record())
        .collect::<Vec<_>>();
    assert_eq!(
        records
            .iter()
            .map(|record| record.occurrence_id())
            .collect::<Vec<_>>(),
        originals
    );
    assert!(store.compaction_epoch(&cid).unwrap().is_none());
    let snapshot = store
        .conversation_replay_snapshot(&cid, &[], |_| {
            Some((
                crate::storage::transcript::ConversationTranscriptRead::All,
                true,
            ))
        })
        .unwrap();
    assert_eq!(
        snapshot.row_records.values().copied().collect::<Vec<_>>(),
        records
    );
    service.set_agent_transcript_store(AgentTranscriptStore::new(store.root()));
    let cold = service
        .agent_context_for_pane_prompt(&pane, "inspect plain child", 0)
        .unwrap();
    let restored = cold
        .chronology()
        .iter()
        .filter(|event| event.block().source == ContextSourceKind::TranscriptAssistant)
        .map(|event| event.record())
        .collect::<Vec<_>>();
    assert_eq!(restored, records);
    service.terminate_all_pane_processes().unwrap();
}

/// A real fully compacted parent produces a durable child with zero physical
/// rows and the complete inherited summary. Reopening the store preserves both
/// counter domains; a later parent append cannot enter the independent child.
#[test]
fn runtime_fork_inheritance_zero_raw_tail_keeps_summary_and_empty_archive() {
    let (mut service, store, primary) = resumed_source(5, 5);
    let before = fs::read(store.transcript_path(CONVERSATION).unwrap()).unwrap();
    let (pane, cid, _) = spawn_child(&mut service, &primary, true);
    let session = service.agent_shell_store().get(&pane).unwrap();
    assert_eq!(
        (session.transcript_high_water, session.transcript_entries),
        (0, 0)
    );
    assert_eq!(
        store.compaction_epoch(&cid).unwrap().unwrap().summary,
        "FROZEN_PREFIX"
    );
    assert!(store.inspect(&cid).unwrap().is_empty());
    assert_eq!(
        fs::read(store.transcript_path(CONVERSATION).unwrap()).unwrap(),
        before
    );
    let reopened = AgentTranscriptStore::new(store.root().to_path_buf());
    assert!(
        reopened.saved_session(&cid).unwrap().is_some(),
        "an empty inherited child must remain discoverable for resume"
    );
    assert!(
        crate::runtime::commands::read_direct_resume(&reopened, &cid, None).is_ok(),
        "the actual resume reader must accept the complete zero-row child"
    );
    let snapshot = reopened.lock_resume_replay_snapshot(&cid, true).unwrap();
    assert_eq!(snapshot.position.logical_high_water, 0);
    assert_eq!(snapshot.position.active_raw_entries, 0);
    drop(snapshot);
    store.append(&row(6)).unwrap();
    let context = service
        .agent_context_for_pane_prompt(&pane, "continue", 0)
        .unwrap();
    assert_eq!(
        context
            .blocks()
            .iter()
            .filter(|block| block.content == "FROZEN_PREFIX")
            .count(),
        1
    );
    assert!(
        !context
            .blocks()
            .iter()
            .any(|block| block.content.contains("raw source"))
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Child selective ranges shift over its own contiguous raw archive and retain
/// source ordering without resending covered text. Repeated context reads leave
/// parent authority and child projection unchanged.
#[test]
fn runtime_fork_inheritance_rebases_selective_ranges_without_parent_mutation() {
    let (mut service, store, primary) = resumed_source(5, 1);
    store
        .save_compaction_ranges(
            CONVERSATION,
            1,
            "FROZEN_PREFIX",
            vec![
                AgentCompactionRange {
                    first_sequence: 2,
                    through_sequence: 3,
                    summary: "FIRST_SELECTIVE".into(),
                },
                AgentCompactionRange {
                    first_sequence: 4,
                    through_sequence: 5,
                    summary: "SECOND_SELECTIVE".into(),
                },
            ],
        )
        .unwrap();
    let parent = store.compaction_epoch(CONVERSATION).unwrap();
    let (pane, cid, _) = spawn_child(&mut service, &primary, true);
    let child = store.compaction_epoch(&cid).unwrap().unwrap();
    assert_eq!(child.through_sequence, 0);
    assert_eq!(
        child
            .ranges
            .iter()
            .map(|r| (r.first_sequence, r.through_sequence))
            .collect::<Vec<_>>(),
        [(1, 2), (3, 4)]
    );
    assert_eq!(
        store
            .inspect(&cid)
            .unwrap()
            .iter()
            .map(|r| r.sequence)
            .collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );
    for _ in 0..2 {
        let context = service
            .agent_context_for_pane_prompt(&pane, "continue", 0)
            .unwrap();
        let projected = context
            .blocks()
            .iter()
            .filter(|block| {
                ["FROZEN_PREFIX", "FIRST_SELECTIVE", "SECOND_SELECTIVE"]
                    .contains(&block.content.as_str())
            })
            .map(|block| block.content.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            projected,
            ["FROZEN_PREFIX", "FIRST_SELECTIVE", "SECOND_SELECTIVE"]
        );
        assert!(
            !context
                .blocks()
                .iter()
                .any(|block| block.content.contains("raw source"))
        );
    }
    assert_eq!(store.compaction_epoch(CONVERSATION).unwrap(), parent);
    assert_eq!(store.compaction_epoch(&cid).unwrap(), Some(child));
    service.terminate_all_pane_processes().unwrap();
}

/// The first non-skipped child turn receives captured summary authority before
/// its archive exists, while initial ownership prevents provider dispatch.
/// Wrong metadata, a wrong path or a row-only outcome cannot release that fence.
/// Only actual complete receipt admission and retirement enables settlement.
#[test]
fn runtime_fork_inheritance_first_context_waits_for_exact_initial_publication() {
    let (mut service, store, primary) = resumed_source(5, 5);
    service.use_transcript_effect_adapter();
    let (pane, cid, turn) = spawn_child(&mut service, &primary, false);
    let turn = turn.unwrap();
    let context = service.agent_turn_contexts().get(&turn).unwrap();
    assert_eq!(
        context
            .blocks()
            .iter()
            .filter(|block| block.content == "FROZEN_PREFIX")
            .count(),
        1
    );
    assert!(service.persistence.initial_transcript_pending(&cid));
    assert!(
        !service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == turn)
    );
    assert!(
        service
            .agent_context_for_pane_prompt(&pane, "another", 0)
            .is_err()
    );
    assert!(
        service
            .begin_agent_prompt_history_preparation(primary.clone(), &pane, "another")
            .is_err()
    );
    let effects = service.persistence.take_transcript_effects();
    let (batch, path) = effects
        .iter()
        .find_map(|effect| match effect {
            RuntimeSideEffect::PersistInitialTranscript { batch, path, .. } => {
                Some((batch.clone(), path.clone()))
            }
            _ => None,
        })
        .unwrap();
    assert!(batch.entries().is_empty());
    let mut changed = batch.initial_compaction_epoch().unwrap().clone();
    changed.summary = "WRONG_FROZEN_SUMMARY".into();
    let changed = TranscriptAppendBatch::initial_projection(changed, vec![]).unwrap();
    assert!(
        !service
            .apply_persistence_transition(PersistenceEvent::InitialTranscriptCompleted {
                batch: changed,
                path: path.clone()
            })
            .unwrap()
            .applied
    );
    assert!(
        !service
            .apply_persistence_transition(PersistenceEvent::InitialTranscriptCompleted {
                batch: batch.clone(),
                path: path.with_extension("other")
            })
            .unwrap()
            .applied
    );
    assert!(
        !service
            .apply_persistence_transition(PersistenceEvent::TranscriptCompleted {
                conversation_id: cid.clone(),
                first_sequence: 0,
                entries: vec![],
                path: path.clone(),
                bytes: 0
            })
            .unwrap()
            .applied
    );
    assert!(service.persistence.initial_transcript_pending(&cid));
    store.accept_transcript_append_receipt(&batch, 42).unwrap();
    store.settle_transcript_append_receipt(&batch, 42).unwrap();
    assert!(
        service
            .apply_persistence_transition(PersistenceEvent::InitialTranscriptCompleted {
                batch: batch.clone(),
                path: path.clone()
            })
            .unwrap()
            .applied
    );
    assert!(!service.persistence.initial_transcript_pending(&cid));
    assert!(
        !service
            .apply_persistence_transition(PersistenceEvent::InitialTranscriptCompleted {
                batch,
                path
            })
            .unwrap()
            .applied
    );
    service.terminate_all_pane_processes().unwrap();
}

/// A committed but unpublished terminal witness is projected without changing
/// parent bytes. The child owns its accepted range summary and an inert audit
/// copy, so reopening never replays a source witness against renumbered rows.
#[test]
fn runtime_fork_inheritance_projects_terminal_witness_without_replaying_it() {
    let (mut service, store, primary) = resumed_source(5, 1);
    let baseline = store.compaction_epoch(CONVERSATION).unwrap();
    let witness = crate::storage::transcript::compaction_handoff::TerminalCompactionHandoff {
        baseline: baseline.clone(),
        replacements: vec![
            crate::storage::transcript::compaction_handoff::TerminalCompactionReplacement {
                summary_record: None,
                sources: vec![row(2).content, row(3).content],
                summary: "TERMINAL_ACCEPTED_SUMMARY".into(),
            },
        ],
    };
    let mut audit = row(6);
    audit.role = TranscriptRole::System;
    audit.content = witness.to_content().unwrap();
    store.append(&audit).unwrap();
    service
        .agent_shell_store_mut()
        .record_transcript_append("%1", std::slice::from_ref(&audit))
        .unwrap();
    let before = fs::read(store.transcript_path(CONVERSATION).unwrap()).unwrap();
    let (pane, cid, _) = spawn_child(&mut service, &primary, true);
    assert_eq!(store.compaction_epoch(CONVERSATION).unwrap(), baseline);
    assert_eq!(
        fs::read(store.transcript_path(CONVERSATION).unwrap()).unwrap(),
        before
    );
    let child_rows = store.inspect(&cid).unwrap();
    assert!(
        child_rows
            .last()
            .unwrap()
            .content
            .starts_with(crate::storage::transcript::compaction_handoff::INHERITED_MARKER)
    );
    for _ in 0..2 {
        let context = service
            .agent_context_for_pane_prompt(&pane, "continue", 0)
            .unwrap();
        assert_eq!(
            context
                .blocks()
                .iter()
                .filter(|block| block.content == "TERMINAL_ACCEPTED_SUMMARY")
                .count(),
            1
        );
        assert!(
            !context
                .blocks()
                .iter()
                .any(|block| block.content.contains("raw source 2")
                    || block.content.contains("raw source 3"))
        );
    }
    service.terminate_all_pane_processes().unwrap();
}

/// Failure after a metadata-only initial effect is queued must cancel that
/// whole publication before any worker can recreate the abandoned child. The
/// parent archive and its sole live session remain unchanged after rollback.
#[test]
fn runtime_fork_inheritance_failed_spawn_cancels_metadata_only_publication() {
    let (mut service, store, primary) = resumed_source(5, 5);
    service.use_transcript_effect_adapter();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let before = fs::read(store.transcript_path(CONVERSATION).unwrap()).unwrap();
    let sessions = service.agent_shell_store().sessions().count();
    service.fail_next_subagent_spawn_after_fork_persistence_for_tests();
    let error = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".into(),
                requested_role: "explorer".into(),
                placement: "new-pane".into(),
                cooperation_mode: CooperationMode::ExploreOnly,
                cooperation_mode_defaulted: false,
                read_scopes: vec![],
                read_scopes_defaulted: false,
                write_scopes: vec![],
                write_scopes_defaulted: false,
                session_mode: mez_agent::SubagentSessionMode::Fork,
                initial_model_size: None,
                initial_reasoning_effort: None,
                task_prompt: "must not survive failed setup".into(),
                explicit_user_approval: false,
                skip_initial_turn: true,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap_err();
    assert!(
        error.message().contains("after fork persistence"),
        "{error}"
    );
    assert_eq!(service.agent_shell_store().sessions().count(), sessions);
    assert!(
        !service
            .persistence
            .take_transcript_effects()
            .iter()
            .any(|effect| matches!(effect, RuntimeSideEffect::PersistInitialTranscript { .. }))
    );
    assert!(store.pending_append_receipts().unwrap().is_empty());
    assert_eq!(
        fs::read(store.transcript_path(CONVERSATION).unwrap()).unwrap(),
        before
    );
    service.terminate_all_pane_processes().unwrap();
}

/// A failed synchronous initial publication retains its exact receipt and child
/// contract for checked recovery. Cleanup must neither erase uncertain data nor
/// allow replay to recreate the child with the default root authority.
#[test]
fn runtime_fork_inheritance_failed_publication_retains_restricted_recovery() {
    let (mut service, store, primary) = resumed_source(5, 5);
    service.start_initial_pane_process(Some("cat")).unwrap();
    let sessions = service.agent_shell_store().sessions().count();
    let parent = fs::read(store.transcript_path(CONVERSATION).unwrap()).unwrap();
    store.fail_next_compaction_epoch_before_rename();
    let error = service
        .spawn_runtime_subagent(
            &primary,
            fork_request(true),
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap_err();
    assert!(
        error.message().contains("retained for checked recovery"),
        "{error}"
    );
    assert_eq!(service.agent_shell_store().sessions().count(), sessions);
    let receipts = store.pending_append_receipts().unwrap();
    assert_eq!(receipts.len(), 1);
    let cid = error
        .message()
        .strip_prefix("initial fork history ")
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    assert!(error.message().contains(&cid));
    store.recover_append_receipts().unwrap();
    assert!(store.pending_append_receipts().unwrap().is_empty());
    let recovered = store.saved_session(&cid).unwrap().unwrap();
    let actions = store.conversation_allowed_actions(&cid).unwrap().unwrap();
    assert!(!actions.contains(mez_agent::AllowedAction::ApplyPatch));
    assert!(store.conversation_subagent_lineage(&cid).unwrap().is_some());
    assert_eq!(
        recovered.conversation_kind,
        mez_agent::AgentConversationKind::Subagent
    );
    assert_eq!(
        store.compaction_epoch(&cid).unwrap().unwrap().summary,
        "FROZEN_PREFIX"
    );
    assert_eq!(service.agent_shell_store().sessions().count(), sessions);
    assert_eq!(
        fs::read(store.transcript_path(CONVERSATION).unwrap()).unwrap(),
        parent
    );
    service.terminate_all_pane_processes().unwrap();
}

/// An ephemeral fork of a summary-only child still reads its captured source
/// at boundary zero. A later source append cannot enter that projection and the
/// ephemeral conversation never gains a persisted archive.
#[test]
fn runtime_fork_inheritance_ephemeral_zero_source_keeps_captured_summary() {
    let (mut service, store, primary) = resumed_source(5, 5);
    let (pane, cid, _) = spawn_child(&mut service, &primary, true);
    let ephemeral = "ephemeral-zero-child";
    service
        .agent_shell_store_mut()
        .bind_ephemeral_conversation_with_lineage_and_transcript_source(
            &pane,
            ephemeral,
            0,
            None,
            Some(cid.clone()),
            0,
        )
        .unwrap();
    let mut late = row(1);
    late.conversation_id = cid.clone();
    late.content = "LATE_SOURCE_ROW".into();
    store.append(&late).unwrap();
    let mut later_sources = vec![row(2), row(3)];
    for source in &mut later_sources {
        source.conversation_id = cid.clone();
        store.append(source).unwrap();
    }
    let witness = crate::storage::transcript::compaction_handoff::TerminalCompactionHandoff {
        baseline: store.compaction_epoch(&cid).unwrap(),
        replacements: vec![
            crate::storage::transcript::compaction_handoff::TerminalCompactionReplacement {
                summary_record: None,
                sources: later_sources
                    .iter()
                    .map(|row| row.content.clone())
                    .collect(),
                summary: "LATER_SOURCE_SUMMARY".into(),
            },
        ],
    };
    let mut later_witness = late.clone();
    later_witness.sequence = 4;
    later_witness.role = TranscriptRole::System;
    later_witness.content = witness.to_content().unwrap();
    store.append(&later_witness).unwrap();
    let work = service
        .prepare_subagent_fork_read_work(&format!("agent-{pane}"))
        .unwrap()
        .unwrap();
    let captured = work.execute().unwrap();
    assert_eq!(captured.source_high_water, 0);
    assert!(captured.entries.is_empty());
    let captured_epoch = captured.compaction_epoch.unwrap();
    assert_eq!(captured_epoch.summary, "FROZEN_PREFIX");
    assert!(captured_epoch.ranges.is_empty());
    let context = service
        .agent_context_for_pane_prompt(&pane, "continue", 0)
        .unwrap();
    assert_eq!(
        context
            .blocks()
            .iter()
            .filter(|block| block.content == "FROZEN_PREFIX")
            .count(),
        1
    );
    assert!(
        !context
            .blocks()
            .iter()
            .any(|block| block.content.contains("LATE_SOURCE_ROW")
                || block.content.contains("LATER_SOURCE_SUMMARY"))
    );
    assert!(!store.transcript_path(ephemeral).unwrap().exists());
    service.terminate_all_pane_processes().unwrap();
}
