//! Tests for transcript persistence, forking, and TSV escaping.

use std::{collections::BTreeMap, fs};

use super::{
    AgentPresentationEntry, AgentSessionMetadata, AgentTranscriptStore, TranscriptEntry,
    TranscriptRole,
};

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
    }
}

/// Builds one presentation entry large enough to force cleartext tail compaction.
fn large_presentation(conversation_id: &str, sequence: u64) -> AgentPresentationEntry {
    let mut entry = presentation(conversation_id, sequence);
    entry.display_lines = vec![format!("mez> {}", "x".repeat(300 * 1024))];
    entry.style_names = vec!["assistant".to_string()];
    entry.copy_lines = entry.display_lines.clone();
    entry.ansi_text = None;
    entry
}

/// Verifies that the store can append, list, inspect, and delete one
/// conversation using the durable TSV representation.
#[test]
fn transcript_store_appends_lists_inspects_and_deletes_conversations() {
    let root = std::env::temp_dir().join(format!("mez-transcript-{}", std::process::id()));
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

/// Verifies oversized presentation tails are moved into concatenated zstd
/// frames while later cleartext appends remain replayable after them.
#[test]
fn transcript_store_compacts_presentation_tail_into_zstd_history() {
    let root = std::env::temp_dir().join(format!(
        "mez-transcript-presentation-zstd-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    let first = large_presentation("conv1", 1);
    let second = large_presentation("conv1", 2);
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
    let root = std::env::temp_dir().join(format!(
        "mez-transcript-presentation-wrap-{}",
        std::process::id()
    ));
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

/// Verifies presentation row normalization caps wide terminal widths at 120
/// columns, matching the agent-mode log rendering contract.
#[test]
fn transcript_store_caps_presentation_rows_at_120_columns() {
    let root = std::env::temp_dir().join(format!(
        "mez-transcript-presentation-cap-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    let mut entry = presentation("conv1", 1);
    entry.terminal_width = 200;
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
    let root =
        std::env::temp_dir().join(format!("mez-transcript-append-many-{}", std::process::id()));
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

/// Verifies that durable presentation entries are persisted separately from
/// model-facing transcript entries while retaining multiline copy text.
#[test]
fn transcript_store_appends_and_inspects_presentation_entries() {
    let root = std::env::temp_dir().join(format!(
        "mez-transcript-presentation-{}",
        std::process::id()
    ));
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

/// Verifies recent transcript inspection reads only the requested tail entries
/// and reports the next append sequence from that bounded tail.
///
/// Agent prompt assembly only needs recent transcript context. This regression
/// keeps that path independent from full-file reads so an unexpectedly large
/// transcript cannot be copied into memory just to find the latest entries.
#[test]
fn transcript_store_inspects_recent_entries_and_next_sequence_from_tail() {
    let root =
        std::env::temp_dir().join(format!("mez-transcript-recent-tail-{}", std::process::id()));
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

/// Verifies that async transcript append and shared prompt-history writes use
/// the same durable layout and decoding behavior as the synchronous store API.
#[tokio::test]
async fn transcript_store_async_appends_entries_and_prompt_history() {
    let root = std::env::temp_dir().join(format!("mez-transcript-async-{}", std::process::id()));
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

    let inspected = store.inspect("conv1").unwrap();
    let history = store.prompt_history_async("conv1").await.unwrap();
    let command_history = store.command_prompt_history_async().await.unwrap();

    assert!(bytes > 0);
    assert_eq!(inspected, entries);
    assert_eq!(history, vec![String::from("inspect project")]);
    assert_eq!(command_history, vec![String::from("list-buffers")]);
    assert!(root.join("conv1").join("history.tsv").exists());
    assert!(root.join("prompt-history.tsv").exists());
    assert!(root.join("command-prompt-history.tsv").exists());
    let _ = fs::remove_dir_all(root);
}

/// Verifies that forking creates a new conversation identity with copied
/// entries and a replacement creation time.
#[test]
fn transcript_store_forks_conversation_to_fresh_identity() {
    let root = std::env::temp_dir().join(format!("mez-transcript-fork-{}", std::process::id()));
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
    let config_root =
        std::env::temp_dir().join(format!("mez-transcript-config-root-{}", std::process::id()));
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
/// file, survive reload through any session identity, and are not duplicated
/// into forked conversation directories.
#[test]
fn transcript_store_persists_prompt_history_in_shared_file() {
    let root = std::env::temp_dir().join(format!(
        "mez-transcript-prompt-history-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append(&entry("conv1", 1, TranscriptRole::User))
        .unwrap();
    assert!(store.append_prompt_history("conv1", "list files").unwrap());
    assert!(
        store
            .append_prompt_history("conv1", "build project")
            .unwrap()
    );
    assert!(store.append_prompt_history("conv2", "run tests").unwrap());

    let history = store.prompt_history("conv1").unwrap();

    assert_eq!(
        history,
        vec![
            String::from("list files"),
            String::from("build project"),
            String::from("run tests")
        ]
    );
    assert_eq!(store.prompt_history("conv2").unwrap(), history);
    assert!(root.join("prompt-history.tsv").exists());
    assert!(!root.join("conv1").join("prompt-history.tsv").exists());
    assert!(!root.join("conv2").join("prompt-history.tsv").exists());
    assert_eq!(store.list().unwrap().len(), 1);

    let fork = store.fork("conv1", "conv2", 99).unwrap();
    assert_eq!(fork.conversation_id, "conv2");
    assert_eq!(store.prompt_history("conv2").unwrap(), history);
    assert!(!root.join("conv2").join("prompt-history.tsv").exists());

    let _ = fs::remove_dir_all(root);
}

/// Verifies that primary command prompt history is stored separately from the
/// agent prompt history while using the same shared, bounded reload behavior.
#[test]
fn transcript_store_persists_command_prompt_history_in_shared_file() {
    let root = std::env::temp_dir().join(format!(
        "mez-transcript-command-history-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    assert!(
        store
            .append_prompt_history("conv1", "agent prompt")
            .unwrap()
    );
    assert!(store.append_command_prompt_history("help").unwrap());
    assert!(store.append_command_prompt_history("list-buffers").unwrap());

    assert_eq!(
        store.command_prompt_history().unwrap(),
        vec![String::from("help"), String::from("list-buffers")]
    );
    assert_eq!(
        store.prompt_history("conv1").unwrap(),
        vec![String::from("agent prompt")]
    );
    assert!(root.join("command-prompt-history.tsv").exists());
    assert!(root.join("prompt-history.tsv").exists());

    let _ = fs::remove_dir_all(root);
}

/// Verifies that active agent-session metadata is replaced per Mezzanine
/// session while preserving rows for unrelated sessions.
#[test]
fn transcript_store_replaces_agent_session_metadata_per_mezzanine_session() {
    let root = std::env::temp_dir().join(format!(
        "mez-transcript-agent-session-metadata-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    let owned_token_usage_key = crate::agent::ModelTokenUsageKey::new("openai", "gpt-fast");
    let owned_token_usage = crate::agent::ModelTokenUsage {
        input_tokens: 100,
        output_tokens: 20,
        reasoning_tokens: 5,
        cached_input_tokens: Some(80),
    };
    let owned = AgentSessionMetadata {
        mezzanine_session_id: "$live".to_string(),
        pane_id: "%1".to_string(),
        conversation_id: "conv1".to_string(),
        prompt_cache_lineage_id: "lineage-live".to_string(),
        visibility: "visible".to_string(),
        running_turn_id: Some("turn-1".to_string()),
        transcript_entries: 2,
        log_level: "trace".to_string(),
        pane_model_profile: Some("work".to_string()),
        planning_enabled: true,
        response_style: Some("concise".to_string()),
        routing_enabled: Some(true),
        approval_policy: Some("full-access".to_string()),
        working_directory: Some("/workspace/live".to_string()),
        project_root: Some("/workspace".to_string()),
        context_usage: Some("10%".to_string()),
        context_usage_snapshot: Some(crate::agent::AgentContextUsageSnapshot {
            input_tokens: 100,
            context_window_tokens: 1000,
            cached_input_tokens: Some(80),
        }),
        token_usage: owned_token_usage,
        token_usage_by_model: BTreeMap::from([(owned_token_usage_key, owned_token_usage)]),
    };
    let foreign = AgentSessionMetadata {
        mezzanine_session_id: "$other".to_string(),
        pane_id: "%1".to_string(),
        conversation_id: "foreign".to_string(),
        prompt_cache_lineage_id: "lineage-other".to_string(),
        visibility: "hidden".to_string(),
        running_turn_id: None,
        transcript_entries: 1,
        log_level: "normal".to_string(),
        pane_model_profile: None,
        planning_enabled: false,
        response_style: None,
        routing_enabled: None,
        approval_policy: None,
        working_directory: None,
        project_root: None,
        context_usage: None,
        context_usage_snapshot: None,
        token_usage: Default::default(),
        token_usage_by_model: Default::default(),
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

/// Verifies that transcript TSV escaping preserves newlines and tabs across an
/// encode/decode round trip.
#[test]
fn transcript_entry_round_trips_escaped_content() {
    let original = TranscriptEntry {
        content: "line one\nline\ttwo".to_string(),
        ..entry("conv1", 1, TranscriptRole::Tool)
    };

    let decoded = TranscriptEntry::decode(&original.encode().unwrap()).unwrap();

    assert_eq!(decoded, original);
}
