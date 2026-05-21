//! Hook configuration and execution planning.
//!
//! Hooks may either invoke an arbitrary program directly or run a shell command
//! inside the focused pane shell. This module validates hook definitions, produces
//! execution plans, queues shell-bound hooks, and emits audit records for runs.

/// Exposes the audit module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod audit;
/// Exposes the execution module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod execution;
/// Exposes the planning module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod planning;
/// Exposes the queue module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod queue;
/// Exposes the types module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod types;

pub use audit::{
    execute_focused_shell_hook_with_audit, execute_program_hook_with_audit,
    hook_execution_audit_record,
};
pub use execution::{execute_focused_shell_hook, execute_program_hook, execute_program_hook_async};
pub use planning::{decide_hook_failure, plan_event, plan_hook, plan_hook_with_payload};
pub use types::{
    FocusedShellExecutor, FocusedShellHookDispatch, FocusedShellHookDispatchStatus,
    FocusedShellHookOutput, FocusedShellHookQueue, HookDefinition, HookEvent, HookEventPlan,
    HookExecutionPlan, HookExecutionResult, HookExecutionStatus, HookFailure, HookFailureDecision,
    HookFailureKind, HookInvocation, HookMatcherGroup, HookMatcherOperator, HookMatcherPredicate,
    HookOnFailure,
};

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
