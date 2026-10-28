//! Real resumed archives shared by fork-source and child-inheritance tests.
//!
//! These helpers own only source setup used by both test owners: typed complete
//! groups, a durable prefix and actual deferred `/resume` adoption. They perform
//! no child allocation, mock acknowledgments or provider execution.

use super::*;

pub(super) const CONVERSATION: &str = "018f6b3a-1b2c-7000-9000-cafebabe0011";

/// Builds independently resumable rows, with complete typed groups at 2..3 and
/// 4..5 so tests can publish valid selective ranges after source admission.
pub(super) fn row(sequence: u64) -> TranscriptEntry {
    let covered = (2..=5).contains(&sequence);
    let content = if covered {
        let (group, ordinal) = if sequence <= 3 {
            ("fork-source-first", sequence - 1)
        } else {
            ("fork-source-later", sequence - 3)
        };
        mez_agent::TranscriptContextEvent::execution_block_with_metadata(
            ContextSourceKind::TranscriptAssistant,
            "assistant",
            format!("raw source {sequence}"),
            mez_agent::ContextExecutionGroupId::new(group).unwrap(),
            ordinal,
            None,
        )
        .unwrap()
        .to_transcript_content()
    } else {
        format!("raw source {sequence}")
    };
    TranscriptEntry {
        conversation_id: CONVERSATION.into(),
        sequence,
        created_at_unix_seconds: sequence,
        role: if covered {
            TranscriptRole::System
        } else {
            TranscriptRole::Assistant
        },
        turn_id: if covered {
            format!("group-{}", (sequence - 2) / 2)
        } else {
            format!("turn-{sequence}")
        },
        agent_id: "agent-%9".into(),
        pane_id: "%9".into(),
        content,
    }
}

/// Runs the actual deferred resume command against an existing prefix. Returns
/// the store for controlled later cooperating appends and epoch publications.
pub(super) fn resumed_source(
    rows: u64,
    prefix: u64,
) -> (
    RuntimeSessionService,
    AgentTranscriptStore,
    mez_core::ids::ClientId,
) {
    let store = AgentTranscriptStore::new(temp_root("fork-source-authority"));
    for sequence in 1..=rows {
        store.append(&row(sequence)).unwrap();
    }
    store
        .save_compaction_epoch(CONVERSATION, prefix, "FROZEN_PREFIX")
        .unwrap();
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .execute_agent_shell_command(&primary, &format!("/resume {CONVERSATION}"))
        .unwrap();
    let result = service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .unwrap();
    assert!(result.contains("resumed=true"), "{result}");
    let session = service.agent_shell_store().get("%1").unwrap();
    assert_eq!(session.transcript_high_water, rows);
    assert_eq!(session.transcript_entries, rows - prefix);
    (service, store, primary)
}
