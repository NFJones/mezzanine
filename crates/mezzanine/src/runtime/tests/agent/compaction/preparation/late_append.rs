//! Later archive writers must not corrupt compaction's runtime row counters.
//!
//! Controlled completion uses actual command and publication paths. Deferred
//! rows remain logical pending evidence rather than being presented as durable.

use super::*;

/// Encodes the actual original task root or steering occurrence authored by
/// another controller. Physical placement is independent of event sequence;
/// neither equal text nor a pane count supplies this source identity.
fn authored_row(owner: &mez_agent::ReplayEpochContext, sequence: u64) -> TranscriptEntry {
    let event = owner.context().chronology().last().unwrap();
    let block = event.block();
    let semantic =
        if let Some(start) = owner.task_admission_start(event.record().occurrence_id()) {
            mez_agent::TranscriptContextEvent::task_admission(
                event.sequence().get(),
                start,
                &block.label,
                &block.content,
            )
        } else {
            mez_agent::TranscriptContextEvent::user_event(
                event.sequence().get(),
                &block.label,
                &block.content,
            )
        }
        .unwrap();
    TranscriptEntry {
        conversation_id: "prepare-owned".into(),
        sequence,
        created_at_unix_seconds: 4,
        role: TranscriptRole::System,
        turn_id: "later-external-turn".into(),
        agent_id: "agent-%other".into(),
        pane_id: "%other".into(),
        content: mez_agent::transcript::TranscriptContextRecord::new(
            semantic,
            event.record(),
            None,
        )
        .unwrap()
        .to_transcript_content()
        .unwrap(),
    }
}

