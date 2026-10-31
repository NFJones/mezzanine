//! Production native send/wait integration over real local acceptance custody.
//!
//! Offline providers exercise normal execution, conversation, context, scheduler
//! and terminal result owners. No lifecycle envelope or acceptance flag is forged.
//! Actor-owned provider claims and the larger fault matrix are separate gates.

mod expiry;
mod lifecycle;
mod protocol;
mod retention;
mod terminal;

use super::*;
use mez_agent::{AgentAction, AgentActionPayload, SayStatus};

/// Creates a genuine native parent conversation and a registered nonowned peer.
/// A non-live parent has explicitly process-free ownership, rather than an
/// unavailable OS observation being mistaken for an absent process.
fn fixture() -> (RuntimeSessionService, AgentTurnRecord, SenderIdentity) {
    let (service, parent, peer, _) = fixture_with_primary();
    (service, parent, peer)
}

/// Retains the actual writable primary for lifecycle commands in fault tests.
fn fixture_with_primary() -> (
    RuntimeSessionService,
    AgentTurnRecord,
    SenderIdentity,
    mez_core::ids::ClientId,
) {
    let mut service = test_runtime_service();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    service.configure_agent_scheduler_limit(1).unwrap();
    service.set_agent_transcript_store(AgentTranscriptStore::new(temp_root(
        "runtime-request-bound",
    )));
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.session.set_pane_live_state("%1", false).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_pane_approval_policy_override("%1", Some(mez_agent::ApprovalPolicy::AutoAllow));
    let started = service
        .start_agent_prompt_turn("%1", "coordinate one exact request")
        .unwrap();
    let parent = service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .clone();
    let peer = service
        .ensure_runtime_message_identity(
            "agent-request-peer",
            None,
            "worker",
            &[],
            current_unix_millis(),
        )
        .unwrap();
    (service, parent, peer, primary)
}

/// Uses the same provider application and action planning boundary as native
/// turns, with a deterministic offline action response and no transport replay.
fn execute(
    service: &mut RuntimeSessionService,
    turn: &AgentTurnRecord,
    mut action: AgentAction,
) -> mez_agent::AgentTurnExecution {
    // The MAAP parser assigns response-local action ordinals. Use that real
    // shape so the runtime scopes each new provider execution independently.
    action.id = "action-1".into();
    let raw_text = format!(
        "request integration {:?} context={}",
        action,
        service.agent_turn_contexts()[&turn.turn_id].event_sequence_high_water_mark()
    );
    service
        .execute_agent_turn_with_provider(
            &turn.turn_id,
            &RuntimeBatchProvider {
                response: mez_agent::ModelResponse {
                    provider: "runtime-batch".into(),
                    model: "test".into(),
                    raw_text,
                    usage: Default::default(),
                    latest_request_usage: None,
                    quota_usage: Default::default(),
                    action_batch: Some(mez_agent::MaapBatch {
                        rationale: "perform exact request coordination".into(),
                        actions: vec![action],
                    }),
                    provider_transcript_events: vec![],
                },
            },
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap()
}

/// Sends once through the real native action boundary and returns the actual
/// accepted message ID; tests never guess scoped action IDs or request IDs.
fn send(
    service: &mut RuntimeSessionService,
    parent: &AgentTurnRecord,
    peer: &str,
    tracked: bool,
) -> String {
    let execution = execute(
        service,
        parent,
        AgentAction {
            id: "question".into(),
            payload: AgentActionPayload::SendMessage {
                expect_reply: tracked,
                recipient: format!("agent:{peer}"),
                scope: Some("session".into()),
                content_type: "text/plain; charset=utf-8".into(),
                payload: "please answer this request".into(),
                correlation_id: None,
            },
        },
    );
    let result = &execution.action_results[0];
    assert_eq!(result.status, ActionStatus::Succeeded, "{result:?}");
    let structured: serde_json::Value =
        serde_json::from_str(result.structured_content_json.as_deref().unwrap()).unwrap();
    structured["message_id"].as_str().unwrap().to_string()
}

/// Parks only on the real accepted ID, optionally selecting one frozen target.
fn wait(
    service: &mut RuntimeSessionService,
    parent: &AgentTurnRecord,
    message: &str,
    from: Option<&str>,
) {
    execute(
        service,
        parent,
        AgentAction {
            id: "targeted-wait".into(),
            payload: AgentActionPayload::Wait {
                for_message: Some(message.into()),
                from_agent: from.map(str::to_owned),
            },
        },
    );
}

/// Accepts actual authenticated peer mail; supplied correlation is only content
/// until the runtime proves its original accepted sender and reverse audience.
fn reply(
    service: &mut RuntimeSessionService,
    parent: &AgentTurnRecord,
    sender: &SenderIdentity,
    id: &str,
    correlation: Option<&str>,
) -> u64 {
    let sequence = accept_reply(service, parent, sender, id, correlation);
    service
        .deliver_pending_runtime_agent_messages(current_unix_millis())
        .unwrap();
    sequence
}

/// Accepts authenticated mail without offering, committing or acknowledging it.
/// This preserves the real acceptance/retirement race for receiver tests.
fn accept_reply(
    service: &mut RuntimeSessionService,
    parent: &AgentTurnRecord,
    sender: &SenderIdentity,
    id: &str,
    correlation: Option<&str>,
) -> u64 {
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            Envelope {
                protocol: "mmp/1",
                id: id.into(),
                message_type: "send".into(),
                time: "fixture:reply".into(),
                sender: sender.clone(),
                recipient: mez_agent::messaging::Recipient::Agent(
                    AgentId::opaque(parent.agent_id.clone()).unwrap(),
                ),
                correlation_id: correlation.map(str::to_owned),
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".into(),
                payload: "peer answer or unrelated coordination".into(),
                extension_fields: vec![],
            },
            MessageScope::Session,
            current_unix_millis(),
        )
        .unwrap()
        .sequence
}

/// Completes the original task after it receives evidence, proving recovery did
/// not replace the task or require automatic resending of the request.
fn finish(service: &mut RuntimeSessionService, parent: &AgentTurnRecord) {
    execute(
        service,
        parent,
        AgentAction {
            id: "finish".into(),
            payload: AgentActionPayload::Say {
                status: SayStatus::Final,
                text: "completed authorized independent work".into(),
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
            },
        },
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Completed
    );
}

/// Creates a genuine persistent task through the normal native spawn owner.
fn spawn_child(service: &mut RuntimeSessionService, parent: &AgentTurnRecord) -> AgentTurnRecord {
    let mut spawn = runtime_spawn_agent_action(
        "tracked-child",
        "produce a result without sending a message",
    );
    if let AgentActionPayload::SpawnAgent {
        lifetime,
        objective,
        ..
    } = &mut spawn.payload
    {
        *lifetime = mez_agent::SubagentLifetime::Persistent;
        *objective = Some("answer assigned work".into());
    }
    let spawned = service
        .execute_spawn_action_for_turn(parent, &spawn)
        .unwrap();
    let structured: serde_json::Value =
        serde_json::from_str(spawned.structured_content_json.as_deref().unwrap()).unwrap();
    let child_id = structured["spawn"]["agent"]["id"].as_str().unwrap();
    service
        .agent_turn_ledger()
        .latest_turn_for_pane(child_id.strip_prefix("agent-").unwrap())
        .unwrap()
        .clone()
}
