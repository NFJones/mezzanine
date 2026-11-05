//! Manual prefix writes compare frozen epoch authority and committed rows.
//!
//! These tests exercise the actual locked publication API independently of
//! actor generation checks. Metadata-only edits invalidate old authority while
//! an unrelated later append remains outside the captured prefix proof.

use super::*;

/// Creates one explicit prefix proposal at the fixture's producer boundary.
fn prefix_summary(content: &str) -> crate::storage::transcript::CompactionSummary<'_> {
    crate::storage::transcript::CompactionSummary {
        content,
        record: mez_agent::ContextBlockRecord::allocate(
            &crate::storage::transcript::prefix_summary_block("source", content),
        )
        .unwrap(),
    }
}

/// Qualified absence permits the first prefix publication, and the resulting
/// epoch permits a later matching prefix. Appends beyond the frozen boundary
/// remain exact and do not force a guess or recharge of the selected source.
#[test]
fn prefix_proof_accepts_matching_epoch_and_preserves_later_rows() {
    let root = temp_root("prefix-proof-matching");
    let store = AgentTranscriptStore::new(root.clone());
    let rows = (1..=4)
        .map(|sequence| entry("source", sequence, TranscriptRole::Assistant))
        .collect::<Vec<_>>();
    store.append_many(&rows[..3]).unwrap();
    store
        .save_compaction_prefix_with_proof(
            "source",
            1,
            prefix_summary("first summary"),
            &rows[..1],
            &None,
            &[],
        )
        .unwrap();
    let baseline = store.compaction_epoch("source").unwrap();
    store.append(&rows[3]).unwrap();
    let next = store
        .save_compaction_prefix_with_proof(
            "source",
            3,
            prefix_summary("later summary"),
            &rows[1..3],
            &baseline,
            &[],
        )
        .unwrap();
    assert_eq!(next, 5);
    assert_eq!(store.inspect("source").unwrap(), rows);
    assert_eq!(
        store
            .compaction_epoch("source")
            .unwrap()
            .unwrap()
            .through_sequence,
        3
    );
    fs::remove_dir_all(root).unwrap();
}

/// An epoch-only edit leaves physical history unchanged but replaces source
/// authority. Neither an old existing-epoch capture nor a capture of absence
/// can publish over it. The rejection occurs inside the actual mutation lane,
/// independently of any earlier runtime freshness check.
#[test]
fn prefix_proof_rejects_changed_epoch_and_qualified_absence() {
    let root = temp_root("prefix-proof-epoch-edit");
    let store = AgentTranscriptStore::new(root.clone());
    let rows = (1..=3)
        .map(|sequence| entry("source", sequence, TranscriptRole::Assistant))
        .collect::<Vec<_>>();
    store.append_many(&rows).unwrap();
    store
        .save_compaction_epoch("source", 1, "old summary")
        .unwrap();
    let captured = store.compaction_epoch("source").unwrap();
    store
        .save_compaction_epoch("source", 1, "new authoritative summary")
        .unwrap();
    let replacement = store.compaction_epoch("source").unwrap();
    for baseline in [captured, None] {
        assert!(
            store
                .save_compaction_prefix_with_proof(
                    "source",
                    3,
                    prefix_summary("stale result"),
                    &rows[1..],
                    &baseline,
                    &[],
                )
                .is_err()
        );
        assert_eq!(store.compaction_epoch("source").unwrap(), replacement);
        assert_eq!(store.inspect("source").unwrap(), rows);
    }
    fs::remove_dir_all(root).unwrap();
}

