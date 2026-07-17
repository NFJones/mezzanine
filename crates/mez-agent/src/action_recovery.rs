//! Provider-independent MAAP action recovery policy.
//!
//! This module validates provider-emitted actions against the active request
//! surface and shapes non-effecting capability and repair continuations.
//! Product turn runners retain provider transport, retry budgets, durable
//! request promotion, and product-error projection.

use std::fmt;

use crate::{
    AgentActionPayload, AgentCapability, AgentTurnNegotiation, AllowedAction, AllowedActionSet,
    CapabilityAvailability, CapabilityRequest, ContextSourceKind, MaapBatch, ModelInteractionKind,
    ModelMessage, ModelMessageRole, ModelRequest, constrain_skill_actions_for_loaded_context,
    continuation_surface, decide_capabilities,
};

/// Maximum previous-response bytes included in one ephemeral MAAP repair prompt.
const MAAP_REPAIR_RAW_TEXT_LIMIT_BYTES: usize = 12 * 1024;

/// Result returned by provider-independent action recovery policy.
pub type ActionRecoveryResult<T> = Result<T, ActionRecoveryError>;

/// A deterministic action-surface or capability-negotiation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionRecoveryError {
    message: String,
}

impl ActionRecoveryError {
    /// Creates an invalid action-recovery failure with a stable diagnostic.
    fn invalid_args(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the stable model-facing diagnostic.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ActionRecoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ActionRecoveryError {}

/// Borrowed canonical inputs used to plan one provider batch continuation.
#[derive(Debug, Clone, Copy)]
pub struct BatchContinuationInput<'a> {
    /// Request that produced the current provider response.
    pub response_request: &'a ModelRequest,
    /// Raw provider response retained for an ephemeral repair excerpt.
    pub response_raw_text: &'a str,
    /// Parsed provider-authored MAAP batch.
    pub batch: &'a MaapBatch,
    /// Active request whose concrete action surface must be enforced.
    pub active_request: &'a ModelRequest,
}

/// Product validation failure retained across lower continuation planning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchValidationFailure<Error> {
    /// Product error returned unchanged to the composition adapter.
    pub error: Error,
    /// Stable model-facing diagnostic used to construct repair context.
    pub message: String,
}

impl<Error> BatchValidationFailure<Error> {
    /// Preserves one product validation error and its model-facing diagnostic.
    pub fn new(error: Error, message: impl Into<String>) -> Self {
        Self {
            error,
            message: message.into(),
        }
    }
}

/// Non-terminal continuation decision for one parsed MAAP batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchContinuationPlan {
    /// Send another capability or repair request before executing effects.
    Continue(Box<ModelRequest>),
    /// Execute the validated batch through product runtime adapters.
    Execute,
}

/// Error retained when batch continuation cannot recover within its budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchContinuationError<ProductError> {
    /// Lower-owned action-surface or capability-negotiation failure.
    Recovery(ActionRecoveryError),
    /// Product-owned MAAP validation failure.
    Product(ProductError),
}

/// Terminal rejection of one provider batch after recovery is exhausted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchContinuationRejection<ProductError> {
    /// Original lower or product validation error.
    pub error: BatchContinuationError<ProductError>,
    /// Stable failure-summary stage identifying the rejected boundary.
    pub stage: &'static str,
}

/// Validates that the provider emitted only actions exposed in the active
/// request schema.
pub fn validate_batch_allowed_actions(
    batch: &MaapBatch,
    request: &ModelRequest,
) -> ActionRecoveryResult<()> {
    for action in &batch.actions {
        let action_type = action.action_type();
        let Some(allowed_action) = AllowedAction::from_action_type(action_type) else {
            return Err(ActionRecoveryError::invalid_args(format!(
                "maap action type {action_type} is not part of the provider action surface"
            )));
        };
        if !request.allowed_actions.contains(allowed_action) {
            return Err(ActionRecoveryError::invalid_args(format!(
                "maap action type {action_type} is not allowed during {} interaction",
                request.interaction_kind.as_str()
            )));
        }
    }
    Ok(())
}

