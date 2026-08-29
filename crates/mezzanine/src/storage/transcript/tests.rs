//! Tests for transcript persistence, forking, and TSV escaping.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
    sync::{
        Arc, Barrier,
        atomic::{AtomicU64, Ordering},
    },
    thread,
};

use super::encoding::{
    decode_structured_prompt_history_entry, decode_transcript_entry, encode_prompt_history_entry,
    encode_structured_prompt_history_entry, encode_transcript_entry,
};
use super::store::{PRESENTATION_CLEAR_TAIL_COMPACT_BYTES, PROMPT_HISTORY_COMPACTION_BYTES};
use super::{
    AgentPresentationEntry, AgentTranscriptStore, SavedSessionCursor, SavedSessionLifecycleFilter,
    SavedSessionPageAnchor, SavedSessionQuery,
};
use mez_agent::transcript::{AgentSessionMetadata, TranscriptEntry, TranscriptRole};
use mez_mux::readline::{ReadlineHistoryEntry, ReadlinePasteRange};
use rusqlite::Connection;

/// Builds a per-test temporary root that is unique within the current process.
fn temp_root(name: &str) -> PathBuf {
    static NEXT_TEMP_ROOT_ID: AtomicU64 = AtomicU64::new(0);

    let unique = NEXT_TEMP_ROOT_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "mez-transcript-{name}-{}-{unique}",
        std::process::id()
    ))
}

/// Runs the entry operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn entry(conversation_id: &str, sequence: u64, role: TranscriptRole) -> TranscriptEntry {
    TranscriptEntry {
        conversation_id: conversation_id.to_string(),
        sequence,
        created_at_unix_seconds: 10 + sequence,
        role,
        turn_id: format!("turn-{sequence}"),
        agent_id: "a1".to_string(),
        pane_id: "%1".to_string(),
        content: format!("content {sequence}"),
    }
}

/// Builds minimal valid active-session metadata for persistence and migration tests.
fn agent_session_metadata(
    mezzanine_session_id: &str,
    conversation_id: &str,
) -> AgentSessionMetadata {
    AgentSessionMetadata {
        mezzanine_session_id: mezzanine_session_id.to_string(),
        pane_id: "%1".to_string(),
        conversation_id: conversation_id.to_string(),
        prompt_cache_lineage_id: format!("lineage-{conversation_id}"),
        visibility: "visible".to_string(),
        running_turn_id: None,
        running_turn_kind: None,
        transcript_entries: 0,
        log_level: "normal".to_string(),
        pane_model_profile: None,
        planning_enabled: false,
        response_style: None,
        directive: None,
        routing_enabled: None,
        root_routing_policy: None,
        approval_policy: None,
        pane_permission_preset_override: None,
        pane_approval_policy_override: None,
        working_directory: None,
        project_root: None,
        token_usage: Default::default(),
        token_usage_by_model: BTreeMap::new(),
        context_usage: None,
        context_usage_snapshot: None,
        latest_request_usage: None,
    }
}

/// Builds one presentation fixture with multiline display and copy payloads.
fn presentation(conversation_id: &str, sequence: u64) -> AgentPresentationEntry {
    AgentPresentationEntry {
        conversation_id: conversation_id.to_string(),
        sequence,
        created_at_unix_seconds: 20 + sequence,
        pane_id: "%1".to_string(),
        turn_id: Some(format!("turn-{sequence}")),
        terminal_width: 80,
        style_names: vec!["assistant".to_string(), "status".to_string()],
        display_lines: vec!["mez> hello".to_string(), "agent: done".to_string()],
        copy_lines: vec!["mez> raw hello".to_string(), "agent: raw done".to_string()],
        ansi_text: Some("\r\n\u{1b}[1m▐ mez> hello\u{1b}[0m\r\n".to_string()),
        source_text: None,
        source_content_type: None,
    }
}

/// Builds one modest presentation entry for a test-owned compaction threshold.
fn compacting_presentation(conversation_id: &str, sequence: u64) -> AgentPresentationEntry {
    let mut entry = presentation(conversation_id, sequence);
    entry.display_lines = vec![format!("mez> {}", "x".repeat(2 * 1024))];
    entry.style_names = vec!["assistant".to_string()];
    entry.copy_lines = entry.display_lines.clone();
    entry.ansi_text = None;
    entry
}

/// Verifies production stores retain the documented 256 KiB cleartext tail.
#[test]
fn transcript_store_uses_production_presentation_compaction_threshold() {
    assert_eq!(PRESENTATION_CLEAR_TAIL_COMPACT_BYTES, 256 * 1024);
}

/// Verifies that the store can append, list, inspect, and delete one
/// conversation using the durable TSV representation.
#[test]
fn transcript_store_appends_lists_inspects_and_deletes_conversations() {
    let root = temp_root("basic");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append(&entry("conv1", 1, TranscriptRole::User))
        .unwrap();
    store
        .append(&entry("conv1", 2, TranscriptRole::Assistant))
        .unwrap();

    let entries = store.inspect("conv1").unwrap();
    let summaries = store.list().unwrap();

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[1].role, TranscriptRole::Assistant);
    assert!(root.join("conv1").join("history.tsv").exists());
    assert!(!root.join("conv1.tsv").exists());
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].entries, 2);
    assert_eq!(summaries[0].last_turn_id, "turn-2");
    assert!(store.delete("conv1").unwrap());
    assert!(store.inspect("conv1").is_err());

    let _ = fs::remove_dir_all(root);
}

/// Verifies conversation-kind metadata round trips, legacy sessions default to
/// root, malformed sidecars fail closed, and deletion removes the sidecar.
///
/// Resume discovery depends on this classification, so corrupt metadata must
/// never silently expose a delegated child as an ordinary root conversation.
#[test]
fn transcript_store_persists_validates_and_deletes_conversation_kind() {
    let root = temp_root("conversation-kind");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append(&entry("legacy-root", 1, TranscriptRole::User))
        .unwrap();
    store
        .append(&entry("durable-child", 1, TranscriptRole::User))
        .unwrap();

    assert_eq!(
        store.conversation_kind("legacy-root").unwrap(),
        mez_agent::AgentConversationKind::Root
    );
    store
        .save_conversation_kind("durable-child", mez_agent::AgentConversationKind::Subagent)
        .unwrap();
    assert_eq!(
        store.conversation_kind("durable-child").unwrap(),
        mez_agent::AgentConversationKind::Subagent
    );
    assert_eq!(
        store
            .saved_sessions()
            .unwrap()
            .into_iter()
            .find(|session| session.summary.conversation_id == "durable-child")
            .unwrap()
            .conversation_kind,
        mez_agent::AgentConversationKind::Subagent
    );

    fs::write(
        root.join("durable-child").join("metadata.json"),
        b"{\"version\":1,\"conversation_kind\":\"unknown\"}\n",
    )
    .unwrap();
    let error = store.conversation_kind("durable-child").unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(
        error
            .message()
            .contains("invalid conversation metadata kind")
    );

    assert!(store.delete("durable-child").unwrap());
    assert!(!root.join("durable-child").join("metadata.json").exists());
    let _ = fs::remove_dir_all(root);
}

/// Verifies oversized presentation tails are moved into concatenated zstd
/// frames while later cleartext appends remain replayable after them.
#[test]
fn transcript_store_compacts_presentation_tail_into_zstd_history() {
    let root = temp_root("presentation-zstd");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone())
        .with_presentation_compaction_threshold(1024)
        .unwrap();
    let first = compacting_presentation("conv1", 1);
    let second = compacting_presentation("conv1", 2);
    let third = presentation("conv1", 3);

    store.append_presentation(&first).unwrap();
    store.append_presentation(&second).unwrap();
    store.append_presentation(&third).unwrap();

    let inspected = store.inspect_presentation("conv1").unwrap();
    let next = store.next_presentation_sequence("conv1").unwrap();
    let compressed_path = store.presentation_compressed_path("conv1").unwrap();
    let cleartext_path = store.presentation_path("conv1").unwrap();

    assert!(compressed_path.exists());
    assert!(cleartext_path.exists());
    assert_eq!(
        inspected,
        vec![
            first.normalized_for_agent_log_wrap(),
            second.normalized_for_agent_log_wrap(),
            third.normalized_for_agent_log_wrap()
        ]
    );
    assert_eq!(next, 4);
    let _ = fs::remove_dir_all(root);
}

/// Verifies durable presentation appends normalize display and copy rows to the
/// recorded pane width so replay does not depend on terminal soft wrapping.
#[test]
fn transcript_store_wraps_presentation_rows_to_recorded_terminal_width() {
    let root = temp_root("presentation-wrap");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    let mut entry = presentation("conv1", 1);
    entry.terminal_width = 12;
    entry.style_names = vec!["assistant".to_string()];
    entry.display_lines = vec!["mez> alpha beta gamma".to_string()];
    entry.copy_lines = vec!["copy alpha beta gamma".to_string()];

    store.append_presentation(&entry).unwrap();

    let inspected = store.inspect_presentation("conv1").unwrap();

    assert_eq!(inspected[0].display_lines, vec!["mez> alpha", "beta gamma"]);
    assert_eq!(inspected[0].style_names, vec!["assistant", "assistant"]);
    assert_eq!(inspected[0].copy_lines, vec!["copy alpha", "beta gamma"]);
    assert!(inspected[0].ansi_text.is_none());
    let _ = fs::remove_dir_all(root);
}

/// Verifies source-backed presentation records preserve their semantic payload
/// rather than normalizing a geometry-specific projection during persistence.
#[test]
fn transcript_store_round_trips_source_backed_presentation_without_wrapping() {
    let root = temp_root("presentation-source");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    let mut entry = presentation("conv1", 1);
    entry.terminal_width = 12;
    entry.display_lines = vec!["mez> alpha beta gamma".to_string()];
    entry.style_names = vec!["assistant".to_string()];
    entry.copy_lines = vec!["alpha beta gamma".to_string()];
    entry.ansi_text = None;
    entry.source_text = Some("# Heading\n\nalpha beta gamma".to_string());
    entry.source_content_type = Some("text/markdown; charset=utf-8".to_string());

    store.append_presentation(&entry).unwrap();

    let inspected = store.inspect_presentation("conv1").unwrap();

    assert_eq!(inspected, vec![entry]);
    let _ = fs::remove_dir_all(root);
}

