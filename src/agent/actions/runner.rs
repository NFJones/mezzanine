//! Agent turn runner.
//!
//! This module owns provider negotiation for one agent turn. It keeps MAAP
//! repair loops, capability negotiation, provider failure summarization, and
//! initial action planning together while leaving shell/MCP execution and
//! transcript persistence to sibling modules.

use super::super::AsyncModelProvider;
#[cfg(test)]
use super::super::ModelProvider;
use super::super::{
    ActionResult, AgentAction, AgentActionPayload, AgentContext, AgentTurnLedger, AgentTurnRecord,
    AgentTurnState, AllowedAction, AllowedActionSet, ContextSourceKind, McpPromptTool, MezError,
    ModelInteractionKind, ModelProfile, ModelRequest, ModelTokenUsage, PathScopes,
    PermissionPolicy, Result, SessionApprovalStore, assemble_model_request,
};
#[cfg(test)]
use super::super::{ActionStatus, local_action_plan};
#[cfg(test)]
use super::super::{MarkerToken, McpToolCallPlan, Path};
#[cfg(test)]
use super::execution::{
    LocalActionExecutor, McpActionExecutor, PaneShellExecutor, PaneShellLocalExecutor,
    execute_local_action, execute_mcp_action_through_runtime,
};
use super::recovery::{
    FailureSummaryInput, FailureSummaryScope, capability_continuation_request,
    capability_requests_from_batch, failed_maap_validation_execution_with_summary_async,
    maap_provider_error_is_repairable, maap_repair_request, mixed_capability_continuation_request,
    provider_error_should_retry_without_summary, summarize_controller_failure_execution_async,
    summarize_provider_failure_execution_async, validate_batch_allowed_actions,
};
#[cfg(test)]
use super::recovery::{
    failed_maap_validation_execution_with_summary, summarize_controller_failure_execution,
    summarize_provider_failure_execution,
};
use super::{AgentTurnExecution, turn_state_from_action_results};
use crate::subagent::SubagentScopeDeclaration;

/// Maximum number of ephemeral provider retries after a MAAP validation error.
///
/// The retry instruction is appended only to a cloned request and is never
/// returned in `AgentTurnExecution.request`, keeping repair diagnostics out of
/// durable transcripts and future model context when the corrected response is
/// valid.
const MAAP_REPAIR_ATTEMPT_LIMIT: usize = 2;

/// Maximum memory searches accepted during one user turn.
///
/// Memory is durable prior context, not a route-discovery fallback. Keeping the
/// runtime cap small gives the model room for one focused search plus one
/// exceptional follow-up while preventing paraphrase loops.
const MEMORY_SEARCH_ACTION_LIMIT_PER_TURN: usize = 2;

/// Maximum memory stores accepted during one user turn.
///
/// Storing multiple records from a single work loop is usually a sign that the
/// model is persisting transient task state instead of saving a durable
/// reusable fact.
const MEMORY_STORE_ACTION_LIMIT_PER_TURN: usize = 1;

#[derive(Debug, Clone, Copy, Default)]
struct MemoryActionBudget {
    /// Number of memory search actions already accepted or observed in the
    /// active turn context.
    search_count: usize,
    /// Number of memory store actions already accepted or observed in the
    /// active turn context.
    store_count: usize,
}

impl MemoryActionBudget {
    /// Builds the memory action budget from current model context.
    ///
    /// Only action-result blocks are counted because they represent memory
    /// actions that have already been surfaced to the model during this active
    /// work loop.
    fn from_context(context: &AgentContext) -> Self {
        let mut budget = Self::default();
        for block in &context.blocks {
            if block.source != ContextSourceKind::ActionResult {
                continue;
            }
            if action_result_block_has_action_type(&block.content, "memory_search") {
                budget.search_count = budget.search_count.saturating_add(1);
            }
            if action_result_block_has_action_type(&block.content, "memory_store") {
                budget.store_count = budget.store_count.saturating_add(1);
            }
        }
        budget
    }

