//! Stable metadata follows the original checked archive scope and frontier.
//!
//! Production history workers must retain excluded older carriers without
//! borrowing later source metadata from beyond an ephemeral view's bound.

use super::super::super::context::{
    RuntimeAgentHistoryEpochInputs, RuntimeAgentHistoryEpochWork,
    execute_runtime_agent_history_epoch_work,
};
use super::*;

/// Owns a real archive with an original snapshot carrier and two peer rows.
fn fixture() -> (Directory, AgentTranscriptStore, AgentContext) {
    let directory = Directory::new();
    let store = AgentTranscriptStore::new(directory.0.join("transcripts"));
    let original = persist(&store);
    (directory, store, original)
}

/// The original snapshot lies outside a latest-row window. Current guidance
/// must reuse its record without restoring old chronological blocks to the tail.
#[test]
fn stable_record_worker_retains_carrier_outside_latest_raw_window() {
    let (_directory, store, original) = fixture();
    store
        .append(&TranscriptEntry {
            conversation_id: "record-round-trip".into(),
            sequence: 4,
            created_at_unix_seconds: 2,
            role: TranscriptRole::Assistant,
            turn_id: "tail".into(),
            agent_id: "agent".into(),
            pane_id: "pane".into(),
            content: "only visible tail work".into(),
        })
        .unwrap();
    let history = execute_runtime_agent_history_epoch_work(RuntimeAgentHistoryEpochWork {
        pending_capture_error: None,
        store,
        inputs: RuntimeAgentHistoryEpochInputs {
            pane_id: "pane".into(),
            conversation_id: "record-round-trip".into(),
            ephemeral_source_entries: None,
            active_entries: Some(1),
            pending_entries: Default::default(),
        },
    })
    .unwrap();
    assert_eq!(history.blocks.len(), 1);
    assert!(history.blocks[0].content.contains("only visible tail work"));
    let mut blocks = vec![guidance("actual guidance")];
    blocks.extend(history.blocks);
    let restored = history.records.import_context(blocks).unwrap();
    assert_eq!(restored.stable_slots(), original.stable_slots());
}

/// Future metadata has different owned IDs despite equal guidance. The actual
/// ephemeral worker must cap metadata selection, as well as semantic blocks.
#[test]
fn stable_record_worker_refuses_future_ephemeral_carrier_metadata() {
    let (_directory, store, original) = fixture();
    let mut future =
        AgentContext::import_durable_blocks(vec![guidance("actual guidance")]).unwrap();
    future
        .append_user_event("prompt", "future private source")
        .unwrap();
    let event = future.chronology().last().unwrap();
    let content = mez_agent::transcript::TranscriptContextRecord::new(
        TranscriptContextEvent::user_event(
            event.sequence().get(),
            "prompt",
            "future private source",
        )
        .unwrap(),
        event.record(),
        Some(future.stable_block_records()),
    )
    .unwrap()
    .to_transcript_content()
    .unwrap();
    store
        .append(&TranscriptEntry {
            conversation_id: "record-round-trip".into(),
            sequence: 4,
            created_at_unix_seconds: 2,
            role: TranscriptRole::System,
            turn_id: "future".into(),
            agent_id: "agent".into(),
            pane_id: "pane".into(),
            content,
        })
        .unwrap();
    let history = execute_runtime_agent_history_epoch_work(RuntimeAgentHistoryEpochWork {
        pending_capture_error: None,
        store,
        inputs: RuntimeAgentHistoryEpochInputs {
            pane_id: "pane".into(),
            conversation_id: "record-round-trip".into(),
            ephemeral_source_entries: Some(3),
            active_entries: Some(1),
            pending_entries: Default::default(),
        },
    })
    .unwrap();
    assert!(
        history
            .blocks
            .iter()
            .all(|block| !block.content.contains("future private source"))
    );
    assert_eq!(history.blocks.len(), 3);
    assert!(
        history
            .source
            .as_ref()
            .unwrap()
            .selected_images(&std::collections::BTreeSet::from([event
                .record()
                .occurrence_id(),]))
            .is_err(),
        "the frozen source retained a future row outside its original frontier"
    );
    let mut blocks = vec![guidance("actual guidance")];
    blocks.extend(history.blocks);
    let restored = history.records.import_context(blocks).unwrap();
    assert_eq!(restored.stable_slots(), original.stable_slots());
    assert_ne!(restored.stable_slots(), future.stable_slots());
}

/// An unsupported later accounting frame must not deny an older bounded
/// source. The same archive still refuses an unbounded attempt to adopt it.
#[test]
fn stable_record_worker_isolates_malformed_future_carrier() {
    let (_directory, store, original) = fixture();
    store
        .append(&TranscriptEntry {
            conversation_id: "record-round-trip".into(),
            sequence: 4,
            created_at_unix_seconds: 2,
            role: TranscriptRole::System,
            turn_id: "future".into(),
            agent_id: "agent".into(),
            pane_id: "pane".into(),
            content: "[mez-transcript-context-record/v99]\nprivate future payload".into(),
        })
        .unwrap();
    let work = |frontier| RuntimeAgentHistoryEpochWork {
        pending_capture_error: None,
        store: store.clone(),
        inputs: RuntimeAgentHistoryEpochInputs {
            pane_id: "pane".into(),
            conversation_id: "record-round-trip".into(),
            ephemeral_source_entries: frontier,
            active_entries: Some(1),
            pending_entries: Default::default(),
        },
    };
    let history = execute_runtime_agent_history_epoch_work(work(Some(3))).unwrap();
    assert_eq!(history.blocks.len(), 3);
    let mut blocks = vec![guidance("actual guidance")];
    blocks.extend(history.blocks);
    let restored = history.records.import_context(blocks).unwrap();
    assert_eq!(restored.stable_slots(), original.stable_slots());
    let error = execute_runtime_agent_history_epoch_work(work(None)).unwrap_err();
    assert!(!error.message().contains("private future payload"));
}

/// A fork captured before its parent has saved any row has an empty source,
/// even when the archive later acquires unsupported metadata. Zero is not a
/// demand for a physical high-water row and cannot adopt that later payload.
#[test]
fn stable_record_worker_keeps_zero_ephemeral_source_empty() {
    let directory = Directory::new();
    let store = AgentTranscriptStore::new(directory.0.join("transcripts"));
    for append_future in [false, true] {
        if append_future {
            store
                .append(&TranscriptEntry {
                    conversation_id: "empty-parent".into(),
                    sequence: 1,
                    created_at_unix_seconds: 2,
                    role: TranscriptRole::System,
                    turn_id: "future".into(),
                    agent_id: "agent".into(),
                    pane_id: "pane".into(),
                    content: "[mez-transcript-context-record/v99]\nprivate future payload".into(),
                })
                .unwrap();
        }
        let history = execute_runtime_agent_history_epoch_work(RuntimeAgentHistoryEpochWork {
            pending_capture_error: None,
            store: store.clone(),
            inputs: RuntimeAgentHistoryEpochInputs {
                pane_id: "pane".into(),
                conversation_id: "empty-parent".into(),
                ephemeral_source_entries: Some(0),
                active_entries: Some(1),
                pending_entries: Default::default(),
            },
        })
        .unwrap();
        assert!(history.blocks.is_empty());
        history.records.ensure_valid().unwrap();
    }
}
