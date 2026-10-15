//! Authoritative content-CAS protection after selective epoch publication.
//!
//! Typed execution metadata remains valid in every proposed edit: rejection must
//! come from committed coverage, not from an accidentally malformed source row.

use super::*;

/// Builds one structurally valid execution row while varying only its body.
fn execution_row(sequence: u64, body: &str) -> TranscriptEntry {
    let mut row = entry("selective-edit", sequence, TranscriptRole::System);
    let (group, ordinal) = match sequence {
        2..=4 => ("first-group", sequence - 1),
        6..=8 => ("second-group", sequence - 5),
        _ => ("uncovered-group", 1),
    };
    row.content = mez_agent::TranscriptContextEvent::execution_block_with_metadata(
        mez_agent::ContextSourceKind::TranscriptAssistant,
        "assistant",
        body,
        mez_agent::ContextExecutionGroupId::new(group).unwrap(),
        ordinal,
        None,
    )
    .unwrap()
    .to_transcript_content();
    row
}

/// Captures all durable bytes, including sidecars and the shared catalog, while
/// ignoring no files. Reads do not create or refresh derived metadata.
fn persisted_bytes(root: &std::path::Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &std::path::Path, path: &std::path::Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for child in fs::read_dir(path).unwrap() {
            let path = child.unwrap().path();
            if path.is_dir() {
                visit(root, &path, out);
            } else {
                out.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut bytes = BTreeMap::new();
    visit(root, root, &mut bytes);
    bytes
}

/// A complete typed group summarized under prefix zero must become immutable.
/// Editing its body would otherwise keep the group structurally valid while
/// leaving the old model-authored summary authoritative and hiding the edit.
#[test]
fn selective_epoch_rejects_content_only_edit_before_any_persisted_change() {
    let root = temp_root("selective-edit-after-publication");
    let store = AgentTranscriptStore::new(root.clone());
    store.initialize(100).unwrap();
    store
        .append(&entry("selective-edit", 1, TranscriptRole::User))
        .unwrap();
    let rows = (2..=4)
        .map(|sequence| execution_row(sequence, "original"))
        .collect::<Vec<_>>();
    store.append_many(&rows).unwrap();
    store
        .save_compaction_ranges_with_proof(
            super::super::AgentCompactionEpoch {
                version: 2,
                conversation_id: "selective-edit".to_string(),
                through_sequence: 0,
                summary: String::new(),
                ranges: vec![super::super::AgentCompactionRange {
                    first_sequence: 2,
                    through_sequence: 4,
                    summary: "original group summary".to_string(),
                }],
            },
            &rows,
        )
        .unwrap();
    let before = persisted_bytes(&root);
    let result = store.compare_and_swap_entry_content(
        "selective-edit",
        3,
        "%1",
        &store.transcript_entry_revision(&rows[1]).unwrap(),
        execution_row(3, "changed body").content,
    );
    assert!(
        result.is_err(),
        "selectively summarized row accepted content edit: {result:?}"
    );
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("already represented by a compaction epoch")
    );
    assert_eq!(persisted_bytes(&root), before);
    assert_eq!(store.inspect("selective-edit").unwrap()[2], rows[1]);
    fs::remove_dir_all(root).unwrap();
}

/// Publishes two disjoint complete groups, with exact rows before, between, and
/// after them. Both zero and nonzero prefix boundaries use this same fixture.
fn publish_disjoint_epoch(store: &AgentTranscriptStore, prefix: u64) -> Vec<TranscriptEntry> {
    let rows = (1..=9)
        .map(|sequence| match sequence {
            2..=4 | 6..=8 => execution_row(sequence, "original"),
            _ => entry("selective-edit", sequence, TranscriptRole::User),
        })
        .collect::<Vec<_>>();
    store.append_many(&rows).unwrap();
    let selected = rows
        .iter()
        .filter(|row| matches!(row.sequence, 2..=4 | 6..=8))
        .cloned()
        .collect::<Vec<_>>();
    store
        .save_compaction_ranges_with_proof(
            super::super::AgentCompactionEpoch {
                version: 2,
                conversation_id: "selective-edit".to_string(),
                through_sequence: prefix,
                summary: if prefix == 0 {
                    String::new()
                } else {
                    "prefix summary".to_string()
                },
                ranges: vec![
                    super::super::AgentCompactionRange {
                        first_sequence: 2,
                        through_sequence: 4,
                        summary: "first summary".to_string(),
                    },
                    super::super::AgentCompactionRange {
                        first_sequence: 6,
                        through_sequence: 8,
                        summary: "second summary".to_string(),
                    },
                ],
            },
            &selected,
        )
        .unwrap();
    rows
}

/// First, interior, and last rows of every range are protected, independently
/// of a zero/nonzero prefix. Exact gaps and suffixes remain editable; their
/// changes survive reopening once, with the original authoritative epoch.
#[test]
fn selective_epoch_content_cas_checks_inclusive_ranges_and_uncovered_controls() {
    for prefix in [0, 1] {
        let root = temp_root("selective-edit-boundaries");
        let store = AgentTranscriptStore::new(root.clone());
        store.initialize(100).unwrap();
        let rows = publish_disjoint_epoch(&store, prefix);
        let epoch = store.compaction_epoch("selective-edit").unwrap();
        for sequence in 1..=9 {
            let row = &rows[sequence as usize - 1];
            let before = persisted_bytes(&root);
            let content = match sequence {
                2..=4 | 6..=8 => execution_row(sequence, "edited").content,
                _ => format!("uncovered edit {sequence}"),
            };
            let result = store.compare_and_swap_entry_content(
                "selective-edit",
                sequence,
                "%1",
                &store.transcript_entry_revision(row).unwrap(),
                content.clone(),
            );
            if sequence <= prefix || matches!(sequence, 2..=4 | 6..=8) {
                assert!(
                    result
                        .unwrap_err()
                        .message()
                        .contains("already represented by a compaction epoch")
                );
                assert_eq!(persisted_bytes(&root), before);
            } else {
                assert!(matches!(
                    result.unwrap(),
                    CompareAndSwapTranscriptEntryResult::Updated(_)
                ));
                let reopened = AgentTranscriptStore::new(root.clone());
                let current = reopened.inspect("selective-edit").unwrap();
                assert_eq!(current.len(), 9);
                assert_eq!(
                    current.iter().filter(|row| row.content == content).count(),
                    1
                );
                assert_eq!(current[sequence as usize - 1].content, content);
            }
            assert_eq!(store.compaction_epoch("selective-edit").unwrap(), epoch);
        }
        fs::remove_dir_all(root).unwrap();
    }
}

/// A required epoch cannot disappear or become malformed and thereby authorize
/// an edit, even to an uncovered row. The error precedes all durable mutation.
#[test]
fn selective_epoch_content_cas_fails_closed_on_missing_or_corrupt_required_epoch() {
    for corruption in [None, Some(b"not json".as_slice())] {
        let root = temp_root("selective-edit-required-epoch");
        let store = AgentTranscriptStore::new(root.clone());
        let rows = publish_disjoint_epoch(&store, 0);
        let path = root.join("selective-edit/compaction-epoch.json");
        if let Some(bytes) = corruption {
            fs::write(&path, bytes).unwrap();
        } else {
            fs::remove_file(&path).unwrap();
        }
        let before = persisted_bytes(&root);
        for sequence in [3, 5] {
            assert!(
                store
                    .compare_and_swap_entry_content(
                        "selective-edit",
                        sequence,
                        "%1",
                        &store
                            .transcript_entry_revision(&rows[sequence as usize - 1])
                            .unwrap(),
                        "must not persist".to_string()
                    )
                    .is_err()
            );
            assert_eq!(persisted_bytes(&root), before);
        }
        fs::remove_dir_all(root).unwrap();
    }
}

/// Selective coverage must not change stale/deleted/wrong-pane outcomes for
/// uncovered entries, and legacy prefix-only epochs retain their prior fence.
#[test]
fn selective_epoch_content_cas_preserves_conflicts_and_legacy_prefix() {
    let root = temp_root("selective-edit-conflicts");
    let store = AgentTranscriptStore::new(root.clone());
    let rows = publish_disjoint_epoch(&store, 0);
    let row = &rows[4];
    let revision = store.transcript_entry_revision(row).unwrap();
    let before = persisted_bytes(&root);
    assert_eq!(
        store
            .compare_and_swap_entry_content(
                "selective-edit",
                5,
                "%other",
                &revision,
                "wrong pane".to_string()
            )
            .unwrap(),
        CompareAndSwapTranscriptEntryResult::WrongPane
    );
    assert!(matches!(
        store
            .compare_and_swap_entry_content(
                "selective-edit",
                5,
                "%1",
                &"0".repeat(64),
                "stale".to_string()
            )
            .unwrap(),
        CompareAndSwapTranscriptEntryResult::Stale { .. }
    ));
    assert_eq!(
        store
            .compare_and_swap_entry_content(
                "selective-edit",
                10,
                "%1",
                &revision,
                "deleted".to_string()
            )
            .unwrap(),
        CompareAndSwapTranscriptEntryResult::Deleted
    );
    assert_eq!(persisted_bytes(&root), before);
    fs::remove_dir_all(root).unwrap();

    let root = temp_root("legacy-prefix-edit");
    let store = AgentTranscriptStore::new(root.clone());
    let rows = [
        entry("selective-edit", 1, TranscriptRole::User),
        entry("selective-edit", 2, TranscriptRole::User),
    ];
    store.append_many(&rows).unwrap();
    store
        .save_compaction_epoch("selective-edit", 1, "legacy prefix")
        .unwrap();
    assert_eq!(
        store
            .compaction_epoch("selective-edit")
            .unwrap()
            .unwrap()
            .version,
        1
    );
    let before = persisted_bytes(&root);
    assert!(
        store
            .compare_and_swap_entry_content(
                "selective-edit",
                1,
                "%1",
                &store.transcript_entry_revision(&rows[0]).unwrap(),
                "protected".to_string()
            )
            .is_err()
    );
    assert_eq!(persisted_bytes(&root), before);
    assert!(matches!(
        store
            .compare_and_swap_entry_content(
                "selective-edit",
                2,
                "%1",
                &store.transcript_entry_revision(&rows[1]).unwrap(),
                "uncovered".to_string()
            )
            .unwrap(),
        CompareAndSwapTranscriptEntryResult::Updated(_)
    ));
    fs::remove_dir_all(root).unwrap();
}
