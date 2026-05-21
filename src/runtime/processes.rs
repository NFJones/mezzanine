//! Runtime Processes implementation.
//!
//! This module owns the runtime processes boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    ActionContentBlock, ActionResult, ActionStatus, ActivePanePipe, AgentId, AgentTurnRecord,
    AgentTurnState, AuditActor, BTreeSet, ContextBlock, ContextSourceKind, DeferredPaneInput,
    DeferredPanePipeWrite, DeferredPaneResize, DeferredPaneTermination, EventKind,
    ExitedPaneProcess, HookEvent, HookExecutionResult, HookExecutionStatus, HookFailure,
    HookFailureKind, MezError, PaneDescriptor, PaneExitRecord, PaneExitStatus, PaneExitUpdate,
    PaneId, PaneOutputUpdate, PaneProcessOutput, PaneProcessStart, PaneReadinessState,
    PaneResizeUpdate, PaneSizeSpec, Path, PathBuf, ReadinessOverrideRevocation, ResizeAxis,
    ResizeDirection, Result, RunningShellTransactionKind, RunningShellTransactionRef,
    RuntimeHookPipelineBlock, RuntimeLifecycleState, RuntimeSessionService,
    RuntimeShellTransactionActionFailure, RuntimeShellTransactionTimerKind,
    RuntimeShellTransactionTimerRef, SessionSnapshotPayload, ShellClassification, ShellTransaction,
    Size, SplitDirection, StoppedPanePipe, TerminalOscEvent, TerminalScreen, WindowId,
    action_result_context_content, current_unix_millis, current_unix_seconds,
    decode_shell_output_transport, focused_shell_pre_action_timeout_result,
    hook_execution_audit_record, json_escape, local_action_plan, optional_i32_json,
    pane_content_size_for_geometry, pane_environment_with_term,
    postprocess_shell_action_success_output, rendered_window_body_size,
    runtime_agent_turn_state_from_action_results, runtime_agent_turn_state_name,
    runtime_execution_ready_for_provider_continuation, runtime_hook_event_name,
    runtime_hook_execution_status_name, runtime_marker_for_action,
    runtime_pane_readiness_state_name, runtime_post_shell_hook_payload,
    runtime_random_marker_token, shell_command_result_content,
    shell_command_structured_content_json, validate_pane_size_for_resize,
};
use crate::agent::{
    AgentActionPayload, ApplyPatchTransactionPhase, DEFAULT_BOOTSTRAP_TIMEOUT_MS,
    apply_patch_transaction_phase, bootstrap_script_for_classification, parse_bootstrap_env_output,
    readiness_probe_command_for_classification,
};
use crate::process::PaneProcess;
use crate::terminal::{TerminalStyledLine, parse_mez_shell_transaction_osc};

// Pane process lifecycle and PTY synchronization.

/// Defines the RUNTIME SHELL TRANSACTION OBSERVATION LIMIT BYTES const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const RUNTIME_SHELL_TRANSACTION_OBSERVATION_LIMIT_BYTES: usize = 64 * 1024;
/// Maximum retained snapshot bytes for the read phase of `apply_patch`.
///
/// The read phase carries remote file bytes that Rust must patch internally, so
/// it needs a larger bound than ordinary model-visible shell observations.
const RUNTIME_APPLY_PATCH_SNAPSHOT_OBSERVATION_LIMIT_BYTES: usize = 16 * 1024 * 1024;
/// Defines the RUNTIME SHELL WRAPPER FILTER RECENT COMMAND LIMIT const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const RUNTIME_SHELL_WRAPPER_FILTER_RECENT_COMMAND_LIMIT: usize = 16;
/// Defines the RUNTIME SHELL WRAPPER FILTER RETENTION POLLS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const RUNTIME_SHELL_WRAPPER_FILTER_RETENTION_POLLS: usize = 4096;
/// Defines the RUNTIME HIDDEN SHELL RENDER RETENTION POLLS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const RUNTIME_HIDDEN_SHELL_RENDER_RETENTION_POLLS: usize = 32;
/// Prefix for the bounded OSC 133 markers Mezzanine owns.
const RUNTIME_MEZ_OSC_PREFIX: &[u8] = b"\x1b]133;";
/// Maximum OSC payload bytes scanned for a Mezzanine-owned transaction marker.
const RUNTIME_MEZ_OSC_SCAN_LIMIT_BYTES: usize = 4096;
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
/// Defines the RUNTIME FOREGROUND TITLE IDLE SYNC POLL INTERVAL const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const RUNTIME_FOREGROUND_TITLE_IDLE_SYNC_POLL_INTERVAL: usize = 16;
/// Defines the RUNTIME READINESS PROBE TIMEOUT MS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const RUNTIME_READINESS_PROBE_TIMEOUT_MS: u64 = 5_000;
/// Maximum time a transaction may wait for its payload receiver start marker.
///
/// Non-stateful agent actions stream the command body only after the shell
/// wrapper emits an OSC start marker. If that marker is lost or the wrapper is
/// stranded before the receiver loop, waiting for the full command timeout makes
/// the pane look hung even though no user command has actually started.
const RUNTIME_SHELL_TRANSACTION_START_TIMEOUT_MS: u64 = 30_000;

/// Runs the runtime running shell transaction kind name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_running_shell_transaction_kind_name(kind: &RunningShellTransactionKind) -> &'static str {
    match kind {
        RunningShellTransactionKind::AgentAction { .. } => "agent_action",
        RunningShellTransactionKind::ReadinessProbe => "readiness_probe",
        RunningShellTransactionKind::Bootstrap => "bootstrap",
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

/// Carries Pane Output Render Mode state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneOutputRenderMode {
    /// Represents the Normal case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Normal,
    /// Represents the Hidden Live Agent Shell case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    HiddenLiveAgentShell,
    /// Represents the Hidden Retained Agent Shell case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    HiddenRetainedAgentShell,
    /// Represents the Verbose Agent Action case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    VerboseAgentAction,
    /// Represents the Trace case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Trace,
}

/// Runs the mez wrapper echo line is hidden operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn mez_wrapper_echo_line_is_hidden(line: &[u8], command_lines: &[String]) -> bool {
    if line.contains(&0x1b) {
        return false;
    }
    let text = String::from_utf8_lossy(line);
    let normalized = text.trim_matches(['\r', '\n']).trim();
    if normalized.is_empty() {
        return false;
    }
    mez_wrapper_echo_text_is_hidden(normalized, command_lines)
}

/// Runs the mez wrapper echo line visible bytes operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn mez_wrapper_echo_line_visible_bytes(line: &[u8], command_lines: &[String]) -> Vec<u8> {
    let mut visible = Vec::with_capacity(line.len());
    let mut text_segment = Vec::new();
    let mut index = 0usize;
    while index < line.len() {
        if line[index] == 0x1b {
            append_visible_mez_wrapper_text_segment(&mut visible, &text_segment, command_lines);
            text_segment.clear();
            let escape_end = terminal_escape_sequence_end(line, index);
            visible.extend_from_slice(&line[index..escape_end]);
            index = escape_end;
        } else {
            text_segment.push(line[index]);
            index += 1;
        }
    }
    append_visible_mez_wrapper_text_segment(&mut visible, &text_segment, command_lines);
    visible
}

/// Runs the append visible mez wrapper text segment operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn append_visible_mez_wrapper_text_segment(
    visible: &mut Vec<u8>,
    segment: &[u8],
    command_lines: &[String],
) {
    if segment.is_empty() || mez_wrapper_echo_line_is_hidden(segment, command_lines) {
        return;
    }
    let text = String::from_utf8_lossy(segment);
    visible.extend_from_slice(mez_wrapper_echo_text_without_leading_prompts(&text).as_bytes());
}

/// Runs the terminal escape sequence end operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn terminal_escape_sequence_end(bytes: &[u8], escape_index: usize) -> usize {
    let Some(kind) = bytes.get(escape_index + 1).copied() else {
        return bytes.len();
    };
    match kind {
        b']' => {
            let mut index = escape_index + 2;
            while index < bytes.len() {
                if bytes[index] == 0x07 {
                    return index + 1;
                }
                if bytes[index] == 0x1b && bytes.get(index + 1).is_some_and(|byte| *byte == b'\\') {
                    return index + 2;
                }
                index += 1;
            }
            bytes.len()
        }
        b'[' => {
            let mut index = escape_index + 2;
            while index < bytes.len() {
                if (0x40..=0x7e).contains(&bytes[index]) {
                    return index + 1;
                }
                index += 1;
            }
            bytes.len()
        }
        _ => (escape_index + 2).min(bytes.len()),
    }
}

/// Runs the mez wrapper echo line is possible prefix operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn mez_wrapper_echo_line_is_possible_prefix(line: &[u8], command_lines: &[String]) -> bool {
    if line.contains(&0x1b) {
        return false;
    }
    let text = String::from_utf8_lossy(line);
    let normalized = text.trim_matches(['\r', '\n']).trim();
    if normalized.is_empty() {
        return true;
    }
    let promptless = mez_wrapper_echo_text_without_inline_prompts(normalized);
    if normalized.starts_with("if [ \"$M") || promptless.starts_with("if [ \"$M") {
        return true;
    }
    [
        "MEZ_MARKER_TOKEN",
        "MEZ_TURN",
        "MEZ_AGENT",
        "MEZ_PANE",
        "MEZ_STATUS",
        "MEZ_RESTORE_ERREXIT",
        "MEZ_RESTORE_NOUNSET",
        "MEZ_RESTORE_HISTORY",
        "MEZ_HISTORY_",
        "MEZ_COMMAND_FILE",
        "MEZ_COMMAND_B64",
        "MEZ_WRITE_STATUS",
        "HISTFILE=/dev/null",
        "case $- in *e*)",
        "set +o history",
        "set -o history",
        "history -d",
        "if [ -n \"$MEZ_COMMAND_FILE\"",
        "if [ -n \"$MEZ_COMMAND_B64\"",
        "if [ \"$MEZ_WRITE_STATUS\"",
        "else",
        "elif command -v",
        "fi",
        "MEZ_WRITE_STATUS=$?",
        "MEZ_STATUS=$MEZ_WRITE_STATUS",
        "rm -f -- \"$MEZ_COMMAND_FILE\"",
        "rm -f -- \"$MEZ_COMMAND_B64\"",
        "unset MEZ_COMMAND_FILE",
        "unset MEZ_COMMAND_FILE MEZ_COMMAND_B64",
        "fish_private_mode",
        "history delete --",
        "printf '\\033]133;",
        "env -u MEZ_MARKER_TOKEN",
        "unset MEZ_MARKER_TOKEN",
        "set -l MEZ_MARKER_TOKEN",
        "set -l MEZ_TURN",
        "set -l MEZ_AGENT",
        "set -l MEZ_PANE",
        "set -l MEZ_STATUS",
        "set -e MEZ_MARKER_TOKEN",
        "MEZ_COMMAND_",
        ">",
        "$",
        "begin",
        "end",
        "{",
        "}",
        "command ",
        "eval ",
    ]
    .iter()
    .any(|hidden| {
        hidden.starts_with(normalized)
            || normalized.starts_with(hidden)
            || hidden.starts_with(promptless.as_str())
            || promptless.starts_with(hidden)
    }) || command_lines.iter().any(|command| {
        let command = command.trim();
        !command.is_empty()
            && (command.starts_with(normalized)
                || mez_wrapper_echo_text_ends_with_command(normalized, command))
    })
}

/// Runs the mez wrapper filter bytes may contain boilerplate operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn mez_wrapper_filter_bytes_may_contain_boilerplate(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    let promptless = mez_wrapper_echo_text_without_inline_prompts(&text);
    [
        "MEZ_MARKER_TOKEN",
        "MEZ_TURN",
        "MEZ_AGENT",
        "MEZ_PANE",
        "MEZ_STATUS",
        "MEZ_RESTORE_ERREXIT",
        "MEZ_RESTORE_NOUNSET",
        "MEZ_RESTORE_HISTORY",
        "MEZ_HISTORY_",
        "MEZ_COMMAND_FILE",
        "MEZ_OUTPUT_FILE",
        "MEZ_WRITE_STATUS",
        "HISTFILE=/dev/null",
        "MEZ_COMMAND_",
        "mez_marker=",
        "\\033]133",
        "env -u MEZ_MARKER_TOKEN",
        "unset MEZ_MARKER_TOKEN",
        "printf '%s\\n' '__MEZ_SHELL_OUTPUT_BASE64_",
        "printf '\\n%s\\n' '__MEZ_SHELL_OUTPUT_BASE64_",
        "set -l MEZ_MARKER_TOKEN",
        "set -e MEZ_MARKER_TOKEN",
    ]
    .iter()
    .any(|marker| text.contains(marker) || promptless.contains(marker))
}

/// Runs the mez wrapper echo text is hidden operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn mez_wrapper_echo_text_is_hidden(normalized: &str, command_lines: &[String]) -> bool {
    let promptless = mez_wrapper_echo_text_without_inline_prompts(normalized);
    if normalized.starts_with("if [ \"$M")
        || promptless.starts_with("if [ \"$M")
        || normalized.contains("MEZ_MARKER_TOKEN")
        || promptless.contains("MEZ_MARKER_TOKEN")
        || normalized.contains("MEZ_TURN")
        || promptless.contains("MEZ_TURN")
        || normalized.contains("MEZ_AGENT")
        || promptless.contains("MEZ_AGENT")
        || normalized.contains("MEZ_PANE")
        || promptless.contains("MEZ_PANE")
        || normalized.contains("MEZ_STATUS")
        || promptless.contains("MEZ_STATUS")
        || normalized.contains("MEZ_RESTORE_ERREXIT")
        || promptless.contains("MEZ_RESTORE_ERREXIT")
        || normalized.contains("MEZ_RESTORE_NOUNSET")
        || promptless.contains("MEZ_RESTORE_NOUNSET")
        || normalized.contains("MEZ_RESTORE_HISTORY")
        || promptless.contains("MEZ_RESTORE_HISTORY")
        || normalized.contains("MEZ_HISTORY_")
        || promptless.contains("MEZ_HISTORY_")
        || normalized.contains("MEZ_COMMAND_FILE")
        || promptless.contains("MEZ_COMMAND_FILE")
        || normalized.contains("MEZ_COMMAND_B64")
        || promptless.contains("MEZ_COMMAND_B64")
        || normalized.contains("MEZ_OUTPUT_FILE")
        || promptless.contains("MEZ_OUTPUT_FILE")
        || normalized.contains("MEZ_WRITE_STATUS")
        || promptless.contains("MEZ_WRITE_STATUS")
        || normalized.contains("HISTFILE=/dev/null")
        || promptless.contains("HISTFILE=/dev/null")
        || normalized.contains("set +o history")
        || promptless.contains("set +o history")
        || normalized.contains("set -o history")
        || promptless.contains("set -o history")
        || normalized.contains("history -d")
        || promptless.contains("history -d")
        || normalized.contains("fish_private_mode")
        || promptless.contains("fish_private_mode")
        || normalized.contains("history delete --")
        || promptless.contains("history delete --")
        || normalized.contains("MEZ_COMMAND_")
        || promptless.contains("MEZ_COMMAND_")
        || normalized.contains("case $- in *e*)")
        || promptless.contains("case $- in *e*)")
        || normalized.contains("mez_marker=")
        || promptless.contains("mez_marker=")
        || normalized.contains("printf '\\033]133;C;mez_marker")
        || promptless.contains("printf '\\033]133;C;mez_marker")
        || normalized.contains("printf '\\033]133;D;%s;mez_marker")
        || promptless.contains("printf '\\033]133;D;%s;mez_marker")
        || ((normalized.contains("printf '%s\\n'") || normalized.contains("printf '\\n%s\\n'"))
            && normalized.contains("__MEZ_SHELL_OUTPUT_BASE64_"))
        || ((promptless.contains("printf '%s\\n'") || promptless.contains("printf '\\n%s\\n'"))
            && promptless.contains("__MEZ_SHELL_OUTPUT_BASE64_"))
        || normalized.contains("env -u MEZ_MARKER_TOKEN -u MEZ_TURN -u MEZ_AGENT -u MEZ_PANE")
        || promptless.contains("env -u MEZ_MARKER_TOKEN -u MEZ_TURN -u MEZ_AGENT -u MEZ_PANE")
        || normalized.contains("cat > \"$MEZ_COMMAND_FILE\"")
        || promptless.contains("cat > \"$MEZ_COMMAND_FILE\"")
        || matches!(normalized, "else" | "fi")
        || matches!(promptless.as_str(), "else" | "fi")
        || normalized.starts_with("if command -v")
        || promptless.starts_with("if command -v")
        || normalized.starts_with("elif command -v")
        || promptless.starts_with("elif command -v")
        || normalized.contains("setsid(); exec @ARGV")
        || promptless.contains("setsid(); exec @ARGV")
        || normalized.contains("os.setsid()")
        || promptless.contains("os.setsid()")
        || normalized.contains("</dev/null")
        || promptless.contains("</dev/null")
        || normalized.contains("unset MEZ_MARKER_TOKEN MEZ_TURN MEZ_AGENT MEZ_PANE MEZ_STATUS")
        || promptless.contains("unset MEZ_MARKER_TOKEN MEZ_TURN MEZ_AGENT MEZ_PANE MEZ_STATUS")
        || normalized.contains("set -l MEZ_STATUS $status")
        || promptless.contains("set -l MEZ_STATUS $status")
        || normalized.contains("set -e MEZ_MARKER_TOKEN MEZ_TURN MEZ_AGENT MEZ_PANE MEZ_STATUS")
        || promptless.contains("set -e MEZ_MARKER_TOKEN MEZ_TURN MEZ_AGENT MEZ_PANE MEZ_STATUS")
        || normalized == ">"
        || normalized == "$"
        || promptless == ">"
        || promptless == "$"
        || normalized == "begin"
        || normalized == "end"
        || normalized == "{"
        || normalized == "}"
        || (normalized.starts_with("command ") && normalized.contains(" -c "))
        || normalized.starts_with("eval ")
    {
        return true;
    }
    if normalized
        .split_whitespace()
        .all(|token| matches!(token, "$" | ">" | "#"))
    {
        return true;
    }
    command_lines.iter().any(|command| {
        let command = command.trim();
        !command.is_empty() && mez_wrapper_echo_text_ends_with_command(normalized, command)
    })
}

/// Runs the mez wrapper echo text ends with command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn mez_wrapper_echo_text_ends_with_command(normalized: &str, command: &str) -> bool {
    if normalized == command {
        return true;
    }
    let normalized_without_prompts = mez_wrapper_echo_text_without_inline_prompts(normalized);
    let command_without_prompts = mez_wrapper_echo_text_without_inline_prompts(command);
    if normalized_without_prompts == command_without_prompts {
        return true;
    }
    if normalized_without_prompts
        .strip_suffix(&command_without_prompts)
        .and_then(|prefix| prefix.chars().next_back())
        .is_some_and(|ch| ch.is_whitespace() || matches!(ch, '$' | '>' | '#'))
    {
        return true;
    }
    let Some(prefix) = normalized.strip_suffix(command) else {
        return false;
    };
    prefix
        .chars()
        .next_back()
        .is_some_and(|ch| ch.is_whitespace() || matches!(ch, '$' | '>' | '#'))
}

/// Runs the mez wrapper echo text without inline prompts operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn mez_wrapper_echo_text_without_inline_prompts(value: &str) -> String {
    value
        .replace("$  ", "")
        .replace("$ ", "")
        .replace(">  ", "")
        .replace("> ", "")
}

/// Strips terminal control traffic from shell transaction bytes before they are
/// stored as model-visible command output.
///
/// The terminal renderer still receives OSC/ANSI data for state updates, but
/// model context should contain the useful textual command output rather than
/// prompt styling, cursor movement, or bracketed-paste toggles.
fn shell_observation_without_terminal_controls(bytes: &[u8]) -> Vec<u8> {
    let mut text = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            0x1b => {
                index = terminal_escape_sequence_end(bytes, index);
            }
            b'\n' | b'\r' | b'\t' => {
                text.push(bytes[index]);
                index += 1;
            }
            byte if byte.is_ascii_control() => {
                index += 1;
            }
            byte => {
                text.push(byte);
                index += 1;
            }
        }
    }
    text
}

/// Returns whether a cleaned transaction line is an interactive shell prompt
/// repaint rather than command output.
///
/// Prompt repaint text is especially common when a user's PS1 is styled with
/// powerline glyphs. Keeping it out of model-visible observations prevents a
/// small capture window from filling with prompts before the command output.
fn shell_observation_line_looks_like_prompt(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    ["\u{e0b6}", "\u{e0b0}", "\u{e0b4}", "\u{f412}", "❯", "➜"]
        .iter()
        .any(|marker| trimmed.contains(marker))
        || matches!(trimmed, "$" | ">" | "#")
        || shell_observation_line_has_common_prompt_suffix(trimmed)
}

/// Returns whether one stripped line looks like a plain shell prompt tail.
fn shell_observation_line_has_common_prompt_suffix(trimmed: &str) -> bool {
    let Some((prefix, suffix)) = trimmed.rsplit_once(' ') else {
        return false;
    };
    matches!(suffix, "$" | ">" | "#")
        && (prefix.starts_with('~')
            || prefix.starts_with('/')
            || prefix.contains('@')
            || prefix.contains(':')
            || prefix.contains("repo"))
}

/// Produces model-visible command output for an agent shell transaction.
///
/// User-facing rendering keeps a richer stream so the terminal can update
/// state, but the model only needs command stdout/stderr. This removes
/// Mezzanine wrapper echo, shell prompt repaint, and terminal styling while
/// preserving the actual command output that should feed follow-up reasoning.
fn agent_shell_transaction_observation_bytes(bytes: &[u8], command: &str) -> Vec<u8> {
    let stripped = shell_observation_without_terminal_controls(bytes);
    let text = String::from_utf8_lossy(&stripped);
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let command_lines = vec![command.to_string()];
    let mut output = String::new();
    for line in normalized.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !output.ends_with('\n') && !output.is_empty() {
                output.push('\n');
            }
            continue;
        }
        if mez_wrapper_echo_text_is_hidden(trimmed, &command_lines) {
            continue;
        }
        let cleaned = mez_wrapper_echo_text_without_leading_prompts(line);
        if cleaned.trim().is_empty() || shell_observation_line_looks_like_prompt(&cleaned) {
            continue;
        }
        output.push_str(cleaned.trim_end());
        output.push('\n');
    }
    output.into_bytes()
}

/// Returns pane bytes that belong to the command before this transaction's end
/// marker.
///
/// A PTY read can contain the command's final output, Mezzanine's OSC 133 end
/// marker, and the parent shell's next prompt repaint in one chunk. The prompt
/// bytes are useful to readiness detection, but they must not replace the
/// transient latest-output line shown for the just-finished command.
fn agent_shell_transaction_bytes_before_end_marker<'a>(bytes: &'a [u8], marker: &str) -> &'a [u8] {
    let marker_field = format!("mez_marker={marker}");
    let Some(marker_index) = find_byte_subsequence(bytes, marker_field.as_bytes()) else {
        return bytes;
    };
    let Some(escape_index) = bytes[..marker_index].iter().rposition(|byte| *byte == 0x1b) else {
        return bytes;
    };
    if bytes.get(escape_index + 1) == Some(&b']')
        && bytes
            .get(escape_index + 2..marker_index)
            .is_some_and(|payload_prefix| payload_prefix.starts_with(b"133;D;"))
    {
        &bytes[..escape_index]
    } else {
        bytes
    }
}

