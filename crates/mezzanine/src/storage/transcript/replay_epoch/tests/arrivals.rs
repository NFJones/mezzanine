//! A frozen rewrite cannot cover a group reopened by later original source.
//! Real owned append receipts preserve both the original closure and renewal;
//! publication must refuse before replacing authority or changing audit rows.

use super::*;
use mez_agent::transcript::{
    TranscriptContextEvent, TranscriptContextRecord, TranscriptExecutionStateRecord,
};
use mez_agent::{
    ContextConversationAppend, ContextExecutionGroupId, TranscriptEntry, TranscriptRole,
};

fn row(sequence: u64, content: String) -> TranscriptEntry {
    TranscriptEntry {
        conversation_id: "conversation-a".into(),
        sequence,
        created_at_unix_seconds: 1,
        role: TranscriptRole::System,
        turn_id: "original-turn".into(),
        agent_id: "original-agent".into(),
        pane_id: "original-pane".into(),
        content,
    }
}

fn member(owner: &ReplayEpochContext, sequence: u64, ordinal: u64) -> TranscriptEntry {
    let event = owner.context().chronology().last().unwrap();
    let block = event.block();
    let semantic = TranscriptContextEvent::execution_block_with_metadata(
        block.source,
        &block.label,
        &block.content,
        event.execution_group_id().unwrap().clone(),
        ordinal,
        None,
    )
    .unwrap();
    row(
        sequence,
        TranscriptContextRecord::new(semantic, event.record(), None)
            .unwrap()
            .to_transcript_content()
            .unwrap(),
    )
}

fn control(owner: &ReplayEpochContext, sequence: u64) -> TranscriptEntry {
    row(
        sequence,
        TranscriptExecutionStateRecord::new(owner.execution_states().unwrap().pop().unwrap())
            .unwrap()
            .to_transcript_content()
            .unwrap(),
    )
}

/// The retired selective mapper's identity regression now exercises the actual
/// single-head writer. An importing publisher cannot replace missing source
/// custody with its own producer, duplicate an occurrence, change its physical
/// placement or skip an original group ordinal. Each refusal preserves audit
/// bytes, and the exact original capture still publishes afterward.
#[test]
fn durable_compaction_mapping_rejects_incomplete_or_ambiguous_identity() {
    for defect in [
        "missing",
        "duplicate",
        "placement",
        "future-placement",
        "timestamp",
        "ordinal",
    ] {
        let fixture = Fixture::new();
        let mut owner = ReplayEpochContext::new(
            ReplayConversationId::new("conversation-a").unwrap(),
            prefix(),
        )
        .unwrap();
        let group = ContextExecutionGroupId::new("original-mapping-group").unwrap();
        owner
            .append_execution_events([ContextConversationAppend::assistant(
                "answer",
                "original answer",
                group.clone(),
            )])
            .unwrap();
        let first = member(&owner, 1, 1);
        let frontier = owner
            .append_execution_events([ContextConversationAppend::evidence(
                ContextSourceKind::ActionResult,
                "result",
                "original result",
                group.clone(),
                None,
                true,
            )])
            .unwrap()[0];
        let second = member(&owner, 2, 2);
        let fence = owner.capture_execution_owner(&group, frontier).unwrap();
        owner.finish_execution_group(&fence).unwrap();
        let raw = vec![first, second];
        let mut entries = raw.clone();
        entries.push(control(&owner, 3));
        let batch = crate::storage::transcript::TranscriptAppendBatch::owned_rows(entries).unwrap();
        fixture
            .store
            .commit_accounted_transcript_batch(&batch, 1)
            .unwrap();
        let audit = fixture.store.inspect("conversation-a").unwrap();
        let mut damaged = raw.clone();
        match defect {
            "missing" => {
                damaged.pop();
            }
            "duplicate" => damaged.push(raw[0].clone()),
            "placement" => damaged[1].sequence = 3,
            "future-placement" => damaged[1].sequence = 99,
            "timestamp" => damaged[1].created_at_unix_seconds += 1,
            "ordinal" => damaged[1] = member(&owner, 2, 3),
            _ => unreachable!(),
        }
        let publisher = || {
            ReplaySourceOrigin::new("importing-turn", "importing-agent", "importing-pane").unwrap()
        };
        let rejected = fixture
            .store
            .prepare_replay_epoch(
                publisher().capture_raw(&damaged),
                "conversation-a",
                None,
                &owner,
                plan(&owner),
                ReplayHeadPublication::Summary("must not publish damaged source".into()),
            )
            .and_then(|proposal| fixture.store.publish_replay_epoch(&proposal));
        assert!(rejected.is_err(), "accepted {defect} original custody");
        assert_eq!(fixture.store.inspect("conversation-a").unwrap(), audit);
        assert!(
            fixture
                .store
                .load_replay_epoch("conversation-a", AgentContext::empty())
                .unwrap()
                .is_none()
        );
        let proposal = fixture
            .store
            .prepare_replay_epoch(
                publisher().capture_raw(&raw),
                "conversation-a",
                None,
                &owner,
                plan(&owner),
                ReplayHeadPublication::Summary("one qualified original head".into()),
            )
            .unwrap();
        fixture.store.publish_replay_epoch(&proposal).unwrap();
        let (_, cold) = fixture.load();
        assert_eq!(cold.context().chronology().len(), 1);
        assert_eq!(
            cold.context().chronology()[0].block().content,
            "one qualified original head"
        );
        assert_eq!(fixture.store.inspect("conversation-a").unwrap(), audit);
    }
}

