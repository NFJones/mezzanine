//! Runtime agent discovery for the read-only `list_agents` action.
//!
//! This module executes the fixed read-only `list_agents` MAAP action over the
//! canonical message-service discovery and presence views. It returns bounded
//! identity rows for the requesting agent itself, offline agents,
//! runtime-internal controllers, and agents in other panes and windows, and it
//! never grants authority or mutates delivery state.

use std::collections::BTreeMap;

use mez_agent::messaging::AgentPresenceStatus;
use mez_agent::{
    AGENT_LIST_MAX_CAPABILITIES, AGENT_LIST_MAX_ROWS, ActionResult, ActionStatus, AgentAction,
    AgentActionPayload, AgentKind, AgentListFilter, agent_list_bounded_text,
    agent_list_text_is_truncated,
};

use super::{
    AgentTurnExecution, AgentTurnRecord, AgentTurnState, MezError, Result, RuntimeSessionService,
    runtime_agent_action_summary, runtime_agent_turn_state_from_action_results,
};

/// Bounded discovery rows plus whether the matching set was truncated.
struct RuntimeAgentListRows {
    /// Ordered discovery rows limited to the documented maximum.
    rows: Vec<serde_json::Value>,
    /// Whether matching agents were dropped to honor the documented bound.
    truncated: bool,
}