/// Finds the first occurrence of a byte sequence inside another byte slice.
fn find_byte_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Scans bytes for bounded Mezzanine-owned OSC 133 transaction events.
///
/// # Parameters
/// - `bytes`: The hidden agent-shell bytes plus any retained fragment from the
///   previous PTY read.
fn scan_mezzanine_osc_transaction_events(bytes: &[u8]) -> (Vec<TerminalOscEvent>, Vec<u8>) {
    let mut events = Vec::new();
    let mut cursor = 0usize;
    let mut retained_start = None;
    while let Some(relative_start) = find_byte_subsequence(&bytes[cursor..], RUNTIME_MEZ_OSC_PREFIX)
    {
        let osc_start = cursor + relative_start;
        let payload_start = osc_start + 2;
        match find_bounded_osc_terminator(bytes, payload_start) {
            Some((payload_end, terminator_end)) => {
                if let Ok(payload) = std::str::from_utf8(&bytes[payload_start..payload_end])
                    && let Some(event) = parse_mez_shell_transaction_osc(payload)
                {
                    events.push(event);
                }
                cursor = terminator_end;
            }
            None => {
                if bytes.len().saturating_sub(payload_start) >= RUNTIME_MEZ_OSC_SCAN_LIMIT_BYTES {
                    cursor = osc_start.saturating_add(1);
                } else {
                    retained_start = Some(osc_start);
                    break;
                }
            }
        }
    }
    let retained = if let Some(start) = retained_start {
        bounded_osc_pending_fragment(&bytes[start..])
    } else {
        trailing_mez_osc_prefix_fragment(bytes)
    };
    (events, retained)
}

/// Finds an OSC string terminator within the bounded Mezzanine marker window.
///
/// # Parameters
/// - `bytes`: The byte slice being scanned.
/// - `payload_start`: The byte offset immediately after `ESC ]`.
fn find_bounded_osc_terminator(bytes: &[u8], payload_start: usize) -> Option<(usize, usize)> {
    let search_end = bytes
        .len()
        .min(payload_start.saturating_add(RUNTIME_MEZ_OSC_SCAN_LIMIT_BYTES));
    let mut index = payload_start;
    while index < search_end {
        match bytes[index] {
            0x07 => return Some((index, index + 1)),
            0x1b if bytes.get(index + 1) == Some(&b'\\') => return Some((index, index + 2)),
            _ => index += 1,
        }
    }
    None
}

/// Bounds one retained OSC parser fragment to the maximum marker window.
///
/// # Parameters
/// - `fragment`: The potential partial OSC marker fragment to retain.
fn bounded_osc_pending_fragment(fragment: &[u8]) -> Vec<u8> {
    if fragment.len() <= RUNTIME_MEZ_OSC_SCAN_LIMIT_BYTES {
        fragment.to_vec()
    } else {
        fragment[fragment.len() - RUNTIME_MEZ_OSC_SCAN_LIMIT_BYTES..].to_vec()
    }
}

/// Returns a trailing byte prefix that could start a future Mezzanine marker.
///
/// # Parameters
/// - `bytes`: The complete scanned byte slice.
fn trailing_mez_osc_prefix_fragment(bytes: &[u8]) -> Vec<u8> {
    let max_len = bytes
        .len()
        .min(RUNTIME_MEZ_OSC_PREFIX.len().saturating_sub(1));
    for len in (1..=max_len).rev() {
        if bytes[bytes.len() - len..] == RUNTIME_MEZ_OSC_PREFIX[..len] {
            return bytes[bytes.len() - len..].to_vec();
        }
    }
    Vec::new()
}

/// Returns the latest non-empty model-visible shell output line.
fn latest_agent_shell_transaction_output_line(output: &str) -> Option<String> {
    decode_shell_output_transport(output)
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .rev()
        .map(sanitized_shell_output_status_line)
        .map(|line| line.trim_end().to_string())
        .find(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty() && !shell_observation_line_looks_like_prompt(trimmed)
        })
}

/// Sanitizes one transient shell-output status line for pane rendering.
fn sanitized_shell_output_status_line(line: &str) -> String {
    line.chars()
        .map(|ch| {
            if ch == '\t' || !ch.is_control() {
                ch
            } else {
                ' '
            }
        })
        .collect()
}

/// Runs the mez wrapper echo text without leading prompts operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn mez_wrapper_echo_text_without_leading_prompts(value: &str) -> String {
    let mut remaining = value;
    loop {
        let trimmed = remaining.trim_start_matches(' ');
        if let Some(next) = trimmed.strip_prefix("$ ") {
            remaining = next;
            continue;
        }
        if let Some(next) = trimmed.strip_prefix("> ") {
            remaining = next;
            continue;
        }
        return trimmed.to_string();
    }
}