    /// Accepts a memory action against the turn budget or returns a skipped result.
    ///
    /// Non-memory actions do not consume this budget. Over-budget memory
    /// actions become successful skipped results so unrelated useful actions in
    /// the same batch are not blocked by the guardrail.
    fn accept_or_skip(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        batch_rationale: &str,
        batch_thought: Option<&str>,
    ) -> Option<ActionResult> {
        if memory_action_is_wrapper_placeholder(action, batch_rationale, batch_thought) {
            return Some(memory_budget_skip_result(
                turn,
                action,
                "memory_wrapper_placeholder",
                "memory action skipped: rationale identified this as action-wrapper compliance rather than a concrete durable-context need; continue with the direct task action instead",
                0,
            ));
        }
        match &action.payload {
            AgentActionPayload::MemorySearch { .. } => {
                if self.search_count >= MEMORY_SEARCH_ACTION_LIMIT_PER_TURN {
                    return Some(memory_budget_skip_result(
                        turn,
                        action,
                        "memory_search_turn_limit",
                        "memory_search skipped: per-turn memory search limit reached; continue the task with direct artifacts, current action results, MCP, shell, web, or a bounded report instead, and do not search memory again this turn",
                        MEMORY_SEARCH_ACTION_LIMIT_PER_TURN,
                    ));
                }
                self.search_count = self.search_count.saturating_add(1);
                None
            }
            AgentActionPayload::MemoryStore { .. } => {
                if self.store_count >= MEMORY_STORE_ACTION_LIMIT_PER_TURN {
                    return Some(memory_budget_skip_result(
                        turn,
                        action,
                        "memory_store_turn_limit",
                        "memory_store skipped: per-turn memory store limit reached; do not persist transient task state",
                        MEMORY_STORE_ACTION_LIMIT_PER_TURN,
                    ));
                }
                self.store_count = self.store_count.saturating_add(1);
                None
            }
            _ => None,
        }
    }
}

/// Reports whether a model-context action-result block has the supplied action type.
fn action_result_block_has_action_type(content: &str, action_type: &str) -> bool {
    let Some(header) = content.lines().next() else {
        return false;
    };
    header.starts_with("[action_result ") && header.split_whitespace().nth(2) == Some(action_type)
}

/// Builds the structured skipped result returned for an over-budget memory action.
fn memory_budget_skip_result(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    code: &str,
    message: &str,
    limit: usize,
) -> ActionResult {
    ActionResult::succeeded(
        turn,
        action,
        vec![message.to_string()],
        Some(
            serde_json::json!({
                "state": "skipped_runtime_memory_guardrail",
                "code": code,
                "limit": limit,
                "message": message,
            })
            .to_string(),
        ),
    )
}

/// Exposes persistent-memory actions on the main model action surface when enabled.
fn expose_default_memory_actions(request: &mut ModelRequest, memory_actions_enabled: bool) {
    request.memory_actions_enabled = memory_actions_enabled;
    if !memory_actions_enabled {
        return;
    }
    request
        .allowed_actions
        .extend([AllowedAction::MemorySearch, AllowedAction::MemoryStore]);
}

/// Reports whether a memory action is only satisfying the function/action wrapper.
///
/// The runtime should keep memory available when it is the right tool, but
/// should not let a model convert wrapper-compliance confusion into a durable
/// memory lookup or store. The match requires both wrapper terminology and
/// compliance/setup language so ordinary memory searches about prompt behavior
/// are not rejected merely for mentioning a function call.
fn memory_action_is_wrapper_placeholder(
    action: &AgentAction,
    batch_rationale: &str,
    batch_thought: Option<&str>,
) -> bool {
    if !matches!(
        action.payload,
        AgentActionPayload::MemorySearch { .. } | AgentActionPayload::MemoryStore { .. }
    ) {
        return false;
    }
    let mut text = String::new();
    text.push_str(batch_rationale);
    text.push('\n');
    if let Some(batch_thought) = batch_thought {
        text.push_str(batch_thought);
        text.push('\n');
    }
    text.push_str(&action.rationale);
    memory_placeholder_text_mentions_wrapper_compliance(&text)
}

