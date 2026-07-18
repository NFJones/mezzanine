//! Provider-independent state for runtime-managed routed worker workflows.
//!
//! Routing uses one persistent child session for worker execution and handoff
//! collection while the parent remains bound to its ordinary model profile.
//! This module defines the durable phase, identity, idempotency, and handoff
//! contracts without depending on product runtime or provider adapters.

use serde::{Deserialize, Serialize};

use crate::{
    AgentActionPayload, AgentContext, AgentTurnExecution, AgentTurnState, ContextBlock,
    ContextPlacement, ContextSourceKind, SayStatus, subagent_task_output_for_execution,
};

/// Version of the structured routed-worker handoff contract.
pub const ROUTED_HANDOFF_VERSION: u32 = 1;

/// Maximum serialized size accepted for one routed-worker handoff.
pub const ROUTED_HANDOFF_MAX_BYTES: usize = 16 * 1024;

/// Maximum number of corrective handoff requests allowed after invalid output.
pub const ROUTED_HANDOFF_REPAIR_LIMIT: u8 = 1;

/// Response-only prompt used to collect a structured routed-worker handoff.
pub const ROUTED_HANDOFF_PROMPT: &str = r#"Return all context needed for the main model to present your completed result safely. Emit one final MAAP say action whose text is exactly one JSON object matching this schema: {"version":1,"result_summary":"...","decisions":["..."],"evidence":["..."],"changes":["..."],"validation":["..."],"assumptions":["..."],"unresolved_risks":["..."],"follow_up_context":["..."]}. Do not perform more work or call tools."#;

/// Response-only prompt used for the single bounded handoff repair attempt.
pub const ROUTED_HANDOFF_REPAIR_PROMPT: &str = r#"Your previous handoff was invalid. Emit one final MAAP say action whose text is exactly one valid JSON object with version 1 and string-array fields decisions, evidence, changes, validation, assumptions, unresolved_risks, and follow_up_context, plus string result_summary. Do not use Markdown fences or call tools."#;

/// Lifecycle phase for one runtime-managed routed worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutedWorkflowPhase {
    /// The runtime is waiting for the auto-sizing decision.
    Classifying,
    /// The runtime is creating and binding the managed worker session.
    SpawningWorker,
    /// The original user prompt is running in the managed worker.
    WaitingForWorkerResult,
    /// The worker result is complete and the same child is producing a handoff.
    WaitingForHandoff,
    /// Worker result and handoff context are ready for the parent model.
    ReadyForPresentation,
    /// The main model is producing the user-visible response.
    Presenting,
    /// A routed failure is ready for one bounded main-model explanation.
    ReadyForErrorExplanation,
    /// The main model is producing the one allowed routed failure explanation.
    ExplainingError,
    /// The main-model presentation completed successfully.
    Completed,
    /// The workflow failed and retains a bounded diagnostic.
    Failed,
    /// The workflow was cancelled or interrupted.
    Interrupted,
}

impl RoutedWorkflowPhase {
    /// Returns whether this phase prevents further workflow transitions.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Interrupted)
    }
}

/// Bounded context produced by the routed worker after its exact final result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutedWorkerHandoff {
    /// Structured handoff schema version.
    pub version: u32,
    /// Concise explanation of the worker's result.
    pub result_summary: String,
    /// Material decisions made while completing the task.
    pub decisions: Vec<String>,
    /// Evidence supporting the result.
    pub evidence: Vec<String>,
    /// Files, configuration, or external state changed by the worker.
    pub changes: Vec<String>,
    /// Validation performed by the worker and its outcomes.
    pub validation: Vec<String>,
    /// Assumptions that affect how the result should be presented.
    pub assumptions: Vec<String>,
    /// Known unresolved risks or incomplete work.
    pub unresolved_risks: Vec<String>,
    /// Additional context that may matter on later parent turns.
    pub follow_up_context: Vec<String>,
}

impl RoutedWorkerHandoff {
    /// Validates the schema version and serialized byte bound.
    ///
    /// Returns an error when the version is unsupported, serialization fails,
    /// or the encoded handoff exceeds `max_bytes`.
    pub fn validate(&self, max_bytes: usize) -> Result<(), String> {
        if self.version != ROUTED_HANDOFF_VERSION {
            return Err(format!(
                "unsupported routed handoff version {}; expected {}",
                self.version, ROUTED_HANDOFF_VERSION
            ));
        }
        let encoded = serde_json::to_vec(self)
            .map_err(|error| format!("failed to encode routed handoff: {error}"))?;
        if encoded.len() > max_bytes {
            return Err(format!(
                "routed handoff is {} bytes; maximum is {max_bytes}",
                encoded.len()
            ));
        }
        Ok(())
    }
}

