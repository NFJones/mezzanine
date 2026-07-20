//! Provider-independent MAAP action-result contracts.
//!
//! This module owns canonical result statuses, content, errors, construction,
//! serialization, and invariant validation. Product turn and action records
//! provide only stable identity through narrow traits, so concrete execution,
//! permissions, persistence, and product error aggregation stay outside.

use std::error::Error;
use std::fmt;

use crate::{AgentTurnState, PermissionEvaluation};

/// Stable identity needed from a product turn record when constructing results.
pub trait AgentTurnResultIdentity {
    /// Returns the stable turn identifier.
    fn turn_id(&self) -> &str;
    /// Returns the stable agent identifier owning the turn.
    fn agent_id(&self) -> &str;
}

/// Stable identity needed from a model-authored action when constructing results.
pub trait AgentActionResultIdentity {
    /// Returns the action identifier within the active turn.
    fn action_id(&self) -> &str;
    /// Returns the canonical MAAP action type.
    fn action_type(&self) -> &'static str;
}

/// Lifecycle status for one agent action result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionStatus {
    /// The controller rejected malformed or unsupported action input.
    Rejected,
    /// Execution is waiting for an approval or other resumable boundary.
    Blocked,
    /// Policy denied execution.
    Denied,
    /// Execution has started but has not reached a terminal result.
    Running,
    /// Execution completed successfully.
    Succeeded,
    /// Execution completed unsuccessfully.
    Failed,
    /// Execution was cancelled before completion.
    Cancelled,
    /// Execution exceeded its time budget.
    TimedOut,
    /// Execution was interrupted by an external event.
    Interrupted,
}

/// Structured error attached to an unsuccessful action result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionError {
    /// Stable machine-readable error code.
    pub code: String,
    /// Human-readable diagnostic message.
    pub message: String,
    /// Optional machine-readable error details encoded as JSON.
    pub data_json: Option<String>,
}

/// One model-visible content block produced by an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionContentBlock {
    /// Stable MAAP content block type.
    pub block_type: &'static str,
    /// Text carried by this content block.
    pub text: String,
}

impl ActionContentBlock {
    /// Constructs a plain text content block.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            block_type: "text",
            text: text.into(),
        }
    }

    /// Encodes this block as compact MAAP JSON.
    pub fn to_json(&self) -> String {
        serde_json::json!({
            "type": self.block_type,
            "text": self.text,
        })
        .to_string()
    }
}

/// Canonical result of planning or executing one MAAP action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionResult {
    /// MAAP protocol version.
    pub protocol: String,
    /// Turn that owns the action.
    pub turn_id: String,
    /// Agent that owns the turn.
    pub agent_id: String,
    /// Action identifier within the turn.
    pub action_id: String,
    /// Canonical MAAP action type.
    pub action_type: &'static str,
    /// Current action lifecycle status.
    pub status: ActionStatus,
    /// Model-visible result content.
    pub content: Vec<ActionContentBlock>,
    /// Optional structured result details encoded as JSON.
    pub structured_content_json: Option<String>,
    /// Permission evaluation computed from the original policy command.
    ///
    /// This typed internal metadata is retained across approval and dispatch
    /// so sandbox compilation does not need to rematch command text.
    pub permission_evaluation: Option<Box<PermissionEvaluation>>,
    /// Whether this result represents an execution error.
    pub is_error: bool,
    /// Structured error details for unsuccessful results.
    pub error: Option<ActionError>,
}

impl ActionResult {
    /// Returns whether this result has reached a deterministic terminal state.
    ///
    /// Running work and blocked approvals remain controller-owned volatile
    /// state. Every other status is settled evidence that may be committed to
    /// immutable conversation chronology.
    pub fn is_terminal(&self) -> bool {
        !matches!(self.status, ActionStatus::Running | ActionStatus::Blocked)
    }

    /// Attaches the permission evaluation computed while planning this action.
    pub fn with_permission_evaluation(
        mut self,
        permission_evaluation: Option<PermissionEvaluation>,
    ) -> Self {
        self.permission_evaluation = permission_evaluation.map(Box::new);
        self
    }

    /// Constructs a nonterminal result for an action still executing.
    pub fn running(
        turn: &(impl AgentTurnResultIdentity + ?Sized),
        action: &(impl AgentActionResultIdentity + ?Sized),
        content: Vec<String>,
        structured_content_json: Option<String>,
    ) -> Self {
        Self::successful(
            turn,
            action,
            ActionStatus::Running,
            content,
            structured_content_json,
        )
    }

    /// Constructs a successfully completed action result.
    pub fn succeeded(
        turn: &(impl AgentTurnResultIdentity + ?Sized),
        action: &(impl AgentActionResultIdentity + ?Sized),
        content: Vec<String>,
        structured_content_json: Option<String>,
    ) -> Self {
        Self::successful(
            turn,
            action,
            ActionStatus::Succeeded,
            content,
            structured_content_json,
        )
    }