/// Reports whether text frames an action as wrapper compliance instead of work.
fn memory_placeholder_text_mentions_wrapper_compliance(text: &str) -> bool {
    let normalized = normalize_memory_placeholder_text(text);
    let mentions_wrapper = [
        "required function call",
        "required tool call",
        "required current actions call",
        "required current action call",
        "current actions call",
        "schema wrapper",
        "action wrapper",
        "function call requirement",
        "schema valid batch",
        "action batch envelope",
        "transport envelope",
    ]
    .iter()
    .any(|phrase| normalized.contains(phrase));
    if !mentions_wrapper {
        return false;
    }
    [
        "comply",
        "complying",
        "satisfy",
        "satisfying",
        "satisfies",
        "placeholder",
        "prerequisite",
        "before proceeding",
        "required immediate",
        "schema valid batch is needed",
        "initial batch is needed",
    ]
    .iter()
    .any(|phrase| normalized.contains(phrase))
}

/// Normalizes model-authored rationale text for guardrail phrase matching.
fn normalize_memory_placeholder_text(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut previous_space = true;
    for character in text.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            output.push(character);
            previous_space = false;
        } else if !previous_space {
            output.push(' ');
            previous_space = true;
        }
    }
    output.trim().to_string()
}

/// Exposes MCP tool calls on the main model action surface when tools are available.
fn expose_default_mcp_actions(request: &mut ModelRequest, available_mcp_tools: &[McpPromptTool]) {
    request.available_mcp_tools = available_mcp_tools.to_vec();
    if available_mcp_tools.is_empty() {
        return;
    }
    request.allowed_actions.extend([AllowedAction::McpCall]);
}

/// Carries the live issue-tracking action gate into provider requests.
fn expose_issue_actions_gate(request: &mut ModelRequest, issue_actions_enabled: bool) {
    request.issue_actions_enabled = issue_actions_enabled;
}

/// Applies runtime-owned default action gates to a model request.
///
/// The main selected-model surface starts as a capability-decision request and
/// is then widened with concrete MCP, memory, and issue-tracking availability
/// owned by the runtime. Keeping this mutation in one helper prevents provider
/// diagnostics from drifting away from the live runner surface.
pub(crate) fn apply_default_action_gates(
    request: &mut ModelRequest,
    available_mcp_tools: &[McpPromptTool],
    memory_actions_enabled: bool,
    issue_actions_enabled: bool,
) {
    expose_default_mcp_actions(request, available_mcp_tools);
    expose_default_memory_actions(request, memory_actions_enabled);
    expose_issue_actions_gate(request, issue_actions_enabled);
}

/// Carries agent turn runner state for this subsystem.
///
/// The fields are kept explicit so callers can inspect and move structured
/// runtime data without parsing display text.
pub struct AgentTurnRunner<'a, P> {
    /// Structured `provider` value carried by this API type.
    pub provider: &'a P,
    /// Structured `model_profile` value carried by this API type.
    pub model_profile: ModelProfile,
    /// Structured `permissions` value carried by this API type.
    pub permissions: &'a PermissionPolicy,
    /// Structured `approvals` value carried by this API type.
    pub approvals: &'a SessionApprovalStore,
    /// Structured `path_scopes` value carried by this API type.
    pub path_scopes: Option<&'a PathScopes>,
    /// Structured `subagent_scope` value carried by this API type.
    pub subagent_scope: Option<&'a SubagentScopeDeclaration>,
    /// Structured `available_mcp_servers` value carried by this API type.
    pub available_mcp_servers: Vec<String>,
    /// Structured `available_mcp_tools` value carried by this API type.
    pub available_mcp_tools: &'a [McpPromptTool],
    /// Whether persistent-memory MAAP actions may be exposed for this turn.
    pub memory_actions_enabled: bool,
    /// Whether local issue-tracking MAAP actions may be exposed for this turn.
    pub issue_actions_enabled: bool,
}

#[cfg(test)]
impl<'a, P: ModelProvider> AgentTurnRunner<'a, P> {
    /// Executes the `run_turn` operation for the owning subsystem.
    ///
    /// Callers receive a typed result or error with context from the underlying
    /// runtime operation.
    pub fn run_turn(
        &self,
        ledger: &mut AgentTurnLedger,
        turn: AgentTurnRecord,
        context: AgentContext,
    ) -> Result<AgentTurnExecution> {
        self.run_turn_ref(ledger, turn, &context)
    }

