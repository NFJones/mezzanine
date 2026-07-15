//! Agent turn runner.
//!
//! This module owns provider negotiation for one agent turn. It keeps MAAP
//! repair loops, capability negotiation, provider failure summarization, and
//! initial action planning together while leaving shell/MCP execution and
//! transcript persistence to sibling modules.

use super::super::AsyncModelProvider;
#[cfg(test)]
use super::super::{ActionStatus, AgentAction, local_action_plan};
use super::super::{
    AgentContext, AgentTurnLedger, AgentTurnRecord, AgentTurnState, AllowedActionSet,
    McpPromptTool, MezError, ModelInteractionKind, ModelProfile, ModelRequest, Result,
    assemble_model_request, provider_error_retry_class,
};
#[cfg(test)]
use super::super::{MarkerToken, McpExecutionRequest, Path};
#[cfg(test)]
use super::execution::{
    LocalActionExecutor, McpActionExecutor, PaneShellExecutor, PaneShellLocalExecutor,
    execute_local_action, execute_mcp_action_through_runtime,
};
use super::recovery::{
    FailureSummaryInput, FailureSummaryScope, failed_maap_validation_execution_with_summary_async,
    maap_provider_error_is_repairable, summarize_controller_failure_execution_async,
    summarize_provider_failure_execution_async,
};
#[cfg(test)]
use super::recovery::{
    failed_maap_validation_execution_with_summary, summarize_controller_failure_execution,
    summarize_provider_failure_execution,
};
use crate::agent::maap::MaapBatchProductValidation;
#[cfg(test)]
use crate::agent::provider::ModelProvider;
#[cfg(test)]
use mez_agent::turn_state_from_action_results;
use mez_agent::{
    AgentTurnExecution, AgentTurnNegotiation, AgentTurnProviderFailureDecision,
    AgentTurnResponseDecision, BatchContinuationError, BatchContinuationInput,
    BatchContinuationPlan, BatchValidationFailure, ProviderResponseAcceptance,
    SubagentScopeDeclaration, apply_default_action_gates, failed_turn_execution_without_batch,
    maap_repair_request, plan_turn_execution_from_batch,
};

/// Maximum number of ephemeral provider retries after a MAAP validation error.
///
/// The retry instruction is appended only to a cloned request and is never
/// returned in `AgentTurnExecution.request`, keeping repair diagnostics out of
/// durable transcripts and future model context when the corrected response is
/// valid.
const MAAP_REPAIR_ATTEMPT_LIMIT: usize = 2;

/// Borrows product-owned facts needed by the lower batch-continuation planner.
struct ProductBatchContinuationInput<'a> {
    /// Request that produced the current provider response.
    response_request: &'a ModelRequest,
    /// Raw provider output used only for ephemeral repair context.
    response_raw_text: &'a str,
    /// Parsed batch being validated.
    batch: &'a super::super::MaapBatch,
    /// Active request whose allowed-action surface must be enforced.
    active_request: &'a ModelRequest,
    /// Active product turn identity.
    turn: &'a AgentTurnRecord,
    /// MCP server ids available to product MAAP validation.
    available_mcp_servers: &'a [String],
    /// Concrete MCP tools available to product MAAP validation.
    available_mcp_tools: &'a [McpPromptTool],
}