/// Verifies presentation row normalization wraps rows at the recorded terminal
/// width, independently of the process-wide agent log wrap configuration.
#[test]
fn transcript_store_wraps_presentation_rows_at_recorded_width() {
    let root = temp_root("presentation-cap");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    let mut entry = presentation("conv1", 1);
    entry.terminal_width = 120;
    entry.style_names = vec!["assistant".to_string()];
    entry.display_lines = vec!["x".repeat(130)];
    entry.copy_lines = entry.display_lines.clone();
    entry.ansi_text = None;

    store.append_presentation(&entry).unwrap();

    let inspected = store.inspect_presentation("conv1").unwrap();

    assert_eq!(inspected[0].display_lines[0].len(), 120);
    assert_eq!(inspected[0].display_lines[1].len(), 10);
    assert_eq!(inspected[0].copy_lines, inspected[0].display_lines);
    let _ = fs::remove_dir_all(root);
}

/// Verifies that `append_many` preserves ordinary transcript append semantics
/// while reporting encoded bytes for async persistence diagnostics.
#[test]
fn transcript_store_append_many_reports_written_bytes() {
    let root = temp_root("append-many");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    let entries = vec![
        entry("conv1", 1, TranscriptRole::User),
        entry("conv1", 2, TranscriptRole::Assistant),
    ];

    let bytes = store.append_many(&entries).unwrap();
    let inspected = store.inspect("conv1").unwrap();

    assert!(bytes > 0);
    assert_eq!(inspected, entries);
    let _ = fs::remove_dir_all(root);
}

/// Verifies deleting one durable entry rewrites the remaining transcript in
/// order, keeps append sequencing contiguous, and refreshes summary metadata.
#[test]
fn transcript_store_deletes_one_entry_and_rebuilds_summary() {
    let root = temp_root("delete-entry");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append_many(&[
            entry("conv1", 1, TranscriptRole::User),
            entry("conv1", 2, TranscriptRole::Tool),
            entry("conv1", 3, TranscriptRole::Assistant),
        ])
        .unwrap();

    assert!(store.delete_entry("conv1", 2).unwrap());
    assert!(!store.delete_entry("conv1", 9).unwrap());

    let inspected = store.inspect("conv1").unwrap();
    assert_eq!(inspected.len(), 2);
    assert_eq!(inspected[0].content, "content 1");
    assert_eq!(inspected[0].sequence, 1);
    assert_eq!(inspected[1].content, "content 3");
    assert_eq!(inspected[1].sequence, 2);
    assert_eq!(store.next_sequence("conv1").unwrap(), 3);

    let summary = store.summary("conv1").unwrap().unwrap();
    assert_eq!(summary.entries, 2);
    assert_eq!(summary.last_turn_id, "turn-3");
    let _ = fs::remove_dir_all(root);
}

/// Verifies that durable presentation entries are persisted separately from
/// model-facing transcript entries while retaining multiline copy text.
#[test]
fn transcript_store_appends_and_inspects_presentation_entries() {
    let root = temp_root("presentation");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    let first = presentation("conv1", 1);
    let second = presentation("conv1", 2);

    store.append_presentation(&first).unwrap();
    store.append_presentation(&second).unwrap();
    let inspected = store.inspect_presentation("conv1").unwrap();
    let next = store.next_presentation_sequence("conv1").unwrap();

    assert_eq!(inspected, vec![first, second]);
    assert_eq!(next, 3);
    assert!(root.join("conv1").join("presentation.tsv").exists());
    assert!(store.inspect("conv1").is_err());
    let _ = fs::remove_dir_all(root);
}

/// Verifies saved-session listing uses the summary sidecar instead of decoding
/// the whole transcript after each conversation append.
///
/// Resume pickers and `/resume --latest` only need bounded metadata. This
/// regression corrupts the durable transcript after the summary sidecar exists;
/// listing must still use the sidecar and avoid the full transcript decode that
/// would otherwise fail before the saved-session picker can render.
#[test]
fn transcript_store_list_uses_summary_sidecar_without_full_decode() {
    let root = temp_root("summary-sidecar");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    let mut first = entry("conv1", 1, TranscriptRole::System);
    first.content = "project_root=/workspace/mezzanine".to_string();
    let mut second = entry("conv1", 2, TranscriptRole::User);
    second.content = "continue the performance work".to_string();

    store.append(&first).unwrap();
    store.append(&second).unwrap();
    fs::write(
        store.transcript_path("conv1").unwrap(),
        "not a transcript\n",
    )
    .unwrap();

    let summaries = store.list().unwrap();

    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].conversation_id, "conv1");
    assert_eq!(summaries[0].entries, 2);
    assert_eq!(
        summaries[0].directory.as_deref(),
        Some("/workspace/mezzanine")
    );
    assert_eq!(
        summaries[0].latest_user_prompt.as_deref(),
        Some("continue the performance work")
    );
    assert!(store.inspect("conv1").is_err());
    let _ = fs::remove_dir_all(root);
}

/// Verifies presentation sequence allocation and bounded replay do not decode
/// compressed historical presentation frames.
///
/// Presentation histories can contain large compressed prefixes. Appending a new
/// row and resuming the visible tail should depend on the sequence index and the
/// cleartext tail only, so a corrupt legacy compressed prefix cannot force an
/// O(history) decode on ordinary append/resume paths.
#[test]
fn transcript_store_presentation_index_and_recent_replay_skip_compressed_history() {
    let root = temp_root("presentation-index");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone())
        .with_presentation_compaction_threshold(1024)
        .unwrap();

    store
        .append_presentation(&compacting_presentation("conv1", 1))
        .unwrap();
    fs::write(
        store.presentation_compressed_path("conv1").unwrap(),
        b"not zstd",
    )
    .unwrap();

    assert_eq!(store.next_presentation_sequence("conv1").unwrap(), 2);
    store
        .append_presentation(&presentation("conv1", 2))
        .unwrap();

    let recent = store
        .inspect_recent_presentation("conv1", 10, 1024 * 1024)
        .unwrap();

    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].sequence, 2);
    assert_eq!(store.next_presentation_sequence("conv1").unwrap(), 3);
    assert!(store.inspect_presentation("conv1").is_err());
    let _ = fs::remove_dir_all(root);
}

/// Verifies recent transcript inspection reads only the requested tail entries
/// and reports the next append sequence from that bounded tail.
///
/// Agent prompt assembly only needs recent transcript context. This regression
/// keeps that path independent from full-file reads so an unexpectedly large
/// transcript cannot be copied into memory just to find the latest entries.
#[test]
fn transcript_store_inspects_recent_entries_and_next_sequence_from_tail() {
    let root = temp_root("recent-tail");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    for sequence in 1..=12 {
        store
            .append(&entry("conv1", sequence, TranscriptRole::User))
            .unwrap();
    }

    let recent = store.inspect_recent("conv1", 2, 256).unwrap();
    let next_sequence = store.next_sequence("conv1").unwrap();

    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0].sequence, 11);
    assert_eq!(recent[1].sequence, 12);
    assert_eq!(next_sequence, 13);
    let _ = fs::remove_dir_all(root);
}

/// Verifies retention applies inclusive age expiry before count enforcement,
/// includes named active sessions, protects live ids, and exempts archives.
#[test]
fn transcript_store_enforces_age_then_count_retention() {
    let root = temp_root("saved-session-retention");
    let _ = fs::remove_dir_all(&root);
    let mut store = AgentTranscriptStore::new(root.clone());
    store
        .set_saved_session_retention_policy(super::SavedSessionRetentionPolicy {
            max_active_sessions: 3,
            retention_days: 1,
        })
        .unwrap();
    let now = 200_000;
    let cutoff = now - 24 * 60 * 60;
    for (conversation_id, created_at) in [
        ("named-at-cutoff", cutoff),
        ("archived-old", cutoff - 1),
        ("protected-old", cutoff - 1),
        ("count-old", cutoff + 1),
        ("count-middle", cutoff + 2),
        ("count-new", cutoff + 3),
    ] {
        let mut transcript_entry = entry(conversation_id, 1, TranscriptRole::User);
        transcript_entry.created_at_unix_seconds = created_at;
        store.append(&transcript_entry).unwrap();
    }
    store
        .name_session("named-at-cutoff", "Named but expiring", cutoff, None)
        .unwrap();
    store.archive_session("archived-old", cutoff + 10).unwrap();
    let protected = ["protected-old".to_string()].into_iter().collect();

    let report = store
        .enforce_saved_session_retention(now, &protected)
        .unwrap();

    assert_eq!(
        report.deleted_conversation_ids,
        vec!["named-at-cutoff", "count-old"]
    );
    assert!(report.failures.is_empty());
    assert!(store.saved_session("named-at-cutoff").unwrap().is_none());
    assert!(store.saved_session("count-old").unwrap().is_none());
    assert!(store.saved_session("protected-old").unwrap().is_some());
    assert_eq!(
        store
            .saved_session("archived-old")
            .unwrap()
            .unwrap()
            .archived_at_unix_seconds,
        Some(cutoff + 10)
    );
    assert!(store.saved_session("count-middle").unwrap().is_some());
    assert!(store.saved_session("count-new").unwrap().is_some());
    let _ = fs::remove_dir_all(root);
}

/// Verifies names are durable independent metadata, merge with transcript
/// summaries, support zero-entry conversations, and disappear on deletion.
#[test]
fn transcript_store_persists_and_merges_named_sessions() {
    let root = temp_root("named-sessions");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append(&entry("with-history", 1, TranscriptRole::User))
        .unwrap();

    store
        .name_session(
            "with-history",
            "  Release investigation  ",
            20,
            Some("/repo".to_string()),
        )
        .unwrap();
    store
        .name_session("empty-session", "Empty but durable", 30, None)
        .unwrap();

    let reopened = AgentTranscriptStore::new(root.clone());
    let sessions = reopened.saved_sessions().unwrap();
    let with_history = sessions
        .iter()
        .find(|session| session.summary.conversation_id == "with-history")
        .unwrap();
    assert_eq!(with_history.name.as_deref(), Some("Release investigation"));
    assert_eq!(with_history.summary.entries, 1);
    let empty = sessions
        .iter()
        .find(|session| session.summary.conversation_id == "empty-session")
        .unwrap();
    assert_eq!(empty.name.as_deref(), Some("Empty but durable"));
    assert_eq!(empty.summary.entries, 0);
    assert_eq!(empty.summary.last_created_at_unix_seconds, 30);

    assert!(reopened.delete("empty-session").unwrap());
    assert!(reopened.named_session("empty-session").unwrap().is_none());
    assert_eq!(reopened.saved_sessions().unwrap().len(), 1);
    let _ = fs::remove_dir_all(root);
}