/// Runtime-independent record for one routed worker workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutedWorkflowState {
    /// Stable workflow identity, equal to the parent turn id.
    pub run_id: String,
    /// Parent agent that owns presentation and visible transcript state.
    pub parent_agent_id: String,
    /// Parent pane that received the original user prompt.
    pub parent_pane_id: String,
    /// Parent conversation fork source.
    pub parent_conversation_id: String,
    /// Number of parent transcript records visible before the current prompt.
    pub parent_transcript_entries: u64,
    /// Original prompt submitted by the user and dispatched once to the worker.
    pub original_user_prompt: String,
    /// Ordinary parent model profile retained for presentation.
    pub main_model_profile: String,
    /// Routed worker profile selected by the classifier.
    pub worker_model_profile: Option<String>,
    /// Persistent managed child agent identity.
    pub child_agent_id: Option<String>,
    /// Ephemeral managed child conversation identity.
    pub child_conversation_id: Option<String>,
    /// Current or most recent managed child turn identity.
    pub child_turn_id: Option<String>,
    /// Exact final result from the worker execution step.
    pub worker_final_result: Option<String>,
    /// Validated same-session handoff context.
    pub handoff: Option<RoutedWorkerHandoff>,
    /// Number of corrective handoff prompts already issued.
    pub handoff_repair_attempts: u8,
    /// Whether the one bounded main-model failure explanation was already queued.
    pub error_explanation_attempted: bool,
    /// Current workflow phase.
    pub phase: RoutedWorkflowPhase,
    /// Bounded failure or interruption diagnostic.
    pub diagnostic: Option<String>,
}

impl RoutedWorkflowState {
    /// Builds the stable idempotency key for worker execution.
    pub fn execute_operation_key(&self) -> String {
        format!("route:{}:execute", self.run_id)
    }

    /// Builds the stable idempotency key for handoff collection.
    pub fn handoff_operation_key(&self) -> String {
        format!("route:{}:handoff", self.run_id)
    }

    /// Builds the stable idempotency key for parent presentation.
    pub fn presentation_operation_key(&self) -> String {
        format!("route:{}:present", self.run_id)
    }

    /// Returns whether another corrective handoff prompt may be issued.
    pub fn can_repair_handoff(&self) -> bool {
        self.handoff_repair_attempts < ROUTED_HANDOFF_REPAIR_LIMIT
    }
}

/// Provider-neutral observation delivered to the routed-workflow reducer.
#[derive(Debug, Clone, Copy)]
pub enum RoutedWorkflowEvent<'a> {
    /// One managed worker or handoff turn settled with a complete execution record.
    ChildSettled(&'a AgentTurnExecution),
    /// The current managed child turn was cancelled by the runtime.
    ChildCancelled,
    /// The parent presentation or failure-explanation request settled.
    PresentationSettled(AgentTurnState),
}

/// Context and diagnostic inputs for one parent-model failure explanation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutedFailurePlan {
    /// Stable workflow stage at which the failure occurred.
    pub stage: String,
    /// Exact worker result retained for the parent when one exists.
    pub child_output: String,
    /// Bounded provider-neutral failure diagnostic.
    pub diagnostic: String,
    /// Additional parent context that must be applied before generic failure context.
    pub parent_context_blocks: Vec<ContextBlock>,
    /// Exact worker result to record in workflow state before recovery, when changed.
    pub worker_final_result_update: Option<String>,
}

/// Provider-neutral effect requested after one managed child turn settles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutedWorkerCompletionPlan {
    /// Queue the response-only structured handoff request in the same managed child.
    RequestHandoff {
        /// Exact successful final worker result.
        worker_final_result: String,
        /// Prompt that the product adapter must queue.
        prompt: &'static str,
        /// Context block appended to the child snapshot before queueing.
        exact_result_block: ContextBlock,
    },
    /// Queue the single bounded response-only handoff repair request.
    RepairHandoff {
        /// Exact successful final worker result retained from the work step.
        worker_final_result: String,
        /// Prompt that the product adapter must queue.
        prompt: &'static str,
        /// Validation diagnostic retained in workflow state.
        diagnostic: String,
        /// Invalid output and feedback blocks appended before queueing.
        context_blocks: Vec<ContextBlock>,
    },
    /// Make the validated worker result available for parent presentation.
    Present {
        /// Exact successful final worker result.
        worker_final_result: String,
        /// Validated structured handoff.
        handoff: RoutedWorkerHandoff,
    },
    /// Recover through the one bounded parent-model failure explanation.
    ExplainFailure(RoutedFailurePlan),
}

/// Provider-neutral state action after a parent presentation turn settles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutedPresentationCompletionPlan {
    /// Successful presentation completed the workflow and may remove durable state.
    Complete,
    /// The bounded failure explanation settled; retain terminal failure state.
    FinishErrorExplanation {
        /// Replacement diagnostic when the explanation itself failed or was interrupted.
        diagnostic: Option<String>,
    },
    /// Queue the one bounded explanation after an initial presentation failure.
    ExplainFailure {
        /// Stable presentation diagnostic retained in workflow state.
        diagnostic: String,
        /// Failure and response-only hint blocks appended to parent context.
        context_blocks: Vec<ContextBlock>,
    },
    /// Settle an unrecoverable or already-explained presentation failure.
    Fail {
        /// Terminal workflow phase corresponding to the parent turn outcome.
        terminal_phase: RoutedWorkflowPhase,
        /// Stable presentation diagnostic retained in workflow state.
        diagnostic: String,
    },
}

