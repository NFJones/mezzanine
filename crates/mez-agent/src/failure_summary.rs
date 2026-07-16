//! Provider-neutral progression for terminal failure summaries.
//!
//! Product adapters may ask a model to characterize a terminal provider or
//! controller failure. Those requests still need bounded transport retries,
//! malformed-output repair, provider-identity enforcement, and response
//! validation. This module owns that state and canonical execution projection
//! without depending on concrete provider I/O, product errors, or product
//! summary-prompt construction.

use std::error::Error;
use std::fmt;

use crate::{
    ActionResult, AgentActionPayload, AgentTurnExecution, AgentTurnRecord, AgentTurnRecoveryBudget,
    AgentTurnState, McpPromptTool, ModelRequest, ModelResponse, ProviderErrorRetryClass, SayStatus,
    say_action_structured_content_json, validate_batch_allowed_actions,
};

/// Failure returned when a model response cannot become a canonical terminal
/// failure-summary execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureSummaryExecutionError {
    message: String,
}

impl FailureSummaryExecutionError {
    /// Creates one execution-projection diagnostic.
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the stable diagnostic for product error projection or repair.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for FailureSummaryExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for FailureSummaryExecutionError {}

/// Converts a valid response-only failure summary into one terminal failed
/// execution.
///
/// Concrete provider calls and the product-authored summary request remain in
/// the application adapter. This function owns the provider-independent MAAP
/// validation, final-say normalization, action-result construction, response
/// accounting, and controller-failure transcript marker.
pub fn failure_summary_execution_from_response(
    turn: &AgentTurnRecord,
    request: ModelRequest,
    failed_response_raw_text: &str,
    mut response: ModelResponse,
    available_mcp_servers: &[String],
    available_mcp_tools: &[McpPromptTool],
) -> Result<AgentTurnExecution, FailureSummaryExecutionError> {
    let batch = response.action_batch.as_ref().ok_or_else(|| {
        FailureSummaryExecutionError::new(
            "failure summary response must include a say action batch",
        )
    })?;
    validate_batch_allowed_actions(batch, &request)
        .map_err(|error| FailureSummaryExecutionError::new(error.message()))?;
    batch
        .validate_harness_contract(
            &turn.turn_id,
            &turn.agent_id,
            available_mcp_servers,
            available_mcp_tools,
        )
        .map_err(|error| FailureSummaryExecutionError::new(error.message()))?;
    if batch.actions.is_empty()
        || batch
            .actions
            .iter()
            .any(|action| !matches!(action.payload, AgentActionPayload::Say { .. }))
    {
        return Err(FailureSummaryExecutionError::new(
            "failure summary response must contain only say actions",
        ));
    }
    let mut terminal_batch = batch.clone();
    terminal_batch.final_turn = true;
    for action in &mut terminal_batch.actions {
        if let AgentActionPayload::Say { status, .. } = &mut action.payload {
            *status = SayStatus::Final;
        }
    }
    let action_results = terminal_batch
        .actions
        .iter()
        .map(|action| match &action.payload {
            AgentActionPayload::Say {
                status,
                text,
                content_type,
            } => Ok(ActionResult::succeeded(
                turn,
                action,
                vec![text.clone()],
                Some(say_action_structured_content_json(
                    *status,
                    content_type,
                    text,
                )),
            )),
            _ => Err(FailureSummaryExecutionError::new(
                "failure summary response must contain only say actions",
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    response.raw_text = format!(
        "{failed_response_raw_text}\ncontroller_failure_summary:\n{}",
        response.raw_text
    );
    response.action_batch = Some(terminal_batch);
    let latest_response_usage = response.latest_request_usage.unwrap_or(response.usage);
    Ok(AgentTurnExecution {
        request,
        response,
        latest_response_usage,
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results,
        final_turn: true,
        terminal_state: AgentTurnState::Failed,
    })
}

/// Decision produced after a failure-summary provider call fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentFailureSummaryProviderDecision {
    /// Retry the unchanged summary request after a runtime-retryable failure.
    RetryProvider {
        /// One-based transport retry accepted for this summary request.
        attempt: usize,
    },
    /// Replace the summary request with a malformed-output repair request.
    RecoverMalformedOutput {
        /// One-based malformed-output repair accepted for this summary request.
        attempt: usize,
    },
    /// Stop best-effort summary negotiation and preserve the original failure.
    Reject,
}

/// Decision produced after a failure-summary provider response is validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentFailureSummaryResponseDecision {
    /// Accept the validated response as the terminal failure summary.
    Accept,
    /// Replace the request with a repair request for the malformed response.
    RecoverMalformedResponse {
        /// One-based malformed-response repair accepted for this summary request.
        attempt: usize,
    },
    /// Reject a response attributed to a different provider.
    RejectProviderIdentity,
    /// Reject a malformed response after its repair budget is exhausted.
    RejectMalformedResponse,
}

/// State for one best-effort terminal failure-summary negotiation.
///
/// Transport retry and malformed-output repair budgets are intentionally
/// independent. Retrying the same provider request must not consume the repair
/// capacity used to replace a malformed request or response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentFailureSummaryNegotiation<Request> {
    request: Request,
    provider_retry_budget: AgentTurnRecoveryBudget,
    repair_budget: AgentTurnRecoveryBudget,
}

impl<Request> AgentFailureSummaryNegotiation<Request> {
    /// Starts summary negotiation from a product-constructed request.
    pub const fn new(request: Request, provider_retry_limit: usize, repair_limit: usize) -> Self {
        Self {
            request,
            provider_retry_budget: AgentTurnRecoveryBudget::new(provider_retry_limit),
            repair_budget: AgentTurnRecoveryBudget::new(repair_limit),
        }
    }

    /// Returns the current request for the next provider call.
    pub const fn request(&self) -> &Request {
        &self.request
    }

    /// Replaces the current request with a product-constructed repair request.
    pub fn replace_request(&mut self, request: Request) {
        self.request = request;
    }

    /// Classifies a failed provider call and advances the matching bounded budget.
    ///
    /// Runtime-retryable failures first retry the unchanged request. Other
    /// malformed provider output may consume the independent repair budget.
    pub const fn advance_provider_failure(
        &mut self,
        retry_class: ProviderErrorRetryClass,
        repairable_malformed_output: bool,
    ) -> AgentFailureSummaryProviderDecision {
        if matches!(
            retry_class,
            ProviderErrorRetryClass::ContextLimit
                | ProviderErrorRetryClass::OutputLimit
                | ProviderErrorRetryClass::RetryableTransport
        ) && self.provider_retry_budget.record_attempt()
        {
            return AgentFailureSummaryProviderDecision::RetryProvider {
                attempt: self.provider_retry_budget.attempts(),
            };
        }
        if repairable_malformed_output && self.repair_budget.record_attempt() {
            return AgentFailureSummaryProviderDecision::RecoverMalformedOutput {
                attempt: self.repair_budget.attempts(),
            };
        }
        AgentFailureSummaryProviderDecision::Reject
    }

    /// Classifies a completed provider response and advances malformed-response repair.
    ///
    /// Provider identity is checked before response validity. A response from a
    /// different provider is terminal for this best-effort negotiation and does
    /// not consume repair capacity.
    pub fn advance_provider_response(
        &mut self,
        expected_provider: &str,
        actual_provider: &str,
        response_is_valid: bool,
    ) -> AgentFailureSummaryResponseDecision {
        if expected_provider != actual_provider {
            return AgentFailureSummaryResponseDecision::RejectProviderIdentity;
        }
        if response_is_valid {
            return AgentFailureSummaryResponseDecision::Accept;
        }
        if self.repair_budget.record_attempt() {
            return AgentFailureSummaryResponseDecision::RecoverMalformedResponse {
                attempt: self.repair_budget.attempts(),
            };
        }
        AgentFailureSummaryResponseDecision::RejectMalformedResponse
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AgentAction, AgentTurnTrigger, AllowedActionSet, MaapBatch, ModelInteractionKind,
        ModelTokenUsage,
    };

    /// Builds one canonical turn used by summary execution projection tests.
    fn turn() -> AgentTurnRecord {
        AgentTurnRecord {
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            pane_id: "%1".to_string(),
            trigger: AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 1,
            policy_profile: "default".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            state: AgentTurnState::Running,
            cooperation_mode: None,
            initial_capability: None,
        }
    }

    /// Builds the response-only request used by a terminal summary turn.
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
            interaction_kind: ModelInteractionKind::ActionExecution,
            allowed_actions: AllowedActionSet::say_only(),
            stop: None,
            messages: Vec::new(),
        }
    }