/// Builds a capability continuation when a valid action is absent from the
/// current action surface.
pub fn disallowed_action_capability_continuation_request(
    previous_request: &ModelRequest,
    batch: &MaapBatch,
    validation_error: &str,
) -> Option<ModelRequest> {
    if !previous_request
        .allowed_actions
        .contains(AllowedAction::RequestCapability)
    {
        return None;
    }

    let mut requests = Vec::new();
    for action in &batch.actions {
        let action_type = action.action_type();
        let allowed_action = AllowedAction::from_action_type(action_type)?;
        if previous_request.allowed_actions.contains(allowed_action) {
            continue;
        }
        let capability = capability_for_allowed_action(allowed_action)?;
        if requests
            .iter()
            .any(|request: &CapabilityRequest| request.capability == capability)
        {
            continue;
        }
        requests.push(CapabilityRequest {
            capability,
            reason: format!(
                "needed to retry `{action_type}` after current action-surface validation rejected it: {validation_error}"
            ),
        });
    }

    if requests.is_empty() {
        return None;
    }

    let mut request = capability_continuation_request(previous_request, &requests);
    request.messages.push(ModelMessage {
        role: ModelMessageRole::Developer,
        source: ContextSourceKind::DeveloperInstruction,
        placement: crate::ContextPlacement::StablePrefix,
        content: format!(
            "[disallowed action capability recovery]\n\
             The previous provider response used an action type outside the current allowed action surface. \
             Mezzanine converted the rejected action into capability routing before running a terminal failure summary. \
             Re-emit the work as one valid action batch on the current allowed action surface.\n\
             validation_error={validation_error}"
        ),
    });
    Some(request)
}

/// Extracts capability requests from a non-executing capability negotiation.
///
/// A provider schema may allow the model to include a visible `say` alongside
/// one or more `request_capability` actions. Executable or blocking work is
/// rejected so the controller can update the action surface before effects run.
pub fn capability_requests_from_batch(
    batch: &MaapBatch,
) -> ActionRecoveryResult<Option<Vec<CapabilityRequest>>> {
    let (requests, incompatible_actions) = capability_requests_and_incompatible_actions(batch);
    if requests.is_empty() {
        return Ok(None);
    }
    if !incompatible_actions.is_empty() {
        let action_list = incompatible_actions.join(",");
        return Err(ActionRecoveryError::invalid_args(format!(
            "request_capability may only be combined with say actions; incompatible actions: {action_list}"
        )));
    }
    Ok(Some(requests))
}

/// Builds a recovery continuation for mixed capability and execution batches.
pub fn mixed_capability_continuation_request(
    previous_request: &ModelRequest,
    batch: &MaapBatch,
) -> Option<ModelRequest> {
    let (requests, incompatible_actions) = capability_requests_and_incompatible_actions(batch);
    if requests.is_empty() || incompatible_actions.is_empty() {
        return None;
    }
    let mut request = capability_continuation_request(previous_request, &requests);
    request.messages.push(ModelMessage {
        role: ModelMessageRole::Developer,
        source: ContextSourceKind::DeveloperInstruction,
        placement: crate::ContextPlacement::StablePrefix,
        content: format!(
            "[mixed capability batch recovery]\n\
             The previous provider response combined request_capability with non-say actions: {}. \
             Mezzanine treated that response as a capability request and did not execute any action from the mixed batch. \
             Re-emit the deferred work as one valid action batch on the current allowed action surface; \
             if a deferred action is now allowed and still useful, emit it again as a fresh action.",
            incompatible_actions.join(",")
        ),
    });
    Some(request)
}

