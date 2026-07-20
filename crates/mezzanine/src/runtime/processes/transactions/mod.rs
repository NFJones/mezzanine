//! Runtime shell transaction observation, timeout, and OSC event handling.
//!
//! This module owns the agent shell transaction paths that retain command
//! output, expire timed-out transactions, recover stranded shell dispatches,
//! and interpret Mezzanine OSC transaction events. The process facade keeps
//! pane lifecycle orchestration while this module keeps transaction-specific
//! state transitions together.

use super::{
    ActionContentBlock, ActionResult, ActionStatus, AgentActionPayload, AgentTurnRecord,
    AgentTurnState, ApplyPatchTransactionPhase, AuditActor, BTreeSet, ClipboardAuthorization,
    ClipboardDecision, DEFAULT_BOOTSTRAP_TIMEOUT_MS, EventKind, HookEvent, HookExecutionResult,
    HookExecutionStatus, HookFailure, HookFailureKind, MezError, PaneReadinessState,
    ReadinessOverrideRevocation, Result, RunningShellTransactionKind, RunningShellTransactionRef,
    RuntimeHookPipelineBlock, RuntimeSessionService, RuntimeShellTransactionActionFailure,
    RuntimeShellTransactionTimerKind, RuntimeShellTransactionTimerRef, ShellTransaction,
    TerminalClipboardOperation, TerminalClipboardRequest, TerminalOscEvent,
    agent_shell_transaction_bytes_before_end_marker, agent_shell_transaction_observation_bytes,
    apply_patch_transaction_phase, bootstrap_script_for_classification, current_unix_millis,
    current_unix_seconds, decode_shell_output_transport_with_diagnostics,
    focused_shell_pre_action_timeout_result, hook_execution_audit_record, json_escape,
    latest_agent_shell_transaction_output_lines, local_action_plan, parse_bootstrap_env_output,
    plan_terminal_clipboard_request, postprocess_shell_action_success_output,
    readiness_probe_command_for_classification, runtime_agent_turn_state_from_action_results,
    runtime_agent_turn_state_name, runtime_execution_ready_for_provider_continuation,
    runtime_hook_event_name, runtime_hook_execution_status_name, runtime_marker_for_action,
    runtime_pane_readiness_state_name, runtime_post_shell_hook_payload,
    runtime_random_marker_token, shell_command_result_content,
};
use crate::runtime::{
    RenderInvalidationReason, RuntimeSideEffect, RuntimeTimerKey, RuntimeTimerKind,
    RuntimeTransition,
};
use mez_agent::AgentAction;

/// Defines the RUNTIME SHELL TRANSACTION OBSERVATION LIMIT BYTES const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const RUNTIME_SHELL_TRANSACTION_OBSERVATION_LIMIT_BYTES: usize = 256 * 1024;
/// Maximum retained snapshot bytes for the read phase of `apply_patch`.
///
/// The read phase carries remote file bytes that Rust must patch internally, so
/// it needs a larger bound than ordinary model-visible shell observations.
pub(super) const RUNTIME_APPLY_PATCH_SNAPSHOT_OBSERVATION_LIMIT_BYTES: usize = 16 * 1024 * 1024;
/// Defines the RUNTIME SHELL WRAPPER FILTER RECENT COMMAND LIMIT const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const RUNTIME_SHELL_WRAPPER_FILTER_RECENT_COMMAND_LIMIT: usize = 16;
/// Defines the RUNTIME SHELL WRAPPER FILTER RETENTION POLLS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const RUNTIME_SHELL_WRAPPER_FILTER_RETENTION_POLLS: usize = 4096;
/// Defines the RUNTIME HIDDEN SHELL RENDER RETENTION POLLS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const RUNTIME_HIDDEN_SHELL_RENDER_RETENTION_POLLS: usize = 32;
/// Prefix for the bounded OSC 133 markers Mezzanine owns.
pub(super) const RUNTIME_MEZ_OSC_PREFIX: &[u8] = b"\x1b]133;";
/// Maximum OSC payload bytes scanned for a Mezzanine-owned transaction marker.
pub(super) const RUNTIME_MEZ_OSC_SCAN_LIMIT_BYTES: usize = 4096;