/// Matching epoch metadata cannot authorize a different physical source. A
/// row-content mismatch fails without advancing the prefix or changing history,
/// preserving the distinction between summary authority and row authority.
#[test]
fn prefix_proof_rejects_changed_physical_source() {
    let root = temp_root("prefix-proof-rows");
    let store = AgentTranscriptStore::new(root.clone());
    let row = entry("source", 1, TranscriptRole::Assistant);
    store.append(&row).unwrap();
    let mut stale = row.clone();
    stale.content = "different captured row".into();
    assert!(
        store
            .save_compaction_prefix_with_proof(
                "source",
                1,
                prefix_summary("summary"),
                &[stale],
                &None,
                &[]
            )
            .is_err()
    );
    assert!(store.compaction_epoch("source").unwrap().is_none());
    assert_eq!(store.inspect("source").unwrap(), [row]);
    fs::remove_dir_all(root).unwrap();
}

/// Publication carries a checked pending suffix into its allocation frontier.
/// Exact overlap is deduplicated, whereas an overlapping content conflict or a
/// missing pending sequence rejects before changing the durable epoch.
#[test]
fn prefix_proof_returns_checked_pending_frontier_and_rejects_conflicts() {
    let root = temp_root("prefix-proof-pending-frontier");
    let store = AgentTranscriptStore::new(root.clone());
    let rows = (1..=4)
        .map(|sequence| entry("source", sequence, TranscriptRole::Assistant))
        .collect::<Vec<_>>();
    store.append_many(&rows[..2]).unwrap();
    assert_eq!(
        store
            .save_compaction_prefix_with_proof(
                "source",
                1,
                prefix_summary("summary"),
                &rows[..1],
                &None,
                &rows[1..3],
            )
            .unwrap(),
        4
    );
    let baseline = store.compaction_epoch("source").unwrap();
    let mut conflict = rows[1].clone();
    conflict.content = "conflicting reservation".into();
    for pending in [vec![conflict], vec![rows[3].clone()]] {
        assert!(
            store
                .save_compaction_prefix_with_proof(
                    "source",
                    2,
                    prefix_summary("rejected summary"),
                    &rows[1..2],
                    &baseline,
                    &pending,
                )
                .is_err()
        );
        assert_eq!(store.compaction_epoch("source").unwrap(), baseline);
        assert_eq!(store.inspect("source").unwrap(), rows[..2]);
    }
    fs::remove_dir_all(root).unwrap();
}

/// Uncertain prefix publication retains one producer-owned proposal through
/// pre-rename failure, durable rename without marker, and cold-store retry.
/// Both initial and extending prefixes recover, while equal-text new proposals
/// and changed physical proofs fail without replacing the accepted occurrence.
/// An unrelated conversation stays usable throughout the failed operation.
#[test]
fn prefix_proof_retries_original_proposal_after_uncertain_publication() {
    for existing in [false, true] {
        let root = temp_root("prefix-proof-proposal-retry");
        let store = AgentTranscriptStore::new(root.clone());
        let rows = (1..=4)
            .map(|sequence| entry("source", sequence, TranscriptRole::Assistant))
            .collect::<Vec<_>>();
        store.append_many(&rows).unwrap();
        if existing {
            store
                .save_compaction_epoch("source", 1, "earlier prefix")
                .unwrap();
        }
        let baseline = store.compaction_epoch("source").unwrap();
        let first = usize::from(existing);
        let frozen = &rows[first..3];
        let proposal = prefix_summary("retained accepted proposal");
        store.fail_next_compaction_epoch_before_rename();
        assert!(
            store
                .save_compaction_prefix_with_proof("source", 3, proposal, frozen, &baseline, &[],)
                .is_err()
        );
        assert_eq!(store.compaction_epoch("source").unwrap(), baseline);
        store.fail_next_compaction_epoch_before_marker();
        assert!(
            store
                .save_compaction_prefix_with_proof("source", 3, proposal, frozen, &baseline, &[],)
                .is_err()
        );
        let published = store.compaction_epoch("source").unwrap();
        assert_eq!(
            published
                .as_ref()
                .unwrap()
                .summary_records
                .as_ref()
                .unwrap()
                .prefix,
            Some(proposal.record),
        );
        store
            .append(&entry("unaffected", 1, TranscriptRole::User))
            .unwrap();
        let reopened = AgentTranscriptStore::new(root.clone());
        assert!(
            reopened
                .save_compaction_prefix_with_proof(
                    "source",
                    3,
                    prefix_summary(proposal.content),
                    frozen,
                    &baseline,
                    &[],
                )
                .is_err()
        );
        let mut changed = frozen.to_vec();
        changed[0].content = "different captured source".into();
        assert!(
            reopened
                .save_compaction_prefix_with_proof("source", 3, proposal, &changed, &baseline, &[],)
                .is_err()
        );
        assert_eq!(reopened.compaction_epoch("source").unwrap(), published);
        assert_eq!(
            reopened
                .save_compaction_prefix_with_proof("source", 3, proposal, frozen, &baseline, &[],)
                .unwrap(),
            5,
        );
        assert!(
            reopened
                .compaction_epoch_marker_is_committed("source")
                .unwrap()
        );
        assert_eq!(reopened.compaction_epoch("source").unwrap(), published);
        assert_eq!(reopened.inspect("source").unwrap(), rows);
        assert_eq!(reopened.inspect("unaffected").unwrap().len(), 1);
        fs::remove_dir_all(root).unwrap();
    }
}

