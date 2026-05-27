//! Provider and controller recovery helpers for agent actions.
//!
//! This module owns non-effecting recovery paths around the main turn runner:
//! capability-continuation requests, ephemeral MAAP repair prompts, provider
//! failure classification, and final failure-summary execution shaping.

#[cfg(test)]
use super::super::ModelProvider;
use super::super::{
    ActionError, ActionResult, ActionStatus, AgentAction, AgentActionPayload, AgentCapability,
    AgentTurnRecord, AgentTurnState, AllowedAction, AllowedActionSet, AsyncModelProvider,
    ContextSourceKind, MaapBatch, McpPromptTool, MezError, ModelInteractionKind, ModelMessage,
    ModelMessageRole, ModelRequest, ModelResponse, ModelTokenUsage, Result, SayStatus,
    action_text_content_blocks, constrain_skill_actions_for_loaded_context, json_escape,
    provider_error_invites_retry, provider_error_is_context_limit_exceeded,
    provider_error_is_output_limit_exceeded,
};
use super::{
    AgentTurnExecution, FAILURE_SUMMARY_RAW_TEXT_LIMIT_BYTES, MAAP_REPAIR_RAW_TEXT_LIMIT_BYTES,
    say_structured_content_json,
};

/// Validates that the provider emitted only actions exposed in the active
/// request schema.
pub(super) fn validate_batch_allowed_actions(
    batch: &MaapBatch,
    request: &ModelRequest,
) -> Result<()> {
    for action in &batch.actions {
        if matches!(action.payload, AgentActionPayload::Complete) {
            continue;
        }
        let action_type = action.action_type();
        let Some(allowed_action) = AllowedAction::from_action_type(action_type) else {
            return Err(MezError::invalid_args(format!(
                "maap action type {action_type} is not part of the provider action surface"
            )));
        };
        if !request.allowed_actions.contains(allowed_action) {
            return Err(MezError::invalid_args(format!(
                "maap action type {action_type} is not allowed during {} interaction",
                request.interaction_kind.as_str()
            )));
        }
    }
    Ok(())
}

/// One model-requested coarse capability and its task-specific reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CapabilityRequest {
    /// Requested coarse capability.
    pub(super) capability: AgentCapability,
    /// Model-authored reason for exposing the capability.
    pub(super) reason: String,
}

/// Extracts capability requests from a non-executing capability negotiation.
///
/// A provider schema may allow the model to include a visible `say` alongside
/// one or more `request_capability` actions during the initial capability
/// decision or later action execution. Treat the batch as one combined
/// capability decision, but reject mixed executable or blocking work so the
/// controller can update the action surface before any effects run.
pub(super) fn capability_requests_from_batch(
    batch: &MaapBatch,
) -> Result<Option<Vec<CapabilityRequest>>> {
    let mut requests = Vec::new();
    let mut incompatible_actions = Vec::new();
    for action in &batch.actions {
        match &action.payload {
            AgentActionPayload::RequestCapability { capability, reason } => {
                requests.push(CapabilityRequest {
                    capability: *capability,
                    reason: reason.clone(),
                });
            }
            AgentActionPayload::Say { .. } => {}
            _ => incompatible_actions.push(action.action_type().to_string()),
        }
    }
    if requests.is_empty() {
        return Ok(None);
    }
    if !incompatible_actions.is_empty() {
        let action_list = incompatible_actions.join(",");
        return Err(MezError::invalid_args(format!(
            "request_capability may only be combined with say actions; incompatible actions: {action_list}"
        )));
    }
    Ok(Some(requests))
}