    /// Verifies transport retries and malformed-output repairs use independent
    /// budgets so one transient call failure does not remove repair capacity.
    #[test]
    fn failure_summary_negotiation_separates_retry_and_repair_budgets() {
        let mut negotiation = AgentFailureSummaryNegotiation::new("summary", 1, 1);

        assert_eq!(
            negotiation
                .advance_provider_failure(ProviderErrorRetryClass::RetryableTransport, false,),
            AgentFailureSummaryProviderDecision::RetryProvider { attempt: 1 }
        );
        assert_eq!(
            negotiation.advance_provider_failure(ProviderErrorRetryClass::NonRetryable, true),
            AgentFailureSummaryProviderDecision::RecoverMalformedOutput { attempt: 1 }
        );
        assert_eq!(
            negotiation.advance_provider_failure(ProviderErrorRetryClass::NonRetryable, true),
            AgentFailureSummaryProviderDecision::Reject
        );
    }

    /// Verifies valid responses are accepted, provider mismatches are terminal,
    /// and malformed responses consume only the bounded response-repair budget.
    #[test]
    fn failure_summary_negotiation_advances_provider_responses() {
        let mut negotiation = AgentFailureSummaryNegotiation::new("summary", 1, 1);

        assert_eq!(
            negotiation.advance_provider_response("openai", "other", false),
            AgentFailureSummaryResponseDecision::RejectProviderIdentity
        );
        assert_eq!(
            negotiation.advance_provider_response("openai", "openai", false),
            AgentFailureSummaryResponseDecision::RecoverMalformedResponse { attempt: 1 }
        );
        assert_eq!(
            negotiation.advance_provider_response("openai", "openai", false),
            AgentFailureSummaryResponseDecision::RejectMalformedResponse
        );

        let mut valid = AgentFailureSummaryNegotiation::new("summary", 0, 0);
        assert_eq!(
            valid.advance_provider_response("openai", "openai", true),
            AgentFailureSummaryResponseDecision::Accept
        );
    }

