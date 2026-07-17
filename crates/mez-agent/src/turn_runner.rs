//! Canonical provider-independent production turn orchestration.
//!
//! This module owns the async provider negotiation loop used by production.
//! Product request assembly, provider transport, MAAP validation, action
//! planning, and failure-summary I/O are injected through `AgentTurnEnvironment`.
//! The orchestrator owns recovery budgets, durable request promotion, response
//! accounting, terminal ledger transitions, and execution projection.

use std::error::Error;

use crate::{
    ActionResult, AgentAction, AgentContext, AgentTurnExecution, AgentTurnLedger,
    AgentTurnLedgerError, AgentTurnNegotiation, AgentTurnProviderFailureDecision, AgentTurnRecord,
    AgentTurnResponseDecision, AgentTurnState, AllowedActionSet, BatchContinuationError,
    BatchContinuationInput, BatchContinuationPlan, BatchValidationFailure, MaapBatch,
    McpPromptTool, ModelInteractionKind, ModelRequest, ModelResponse, ProviderErrorRetryClass,
    ProviderResponseAcceptance, apply_default_action_gates, failed_turn_execution_without_batch,
    maap_repair_request, plan_batch_continuation, plan_turn_execution_from_batch,
};

/// Default number of ephemeral repairs accepted during one provider negotiation phase.
pub const DEFAULT_MAAP_REPAIR_ATTEMPT_LIMIT: usize = 2;

/// Normalized provider failure facts consumed by canonical recovery policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTurnProviderFailure {
    /// Whether malformed provider output can be repaired with a corrective request.
    pub repairable_malformed_output: bool,
    /// Retry category used to distinguish runtime recovery from summarization.
    pub retry_class: ProviderErrorRetryClass,
    /// Secret-safe provider failure message.
    pub message: String,
    /// Secret-safe raw provider output used only for ephemeral repair context.
    pub raw_text: String,
}

/// Product effects and policy required by canonical turn orchestration.
#[allow(async_fn_in_trait)]
pub trait AgentTurnEnvironment {
    /// Product error projected by request, provider, validation, and planning adapters.
    type Error: Error + From<AgentTurnLedgerError>;

    /// Returns the provider identity expected on every response.
    fn provider_id(&self) -> &str;

    /// Assembles the initial provider-neutral request for one turn.
    fn assemble_request(
        &self,
        turn: &AgentTurnRecord,
        context: &AgentContext,
    ) -> Result<ModelRequest, Self::Error>;

    /// Returns MCP tools exposed to default action-gating policy.
    fn available_mcp_tools(&self) -> &[McpPromptTool];

    /// Reports whether persistent-memory actions may be exposed.
    fn memory_actions_enabled(&self) -> bool;

    /// Reports whether local issue actions may be exposed.
    fn issue_actions_enabled(&self) -> bool;

    /// Sends one provider request through the concrete transport.
    async fn send_request(&self, request: &ModelRequest) -> Result<ModelResponse, Self::Error>;

    /// Classifies one provider failure for canonical recovery progression.
    fn provider_failure(&self, error: &Self::Error) -> AgentTurnProviderFailure;

    /// Validates one parsed action batch against product-owned policy facts.
    fn validate_batch(&self, turn: &AgentTurnRecord, batch: &MaapBatch) -> Result<(), Self::Error>;

    /// Plans the initial result state for one accepted action.
    fn plan_action_result(
        &self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
    ) -> Result<ActionResult, Self::Error>;

    /// Constructs a product invalid-argument error without coupling the lower crate to it.
    fn invalid_args(&self, message: String) -> Self::Error;

    /// Constructs a product invalid-state error without coupling the lower crate to it.
    fn invalid_state(&self, message: String) -> Self::Error;

    /// Returns the stable model-safe message for one product error.
    fn error_message<'a>(&self, error: &'a Self::Error) -> &'a str;

    /// Attempts a best-effort user-facing summary of a provider failure.
    async fn summarize_provider_failure(
        &self,
        turn: &AgentTurnRecord,
        previous_request: &ModelRequest,
        error: &Self::Error,
    ) -> Option<AgentTurnExecution>;

    /// Attempts a best-effort user-facing summary of a controller failure.
    async fn summarize_controller_failure(
        &self,
        turn: &AgentTurnRecord,
        previous_request: &ModelRequest,
        failed_response: ModelResponse,
        error: &Self::Error,
        stage: &'static str,
    ) -> Option<AgentTurnExecution>;
}

