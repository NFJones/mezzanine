//! Provider-independent agent turn orchestration.
//!
//! This module owns the smallest complete turn loop shared by provider and
//! product adapters: request construction from model messages, action-surface
//! validation, recoverable provider retries, action-result replay, transcript
//! persistence, and terminal completion. Concrete provider I/O, permission
//! checks, shell/MCP/filesystem execution, and durable transcript storage stay
//! behind the ports defined here.

use std::error::Error;
use std::fmt;

use serde_json::Value;

use crate::{
    AgentTranscriptEntry, AgentTranscriptRole, AllowedAction, AllowedActionSet, ModelMessage,
    ModelMessageRole, TranscriptPersistence,
};

/// Default number of recoverable provider failures accepted for one turn.
pub const DEFAULT_TURN_RECOVERY_LIMIT: usize = 2;

/// One provider request in a portable agent turn.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentHarnessRequest {
    /// Provider-neutral messages, including action-result replay.
    pub messages: Vec<ModelMessage>,
    /// Concrete MAAP action surface exposed for this request.
    pub allowed_actions: AllowedActionSet,
}

/// One model-authored action accepted by the portable turn harness.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentHarnessAction {
    /// Stable action id within the active turn.
    pub id: String,
    /// Concrete action kind selected from the exposed surface.
    pub action: AllowedAction,
    /// Provider-neutral JSON payload passed to the product action adapter.
    pub payload: Value,
}

/// One provider response consumed by the portable turn harness.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentHarnessResponse {
    /// Model-authored rationale for this action batch.
    pub rationale: String,
    /// Actions selected by the model.
    pub actions: Vec<AgentHarnessAction>,
    /// Whether this response completes the user turn.
    pub final_turn: bool,
}

/// One normalized action result replayed to the provider and transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentHarnessActionResult {
    /// Action id supplied by the provider response.
    pub action_id: String,
    /// Stable MAAP action type.
    pub action_type: String,
    /// Whether the product adapter completed the action successfully.
    pub succeeded: bool,
    /// Bounded model-visible result text.
    pub content: String,
}

/// Terminal output from one complete portable agent turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentHarnessOutcome {
    /// Number of provider requests, including recoverable retries.
    pub provider_request_count: usize,
    /// Number of recoverable provider failures observed.
    pub recovery_count: usize,
    /// Results produced by action execution.
    pub action_results: Vec<AgentHarnessActionResult>,
}

/// Inputs and durable identity for one portable agent turn.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentHarnessTurn {
    /// Durable conversation identity used by transcript persistence.
    pub conversation_id: String,
    /// Active turn identity.
    pub turn_id: String,
    /// Agent identity executing the turn.
    pub agent_id: String,
    /// Pane identity associated with the turn.
    pub pane_id: String,
    /// Creation time applied to projected transcript entries.
    pub created_at_unix_seconds: u64,
    /// Initial provider-neutral messages assembled for the turn.
    pub initial_messages: Vec<ModelMessage>,
    /// Concrete MAAP action surface exposed for the turn.
    pub allowed_actions: AllowedActionSet,
    /// Maximum number of recoverable provider failures accepted.
    pub recovery_limit: usize,
}

/// Provider boundary required by the portable turn harness.
pub trait AgentTurnProvider {
    /// Provider-specific failure type.
    type Error: Error;

    /// Sends one provider-neutral request.
    fn send(&mut self, request: &AgentHarnessRequest) -> Result<AgentHarnessResponse, Self::Error>;

    /// Reports whether a provider failure may be retried in the same turn.
    fn is_recoverable(&self, error: &Self::Error) -> bool;
}

/// Product action-execution boundary required by the portable turn harness.
pub trait AgentActionExecutor {
    /// Product-specific execution failure type.
    type Error: Error;

    /// Executes one validated action and returns model-visible output.
    fn execute(
        &mut self,
        action: &AgentHarnessAction,
    ) -> Result<AgentHarnessActionResult, Self::Error>;
}

/// Stable categories for portable turn failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentHarnessErrorKind {
    /// A provider response violated the exposed MAAP action contract.
    InvalidResponse,
    /// Provider execution failed and recovery was unavailable or exhausted.
    Provider,
    /// A validated action failed at the product execution boundary.
    ActionExecution,
    /// Transcript projection or persistence failed.
    Transcript,
}

/// Failure returned by the portable turn harness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentHarnessError {
    kind: AgentHarnessErrorKind,
    message: String,
}