/// Supplies product MAAP validation to canonical lower continuation planning.
fn plan_product_batch_continuation(
    input: ProductBatchContinuationInput<'_>,
    negotiation: &mut AgentTurnNegotiation<ModelRequest>,
) -> std::result::Result<BatchContinuationPlan, (MezError, &'static str)> {
    mez_agent::plan_batch_continuation(
        BatchContinuationInput {
            response_request: input.response_request,
            response_raw_text: input.response_raw_text,
            batch: input.batch,
            active_request: input.active_request,
        },
        negotiation,
        || {
            input
                .batch
                .validate(
                    input.turn,
                    input.available_mcp_servers,
                    input.available_mcp_tools,
                )
                .map_err(|error| {
                    let message = error.message().to_string();
                    BatchValidationFailure::new(error, message)
                })
        },
    )
    .map_err(|rejection| {
        let error = match rejection.error {
            BatchContinuationError::Recovery(error) => MezError::invalid_args(error.message()),
            BatchContinuationError::Product(error) => error,
        };
        (error, rejection.stage)
    })
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
    pub permissions: &'a dyn mez_agent::PermissionPlanning,
    /// Structured `subagent_scope` value carried by this API type.
    pub subagent_scope: Option<&'a SubagentScopeDeclaration>,
    /// Product adapter for shell and patch scope classification.
    pub subagent_scope_enforcement: &'a dyn mez_agent::SubagentScopeEnforcement,
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
        let mut negotiation = AgentTurnNegotiation::new(request.clone(), MAAP_REPAIR_ATTEMPT_LIMIT);
        let mut response_request: ModelRequest;
        let mut response = loop {
            response_request = request.clone();
            let response = match self.provider.send_request(&request) {
                Ok(response) => response,
                Err(error) => {
                    match negotiation.advance_provider_failure(
                        maap_provider_error_is_repairable(&error),
                        provider_error_retry_class(&error),
                    ) {
                        AgentTurnProviderFailureDecision::RecoverMalformedOutput { attempt } => {
                            request = maap_repair_request(
                                &response_request,
                                error.message(),
                                error.provider_raw_text().unwrap_or(""),
                                attempt,
                            );
                            continue;
                        }
                        AgentTurnProviderFailureDecision::ReturnToRuntime => return Err(error),
                        AgentTurnProviderFailureDecision::Summarize => {}
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
            let response_decision = negotiation.advance_provider_response(
                &response_request,
                self.provider.provider_id(),
                &response.provider,
                response_request.interaction_kind == ModelInteractionKind::Repair,
                response.action_batch.is_some(),
                response.usage,
                response.latest_request_usage,
                &response.quota_usage,
            );
            if let AgentTurnResponseDecision::Reject(
                response_acceptance @ ProviderResponseAcceptance::ProviderIdentityMismatch,
            ) = response_decision
            {
                let error = MezError::invalid_state(
                    response_acceptance
                        .rejection_message()
                        .expect("provider identity rejection has a diagnostic"),
                );
                if let Some(execution) = summarize_controller_failure_execution(
                    self.provider,
                    &turn,
                    &response_request,
                    FailureSummaryInput {
                        failed_response: response.clone(),
                        error: &error,
                        scope: FailureSummaryScope {
                            stage: response_acceptance
                                .rejection_stage()
                                .expect("provider identity rejection has a failure stage"),
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
            let Some(batch) = &response.action_batch else {
                let error = MezError::invalid_args(
                    "provider response did not include a parsed MAAP action_batch",
                );
                if let AgentTurnResponseDecision::RecoverMissingActionBatch { attempt } =
                    response_decision
                {
                    request = maap_repair_request(
                        &response_request,
                        error.message(),
                        &response.raw_text,
                        attempt,
                    );
                    continue;
                }
                debug_assert_eq!(
                    response_decision,
                    AgentTurnResponseDecision::Reject(
                        ProviderResponseAcceptance::MissingActionBatch
                    )
                );
                ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                let mut response = response;
                response.usage = negotiation.cumulative_response_usage();
                response.quota_usage = negotiation.latest_quota_usage().to_vec();
                return Ok(failed_maap_validation_execution_with_summary(
                    self.provider,
                    &turn,
                    negotiation.durable_request().clone(),
                    response,
                    negotiation.latest_response_usage(),
                    &error,
                    FailureSummaryScope {
                        stage: "maap_missing_action_batch",
                        available_mcp_servers: &self.available_mcp_servers,
                        available_mcp_tools: self.available_mcp_tools,
                    },
                ));
            };
            match plan_product_batch_continuation(
                ProductBatchContinuationInput {
                    response_request: &response_request,
                    response_raw_text: &response.raw_text,
                    batch,
                    active_request: &request,
                    turn: &turn,
                    available_mcp_servers: &self.available_mcp_servers,
                    available_mcp_tools: self.available_mcp_tools,
                },
                &mut negotiation,
            ) {
                Ok(BatchContinuationPlan::Continue(next_request)) => {
                    request = *next_request;
                    continue;
                }
                Ok(BatchContinuationPlan::Execute) => {}
                Err((error, stage)) => {
                    ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                    let mut response = response;
                    response.usage = negotiation.cumulative_response_usage();
                    response.quota_usage = negotiation.latest_quota_usage().to_vec();
                    return Ok(failed_maap_validation_execution_with_summary(
                        self.provider,
                        &turn,
                        negotiation.durable_request().clone(),
                        response,
                        negotiation.latest_response_usage(),
                        &error,
                        FailureSummaryScope {
                            stage,
                            available_mcp_servers: &self.available_mcp_servers,
                            available_mcp_tools: self.available_mcp_tools,
                        },
                    ));
                }
            }
            break response;
        };
        response.usage = negotiation.cumulative_response_usage();
        response.quota_usage = negotiation.latest_quota_usage().to_vec();

        let Some(batch) = response.action_batch.clone() else {
            ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
            return Ok(failed_turn_execution_without_batch(
                negotiation.durable_request().clone(),
                response,
                negotiation.latest_response_usage(),
            ));
        };

        let execution = plan_turn_execution_from_batch(
            &turn,
            context,
            negotiation.durable_request().clone(),
            response,
            negotiation.latest_response_usage(),
            &batch,
            |action| self.plan_action_result(&turn, action),
        )?;
        if execution.terminal_state != AgentTurnState::Running {
            ledger.finish_turn(&turn.turn_id, execution.terminal_state)?;
        }

        Ok(execution)
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
        F: FnMut(&AgentAction) -> Result<McpExecutionRequest>,
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
        let mut negotiation = AgentTurnNegotiation::new(request.clone(), MAAP_REPAIR_ATTEMPT_LIMIT);
        let mut response_request: ModelRequest;
        let mut response = loop {
            response_request = request.clone();
            let response = match self.provider.send_request_async(&request).await {
                Ok(response) => response,
                Err(error) => {
                    match negotiation.advance_provider_failure(
                        maap_provider_error_is_repairable(&error),
                        provider_error_retry_class(&error),
                    ) {
                        AgentTurnProviderFailureDecision::RecoverMalformedOutput { attempt } => {
                            request = maap_repair_request(
                                &response_request,
                                error.message(),
                                error.provider_raw_text().unwrap_or(""),
                                attempt,
                            );
                            continue;
                        }
                        AgentTurnProviderFailureDecision::ReturnToRuntime => return Err(error),
                        AgentTurnProviderFailureDecision::Summarize => {}
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
            let response_decision = negotiation.advance_provider_response(
                &response_request,
                self.provider.provider_id(),
                &response.provider,
                response_request.interaction_kind == ModelInteractionKind::Repair,
                response.action_batch.is_some(),
                response.usage,
                response.latest_request_usage,
                &response.quota_usage,
            );
            if let AgentTurnResponseDecision::Reject(
                response_acceptance @ ProviderResponseAcceptance::ProviderIdentityMismatch,
            ) = response_decision
            {
                let error = MezError::invalid_state(
                    response_acceptance
                        .rejection_message()
                        .expect("provider identity rejection has a diagnostic"),
                );
                if let Some(execution) = summarize_controller_failure_execution_async(
                    self.provider,
                    &turn,
                    &response_request,
                    FailureSummaryInput {
                        failed_response: response.clone(),
                        error: &error,
                        scope: FailureSummaryScope {
                            stage: response_acceptance
                                .rejection_stage()
                                .expect("provider identity rejection has a failure stage"),
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
            let Some(batch) = &response.action_batch else {
                let error = MezError::invalid_args(
                    "provider response did not include a parsed MAAP action_batch",
                );
                if let AgentTurnResponseDecision::RecoverMissingActionBatch { attempt } =
                    response_decision
                {
                    request = maap_repair_request(
                        &response_request,
                        error.message(),
                        &response.raw_text,
                        attempt,
                    );
                    continue;
                }
                debug_assert_eq!(
                    response_decision,
                    AgentTurnResponseDecision::Reject(
                        ProviderResponseAcceptance::MissingActionBatch
                    )
                );
                ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                let mut response = response;
                response.usage = negotiation.cumulative_response_usage();
                response.quota_usage = negotiation.latest_quota_usage().to_vec();
                return Ok(failed_maap_validation_execution_with_summary_async(
                    self.provider,
                    &turn,
                    negotiation.durable_request().clone(),
                    response,
                    negotiation.latest_response_usage(),
                    &error,
                    FailureSummaryScope {
                        stage: "maap_missing_action_batch",
                        available_mcp_servers: &self.available_mcp_servers,
                        available_mcp_tools: self.available_mcp_tools,
                    },
                )
                .await);
            };
            match plan_product_batch_continuation(
                ProductBatchContinuationInput {
                    response_request: &response_request,
                    response_raw_text: &response.raw_text,
                    batch,
                    active_request: &request,
                    turn: &turn,
                    available_mcp_servers: &self.available_mcp_servers,
                    available_mcp_tools: self.available_mcp_tools,
                },
                &mut negotiation,
            ) {
                Ok(BatchContinuationPlan::Continue(next_request)) => {
                    request = *next_request;
                    continue;
                }
                Ok(BatchContinuationPlan::Execute) => {}
                Err((error, stage)) => {
                    ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                    let mut response = response;
                    response.usage = negotiation.cumulative_response_usage();
                    response.quota_usage = negotiation.latest_quota_usage().to_vec();
                    return Ok(failed_maap_validation_execution_with_summary_async(
                        self.provider,
                        &turn,
                        negotiation.durable_request().clone(),
                        response,
                        negotiation.latest_response_usage(),
                        &error,
                        FailureSummaryScope {
                            stage,
                            available_mcp_servers: &self.available_mcp_servers,
                            available_mcp_tools: self.available_mcp_tools,
                        },
                    )
                    .await);
                }
            }
            break response;
        };
        response.usage = negotiation.cumulative_response_usage();
        response.quota_usage = negotiation.latest_quota_usage().to_vec();

        let Some(batch) = response.action_batch.clone() else {
            ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
            return Ok(failed_turn_execution_without_batch(
                negotiation.durable_request().clone(),
                response,
                negotiation.latest_response_usage(),
            ));
        };

        let execution = plan_turn_execution_from_batch(
            &turn,
            context,
            negotiation.durable_request().clone(),
            response,
            negotiation.latest_response_usage(),
            &batch,
            |action| self.plan_action_result(&turn, action),
        )?;
        if execution.terminal_state != AgentTurnState::Running {
            ledger.finish_turn(&turn.turn_id, execution.terminal_state)?;
        }

        Ok(execution)
    }
}