impl RuntimeSessionService {
    /// Runs the shell classification for pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn shell_classification_for_pane(&self, pane_id: &str) -> ShellClassification {
        self.pane_environment_signatures
            .get(pane_id)
            .map(|signature| signature.shell_classification)
            .unwrap_or_else(|| ShellClassification::classify(self.session.shell.path()))
    }

    /// Runs the start initial pane process operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn start_initial_pane_process(
        &mut self,
        explicit_command: Option<&str>,
    ) -> Result<PaneProcessStart> {
        self.require_live()?;
        let descriptor = self.initial_pane_descriptor()?;
        let started = self.start_pane_process(descriptor, explicit_command)?;
        self.run_configured_completed_hooks(
            HookEvent::SessionStart,
            &format!(
                r#"{{"session_id":"{}","initial_pane_id":"{}"}}"#,
                json_escape(self.session.id.as_str()),
                json_escape(&started.pane_id)
            ),
        )?;
        Ok(started)
    }

    /// Runs the restart restored pane processes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn restart_restored_pane_processes(
        &mut self,
        explicit_command: Option<&str>,
    ) -> Result<Vec<PaneProcessStart>> {
        self.require_live()?;
        let descriptors = self
            .session
            .windows()
            .iter()
            .flat_map(|window| {
                window.panes().iter().filter_map(|pane| {
                    if pane.live || self.pane_processes.contains_pane(pane.id.as_str()) {
                        None
                    } else {
                        Some(PaneDescriptor {
                            window_id: window.id.clone(),
                            pane_id: pane.id.clone(),
                            size: pane.size,
                        })
                    }
                })
            })
            .collect::<Vec<_>>();
        let mut starts = Vec::with_capacity(descriptors.len());
        for descriptor in descriptors {
            let restored_screen = self.pane_screens.get(descriptor.pane_id.as_str()).cloned();
            let started = self.start_pane_process(descriptor, explicit_command)?;
            if let Some(mut screen) = restored_screen {
                screen.feed(b"\n[mezzanine: pane restarted with a fresh primary PID]\n");
                self.pane_screens.insert(started.pane_id.clone(), screen);
            }
            self.session.set_pane_live_state(&started.pane_id, true)?;
            self.append_lifecycle_event(
                EventKind::PaneChanged,
                format!(
                    r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"process_state":"running","restarted":true}}"#,
                    json_escape(&started.pane_id),
                    json_escape(&started.window_id),
                    started.primary_pid
                ),
            )?;
            starts.push(started);
        }
        Ok(starts)
    }

    /// Runs the seed terminal screens from snapshot payload operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn seed_terminal_screens_from_snapshot_payload(
        &mut self,
        payload: &SessionSnapshotPayload,
    ) -> Result<usize> {
        self.require_live()?;
        self.require_snapshot_resume_hooks_allow(payload)?;
        self.seed_terminal_screens_from_snapshot_payload_without_hooks(payload)
    }

    /// Runs the require snapshot resume hooks allow operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn require_snapshot_resume_hooks_allow(
        &mut self,
        payload: &SessionSnapshotPayload,
    ) -> Result<()> {
        if let Some(block) = self.run_configured_pre_action_hooks(
            HookEvent::SnapshotResume,
            &format!(
                r#"{{"session_id":"{}","windows":{},"panes":{}}}"#,
                json_escape(&payload.session_id),
                payload.windows.len(),
                payload
                    .windows
                    .iter()
                    .map(|window| window.panes.len())
                    .sum::<usize>()
            ),
        )? {
            return Err(MezError::forbidden(format!(
                "snapshot resume blocked by hook `{}`: {}",
                block.hook_id, block.message
            )));
        }
        Ok(())
    }

    /// Runs the seed terminal screens from snapshot payload without hooks operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn seed_terminal_screens_from_snapshot_payload_without_hooks(
        &mut self,
        payload: &SessionSnapshotPayload,
    ) -> Result<usize> {
        let mut seeded = 0usize;
        for window in &payload.windows {
            for pane in &window.panes {
                let Some(descriptor) = self.find_pane_descriptor(&pane.pane_id) else {
                    continue;
                };
                let mut screen = TerminalScreen::new_with_history_config(
                    descriptor.size,
                    self.terminal_history_limit,
                    self.terminal_history_rotate_lines,
                )?;
                let history_lines = pane
                    .terminal_history
                    .iter()
                    .enumerate()
                    .map(|(line_index, line)| TerminalStyledLine {
                        text: line.clone(),
                        style_spans: pane
                            .terminal_history_line_style_spans
                            .get(line_index)
                            .cloned()
                            .unwrap_or_default(),
                        copy_text: None,
                    })
                    .collect::<Vec<_>>();
                let visible_lines = pane
                    .visible_lines
                    .iter()
                    .enumerate()
                    .map(|(line_index, line)| TerminalStyledLine {
                        text: line.clone(),
                        style_spans: pane
                            .visible_line_style_spans
                            .get(line_index)
                            .cloned()
                            .unwrap_or_default(),
                        copy_text: None,
                    })
                    .collect::<Vec<_>>();
                screen.restore_normal_styled_history_content(&history_lines, &visible_lines);
                screen.restore_mode_state(&pane.terminal_modes);
                screen.restore_saved_state(&pane.terminal_saved_state);
                self.pane_screens.insert(pane.pane_id.clone(), screen);
                seeded = seeded.saturating_add(1);
            }
        }
        if seeded > 0 {
            self.append_lifecycle_event(
                EventKind::SnapshotChanged,
                format!(r#"{{"snapshot_restore":"terminal_screens_seeded","panes":{seeded}}}"#),
            )?;
        }
        Ok(seeded)
    }

    /// Runs the create window with pane process operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn create_window_with_pane_process(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        name: impl Into<String>,
        select: bool,
        explicit_command: Option<&str>,
    ) -> Result<PaneProcessStart> {
        self.create_window_with_pane_process_with_options(
            primary_client_id,
            name,
            select,
            explicit_command,
            None,
            None,
        )
    }

    /// Creates a window with one pane and starts the pane process with creation options.
    ///
    /// The caller must be the active primary client. `start_directory`, when
    /// present, is applied to the spawned shell. `requested_size`, when present,
    /// resizes the created pane before the PTY is opened.
    pub fn create_window_with_pane_process_with_options(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        name: impl Into<String>,
        select: bool,
        explicit_command: Option<&str>,
        start_directory: Option<&Path>,
        requested_size: Option<PaneSizeSpec>,
    ) -> Result<PaneProcessStart> {
        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        validate_runtime_start_directory(start_directory)?;
        if let Some(spec) = requested_size {
            validate_new_window_requested_pane_size(self.session.authoritative_size, spec)?;
        }
        let previous_session = self.session.clone();
        let previous_window_created_at_unix_seconds = self.window_created_at_unix_seconds.clone();
        let window_id = self.session.new_window(primary_client_id, name, select)?;
        self.window_created_at_unix_seconds
            .insert(window_id.to_string(), current_unix_seconds());
        if let Some(spec) = requested_size {
            let pane_id = self
                .session
                .windows()
                .iter()
                .find(|window| window.id == window_id)
                .and_then(|window| window.panes().first())
                .map(|pane| pane.id.clone())
                .ok_or_else(|| {
                    MezError::new(
                        crate::error::MezErrorKind::NotFound,
                        "created pane not found",
                    )
                })?;
            let pane = self.session.resize_pane_in_window_with_spec(
                primary_client_id,
                &window_id,
                &pane_id,
                spec,
            )?;
            validate_pane_size_for_resize(pane.size)?;
        }
        let window = self
            .session
            .windows()
            .iter()
            .find(|window| window.id == window_id)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "created window not found",
                )
            })?;
        let pane = window.active_pane();
        let size = self
            .pane_process_size_for(window, pane.id.as_str())
            .unwrap_or(pane.size);
        let descriptor = PaneDescriptor {
            window_id: window.id.clone(),
            pane_id: pane.id.clone(),
            size,
        };
        let started = match self.start_pane_process_with_start_directory(
            descriptor,
            explicit_command,
            start_directory,
        ) {
            Ok(started) => started,
            Err(error) => {
                self.session = previous_session;
                self.window_created_at_unix_seconds = previous_window_created_at_unix_seconds;
                return Err(error);
            }
        };
        self.append_lifecycle_event(
            EventKind::WindowChanged,
            format!(
                r#"{{"window_id":"{}","state":"created","pane_id":"{}"}}"#,
                json_escape(&started.window_id),
                json_escape(&started.pane_id)
            ),
        )?;
        Ok(started)
    }

    /// Creates a window in a specific group and starts its initial pane process.
    ///
    /// Unlike the normal window creation path, this helper does not require the
    /// target group to be active and never focuses the created window. It is
    /// used for subagent windows that should belong beside their controller
    /// without stealing user focus.
    pub fn create_unfocused_window_in_group_with_pane_process(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        group_id: &crate::ids::WindowGroupId,
        name: impl Into<String>,
        layout_policy: crate::layout::LayoutPolicy,
        start_directory: Option<&Path>,
    ) -> Result<PaneProcessStart> {
        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        validate_runtime_start_directory(start_directory)?;
        let previous_session = self.session.clone();
        let previous_window_created_at_unix_seconds = self.window_created_at_unix_seconds.clone();
        let window_id =
            self.session
                .new_window_in_group(primary_client_id, group_id, name, false)?;
        self.window_created_at_unix_seconds
            .insert(window_id.to_string(), current_unix_seconds());
        self.session
            .set_window_layout_policy(primary_client_id, &window_id, layout_policy)?;
        let window = self
            .session
            .windows()
            .iter()
            .find(|window| window.id == window_id)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "created window not found",
                )
            })?;
        let pane = window.active_pane();
        let size = self
            .pane_process_size_for(window, pane.id.as_str())
            .unwrap_or(pane.size);
        let descriptor = PaneDescriptor {
            window_id: window.id.clone(),
            pane_id: pane.id.clone(),
            size,
        };
        let started =
            match self.start_pane_process_with_start_directory(descriptor, None, start_directory) {
                Ok(started) => started,
                Err(error) => {
                    self.session = previous_session;
                    self.window_created_at_unix_seconds = previous_window_created_at_unix_seconds;
                    return Err(error);
                }
            };
        self.append_lifecycle_event(
            EventKind::WindowChanged,
            format!(
                r#"{{"window_id":"{}","group_id":"{}","state":"created","pane_id":"{}","layout_policy":"{}"}}"#,
                json_escape(&started.window_id),
                json_escape(group_id.as_str()),
                json_escape(&started.pane_id),
                layout_policy.name()
            ),
        )?;
        Ok(started)
    }

    /// Creates a new window group with one landing pane and starts its process.
    ///
    /// This follows the same runtime path as `window/create`: the in-memory
    /// session mutation is rolled back if the pane process cannot be spawned.
    pub fn create_group_with_pane_process(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        name: impl Into<String>,
        select: bool,
        explicit_command: Option<&str>,
        start_directory: Option<&Path>,
    ) -> Result<PaneProcessStart> {
        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        validate_runtime_start_directory(start_directory)?;
        let previous_session = self.session.clone();
        let previous_window_created_at_unix_seconds = self.window_created_at_unix_seconds.clone();
        let (group_id, window_id) = self.session.new_group(primary_client_id, name, select)?;
        self.window_created_at_unix_seconds
            .insert(window_id.to_string(), current_unix_seconds());
        let window = self
            .session
            .windows()
            .iter()
            .find(|window| window.id == window_id)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "created group window not found",
                )
            })?;
        let pane = window.active_pane();
        let size = self
            .pane_process_size_for(window, pane.id.as_str())
            .unwrap_or(pane.size);
        let descriptor = PaneDescriptor {
            window_id: window.id.clone(),
            pane_id: pane.id.clone(),
            size,
        };
        let started = match self.start_pane_process_with_start_directory(
            descriptor,
            explicit_command,
            start_directory,
        ) {
            Ok(started) => started,
            Err(error) => {
                self.session = previous_session;
                self.window_created_at_unix_seconds = previous_window_created_at_unix_seconds;
                return Err(error);
            }
        };
        self.append_lifecycle_event(
            EventKind::WindowChanged,
            format!(
                r#"{{"group_id":"{}","window_id":"{}","state":"created","pane_id":"{}"}}"#,
                json_escape(group_id.as_str()),
                json_escape(&started.window_id),
                json_escape(&started.pane_id)
            ),
        )?;
        Ok(started)
    }

    /// Runs the split pane with process operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn split_pane_with_process(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        direction: SplitDirection,
        explicit_command: Option<&str>,
    ) -> Result<PaneProcessStart> {
        self.split_pane_with_process_with_options(
            primary_client_id,
            direction,
            true,
            explicit_command,
            None,
            None,
        )
    }

    /// Splits the active pane and starts the new pane process with creation options.
    ///
    /// The caller must be the active primary client. The new pane inherits the
    /// normal split geometry unless `requested_size` is provided, in which case
    /// the pane and PTY are resized before process spawn.
    pub fn split_pane_with_process_with_options(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        direction: SplitDirection,
        select_new: bool,
        explicit_command: Option<&str>,
        start_directory: Option<&Path>,
        requested_size: Option<PaneSizeSpec>,
    ) -> Result<PaneProcessStart> {
        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        validate_runtime_start_directory(start_directory)?;
        let previous_session = self.session.clone();
        let previous_window_created_at_unix_seconds = self.window_created_at_unix_seconds.clone();
        let pane_id = match requested_size {
            Some(spec) => self.session.split_active_pane_with_size_spec_select(
                primary_client_id,
                direction,
                spec,
                select_new,
            )?,
            None => {
                self.session
                    .split_active_pane_select(primary_client_id, direction, select_new)?
            }
        };
        if let Err(error) = self.sync_tracked_pty_sizes() {
            self.session = previous_session;
            self.window_created_at_unix_seconds = previous_window_created_at_unix_seconds;
            let _ = self.sync_tracked_pty_sizes();
            return Err(error);
        }
        let descriptor = match self.find_pane_descriptor(pane_id.as_str()) {
            Some(descriptor) => descriptor,
            None => {
                self.session = previous_session;
                self.window_created_at_unix_seconds = previous_window_created_at_unix_seconds;
                let _ = self.sync_tracked_pty_sizes();
                return Err(MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "created pane not found",
                ));
            }
        };
        match self.start_pane_process_with_start_directory(
            descriptor,
            explicit_command,
            start_directory,
        ) {
            Ok(started) => Ok(started),
            Err(error) => {
                self.session = previous_session;
                self.window_created_at_unix_seconds = previous_window_created_at_unix_seconds;
                let _ = self.sync_tracked_pty_sizes();
                Err(error)
            }
        }
    }

    /// Splits a target window and starts a process in the created pane.
    ///
    /// The session-level focused window is left untouched. This lets background
    /// orchestration append panes to a hidden or non-focused window while still
    /// reusing the normal process, PTY-size synchronization, and rollback path.
    pub fn split_pane_in_window_with_process(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        window_id: &WindowId,
        direction: SplitDirection,
        select_new: bool,
        explicit_command: Option<&str>,
        start_directory: Option<&Path>,
    ) -> Result<PaneProcessStart> {
        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        validate_runtime_start_directory(start_directory)?;
        let previous_session = self.session.clone();
        let previous_window_created_at_unix_seconds = self.window_created_at_unix_seconds.clone();
        let pane_id = self.session.split_pane_in_window_select(
            primary_client_id,
            window_id,
            direction,
            select_new,
        )?;
        if let Err(error) = self.sync_tracked_pty_sizes() {
            self.session = previous_session;
            self.window_created_at_unix_seconds = previous_window_created_at_unix_seconds;
            let _ = self.sync_tracked_pty_sizes();
            return Err(error);
        }
        let descriptor = match self.find_pane_descriptor(pane_id.as_str()) {
            Some(descriptor) => descriptor,
            None => {
                self.session = previous_session;
                self.window_created_at_unix_seconds = previous_window_created_at_unix_seconds;
                let _ = self.sync_tracked_pty_sizes();
                return Err(MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "created pane not found",
                ));
            }
        };
        match self.start_pane_process_with_start_directory(
            descriptor,
            explicit_command,
            start_directory,
        ) {
            Ok(started) => Ok(started),
            Err(error) => {
                self.session = previous_session;
                self.window_created_at_unix_seconds = previous_window_created_at_unix_seconds;
                let _ = self.sync_tracked_pty_sizes();
                Err(error)
            }
        }
    }

    /// Runs the resize pane pty operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn resize_pane_pty(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        target: Option<&str>,
        size: Size,
    ) -> Result<PaneResizeUpdate> {
        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        validate_pane_size_for_resize(size)?;
        let descriptor = self.active_window_pane_descriptor(target)?;
        let target_pane_id = descriptor.pane_id.to_string();
        if self
            .primary_pid_for_live_pane_process(descriptor.pane_id.as_str())
            .is_none()
        {
            return Err(MezError::new(
                crate::error::MezErrorKind::NotFound,
                "pane process not found",
            ));
        }

        let mut next_session = self.session.clone();
        next_session.resize_pane(primary_client_id, target, size)?;
        self.session = next_session;
        self.sync_tracked_pty_sizes()?
            .into_iter()
            .find(|update| update.pane_id == target_pane_id)
            .ok_or_else(|| MezError::invalid_state("resized pane process was not synchronized"))
    }

    /// Resolves a size spec, resizes the pane PTY, and updates session state.
    pub fn resize_pane_pty_with_spec(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        target: Option<&str>,
        spec: PaneSizeSpec,
    ) -> Result<PaneResizeUpdate> {
        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        let descriptor = self.active_window_pane_descriptor(target)?;
        let size = self
            .session
            .windows()
            .iter()
            .find(|window| window.id == descriptor.window_id)
            .ok_or_else(|| MezError::invalid_state("pane window not found"))?
            .resolve_pane_size_spec(Some(descriptor.pane_id.as_str()), spec)?;
        self.resize_pane_pty(primary_client_id, target, size)
    }

    /// Runs the swap panes and sync pty sizes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn swap_panes_and_sync_pty_sizes(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        source: Option<&str>,
        destination: &str,
    ) -> Result<Vec<PaneResizeUpdate>> {
        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        self.session
            .swap_panes(primary_client_id, source, destination)?;
        self.sync_tracked_pty_sizes()
    }

    /// Runs the break pane and sync pty sizes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn break_pane_and_sync_pty_sizes(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        target: Option<&str>,
        name: Option<String>,
        select_new_window: bool,
    ) -> Result<(WindowId, Vec<PaneResizeUpdate>)> {
        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        let window_id =
            self.session
                .break_pane(primary_client_id, target, name, select_new_window)?;
        self.window_created_at_unix_seconds
            .insert(window_id.to_string(), current_unix_seconds());
        let updates = self.sync_tracked_pty_sizes()?;
        Ok((window_id, updates))
    }

    /// Runs the join pane and sync pty sizes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn join_pane_and_sync_pty_sizes(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        source: Option<&str>,
        destination: &str,
        direction: SplitDirection,
        select_joined_pane: bool,
    ) -> Result<(PaneId, Vec<PaneResizeUpdate>)> {
        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        let pane_id = self.session.join_pane(
            primary_client_id,
            source,
            destination,
            direction,
            select_joined_pane,
        )?;
        let updates = self.sync_tracked_pty_sizes()?;
        Ok((pane_id, updates))
    }

    /// Runs the sync tracked pty sizes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn sync_tracked_pty_sizes(&mut self) -> Result<Vec<PaneResizeUpdate>> {
        self.require_live()?;
        let descriptors = self.tracked_pane_descriptors();
        let mut updates = Vec::new();

        for descriptor in descriptors {
            let pane_id = descriptor.pane_id.as_str();
            let Some(primary_pid) = self.primary_pid_for_live_pane_process(pane_id) else {
                continue;
            };
            if self.pane_processes.contains_pane(pane_id) {
                self.pane_processes.resize_pane(pane_id, descriptor.size)?;
            } else if self.pane_process_is_async_owned(pane_id) {
                self.deferred_pane_resizes.insert(
                    pane_id.to_string(),
                    DeferredPaneResize {
                        size: descriptor.size,
                    },
                );
            }
            if let Some(screen) = self.pane_screens.get_mut(descriptor.pane_id.as_str()) {
                screen.resize(descriptor.size);
            }
            if let Some(screen) = self
                .pane_transaction_osc_screens
                .get_mut(descriptor.pane_id.as_str())
            {
                screen.resize(descriptor.size);
            }
            let update = PaneResizeUpdate {
                session_id: self.session.id.to_string(),
                window_id: descriptor.window_id.to_string(),
                pane_id: descriptor.pane_id.to_string(),
                primary_pid,
                size: descriptor.size,
                registry_update: self.registry_update_plan(),
            };
            self.append_pane_resize_event(&update)?;
            updates.push(update);
        }

        Ok(updates)
    }

    /// Applies a primary terminal resize to session geometry and tracked pane PTYs.
    pub fn resize_attached_primary_terminal(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        size: Size,
    ) -> Result<Vec<PaneResizeUpdate>> {
        self.require_live()?;
        validate_pane_size_for_resize(size)?;
        self.session
            .resize_authoritative_terminal(primary_client_id, size)?;
        let updates = self.sync_tracked_pty_sizes()?;
        self.append_lifecycle_event(
            EventKind::PaneChanged,
            format!(
                r#"{{"terminal_resize":"primary","columns":{},"rows":{},"resized_panes":{}}}"#,
                size.columns,
                size.rows,
                updates.len()
            ),
        )?;
        Ok(updates)
    }

    /// Runs the poll pane processes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn poll_pane_processes(&mut self) -> Result<Vec<PaneExitUpdate>> {
        self.require_live()?;
        let exited = self.pane_processes.poll_exited()?;
        let mut updates = Vec::new();

        for process in exited {
            updates.push(self.apply_exited_pane_process(process, true)?);
        }

        Ok(updates)
    }

    /// Applies one pane process exit event delivered by an async process watcher.
    ///
    /// The live polling path still removes recorded exits from
    /// `PaneProcessManager`; event-driven callers may not keep ownership there,
    /// so this method applies the session, event-log, registry, pane-pipe, and
    /// agent-turn cleanup without assuming the synchronous manager observed the
    /// exit first.
    pub fn apply_pane_process_exit_event(
        &mut self,
        pane_id: impl Into<String>,
        primary_pid: u32,
        exit_status: PaneExitStatus,
    ) -> Result<Option<PaneExitUpdate>> {
        self.require_live()?;
        let pane_id = pane_id.into();
        if self.find_pane_descriptor(&pane_id).is_none() {
            return Ok(None);
        }
        let primary_pid = if primary_pid == 0 {
            self.primary_pid_for_live_pane_process(&pane_id)
                .unwrap_or(0)
        } else {
            primary_pid
        };
        self.apply_exited_pane_process(
            ExitedPaneProcess {
                pane_id: pane_id.clone(),
                primary_pid,
                status: exit_status,
            },
            false,
        )
        .map(|update| {
            self.async_owned_pane_processes.remove(&pane_id);
            Some(update)
        })
    }

    /// Applies one process lifecycle failure delivered by an async watcher.
    ///
    /// This records a diagnostic pane event without closing the pane. The
    /// process may still be live when a watcher, wait task, resize operation, or
    /// write bridge fails, so lifecycle mutation remains the responsibility of a
    /// later explicit exit or termination event.
    pub fn apply_pane_process_failure_event(
        &mut self,
        pane_id: impl Into<String>,
        error: impl Into<String>,
    ) -> Result<bool> {
        self.require_live()?;
        let pane_id = pane_id.into();
        let Some(descriptor) = self.find_pane_descriptor(&pane_id) else {
            return Ok(false);
        };
        let error = error.into();
        let primary_pid = self
            .primary_pid_for_live_pane_process(descriptor.pane_id.as_str())
            .unwrap_or(0);
        self.append_lifecycle_event(
            EventKind::Diagnostic,
            format!(
                r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"process_state":"failed","error":"{}"}}"#,
                json_escape(descriptor.pane_id.as_str()),
                json_escape(descriptor.window_id.as_str()),
                primary_pid,
                json_escape(&error)
            ),
        )?;
        Ok(true)
    }

    /// Applies one process-spawn lifecycle event delivered by an async watcher.
    ///
    /// This is the event-driven equivalent of the post-spawn bookkeeping in
    /// `start_pane_process_with_start_directory`: it refreshes the pane's
    /// terminal screen state, marks readiness as unknown, queues bootstrap
    /// observation, and emits the replayable pane-start lifecycle event. The
    /// async process owner is responsible for retaining the live process handle.
    pub fn apply_pane_process_spawn_event(
        &mut self,
        pane_id: impl Into<String>,
        pid: Option<u32>,
    ) -> Result<bool> {
        self.require_live()?;
        let pane_id = pane_id.into();
        let Some(descriptor) = self.find_pane_descriptor(&pane_id) else {
            return Ok(false);
        };
        let primary_pid = pid
            .or_else(|| self.pane_processes.primary_pid(descriptor.pane_id.as_str()))
            .unwrap_or(0);
        self.pane_exit_records.remove(descriptor.pane_id.as_str());
        self.session
            .set_pane_live_state(descriptor.pane_id.as_str(), true)?;
        self.pane_screens.insert(
            descriptor.pane_id.to_string(),
            TerminalScreen::new_with_history_config(
                descriptor.size,
                self.terminal_history_limit,
                self.terminal_history_rotate_lines,
            )?,
        );
        self.pane_transaction_osc_screens.insert(
            descriptor.pane_id.to_string(),
            TerminalScreen::new_with_history_config(
                descriptor.size,
                self.terminal_history_limit,
                self.terminal_history_rotate_lines,
            )?,
        );
        self.pane_readiness_states
            .insert(descriptor.pane_id.to_string(), PaneReadinessState::Unknown);
        self.pane_bootstrap_pending
            .insert(descriptor.pane_id.to_string());

        let update = PaneProcessStart {
            session_id: self.session.id.to_string(),
            window_id: descriptor.window_id.to_string(),
            pane_id: descriptor.pane_id.to_string(),
            primary_pid,
            size: descriptor.size,
            registry_update: self.registry_update_plan(),
        };
        self.append_pane_start_event(&update)?;
        Ok(true)
    }

    /// Applies one pane input write failure delivered by an async pane driver.
    pub fn apply_pane_write_failure_event(
        &mut self,
        pane_id: impl Into<String>,
        error: impl Into<String>,
    ) -> Result<bool> {
        self.require_live()?;
        let pane_id = pane_id.into();
        let Some(descriptor) = self.find_pane_descriptor(&pane_id) else {
            return Ok(false);
        };
        let error = error.into();
        let pane_id = descriptor.pane_id.to_string();
        let window_id = descriptor.window_id.to_string();
        let primary_pid = self
            .primary_pid_for_live_pane_process(pane_id.as_str())
            .unwrap_or(0);
        self.append_lifecycle_event(
            EventKind::Diagnostic,
            format!(
                r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"pane_io":"write_failed","error":"{}"}}"#,
                json_escape(&pane_id),
                json_escape(&window_id),
                primary_pid,
                json_escape(&error)
            ),
        )?;
        self.fail_shell_transactions_for_pane_write_failure(&pane_id, &error)?;
        Ok(true)
    }

    /// Applies one pane input write completion delivered by an async pane driver.
    pub fn apply_pane_input_written_event(
        &mut self,
        pane_id: impl Into<String>,
        bytes: usize,
    ) -> Result<bool> {
        self.require_live()?;
        let pane_id = pane_id.into();
        if self.find_pane_descriptor(&pane_id).is_none() {
            return Ok(false);
        }
        let active_transactions = self
            .running_shell_transactions
            .iter()
            .filter(|(_, transaction)| transaction.pane_id == pane_id)
            .map(|(marker, transaction)| (marker.clone(), transaction.clone()))
            .collect::<Vec<_>>();
        for (marker, transaction) in &active_transactions {
            let action_fragment = match &transaction.kind {
                RunningShellTransactionKind::AgentAction { action_id } => {
                    format!(" action={action_id}")
                }
                RunningShellTransactionKind::ReadinessProbe
                | RunningShellTransactionKind::Bootstrap => String::new(),
            };
            self.append_agent_trace_turn_event(
                &pane_id,
                &transaction.turn_id,
                &format!(
                    "pane_input written bytes={} marker={} kind={}{}",
                    bytes,
                    marker,
                    runtime_running_shell_transaction_kind_name(&transaction.kind),
                    action_fragment
                ),
            )?;
        }
        Ok(!active_transactions.is_empty())
    }

    /// Fails live shell transactions for a pane whose PTY input write failed.
    fn fail_shell_transactions_for_pane_write_failure(
        &mut self,
        pane_id: &str,
        error: &str,
    ) -> Result<usize> {
        let failed_transactions = self
            .running_shell_transactions
            .iter()
            .filter(|(_, transaction)| transaction.pane_id == pane_id)
            .map(|(marker, transaction)| (marker.clone(), transaction.clone()))
            .collect::<Vec<_>>();
        let mut failed_count = 0usize;
        for (marker, transaction) in failed_transactions {
            if self.running_shell_transactions.remove(&marker).is_none() {
                continue;
            }
            failed_count = failed_count.saturating_add(1);
            match transaction.kind.clone() {
                RunningShellTransactionKind::AgentAction { action_id } => {
                    self.fail_agent_action_for_pane_write_failure(
                        &marker,
                        transaction,
                        &action_id,
                        error,
                    )?;
                }
                RunningShellTransactionKind::ReadinessProbe => {
                    self.fail_readiness_probe_for_pane_write_failure(&marker, transaction, error)?;
                }
                RunningShellTransactionKind::Bootstrap => {
                    self.fail_bootstrap_for_pane_write_failure(&marker, transaction, error)?;
                }
            }
        }
        Ok(failed_count)
    }

    /// Fails one running agent action when its pane input cannot be written.
    fn fail_agent_action_for_pane_write_failure(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        action_id: &str,
        error: &str,
    ) -> Result<()> {
        let previous = self.pane_readiness_state(&transaction.pane_id);
        self.set_pane_readiness(&transaction.pane_id, PaneReadinessState::Degraded);
        self.append_agent_trace_turn_event(
            &transaction.pane_id,
            &transaction.turn_id,
            &format!(
                "pane_readiness {} -> degraded reason=pane_input_write_failed marker={}",
                runtime_pane_readiness_state_name(previous),
                marker
            ),
        )?;
        let terminal_observation = pane_write_failure_terminal_observation(
            marker,
            &transaction,
            "pane-input-write-failed",
            error,
        );
        let _ = self.fail_running_shell_transaction_action(
            &transaction,
            marker,
            RuntimeShellTransactionActionFailure {
                action_id: action_id.to_string(),
                status: ActionStatus::Failed,
                code: "pane_input_write_failed".to_string(),
                message: format!("pane input write failed while sending shell action: {error}"),
                sent_to_pane: false,
                terminal_observation,
                trace_reason: "pane_input_write_failed".to_string(),
            },
        )?;
        Ok(())
    }

    /// Fails a pending shell action when its readiness probe cannot be written.
    fn fail_readiness_probe_for_pane_write_failure(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        error: &str,
    ) -> Result<()> {
        self.pane_readiness_overrides
            .clear_pending_probe(&transaction.pane_id);
        let previous = self.pane_readiness_state(&transaction.pane_id);
        self.set_pane_readiness(&transaction.pane_id, PaneReadinessState::Degraded);
        self.append_agent_trace_turn_event(
            &transaction.pane_id,
            &transaction.turn_id,
            &format!(
                "pane_readiness {} -> degraded reason=readiness_probe_pane_input_write_failed marker={}",
                runtime_pane_readiness_state_name(previous),
                marker
            ),
        )?;
        if let Some(action_id) = self.pending_shell_action_id_for_turn(&transaction.turn_id) {
            let terminal_observation = pane_write_failure_terminal_observation(
                marker,
                &transaction,
                "readiness-probe-pane-input-write-failed",
                error,
            );
            let _ = self.fail_running_shell_transaction_action(
                &transaction,
                marker,
                RuntimeShellTransactionActionFailure {
                    action_id,
                    status: ActionStatus::Failed,
                    code: "pane_input_write_failed".to_string(),
                    message: format!(
                        "pane input write failed while sending shell readiness probe: {error}"
                    ),
                    sent_to_pane: false,
                    terminal_observation,
                    trace_reason: "readiness_probe_pane_input_write_failed".to_string(),
                },
            )?;
        } else {
            self.append_agent_error_text_to_terminal_buffer(
                &transaction.pane_id,
                &format!("agent: shell readiness probe write failed: {error}"),
            )?;
            self.append_lifecycle_event(
                EventKind::AgentStatus,
                format!(
                    r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"degraded","readiness_probe":"write_failed","marker":"{}","error":"{}"}}"#,
                    json_escape(&transaction.pane_id),
                    json_escape(&transaction.turn_id),
                    json_escape(marker),
                    json_escape(error)
                ),
            )?;
        }
        Ok(())
    }

    /// Marks a bootstrap transaction degraded when its pane input cannot write.
    fn fail_bootstrap_for_pane_write_failure(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        error: &str,
    ) -> Result<()> {
        self.pane_bootstrap_pending.remove(&transaction.pane_id);
        let previous = self.pane_readiness_state(&transaction.pane_id);
        self.set_pane_readiness(&transaction.pane_id, PaneReadinessState::Degraded);
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","bootstrap":"write_failed","marker":"{}","previous_state":"{}","state":"degraded","error":"{}"}}"#,
                json_escape(&transaction.pane_id),
                json_escape(marker),
                runtime_pane_readiness_state_name(previous),
                json_escape(error)
            ),
        )?;
        Ok(())
    }

    /// Applies one PTY resize completion delivered by an async pane driver.
    pub fn apply_pane_resize_completion_event(
        &mut self,
        pane_id: impl Into<String>,
        size: Size,
    ) -> Result<bool> {
        self.require_live()?;
        let pane_id = pane_id.into();
        let Some(descriptor) = self.find_pane_descriptor(&pane_id) else {
            return Ok(false);
        };
        if let Some(screen) = self.pane_screens.get_mut(descriptor.pane_id.as_str()) {
            screen.resize(size);
        }
        if let Some(screen) = self
            .pane_transaction_osc_screens
            .get_mut(descriptor.pane_id.as_str())
        {
            screen.resize(size);
        }
        let primary_pid = self
            .pane_processes
            .primary_pid(descriptor.pane_id.as_str())
            .unwrap_or(0);
        self.append_lifecycle_event(
            EventKind::PaneChanged,
            format!(
                r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"pty_resize":"applied","columns":{},"rows":{}}}"#,
                json_escape(descriptor.pane_id.as_str()),
                json_escape(descriptor.window_id.as_str()),
                primary_pid,
                size.columns,
                size.rows
            ),
        )?;
        Ok(true)
    }

    /// Runs the apply exited pane process operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn apply_exited_pane_process(
        &mut self,
        process: ExitedPaneProcess,
        remove_recorded_process: bool,
    ) -> Result<PaneExitUpdate> {
        let descriptor = self.find_pane_descriptor(&process.pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "exited pane process has no matching pane",
            )
        })?;
        let previous_window_count = self.session.windows().len();

        let _ = self.stop_active_pane_pipe(process.pane_id.as_str());
        self.pane_current_working_directories
            .remove(process.pane_id.as_str());
        self.fail_agent_turns_for_pane_shutdown(
            std::slice::from_ref(&process.pane_id),
            "pane primary process exited",
        )?;
        self.pane_exit_records.insert(
            process.pane_id.clone(),
            PaneExitRecord {
                exit_status: process.status,
            },
        );
        self.close_exited_pane(&descriptor)?;
        if !self.session.windows().is_empty() {
            self.sync_tracked_pty_sizes()?;
        }
        if remove_recorded_process {
            self.pane_processes.remove_exited(&process.pane_id)?;
        }
        self.lifecycle_state = RuntimeLifecycleState::from_session_state(self.session.state);

        let closed_window = self.session.windows().len() < previous_window_count;
        let update = PaneExitUpdate {
            session_id: self.session.id.to_string(),
            window_id: descriptor.window_id.to_string(),
            pane_id: descriptor.pane_id.to_string(),
            primary_pid: process.primary_pid,
            exit_status: process.status,
            closed_window,
            session_empty: self.session.windows().is_empty(),
            registry_update: self.registry_update_plan(),
        };
        self.append_pane_exit_event(&update)?;
        if closed_window {
            self.append_lifecycle_event(
                EventKind::WindowChanged,
                format!(
                    r#"{{"window_id":"{}","state":"closed","session_empty":{}}}"#,
                    json_escape(&update.window_id),
                    update.session_empty
                ),
            )?;
        }
        self.persist_or_defer_registry_update_plan(update.registry_update.clone())?;
        Ok(update)
    }

    /// Runs the poll pane outputs operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn poll_pane_outputs(
        &mut self,
        max_bytes_per_pane: usize,
    ) -> Result<Vec<PaneOutputUpdate>> {
        self.require_live()?;
        let outputs = self
            .pane_processes
            .read_available_output(max_bytes_per_pane)?;
        let mut updates = Vec::new();
        let mut terminal_title_panes = BTreeSet::new();

        for output in outputs {
            updates.push(self.apply_pane_process_output(output, &mut terminal_title_panes)?);
        }

        if self.should_sync_pane_titles_from_foreground_processes(!updates.is_empty()) {
            self.sync_pane_titles_from_foreground_processes(&terminal_title_panes)?;
        }

        Ok(updates)
    }

    /// Applies one pane-output event delivered by an async pane driver.
    ///
    /// This is the event-driven equivalent of one item returned by
    /// `PaneProcessManager::read_available_output`. It preserves the same
    /// filtering, OSC observation, screen feeding, shell transaction tracking,
    /// pane-pipe forwarding, title syncing, and event-log behavior used by the
    /// synchronous polling path.
    pub fn apply_pane_output_bytes(
        &mut self,
        pane_id: impl Into<String>,
        bytes: Vec<u8>,
    ) -> Result<Option<PaneOutputUpdate>> {
        self.require_live()?;
        if bytes.is_empty() {
            return Ok(None);
        }
        let pane_id = pane_id.into();
        let primary_pid = self
            .primary_pid_for_live_pane_process(&pane_id)
            .unwrap_or(0);
        let mut terminal_title_panes = BTreeSet::new();
        let update = self.apply_pane_process_output(
            PaneProcessOutput {
                pane_id,
                primary_pid,
                bytes,
            },
            &mut terminal_title_panes,
        )?;
        if self.should_sync_pane_titles_from_foreground_processes(true) {
            self.sync_pane_titles_from_foreground_processes(&terminal_title_panes)?;
        }
        Ok(Some(update))
    }

    /// Runs the apply pane process output operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn apply_pane_process_output(
        &mut self,
        output: PaneProcessOutput,
        terminal_title_panes: &mut BTreeSet<String>,
    ) -> Result<PaneOutputUpdate> {
        let descriptor = self.find_pane_descriptor(&output.pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "pane output has no matching pane",
            )
        })?;
        let descriptor_size = descriptor.size;
        let descriptor_window_id = descriptor.window_id.to_string();
        let background = self
            .session
            .active_window()
            .is_none_or(|window| window.active_pane().id.as_str() != descriptor.pane_id.as_str());
        let transaction_bytes =
            self.visible_pane_output_bytes(output.pane_id.as_str(), &output.bytes);
        let render_bytes =
            self.renderable_pane_output_bytes(output.pane_id.as_str(), &transaction_bytes);
        let (osc_events, transaction_alternate_active) = self.terminal_osc_events_for_pane_bytes(
            output.pane_id.as_str(),
            descriptor_size,
            &transaction_bytes,
        )?;
        let (title, activity_events, bell_events, render_alternate_active) = {
            let screen = self.pane_screens.entry(output.pane_id.clone()).or_insert(
                TerminalScreen::new_with_history_config(
                    descriptor_size,
                    self.terminal_history_limit,
                    self.terminal_history_rotate_lines,
                )?,
            );
            let previous_activity_events = screen.activity_events();
            let previous_bell_events = screen.bell_events();
            screen.feed(&render_bytes);
            let _ = screen.drain_osc_events();
            (
                screen.title().map(ToOwned::to_owned),
                screen
                    .activity_events()
                    .saturating_sub(previous_activity_events),
                screen.bell_events().saturating_sub(previous_bell_events),
                screen.alternate_screen_active(),
            )
        };
        let alternate_active = transaction_alternate_active || render_alternate_active;
        let terminal_title = osc_events.iter().rev().find_map(|event| match event {
            TerminalOscEvent::TitleChanged { title } => Some(title.clone()),
            _ => None,
        });
        if terminal_title.is_some() {
            terminal_title_panes.insert(output.pane_id.clone());
        }
        self.apply_terminal_osc_events(&osc_events)?;
        if alternate_active {
            self.pane_readiness_overrides.revoke(
                output.pane_id.as_str(),
                ReadinessOverrideRevocation::AlternateScreenEntry,
            );
            self.set_pane_readiness(
                output.pane_id.as_str(),
                PaneReadinessState::InteractiveBlocked,
            );
        }
        self.record_running_shell_transaction_output(output.pane_id.as_str(), &transaction_bytes);
        self.observe_agent_shell_transaction_events(output.pane_id.as_str(), &osc_events)?;
        self.write_active_pane_pipe(output.pane_id.as_str(), &render_bytes)?;
        let title_changed = if let Some(title) = terminal_title.or(title) {
            self.session
                .set_pane_title_from_terminal(output.pane_id.as_str(), title)?
        } else {
            false
        };

        let update = PaneOutputUpdate {
            session_id: self.session.id.to_string(),
            window_id: descriptor_window_id,
            pane_id: output.pane_id,
            primary_pid: output.primary_pid,
            bytes_read: output.bytes.len(),
            activity_events,
            bell_events,
            background,
        };
        self.append_pane_output_event(&update)?;
        if title_changed {
            self.append_pane_title_event(&update)?;
        }
        Ok(update)
    }

    /// Returns pane bytes that should be retained for active Mezzanine-owned
    /// shell transactions after filtering wrapper echo that is irrelevant to the
    /// model and the runtime state machine.
    ///
    /// Interactive shells echo the wrapper lines that Mezzanine writes around
    /// agent actions, readiness probes, and bootstrap probes. Those lines are
    /// implementation traffic, not user commands, so normal transaction
    /// observation hides them while preserving command output and the OSC
    /// transaction markers that drive the runtime state machine. Trace logging
    /// disables this filter for diagnosis.
    pub(super) fn visible_pane_output_bytes(&mut self, pane_id: &str, bytes: &[u8]) -> Vec<u8> {
        if bytes.is_empty() {
            return Vec::new();
        }
        let active_transaction = self
            .running_shell_transactions
            .values()
            .any(|transaction| transaction.pane_id == pane_id);
        let filter_commands = self.mez_wrapper_filter_commands_for_pane(pane_id);
        if self.agent_trace_enabled(pane_id)
            || (filter_commands.is_empty()
                && !mez_wrapper_filter_bytes_may_contain_boilerplate(bytes))
        {
            let mut visible = self
                .pane_mez_wrapper_filter_pending
                .remove(pane_id)
                .unwrap_or_default();
            visible.extend_from_slice(bytes);
            if !active_transaction {
                self.tick_mez_wrapper_filter_retention(pane_id);
            }
            return visible;
        }

        let mut pending = self
            .pane_mez_wrapper_filter_pending
            .remove(pane_id)
            .unwrap_or_default();
        pending.extend_from_slice(bytes);
        let mut visible = Vec::with_capacity(pending.len());
        let mut filtered_wrapper_echo = false;
        let mut line_start = 0usize;
        while let Some(relative_terminator) = pending[line_start..]
            .iter()
            .position(|byte| *byte == b'\n' || *byte == b'\r')
        {
            let terminator = line_start + relative_terminator;
            let line_end = if pending[terminator] == b'\r'
                && pending
                    .get(terminator + 1)
                    .is_some_and(|byte| *byte == b'\n')
            {
                terminator + 2
            } else {
                terminator + 1
            };
            let line = &pending[line_start..line_end];
            let filtered_line = mez_wrapper_echo_line_visible_bytes(line, &filter_commands);
            if filtered_line.len() != line.len() {
                filtered_wrapper_echo = true;
            }
            visible.extend_from_slice(&filtered_line);
            line_start = line_end;
        }

        if line_start < pending.len() {
            let tail = &pending[line_start..];
            if tail.contains(&0x1b) {
                let filtered_tail = mez_wrapper_echo_line_visible_bytes(tail, &filter_commands);
                if filtered_tail.len() != tail.len() {
                    filtered_wrapper_echo = true;
                }
                visible.extend_from_slice(&filtered_tail);
            } else if mez_wrapper_echo_line_is_hidden(tail, &filter_commands) {
                filtered_wrapper_echo = true;
            } else if !mez_wrapper_echo_line_is_possible_prefix(tail, &filter_commands) {
                visible.extend_from_slice(tail);
            } else {
                filtered_wrapper_echo = true;
                self.pane_mez_wrapper_filter_pending
                    .insert(pane_id.to_string(), tail.to_vec());
            }
        }
        if !active_transaction {
            if filtered_wrapper_echo {
                self.pane_mez_wrapper_filter_recent_polls.insert(
                    pane_id.to_string(),
                    RUNTIME_SHELL_WRAPPER_FILTER_RETENTION_POLLS,
                );
            } else {
                self.tick_mez_wrapper_filter_retention(pane_id);
            }
        }
        visible
    }

    /// Runs the pane output render mode operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn pane_output_render_mode(&self, pane_id: &str) -> PaneOutputRenderMode {
        if self.agent_trace_enabled(pane_id) {
            return PaneOutputRenderMode::Trace;
        }
        let shell_view_enabled = self.agent_shell_view_enabled(pane_id);
        let mut has_agent_action = false;
        for transaction in self
            .running_shell_transactions
            .values()
            .filter(|transaction| transaction.pane_id == pane_id)
        {
            match &transaction.kind {
                RunningShellTransactionKind::AgentAction { .. } => {
                    has_agent_action = true;
                }
                RunningShellTransactionKind::ReadinessProbe
                | RunningShellTransactionKind::Bootstrap => {
                    return PaneOutputRenderMode::HiddenLiveAgentShell;
                }
            }
        }
        if has_agent_action {
            if shell_view_enabled {
                PaneOutputRenderMode::VerboseAgentAction
            } else {
                PaneOutputRenderMode::HiddenLiveAgentShell
            }
        } else if !shell_view_enabled
            && (self.pane_has_running_agent_turn(pane_id)
                || self.pane_agent_subshell_active(pane_id))
        {
            PaneOutputRenderMode::HiddenLiveAgentShell
        } else if !shell_view_enabled
            && self
                .pane_hidden_shell_render_recent_polls
                .contains_key(pane_id)
        {
            PaneOutputRenderMode::HiddenRetainedAgentShell
        } else {
            PaneOutputRenderMode::Normal
        }
    }

    /// Runs the renderable pane output bytes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn renderable_pane_output_bytes(
        &mut self,
        pane_id: &str,
        transaction_bytes: &[u8],
    ) -> Vec<u8> {
        match self.pane_output_render_mode(pane_id) {
            PaneOutputRenderMode::Normal
            | PaneOutputRenderMode::VerboseAgentAction
            | PaneOutputRenderMode::Trace => transaction_bytes.to_vec(),
            PaneOutputRenderMode::HiddenLiveAgentShell => {
                if !transaction_bytes.is_empty() {
                    self.remember_hidden_shell_render_suppression(pane_id);
                }
                Vec::new()
            }
            PaneOutputRenderMode::HiddenRetainedAgentShell => Vec::new(),
        }
    }

    /// Reports whether the pane has a runtime agent turn currently occupying
    /// the pane's agent shell session.
    fn pane_has_running_agent_turn(&self, pane_id: &str) -> bool {
        self.agent_shell_store
            .get(pane_id)
            .and_then(|session| session.running_turn_id.as_deref())
            .is_some()
    }

    /// Reports whether a pane currently owns a child shell for agent mode.
    ///
    /// The child shell's prompt and setup repaint are implementation traffic
    /// unless shell-view diagnostics are enabled.
    fn pane_agent_subshell_active(&self, pane_id: &str) -> bool {
        self.agent_subshell_panes.contains(pane_id)
    }

    /// Retains short-lived shell-output suppression after a hidden agent shell
    /// transaction so delayed prompt repaint bytes do not leak into the pane.
    pub(super) fn remember_hidden_shell_render_suppression(&mut self, pane_id: &str) {
        self.pane_hidden_shell_render_recent_polls.insert(
            pane_id.to_string(),
            RUNTIME_HIDDEN_SHELL_RENDER_RETENTION_POLLS,
        );
    }

    /// Clears retained shell-output filters for explicit foreground input.
    ///
    /// Hidden-shell and wrapper-echo retention suppress delayed implementation
    /// prompt repaint bytes after agent-owned shell work. Once foreground
    /// control returns to the pane, following PTY output belongs to the user's
    /// interaction and must not be swallowed or reduced to cursor-control
    /// remnants by the previous agent turn's cleanup window.
    pub(super) fn clear_shell_output_filters_for_foreground_input(&mut self, pane_id: &str) {
        self.pane_hidden_shell_render_recent_polls.remove(pane_id);
        self.pane_mez_wrapper_filter_pending.remove(pane_id);
        self.pane_mez_wrapper_filter_recent_commands.remove(pane_id);
        self.pane_mez_wrapper_filter_recent_polls.remove(pane_id);
    }

    /// Ages out retained shell-output suppression for panes whose agent turn and
    /// Mezzanine-owned shell transaction have both settled.
    pub(super) fn tick_hidden_shell_render_retention(&mut self) -> usize {
        let mut aged = 0usize;
        let retained = self
            .pane_hidden_shell_render_recent_polls
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for pane_id in retained {
            if self.pane_has_running_agent_turn(&pane_id)
                || self
                    .running_shell_transactions
                    .values()
                    .any(|transaction| transaction.pane_id == pane_id)
            {
                continue;
            }
            let Some(remaining) = self.pane_hidden_shell_render_recent_polls.get_mut(&pane_id)
            else {
                continue;
            };
            *remaining = remaining.saturating_sub(1);
            aged = aged.saturating_add(1);
            if *remaining == 0 {
                self.pane_hidden_shell_render_recent_polls.remove(&pane_id);
            }
        }
        aged
    }

    /// Applies runtime idle-cleanup timer work that does not require polling PTY
    /// or process state.
    ///
    /// Migrated cleanup targets include hidden-shell render suppression
    /// retention, stranded agent dispatch recovery, and unreachable running
    /// turn failure. These operations
    /// are driven by actor timer events in the daemon path so idle sessions do
    /// not need to scan them through the compatibility tick.
    pub fn apply_idle_cleanup_timer_event(&mut self) -> Result<usize> {
        self.apply_idle_cleanup_timer_event_with_actor_progress(&BTreeSet::new())
    }

    /// Applies runtime idle-cleanup timer work while honoring actor-owned
    /// progress.
    ///
    /// # Parameters
    /// - `actor_progress_turn_ids`: Running turns with progress represented by
    ///   async actor state rather than service-owned queues.
    pub fn apply_idle_cleanup_timer_event_with_actor_progress(
        &mut self,
        actor_progress_turn_ids: &BTreeSet<String>,
    ) -> Result<usize> {
        match self.lifecycle_state {
            RuntimeLifecycleState::Killed | RuntimeLifecycleState::Failed => Ok(0),
            RuntimeLifecycleState::Running
            | RuntimeLifecycleState::Detached
            | RuntimeLifecycleState::Stopping => {
                let hidden_shell_retention_aged = self.tick_hidden_shell_render_retention();
                let reconciled = self.reconcile_agent_runtime_progress_paths_with_actor_progress(
                    actor_progress_turn_ids,
                )?;
                Ok(hidden_shell_retention_aged.saturating_add(reconciled))
            }
        }
    }

    /// Reconciles running agent turns after actor-owned state transitions.
    ///
    /// The runtime specification requires every running turn to retain an
    /// observable progress path. Calling this after event application prevents
    /// stranded turns from depending on a later idle timer before they are
    /// requeued or failed.
    pub fn reconcile_agent_runtime_progress_paths(&mut self) -> Result<usize> {
        self.reconcile_agent_runtime_progress_paths_with_actor_progress(&BTreeSet::new())
    }

    /// Reconciles running agent turns while honoring actor-owned progress.
    ///
    /// # Parameters
    /// - `actor_progress_turn_ids`: Turns waiting on progress owned by the
    ///   async actor, such as provider retry timers that are not represented in
    ///   service-owned queues.
    pub fn reconcile_agent_runtime_progress_paths_with_actor_progress(
        &mut self,
        actor_progress_turn_ids: &BTreeSet<String>,
    ) -> Result<usize> {
        if matches!(
            self.lifecycle_state,
            RuntimeLifecycleState::Killed | RuntimeLifecycleState::Failed
        ) {
            return Ok(0);
        }
        let stranded_shell_recoveries = self.recover_stranded_agent_shell_dispatches()?;
        let unreachable_turn_failures =
            self.fail_unreachable_running_agent_turns_with_actor_progress(actor_progress_turn_ids)?;
        Ok(stranded_shell_recoveries.saturating_add(unreachable_turn_failures))
    }

    /// Reports whether actor-owned idle cleanup should remain scheduled.
    ///
    /// Hidden agent-shell render suppression retention must age out after the
    /// shell transaction and running turn settle so delayed prompt bytes do not
    /// leak into normal pane rendering. Stranded shell-dispatch recovery must
    /// also retry while a pending shell command is blocked behind stale
    /// readiness state.
    pub fn idle_cleanup_timer_needed(&self) -> bool {
        self.idle_cleanup_timer_needed_with_actor_progress(&BTreeSet::new())
    }

    /// Reports whether actor-owned idle cleanup should remain scheduled while
    /// honoring actor-owned progress.
    ///
    /// # Parameters
    /// - `actor_progress_turn_ids`: Running turns with progress represented by
    ///   async actor state rather than service-owned queues.
    pub fn idle_cleanup_timer_needed_with_actor_progress(
        &self,
        actor_progress_turn_ids: &BTreeSet<String>,
    ) -> bool {
        self.hidden_shell_render_retention_timer_needed()
            || self.stranded_agent_shell_dispatch_recovery_timer_needed()
            || self.unreachable_running_agent_turn_timer_needed_with_actor_progress(
                actor_progress_turn_ids,
            )
    }

    /// Reports whether hidden shell-render suppression still needs to age out.
    pub fn hidden_shell_render_retention_timer_needed(&self) -> bool {
        !self.pane_hidden_shell_render_recent_polls.is_empty()
    }

    /// Reports whether any pending agent shell dispatch may need recovery.
    pub fn stranded_agent_shell_dispatch_recovery_timer_needed(&self) -> bool {
        !self
            .stranded_agent_shell_dispatch_recovery_candidates()
            .is_empty()
    }

    /// Reports whether any running turn has no remaining runtime progress path.
    pub fn unreachable_running_agent_turn_timer_needed(&self) -> bool {
        self.unreachable_running_agent_turn_timer_needed_with_actor_progress(&BTreeSet::new())
    }

    /// Reports whether any running turn has no remaining runtime progress path
    /// after accounting for async actor-owned progress.
    ///
    /// # Parameters
    /// - `actor_progress_turn_ids`: Running turns with progress represented by
    ///   async actor state rather than service-owned queues.
    pub fn unreachable_running_agent_turn_timer_needed_with_actor_progress(
        &self,
        actor_progress_turn_ids: &BTreeSet<String>,
    ) -> bool {
        !self
            .unreachable_running_agent_turn_candidates(actor_progress_turn_ids)
            .is_empty()
    }

    /// Runs the terminal osc events for pane bytes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn terminal_osc_events_for_pane_bytes(
        &mut self,
        pane_id: &str,
        size: Size,
        bytes: &[u8],
    ) -> Result<(Vec<TerminalOscEvent>, bool)> {
        if bytes.is_empty() {
            return Ok((Vec::new(), false));
        }
        if matches!(
            self.pane_output_render_mode(pane_id),
            PaneOutputRenderMode::HiddenLiveAgentShell
                | PaneOutputRenderMode::HiddenRetainedAgentShell
        ) {
            return Ok((
                self.hidden_agent_shell_osc_events_for_pane_bytes(pane_id, bytes),
                false,
            ));
        }
        let screen = if let Some(screen) = self.pane_transaction_osc_screens.get_mut(pane_id) {
            screen.resize(size);
            screen
        } else {
            self.pane_transaction_osc_screens.insert(
                pane_id.to_string(),
                TerminalScreen::new_with_history_config(
                    size,
                    self.terminal_history_limit,
                    self.terminal_history_rotate_lines,
                )?,
            );
            self.pane_transaction_osc_screens
                .get_mut(pane_id)
                .ok_or_else(|| {
                    MezError::invalid_state("transaction OSC parser was not retained for pane")
                })?
        };
        screen.feed(bytes);
        Ok((screen.drain_osc_events(), screen.alternate_screen_active()))
    }

    /// Scans hidden agent-shell bytes for Mezzanine-owned OSC transaction
    /// markers without feeding arbitrary command output into a terminal parser.
    ///
    /// Hidden agent-shell output is command data for the model. Treating long
    /// file bodies or embedded escape sequences as terminal traffic can
    /// monopolize the runtime actor and mutate parser state. This scanner keeps
    /// only a bounded fragment that may contain a split `ESC ] 133` marker and
    /// ignores all other hidden bytes.
    fn hidden_agent_shell_osc_events_for_pane_bytes(
        &mut self,
        pane_id: &str,
        bytes: &[u8],
    ) -> Vec<TerminalOscEvent> {
        let mut pending = self
            .pane_transaction_osc_pending
            .remove(pane_id)
            .unwrap_or_default();
        pending.extend_from_slice(bytes);
        let (events, retained) = scan_mezzanine_osc_transaction_events(&pending);
        if retained.is_empty() {
            self.pane_transaction_osc_pending.remove(pane_id);
        } else {
            self.pane_transaction_osc_pending
                .insert(pane_id.to_string(), retained);
        }
        events
    }

    /// Runs the remember mez wrapper filter command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn remember_mez_wrapper_filter_command(&mut self, pane_id: &str, command: &str) {
        let retained = self
            .pane_mez_wrapper_filter_recent_commands
            .entry(pane_id.to_string())
            .or_default();
        for line in command
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            if !retained.iter().any(|existing| existing == line) {
                retained.push(line.to_string());
            }
        }
        let extra = retained
            .len()
            .saturating_sub(RUNTIME_SHELL_WRAPPER_FILTER_RECENT_COMMAND_LIMIT);
        if extra > 0 {
            retained.drain(0..extra);
        }
        self.pane_mez_wrapper_filter_recent_polls.insert(
            pane_id.to_string(),
            RUNTIME_SHELL_WRAPPER_FILTER_RETENTION_POLLS,
        );
    }

    /// Runs the mez wrapper filter commands for pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn mez_wrapper_filter_commands_for_pane(&self, pane_id: &str) -> Vec<String> {
        let mut commands = self
            .running_shell_transactions
            .values()
            .filter(|transaction| transaction.pane_id == pane_id)
            .flat_map(|transaction| {
                transaction
                    .command
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        if let Some(retained) = self.pane_mez_wrapper_filter_recent_commands.get(pane_id) {
            for command in retained {
                if !commands.iter().any(|existing| existing == command) {
                    commands.push(command.clone());
                }
            }
        }
        commands
    }

    /// Runs the tick mez wrapper filter retention operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn tick_mez_wrapper_filter_retention(&mut self, pane_id: &str) {
        let Some(remaining) = self.pane_mez_wrapper_filter_recent_polls.get_mut(pane_id) else {
            return;
        };
        *remaining = remaining.saturating_sub(1);
        if *remaining == 0 {
            self.pane_mez_wrapper_filter_recent_polls.remove(pane_id);
            self.pane_mez_wrapper_filter_recent_commands.remove(pane_id);
            self.pane_mez_wrapper_filter_pending.remove(pane_id);
        }
    }

    /// Runs the sync pane titles from foreground processes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn sync_pane_titles_from_foreground_processes(
        &mut self,
        skipped_panes: &BTreeSet<String>,
    ) -> Result<usize> {
        let mut changed = 0usize;
        for pane_id in self.pane_processes.tracked_pane_ids() {
            if skipped_panes.contains(&pane_id) {
                continue;
            }
            let Some(title) = self.foreground_process_pane_title(&pane_id) else {
                continue;
            };
            if !self
                .session
                .set_pane_title_from_terminal(pane_id.as_str(), title)?
            {
                continue;
            }
            let Some(update) = self.pane_title_only_update(&pane_id) else {
                continue;
            };
            self.append_pane_title_event(&update)?;
            changed = changed.saturating_add(1);
        }
        Ok(changed)
    }

    /// Runs the should sync pane titles from foreground processes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn should_sync_pane_titles_from_foreground_processes(&mut self, observed_output: bool) -> bool {
        if observed_output {
            self.foreground_title_idle_sync_polls = 0;
            return true;
        }
        let should_sync = self.foreground_title_idle_sync_polls == 0;
        self.foreground_title_idle_sync_polls =
            self.foreground_title_idle_sync_polls.saturating_add(1)
                % RUNTIME_FOREGROUND_TITLE_IDLE_SYNC_POLL_INTERVAL;
        should_sync
    }

    /// Runs the foreground process pane title operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn foreground_process_pane_title(&self, pane_id: &str) -> Option<String> {
        let foreground_name = self.pane_processes.foreground_process_name(pane_id)?;
        let foreground_group = self.pane_processes.foreground_process_group_id(pane_id)?;
        let primary_pid = self.pane_processes.primary_pid(pane_id)?;
        self.title_from_foreground_process_metadata(
            pane_id,
            foreground_name,
            foreground_group,
            primary_pid,
        )
    }

    /// Applies foreground process metadata delivered by an async pane worker.
    pub fn apply_pane_foreground_process_event(
        &mut self,
        pane_id: impl Into<String>,
        process_name: impl Into<String>,
        process_group_id: u32,
        current_working_directory: Option<String>,
    ) -> Result<bool> {
        self.require_live()?;
        let pane_id = pane_id.into();
        let Some(primary_pid) = self.primary_pid_for_live_pane_process(&pane_id) else {
            return Ok(false);
        };
        if let Some(current_working_directory) = current_working_directory
            && !current_working_directory.trim().is_empty()
        {
            self.pane_current_working_directories
                .insert(pane_id.clone(), PathBuf::from(current_working_directory));
        }
        let Some(title) = self.title_from_foreground_process_metadata(
            &pane_id,
            process_name.into(),
            process_group_id,
            primary_pid,
        ) else {
            return Ok(false);
        };
        if !self
            .session
            .set_pane_title_from_terminal(pane_id.as_str(), title)?
        {
            return Ok(false);
        }
        let Some(update) = self.pane_title_only_update(&pane_id) else {
            return Ok(false);
        };
        self.append_pane_title_event(&update)?;
        Ok(true)
    }

    /// Returns the best known current working directory for a live pane.
    ///
    /// Async pane workers publish foreground metadata into
    /// `pane_current_working_directories`; prefer that actor-owned snapshot so
    /// command planning does not synchronously probe host process metadata when
    /// an async observation is already available.
    pub(super) fn pane_current_working_directory(&self, pane_id: &str) -> Option<PathBuf> {
        self.pane_current_working_directories
            .get(pane_id)
            .cloned()
            .or_else(|| self.pane_processes.current_working_directory(pane_id))
    }

    /// Runs the title from foreground process metadata operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn title_from_foreground_process_metadata(
        &self,
        pane_id: &str,
        foreground_name: String,
        foreground_group: u32,
        primary_pid: u32,
    ) -> Option<String> {
        if foreground_group == primary_pid
            && Some(foreground_name.as_str()) == self.session.shell.path().file_name()?.to_str()
        {
            return self
                .pane_screens
                .get(pane_id)
                .and_then(TerminalScreen::title)
                .filter(|title| !title.trim().is_empty())
                .map(ToOwned::to_owned)
                .or_else(|| Some("shell".to_string()));
        }
        Some(foreground_name)
    }

    /// Runs the pane title only update operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn pane_title_only_update(&self, pane_id: &str) -> Option<PaneOutputUpdate> {
        let descriptor = self.find_pane_descriptor(pane_id)?;
        Some(PaneOutputUpdate {
            session_id: self.session.id.to_string(),
            window_id: descriptor.window_id.to_string(),
            pane_id: pane_id.to_string(),
            primary_pid: self.primary_pid_for_live_pane_process(pane_id)?,
            bytes_read: 0,
            activity_events: 0,
            bell_events: 0,
            background: !self.session.active_window().is_some_and(|window| {
                window.active_pane().id.as_str() == descriptor.pane_id.as_str()
            }),
        })
    }

    /// Runs the record running shell transaction output operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn record_running_shell_transaction_output(&mut self, pane_id: &str, bytes: &[u8]) {
        let mut status_line_updates = Vec::new();
        for (marker, transaction) in self.running_shell_transactions.iter_mut() {
            if transaction.pane_id == pane_id {
                let observed_bytes = match transaction.kind {
                    RunningShellTransactionKind::AgentAction { .. } => {
                        let transaction_bytes =
                            agent_shell_transaction_bytes_before_end_marker(bytes, marker);
                        agent_shell_transaction_observation_bytes(
                            transaction_bytes,
                            &transaction.command,
                        )
                    }
                    RunningShellTransactionKind::ReadinessProbe
                    | RunningShellTransactionKind::Bootstrap => bytes.to_vec(),
                };
                transaction.observed_output_bytes = transaction
                    .observed_output_bytes
                    .saturating_add(observed_bytes.len());
                let observation_limit = runtime_shell_transaction_observation_limit(transaction);
                if transaction.observed_output_preview.len() >= observation_limit {
                    if !observed_bytes.is_empty() {
                        transaction.observed_output_truncated = true;
                    }
                    continue;
                }
                let remaining =
                    observation_limit.saturating_sub(transaction.observed_output_preview.len());
                let text = String::from_utf8_lossy(&observed_bytes);
                let mut appended = 0usize;
                for ch in text.chars() {
                    let char_len = ch.len_utf8();
                    if appended + char_len > remaining {
                        transaction.observed_output_truncated = true;
                        break;
                    }
                    transaction.observed_output_preview.push(ch);
                    appended += char_len;
                }
                if appended < text.len() {
                    transaction.observed_output_truncated = true;
                }
                if let RunningShellTransactionKind::AgentAction { action_id } = &transaction.kind
                    && let Some(line) = latest_agent_shell_transaction_output_line(
                        &transaction.observed_output_preview,
                    )
                {
                    status_line_updates.push((
                        transaction.turn_id.clone(),
                        action_id.clone(),
                        transaction.pane_id.clone(),
                        line,
                    ));
                }
            }
        }
        for (turn_id, action_id, pane_id, line) in status_line_updates {
            if self.agent_shell_transaction_action_shows_live_output(&turn_id, &action_id) {
                let _ =
                    self.append_agent_shell_output_status_line_to_terminal_buffer(&pane_id, &line);
            }
        }
    }

    /// Applies a runtime timer firing for live Mezzanine-owned shell
    /// transactions.
    ///
    /// Returns the number of transactions that were expired. A zero return
    /// means the timer was accepted but no live transaction had reached its
    /// deadline.
    pub fn apply_shell_transaction_timer_event(&mut self, now_unix_ms: u64) -> Result<usize> {
        let expired = self.expire_timed_out_shell_transactions(now_unix_ms)?;
        let focused = self.expire_timed_out_focused_shell_hooks(now_unix_ms)?;
        Ok(expired.saturating_add(focused))
    }

    /// Returns timer-visible snapshots for live shell transactions with
    /// configured timeouts.
    pub fn running_shell_transaction_timers(&self) -> Vec<RuntimeShellTransactionTimerRef> {
        let mut timers = self
            .running_shell_transactions
            .iter()
            .filter_map(|(marker, transaction)| {
                let timeout_ms = runtime_shell_transaction_effective_timeout_ms(transaction)?;
                Some(RuntimeShellTransactionTimerRef {
                    marker: marker.clone(),
                    kind: runtime_shell_transaction_timer_kind(&transaction.kind),
                    started_at_unix_ms: transaction.started_at_unix_ms,
                    timeout_ms,
                })
            })
            .collect::<Vec<_>>();
        timers.extend(
            self.focused_shell_hook_transactions
                .iter()
                .map(|(marker, transaction)| RuntimeShellTransactionTimerRef {
                    marker: marker.clone(),
                    kind: RuntimeShellTransactionTimerKind::FocusedShellHook,
                    started_at_unix_ms: transaction.started_at_unix_ms,
                    timeout_ms: transaction.timeout_ms,
                }),
        );
        timers
    }

    /// Expires live Mezzanine-owned shell transactions whose runtime timeout has
    /// elapsed without observing their expected terminal marker.
    pub(super) fn expire_timed_out_shell_transactions(
        &mut self,
        now_unix_ms: u64,
    ) -> Result<usize> {
        let expired = self
            .running_shell_transactions
            .iter()
            .filter_map(|(marker, transaction)| {
                let timeout_ms = runtime_shell_transaction_effective_timeout_ms(transaction)?;
                let elapsed_ms = now_unix_ms.saturating_sub(transaction.started_at_unix_ms);
                (elapsed_ms >= timeout_ms)
                    .then(|| (marker.clone(), transaction.clone(), timeout_ms, elapsed_ms))
            })
            .collect::<Vec<_>>();
        let mut expired_count = 0usize;
        for (marker, transaction, timeout_ms, elapsed_ms) in expired {
            if self.running_shell_transactions.remove(&marker).is_none() {
                continue;
            }
            expired_count = expired_count.saturating_add(1);
            match transaction.kind.clone() {
                RunningShellTransactionKind::AgentAction { action_id } => {
                    self.expire_agent_action_shell_transaction(
                        &marker,
                        transaction,
                        &action_id,
                        timeout_ms,
                        elapsed_ms,
                    )?;
                }
                RunningShellTransactionKind::ReadinessProbe => {
                    self.expire_readiness_probe_shell_transaction(
                        &marker,
                        transaction,
                        timeout_ms,
                        elapsed_ms,
                    )?;
                }
                RunningShellTransactionKind::Bootstrap => {
                    self.expire_bootstrap_shell_transaction(
                        &marker,
                        transaction,
                        timeout_ms,
                        elapsed_ms,
                    )?;
                }
            }
        }
        Ok(expired_count)
    }

    /// Runs the expire timed out focused shell hooks operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn expire_timed_out_focused_shell_hooks(
        &mut self,
        now_unix_ms: u64,
    ) -> Result<usize> {
        let expired = self
            .focused_shell_hook_transactions
            .iter()
            .filter_map(|(marker, transaction)| {
                let elapsed_ms = now_unix_ms.saturating_sub(transaction.started_at_unix_ms);
                (elapsed_ms >= transaction.timeout_ms).then(|| marker.clone())
            })
            .collect::<Vec<_>>();
        let mut expired_count = 0usize;
        for marker in expired {
            let Some(pending) = self.focused_shell_hook_transactions.remove(&marker) else {
                continue;
            };
            expired_count = expired_count.saturating_add(1);
            let result = focused_shell_pre_action_timeout_result(&pending.plan);
            if let Some(audit_log) = self.audit_log.as_mut() {
                let record = hook_execution_audit_record(
                    &pending.plan,
                    self.session.id.as_str(),
                    AuditActor {
                        kind: "runtime".to_string(),
                        id: "focused-shell-hook-timeout".to_string(),
                    },
                    "runtime_focused_shell_timeout",
                    &result,
                )
                .with_pane_id(pending.pane_id.clone());
                let _ = audit_log.append(record)?;
            }
            self.append_lifecycle_event(
                EventKind::HookFailed,
                format!(
                    r#"{{"hook_id":"{}","event":"{}","pane_id":"{}","marker":"{}","failure_kind":"Timeout"}}"#,
                    json_escape(&pending.plan.hook_id),
                    runtime_hook_event_name(pending.plan.event),
                    json_escape(&pending.pane_id),
                    json_escape(&marker)
                ),
            )?;
            if let Some(continuation) = pending.continuation.as_ref() {
                let decision = self.record_hook_result(&pending.plan, &result, false)?;
                if decision == crate::hooks::HookFailureDecision::Block {
                    let block = RuntimeHookPipelineBlock::from_result(&result);
                    let _ = self.fail_pending_shell_action_for_hook_block(continuation, &block)?;
                } else {
                    self.record_agent_pre_shell_hook_completed(continuation, &pending.plan.hook_id);
                    let _ = self.dispatch_stored_running_shell_actions(&continuation.turn_id)?;
                }
            }
            self.push_focused_shell_hook_result(result);
        }
        Ok(expired_count)
    }

    /// Fails a timed-out agent shell action and interrupts the pane command when
    /// the runtime can still reach the pane process.
    fn expire_agent_action_shell_transaction(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        action_id: &str,
        timeout_ms: u64,
        elapsed_ms: u64,
    ) -> Result<()> {
        self.interrupt_shell_transaction_pane(&transaction.pane_id)?;
        let previous = self.pane_readiness_state(&transaction.pane_id);
        self.set_pane_readiness(&transaction.pane_id, PaneReadinessState::Degraded);
        self.append_agent_trace_turn_event(
            &transaction.pane_id,
            &transaction.turn_id,
            &format!(
                "pane_readiness {} -> degraded reason=shell_transaction_timeout marker={}",
                runtime_pane_readiness_state_name(previous),
                marker
            ),
        )?;
        let message = format!("shell command timed out after {timeout_ms} ms");
        let terminal_observation = serde_json::json!({
            "source": "pty",
            "stream": "pty_combined",
            "marker": marker,
            "exit_code": null,
            "signal": null,
            "timed_out": true,
            "timeout_ms": timeout_ms,
            "elapsed_ms": elapsed_ms,
            "combined_output_bytes": transaction.observed_output_bytes,
            "combined_output_preview": transaction.observed_output_preview,
            "boundary_state": "timeout",
            "output_truncated": transaction.observed_output_truncated
        });
        let _ = self.fail_running_shell_transaction_action(
            &transaction,
            marker,
            RuntimeShellTransactionActionFailure {
                action_id: action_id.to_string(),
                status: ActionStatus::TimedOut,
                code: "shell_timeout".to_string(),
                message,
                sent_to_pane: true,
                terminal_observation,
                trace_reason: "shell_transaction_timeout".to_string(),
            },
        )?;
        Ok(())
    }

    /// Settles a readiness probe timeout and fails the pending shell action that
    /// depended on the probe, when such an action is still present.
    fn expire_readiness_probe_shell_transaction(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        timeout_ms: u64,
        elapsed_ms: u64,
    ) -> Result<()> {
        self.interrupt_shell_transaction_pane(&transaction.pane_id)?;
        self.pane_readiness_overrides
            .clear_pending_probe(&transaction.pane_id);
        let previous = self.pane_readiness_state(&transaction.pane_id);
        self.set_pane_readiness(&transaction.pane_id, PaneReadinessState::Degraded);
        self.append_agent_trace_turn_event(
            &transaction.pane_id,
            &transaction.turn_id,
            &format!(
                "pane_readiness {} -> degraded reason=readiness_probe_timeout marker={}",
                runtime_pane_readiness_state_name(previous),
                marker
            ),
        )?;
        if let Some(action_id) = self.pending_shell_action_id_for_turn(&transaction.turn_id) {
            let message =
                format!("shell readiness probe timed out after {timeout_ms} ms before dispatch");
            let terminal_observation = serde_json::json!({
                "source": "pty",
                "stream": "pty_combined",
                "marker": marker,
                "exit_code": null,
                "signal": null,
                "timed_out": true,
                "timeout_ms": timeout_ms,
                "elapsed_ms": elapsed_ms,
                "combined_output_bytes": transaction.observed_output_bytes,
                "combined_output_preview": transaction.observed_output_preview,
                "boundary_state": "readiness-probe-timeout",
                "output_truncated": transaction.observed_output_truncated
            });
            let _ = self.fail_running_shell_transaction_action(
                &transaction,
                marker,
                RuntimeShellTransactionActionFailure {
                    action_id,
                    status: ActionStatus::TimedOut,
                    code: "readiness_probe_timeout".to_string(),
                    message,
                    sent_to_pane: false,
                    terminal_observation,
                    trace_reason: "readiness_probe_timeout".to_string(),
                },
            )?;
        } else {
            self.append_agent_error_text_to_terminal_buffer(
                &transaction.pane_id,
                &format!("agent: shell readiness probe timed out after {timeout_ms} ms"),
            )?;
            self.append_lifecycle_event(
                EventKind::AgentStatus,
                format!(
                    r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"degraded","readiness_probe":"timed_out","marker":"{}","timeout_ms":{},"elapsed_ms":{}}}"#,
                    json_escape(&transaction.pane_id),
                    json_escape(&transaction.turn_id),
                    json_escape(marker),
                    timeout_ms,
                    elapsed_ms
                ),
            )?;
        }
        Ok(())
    }

    /// Marks a timed-out bootstrap transaction as a degraded one-shot attempt
    /// instead of retrying the hidden bootstrap wrapper indefinitely.
    fn expire_bootstrap_shell_transaction(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        timeout_ms: u64,
        elapsed_ms: u64,
    ) -> Result<()> {
        self.interrupt_shell_transaction_pane(&transaction.pane_id)?;
        self.pane_bootstrap_pending.remove(&transaction.pane_id);
        let previous = self.pane_readiness_state(&transaction.pane_id);
        self.set_pane_readiness(&transaction.pane_id, PaneReadinessState::Degraded);
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","bootstrap":"timed_out","marker":"{}","previous_state":"{}","state":"degraded","timeout_ms":{},"elapsed_ms":{},"output_bytes":{},"output_truncated":{}}}"#,
                json_escape(&transaction.pane_id),
                json_escape(marker),
                runtime_pane_readiness_state_name(previous),
                timeout_ms,
                elapsed_ms,
                transaction.observed_output_bytes,
                transaction.observed_output_truncated
            ),
        )?;
        Ok(())
    }

    /// Sends an interrupt to the pane shell for a timed-out transaction while
    /// tolerating panes that have already exited.
    fn interrupt_shell_transaction_pane(&mut self, pane_id: &str) -> Result<()> {
        match self.write_runtime_pane_input(pane_id, b"\x03") {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == crate::error::MezErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Returns the first still-running shell action that has not produced a
    /// terminal action result for the given turn.
    fn pending_shell_action_id_for_turn(&self, turn_id: &str) -> Option<String> {
        let execution = self.agent_turn_executions.get(turn_id)?;
        let batch = execution.response.action_batch.as_ref()?;
        execution
            .action_results
            .iter()
            .find(|result| {
                result.status == ActionStatus::Running
                    && batch
                        .actions
                        .iter()
                        .find(|action| action.id == result.action_id)
                        .and_then(|action| local_action_plan(action).ok().flatten())
                        .is_some()
            })
            .map(|result| result.action_id.clone())
    }

    /// Requeues pending shell dispatches that have no live transaction and are
    /// waiting behind readiness state that can be safely retried.
    pub(super) fn recover_stranded_agent_shell_dispatches(&mut self) -> Result<usize> {
        let candidates = self.stranded_agent_shell_dispatch_recovery_candidates();
        let mut recovered = 0usize;
        for turn_id in candidates {
            let Some(turn) = self
                .agent_turn_ledger
                .turns()
                .iter()
                .find(|turn| turn.turn_id == turn_id && turn.state == AgentTurnState::Running)
                .cloned()
            else {
                continue;
            };
            if self
                .agent_turn_executions
                .get(&turn_id)
                .is_some_and(runtime_execution_ready_for_provider_continuation)
            {
                if self
                    .pending_agent_provider_tasks
                    .insert(turn.turn_id.clone())
                {
                    recovered = recovered.saturating_add(1);
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        "provider_task queued reason=ready_provider_continuation_recovery",
                    )?;
                }
                continue;
            }
            let readiness = self.pane_readiness_state(&turn.pane_id);
            match readiness {
                PaneReadinessState::Ready
                | PaneReadinessState::Unknown
                | PaneReadinessState::PromptCandidate
                | PaneReadinessState::Degraded => {
                    if self
                        .pending_agent_provider_tasks
                        .insert(turn.turn_id.clone())
                    {
                        recovered = recovered.saturating_add(1);
                        self.append_agent_trace_turn_event(
                            &turn.pane_id,
                            &turn.turn_id,
                            &format!(
                                "provider_task queued reason=pending_shell_dispatch_recovery readiness={}",
                                runtime_pane_readiness_state_name(readiness)
                            ),
                        )?;
                    }
                }
                PaneReadinessState::Probing => {
                    if !self.turn_has_running_readiness_probe(&turn.turn_id) {
                        self.pane_readiness_overrides
                            .clear_pending_probe(&turn.pane_id);
                        self.set_pane_readiness(&turn.pane_id, PaneReadinessState::Degraded);
                        if self
                            .pending_agent_provider_tasks
                            .insert(turn.turn_id.clone())
                        {
                            recovered = recovered.saturating_add(1);
                            self.append_agent_status_text_to_terminal_buffer(
                                &turn.pane_id,
                                "agent: shell readiness probe was lost; retrying pending shell command",
                            )?;
                            self.append_agent_trace_turn_event(
                                &turn.pane_id,
                                &turn.turn_id,
                                "provider_task queued reason=lost_readiness_probe_recovery",
                            )?;
                        }
                    }
                }
                PaneReadinessState::Busy => {
                    let recovery = match self.pane_foreground_primary_shell_state(&turn.pane_id) {
                        Some(true) => Some((
                            PaneReadinessState::PromptCandidate,
                            "agent: shell readiness looked stale; retrying pending shell command",
                            "provider_task queued reason=stale_busy_recovery",
                        )),
                        Some(false) => None,
                        None => Some((
                            PaneReadinessState::Degraded,
                            "agent: shell readiness metadata was unavailable; retrying pending shell command",
                            "provider_task queued reason=unknown_busy_recovery",
                        )),
                    };
                    if let Some((next_readiness, status, trace)) = recovery {
                        self.set_pane_readiness(&turn.pane_id, next_readiness);
                        if self
                            .pending_agent_provider_tasks
                            .insert(turn.turn_id.clone())
                        {
                            recovered = recovered.saturating_add(1);
                            self.append_agent_status_text_to_terminal_buffer(
                                &turn.pane_id,
                                status,
                            )?;
                            self.append_agent_trace_turn_event(
                                &turn.pane_id,
                                &turn.turn_id,
                                trace,
                            )?;
                        }
                    }
                }
                PaneReadinessState::FullScreen
                | PaneReadinessState::PasswordPrompt
                | PaneReadinessState::InteractiveBlocked => {}
            }
        }
        Ok(recovered)
    }

    /// Runs the stranded agent shell dispatch recovery candidates operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn stranded_agent_shell_dispatch_recovery_candidates(&self) -> Vec<String> {
        self.agent_turn_executions
            .iter()
            .filter(|(turn_id, execution)| {
                (self.execution_has_pending_shell_dispatch(turn_id, execution)
                    || runtime_execution_ready_for_provider_continuation(execution))
                    && !self.pending_agent_provider_tasks.contains(*turn_id)
                    && !self.claimed_agent_provider_tasks.contains_key(*turn_id)
                    && !self
                        .running_shell_transactions
                        .values()
                        .any(|transaction| transaction.turn_id == turn_id.as_str())
            })
            .map(|(turn_id, _)| turn_id.clone())
            .collect()
    }

    /// Fails running turns that have no service-owned or actor-owned progress.
    ///
    /// # Parameters
    /// - `actor_progress_turn_ids`: Running turns with progress represented by
    ///   actor-owned scheduler state.
    fn fail_unreachable_running_agent_turns_with_actor_progress(
        &mut self,
        actor_progress_turn_ids: &BTreeSet<String>,
    ) -> Result<usize> {
        let candidates = self.unreachable_running_agent_turn_candidates(actor_progress_turn_ids);
        let mut failed = 0usize;
        for turn_id in candidates {
            let Some(turn) = self
                .agent_turn_ledger
                .turns()
                .iter()
                .find(|turn| turn.turn_id == turn_id && turn.state == AgentTurnState::Running)
                .cloned()
            else {
                continue;
            };
            self.append_agent_status_text_to_terminal_buffer(
                &turn.pane_id,
                "agent: runtime found no remaining progress path; failing turn",
            )?;
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                "provider_task failed reason=no_runtime_progress_path",
            )?;
            let error = MezError::invalid_state(
                "running agent turn has no pending provider, claimed provider, shell, hook, approval, subagent, or continuation work",
            );
            self.fail_configured_agent_provider_task(&turn.turn_id, &error)?;
            failed = failed.saturating_add(1);
        }
        Ok(failed)
    }

    /// Returns running turns that cannot make forward progress without runtime
    /// intervention.
    fn unreachable_running_agent_turn_candidates(
        &self,
        actor_progress_turn_ids: &BTreeSet<String>,
    ) -> Vec<String> {
        self.agent_turn_ledger
            .turns()
            .iter()
            .filter(|turn| turn.state == AgentTurnState::Running)
            .filter(|turn| !self.turn_has_runtime_progress_path(turn, actor_progress_turn_ids))
            .map(|turn| turn.turn_id.clone())
            .collect()
    }

    /// Reports whether a running turn still has a known path to progress.
    fn turn_has_runtime_progress_path(
        &self,
        turn: &AgentTurnRecord,
        actor_progress_turn_ids: &BTreeSet<String>,
    ) -> bool {
        let turn_id = turn.turn_id.as_str();
        self.pending_agent_provider_tasks.contains(turn_id)
            || actor_progress_turn_ids.contains(turn_id)
            || self.claimed_agent_provider_tasks.contains_key(turn_id)
            || self.agent_turn_pending_steering.contains_key(turn_id)
            || self
                .running_shell_transactions
                .values()
                .any(|transaction| transaction.turn_id == turn_id)
            || self.turn_has_pending_focused_shell_hook_continuation(turn_id)
            || self.joined_subagent_dependencies.contains_key(turn_id)
            || self
                .blocked_agent_approval_refs
                .values()
                .any(|approval_ref| approval_ref.turn_id == turn_id)
            || self
                .agent_turn_executions
                .get(turn_id)
                .is_some_and(|execution| {
                    runtime_execution_ready_for_provider_continuation(execution)
                        || self.execution_has_pending_shell_dispatch(turn_id, execution)
                        || execution.action_results.iter().any(|result| {
                            result.status == ActionStatus::Running
                                && matches!(result.action_type, "spawn_agent")
                        })
                })
    }

    /// Reports whether a focused-shell hook can still resume one of this turn's
    /// shell actions.
    fn turn_has_pending_focused_shell_hook_continuation(&self, turn_id: &str) -> bool {
        self.focused_shell_hook_transactions
            .values()
            .filter_map(|pending| pending.continuation.as_ref())
            .any(|continuation| continuation.turn_id == turn_id)
    }

    /// Reports whether host process metadata can determine if the pane primary
    /// shell is the foreground process group for its PTY.
    pub(super) fn pane_foreground_primary_shell_state(&self, pane_id: &str) -> Option<bool> {
        let primary_pid = self.pane_processes.primary_pid(pane_id)?;
        let foreground_pid = self.pane_processes.foreground_process_group_id(pane_id)?;
        Some(foreground_pid == primary_pid)
    }

    /// Runs the observe agent shell transaction events operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn observe_agent_shell_transaction_events(
        &mut self,
        output_pane_id: &str,
        events: &[TerminalOscEvent],
    ) -> Result<usize> {
        let mut observed = 0usize;
        let mut observed_harness_transaction_end = false;
        for event in events {
            match event {
                TerminalOscEvent::TitleChanged { .. } | TerminalOscEvent::ClipboardSet { .. } => {}
                TerminalOscEvent::ShellPromptStart => {
                    if !observed_harness_transaction_end {
                        observed =
                            observed.saturating_add(self.observe_passive_shell_prompt_candidate(
                                output_pane_id,
                                "osc133-prompt-start",
                            )?);
                    }
                }
                TerminalOscEvent::ShellPromptEnd => {
                    if !observed_harness_transaction_end {
                        observed =
                            observed.saturating_add(self.observe_passive_shell_prompt_candidate(
                                output_pane_id,
                                "osc133-prompt-end",
                            )?);
                    }
                }
                TerminalOscEvent::ShellCommandFinished { .. } => {
                    if !observed_harness_transaction_end {
                        observed =
                            observed.saturating_add(self.observe_passive_shell_prompt_candidate(
                                output_pane_id,
                                "osc133-command-finished",
                            )?);
                    }
                }
                TerminalOscEvent::ShellCommandOutputStart => {
                    if !observed_harness_transaction_end {
                        observed =
                            observed.saturating_add(self.observe_passive_shell_busy(
                                output_pane_id,
                                "osc133-command-start",
                            )?);
                    }
                }
                TerminalOscEvent::ShellTransactionStart {
                    marker,
                    turn_id,
                    agent_id,
                    pane_id,
                } => {
                    observed =
                        observed.saturating_add(self.observe_agent_shell_transaction_start(
                            output_pane_id,
                            marker,
                            turn_id,
                            agent_id,
                            pane_id,
                        )?);
                }
                TerminalOscEvent::ShellTransactionEnd {
                    marker,
                    turn_id,
                    agent_id,
                    pane_id,
                    exit_code,
                } => {
                    let agent_observed = self.observe_agent_shell_transaction_end(
                        output_pane_id,
                        marker,
                        turn_id,
                        agent_id,
                        pane_id,
                        *exit_code,
                    )?;
                    if agent_observed == 0 {
                        observed = observed.saturating_add(
                            self.observe_focused_shell_hook_transaction_end(
                                output_pane_id,
                                marker,
                                pane_id,
                                *exit_code,
                            )?,
                        );
                    } else {
                        observed = observed.saturating_add(agent_observed);
                        observed_harness_transaction_end = true;
                    }
                }
            }
        }
        Ok(observed)
    }

    /// Runs the pane agent turn waiting for provider or shell dispatch operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn pane_agent_turn_waiting_for_provider_or_shell_dispatch(
        &self,
        pane_id: &str,
    ) -> Option<String> {
        let turn_id = self
            .agent_shell_store
            .get(pane_id)
            .and_then(|session| session.running_turn_id.as_deref())?;
        let turn_is_running = self
            .agent_turn_ledger
            .turns()
            .iter()
            .any(|turn| turn.turn_id == turn_id && turn.state == AgentTurnState::Running);
        if !turn_is_running {
            return None;
        }
        if self.pending_agent_provider_tasks.contains(turn_id) {
            return Some(turn_id.to_string());
        }
        if self.claimed_agent_provider_tasks.contains_key(turn_id) {
            return None;
        }
        let execution = self.agent_turn_executions.get(turn_id)?;
        if runtime_execution_ready_for_provider_continuation(execution)
            || self.execution_has_pending_shell_dispatch(turn_id, execution)
        {
            Some(turn_id.to_string())
        } else {
            None
        }
    }

    /// Runs the queue waiting agent turn for passive readiness operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn queue_waiting_agent_turn_for_passive_readiness(
        &mut self,
        pane_id: &str,
        reason: &str,
    ) -> Result<usize> {
        let Some(turn_id) = self.pane_agent_turn_waiting_for_provider_or_shell_dispatch(pane_id)
        else {
            return Ok(0);
        };
        if !self.pending_agent_provider_tasks.insert(turn_id.clone()) {
            return Ok(0);
        }
        self.append_agent_trace_turn_event(
            pane_id,
            &turn_id,
            &format!("provider_task queued reason={reason}"),
        )?;
        Ok(1)
    }

    /// Runs the apply terminal osc events operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn apply_terminal_osc_events(
        &mut self,
        events: &[TerminalOscEvent],
    ) -> Result<usize> {
        let mut applied = 0usize;
        for event in events {
            match event {
                TerminalOscEvent::ClipboardSet { selection, content }
                    if terminal_clipboard_policy_accepts_osc52(&self.terminal_clipboard) =>
                {
                    self.copy_text_to_buffer_and_host_clipboard(
                        "osc52",
                        content.clone(),
                        format!("terminal-osc52:{selection}"),
                    )?;
                    applied = applied.saturating_add(1);
                }
                TerminalOscEvent::TitleChanged { .. }
                | TerminalOscEvent::ClipboardSet { .. }
                | TerminalOscEvent::ShellPromptStart
                | TerminalOscEvent::ShellPromptEnd
                | TerminalOscEvent::ShellCommandOutputStart
                | TerminalOscEvent::ShellCommandFinished { .. }
                | TerminalOscEvent::ShellTransactionStart { .. }
                | TerminalOscEvent::ShellTransactionEnd { .. } => {}
            }
        }
        Ok(applied)
    }

    /// Runs the observe passive shell prompt candidate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn observe_passive_shell_prompt_candidate(
        &mut self,
        pane_id: &str,
        source: &str,
    ) -> Result<usize> {
        let previous = self.pane_readiness_state(pane_id);
        if !matches!(
            previous,
            PaneReadinessState::Unknown | PaneReadinessState::Busy
        ) {
            return Ok(0);
        }
        self.set_pane_readiness(pane_id, PaneReadinessState::PromptCandidate);
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","readiness_event":"prompt_candidate","source":"{}","previous_state":"{}","state":"prompt-candidate"}}"#,
                json_escape(pane_id),
                json_escape(source),
                runtime_pane_readiness_state_name(previous)
            ),
        )?;
        let queued =
            self.queue_waiting_agent_turn_for_passive_readiness(pane_id, "prompt_candidate")?;
        Ok(1usize.saturating_add(queued))
    }

    /// Runs the observe passive shell busy operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn observe_passive_shell_busy(
        &mut self,
        pane_id: &str,
        source: &str,
    ) -> Result<usize> {
        let previous = self.pane_readiness_state(pane_id);
        if source == "osc133-command-start"
            && let Some(turn_id) =
                self.pane_agent_turn_waiting_for_provider_or_shell_dispatch(pane_id)
        {
            self.append_agent_trace_turn_event(
                pane_id,
                &turn_id,
                "passive command-start ignored reason=agent_turn_waiting",
            )?;
            return Ok(0);
        }
        if matches!(
            previous,
            PaneReadinessState::Probing
                | PaneReadinessState::FullScreen
                | PaneReadinessState::PasswordPrompt
                | PaneReadinessState::InteractiveBlocked
        ) {
            return Ok(0);
        }
        let revoked = self
            .pane_readiness_overrides
            .revoke(pane_id, ReadinessOverrideRevocation::CommandStartMetadata)
            .is_some();
        if previous == PaneReadinessState::Busy && !revoked {
            return Ok(0);
        }
        self.set_pane_readiness(pane_id, PaneReadinessState::Busy);
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","readiness_event":"busy","source":"{}","previous_state":"{}","state":"busy","override_revoked":{}}}"#,
                json_escape(pane_id),
                json_escape(source),
                runtime_pane_readiness_state_name(previous),
                revoked
            ),
        )?;
        Ok(1)
    }

    /// Sends any deferred transaction payload after the shell wrapper receiver
    /// has started.
    pub(super) fn observe_agent_shell_transaction_start(
        &mut self,
        output_pane_id: &str,
        marker: &str,
        turn_id: &str,
        _agent_id: &str,
        pane_id: &str,
    ) -> Result<usize> {
        let Some(transaction) = self.running_shell_transactions.get(marker) else {
            return Ok(0);
        };
        if transaction.turn_id != turn_id
            || transaction.pane_id != pane_id
            || output_pane_id != pane_id
        {
            return Err(MezError::invalid_state(
                "shell transaction start marker metadata does not match runtime dispatch state",
            ));
        }
        let kind_name = runtime_running_shell_transaction_kind_name(&transaction.kind).to_string();
        let payload = self
            .running_shell_transactions
            .get_mut(marker)
            .and_then(|transaction| transaction.pending_input_payload.take());
        let Some(payload) = payload else {
            return Ok(1);
        };
        let payload_len = payload.len();
        if let Err(error) = self.write_runtime_pane_input_priority(pane_id, &payload) {
            self.fail_shell_transactions_for_pane_write_failure(pane_id, error.message())?;
            return Ok(1);
        }
        if let Some(transaction) = self.running_shell_transactions.get_mut(marker) {
            transaction.started_at_unix_ms = current_unix_millis();
        }
        self.append_agent_trace_turn_event(
            pane_id,
            turn_id,
            &format!(
                "shell_transaction payload_sent marker={} kind={} bytes={}",
                marker, kind_name, payload_len
            ),
        )?;
        Ok(1)
    }

    /// Runs the observe agent shell transaction end operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn observe_agent_shell_transaction_end(
        &mut self,
        output_pane_id: &str,
        marker: &str,
        turn_id: &str,
        agent_id: &str,
        pane_id: &str,
        exit_code: i32,
    ) -> Result<usize> {
        let Some(transaction_ref) = self.running_shell_transactions.get(marker).cloned() else {
            return Ok(0);
        };
        self.append_agent_trace_turn_event(
            pane_id,
            turn_id,
            &format!(
                "shell_transaction observed marker={} kind={} exit_code={}",
                marker,
                runtime_running_shell_transaction_kind_name(&transaction_ref.kind),
                exit_code
            ),
        )?;
        if transaction_ref.turn_id != turn_id
            || transaction_ref.pane_id != pane_id
            || output_pane_id != pane_id
        {
            return Err(MezError::invalid_state(
                "shell transaction marker metadata does not match runtime dispatch state",
            ));
        }
        let Some(mut transaction_ref) = self.running_shell_transactions.remove(marker) else {
            return Ok(0);
        };
        if transaction_ref.kind == RunningShellTransactionKind::ReadinessProbe {
            return self.observe_readiness_probe_transaction_end(
                marker, turn_id, agent_id, pane_id, exit_code,
            );
        }
        if transaction_ref.kind == RunningShellTransactionKind::Bootstrap {
            return self.observe_bootstrap_transaction_end(
                marker,
                pane_id,
                exit_code,
                &transaction_ref.observed_output_preview,
                transaction_ref.observed_output_truncated,
            );
        }
        let RunningShellTransactionKind::AgentAction { ref action_id } = transaction_ref.kind
        else {
            return Err(MezError::invalid_state(
                "shell transaction kind was not handled",
            ));
        };
        let turn = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "turn not found"))?;
        if turn.agent_id != agent_id || turn.pane_id != pane_id {
            return Err(MezError::invalid_state(
                "shell transaction marker identity does not match agent turn",
            ));
        }
        if self.dispatch_apply_patch_followup_if_needed(
            &turn,
            action_id,
            &transaction_ref,
            exit_code,
        )? {
            return Ok(1);
        }

        let (
            mut terminal_state,
            observed_contexts,
            ready_for_provider_continuation,
            post_shell_hook_payload,
            action_transition_trace,
            observed_result,
            observed_results,
            observed_action,
            display_output_after_completion,
        ) = {
            let execution = self
                .agent_turn_executions
                .get_mut(turn_id)
                .ok_or_else(|| MezError::invalid_state("running agent execution is unavailable"))?;
            let batch = execution.response.action_batch.as_ref().ok_or_else(|| {
                MezError::invalid_state("running agent execution has no action batch")
            })?;
            let action = batch
                .actions
                .iter()
                .find(|action| action.id == action_id.as_str())
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_state("shell transaction does not match an agent action")
                })?;
            let mut shell_backed_actions = Vec::new();
            for candidate in &batch.actions {
                if local_action_plan(candidate)?.is_some() {
                    shell_backed_actions.push(candidate.clone());
                }
            }
            let result_index = execution
                .action_results
                .iter()
                .position(|result| result.action_id == action_id.as_str())
                .ok_or_else(|| {
                    MezError::invalid_state("shell transaction does not match an action result")
                })?;
            if execution.action_results[result_index].status != ActionStatus::Running {
                return Ok(0);
            }
            let Some(local_plan) = local_action_plan(&action)? else {
                return Err(MezError::invalid_state(
                    "shell transaction does not match shell-backed action payload",
                ));
            };
            transaction_ref.observed_output_preview =
                decode_shell_output_transport(&transaction_ref.observed_output_preview);
            transaction_ref.observed_output_bytes = transaction_ref.observed_output_preview.len();
            if exit_code == 0 {
                transaction_ref.observed_output_preview = postprocess_shell_action_success_output(
                    &action,
                    transaction_ref.observed_output_preview.clone(),
                )?;
                transaction_ref.observed_output_bytes =
                    transaction_ref.observed_output_preview.len();
            }
            let signal: Option<i32> = if exit_code > 128 && exit_code < 256 {
                Some(exit_code - 128)
            } else {
                None
            };
            let structured_content = shell_command_structured_content_json(
                &action,
                true,
                serde_json::Value::Null,
                &[],
                serde_json::json!({
                    "source": "pty",
                    "stream": "pty_combined",
                    "marker": marker,
                    "exit_code": exit_code,
                    "signal": signal,
                    "timed_out": false,
                    "combined_output_bytes": transaction_ref.observed_output_bytes,
                    "combined_output_preview": transaction_ref.observed_output_preview,
                    "boundary_state": "end-marker-observed",
                    "output_truncated": transaction_ref.observed_output_truncated
                }),
            )?;
            let plain_shell_command =
                matches!(action.payload, AgentActionPayload::ShellCommand { .. });
            execution.action_results[result_index] = if exit_code == 0 || plain_shell_command {
                let success_content = if plain_shell_command && exit_code != 0 {
                    shell_command_result_content(
                        &transaction_ref.observed_output_preview,
                        Some(exit_code),
                        false,
                        false,
                    )
                } else if local_plan.display_output_after_completion
                    && !transaction_ref.observed_output_preview.trim().is_empty()
                {
                    vec![transaction_ref.observed_output_preview.clone()]
                } else {
                    vec!["shell command exited with status 0".to_string()]
                };
                ActionResult::succeeded(&turn, &action, success_content, Some(structured_content))
            } else {
                let mut result = ActionResult::failed(
                    &turn,
                    &action,
                    ActionStatus::Failed,
                    "shell_command_failed",
                    format!("shell command exited with status {exit_code}"),
                )?;
                if !transaction_ref.observed_output_preview.trim().is_empty() {
                    result.content = vec![ActionContentBlock::text(
                        transaction_ref.observed_output_preview.clone(),
                    )];
                }
                result.structured_content_json = Some(structured_content);
                result
            };
            let shell_command_nonzero_result = exit_code != 0 && plain_shell_command;
            execution.terminal_state = if shell_command_nonzero_result {
                AgentTurnState::Running
            } else {
                runtime_agent_turn_state_from_action_results(
                    &execution.action_results,
                    execution.final_turn,
                )
            };
            let mut observed_results = vec![execution.action_results[result_index].clone()];
            if shell_command_nonzero_result {
                let skipped_content = vec![format!(
                    "shell command not run because `{action_id}` exited with status {exit_code}"
                )];
                for result in &mut execution.action_results {
                    if result.status != ActionStatus::Running
                        || result.action_id == action_id.as_str()
                    {
                        continue;
                    }
                    let Some(skipped_action) = shell_backed_actions
                        .iter()
                        .find(|candidate| candidate.id == result.action_id)
                    else {
                        continue;
                    };
                    let structured_content = shell_command_structured_content_json(
                        skipped_action,
                        false,
                        serde_json::Value::Null,
                        &[],
                        serde_json::json!({
                            "source": "runtime",
                            "stream": "pty_input",
                            "marker": marker,
                            "exit_code": null,
                            "signal": null,
                            "timed_out": false,
                            "combined_output_bytes": 0,
                            "combined_output_preview": "",
                            "boundary_state": "skipped-after-nonzero-shell-exit",
                            "output_truncated": false,
                            "skipped": true,
                            "previous_action_id": action_id,
                            "previous_exit_code": exit_code
                        }),
                    )?;
                    *result = ActionResult::succeeded(
                        &turn,
                        skipped_action,
                        skipped_content.clone(),
                        Some(structured_content),
                    );
                    observed_results.push(result.clone());
                }
            }
            let action_transition_trace = format!(
                "action {} {} reason=shell_transaction_exit terminal_state={}",
                action_id,
                if execution.action_results[result_index].status == ActionStatus::Succeeded {
                    "succeeded"
                } else {
                    "failed"
                },
                runtime_agent_turn_state_name(execution.terminal_state)
            );
            let observed_result = execution.action_results[result_index].clone();
            let observed_contexts = observed_results
                .iter()
                .map(|result| ContextBlock {
                    source: ContextSourceKind::ActionResult,
                    label: format!("action result {}", result.action_id),
                    content: action_result_context_content(result),
                })
                .collect::<Vec<_>>();
            let post_shell_hook_payload =
                runtime_post_shell_hook_payload(&turn, &action, &observed_result, exit_code);
            let ready_for_provider_continuation = shell_command_nonzero_result
                || runtime_execution_ready_for_provider_continuation(execution);
            (
                execution.terminal_state,
                observed_contexts,
                ready_for_provider_continuation,
                post_shell_hook_payload,
                action_transition_trace,
                observed_result,
                observed_results,
                action,
                local_plan.display_output_after_completion,
            )
        };
        if exit_code == 0 {
            self.record_shell_dispatch_success(turn_id, &transaction_ref.command);
        }
        if exit_code == 0
            && matches!(
                observed_action.payload,
                AgentActionPayload::ApplyPatch { .. }
            )
            && apply_patch_transaction_phase(&transaction_ref.command)
                == Some(ApplyPatchTransactionPhase::Write)
        {
            self.record_agent_modified_files_from_diff(
                pane_id,
                &transaction_ref.observed_output_preview,
            );
        }
        self.append_agent_trace_turn_event(pane_id, turn_id, &action_transition_trace)?;
        self.append_agent_trace_maap_action_results(
            pane_id,
            turn_id,
            "shell_transaction_action_result",
            &observed_results,
        )?;
        if let Some(execution) = self.agent_turn_executions.get(turn_id).cloned() {
            self.record_runtime_agent_patch_results_for_turn(&turn, &execution);
        }
        if exit_code == 0
            && display_output_after_completion
            && (self.agent_debug_enabled(pane_id)
                || self.agent_action_result_renders_in_normal_mode(&observed_action))
            && !self.agent_shell_view_enabled(pane_id)
            && !transaction_ref.observed_output_preview.trim().is_empty()
        {
            self.append_agent_action_result_text_to_terminal_buffer(
                pane_id,
                &observed_action,
                &observed_result,
                &transaction_ref.observed_output_preview,
            )?;
        }

        self.run_configured_completed_hooks(HookEvent::PostShellCommand, &post_shell_hook_payload)?;

        let mut transcript_entries = 0usize;
        if matches!(
            terminal_state,
            AgentTurnState::Completed | AgentTurnState::Failed | AgentTurnState::Interrupted
        ) {
            self.set_pane_readiness(pane_id, PaneReadinessState::Ready);
            let mut execution = self
                .agent_turn_executions
                .get(turn_id)
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_state("observed agent execution was not stored")
                })?;
            let failure_feedback_queued = if terminal_state == AgentTurnState::Failed {
                self.append_runtime_agent_execution_failure_audit(&turn, &execution)?;
                self.queue_agent_failure_feedback_for_correction(
                    &turn,
                    &mut execution,
                    "shell_transaction_failed_action",
                )?
            } else {
                false
            };
            if failure_feedback_queued {
                self.agent_turn_executions.remove(turn_id);
                terminal_state = AgentTurnState::Running;
            } else {
                self.present_deferred_agent_say_actions_to_terminal_buffer(pane_id, &execution)?;
                transcript_entries =
                    self.persist_runtime_agent_turn_execution_transcript(&turn, &execution)?;
                self.emit_subagent_task_result_for_execution(&turn, &execution)?;
                let _ = self.agent_scheduler.complete(turn_id);
                self.append_agent_trace_turn_event(
                    pane_id,
                    turn_id,
                    &format!(
                        "scheduler running -> {} reason=shell_transaction_settled",
                        runtime_agent_turn_state_name(terminal_state)
                    ),
                )?;
                self.finish_agent_turn(pane_id, turn_id, terminal_state)?;
            }
        } else if terminal_state == AgentTurnState::Running {
            self.agent_turn_contexts
                .get_mut(turn_id)
                .ok_or_else(|| {
                    MezError::invalid_state("running agent turn context is unavailable")
                })?
                .blocks
                .extend(observed_contexts);
            self.set_pane_readiness(pane_id, PaneReadinessState::Ready);
            if ready_for_provider_continuation {
                self.pending_agent_provider_tasks
                    .insert(turn_id.to_string());
                self.append_agent_trace_turn_event(
                    pane_id,
                    turn_id,
                    "provider_task queued reason=shell_transaction_result_ready",
                )?;
            } else {
                let should_dispatch_stored_shell = self
                    .agent_turn_executions
                    .get(turn_id)
                    .is_some_and(|execution| {
                        self.execution_has_pending_shell_dispatch(turn_id, execution)
                    });
                if should_dispatch_stored_shell {
                    self.append_agent_trace_turn_event(
                        pane_id,
                        turn_id,
                        "pending_shell_dispatch available reason=shell_transaction_result",
                    )?;
                    let _ = self.dispatch_stored_running_shell_actions(turn_id)?;
                }
            }
        } else {
            self.set_pane_readiness(pane_id, PaneReadinessState::Ready);
        }

        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"{}","shell_transaction":"observed","marker":"{}","exit_code":{},"transcript_entries":{}}}"#,
                json_escape(pane_id),
                json_escape(turn_id),
                runtime_agent_turn_state_name(terminal_state),
                json_escape(marker),
                exit_code,
                transcript_entries
            ),
        )?;
        Ok(1)
    }

    /// Runs the dispatch readiness probe to pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_readiness_probe_to_pane(
        &mut self,
        turn: &AgentTurnRecord,
    ) -> Result<()> {
        if self
            .running_shell_transactions
            .values()
            .any(|transaction| transaction.pane_id == turn.pane_id)
        {
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                "readiness_probe dispatch skipped reason=shell_transaction_running",
            )?;
            return Ok(());
        }
        if self.running_shell_transactions.values().any(|transaction| {
            transaction.turn_id == turn.turn_id
                && transaction.kind == RunningShellTransactionKind::ReadinessProbe
        }) {
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                "readiness_probe dispatch skipped reason=already_running",
            )?;
            return Ok(());
        }
        let previous_readiness = self.pane_readiness_state(&turn.pane_id);
        let marker = runtime_marker_for_action(turn, "readiness-probe")?;
        let marker_id = marker.as_str().to_string();
        let classification = self.shell_classification_for_pane(&turn.pane_id);
        let probe_command = readiness_probe_command_for_classification(classification);
        let transaction = ShellTransaction::new(
            marker,
            &turn.turn_id,
            &turn.agent_id,
            &turn.pane_id,
            self.session.shell.path(),
            probe_command,
        )?;
        let transaction_input = transaction.render_for_classification_input(classification);
        let mut wrapper = transaction_input.wrapper;
        if !wrapper.ends_with('\n') {
            wrapper.push('\n');
        }
        self.remember_mez_wrapper_filter_command(&turn.pane_id, probe_command);
        self.write_runtime_pane_input(&turn.pane_id, wrapper.as_bytes())?;
        self.pane_readiness_overrides
            .record_pending_probe(&turn.pane_id)?;
        self.set_pane_readiness(&turn.pane_id, PaneReadinessState::Probing);
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "pane_readiness {} -> probing reason=readiness_probe_sent marker={}",
                runtime_pane_readiness_state_name(previous_readiness),
                marker_id
            ),
        )?;
        self.running_shell_transactions.insert(
            marker_id.clone(),
            RunningShellTransactionRef {
                turn_id: turn.turn_id.clone(),
                kind: RunningShellTransactionKind::ReadinessProbe,
                pane_id: turn.pane_id.clone(),
                command: probe_command.to_string(),
                started_at_unix_ms: current_unix_millis(),
                timeout_ms: Some(RUNTIME_READINESS_PROBE_TIMEOUT_MS),
                pending_input_payload: (!transaction_input.payload.is_empty())
                    .then(|| transaction_input.payload.into_bytes()),
                observed_output_bytes: 0,
                observed_output_preview: String::new(),
                observed_output_truncated: false,
            },
        );
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"probing","readiness_probe":"sent","marker":"{}"}}"#,
                json_escape(&turn.pane_id),
                json_escape(&turn.turn_id),
                json_escape(&marker_id)
            ),
        )?;
        Ok(())
    }

    /// Runs the observe readiness probe transaction end operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn observe_readiness_probe_transaction_end(
        &mut self,
        marker: &str,
        turn_id: &str,
        agent_id: &str,
        pane_id: &str,
        exit_code: i32,
    ) -> Result<usize> {
        let turn = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "turn not found"))?;
        if turn.agent_id != agent_id || turn.pane_id != pane_id {
            return Err(MezError::invalid_state(
                "readiness probe marker identity does not match agent turn",
            ));
        }
        self.pane_readiness_overrides.clear_pending_probe(pane_id);
        if exit_code == 0 {
            let previous_readiness = self.pane_readiness_state(pane_id);
            self.set_pane_readiness(pane_id, PaneReadinessState::Ready);
            self.append_agent_trace_turn_event(
                pane_id,
                turn_id,
                &format!(
                    "pane_readiness {} -> ready reason=readiness_probe_completed marker={}",
                    runtime_pane_readiness_state_name(previous_readiness),
                    marker
                ),
            )?;
            self.append_lifecycle_event(
                EventKind::AgentStatus,
                format!(
                    r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"ready","readiness_probe":"completed","marker":"{}","exit_code":0}}"#,
                    json_escape(pane_id),
                    json_escape(turn_id),
                    json_escape(marker)
                ),
            )?;
            let should_dispatch_stored_shell =
                self.agent_turn_executions
                    .get(turn_id)
                    .is_some_and(|execution| {
                        self.execution_has_pending_shell_dispatch(turn_id, execution)
                    });
            if should_dispatch_stored_shell {
                self.append_agent_trace_turn_event(
                    pane_id,
                    turn_id,
                    "pending_shell_dispatch available reason=readiness_probe_completed",
                )?;
                let _ = self.dispatch_stored_running_shell_actions(turn_id)?;
            } else if self
                .agent_turn_ledger
                .turns()
                .iter()
                .any(|turn| turn.turn_id == turn_id && turn.state == AgentTurnState::Running)
                && self
                    .agent_turn_executions
                    .get(turn_id)
                    .is_some_and(runtime_execution_ready_for_provider_continuation)
            {
                self.pending_agent_provider_tasks
                    .insert(turn_id.to_string());
                self.append_agent_trace_turn_event(
                    pane_id,
                    turn_id,
                    "provider_task queued reason=readiness_probe_completed",
                )?;
            }
        } else {
            self.pane_readiness_overrides
                .revoke(pane_id, ReadinessOverrideRevocation::ReadinessProbeFailed);
            let previous_readiness = self.pane_readiness_state(pane_id);
            self.set_pane_readiness(pane_id, PaneReadinessState::Degraded);
            self.append_agent_trace_turn_event(
                pane_id,
                turn_id,
                &format!(
                    "pane_readiness {} -> degraded reason=readiness_probe_failed marker={} exit_code={}",
                    runtime_pane_readiness_state_name(previous_readiness),
                    marker,
                    exit_code
                ),
            )?;
            self.append_lifecycle_event(
                EventKind::AgentStatus,
                format!(
                    r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"degraded","readiness_probe":"failed","marker":"{}","exit_code":{}}}"#,
                    json_escape(pane_id),
                    json_escape(turn_id),
                    json_escape(marker),
                    exit_code
                ),
            )?;
        }
        Ok(1)
    }

    /// Runs the dispatch bootstrap to pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_bootstrap_to_pane(&mut self, pane_id: &str) -> Result<()> {
        if self
            .running_shell_transactions
            .values()
            .any(|transaction| transaction.pane_id == pane_id)
        {
            return Ok(());
        }
        let agent_id = format!("agent-{pane_id}");
        let turn_id = format!("bootstrap-{pane_id}-{}", current_unix_seconds());
        let marker = runtime_random_marker_token(&format!("bootstrap\0{pane_id}\0{turn_id}"))?;
        let marker_id = marker.as_str().to_string();
        let classification = self.shell_classification_for_pane(pane_id);
        let bootstrap_script = bootstrap_script_for_classification(classification);
        let transaction = ShellTransaction::new(
            marker,
            &turn_id,
            &agent_id,
            pane_id,
            self.session.shell.path(),
            bootstrap_script.clone(),
        )?;
        let transaction_input = transaction.render_for_classification_input(classification);
        let mut wrapper = transaction_input.wrapper;
        if !wrapper.ends_with('\n') {
            wrapper.push('\n');
        }
        self.remember_mez_wrapper_filter_command(pane_id, &bootstrap_script);
        self.write_runtime_pane_input(pane_id, wrapper.as_bytes())?;
        self.set_pane_readiness(pane_id, PaneReadinessState::Busy);
        self.running_shell_transactions.insert(
            marker_id.clone(),
            RunningShellTransactionRef {
                turn_id: turn_id.clone(),
                kind: RunningShellTransactionKind::Bootstrap,
                pane_id: pane_id.to_string(),
                command: bootstrap_script,
                started_at_unix_ms: current_unix_millis(),
                timeout_ms: Some(DEFAULT_BOOTSTRAP_TIMEOUT_MS),
                pending_input_payload: (!transaction_input.payload.is_empty())
                    .then(|| transaction_input.payload.into_bytes()),
                observed_output_bytes: 0,
                observed_output_preview: String::new(),
                observed_output_truncated: false,
            },
        );
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","bootstrap":"sent","marker":"{}"}}"#,
                json_escape(pane_id),
                json_escape(&marker_id)
            ),
        )?;
        Ok(())
    }

    /// Runs the observe bootstrap transaction end operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn observe_bootstrap_transaction_end(
        &mut self,
        marker: &str,
        pane_id: &str,
        exit_code: i32,
        observed_output_preview: &str,
        observed_output_truncated: bool,
    ) -> Result<usize> {
        self.pane_bootstrap_pending.remove(pane_id);
        let mut bootstrap_parsed = false;
        if exit_code == 0 {
            let all_output = if observed_output_preview.trim().is_empty() {
                let screen = self.pane_screens.get(pane_id).ok_or_else(|| {
                    MezError::new(
                        crate::error::MezErrorKind::NotFound,
                        "pane terminal screen not found",
                    )
                })?;
                screen.normal_content_lines().join("\n")
            } else {
                observed_output_preview.to_string()
            };

            let (signature, inventory, instruction_files) =
                parse_bootstrap_env_output(&all_output, self.session.shell.path());

            if let Some(sig) = signature.clone() {
                bootstrap_parsed = true;
                self.pane_environment_signatures
                    .insert(pane_id.to_string(), sig.clone());
                if let Some(inv) = inventory.clone() {
                    self.tool_discovery_cache.record(sig, inv);
                }
                if !instruction_files.is_empty() {
                    self.pane_instruction_files
                        .insert(pane_id.to_string(), instruction_files);
                }
                self.append_lifecycle_event(
                    EventKind::AgentStatus,
                    format!(
                        r#"{{"pane_id":"{}","bootstrap":"completed","marker":"{}","exit_code":0,"output_truncated":{}}}"#,
                        json_escape(pane_id),
                        json_escape(marker),
                        observed_output_truncated
                    ),
                )?;
            } else {
                self.append_lifecycle_event(
                    EventKind::Diagnostic,
                    format!(
                        r#"{{"pane_id":"{}","bootstrap":"unparsed","marker":"{}","exit_code":0,"output_truncated":{},"message":"bootstrap completed but no environment signature was parsed; continuing with degraded context"}}"#,
                        json_escape(pane_id),
                        json_escape(marker),
                        observed_output_truncated
                    ),
                )?;
            }
        } else {
            self.append_lifecycle_event(
                EventKind::AgentStatus,
                format!(
                    r#"{{"pane_id":"{}","bootstrap":"failed","marker":"{}","exit_code":{}}}"#,
                    json_escape(pane_id),
                    json_escape(marker),
                    exit_code
                ),
            )?;
        }
        if bootstrap_parsed {
            self.set_pane_readiness(pane_id, PaneReadinessState::Ready);
        } else if self.pane_readiness_state(pane_id) == PaneReadinessState::Busy {
            self.set_pane_readiness(pane_id, PaneReadinessState::PromptCandidate);
        }
        let pending_shell_turns = self
            .agent_turn_executions
            .iter()
            .filter(|(turn_id, execution)| {
                self.execution_has_pending_shell_dispatch(turn_id, execution)
                    && self.agent_turn_ledger.turns().iter().any(|turn| {
                        turn.turn_id == **turn_id
                            && turn.pane_id == pane_id
                            && turn.state == AgentTurnState::Running
                    })
            })
            .map(|(turn_id, _)| turn_id.clone())
            .collect::<Vec<_>>();
        for turn_id in pending_shell_turns {
            let _ = self.dispatch_stored_running_shell_actions(&turn_id)?;
        }
        let _ = self.recover_stranded_agent_shell_dispatches()?;
        Ok(1)
    }

    /// Dispatches hidden bootstrap wrappers for pending panes that have reached
    /// prompt-like readiness.
    pub(crate) fn maybe_bootstrap_ready_panes(&mut self) -> Result<usize> {
        let ready_panes: Vec<String> = self
            .pane_readiness_states
            .iter()
            .filter(|(k, v)| {
                self.pane_bootstrap_pending.contains(k.as_str())
                    && !self
                        .running_shell_transactions
                        .values()
                        .any(|transaction| transaction.pane_id == k.as_str())
                    && matches!(
                        v,
                        PaneReadinessState::Ready | PaneReadinessState::PromptCandidate
                    )
            })
            .map(|(k, _)| k.clone())
            .collect();
        let dispatches = ready_panes.len();
        for pane_id in ready_panes {
            self.dispatch_bootstrap_to_pane(&pane_id)?;
        }
        Ok(dispatches)
    }

    /// Runs the observe focused shell hook transaction end operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn observe_focused_shell_hook_transaction_end(
        &mut self,
        output_pane_id: &str,
        marker: &str,
        pane_id: &str,
        exit_code: i32,
    ) -> Result<usize> {
        let Some(pending) = self.focused_shell_hook_transactions.remove(marker) else {
            return Ok(0);
        };
        if pending.pane_id != pane_id || output_pane_id != pane_id {
            return Err(MezError::invalid_state(
                "focused-shell hook marker metadata does not match runtime dispatch state",
            ));
        }
        let success = exit_code == 0;
        let result = HookExecutionResult {
            hook_id: pending.plan.hook_id.clone(),
            event: pending.plan.event,
            status: if success {
                HookExecutionStatus::Succeeded
            } else {
                HookExecutionStatus::Failed
            },
            exit_code: Some(exit_code),
            stdout: format!("focused-shell hook exited with status {exit_code}"),
            stderr: String::new(),
            failure: if success {
                None
            } else {
                Some(HookFailure {
                    hook_id: pending.plan.hook_id.clone(),
                    event: pending.plan.event,
                    kind: HookFailureKind::ExitNonZero,
                    message: "focused-shell hook exited with non-zero status".to_string(),
                    retryable: false,
                })
            },
        };
        if !success {
            self.append_lifecycle_event(
                EventKind::HookFailed,
                format!(
                    r#"{{"hook_id":"{}","event":"{}","pane_id":"{}","exit_code":{},"marker":"{}"}}"#,
                    json_escape(&pending.plan.hook_id),
                    runtime_hook_event_name(pending.plan.event),
                    json_escape(pane_id),
                    exit_code,
                    json_escape(marker)
                ),
            )?;
        }
        if let Some(audit_log) = self.audit_log.as_mut() {
            let record = hook_execution_audit_record(
                &pending.plan,
                self.session.id.as_str(),
                AuditActor {
                    kind: "runtime".to_string(),
                    id: "focused-shell-hook-observer".to_string(),
                },
                "runtime_focused_shell_completion",
                &result,
            )
            .with_pane_id(pane_id.to_string());
            let _ = audit_log.append(record)?;
        }
        if let Some(continuation) = pending.continuation.as_ref() {
            let decision = self.record_hook_result(&pending.plan, &result, false)?;
            if decision == crate::hooks::HookFailureDecision::Block {
                let block = RuntimeHookPipelineBlock::from_result(&result);
                let _ = self.fail_pending_shell_action_for_hook_block(continuation, &block)?;
            } else {
                self.record_agent_pre_shell_hook_completed(continuation, &pending.plan.hook_id);
                let continuation_pane_id = self
                    .agent_turn_ledger
                    .turns()
                    .iter()
                    .find(|turn| turn.turn_id == continuation.turn_id)
                    .map(|turn| turn.pane_id.clone())
                    .unwrap_or_else(|| pane_id.to_string());
                self.append_agent_trace_turn_event(
                    &continuation_pane_id,
                    &continuation.turn_id,
                    &format!(
                        "action {} pre_shell_hook {} completed status={}",
                        continuation.action_id,
                        pending.plan.hook_id,
                        runtime_hook_execution_status_name(result.status)
                    ),
                )?;
                let _ = self.dispatch_stored_running_shell_actions(&continuation.turn_id)?;
            }
        }
        self.push_focused_shell_hook_result(result);
        Ok(1)
    }

    /// Runs the write active pane pipe operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn write_active_pane_pipe(&mut self, pane_id: &str, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let Some(pipe) = self.active_pane_pipes.get_mut(pane_id) else {
            return Ok(());
        };
        if self.defer_file_pane_pipe_writes
            && let Some(path) = pipe.file_target_path()
        {
            pipe.record_deferred_output(bytes.len());
            self.deferred_pane_pipe_writes.push(DeferredPanePipeWrite {
                pane_id: pane_id.to_string(),
                path,
                bytes: bytes.to_vec(),
            });
            return Ok(());
        }
        let Err(error) = pipe.write_output(bytes) else {
            return Ok(());
        };
        let failure = error.message().to_string();
        let stopped = self.stop_active_pane_pipe(pane_id)?;
        self.append_lifecycle_event(
            EventKind::PaneChanged,
            format!(
                r#"{{"pane_id":"{}","pipe":"stopped","mode":"{}","target":"{}","reason":"write-failed","bytes_written":{},"failure":"{}"}}"#,
                json_escape(&stopped.pane_id),
                stopped.mode,
                json_escape(&stopped.target),
                stopped.bytes_written,
                json_escape(&stopped.failure.unwrap_or(failure))
            ),
        )
    }

    /// Runs the start file pane pipe operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn start_file_pane_pipe(
        &mut self,
        pane_id: String,
        path: PathBuf,
    ) -> Result<String> {
        let _ = self.stop_active_pane_pipe(pane_id.as_str());
        let pipe = if self.defer_file_pane_pipe_writes {
            ActivePanePipe::deferred_file(pane_id.clone(), path)
        } else {
            ActivePanePipe::file(pane_id.clone(), path)?
        };
        let body = format!(
            "target={}:pipe=started:mode={}:output={}:active_pipes={}",
            pipe.pane_id,
            pipe.mode(),
            pipe.target_label(),
            self.active_pane_pipes.len().saturating_add(1)
        );
        self.active_pane_pipes.insert(pane_id, pipe);
        Ok(body)
    }

    /// Runs the start command pane pipe operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn start_command_pane_pipe(
        &mut self,
        pane_id: String,
        command: String,
    ) -> Result<String> {
        let _ = self.stop_active_pane_pipe(pane_id.as_str());
        let pipe = if self.defer_command_pane_pipe_startup {
            ActivePanePipe::deferred_command(pane_id.clone(), self.session.shell.path(), command)?
        } else {
            ActivePanePipe::command(pane_id.clone(), self.session.shell.path(), command)?
        };
        let body = format!(
            "target={}:pipe=started:mode={}:command={}:active_pipes={}",
            pipe.pane_id,
            pipe.mode(),
            pipe.target_label(),
            self.active_pane_pipes.len().saturating_add(1)
        );
        self.active_pane_pipes.insert(pane_id, pipe);
        Ok(body)
    }

    /// Runs the stop active pane pipe operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn stop_active_pane_pipe(&mut self, pane_id: &str) -> Result<StoppedPanePipe> {
        let pipe = self.active_pane_pipes.remove(pane_id).ok_or_else(|| {
            MezError::new(crate::error::MezErrorKind::NotFound, "pane pipe not found")
        })?;
        Ok(pipe.stop())
    }

    /// Runs the stop active pane pipes for operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn stop_active_pane_pipes_for(&mut self, pane_ids: &[&str]) -> Vec<StoppedPanePipe> {
        pane_ids
            .iter()
            .filter_map(|pane_id| {
                self.active_pane_pipes
                    .remove(*pane_id)
                    .map(ActivePanePipe::stop)
            })
            .collect()
    }

    /// Runs the stop all active pane pipes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn stop_all_active_pane_pipes(&mut self) -> Vec<StoppedPanePipe> {
        std::mem::take(&mut self.active_pane_pipes)
            .into_values()
            .map(ActivePanePipe::stop)
            .collect()
    }

    /// Returns whether the pane has a command-backed pipe that should be
    /// checked by the actor-owned health timer after accepted output.
    pub(crate) fn command_pane_pipe_health_check_needed(&self, pane_id: &str) -> Result<bool> {
        self.active_pane_pipes
            .get(pane_id)
            .map(|pipe| pipe.command_status())
            .transpose()
            .map(|status| status.flatten().is_some())
    }

    /// Returns pane ids that currently have command-backed pipes.
    pub(crate) fn active_command_pane_pipe_ids(&self) -> Vec<String> {
        self.active_pane_pipes
            .iter()
            .filter_map(|(pane_id, pipe)| match pipe.command_status() {
                Ok(Some(_)) => Some(pane_id.clone()),
                Ok(None) | Err(_) => None,
            })
            .collect()
    }

    /// Stops a command-backed pane pipe when its background command has exited
    /// or failed after accepting output.
    ///
    /// Command-pipe writers run outside actor state. A short actor-owned timer
    /// calls this after pane output is delivered so an asynchronously completed
    /// or failed command is reflected in pane state without waiting for a later
    /// pane-output write or explicit `pipe-pane --stop`.
    pub(crate) fn stop_completed_command_pane_pipe_for(&mut self, pane_id: &str) -> Result<usize> {
        let Some(status) = self
            .active_pane_pipes
            .get(pane_id)
            .map(|pipe| pipe.command_status())
            .transpose()?
            .flatten()
        else {
            return Ok(0);
        };
        if !status.completed && status.failure.is_none() {
            return Ok(0);
        }
        let stopped = self.stop_active_pane_pipe(pane_id)?;
        let reason = if stopped.failure.is_some() {
            "command-failed"
        } else {
            "command-completed"
        };
        let failure_json = stopped
            .failure
            .as_ref()
            .map(|failure| format!(r#","failure":"{}""#, json_escape(failure)))
            .unwrap_or_default();
        self.append_lifecycle_event(
            EventKind::PaneChanged,
            format!(
                r#"{{"pane_id":"{}","pipe":"stopped","mode":"{}","target":"{}","reason":"{}","bytes_written":{}{}}}"#,
                json_escape(&stopped.pane_id),
                stopped.mode,
                json_escape(&stopped.target),
                reason,
                stopped.bytes_written,
                failure_json
            ),
        )?;
        Ok(1)
    }

    /// Stops active file-backed pane pipes that target the provided path.
    ///
    /// Async persistence failures arrive with a persistence path rather than a
    /// pane id. This helper reconciles that failure back into runtime pane-pipe
    /// state and emits one pane lifecycle event per stopped pipe.
    pub(crate) fn stop_file_pane_pipes_for_path(
        &mut self,
        path: &Path,
        reason: &str,
    ) -> Result<usize> {
        let pane_ids = self
            .active_pane_pipes
            .iter()
            .filter_map(|(pane_id, pipe)| {
                pipe.file_target_path()
                    .filter(|target_path| target_path == path)
                    .map(|_| pane_id.clone())
            })
            .collect::<Vec<_>>();
        let mut stopped_pipes = 0usize;
        for pane_id in pane_ids {
            let stopped = self.stop_active_pane_pipe(pane_id.as_str())?;
            self.append_lifecycle_event(
                EventKind::PaneChanged,
                format!(
                    r#"{{"pane_id":"{}","pipe":"stopped","mode":"{}","target":"{}","reason":"{}","bytes_written":{}}}"#,
                    json_escape(&stopped.pane_id),
                    stopped.mode,
                    json_escape(&stopped.target),
                    json_escape(reason),
                    stopped.bytes_written
                ),
            )?;
            stopped_pipes = stopped_pipes.saturating_add(1);
        }
        Ok(stopped_pipes)
    }

    /// Enables or disables deferred file pipe writes for async persistence.
    pub(crate) fn set_defer_file_pane_pipe_writes(&mut self, defer: bool) {
        self.defer_file_pane_pipe_writes = defer;
    }

    /// Enables or disables deferred command pipe startup for async runtimes.
    pub(crate) fn set_defer_command_pane_pipe_startup(&mut self, defer: bool) {
        self.defer_command_pane_pipe_startup = defer;
    }

    /// Drains file-backed pane pipe writes queued for async persistence.
    pub(crate) fn drain_deferred_pane_pipe_writes(&mut self) -> Vec<DeferredPanePipeWrite> {
        std::mem::take(&mut self.deferred_pane_pipe_writes)
    }

    /// Returns the user-facing active pane pipe status line used by
    /// `pipe-pane` and async actor tests that verify pipe lifecycle state.
    pub(crate) fn active_pane_pipe_display(&self) -> String {
        if self.active_pane_pipes.is_empty() {
            return "active_pipes=0".to_string();
        }
        self.active_pane_pipes
            .values()
            .map(|pipe| {
                format!(
                    "pane={}:mode={}:target={}:bytes={}",
                    pipe.pane_id,
                    pipe.mode(),
                    pipe.target_label(),
                    pipe.bytes_written
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Runs the start pane process operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn start_pane_process(
        &mut self,
        descriptor: PaneDescriptor,
        explicit_command: Option<&str>,
    ) -> Result<PaneProcessStart> {
        self.start_pane_process_with_start_directory(descriptor, explicit_command, None)
    }

    /// Runs the start pane process with start directory operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn start_pane_process_with_start_directory(
        &mut self,
        descriptor: PaneDescriptor,
        explicit_command: Option<&str>,
        start_directory: Option<&Path>,
    ) -> Result<PaneProcessStart> {
        let environment = pane_environment_with_term(
            &self.socket_path,
            &self.session.id,
            &descriptor.window_id,
            &descriptor.pane_id,
            &self.terminal_term,
        )?;
        let shell = self.session.shell.clone();
        let primary_pid = self.pane_processes.spawn_for_pane_with_start_directory(
            descriptor.pane_id.as_str(),
            &shell,
            explicit_command,
            &environment,
            descriptor.size,
            start_directory,
        )?;
        self.pane_exit_records.remove(descriptor.pane_id.as_str());
        self.pane_screens.insert(
            descriptor.pane_id.to_string(),
            TerminalScreen::new_with_history_config(
                descriptor.size,
                self.terminal_history_limit,
                self.terminal_history_rotate_lines,
            )?,
        );
        self.pane_transaction_osc_screens.insert(
            descriptor.pane_id.to_string(),
            TerminalScreen::new_with_history_config(
                descriptor.size,
                self.terminal_history_limit,
                self.terminal_history_rotate_lines,
            )?,
        );
        self.pane_readiness_states
            .insert(descriptor.pane_id.to_string(), PaneReadinessState::Unknown);
        self.pane_bootstrap_pending
            .insert(descriptor.pane_id.to_string());
        if let Some(start_directory) = start_directory {
            self.pane_current_working_directories.insert(
                descriptor.pane_id.to_string(),
                start_directory.to_path_buf(),
            );
        }

        if shell.used_fallback() {
            self.append_lifecycle_event(
                EventKind::Diagnostic,
                format!(
                    r#"{{"pane_id":"{}","diagnostic":"resolved shell fell back to /bin/sh"}}"#,
                    json_escape(descriptor.pane_id.as_str())
                ),
            )?;
        }

        let update = PaneProcessStart {
            session_id: self.session.id.to_string(),
            window_id: descriptor.window_id.to_string(),
            pane_id: descriptor.pane_id.to_string(),
            primary_pid,
            size: descriptor.size,
            registry_update: self.registry_update_plan(),
        };
        self.append_pane_start_event(&update)?;
        Ok(update)
    }

    /// Removes a live pane process from synchronous manager ownership for an
    /// async pane process owner.
    ///
    /// The session, screen, readiness, and lifecycle metadata stay in the
    /// runtime service; only PTY/process I/O ownership moves. Callers must start
    /// a replacement async owner before routing user input away from the
    /// compatibility manager path.
    pub fn take_running_pane_process_for_async_owner(
        &mut self,
        pane_id: &str,
    ) -> Result<PaneProcess> {
        self.require_live()?;
        let primary_pid = self.pane_processes.primary_pid(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "pane process not found",
            )
        })?;
        if let Some(current_working_directory) =
            self.pane_processes.current_working_directory(pane_id)
        {
            self.pane_current_working_directories
                .insert(pane_id.to_string(), current_working_directory);
        }
        let process = self.pane_processes.take_running_pane_process(pane_id)?;
        self.async_owned_pane_processes
            .insert(pane_id.to_string(), primary_pid);
        Ok(process)
    }

    /// Removes up to `limit` running pane processes for async pane workers.
    ///
    /// This is the dynamic production handoff entry point used by the async
    /// pane-process supervisor. Pane state remains in the runtime service while
    /// process, PTY output, input, resize, and termination ownership moves to
    /// one async worker per returned process.
    pub fn take_running_pane_processes_for_async_owner(
        &mut self,
        limit: usize,
    ) -> Result<Vec<(String, PaneProcess)>> {
        self.require_live()?;
        if limit == 0 {
            return Err(MezError::invalid_args(
                "async pane process handoff limit must be greater than zero",
            ));
        }
        let pane_ids = self
            .pane_processes
            .tracked_running_pane_ids()
            .into_iter()
            .take(limit)
            .collect::<Vec<_>>();
        let mut processes = Vec::with_capacity(pane_ids.len());
        for pane_id in pane_ids {
            let process = self.take_running_pane_process_for_async_owner(&pane_id)?;
            processes.push((pane_id, process));
        }
        Ok(processes)
    }

    /// Restores a pane process to synchronous manager ownership after a
    /// cancelled async owner handoff.
    pub fn restore_running_pane_process_from_async_owner(
        &mut self,
        pane_id: impl Into<String>,
        process: PaneProcess,
    ) -> Result<u32> {
        self.require_live()?;
        let pane_id = pane_id.into();
        self.async_owned_pane_processes.remove(&pane_id);
        self.pane_processes
            .insert_running_pane_process(pane_id, process)
    }

    /// Drains pane input operations deferred for async pane process workers.
    pub fn drain_deferred_pane_inputs(&mut self) -> Vec<DeferredPaneInput> {
        std::mem::take(&mut self.deferred_pane_inputs)
    }

    /// Drains coalesced pane resize operations deferred for async workers.
    pub fn drain_deferred_pane_resizes(&mut self) -> Vec<(String, DeferredPaneResize)> {
        std::mem::take(&mut self.deferred_pane_resizes)
            .into_iter()
            .collect()
    }

    /// Drains coalesced pane termination operations deferred for async workers.
    pub fn drain_deferred_pane_terminations(&mut self) -> Vec<(String, DeferredPaneTermination)> {
        std::mem::take(&mut self.deferred_pane_terminations)
            .into_iter()
            .collect()
    }

    /// Returns true when a pane's PTY/process handle is owned by an async worker.
    pub fn pane_process_is_async_owned(&self, pane_id: &str) -> bool {
        self.async_owned_pane_processes.contains_key(pane_id)
    }

    /// Runs the primary pid for live pane process operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn primary_pid_for_live_pane_process(&self, pane_id: &str) -> Option<u32> {
        self.pane_processes
            .primary_pid(pane_id)
            .or_else(|| self.async_owned_pane_processes.get(pane_id).copied())
    }

    /// Writes pane input immediately when the synchronous manager still owns
    /// the pane, or records it for the async pane worker when ownership has
    /// moved across the actor boundary.
    pub(super) fn write_runtime_pane_input(&mut self, pane_id: &str, input: &[u8]) -> Result<()> {
        self.write_runtime_pane_input_with_priority(pane_id, input, false)
    }

    /// Writes pane input with optional async queue priority.
    fn write_runtime_pane_input_with_priority(
        &mut self,
        pane_id: &str,
        input: &[u8],
        priority: bool,
    ) -> Result<()> {
        if input.is_empty() {
            return Err(MezError::invalid_args("pane input must not be empty"));
        }
        if self.pane_processes.contains_pane(pane_id) {
            return self.pane_processes.write_pane_input(pane_id, input);
        }
        if self.pane_process_is_async_owned(pane_id) {
            self.deferred_pane_inputs.push(DeferredPaneInput {
                pane_id: pane_id.to_string(),
                bytes: input.to_vec(),
                priority,
            });
            return Ok(());
        }
        Err(MezError::new(
            crate::error::MezErrorKind::NotFound,
            "pane process not found",
        ))
    }

    /// Writes pane input ahead of later queued input for the same async pane.
    pub(super) fn write_runtime_pane_input_priority(
        &mut self,
        pane_id: &str,
        input: &[u8],
    ) -> Result<()> {
        self.write_runtime_pane_input_with_priority(pane_id, input, true)
    }

    /// Terminates a pane process immediately when manager-owned, or queues a
    /// termination request for an async worker when ownership has moved.
    pub(super) fn terminate_runtime_pane_process(
        &mut self,
        pane_id: &str,
        force: bool,
    ) -> Result<bool> {
        self.agent_subshell_panes.remove(pane_id);
        self.agent_subshell_command_exit_panes.remove(pane_id);
        if self.pane_processes.contains_pane(pane_id) {
            return self
                .pane_processes
                .terminate_pane(pane_id)
                .map(|process| process.is_some());
        }
        if self.async_owned_pane_processes.remove(pane_id).is_some() {
            self.deferred_pane_terminations
                .insert(pane_id.to_string(), DeferredPaneTermination { force });
            return Ok(true);
        }
        Ok(false)
    }

    /// Terminates each listed pane process through the current owner boundary.
    pub(super) fn terminate_runtime_pane_processes<'a>(
        &mut self,
        pane_ids: impl IntoIterator<Item = &'a str>,
        force: bool,
    ) -> Result<usize> {
        let mut terminated = 0usize;
        for pane_id in pane_ids {
            if self.terminate_runtime_pane_process(pane_id, force)? {
                terminated = terminated.saturating_add(1);
            }
        }
        Ok(terminated)
    }

    /// Terminates all manager-owned and async-owned pane processes.
    pub(super) fn terminate_all_runtime_pane_processes(&mut self, force: bool) -> Result<usize> {
        let mut pane_ids = self.pane_processes.tracked_pane_ids();
        pane_ids.extend(self.async_owned_pane_processes.keys().cloned());
        self.terminate_runtime_pane_processes(pane_ids.iter().map(String::as_str), force)
    }

    /// Drops runtime-only state for a pane that has been removed from the
    /// session layout.
    ///
    /// Pane closure and process-exit paths remove the pane from the session
    /// model first, then call this helper to clear prompt, readiness, screen,
    /// deferred I/O, and subagent bookkeeping that would otherwise make a
    /// closed pane appear partially alive to later agent/session surfaces.
    pub(super) fn cleanup_removed_pane_runtime_state(&mut self, pane_id: &str) {
        self.agent_shell_store.remove_session(pane_id);
        self.agent_subshell_panes.remove(pane_id);
        self.agent_subshell_command_exit_panes.remove(pane_id);
        self.agent_prompt_inputs.remove(pane_id);
        self.agent_planning_modes.remove(pane_id);
        self.agent_personality_selections.remove(pane_id);
        self.agent_response_styles.remove(pane_id);
        self.agent_auto_reasoning_overrides.remove(pane_id);
        self.agent_copy_outputs.remove(pane_id);
        self.agent_modified_files.remove(pane_id);
        self.active_copy_modes.remove(pane_id);
        self.pane_current_working_directories.remove(pane_id);
        self.deferred_pane_inputs
            .retain(|input| input.pane_id != pane_id);
        self.deferred_pane_resizes.remove(pane_id);
        self.deferred_pane_pipe_writes
            .retain(|write| write.pane_id != pane_id);
        self.pane_screens.remove(pane_id);
        self.pane_transaction_osc_screens.remove(pane_id);
        self.pane_transaction_osc_pending.remove(pane_id);
        self.pane_mez_wrapper_filter_pending.remove(pane_id);
        self.pane_mez_wrapper_filter_recent_commands.remove(pane_id);
        self.pane_mez_wrapper_filter_recent_polls.remove(pane_id);
        self.pane_hidden_shell_render_recent_polls.remove(pane_id);
        self.pane_exit_records.remove(pane_id);
        self.active_pane_pipes.remove(pane_id);
        self.pane_transcript_refs.remove(pane_id);
        self.pane_readiness_states.remove(pane_id);
        self.pane_readiness_overrides.clear_pending_probe(pane_id);
        self.pane_readiness_overrides
            .revoke(pane_id, ReadinessOverrideRevocation::PaneClosed);
        self.pane_environment_signatures.remove(pane_id);
        self.pane_bootstrap_pending.remove(pane_id);
        self.pane_instruction_files.remove(pane_id);
        self.pane_closing.remove(pane_id);
        self.pending_terminal_subagent_pane_closes.remove(pane_id);
        self.model_profile_overrides.pane_profiles.remove(pane_id);
        let pane_turn_ids = self
            .agent_turn_ledger
            .turns()
            .iter()
            .filter(|turn| turn.pane_id == pane_id)
            .map(|turn| turn.turn_id.clone())
            .collect::<Vec<_>>();
        for turn_id in &pane_turn_ids {
            self.clear_agent_failure_feedback_attempts_for_turn(turn_id);
            self.subagent_task_routes.remove(turn_id);
            self.clear_joined_subagent_dependencies_for_turn(turn_id);
        }

        let agent_id = format!("agent-{pane_id}");
        self.subagent_task_routes
            .retain(|_child_turn_id, parent_agent_id| parent_agent_id != &agent_id);
        self.joined_subagent_dependencies
            .retain(|_child_turn_id, dependency| dependency.child_agent_id != agent_id);
        self.model_profile_overrides
            .agent_profiles
            .remove(&agent_id);
        self.subagent_scopes.unregister(&agent_id);
        self.subagent_scope_declarations.remove(&agent_id);
        self.subagent_lineage.remove(&agent_id);
        if let Some(agent_id) = AgentId::opaque(agent_id)
            && self
                .message_service
                .registered_identity(&agent_id)
                .is_some()
        {
            let _ = self.message_service.update_presence(
                &agent_id,
                crate::message::AgentPresenceStatus::Offline,
                current_unix_seconds().saturating_mul(1000),
            );
        }

        let live_windows = self
            .session
            .windows()
            .iter()
            .map(|window| window.id.to_string())
            .collect::<BTreeSet<_>>();
        self.subagent_window_ids
            .retain(|window_id| live_windows.contains(window_id));
    }

    /// Runs the close exited pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn close_exited_pane(&mut self, descriptor: &PaneDescriptor) -> Result<()> {
        self.pane_closing.remove(descriptor.pane_id.as_str());
        self.session
            .close_exited_pane(descriptor.pane_id.as_str())?;
        self.cleanup_removed_pane_runtime_state(descriptor.pane_id.as_str());
        Ok(())
    }

    /// Runs the initial pane descriptor operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn initial_pane_descriptor(&self) -> Result<PaneDescriptor> {
        let window = self
            .session
            .windows()
            .first()
            .ok_or_else(|| MezError::invalid_state("session has no windows"))?;
        let pane = window
            .panes()
            .first()
            .ok_or_else(|| MezError::invalid_state("initial window has no panes"))?;
        let size = self
            .pane_process_size_for(window, pane.id.as_str())
            .unwrap_or(pane.size);
        Ok(PaneDescriptor {
            window_id: window.id.clone(),
            pane_id: pane.id.clone(),
            size,
        })
    }

    /// Runs the active window pane descriptor operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn active_window_pane_descriptor(
        &self,
        target: Option<&str>,
    ) -> Result<PaneDescriptor> {
        let window = self
            .session
            .active_window()
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        let pane = match target {
            Some(target) => window
                .panes()
                .iter()
                .find(|pane| pane.id.as_str() == target || pane.index.to_string() == target)
                .ok_or_else(|| {
                    MezError::new(crate::error::MezErrorKind::NotFound, "pane not found")
                })?,
            None => window.active_pane(),
        };
        let size = self
            .pane_process_size_for(window, pane.id.as_str())
            .unwrap_or(pane.size);
        Ok(PaneDescriptor {
            window_id: window.id.clone(),
            pane_id: pane.id.clone(),
            size,
        })
    }

    /// Runs the find pane descriptor operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn find_pane_descriptor(&self, pane_id: &str) -> Option<PaneDescriptor> {
        self.session.windows().iter().find_map(|window| {
            window
                .panes()
                .iter()
                .find(|pane| pane.id.as_str() == pane_id)
                .map(|pane| PaneDescriptor {
                    window_id: window.id.clone(),
                    pane_id: pane.id.clone(),
                    size: self
                        .pane_process_size_for(window, pane.id.as_str())
                        .unwrap_or(pane.size),
                })
        })
    }

    /// Runs the find pane title operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn find_pane_title(&self, pane_id: &str) -> Option<String> {
        self.session.windows().iter().find_map(|window| {
            window
                .panes()
                .iter()
                .find(|pane| pane.id.as_str() == pane_id)
                .map(|pane| pane.title.clone())
        })
    }

    /// Runs the tracked pane descriptors operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn tracked_pane_descriptors(&self) -> Vec<PaneDescriptor> {
        self.session
            .windows()
            .iter()
            .flat_map(|window| {
                window.panes().iter().filter_map(|pane| {
                    if self.pane_processes.contains_pane(pane.id.as_str())
                        || self.pane_process_is_async_owned(pane.id.as_str())
                    {
                        let size = self
                            .pane_process_size_for(window, pane.id.as_str())
                            .unwrap_or(pane.size);
                        Some(PaneDescriptor {
                            window_id: window.id.clone(),
                            pane_id: pane.id.clone(),
                            size,
                        })
                    } else {
                        None
                    }
                })
            })
            .collect()
    }

    /// Runs the pane process size for operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn pane_process_size_for(&self, window: &crate::layout::Window, pane_id: &str) -> Option<Size> {
        let pane = window
            .panes()
            .iter()
            .find(|pane| pane.id.as_str() == pane_id)?;
        let window_frame_visible = self.window_frames_enabled;
        let group_rows = u16::from(self.session.window_groups().len() > 1);
        let display_size = Size::new(
            window.size.columns,
            window.size.rows.saturating_sub(group_rows).max(1),
        )
        .ok()?;
        if window.zoomed_pane_id() == Some(&pane.id) {
            let body_size = rendered_window_body_size(display_size, window_frame_visible).ok()?;
            let geometry = crate::layout::PaneGeometry {
                index: pane.index,
                column: 0,
                row: 0,
                columns: body_size.columns,
                rows: body_size.rows,
            };
            let content_size = pane_content_size_for_geometry(
                &geometry,
                std::slice::from_ref(&geometry),
                self.pane_frames_enabled,
                self.pane_frame_position,
            )
            .ok()?;
            return Some(
                self.pane_process_size_after_agent_prompt_reservation(
                    pane.id.as_str(),
                    content_size,
                ),
            );
        }

        let body_size = rendered_window_body_size(display_size, window_frame_visible).ok()?;
        let geometries = window.pane_geometries_for_size(body_size);
        let geometry = geometries
            .iter()
            .find(|geometry| geometry.index == pane.index)?;
        let content_size = pane_content_size_for_geometry(
            geometry,
            &geometries,
            self.pane_frames_enabled,
            self.pane_frame_position,
        )
        .ok()?;
        Some(self.pane_process_size_after_agent_prompt_reservation(pane.id.as_str(), content_size))
    }

    /// Removes rows reserved for the pane-local agent prompt from the PTY size
    /// advertised to the shell. Keeping the process size aligned with the
    /// visible terminal buffer prevents prompts and cursor reports from
    /// landing underneath the agent input row.
    fn pane_process_size_after_agent_prompt_reservation(&self, pane_id: &str, size: Size) -> Size {
        let reserved_rows = self.agent_prompt_reserved_rows_for_pane(
            pane_id,
            usize::from(size.columns),
            usize::from(size.rows),
        );
        let reserved_rows = u16::try_from(reserved_rows)
            .unwrap_or(u16::MAX)
            .min(size.rows.saturating_sub(1));
        Size {
            columns: size.columns,
            rows: size.rows.saturating_sub(reserved_rows).max(1),
        }
    }

    /// Runs the append pane start event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_pane_start_event(&mut self, update: &PaneProcessStart) -> Result<()> {
        self.append_lifecycle_event(
            EventKind::PaneChanged,
            format!(
                r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"process_state":"running","columns":{},"rows":{}}}"#,
                json_escape(&update.pane_id),
                json_escape(&update.window_id),
                update.primary_pid,
                update.size.columns,
                update.size.rows
            ),
        )
    }

    /// Runs the append pane resize event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_pane_resize_event(&mut self, update: &PaneResizeUpdate) -> Result<()> {
        self.append_lifecycle_event(
            EventKind::PaneChanged,
            format!(
                r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"process_state":"running","layout":"resized","columns":{},"rows":{}}}"#,
                json_escape(&update.pane_id),
                json_escape(&update.window_id),
                update.primary_pid,
                update.size.columns,
                update.size.rows
            ),
        )
    }

    /// Runs the append pane output event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_pane_output_event(&mut self, update: &PaneOutputUpdate) -> Result<()> {
        self.append_lifecycle_event(
            EventKind::PaneChanged,
            format!(
                r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"process_state":"running","output_bytes":{},"activity_events":{},"bell_events":{},"background":{}}}"#,
                json_escape(&update.pane_id),
                json_escape(&update.window_id),
                update.primary_pid,
                update.bytes_read,
                update.activity_events,
                update.bell_events,
                update.background
            ),
        )
    }

    /// Runs the append pane title event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_pane_title_event(&mut self, update: &PaneOutputUpdate) -> Result<()> {
        let title = self
            .find_pane_title(update.pane_id.as_str())
            .unwrap_or_else(|| "shell".to_string());
        self.append_lifecycle_event(
            EventKind::PaneChanged,
            format!(
                r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"process_state":"running","title":"{}"}}"#,
                json_escape(&update.pane_id),
                json_escape(&update.window_id),
                update.primary_pid,
                json_escape(&title)
            ),
        )
    }

    /// Runs the append pane exit event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_pane_exit_event(&mut self, update: &PaneExitUpdate) -> Result<()> {
        self.append_lifecycle_event(
            EventKind::PaneChanged,
            format!(
                r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"process_state":"exited","exit_status":{},"exit_code":{},"signal":{},"closed_window":{},"session_empty":{}}}"#,
                json_escape(&update.pane_id),
                json_escape(&update.window_id),
                update.primary_pid,
                update.exit_status.to_json(),
                optional_i32_json(update.exit_status.code),
                optional_i32_json(update.exit_status.signal),
                update.closed_window,
                update.session_empty
            ),
        )
    }

    /// Runs the append pane close event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_pane_close_event(
        &mut self,
        pane_id: &str,
        window_id: &str,
        terminated_panes: usize,
        session_empty: bool,
    ) -> Result<()> {
        self.append_lifecycle_event(
            EventKind::PaneChanged,
            format!(
                r#"{{"pane_id":"{}","window_id":"{}","state":"closed","closed":true,"terminated_panes":{},"session_empty":{}}}"#,
                json_escape(pane_id),
                json_escape(window_id),
                terminated_panes,
                session_empty
            ),
        )
    }

    /// Runs the append window close event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_window_close_event(
        &mut self,
        window_id: &str,
        terminated_panes: usize,
        session_empty: bool,
    ) -> Result<()> {
        self.append_lifecycle_event(
            EventKind::WindowChanged,
            format!(
                r#"{{"window_id":"{}","state":"closed","closed":true,"terminated_panes":{},"session_empty":{}}}"#,
                json_escape(window_id),
                terminated_panes,
                session_empty
            ),
        )
    }
}