    /// Verifies valid say-only responses become final failed executions while
    /// preserving the original controller failure as transcript evidence.
    #[test]
    fn failure_summary_response_projects_terminal_failed_execution() {
        let response = ModelResponse {
            provider: "test".to_string(),
            model: "test-model".to_string(),
            raw_text: "provider unavailable".to_string(),
            usage: ModelTokenUsage::default(),
            latest_request_usage: None,
            quota_usage: Vec::new(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "Explain the failure".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-1".to_string(),
                actions: vec![AgentAction {
                    id: "say-1".to_string(),
                    rationale: "Explain the failure".to_string(),
                    payload: AgentActionPayload::Say {
                        status: SayStatus::Progress,
                        text: "The provider is unavailable.".to_string(),
                        content_type: "text/plain".to_string(),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        };

        let execution = failure_summary_execution_from_response(
            &turn(),
            request(),
            "original failure",
            response,
            &[],
            &[],
        )
        .unwrap();

        assert_eq!(execution.terminal_state, AgentTurnState::Failed);
        assert!(execution.final_turn);
        assert!(
            execution
                .response
                .raw_text
                .contains("original failure\ncontroller_failure_summary:")
        );
        assert!(matches!(
            execution.response.action_batch.unwrap().actions[0].payload,
            AgentActionPayload::Say {
                status: SayStatus::Final,
                ..
            }
        ));
    }
}