/// Classifies semantic local-action failures by the failed boundary instead of
/// conflating every non-zero write transaction with an ordinary shell exit.
fn shell_action_failure_diagnostic(
    action: &AgentAction,
    exit_code: i32,
    output: &str,
    command: &str,
) -> (&'static str, String) {
    if matches!(action.payload, AgentActionPayload::ApplyPatch { .. }) {
        let diagnostic_text = if output.is_empty() {
            command.to_string()
        } else {
            format!(
                "{output}
{command}"
            )
        };
        if diagnostic_text.contains("read phase output was truncated or transport-incomplete") {
            return (
                "apply_patch_read_transport_incomplete",
                "apply_patch read phase transport was truncated or incomplete".to_string(),
            );
        }
        if diagnostic_text.contains("execution mode changed") {
            return (
                "apply_patch_execution_mode_changed",
                "apply_patch execution mode changed mid-action".to_string(),
            );
        }
        if diagnostic_text.contains("transport-incomplete") {
            return (
                "apply_patch_transport_incomplete",
                "apply_patch transport was incomplete".to_string(),
            );
        }
        if diagnostic_text.contains("payload") && diagnostic_text.contains("cap") {
            return (
                "apply_patch_payload_cap_exceeded",
                "apply_patch payload exceeded a transport boundary".to_string(),
            );
        }
        if diagnostic_text.contains("checksum") {
            return (
                "apply_patch_snapshot_checksum_mismatch",
                "apply_patch snapshot checksum mismatch".to_string(),
            );
        }
        if diagnostic_text.contains("byte-count") || diagnostic_text.contains("byte count") {
            return (
                "apply_patch_snapshot_byte_count_mismatch",
                "apply_patch snapshot byte-count mismatch".to_string(),
            );
        }
        if diagnostic_text.contains("hunk did not match")
            || diagnostic_text.contains("hunk header anchor")
        {
            return (
                "apply_patch_hunk_context_mismatch",
                "apply_patch hunk or context did not match".to_string(),
            );
        }
        return (
            "apply_patch_write_failed",
            format!("apply_patch write phase exited with status {exit_code}"),
        );
    }
    (
        "shell_command_failed",
        format!("shell command exited with status {exit_code}"),
    )
}
/// Maximum time a transaction may wait for its payload receiver start marker.
///
/// Non-stateful agent actions stream the command body only after the shell
/// wrapper emits an OSC start marker. If that marker is lost or the wrapper is
/// stranded before the receiver loop, waiting for the full command timeout makes
/// the pane look hung even though no user command has actually started.
const RUNTIME_SHELL_TRANSACTION_START_TIMEOUT_MS: u64 = 30_000;
/// Maximum time a readiness probe may run before Mezzanine degrades the pane.
///
/// Readiness probes are short shell health checks dispatched before pending
/// agent shell actions. Keeping their timeout in the transaction module keeps
/// timeout policy beside the dispatch and settlement code that consumes it.
const RUNTIME_READINESS_PROBE_TIMEOUT_MS: u64 = 5_000;
/// Runs the runtime running shell transaction kind name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_running_shell_transaction_kind_name(
    kind: &RunningShellTransactionKind,
) -> &'static str {
    match kind {
        RunningShellTransactionKind::AgentAction { .. } => "agent_action",
        RunningShellTransactionKind::ReadinessProbe => "readiness_probe",
        RunningShellTransactionKind::Bootstrap => "bootstrap",
        RunningShellTransactionKind::PathResolution { .. } => "path_resolution",
        RunningShellTransactionKind::BubblewrapCapabilityProbe { .. } => {
            "bubblewrap_capability_probe"
        }
    }
}

