//! Provider and controller recovery adapters for agent actions.
//!
//! This module owns product recovery paths around the main turn runner:
//! provider failure classification, provider retries, product error projection,
//! and final failure-summary execution shaping.

use super::super::{
    AgentTurnRecord, AgentTurnState, AllowedActionSet, AsyncModelProvider, ContextSourceKind,
    McpPromptTool, MezError, ModelInteractionKind, ModelMessage, ModelMessageRole, ModelRequest,
    ModelResponse, provider_error_retry_class,
};
use super::FAILURE_SUMMARY_RAW_TEXT_LIMIT_BYTES;
#[cfg(test)]
use crate::agent::provider::ModelProvider;
use mez_agent::{
    AgentFailureSummaryNegotiation, AgentFailureSummaryProviderDecision,
    AgentFailureSummaryResponseDecision, AgentTurnExecution, maap_repair_request,
};

/// Reports whether a provider error came from malformed model MAAP output that
/// can be repaired by asking the same model to re-emit the action batch.
pub(super) fn maap_provider_error_is_repairable(error: &MezError) -> bool {
    error.provider_raw_text().is_some()
        && error
            .message()
            .starts_with("provider MAAP output is malformed:")
}

/// Builds the terminal failed execution for a provider error when a final model
/// summary could not be obtained.
fn failed_provider_error_execution(
    request: ModelRequest,
    provider_id: &str,
    model: &str,
    error: &MezError,
) -> AgentTurnExecution {
    AgentTurnExecution {
        request,
        response: ModelResponse {
            provider: provider_id.to_string(),
            model: model.to_string(),
            raw_text: provider_error_raw_text(error),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: None,
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: Vec::new(),
        final_turn: true,
        terminal_state: AgentTurnState::Failed,
    }
}

/// Formats provider error detail for durable failed-turn output.
fn provider_error_raw_text(error: &MezError) -> String {
    match error.provider_raw_text() {
        Some(raw_text) => format!("{raw_text}\nprovider_error: {error}"),
        None => format!("provider_error: {error}"),
    }
}

/// Maximum number of retryable provider transport retries for one
/// failure-summary request.
const FAILURE_SUMMARY_PROVIDER_RETRY_LIMIT: usize = 1;

/// Maximum number of MAAP repair retries for one malformed failure-summary
/// response.
const FAILURE_SUMMARY_MAAP_REPAIR_LIMIT: usize = 1;

/// Builds a response-only model request for final failure characterization.
fn failure_summary_request(
    previous_request: &ModelRequest,
    stage: &str,
    error: &MezError,
    failed_response_raw_text: &str,
) -> ModelRequest {
    let mut request = previous_request.clone();
    request.interaction_kind = ModelInteractionKind::ActionExecution;
    request.allowed_actions = AllowedActionSet::say_only();
    request.messages.push(ModelMessage {
        role: ModelMessageRole::Developer,
        source: ContextSourceKind::Configuration,
        content: format!(
            "[controller failure summary]\n\
             Mezzanine has already failed this turn at the controller/provider boundary. \
             Return exactly one say action with status final that briefly characterizes the failure for the user. \
             Do not request capabilities, call tools, retry work, or claim the original task succeeded. \
             Name the failure class and the most useful next diagnostic step.\n\
             stage={stage}\n\
             error_kind={:?} error_message={}\n\
             failed_response_excerpt:\n{}",
            error.kind(),
            error.message(),
            failure_summary_raw_text_excerpt(failed_response_raw_text)
        ),
    });
    request
}

/// Returns a bounded UTF-8-safe excerpt for terminal failure summary prompts.
fn failure_summary_raw_text_excerpt(raw_text: &str) -> String {
    if raw_text.len() <= FAILURE_SUMMARY_RAW_TEXT_LIMIT_BYTES {
        return raw_text.to_string();
    }
    let mut end = FAILURE_SUMMARY_RAW_TEXT_LIMIT_BYTES;
    while !raw_text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!(
        "{}\n[truncated: original_bytes={}]",
        &raw_text[..end],
        raw_text.len()
    )
}

/// Validation surface needed to accept a final failure-summary response.
#[derive(Clone, Copy)]
pub(super) struct FailureSummaryScope<'a> {
    /// Failure stage reported to the model.
    pub(super) stage: &'a str,
    /// MCP servers available to the failed interaction.
    pub(super) available_mcp_servers: &'a [String],
    /// MCP tools available to the failed interaction.
    pub(super) available_mcp_tools: &'a [McpPromptTool],
}

