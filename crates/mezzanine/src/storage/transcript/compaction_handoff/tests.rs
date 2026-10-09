//! Durable terminal handoff proof, failure, and restart regressions.

use super::*;

/// Existing selective history may cover a later group while an earlier raw
/// group remains eligible. The distinct baseline-fenced handoff must insert its
/// new range in order without changing the previously durable model summary.
#[test]
fn terminal_handoff_inserts_before_prior_selective_range() {
    let (store, rows) = fixture();
    store.append_many(&rows[..2]).unwrap();
    store
        .save_compaction_ranges(
            "handoff-conversation",
            0,
            "",
            vec![AgentCompactionRange {
                first_sequence: 2,
                through_sequence: 2,
                summary: "already durable".into(),
            }],
        )
        .unwrap();
    let baseline = store.compaction_epoch("handoff-conversation").unwrap();
    let content = TerminalCompactionHandoff {
        baseline,
        replacements: vec![TerminalCompactionReplacement {
            sources: vec![rows[0].content.clone()],
            summary: "new earlier summary".into(),
        }],
    }
    .to_content()
    .unwrap();
    store.append_many(&[row(3, content)]).unwrap();
    let epoch = store
        .compaction_epoch("handoff-conversation")
        .unwrap()
        .unwrap();
    assert_eq!(
        epoch
            .ranges
            .iter()
            .map(|range| range.summary.as_str())
            .collect::<Vec<_>>(),
        vec!["new earlier summary", "already durable"]
    );
}

/// Creates independent occurrence-bearing source and a terminal certificate.
fn fixture() -> (AgentTranscriptStore, Vec<TranscriptEntry>) {
    let root = std::env::temp_dir().join(format!(
        "mez-handoff-{}",
        crate::storage::token_usage::new_token_usage_event_id()
    ));
    let store = AgentTranscriptStore::new(root);
    let source = |group: &str| {
        TranscriptContextEvent::execution_block_with_metadata(
            mez_agent::ContextSourceKind::TranscriptAssistant,
            "same label",
            "identical text",
            mez_agent::ContextExecutionGroupId::new(group).unwrap(),
            1,
            None,
        )
        .unwrap()
        .to_transcript_content()
    };
    let sources = vec![source("first"), source("second")];
    let handoff = TerminalCompactionHandoff {
        baseline: None,
        replacements: sources
            .iter()
            .enumerate()
            .map(|(index, source)| TerminalCompactionReplacement {
                sources: vec![source.clone()],
                summary: format!("summary-{index}"),
            })
            .collect(),
    };
    let mut rows = sources
        .into_iter()
        .enumerate()
        .map(|(index, content)| row(index as u64 + 1, content))
        .collect::<Vec<_>>();
    rows.push(row(3, handoff.to_content().unwrap()));
    (store, rows)
}

/// Constructs a canonical audit row with stable conversation ownership.
fn row(sequence: u64, content: String) -> TranscriptEntry {
    TranscriptEntry {
        conversation_id: "handoff-conversation".into(),
        sequence,
        created_at_unix_seconds: 1,
        role: TranscriptRole::System,
        turn_id: "terminal-turn".into(),
        agent_id: "agent-%1".into(),
        pane_id: "%1".into(),
        content,
    }
}

/// Repeated text is not occurrence identity. Two groups remain independently
/// anchored, and repeating append/publication after reopening never duplicates
/// their summaries or mutates the archive.
#[test]
fn repeated_text_reopens_with_exactly_one_summary_per_group() {
    let (store, rows) = fixture();
    store.append_many(&rows).unwrap();
    let before = store.inspect("handoff-conversation").unwrap();
    let reopened = AgentTranscriptStore::new(store.root());
    reopened.append_many(&rows).unwrap();
    let epoch = reopened
        .compaction_epoch("handoff-conversation")
        .unwrap()
        .unwrap();
    assert_eq!(epoch.ranges.len(), 2);
    assert_eq!(epoch.ranges[0].summary, "summary-0");
    assert_eq!(epoch.ranges[1].summary, "summary-1");
    assert_eq!(reopened.inspect("handoff-conversation").unwrap(), before);
}

/// A crash after raw append but before epoch publication retains the exact
/// receipt. Restart retries only persistence, publishes both replacements in
/// one epoch, then settles that receipt without executing any actions.
#[test]
fn failed_publication_recovers_through_exact_append_receipt() {
    let (store, rows) = fixture();
    store.accept_append_receipt(&rows, 1).unwrap();
    store.fail_next_compaction_epoch_write();
    assert!(store.append_many(&rows).is_err());
    assert_eq!(store.inspect("handoff-conversation").unwrap(), rows);
    assert!(
        store
            .compaction_epoch("handoff-conversation")
            .unwrap()
            .is_none()
    );
    assert_eq!(store.pending_append_receipts().unwrap(), vec![rows]);
    let reopened = AgentTranscriptStore::new(store.root());
    reopened.recover_append_receipts().unwrap();
    assert!(reopened.pending_append_receipts().unwrap().is_empty());
    assert_eq!(
        reopened
            .compaction_epoch("handoff-conversation")
            .unwrap()
            .unwrap()
            .ranges
            .len(),
        2
    );
}

