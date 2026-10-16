//! Saved-fork continuity at the actual first provider encoding boundary.
//!
//! All history is synthetic. These tests run the real fork command and reopen
//! durable storage without sending provider requests or replaying any actions.

use super::*;

/// Encodes the first prompt and extracts ordered provider text items, rather
/// than relying on substring matches in metadata or on archive preservation.
fn encoded_history(service: &mut RuntimeSessionService, pane: &str) -> Vec<String> {
    let context = service.agent_context_for_pane_prompt(pane, "continue", 0).unwrap();
    let (_, profile) = service.active_model_profile_for_pane(pane, "agent-%1", None).unwrap();
    let turn = mez_agent::AgentTurnRecord {
        turn_id: "fork-first-request".into(),
        conversation_id: "fork-target".into(),
        agent_id: "agent-%1".into(),
        pane_id: pane.into(),
        trigger: mez_agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 3,
        deadline_at_unix_millis: 0,
        policy_profile: "runtime".into(),
        model_profile: "test".into(),
        parent_turn_id: None,
        state: AgentTurnState::Running,
        cooperation_mode: None,
        initial_capability: None,
    };
    let request = crate::integrations::agent::context::assemble_model_request(
        &profile, mez_agent::ProviderApiCompatibility::OpenAiResponses, &turn, &context,
    ).unwrap();
    let bytes = mez_agent::openai_responses_request_body_with_stream(&request, false).unwrap();
    let body: serde_json::Value = serde_json::from_str(&bytes).unwrap();
    body["input"].as_array().unwrap().iter()
        .flat_map(|item| item["content"].as_array().into_iter().flatten())
        .filter_map(|part| part["text"].as_str())
        .map(ToOwned::to_owned)
        .filter(|text| text.contains("FORK_ROW_") || text.contains("FORK_SUMMARY"))
        .collect()
}

/// A saved source summarized through row 80 of 100 must fork its active
/// projection, not resurrect 80 covered rows. The real command's first encoded
/// request and a reopened/resumed first request must contain precisely the same
/// summary and ordered suffix while the full archive remains available.
#[test]
fn runtime_saved_fork_epoch_first_encoded_request_survives_reopen() {
    let root = temp_root("runtime-saved-fork-epoch");
    let store = AgentTranscriptStore::new(root.clone());
    for sequence in 1..=100 {
        store.append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "fork-source".into(), sequence,
            created_at_unix_seconds: sequence,
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: format!("turn-{sequence}"), agent_id: "agent-%1".into(),
            pane_id: "%1".into(), content: format!("FORK_ROW_{sequence:03}"),
        }).unwrap();
    }
    store.save_compaction_epoch("fork-source", 80, "FORK_SUMMARY").unwrap();
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
    let primary = service.attach_primary("primary", true, Size::new(80, 24).unwrap(), 120).unwrap();
    service.start_initial_pane_process(Some("cat >/dev/null")).unwrap();
    service.agent_shell_store_mut().enter_or_resume("%1").unwrap();
    let resumed = service.execute_agent_shell_command(&primary, "/resume fork-source").unwrap();
    assert!(!resumed.contains("\"error\""), "{resumed}");
    let forked = service.execute_agent_shell_command(&primary, "/fork fork-target").unwrap();
    assert!(forked.contains("forked=true"), "{forked}");
    let pane = service.agent_shell_store().sessions()
        .find(|session| session.session_id == "fork-target").unwrap().pane_id.clone();
    let encoded = encoded_history(&mut service, &pane);
    service.terminate_all_pane_processes().unwrap();
    assert_eq!(encoded.len(), 21, "{encoded:#?}");
    assert!(encoded[0].contains("FORK_SUMMARY"), "{encoded:#?}");
    for (text, sequence) in encoded[1..].iter().zip(81..=100) {
        assert!(text.ends_with(&format!("FORK_ROW_{sequence:03}")), "{text}");
    }
    assert_eq!(store.inspect("fork-target").unwrap().len(), 100);
    let mut reopened = test_runtime_service();
    reopened.set_agent_transcript_store(AgentTranscriptStore::new(root));
    let primary = reopened.attach_primary("reopened", true, Size::new(80, 24).unwrap(), 120).unwrap();
    reopened.agent_shell_store_mut().enter_or_resume("%1").unwrap();
    let resumed = reopened.execute_agent_shell_command(&primary, "/resume fork-target").unwrap();
    assert!(!resumed.contains("\"error\""), "{resumed}");
    assert_eq!(encoded_history(&mut reopened, "%1"), encoded);
}