/// Data needed to ask the model for a final failure summary.
pub(super) struct FailureSummaryInput<'a> {
    /// Failed response being characterized.
    pub(super) failed_response: ModelResponse,
    /// Controller/provider error being characterized.
    pub(super) error: &'a MezError,
    /// Summary validation and stage context.
    pub(super) scope: FailureSummaryScope<'a>,
}

/// Advances a failed summary provider call and installs a repair request when needed.
fn advance_failure_summary_provider_failure(
    negotiation: &mut AgentFailureSummaryNegotiation<ModelRequest>,
    error: &MezError,
) -> bool {
    match negotiation.advance_provider_failure(
        provider_error_retry_class(error),
        maap_provider_error_is_repairable(error),
    ) {
        AgentFailureSummaryProviderDecision::RetryProvider { .. } => true,
        AgentFailureSummaryProviderDecision::RecoverMalformedOutput { attempt } => {
            let request = maap_repair_request(
                negotiation.request(),
                error.message(),
                error.provider_raw_text().unwrap_or(""),
                attempt,
            );
            negotiation.replace_request(request);
            true
        }
        AgentFailureSummaryProviderDecision::Reject => false,
    }
}

/// Product-side outcome after lower-crate summary response progression.
enum FailureSummaryResponsePlan {
    /// Return a valid terminal summary execution.
    Finish(Box<AgentTurnExecution>),
    /// Send the repair request installed in the negotiation state.
    Continue,
    /// Stop best-effort summary negotiation.
    Reject,
}

/// Validates one summary response and advances canonical response progression.
fn advance_failure_summary_response(
    negotiation: &mut AgentFailureSummaryNegotiation<ModelRequest>,
    expected_provider: &str,
    turn: &AgentTurnRecord,
    failed_response_raw_text: &str,
    response: ModelResponse,
    scope: FailureSummaryScope<'_>,
) -> FailureSummaryResponsePlan {
    let actual_provider = response.provider.clone();
    let response_raw_text = response.raw_text.clone();
    let execution = mez_agent::failure_summary_execution_from_response(
        turn,
        negotiation.request().clone(),
        failed_response_raw_text,
        response,
        scope.available_mcp_servers,
        scope.available_mcp_tools,
    );
    let decision = negotiation.advance_provider_response(
        expected_provider,
        &actual_provider,
        execution.is_ok(),
    );
    match (decision, execution) {
        (AgentFailureSummaryResponseDecision::Accept, Ok(execution)) => {
            FailureSummaryResponsePlan::Finish(Box::new(execution))
        }
        (AgentFailureSummaryResponseDecision::RecoverMalformedResponse { attempt }, Err(error)) => {
            let request = maap_repair_request(
                negotiation.request(),
                error.message(),
                &response_raw_text,
                attempt,
            );
            negotiation.replace_request(request);
            FailureSummaryResponsePlan::Continue
        }
        (
            AgentFailureSummaryResponseDecision::RejectProviderIdentity
            | AgentFailureSummaryResponseDecision::RejectMalformedResponse,
            _,
        ) => FailureSummaryResponsePlan::Reject,
        _ => FailureSummaryResponsePlan::Reject,
    }
}