    /// Executes the `run_turn` operation with a borrowed context.
    ///
    /// Callers that keep large active-turn contexts in runtime storage can
    /// avoid cloning those blocks before request assembly.
    pub fn run_turn_ref(
        &self,
        ledger: &mut AgentTurnLedger,
        turn: AgentTurnRecord,
        context: &AgentContext,
    ) -> Result<AgentTurnExecution> {
        self.run_turn_ref_with_allowed_actions(ledger, turn, context, None)
    }

    /// Executes a borrowed-context turn with an optional controller-selected
    /// initial action surface.
    #[cfg(test)]
    pub fn run_turn_ref_with_allowed_actions(
        &self,
        ledger: &mut AgentTurnLedger,
        turn: AgentTurnRecord,
        context: &AgentContext,
        allowed_actions: Option<AllowedActionSet>,
    ) -> Result<AgentTurnExecution> {
        ledger.start_turn(turn.clone())?;
        let mut request = assemble_model_request(&self.model_profile, &turn, context)?;
        if let Some(allowed_actions) = allowed_actions {
            request.interaction_kind = ModelInteractionKind::ActionExecution;
            request.allowed_actions = allowed_actions;
        }
        apply_default_action_gates(
            &mut request,
            self.available_mcp_tools,
            self.memory_actions_enabled,
            self.issue_actions_enabled,
        );
        let mut repair_attempts = 0usize;
        let mut response_request: ModelRequest;
        let mut durable_response_request = request.clone();
        let mut cumulative_usage = ModelTokenUsage::default();
        let mut latest_response_usage;
        let mut latest_quota_usage = Vec::new();
        let mut response = loop {
            response_request = request.clone();
            let response = match self.provider.send_request(&request) {
                Ok(response) => response,
                Err(error)
                    if repair_attempts < MAAP_REPAIR_ATTEMPT_LIMIT
                        && maap_provider_error_is_repairable(&error) =>
                {
                    repair_attempts = repair_attempts.saturating_add(1);
                    request = maap_repair_request(
                        &response_request,
                        error.message(),
                        error.provider_raw_text().unwrap_or(""),
                        repair_attempts,
                    );
                    continue;
                }
                Err(error) => {
                    if provider_error_should_retry_without_summary(&error) {
                        return Err(error);
                    }
                    if let Some(execution) = summarize_provider_failure_execution(
                        self.provider,
                        &turn,
                        &response_request,
                        &error,
                    ) {
                        ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                        return Ok(execution);
                    }
                    ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                    return Err(error);
                }
            };
            latest_response_usage = response.latest_request_usage.unwrap_or(response.usage);
            cumulative_usage.add_assign(latest_response_usage);
            if !response.quota_usage.is_empty() {
                latest_quota_usage = response.quota_usage.clone();
            }
            if response.provider != self.provider.provider_id() {
                let error = MezError::invalid_state(
                    "model provider response identity does not match the selected provider",
                );
                if let Some(execution) = summarize_controller_failure_execution(
                    self.provider,
                    &turn,
                    &response_request,
                    FailureSummaryInput {
                        failed_response: response.clone(),
                        error: &error,
                        scope: FailureSummaryScope {
                            stage: "provider_identity",
                            available_mcp_servers: &self.available_mcp_servers,
                            available_mcp_tools: self.available_mcp_tools,
                        },
                    },
                ) {
                    ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                    return Ok(execution);
                }
                ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                return Err(error);
            }
            if response_request.interaction_kind != ModelInteractionKind::Repair {
                durable_response_request = response_request.clone();
            }
            let Some(batch) = &response.action_batch else {
                break response;
            };
            if let Some(next_request) =
                mixed_capability_continuation_request(&response_request, batch)
            {
                request = next_request;
                repair_attempts = 0;
                continue;
            }
            if let Err(error) = validate_batch_allowed_actions(batch, &request) {
                if repair_attempts < MAAP_REPAIR_ATTEMPT_LIMIT {
                    repair_attempts = repair_attempts.saturating_add(1);
                    request = maap_repair_request(
                        &response_request,
                        error.message(),
                        &response.raw_text,
                        repair_attempts,
                    );
                    continue;
                }
                ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                let mut response = response;
                response.usage = cumulative_usage;
                response.quota_usage = latest_quota_usage;
                return Ok(failed_maap_validation_execution_with_summary(
                    self.provider,
                    &turn,
                    durable_response_request,
                    response,
                    latest_response_usage,
                    &error,
                    FailureSummaryScope {
                        stage: "allowed_actions",
                        available_mcp_servers: &self.available_mcp_servers,
                        available_mcp_tools: self.available_mcp_tools,
                    },
                ));
            }
            if let Err(error) =
                batch.validate(&turn, &self.available_mcp_servers, self.available_mcp_tools)
            {
                if repair_attempts < MAAP_REPAIR_ATTEMPT_LIMIT {
                    repair_attempts = repair_attempts.saturating_add(1);
                    request = maap_repair_request(
                        &response_request,
                        error.message(),
                        &response.raw_text,
                        repair_attempts,
                    );
                    continue;
                }
                ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                let mut response = response;
                response.usage = cumulative_usage;
                response.quota_usage = latest_quota_usage;
                return Ok(failed_maap_validation_execution_with_summary(
                    self.provider,
                    &turn,
                    durable_response_request,
                    response,
                    latest_response_usage,
                    &error,
                    FailureSummaryScope {
                        stage: "maap_validation",
                        available_mcp_servers: &self.available_mcp_servers,
                        available_mcp_tools: self.available_mcp_tools,
                    },
                ));
            }
            let capability_request = match capability_requests_from_batch(batch) {
                Ok(capability_request) => capability_request,
                Err(error) => {
                    if repair_attempts < MAAP_REPAIR_ATTEMPT_LIMIT {
                        repair_attempts = repair_attempts.saturating_add(1);
                        request = maap_repair_request(
                            &response_request,
                            error.message(),
                            &response.raw_text,
                            repair_attempts,
                        );
                        continue;
                    }
                    ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                    let mut response = response;
                    response.usage = cumulative_usage;
                    response.quota_usage = latest_quota_usage;
                    return Ok(failed_maap_validation_execution_with_summary(
                        self.provider,
                        &turn,
                        durable_response_request,
                        response,
                        latest_response_usage,
                        &error,
                        FailureSummaryScope {
                            stage: "capability_negotiation",
                            available_mcp_servers: &self.available_mcp_servers,
                            available_mcp_tools: self.available_mcp_tools,
                        },
                    ));
                }
            };
            if let Some(capability_request) = capability_request {
                request = capability_continuation_request(&response_request, &capability_request);
                repair_attempts = 0;
                continue;
            }
            break response;
        };
        response.usage = cumulative_usage;
        response.quota_usage = latest_quota_usage;

        let Some(batch) = &response.action_batch else {
            ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
            return Ok(AgentTurnExecution {
                request: durable_response_request,
                response,
                latest_response_usage,
                routing_token_usage_by_model: std::collections::BTreeMap::new(),
                action_results: Vec::new(),
                final_turn: true,
                terminal_state: AgentTurnState::Failed,
            });
        };

        let final_turn = batch.final_turn;
        let mut action_results = Vec::with_capacity(batch.actions.len());
        let mut memory_budget = MemoryActionBudget::from_context(context);
        for action in &batch.actions {
            if let Some(result) = memory_budget.accept_or_skip(
                &turn,
                action,
                &batch.rationale,
                batch.thought.as_deref(),
            ) {
                action_results.push(result);
                continue;
            }
            action_results.push(self.plan_action_result(&turn, action)?);
        }
        let terminal_state = turn_state_from_action_results(&action_results, final_turn);
        if terminal_state != AgentTurnState::Running {
            ledger.finish_turn(&turn.turn_id, terminal_state)?;
        }

        Ok(AgentTurnExecution {
            request: durable_response_request,
            response,
            latest_response_usage,
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results,
            final_turn,
            terminal_state,
        })
    }