/// Typed effect plan returned by the provider-neutral routed-workflow reducer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutedWorkflowTransitionPlan {
    /// Apply one managed-child completion effect in the product runtime.
    Worker(RoutedWorkerCompletionPlan),
    /// Apply phase-specific child-cancellation recovery in the product runtime.
    ChildCancellation(RoutedFailurePlan),
    /// Apply one parent-presentation settlement effect in the product runtime.
    Presentation(RoutedPresentationCompletionPlan),
    /// Ignore a duplicate, stale, non-terminal, or phase-inapplicable observation.
    Ignored,
}

/// Plans the next provider-neutral routed-workflow transition.
///
/// The product adapter remains responsible for obtaining execution records,
/// queueing provider work, mutating scheduler and pane state, persisting
/// transcript entries, and reporting whether each requested effect succeeded.
/// This reducer performs no I/O and never assumes an external effect occurred.
///
/// Returns an error when a child settlement arrives in a phase that cannot own
/// managed child work.
pub fn plan_routed_workflow_transition(
    state: &RoutedWorkflowState,
    event: RoutedWorkflowEvent<'_>,
) -> Result<RoutedWorkflowTransitionPlan, String> {
    match event {
        RoutedWorkflowEvent::ChildSettled(execution)
            if matches!(
                state.phase,
                RoutedWorkflowPhase::WaitingForWorkerResult
                    | RoutedWorkflowPhase::WaitingForHandoff
            ) =>
        {
            plan_routed_child_settlement(state, execution).map(RoutedWorkflowTransitionPlan::Worker)
        }
        RoutedWorkflowEvent::ChildSettled(_) => Ok(RoutedWorkflowTransitionPlan::Ignored),
        RoutedWorkflowEvent::ChildCancelled => Ok(plan_routed_child_cancellation(state)
            .map(RoutedWorkflowTransitionPlan::ChildCancellation)
            .unwrap_or(RoutedWorkflowTransitionPlan::Ignored)),
        RoutedWorkflowEvent::PresentationSettled(terminal_state) => {
            Ok(plan_routed_presentation_settlement(state, terminal_state)
                .map(RoutedWorkflowTransitionPlan::Presentation)
                .unwrap_or(RoutedWorkflowTransitionPlan::Ignored))
        }
    }
}