/// Verifies count ties use UUID order and one failed candidate does not prevent
/// independent later deletions in the same retention pass.
#[test]
fn transcript_store_retention_orders_ties_and_reports_partial_failures() {
    let root = temp_root("saved-session-retention-failures");
    let _ = fs::remove_dir_all(&root);
    let mut store = AgentTranscriptStore::new(root.clone());
    store
        .set_saved_session_retention_policy(super::SavedSessionRetentionPolicy {
            max_active_sessions: 1,
            retention_days: 365,
        })
        .unwrap();
    for conversation_id in ["fail", "middle", "new"] {
        let mut transcript_entry = entry(conversation_id, 1, TranscriptRole::User);
        transcript_entry.created_at_unix_seconds = 100;
        store.append(&transcript_entry).unwrap();
    }
    fs::remove_file(root.join(".conversation-locks/fail.lock")).unwrap();
    fs::create_dir(root.join(".conversation-locks/fail.lock")).unwrap();

    let report = store
        .enforce_saved_session_retention(101, &BTreeSet::new())
        .unwrap();

    assert_eq!(report.deleted_conversation_ids, vec!["middle", "new"]);
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.failures[0].conversation_id, "fail");
    assert!(store.saved_session("fail").unwrap().is_some());
    assert!(store.saved_session("middle").unwrap().is_none());
    assert!(store.saved_session("new").unwrap().is_none());
    let _ = fs::remove_dir_all(root);
}

/// Verifies invalid names are rejected before the name index is mutated.
#[test]
fn transcript_store_rejects_invalid_session_names() {
    let root = temp_root("invalid-session-names");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());

    assert!(store.name_session("conv", "   ", 1, None).is_err());
    assert!(store.name_session("conv", "line\nbreak", 1, None).is_err());
    assert!(
        store
            .name_session("conv", &"x".repeat(81), 1, None)
            .is_err()
    );
    assert!(store.named_sessions().unwrap().is_empty());
    let _ = fs::remove_dir_all(root);
}

/// Verifies that async transcript and shared prompt-history writes use the same
/// durable layout and decoding behavior as the synchronous store API.
#[tokio::test]
async fn transcript_store_async_appends_entries_and_prompt_history() {
    let root = temp_root("async");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    let entries = vec![
        entry("conv1", 1, TranscriptRole::User),
        entry("conv1", 2, TranscriptRole::Assistant),
    ];

    let bytes = store.append_many_async(&entries).await.unwrap();
    assert!(
        store
            .append_prompt_history_async("conv1", "inspect project")
            .await
            .unwrap()
    );
    assert!(
        store
            .append_command_prompt_history_async("list-buffers")
            .await
            .unwrap()
    );
    assert!(
        !store
            .append_command_prompt_history_async("list-buffers")
            .await
            .unwrap()
    );

    let inspected = store.inspect("conv1").unwrap();
    let history = store.prompt_history("conv1").unwrap();
    let command_history = store.command_prompt_history_async().await.unwrap();

    assert!(bytes > 0);
    assert_eq!(inspected, entries);
    assert_eq!(history, vec![String::from("inspect project")]);
    assert_eq!(command_history, vec![String::from("list-buffers")]);
    assert!(root.join("conv1").join("history.tsv").exists());
    assert!(root.join("prompt-history.tsv").exists());
    assert!(!root.join("conv1").join("prompt-history.tsv").exists());
    assert!(root.join("command-prompt-history.tsv").exists());
    let _ = fs::remove_dir_all(root);
}

/// Verifies that forking creates a new conversation identity with copied
/// entries and a replacement creation time.
#[test]
fn transcript_store_forks_conversation_to_fresh_identity() {
    let root = temp_root("fork");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append(&entry("conv1", 1, TranscriptRole::User))
        .unwrap();
    store
        .append_presentation(&presentation("conv1", 1))
        .unwrap();

    let summary = store.fork("conv1", "conv2", 99).unwrap();
    let forked = store.inspect("conv2").unwrap();
    let forked_presentation = store.inspect_presentation("conv2").unwrap();

    assert_eq!(summary.conversation_id, "conv2");
    assert_eq!(forked[0].conversation_id, "conv2");
    assert_eq!(forked[0].created_at_unix_seconds, 99);
    assert_eq!(forked[0].content, "content 1");
    assert_eq!(forked_presentation[0].conversation_id, "conv2");
    assert_eq!(forked_presentation[0].created_at_unix_seconds, 99);
    assert_eq!(forked_presentation[0].display_lines[0], "mez> hello");

    let _ = fs::remove_dir_all(root);
}

/// Verifies that standard config-root placement uses a parent agent-session
/// directory with one child directory per conversation id.
#[test]
fn transcript_store_under_config_root_uses_session_directories() {
    let config_root = temp_root("config-root");
    let _ = fs::remove_dir_all(&config_root);
    let store = AgentTranscriptStore::under_config_root(config_root.clone());

    store
        .append(&entry("conv1", 1, TranscriptRole::User))
        .unwrap();

    assert_eq!(store.root(), config_root.join("agent-sessions"));
    assert_eq!(
        store.session_dir("conv1").unwrap(),
        config_root.join("agent-sessions").join("conv1")
    );
    assert_eq!(
        store.transcript_path("conv1").unwrap(),
        config_root
            .join("agent-sessions")
            .join("conv1")
            .join("history.tsv")
    );

    let _ = fs::remove_dir_all(config_root);
}

/// Verifies that submitted agent prompts are retained in one shared history
/// file, survive lookup through any conversation, and are not copied by forks.
#[test]
fn transcript_store_persists_prompt_history_in_shared_file() {
    let root = temp_root("prompt-history");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append(&entry("conv1", 1, TranscriptRole::User))
        .unwrap();
    assert!(store.append_prompt_history("conv1", "list files").unwrap());
    assert!(!store.append_prompt_history("conv1", "list files").unwrap());
    assert!(
        store
            .append_prompt_history("conv1", "build project")
            .unwrap()
    );
    assert!(store.append_prompt_history("conv1", "list files").unwrap());
    assert!(store.append_prompt_history("conv2", "run tests").unwrap());

    let history = vec![
        String::from("list files"),
        String::from("build project"),
        String::from("list files"),
        String::from("run tests"),
    ];
    assert_eq!(store.prompt_history("conv1").unwrap(), history);
    assert_eq!(store.prompt_history("conv2").unwrap(), history);
    assert!(root.join("prompt-history.tsv").exists());
    assert!(!root.join("conv1").join("prompt-history.tsv").exists());
    assert!(!root.join("conv2").join("prompt-history.tsv").exists());
    assert_eq!(store.list().unwrap().len(), 1);
    assert_eq!(
        fs::read_to_string(root.join("prompt-history.tsv"))
            .unwrap()
            .lines()
            .count(),
        history.len()
    );

    let fork = store.fork("conv1", "conv3", 99).unwrap();
    assert_eq!(fork.conversation_id, "conv3");
    assert_eq!(store.prompt_history("conv3").unwrap(), history);
    assert!(!root.join("conv3").join("prompt-history.tsv").exists());

    let _ = fs::remove_dir_all(root);
}

/// Verifies pathological prompt history rejects one oversized entry and
/// compacts append-only rows to the newest aggregate byte-bounded set.
#[test]
fn transcript_store_bounds_and_compacts_pathological_prompt_history() {
    let root = temp_root("prompt-history-bounds");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    let oversized = "z".repeat(mez_mux::readline::MAX_READLINE_HISTORY_ENTRY_BYTES + 1);

    assert!(!store.append_prompt_history("conv1", &oversized).unwrap());
    let prompt_bytes = mez_mux::readline::MAX_READLINE_HISTORY_ENTRY_BYTES - 3;
    for index in 0..9 {
        let prompt = format!("{index:02}-{}", "x".repeat(prompt_bytes));
        assert!(store.append_prompt_history("conv1", &prompt).unwrap());
    }

    let history = store.prompt_history("reader").unwrap();
    assert_eq!(history.len(), 4);
    assert!(
        history.iter().map(String::len).sum::<usize>()
            <= mez_mux::readline::MAX_READLINE_HISTORY_BYTES
    );
    assert!(
        history
            .first()
            .is_some_and(|entry| entry.starts_with("05-"))
    );
    assert!(history.last().is_some_and(|entry| entry.starts_with("08-")));
    assert!(
        fs::metadata(root.join("prompt-history.tsv")).unwrap().len()
            <= PROMPT_HISTORY_COMPACTION_BYTES
    );

    let _ = fs::remove_dir_all(root);
}

/// Verifies that the temporary per-conversation history layout is imported
/// once in deterministic path order without deleting the source files.
#[test]
fn transcript_store_imports_isolated_prompt_history_once() {
    let root = temp_root("prompt-history-migration");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("conversation-b")).unwrap();
    fs::create_dir_all(root.join("conversation-a")).unwrap();
    fs::write(
        root.join("prompt-history.tsv"),
        format!(
            "{}\n{}\n",
            encode_prompt_history_entry("shared first").unwrap(),
            encode_prompt_history_entry("shared first").unwrap()
        ),
    )
    .unwrap();
    fs::write(
        root.join("conversation-a").join("prompt-history.tsv"),
        format!(
            "{}\n{}\n{}\n",
            encode_prompt_history_entry("shared first").unwrap(),
            encode_prompt_history_entry("from a").unwrap(),
            encode_prompt_history_entry("from a").unwrap()
        ),
    )
    .unwrap();
    fs::write(
        root.join("conversation-b").join("prompt-history.tsv"),
        format!(
            "{}\n{}\n",
            encode_prompt_history_entry("from a").unwrap(),
            encode_prompt_history_entry("from b").unwrap()
        ),
    )
    .unwrap();
    let store = AgentTranscriptStore::new(root.clone());

    assert_eq!(
        store.prompt_history("current").unwrap(),
        vec![
            String::from("shared first"),
            String::from("from a"),
            String::from("from b"),
        ]
    );
    assert!(root.join(".prompt-history-shared-v1").exists());
    assert!(
        root.join("conversation-a")
            .join("prompt-history.tsv")
            .exists()
    );
    fs::write(
        root.join("conversation-a").join("prompt-history.tsv"),
        format!(
            "{}\n",
            encode_prompt_history_entry("late legacy row").unwrap()
        ),
    )
    .unwrap();
    assert_eq!(
        store.prompt_history("other").unwrap(),
        vec![
            String::from("shared first"),
            String::from("from a"),
            String::from("from b"),
        ]
    );

    let _ = fs::remove_dir_all(root);
}