    /// Executes the `run_turn_with_shell_executor` operation for the owning subsystem.
    ///
    /// Callers receive a typed result or error with context from the underlying
    /// runtime operation.
    pub fn run_turn_with_shell_executor<M>(
        &self,
        ledger: &mut AgentTurnLedger,
        turn: AgentTurnRecord,
        context: AgentContext,
        shell_path: &Path,
        executor: &mut impl PaneShellExecutor,
        marker_for_action: M,
    ) -> Result<AgentTurnExecution>
    where
        M: FnMut(&AgentAction) -> Result<MarkerToken>,
    {
        let mut local_executor = PaneShellLocalExecutor::new(shell_path, executor);
        self.run_turn_with_local_executor(
            ledger,
            turn,
            context,
            &mut local_executor,
            marker_for_action,
        )
    }

    /// Executes local actions through a transport-neutral executor.
    ///
    /// Callers receive a typed result or error with context from the underlying
    /// runtime operation.
    pub fn run_turn_with_local_executor<M>(
        &self,
        ledger: &mut AgentTurnLedger,
        turn: AgentTurnRecord,
        context: AgentContext,
        executor: &mut impl LocalActionExecutor,
        mut marker_for_action: M,
    ) -> Result<AgentTurnExecution>
    where
        M: FnMut(&AgentAction) -> Result<MarkerToken>,
    {
        let mut execution = self.run_turn(ledger, turn.clone(), context)?;
        if execution.terminal_state != AgentTurnState::Running {
            return Ok(execution);
        }

        let Some(batch) = &execution.response.action_batch else {
            return Ok(execution);
        };
        for result in &mut execution.action_results {
            if result.status != ActionStatus::Running {
                continue;
            }
            let action = batch
                .actions
                .iter()
                .find(|action| action.id == result.action_id)
                .ok_or_else(|| {
                    MezError::invalid_state("running shell result does not match an action")
                })?;
            if local_action_plan(action)?.is_none() {
                continue;
            }
            let marker = marker_for_action(action)?;
            *result = execute_local_action(&turn, action, marker, executor)?;
        }

        execution.terminal_state =
            turn_state_from_action_results(&execution.action_results, execution.final_turn);
        if execution.terminal_state != AgentTurnState::Running {
            ledger.finish_turn(&turn.turn_id, execution.terminal_state)?;
        }
        Ok(execution)
    }

