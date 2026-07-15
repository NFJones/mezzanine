//! Provider-independent initial action-result planning.
//!
//! This module converts one validated MAAP action plus product-supplied policy
//! facts into its initial canonical `ActionResult`. Product adapters retain
//! command classification, local semantic lowering, subagent scope inspection,
//! MCP registry lookup, and concrete execution. The lower planner owns approval
//! transitions, auto-allow metadata, pending-runtime envelopes, and terminal
//! complete/abort results.

use std::error::Error;
use std::fmt;

use crate::{
    ActionResult, ActionStatus, AgentAction, AgentActionPayload, AgentContext, AgentTurnExecution,
    AgentTurnResultIdentity, AgentTurnState, ApprovalPolicy, LocalActionPlan, MaapBatch,
    MemoryActionBudget, ModelRequest, ModelResponse, ModelTokenUsage, NetworkActionPlan,
    RuleDecision, SayStatus, network_action_structured_content_json,
    shell_read_observations_for_command, turn_state_from_action_results,
};

/// Product-supplied facts needed to plan one validated action result.
#[derive(Debug, Clone, Copy)]
pub struct ActionPlanningInput<'a> {
    /// Lowered local execution plan, when this is a shell-backed action.
    pub local_plan: Option<&'a LocalActionPlan>,
    /// Product permission decision for the local plan.
    pub local_rule_decision: Option<RuleDecision>,
    /// Lowered network execution plan, when this is a network action.
    pub network_plan: Option<&'a NetworkActionPlan>,
    /// Product permission decision for the network plan.
    pub network_rule_decision: Option<RuleDecision>,
    /// Active prompt handling policy.
    pub approval_policy: ApprovalPolicy,
    /// Whether fresh approval prompts are bypassed by product policy.
    pub approval_bypass: bool,
    /// Whether the selected MCP tool requires approval.
    pub mcp_approval_required: bool,
    /// Product-computed subagent scope violation for a local action.
    pub subagent_scope_violation: Option<&'a str>,
}

impl Default for ActionPlanningInput<'_> {
    fn default() -> Self {
        Self {
            local_plan: None,
            local_rule_decision: None,
            network_plan: None,
            network_rule_decision: None,
            approval_policy: ApprovalPolicy::Ask,
            approval_bypass: false,
            mcp_approval_required: true,
            subagent_scope_violation: None,
        }
    }
}

/// Canonical initial results and lifecycle state derived for one MAAP batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedBatchActionResults {
    /// Initial result for every action in provider-authored order.
    pub action_results: Vec<ActionResult>,
    /// Whether the provider declared this batch terminal after effects finish.
    pub final_turn: bool,
    /// Lifecycle state derived from all initial results and final-turn intent.
    pub terminal_state: AgentTurnState,
}

/// Plans initial canonical results for one validated MAAP batch.
///
/// Persistent-memory guardrails run before the injected product planner so
/// skipped memory actions cannot reach concrete runtime dispatch. All other
/// actions retain provider order and delegate product-specific planning facts
/// through the callback.
pub fn plan_batch_action_results<Error>(
    turn: &(impl AgentTurnResultIdentity + ?Sized),
    context: &AgentContext,
    batch: &MaapBatch,
    mut plan_action: impl FnMut(&AgentAction) -> Result<ActionResult, Error>,
) -> Result<PlannedBatchActionResults, Error> {
    let mut action_results = Vec::with_capacity(batch.actions.len());
    let mut memory_budget = MemoryActionBudget::from_context(context);
    for action in &batch.actions {
        if let Some(result) =
            memory_budget.accept_or_skip(turn, action, &batch.rationale, batch.thought.as_deref())
        {
            action_results.push(result);
            continue;
        }
        action_results.push(plan_action(action)?);
    }
    let final_turn = batch.final_turn;
    let terminal_state = turn_state_from_action_results(&action_results, final_turn);
    Ok(PlannedBatchActionResults {
        action_results,
        final_turn,
        terminal_state,
    })
}