/// Original raw images can be pending ordinary appends. Publication backs them
/// in immutable escrow without inventing their physical commit; actual later
/// accounting still preserves the same head and must not replay covered data.
#[test]
fn raw_capture_preserves_uncommitted_source_until_actual_append_commit() {
    let fixture = Fixture::new();
    let mut owner = ReplayEpochContext::new(
        ReplayConversationId::new("conversation-a").unwrap(),
        prefix(),
    )
    .unwrap();
    let group = ContextExecutionGroupId::new("original-uncommitted-group").unwrap();
    let frontier = owner
        .append_execution_events([ContextConversationAppend::assistant(
            "uncommitted original assistant",
            "original source before ordinary append",
            group.clone(),
        )])
        .unwrap()[0];
    let raw = vec![member(&owner, 1, 1)];
    let fence = owner.capture_execution_owner(&group, frontier).unwrap();
    owner.finish_execution_group(&fence).unwrap();
    let proposal = fixture
        .store
        .prepare_replay_epoch(
            origin().capture_raw(&raw),
            "conversation-a",
            None,
            &owner,
            plan(&owner),
            ReplayHeadPublication::Summary("head before ordinary append".into()),
        )
        .unwrap();
    let receipt = fixture.store.publish_replay_epoch(&proposal).unwrap();
    assert!(fixture.store.inspect("conversation-a").unwrap().is_empty());
    let (identity, cold) = fixture.load();
    assert_eq!(receipt.identity(), &identity);
    assert_eq!(cold.context().chronology().len(), 1);
    let batch = crate::storage::transcript::TranscriptAppendBatch::owned_rows(raw.clone()).unwrap();
    fixture
        .store
        .commit_accounted_transcript_batch(&batch, 2)
        .unwrap();
    assert_eq!(fixture.store.inspect("conversation-a").unwrap(), raw);
    let (after, cold) = fixture.load();
    assert_eq!(after, identity);
    assert_eq!(cold.context().chronology().len(), 1);
    assert_eq!(
        cold.context().chronology()[0].block().content,
        "head before ordinary append"
    );
}

/// A pending original can commit unchanged between preparation and publication.
/// A different physical image arriving at that same boundary must refuse;
/// preparation alone cannot certify source that was not yet in the archive.
#[test]
fn raw_capture_rechecks_original_rows_committed_after_preparation() {
    for changed in [false, true] {
        let fixture = Fixture::new();
        let mut owner = ReplayEpochContext::new(
            ReplayConversationId::new("conversation-a").unwrap(),
            prefix(),
        )
        .unwrap();
        let group = ContextExecutionGroupId::new("original-delayed-append-group").unwrap();
        let frontier = owner
            .append_execution_events([ContextConversationAppend::assistant(
                "original delayed assistant",
                "original delayed source",
                group.clone(),
            )])
            .unwrap()[0];
        let raw = vec![member(&owner, 1, 1)];
        let fence = owner.capture_execution_owner(&group, frontier).unwrap();
        owner.finish_execution_group(&fence).unwrap();
        let proposal = fixture
            .store
            .prepare_replay_epoch(
                origin().capture_raw(&raw),
                "conversation-a",
                None,
                &owner,
                plan(&owner),
                ReplayHeadPublication::Summary("head after original delayed append".into()),
            )
            .unwrap();
        let mut committed = raw;
        if changed {
            committed[0].created_at_unix_seconds += 1;
        }
        let batch =
            crate::storage::transcript::TranscriptAppendBatch::owned_rows(committed.clone())
                .unwrap();
        fixture
            .store
            .commit_accounted_transcript_batch(&batch, 2)
            .unwrap();
        let result = fixture.store.publish_replay_epoch(&proposal);
        assert_eq!(fixture.store.inspect("conversation-a").unwrap(), committed);
        if changed {
            let error = result.unwrap_err();
            assert!(error.message().contains("original row image"), "{error:?}");
            assert!(
                fixture
                    .store
                    .load_replay_epoch("conversation-a", AgentContext::empty())
                    .unwrap()
                    .is_none()
            );
        } else {
            let receipt = result.unwrap();
            let (identity, cold) = fixture.load();
            assert_eq!(receipt.identity(), &identity);
            assert_eq!(cold.context().chronology().len(), 1);
            assert_eq!(
                cold.context().chronology()[0].block().content,
                "head after original delayed append"
            );
        }
    }
}