/// Verifies that overlapping store handles cannot lose accepted shared prompt
/// history entries during the read-modify-rewrite operation.
#[test]
fn transcript_store_serializes_shared_prompt_history_writers() {
    let root = temp_root("prompt-history-concurrency");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    let writers = 24;
    let barrier = Arc::new(Barrier::new(writers));
    let handles = (0..writers)
        .map(|index| {
            let store = store.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                store
                    .append_prompt_history(
                        &format!("conversation-{index}"),
                        &format!("prompt-{index}"),
                    )
                    .unwrap();
            })
        })
        .collect::<Vec<_>>();
    for handle in handles {
        handle.join().unwrap();
    }

    let history = store.prompt_history("reader").unwrap();
    assert_eq!(history.len(), writers);
    for index in 0..writers {
        assert!(history.contains(&format!("prompt-{index}")));
    }

    let _ = fs::remove_dir_all(root);
}

/// Verifies that primary command prompt history is stored separately from the
/// agent prompt history while using the same shared, bounded reload behavior.
#[test]
fn transcript_store_persists_command_prompt_history_in_shared_file() {
    let root = temp_root("command-history");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    assert!(
        store
            .append_prompt_history("conv1", "agent prompt")
            .unwrap()
    );
    assert!(store.append_command_prompt_history("help").unwrap());
    assert!(!store.append_command_prompt_history("help").unwrap());
    assert!(store.append_command_prompt_history("list-buffers").unwrap());
    assert!(store.append_command_prompt_history("help").unwrap());

    assert_eq!(
        store.command_prompt_history().unwrap(),
        vec![
            String::from("help"),
            String::from("list-buffers"),
            String::from("help"),
        ]
    );
    assert_eq!(
        store.prompt_history("conv1").unwrap(),
        vec![String::from("agent prompt")]
    );
    assert!(root.join("command-prompt-history.tsv").exists());
    assert!(root.join("prompt-history.tsv").exists());
    assert!(!root.join("conv1").join("prompt-history.tsv").exists());
    assert_eq!(
        fs::read_to_string(root.join("command-prompt-history.tsv"))
            .unwrap()
            .lines()
            .count(),
        3
    );

    let _ = fs::remove_dir_all(root);
}

/// Verifies that active agent-session metadata is replaced per Mezzanine
/// session while preserving rows for unrelated sessions.
#[test]
fn transcript_store_replaces_agent_session_metadata_per_mezzanine_session() {
    let root = temp_root("agent-session-metadata");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    let owned_token_usage_key = mez_agent::ModelTokenUsageKey::new("openai", "gpt-fast");
    let owned_token_usage = mez_agent::ModelTokenUsage {
        input_tokens: 100,
        output_tokens: 20,
        reasoning_tokens: 5,
        cached_input_tokens: Some(80),
        cache_write_input_tokens: Some(12),
    };
    let owned = AgentSessionMetadata {
        running_turn_id: Some("turn-1".to_string()),
        transcript_entries: 2,
        log_level: "trace".to_string(),
        pane_model_profile: Some("work".to_string()),
        planning_enabled: true,
        response_style: Some("concise".to_string()),
        directive: Some("Prefer focused regressions.".to_string()),
        routing_enabled: Some(true),
        root_routing_policy: Some("in-place".to_string()),
        approval_policy: Some("full-access".to_string()),
        pane_permission_preset_override: Some("auto".to_string()),
        pane_approval_policy_override: Some("full-access".to_string()),
        working_directory: Some("/workspace/live".to_string()),
        project_root: Some("/workspace".to_string()),
        token_usage: owned_token_usage,
        token_usage_by_model: BTreeMap::from([(owned_token_usage_key, owned_token_usage)]),
        context_usage: Some("10%".to_string()),
        context_usage_snapshot: Some(mez_agent::AgentContextUsageSnapshot {
            input_tokens: 100,
            context_window_tokens: 1000,
            cached_input_tokens: Some(80),
        }),
        latest_request_usage: Some(mez_agent::LatestModelRequestUsage {
            model: mez_agent::ModelTokenUsageKey::new("openai", "gpt-fast"),
            usage: owned_token_usage,
        }),
        ..agent_session_metadata("$live", "conv1")
    };
    let foreign = AgentSessionMetadata {
        visibility: "hidden".to_string(),
        transcript_entries: 1,
        ..agent_session_metadata("$other", "foreign")
    };

    assert_eq!(
        store
            .save_agent_session_metadata("$live", std::slice::from_ref(&owned))
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .save_agent_session_metadata("$other", std::slice::from_ref(&foreign))
            .unwrap(),
        1
    );
    let replacement = AgentSessionMetadata {
        conversation_id: "conv2".to_string(),
        running_turn_id: None,
        transcript_entries: 3,
        ..owned.clone()
    };
    assert_eq!(
        store
            .save_agent_session_metadata("$live", std::slice::from_ref(&replacement))
            .unwrap(),
        1
    );

    let live = store.load_agent_session_metadata("$live").unwrap();
    let other = store.load_agent_session_metadata("$other").unwrap();

    assert_eq!(live, vec![replacement]);
    assert_eq!(other, vec![foreign]);
    assert!(store.list().unwrap().is_empty());
    assert!(store.agent_session_metadata_file().exists());
    let _ = fs::remove_dir_all(root);
}

/// Verifies catalog migration ignores root-owned control TSVs while importing
/// a genuine legacy transcript and preserving each control store unchanged.
///
/// Existing installations normally contain active-session metadata plus shared
/// agent and command prompt histories beside legacy `<conversation-id>.tsv`
/// transcripts. Startup and later exact-row repair must classify those reserved
/// files by ownership rather than attempting to decode them as transcripts.
#[test]
fn transcript_store_catalog_migration_ignores_root_control_tsv_files() {
    let root = temp_root("catalog-root-control-tsv");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    let active_metadata = agent_session_metadata("$live", "active-conversation");
    store
        .save_agent_session_metadata("$live", std::slice::from_ref(&active_metadata))
        .unwrap();
    assert!(
        store
            .append_prompt_history("active-conversation", "inspect the migration")
            .unwrap()
    );
    assert!(store.append_command_prompt_history("list-buffers").unwrap());
    let legacy = entry("legacy", 1, TranscriptRole::User);
    fs::write(
        root.join("legacy.tsv"),
        format!("{}\n", encode_transcript_entry(&legacy).unwrap()),
    )
    .unwrap();

    store.initialize(100).unwrap();

    assert!(store.catalog_saved_session("legacy").unwrap().is_some());
    for reserved_id in [
        "active-agent-sessions",
        "command-prompt-history",
        "prompt-history",
    ] {
        assert!(store.catalog_saved_session(reserved_id).unwrap().is_none());
        assert!(store.saved_session(reserved_id).unwrap().is_none());
    }
    assert_eq!(
        store.load_agent_session_metadata("$live").unwrap(),
        vec![active_metadata]
    );
    assert_eq!(
        store.prompt_history("active-conversation").unwrap(),
        vec![String::from("inspect the migration")]
    );
    assert_eq!(
        store.command_prompt_history().unwrap(),
        vec![String::from("list-buffers")]
    );
    assert!(root.join(".catalog-migrated-v1").is_file());
    let _ = fs::remove_dir_all(root);
}

/// Verifies that transcript TSV escaping preserves newlines and tabs across an
/// encode/decode round trip.
#[test]
fn transcript_entry_round_trips_escaped_content() {
    let original = TranscriptEntry {
        content: "line one\nline\ttwo".to_string(),
        ..entry("conv1", 1, TranscriptRole::Tool)
    };

    let decoded = decode_transcript_entry(&encode_transcript_entry(&original).unwrap()).unwrap();

    assert_eq!(decoded, original);
}

/// Verifies v2 prompt-history rows preserve multiple UTF-8-aligned pasted
/// ranges while legacy v1 rows load as literal text with no invented provenance.
#[test]
fn prompt_history_codec_preserves_v2_ranges_and_loads_v1_literally() {
    let text = format!("typed {} middle {} end", "a".repeat(1100), "界".repeat(400));
    let first_start = "typed ".len();
    let second_start = first_start + 1100 + " middle ".len();
    let entry = ReadlineHistoryEntry {
        text: text.clone(),
        collapsed_paste_ranges: vec![
            ReadlinePasteRange {
                start: first_start,
                end: first_start + 1100,
            },
            ReadlinePasteRange {
                start: second_start,
                end: second_start + "界".len() * 400,
            },
        ],
    };

    let encoded = encode_structured_prompt_history_entry(&entry).unwrap();
    assert_eq!(
        decode_structured_prompt_history_entry(&encoded).unwrap(),
        entry
    );

    let legacy = format!("mez-agent-prompt-history/1\t{}", "z".repeat(1200));
    let decoded_legacy = decode_structured_prompt_history_entry(&legacy).unwrap();
    assert_eq!(decoded_legacy.text, "z".repeat(1200));
    assert!(decoded_legacy.collapsed_paste_ranges.is_empty());
    assert_eq!(decoded_legacy.rendered(), decoded_legacy.text);
}

/// Verifies both durable readline histories retain collapsed-paste ranges
/// across store reconstruction and replace duplicate raw-text metadata with
/// the newest submitted representation.
#[test]
fn transcript_store_round_trips_structured_agent_and_command_history() {
    let root = temp_root("structured-prompt-history");
    let _ = fs::remove_dir_all(&root);
    let raw = format!("before {} after", "p".repeat(1200));
    let literal = ReadlineHistoryEntry::literal(raw.clone());
    let structured = ReadlineHistoryEntry {
        text: raw.clone(),
        collapsed_paste_ranges: vec![ReadlinePasteRange {
            start: "before ".len(),
            end: "before ".len() + 1200,
        }],
    };
    let store = AgentTranscriptStore::new(root.clone());

    assert!(
        store
            .append_structured_prompt_history("conv", &literal)
            .unwrap()
    );
    assert!(
        store
            .append_structured_prompt_history("conv", &structured)
            .unwrap()
    );
    assert!(
        store
            .append_structured_command_prompt_history(&structured)
            .unwrap()
    );

    let reopened = AgentTranscriptStore::new(root.clone());
    assert_eq!(
        reopened.structured_prompt_history("conv").unwrap(),
        vec![structured.clone()]
    );
    assert_eq!(
        reopened.structured_command_prompt_history().unwrap(),
        vec![structured]
    );
    assert_eq!(reopened.prompt_history("conv").unwrap(), vec![raw]);

    let _ = fs::remove_dir_all(root);
}