/// Runs the validate runtime start directory operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_runtime_start_directory(start_directory: Option<&Path>) -> Result<()> {
    let Some(start_directory) = start_directory else {
        return Ok(());
    };
    let metadata = std::fs::metadata(start_directory).map_err(|error| {
        MezError::invalid_args(format!(
            "start_directory `{}` is not accessible: {error}",
            start_directory.display()
        ))
    })?;
    if !metadata.is_dir() {
        return Err(MezError::invalid_args(format!(
            "start_directory `{}` is not a directory",
            start_directory.display()
        )));
    }
    Ok(())
}

/// Runs the validate new window requested pane size operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_new_window_requested_pane_size(window_size: Size, spec: PaneSizeSpec) -> Result<()> {
    let size = requested_new_window_pane_size(window_size, spec)?;
    validate_pane_size_for_resize(size)?;
    if size.columns > window_size.columns || size.rows > window_size.rows {
        return Err(MezError::invalid_args(
            "pane creation size must fit inside the new window",
        ));
    }
    Ok(())
}

/// Runs the requested new window pane size operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn requested_new_window_pane_size(window_size: Size, spec: PaneSizeSpec) -> Result<Size> {
    match spec {
        PaneSizeSpec::Cells { columns, rows } => Size::new(
            columns.unwrap_or(window_size.columns),
            rows.unwrap_or(window_size.rows),
        ),
        PaneSizeSpec::Percent { percent, axis } => {
            if percent == 0 {
                return Err(MezError::invalid_args(
                    "percent pane creation size requires a positive percent",
                ));
            }
            let columns = if matches!(axis, ResizeAxis::Columns | ResizeAxis::Both) {
                requested_percent_dimension(window_size.columns, percent, "columns")?
            } else {
                window_size.columns
            };
            let rows = if matches!(axis, ResizeAxis::Rows | ResizeAxis::Both) {
                requested_percent_dimension(window_size.rows, percent, "rows")?
            } else {
                window_size.rows
            };
            Size::new(columns, rows)
        }
        PaneSizeSpec::Delta { direction, amount }
        | PaneSizeSpec::Edge {
            edge: direction,
            amount,
        } => requested_new_window_pane_size_from_direction(window_size, direction, amount),
    }
}