/// Builds the next provider request after a non-executing capability request.
pub fn capability_continuation_request(
    previous_request: &ModelRequest,
    requests: &[CapabilityRequest],
) -> ModelRequest {
    let decisions = decide_capabilities(
        requests,
        CapabilityAvailability {
            mcp_available: !previous_request.available_mcp_tools.is_empty(),
            memory_enabled: previous_request.memory_actions_enabled,
            issues_enabled: previous_request.issue_actions_enabled,
        },
    );
    let (interaction_kind, allowed_actions) = continuation_surface(
        previous_request.interaction_kind,
        &previous_request.allowed_actions,
        &decisions,
    );
    let mut request = previous_request.clone();
    request.interaction_kind = interaction_kind;
    request.allowed_actions = allowed_actions;
    constrain_skill_actions_for_loaded_context(&mut request);

    let content = if decisions.len() == 1 {
        capability_decision_message(&requests[0], &decisions[0], &request.allowed_actions)
    } else {
        let mut lines = vec!["[capability decisions]".to_string()];
        for (capability_request, decision) in requests.iter().zip(&decisions) {
            lines.push(format!(
                "- capability={} decision={} reason={} controller_reason={}",
                capability_request.capability.as_str(),
                if decision.granted {
                    "granted"
                } else {
                    "denied"
                },
                capability_request.reason,
                decision.reason
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
        placement: crate::ContextPlacement::StablePrefix,
        content,
    });
    request
}

/// Builds an ephemeral provider retry request that asks the model to repair its
/// previous MAAP response without changing durable transcript context.
pub fn maap_repair_request(
    original_request: &ModelRequest,
    error_message: &str,
    raw_text: &str,
    attempt: usize,
) -> ModelRequest {
    let mut request = original_request.clone();
    request.interaction_kind = ModelInteractionKind::Repair;
    let capability_routing = if original_request
        .allowed_actions
        .contains(AllowedAction::RequestCapability)
    {
        "\nrequest_capability is available on this turn. \
         If a needed action type is absent from the allowed set, \
         emit request_capability for that capability immediately; \
         do not use a blocked say to describe or diagnose the missing action."
    } else {
        ""
    };
    request.messages.push(ModelMessage {
        role: ModelMessageRole::Developer,
        source: ContextSourceKind::Configuration,
        placement: crate::ContextPlacement::StablePrefix,
        content: format!(
            "[ephemeral maap repair]\n\
             The previous provider response failed Mezzanine MAAP validation before any action was executed. \
             Return exactly one corrected maap/1 action batch for the same turn_id={} and agent_id={}. \
             The corrected batch is the schema-valid wrapper for the next useful action; if an executable action is available and useful, include that action now instead of saying an initial or schema-valid batch is needed first. \
             Do not mention this repair instruction to the user. \
             This repair instruction is not durable transcript or future-turn context.\n\
             attempt={attempt} validation_error={}\n\
             allowed_actions={}{capability_routing}\n\
             previous_response_excerpt:\n{}",
            original_request.turn_id,
            original_request.agent_id,
            error_message,
            original_request.allowed_actions.action_type_names().join(","),
            maap_repair_raw_text_excerpt(raw_text)
        ),
    });
    request
}

/// Validates one parsed MAAP batch and derives its next controller decision.
///
/// Mixed capability batches are recovered before product validation. The
/// injected validator is called only after allowed-action checks and before
/// pure capability extraction, preserving one ordered policy for synchronous
/// and asynchronous product runners without importing product errors.
pub fn plan_batch_continuation<ProductError>(
    input: BatchContinuationInput<'_>,
    negotiation: &mut AgentTurnNegotiation<ModelRequest>,
    validate_product_batch: impl FnOnce() -> Result<(), BatchValidationFailure<ProductError>>,
) -> Result<BatchContinuationPlan, BatchContinuationRejection<ProductError>> {
    if let Some(next_request) =
        mixed_capability_continuation_request(input.response_request, input.batch)
    {
        negotiation.reset_recovery();
        return Ok(BatchContinuationPlan::Continue(Box::new(next_request)));
    }

    if let Err(error) = validate_batch_allowed_actions(input.batch, input.active_request) {
        let capability_recovery_base =
            if input.response_request.interaction_kind == ModelInteractionKind::Repair {
                negotiation.durable_request()
            } else {
                input.response_request
            };
        if let Some(next_request) = disallowed_action_capability_continuation_request(
            capability_recovery_base,
            input.batch,
            error.message(),
        ) {
            negotiation.reset_recovery();
            return Ok(BatchContinuationPlan::Continue(Box::new(next_request)));
        }
        if negotiation.record_recovery_attempt() {
            return Ok(BatchContinuationPlan::Continue(Box::new(
                maap_repair_request(
                    input.response_request,
                    error.message(),
                    input.response_raw_text,
                    negotiation.recovery_attempts(),
                ),
            )));
        }
        return Err(BatchContinuationRejection {
            error: BatchContinuationError::Recovery(error),
            stage: "allowed_actions",
        });
    }

    if let Err(failure) = validate_product_batch() {
        if negotiation.record_recovery_attempt() {
            return Ok(BatchContinuationPlan::Continue(Box::new(
                maap_repair_request(
                    input.response_request,
                    &failure.message,
                    input.response_raw_text,
                    negotiation.recovery_attempts(),
                ),
            )));
        }
        return Err(BatchContinuationRejection {
            error: BatchContinuationError::Product(failure.error),
            stage: "maap_validation",
        });
    }

    match capability_requests_from_batch(input.batch) {
        Ok(Some(capability_requests)) => {
            negotiation.reset_recovery();
            Ok(BatchContinuationPlan::Continue(Box::new(
                capability_continuation_request(input.response_request, &capability_requests),
            )))
        }
        Ok(None) => Ok(BatchContinuationPlan::Execute),
        Err(error) if negotiation.record_recovery_attempt() => Ok(BatchContinuationPlan::Continue(
            Box::new(maap_repair_request(
                input.response_request,
                error.message(),
                input.response_raw_text,
                negotiation.recovery_attempts(),
            )),
        )),
        Err(error) => Err(BatchContinuationRejection {
            error: BatchContinuationError::Recovery(error),
            stage: "capability_negotiation",
        }),
    }
}

/// Returns the coarse capability that exposes one concrete action type.
fn capability_for_allowed_action(action: AllowedAction) -> Option<AgentCapability> {
    match action {
        AllowedAction::ShellCommand | AllowedAction::ApplyPatch => Some(AgentCapability::Shell),
        AllowedAction::WebSearch => Some(AgentCapability::NetworkSearch),
        AllowedAction::FetchUrl => Some(AgentCapability::NetworkFetch),
        AllowedAction::McpCall => Some(AgentCapability::Mcp),
        AllowedAction::SendMessage | AllowedAction::SpawnAgent => Some(AgentCapability::Subagent),
        AllowedAction::ConfigChange => Some(AgentCapability::ConfigChange),
        AllowedAction::MemorySearch | AllowedAction::MemoryStore => Some(AgentCapability::Memory),
        AllowedAction::IssueAdd
        | AllowedAction::IssueUpdate
        | AllowedAction::IssueQuery
        | AllowedAction::IssueDelete => Some(AgentCapability::Issues),
        AllowedAction::Say => Some(AgentCapability::RespondOnly),
        AllowedAction::RequestCapability
        | AllowedAction::RequestSkills
        | AllowedAction::CallSkill => None,
    }
}

/// Splits one MAAP batch into capability requests and non-say actions.
fn capability_requests_and_incompatible_actions(
    batch: &MaapBatch,
) -> (Vec<CapabilityRequest>, Vec<String>) {
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
    (requests, incompatible_actions)
}

/// Renders one capability decision into stable developer context.
fn capability_decision_message(
    capability_request: &CapabilityRequest,
    decision: &crate::CapabilityDecision,
    allowed_actions: &AllowedActionSet,
) -> String {
    format!(
        "[capability {}]\ncapability={}\nreason={}\ncontroller_reason={}\nallowed_actions={}",
        if decision.granted {
            "granted"
        } else {
            "denied"
        },
        capability_request.capability.as_str(),
        capability_request.reason,
        decision.reason,
        allowed_actions.action_type_names().join(",")
    )
}

/// Returns a bounded UTF-8-safe excerpt of invalid provider MAAP output.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentAction, SayStatus};

    /// Builds a minimal capability-decision request for recovery policy tests.
    fn request() -> ModelRequest {
        ModelRequest {
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
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
            memory_actions_enabled: true,
            issue_actions_enabled: true,
            interaction_kind: ModelInteractionKind::CapabilityDecision,
            allowed_actions: AllowedActionSet::capability_decision(),
            stop: None,
            messages: Vec::new(),
        }
    }

    /// Builds one MAAP batch with stable identity for recovery policy tests.
    fn batch(actions: Vec<AgentAction>) -> MaapBatch {
        MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "exercise recovery policy".to_string(),
            thought: None,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            actions,
            final_turn: false,
        }
    }

    /// Builds one shell action that requires a capability beyond the initial
    /// capability-decision surface.
    fn shell_action() -> AgentAction {
        AgentAction {
            id: "action-shell".to_string(),
            rationale: "inspect repository state".to_string(),
            payload: AgentActionPayload::ShellCommand {
                summary: "Inspect repository".to_string(),
                command: "git status --short".to_string(),
                interactive: false,
                stateful: false,
                timeout_ms: None,
            },
        }
    }

    /// Builds one model-authored shell capability request.
    fn shell_capability_action() -> AgentAction {
        AgentAction {
            id: "action-capability".to_string(),
            rationale: "request shell access".to_string(),
            payload: AgentActionPayload::RequestCapability {
                capability: AgentCapability::Shell,
                reason: "inspect repository state".to_string(),
            },
        }
    }

    /// Allowed-action validation rejects actions outside the active request
    /// surface and accepts the same batch once that action is exposed.
    #[test]
    fn allowed_action_validation_tracks_the_active_request_surface() {
        let batch = batch(vec![shell_action()]);
        let mut request = request();

        let error = validate_batch_allowed_actions(&batch, &request).unwrap_err();
        assert!(
            error
                .message()
                .contains("not allowed during capability_decision")
        );

        request
            .allowed_actions
            .extend([AllowedAction::ShellCommand]);
        validate_batch_allowed_actions(&batch, &request).unwrap();
    }

    /// Capability extraction rejects a batch that could execute effects before
    /// the requested surface has been resolved by the controller.
    #[test]
    fn capability_extraction_rejects_mixed_executable_actions() {
        let batch = batch(vec![shell_capability_action(), shell_action()]);

        let error = capability_requests_from_batch(&batch).unwrap_err();

        assert_eq!(
            error.message(),
            "request_capability may only be combined with say actions; incompatible actions: shell_command"
        );
    }

    /// A granted capability continuation preserves request identity, exposes
    /// the corresponding actions, and records the deterministic decision.
    #[test]
    fn capability_continuation_exposes_granted_actions_and_context() {
        let original = request();
        let continuation = capability_continuation_request(
            &original,
            &[CapabilityRequest {
                capability: AgentCapability::Shell,
                reason: "inspect repository state".to_string(),
            }],
        );

        assert_eq!(continuation.turn_id, original.turn_id);
        assert_eq!(
            continuation.interaction_kind,
            ModelInteractionKind::ActionExecution
        );
        assert!(
            continuation
                .allowed_actions
                .contains(AllowedAction::ShellCommand)
        );
        assert!(
            continuation
                .allowed_actions
                .contains(AllowedAction::ApplyPatch)
        );
        assert!(
            continuation
                .messages
                .last()
                .unwrap()
                .content
                .contains("[capability granted]")
        );
    }

    /// A disallowed executable action is converted into capability routing when
    /// the current request surface permits capability negotiation.
    #[test]
    fn disallowed_action_recovery_routes_through_capability_policy() {
        let continuation = disallowed_action_capability_continuation_request(
            &request(),
            &batch(vec![shell_action()]),
            "shell_command is not allowed",
        )
        .unwrap();

        assert!(
            continuation
                .allowed_actions
                .contains(AllowedAction::ShellCommand)
        );
        assert!(continuation.messages.iter().any(|message| {
            message
                .content
                .contains("[disallowed action capability recovery]")
        }));
    }

    /// Repair requests retain the original request as immutable durable input
    /// while bounding invalid provider text at a valid UTF-8 character boundary.
    #[test]
    fn maap_repair_requests_are_ephemeral_and_utf8_bounded() {
        let original = request();
        let raw_text = "é".repeat(7_000);

        let repair = maap_repair_request(&original, "invalid action batch", &raw_text, 2);

        assert_eq!(
            original.interaction_kind,
            ModelInteractionKind::CapabilityDecision
        );
        assert!(original.messages.is_empty());
        assert_eq!(repair.interaction_kind, ModelInteractionKind::Repair);
        let content = &repair.messages.last().unwrap().content;
        assert!(content.contains("attempt=2 validation_error=invalid action batch"));
        assert!(content.contains("[truncated: original_bytes=14000]"));
        assert!(content.contains("request_capability is available on this turn"));
    }

    /// Capability extraction permits visible say output alongside one or more
    /// non-effecting capability requests.
    #[test]
    fn capability_extraction_allows_say_actions() {
        let say = AgentAction {
            id: "action-say".to_string(),
            rationale: "report routing progress".to_string(),
            payload: AgentActionPayload::Say {
                status: SayStatus::Progress,
                text: "Requesting shell access.".to_string(),
                content_type: "text/plain; charset=utf-8".to_string(),
            },
        };

        let requests = capability_requests_from_batch(&batch(vec![say, shell_capability_action()]))
            .unwrap()
            .unwrap();

        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].capability, AgentCapability::Shell);
    }

    /// Ordered continuation planning handles mixed capability batches before
    /// invoking product validation so no executable action is inspected or run.
    #[test]
    fn batch_continuation_recovers_mixed_capabilities_before_product_validation() {
        let request = request();
        let batch = batch(vec![shell_capability_action(), shell_action()]);
        let mut negotiation = AgentTurnNegotiation::new(request.clone(), 2);

        let plan = plan_batch_continuation(
            BatchContinuationInput {
                response_request: &request,
                response_raw_text: "mixed response",
                batch: &batch,
                active_request: &request,
            },
            &mut negotiation,
            || -> Result<(), BatchValidationFailure<String>> {
                panic!("mixed capability recovery must precede product validation")
            },
        )
        .unwrap();

        let BatchContinuationPlan::Continue(continuation) = plan else {
            panic!("mixed capability batch must produce a continuation")
        };
        assert!(
            continuation
                .messages
                .last()
                .unwrap()
                .content
                .contains("[mixed capability batch recovery]")
        );
    }

    /// Product validation failures retain their original error after the shared
    /// repair budget is consumed while exposing a stable summary stage.
    #[test]
    fn batch_continuation_preserves_product_failure_after_repair_exhaustion() {
        let mut request = request();
        request.interaction_kind = ModelInteractionKind::ActionExecution;
        request
            .allowed_actions
            .extend([AllowedAction::ShellCommand]);
        let batch = batch(vec![shell_action()]);
        let mut negotiation = AgentTurnNegotiation::new(request.clone(), 1);
        let input = BatchContinuationInput {
            response_request: &request,
            response_raw_text: "invalid response",
            batch: &batch,
            active_request: &request,
        };

        let first = plan_batch_continuation(input, &mut negotiation, || {
            Err(BatchValidationFailure::new(
                "product-error",
                "shell validation failed",
            ))
        })
        .unwrap();
        assert!(matches!(first, BatchContinuationPlan::Continue(_)));

        let rejection = plan_batch_continuation(input, &mut negotiation, || {
            Err(BatchValidationFailure::new(
                "product-error",
                "shell validation failed",
            ))
        })
        .unwrap_err();
        assert_eq!(rejection.stage, "maap_validation");
        assert_eq!(
            rejection.error,
            BatchContinuationError::Product("product-error")
        );
    }
}