/// Verifies first initialization imports every supported saved-session layout
/// into the private catalog without moving or deleting source payloads.
///
/// Existing installations can contain current directory sessions, legacy
/// root-level transcripts, presentation-only histories, subagents, and named
/// conversations with no payload. The one-time migration must preserve their
/// discovery metadata and prefer a directory payload over a duplicate legacy
/// transcript for the same durable identity.
#[test]
fn transcript_store_catalog_migrates_existing_session_metadata() {
    let root = temp_root("catalog-migration");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());

    let mut current = entry("current", 1, TranscriptRole::User);
    current.content = "project_root=/workspace/current\nship the catalog".to_string();
    store.append(&current).unwrap();
    store
        .save_conversation_kind("current", mez_agent::AgentConversationKind::Subagent)
        .unwrap();
    store
        .name_session("current", "Current session", 40, None)
        .unwrap();
    store
        .append_presentation(&presentation("presentation-only", 1))
        .unwrap();
    store
        .name_session(
            "presentation-only",
            "Presentation only",
            41,
            Some("/workspace/presentation".to_string()),
        )
        .unwrap();
    store
        .name_session("named-empty", "No payload yet", 42, None)
        .unwrap();

    let legacy = entry("legacy", 1, TranscriptRole::User);
    fs::write(
        root.join("legacy.tsv"),
        format!("{}\n", encode_transcript_entry(&legacy).unwrap()),
    )
    .unwrap();
    store
        .append(&entry("duplicate", 1, TranscriptRole::User))
        .unwrap();
    let duplicate_legacy = entry("duplicate", 1, TranscriptRole::Assistant);
    fs::write(
        root.join("duplicate.tsv"),
        format!("{}\n", encode_transcript_entry(&duplicate_legacy).unwrap()),
    )
    .unwrap();

    store.initialize(100).unwrap();

    let current = store.catalog_saved_session("current").unwrap().unwrap();
    assert_eq!(current.name.as_deref(), Some("Current session"));
    assert_eq!(
        current.conversation_kind,
        mez_agent::AgentConversationKind::Subagent
    );
    assert_eq!(
        current.summary.directory.as_deref(),
        Some("/workspace/current")
    );
    let presentation = store
        .catalog_saved_session("presentation-only")
        .unwrap()
        .unwrap();
    assert_eq!(presentation.summary.entries, 0);
    assert_eq!(presentation.summary.last_turn_id, "turn-1");
    assert_eq!(
        presentation.summary.directory.as_deref(),
        Some("/workspace/presentation")
    );
    let named_empty = store.catalog_saved_session("named-empty").unwrap().unwrap();
    assert_eq!(named_empty.summary.entries, 0);
    assert_eq!(named_empty.summary.last_created_at_unix_seconds, 42);

    let connection = Connection::open(store.catalog_path()).unwrap();
    let records: i64 = connection
        .query_row("SELECT COUNT(*) FROM saved_conversations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(records, 5);
    let legacy_layout: String = connection
        .query_row(
            "SELECT payload_layout FROM saved_conversations WHERE conversation_id = 'legacy'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(legacy_layout, "legacy-tsv");
    let duplicate_layout: String = connection
        .query_row(
            "SELECT payload_layout FROM saved_conversations WHERE conversation_id = 'duplicate'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(duplicate_layout, "directory");
    let presentation_flags: (i64, i64) = connection
        .query_row(
            "SELECT has_transcript, has_presentation FROM saved_conversations
             WHERE conversation_id = 'presentation-only'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(presentation_flags, (0, 1));
    drop(connection);

    assert!(root.join("current").join("history.tsv").exists());
    assert!(root.join("legacy.tsv").exists());
    assert!(root.join("duplicate.tsv").exists());
    assert!(root.join("named-sessions.json").exists());
    assert!(root.join(".catalog-migrated-v1").exists());
    let _ = fs::remove_dir_all(root);
}

/// Verifies later startups preserve dual-written catalog metadata, while
/// deleting only the database causes retained sidecars to be imported again.
///
/// The marker makes ordinary startup bounded. The database itself remains
/// disposable, so losing it must recover catalog rows without touching saved
/// transcript payloads.
#[test]
fn transcript_store_catalog_restart_is_bounded_and_missing_database_recovers() {
    let root = temp_root("catalog-restart");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append(&entry("before-migration", 1, TranscriptRole::User))
        .unwrap();
    store.initialize(100).unwrap();

    store
        .name_session("after-migration", "Not reimported", 101, None)
        .unwrap();
    store.initialize(102).unwrap();
    assert_eq!(
        store
            .catalog_saved_session("after-migration")
            .unwrap()
            .unwrap()
            .name
            .as_deref(),
        Some("Not reimported")
    );

    fs::remove_file(store.catalog_path()).unwrap();
    store.initialize(103).unwrap();
    assert!(
        store
            .catalog_saved_session("before-migration")
            .unwrap()
            .is_some()
    );
    assert_eq!(
        store
            .catalog_saved_session("after-migration")
            .unwrap()
            .unwrap()
            .name
            .as_deref(),
        Some("Not reimported")
    );
    assert!(root.join("before-migration").join("history.tsv").exists());
    let _ = fs::remove_dir_all(root);
}

/// Verifies the catalog schema, indexes, integrity check, and private Unix
/// permissions are installed on an empty transcript root.
#[test]
fn transcript_store_catalog_initializes_private_indexed_schema() {
    let root = temp_root("catalog-schema");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());

    store.initialize(100).unwrap();

    let connection = Connection::open(store.catalog_path()).unwrap();
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 2);
    let quick_check: String = connection
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(quick_check, "ok");
    let indexes: Vec<String> = connection
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'index' AND tbl_name = 'saved_conversations'
             ORDER BY name",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        indexes
            .iter()
            .any(|name| name == "saved_conversations_latest_root")
    );
    assert!(
        indexes
            .iter()
            .any(|name| name == "saved_conversations_picker")
    );
    assert!(
        indexes
            .iter()
            .any(|name| name == "saved_conversations_pruning")
    );
    drop(connection);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(store.catalog_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let _ = fs::remove_dir_all(root);
}

/// Verifies a schema-v1 catalog is upgraded in place with nullable archive
/// metadata while preserving every existing active saved-conversation row.
#[test]
fn transcript_store_catalog_migrates_v1_rows_to_active_v2_rows() {
    let root = temp_root("catalog-v1-v2-migration");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let store = AgentTranscriptStore::new(root.clone());
    let connection = Connection::open(store.catalog_path()).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE saved_conversations (
                 conversation_id TEXT PRIMARY KEY NOT NULL,
                 conversation_kind TEXT NOT NULL DEFAULT 'root',
                 name TEXT,
                 named_at INTEGER,
                 entry_count INTEGER NOT NULL DEFAULT 0,
                 first_created_at INTEGER NOT NULL,
                 last_created_at INTEGER NOT NULL,
                 last_turn_id TEXT NOT NULL DEFAULT '',
                 agent_id TEXT NOT NULL DEFAULT '',
                 pane_id TEXT NOT NULL DEFAULT '',
                 directory TEXT,
                 initial_prompt TEXT,
                 latest_user_prompt TEXT,
                 has_transcript INTEGER NOT NULL DEFAULT 0,
                 has_presentation INTEGER NOT NULL DEFAULT 0,
                 payload_layout TEXT NOT NULL DEFAULT 'directory',
                 catalog_updated_at INTEGER NOT NULL
             );
             INSERT INTO saved_conversations (
                 conversation_id, conversation_kind, entry_count,
                 first_created_at, last_created_at, last_turn_id,
                 agent_id, pane_id, latest_user_prompt, has_transcript,
                 payload_layout, catalog_updated_at
             ) VALUES (
                 'legacy-active', 'root', 1, 10, 20, 'turn-1',
                 'agent', '%1', 'legacy prompt', 1, 'directory', 20
             );
             PRAGMA user_version = 1;",
        )
        .unwrap();
    drop(connection);
    fs::write(root.join(".catalog-migrated-v1"), b"complete\n").unwrap();

    store.initialize(100).unwrap();

    let connection = Connection::open(store.catalog_path()).unwrap();
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 2);
    let lifecycle: (Option<i64>, Option<i64>, Option<String>) = connection
        .query_row(
            "SELECT archived_at, archive_compressed_bytes, archive_sha256
             FROM saved_conversations WHERE conversation_id = 'legacy-active'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(lifecycle, (None, None, None));
    let _ = fs::remove_dir_all(root);
}

/// Verifies a readable catalog from a future release fails closed rather than
/// being overwritten or downgraded during startup initialization.
#[test]
fn transcript_store_catalog_rejects_future_schema_versions() {
    let root = temp_root("catalog-future-schema");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let store = AgentTranscriptStore::new(root.clone());
    let connection = Connection::open(store.catalog_path()).unwrap();
    connection.pragma_update(None, "user_version", 3).unwrap();
    drop(connection);

    let error = store.initialize(100).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert!(error.message().contains("newer than supported version 2"));
    let connection = Connection::open(store.catalog_path()).unwrap();
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 3);
    let _ = fs::remove_dir_all(root);
}

/// Verifies startup recognizes an unreadable SQLite file, preserves it as the
/// rebuild backup, and reconstructs the catalog from retained session files.
///
/// Catalog corruption must not make durable transcripts unavailable or cause
/// payload cleanup. Only explicit SQLite corruption classes trigger this
/// recovery path; readable future schemas remain protected by the preceding
/// regression.
#[test]
fn transcript_store_catalog_recovers_from_corrupt_database() {
    let root = temp_root("catalog-corruption");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append(&entry("recoverable", 1, TranscriptRole::User))
        .unwrap();
    store.initialize(100).unwrap();

    fs::write(store.catalog_path(), b"not a sqlite database").unwrap();
    let _ = fs::remove_file(format!("{}-wal", store.catalog_path().display()));
    let _ = fs::remove_file(format!("{}-shm", store.catalog_path().display()));

    store.initialize(101).unwrap();

    assert!(
        store
            .catalog_saved_session("recoverable")
            .unwrap()
            .is_some()
    );
    assert_eq!(
        fs::read(root.join(".catalog.sqlite3.backup")).unwrap(),
        b"not a sqlite database"
    );
    assert!(root.join("recoverable").join("history.tsv").exists());
    let _ = fs::remove_dir_all(root);
}

