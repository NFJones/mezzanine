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
    AgentContext, AgentTurnLedger, AgentTurnRecord, AllowedActionSet, McpPromptTool, MezError,
    ModelProfile, ModelRequest, Result, assemble_model_request, provider_error_retry_class,
};
#[cfg(test)]
use super::super::{AgentTurnState, MarkerToken, McpExecutionRequest, Path};
#[cfg(test)]
use super::execution::{
    PaneShellLocalExecutor, execute_local_action, execute_mcp_action_through_runtime,
};
use super::recovery::{
    FailureSummaryInput, FailureSummaryScope, maap_provider_error_is_repairable,
    summarize_controller_failure_execution_async, summarize_provider_failure_execution_async,
};
#[cfg(test)]
use super::recovery::{
    summarize_controller_failure_execution, summarize_provider_failure_execution,
};
#[cfg(test)]
use crate::integrations::agent::provider::ModelProvider;
#[cfg(test)]
use mez_agent::turn_state_from_action_results;
use mez_agent::{
    AgentTurnEnvironment, AgentTurnExecution, AgentTurnProviderFailure, SubagentScopeDeclaration,
};
#[cfg(test)]
use mez_agent::{LocalActionExecutor, McpActionExecutor, PaneShellExecutor};

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

/// Synchronous fake-provider adapter for canonical lower turn orchestration.
#[cfg(test)]
struct SyncProductAgentTurnEnvironment<'runner, 'config, P> {
    runner: &'runner AgentTurnRunner<'config, P>,
}

#[cfg(test)]
impl<P: ModelProvider> AgentTurnEnvironment for SyncProductAgentTurnEnvironment<'_, '_, P> {
    type Error = MezError;

    fn provider_id(&self) -> &str {
        self.runner.provider.provider_id()
    }

    fn assemble_request(
        &self,
        turn: &AgentTurnRecord,
        context: &AgentContext,
    ) -> Result<ModelRequest> {
        Ok(assemble_model_request(
            &self.runner.model_profile,
            turn,
            context,
        )?)
    }

    fn available_mcp_tools(&self) -> &[McpPromptTool] {
        self.runner.available_mcp_tools
    }

    fn memory_actions_enabled(&self) -> bool {
        self.runner.memory_actions_enabled
    }

    fn issue_actions_enabled(&self) -> bool {
        self.runner.issue_actions_enabled
    }

    async fn send_request(&self, request: &ModelRequest) -> Result<super::super::ModelResponse> {
        self.runner.provider.send_request(request)
    }

    fn provider_failure(&self, error: &MezError) -> AgentTurnProviderFailure {
        AgentTurnProviderFailure {
            repairable_malformed_output: maap_provider_error_is_repairable(error),
            retry_class: provider_error_retry_class(error),
            message: error.message().to_string(),
            raw_text: error.provider_raw_text().unwrap_or("").to_string(),
        }
    }

    fn validate_batch(
        &self,
        turn: &AgentTurnRecord,
        batch: &super::super::MaapBatch,
    ) -> Result<()> {
        Ok(batch.validate_harness_contract(
            &turn.turn_id,
            &turn.agent_id,
            &self.runner.available_mcp_servers,
            self.runner.available_mcp_tools,
        )?)
    }

    fn plan_action_result(
        &self,
        turn: &AgentTurnRecord,
        action: &super::super::AgentAction,
    ) -> Result<super::super::ActionResult> {
        self.runner.plan_action_result(turn, action)
    }

    fn invalid_args(&self, message: String) -> MezError {
        MezError::invalid_args(message)
    }

    fn invalid_state(&self, message: String) -> MezError {
        MezError::invalid_state(message)
    }

    fn error_message<'a>(&self, error: &'a MezError) -> &'a str {
        error.message()
    }

    async fn summarize_provider_failure(
        &self,
        turn: &AgentTurnRecord,
        previous_request: &ModelRequest,
        error: &MezError,
    ) -> Option<AgentTurnExecution> {
        summarize_provider_failure_execution(self.runner.provider, turn, previous_request, error)
    }

    async fn summarize_controller_failure(
        &self,
        turn: &AgentTurnRecord,
        previous_request: &ModelRequest,
        failed_response: super::super::ModelResponse,
        error: &MezError,
        stage: &'static str,
    ) -> Option<AgentTurnExecution> {
        summarize_controller_failure_execution(
            self.runner.provider,
            turn,
            previous_request,
            FailureSummaryInput {
                failed_response,
                error,
                scope: FailureSummaryScope {
                    stage,
                    available_mcp_servers: &self.runner.available_mcp_servers,
                    available_mcp_tools: self.runner.available_mcp_tools,
                },
            },
        )
    }
}

