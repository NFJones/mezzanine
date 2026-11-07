//! Real transcript append/reopen and canonical replay of accounted occurrences.
//! These controls exercise the persistence boundary without a paid provider or
//! importing stored metadata as instruction or execution authority.

use super::*;
use crate::storage::transcript::AgentTranscriptStore;
use mez_agent::transcript::TranscriptRole;
use mez_agent::{ContextPlacement, ContextSourceKind};
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;

mod authored_cache;
mod bootstrap;
mod cache;
mod pending_ownership;
mod raw_history;
mod recorded_tasks;
mod replay_arrivals;
mod replay_cursor;
mod replay_epoch;
mod stable_scope;

/// Owns only a generated private fixture directory.
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "mez-context-records-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn guidance(content: &str) -> ContextBlock {
    ContextBlock {
        source: ContextSourceKind::ProjectGuidance,
        placement: ContextPlacement::StablePrefix,
        label: "project guidance".to_owned(),
        content: content.to_owned(),
    }
}

/// Persists exact canonical records with a user-boundary stable snapshot.
fn persist(store: &AgentTranscriptStore) -> AgentContext {
    let mut context =
        AgentContext::import_durable_blocks(vec![guidance("actual guidance")]).unwrap();
    context
        .append_user_event("prompt", "exact user source")
        .unwrap();
    for _ in 0..2 {
        context
            .append_peer_message_event("peer", "equal é😀 source")
            .unwrap();
    }
    for (index, event) in context.chronology().iter().enumerate() {
        let block = event.block();
        let (semantic, stable) = if index == 0 {
            (
                TranscriptContextEvent::user_event(
                    event.sequence().get(),
                    block.label.clone(),
                    block.content.clone(),
                )
                .unwrap(),
                Some(context.stable_block_records()),
            )
        } else {
            (
                TranscriptContextEvent::prompt_boundary_with_event_sequence(
                    block.source,
                    event.sequence().get(),
                    block.label.clone(),
                    block.content.clone(),
                )
                .unwrap(),
                None,
            )
        };
        let content = TranscriptContextRecord::new(semantic, event.record(), stable)
            .unwrap()
            .to_transcript_content()
            .unwrap();
        store
            .append(&TranscriptEntry {
                conversation_id: "record-round-trip".to_owned(),
                sequence: index as u64 + 1,
                created_at_unix_seconds: 1,
                role: TranscriptRole::System,
                turn_id: "original-turn".to_owned(),
                agent_id: "original-agent".to_owned(),
                pane_id: "original-pane".to_owned(),
                content,
            })
            .unwrap();
    }
    context
}

/// Actual archive reopen preserves all origins/caches, including distinct equal
/// peer text. Live guidance remains authoritative: changed guidance keeps the
/// slot origin, revises its cost, and never restores saved instruction bytes.
#[test]
fn accounted_records_survive_real_store_reopen_and_live_guidance_revision() {
    let directory = Directory::new();
    let store = AgentTranscriptStore::new(directory.0.join("transcripts"));
    let original = persist(&store);
    drop(store);
    let store = AgentTranscriptStore::new(directory.0.join("transcripts"));
    let rows = store.inspect("record-round-trip").unwrap();
    let replay = super::super::context::runtime_agent_transcript_context("different-pane", &rows);
    let mut blocks = vec![guidance("actual guidance")];
    blocks.extend(replay.blocks.clone());
    let restored = replay.records.import_context(blocks).unwrap();
    assert_eq!(original.block_records(), restored.block_records());
    assert_ne!(
        restored.chronology()[1].record().occurrence_id(),
        restored.chronology()[2].record().occurrence_id()
    );
    let changed_guidance = "current trusted guidance ".repeat(100);
    let mut blocks = vec![guidance(&changed_guidance)];
    blocks.extend(replay.blocks);
    let changed = replay.records.import_context(blocks).unwrap();
    assert_eq!(changed.stable_slots()[0].block().content, changed_guidance);
    assert_eq!(
        changed.stable_slots()[0].record().occurrence_id(),
        original.stable_slots()[0].record().occurrence_id()
    );
    assert_ne!(
        changed.stable_slots()[0].record(),
        original.stable_slots()[0].record()
    );
    for (old, new) in original.chronology().iter().zip(changed.chronology()) {
        assert_eq!(old.record(), new.record());
    }
}

/// A future reserved record after genuine source cannot silently disappear and
/// leave a seemingly valid partial replay. Unrelated raw user text that merely
/// resembles metadata remains user text rather than a record decoder input.
#[test]
fn malformed_accounted_archive_refuses_partial_replay() {
    let directory = Directory::new();
    let store = AgentTranscriptStore::new(directory.0.join("transcripts"));
    persist(&store);
    let mut rows = store.inspect("record-round-trip").unwrap();
    let mut future = rows[0].clone();
    future.sequence = 4;
    future.content = "[mez-transcript-context-record/v999]\n{}".to_owned();
    store.append(&future).unwrap();
    rows = store.inspect("record-round-trip").unwrap();
    let replay = super::super::context::runtime_agent_transcript_context("pane", &rows);
    assert!(!replay.blocks.is_empty());
    assert!(replay.records.import_context(replay.blocks).is_err());
    let mut literal = future;
    literal.role = TranscriptRole::User;
    let replay =
        super::super::context::runtime_agent_transcript_context("pane", &[literal.clone()]);
    let context = replay.records.import_context(replay.blocks).unwrap();
    assert_eq!(context.blocks()[0].content, literal.content);
}

/// Legacy rows establish IDs from archived occurrences and structural positions;
/// local context order/current pane and duplicate text do not coalesce them.
#[test]
fn legacy_import_identity_uses_archive_occurrence_instead_of_text_or_pane() {
    let semantic = TranscriptContextEvent::user_event(1, "same", "same text").unwrap();
    let first = TranscriptEntry {
        conversation_id: "legacy-original".to_owned(),
        sequence: 7,
        created_at_unix_seconds: 1,
        role: TranscriptRole::System,
        turn_id: "turn".to_owned(),
        agent_id: "agent".to_owned(),
        pane_id: "old-pane".to_owned(),
        content: semantic.to_transcript_content(),
    };
    let mut second = first.clone();
    second.sequence = 8;
    let rows = [first, second];
    let first = super::super::context::runtime_agent_transcript_context("pane-a", &rows);
    let second = super::super::context::runtime_agent_transcript_context("pane-b", &rows);
    let first = first.records.import_context(first.blocks).unwrap();
    let second = second.records.import_context(second.blocks).unwrap();
    assert_eq!(first.block_records(), second.block_records());
    assert_ne!(
        first.chronology()[0].record().occurrence_id(),
        first.chronology()[1].record().occurrence_id()
    );
}