/// Returns the next runtime timeout deadline for one shell transaction.
///
/// Transactions with deferred payloads have an additional short start deadline:
/// the shell must reach the receiver loop and emit its start marker before the
/// command body is sent. Once that happens the pending payload is cleared and
/// the ordinary command timeout applies.
fn runtime_shell_transaction_effective_timeout_ms(
    transaction: &RunningShellTransactionRef,
) -> Option<u64> {
    let timeout_ms = transaction.timeout_ms?;
    if transaction.pending_input_payload.is_some() {
        Some(timeout_ms.min(RUNTIME_SHELL_TRANSACTION_START_TIMEOUT_MS))
    } else {
        Some(timeout_ms)
    }
}

/// Builds structured terminal observation data for a shell protocol violation.
fn shell_transaction_protocol_violation_observation(
    marker: &str,
    transaction: &RunningShellTransactionRef,
    boundary_state: &str,
    message: &str,
) -> serde_json::Value {
    serde_json::json!({
        "source": "pty",
        "stream": "pty_combined",
        "marker": marker,
        "exit_code": null,
        "signal": null,
        "timed_out": false,
        "combined_output_bytes": transaction.observed_output_bytes,
        "combined_output_preview": transaction.observed_output_preview,
        "boundary_state": boundary_state,
        "output_truncated": transaction.observed_output_truncated,
        "protocol_violation": true,
        "protocol_violation_message": message
    })
}

/// Builds model-facing terminal evidence for a pane input write failure.
fn pane_write_failure_terminal_observation(
    marker: &str,
    transaction: &RunningShellTransactionRef,
    boundary_state: &str,
    error: &str,
) -> serde_json::Value {
    serde_json::json!({
        "source": "pty",
        "stream": "pty_input",
        "marker": marker,
        "exit_code": null,
        "signal": null,
        "timed_out": false,
        "error": error,
        "combined_output_bytes": transaction.observed_output_bytes,
        "combined_output_preview": transaction.observed_output_preview,
        "boundary_state": boundary_state,
        "output_truncated": transaction.observed_output_truncated
    })
}
/// Returns the retained output bound for one running transaction.
///
/// # Parameters
/// - `transaction`: The transaction whose observed output is being retained.
fn runtime_shell_transaction_observation_limit(transaction: &RunningShellTransactionRef) -> usize {
    if matches!(
        transaction.kind,
        RunningShellTransactionKind::AgentAction { .. }
    ) && apply_patch_transaction_phase(&transaction.command)
        == Some(ApplyPatchTransactionPhase::Read)
    {
        RUNTIME_APPLY_PATCH_SNAPSHOT_OBSERVATION_LIMIT_BYTES
    } else {
        RUNTIME_SHELL_TRANSACTION_OBSERVATION_LIMIT_BYTES
    }
}

mod agent_actions;
mod bootstrap;
mod bubblewrap;
mod expiry;
mod hooks;
mod observation;
mod output;
mod path_resolution;
mod readiness;
mod recovery;
mod timers;
mod write_failures;

/// Runs the runtime shell transaction timer kind operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_shell_transaction_timer_kind(
    kind: &RunningShellTransactionKind,
) -> RuntimeShellTransactionTimerKind {
    match kind {
        RunningShellTransactionKind::AgentAction { .. } => {
            RuntimeShellTransactionTimerKind::AgentAction
        }
        RunningShellTransactionKind::ReadinessProbe => {
            RuntimeShellTransactionTimerKind::ReadinessProbe
        }
        RunningShellTransactionKind::Bootstrap => RuntimeShellTransactionTimerKind::Bootstrap,
        RunningShellTransactionKind::PathResolution { .. } => {
            RuntimeShellTransactionTimerKind::PathResolution
        }
        RunningShellTransactionKind::BubblewrapCapabilityProbe { .. } => {
            RuntimeShellTransactionTimerKind::BubblewrapCapabilityProbe
        }
    }
}
