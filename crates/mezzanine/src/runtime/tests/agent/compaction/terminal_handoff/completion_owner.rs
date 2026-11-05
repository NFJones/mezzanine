//! Ordinary terminal publication keeps the source that produced compaction.
//!
//! Replacing a repository after source admission must not reroute original
//! accounted rows or their accepted terminal certificate. These tests exercise
//! actual bookkeeping and cold replay rather than inspecting a store label.

use super::*;

/// A terminal execution persists original imported/live occurrences and both
/// accepted summaries to the original archive after repository replacement.
/// The competing repository has valid same-CID rows with different contents;
/// it must receive neither this execution nor its compaction replay authority.
#[test]
fn terminal_compaction_publication_retains_original_turn_store() {
    for accounted in [false, true] {
        let (mut service, original, turn_id) = live_fixture_with_source_codec(200, true, accounted);
        let conversation = service
            .agent_turn_ledger()
            .turn(&turn_id)
            .unwrap()
            .conversation_id
            .clone();
        let before = original.inspect(&conversation).unwrap();
        compact_live(&mut service, &turn_id, "ORIGINAL_IMPORTED_SUMMARY", false);
        compact_live(&mut service, &turn_id, "ORIGINAL_LIVE_SUMMARY", false);
        let expected = service
            .agent_turn_contexts()
            .get(&turn_id)
            .unwrap()
            .chronology()
            .iter()
            .filter(|event| event.block().source == ContextSourceKind::CompactionSelective)
            .map(|event| event.record())
            .collect::<Vec<_>>();
        let replacement = AgentTranscriptStore::new(temp_root("terminal-compaction-replacement"));
        let mut other = before.clone();
        for row in &mut other {
            row.role = TranscriptRole::Assistant;
            row.content = "UNRELATED_REPLACEMENT_SOURCE".into();
        }
        replacement.append_many(&other).unwrap();
        service.set_agent_transcript_store(replacement.clone());
        complete_turn(&mut service, &turn_id);
        let rows = original.inspect(&conversation).unwrap();
        assert!(rows.starts_with(&before));
        assert!(rows.len() > before.len());
        let reopened = AgentTranscriptStore::new(original.root());
        let epoch = reopened.compaction_epoch(&conversation).unwrap().unwrap();
        assert_eq!(epoch.summary_records.as_ref().unwrap().ranges, expected);
        assert_eq!(
            epoch
                .ranges
                .iter()
                .map(|range| range.summary.as_str())
                .collect::<Vec<_>>(),
            vec!["ORIGINAL_IMPORTED_SUMMARY", "ORIGINAL_LIVE_SUMMARY"]
        );
        reopened
            .publish_terminal_compaction_handoffs(&conversation)
            .unwrap();
        assert_eq!(
            reopened.compaction_epoch(&conversation).unwrap(),
            Some(epoch)
        );
        assert_eq!(reopened.inspect(&conversation).unwrap(), rows);
        assert_eq!(replacement.inspect(&conversation).unwrap(), other);
        assert!(
            replacement
                .compaction_epoch(&conversation)
                .unwrap()
                .is_none()
        );
    }
}

/// Original memory-only policy survives through ordinary terminal persistence,
/// even when storage is installed after two accepted local summaries. Canonical
/// completion still finishes, without manufacturing an archive in the new store.
#[test]
fn terminal_compaction_preserves_original_no_store_policy() {
    let (mut service, _, turn_id) = live_fixture_with_source_store(200, false, false, false);
    let conversation = service
        .agent_turn_ledger()
        .turn(&turn_id)
        .unwrap()
        .conversation_id
        .clone();
    compact_live(&mut service, &turn_id, "MEMORY_ONLY_FIRST", false);
    compact_live(&mut service, &turn_id, "MEMORY_ONLY_SECOND", false);
    let replacement = AgentTranscriptStore::new(temp_root("terminal-compaction-late-store"));
    service.set_agent_transcript_store(replacement.clone());
    complete_turn(&mut service, &turn_id);
    assert!(!replacement.transcript_path(&conversation).unwrap().exists());
    assert!(
        replacement
            .compaction_epoch(&conversation)
            .unwrap()
            .is_none()
    );
}