/// Verifies an explicit rebuild replaces divergent catalog contents from the
/// retained filesystem snapshot and keeps the prior valid database as backup.
#[test]
fn transcript_store_catalog_explicit_rebuild_restores_metadata() {
    let root = temp_root("catalog-explicit-rebuild");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append(&entry("restore-me", 1, TranscriptRole::User))
        .unwrap();
    store.initialize(100).unwrap();

    let connection = Connection::open(store.catalog_path()).unwrap();
    connection
        .execute("DELETE FROM saved_conversations", [])
        .unwrap();
    drop(connection);
    assert!(store.catalog_saved_session("restore-me").unwrap().is_none());

    store.rebuild_catalog(101).unwrap();

    assert!(store.catalog_saved_session("restore-me").unwrap().is_some());
    assert!(root.join(".catalog.sqlite3.backup").exists());
    assert!(root.join("restore-me").join("history.tsv").exists());
    let _ = fs::remove_dir_all(root);
}

/// Verifies ordinary metadata mutations update the catalog only after their
/// filesystem payload or compatibility sidecar has been persisted.
///
/// Phase-two discovery reads SQLite directly, so transcript appends,
/// presentation-only sessions, conversation-kind changes, entry rewrites,
/// naming, name clearing, and deletion must remain visible without a rebuild.
#[test]
fn transcript_store_catalog_dual_writes_session_mutations() {
    let root = temp_root("catalog-dual-writes");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    store.initialize(100).unwrap();

    store
        .append(&entry("conversation", 1, TranscriptRole::User))
        .unwrap();
    store
        .append(&entry("conversation", 2, TranscriptRole::Assistant))
        .unwrap();
    store
        .save_conversation_kind("conversation", mez_agent::AgentConversationKind::Subagent)
        .unwrap();
    store
        .name_session("conversation", "Catalogued", 30, Some("/repo".to_string()))
        .unwrap();
    store
        .append_presentation(&presentation("presentation-only", 1))
        .unwrap();

    let conversation = store
        .catalog_saved_session("conversation")
        .unwrap()
        .unwrap();
    assert_eq!(conversation.summary.entries, 2);
    assert_eq!(conversation.name.as_deref(), Some("Catalogued"));
    assert_eq!(
        conversation.conversation_kind,
        mez_agent::AgentConversationKind::Subagent
    );
    let presentation_only = store
        .catalog_saved_session("presentation-only")
        .unwrap()
        .unwrap();
    assert_eq!(presentation_only.summary.entries, 0);
    assert_eq!(presentation_only.summary.last_turn_id, "turn-1");

    assert!(store.delete_entry("conversation", 2).unwrap());
    assert_eq!(
        store
            .catalog_saved_session("conversation")
            .unwrap()
            .unwrap()
            .summary
            .entries,
        1
    );
    assert!(store.clear_session_name("conversation").unwrap());
    assert!(
        store
            .catalog_saved_session("conversation")
            .unwrap()
            .unwrap()
            .name
            .is_none()
    );

    store
        .name_session("conversation", "Keep empty", 31, None)
        .unwrap();
    assert!(store.delete_entry("conversation", 1).unwrap());
    let named_empty = store
        .catalog_saved_session("conversation")
        .unwrap()
        .unwrap();
    assert_eq!(named_empty.summary.entries, 0);
    assert_eq!(named_empty.name.as_deref(), Some("Keep empty"));

    assert!(store.delete("conversation").unwrap());
    assert!(
        store
            .catalog_saved_session("conversation")
            .unwrap()
            .is_none()
    );

    store
        .name_session("name-only", "Temporary", 32, None)
        .unwrap();
    assert!(store.clear_session_name("name-only").unwrap());
    assert!(store.catalog_saved_session("name-only").unwrap().is_none());
    let _ = fs::remove_dir_all(root);
}

/// Verifies a presentation append repairs missing catalog name metadata from
/// the retained compatibility sidecar without replaying the presentation log.
#[test]
fn transcript_store_catalog_presentation_repair_recovers_name_sidecar() {
    let root = temp_root("catalog-presentation-name-repair");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    store.initialize(100).unwrap();
    store
        .name_session(
            "presentation",
            "Named presentation",
            31,
            Some("/repo".to_string()),
        )
        .unwrap();

    let connection = Connection::open(store.catalog_path()).unwrap();
    connection
        .execute(
            "DELETE FROM saved_conversations WHERE conversation_id = 'presentation'",
            [],
        )
        .unwrap();
    drop(connection);

    store
        .append_presentation(&presentation("presentation", 1))
        .unwrap();

    let repaired = store
        .catalog_saved_session("presentation")
        .unwrap()
        .unwrap();
    assert_eq!(repaired.name.as_deref(), Some("Named presentation"));
    assert_eq!(repaired.summary.directory.as_deref(), Some("/repo"));
    let _ = fs::remove_dir_all(root);
}

/// Verifies exact lookup repairs a missing row from one session's retained
/// files and removes a stale row whose payload disappeared.
///
/// The repair path must be UUID-local: an unrelated malformed sibling must not
/// be parsed or prevent the requested conversation from being recovered.
#[test]
fn transcript_store_catalog_exact_lookup_repairs_missing_and_stale_rows() {
    let root = temp_root("catalog-exact-repair");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    store.initialize(100).unwrap();
    store
        .append(&entry("repair-me", 1, TranscriptRole::User))
        .unwrap();
    fs::create_dir_all(root.join("unrelated")).unwrap();
    fs::write(root.join("unrelated").join("metadata.json"), b"not json\n").unwrap();

    let connection = Connection::open(store.catalog_path()).unwrap();
    connection
        .execute(
            "DELETE FROM saved_conversations WHERE conversation_id = 'repair-me'",
            [],
        )
        .unwrap();
    drop(connection);

    let repaired = store.saved_session("repair-me").unwrap().unwrap();
    assert_eq!(repaired.summary.entries, 1);
    assert!(store.catalog_saved_session("repair-me").unwrap().is_some());

    fs::remove_dir_all(root.join("repair-me")).unwrap();
    assert!(store.saved_session("repair-me").unwrap().is_none());
    assert!(store.catalog_saved_session("repair-me").unwrap().is_none());
    let _ = fs::remove_dir_all(root);
}

/// Verifies latest lookup uses activity ordering, excludes subagents, and
/// advances past a stale newest root row without scanning all session files.
#[test]
fn transcript_store_catalog_latest_root_skips_subagents_and_stale_rows() {
    let root = temp_root("catalog-latest-root");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    store.initialize(100).unwrap();

    let mut older_root = entry("older-root", 1, TranscriptRole::User);
    older_root.created_at_unix_seconds = 10;
    let mut newest_subagent = entry("newest-subagent", 1, TranscriptRole::User);
    newest_subagent.created_at_unix_seconds = 30;
    let mut stale_root = entry("stale-root", 1, TranscriptRole::User);
    stale_root.created_at_unix_seconds = 20;
    store.append(&older_root).unwrap();
    store.append(&newest_subagent).unwrap();
    store
        .save_conversation_kind(
            "newest-subagent",
            mez_agent::AgentConversationKind::Subagent,
        )
        .unwrap();
    store.append(&stale_root).unwrap();
    fs::remove_dir_all(root.join("stale-root")).unwrap();

    let latest = store.latest_root_session().unwrap().unwrap();
    assert_eq!(latest.summary.conversation_id, "older-root");
    assert!(store.catalog_saved_session("stale-root").unwrap().is_none());
    let _ = fs::remove_dir_all(root);
}

