//! Hook data types, validation, and focused-shell executor traits.
//!
//! The types module defines hook configuration and execution result shapes while
//! keeping planning, queueing, process execution, and audit emission separate.

use std::collections::VecDeque;

use crate::error::{MezError, Result};
use serde_json::Value;

/// Carries Hook Event state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    /// Represents the Session Start case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    SessionStart,
    /// Represents the Session Stop case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    SessionStop,
    /// Represents the Client Attach case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ClientAttach,
    /// Represents the Client Detach case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ClientDetach,
    /// Represents the Window Create case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    WindowCreate,
    /// Represents the Window Close case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    WindowClose,
    /// Represents the Session Detach case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    SessionDetach,
    /// Represents the Pane Create case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    PaneCreate,
    /// Represents the Pane Close case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    PaneClose,
    /// Represents the User Prompt Submit case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    UserPromptSubmit,
    /// Represents the Agent Turn Start case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    AgentTurnStart,
    /// Represents the Agent Turn Stop case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    AgentTurnStop,
    /// Represents the Pre Shell Command case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    PreShellCommand,
    /// Represents the Post Shell Command case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    PostShellCommand,
    /// Represents the Permission Request case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    PermissionRequest,
    /// Represents the Permission Decision case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    PermissionDecision,
    /// Represents the Pre Mcp Tool Use case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    PreMcpToolUse,
    /// Represents the Post Mcp Tool Use case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    PostMcpToolUse,
    /// Represents the Snapshot Create case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    SnapshotCreate,
    /// Represents the Snapshot Resume case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    SnapshotResume,
}

/// Carries Hook Invocation state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookInvocation {
    /// Represents the Program case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Program {
        /// Stores the command value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        command: String,
        /// Stores the args value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        args: Vec<String>,
    },
    /// Represents the Focused Shell case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    FocusedShell {
        /// Stores the command value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        command: String,
    },
}

/// Carries Hook Definition state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookDefinition {
    /// Stores the id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub id: String,
    /// Stores the event value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub event: HookEvent,
    /// Stores the invocation value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub invocation: HookInvocation,
    /// Stores the enabled value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub enabled: bool,
    /// Stores the required value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub required: bool,
    /// Stores the agent hook value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub agent_hook: bool,
    /// Stores the matcher groups value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub matcher_groups: Vec<HookMatcherGroup>,
    /// Stores the timeout ms value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub timeout_ms: Option<u64>,
    /// Stores the on failure value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub on_failure: Option<HookOnFailure>,
}

/// A group of hook match predicates combined with logical AND.
///
/// Hook definitions match when any configured group matches the structured
/// event payload. Empty groups are treated as non-matching so invalid
/// configuration cannot broaden hook execution silently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookMatcherGroup {
    /// Predicates that must all match within this group.
    pub predicates: Vec<HookMatcherPredicate>,
}

/// One path-based predicate within a hook matcher group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookMatcherPredicate {
    /// Dot path or JSON Pointer used to locate the event payload value.
    pub path: String,
    /// Predicate operation applied to the located value.
    pub operator: HookMatcherOperator,
}

/// Supported predicate operators for hook matcher groups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookMatcherOperator {
    /// Match when the payload value's scalar representation equals this value.
    Equals(String),
    /// Match when the payload value's scalar representation has this prefix.
    Prefix(String),
    /// Match when the payload value's scalar representation has this suffix.
    Suffix(String),
    /// Match when the payload value's scalar representation contains this text.
    Contains(String),
    /// Match based on whether the payload path exists.
    Exists(bool),
}

/// Carries Hook On Failure state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookOnFailure {
    /// Represents the Block case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Block,
    /// Represents the Warn case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Warn,
    /// Represents the Ignore case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Ignore,
}

impl HookDefinition {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn validate(&self) -> Result<()> {
        if self.id.is_empty() {
            return Err(MezError::invalid_args("hook id must not be empty"));
        }
        match &self.invocation {
            HookInvocation::Program { command, .. } if command.trim().is_empty() => Err(
                MezError::invalid_args("program hook command must not be empty"),
            ),
            HookInvocation::FocusedShell { command } if command.trim().is_empty() => Err(
                MezError::invalid_args("focused-shell hook command must not be empty"),
            ),
            _ => Ok(()),
        }
    }

    /// Runs the matches payload operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn matches_payload(&self, event_payload_json: &str) -> Result<bool> {
        if self.matcher_groups.is_empty() {
            return Ok(true);
        }
        let payload = serde_json::from_str::<Value>(event_payload_json).map_err(|error| {
            MezError::invalid_args(format!("hook event payload must be JSON: {error}"))
        })?;
        Ok(self
            .matcher_groups
            .iter()
            .any(|group| group.matches_payload(&payload)))
    }
}

impl HookMatcherGroup {
    /// Runs the matches payload operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn matches_payload(&self, payload: &Value) -> bool {
        !self.predicates.is_empty()
            && self
                .predicates
                .iter()
                .all(|predicate| predicate.matches_payload(payload))
    }
}

impl HookMatcherPredicate {
    /// Runs the matches payload operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn matches_payload(&self, payload: &Value) -> bool {
        let value = hook_payload_value_at_path(payload, &self.path);
        match &self.operator {
            HookMatcherOperator::Exists(expected) => value.is_some() == *expected,
            HookMatcherOperator::Equals(expected) => value
                .and_then(hook_payload_value_string)
                .is_some_and(|actual| actual == *expected),
            HookMatcherOperator::Prefix(expected) => value
                .and_then(hook_payload_value_string)
                .is_some_and(|actual| actual.starts_with(expected)),
            HookMatcherOperator::Suffix(expected) => value
                .and_then(hook_payload_value_string)
                .is_some_and(|actual| actual.ends_with(expected)),
            HookMatcherOperator::Contains(expected) => value
                .and_then(hook_payload_value_string)
                .is_some_and(|actual| actual.contains(expected)),
        }
    }
}