/// Builds the next provider request after a non-executing capability request.
pub(super) fn capability_continuation_request(
    previous_request: &ModelRequest,
    requests: &[CapabilityRequest],
) -> ModelRequest {
    let decisions = requests
        .iter()
        .map(|request| {
            (
                request,
                capability_decision(previous_request, request.capability),
            )
        })
        .collect::<Vec<_>>();
    let carried_execution_surface =
        previous_request.interaction_kind == ModelInteractionKind::ActionExecution;
    let mut allowed_actions = if carried_execution_surface {
        previous_request.allowed_actions.clone()
    } else {
        AllowedActionSet::action_execution_base()
    };
    for (_, decision) in &decisions {
        if decision.granted {
            allowed_actions.extend_set(&decision.allowed_actions);
        }
    }
    let granted_any = decisions.iter().any(|(_, decision)| decision.granted);
    let mut request = previous_request.clone();
    request.interaction_kind = if granted_any || carried_execution_surface {
        ModelInteractionKind::ActionExecution
    } else {
        ModelInteractionKind::CapabilityDecision
    };
    request.allowed_actions = if granted_any || carried_execution_surface {
        allowed_actions
    } else {
        AllowedActionSet::capability_decision()
    };
    constrain_skill_actions_for_loaded_context(&mut request);
    let content = if decisions.len() == 1 {
        let (capability_request, decision) = &decisions[0];
        format!(
            "[capability {}]\ncapability={}\nreason={}\ncontroller_reason={}\nallowed_actions={}",
            if decision.granted {
                "granted"
            } else {
                "denied"
            },
            capability_request.capability.as_str(),
            capability_request.reason.as_str(),
            decision.reason.as_str(),
            request.allowed_actions.action_type_names().join(",")
        )
    } else {
        let mut lines = vec!["[capability decisions]".to_string()];
        for (capability_request, decision) in &decisions {
            lines.push(format!(
                "- capability={} decision={} reason={} controller_reason={}",
                capability_request.capability.as_str(),
                if decision.granted {
                    "granted"
                } else {
                    "denied"
                },
                capability_request.reason.as_str(),
                decision.reason.as_str()
            ));
        }
        lines.push(format!(
            "allowed_actions={}",
            request.allowed_actions.action_type_names().join(",")
        ));
        lines.join("\n")
    };
    request.messages.push(ModelMessage {
        role: ModelMessageRole::Developer,
        source: ContextSourceKind::DeveloperInstruction,
        content,
    });
    request
}

/// Controller decision for a requested capability.
struct CapabilityDecision {
    /// Whether the capability was granted.
    granted: bool,
    /// Action set for the next request.
    allowed_actions: AllowedActionSet,
    /// Deterministic controller reason.
    reason: String,
}

/// Grants or denies a coarse capability with deterministic policy checks.
///
/// This deliberately does not try to solve the task or validate the eventual
/// concrete action target. Target validation belongs to the action parser,
/// permission layer, and action executor after the capability-specific action
/// surface has been exposed.
fn capability_decision(request: &ModelRequest, capability: AgentCapability) -> CapabilityDecision {
    match capability {
        AgentCapability::Mcp if request.available_mcp_tools.is_empty() => CapabilityDecision {
            granted: false,
            allowed_actions: AllowedActionSet::capability_decision(),
            reason: "mcp capability requires at least one available MCP tool in runtime context"
                .to_string(),
        },
        _ => CapabilityDecision {
            granted: true,
            allowed_actions: AllowedActionSet::for_capability(capability),
            reason: "capability is permitted by deterministic action-surface rules".to_string(),
        },
    }
}