/// Projects a canonical ledger result into the product environment error type.
fn project_ledger_result<T, E>(result: Result<T, AgentTurnLedgerError>) -> Result<T, E>
where
    E: From<AgentTurnLedgerError>,
{
    result.map_err(E::from)
}

/// Executes one production agent turn through injected product effects.
///
/// The returned execution is canonical and may be running when product action
/// adapters still need to dispatch approved work. Ledger failures and product
/// adapter failures are returned through `AgentTurnEnvironment::Error`.
pub async fn run_agent_turn_async<E: AgentTurnEnvironment>(
    environment: &E,
    ledger: &mut AgentTurnLedger,
    turn: AgentTurnRecord,
    context: &AgentContext,
    allowed_actions: Option<AllowedActionSet>,
) -> Result<AgentTurnExecution, E::Error> {
    project_ledger_result(ledger.start_turn(turn.clone()))?;
    let mut request = environment.assemble_request(&turn, context)?;
    if let Some(allowed_actions) = allowed_actions {
        request.interaction_kind = ModelInteractionKind::ActionExecution;
        request.allowed_actions = allowed_actions;
    }
    apply_default_action_gates(
        &mut request,
        environment.available_mcp_tools(),
        environment.memory_actions_enabled(),
        environment.issue_actions_enabled(),
    );
    let mut negotiation =
        AgentTurnNegotiation::new(request.clone(), DEFAULT_MAAP_REPAIR_ATTEMPT_LIMIT);

    let mut response = loop {
        let response_request = request.clone();
        let mut response = match environment.send_request(&request).await {
            Ok(response) => response,
            Err(error) => {
                let failure = environment.provider_failure(&error);
                match negotiation.advance_provider_failure(
                    failure.repairable_malformed_output,
                    failure.retry_class,
                ) {
                    AgentTurnProviderFailureDecision::RecoverMalformedOutput { attempt } => {
                        request = maap_repair_request(
                            &response_request,
                            &failure.message,
                            &failure.raw_text,
                            attempt,
                        );
                        continue;
                    }
                    AgentTurnProviderFailureDecision::ReturnToRuntime => return Err(error),
                    AgentTurnProviderFailureDecision::Summarize => {}
                }
                if let Some(execution) = environment
                    .summarize_provider_failure(&turn, &response_request, &error)
                    .await
                {
                    project_ledger_result(
                        ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed),
                    )?;
                    return Ok(execution);
                }
                project_ledger_result(ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed))?;
                return Err(error);
            }
        };

        let response_decision = negotiation.advance_provider_response(
            &response_request,
            environment.provider_id(),
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
            let error = environment.invalid_state(
                response_acceptance
                    .rejection_message()
                    .unwrap_or("provider response identity does not match the active provider")
                    .to_string(),
            );
            let stage = response_acceptance
                .rejection_stage()
                .unwrap_or("provider_identity");
            if let Some(execution) = environment
                .summarize_controller_failure(
                    &turn,
                    &response_request,
                    response.clone(),
                    &error,
                    stage,
                )
                .await
            {
                project_ledger_result(ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed))?;
                return Ok(execution);
            }
            project_ledger_result(ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed))?;
            return Err(error);
        }

        let Some(batch) = &response.action_batch else {
            let error = environment.invalid_args(
                "provider response did not include a parsed MAAP action_batch".to_string(),
            );
            if let AgentTurnResponseDecision::RecoverMissingActionBatch { attempt } =
                response_decision
            {
                request = maap_repair_request(
                    &response_request,
                    environment.error_message(&error),
                    &response.raw_text,
                    attempt,
                );
                continue;
            }
            project_ledger_result(ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed))?;
            response.usage = negotiation.cumulative_response_usage();
            response.quota_usage = negotiation.latest_quota_usage().to_vec();
            return Ok(failed_validation_execution_with_summary(
                environment,
                &turn,
                negotiation.durable_request().clone(),
                response,
                negotiation.latest_response_usage(),
                &error,
                "maap_missing_action_batch",
            )
            .await);
        };

        let continuation = plan_batch_continuation(
            BatchContinuationInput {
                response_request: &response_request,
                response_raw_text: &response.raw_text,
                batch,
                active_request: &request,
            },
            &mut negotiation,
            || {
                environment.validate_batch(&turn, batch).map_err(|error| {
                    let message = environment.error_message(&error).to_string();
                    BatchValidationFailure::new(error, message)
                })
            },
        );
        match continuation {
            Ok(BatchContinuationPlan::Continue(next_request)) => {
                request = *next_request;
                continue;
            }
            Ok(BatchContinuationPlan::Execute) => break response,
            Err(rejection) => {
                let error = match rejection.error {
                    BatchContinuationError::Recovery(error) => {
                        environment.invalid_args(error.message().to_string())
                    }
                    BatchContinuationError::Product(error) => error,
                };
                project_ledger_result(ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed))?;
                response.usage = negotiation.cumulative_response_usage();
                response.quota_usage = negotiation.latest_quota_usage().to_vec();
                return Ok(failed_validation_execution_with_summary(
                    environment,
                    &turn,
                    negotiation.durable_request().clone(),
                    response,
                    negotiation.latest_response_usage(),
                    &error,
                    rejection.stage,
                )
                .await);
            }
        }
    };

    response.usage = negotiation.cumulative_response_usage();
    response.quota_usage = negotiation.latest_quota_usage().to_vec();
    let Some(batch) = response.action_batch.clone() else {
        project_ledger_result(ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed))?;
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
        |action| environment.plan_action_result(&turn, action),
    )?;
    if execution.terminal_state != AgentTurnState::Running {
        project_ledger_result(ledger.finish_turn(&turn.turn_id, execution.terminal_state))?;
    }
    Ok(execution)
}