/// Runs the hook payload value at path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn hook_payload_value_at_path<'a>(payload: &'a Value, path: &str) -> Option<&'a Value> {
    if path.starts_with('/') {
        return payload.pointer(path);
    }
    let mut current = payload;
    for segment in path.split('.').filter(|segment| !segment.is_empty()) {
        match current {
            Value::Object(object) => current = object.get(segment)?,
            Value::Array(items) => {
                let index = segment.parse::<usize>().ok()?;
                current = items.get(index)?;
            }
            _ => return None,
        }
    }
    Some(current)
}

/// Runs the hook payload value string operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn hook_payload_value_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        Value::Null => Some("null".to_string()),
        Value::Array(_) | Value::Object(_) => serde_json::to_string(value).ok(),
    }
}

/// Carries Hook Execution Plan state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookExecutionPlan {
    /// Stores the hook id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub hook_id: String,
    /// Stores the event value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub event: HookEvent,
    /// Stores the run in focused shell value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub run_in_focused_shell: bool,
    /// Stores the target pane id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub target_pane_id: Option<String>,
    /// Stores the blocks on shell availability value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub blocks_on_shell_availability: bool,
    /// Stores the program value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub program: Option<String>,
    /// Stores the args value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub args: Vec<String>,
    /// Stores the shell command value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub shell_command: Option<String>,
    /// Stores the event payload json value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub event_payload_json: String,
    /// Stores the timeout ms value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub timeout_ms: u64,
    /// Stores the on failure value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub on_failure: HookOnFailure,
}

/// Carries Hook Event Plan state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookEventPlan {
    /// Stores the plans value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub plans: Vec<HookExecutionPlan>,
}

/// Carries Hook Failure Kind state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookFailureKind {
    /// Represents the Planning case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Planning,
    /// Represents the Spawn case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Spawn,
    /// Represents the Exit Non Zero case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ExitNonZero,
    /// Represents the Timeout case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Timeout,
    /// Represents the Policy Denied case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    PolicyDenied,
    /// Represents the Shell Unavailable case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ShellUnavailable,
}

/// Carries Hook Failure state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookFailure {
    /// Stores the hook id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub hook_id: String,
    /// Stores the event value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub event: HookEvent,
    /// Stores the kind value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub kind: HookFailureKind,
    /// Stores the message value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub message: String,
    /// Stores the retryable value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub retryable: bool,
}

/// Carries Hook Failure Decision state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookFailureDecision {
    /// Represents the Block case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Block,
    /// Represents the Warn case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Warn,
    /// Represents the Ignore case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Ignore,
}

/// Carries Hook Execution Status state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookExecutionStatus {
    /// Represents the Succeeded case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Succeeded,
    /// Represents the Queued case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Queued,
    /// Represents the Failed case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Failed,
    /// Represents the Timed Out case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    TimedOut,
}

/// Carries Hook Execution Result state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookExecutionResult {
    /// Stores the hook id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub hook_id: String,
    /// Stores the event value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub event: HookEvent,
    /// Stores the status value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub status: HookExecutionStatus,
    /// Stores the exit code value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub exit_code: Option<i32>,
    /// Stores the stdout value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub stdout: String,
    /// Stores the stderr value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub stderr: String,
    /// Stores the failure value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub failure: Option<HookFailure>,
}

/// Carries Focused Shell Hook Output state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusedShellHookOutput {
    /// Stores the exit code value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub exit_code: Option<i32>,
    /// Stores the stdout value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub stdout: String,
    /// Stores the stderr value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub stderr: String,
    /// Stores the timed out value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub timed_out: bool,
    /// Stores the shell unavailable value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub shell_unavailable: bool,
    /// Stores the policy denied value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub policy_denied: bool,
}

/// Defines the Focused Shell Executor behavior contract for this subsystem.
///
/// Implementors provide the concrete I/O or state transition boundary
/// consumed by higher-level orchestration code.
pub trait FocusedShellExecutor {
    /// Runs the run hook command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn run_hook_command(&mut self, plan: &HookExecutionPlan) -> Result<FocusedShellHookOutput>;
}

/// Carries Focused Shell Hook Queue Entry state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusedShellHookQueueEntry {
    /// Stores the sequence value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub sequence: u64,
    /// Stores the plan value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub plan: HookExecutionPlan,
}

/// Carries Focused Shell Hook Dispatch Status state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusedShellHookDispatchStatus {
    /// Represents the Executed case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Executed,
    /// Represents the Blocked On Shell case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    BlockedOnShell,
}

/// Carries Focused Shell Hook Dispatch state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusedShellHookDispatch {
    /// Stores the sequence value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub sequence: u64,
    /// Stores the status value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub status: FocusedShellHookDispatchStatus,
    /// Stores the result value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub result: Option<HookExecutionResult>,
    /// Stores the hook id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub hook_id: String,
}

/// Carries Focused Shell Hook Queue state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Default)]
pub struct FocusedShellHookQueue {
    /// Stores the next sequence value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) next_sequence: u64,
    /// Stores the pending value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pending: VecDeque<FocusedShellHookQueueEntry>,
}