/// Retained selective slots remain part of the exact retry authority. A forged
/// baseline with equal summary bytes and a fresh retained occurrence cannot
/// settle a prefix that already reached disk. The original typed baseline can
/// settle it without changing either the new prefix or inherited range record.
#[test]
fn prefix_retry_requires_original_retained_selective_records() {
    let root = temp_root("prefix-proof-retained-record-retry");
    let store = AgentTranscriptStore::new(root.clone());
    let mut rows = (1..=4)
        .map(|sequence| entry("source", sequence, TranscriptRole::Assistant))
        .collect::<Vec<_>>();
    for (index, row) in rows[2..].iter_mut().enumerate() {
        row.role = TranscriptRole::System;
        row.turn_id = "retained-turn".into();
        row.content = mez_agent::TranscriptContextEvent::execution_block_with_metadata(
            mez_agent::ContextSourceKind::TranscriptAssistant,
            "assistant",
            "retained execution",
            mez_agent::ContextExecutionGroupId::new("retained-group").unwrap(),
            index as u64 + 1,
            None,
        )
        .unwrap()
        .to_transcript_content();
    }
    store.append_many(&rows).unwrap();
    store
        .save_compaction_ranges(
            "source",
            1,
            "earlier prefix",
            vec![crate::storage::transcript::AgentCompactionRange {
                first_sequence: 3,
                through_sequence: 4,
                summary: "retained range summary".into(),
            }],
        )
        .unwrap();
    let baseline = store.compaction_epoch("source").unwrap();
    let proposal = prefix_summary("new prefix");
    store.fail_next_compaction_epoch_before_marker();
    assert!(
        store
            .save_compaction_prefix_with_proof("source", 2, proposal, &rows[1..2], &baseline, &[],)
            .is_err()
    );
    let published = store.compaction_epoch("source").unwrap();
    let mut forged = baseline.clone().unwrap();
    let replacement = mez_agent::ContextBlockRecord::allocate(
        &crate::storage::transcript::selective_summary_block(&forged.ranges[0].summary),
    )
    .unwrap();
    forged.summary_records.as_mut().unwrap().ranges[0] = replacement;
    assert!(
        store
            .save_compaction_prefix_with_proof(
                "source",
                2,
                proposal,
                &rows[1..2],
                &Some(forged),
                &[],
            )
            .is_err()
    );
    store
        .save_compaction_prefix_with_proof("source", 2, proposal, &rows[1..2], &baseline, &[])
        .unwrap();
    assert_eq!(store.compaction_epoch("source").unwrap(), published);
    assert_eq!(
        published.unwrap().summary_records.unwrap().ranges,
        baseline.unwrap().summary_records.unwrap().ranges,
    );
    assert_eq!(store.inspect("source").unwrap(), rows);
    fs::remove_dir_all(root).unwrap();
}
