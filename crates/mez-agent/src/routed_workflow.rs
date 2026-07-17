//! Provider-independent state for runtime-managed routed worker workflows.
//!
//! Routing uses one persistent child session for worker execution and handoff
//! collection while the parent remains bound to its ordinary model profile.
//! This module defines the durable phase, identity, idempotency, and handoff
//! contracts without depending on product runtime or provider adapters.

use serde::{Deserialize, Serialize};

/// Version of the structured routed-worker handoff contract.
pub const ROUTED_HANDOFF_VERSION: u32 = 1;

/// Maximum number of corrective handoff requests allowed after invalid output.
pub const ROUTED_HANDOFF_REPAIR_LIMIT: u8 = 1;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