/// Builds a terminal validation failure and lets the product adapter summarize it.
async fn failed_validation_execution_with_summary<E: AgentTurnEnvironment>(
    environment: &E,
    turn: &AgentTurnRecord,
    request: ModelRequest,
    mut response: ModelResponse,
    latest_response_usage: crate::ModelTokenUsage,
    error: &E::Error,
    stage: &'static str,
) -> AgentTurnExecution {
    response.raw_text = format!(
        "{}\nmaap_validation_error: {}",
        response.raw_text,
        environment.error_message(error)
    );
    let failed = AgentTurnExecution {
        request: request.clone(),
        response,
        latest_response_usage,
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: Vec::new(),
        final_turn: true,
        terminal_state: AgentTurnState::Failed,
    };
    environment
        .summarize_controller_failure(turn, &request, failed.response.clone(), error, stage)
        .await
        .unwrap_or(failed)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::fmt;
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    use super::*;
    use crate::{
        AgentActionPayload, AgentTurnTrigger, ContextBlock, ContextSourceKind, ModelMessage,
        ModelMessageRole, ModelTokenUsage, SayStatus,
    };

    /// Product-shaped test error used by every fake environment boundary.
    #[derive(Debug, Clone)]
    struct TestError(String);

    impl fmt::Display for TestError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(&self.0)
        }
    }

    impl Error for TestError {}

    impl From<AgentTurnLedgerError> for TestError {
        fn from(error: AgentTurnLedgerError) -> Self {
            Self(error.to_string())
        }
    }

    /// Fake product environment that records canonical provider requests.
    struct FakeEnvironment {
        responses: RefCell<VecDeque<ModelResponse>>,
        requests: RefCell<Vec<ModelRequest>>,
    }

    impl AgentTurnEnvironment for FakeEnvironment {
        type Error = TestError;

        fn provider_id(&self) -> &str {
            "test"
        }

        fn assemble_request(
            &self,
            turn: &AgentTurnRecord,
            _context: &AgentContext,
        ) -> Result<ModelRequest, Self::Error> {
            Ok(ModelRequest {
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
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                available_mcp_tools: Vec::new(),
                memory_actions_enabled: false,
                issue_actions_enabled: false,
                interaction_kind: ModelInteractionKind::ActionExecution,
                allowed_actions: AllowedActionSet::say_only(),
                stop: None,
                messages: vec![ModelMessage {
                    role: ModelMessageRole::User,
                    source: ContextSourceKind::UserInstruction,
                    placement: crate::ContextPlacement::EphemeralTail,
                    content: "finish the task".to_string(),
                }],
            })
        }

        fn available_mcp_tools(&self) -> &[McpPromptTool] {
            &[]
        }

        fn memory_actions_enabled(&self) -> bool {
            false
        }

        fn issue_actions_enabled(&self) -> bool {
            false
        }

        async fn send_request(&self, request: &ModelRequest) -> Result<ModelResponse, Self::Error> {
            self.requests.borrow_mut().push(request.clone());
            self.responses
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| TestError("fake provider response queue is empty".to_string()))
        }

        fn provider_failure(&self, error: &Self::Error) -> AgentTurnProviderFailure {
            AgentTurnProviderFailure {
                repairable_malformed_output: false,
                retry_class: ProviderErrorRetryClass::NonRetryable,
                message: error.to_string(),
                raw_text: String::new(),
            }
        }

        fn validate_batch(
            &self,
            _turn: &AgentTurnRecord,
            _batch: &MaapBatch,
        ) -> Result<(), Self::Error> {
            Ok(())
        }

        fn plan_action_result(
            &self,
            turn: &AgentTurnRecord,
            action: &AgentAction,
        ) -> Result<ActionResult, Self::Error> {
            Ok(ActionResult::succeeded(
                turn,
                action,
                vec!["completed".to_string()],
                None,
            ))
        }

        fn invalid_args(&self, message: String) -> Self::Error {
            TestError(message)
        }

        fn invalid_state(&self, message: String) -> Self::Error {
            TestError(message)
        }

        fn error_message<'a>(&self, error: &'a Self::Error) -> &'a str {
            &error.0
        }

        async fn summarize_provider_failure(
            &self,
            _turn: &AgentTurnRecord,
            _previous_request: &ModelRequest,
            _error: &Self::Error,
        ) -> Option<AgentTurnExecution> {
            None
        }

        async fn summarize_controller_failure(
            &self,
            _turn: &AgentTurnRecord,
            _previous_request: &ModelRequest,
            _failed_response: ModelResponse,
            _error: &Self::Error,
            _stage: &'static str,
        ) -> Option<AgentTurnExecution> {
            None
        }
    }

    /// Polls a fake-environment future that must never wait on external I/O.
    fn run_ready<T>(future: impl Future<Output = T>) -> T {
        let mut future = pin!(future);
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("fake turn environment unexpectedly returned pending"),
        }
    }

    /// Verifies fake provider and product ports execute the same canonical
    /// async state machine invoked by the production root adapter.
    #[test]
    fn fake_provider_and_ports_complete_one_agent_turn() {
        let turn = AgentTurnRecord {
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            pane_id: "pane-1".to_string(),
            trigger: AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 1,
            policy_profile: "default".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            state: AgentTurnState::Queued,
            cooperation_mode: None,
            initial_capability: None,
        };
        let action = AgentAction {
            id: "say-1".to_string(),
            rationale: "finish the turn".to_string(),
            payload: AgentActionPayload::Say {
                status: SayStatus::Final,
                text: "Done.".to_string(),
                content_type: "text/plain".to_string(),
            },
        };
        let environment = FakeEnvironment {
            responses: RefCell::new(VecDeque::from([ModelResponse {
                provider: "test".to_string(),
                model: "test-model".to_string(),
                raw_text: "done".to_string(),
                usage: ModelTokenUsage::default(),
                latest_request_usage: None,
                quota_usage: Vec::new(),
                action_batch: Some(MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "finish the task".to_string(),
                    thought: None,
                    turn_id: turn.turn_id.clone(),
                    agent_id: turn.agent_id.clone(),
                    actions: vec![action],
                    final_turn: true,
                }),
                provider_transcript_events: Vec::new(),
            }])),
            requests: RefCell::new(Vec::new()),
        };
        let context = AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            placement: crate::ContextPlacement::EphemeralTail,
            label: "request".to_string(),
            content: "finish the task".to_string(),
        }])
        .expect("test context should be valid");
        let mut ledger = AgentTurnLedger::new(false);

        let execution = run_ready(run_agent_turn_async(
            &environment,
            &mut ledger,
            turn,
            &context,
            None,
        ))
        .expect("canonical turn should complete");

        assert_eq!(execution.terminal_state, AgentTurnState::Completed);
        assert_eq!(execution.action_results[0].action_id, "say-1");
        assert_eq!(environment.requests.borrow().len(), 1);
    }
}