/// Projects one validated MAAP batch into the canonical turn-execution record.
///
/// Product adapters supply only per-action policy planning. Request, response,
/// token accounting, ordered results, final-turn intent, and lifecycle state
/// are assembled by the provider-independent owner.
pub fn plan_turn_execution_from_batch<Error>(
    turn: &(impl AgentTurnResultIdentity + ?Sized),
    context: &AgentContext,
    request: ModelRequest,
    response: ModelResponse,
    latest_response_usage: ModelTokenUsage,
    batch: &MaapBatch,
    plan_action: impl FnMut(&AgentAction) -> Result<ActionResult, Error>,
) -> Result<AgentTurnExecution, Error> {
    let planned = plan_batch_action_results(turn, context, batch, plan_action)?;
    Ok(AgentTurnExecution {
        request,
        response,
        latest_response_usage,
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: planned.action_results,
        final_turn: planned.final_turn,
        terminal_state: planned.terminal_state,
    })
}

/// Builds the canonical terminal failure used when negotiation finishes
/// without an executable MAAP batch.
pub fn failed_turn_execution_without_batch(
    request: ModelRequest,
    response: ModelResponse,
    latest_response_usage: ModelTokenUsage,
) -> AgentTurnExecution {
    AgentTurnExecution {
        request,
        response,
        latest_response_usage,
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: Vec::new(),
        final_turn: true,
        terminal_state: AgentTurnState::Failed,
    }
}