impl RuntimeSessionService {
    /// Executes pending read-only `list_agents` actions for one running turn.
    pub(crate) fn execute_running_list_agents_actions_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &mut AgentTurnExecution,
    ) -> Result<usize> {
        if execution.terminal_state != AgentTurnState::Running {
            return Ok(0);
        }
        let Some(batch) = execution.response.action_batch.clone() else {
            return Ok(0);
        };
        let mut executed = 0usize;
        for index in 0..execution.action_results.len() {
            if execution.action_results[index].status != ActionStatus::Running
                || execution.action_results[index].action_type != "list_agents"
            {
                continue;
            }
            let action = batch
                .actions
                .iter()
                .find(|action| action.id == execution.action_results[index].action_id)
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_state(
                        "running agent discovery result does not match an action",
                    )
                })?;
            if !self
                .append_agent_action_execution_text_to_terminal_buffer(&turn.pane_id, &action)?
            {
                self.append_agent_status_text_to_terminal_buffer(
                    &turn.pane_id,
                    &format!(
                        "agent: {}",
                        runtime_agent_action_summary(&action)
                            .unwrap_or_else(|| "agent discovery".to_string())
                    ),
                )?;
            }
            execution.action_results[index] = self.execute_list_agents_action(turn, &action)?;
            executed = executed.saturating_add(1);
        }
        execution.terminal_state = runtime_agent_turn_state_from_action_results(
            &execution.action_results,
            execution.final_turn,
        );
        Ok(executed)
    }

    /// Projects one read-only `list_agents` action into a settled result.
    fn execute_list_agents_action(
        &self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
    ) -> Result<ActionResult> {
        let AgentActionPayload::ListAgents { agent_type } = &action.payload else {
            return Err(MezError::invalid_args(
                "agent discovery execution requires a list_agents action",
            ));
        };
        let filter = super::super::json::runtime_list_agents_agent_type(agent_type.as_deref())?;
        let RuntimeAgentListRows { rows, truncated } = self.runtime_agent_list_rows(turn, filter);
        let structured = serde_json::json!({
            "agent_type": filter.as_str(),
            "count": rows.len(),
            "truncated": truncated,
            "agents": rows,
        })
        .to_string();
        Ok(ActionResult::succeeded(
            turn,
            action,
            vec![format!(
                "agent discovery returned {} agent(s) for agent_type={}",
                rows.len(),
                filter.as_str()
            )],
            Some(structured),
        ))
    }

    /// Builds the bounded, ordered discovery rows for one agent-type filter.
    fn runtime_agent_list_rows(
        &self,
        turn: &AgentTurnRecord,
        filter: AgentListFilter,
    ) -> RuntimeAgentListRows {
        let service = self.control.message_service();
        let presence = service
            .presence()
            .into_iter()
            .map(|record| {
                (
                    record.identity.agent_id.as_str().to_string(),
                    runtime_agent_presence_name(record.status),
                )
            })
            .collect::<BTreeMap<String, &'static str>>();
        let mut candidates = BTreeMap::new();
        for identity in service.discover_agents_filtered(None, None, None, None, None, &[]) {
            let agent_id = identity.agent_id.as_str().to_string();
            let persistent = self.persistent_subagent(&agent_id);
            let capabilities = identity
                .capabilities
                .iter()
                .take(AGENT_LIST_MAX_CAPABILITIES)
                .map(|capability| agent_list_bounded_text(capability))
                .collect::<Vec<_>>();
            let mut row_truncated = identity.capabilities.len() > AGENT_LIST_MAX_CAPABILITIES
                || agent_list_text_is_truncated(&agent_id)
                || identity
                    .role
                    .as_deref()
                    .is_some_and(agent_list_text_is_truncated)
                || identity
                    .pane_id
                    .as_ref()
                    .is_some_and(|pane_id| agent_list_text_is_truncated(pane_id.as_str()))
                || identity
                    .window_id
                    .as_ref()
                    .is_some_and(|window_id| agent_list_text_is_truncated(window_id.as_str()))
                || identity
                    .objective
                    .as_deref()
                    .is_some_and(agent_list_text_is_truncated);
            row_truncated |= identity
                .capabilities
                .iter()
                .any(|capability| agent_list_text_is_truncated(capability));
            let row = serde_json::json!({
                "agent_id": agent_list_bounded_text(&agent_id),
                "kind": self.runtime_agent_kind(&agent_id).as_str(),
                "is_self": agent_id == turn.agent_id,
                "role": identity.role.as_deref().map(agent_list_bounded_text),
                "pane_id": identity
                    .pane_id
                    .as_ref()
                    .map(|pane_id| agent_list_bounded_text(pane_id.as_str())),
                "window_id": identity
                    .window_id
                    .as_ref()
                    .map(|window_id| agent_list_bounded_text(window_id.as_str())),
                "capabilities": capabilities,
                "status": presence.get(&agent_id).copied(),
                "objective": identity.objective.as_deref().map(agent_list_bounded_text),
                "persistent": persistent.is_some(),
                "parent_agent_id": persistent
                    .map(|record| agent_list_bounded_text(&record.parent_agent_id)),
                "owned_by_self": persistent.is_some_and(|record| {
                    record.parent_agent_id == turn.agent_id
                        && record.parent_conversation_id == turn.conversation_id
                }),
                "truncated": row_truncated,
            });
            candidates.insert(agent_id, row);
        }
        candidates
            .entry(turn.agent_id.clone())
            .or_insert_with(|| self.runtime_self_agent_list_row(turn, &presence));
        let mut rows = candidates
            .into_iter()
            .filter(|(agent_id, _)| filter.accepts(self.runtime_agent_kind(agent_id)))
            .map(|(_, row)| row)
            .collect::<Vec<_>>();
        let truncated = rows.len() > AGENT_LIST_MAX_ROWS;
        rows.truncate(AGENT_LIST_MAX_ROWS);
        RuntimeAgentListRows { rows, truncated }
    }

    /// Builds the requesting agent's own discovery row.
    ///
    /// The requesting agent is always discoverable, including before its first
    /// message registers an identity with the message service.
    fn runtime_self_agent_list_row(
        &self,
        turn: &AgentTurnRecord,
        presence: &BTreeMap<String, &'static str>,
    ) -> serde_json::Value {
        serde_json::json!({
            "agent_id": agent_list_bounded_text(&turn.agent_id),
            "kind": self.runtime_agent_kind(&turn.agent_id).as_str(),
            "is_self": true,
            "role": "agent",
            "pane_id": agent_list_bounded_text(&turn.pane_id),
            "window_id": self
                .find_pane_descriptor(&turn.pane_id)
                .map(|descriptor| agent_list_bounded_text(descriptor.window_id.as_str())),
            "capabilities": ["agent-harness"],
            "status": presence.get(&turn.agent_id).copied(),
            "objective": self
                .runtime_agent_turn_objective(turn)
                .as_deref()
                .map(agent_list_bounded_text),
            "truncated": false,
        })
    }

    /// Classifies one discovered agent into its discovery kind.
    ///
    /// Routed workers and macro-managed bridge children are runtime-internal
    /// controllers; a macro judge runs on the parent turn and never registers an
    /// identity of its own. Agents with a subagent lineage map to
    /// `AgentConversationKind::Subagent`, which default resume discovery hides,
    /// and every remaining pane agent maps to a `Root` primary agent.
    fn runtime_agent_kind(&self, agent_id: &str) -> AgentKind {
        if self.runtime_agent_is_internal_controller(agent_id) {
            AgentKind::Internal
        } else if self.agent.subagent_lineage.contains_key(agent_id) {
            AgentKind::Subagent
        } else {
            AgentKind::Primary
        }
    }

    /// Reports whether one agent is a runtime-internal controller agent.
    fn runtime_agent_is_internal_controller(&self, agent_id: &str) -> bool {
        self.agent
            .macro_managed_subagent_agents
            .contains_key(agent_id)
            || self
                .agent
                .routed_workflows_by_parent_turn
                .values()
                .any(|workflow| workflow.child_agent_id.as_deref() == Some(agent_id))
    }
}

/// Returns the stable discovery name for one agent presence status.
fn runtime_agent_presence_name(status: AgentPresenceStatus) -> &'static str {
    match status {
        AgentPresenceStatus::Available => "available",
        AgentPresenceStatus::Busy => "busy",
        AgentPresenceStatus::Blocked => "blocked",
        AgentPresenceStatus::Offline => "offline",
    }
}
