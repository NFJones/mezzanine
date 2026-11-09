//! Ordinary terminal publication keeps the source that produced compaction.
//!
//! Replacing a repository after source admission must not reroute original
//! accounted rows or their accepted typed replay head. These tests exercise
//! actual bookkeeping and cold replay rather than inspecting a store label.

use super::*;

/// Actual final execution persists to its admitted original archive after
/// replacement. Two accepted rewrites keep only the latest typed head through
/// ordinary completion and reopened history; audit rows remain unchanged.
#[test]
fn terminal_compaction_publication_retains_original_turn_store() {
    for accounted in [false, true] {
        let (mut service, original, mut turn_id) = typed_source::saved(accounted);
        let conversation = service
            .agent_turn_ledger()
            .turn(&turn_id)
            .unwrap()
            .conversation_id
            .clone();
        let before = original.inspect(&conversation).unwrap();
        for summary in ["ORIGINAL_IMPORTED_SUMMARY", "ORIGINAL_LIVE_SUMMARY"] {
            if summary == "ORIGINAL_LIVE_SUMMARY" {
                turn_id = crate::test_support::compaction_source::advance_task(
                    &mut service,
                    &turn_id,
                    "next original-store task",
                );
            }
            typed_source::queue(&mut service, &turn_id, true);
            typed_source::complete(&mut service, &turn_id, summary);
        }
        let (_, accepted) = original
            .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .unwrap();
        assert_eq!(accepted.revision().get(), 3);
        assert_eq!(
            accepted.context().chronology(),
            service.agent_turn_contexts()[&turn_id].chronology()
        );
        let replacement = AgentTranscriptStore::new(temp_root("terminal-compaction-replacement"));
        let mut other = before.clone();
        for row in &mut other {
            row.role = TranscriptRole::Assistant;
            row.content = "UNRELATED_REPLACEMENT_SOURCE".into();
        }
        replacement.append_many(&other).unwrap();
        service.set_agent_transcript_store(replacement.clone());
        crate::test_support::compaction_source::complete_task(
            &mut service,
            &turn_id,
            "done",
            mez_agent::ProviderApiCompatibility::OpenAiResponses,
        );
        let rows = original.inspect(&conversation).unwrap();
        assert!(rows.starts_with(&before));
        assert!(rows.len() > before.len());
        assert!(
            !rows
                .iter()
                .any(|row| row.content.starts_with("[mez-terminal-compaction-handoff/"))
        );
        let reopened = AgentTranscriptStore::new(original.root());
        let (_, cold) = reopened
            .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .unwrap();
        assert_eq!(cold.head(), accepted.head());
        assert_eq!(cold.revision(), accepted.revision());
        assert_eq!(cold.context().chronology(), accepted.context().chronology());
        service.set_agent_transcript_store(reopened.clone());
        let replay = service.runtime_agent_history_epoch_context("%1").unwrap();
        assert_eq!(
            replay
                .blocks
                .iter()
                .filter(|block| block.content == "ORIGINAL_LIVE_SUMMARY")
                .count(),
            1
        );
        assert!(
            !replay
                .blocks
                .iter()
                .any(|block| block.content == "ORIGINAL_IMPORTED_SUMMARY"
                    || block.content.contains("LIVE_ORIGINAL_SOURCE"))
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
    let (mut service, unused, mut turn_id) = typed_source::memory();
    let conversation = service
        .agent_turn_ledger()
        .turn(&turn_id)
        .unwrap()
        .conversation_id
        .clone();
    for summary in ["MEMORY_ONLY_FIRST", "MEMORY_ONLY_SECOND"] {
        if summary == "MEMORY_ONLY_SECOND" {
            turn_id = crate::test_support::compaction_source::advance_task(
                &mut service,
                &turn_id,
                "next memory-only task",
            );
        }
        typed_source::queue(&mut service, &turn_id, true);
        typed_source::complete(&mut service, &turn_id, summary);
    }
    let accepted = mez_agent::ReplayEpochContext::from_owned_context(
        mez_agent::ReplayConversationId::new(&conversation).unwrap(),
        service.agent_turn_contexts()[&turn_id].clone(),
    )
    .unwrap();
    let replacement = AgentTranscriptStore::new(temp_root("terminal-compaction-late-store"));
    service.set_agent_transcript_store(replacement.clone());
    crate::test_support::compaction_source::complete_task(
        &mut service,
        &turn_id,
        "done",
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
    );
    assert!(!replacement.transcript_path(&conversation).unwrap().exists());
    assert!(
        replacement
            .compaction_epoch(&conversation)
            .unwrap()
            .is_none()
    );
    assert!(fs::read_dir(unused.root()).unwrap().next().is_none());
    let replay = service
        .runtime_agent_history_epoch_context("%1")
        .unwrap()
        .replay_owner()
        .unwrap()
        .unwrap();
    assert_eq!(replay.head(), accepted.head());
    assert_eq!(replay.revision(), accepted.revision());
    assert_eq!(
        replay.context().chronology()[0],
        accepted.context().chronology()[0]
    );
    assert!(
        !replay
            .context()
            .blocks()
            .iter()
            .any(|block| block.content == "MEMORY_ONLY_FIRST")
    );
    // Installing storage also cannot retarget a task admitted with the normal
    // save policy but no store. Unlike the NoStore fixture, its irreducible
    // first task has no invented prior source or accepted summary.
    let (mut service, unused, turn_id) = typed_source::missing_store();
    let conversation = service
        .agent_turn_ledger()
        .turn(&turn_id)
        .unwrap()
        .conversation_id
        .clone();
    let replacement = AgentTranscriptStore::new(temp_root("terminal-originally-missing-store"));
    service.set_agent_transcript_store(replacement.clone());
    crate::test_support::compaction_source::complete_task(
        &mut service,
        &turn_id,
        "done",
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
    );
    assert!(!replacement.transcript_path(&conversation).unwrap().exists());
    assert!(
        replacement
            .compaction_epoch(&conversation)
            .unwrap()
            .is_none()
    );
    assert!(fs::read_dir(unused.root()).unwrap().next().is_none());
}