/// Plans the initial canonical result for one validated MAAP action.
pub fn plan_action_result(
    turn: &(impl AgentTurnResultIdentity + ?Sized),
    action: &AgentAction,
    input: ActionPlanningInput<'_>,
) -> ActionPlanningResult<ActionResult> {
    match &action.payload {
        AgentActionPayload::Say { status, text, content_type } => Ok(ActionResult::succeeded(
            turn,
            action,
            vec![text.clone()],
            Some(say_action_structured_content_json(*status, content_type, text)),
        )),
        AgentActionPayload::RequestCapability { .. } => Err(ActionPlanningError::new(
            "request_capability reached executable action planning",
        )),
        AgentActionPayload::RequestSkills => Ok(ActionResult::running(
            turn, action, vec!["skill catalog accepted for runtime lookup".to_string()],
            Some(r#"{"state":"pending_runtime_skill_lookup"}"#.to_string()),
        )),
        AgentActionPayload::CallSkill { name, .. } => Ok(ActionResult::running(
            turn, action, vec![format!("skill {name} accepted for runtime loading")],
            Some(serde_json::json!({"state":"pending_runtime_skill_load","name":name}).to_string()),
        )),
        _ if input.local_plan.is_some() => plan_local_action(turn, action, input),
        _ if input.network_plan.is_some() => plan_network_action(turn, action, input),
        AgentActionPayload::SendMessage { recipient, content_type, payload } => Ok(ActionResult::running(
            turn, action, vec!["message accepted for local delivery".to_string()],
            Some(serde_json::json!({
                "recipient":recipient,"content_type":content_type,"bytes":payload.len(),
                "message_id":serde_json::Value::Null,"delivery_status":"pending_runtime_delivery",
                "protocol_error":serde_json::Value::Null
            }).to_string()),
        )),
        AgentActionPayload::SpawnAgent { role, placement, cooperation_mode, read_scopes, write_scopes, task_prompt } => Ok(ActionResult::running(
            turn, action, vec!["subagent spawn accepted for control endpoint placement".to_string()],
            Some(serde_json::json!({
                "role":role,"placement":placement,"cooperation_mode":cooperation_mode,
                "read_scopes":read_scopes,"write_scopes":write_scopes,"prompt_bytes":task_prompt.len()
            }).to_string()),
        )),
        AgentActionPayload::MemorySearch { .. } | AgentActionPayload::MemoryStore { .. } => Ok(ActionResult::running(
            turn, action, vec!["memory action accepted for runtime execution".to_string()],
            Some(r#"{"state":"pending_runtime_memory"}"#.to_string()),
        )),
        AgentActionPayload::IssueAdd { .. } | AgentActionPayload::IssueUpdate { .. }
        | AgentActionPayload::IssueQuery { .. } | AgentActionPayload::IssueDelete { .. } => Ok(ActionResult::running(
            turn, action, vec!["issue action accepted for runtime execution".to_string()],
            Some(r#"{"state":"pending_runtime_issue"}"#.to_string()),
        )),
        AgentActionPayload::ConfigChange { setting_path, operation, .. } => {
            plan_config_change(turn, action, setting_path, operation, input)
        }
        AgentActionPayload::McpCall { server, tool, arguments_json } => {
            plan_mcp_call(turn, action, server, tool, arguments_json, input)
        }
        AgentActionPayload::Complete => Ok(ActionResult::succeeded(
            turn, action, vec!["turn complete".to_string()], Some(r#"{"complete":true}"#.to_string()),
        )),
        AgentActionPayload::Abort { reason } => ActionResult::failed(
            turn, action, ActionStatus::Cancelled, "agent_aborted", reason,
        ).map_err(ActionPlanningError::from_contract),
        _ => Err(ActionPlanningError::new(
            "shell-backed action was not planned before action-result planning",
        )),
    }
}

/// Plans one local action after product lowering and permission classification.
fn plan_local_action(
    turn: &(impl AgentTurnResultIdentity + ?Sized),
    action: &AgentAction,
    input: ActionPlanningInput<'_>,
) -> ActionPlanningResult<ActionResult> {
    let plan = input
        .local_plan
        .ok_or_else(|| ActionPlanningError::new("local action plan is required"))?;
    if let Some(message) = input.subagent_scope_violation {
        return ActionResult::failed(
            turn,
            action,
            ActionStatus::Denied,
            "subagent_scope_violation",
            message,
        )
        .map_err(ActionPlanningError::from_contract);
    }
    let decision = input
        .local_rule_decision
        .ok_or_else(|| ActionPlanningError::new("local action permission decision is required"))?;
    match decision {
        RuleDecision::Allow => Ok(ActionResult::running(
            turn,
            action,
            vec!["local action accepted for local dispatch".to_string()],
            Some(shell_action_structured_content_json(
                action,
                plan,
                Some("pending_local_dispatch"),
                false,
                serde_json::Value::Null,
                &[],
                serde_json::json!({"state":"pending_dispatch"}),
            )),
        )),
        RuleDecision::Prompt
            if input.approval_policy == ApprovalPolicy::AutoAllow
                && action_supports_auto_allow(action, input) =>
        {
            let reason = action_auto_allow_reason(action, input);
            Ok(ActionResult::running(
                turn,
                action,
                vec![
                    "local action auto-allowed by model assessment".to_string(),
                    reason,
                ],
                Some(shell_action_structured_content_json(
                    action,
                    plan,
                    Some("pending_local_dispatch"),
                    false,
                    auto_allow_approval_json(action, action.action_type(), input),
                    &[],
                    serde_json::json!({"state":"pending_dispatch"}),
                )),
            ))
        }
        RuleDecision::Prompt => Ok(ActionResult::blocked(
            turn,
            action,
            vec!["approval required before executing local action".to_string()],
            shell_action_structured_content_json(
                action,
                plan,
                Some("pending_local_dispatch"),
                false,
                serde_json::json!({"state":"pending","kind":action.action_type(),"action_id":action.id,"command":plan.policy_command}),
                &[],
                serde_json::json!({"state":"pending_approval"}),
            ),
        )),
        RuleDecision::Forbid => ActionResult::failed(
            turn,
            action,
            ActionStatus::Denied,
            "policy_forbidden",
            "local action denied by permission policy",
        )
        .map_err(ActionPlanningError::from_contract),
    }
}

/// Plans one network action after product permission classification.
fn plan_network_action(
    turn: &(impl AgentTurnResultIdentity + ?Sized),
    action: &AgentAction,
    input: ActionPlanningInput<'_>,
) -> ActionPlanningResult<ActionResult> {
    let plan = input
        .network_plan
        .ok_or_else(|| ActionPlanningError::new("network action plan is required"))?;
    let decision = input.network_rule_decision.ok_or_else(|| {
        ActionPlanningError::new("network action permission decision is required")
    })?;
    let (content, approval, response) = match decision {
        RuleDecision::Allow => (vec!["network action accepted for runtime execution".to_string()], serde_json::Value::Null, serde_json::json!({"state":"pending_runtime_network"})),
        RuleDecision::Prompt if input.approval_policy == ApprovalPolicy::AutoAllow && action_supports_auto_allow(action, input) => (
            vec!["network action auto-allowed by model assessment".to_string(), action_auto_allow_reason(action, input)],
            auto_allow_approval_json(action, action.action_type(), input), serde_json::json!({"state":"pending_runtime_network"})),
        RuleDecision::Prompt => return Ok(ActionResult::blocked(
            turn, action, vec!["approval required before executing network action".to_string()],
            network_action_structured_content_json(action, serde_json::json!({"state":"pending","kind":action.action_type(),"action_id":action.id,"policy_command":plan.policy_command}), serde_json::json!({"state":"pending_approval"}))
                .map_err(ActionPlanningError::from_network)?,
        )),
        RuleDecision::Forbid => return ActionResult::failed(turn, action, ActionStatus::Denied, "policy_forbidden", "network action denied by permission policy")
            .map_err(ActionPlanningError::from_contract),
    };
    Ok(ActionResult::running(
        turn,
        action,
        content,
        Some(
            network_action_structured_content_json(action, approval, response)
                .map_err(ActionPlanningError::from_network)?,
        ),
    ))
}

/// Plans one configuration mutation against approval policy facts.
fn plan_config_change(
    turn: &(impl AgentTurnResultIdentity + ?Sized),
    action: &AgentAction,
    setting_path: &str,
    operation: &str,
    input: ActionPlanningInput<'_>,
) -> ActionPlanningResult<ActionResult> {
    let policy_allowed = prompt_gate_satisfied_by_policy(input);
    let auto_allowed = !policy_allowed
        && input.approval_policy == ApprovalPolicy::AutoAllow
        && action_supports_auto_allow(action, input);
    if !policy_allowed && !auto_allowed {
        return Ok(ActionResult::blocked(turn, action, vec!["approval required before applying configuration change".to_string()],
            serde_json::json!({"approval":{"state":"pending","kind":"config_change","path":setting_path,"operation":operation,"required_command":"/approve"},"setting_path":setting_path,"operation":operation,"validation":{"status":"pending_primary_approval"},"applied_layer":serde_json::Value::Null,"persistence":{"requested":true,"completed":false,"scope":"user"}}).to_string()));
    }
    let approval = if auto_allowed {
        auto_allow_approval_json(action, "config_change", input)
    } else {
        policy_approval_json(action, "config_change", input)
    };
    Ok(ActionResult::running(turn, action, vec!["configuration change accepted for runtime application".to_string()], Some(
        serde_json::json!({"approval":approval,"setting_path":setting_path,"operation":operation,"validation":{"status":"pending_runtime_config_change"},"applied_layer":serde_json::Value::Null,"persistence":{"requested":true,"completed":false,"scope":"user"}}).to_string()
    )))
}

/// Plans one MCP call against tool and approval policy facts.
fn plan_mcp_call(
    turn: &(impl AgentTurnResultIdentity + ?Sized),
    action: &AgentAction,
    server: &str,
    tool: &str,
    arguments_json: &str,
    input: ActionPlanningInput<'_>,
) -> ActionPlanningResult<ActionResult> {
    let policy_allowed = input.mcp_approval_required && prompt_gate_satisfied_by_policy(input);
    let auto_allowed = input.mcp_approval_required
        && !policy_allowed
        && input.approval_policy == ApprovalPolicy::AutoAllow
        && action_supports_auto_allow(action, input);
    if input.mcp_approval_required && !policy_allowed && !auto_allowed {
        return Ok(ActionResult::blocked(turn, action, vec!["approval required before executing MCP tool call".to_string()],
            serde_json::json!({"approval":{"state":"pending","kind":"mcp_call","action_id":action.id,"server":server,"tool":tool}}).to_string()));
    }
    let content = if auto_allowed {
        vec![
            "mcp call auto-allowed by model assessment".to_string(),
            action_auto_allow_reason(action, input),
        ]
    } else if input.mcp_approval_required {
        vec!["mcp call accepted by approval policy".to_string()]
    } else {
        vec!["mcp call accepted for external-integration execution".to_string()]
    };
    let approval = if auto_allowed {
        auto_allow_approval_json(action, "mcp_call", input)
    } else if input.mcp_approval_required {
        policy_approval_json(action, "mcp_call", input)
    } else {
        serde_json::Value::Null
    };
    let arguments = serde_json::from_str::<serde_json::Value>(arguments_json)
        .map_err(|_| ActionPlanningError::new("validated MCP arguments must be JSON"))?;
    Ok(ActionResult::running(turn, action, content, Some(
        serde_json::json!({"server":server,"tool":tool,"arguments":arguments,"approval":approval}).to_string()
    )))
}

/// Builds the canonical structured payload for a visible say action.
pub fn say_action_structured_content_json(
    status: SayStatus,
    content_type: &str,
    text: &str,
) -> String {
    serde_json::json!({"kind":"say","status":status.as_str(),"content_type":content_type,"text":text}).to_string()
}

/// Builds canonical structured content for a lowered local action.
pub fn shell_action_structured_content_json(
    action: &AgentAction,
    plan: &LocalActionPlan,
    execution_transport: Option<&str>,
    sent_to_pane: bool,
    approval: serde_json::Value,
    matched_rules: &[String],
    terminal_observation: serde_json::Value,
) -> String {
    let generated_command_elided =
        !matches!(action.payload, AgentActionPayload::ShellCommand { .. });
    let command = if generated_command_elided {
        plan.policy_command.clone()
    } else {
        plan.command.clone()
    };
    serde_json::json!({
        "kind":action.action_type(),"summary":plan.summary,"command":command,
        "read_observations":shell_read_observations_for_command(&command),
        "generated_command_elided":generated_command_elided,
        "generated_command_bytes":if generated_command_elided { Some(plan.command.len()) } else { None },
        "execution_transport":execution_transport.unwrap_or("pane_shell"),"sent_to_pane":sent_to_pane,
        "stateful":plan.stateful,"approval":approval,"matched_rules":matched_rules,
        "terminal_observation":terminal_observation
    }).to_string()
}

/// Returns whether the action has a stable explanation for auto-allow metadata.
pub fn action_supports_auto_allow(action: &AgentAction, input: ActionPlanningInput<'_>) -> bool {
    !action_auto_allow_reason(action, input).trim().is_empty()
}

/// Returns the most concise available model-authored action explanation.
pub fn action_auto_allow_reason(action: &AgentAction, input: ActionPlanningInput<'_>) -> String {
    if !action.rationale.trim().is_empty() {
        return action.rationale.clone();
    }
    if let Some(plan) = input.local_plan
        && !plan.summary.trim().is_empty()
    {
        return plan.summary.clone();
    }
    if let Some(plan) = input.network_plan
        && !plan.summary.trim().is_empty()
    {
        return plan.summary.clone();
    }
    match &action.payload {
        AgentActionPayload::Say { text, .. } => text.clone(),
        AgentActionPayload::Abort { reason } => reason.clone(),
        AgentActionPayload::CallSkill { name, .. } => format!("load skill {name}"),
        AgentActionPayload::RequestSkills => "request available skills".to_string(),
        _ => String::new(),
    }
}

fn prompt_gate_satisfied_by_policy(input: ActionPlanningInput<'_>) -> bool {
    input.approval_bypass || input.approval_policy == ApprovalPolicy::FullAccess
}

fn policy_approval_json(
    action: &AgentAction,
    kind: &str,
    input: ActionPlanningInput<'_>,
) -> serde_json::Value {
    serde_json::json!({"state":if input.approval_bypass {"bypassed"} else {"full_access"},"kind":kind,"action_id":action.id})
}

fn auto_allow_approval_json(
    action: &AgentAction,
    kind: &str,
    input: ActionPlanningInput<'_>,
) -> serde_json::Value {
    serde_json::json!({"state":"auto_allowed","kind":kind,"action_id":action.id,"reason":action_auto_allow_reason(action, input)})
}

/// Result returned by canonical action-result planning.
pub type ActionPlanningResult<T> = Result<T, ActionPlanningError>;

/// Typed failure returned when product planning facts contradict the action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionPlanningError {
    message: String,
}

impl ActionPlanningError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
    fn from_contract(error: crate::ActionResultContractError) -> Self {
        Self::new(error.message())
    }
    fn from_network(error: crate::NetworkActionPlanError) -> Self {
        Self::new(error.message())
    }
    /// Returns the unformatted planning diagnostic.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ActionPlanningError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ActionPlanningError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentActionResultIdentity, ContextBlock, ContextSourceKind, LocalActionKind};

    struct TestTurn;
    impl AgentTurnResultIdentity for TestTurn {
        fn turn_id(&self) -> &str {
            "turn-1"
        }
        fn agent_id(&self) -> &str {
            "agent-1"
        }
    }

    fn request() -> ModelRequest {
        ModelRequest {
            provider: "test".to_string(),
            model: "test-model".to_string(),
            reasoning_effort: None,
            thinking_enabled: None,
            latency_preference: None,
            prompt_cache_retention: None,
            max_output_tokens: None,
            temperature: None,
            prompt_cache_session_id: None,
            prompt_cache_lineage_id: None,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            available_mcp_tools: Vec::new(),
            memory_actions_enabled: false,
            issue_actions_enabled: false,
            interaction_kind: crate::ModelInteractionKind::ActionExecution,
            allowed_actions: crate::AllowedActionSet::respond_only(),
            stop: None,
            messages: Vec::new(),
        }
    }

    fn response(action_batch: Option<MaapBatch>) -> ModelResponse {
        ModelResponse {
            provider: "test".to_string(),
            model: "test-model".to_string(),
            raw_text: "response".to_string(),
            usage: ModelTokenUsage::default(),
            latest_request_usage: None,
            quota_usage: Vec::new(),
            action_batch,
            provider_transcript_events: Vec::new(),
        }
    }

    fn shell_action(rationale: &str) -> AgentAction {
        AgentAction {
            id: "shell-1".to_string(),
            rationale: rationale.to_string(),
            payload: AgentActionPayload::ShellCommand {
                summary: "Inspect files".to_string(),
                command: "rg --files".to_string(),
                interactive: false,
                stateful: false,
                timeout_ms: None,
            },
        }
    }

    fn local_plan() -> LocalActionPlan {
        LocalActionPlan {
            kind: LocalActionKind::ShellCommand,
            summary: "Inspect files".to_string(),
            command: "rg --files".to_string(),
            policy_command: "rg --files".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
            display_output_after_completion: false,
        }
    }

    /// Builds a valid context with no prior action-result budget usage.
    fn context() -> AgentContext {
        AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            label: "user".to_string(),
            content: "complete the task".to_string(),
        }])
        .unwrap()
    }

    #[test]
    /// Verifies local permission decisions map to running, blocked, and denied
    /// canonical results while preserving the lower-owned pending-dispatch
    /// structured envelope.
    fn action_planning_maps_local_permission_decisions() {
        let action = shell_action("inspect repository files");
        let plan = local_plan();
        for (decision, status) in [
            (RuleDecision::Allow, ActionStatus::Running),
            (RuleDecision::Prompt, ActionStatus::Blocked),
            (RuleDecision::Forbid, ActionStatus::Denied),
        ] {
            let result = plan_action_result(
                &TestTurn,
                &action,
                ActionPlanningInput {
                    local_plan: Some(&plan),
                    local_rule_decision: Some(decision),
                    ..ActionPlanningInput::default()
                },
            )
            .unwrap();
            assert_eq!(result.status, status);
        }
    }

    #[test]
    /// Verifies auto-allow converts a prompting local action into running work
    /// and records the model-authored rationale in canonical approval metadata.
    fn action_planning_auto_allows_prompted_local_action_with_reason() {
        let action = shell_action("inspect repository files");
        let plan = local_plan();
        let result = plan_action_result(
            &TestTurn,
            &action,
            ActionPlanningInput {
                local_plan: Some(&plan),
                local_rule_decision: Some(RuleDecision::Prompt),
                approval_policy: ApprovalPolicy::AutoAllow,
                ..ActionPlanningInput::default()
            },
        )
        .unwrap();

        assert_eq!(result.status, ActionStatus::Running);
        let structured = result.structured_content_json.unwrap();
        assert!(
            structured.contains(r#""state":"auto_allowed""#),
            "{structured}"
        );
        assert!(
            structured.contains("inspect repository files"),
            "{structured}"
        );
    }

    #[test]
    /// Verifies product-computed subagent scope violations override permissive
    /// command policy and become denied results before local dispatch.
    fn action_planning_denies_local_subagent_scope_violation() {
        let action = shell_action("inspect repository files");
        let plan = local_plan();
        let result = plan_action_result(
            &TestTurn,
            &action,
            ActionPlanningInput {
                local_plan: Some(&plan),
                local_rule_decision: Some(RuleDecision::Allow),
                subagent_scope_violation: Some("path escapes delegated write scope"),
                ..ActionPlanningInput::default()
            },
        )
        .unwrap();

        assert_eq!(result.status, ActionStatus::Denied);
        assert_eq!(result.error.unwrap().code, "subagent_scope_violation");
    }

    #[test]
    /// Verifies configuration and MCP prompt gates block under ask policy but
    /// become running results when full-access policy supplies the required
    /// product authorization fact.
    fn action_planning_applies_config_and_mcp_prompt_policy() {
        let config = AgentAction {
            id: "config-1".to_string(),
            rationale: "set theme".to_string(),
            payload: AgentActionPayload::ConfigChange {
                setting_path: "ui.theme".to_string(),
                operation: "set".to_string(),
                value: Some(r#""default""#.to_string()),
            },
        };
        let mcp = AgentAction {
            id: "mcp-1".to_string(),
            rationale: "inspect issue".to_string(),
            payload: AgentActionPayload::McpCall {
                server: "gitlab".to_string(),
                tool: "get_issue".to_string(),
                arguments_json: r#"{"id":1}"#.to_string(),
            },
        };
        assert_eq!(
            plan_action_result(&TestTurn, &config, ActionPlanningInput::default())
                .unwrap()
                .status,
            ActionStatus::Blocked
        );
        assert_eq!(
            plan_action_result(&TestTurn, &mcp, ActionPlanningInput::default())
                .unwrap()
                .status,
            ActionStatus::Blocked
        );
        let full_access = ActionPlanningInput {
            approval_policy: ApprovalPolicy::FullAccess,
            mcp_approval_required: true,
            ..ActionPlanningInput::default()
        };
        assert_eq!(
            plan_action_result(&TestTurn, &config, full_access)
                .unwrap()
                .status,
            ActionStatus::Running
        );
        assert_eq!(
            plan_action_result(&TestTurn, &mcp, full_access)
                .unwrap()
                .status,
            ActionStatus::Running
        );
    }

    #[test]
    /// Verifies shell result shaping uses the product-supplied local plan while
    /// preserving command observation metadata and action identity.
    fn shell_action_structured_content_uses_lowered_plan() {
        let action = shell_action("");
        let plan = local_plan();
        let structured = shell_action_structured_content_json(
            &action,
            &plan,
            None,
            true,
            serde_json::Value::Null,
            &[],
            serde_json::json!({"exit_code":0}),
        );

        let value: serde_json::Value = serde_json::from_str(&structured).unwrap();
        assert_eq!(value["kind"], "shell_command");
        assert_eq!(value["command"], "rg --files");
        assert_eq!(value["sent_to_pane"], true);
        assert_eq!(action.action_id(), "shell-1");
    }

    /// Batch planning preserves provider action order and derives terminal
    /// completion from canonical results plus the final-turn declaration.
    #[test]
    fn batch_action_planning_derives_terminal_state() {
        let action = AgentAction {
            id: "say-1".to_string(),
            rationale: "finish the turn".to_string(),
            payload: AgentActionPayload::Say {
                status: SayStatus::Final,
                text: "Done.".to_string(),
                content_type: "text/plain; charset=utf-8".to_string(),
            },
        };
        let batch = MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "finish the requested work".to_string(),
            thought: None,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            actions: vec![action],
            final_turn: true,
        };

        let planned = plan_batch_action_results(&TestTurn, &context(), &batch, |action| {
            plan_action_result(&TestTurn, action, ActionPlanningInput::default())
        })
        .unwrap();

        assert_eq!(planned.action_results[0].action_id, "say-1");
        assert!(planned.final_turn);
        assert_eq!(planned.terminal_state, AgentTurnState::Completed);
    }

    /// Verifies validated batch projection constructs the canonical execution
    /// record with ordered results and lifecycle state in the lower crate.
    #[test]
    fn batch_execution_projection_owns_canonical_turn_state() {
        let action = AgentAction {
            id: "say-1".to_string(),
            rationale: "finish the turn".to_string(),
            payload: AgentActionPayload::Say {
                status: SayStatus::Final,
                text: "Done.".to_string(),
                content_type: "text/plain".to_string(),
            },
        };
        let batch = MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "finish".to_string(),
            thought: None,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            actions: vec![action],
            final_turn: true,
        };

        let execution = plan_turn_execution_from_batch(
            &TestTurn,
            &context(),
            request(),
            response(Some(batch.clone())),
            ModelTokenUsage::default(),
            &batch,
            |action| plan_action_result(&TestTurn, action, ActionPlanningInput::default()),
        )
        .unwrap();

        assert_eq!(execution.action_results[0].action_id, "say-1");
        assert!(execution.final_turn);
        assert_eq!(execution.terminal_state, AgentTurnState::Completed);
        assert!(execution.routing_token_usage_by_model.is_empty());
    }

    /// Verifies missing-batch failure projection is terminal, result-free, and
    /// preserves the durable request and latest response usage.
    #[test]
    fn missing_batch_execution_projection_is_terminal_failure() {
        let latest_usage = ModelTokenUsage {
            input_tokens: 12,
            output_tokens: 3,
            ..ModelTokenUsage::default()
        };
        let execution =
            failed_turn_execution_without_batch(request(), response(None), latest_usage);

        assert_eq!(execution.latest_response_usage, latest_usage);
        assert!(execution.action_results.is_empty());
        assert!(execution.final_turn);
        assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    }

    /// Batch planning applies memory wrapper guardrails before invoking the
    /// product planner so skipped placeholders cannot reach runtime dispatch.
    #[test]
    fn batch_action_planning_skips_memory_placeholders_before_product_planning() {
        let action = AgentAction {
            id: "memory-1".to_string(),
            rationale: "satisfy the required function call before proceeding".to_string(),
            payload: AgentActionPayload::MemorySearch {
                query: "placeholder".to_string(),
                limit: Some(1),
            },
        };
        let batch = MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "comply with the required function call".to_string(),
            thought: None,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            actions: vec![action],
            final_turn: false,
        };

        let planned = plan_batch_action_results(
            &TestTurn,
            &context(),
            &batch,
            |_| -> ActionPlanningResult<ActionResult> {
                panic!("memory placeholder must not reach product planning")
            },
        )
        .unwrap();

        assert_eq!(planned.action_results[0].status, ActionStatus::Succeeded);
        assert!(
            planned.action_results[0]
                .structured_content_json
                .as_deref()
                .unwrap()
                .contains("memory_wrapper_placeholder")
        );
    }
}
