//! Agent tests for transcript behavior.
//!
//! This bounded leaf owns the scenarios for this concern while shared
//! fixtures remain in the parent module.

use super::*;

#[test]
/// Verifies turn execution persistence appends to durable transcript store.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn turn_execution_persistence_appends_to_durable_transcript_store() {
    let root =
        std::env::temp_dir().join(format!("mez-agent-turn-persistence-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root);
    let turn = turn();
    let execution = AgentTurnExecution {
        request: assemble_model_request(
            &ModelProfile {
                provider: "openai".to_string(),
                model: "default".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
            &turn,
            &AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "user".to_string(),
                content: "run pwd".to_string(),
            }])
            .unwrap(),
        )
        .unwrap(),
        response: ModelResponse {
            provider: "openai".to_string(),
            model: "default".to_string(),
            raw_text: "I will inspect the directory.".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: None,
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![ActionResult::succeeded(
            &turn,
            &shell_action("a1"),
            vec!["/repo\n".to_string()],
            Some(r#"{"exit_code":0}"#.to_string()),
        )],
        final_turn: true,
        terminal_state: AgentTurnState::Completed,
    };

    let first = persist_turn_execution_transcript(&store, "conv1", 200, &turn, &execution).unwrap();
    let second =
        persist_turn_execution_transcript(&store, "conv1", 201, &turn, &execution).unwrap();
    let persisted = store.inspect("conv1").unwrap();

    assert_eq!(first[0].sequence, 1);
    assert_eq!(second[0].sequence, first.len() as u64 + 1);
    assert_eq!(persisted.len(), first.len() + second.len());
    assert!(persisted.iter().any(|entry| {
        entry.role == DurableTranscriptRole::Tool && entry.content.contains("exit_code")
    }));
}