/// Attempts one response-only provider call to summarize a provider failure.
#[cfg(test)]
pub(super) fn summarize_provider_failure_execution<P: ModelProvider>(
    provider: &P,
    turn: &AgentTurnRecord,
    previous_request: &ModelRequest,
    error: &MezError,
) -> Option<AgentTurnExecution> {
    let failed = failed_provider_error_execution(
        previous_request.clone(),
        provider.provider_id(),
        &previous_request.model,
        error,
    );
    summarize_controller_failure_execution(
        provider,
        turn,
        previous_request,
        FailureSummaryInput {
            failed_response: failed.response.clone(),
            error,
            scope: FailureSummaryScope {
                stage: "provider_error",
                available_mcp_servers: &[],
                available_mcp_tools: &[],
            },
        },
    )
}

/// Attempts one response-only provider call to summarize a controller failure.
#[cfg(test)]
pub(super) fn summarize_controller_failure_execution<P: ModelProvider>(
    provider: &P,
    turn: &AgentTurnRecord,
    previous_request: &ModelRequest,
    input: FailureSummaryInput<'_>,
) -> Option<AgentTurnExecution> {
    let mut negotiation = AgentFailureSummaryNegotiation::new(
        failure_summary_request(
            previous_request,
            input.scope.stage,
            input.error,
            &input.failed_response.raw_text,
        ),
        FAILURE_SUMMARY_PROVIDER_RETRY_LIMIT,
        FAILURE_SUMMARY_MAAP_REPAIR_LIMIT,
    );
    loop {
        let response = match provider.send_request(negotiation.request()) {
            Ok(response) => response,
            Err(error) if advance_failure_summary_provider_failure(&mut negotiation, &error) => {
                continue;
            }
            Err(_) => return None,
        };
        match advance_failure_summary_response(
            &mut negotiation,
            provider.provider_id(),
            turn,
            &input.failed_response.raw_text,
            response,
            input.scope,
        ) {
            FailureSummaryResponsePlan::Finish(execution) => return Some(*execution),
            FailureSummaryResponsePlan::Continue => {}
            FailureSummaryResponsePlan::Reject => return None,
        }
    }
}

/// Attempts one response-only provider call to summarize a provider failure.
pub(super) async fn summarize_provider_failure_execution_async<P: AsyncModelProvider>(
    provider: &P,
    turn: &AgentTurnRecord,
    previous_request: &ModelRequest,
    error: &MezError,
) -> Option<AgentTurnExecution> {
    let failed = failed_provider_error_execution(
        previous_request.clone(),
        provider.provider_id(),
        &previous_request.model,
        error,
    );
    summarize_controller_failure_execution_async(
        provider,
        turn,
        previous_request,
        FailureSummaryInput {
            failed_response: failed.response.clone(),
            error,
            scope: FailureSummaryScope {
                stage: "provider_error",
                available_mcp_servers: &[],
                available_mcp_tools: &[],
            },
        },
    )
    .await
}

/// Attempts one response-only provider call to summarize a controller failure.
pub(super) async fn summarize_controller_failure_execution_async<P: AsyncModelProvider>(
    provider: &P,
    turn: &AgentTurnRecord,
    previous_request: &ModelRequest,
    input: FailureSummaryInput<'_>,
) -> Option<AgentTurnExecution> {
    let mut negotiation = AgentFailureSummaryNegotiation::new(
        failure_summary_request(
            previous_request,
            input.scope.stage,
            input.error,
            &input.failed_response.raw_text,
        ),
        FAILURE_SUMMARY_PROVIDER_RETRY_LIMIT,
        FAILURE_SUMMARY_MAAP_REPAIR_LIMIT,
    );
    loop {
        let response = match provider.send_request_async(negotiation.request()).await {
            Ok(response) => response,
            Err(error) if advance_failure_summary_provider_failure(&mut negotiation, &error) => {
                continue;
            }
            Err(_) => return None,
        };
        match advance_failure_summary_response(
            &mut negotiation,
            provider.provider_id(),
            turn,
            &input.failed_response.raw_text,
            response,
            input.scope,
        ) {
            FailureSummaryResponsePlan::Finish(execution) => return Some(*execution),
            FailureSummaryResponsePlan::Continue => {}
            FailureSummaryResponsePlan::Reject => return None,
        }
    }
}