/// Runs the requested new window pane size from direction operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn requested_new_window_pane_size_from_direction(
    current: Size,
    direction: ResizeDirection,
    amount: u16,
) -> Result<Size> {
    if amount == 0 {
        return Err(MezError::invalid_args(
            "directional pane creation size amount must be positive",
        ));
    }
    match direction {
        ResizeDirection::Left => Size::new(
            current.columns.checked_sub(amount).ok_or_else(|| {
                MezError::invalid_args("pane creation size would reduce columns below zero")
            })?,
            current.rows,
        ),
        ResizeDirection::Right => Size::new(
            current.columns.checked_add(amount).ok_or_else(|| {
                MezError::invalid_args("pane creation size columns are out of range")
            })?,
            current.rows,
        ),
        ResizeDirection::Up => Size::new(
            current.columns,
            current.rows.checked_sub(amount).ok_or_else(|| {
                MezError::invalid_args("pane creation size would reduce rows below zero")
            })?,
        ),
        ResizeDirection::Down => Size::new(
            current.columns,
            current.rows.checked_add(amount).ok_or_else(|| {
                MezError::invalid_args("pane creation size rows are out of range")
            })?,
        ),
    }
}

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
    }
}

/// Runs the requested percent dimension operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn requested_percent_dimension(total: u16, percent: u16, axis: &'static str) -> Result<u16> {
    let scaled = u32::from(total)
        .saturating_mul(u32::from(percent))
        .saturating_add(99)
        / 100;
    u16::try_from(scaled.max(1)).map_err(|_| {
        MezError::invalid_args(format!("percent pane creation size {axis} is out of range"))
    })
}

/// Runs the terminal clipboard policy accepts osc52 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn terminal_clipboard_policy_accepts_osc52(policy: &str) -> bool {
    matches!(policy, "external" | "host" | "internal")
}