/// Builds a failed execution for capability-negotiation failures that happen
/// before executable actions are available.
pub(super) fn failed_capability_request_execution(
    request: ModelRequest,
    mut response: ModelResponse,
    latest_response_usage: ModelTokenUsage,
    code: &str,
    message: &str,
) -> AgentTurnExecution {
    let turn_id = request.turn_id.clone();
    let agent_id = request.agent_id.clone();
    let original_batch = response.action_batch.as_ref();
    let mut actions = original_batch
        .map(|batch| {
            batch
                .actions
                .iter()
                .filter(|action| {
                    matches!(action.payload, AgentActionPayload::RequestCapability { .. })
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if actions.is_empty() {
        actions.push(AgentAction {
            id: "capability-request".to_string(),
            rationale: message.to_string(),
            payload: AgentActionPayload::RequestCapability {
                capability: AgentCapability::Shell,
                reason: message.to_string(),
            },
        });
    }
    let terminal_batch = MaapBatch {
        protocol: original_batch
            .map(|batch| batch.protocol.clone())
            .unwrap_or_else(|| "maap/1".to_string()),
        rationale: original_batch
            .map(|batch| batch.rationale.clone())
            .filter(|rationale| !rationale.trim().is_empty())
            .unwrap_or_else(|| "capability request failed before execution".to_string()),
        thought: original_batch.and_then(|batch| batch.thought.clone()),
        turn_id: turn_id.clone(),
        agent_id: agent_id.clone(),
        actions,
        final_turn: true,
        next_phase: None,
    };
    let action_results = terminal_batch
        .actions
        .iter()
        .map(|action| ActionResult {
            protocol: "maap/1".to_string(),
            turn_id: turn_id.clone(),
            agent_id: agent_id.clone(),
            action_id: action.id.clone(),
            action_type: action.action_type(),
            status: ActionStatus::Failed,
            content: action_text_content_blocks(vec![message.to_string()]),
            structured_content_json: Some(format!(
                r#"{{"kind":"request_capability","status":"failed","code":"{}","message":"{}"}}"#,
                json_escape(code),
                json_escape(message)
            )),
            is_error: true,
            error: Some(ActionError {
                code: code.to_string(),
                message: message.to_string(),
                data_json: None,
            }),
        })
        .collect();
    response.action_batch = Some(terminal_batch);
    AgentTurnExecution {
        request,
        response,
        latest_response_usage,
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results,
        final_turn: true,
        terminal_state: AgentTurnState::Failed,
    }
}

/// Builds an ephemeral provider retry request that asks the model to repair its
/// previous MAAP response without adding the repair instruction to transcript
/// state or future model context.
pub(super) fn maap_repair_request(
    original_request: &ModelRequest,
    error_message: &str,
    raw_text: &str,
    attempt: usize,
) -> ModelRequest {
    let mut request = original_request.clone();
    request.interaction_kind = ModelInteractionKind::Repair;
    request.messages.push(ModelMessage {
        role: ModelMessageRole::Developer,
        source: ContextSourceKind::Configuration,
        content: format!(
            "[ephemeral maap repair]\n\
             The previous provider response failed Mezzanine MAAP validation before any action was executed. \
             Return exactly one corrected maap/1 action batch for the same turn_id={} and agent_id={}. \
             Do not mention this repair instruction to the user. \
             This repair instruction is not durable transcript or future-turn context.\n\
             attempt={attempt} validation_error={}\n\
             previous_response_excerpt:\n{}",
            original_request.turn_id,
            original_request.agent_id,
            error_message,
            maap_repair_raw_text_excerpt(raw_text)
        ),
    });
    request
}

/// Returns a bounded UTF-8-safe excerpt of invalid provider MAAP output for an
/// ephemeral repair retry.
fn maap_repair_raw_text_excerpt(raw_text: &str) -> String {
    if raw_text.len() <= MAAP_REPAIR_RAW_TEXT_LIMIT_BYTES {
        return raw_text.to_string();
    }
    let mut end = MAAP_REPAIR_RAW_TEXT_LIMIT_BYTES;
    while !raw_text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!(
        "{}\n[truncated: original_bytes={}]",
        &raw_text[..end],
        raw_text.len()
    )
}

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

/// Reports whether a provider error should remain visible to runtime retry
/// handling instead of being converted into a terminal failure summary.
pub(super) fn provider_error_should_retry_without_summary(error: &MezError) -> bool {
    if provider_error_is_context_limit_exceeded(error.message(), error.provider_failure_json()) {
        return true;
    }
    if provider_error_is_output_limit_exceeded(error.message(), error.provider_failure_json()) {
        return true;
    }
    if let Some(status_code) = provider_failure_status_code(error.provider_failure_json()) {
        if status_code == 400
            && (error.message().contains("Unsupported") || error.message().contains("unsupported"))
        {
            return false;
        }
        return status_code == 429 || (500..=599).contains(&status_code);
    }
    if error.kind() == crate::error::MezErrorKind::Io {
        return true;
    }
    if error.kind() != crate::error::MezErrorKind::InvalidState {
        return false;
    }
    let message = error.message();
    message.contains("provider HTTP request failed")
        || message.contains("provider HTTP response read failed")
        || provider_error_invites_retry(message, error.provider_failure_json())
}

/// Extracts an HTTP status code from provider failure diagnostics.
fn provider_failure_status_code(provider_failure_json: Option<&str>) -> Option<u16> {
    let value: serde_json::Value = serde_json::from_str(provider_failure_json?).ok()?;
    let status_code = value.get("status_code")?.as_u64()?;
    u16::try_from(status_code).ok()
}

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

/// Converts a valid summary response into a terminal failed execution.
fn failure_summary_execution_from_response(
    turn: &AgentTurnRecord,
    request: ModelRequest,
    failed_response_raw_text: &str,
    mut response: ModelResponse,
    scope: FailureSummaryScope<'_>,
) -> Result<AgentTurnExecution> {
    let batch = response.action_batch.as_ref().ok_or_else(|| {
        MezError::invalid_args("failure summary response must include a say action batch")
    })?;
    validate_batch_allowed_actions(batch, &request)?;
    batch.validate(turn, scope.available_mcp_servers, scope.available_mcp_tools)?;
    if batch.actions.is_empty()
        || batch
            .actions
            .iter()
            .any(|action| !matches!(action.payload, AgentActionPayload::Say { .. }))
    {
        return Err(MezError::invalid_args(
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
                Some(say_structured_content_json(*status, content_type, text)),
            )),
            _ => Err(MezError::invalid_args(
                "failure summary response must contain only say actions",
            )),
        })
        .collect::<Result<Vec<_>>>()?;
    response.raw_text = format!(
        "{}\ncontroller_failure_summary:\n{}",
        failed_response_raw_text, response.raw_text
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
    let request = failure_summary_request(
        previous_request,
        input.scope.stage,
        input.error,
        &input.failed_response.raw_text,
    );
    let response = provider.send_request(&request).ok()?;
    if response.provider != provider.provider_id() {
        return None;
    }
    failure_summary_execution_from_response(
        turn,
        request,
        &input.failed_response.raw_text,
        response,
        input.scope,
    )
    .ok()
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
    let request = failure_summary_request(
        previous_request,
        input.scope.stage,
        input.error,
        &input.failed_response.raw_text,
    );
    let response = provider.send_request_async(&request).await.ok()?;
    if response.provider != provider.provider_id() {
        return None;
    }
    failure_summary_execution_from_response(
        turn,
        request,
        &input.failed_response.raw_text,
        response,
        input.scope,
    )
    .ok()
}

/// Builds the terminal failed execution for a MAAP response that remained
/// invalid after all ephemeral repair attempts were exhausted.
fn failed_maap_validation_execution(
    request: ModelRequest,
    mut response: ModelResponse,
    latest_response_usage: ModelTokenUsage,
    error: &MezError,
) -> AgentTurnExecution {
    response.raw_text = format!("{}\nmaap_validation_error: {}", response.raw_text, error);
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

/// Builds a terminal failed MAAP validation execution, asking the model for one
/// final user-facing characterization when possible.
#[cfg(test)]
pub(super) fn failed_maap_validation_execution_with_summary<P: ModelProvider>(
    provider: &P,
    turn: &AgentTurnRecord,
    request: ModelRequest,
    response: ModelResponse,
    latest_response_usage: ModelTokenUsage,
    error: &MezError,
    scope: FailureSummaryScope<'_>,
) -> AgentTurnExecution {
    let failed =
        failed_maap_validation_execution(request.clone(), response, latest_response_usage, error);
    summarize_controller_failure_execution(
        provider,
        turn,
        &request,
        FailureSummaryInput {
            failed_response: failed.response.clone(),
            error,
            scope,
        },
    )
    .unwrap_or(failed)
}

/// Builds a terminal failed MAAP validation execution, asking the model for one
/// final user-facing characterization when possible.
pub(super) async fn failed_maap_validation_execution_with_summary_async<P: AsyncModelProvider>(
    provider: &P,
    turn: &AgentTurnRecord,
    request: ModelRequest,
    response: ModelResponse,
    latest_response_usage: ModelTokenUsage,
    error: &MezError,
    scope: FailureSummaryScope<'_>,
) -> AgentTurnExecution {
    let failed =
        failed_maap_validation_execution(request.clone(), response, latest_response_usage, error);
    summarize_controller_failure_execution_async(
        provider,
        turn,
        &request,
        FailureSummaryInput {
            failed_response: failed.response.clone(),
            error,
            scope,
        },
    )
    .await
    .unwrap_or(failed)
}