    /// Executes the `run_turn_with_mcp_executor` operation for the owning subsystem.
    ///
    /// Callers receive a typed result or error with context from the underlying
    /// runtime operation.
    pub fn run_turn_with_mcp_executor<F>(
        &self,
        ledger: &mut AgentTurnLedger,
        turn: AgentTurnRecord,
        context: AgentContext,
        executor: &mut impl McpActionExecutor,
        mut plan_for_action: F,
    ) -> Result<AgentTurnExecution>
    where
        F: FnMut(&AgentAction) -> Result<McpToolCallPlan>,
    {
        let mut execution = self.run_turn(ledger, turn.clone(), context)?;
        if execution.terminal_state != AgentTurnState::Running {
            return Ok(execution);
        }

        let Some(batch) = &execution.response.action_batch else {
            return Ok(execution);
        };
        for result in &mut execution.action_results {
            if result.status != ActionStatus::Running || result.action_type != "mcp_call" {
                continue;
            }
            let action = batch
                .actions
                .iter()
                .find(|action| action.id == result.action_id)
                .ok_or_else(|| {
                    MezError::invalid_state("running MCP result does not match an action")
                })?;
            let plan = plan_for_action(action)?;
            *result = execute_mcp_action_through_runtime(&turn, action, &plan, executor)?;
        }

        execution.terminal_state =
            turn_state_from_action_results(&execution.action_results, execution.final_turn);
        if execution.terminal_state != AgentTurnState::Running {
            ledger.finish_turn(&turn.turn_id, execution.terminal_state)?;
        }
        Ok(execution)
    }
}

impl<'a, P: AsyncModelProvider> AgentTurnRunner<'a, P> {
    /// Executes the `run_turn_async` operation for the owning subsystem.
    ///
    /// Callers receive a typed result or error with context from the underlying
    /// runtime operation.
    pub async fn run_turn_async(
        &self,
        ledger: &mut AgentTurnLedger,
        turn: AgentTurnRecord,
        context: AgentContext,
    ) -> Result<AgentTurnExecution> {
        self.run_turn_async_ref(ledger, turn, &context).await
    }