/// Selects the authoritative terminal output for one routed child execution.
///
/// A successful routed turn prefers the last executed final `say` action.
/// Failed, interrupted, malformed, or result-free turns retain the shared
/// subagent formatter so bounded action and provider diagnostics remain useful.
pub fn routed_child_output_for_execution(execution: &AgentTurnExecution) -> String {
    if execution.terminal_state == AgentTurnState::Completed
        && let Some(batch) = execution.response.action_batch.as_ref()
    {
        for action in batch.actions.iter().rev() {
            if !matches!(
                action.payload,
                AgentActionPayload::Say {
                    status: SayStatus::Final,
                    ..
                }
            ) {
                continue;
            }
            let Some(result) = execution.action_results.iter().find(|result| {
                result.action_id == action.id
                    && result.status == crate::ActionStatus::Succeeded
                    && result.error.is_none()
            }) else {
                continue;
            };
            let output = result
                .content_texts()
                .into_iter()
                .filter(|text| !text.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            if !output.is_empty() {
                return output;
            }
        }
        return "completed without user-facing response".to_string();
    }
    subagent_task_output_for_execution(execution)
}

/// Projects a parent context into the deterministic seed for one routed worker.
///
/// Conversation, skills, messages, and execution evidence are inherited.
/// Product/system/project policy is rebuilt for the child, the active prompt is
/// appended by the queue adapter exactly once, and prior routed handoffs never
/// leak into a new routed job.
pub fn routed_worker_seed_context(
    parent_context: &AgentContext,
    original_user_prompt: &str,
) -> AgentContext {
    let blocks = parent_context
        .blocks()
        .iter()
        .filter(|block| {
            if block.placement == ContextPlacement::EphemeralTail {
                return false;
            }
            if block.source == ContextSourceKind::UserInstruction
                && block.label == "user prompt"
                && block.content == original_user_prompt
            {
                return false;
            }
            !matches!(
                block.source,
                ContextSourceKind::System
                    | ContextSourceKind::DeveloperInstruction
                    | ContextSourceKind::Policy
                    | ContextSourceKind::Configuration
                    | ContextSourceKind::ProjectGuidance
                    | ContextSourceKind::RoutedHandoff
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    if blocks.is_empty() {
        AgentContext::empty()
    } else {
        AgentContext::import_durable_blocks(blocks)
            .expect("routed worker seed context remains durable")
    }
}

/// Parses and validates one structured routed-worker handoff.
///
/// Plain JSON and one optional Markdown fence are accepted. Missing or wrongly
/// typed fields, unknown fields, unsupported versions, and payloads over the
/// fixed byte bound return a stable diagnostic.
pub fn parse_routed_worker_handoff(output: &str) -> Result<RoutedWorkerHandoff, String> {
    let trimmed = output.trim();
    let json = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|value| value.strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(trimmed);
    let handoff: RoutedWorkerHandoff = serde_json::from_str(json)
        .map_err(|error| format!("invalid routed handoff JSON: {error}"))?;
    handoff.validate(ROUTED_HANDOFF_MAX_BYTES)?;
    Ok(handoff)
}

/// Builds parent context for a routed failure explanation.
pub fn routed_failure_context_blocks(
    stage: &str,
    child_output: &str,
    diagnostic: &str,
) -> Vec<ContextBlock> {
    let mut blocks = Vec::new();
    if !child_output.is_empty() {
        blocks.push(routed_exact_result_block(child_output));
    }
    blocks.push(ContextBlock {
        source: ContextSourceKind::RoutedHandoff,
        placement: ContextPlacement::ConversationAppend,
        label: "routed workflow failure".to_string(),
        content: format!("stage={stage}\ndiagnostic={diagnostic}"),
    });
    blocks
}

/// Builds parent context for successful routed-result presentation.
///
/// Returns an error only if the already validated handoff cannot be serialized.
pub fn routed_presentation_context_blocks(
    final_result: &str,
    handoff: Option<&RoutedWorkerHandoff>,
    diagnostic: Option<&str>,
) -> Result<Vec<ContextBlock>, String> {
    let handoff_content = match handoff {
        Some(handoff) => serde_json::to_string(handoff)
            .map_err(|error| format!("failed to encode routed handoff: {error}"))?,
        None => format!(
            "handoff unavailable: {}",
            diagnostic.unwrap_or("worker did not provide valid handoff context")
        ),
    };
    Ok(vec![
        routed_exact_result_block(final_result),
        ContextBlock {
            source: ContextSourceKind::RoutedHandoff,
            placement: ContextPlacement::ConversationAppend,
            label: "routed worker handoff context".to_string(),
            content: handoff_content,
        },
    ])
}

/// Inserts provider-neutral routed blocks at their declared lifecycle boundaries.
pub fn insert_routed_context_blocks(
    context: &mut AgentContext,
    blocks: Vec<ContextBlock>,
) -> crate::AgentContextResult<()> {
    for block in blocks {
        if block.source == ContextSourceKind::RoutedHandoff
            && block.placement == ContextPlacement::ConversationAppend
        {
            context.append_reference_event(block.source, block.label, block.content)?;
        } else {
            context.insert_typed_block(
                block.clone(),
                block.semantic_kind(),
                block.retention(),
                block.recoverable_for_compaction(),
            )?;
        }
    }
    Ok(())
}

/// Builds the canonical exact-worker-result context block.
pub fn routed_exact_result_block(output: &str) -> ContextBlock {
    ContextBlock {
        source: ContextSourceKind::RoutedHandoff,
        placement: ContextPlacement::ConversationAppend,
        label: "routed worker exact final result".to_string(),
        content: output.to_string(),
    }
}

/// Builds separately labeled provider output retained after handoff failure.
pub fn routed_handoff_failure_output_block(output: &str) -> Option<ContextBlock> {
    (!output.is_empty()).then(|| ContextBlock {
        source: ContextSourceKind::RoutedHandoff,
        placement: ContextPlacement::ConversationAppend,
        label: "routed handoff failure output".to_string(),
        content: output.to_string(),
    })
}

/// Plans one managed child settlement without executing runtime effects.
fn plan_routed_child_settlement(
    state: &RoutedWorkflowState,
    execution: &AgentTurnExecution,
) -> Result<RoutedWorkerCompletionPlan, String> {
    let output = routed_child_output_for_execution(execution);
    match state.phase {
        RoutedWorkflowPhase::WaitingForWorkerResult => {
            if execution.terminal_state != AgentTurnState::Completed {
                return Ok(RoutedWorkerCompletionPlan::ExplainFailure(
                    RoutedFailurePlan {
                        stage: "worker".to_string(),
                        child_output: output.clone(),
                        diagnostic: "routed worker failed before handoff".to_string(),
                        parent_context_blocks: Vec::new(),
                        worker_final_result_update: Some(output),
                    },
                ));
            }
            Ok(RoutedWorkerCompletionPlan::RequestHandoff {
                exact_result_block: routed_exact_result_block(&output),
                worker_final_result: output,
                prompt: ROUTED_HANDOFF_PROMPT,
            })
        }
        RoutedWorkflowPhase::WaitingForHandoff => {
            let worker_final_result = state.worker_final_result.clone().unwrap_or_default();
            if execution.terminal_state != AgentTurnState::Completed {
                return Ok(RoutedWorkerCompletionPlan::ExplainFailure(
                    RoutedFailurePlan {
                        stage: "summary request".to_string(),
                        child_output: worker_final_result,
                        diagnostic: "routed handoff provider step failed".to_string(),
                        parent_context_blocks: routed_handoff_failure_output_block(&output)
                            .into_iter()
                            .collect(),
                        worker_final_result_update: None,
                    },
                ));
            }
            match parse_routed_worker_handoff(&output) {
                Ok(handoff) => Ok(RoutedWorkerCompletionPlan::Present {
                    worker_final_result,
                    handoff,
                }),
                Err(diagnostic) if state.can_repair_handoff() => {
                    Ok(RoutedWorkerCompletionPlan::RepairHandoff {
                        worker_final_result,
                        prompt: ROUTED_HANDOFF_REPAIR_PROMPT,
                        context_blocks: routed_handoff_repair_context_blocks(&output, &diagnostic),
                        diagnostic,
                    })
                }
                Err(diagnostic) => Ok(RoutedWorkerCompletionPlan::ExplainFailure(
                    RoutedFailurePlan {
                        stage: "summary parse".to_string(),
                        child_output: worker_final_result,
                        diagnostic,
                        parent_context_blocks: Vec::new(),
                        worker_final_result_update: None,
                    },
                )),
            }
        }
        _ => Err("routed child completed in an unexpected workflow phase".to_string()),
    }
}

/// Builds invalid-output and validation-feedback context for one handoff repair.
fn routed_handoff_repair_context_blocks(output: &str, diagnostic: &str) -> Vec<ContextBlock> {
    vec![
        ContextBlock {
            source: ContextSourceKind::RoutedHandoff,
            placement: ContextPlacement::ConversationAppend,
            label: "invalid routed handoff output".to_string(),
            content: output.to_string(),
        },
        ContextBlock {
            source: ContextSourceKind::RoutedHandoff,
            placement: ContextPlacement::ConversationAppend,
            label: "routed handoff validation feedback".to_string(),
            content: diagnostic.to_string(),
        },
    ]
}

/// Classifies phase-specific child cancellation recovery.
fn plan_routed_child_cancellation(state: &RoutedWorkflowState) -> Option<RoutedFailurePlan> {
    match state.phase {
        RoutedWorkflowPhase::WaitingForWorkerResult => Some(RoutedFailurePlan {
            stage: "worker".to_string(),
            child_output: String::new(),
            diagnostic: "routed worker was cancelled".to_string(),
            parent_context_blocks: Vec::new(),
            worker_final_result_update: None,
        }),
        RoutedWorkflowPhase::WaitingForHandoff => Some(RoutedFailurePlan {
            stage: "summary request".to_string(),
            child_output: state.worker_final_result.clone().unwrap_or_default(),
            diagnostic: "routed handoff was cancelled".to_string(),
            parent_context_blocks: Vec::new(),
            worker_final_result_update: None,
        }),
        _ => None,
    }
}

/// Classifies one settled parent presentation without executing runtime effects.
fn plan_routed_presentation_settlement(
    state: &RoutedWorkflowState,
    terminal_state: AgentTurnState,
) -> Option<RoutedPresentationCompletionPlan> {
    let terminal_phase = match terminal_state {
        AgentTurnState::Completed => RoutedWorkflowPhase::Completed,
        AgentTurnState::Failed => RoutedWorkflowPhase::Failed,
        AgentTurnState::Interrupted => RoutedWorkflowPhase::Interrupted,
        AgentTurnState::Queued | AgentTurnState::Running | AgentTurnState::Blocked => return None,
    };
    if matches!(
        state.phase,
        RoutedWorkflowPhase::ReadyForErrorExplanation | RoutedWorkflowPhase::ExplainingError
    ) {
        let diagnostic = match terminal_state {
            AgentTurnState::Completed => None,
            AgentTurnState::Failed => Some("routed error explanation failed".to_string()),
            AgentTurnState::Interrupted => {
                Some("routed error explanation was interrupted".to_string())
            }
            _ => None,
        };
        return Some(RoutedPresentationCompletionPlan::FinishErrorExplanation { diagnostic });
    }
    if terminal_phase == RoutedWorkflowPhase::Completed {
        return Some(RoutedPresentationCompletionPlan::Complete);
    }
    let diagnostic = match terminal_state {
        AgentTurnState::Failed => "routed parent presentation failed".to_string(),
        AgentTurnState::Interrupted => "routed parent presentation was interrupted".to_string(),
        _ => return None,
    };
    if !state.error_explanation_attempted {
        return Some(RoutedPresentationCompletionPlan::ExplainFailure {
            context_blocks: routed_presentation_failure_context_blocks(&diagnostic),
            diagnostic,
        });
    }
    Some(RoutedPresentationCompletionPlan::Fail {
        terminal_phase,
        diagnostic,
    })
}

/// Builds parent context for one bounded presentation-failure explanation.
fn routed_presentation_failure_context_blocks(diagnostic: &str) -> Vec<ContextBlock> {
    vec![ContextBlock {
        source: ContextSourceKind::RoutedHandoff,
        placement: ContextPlacement::ConversationAppend,
        label: "routed workflow failure".to_string(),
        content: format!("stage=parent presentation\ndiagnostic={diagnostic}"),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE, ActionContentBlock, ActionResult, ActionStatus,
        AgentAction, AllowedActionSet, MaapBatch, ModelInteractionKind, ModelRequest,
        ModelResponse, ModelTokenUsage,
    };
    use std::collections::BTreeMap;

    /// Verifies routed operation keys remain stable across retries and phases.
    #[test]
    fn routed_workflow_operation_keys_are_parent_turn_scoped() {
        let state = state();

        assert_eq!(state.execute_operation_key(), "route:turn-7:execute");
        assert_eq!(state.handoff_operation_key(), "route:turn-7:handoff");
        assert_eq!(state.presentation_operation_key(), "route:turn-7:present");
    }

    /// Verifies valid handoffs accept empty optional detail lists.
    #[test]
    fn routed_handoff_accepts_empty_detail_lists_within_bound() {
        let handoff = handoff();

        assert_eq!(handoff.validate(4096), Ok(()));
    }

    /// Verifies unsupported versions and oversized handoffs are rejected.
    #[test]
    fn routed_handoff_rejects_invalid_version_and_byte_bound() {
        let mut handoff = handoff();
        handoff.version = 2;
        assert!(handoff.validate(4096).unwrap_err().contains("version 2"));

        handoff.version = ROUTED_HANDOFF_VERSION;
        handoff.result_summary = "x".repeat(256);
        assert!(handoff.validate(32).unwrap_err().contains("maximum is 32"));
    }

    /// Verifies only completed, failed, and interrupted workflows are terminal.
    #[test]
    fn routed_workflow_terminal_phases_are_explicit() {
        assert!(!RoutedWorkflowPhase::ReadyForPresentation.is_terminal());
        assert!(RoutedWorkflowPhase::Completed.is_terminal());
        assert!(RoutedWorkflowPhase::Failed.is_terminal());
        assert!(RoutedWorkflowPhase::Interrupted.is_terminal());
    }

    /// Verifies handoff parsing accepts the documented fenced form while
    /// rejecting missing fields, wrong field types, and oversized payloads.
    #[test]
    fn routed_handoff_parser_enforces_the_versioned_bounded_contract() {
        let encoded = serde_json::to_string(&handoff()).unwrap();
        assert_eq!(
            parse_routed_worker_handoff(&format!("```json\n{encoded}\n```")),
            Ok(handoff())
        );
        assert!(
            parse_routed_worker_handoff(r#"{"version":1}"#)
                .unwrap_err()
                .contains("missing field")
        );
        assert!(
            parse_routed_worker_handoff(
                r#"{"version":1,"result_summary":[],"decisions":[],"evidence":[],"changes":[],"validation":[],"assumptions":[],"unresolved_risks":[],"follow_up_context":[]}"#,
            )
            .unwrap_err()
            .contains("invalid type")
        );
        let mut oversized = handoff();
        oversized.result_summary = "x".repeat(ROUTED_HANDOFF_MAX_BYTES);
        assert!(
            parse_routed_worker_handoff(&serde_json::to_string(&oversized).unwrap())
                .unwrap_err()
                .contains("maximum is")
        );
    }

    /// Verifies worker seed projection keeps conversational evidence while
    /// removing rebuilt policy, the active prompt, and prior routed context.
    #[test]
    fn routed_worker_seed_context_has_a_deterministic_policy_boundary() {
        let parent = AgentContext::new_durable(vec![
            context_block(
                ContextSourceKind::Policy,
                ContextPlacement::StablePrefix,
                "product policy",
                "rebuild me",
            ),
            context_block(
                ContextSourceKind::SkillInstruction,
                ContextPlacement::ConversationAppend,
                "loaded skill",
                "retain me",
            ),
            context_block(
                ContextSourceKind::UserInstruction,
                ContextPlacement::ConversationAppend,
                "user prompt",
                "fix routing",
            ),
            context_block(
                ContextSourceKind::TranscriptAssistant,
                ContextPlacement::ConversationAppend,
                "prior answer",
                "retain evidence",
            ),
            context_block(
                ContextSourceKind::RoutedHandoff,
                ContextPlacement::ConversationAppend,
                "prior routed result",
                "do not leak",
            ),
        ])
        .unwrap();

        let seed = routed_worker_seed_context(&parent, "fix routing");
        assert_eq!(
            seed.blocks(),
            vec![
                context_block(
                    ContextSourceKind::SkillInstruction,
                    ContextPlacement::ConversationAppend,
                    "loaded skill",
                    "retain me",
                ),
                context_block(
                    ContextSourceKind::TranscriptAssistant,
                    ContextPlacement::ConversationAppend,
                    "prior answer",
                    "retain evidence",
                ),
            ]
        );
    }

    /// Verifies routed output projection selects only the last successful final
    /// say from a retained batch and ignores progress or failed final actions.
    #[test]
    fn routed_child_output_projection_requires_a_successful_final_say() {
        let progress = say_action("progress", SayStatus::Progress, "still working");
        let failed_final = say_action("failed-final", SayStatus::Final, "unverified result");
        let successful_final = say_action("final", SayStatus::Final, "verified result");
        let execution = execution_with_actions(
            AgentTurnState::Completed,
            vec![
                progress.clone(),
                failed_final.clone(),
                successful_final.clone(),
            ],
            vec![
                action_result(&progress, ActionStatus::Succeeded, "still working"),
                action_result(&failed_final, ActionStatus::Failed, "unverified result"),
                action_result(
                    &successful_final,
                    ActionStatus::Succeeded,
                    "verified result",
                ),
            ],
        );
        assert_eq!(
            routed_child_output_for_execution(&execution),
            "verified result"
        );

        let without_successful_final = execution_with_actions(
            AgentTurnState::Completed,
            vec![progress.clone(), failed_final.clone()],
            vec![
                action_result(&progress, ActionStatus::Succeeded, "still working"),
                action_result(&failed_final, ActionStatus::Failed, "unverified result"),
            ],
        );
        assert_eq!(
            routed_child_output_for_execution(&without_successful_final),
            "completed without user-facing response"
        );
    }

    /// Verifies worker success, worker failure, handoff repair, and exhausted
    /// handoff parsing produce explicit product effect plans in phase order.
    #[test]
    fn routed_child_settlement_transition_table_is_deterministic() {
        let mut worker_state = state();
        worker_state.phase = RoutedWorkflowPhase::WaitingForWorkerResult;
        let final_action = say_action("final", SayStatus::Final, "worker result");
        let completed_worker = execution_with_actions(
            AgentTurnState::Completed,
            vec![final_action.clone()],
            vec![action_result(
                &final_action,
                ActionStatus::Succeeded,
                "worker result",
            )],
        );
        let RoutedWorkflowTransitionPlan::Worker(RoutedWorkerCompletionPlan::RequestHandoff {
            worker_final_result,
            prompt,
            exact_result_block,
        }) = plan_routed_workflow_transition(
            &worker_state,
            RoutedWorkflowEvent::ChildSettled(&completed_worker),
        )
        .unwrap()
        else {
            panic!("completed worker should request handoff")
        };
        assert_eq!(worker_final_result, "worker result");
        assert_eq!(prompt, ROUTED_HANDOFF_PROMPT);
        assert_eq!(exact_result_block.content, "worker result");

        let failed_worker = execution_with_actions(AgentTurnState::Failed, Vec::new(), Vec::new());
        let RoutedWorkflowTransitionPlan::Worker(RoutedWorkerCompletionPlan::ExplainFailure(
            failure,
        )) = plan_routed_workflow_transition(
            &worker_state,
            RoutedWorkflowEvent::ChildSettled(&failed_worker),
        )
        .unwrap()
        else {
            panic!("failed worker should request parent recovery")
        };
        assert_eq!(failure.stage, "worker");
        assert_eq!(
            failure.worker_final_result_update.as_deref(),
            Some("provider output")
        );

        let mut waiting = state();
        waiting.phase = RoutedWorkflowPhase::WaitingForHandoff;
        waiting.worker_final_result = Some("worker result".to_string());
        let malformed_action = say_action("final", SayStatus::Final, "not json");
        let malformed = execution_with_actions(
            AgentTurnState::Completed,
            vec![malformed_action.clone()],
            vec![action_result(
                &malformed_action,
                ActionStatus::Succeeded,
                "not json",
            )],
        );
        let RoutedWorkflowTransitionPlan::Worker(RoutedWorkerCompletionPlan::RepairHandoff {
            prompt,
            diagnostic,
            context_blocks,
            ..
        }) = plan_routed_workflow_transition(
            &waiting,
            RoutedWorkflowEvent::ChildSettled(&malformed),
        )
        .unwrap()
        else {
            panic!("malformed first handoff should request repair")
        };
        assert_eq!(prompt, ROUTED_HANDOFF_REPAIR_PROMPT);
        assert!(diagnostic.contains("invalid routed handoff JSON"));
        assert_eq!(
            context_blocks
                .iter()
                .map(|block| block.label.as_str())
                .collect::<Vec<_>>(),
            vec![
                "invalid routed handoff output",
                "routed handoff validation feedback"
            ]
        );
        waiting.handoff_repair_attempts = ROUTED_HANDOFF_REPAIR_LIMIT;
        assert!(matches!(
            plan_routed_workflow_transition(
                &waiting,
                RoutedWorkflowEvent::ChildSettled(&malformed),
            )
            .unwrap(),
            RoutedWorkflowTransitionPlan::Worker(RoutedWorkerCompletionPlan::ExplainFailure(_))
        ));

        let failed_handoff = execution_with_actions(AgentTurnState::Failed, Vec::new(), Vec::new());
        let RoutedWorkflowTransitionPlan::Worker(RoutedWorkerCompletionPlan::ExplainFailure(
            failure,
        )) = plan_routed_workflow_transition(
            &waiting,
            RoutedWorkflowEvent::ChildSettled(&failed_handoff),
        )
        .unwrap()
        else {
            panic!("failed handoff request should request parent recovery")
        };
        assert_eq!(failure.child_output, "worker result");
        assert_eq!(failure.parent_context_blocks.len(), 1);
        assert_eq!(
            failure.parent_context_blocks[0].label,
            "routed handoff failure output"
        );
        assert_eq!(failure.parent_context_blocks[0].content, "provider output");
    }

    /// Verifies cancellation and presentation outcomes distinguish stale,
    /// recoverable, successful, and terminal workflow transitions.
    #[test]
    fn routed_cancellation_and_presentation_transitions_are_phase_specific() {
        let mut worker_state = state();
        worker_state.phase = RoutedWorkflowPhase::WaitingForWorkerResult;
        let RoutedWorkflowTransitionPlan::ChildCancellation(failure) =
            plan_routed_workflow_transition(&worker_state, RoutedWorkflowEvent::ChildCancelled)
                .unwrap()
        else {
            panic!("active worker cancellation should request recovery")
        };
        assert_eq!(failure.diagnostic, "routed worker was cancelled");

        let mut stale = state();
        stale.phase = RoutedWorkflowPhase::ReadyForPresentation;
        assert_eq!(
            plan_routed_workflow_transition(&stale, RoutedWorkflowEvent::ChildCancelled).unwrap(),
            RoutedWorkflowTransitionPlan::Ignored
        );
        assert_eq!(
            plan_routed_workflow_transition(
                &stale,
                RoutedWorkflowEvent::PresentationSettled(AgentTurnState::Running),
            )
            .unwrap(),
            RoutedWorkflowTransitionPlan::Ignored
        );
        assert_eq!(
            plan_routed_workflow_transition(
                &stale,
                RoutedWorkflowEvent::PresentationSettled(AgentTurnState::Completed),
            )
            .unwrap(),
            RoutedWorkflowTransitionPlan::Presentation(RoutedPresentationCompletionPlan::Complete)
        );
        assert!(matches!(
            plan_routed_workflow_transition(
                &stale,
                RoutedWorkflowEvent::PresentationSettled(AgentTurnState::Failed),
            )
            .unwrap(),
            RoutedWorkflowTransitionPlan::Presentation(
                RoutedPresentationCompletionPlan::ExplainFailure { .. }
            )
        ));

        stale.phase = RoutedWorkflowPhase::ExplainingError;
        stale.error_explanation_attempted = true;
        assert_eq!(
            plan_routed_workflow_transition(
                &stale,
                RoutedWorkflowEvent::PresentationSettled(AgentTurnState::Interrupted),
            )
            .unwrap(),
            RoutedWorkflowTransitionPlan::Presentation(
                RoutedPresentationCompletionPlan::FinishErrorExplanation {
                    diagnostic: Some("routed error explanation was interrupted".to_string())
                }
            )
        );
    }

    fn handoff() -> RoutedWorkerHandoff {
        RoutedWorkerHandoff {
            version: ROUTED_HANDOFF_VERSION,
            result_summary: "done".to_string(),
            decisions: Vec::new(),
            evidence: Vec::new(),
            changes: Vec::new(),
            validation: Vec::new(),
            assumptions: Vec::new(),
            unresolved_risks: Vec::new(),
            follow_up_context: Vec::new(),
        }
    }

    fn state() -> RoutedWorkflowState {
        RoutedWorkflowState {
            run_id: "turn-7".to_string(),
            parent_agent_id: "agent-%1".to_string(),
            parent_pane_id: "%1".to_string(),
            parent_conversation_id: "conversation-1".to_string(),
            parent_transcript_entries: 4,
            original_user_prompt: "implement this".to_string(),
            main_model_profile: "default".to_string(),
            worker_model_profile: None,
            child_agent_id: None,
            child_conversation_id: None,
            child_turn_id: None,
            worker_final_result: None,
            handoff: None,
            handoff_repair_attempts: 0,
            error_explanation_attempted: false,
            phase: RoutedWorkflowPhase::Classifying,
            diagnostic: None,
        }
    }

    fn say_action(id: &str, status: SayStatus, text: &str) -> AgentAction {
        AgentAction {
            id: id.to_string(),
            rationale: "test routed output".to_string(),
            payload: AgentActionPayload::Say {
                status,
                text: text.to_string(),
                content_type: AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            },
        }
    }

    fn action_result(action: &AgentAction, status: ActionStatus, text: &str) -> ActionResult {
        ActionResult {
            protocol: "maap/1".to_string(),
            turn_id: "turn-7".to_string(),
            agent_id: "agent-worker".to_string(),
            action_id: action.id.clone(),
            action_type: "say",
            status,
            content: vec![ActionContentBlock::text(text)],
            structured_content_json: None,
            is_error: status != ActionStatus::Succeeded,
            error: None,
        }
    }

    fn execution_with_actions(
        terminal_state: AgentTurnState,
        actions: Vec<AgentAction>,
        action_results: Vec<ActionResult>,
    ) -> AgentTurnExecution {
        AgentTurnExecution {
            request: ModelRequest {
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
                turn_id: "turn-7".to_string(),
                agent_id: "agent-worker".to_string(),
                available_mcp_tools: Vec::new(),
                memory_actions_enabled: false,
                issue_actions_enabled: false,
                interaction_kind: ModelInteractionKind::ActionExecution,
                allowed_actions: AllowedActionSet::say_only(),
                stop: None,
                messages: Vec::new(),
            },
            response: ModelResponse {
                provider: "test".to_string(),
                model: "test-model".to_string(),
                raw_text: "provider output".to_string(),
                usage: ModelTokenUsage::default(),
                latest_request_usage: None,
                quota_usage: Vec::new(),
                action_batch: (!actions.is_empty()).then(|| MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "test routed transition".to_string(),
                    thought: None,
                    turn_id: "turn-7".to_string(),
                    agent_id: "agent-worker".to_string(),
                    actions,
                    final_turn: true,
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: ModelTokenUsage::default(),
            routing_token_usage_by_model: BTreeMap::new(),
            action_results,
            final_turn: true,
            terminal_state,
        }
    }

    fn context_block(
        source: ContextSourceKind,
        placement: ContextPlacement,
        label: &str,
        content: &str,
    ) -> ContextBlock {
        ContextBlock {
            source,
            placement,
            label: label.to_string(),
            content: content.to_string(),
        }
    }
}