/// Captures real compactor work, appends outside that source, and completes the
/// original task. Both counter domains must follow checked assigned sequences;
/// neither a stale local cursor nor model-member counts describe that frontier.
fn complete_with_later_row(deferred: bool, cached_cursor: bool, prepared: bool) {
    let (mut service, primary, store) =
        fixture_with_cursor("manual-later-assigned-sequence", true, cached_cursor);
    if deferred {
        service.use_transcript_effect_adapter();
    }
    assert_eq!(store.inspect("prepare-owned").unwrap().len(), 3);
    assert!(!service.persistence.bookkeeping_pending("prepare-owned"));
    let mut external = service
        .runtime_agent_history_epoch_context("%1")
        .unwrap()
        .replay_owner()
        .unwrap()
        .unwrap();
    let work = request_work(&mut service, &primary);
    let task = work.execute_request().unwrap();
    assert!(task.replay_source.is_some());
    assert!(
        !task
            .request
            .messages
            .last()
            .unwrap()
            .content
            .contains("LATER_EXTERNAL_ROW")
    );
    assert!(
        service
            .complete_manual_compaction_request(&work, Ok(task))
            .unwrap()
    );
    let generation = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .task_generation;
    if prepared {
        // Freeze the actual proposal, then fail its physical swap. The later
        // append must survive recovery of this exact already prepared proposal.
        store.inject_replay_epoch_fault(
            crate::storage::transcript::replay_epoch::Phase::BeforeManifestSwap,
            1,
            false,
        );
        complete_runtime_test_compaction(&mut service, "%1", "small summary");
        let failed = service
            .take_replay_epoch_attempt_for_tests()
            .unwrap()
            .execute();
        assert!(failed.error().is_some());
        service.apply_replay_epoch_completion(failed).unwrap();
        assert!(
            store
                .load_replay_epoch("prepare-owned", mez_agent::AgentContext::empty())
                .unwrap()
                .is_none()
        );
    }
    external
        .admit_instruction_task(
            vec![ContextBlock::user_event(
                "user prompt",
                "LATER_EXTERNAL_ROW",
            )],
            &[],
            &[],
            0,
        )
        .unwrap();
    let late = authored_row(&external, 4);
    let batch = crate::storage::transcript::TranscriptAppendBatch::owned_rows(vec![late]).unwrap();
    store.commit_accounted_transcript_batch(&batch, 1).unwrap();
    let mut deferred_batch = None;
    if deferred {
        let mut body = external.into_owned_context();
        body.append_user_event("user steering", "LATER_PENDING_ROW")
            .unwrap();
        let external = mez_agent::ReplayEpochContext::from_owned_context(
            mez_agent::ReplayConversationId::new("prepare-owned").unwrap(),
            body,
        )
        .unwrap();
        let batch =
            crate::storage::transcript::TranscriptAppendBatch::owned_rows(vec![authored_row(
                &external, 5,
            )])
            .unwrap();
        service
            .persistence
            .queue_transcript(RuntimeSideEffect::PersistTranscriptEntries {
                store: store.clone(),
                path: store.transcript_path("prepare-owned").unwrap(),
                batch: batch.clone(),
            });
        deferred_batch = Some(batch);
    }
    if prepared {
        service
            .poll_replay_epoch_publication("%1", generation)
            .unwrap();
    } else {
        complete_runtime_test_compaction(&mut service, "%1", "small summary");
    }
    let receipt =
        crate::test_support::compaction_publication::execute_pending_publication(&mut service);
    assert!(
        service
            .apply_replay_epoch_completion(receipt)
            .unwrap()
            .applied
    );
    let pending = service
        .persistence
        .pending_transcript_entries("prepare-owned");
    let view = store
        .conversation_transcript_view(
            "prepare-owned",
            crate::storage::transcript::ConversationTranscriptRead::All,
            true,
            &pending,
        )
        .unwrap();
    let high = view.logical.last().unwrap().sequence;
    let history = service.runtime_agent_history_epoch_context("%1").unwrap();
    let cut = history
        .source
        .as_ref()
        .unwrap()
        .replay
        .as_ref()
        .unwrap()
        .covered_archive_frontier();
    let replay = history.replay_owner().unwrap().unwrap();
    let session = service.agent_shell_store().get("%1").unwrap();
    assert_eq!(
        session.transcript_high_water, high,
        "logical frontier must use actual assigned sequences after a later archive writer"
    );
    assert_eq!(
        session.transcript_entries,
        high - cut,
        "physical post-cut rows are distinct from retained canonical model members"
    );
    assert!(
        replay
            .context()
            .chronology()
            .iter()
            .any(|event| event.block().content == "LATER_EXTERNAL_ROW")
    );
    assert_eq!(cut, 3);
    assert_eq!(replay.revision().get(), 1);
    assert_eq!(replay.members().len(), if deferred { 4 } else { 3 });
    assert!(
        !replay
            .context()
            .chronology()
            .iter()
            .any(|event| event.block().content == "completed source 1")
    );
    for content in [
        "completed source 2",
        "completed source 3",
        "LATER_EXTERNAL_ROW",
    ] {
        assert_eq!(
            replay
                .context()
                .chronology()
                .iter()
                .filter(|event| event.block().content == content)
                .count(),
            1
        );
    }
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert!(service.agent_turn_ledger().turns().is_empty());
    assert_eq!(
        replay.context().chronology()[0].block().content,
        "small summary"
    );
    if let Some(batch) = deferred_batch {
        assert_eq!(pending, batch.entries());
        assert_eq!(store.inspect("prepare-owned").unwrap().len(), 4);
        assert!(
            replay
                .context()
                .chronology()
                .iter()
                .any(|event| event.block().content == "LATER_PENDING_ROW")
        );
    }
}

/// Direct durable publication must observe the intervening real archive row and
/// leave the runtime frontier equal to the checked archive after publication.
#[test]
fn runtime_manual_compaction_later_append_updates_direct_position() {
    for prepared in [false, true] {
        complete_with_later_row(false, false, prepared);
    }
}

/// Queued ordinary writes contribute their actual original sequences to the logical
/// frontier without pretending that the pending suffix has already committed.
#[test]
fn runtime_manual_compaction_later_append_updates_deferred_position() {
    for prepared in [false, true] {
        complete_with_later_row(true, false, prepared);
    }
}

/// A previously settled real write leaves a valid cursor that becomes stale
/// after another writer appends. Publication must use checked current rows and
/// original pending reservations rather than the stale physical cursor.
#[test]
fn runtime_manual_compaction_later_append_updates_cached_deferred_position() {
    for prepared in [false, true] {
        complete_with_later_row(true, true, prepared);
    }
}