#[test]
fn publication_refuses_late_selected_group_members_and_control_only_renewal() {
    for (extend, stale_after_renewal, reclose) in [
        (false, false, false),
        (false, true, false),
        (false, false, true),
        (true, false, false),
    ] {
        let fixture = Fixture::new();
        let mut owner = ReplayEpochContext::new(
            ReplayConversationId::new("conversation-a").unwrap(),
            prefix(),
        )
        .unwrap();
        let group = ContextExecutionGroupId::new("selected-original-group").unwrap();
        let frontier = owner
            .append_execution_events([ContextConversationAppend::assistant(
                "original assistant",
                "original closed source",
                group.clone(),
            )])
            .unwrap()[0];
        let receipt = owner.capture_execution_owner(&group, frontier).unwrap();
        owner.finish_execution_group(&receipt).unwrap();
        let original_control = control(&owner, 2);
        let batch = crate::storage::transcript::TranscriptAppendBatch::owned_rows(vec![
            member(&owner, 1, 1),
            original_control.clone(),
        ])
        .unwrap();
        fixture
            .store
            .commit_accounted_transcript_batch(&batch, 1)
            .unwrap();
        // The raw semantic capture contains only its model occurrence. The
        // separate zero-model control is qualified from the physical window.
        let proposal = fixture
            .store
            .prepare_replay_epoch(
                origin().capture_raw(&batch.entries()[..1]),
                "conversation-a",
                None,
                &owner,
                plan(&owner),
                ReplayHeadPublication::Summary("must not cover renewed source".into()),
            )
            .unwrap();
        let mut later = Vec::new();
        if extend {
            owner
                .append_execution_events([ContextConversationAppend::assistant(
                    "late original assistant",
                    "original renewed source",
                    group,
                )])
                .unwrap();
            later.push(member(&owner, 3, 2));
        } else {
            let mut body = owner.into_owned_context();
            body.retain_pending_execution_group(&group).unwrap();
            owner = ReplayEpochContext::from_owned_context(
                ReplayConversationId::new("conversation-a").unwrap(),
                body,
            )
            .unwrap();
            if reclose {
                let frontier = owner.context().chronology().last().unwrap().sequence();
                let current = owner.capture_execution_owner(&group, frontier).unwrap();
                owner.finish_execution_group(&current).unwrap();
            }
        }
        later.push(control(&owner, later.len() as u64 + 3));
        if stale_after_renewal {
            // An actual delayed copy of the original closed descriptor cannot
            // erase the independently authored newer protection interval.
            let mut stale = original_control;
            stale.sequence = later.last().unwrap().sequence + 1;
            later.push(stale);
        }
        let batch = crate::storage::transcript::TranscriptAppendBatch::owned_rows(later).unwrap();
        fixture
            .store
            .commit_accounted_transcript_batch(&batch, 2)
            .unwrap();
        let audit = fixture.store.inspect("conversation-a").unwrap();
        let error = fixture.store.publish_replay_epoch(&proposal).unwrap_err();
        assert!(
            error.message().contains("selected execution group"),
            "{error:?}"
        );
        assert!(
            fixture
                .store
                .load_replay_epoch("conversation-a", AgentContext::empty())
                .unwrap()
                .is_none()
        );
        assert_eq!(fixture.store.inspect("conversation-a").unwrap(), audit);
    }
}

/// Original pending/frontier controls remain in the audit after actual closure.
/// They must be inert during publication, not mistaken for a later renewal.
#[test]
fn publication_accepts_original_pending_controls_before_selected_group_closure() {
    let fixture = Fixture::new();
    let mut owner = ReplayEpochContext::new(
        ReplayConversationId::new("conversation-a").unwrap(),
        prefix(),
    )
    .unwrap();
    let group = ContextExecutionGroupId::new("original-closed-group").unwrap();
    owner
        .append_execution_events([ContextConversationAppend::assistant(
            "first",
            "first original source",
            group.clone(),
        )])
        .unwrap();
    let first = member(&owner, 1, 1);
    let earlier = control(&owner, 2);
    let frontier = owner
        .append_execution_events([ContextConversationAppend::assistant(
            "second",
            "second original source",
            group.clone(),
        )])
        .unwrap()[0];
    let second = member(&owner, 3, 2);
    let pending = control(&owner, 4);
    let original = owner.capture_execution_owner(&group, frontier).unwrap();
    owner.finish_execution_group(&original).unwrap();
    let closed = control(&owner, 5);
    let raw = vec![first.clone(), second.clone()];
    let batch = crate::storage::transcript::TranscriptAppendBatch::owned_rows(vec![
        first, earlier, second, pending, closed,
    ])
    .unwrap();
    fixture
        .store
        .commit_accounted_transcript_batch(&batch, 1)
        .unwrap();
    let proposal = fixture
        .store
        .prepare_replay_epoch(
            origin().capture_raw(&raw),
            "conversation-a",
            None,
            &owner,
            plan(&owner),
            ReplayHeadPublication::Summary("one actual closed group".into()),
        )
        .unwrap();
    fixture.store.publish_replay_epoch(&proposal).unwrap();
    let (_, cold) = fixture
        .store
        .load_replay_epoch("conversation-a", AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(cold.context().chronology().len(), 1);
    assert_eq!(
        cold.context().chronology()[0].block().content,
        "one actual closed group"
    );
    assert_eq!(
        fixture.store.inspect("conversation-a").unwrap(),
        batch.entries()
    );
}
