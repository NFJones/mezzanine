//! Create a genuine delegated request for physical root-budget contention.
//! Spawn, native startup, lineage, membership storage, provider preparation and
//! native process retirement use production owners; no membership is injected.

use super::*;

pub(super) async fn prepare(
    service: &mut RuntimeSessionService,
    original: &RuntimeAgentProviderDispatch,
    member: &BudgetMembership,
) -> RuntimeAgentProviderDispatch {
    use crate::runtime::{RuntimeSubagentPlacement, SubagentWaitPolicy};
    use mez_agent::{CooperationMode, SubagentSessionMode, SubagentSpawnRequest};
    let primary = service
        .session()
        .clients()
        .iter()
        .find(|client| service.session().is_attached_primary(&client.id))
        .unwrap()
        .id
        .clone();
    service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    service.configure_subagent_policy(8, 8, 8, 8, SubagentWaitPolicy::Detach);
    let response = service
        .spawn_runtime_subagent_session_owned(
            SubagentSpawnRequest {
                parent_agent_id: original.turn.agent_id.clone(),
                requested_role: "worker".into(),
                placement: "new-pane".into(),
                cooperation_mode: CooperationMode::ExploreOnly,
                cooperation_mode_defaulted: false,
                read_scopes: Vec::new(),
                read_scopes_defaulted: false,
                write_scopes: Vec::new(),
                write_scopes_defaulted: false,
                session_mode: SubagentSessionMode::New,
                initial_model_size: None,
                initial_reasoning_effort: None,
                task_prompt: "Qualify original descendant provider contention.".into(),
                explicit_user_approval: false,
                skip_initial_turn: false,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: mez_mux::layout::SplitDirection::Vertical,
                select: false,
            },
        )
        .unwrap();
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    let turn = service
        .agent_turn_ledger()
        .turn(response["turn"]["id"].as_str().unwrap())
        .unwrap()
        .clone();
    assert_ne!(turn.conversation_id, member.conversation);
    assert!(service.agent_provider_task_is_pending(&turn.turn_id));
    let preparation = service.prepare_agent_provider_work(&turn.turn_id).unwrap();
    let prepared = RuntimeSessionService::execute_agent_provider_preparation(preparation).await;
    service.apply_agent_provider_preparation(prepared).unwrap();
    let mut dispatch = service
        .claim_configured_agent_provider_task(
            &AgentId::opaque(turn.agent_id).unwrap(),
            &turn.turn_id,
        )
        .unwrap()
        .unwrap();
    let child = &dispatch.budget_owner.as_ref().unwrap().member;
    assert_eq!(child.root, member.root);
    assert_eq!(child.namespace, member.namespace);
    assert_eq!(child.parent.as_deref(), Some(member.conversation.as_str()));
    service
        .record_claimed_agent_provider_task(&dispatch, 7, 30_000)
        .unwrap();
    dispatch.claim_generation = 7;
    dispatch
}