/// Polls a synchronous fake-provider turn future that must not wait on I/O.
#[cfg(test)]
fn run_ready_turn<T>(future: impl std::future::Future<Output = T>) -> T {
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    let mut future = pin!(future);
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    match future.as_mut().poll(&mut context) {
        Poll::Ready(output) => output,
        Poll::Pending => panic!("synchronous test provider unexpectedly returned pending"),
    }
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
        self.run_turn_ref_with_allowed_actions(ledger, turn, context, None, None)
    }

    /// Executes a borrowed-context turn with an optional controller-selected
    /// initial action surface.
    pub fn run_turn_ref_with_allowed_actions(
        &self,
        ledger: &mut AgentTurnLedger,
        turn: AgentTurnRecord,
        context: &AgentContext,
        allowed_actions: Option<AllowedActionSet>,
        interaction_kind: Option<mez_agent::ModelInteractionKind>,
    ) -> Result<AgentTurnExecution> {
        let environment = SyncProductAgentTurnEnvironment { runner: self };
        run_ready_turn(mez_agent::run_agent_turn_async(
            &environment,
            ledger,
            turn,
            context,
            allowed_actions,
            interaction_kind,
        ))
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
        executor: &mut impl PaneShellExecutor<Error = MezError>,
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
        executor: &mut impl LocalActionExecutor<Error = MezError>,
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
        executor: &mut impl McpActionExecutor<Error = MezError>,
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

/// Product effects supplied to the canonical lower production turn loop.
struct ProductAgentTurnEnvironment<'runner, 'config, P> {
    runner: &'runner AgentTurnRunner<'config, P>,
}

impl<P: AsyncModelProvider> AgentTurnEnvironment for ProductAgentTurnEnvironment<'_, '_, P> {
    type Error = MezError;

    fn provider_id(&self) -> &str {
        self.runner.provider.provider_id()
    }

    fn assemble_request(
        &self,
        turn: &AgentTurnRecord,
        context: &AgentContext,
    ) -> Result<ModelRequest> {
        Ok(assemble_model_request(
            &self.runner.model_profile,
            turn,
            context,
        )?)
    }

    fn available_mcp_tools(&self) -> &[McpPromptTool] {
        self.runner.available_mcp_tools
    }

    fn memory_actions_enabled(&self) -> bool {
        self.runner.memory_actions_enabled
    }

    fn issue_actions_enabled(&self) -> bool {
        self.runner.issue_actions_enabled
    }

    async fn send_request(&self, request: &ModelRequest) -> Result<super::super::ModelResponse> {
        self.runner.provider.send_request_async(request).await
    }

    fn provider_failure(&self, error: &MezError) -> AgentTurnProviderFailure {
        AgentTurnProviderFailure {
            repairable_malformed_output: maap_provider_error_is_repairable(error),
            retry_class: provider_error_retry_class(error),
            message: error.message().to_string(),
            raw_text: error.provider_raw_text().unwrap_or("").to_string(),
        }
    }

    fn validate_batch(
        &self,
        turn: &AgentTurnRecord,
        batch: &super::super::MaapBatch,
    ) -> Result<()> {
        Ok(batch.validate_harness_contract(
            &turn.turn_id,
            &turn.agent_id,
            &self.runner.available_mcp_servers,
            self.runner.available_mcp_tools,
        )?)
    }

    fn plan_action_result(
        &self,
        turn: &AgentTurnRecord,
        action: &super::super::AgentAction,
    ) -> Result<super::super::ActionResult> {
        self.runner.plan_action_result(turn, action)
    }

    fn invalid_args(&self, message: String) -> MezError {
        MezError::invalid_args(message)
    }

    fn invalid_state(&self, message: String) -> MezError {
        MezError::invalid_state(message)
    }

    fn error_message<'a>(&self, error: &'a MezError) -> &'a str {
        error.message()
    }

    async fn summarize_provider_failure(
        &self,
        turn: &AgentTurnRecord,
        previous_request: &ModelRequest,
        error: &MezError,
    ) -> Option<AgentTurnExecution> {
        summarize_provider_failure_execution_async(
            self.runner.provider,
            turn,
            previous_request,
            error,
        )
        .await
    }

    async fn summarize_controller_failure(
        &self,
        turn: &AgentTurnRecord,
        previous_request: &ModelRequest,
        failed_response: super::super::ModelResponse,
        error: &MezError,
        stage: &'static str,
    ) -> Option<AgentTurnExecution> {
        summarize_controller_failure_execution_async(
            self.runner.provider,
            turn,
            previous_request,
            FailureSummaryInput {
                failed_response,
                error,
                scope: FailureSummaryScope {
                    stage,
                    available_mcp_servers: &self.runner.available_mcp_servers,
                    available_mcp_tools: self.runner.available_mcp_tools,
                },
            },
        )
        .await
    }
}

impl<'a, P: AsyncModelProvider> AgentTurnRunner<'a, P> {
    /// Executes the `run_turn_async` operation for the owning subsystem.
    ///
    /// Callers receive a typed result or error with context from the underlying
    /// runtime operation.
    #[cfg(test)]
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
    #[cfg(test)]
    pub async fn run_turn_async_ref(
        &self,
        ledger: &mut AgentTurnLedger,
        turn: AgentTurnRecord,
        context: &AgentContext,
    ) -> Result<AgentTurnExecution> {
        self.run_turn_async_ref_with_allowed_actions(ledger, turn, context, None, None)
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
        interaction_kind: Option<mez_agent::ModelInteractionKind>,
    ) -> Result<AgentTurnExecution> {
        let environment = ProductAgentTurnEnvironment { runner: self };
        mez_agent::run_agent_turn_async(
            &environment,
            ledger,
            turn,
            context,
            allowed_actions,
            interaction_kind,
        )
        .await
    }
}