    /// Constructs a resumable blocked result with approval structure.
    pub fn blocked(
        turn: &(impl AgentTurnResultIdentity + ?Sized),
        action: &(impl AgentActionResultIdentity + ?Sized),
        content: Vec<String>,
        structured_content_json: String,
    ) -> Self {
        Self {
            protocol: "maap/1".to_string(),
            turn_id: turn.turn_id().to_string(),
            agent_id: turn.agent_id().to_string(),
            action_id: action.action_id().to_string(),
            action_type: action.action_type(),
            status: ActionStatus::Blocked,
            content: action_text_content_blocks(content),
            structured_content_json: Some(structured_content_json),
            permission_evaluation: None,
            is_error: false,
            error: None,
        }
    }

    /// Constructs an unsuccessful result with a terminal error status.
    ///
    /// Returns an error when `status` denotes running, successful, or blocked
    /// execution because those states cannot carry action errors.
    pub fn failed(
        turn: &(impl AgentTurnResultIdentity + ?Sized),
        action: &(impl AgentActionResultIdentity + ?Sized),
        status: ActionStatus,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> ActionResultContractResult<Self> {
        if matches!(
            status,
            ActionStatus::Running | ActionStatus::Succeeded | ActionStatus::Blocked
        ) {
            return Err(ActionResultContractError::new(
                "failed action result requires an error status",
            ));
        }
        Ok(Self {
            protocol: "maap/1".to_string(),
            turn_id: turn.turn_id().to_string(),
            agent_id: turn.agent_id().to_string(),
            action_id: action.action_id().to_string(),
            action_type: action.action_type(),
            status,
            content: Vec::new(),
            structured_content_json: None,
            permission_evaluation: None,
            is_error: true,
            error: Some(ActionError {
                code: code.into(),
                message: message.into(),
                data_json: None,
            }),
        })
    }

    /// Validates status, error, and structured-content invariants.
    pub fn validate_invariants(&self) -> ActionResultContractResult<()> {
        match self.status {
            ActionStatus::Succeeded | ActionStatus::Running => {
                if self.is_error || self.error.is_some() {
                    return Err(ActionResultContractError::new(
                        "successful or running action results must not carry errors",
                    ));
                }
            }
            ActionStatus::Blocked => {
                if self.is_error || self.error.is_some() || self.structured_content_json.is_none() {
                    return Err(ActionResultContractError::new(
                        "blocked action results must include approval structure without error",
                    ));
                }
            }
            ActionStatus::Rejected
            | ActionStatus::Denied
            | ActionStatus::Failed
            | ActionStatus::Cancelled
            | ActionStatus::TimedOut
            | ActionStatus::Interrupted => {
                if !self.is_error || self.error.is_none() {
                    return Err(ActionResultContractError::new(
                        "error action results must set is_error and include an error",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Returns owned text from each result content block.
    pub fn content_texts(&self) -> Vec<String> {
        self.content
            .iter()
            .map(|block| block.text.clone())
            .collect()
    }

    /// Joins result content blocks with newline separators.
    pub fn content_text(&self) -> String {
        self.content
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Encodes all result content blocks as a compact JSON array.
    pub fn content_json(&self) -> String {
        format!(
            "[{}]",
            self.content
                .iter()
                .map(ActionContentBlock::to_json)
                .collect::<Vec<_>>()
                .join(",")
        )
    }

    fn successful(
        turn: &(impl AgentTurnResultIdentity + ?Sized),
        action: &(impl AgentActionResultIdentity + ?Sized),
        status: ActionStatus,
        content: Vec<String>,
        structured_content_json: Option<String>,
    ) -> Self {
        Self {
            protocol: "maap/1".to_string(),
            turn_id: turn.turn_id().to_string(),
            agent_id: turn.agent_id().to_string(),
            action_id: action.action_id().to_string(),
            action_type: action.action_type(),
            status,
            content: action_text_content_blocks(content),
            structured_content_json,
            permission_evaluation: None,
            is_error: false,
            error: None,
        }
    }
}

/// Converts plain result strings into canonical MAAP text content blocks.
pub fn action_text_content_blocks(content: Vec<String>) -> Vec<ActionContentBlock> {
    content.into_iter().map(ActionContentBlock::text).collect()
}

/// Derives one turn's terminal or resumable state from its current action results.
///
/// Blocked results take precedence over failures, failures take precedence over
/// running work, and an explicit final batch or non-empty all-`complete` batch
/// completes the turn. Empty non-final batches remain running.
pub fn turn_state_from_action_results(
    results: &[ActionResult],
    final_turn: bool,
) -> AgentTurnState {
    if results
        .iter()
        .any(|result| result.status == ActionStatus::Blocked)
    {
        AgentTurnState::Blocked
    } else if results.iter().any(|result| result.is_error) {
        AgentTurnState::Failed
    } else if results
        .iter()
        .any(|result| result.status == ActionStatus::Running)
    {
        AgentTurnState::Running
    } else if final_turn
        || (!results.is_empty()
            && results
                .iter()
                .all(|result| result.action_type == "complete"))
    {
        AgentTurnState::Completed
    } else {
        AgentTurnState::Running
    }
}

/// Result type returned by action-result contract validation.
pub type ActionResultContractResult<T> = Result<T, ActionResultContractError>;

/// Failure returned when an action result violates its MAAP invariants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionResultContractError {
    message: String,
}

impl ActionResultContractError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the unformatted invariant diagnostic.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ActionResultContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ActionResultContractError {}

#[cfg(test)]
mod tests {
    use super::*;

    struct Turn;
    impl AgentTurnResultIdentity for Turn {
        fn turn_id(&self) -> &str {
            "turn-1"
        }
        fn agent_id(&self) -> &str {
            "agent-1"
        }
    }

    struct Action;
    impl AgentActionResultIdentity for Action {
        fn action_id(&self) -> &str {
            "action-1"
        }
        fn action_type(&self) -> &'static str {
            "shell_command"
        }
    }

    /// Verifies canonical constructors preserve product-supplied identities,
    /// content serialization, and valid running, blocked, and error states.
    #[test]
    fn action_result_constructors_preserve_contract() {
        let running = ActionResult::running(&Turn, &Action, vec!["pending".to_string()], None);
        assert_eq!(running.turn_id, "turn-1");
        assert_eq!(running.action_type, "shell_command");
        assert_eq!(
            running.content_json(),
            r#"[{"text":"pending","type":"text"}]"#
        );
        assert!(running.validate_invariants().is_ok());

        let blocked = ActionResult::blocked(
            &Turn,
            &Action,
            vec!["approval required".to_string()],
            r#"{"approval":"pending"}"#.to_string(),
        );
        assert!(blocked.validate_invariants().is_ok());

        let failed = ActionResult::failed(
            &Turn,
            &Action,
            ActionStatus::Failed,
            "execution_failed",
            "command failed",
        )
        .unwrap();
        assert!(failed.validate_invariants().is_ok());
    }

    /// Verifies unsuccessful construction and invariant validation reject
    /// contradictory status and error combinations at the lower-crate boundary.
    #[test]
    fn action_result_contract_rejects_invalid_status_combinations() {
        assert!(
            ActionResult::failed(
                &Turn,
                &Action,
                ActionStatus::Succeeded,
                "invalid",
                "invalid",
            )
            .is_err()
        );
        let mut running = ActionResult::running(&Turn, &Action, Vec::new(), None);
        running.is_error = true;
        assert!(running.validate_invariants().is_err());
    }

    #[test]
    /// Verifies action result invariants match status.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    fn action_result_invariants_match_status() {
        let running = ActionResult::running(
            &Turn,
            &Action,
            vec!["accepted".to_string()],
            Some("{\"command\":\"pwd\"}".to_string()),
        );
        let succeeded = ActionResult::succeeded(
            &Turn,
            &Action,
            vec!["ok".to_string()],
            Some("{\"command\":\"pwd\"}".to_string()),
        );
        let blocked = ActionResult::blocked(
            &Turn,
            &Action,
            vec!["approval pending".to_string()],
            "{\"approval\":{\"state\":\"pending\"}}".to_string(),
        );
        let failed = ActionResult::failed(
            &Turn,
            &Action,
            ActionStatus::Denied,
            "policy_forbidden",
            "command denied",
        )
        .unwrap();

        running.validate_invariants().unwrap();
        succeeded.validate_invariants().unwrap();
        blocked.validate_invariants().unwrap();
        failed.validate_invariants().unwrap();
    }

    /// Verifies canonical turn-state derivation preserves blocked and failed
    /// precedence while rejecting empty non-final batches as completion.
    #[test]
    fn action_results_derive_agent_turn_state() {
        assert_eq!(
            turn_state_from_action_results(&[], false),
            AgentTurnState::Running
        );
        let complete = ActionResult::succeeded(&Turn, &CompleteAction, Vec::new(), None);
        assert_eq!(
            turn_state_from_action_results(&[complete], false),
            AgentTurnState::Completed
        );
        let failed =
            ActionResult::failed(&Turn, &Action, ActionStatus::Failed, "failed", "failed").unwrap();
        let blocked = ActionResult::blocked(&Turn, &Action, Vec::new(), "{}".to_string());
        assert_eq!(
            turn_state_from_action_results(&[failed, blocked], true),
            AgentTurnState::Blocked
        );
    }

    struct CompleteAction;
    impl AgentActionResultIdentity for CompleteAction {
        fn action_id(&self) -> &str {
            "complete-1"
        }
        fn action_type(&self) -> &'static str {
            "complete"
        }
    }
}
