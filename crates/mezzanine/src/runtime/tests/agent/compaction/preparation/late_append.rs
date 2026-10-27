//! Later archive writers must not corrupt compaction's runtime row counters.
//!
//! Controlled completion uses actual command and publication paths. Deferred
//! rows remain logical pending evidence rather than being presented as durable.

use super::*;

/// Captures real compactor work, appends outside that source, and completes the
/// original task. Both counter domains must follow checked assigned sequences;
/// counting only this actor's newly appended MCP rows misses the intervening row.
fn complete_with_later_row(deferred: bool, cached_cursor: bool) {
    let (mut service, primary, store) = fixture("manual-later-assigned-sequence");
    if deferred {
        service.use_transcript_effect_adapter();
    }
    if cached_cursor {
        let rows = store.inspect("prepare-owned").unwrap();
        let path = store.transcript_path("prepare-owned").unwrap();
        service
            .persistence
            .queue_transcript(RuntimeSideEffect::PersistTranscriptEntries {
                store: store.clone(),
                path: path.clone(),
                entries: rows.clone(),
            });
        assert_eq!(service.persistence.take_transcript_effects().len(), 1);
        store.append_many(&rows).unwrap();
        assert!(service.persistence.settle_transcript_write_at_path(
            "prepare-owned",
            1,
            &path,
            &rows
        ));
        assert_eq!(
            service
                .persistence
                .deferred_transcript_next_sequence("prepare-owned"),
            Some(4)
        );
        assert!(!service.persistence.bookkeeping_pending("prepare-owned"));
    }
    let work = request_work(&mut service, &primary);
    let task = work.execute_request().unwrap();
    assert_eq!(task.compacted_through_sequence, Some(1));
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
    store
        .append(&TranscriptEntry {
            conversation_id: "prepare-owned".into(),
            sequence: 4,
            created_at_unix_seconds: 4,
            role: TranscriptRole::Assistant,
            turn_id: "later-external-turn".into(),
            agent_id: "agent-%other".into(),
            pane_id: "%other".into(),
            content: "LATER_EXTERNAL_ROW".into(),
        })
        .unwrap();
    complete_runtime_test_compaction(&mut service, "%1", "small summary");
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
    let epoch = store.compaction_epoch("prepare-owned").unwrap().unwrap();
    let session = service.agent_shell_store().get("%1").unwrap();
    assert_eq!(
        session.transcript_high_water, high,
        "logical frontier must use actual assigned sequences after a later archive writer"
    );
    assert_eq!(
        session.transcript_entries,
        high - epoch.through_sequence,
        "raw count must include every retained physical row, not only locally counted appends"
    );
    assert!(
        view.logical
            .iter()
            .any(|row| row.content == "LATER_EXTERNAL_ROW")
    );
    assert_eq!(epoch.through_sequence, 1);
}

/// Direct durable MCP writes must observe the intervening real archive row and
/// leave the runtime frontier equal to the checked archive after publication.
#[test]
fn runtime_manual_compaction_later_append_updates_direct_position() {
    complete_with_later_row(false, false);
}

/// Queued MCP writes contribute their actual reserved sequences to the logical
/// frontier without pretending that the pending suffix has already committed.
#[test]
fn runtime_manual_compaction_later_append_updates_deferred_position() {
    complete_with_later_row(true, false);
}

/// A previously settled real write leaves a valid cursor that becomes stale
/// after another writer appends. MCP allocation must use checked current rows
/// and pending reservations instead of colliding with that now-occupied slot.
#[test]
fn runtime_manual_compaction_later_append_updates_cached_deferred_position() {
    complete_with_later_row(true, true);
}