    /// Executes the `run_turn_async` operation with a borrowed context.
    ///
    /// Provider workers use this entry point so queued dispatches do not clone
    /// large prompt contexts again immediately before request assembly.
    pub async fn run_turn_async_ref(
        &self,
        ledger: &mut AgentTurnLedger,
        turn: AgentTurnRecord,
        context: &AgentContext,
    ) -> Result<AgentTurnExecution> {
        self.run_turn_async_ref_with_allowed_actions(ledger, turn, context, None)
            .await
    }

    /// Executes a borrowed-context async turn with an optional
    /// controller-selected initial action surface.
    pub async fn run_turn_async_ref_with_allowed_actions(
        &self,
        ledger: &mut AgentTurnLedger,
        turn: AgentTurnRecord,
        context: &AgentContext,
        allowed_actions: Option<AllowedActionSet>,
    ) -> Result<AgentTurnExecution> {
        ledger.start_turn(turn.clone())?;
        let mut request = assemble_model_request(&self.model_profile, &turn, context)?;
        if let Some(allowed_actions) = allowed_actions {
            request.interaction_kind = ModelInteractionKind::ActionExecution;
            request.allowed_actions = allowed_actions;
        }
        apply_default_action_gates(
            &mut request,
            self.available_mcp_tools,
            self.memory_actions_enabled,
            self.issue_actions_enabled,
        );
        let mut repair_attempts = 0usize;
        let mut response_request: ModelRequest;
        let mut durable_response_request = request.clone();
        let mut cumulative_usage = ModelTokenUsage::default();
        let mut latest_response_usage;
        let mut latest_quota_usage = Vec::new();
        let mut response = loop {
            response_request = request.clone();
            let response = match self.provider.send_request_async(&request).await {
                Ok(response) => response,
                Err(error)
                    if repair_attempts < MAAP_REPAIR_ATTEMPT_LIMIT
                        && maap_provider_error_is_repairable(&error) =>
                {
                    repair_attempts = repair_attempts.saturating_add(1);
                    request = maap_repair_request(
                        &response_request,
                        error.message(),
                        error.provider_raw_text().unwrap_or(""),
                        repair_attempts,
                    );
                    continue;
                }
                Err(error) => {
                    if provider_error_should_retry_without_summary(&error) {
                        return Err(error);
                    }
                    if let Some(execution) = summarize_provider_failure_execution_async(
                        self.provider,
                        &turn,
                        &response_request,
                        &error,
                    )
                    .await
                    {
                        ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                        return Ok(execution);
                    }
                    ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                    return Err(error);
                }
            };
            latest_response_usage = response.latest_request_usage.unwrap_or(response.usage);
            cumulative_usage.add_assign(latest_response_usage);
            if !response.quota_usage.is_empty() {
                latest_quota_usage = response.quota_usage.clone();
            }
            if response.provider != self.provider.provider_id() {
                let error = MezError::invalid_state(
                    "model provider response identity does not match the selected provider",
                );
                if let Some(execution) = summarize_controller_failure_execution_async(
                    self.provider,
                    &turn,
                    &response_request,
                    FailureSummaryInput {
                        failed_response: response.clone(),
                        error: &error,
                        scope: FailureSummaryScope {
                            stage: "provider_identity",
                            available_mcp_servers: &self.available_mcp_servers,
                            available_mcp_tools: self.available_mcp_tools,
                        },
                    },
                )
                .await
                {
                    ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                    return Ok(execution);
                }
                ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                return Err(error);
            }
            if response_request.interaction_kind != ModelInteractionKind::Repair {
                durable_response_request = response_request.clone();
            }
            let Some(batch) = &response.action_batch else {
                break response;
            };
            if let Some(next_request) =
                mixed_capability_continuation_request(&response_request, batch)
            {
                request = next_request;
                repair_attempts = 0;
                continue;
            }
            if let Err(error) = validate_batch_allowed_actions(batch, &request) {
                if repair_attempts < MAAP_REPAIR_ATTEMPT_LIMIT {
                    repair_attempts = repair_attempts.saturating_add(1);
                    request = maap_repair_request(
                        &response_request,
                        error.message(),
                        &response.raw_text,
                        repair_attempts,
                    );
                    continue;
                }
                ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                let mut response = response;
                response.usage = cumulative_usage;
                response.quota_usage = latest_quota_usage;
                return Ok(failed_maap_validation_execution_with_summary_async(
                    self.provider,
                    &turn,
                    durable_response_request,
                    response,
                    latest_response_usage,
                    &error,
                    FailureSummaryScope {
                        stage: "allowed_actions",
                        available_mcp_servers: &self.available_mcp_servers,
                        available_mcp_tools: self.available_mcp_tools,
                    },
                )
                .await);
            }
            if let Err(error) =
                batch.validate(&turn, &self.available_mcp_servers, self.available_mcp_tools)
            {
                if repair_attempts < MAAP_REPAIR_ATTEMPT_LIMIT {
                    repair_attempts = repair_attempts.saturating_add(1);
                    request = maap_repair_request(
                        &response_request,
                        error.message(),
                        &response.raw_text,
                        repair_attempts,
                    );
                    continue;
                }
                ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                let mut response = response;
                response.usage = cumulative_usage;
                response.quota_usage = latest_quota_usage;
                return Ok(failed_maap_validation_execution_with_summary_async(
                    self.provider,
                    &turn,
                    durable_response_request,
                    response,
                    latest_response_usage,
                    &error,
                    FailureSummaryScope {
                        stage: "maap_validation",
                        available_mcp_servers: &self.available_mcp_servers,
                        available_mcp_tools: self.available_mcp_tools,
                    },
                )
                .await);
            }
            let capability_request = match capability_requests_from_batch(batch) {
                Ok(capability_request) => capability_request,
                Err(error) => {
                    if repair_attempts < MAAP_REPAIR_ATTEMPT_LIMIT {
                        repair_attempts = repair_attempts.saturating_add(1);
                        request = maap_repair_request(
                            &response_request,
                            error.message(),
                            &response.raw_text,
                            repair_attempts,
                        );
                        continue;
                    }
                    ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                    let mut response = response;
                    response.usage = cumulative_usage;
                    response.quota_usage = latest_quota_usage;
                    return Ok(failed_maap_validation_execution_with_summary_async(
                        self.provider,
                        &turn,
                        durable_response_request,
                        response,
                        latest_response_usage,
                        &error,
                        FailureSummaryScope {
                            stage: "capability_negotiation",
                            available_mcp_servers: &self.available_mcp_servers,
                            available_mcp_tools: self.available_mcp_tools,
                        },
                    )
                    .await);
                }
            };
            if let Some(capability_request) = capability_request {
                request = capability_continuation_request(&response_request, &capability_request);
                repair_attempts = 0;
                continue;
            }
            break response;
        };
        response.usage = cumulative_usage;
        response.quota_usage = latest_quota_usage;

        let Some(batch) = &response.action_batch else {
            ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
            return Ok(AgentTurnExecution {
                request: durable_response_request,
                response,
                latest_response_usage,
                routing_token_usage_by_model: std::collections::BTreeMap::new(),
                action_results: Vec::new(),
                final_turn: true,
                terminal_state: AgentTurnState::Failed,
            });
        };

        let final_turn = batch.final_turn;
        let mut action_results = Vec::with_capacity(batch.actions.len());
        let mut memory_budget = MemoryActionBudget::from_context(context);
        for action in &batch.actions {
            if let Some(result) = memory_budget.accept_or_skip(
                &turn,
                action,
                &batch.rationale,
                batch.thought.as_deref(),
            ) {
                action_results.push(result);
                continue;
            }
            action_results.push(self.plan_action_result(&turn, action)?);
        }
        let terminal_state = turn_state_from_action_results(&action_results, final_turn);
        if terminal_state != AgentTurnState::Running {
            ledger.finish_turn(&turn.turn_id, terminal_state)?;
        }

        Ok(AgentTurnExecution {
            request: durable_response_request,
            response,
            latest_response_usage,
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results,
            final_turn,
            terminal_state,
        })
    }
}
