//! Objective publication and metadata-refresh regressions.
//!
//! Absent or unreadable metadata must not erase a published objective, while
//! explicit model objectives take precedence over bounded prompt fallbacks.

use super::fixtures::{messaging_test_execution, messaging_test_turn};
use super::*;

/// Verifies an objective refresh that carries no objective is a no-op: the
/// published value and the presence timestamp survive it unchanged.
#[test]
fn runtime_absent_objective_refresh_keeps_published_value() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "inspect the objective contract")
        .unwrap();
    let agent_id = AgentId::opaque(started.agent_id.clone()).unwrap();
    let published = service
        .message_service()
        .registered_identity(&agent_id)
        .and_then(|identity| identity.objective.clone());
    assert_eq!(published.as_deref(), Some("inspect the objective contract"));
    let published_at_ms = service
        .message_service()
        .presence()
        .into_iter()
        .find(|record| record.identity.agent_id == agent_id)
        .map(|record| record.updated_at_ms)
        .expect("published presence record");

    assert!(!service.publish_prepared_runtime_agent_objective(started.agent_id.as_str(), None));

    assert_eq!(
        service
            .message_service()
            .registered_identity(&agent_id)
            .and_then(|identity| identity.objective.clone()),
        published
    );
    assert_eq!(
        service
            .message_service()
            .presence()
            .into_iter()
            .find(|record| record.identity.agent_id == agent_id)
            .map(|record| record.updated_at_ms),
        Some(published_at_ms),
        "an absent objective refresh must not churn presence"
    );
}

/// Verifies the model-generated objective path is live: an objective carried by
/// the turn envelope is published and wins over the prompt-derived fallback.
#[test]
fn runtime_model_authored_objective_wins_over_prompt_fallback() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "inspect the fallback objective")
        .unwrap();
    let turn = messaging_test_turn(&service, &started.turn_id);
    let action = mez_agent::AgentAction {
        id: "list-agents-objective".to_string(),
        payload: mez_agent::AgentActionPayload::ListAgents {
            agent_type: None,
            scope: None,
        },
    };
    let planned =
        mez_agent::plan_action_result(&turn, &action, mez_agent::ActionPlanningInput::default())
            .expect("list_agents plan");
    let mut execution =
        messaging_test_execution(&turn, &action, planned, mez_agent::AgentTurnState::Running);
    execution.response.raw_text = r#"{"rationale":"inspect","objective":"Review the peer discovery bounds","actions":[{"type":"list_agents"}]}"#.to_string();

    assert!(service.publish_runtime_agent_objective_for_response(&turn, &execution));

    let agent_id = AgentId::opaque(started.agent_id.clone()).unwrap();
    assert_eq!(
        service
            .message_service()
            .registered_identity(&agent_id)
            .and_then(|identity| identity.objective.as_deref()),
        Some("Review the peer discovery bounds"),
        "the model-authored objective must win over the prompt fallback"
    );
}