/// Partial append cannot publish a selective epoch referencing its missing
/// suffix. Exact receipt recovery appends the remaining source and certificate
/// before it installs the complete replay projection.
#[test]
fn partial_append_preserves_old_authority_until_receipt_recovery() {
    let (store, rows) = fixture();
    store.accept_append_receipt(&rows, 1).unwrap();
    store.fail_transcript_append_after_first();
    assert!(store.append_many(&rows).is_err());
    assert_eq!(store.inspect("handoff-conversation").unwrap().len(), 1);
    assert!(
        store
            .compaction_epoch("handoff-conversation")
            .unwrap()
            .is_none()
    );
    AgentTranscriptStore::new(store.root())
        .recover_append_receipts()
        .unwrap();
    assert_eq!(
        store
            .compaction_epoch("handoff-conversation")
            .unwrap()
            .unwrap()
            .ranges
            .len(),
        2
    );
}

/// A changed authoritative epoch invalidates the terminal witness. A matching
/// source alone is insufficient to overwrite a newer independently published
/// projection; the previous epoch remains intact after the failed append.
#[test]
fn stale_epoch_is_not_overwritten_by_terminal_handoff() {
    let (store, rows) = fixture();
    store.append_many(&rows[..2]).unwrap();
    store
        .save_compaction_ranges("handoff-conversation", 0, "new authority", Vec::new())
        .unwrap();
    let previous = store.compaction_epoch("handoff-conversation").unwrap();
    assert!(store.append_many(&rows[2..]).is_err());
    assert_eq!(
        store.compaction_epoch("handoff-conversation").unwrap(),
        previous
    );
}

/// Exact user barriers must never be silently swallowed to make a raw source
/// slice contiguous. Logical admission distinguishes a valid but unsupported
/// interleaved layout from missing or conflicting occurrence evidence.
#[test]
fn admission_rejects_interleaved_source_without_guessing_rows() {
    let (store, mut rows) = fixture();
    let joined = TerminalCompactionHandoff {
        baseline: None,
        replacements: vec![TerminalCompactionReplacement {
            sources: rows[..2].iter().map(|row| row.content.clone()).collect(),
            summary: "joined".into(),
        }],
    }
    .to_content()
    .unwrap();
    rows.insert(1, row(2, "suppressed display or exact barrier".into()));
    for (index, row) in rows.iter_mut().enumerate() {
        row.sequence = index as u64 + 1;
    }
    assert!(!terminal_handoff_source_is_contiguous(&joined, &rows).unwrap());
    rows.remove(2);
    assert!(terminal_handoff_source_is_contiguous(&joined, &rows).is_err());
    assert!(
        !store
            .transcript_path("handoff-conversation")
            .unwrap()
            .exists()
    );
}

/// Incomplete group coverage cannot be certified even when every listed row
/// matches: an unselected ordinal belonging to that group would otherwise be
/// replayed next to a summary of only part of its native execution.
#[test]
fn incomplete_group_keeps_epoch_unpublished() {
    let (store, mut rows) = fixture();
    let extra = TranscriptContextEvent::execution_block_with_metadata(
        mez_agent::ContextSourceKind::ActionResult,
        "result",
        "settled native result",
        mez_agent::ContextExecutionGroupId::new("first").unwrap(),
        2,
        None,
    )
    .unwrap()
    .to_transcript_content();
    rows.insert(1, row(2, extra));
    for (index, row) in rows.iter_mut().enumerate() {
        row.sequence = index as u64 + 1;
    }
    assert!(store.append_many(&rows).is_err());
    assert!(
        store
            .compaction_epoch("handoff-conversation")
            .unwrap()
            .is_none()
    );
}

/// A later legitimate prefix compaction supersedes prior terminal selective
/// summaries. Reconciliation must not mistake that durable successor for a
/// stale handoff and poison all subsequent prompt admission.
#[test]
fn later_prefix_epoch_supersedes_terminal_certificates() {
    let (store, rows) = fixture();
    store.append_many(&rows).unwrap();
    store
        .save_compaction_epoch("handoff-conversation", 3, "later prefix")
        .unwrap();
    store
        .publish_terminal_compaction_handoffs("handoff-conversation")
        .unwrap();
    assert_eq!(
        store
            .compaction_epoch("handoff-conversation")
            .unwrap()
            .unwrap()
            .summary,
        "later prefix"
    );
}
