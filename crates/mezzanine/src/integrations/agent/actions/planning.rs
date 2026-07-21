//! Product inputs for provider-independent action-result planning.
//!
//! This adapter computes concrete permission, subagent-scope, local lowering,
//! and MCP approval facts, then delegates canonical action-result construction
//! to `mez-agent`. Runtime execution and product error projection remain in the
//! composition crate.

use super::super::{
    ActionResult, AgentAction, AgentActionPayload, AgentTurnRecord, MezError, Result,
    local_action_plan,
};
use super::AgentTurnRunner;
use mez_agent::{ActionPlanningInput, network_action_plan, subagent_action_scope_violation};

impl<'a, P> AgentTurnRunner<'a, P> {
    /// Plans one initial action result from product-owned policy facts.
    pub(super) fn plan_action_result(
        &self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
    ) -> Result<ActionResult> {
        let local_plan = local_action_plan(action)?;
        let network_plan = network_action_plan(action);
        let subagent_scope_violation = match (self.subagent_scope, local_plan.as_ref()) {
            (Some(scope), Some(plan)) => subagent_action_scope_violation(
                self.subagent_scope_enforcement,
                scope,
                action,
                &plan.policy_command,
            )
            .map_err(MezError::invalid_args)?,
            _ => None,
        };
        let local_permission_evaluation = local_plan.as_ref().map(|plan| {
            self.permissions
                .evaluate_command_structured(&plan.policy_command)
        });
        let network_permission_evaluation = network_plan.as_ref().map(|plan| {
            self.permissions
                .evaluate_command_structured(&plan.policy_command)
        });
        let mcp_approval_required = match &action.payload {
            AgentActionPayload::McpCall { server, tool, .. } => {
                self.mcp_tool_requires_approval(server, tool)
            }
            _ => false,
        };

        mez_agent::plan_action_result(
            turn,
            action,
            ActionPlanningInput {
                local_plan: local_plan.as_ref(),
                local_rule_decision: None,
                local_permission_evaluation: local_permission_evaluation.as_ref(),
                network_plan: network_plan.as_ref(),
                network_rule_decision: None,
                network_permission_evaluation: network_permission_evaluation.as_ref(),
                approval_policy: self.permissions.approval_policy(),
                approval_bypass: self.permissions.approval_bypass(),
                mcp_approval_required,
                subagent_scope_violation: subagent_scope_violation.as_deref(),
                sandbox_first_local_prompts: self.permissions.sandbox_first_local_prompts(),
            },
        )
        .map_err(|error| MezError::invalid_state(error.message()))
    }

    /// Returns whether the selected live MCP tool requires approval.
    fn mcp_tool_requires_approval(&self, server: &str, tool: &str) -> bool {
        self.available_mcp_tools
            .iter()
            .find(|available| available.server_id == server && available.tool_name == tool)
            .map(|available| available.approval_required)
            .unwrap_or(true)
    }
}
