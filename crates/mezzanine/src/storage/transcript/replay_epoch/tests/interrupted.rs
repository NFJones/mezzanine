//! Interruption references retain semantic and producer custody without rights.

use super::*;
use mez_agent::transcript::{
    TranscriptContextEvent, TranscriptContextRecord, interrupted_turn_context_block,
};
use mez_agent::{TranscriptEntry, TranscriptRole};

/// The same readable hint cannot borrow another producer's identity, and its
/// original record cannot authorize changed prompt/reason/observations. This
/// checks the shared source matcher used by later publication and saved copies.
#[test]
fn interrupted_reference_requires_original_producer_and_semantics() {
    let block =
        interrupted_turn_context_block("prompt 雪", "cancelled", &["settled result".into()]);
    let owner = ReplayEpochContext::new(
        ReplayConversationId::new("interrupted-source").unwrap(),
        AgentContext::new(vec![block.clone()]).unwrap(),
    )
    .unwrap();
    let record = owner.context().chronology()[0].record();
    let event = TranscriptContextEvent::InterruptedTurn {
        prompt: "prompt 雪".into(),
        reason: "cancelled".into(),
        evidence: vec!["settled result".into()],
    };
    let authored = TranscriptContextRecord::new(event, record, None).unwrap();
    let row = TranscriptEntry {
        conversation_id: "interrupted-source".into(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::System,
        turn_id: "hint-turn".into(),
        agent_id: "hint-agent".into(),
        pane_id: "hint-pane".into(),
        content: authored.to_transcript_content().unwrap(),
    };
    let producer = ReplaySourceOrigin::new(&row.turn_id, &row.agent_id, &row.pane_id).unwrap();
    let items = escrow::extend(&[], &owner, &producer, std::slice::from_ref(&row)).unwrap();
    assert_eq!(items.len(), 1);
    assert!(items[0].matches(&row, &authored, &block.content));
    for field in ["turn", "agent", "pane", "role"] {
        let mut changed = row.clone();
        match field {
            "turn" => changed.turn_id = "another-turn".into(),
            "agent" => changed.agent_id = "another-agent".into(),
            "pane" => changed.pane_id = "another-pane".into(),
            _ => changed.role = TranscriptRole::Assistant,
        }
        assert!(
            !items[0].matches(&changed, &authored, &block.content),
            "{field}"
        );
    }
    for field in ["prompt", "reason", "evidence"] {
        let event = TranscriptContextEvent::InterruptedTurn {
            prompt: if field == "prompt" {
                "changed"
            } else {
                "prompt 雪"
            }
            .into(),
            reason: if field == "reason" {
                "changed"
            } else {
                "cancelled"
            }
            .into(),
            evidence: vec![
                if field == "evidence" {
                    "changed"
                } else {
                    "settled result"
                }
                .into(),
            ],
        };
        let altered = TranscriptContextRecord::new(event, record, None).unwrap();
        let changed = TranscriptEntry {
            content: altered.to_transcript_content().unwrap(),
            ..row.clone()
        };
        assert!(
            !items[0].matches(&changed, &altered, &block.content),
            "{field}"
        );
    }
}