/// Verifies each caller resolves durable objective metadata exactly once before
/// publishing it, so a second-read failure or concurrent metadata replacement
/// cannot discard a prepared prompt, model-response, or sender identity value.
///
/// The storage probe fails only the second read after it is armed. Each path
/// must therefore publish the first resolved durable override and must not ask
/// the raw publisher to resolve persistence again after preparation.
#[test]
fn runtime_prepared_objectives_survive_would_be_second_metadata_reads() {
    let root = temp_root("runtime-prepared-objective-single-read");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    store
        .save_user_objective(&conversation_id, Some("Prepared durable objective"))
        .unwrap();

    store.fail_second_subsequent_user_objective_read();
    let started = service
        .start_agent_prompt_turn("%1", "Automatic prompt objective")
        .unwrap();
    let agent_id = AgentId::opaque(started.agent_id.clone()).unwrap();
    assert_eq!(
        service
            .message_service()
            .registered_identity(&agent_id)
            .and_then(|identity| identity.objective.as_deref()),
        Some("Prepared durable objective"),
        "prompt start must publish its prepared durable value"
    );

    let turn = messaging_test_turn(&service, &started.turn_id);
    let action = mez_agent::AgentAction {
        id: "prepared-objective-response".to_string(),
        payload: mez_agent::AgentActionPayload::ListAgents {
            agent_type: None,
            scope: None,
        },
    };
    let planned =
        mez_agent::plan_action_result(&turn, &action, mez_agent::ActionPlanningInput::default())
            .expect("list_agents plan");
    let mut execution =
        messaging_test_execution(&turn, &action, planned, mez_agent::AgentTurnState::Running);
    execution.response.raw_text = r#"{"rationale":"inspect","objective":"Model replacement","actions":[{"type":"list_agents"}]}"#.to_string();
    store.fail_second_subsequent_user_objective_read();
    assert!(!service.publish_runtime_agent_objective_for_response(&turn, &execution));
    assert_eq!(
        service
            .message_service()
            .registered_identity(&agent_id)
            .and_then(|identity| identity.objective.as_deref()),
        Some("Prepared durable objective"),
        "model response must retain the prepared durable override"
    );

    store.fail_second_subsequent_user_objective_read();
    let sender = service.runtime_message_sender_identity(&turn).unwrap();
    assert_eq!(
        sender.objective.as_deref(),
        Some("Prepared durable objective"),
        "sender identity must return its prepared durable override"
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies unreadable durable metadata blocks both prompt and model objective
/// refreshes, preserving the already-published identity rather than admitting
/// an automatic overwrite through either path.
#[test]
fn runtime_unreadable_objective_metadata_preserves_published_prompt_and_model_values() {
    let root = temp_root("runtime-unreadable-objective-metadata");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "Preserve this published objective")
        .unwrap();
    let turn = messaging_test_turn(&service, &started.turn_id);
    let agent_id = AgentId::opaque(started.agent_id.clone()).unwrap();
    let conversation_id = turn.conversation_id.clone();
    store
        .save_user_objective(&conversation_id, Some("Durable objective"))
        .unwrap();
    fs::write(
        root.join(&conversation_id).join("metadata.json"),
        b"not valid metadata\n",
    )
    .unwrap();

    let prompt_error = service
        .start_agent_prompt_turn("%1", "Do not replace the published objective")
        .unwrap_err();
    assert!(
        prompt_error
            .message()
            .contains("conversation objective metadata is unavailable")
    );

    let action = mez_agent::AgentAction {
        id: "list-agents-corrupt-objective".to_string(),
        payload: mez_agent::AgentActionPayload::ListAgents {
            agent_type: None,
            scope: None,
        },
    };
    let planned =
        mez_agent::plan_action_result(&turn, &action, mez_agent::ActionPlanningInput::default())
            .expect("list_agents plan");
    let mut execution =
        messaging_test_execution(&turn, &action, planned, mez_agent::AgentTurnState::Running);
    execution.response.raw_text = r#"{"rationale":"inspect","objective":"Model overwrite","actions":[{"type":"list_agents"}]}"#.to_string();
    assert!(!service.publish_runtime_agent_objective_for_response(&turn, &execution));
    assert_eq!(
        service
            .message_service()
            .registered_identity(&agent_id)
            .and_then(|identity| identity.objective.as_deref()),
        Some("Preserve this published objective")
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies sender identity is read after its objective refresh, so an
/// immediately dispatched message carries the durable user override instead of
/// the stale identity returned by registration.
#[test]
fn runtime_message_sender_identity_returns_the_post_refresh_objective() {
    let root = temp_root("runtime-message-sender-objective-refresh");
    let _ = fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root.clone());
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "Initial automatic objective")
        .unwrap();
    let turn = messaging_test_turn(&service, &started.turn_id);
    store
        .save_user_objective(
            &turn.conversation_id,
            Some("Immediate durable sender objective"),
        )
        .unwrap();

    let sender = service.runtime_message_sender_identity(&turn).unwrap();

    assert_eq!(
        sender.objective.as_deref(),
        Some("Immediate durable sender objective")
    );
    let _ = fs::remove_dir_all(root);
}
