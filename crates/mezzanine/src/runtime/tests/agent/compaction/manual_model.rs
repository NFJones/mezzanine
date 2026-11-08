//! Idle model tests share one original committed historical reference.
//!
//! Literal user history is eligible without inventing task or execution
//! completion. `/compact` captures the actual raw source and queues canonical
//! work; callers retain responsibility for provider events and real receipts.

use super::*;

pub(super) fn queued(
    conversation: &str,
    content: String,
) -> (RuntimeSessionService, AgentTranscriptStore) {
    let mut service = test_runtime_service();
    service.replace_config_layers(vec![ConfigLayer {
        name: "manual-correction".into(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider=\"runtime-batch\"\ndefault_model_profile=\"manual-correction\"\n[providers.runtime-batch]\nkind=\"openai\"\nmodels=[\"test\"]\ndefault_model=\"test\"\n[model_profiles.manual-correction]\nprovider=\"runtime-batch\"\nmodel=\"test\"\ncontext_window_tokens=40000\nmax_input_tokens=20000\n".into(),
    }]).unwrap();
    let store = AgentTranscriptStore::new(temp_root(conversation));
    store
        .append(&TranscriptEntry {
            conversation_id: conversation.into(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::User,
            turn_id: "old".into(),
            agent_id: "agent-%1".into(),
            pane_id: "%1".into(),
            content,
        })
        .unwrap();
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", conversation, 1)
        .unwrap();
    let response = service
        .execute_agent_shell_command(&primary, "/compact")
        .unwrap();
    assert!(response.contains("state=queued"), "{response}");
    assert!(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap()
            .replay_source
            .is_some()
    );
    assert!(service.agent_turn_ledger().turns().is_empty());
    (service, store)
}
