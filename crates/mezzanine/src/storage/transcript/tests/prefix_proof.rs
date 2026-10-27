//! Manual prefix writes compare frozen epoch authority and committed rows.
//!
//! These tests exercise the actual locked publication API independently of
//! actor generation checks. Metadata-only edits invalidate old authority while
//! an unrelated later append remains outside the captured prefix proof.

use super::*;

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
        .save_compaction_prefix_with_proof("source", 1, "first summary", &rows[..1], &None)
        .unwrap();
    let baseline = store.compaction_epoch("source").unwrap();
    store.append(&rows[3]).unwrap();
    store
        .save_compaction_prefix_with_proof("source", 3, "later summary", &rows[1..3], &baseline)
        .unwrap();
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
                    "stale result",
                    &rows[1..],
                    &baseline
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
            .save_compaction_prefix_with_proof("source", 1, "summary", &[stale], &None)
            .is_err()
    );
    assert!(store.compaction_epoch("source").unwrap().is_none());
    assert_eq!(store.inspect("source").unwrap(), [row]);
    fs::remove_dir_all(root).unwrap();
}
