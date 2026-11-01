//! Real RAW-prefix reads, exact source binding and recovery ownership controls.

use super::*;
use crate::storage::transcript::TranscriptPrefixExpectation;

/// Missing and short physical data remain pending; a complete original prefix
/// produces a seal bound to the original destination and every checked row.
#[test]
fn physical_prefix_requires_complete_original_rows() {
    let root = temp_root("physical-prefix-complete");
    let store = AgentTranscriptStore::new(root.clone());
    let rows = vec![
        entry("physical", 1, TranscriptRole::User),
        entry("physical", 2, TranscriptRole::Assistant),
    ];
    let source = TranscriptPrefixExpectation::capture("physical", &rows).unwrap();
    assert!(store.seal_transcript_prefix(&source).unwrap().is_none());
    store.append(&rows[0]).unwrap();
    assert!(store.seal_transcript_prefix(&source).unwrap().is_none());
    store.append(&rows[1]).unwrap();
    let seal = store.seal_transcript_prefix(&source).unwrap().unwrap();
    assert!(seal.matches(&store.transcript_path("physical").unwrap(), &source));
    assert!(!seal.matches(&root.join("unrelated"), &source));
    let mut changed = rows.clone();
    changed[0].agent_id = "foreign-agent".into();
    let foreign = TranscriptPrefixExpectation::capture("physical", &changed).unwrap();
    assert!(!seal.matches(&store.transcript_path("physical").unwrap(), &foreign));
    assert!(store.seal_transcript_prefix(&foreign).is_err());
    fs::remove_dir_all(root).unwrap();
}

/// Durable recovery ownership blocks only overlapping source rows, even if
/// their bytes already exist. Verification neither retires nor replays receipts.
#[test]
fn physical_prefix_waits_for_exact_overlapping_receipt_settlement() {
    let root = temp_root("physical-prefix-receipts");
    let store = AgentTranscriptStore::new(root.clone());
    let row = entry("physical", 1, TranscriptRole::User);
    let source = TranscriptPrefixExpectation::capture("physical", [&row]).unwrap();
    store
        .accept_append_receipt(std::slice::from_ref(&row), 1)
        .unwrap();
    store.append(&row).unwrap();
    assert!(store.seal_transcript_prefix(&source).unwrap().is_none());
    assert_eq!(store.pending_append_receipts().unwrap().len(), 1);
    store
        .settle_append_receipt(std::slice::from_ref(&row), 1)
        .unwrap();
    let later = entry("physical", 2, TranscriptRole::Assistant);
    store.accept_append_receipt(&[later], 2).unwrap();
    assert!(store.seal_transcript_prefix(&source).unwrap().is_some());
    assert_eq!(store.pending_append_receipts().unwrap().len(), 1);
    assert_eq!(
        fs::read_to_string(store.transcript_path("physical").unwrap()).unwrap(),
        format!("{}\n", encode_transcript_entry(&row).unwrap())
    );
    fs::remove_dir_all(root).unwrap();
}

/// The required prefix is finite: unrelated trailing bytes do not enlarge its
/// authority, while an incomplete or oversized required row cannot be accepted.
#[test]
fn physical_prefix_bounds_required_rows_and_ignores_later_tail() {
    use std::io::Write;
    let root = temp_root("physical-prefix-bounds");
    let store = AgentTranscriptStore::new(root.clone());
    let row = entry("physical", 1, TranscriptRole::User);
    let source = TranscriptPrefixExpectation::capture("physical", [&row]).unwrap();
    store.append(&row).unwrap();
    let path = store.transcript_path("physical").unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"unrelated corrupt tail")
        .unwrap();
    assert!(store.seal_transcript_prefix(&source).unwrap().is_some());
    let encoded = encode_transcript_entry(&row).unwrap();
    fs::write(&path, &encoded).unwrap();
    assert!(store.seal_transcript_prefix(&source).is_err());
    fs::write(&path, "x".repeat(encoded.len() + 100)).unwrap();
    assert!(store.seal_transcript_prefix(&source).is_err());
    fs::write(&path, format!("{encoded}\r\n")).unwrap();
    assert!(store.seal_transcript_prefix(&source).unwrap().is_some());
    fs::remove_dir_all(root).unwrap();
}

/// Expectations reject missing sequence positions and foreign conversations
/// before creating work, preventing a caller from digesting an arbitrary subset.
#[test]
fn physical_prefix_capture_rejects_noncontiguous_or_foreign_rows() {
    let row = entry("physical", 2, TranscriptRole::User);
    assert!(TranscriptPrefixExpectation::capture("physical", [&row]).is_err());
    let row = entry("foreign", 1, TranscriptRole::User);
    assert!(TranscriptPrefixExpectation::capture("physical", [&row]).is_err());
}