impl AgentHarnessError {
    fn new(kind: AgentHarnessErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Returns the stable failure category.
    pub fn kind(&self) -> AgentHarnessErrorKind {
        self.kind
    }

    /// Returns the unformatted diagnostic message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for AgentHarnessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for AgentHarnessError {}

/// Executes one complete provider-independent turn through injected ports.
pub fn run_agent_turn<P, A, T>(
    provider: &mut P,
    executor: &mut A,
    transcript: &T,
    turn: AgentHarnessTurn,
) -> Result<AgentHarnessOutcome, AgentHarnessError>
where
    P: AgentTurnProvider,
    A: AgentActionExecutor,
    T: TranscriptPersistence,
    T::Error: Error,
{
    let mut request = AgentHarnessRequest {
        messages: turn.initial_messages,
        allowed_actions: turn.allowed_actions,
    };
    let mut provider_request_count = 0usize;
    let mut recovery_count = 0usize;
    let mut action_results = Vec::new();

    loop {
        provider_request_count = provider_request_count.saturating_add(1);
        let response = match provider.send(&request) {
            Ok(response) => response,
            Err(error)
                if recovery_count < turn.recovery_limit && provider.is_recoverable(&error) =>
            {
                recovery_count = recovery_count.saturating_add(1);
                request.messages.push(ModelMessage {
                    role: ModelMessageRole::Developer,
                    source: crate::ContextSourceKind::RuntimeHint,
                    content: format!(
                        "The provider request failed recoverably: {error}. Retry the current turn."
                    ),
                });
                continue;
            }
            Err(error) => {
                return Err(AgentHarnessError::new(
                    AgentHarnessErrorKind::Provider,
                    format!("provider turn failed: {error}"),
                ));
            }
        };

        validate_response(&response, &request.allowed_actions)?;
        for action in &response.actions {
            let result = executor.execute(action).map_err(|error| {
                AgentHarnessError::new(
                    AgentHarnessErrorKind::ActionExecution,
                    format!("action `{}` failed: {error}", action.id),
                )
            })?;
            request.messages.push(ModelMessage {
                role: ModelMessageRole::Tool,
                source: crate::ContextSourceKind::ActionResult,
                content: action_result_message(&result),
            });
            action_results.push(result);
        }

        if response.final_turn {
            persist_turn_transcript(
                transcript,
                &turn.conversation_id,
                &turn.turn_id,
                &turn.agent_id,
                &turn.pane_id,
                turn.created_at_unix_seconds,
                &request.messages,
                &response,
            )?;
            return Ok(AgentHarnessOutcome {
                provider_request_count,
                recovery_count,
                action_results,
            });
        }
    }
}

fn validate_response(
    response: &AgentHarnessResponse,
    allowed_actions: &AllowedActionSet,
) -> Result<(), AgentHarnessError> {
    if response.rationale.trim().is_empty() {
        return Err(AgentHarnessError::new(
            AgentHarnessErrorKind::InvalidResponse,
            "agent action batch rationale must not be empty",
        ));
    }
    if response.actions.is_empty() && !response.final_turn {
        return Err(AgentHarnessError::new(
            AgentHarnessErrorKind::InvalidResponse,
            "non-final agent action batch must include at least one action",
        ));
    }
    let mut ids = std::collections::BTreeSet::new();
    for action in &response.actions {
        if action.id.trim().is_empty() {
            return Err(AgentHarnessError::new(
                AgentHarnessErrorKind::InvalidResponse,
                "agent action id must not be empty",
            ));
        }
        if !ids.insert(action.id.as_str()) {
            return Err(AgentHarnessError::new(
                AgentHarnessErrorKind::InvalidResponse,
                "agent action batch contains duplicate action ids",
            ));
        }
        if !allowed_actions.contains(action.action) {
            return Err(AgentHarnessError::new(
                AgentHarnessErrorKind::InvalidResponse,
                format!(
                    "agent action `{}` is not present on the allowed action surface",
                    action.action.action_type()
                ),
            ));
        }
    }
    Ok(())
}

fn action_result_message(result: &AgentHarnessActionResult) -> String {
    serde_json::json!({
        "action_id": result.action_id,
        "action_type": result.action_type,
        "succeeded": result.succeeded,
        "content": result.content,
    })
    .to_string()
}

#[allow(clippy::too_many_arguments)]
fn persist_turn_transcript<T: TranscriptPersistence>(
    transcript: &T,
    conversation_id: &str,
    turn_id: &str,
    agent_id: &str,
    pane_id: &str,
    created_at_unix_seconds: u64,
    messages: &[ModelMessage],
    final_response: &AgentHarnessResponse,
) -> Result<(), AgentHarnessError>
where
    T::Error: Error,
{
    let mut sequence = transcript
        .next_sequence(conversation_id)
        .map_err(transcript_error)?
        .unwrap_or(1);
    for message in messages {
        let role = match message.role {
            ModelMessageRole::System | ModelMessageRole::Developer => AgentTranscriptRole::System,
            ModelMessageRole::User => AgentTranscriptRole::User,
            ModelMessageRole::Assistant => AgentTranscriptRole::Assistant,
            ModelMessageRole::Tool => AgentTranscriptRole::Tool,
        };
        let entry = AgentTranscriptEntry {
            conversation_id: conversation_id.to_string(),
            sequence,
            created_at_unix_seconds,
            role,
            turn_id: turn_id.to_string(),
            agent_id: agent_id.to_string(),
            pane_id: pane_id.to_string(),
            content: message.content.clone(),
        };
        entry.validate().map_err(|error| {
            AgentHarnessError::new(AgentHarnessErrorKind::Transcript, error.to_string())
        })?;
        transcript.append(&entry).map_err(transcript_error)?;
        sequence = sequence.saturating_add(1);
    }
    let assistant = AgentTranscriptEntry {
        conversation_id: conversation_id.to_string(),
        sequence,
        created_at_unix_seconds,
        role: AgentTranscriptRole::Assistant,
        turn_id: turn_id.to_string(),
        agent_id: agent_id.to_string(),
        pane_id: pane_id.to_string(),
        content: final_response.rationale.clone(),
    };
    assistant.validate().map_err(|error| {
        AgentHarnessError::new(AgentHarnessErrorKind::Transcript, error.to_string())
    })?;
    transcript.append(&assistant).map_err(transcript_error)
}

fn transcript_error(error: impl Error) -> AgentHarnessError {
    AgentHarnessError::new(
        AgentHarnessErrorKind::Transcript,
        format!("transcript persistence failed: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::io;

    use super::*;
    use crate::ContextSourceKind;

    struct FakeProvider {
        responses: VecDeque<Result<AgentHarnessResponse, io::Error>>,
        requests: Vec<AgentHarnessRequest>,
    }

    impl AgentTurnProvider for FakeProvider {
        type Error = io::Error;

        fn send(
            &mut self,
            request: &AgentHarnessRequest,
        ) -> Result<AgentHarnessResponse, Self::Error> {
            self.requests.push(request.clone());
            self.responses.pop_front().expect("scripted response")
        }

        fn is_recoverable(&self, error: &Self::Error) -> bool {
            error.kind() == io::ErrorKind::TimedOut
        }
    }

    #[derive(Default)]
    struct FakeExecutor {
        actions: Vec<String>,
    }

    impl AgentActionExecutor for FakeExecutor {
        type Error = io::Error;

        fn execute(
            &mut self,
            action: &AgentHarnessAction,
        ) -> Result<AgentHarnessActionResult, Self::Error> {
            self.actions.push(action.id.clone());
            Ok(AgentHarnessActionResult {
                action_id: action.id.clone(),
                action_type: action.action.action_type().to_string(),
                succeeded: true,
                content: "workspace inspected".to_string(),
            })
        }
    }

    #[derive(Default)]
    struct FakeTranscript {
        entries: RefCell<Vec<AgentTranscriptEntry>>,
    }

    impl TranscriptPersistence for FakeTranscript {
        type Error = io::Error;

        fn next_sequence(&self, _conversation_id: &str) -> Result<Option<u64>, Self::Error> {
            Ok(None)
        }

        fn append(&self, entry: &AgentTranscriptEntry) -> Result<(), Self::Error> {
            self.entries.borrow_mut().push(entry.clone());
            Ok(())
        }
    }

    #[test]
    /// Verifies a complete provider-independent turn assembles context,
    /// retries a recoverable provider failure, validates and executes a MAAP
    /// action, replays its result, persists the transcript, and completes.
    fn fake_provider_and_ports_complete_one_agent_turn() {
        let action = AgentHarnessAction {
            id: "inspect-1".to_string(),
            action: AllowedAction::ShellCommand,
            payload: serde_json::json!({"command": "rg --files"}),
        };
        let mut provider = FakeProvider {
            responses: VecDeque::from([
                Err(io::Error::new(io::ErrorKind::TimedOut, "temporary timeout")),
                Ok(AgentHarnessResponse {
                    rationale: "Inspect the workspace".to_string(),
                    actions: vec![action],
                    final_turn: false,
                }),
                Ok(AgentHarnessResponse {
                    rationale: "Workspace inspection completed".to_string(),
                    actions: Vec::new(),
                    final_turn: true,
                }),
            ]),
            requests: Vec::new(),
        };
        let mut executor = FakeExecutor::default();
        let transcript = FakeTranscript::default();
        let outcome = run_agent_turn(
            &mut provider,
            &mut executor,
            &transcript,
            AgentHarnessTurn {
                conversation_id: "conversation-1".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-1".to_string(),
                pane_id: "pane-1".to_string(),
                created_at_unix_seconds: 1,
                initial_messages: vec![ModelMessage {
                    role: ModelMessageRole::User,
                    source: ContextSourceKind::UserInstruction,
                    content: "inspect the workspace".to_string(),
                }],
                allowed_actions: AllowedActionSet::for_capability(crate::AgentCapability::Shell),
                recovery_limit: DEFAULT_TURN_RECOVERY_LIMIT,
            },
        )
        .unwrap();

        assert_eq!(outcome.provider_request_count, 3);
        assert_eq!(outcome.recovery_count, 1);
        assert_eq!(executor.actions, ["inspect-1"]);
        assert!(provider.requests[1].messages.iter().any(|message| {
            message.source == ContextSourceKind::RuntimeHint
                && message.content.contains("temporary timeout")
        }));
        assert!(provider.requests[2].messages.iter().any(|message| {
            message.source == ContextSourceKind::ActionResult
                && message.content.contains("workspace inspected")
        }));
        let entries = transcript.entries.borrow();
        assert!(
            entries
                .iter()
                .any(|entry| entry.role == AgentTranscriptRole::Tool)
        );
        assert_eq!(entries.last().unwrap().role, AgentTranscriptRole::Assistant);
    }
}