/// Verifies completion is bounded and root-only, while picker pages retain
/// deterministic named-first ordering across forward, backward, and last-page
/// keyset boundaries.
///
/// Equal activity timestamps exercise the conversation-id tie-breaker. The
/// completion assertion includes a named zero-entry root and excludes a newer
/// delegated child so discovery semantics do not depend on transcript rows.
#[test]
fn transcript_store_catalog_bounds_completion_and_keyset_pages() {
    let root = temp_root("catalog-keyset-pages");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    store.initialize(100).unwrap();

    for (conversation_id, created_at, content) in [
        ("named-a", 20, "cwd=/repo/a\nalpha needle"),
        ("named-b", 20, "cwd=/repo/a\nbeta needle"),
        ("unnamed-a", 30, "cwd=/repo/b\ngamma"),
        ("unnamed-b", 10, "cwd=/repo/b\ndelta"),
        ("root-prefix", 40, "completion root"),
        ("root-child", 50, "completion child"),
    ] {
        let mut transcript_entry = entry(conversation_id, 1, TranscriptRole::User);
        transcript_entry.created_at_unix_seconds = created_at;
        transcript_entry.content = content.to_string();
        store.append(&transcript_entry).unwrap();
    }
    store.name_session("named-a", "Named A", 20, None).unwrap();
    store.name_session("named-b", "Named B", 20, None).unwrap();
    store
        .name_session("root-zero", "Zero entry", 60, None)
        .unwrap();
    store
        .save_conversation_kind("root-child", mez_agent::AgentConversationKind::Subagent)
        .unwrap();

    let completion_ids = store
        .root_session_completions("root-", 2)
        .unwrap()
        .into_iter()
        .map(|session| session.summary.conversation_id)
        .collect::<Vec<_>>();
    assert_eq!(completion_ids, vec!["root-zero", "root-prefix"]);

    let query = SavedSessionQuery {
        lifecycle: SavedSessionLifecycleFilter::Active,
        directory: None,
        include_subagents: false,
        require_latest_user_prompt: true,
        search: None,
        anchor: None,
        limit: 2,
    };
    let first = store.query_saved_sessions(&query).unwrap().sessions;
    assert_eq!(
        first
            .iter()
            .map(|session| session.summary.conversation_id.as_str())
            .collect::<Vec<_>>(),
        vec!["named-a", "named-b"]
    );

    let second = store
        .query_saved_sessions(&SavedSessionQuery {
            anchor: Some(SavedSessionPageAnchor::After(
                SavedSessionCursor::from_session(first.last().unwrap()),
            )),
            ..query.clone()
        })
        .unwrap()
        .sessions;
    assert_eq!(
        second
            .iter()
            .map(|session| session.summary.conversation_id.as_str())
            .collect::<Vec<_>>(),
        vec!["root-prefix", "unnamed-a"]
    );

    let previous = store
        .query_saved_sessions(&SavedSessionQuery {
            anchor: Some(SavedSessionPageAnchor::Before(
                SavedSessionCursor::from_session(second.first().unwrap()),
            )),
            ..query.clone()
        })
        .unwrap()
        .sessions;
    assert_eq!(previous, first);

    let last = store
        .query_saved_sessions(&SavedSessionQuery {
            anchor: Some(SavedSessionPageAnchor::Last),
            ..query.clone()
        })
        .unwrap()
        .sessions;
    assert_eq!(
        last.iter()
            .map(|session| session.summary.conversation_id.as_str())
            .collect::<Vec<_>>(),
        vec!["unnamed-a", "unnamed-b"]
    );

    let filtered = store
        .query_saved_sessions(&SavedSessionQuery {
            directory: Some("/repo/a".to_string()),
            search: Some("NEEDLE".to_string()),
            anchor: None,
            limit: 10,
            ..query
        })
        .unwrap()
        .sessions;
    assert_eq!(
        filtered
            .iter()
            .map(|session| session.summary.conversation_id.as_str())
            .collect::<Vec<_>>(),
        vec!["named-a", "named-b"]
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies active discovery paths exclude archived rows while exact lookup
/// and an explicit archived pager query retain lifecycle-aware addressability.
#[test]
fn transcript_store_catalog_filters_active_and_archived_lifecycles() {
    let root = temp_root("catalog-lifecycle-filter");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    let mut active_entry = entry("active-session", 1, TranscriptRole::User);
    active_entry.created_at_unix_seconds = 20;
    let mut archived_entry = entry("archived-session", 1, TranscriptRole::User);
    archived_entry.created_at_unix_seconds = 30;
    store.append(&active_entry).unwrap();
    store.append(&archived_entry).unwrap();
    let archived_info = store.archive_session("archived-session", 40).unwrap();

    assert_eq!(
        store
            .latest_root_session()
            .unwrap()
            .unwrap()
            .summary
            .conversation_id,
        "active-session"
    );
    assert!(
        store
            .root_session_completions("archived-", 10)
            .unwrap()
            .is_empty()
    );
    let archived = store.saved_session("archived-session").unwrap().unwrap();
    assert_eq!(archived.archived_at_unix_seconds, Some(40));
    assert_eq!(
        archived.archive_compressed_bytes,
        Some(archived_info.compressed_bytes)
    );

    let query = SavedSessionQuery {
        lifecycle: SavedSessionLifecycleFilter::Archived,
        directory: None,
        include_subagents: true,
        require_latest_user_prompt: true,
        search: None,
        anchor: None,
        limit: 10,
    };
    let archived_rows = store.query_saved_sessions(&query).unwrap().sessions;
    assert_eq!(archived_rows, vec![archived]);
    let active_rows = store
        .query_saved_sessions(&SavedSessionQuery {
            lifecycle: SavedSessionLifecycleFilter::Active,
            ..query
        })
        .unwrap()
        .sessions;
    assert_eq!(
        active_rows
            .iter()
            .map(|session| session.summary.conversation_id.as_str())
            .collect::<Vec<_>>(),
        vec!["active-session"]
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies archive and restore preserve transcript, presentation, naming,
/// classification, catalog lifecycle metadata, and standard tar+zstd layout.
#[test]
fn transcript_store_archives_and_restores_session_round_trip() {
    let root = temp_root("archive-round-trip");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append(&entry("archive-round-trip", 1, TranscriptRole::User))
        .unwrap();
    store
        .append(&entry("archive-round-trip", 2, TranscriptRole::Assistant))
        .unwrap();
    store
        .append_presentation(&presentation("archive-round-trip", 1))
        .unwrap();
    store
        .save_conversation_kind(
            "archive-round-trip",
            mez_agent::AgentConversationKind::Subagent,
        )
        .unwrap();
    store
        .name_session(
            "archive-round-trip",
            "Archived work",
            30,
            Some("/repo".into()),
        )
        .unwrap();

    let archived = store.archive_session("archive-round-trip", 100).unwrap();

    assert_eq!(archived.archived_at_unix_seconds, 100);
    assert_eq!(archived.summary.entries, 2);
    assert_eq!(archived.name.as_deref(), Some("Archived work"));
    assert_eq!(
        archived.conversation_kind,
        mez_agent::AgentConversationKind::Subagent
    );
    assert!(!root.join("archive-round-trip").exists());
    let archive_path = root.join("archived/archive-round-trip.tar.zst");
    assert!(archive_path.is_file());
    assert!(root.join("archived/archive-round-trip.json").is_file());

    let decoder = zstd::stream::read::Decoder::new(fs::File::open(&archive_path).unwrap()).unwrap();
    let mut archive = tar::Archive::new(decoder);
    let paths = archive
        .entries()
        .unwrap()
        .map(|entry| entry.unwrap().path().unwrap().into_owned())
        .collect::<Vec<_>>();
    assert!(paths.contains(&PathBuf::from("archive-round-trip/history.tsv")));
    assert!(paths.contains(&PathBuf::from("archive-round-trip/archive-manifest.json")));

    let archived_record = store.saved_session("archive-round-trip").unwrap().unwrap();
    assert_eq!(archived_record.archived_at_unix_seconds, Some(100));
    assert_eq!(
        archived_record.archive_compressed_bytes,
        Some(archived.compressed_bytes)
    );
    assert_eq!(
        archived_record.archive_sha256.as_deref(),
        Some(archived.sha256.as_str())
    );

    let restored = store
        .restore_archived_session("archive-round-trip")
        .unwrap();

    assert_eq!(restored, archived);
    assert!(!archive_path.exists());
    assert!(!root.join("archived/archive-round-trip.json").exists());
    assert_eq!(store.inspect("archive-round-trip").unwrap().len(), 2);
    assert_eq!(
        store
            .inspect_presentation("archive-round-trip")
            .unwrap()
            .len(),
        1
    );
    let active_record = store.saved_session("archive-round-trip").unwrap().unwrap();
    assert_eq!(active_record.archived_at_unix_seconds, None);
    assert_eq!(active_record.name.as_deref(), Some("Archived work"));
    let _ = fs::remove_dir_all(root);
}

/// Verifies restore rejects a tampered compressed payload without removing the
/// archive sidecar or installing a partial active conversation.
#[test]
fn transcript_store_restore_rejects_archive_digest_mismatch() {
    let root = temp_root("archive-digest-mismatch");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append(&entry("archive-digest", 1, TranscriptRole::User))
        .unwrap();
    store.archive_session("archive-digest", 100).unwrap();
    let archive_path = root.join("archived/archive-digest.tar.zst");
    let mut bytes = fs::read(&archive_path).unwrap();
    bytes[0] ^= 0xff;
    fs::write(&archive_path, bytes).unwrap();

    let error = store
        .restore_archived_session("archive-digest")
        .unwrap_err();

    assert!(error.message().contains("digest verification failed"));
    assert!(archive_path.is_file());
    assert!(root.join("archived/archive-digest.json").is_file());
    assert!(!root.join("archive-digest").exists());
    let _ = fs::remove_dir_all(root);
}

/// Verifies explicit catalog reconstruction imports archive sidecars without
/// decompressing the archived payload or making it active by default.
#[test]
fn transcript_store_catalog_rebuild_imports_archived_sidecars() {
    let root = temp_root("archive-catalog-rebuild");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append(&entry("archive-rebuild", 1, TranscriptRole::User))
        .unwrap();
    let archived = store.archive_session("archive-rebuild", 100).unwrap();
    fs::remove_file(store.catalog_path()).unwrap();

    store.initialize(101).unwrap();

    let rebuilt = store.saved_session("archive-rebuild").unwrap().unwrap();
    assert_eq!(rebuilt.archived_at_unix_seconds, Some(100));
    assert_eq!(
        rebuilt.archive_sha256.as_deref(),
        Some(archived.sha256.as_str())
    );
    assert!(store.latest_root_session().unwrap().is_none());
    let _ = fs::remove_dir_all(root);
}

/// Verifies mutable naming metadata remains consistent across the compatibility
/// index, archive sidecar, catalog row, and a later verified restore.
#[test]
fn transcript_store_updates_archived_session_names() {
    let root = temp_root("archive-name-update");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append(&entry("archive-name", 1, TranscriptRole::User))
        .unwrap();
    store.archive_session("archive-name", 100).unwrap();

    store
        .name_session("archive-name", "Retained archive", 110, None)
        .unwrap();
    assert_eq!(
        store
            .saved_session("archive-name")
            .unwrap()
            .unwrap()
            .name
            .as_deref(),
        Some("Retained archive")
    );
    assert!(store.clear_session_name("archive-name").unwrap());
    assert_eq!(
        store.saved_session("archive-name").unwrap().unwrap().name,
        None
    );

    store.restore_archived_session("archive-name").unwrap();
    assert_eq!(
        store.saved_session("archive-name").unwrap().unwrap().name,
        None
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies startup repairs only the conversation named by an interrupted
/// archive journal and restores its staged active payload when no archive was installed.
#[test]
fn transcript_store_recovers_interrupted_archive_from_journal() {
    let root = temp_root("archive-journal-recovery");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append(&entry("journal-recovery", 1, TranscriptRole::User))
        .unwrap();
    fs::rename(
        root.join("journal-recovery"),
        root.join(".archive-stage-journal-recovery"),
    )
    .unwrap();
    fs::create_dir_all(root.join(".archive-recovery")).unwrap();
    fs::write(
        root.join(".archive-recovery/journal-recovery.json"),
        br#"{"version":1,"conversation_id":"journal-recovery","operation":"archive","payload_layout":"directory"}"#,
    )
    .unwrap();

    store.initialize(101).unwrap();

    assert!(root.join("journal-recovery/history.tsv").is_file());
    assert!(!root.join(".archive-stage-journal-recovery").exists());
    assert!(
        !root
            .join(".archive-recovery/journal-recovery.json")
            .exists()
    );
    assert_eq!(store.inspect("journal-recovery").unwrap().len(), 1);
    assert_eq!(
        store
            .saved_session("journal-recovery")
            .unwrap()
            .unwrap()
            .archived_at_unix_seconds,
        None
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies an active payload wins catalog reconstruction when a retained
/// archive with the same UUID remains available for diagnosis.
#[test]
fn transcript_store_catalog_rebuild_prefers_active_over_archived_duplicate() {
    let root = temp_root("archive-active-precedence");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append(&entry("duplicate-session", 1, TranscriptRole::User))
        .unwrap();
    store.archive_session("duplicate-session", 100).unwrap();
    fs::create_dir_all(root.join("duplicate-session")).unwrap();
    fs::write(
        root.join("duplicate-session/history.tsv"),
        format!(
            "{}\n",
            encode_transcript_entry(&entry("duplicate-session", 1, TranscriptRole::User)).unwrap()
        ),
    )
    .unwrap();
    fs::remove_file(store.catalog_path()).unwrap();

    store.initialize(101).unwrap();

    let rebuilt = store.saved_session("duplicate-session").unwrap().unwrap();
    assert_eq!(rebuilt.archived_at_unix_seconds, None);
    assert!(root.join("archived/duplicate-session.tar.zst").is_file());
    assert!(root.join("archived/duplicate-session.json").is_file());
    let _ = fs::remove_dir_all(root);
}

/// Verifies archive extraction path validation rejects absolute paths,
/// traversal, and unexpected top-level roots before touching the filesystem.
#[test]
fn transcript_store_archive_rejects_unsafe_entry_paths() {
    for path in [
        PathBuf::from("/archive-session/history.tsv"),
        PathBuf::from("archive-session/../escape"),
        PathBuf::from("other-session/history.tsv"),
    ] {
        assert!(
            super::archive::validate_archive_path(&path, "archive-session").is_err(),
            "unsafe archive path was accepted: {path:?}"
        );
    }
    assert!(
        super::archive::validate_archive_path(
            &PathBuf::from("archive-session/history.tsv"),
            "archive-session",
        )
        .is_ok()
    );
}

/// Exercises bounded catalog discovery against a realistically large metadata
/// set and verifies representative normal queries retain indexed plans.
///
/// This is ignored in ordinary CI because constructing 100,000 rows is scale
/// validation rather than a functional regression. It deliberately asserts
/// bounded result counts and query plans instead of wall-clock timing.
#[test]
#[ignore = "large saved-session catalog scale and query-plan check"]
fn transcript_store_catalog_scales_bounded_queries_to_one_hundred_thousand_rows() {
    let root = temp_root("catalog-scale");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    store.initialize(100).unwrap();

    let mut connection = Connection::open(store.catalog_path()).unwrap();
    let transaction = connection.transaction().unwrap();
    {
        let mut insert = transaction
            .prepare(
                "INSERT INTO saved_conversations (
                     conversation_id, conversation_kind, name, named_at,
                     entry_count, first_created_at, last_created_at,
                     last_turn_id, agent_id, pane_id, directory,
                     initial_prompt, latest_user_prompt, has_transcript,
                     has_presentation, payload_layout, catalog_updated_at
                 ) VALUES (?1, 'root', ?2, ?3, 1, ?4, ?4, 'turn',
                           'agent', '%1', ?5, ?6, ?6, 1, 0, 'directory', ?4)",
            )
            .unwrap();
        for index in 0..100_000u64 {
            let conversation_id = format!("00000000-0000-0000-{:04x}-{index:012x}", index % 65_536);
            let name = (index % 10 == 0).then(|| format!("Named {index}"));
            let named_at = name.as_ref().map(|_| index as i64 + 1);
            let directory = if index % 2 == 0 { "/repo/a" } else { "/repo/b" };
            let prompt = format!("saved prompt {index}");
            insert
                .execute(rusqlite::params![
                    conversation_id,
                    name,
                    named_at,
                    index as i64 + 1,
                    directory,
                    prompt,
                ])
                .unwrap();
        }
    }
    transaction.commit().unwrap();

    assert_eq!(
        store
            .root_session_completions("00000000", 200)
            .unwrap()
            .len(),
        200
    );
    assert_eq!(
        store
            .query_saved_sessions(&SavedSessionQuery {
                lifecycle: SavedSessionLifecycleFilter::Active,
                directory: Some("/repo/a".to_string()),
                include_subagents: false,
                require_latest_user_prompt: true,
                search: None,
                anchor: None,
                limit: 40,
            })
            .unwrap()
            .sessions
            .len(),
        40
    );

    let picker_plan = connection
        .prepare(
            "EXPLAIN QUERY PLAN
             SELECT conversation_id FROM saved_conversations
             WHERE archived_at IS NULL
               AND directory = '/repo/a' AND conversation_kind = 'root'
               AND latest_user_prompt IS NOT NULL
             ORDER BY (name IS NOT NULL) DESC, last_created_at DESC,
                      first_created_at DESC, conversation_id ASC
             LIMIT 40",
        )
        .unwrap()
        .query_map([], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
        .join("\n");
    assert!(
        picker_plan.contains("saved_conversations_directory_picker"),
        "{picker_plan}"
    );

    let completion_plan = connection
        .prepare(
            "EXPLAIN QUERY PLAN
             SELECT conversation_id FROM saved_conversations
             WHERE archived_at IS NULL AND conversation_kind = 'root'
             ORDER BY last_created_at DESC, first_created_at DESC,
                      conversation_id ASC
             LIMIT 200",
        )
        .unwrap()
        .query_map([], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
        .join("\n");
    assert!(
        completion_plan.contains("saved_conversations_latest_root"),
        "{completion_plan}"
    );

    let exact_plan = connection
        .prepare(
            "EXPLAIN QUERY PLAN
             SELECT conversation_kind, name FROM saved_conversations
             WHERE conversation_id = '00000000-0000-0000-0000-000000000001'",
        )
        .unwrap()
        .query_map([], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
        .join("\n");
    assert!(
        exact_plan.contains("sqlite_autoindex_saved_conversations_1"),
        "{exact_plan}"
    );

    let pruning_plan = connection
        .prepare(
            "EXPLAIN QUERY PLAN
             SELECT conversation_id FROM saved_conversations
             WHERE archived_at IS NULL
               AND (has_transcript = 1 OR has_presentation = 1)
             ORDER BY last_created_at ASC, first_created_at ASC,
                      conversation_id ASC
             LIMIT 40",
        )
        .unwrap()
        .query_map([], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
        .join("\n");
    assert!(
        pruning_plan.contains("saved_conversations_pruning"),
        "{pruning_plan}"
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies bounded status reports health and process-local indexed/repair metrics.
#[test]
fn transcript_store_catalog_status_reports_health_and_bounded_metrics() {
    let root = temp_root("catalog-status");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());

    let missing = store.catalog_status();
    assert!(!missing.database_exists);
    assert!(!missing.integrity_ok);
    assert!(missing.diagnostic.unwrap().contains("rebuild"));

    store.initialize(100).unwrap();
    store
        .append(&entry("status-session", 1, TranscriptRole::User))
        .unwrap();
    let before = store.catalog_status();
    assert!(before.database_exists);
    assert!(before.migration_complete);
    assert!(before.integrity_ok);
    assert_eq!(before.schema_version, Some(2));
    assert_eq!(before.indexed_conversations, Some(1));
    assert!(before.lock_available);

    let connection = Connection::open(store.catalog_path()).unwrap();
    connection
        .execute(
            "DELETE FROM saved_conversations WHERE conversation_id = 'status-session'",
            [],
        )
        .unwrap();
    drop(connection);
    assert!(store.saved_session("status-session").unwrap().is_some());

    let after = store.catalog_status();
    assert!(after.indexed_queries > before.indexed_queries);
    assert!(after.exact_repairs > before.exact_repairs);
    assert!(after.full_scans >= before.full_scans);
    let _ = fs::remove_dir_all(root);
}

/// Verifies rebuild rejects future schemas and removes stale temporary files.
#[test]
fn transcript_store_catalog_rebuild_rejects_future_schema_and_cleans_temporary_files() {
    let root = temp_root("catalog-rebuild-guards");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    store.initialize(100).unwrap();
    fs::write(root.join(".catalog.sqlite3.rebuild"), b"stale").unwrap();
    fs::write(root.join(".catalog.sqlite3.rebuild-wal"), b"stale").unwrap();

    store.rebuild_catalog(101).unwrap();
    assert!(!root.join(".catalog.sqlite3.rebuild").exists());
    assert!(!root.join(".catalog.sqlite3.rebuild-wal").exists());

    let connection = Connection::open(store.catalog_path()).unwrap();
    connection.pragma_update(None, "user_version", 3).unwrap();
    drop(connection);
    let error = store.rebuild_catalog(102).unwrap_err();
    assert!(error.message().contains("refusing to rebuild or downgrade"));
    let connection = Connection::open(store.catalog_path()).unwrap();
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 3);
    let _ = fs::remove_dir_all(root);
}

/// Verifies a failed rebuild removes its temporary SQLite family and leaves
/// the previously healthy catalog available for indexed discovery.
#[test]
fn transcript_store_catalog_failed_rebuild_cleans_temporary_files() {
    let root = temp_root("catalog-failed-rebuild-cleanup");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    store.initialize(100).unwrap();
    store
        .append(&entry("healthy", 1, TranscriptRole::User))
        .unwrap();
    fs::write(
        root.join("healthy").join("summary.json"),
        b"not valid summary json\n",
    )
    .unwrap();

    let error = store.rebuild_catalog(101).unwrap_err();
    assert!(
        error
            .message()
            .contains("conversation summary decode failed")
    );
    assert!(!root.join(".catalog.sqlite3.rebuild").exists());
    assert!(!root.join(".catalog.sqlite3.rebuild-wal").exists());
    assert!(!root.join(".catalog.sqlite3.rebuild-shm").exists());
    assert!(store.catalog_status().integrity_ok);
    assert!(store.catalog_saved_session("healthy").unwrap().is_some());
    let _ = fs::remove_dir_all(root);
}

/// Verifies status detects lock contention without waiting or scanning payloads.
#[test]
fn transcript_store_catalog_status_reports_lock_contention() {
    let root = temp_root("catalog-lock-status");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    store.initialize(100).unwrap();
    let lock_path = root.join(".catalog-migration.lock");
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path)
        .unwrap();
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).unwrap();

    let status = store.catalog_status();
    assert!(!status.lock_available);
    assert!(status.integrity_ok);
    let _ = fs::remove_dir_all(root);
}

/// Verifies an explicit rebuild reports bounded lock contention instead of
/// waiting indefinitely or entering the recovery scanner concurrently.
#[test]
fn transcript_store_catalog_rebuild_reports_bounded_lock_contention() {
    let root = temp_root("catalog-lock-rebuild");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    store.initialize(100).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join(".catalog-migration.lock"))
        .unwrap();
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).unwrap();

    let error = store.rebuild_catalog(101).unwrap_err();

    assert!(error.message().contains("migration lock is busy"));
    assert!(!root.join(".catalog.sqlite3.rebuild").exists());
    assert!(store.catalog_status().integrity_ok);
    let _ = fs::remove_dir_all(root);
}
