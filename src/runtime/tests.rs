//! Regression coverage for the runtime tests subsystem.
//!
//! These tests describe the behavior protected by the repository
//! specification and workflow guidance. Keeping the scenarios documented
//! makes failures easier to map back to the user-visible contract.

// Runtime module tests.

use super::{
    ActionStatus, AgentId, AgentShellVisibility, AgentTurnState, ApprovalPolicy, AuditLog,
    AuthStore, AuxiliarySocketKind, BlockedApprovalRequest, CommandRuleScope, ConfigFormat,
    ConfigLayer, ConfigScope, ContextBlock, ContextSourceKind, ControlConnectionState,
    CooperationMode, EventAudience, EventKind, HookEvent, JoinedSubagentDependency,
    MEZ_ENV_FIELD_SEPARATOR, MemoryRecord, ModelProfile, ModelProvider, OsString, PaneExitRecord,
    PaneExitUpdate, PaneReadinessState, Path, PathBuf, ProjectTrustStore, Result, RuleDecision,
    RuleMatch, RunningShellTransactionKind, RunningShellTransactionRef,
    RuntimeAgentModifiedFileSummary, RuntimeEnv, RuntimeLifecycleState, RuntimeRegistryUpdatePlan,
    RuntimeSessionService, RuntimeSubagentLineage, RuntimeSubagentPlacement, SenderIdentity,
    SocketDirectorySource, SplitDirection, SubagentWaitPolicy, TrustDecision, UnixStream,
    authorize_unix_peer, authorize_unix_peer_uid, auxiliary_socket_path_for_control_socket,
    bind_control_socket, default_socket_directory, effective_uid, ensure_private_socket_directory,
    fs, json_escape, pane_environment, pane_environment_with_term,
    prune_stale_socket_files_in_directory, runtime_hook_event_for_lifecycle,
    runtime_hook_event_name, runtime_marker_for_action, socket_path_for_name,
};
use crate::MezError;
use crate::agent::AgentLogLevel;
use crate::scheduler::{ScheduledWork, ScheduledWorkKind};
use crate::session::Session;
use crate::subagent::SubagentSpawnRequest;
use crate::terminal::{
    AttachedTerminalClientStepPlan, ClientViewRole, CopyPosition, DEFAULT_PANE_TERM, HostClipboard,
    MouseAction, MuxAction, PaneAgentStatusField, PaneFocusDirection, TerminalClientLoopAction,
    TerminalClientLoopConfig, TerminalColor, TerminalOscEvent, TerminalScreen, TerminalStyledLine,
    UI_COLOR_SLOT_NAMES,
};
use crate::transcript::AgentTranscriptStore;
use base64::Engine;
use std::cell::RefCell;
use std::os::unix::fs::PermissionsExt;
use unicode_width::UnicodeWidthStr;

const EXPECTED_MARKDOWN_BLOCK_DIVIDER_GLYPH: char = '─';
const EXPECTED_MARKDOWN_INLINE_CODE_FOREGROUND: TerminalColor =
    TerminalColor::Rgb(0xe6, 0xe6, 0xe6);
const EXPECTED_MARKDOWN_TABLE_ALTERNATE_ROW_FOREGROUND: TerminalColor =
    TerminalColor::Rgb(0xe6, 0xe6, 0xe6);

/// Returns the rendered style active at one displayed terminal column.
///
/// # Parameters
/// - `line`: The styled terminal line to inspect.
/// - `column`: The zero-based display column within the line.
fn styled_line_rendition_at(
    line: &TerminalStyledLine,
    column: usize,
) -> crate::terminal::GraphicRendition {
    line.style_spans
        .iter()
        .rev()
        .find(|span| column >= span.start && column < span.start.saturating_add(span.length))
        .map(|span| span.rendition)
        .unwrap_or_default()
}

/// Returns the display column at which one text fragment starts.
///
/// # Parameters
/// - `line`: The rendered terminal line to inspect.
/// - `needle`: The text fragment whose starting column is needed.
fn display_column_for_fragment(line: &str, needle: &str) -> usize {
    let byte_index = line
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} missing from {line:?}"));
    UnicodeWidthStr::width(&line[..byte_index])
}

/// Returns the expected full-width markdown frame row for a pane.
fn expected_markdown_block_divider_line(columns: usize) -> String {
    format!(
        "▐ {}",
        EXPECTED_MARKDOWN_BLOCK_DIVIDER_GLYPH
            .to_string()
            .repeat(columns.saturating_sub("▐ ".chars().count()))
    )
}

/// Runs the effective uid for tests operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub(crate) fn effective_uid_for_tests() -> u32 {
    super::current_effective_uid()
}

use crate::control::{decode_control_frame, encode_control_body};
use crate::ids::IdFactory;
use crate::layout::Size;
use crate::registry::{RegistrySessionState, SessionRegistry};
use crate::shell::{ResolvedShell, ShellSource};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

/// Defines the TEST HOST CLIPBOARD WRITES static used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
static TEST_HOST_CLIPBOARD_WRITES: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Defines the TEST HOST CLIPBOARD TEST LOCK static used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
static TEST_HOST_CLIPBOARD_TEST_LOCK: Mutex<()> = Mutex::new(());

/// Runs the record host clipboard copy operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn record_host_clipboard_copy(content: &str) -> bool {
    TEST_HOST_CLIPBOARD_WRITES
        .lock()
        .unwrap()
        .push(content.to_string());
    true
}

/// Runs the empty host clipboard read operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn empty_host_clipboard_read() -> Option<String> {
    None
}

/// Runs the test session operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn test_session() -> Session {
    Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
}

/// Runs the test session with size operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn test_session_with_size(size: Size) -> Session {
    Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        size,
    )
}

/// Runs the test runtime service operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn test_runtime_service() -> RuntimeSessionService {
    let mut service = RuntimeSessionService::with_event_log(
        test_session(),
        PathBuf::from("/tmp/mez-1000/default.sock"),
        100,
        10,
        1024,
    )
    .unwrap();
    service.host_clipboard = HostClipboard::disabled();
    service
}

/// Runs the test runtime service with size operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn test_runtime_service_with_size(size: Size) -> RuntimeSessionService {
    let mut service = RuntimeSessionService::with_event_log(
        test_session_with_size(size),
        PathBuf::from("/tmp/mez-1000/default.sock"),
        100,
        10,
        1024,
    )
    .unwrap();
    service.host_clipboard = HostClipboard::disabled();
    service
}

/// Resolves a bash binary for tests that need to exercise interactive bash
/// parent-shell behavior rather than the fallback POSIX shell.
fn bash_path_for_tests() -> Option<PathBuf> {
    ["/usr/bin/bash", "/bin/bash"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| {
            fs::metadata(path)
                .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        })
}

/// Runs the mark test pane ready operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn mark_test_pane_ready(service: &mut RuntimeSessionService, pane_id: &str) {
    service.set_pane_readiness(pane_id, PaneReadinessState::Ready);
}

/// Runs the temp root operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn temp_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("mez-runtime-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

/// Verifies that runtime hook diagnostics use the same canonical event label as
/// hook audit records and hook configuration. This matters because blocked
/// action payloads and hook failure events are user-visible protocol surfaces
/// that automation can match exactly.
#[test]
fn runtime_hook_event_name_uses_canonical_agent_turn_stop_label() {
    assert_eq!(
        runtime_hook_event_name(HookEvent::AgentTurnStop),
        "agent_turn_stop"
    );
}

/// Ensures every terminal agent-turn lifecycle state feeds the same turn-end
/// hook. This keeps user stops aligned with provider completion and failure so
/// configured cleanup hooks run regardless of how the turn ended.
#[test]
fn runtime_hook_lifecycle_maps_cancelled_turns_to_agent_turn_end() {
    assert_eq!(
        runtime_hook_event_for_lifecycle(
            EventKind::AgentStatus,
            r#"{"agent_prompt_turn":"turn-1","state":"completed"}"#,
        ),
        Some(HookEvent::AgentTurnStop)
    );
    assert_eq!(
        runtime_hook_event_for_lifecycle(
            EventKind::AgentStatus,
            r#"{"agent_prompt_turn":"turn-2","state":"failed"}"#,
        ),
        Some(HookEvent::AgentTurnStop)
    );
    assert_eq!(
        runtime_hook_event_for_lifecycle(
            EventKind::AgentStatus,
            r#"{"agent_prompt_turn":"turn-3","state":"cancelled"}"#,
        ),
        Some(HookEvent::AgentTurnStop)
    );
}

/// Carries Runtime Echo Provider state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
struct RuntimeEchoProvider;

/// Runs the runtime complete batch operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_complete_batch(turn_id: impl Into<String>) -> crate::agent::MaapBatch {
    runtime_complete_batch_for(turn_id, "agent-%1")
}

/// Runs the runtime complete batch for operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_complete_batch_for(
    turn_id: impl Into<String>,
    agent_id: impl Into<String>,
) -> crate::agent::MaapBatch {
    crate::agent::MaapBatch {
        protocol: "maap/1".to_string(),
        rationale: "test action batch rationale".to_string(),
        turn_id: turn_id.into(),
        agent_id: agent_id.into(),
        actions: vec![crate::agent::AgentAction {
            id: "complete-1".to_string(),
            rationale: "finish the turn".to_string(),
            payload: crate::agent::AgentActionPayload::Complete,
        }],
        final_turn: true,
    }
}

/// Verifies provider-completion validation accepts terminal controller failure
/// summaries.
///
/// Failure-summary completions are synthetic runtime-owned failures: the model
/// supplies a user-facing `say`, but the turn remains failed because the
/// provider/controller boundary had already failed before ordinary action
/// execution could continue.
#[test]
fn runtime_provider_completion_accepts_controller_failure_summary_state() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "determine the next implementation target")
        .unwrap();
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    let action = crate::agent::AgentAction {
        id: "say-1".to_string(),
        rationale: "summarize the provider failure".to_string(),
        payload: crate::agent::AgentActionPayload::Say {
            status: crate::agent::SayStatus::Progress,
            text: "The provider request failed before any action could run.".to_string(),
            content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    let result = crate::agent::ActionResult::succeeded(
        &turn,
        &action,
        vec!["The provider request failed before any action could run.".to_string()],
        None,
    );
    let execution = crate::agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: crate::agent::ModelResponse {
            provider: "openai".to_string(),
            model: "test".to_string(),
            raw_text:
                "provider_error: InvalidState: upstream failure\ncontroller_failure_summary:\nsummary"
                    .to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![action],
                final_turn: false,
            }),
        },
        latest_response_usage: Default::default(),
        action_results: vec![result],
        final_turn: true,
        terminal_state: AgentTurnState::Failed,
    };

    super::agent::runtime_validate_provider_completion_execution(&turn, &execution).unwrap();
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies provider-completion validation accepts controller-synthesized
/// terminal capability negotiation failures.
///
/// A model can exceed the bounded `request_capability` negotiation budget
/// before any executable action runs. The runner then creates a failed action
/// result, and that result must match a terminal MAAP batch so the runtime can
/// fail the turn cleanly instead of producing a completion-application error.
#[test]
fn runtime_provider_completion_accepts_terminal_capability_failure_state() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "determine the next implementation target")
        .unwrap();
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    let action = crate::agent::AgentAction {
        id: "capability-1".to_string(),
        rationale: "need shell actions".to_string(),
        payload: crate::agent::AgentActionPayload::RequestCapability {
            capability: crate::agent::AgentCapability::Shell,
            reason: "need shell actions".to_string(),
        },
    };
    let result = crate::agent::ActionResult::failed(
        &turn,
        &action,
        crate::agent::ActionStatus::Failed,
        "capability_request_limit",
        "model exceeded capability request limit before emitting executable or user-facing output",
    )
    .unwrap();
    let execution = crate::agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: crate::agent::ModelResponse {
            provider: "openai".to_string(),
            model: "test".to_string(),
            raw_text: "request shell capability".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![action],
                final_turn: true,
            }),
        },
        latest_response_usage: Default::default(),
        action_results: vec![result],
        final_turn: true,
        terminal_state: AgentTurnState::Failed,
    };

    super::agent::runtime_validate_provider_completion_execution(&turn, &execution).unwrap();
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies provider-completion validation accepts controller-owned MAAP
/// validation failures while retaining the rejected model batch.
///
/// The retained batch is diagnostic evidence only: validation has already
/// rejected it before action execution, so no action results exist even though
/// the response still carries the parsed batch for audit and transcript output.
#[test]
fn runtime_provider_completion_accepts_terminal_maap_validation_failure_state() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "call unavailable tool")
        .unwrap();
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    let action = crate::agent::AgentAction {
        id: "mcp-1".to_string(),
        rationale: "call missing tool".to_string(),
        payload: crate::agent::AgentActionPayload::McpCall {
            server: "missing".to_string(),
            tool: "read".to_string(),
            arguments_json: "{}".to_string(),
        },
    };
    let execution = crate::agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: crate::agent::ModelResponse {
            provider: "openai".to_string(),
            model: "test".to_string(),
            raw_text: "bad maap action\nmaap_validation_error: unavailable server".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![action],
                final_turn: false,
            }),
        },
        latest_response_usage: Default::default(),
        action_results: Vec::new(),
        final_turn: true,
        terminal_state: AgentTurnState::Failed,
    };

    super::agent::runtime_validate_provider_completion_execution(&turn, &execution).unwrap();
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies provider-completion validation rejects missing-batch executions
/// that are not terminal failures.
///
/// A missing action batch means the provider/controller path failed before
/// MAAP execution. That state is valid only as a terminal failed turn; accepting
/// it as running or completed would let malformed provider output enter the
/// scheduler as ordinary progress.
#[test]
fn runtime_provider_completion_rejects_nonterminal_missing_batch_state() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service.start_agent_prompt_turn("%1", "hello").unwrap();
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    let execution = crate::agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: crate::agent::ModelResponse {
            provider: "openai".to_string(),
            model: "test".to_string(),
            raw_text: "plain text without MAAP".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: None,
        },
        latest_response_usage: Default::default(),
        action_results: Vec::new(),
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };

    let error = super::agent::runtime_validate_provider_completion_execution(&turn, &execution)
        .unwrap_err();

    assert!(
        error
            .message()
            .contains("without an action batch must be a terminal failed execution"),
        "{error}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies provider-completion validation rejects non-final empty action
/// batches.
///
/// Empty `all(...)` checks previously made an empty non-final batch look like a
/// display-only completion. The runtime boundary should reject that malformed
/// batch explicitly instead.
#[test]
fn runtime_provider_completion_rejects_empty_nonfinal_batch_state() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service.start_agent_prompt_turn("%1", "hello").unwrap();
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    let execution = crate::agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: crate::agent::ModelResponse {
            provider: "openai".to_string(),
            model: "test".to_string(),
            raw_text: "empty batch".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: Vec::new(),
                final_turn: false,
            }),
        },
        latest_response_usage: Default::default(),
        action_results: Vec::new(),
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };

    let error = super::agent::runtime_validate_provider_completion_execution(&turn, &execution)
        .unwrap_err();

    assert!(
        error
            .message()
            .contains("action batch has no actions but is not final"),
        "{error}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

impl ModelProvider for RuntimeEchoProvider {
    /// Runs the provider id operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn provider_id(&self) -> &str {
        "runtime-echo"
    }

    /// Runs the send request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn send_request(
        &self,
        request: &crate::agent::ModelRequest,
    ) -> Result<crate::agent::ModelResponse> {
        Ok(crate::agent::ModelResponse {
            provider: self.provider_id().to_string(),
            model: request.model.clone(),
            raw_text: "done".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(runtime_complete_batch(request.turn_id.clone())),
        })
    }
}

/// Carries Runtime Failing Provider state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
struct RuntimeFailingProvider;

impl ModelProvider for RuntimeFailingProvider {
    /// Runs the provider id operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn provider_id(&self) -> &str {
        "runtime-fail"
    }

    /// Runs the send request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn send_request(
        &self,
        _request: &crate::agent::ModelRequest,
    ) -> Result<crate::agent::ModelResponse> {
        Err(MezError::invalid_state("provider API request failed").with_provider_failure_json(
            r#"{"status_code":400,"error":{"message":"stream must be set to true","type":"invalid_request_error","code":"missing_required_parameter"}}"#,
        ))
    }
}

/// Carries Runtime Provider Raw Text Failing Provider state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
struct RuntimeProviderRawTextFailingProvider;

impl ModelProvider for RuntimeProviderRawTextFailingProvider {
    /// Runs the provider id operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn provider_id(&self) -> &str {
        "runtime-raw-fail"
    }

    /// Runs the send request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn send_request(
        &self,
        _request: &crate::agent::ModelRequest,
    ) -> Result<crate::agent::ModelResponse> {
        Err(
            MezError::invalid_args("provider MAAP output is malformed: missing turn_id")
                .with_provider_raw_text("{\"protocol\":\"maap/1\",\"actions\":[]}")
                .with_provider_failure_json(
                    r#"{"type":"malformed_model_output","error":{"kind":"invalid_args","message":"missing turn_id"},"output":{"format":"json","bytes":34,"top_level_keys":["actions","protocol"]}}"#,
                ),
        )
    }
}

/// Carries Runtime Batch Provider state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
struct RuntimeBatchProvider {
    /// Stores the response value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    response: crate::agent::ModelResponse,
}

/// Carries Runtime Batch Failing Provider state for this subsystem.
///
/// The type keeps a failing provider under the `runtime-batch` id so tests can
/// exercise provider-continuation failures after a successful first batch.
struct RuntimeBatchFailingProvider;

impl ModelProvider for RuntimeBatchFailingProvider {
    /// Runs the provider id operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn provider_id(&self) -> &str {
        "runtime-batch"
    }

    /// Runs the send request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn send_request(
        &self,
        _request: &crate::agent::ModelRequest,
    ) -> Result<crate::agent::ModelResponse> {
        Err(MezError::invalid_state(
            "provider continuation failed after shell command result",
        ))
    }
}

/// Infers the coarse capability needed to reach a runtime test response.
///
/// Most runtime tests are concerned with dispatch behavior after the model has
/// selected an executable action. The production runner now performs a
/// non-executing capability round-trip first, so test providers synthesize that
/// request when their fixed response contains executable actions.
fn runtime_capability_for_response(
    response: &crate::agent::ModelResponse,
) -> Option<crate::agent::AgentCapability> {
    response
        .action_batch
        .as_ref()?
        .actions
        .iter()
        .find_map(|action| match &action.payload {
            crate::agent::AgentActionPayload::ShellCommand { .. }
            | crate::agent::AgentActionPayload::ApplyPatch { .. } => {
                Some(crate::agent::AgentCapability::Shell)
            }
            crate::agent::AgentActionPayload::WebSearch { .. } => {
                Some(crate::agent::AgentCapability::NetworkSearch)
            }
            crate::agent::AgentActionPayload::FetchUrl { .. } => {
                Some(crate::agent::AgentCapability::NetworkFetch)
            }
            crate::agent::AgentActionPayload::McpCall { .. } => {
                Some(crate::agent::AgentCapability::Mcp)
            }
            crate::agent::AgentActionPayload::SendMessage { .. }
            | crate::agent::AgentActionPayload::SpawnAgent { .. } => {
                Some(crate::agent::AgentCapability::Subagent)
            }
            crate::agent::AgentActionPayload::ConfigChange { .. } => {
                Some(crate::agent::AgentCapability::ConfigChange)
            }
            crate::agent::AgentActionPayload::Say { .. }
            | crate::agent::AgentActionPayload::RequestCapability { .. }
            | crate::agent::AgentActionPayload::RequestSkills
            | crate::agent::AgentActionPayload::CallSkill { .. }
            | crate::agent::AgentActionPayload::Complete
            | crate::agent::AgentActionPayload::Abort { .. } => None,
        })
}

/// Builds the synthetic capability response used by runtime test providers.
fn runtime_capability_response(
    provider_id: &str,
    request: &crate::agent::ModelRequest,
    capability: crate::agent::AgentCapability,
) -> crate::agent::ModelResponse {
    crate::agent::ModelResponse {
        provider: provider_id.to_string(),
        model: request.model.clone(),
        raw_text: format!("request {}", capability.as_str()),
        usage: Default::default(),
        quota_usage: Default::default(),
        action_batch: Some(crate::agent::MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "test action batch rationale".to_string(),
            turn_id: request.turn_id.clone(),
            agent_id: request.agent_id.clone(),
            actions: vec![crate::agent::AgentAction {
                id: "capability-1".to_string(),
                rationale: "request the action surface needed for the runtime test".to_string(),
                payload: crate::agent::AgentActionPayload::RequestCapability {
                    capability,
                    reason: format!("need {} actions for this runtime test", capability.as_str()),
                },
            }],
            final_turn: false,
        }),
    }
}

/// Runs the runtime model profile operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_model_profile(provider: &str, model: &str) -> ModelProfile {
    ModelProfile {
        provider: provider.to_string(),
        model: model.to_string(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options: std::collections::BTreeMap::new(),
        safety_tier: None,
    }
}

impl ModelProvider for RuntimeBatchProvider {
    /// Runs the provider id operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn provider_id(&self) -> &str {
        "runtime-batch"
    }

    /// Runs the send request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn send_request(
        &self,
        request: &crate::agent::ModelRequest,
    ) -> Result<crate::agent::ModelResponse> {
        if request.interaction_kind == crate::agent::ModelInteractionKind::CapabilityDecision
            && let Some(capability) = runtime_capability_for_response(&self.response)
        {
            return Ok(runtime_capability_response(
                self.provider_id(),
                request,
                capability,
            ));
        }
        Ok(self.response.clone())
    }
}

/// Builds a simple `say` response for runtime provider tests.
fn runtime_say_response(
    turn_id: &str,
    text: &str,
    final_turn: bool,
) -> crate::agent::ModelResponse {
    runtime_say_response_for_agent(turn_id, "agent-%1", text, final_turn)
}

/// Builds a simple `say` response for a selected runtime agent.
fn runtime_say_response_for_agent(
    turn_id: &str,
    agent_id: &str,
    text: &str,
    final_turn: bool,
) -> crate::agent::ModelResponse {
    crate::agent::ModelResponse {
        provider: "runtime-batch".to_string(),
        model: "test".to_string(),
        raw_text: text.to_string(),
        usage: Default::default(),
        quota_usage: Default::default(),
        action_batch: Some(crate::agent::MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "test action batch rationale".to_string(),
            turn_id: turn_id.to_string(),
            agent_id: agent_id.to_string(),
            actions: vec![crate::agent::AgentAction {
                id: "say-1".to_string(),
                rationale: "respond to the pane".to_string(),
                payload: crate::agent::AgentActionPayload::Say {
                    status: crate::agent::SayStatus::Final,
                    text: text.to_string(),
                    content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
                },
            }],
            final_turn,
        }),
    }
}

/// Builds a joined-spawn action fixture for scheduler and subagent tests.
fn runtime_spawn_agent_action(id: &str, task_prompt: &str) -> crate::agent::AgentAction {
    crate::agent::AgentAction {
        id: id.to_string(),
        rationale: "delegate a bounded child task".to_string(),
        payload: crate::agent::AgentActionPayload::SpawnAgent {
            role: "default".to_string(),
            placement: "new-window".to_string(),
            cooperation_mode: "explore-only".to_string(),
            read_scopes: Vec::new(),
            write_scopes: Vec::new(),
            task_prompt: task_prompt.to_string(),
        },
    }
}

/// Builds the request fixture used when a provider response was already
/// underway before a mid-turn steering prompt was submitted.
fn runtime_model_request_fixture(turn_id: &str) -> crate::agent::ModelRequest {
    runtime_model_request_fixture_for_agent(turn_id, "agent-%1")
}

/// Builds a request fixture for a selected runtime agent.
fn runtime_model_request_fixture_for_agent(
    turn_id: &str,
    agent_id: &str,
) -> crate::agent::ModelRequest {
    crate::agent::ModelRequest {
        provider: "runtime-batch".to_string(),
        model: "test".to_string(),
        reasoning_effort: None,
        prompt_cache_retention: None,
        max_output_tokens: None,
        turn_id: turn_id.to_string(),
        agent_id: agent_id.to_string(),
        available_mcp_tools: Vec::new(),
        interaction_kind: crate::agent::ModelInteractionKind::ActionExecution,
        allowed_actions: crate::agent::AllowedActionSet::capability_decision(),
        messages: vec![crate::agent::ModelMessage {
            role: crate::agent::ModelMessageRole::User,
            source: ContextSourceKind::UserInstruction,
            content: "initial request".to_string(),
        }],
    }
}

/// Carries Runtime Recording Provider state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
struct RuntimeRecordingProvider {
    /// Stores the provider value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    provider: &'static str,
    /// Stores the response value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    response: crate::agent::ModelResponse,
    /// Stores the last request value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    last_request: RefCell<Option<crate::agent::ModelRequest>>,
}

impl ModelProvider for RuntimeRecordingProvider {
    /// Runs the provider id operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn provider_id(&self) -> &str {
        self.provider
    }

    /// Runs the send request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn send_request(
        &self,
        request: &crate::agent::ModelRequest,
    ) -> Result<crate::agent::ModelResponse> {
        *self.last_request.borrow_mut() = Some(request.clone());
        if request.interaction_kind == crate::agent::ModelInteractionKind::CapabilityDecision
            && let Some(capability) = runtime_capability_for_response(&self.response)
        {
            return Ok(runtime_capability_response(
                self.provider_id(),
                request,
                capability,
            ));
        }
        Ok(self.response.clone())
    }
}

/// Fails the first provider request with a context-limit error and succeeds on
/// the retry after runtime recovery has reduced active-turn context.
struct RuntimeContextLimitThenSuccessProvider {
    /// Requests observed by the test provider.
    requests: RefCell<Vec<crate::agent::ModelRequest>>,
}

impl ModelProvider for RuntimeContextLimitThenSuccessProvider {
    /// Returns the provider id used by the context-limit recovery test.
    fn provider_id(&self) -> &str {
        "runtime-batch"
    }

    /// Returns one context-limit error, then a successful completion response.
    fn send_request(
        &self,
        request: &crate::agent::ModelRequest,
    ) -> Result<crate::agent::ModelResponse> {
        let mut requests = self.requests.borrow_mut();
        requests.push(request.clone());
        if requests.len() == 1 {
            return Err(MezError::invalid_state(
                "OpenAI Responses API returned status 400: This model's maximum context length is 128000 tokens. However, your messages resulted in 130000 tokens. Please reduce the length of the messages.",
            )
            .with_provider_failure_json(
                r#"{"status_code":400,"error":{"message":"This model's maximum context length is 128000 tokens. However, your messages resulted in 130000 tokens. Please reduce the length of the messages.","type":"invalid_request_error","code":"context_length_exceeded"}}"#,
            ));
        }
        Ok(crate::agent::ModelResponse {
            provider: self.provider_id().to_string(),
            model: request.model.clone(),
            raw_text: "done after compaction".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(runtime_complete_batch(request.turn_id.clone())),
        })
    }
}

/// Fails the first provider request with context-window wording and succeeds on
/// the retry after runtime recovery has reduced active-turn context.
struct RuntimeContextWindowErrorProvider {
    /// Requests observed by the test provider.
    requests: RefCell<Vec<crate::agent::ModelRequest>>,
}

impl ModelProvider for RuntimeContextWindowErrorProvider {
    /// Returns the provider id used by the context-window recovery test.
    fn provider_id(&self) -> &str {
        "runtime-batch"
    }

    /// Returns one context-window error, then a successful completion response.
    fn send_request(
        &self,
        request: &crate::agent::ModelRequest,
    ) -> Result<crate::agent::ModelResponse> {
        let mut requests = self.requests.borrow_mut();
        requests.push(request.clone());
        if requests.len() == 1 {
            return Err(MezError::invalid_state(
                "Your input exceeds the context window of this model. Please adjust your input and try again.",
            ));
        }
        Ok(crate::agent::ModelResponse {
            provider: self.provider_id().to_string(),
            model: request.model.clone(),
            raw_text: "done after compaction".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(runtime_complete_batch(request.turn_id.clone())),
        })
    }
}

/// Fails the first provider request with an output-limit incomplete response
/// and succeeds after runtime recovery adds compact-output retry guidance.
struct RuntimeOutputLimitThenSuccessProvider {
    /// Requests observed by the test provider.
    requests: RefCell<Vec<crate::agent::ModelRequest>>,
}

impl ModelProvider for RuntimeOutputLimitThenSuccessProvider {
    /// Returns the provider id used by the output-limit recovery test.
    fn provider_id(&self) -> &str {
        "runtime-batch"
    }

    /// Returns one output-limit error, then a successful completion response.
    fn send_request(
        &self,
        request: &crate::agent::ModelRequest,
    ) -> Result<crate::agent::ModelResponse> {
        let mut requests = self.requests.borrow_mut();
        requests.push(request.clone());
        if requests.len() == 1 {
            return Err(MezError::invalid_state(
                "OpenAI stream returned an incomplete response: max_output_tokens",
            )
            .with_provider_failure_json(
                r#"{"incomplete_details":{"reason":"max_output_tokens"}}"#,
            ));
        }
        Ok(crate::agent::ModelResponse {
            provider: self.provider_id().to_string(),
            model: request.model.clone(),
            raw_text: "done after compact retry".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(runtime_complete_batch(request.turn_id.clone())),
        })
    }
}

/// Records auto-sizing and normal provider requests while returning distinct
/// responses for the internal router and the user-facing turn.
struct RuntimeAutoSizingProvider {
    /// Stores every request sent through the provider.
    requests: RefCell<Vec<crate::agent::ModelRequest>>,
}

impl ModelProvider for RuntimeAutoSizingProvider {
    /// Returns the configured provider id used by the runtime test config.
    fn provider_id(&self) -> &str {
        "runtime-batch"
    }

    /// Returns a structured router decision for auto-sizing requests and a
    /// simple `say` response for the selected model request.
    fn send_request(
        &self,
        request: &crate::agent::ModelRequest,
    ) -> Result<crate::agent::ModelResponse> {
        self.requests.borrow_mut().push(request.clone());
        if request.interaction_kind == crate::agent::ModelInteractionKind::AutoSizing {
            return Ok(crate::agent::ModelResponse {
                provider: self.provider_id().to_string(),
                model: request.model.clone(),
                raw_text: r#"{"version":1,"size":"large","reasoning_effort":"high","confidence":0.92,"rationale":"multi-file feature work"}"#.to_string(),
                usage: Default::default(),
                quota_usage: Default::default(),
                action_batch: None,
            });
        }
        Ok(runtime_say_response(
            &request.turn_id,
            "auto-sized response",
            true,
        ))
    }
}

/// Runs the runtime mcp fixture script operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_mcp_fixture_script(tool_error: bool) -> String {
    let tool_response = if tool_error {
        r#"{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"tool denied"}],"structuredContent":{"status":"denied"},"isError":true}}"#
    } else {
        r#"{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"hello from mcp"}],"structuredContent":{"status":"ok"},"isError":false}}"#
    };
    format!(
        r#"while IFS= read -r line; do
case "$line" in
  *initialize*)
printf '%s\n' '{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"2025-11-25","capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"fixture","version":"1.0.0"}}}}}}'
;;
  *notifications/initialized*)
;;
  *tools/list*)
printf '%s\n' '{{"jsonrpc":"2.0","id":2,"result":{{"tools":[{{"name":"echo","description":"Echo a message","inputSchema":{{"type":"object","properties":{{"message":{{"type":"string"}}}}}}}}]}}}}'
;;
  *tools/call*)
printf '%s\n' '{}'
;;
esac
done"#,
        tool_response
    )
}

/// Runs the toml string operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn toml_string(value: &str) -> String {
    serde_json::to_string(value).unwrap()
}

/// Runs the poll until exit operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn poll_until_exit(service: &mut RuntimeSessionService) -> Vec<PaneExitUpdate> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let activity_sequences = tracked_pane_activity_sequences(service);
        let updates = service.poll_pane_processes().unwrap();
        if !updates.is_empty() {
            return updates;
        }
        wait_for_any_tracked_pane_activity_after(
            service,
            activity_sequences,
            Duration::from_millis(10),
        );
        thread::yield_now();
    }
    panic!("pane process did not exit before test timeout");
}

/// Runs the poll until turn state operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn poll_until_turn_state(
    service: &mut RuntimeSessionService,
    turn_id: &str,
    expected_state: AgentTurnState,
) {
    for _ in 0..50 {
        let state = service
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .map(|turn| turn.state);
        if state == Some(expected_state) {
            return;
        }
        let _ = service.poll_pane_outputs(4096).unwrap();
        wait_for_pane_process_activity(service, "%1", Duration::from_millis(10));
    }
    panic!("agent turn did not reach expected state before test timeout");
}

/// Runs the poll until action result context contains operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn poll_until_action_result_context_contains(
    service: &mut RuntimeSessionService,
    turn_id: &str,
    needle: &str,
) -> String {
    for _ in 0..50 {
        let observed = service
            .agent_turn_contexts
            .get(turn_id)
            .and_then(|context| {
                context
                    .blocks
                    .iter()
                    .find(|block| {
                        block.source == ContextSourceKind::ActionResult
                            && block.content.contains(needle)
                    })
                    .map(|block| block.content.clone())
            });
        if let Some(content) = observed {
            return content;
        }
        let _ = service.poll_pane_outputs(4096).unwrap();
        wait_for_pane_process_activity(service, "%1", Duration::from_millis(10));
    }
    let context = service
        .agent_turn_contexts
        .get(turn_id)
        .map(|context| {
            context
                .blocks
                .iter()
                .map(|block| format!("{:?}: {}", block.source, block.content))
                .collect::<Vec<_>>()
                .join("\n---\n")
        })
        .unwrap_or_else(|| "<missing context>".to_string());
    let pane_text = service
        .pane_screen("%1")
        .map(|screen| screen.normal_content_lines().join("\n"))
        .unwrap_or_else(|| "<missing pane>".to_string());
    panic!(
        "agent action result context did not arrive before test timeout; context={context}\npane={pane_text}"
    );
}

/// Runs the wait for pane process activity operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn wait_for_pane_process_activity(
    service: &RuntimeSessionService,
    pane_id: &str,
    timeout: Duration,
) {
    let Some(activity_sequence) = service.pane_processes().output_activity_sequence(pane_id) else {
        let _ = timeout;
        return;
    };
    wait_for_pane_process_activity_after(service, pane_id, Some(activity_sequence), timeout);
}

/// Runs the wait for pane process activity after operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn wait_for_pane_process_activity_after(
    service: &RuntimeSessionService,
    pane_id: &str,
    activity_sequence: Option<u64>,
    timeout: Duration,
) {
    let Some(activity_sequence) = activity_sequence else {
        let _ = timeout;
        return;
    };
    let _ = service.pane_processes().wait_for_output_activity_after(
        pane_id,
        activity_sequence,
        timeout,
    );
}

/// Runs the tracked pane activity sequences operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn tracked_pane_activity_sequences(service: &RuntimeSessionService) -> Vec<(String, u64)> {
    service
        .pane_processes()
        .tracked_pane_ids()
        .into_iter()
        .filter_map(|pane_id| {
            service
                .pane_processes()
                .output_activity_sequence(&pane_id)
                .map(|sequence| (pane_id, sequence))
        })
        .collect()
}

/// Runs the wait for any tracked pane activity after operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn wait_for_any_tracked_pane_activity_after(
    service: &RuntimeSessionService,
    sequences: Vec<(String, u64)>,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    for (pane_id, sequence) in sequences {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        if service
            .pane_processes()
            .wait_for_output_activity_after(&pane_id, sequence, remaining)
            .unwrap_or(false)
        {
            return;
        }
    }
}

/// Verifies default socket directory prefers mez tmpdir.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn default_socket_directory_prefers_mez_tmpdir() {
    let env = RuntimeEnv {
        mez_tmpdir: Some(OsString::from("/run/user/custom")),
        xdg_runtime_dir: Some(OsString::from("/run/user/1000")),
        uid: 1000,
    };

    let directory = default_socket_directory(&env).unwrap();

    assert_eq!(directory.source, SocketDirectorySource::MezTmpdir);
    assert_eq!(directory.path, PathBuf::from("/run/user/custom/mez-1000"));
}

/// Verifies default socket directory uses xdg runtime dir before tmp.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn default_socket_directory_uses_xdg_runtime_dir_before_tmp() {
    let env = RuntimeEnv {
        mez_tmpdir: None,
        xdg_runtime_dir: Some(OsString::from("/run/user/1000")),
        uid: 1000,
    };

    let directory = default_socket_directory(&env).unwrap();

    assert_eq!(directory.source, SocketDirectorySource::XdgRuntimeDir);
    assert_eq!(directory.path, PathBuf::from("/run/user/1000/mez"));
}

/// Verifies default socket directory rejects relative env paths.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn default_socket_directory_rejects_relative_env_paths() {
    let env = RuntimeEnv {
        mez_tmpdir: Some(OsString::from("relative")),
        xdg_runtime_dir: None,
        uid: 1000,
    };

    let error = default_socket_directory(&env).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies ensure private socket directory creates mode 0700 directory.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn ensure_private_socket_directory_creates_mode_0700_directory() {
    let root = std::env::temp_dir().join(format!("mez-runtime-test-create-{}", std::process::id()));
    let path = root.join("socket");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir(&root).unwrap();

    ensure_private_socket_directory(&path, effective_uid()).unwrap();
    let metadata = fs::metadata(&path).unwrap();

    assert!(metadata.is_dir());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o700);

    let _ = fs::remove_dir_all(&root);
}

/// Verifies ensure private socket directory rejects group permissions.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn ensure_private_socket_directory_rejects_group_permissions() {
    let root = std::env::temp_dir().join(format!("mez-runtime-test-mode-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();

    let error = ensure_private_socket_directory(&root, effective_uid()).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);

    let _ = fs::remove_dir_all(&root);
}

/// Verifies socket name must be single component.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn socket_name_must_be_single_component() {
    let error = socket_path_for_name(Path::new("/tmp/mez-1000"), "../bad").unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies auxiliary socket paths are derived from control socket name.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn auxiliary_socket_paths_are_derived_from_control_socket_name() {
    let control = Path::new("/tmp/mez-1000/default.sock");

    let message =
        auxiliary_socket_path_for_control_socket(control, AuxiliarySocketKind::Message).unwrap();
    let event =
        auxiliary_socket_path_for_control_socket(control, AuxiliarySocketKind::Event).unwrap();

    assert_eq!(message, PathBuf::from("/tmp/mez-1000/default.message.sock"));
    assert_eq!(event, PathBuf::from("/tmp/mez-1000/default.event.sock"));
}

/// Verifies auxiliary socket paths preserve nonstandard control socket names.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn auxiliary_socket_paths_preserve_nonstandard_control_socket_names() {
    let control = Path::new("/tmp/mez-1000/control");

    let message =
        auxiliary_socket_path_for_control_socket(control, AuxiliarySocketKind::Message).unwrap();

    assert_eq!(message, PathBuf::from("/tmp/mez-1000/control.message.sock"));
}

/// Verifies unix peer uid authorization rejects uid mismatch.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn unix_peer_uid_authorization_rejects_uid_mismatch() {
    let error = authorize_unix_peer_uid(1001, 1000).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
}

/// Verifies unix peer authorization accepts same user stream.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn unix_peer_authorization_accepts_same_user_stream() {
    let (_client, server) = UnixStream::pair().unwrap();

    authorize_unix_peer(&server, effective_uid()).unwrap();
}

/// Verifies stale socket cleanup removes only unserved runtime sockets.
///
/// This regression scenario protects startup cleanup from deleting live Mez
/// endpoints while still removing refused socket files left behind by crashed
/// processes.
#[test]
fn prune_stale_socket_files_removes_refused_socket_and_preserves_live_socket() {
    let root = std::env::temp_dir().join(format!(
        "mez-runtime-test-stale-sockets-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    ensure_private_socket_directory(&root, effective_uid()).unwrap();
    let stale = root.join("stale.sock");
    let live = root.join("live.sock");
    let non_socket = root.join("not-a-socket.sock");

    let stale_listener = std::os::unix::net::UnixListener::bind(&stale).unwrap();
    drop(stale_listener);
    let _live_listener = bind_control_socket(&live, effective_uid()).unwrap();
    fs::write(&non_socket, "leave this alone").unwrap();

    let removed = prune_stale_socket_files_in_directory(&root, effective_uid()).unwrap();

    assert_eq!(removed, 1);
    assert!(!stale.exists());
    assert!(live.exists());
    assert!(non_socket.exists());

    let _ = fs::remove_dir_all(&root);
}

/// Verifies pane environment places socket path first.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn pane_environment_places_socket_path_first() {
    let mut ids = IdFactory::default();
    let session = ids.session();
    let window = ids.window();
    let pane = ids.pane();

    let env = pane_environment(
        Path::new("/tmp/mez-1000/default.sock"),
        &session,
        &window,
        &pane,
    )
    .unwrap();

    let separator = MEZ_ENV_FIELD_SEPARATOR.to_string();
    let fields = env.mez.split(MEZ_ENV_FIELD_SEPARATOR).collect::<Vec<_>>();
    assert_eq!(fields[0], "/tmp/mez-1000/default.sock");
    assert_eq!(fields[1], format!("session={session}"));
    assert!(env.mez.contains(&separator));
    assert_eq!(env.session, session.to_string());
    assert_eq!(env.window, window.to_string());
    assert_eq!(env.pane, pane.to_string());
    assert_eq!(env.term, DEFAULT_PANE_TERM);
}

/// Verifies pane environment accepts explicit term selection.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn pane_environment_accepts_explicit_term_selection() {
    let mut ids = IdFactory::default();
    let env = pane_environment_with_term(
        Path::new("/tmp/mez-1000/default.sock"),
        &ids.session(),
        &ids.window(),
        &ids.pane(),
        "screen-256color",
    )
    .unwrap();

    assert_eq!(env.term, "screen-256color");
}

/// Verifies runtime service tracks attach detach lifecycle.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_service_tracks_attach_detach_lifecycle() {
    let mut service = test_runtime_service();

    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    assert_eq!(service.lifecycle_state(), RuntimeLifecycleState::Running);
    assert_eq!(service.last_attach_at_unix_seconds(), Some(120));
    assert_eq!(service.session().primary_client_id(), Some(&primary));
    assert_eq!(
        service.session().authoritative_size,
        Size::new(100, 40).unwrap()
    );

    service
        .detach_primary(&primary, Size::new(132, 43).unwrap())
        .unwrap();
    assert_eq!(service.lifecycle_state(), RuntimeLifecycleState::Detached);
    assert!(service.session().primary_client_id().is_none());
    assert_eq!(
        service.session().authoritative_size,
        Size::new(132, 43).unwrap()
    );

    let reattached = service
        .attach_primary("reattach", true, Size::new(90, 30).unwrap(), 180)
        .unwrap();
    assert_eq!(service.lifecycle_state(), RuntimeLifecycleState::Running);
    assert_eq!(service.session().primary_client_id(), Some(&reattached));
    assert_eq!(service.last_attach_at_unix_seconds(), Some(180));
    assert_ne!(primary, reattached);
}

/// Verifies that the initial primary attach applies the attached terminal size
/// to existing window geometry. The first pane is created at bootstrap size
/// before a client is attached, so agent prompt rendering must depend on the
/// post-attach resize path instead of stale default geometry.
#[test]
fn runtime_primary_attach_resizes_initial_window_for_agent_prompt() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(120, 40).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    assert_eq!(
        service.session().active_window().unwrap().size,
        Size::new(120, 40).unwrap()
    );

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(120, 40).unwrap(),
            &config,
        )
        .unwrap()
        .unwrap();
    let region = view.agent_prompt_region.unwrap();

    assert_eq!(view.lines.len(), 40);
    assert_eq!(view.authoritative_size, Size::new(120, 40).unwrap());
    assert_eq!(region.columns, 120);
    assert_eq!(region.rows, 38);
    assert!(
        view.cursor_row >= 38,
        "agent prompt cursor should render at attached terminal bottom: {view:?}"
    );
}

/// Verifies runtime control agent shell state persists in service.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_control_agent_shell_state_persists_in_service() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();

    let show = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"show","method":"agent/shell/show","params":{"target":{"pane_id":"%1"},"idempotency_key":"show-agent"}}"#,
        &primary,
    );
    assert!(show.contains(r#""visible":true"#), "{show}");
    let conversation_id = service
        .agent_shell_store
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    assert!(
        show.contains(&format!(r#""conversation_id":"{conversation_id}""#)),
        "{show}"
    );

    let list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"list","method":"agent/list","params":{}}"#,
        &primary,
    );
    assert!(list.contains(r#""pane_id":"%1""#), "{list}");
    assert!(list.contains(r#""visible":true"#), "{list}");
    assert!(
        list.contains(&format!(r#""conversation_id":"{conversation_id}""#)),
        "{list}"
    );

    let targeted_list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"targeted-list","method":"agent/list","params":{"target":{"default":true}}}"#,
        &primary,
    );
    assert!(
        targeted_list.contains(&format!(r#""conversation_id":"{conversation_id}""#)),
        "{targeted_list}"
    );

    let missing_session_list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"missing-list","method":"agent/list","params":{"target":{"session_id":"missing"}}}"#,
        &primary,
    );
    assert!(
        missing_session_list.contains(r#""mezzanine_code":"not_found""#),
        "{missing_session_list}"
    );

    let hide = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"hide","method":"agent/shell/hide","params":{"target":{"pane_id":"%1"},"idempotency_key":"hide-agent"}}"#,
        &primary,
    );
    assert!(hide.contains(r#""visible":false"#), "{hide}");

    let relist = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"relist","method":"agent/list","params":{}}"#,
        &primary,
    );
    assert!(relist.contains(r#""visible":false"#), "{relist}");
    assert!(
        relist.contains(&format!(r#""conversation_id":"{conversation_id}""#)),
        "{relist}"
    );
}

/// Verifies that the JSON-RPC agent shell visibility endpoints apply the same
/// live pane subshell side effects as the terminal `agent-shell` command. This
/// protects clients that enter agent mode through control APIs from bypassing
/// the parent-shell isolation boundary.
#[test]
fn runtime_control_agent_shell_visibility_enters_and_exits_pane_subshell() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let mut process = service
        .take_running_pane_process_for_async_owner(&pane_id)
        .unwrap();
    service
        .pane_screens
        .get_mut(&pane_id)
        .unwrap()
        .feed(b"control show history\ncontrol show visible text");

    let show = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"show","method":"agent/shell/show","params":{"target":{"pane_id":"%1"},"idempotency_key":"show-agent"}}"#,
        &primary,
    );
    assert!(show.contains(r#""visible":true"#), "{show}");
    let after_show_screen = service.pane_screen(&pane_id).unwrap();
    assert!(
        !after_show_screen
            .visible_lines()
            .join("\n")
            .contains("control show visible text")
    );
    assert!(
        after_show_screen
            .normal_content_lines()
            .join("\n")
            .contains("control show visible text")
    );
    let enter_input = service.drain_deferred_pane_inputs();
    assert_eq!(enter_input.len(), 1);
    assert_eq!(enter_input[0].pane_id, pane_id);
    assert!(service.agent_subshell_panes.contains(&pane_id));

    let hide = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"hide","method":"agent/shell/hide","params":{"target":{"pane_id":"%1"},"idempotency_key":"hide-agent"}}"#,
        &primary,
    );
    assert!(hide.contains(r#""visible":false"#), "{hide}");
    let exit_input = service.drain_deferred_pane_inputs();
    assert_eq!(exit_input.len(), 1);
    assert_eq!(exit_input[0].pane_id, pane_id);
    assert_eq!(exit_input[0].bytes, b"\x04");
    assert!(!service.agent_subshell_panes.contains(&pane_id));
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies runtime owns agent turn start and finish lifecycle.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_owns_agent_turn_start_and_finish_lifecycle() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Trace)
        .unwrap();

    let started = service
        .start_agent_turn(crate::agent::AgentTurnRecord {
            turn_id: "turn-1".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            trigger: crate::agent::AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 200,
            policy_profile: "default".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            cooperation_mode: None,
            state: crate::agent::AgentTurnState::Queued,
        })
        .unwrap();
    assert_eq!(started.running_turn_id.as_deref(), Some("turn-1"));

    let agents = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agents","method":"agent/list","params":{}}"#,
        &primary,
    );
    assert!(agents.contains(r#""status":"running""#), "{agents}");
    assert!(agents.contains(r#""last_turn_id":"turn-1""#), "{agents}");

    let tasks = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"tasks","method":"agent/task/list","params":{"target":{"pane_id":"%1"}}}"#,
        &primary,
    );
    assert!(tasks.contains(r#""id":"turn-1""#), "{tasks}");
    assert!(tasks.contains(r#""state":"running""#), "{tasks}");

    let session_tasks = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"session-tasks","method":"agent/task/list","params":{"target":{"default":true}}}"#,
        &primary,
    );
    assert!(
        session_tasks.contains(r#""id":"turn-1""#),
        "{session_tasks}"
    );

    let conflicting_target_tasks = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"conflicting-tasks","method":"agent/task/list","params":{"target":{"agent_id":"agent-%1","pane_id":"%1"}}}"#,
        &primary,
    );
    assert!(
        conflicting_target_tasks.contains(r#""mezzanine_code":"invalid_params""#),
        "{conflicting_target_tasks}"
    );

    service.agent_shell_store_mut().request_exit("%1").unwrap();
    let finished = service
        .finish_agent_turn("%1", "turn-1", crate::agent::AgentTurnState::Completed)
        .unwrap();
    assert_eq!(finished.running_turn_id, None);
    assert_eq!(finished.visibility, AgentShellVisibility::Hidden);

    let completed_tasks = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"tasks2","method":"agent/task/list","params":{"target":{"pane_id":"%1"}}}"#,
        &primary,
    );
    assert!(
        completed_tasks.contains(r#""state":"completed""#),
        "{completed_tasks}"
    );
}

/// Verifies that runtime shell transaction markers are generated with fresh
/// entropy for every dispatch. Identical turn/action metadata must not produce
/// reusable marker tokens.
#[test]
fn runtime_marker_for_action_uses_fresh_entropy() {
    let turn = crate::agent::AgentTurnRecord {
        turn_id: "turn-1".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        trigger: crate::agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 200,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        cooperation_mode: None,
        state: crate::agent::AgentTurnState::Running,
    };

    let first = runtime_marker_for_action(&turn, "a1").unwrap();
    let second = runtime_marker_for_action(&turn, "a1").unwrap();

    assert_ne!(first.as_str(), second.as_str());
    assert!(first.as_str().len() >= 64);
    assert!(second.as_str().len() >= 64);
}

/// Verifies that runtime shell transaction observation stores bounded terminal
/// text and reports truncation once the observation cap is exceeded.
#[test]
fn runtime_shell_transaction_observation_is_bounded_and_truncated() {
    let mut service = test_runtime_service();
    service.running_shell_transactions.insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "a1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "printf marker\n".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );
    let output = vec![b'x'; 70_000];

    service.record_running_shell_transaction_output("%1", &output);

    let transaction = service.running_shell_transactions.get("marker-1").unwrap();
    assert_eq!(transaction.observed_output_bytes, 70_001);
    assert_eq!(transaction.observed_output_preview.len(), 65_536);
    assert!(transaction.observed_output_truncated);
}

/// Verifies async pane write completions are retained in the hidden trace log.
///
/// A shell transaction being recorded as running is not enough evidence that
/// the async pane worker actually wrote its wrapper bytes to the PTY. The trace
/// log should include write progress so file-action hangs can be diagnosed at
/// the delivery boundary instead of only at the transaction marker boundary.
#[test]
fn runtime_pane_input_written_traces_active_shell_transaction() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.running_shell_transactions.insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "create-1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "cat > note.txt".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    assert!(service.apply_pane_input_written_event("%1", 4096).unwrap());

    let trace = service.agent_pane_trace_log_text("%1").unwrap();
    assert!(trace.contains("pane input written bytes: 4096"), "{trace}");
    assert!(trace.contains("marker: marker-1"), "{trace}");
    assert!(trace.contains("action: create-1"), "{trace}");
}

/// Verifies model-visible shell transaction observation strips prompt styling
/// and Mezzanine wrapper echo while preserving command output.
///
/// Styled shell prompts can be much larger than the useful output for common
/// commands like `ls`. The agent context must contain the file names rather
/// than consuming its bounded observation budget with PS1 repaint bytes.
#[test]
fn runtime_shell_transaction_observation_strips_prompt_and_wrapper_noise() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.running_shell_transactions.insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "a1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "ls".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let filtered = service.visible_pane_output_bytes(
        "%1",
        b"\x1b[38;2;214;93;14m\xee\x82\xb6\x1b[48;2;214;93;14m\xef\xb0\x95 neil \x1b[0m\r\n\x1b[38;2;214;93;14m\xee\x82\xb6\x1b[48;2;214;93;14m\xef\xb0\x95 neil \x1b[0m MEZ_MARKER_TOKEN='abc'\r\n\x1b[38;2;214;93;14m\xee\x82\xb6\x1b[48;2;214;93;14m\xef\xb0\x95 neil \x1b[0m MEZ_TURN='turn-1'\r\n\x1b[1;38;2;152;151;26m\xef\x90\xb2\x1b[0m ls\r\nCargo.toml\r\nsrc\r\n\x1b]133;D;0;mez_marker=abc;mez_turn=turn-1;mez_agent=agent-%1;mez_pane=%1\x1b\\",
    );
    service.record_running_shell_transaction_output("%1", &filtered);

    let transaction = service.running_shell_transactions.get("marker-1").unwrap();
    assert!(
        transaction.observed_output_preview.contains("src"),
        "{}",
        transaction.observed_output_preview
    );
    assert!(
        !transaction.observed_output_preview.contains("MEZ_"),
        "{}",
        transaction.observed_output_preview
    );
    assert!(
        !transaction.observed_output_preview.contains("neil"),
        "{}",
        transaction.observed_output_preview
    );
    assert!(transaction.observed_output_bytes > 0);
    assert!(!transaction.observed_output_truncated);
}

/// Verifies that transaction observation hides echoed Mezzanine-owned wrapper
/// lines for active shell transactions while preserving actual command output and the
/// OSC transaction markers that the runtime needs to observe completion.
#[test]
fn runtime_shell_transaction_wrapper_echo_is_hidden_by_default() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.running_shell_transactions.insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "a1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "ls".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let visible = service.visible_pane_output_bytes(
        "%1",
        b"MEZ_RESTORE_ERREXIT=0; case $- in *e*) MEZ_RESTORE_ERREXIT=1; set +e;; esac; MEZ_HISTORY_RESTORE=0; case \"$(set -o 2>/dev/null | awk '$1==\"history\"{print $2; exit}')\" in on) MEZ_HISTORY_RESTORE=1; set +o history 2>/dev/null || :; history -d $((HISTCMD-1)) 2>/dev/null || :;; esac\r\nMEZ_HISTORY_HISTFILE_WAS_SET=0\r\nHISTFILE=/dev/null\r\nMEZ_MARKER_TOKEN='abc'\r\nMEZ_TURN='turn-1'\r\nls\r\nprintf '\\033]133;D;%s;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\\033\\\\'\r\n\"$MEZ_STATUS\" \"$MEZ_MARKER_TOKEN\" \"$MEZ_TURN\" \"$MEZ_AGENT\" \"$MEZ_PANE\"\r\nif [ \"$MEZ_HISTORY_HISTFILE_WAS_SET\" = 1 ]; then HISTFILE=$MEZ_HISTORY_HISTFILE_SAVED; else unset HISTFILE; fi\r\nMEZ_RESTORE_HISTORY_NOW=$MEZ_HISTORY_RESTORE\r\nunset MEZ_MARKER_TOKEN MEZ_TURN MEZ_AGENT MEZ_PANE MEZ_STATUS\r\nif [ \"$MEZ_RESTORE_HISTORY_NOW\" = 1 ]; then set -o history 2>/dev/null || :; fi; if [ \"$MEZ_RESTORE_ERREXIT_NOW\" = 1 ]; then set -e; fi; unset MEZ_RESTORE_HISTORY_NOW MEZ_RESTORE_ERREXIT_NOW\r\n>\r\nfile-a\n\x1b]133;D;0;mez_marker=abc;mez_turn=turn-1;mez_agent=agent-%1;mez_pane=%1\x1b\\",
    );
    let visible_text = String::from_utf8_lossy(&visible);

    assert!(!visible_text.contains("MEZ_MARKER_TOKEN"), "{visible_text}");
    assert!(!visible_text.contains("MEZ_TURN"), "{visible_text}");
    assert!(!visible_text.contains("MEZ_STATUS"), "{visible_text}");
    assert!(
        !visible_text.contains("MEZ_RESTORE_ERREXIT"),
        "{visible_text}"
    );
    assert!(!visible_text.contains("MEZ_HISTORY"), "{visible_text}");
    assert!(!visible_text.contains("HISTFILE"), "{visible_text}");
    assert!(!visible_text.contains("history -d"), "{visible_text}");
    assert!(!visible_text.contains("case $-"), "{visible_text}");
    assert!(!visible_text.contains("\nls"), "{visible_text}");
    assert!(visible_text.contains("file-a"), "{visible_text}");
    assert!(visible.contains(&0x1b));
}

/// Verifies that runtime transaction marker parsing is stateful per pane rather
/// than per PTY read chunk. Real PTY reads can split the OSC 133 transaction end
/// marker across chunks; losing that fragment leaves the agent shell action in a
/// permanent running state even though the command has already exited.
#[test]
fn runtime_shell_transaction_osc_parser_preserves_fragmented_markers() {
    let mut service = test_runtime_service();
    let size = Size::new(80, 24).unwrap();

    let (first_events, _) = service
        .terminal_osc_events_for_pane_bytes(
            "%1",
            size,
            b"file-a\n\x1b]133;D;0;mez_marker=marker-1;mez_turn=turn-1;mez_agent=agent-%1;mez",
        )
        .unwrap();
    let (second_events, _) = service
        .terminal_osc_events_for_pane_bytes("%1", size, b"_pane=%1\x1b\\")
        .unwrap();

    assert_eq!(first_events, Vec::<TerminalOscEvent>::new());
    assert_eq!(
        second_events,
        vec![TerminalOscEvent::ShellTransactionEnd {
            marker: "marker-1".to_string(),
            turn_id: "turn-1".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            exit_code: 0,
        }]
    );
}

/// Verifies that hidden agent-shell output uses a bounded Mezzanine-marker
/// scanner instead of feeding arbitrary command output into a terminal screen.
/// Long shell-command bodies can contain megabytes of plain text or embedded
/// terminal escapes; those bytes are model data and must not monopolize the UI
/// parser while the runtime waits for its own transaction marker.
#[test]
fn runtime_hidden_agent_shell_osc_parser_skips_large_command_bodies() {
    let mut service = test_runtime_service();
    let size = Size::new(80, 24).unwrap();
    service.pane_transaction_osc_screens.remove("%1");
    service.running_shell_transactions.insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "read-1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "head -c 1048577 -- src/lib.rs".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );
    let mut output = vec![b'x'; 2 * 1024 * 1024];
    output.extend_from_slice(b"\x1b[?1049hignored alternate-screen bytes from file content\n");
    output.extend_from_slice(
        b"\x1b]133;D;0;mez_marker=marker-1;mez_turn=turn-1;mez_agent=agent-%1;mez_pane=%1\x1b\\",
    );

    let (events, alternate_active) = service
        .terminal_osc_events_for_pane_bytes("%1", size, &output)
        .unwrap();

    assert!(!alternate_active);
    assert_eq!(
        events,
        vec![TerminalOscEvent::ShellTransactionEnd {
            marker: "marker-1".to_string(),
            turn_id: "turn-1".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            exit_code: 0,
        }]
    );
    assert!(
        !service.pane_transaction_osc_screens.contains_key("%1"),
        "hidden agent shell output should not allocate or feed the full terminal parser"
    );
    assert!(!service.pane_transaction_osc_pending.contains_key("%1"));
}

/// Verifies the bounded hidden-output marker scanner still preserves
/// transaction markers split across PTY reads. This keeps the lightweight path
/// compatible with the real-world fragmentation that the full terminal parser
/// handled before hidden agent-shell output was bypassed.
#[test]
fn runtime_hidden_agent_shell_osc_parser_preserves_fragmented_markers() {
    let mut service = test_runtime_service();
    let size = Size::new(80, 24).unwrap();
    service.pane_transaction_osc_screens.remove("%1");
    service.running_shell_transactions.insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "read-1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "head -c 1048577 -- src/lib.rs".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let (first_events, _) = service
        .terminal_osc_events_for_pane_bytes(
            "%1",
            size,
            b"large body\n\x1b]133;D;0;mez_marker=marker-1;mez_turn=turn-1;mez_agent=agent-%1;mez",
        )
        .unwrap();
    let (second_events, _) = service
        .terminal_osc_events_for_pane_bytes("%1", size, b"_pane=%1\x1b\\")
        .unwrap();

    assert_eq!(first_events, Vec::<TerminalOscEvent>::new());
    assert_eq!(
        second_events,
        vec![TerminalOscEvent::ShellTransactionEnd {
            marker: "marker-1".to_string(),
            turn_id: "turn-1".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            exit_code: 0,
        }]
    );
    assert!(!service.pane_transaction_osc_pending.contains_key("%1"));
}

/// Verifies that terminal-wrapped fragments of Mezzanine wrapper echo are hidden
/// even when a PTY splits the original wrapper line before the filter receives a
/// newline. The visible pane must contain command output, not implementation
/// variable fragments.
#[test]
fn runtime_shell_transaction_wrapper_echo_fragments_are_hidden_by_default() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.running_shell_transactions.insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "a1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "printf 'file-a\\n'".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let visible = service.visible_pane_output_bytes(
        "%1",
        b"Z_TURN\" \"$MEZ_AGENT\" \"$MEZ_PANE\"\r\nEZ_PANE MEZ_STATUS\r\nfile-a\n",
    );
    let visible_text = String::from_utf8_lossy(&visible);

    assert!(!visible_text.contains("Z_TURN"), "{visible_text}");
    assert!(!visible_text.contains("MEZ_AGENT"), "{visible_text}");
    assert!(!visible_text.contains("MEZ_STATUS"), "{visible_text}");
    assert!(visible_text.contains("file-a"), "{visible_text}");
}

/// Verifies that `/log-level trace` is the high-verbosity escape hatch for raw
/// shell-wrapper diagnosis. When enabled, the runtime leaves echoed wrapper
/// traffic untouched so developers can inspect exactly what was written to and
/// echoed by the pane PTY.
#[test]
fn runtime_shell_transaction_wrapper_echo_is_visible_with_trace_enabled() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Trace)
        .unwrap();
    service.running_shell_transactions.insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "a1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "ls".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let visible =
        service.visible_pane_output_bytes("%1", b"MEZ_MARKER_TOKEN='abc'\r\nls\r\nfile-a\n");
    let visible_text = String::from_utf8_lossy(&visible);

    assert!(visible_text.contains("MEZ_MARKER_TOKEN"), "{visible_text}");
    assert!(visible_text.contains("ls"), "{visible_text}");
    assert!(visible_text.contains("file-a"), "{visible_text}");
}

/// Verifies that agent command output retained for transaction observation is
/// not rendered into the user pane by default. This keeps default agent turns
/// conversational while still preserving the bytes needed for command-result
/// context.
#[test]
fn runtime_agent_shell_transaction_output_is_hidden_from_pane_by_default() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.running_shell_transactions.insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "a1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "ls".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let rendered = service.renderable_pane_output_bytes("%1", b"file-a\n");

    assert!(rendered.is_empty());
}

/// Verifies mutating semantic actions log a compact execution line and a
/// colored diff in normal agent mode.
///
/// File-change actions run through generated pane shell transactions so they
/// affect the same local, remote, or container shell that the user is operating.
/// Normal mode should still show the resulting change as a readable diff
/// instead of Mezzanine's execution machinery.
#[test]
fn runtime_semantic_mutation_logs_colored_diff_in_normal_mode() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(160, 60).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let target_rel = format!(
        "target/mez-semantic-mutation-diff-{}-{unique}/note.txt",
        std::process::id()
    );
    let target = PathBuf::from(&target_rel);
    fs::create_dir_all(target.parent().unwrap()).unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-semantic-diff","input":"create a note"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap semantic response".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "patch-1".to_string(),
                    rationale: "create a file".to_string(),
                    payload: crate::agent::AgentActionPayload::ApplyPatch {
                        patch: format!(
                            "*** Begin Patch\n*** Add File: {target_rel}\n+alpha\n+beta\n*** End Patch"
                        ),
                        strip: None,
                    },
                }],
                final_turn: false,
            }),
        },
    };
    service.pending_agent_provider_tasks.remove("turn-1");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    let action_transaction = service
        .running_shell_transactions
        .values()
        .find(|transaction| {
            matches!(
                transaction.kind,
                RunningShellTransactionKind::AgentAction { ref action_id }
                    if action_id == "patch-1"
            )
        })
        .expect("apply_patch should dispatch through the pane shell");
    let timeout_ms = action_transaction.timeout_ms.unwrap();
    assert_eq!(timeout_ms, 30 * 1000);
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get("%1").unwrap();
    assert_eq!(pane_context.agent_status.as_deref(), Some("executing"));
    assert!(
        pane_context
            .agent_display_lines
            .iter()
            .any(|line| line.starts_with("executing (")),
        "{pane_context:?}"
    );
    let context =
        poll_until_action_result_context_contains(&mut service, "turn-1", "diff -- apply patch");
    assert!(context.contains("command: apply_patch"), "{context}");
    assert!(!context.contains("command: cat >"), "{context}");
    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let pane_text = styled_lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let collapsed_agent_wraps = pane_text.replace("\n▐ ", "");
    assert!(
        pane_text.contains("agent: apply patch: ") && collapsed_agent_wraps.contains(&target_rel),
        "{pane_text}"
    );
    assert_eq!(
        styled_lines
            .iter()
            .filter(|line| line.text.contains("agent: apply patch"))
            .count(),
        1,
        "{pane_text}"
    );
    assert!(collapsed_agent_wraps.contains("note.txt"), "{pane_text}");
    assert!(pane_text.contains("• Created"), "{pane_text}");
    assert!(pane_text.contains("(+2 -0)"), "{pane_text}");
    assert!(pane_text.contains("       1 +alpha"), "{pane_text}");
    assert!(!pane_text.contains("$ python3 - <<'MEZ_PY'"), "{pane_text}");
    assert!(!pane_text.contains("MEZ_MARKER_TOKEN"), "{pane_text}");
    assert!(!pane_text.contains("MEZ_COMMAND_"), "{pane_text}");
    assert!(
        !pane_text.contains("MEZ_RESTORE_NOUNSET_NOW"),
        "{pane_text}"
    );
    assert!(!pane_text.contains(""), "{pane_text}");
    assert!(!pane_text.contains("∙"), "{pane_text}");
    let action_line = styled_lines
        .iter()
        .find(|line| line.text.contains("agent: apply patch"))
        .unwrap();
    assert!(!action_line.style_spans.is_empty());
    let addition_line = styled_lines
        .iter()
        .find(|line| line.text.contains("       1 +alpha"))
        .unwrap();
    assert!(
        addition_line
            .style_spans
            .iter()
            .any(|span| span.rendition.bold),
        "{addition_line:?}"
    );
    fs::remove_dir_all(target.parent().unwrap()).unwrap();
    service.pane_processes_mut().terminate_all().unwrap();
    let _ = fs::remove_dir_all(target.parent().unwrap());
}

/// Verifies mixed `say` plus semantic file-mutation batches present the file
/// diff before the assistant summary.
///
/// Providers can emit a convenient final message in the same batch as a file
/// action. Normal mode should not show that prose before the runtime has
/// actually applied the file action and displayed its diff, otherwise users see
/// a completion claim followed by unrelated-looking edit logs.
#[test]
fn runtime_mixed_say_and_file_mutation_defers_say_until_after_diff() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(160, 60).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let target_rel = format!(
        "target/mez-semantic-mutation-deferred-say-{}-{unique}/note.txt",
        std::process::id()
    );
    let target = PathBuf::from(&target_rel);
    fs::create_dir_all(target.parent().unwrap()).unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-semantic-deferred-say","input":"create a note"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap semantic response".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![
                    crate::agent::AgentAction {
                        id: "say-1".to_string(),
                        rationale: String::new(),
                        payload: crate::agent::AgentActionPayload::Say {
                            status: crate::agent::SayStatus::Final,
                            text: "Created `note.txt`.".to_string(),
                            content_type: crate::agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE
                                .to_string(),
                        },
                    },
                    crate::agent::AgentAction {
                        id: "patch-1".to_string(),
                        rationale: "write a file".to_string(),
                        payload: crate::agent::AgentActionPayload::ApplyPatch {
                            patch: format!(
                                "*** Begin Patch\n*** Add File: {target_rel}\n+alpha\n+beta\n*** End Patch"
                            ),
                            strip: None,
                        },
                    },
                ],
                final_turn: true,
            }),
        },
    };
    service.pending_agent_provider_tasks.remove("turn-1");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results[1].status, ActionStatus::Running);
    assert!(
        service
            .running_shell_transactions
            .values()
            .any(|transaction| matches!(
                transaction.kind,
                RunningShellTransactionKind::AgentAction { ref action_id }
                    if action_id == "patch-1"
            )),
        "file actions should dispatch through pane shell transactions"
    );
    poll_until_turn_state(&mut service, "turn-1", AgentTurnState::Completed);

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let diff_index = pane_text.find("• Created").unwrap_or(usize::MAX);
    let say_index = pane_text.find("Created note.txt.").unwrap_or(usize::MAX);
    assert!(diff_index < say_index, "{pane_text}");
    assert!(pane_text.contains("Worked for"), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
    let _ = fs::remove_dir_all(target.parent().unwrap());
}

/// Verifies runtime-owned URL actions render a human-readable execution line
/// with themed gutter colors in normal mode.
///
/// URL actions do not pass through the pane shell, so they need their own
/// concise action line. Their result payload should remain out of normal mode
/// and be left to elevated logging and provider context.
#[test]
fn runtime_url_action_logs_single_action_line_in_normal_mode() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let action = crate::agent::AgentAction {
        id: "fetch-1".to_string(),
        rationale: String::new(),
        payload: crate::agent::AgentActionPayload::FetchUrl {
            url: "https://example.test/file.txt".to_string(),
            format: None,
            max_bytes: None,
        },
    };

    let emitted = service
        .append_agent_action_execution_text_to_terminal_buffer("%1", &action)
        .unwrap();
    assert!(emitted);

    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let pane_text = styled_lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(pane_text.contains("agent: fetch url: https://example.test/file.txt"));
    assert!(!pane_text.contains("line one"));
    assert!(!pane_text.contains("line two"));
    let action_line = styled_lines
        .iter()
        .find(|line| line.text.contains("agent: fetch url:"))
        .unwrap();
    let theme = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .ui_theme;
    let prefix_column = display_column_for_fragment(&action_line.text, "agent:");
    let action_column = display_column_for_fragment(&action_line.text, "fetch url");
    let argument_column = display_column_for_fragment(&action_line.text, "https://");
    let prefix_rendition = styled_line_rendition_at(action_line, prefix_column);
    let action_rendition = styled_line_rendition_at(action_line, action_column);
    let argument_rendition = styled_line_rendition_at(action_line, argument_column);
    assert_eq!(
        prefix_rendition.foreground,
        Some(theme.colors.agent_transcript_status.foreground)
    );
    assert!(prefix_rendition.dim);
    assert_eq!(
        action_rendition.foreground,
        Some(theme.colors.agent_transcript_command.foreground)
    );
    assert!(action_rendition.bold);
    assert_ne!(
        argument_rendition.foreground,
        Some(theme.colors.agent_transcript_command.foreground),
        "{action_line:?}"
    );
}

/// Verifies runtime-owned config changes render with the same stylized
/// normal-mode action line as other non-shell actions.
///
/// Config mutations do not go through the pane shell, but users still need a
/// compact action row that makes the operation and setting path visible without
/// dumping result payloads into the pane.
#[test]
fn runtime_config_change_action_logs_styled_action_line_in_normal_mode() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let action = crate::agent::AgentAction {
        id: "config-1".to_string(),
        rationale: String::new(),
        payload: crate::agent::AgentActionPayload::ConfigChange {
            setting_path: "theme.active".to_string(),
            operation: "set".to_string(),
            value: Some("kanagawa".to_string()),
        },
    };

    let emitted = service
        .append_agent_action_execution_text_to_terminal_buffer("%1", &action)
        .unwrap();
    assert!(emitted);

    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let pane_text = styled_lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(pane_text.contains("agent: config change: set theme.active"));
    assert!(!pane_text.contains("kanagawa"));
    let action_line = styled_lines
        .iter()
        .find(|line| line.text.contains("agent: config change:"))
        .unwrap();
    let theme = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .ui_theme;
    let prefix_column = display_column_for_fragment(&action_line.text, "agent:");
    let action_column = display_column_for_fragment(&action_line.text, "config change");
    let argument_column = display_column_for_fragment(&action_line.text, "theme.active");
    let prefix_rendition = styled_line_rendition_at(action_line, prefix_column);
    let action_rendition = styled_line_rendition_at(action_line, action_column);
    let argument_rendition = styled_line_rendition_at(action_line, argument_column);
    assert_eq!(
        prefix_rendition.foreground,
        Some(theme.colors.agent_transcript_status.foreground)
    );
    assert!(prefix_rendition.dim);
    assert_eq!(
        action_rendition.foreground,
        Some(theme.colors.agent_transcript_command.foreground)
    );
    assert!(action_rendition.bold);
    assert_ne!(
        argument_rendition.foreground,
        Some(theme.colors.agent_transcript_command.foreground),
        "{action_line:?}"
    );
}

/// Verifies approved non-theme agent `config_change` actions persist through
/// the same user config mutation path that terminal control requests use.
///
/// The action is model-authored, but once `/approve` has accepted it the
/// resulting config file and live runtime setting should agree without a second
/// model-visible live-only override.
#[test]
fn runtime_config_change_persists_generic_setting_and_applies_live() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let config_root = temp_root("runtime-agent-config-change-generic");
    service.set_config_root(config_root.clone());
    let turn = crate::agent::AgentTurnRecord {
        turn_id: "turn-config-generic".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        trigger: crate::agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 200,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        cooperation_mode: None,
        state: AgentTurnState::Running,
    };
    let action = crate::agent::AgentAction {
        id: "config-generic".to_string(),
        rationale: String::new(),
        payload: crate::agent::AgentActionPayload::ConfigChange {
            setting_path: "history.lines".to_string(),
            operation: "set".to_string(),
            value: Some("7".to_string()),
        },
    };

    let result = service
        .execute_config_change_action_for_turn(&turn, &action, &primary, "approved")
        .unwrap();

    assert_eq!(result.status, ActionStatus::Succeeded);
    assert_eq!(service.terminal_history_limit(), 7);
    let config_text = fs::read_to_string(config_root.join("config.toml")).unwrap();
    assert!(config_text.contains("lines = 7"), "{config_text}");
    assert!(
        result
            .structured_content_json
            .as_deref()
            .unwrap_or_default()
            .contains("persistent_control_response"),
        "{result:?}"
    );
    let _ = fs::remove_dir_all(config_root);
}

/// Verifies agent `config_change` reset removes the explicit override.
///
/// Reset is model-facing language for returning a field to its lower-precedence
/// or default value. Runtime execution should therefore share the `config/unset`
/// path while exposing the clearer operation name in MAAP.
#[test]
fn runtime_config_change_reset_removes_override_and_restores_default() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let config_root = temp_root("runtime-agent-config-change-reset");
    service.set_config_root(config_root.clone());
    let turn = crate::agent::AgentTurnRecord {
        turn_id: "turn-config-reset".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        trigger: crate::agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 200,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        cooperation_mode: None,
        state: AgentTurnState::Running,
    };
    let set_action = crate::agent::AgentAction {
        id: "config-reset-set".to_string(),
        rationale: String::new(),
        payload: crate::agent::AgentActionPayload::ConfigChange {
            setting_path: "history.lines".to_string(),
            operation: "set".to_string(),
            value: Some("7".to_string()),
        },
    };
    let reset_action = crate::agent::AgentAction {
        id: "config-reset".to_string(),
        rationale: String::new(),
        payload: crate::agent::AgentActionPayload::ConfigChange {
            setting_path: "history.lines".to_string(),
            operation: "reset".to_string(),
            value: None,
        },
    };

    service
        .execute_config_change_action_for_turn(&turn, &set_action, &primary, "approved")
        .unwrap();
    assert_eq!(service.terminal_history_limit(), 7);
    let result = service
        .execute_config_change_action_for_turn(&turn, &reset_action, &primary, "approved")
        .unwrap();

    assert_eq!(result.status, ActionStatus::Succeeded);
    assert_eq!(service.terminal_history_limit(), 10_000);
    let config_text = fs::read_to_string(config_root.join("config.toml")).unwrap();
    assert!(!config_text.contains("lines = 7"), "{config_text}");
    let _ = fs::remove_dir_all(config_root);
}

/// Verifies config-change control idempotency keys are unique for distinct
/// payloads even if recovery or compatibility paths reuse an action id.
///
/// The JSON-RPC control layer treats idempotency keys as request identities, so
/// a batch of independent model-authored config changes must not collide merely
/// because the local action id is repeated.
#[test]
fn runtime_config_change_idempotency_uses_setting_payload() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let config_root = temp_root("runtime-agent-config-change-idempotency");
    service.set_config_root(config_root.clone());
    let turn = crate::agent::AgentTurnRecord {
        turn_id: "turn-config-idempotency".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        trigger: crate::agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 200,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        cooperation_mode: None,
        state: AgentTurnState::Running,
    };
    let first = crate::agent::AgentAction {
        id: "config-reused".to_string(),
        rationale: String::new(),
        payload: crate::agent::AgentActionPayload::ConfigChange {
            setting_path: "history.lines".to_string(),
            operation: "set".to_string(),
            value: Some("7".to_string()),
        },
    };
    let second = crate::agent::AgentAction {
        id: "config-reused".to_string(),
        rationale: String::new(),
        payload: crate::agent::AgentActionPayload::ConfigChange {
            setting_path: "history.rotate_lines".to_string(),
            operation: "set".to_string(),
            value: Some("3".to_string()),
        },
    };

    let first_result = service
        .execute_config_change_action_for_turn(&turn, &first, &primary, "approved")
        .unwrap();
    let second_result = service
        .execute_config_change_action_for_turn(&turn, &second, &primary, "approved")
        .unwrap();

    assert_eq!(first_result.status, ActionStatus::Succeeded);
    assert_eq!(second_result.status, ActionStatus::Succeeded);
    assert_eq!(service.terminal_history_limit(), 7);
    assert_eq!(service.terminal_history_rotate_lines(), 3);
    let config_text = fs::read_to_string(config_root.join("config.toml")).unwrap();
    assert!(config_text.contains("lines = 7"), "{config_text}");
    assert!(config_text.contains("rotate_lines = 3"), "{config_text}");
    let _ = fs::remove_dir_all(config_root);
}

/// Verifies broad theme color changes from an agent turn are applied in one
/// runtime config batch.
///
/// A user-level `$mez-config` skill can legitimately emit aliases plus every
/// `theme.colors.*` slot when the user asks for a complete palette. Applying
/// those changes as independent config-control requests reloads and redraws
/// the runtime dozens of times in one turn; batching preserves the same final
/// config while keeping live mutation to one validated reload.
#[test]
fn runtime_agent_config_change_batches_broad_theme_palette() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(120, 40).unwrap(), 120)
        .unwrap();
    let config_root = temp_root("runtime-agent-config-change-theme-batch");
    service.set_config_root(config_root.clone());
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-config-theme-batch","input":"$mez-config make my terminal look like a mcdonalds. Don't leave any colors unset"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let before_config_events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary)
        .into_iter()
        .filter(|event| event.kind == EventKind::ConfigChanged)
        .count();
    let mut actions = Vec::new();
    for (name, value) in [
        ("primary", "#ffc72c"),
        ("secondary", "#da291c"),
        ("surface", "#fff8e1"),
        ("foreground", "#241400"),
        ("muted", "#6b5a32"),
        ("tertiary", "#009a44"),
        ("danger", "#b00020"),
        ("thinking", "#7a5c00"),
    ] {
        actions.push(crate::agent::AgentAction {
            id: format!("alias-{name}"),
            rationale: String::new(),
            payload: crate::agent::AgentActionPayload::ConfigChange {
                setting_path: format!("theme.aliases.{name}"),
                operation: "set".to_string(),
                value: Some(value.to_string()),
            },
        });
    }
    for slot in UI_COLOR_SLOT_NAMES {
        let value = if slot.ends_with("_bg") {
            if slot.contains("error") || slot.contains("danger") {
                "surface"
            } else {
                "primary"
            }
        } else if slot.contains("error") || slot.contains("danger") {
            "danger"
        } else if slot.contains("comment") || slot.contains("muted") {
            "muted"
        } else if slot.contains("string") || slot.contains("function") {
            "secondary"
        } else {
            "foreground"
        };
        actions.push(crate::agent::AgentAction {
            id: format!("color-{slot}"),
            rationale: String::new(),
            payload: crate::agent::AgentActionPayload::ConfigChange {
                setting_path: format!("theme.colors.{slot}"),
                operation: "set".to_string(),
                value: Some(value.to_string()),
            },
        });
    }
    let action_count = actions.len();
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap config response".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "set every terminal theme color".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions,
                final_turn: true,
            }),
        },
    };
    service.pending_agent_provider_tasks.remove("turn-1");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert_eq!(execution.action_results.len(), action_count);
    assert!(
        execution
            .action_results
            .iter()
            .all(|result| result.status == ActionStatus::Succeeded),
        "{:?}",
        execution.action_results
    );
    assert_eq!(
        service.ui_theme.colors.prompt.background,
        TerminalColor::Rgb(0xff, 0xc7, 0x2c)
    );
    assert_eq!(
        service.ui_theme.colors.agent_transcript_error.foreground,
        TerminalColor::Rgb(0xb0, 0x00, 0x20)
    );
    let after_config_events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary)
        .into_iter()
        .filter(|event| event.kind == EventKind::ConfigChanged)
        .count();
    assert_eq!(after_config_events - before_config_events, 1);
    let config_text = fs::read_to_string(config_root.join("config.toml")).unwrap();
    assert!(
        config_text.contains(r##"primary = "#ffc72c""##),
        "{config_text}"
    );
    assert!(
        config_text.contains(r#"prompt_bg = "primary""#),
        "{config_text}"
    );
    assert!(
        execution.action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap_or_default()
            .contains(r#""persistent_batch""#),
        "{:?}",
        execution.action_results[0]
    );
    service.pane_processes_mut().terminate_all().unwrap();
    let _ = fs::remove_dir_all(config_root);
}

/// Verifies pending config-change approvals are reconciled when the approval
/// policy changes to full access.
///
/// Configuration changes use the same approval mechanism as other privileged
/// model actions. A policy update that would satisfy the pending action should
/// resume it through the runtime config-control path without requiring a second
/// explicit `/approve`.
#[test]
fn runtime_config_change_resumes_after_full_access_change() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let config_root = temp_root("runtime-agent-config-change-persist");
    service.set_config_root(config_root.clone());
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 12).unwrap(), 20).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-config-approval","input":"change my mez theme to catppuccin_latte"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap config response".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "change the requested live configuration".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "config-1".to_string(),
                    rationale: String::new(),
                    payload: crate::agent::AgentActionPayload::ConfigChange {
                        setting_path: "theme.active".to_string(),
                        operation: "set".to_string(),
                        value: Some("catppuccin_latte".to_string()),
                    },
                }],
                final_turn: false,
            }),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Blocked);
    assert_eq!(service.blocked_approvals().pending().len(), 1);
    let approval_change = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-approval","method":"agent/shell/command","params":{"idempotency_key":"agent-approval-full-access","input":"/approval full-access"}}"#,
        &primary,
    );
    assert!(
        approval_change.contains("requested=full-access"),
        "{approval_change}"
    );
    assert_eq!(
        service.permission_policy().approval_policy,
        crate::permissions::ApprovalPolicy::FullAccess
    );
    assert_eq!(service.blocked_approvals().pending().len(), 0);
    assert_eq!(service.ui_theme.name, "catppuccin_latte");
    assert_eq!(
        service.permission_policy().approval_policy,
        crate::permissions::ApprovalPolicy::FullAccess
    );
    let config_text = fs::read_to_string(config_root.join("config.toml")).unwrap();
    assert!(
        config_text.contains(r#"active = "catppuccin_latte""#),
        "{config_text}"
    );
    assert!(config_text.contains("[theme.colors]"), "{config_text}");
    service.pane_processes_mut().terminate_all().unwrap();
    let _ = fs::remove_dir_all(config_root);
}

/// Verifies subagents inherit the live parent pane auto-reasoning decision.
///
/// Auto-reasoning is a pane-local agent behavior, not just a global default.
/// Child agents should continue with the parent pane's effective setting so a
/// user does not have to re-toggle it after spawning helpers.
#[test]
fn runtime_subagent_auto_reasoning_inherits_parent_pane_setting() {
    let mut service = test_runtime_service();
    service.agent_auto_reasoning = false;
    service
        .agent_auto_reasoning_overrides
        .insert("%1".to_string(), true);

    assert_eq!(
        service.inherited_auto_reasoning_for_child_agent("agent-%1"),
        Some(true)
    );

    service.agent_auto_reasoning_overrides.remove("%1");
    service.agent_auto_reasoning = true;
    assert_eq!(
        service.inherited_auto_reasoning_for_child_agent("agent-%1"),
        Some(true)
    );
}

/// Verifies exiting a parent agent shell closes active child subagent panes.
///
/// Subagent panes are owned by the parent delegation tree. Leaving the parent
/// session should not leave child agents, write scopes, or panes behind as
/// orphaned runtime state.
#[test]
fn runtime_parent_agent_shell_exit_closes_child_subagent_panes() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .execute_terminal_command(&primary, "split-window")
        .unwrap();
    let child_pane_id = service
        .session()
        .active_window()
        .unwrap()
        .panes()
        .iter()
        .find(|pane| pane.id.as_str() != "%1")
        .map(|pane| pane.id.to_string())
        .expect("split-window should create a child pane");
    let child_agent_id = format!("agent-{child_pane_id}");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&child_pane_id)
        .unwrap();
    service.subagent_lineage.insert(
        child_agent_id.clone(),
        RuntimeSubagentLineage {
            parent_agent_id: "agent-%1".to_string(),
            root_agent_id: "agent-%1".to_string(),
            depth: 1,
            display_name: "helper".to_string(),
        },
    );

    service.request_agent_shell_exit_for_pane("%1").unwrap();
    assert!(
        service
            .session()
            .active_window()
            .unwrap()
            .panes()
            .iter()
            .all(|pane| pane.id.as_str() != child_pane_id)
    );
    assert!(!service.subagent_lineage.contains_key(&child_agent_id));
    assert!(service.agent_shell_store().get(&child_pane_id).is_none());
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies a spawned subagent pane records the exact parent prompt before the
/// child turn starts.
///
/// Parent-authored task text is the child agent's effective user instruction.
/// Showing it as a `parent>` log entry lets users inspect the child pane
/// without reconstructing the prompt from parent-pane status messages.
#[test]
fn runtime_subagent_spawn_logs_parent_prompt_in_child_pane() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let spawn = SubagentSpawnRequest {
        parent_agent_id: "agent-%1".to_string(),
        requested_role: "explorer".to_string(),
        placement: "new-pane".to_string(),
        cooperation_mode: CooperationMode::ExploreOnly,
        cooperation_mode_defaulted: false,
        read_scopes: Vec::new(),
        read_scopes_defaulted: false,
        write_scopes: Vec::new(),
        write_scopes_defaulted: false,
        task_prompt: "inspect the renderer issue".to_string(),
        explicit_user_approval: false,
    };

    let spawned = service
        .spawn_runtime_subagent(
            &primary,
            spawn,
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    assert!(spawned.contains(r#""id":"turn-1""#), "{spawned}");
    let child_pane_id = serde_json::from_str::<serde_json::Value>(&spawned)
        .unwrap()
        .get("pane")
        .and_then(|pane| pane.get("pane_id"))
        .and_then(serde_json::Value::as_str)
        .expect("spawned pane id")
        .to_string();
    let child_text = service
        .pane_screen(&child_pane_id)
        .unwrap()
        .normal_content_lines()
        .join("\n");

    assert!(
        child_text.contains("parent> inspect the renderer issue"),
        "{child_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies action execution rows keep secondary count metadata visually quiet.
///
/// Multi-target action previews need to tell users there are additional
/// targets without letting that bookkeeping compete with the action verb or
/// the primary path argument.
#[test]
fn runtime_multi_target_action_line_mutes_secondary_count() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let action = crate::agent::AgentAction {
        id: "patch-many".to_string(),
        rationale: String::new(),
        payload: crate::agent::AgentActionPayload::ApplyPatch {
            patch: concat!(
                "*** Begin Patch\n",
                "*** Update File: src/runtime/render.rs\n",
                "@@\n-old\n+new\n",
                "*** Update File: src/agent/maap.rs\n",
                "@@\n-old\n+new\n",
                "*** Update File: src/terminal/screen.rs\n",
                "@@\n-old\n+new\n",
                "*** End Patch"
            )
            .to_string(),
            strip: None,
        },
    };

    let emitted = service
        .append_agent_action_execution_text_to_terminal_buffer("%1", &action)
        .unwrap();
    assert!(emitted);

    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let action_line = styled_lines
        .iter()
        .find(|line| line.text.contains("agent: apply patch:"))
        .unwrap();
    assert!(
        action_line
            .text
            .contains("agent: apply patch: src/agent/maap.rs (+2 more)"),
        "{action_line:?}"
    );
    let theme = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .ui_theme;
    let metadata_column = display_column_for_fragment(&action_line.text, "(+2 more)");
    let metadata_rendition = styled_line_rendition_at(action_line, metadata_column);
    assert_eq!(
        metadata_rendition.foreground,
        Some(theme.colors.agent_transcript_status.foreground)
    );
    assert!(metadata_rendition.dim);
}

/// Verifies that the pane renderer blocks shell prompt repaint bytes while an
/// agent turn is running, even when no shell transaction is currently active.
/// Provider iteration can leave the pane between command result handling and
/// the next model response; default and debug views must not show PS1 content
/// during that gap.
#[test]
fn runtime_running_agent_turn_hides_shell_prompt_repaints_by_default() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 10).unwrap(),
    );
    let started = service
        .start_agent_prompt_turn("%1", "inspect the pane")
        .unwrap();
    assert_eq!(started.state, AgentTurnState::Running);

    let rendered = service
        .renderable_pane_output_bytes("%1", b"\x1b[38;2;214;93;14muser@host\x1b[0m ~/repo $ ");

    assert!(rendered.is_empty());
}

/// Verifies that `/log-level verbose` remains the explicit mode where shell
/// output is visible during a running agent turn. The hidden default must not
/// make verbose unusable for users who intentionally opted into command output.
#[test]
fn runtime_running_agent_turn_shell_prompt_is_visible_with_verbose_enabled() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Verbose)
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 10).unwrap(),
    );
    let started = service
        .start_agent_prompt_turn("%1", "inspect the pane")
        .unwrap();
    assert_eq!(started.state, AgentTurnState::Running);

    let rendered = service.renderable_pane_output_bytes("%1", b"user@host ~/repo $ ");

    assert_eq!(rendered, b"user@host ~/repo $ ");
}

/// Verifies that shell prompt bytes arriving after a hidden Mezzanine-owned
/// shell transaction is removed are still suppressed for a short retention
/// window. This covers shells that repaint PS1 in a later PTY read after the
/// transaction marker has already settled the action.
#[test]
fn runtime_hidden_agent_shell_rendering_retains_prompt_suppression_after_transaction() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.running_shell_transactions.insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "a1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "ls".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let command_output = service.renderable_pane_output_bytes("%1", b"file-a\n");
    service.running_shell_transactions.remove("marker-1");
    let prompt_repaint = service.renderable_pane_output_bytes("%1", b"user@host ~/repo $ ");
    let mut aged = 0usize;
    for _ in 0..64 {
        aged = aged.saturating_add(service.apply_idle_cleanup_timer_event().unwrap());
    }
    let later_shell_output = service.renderable_pane_output_bytes("%1", b"later\n");

    assert!(command_output.is_empty());
    assert!(prompt_repaint.is_empty());
    assert!(aged > 0);
    assert_eq!(later_shell_output, b"later\n");
}

/// Verifies that foreground pane input applied through the async deferred I/O
/// path clears retained agent-shell output filters before the pane process
/// echoes new user-owned bytes. Without this boundary reset, a delayed parent
/// prompt repaint can be reduced to a carriage return while the foreground
/// cursor remains visually placed after the old prompt, causing the next echoed
/// input to render at column zero.
#[test]
fn runtime_deferred_foreground_input_clears_agent_shell_output_filters() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.remember_hidden_shell_render_suppression("%1");
    service.remember_mez_wrapper_filter_command("%1", "MEZ_MARKER_TOKEN='abc'");

    let (report, deferred) = service
        .apply_attached_terminal_step_plan_deferred_pane_io(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"a".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 1);
    assert_eq!(deferred.len(), 1);
    assert_eq!(deferred[0].pane_id, "%1");
    assert_eq!(deferred[0].bytes, b"a");
    assert!(!service.hidden_shell_render_retention_timer_needed());
    let prompt_repaint = service.visible_pane_output_bytes("%1", b"\r$ ");
    assert_eq!(prompt_repaint, b"\r$ ");
}

/// Verifies large foreground paste payloads stay intact and exit copy mode.
///
/// Host clipboard paste can deliver tens or hundreds of kilobytes as one
/// terminal input event. The runtime should preserve the logical byte stream as
/// one ordered pane-input side effect for the async pane worker to chunk, while
/// returning the target pane to the live bottom so stale copy-mode scroll state
/// cannot keep the user looking at old history.
#[test]
fn runtime_deferred_foreground_paste_stays_ordered_and_exits_copy_mode() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 10).unwrap(),
    );
    service.ensure_active_copy_mode("%1").unwrap();

    let input = vec![b'x'; crate::process::PTY_INPUT_WRITE_CHUNK_BYTES * 2 + 17];
    let (report, deferred) = service
        .apply_attached_terminal_step_plan_deferred_pane_io(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(input.clone())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, input.len());
    assert_eq!(deferred.len(), 1);
    assert_eq!(deferred[0].bytes, input);
    assert!(deferred.iter().all(|chunk| chunk.pane_id == "%1"));
    assert!(deferred.iter().all(|chunk| !chunk.priority));
    assert!(!service.active_copy_modes.contains_key("%1"));
}

/// Verifies that `/log-level verbose` opts the pane back into agent command
/// output without enabling raw wrapper traffic. Verbose remains the shell-view
/// level for commands and their output; trace remains reserved for wrapper
/// internals and full diagnostic payloads.
#[test]
fn runtime_agent_shell_transaction_output_is_visible_with_verbose_enabled() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Verbose)
        .unwrap();
    service.running_shell_transactions.insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "a1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "ls".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let rendered = service.renderable_pane_output_bytes("%1", b"file-a\n");

    assert_eq!(rendered, b"file-a\n");
}

/// Verifies runtime control mcp list uses runtime owned registry.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_control_mcp_list_uses_runtime_owned_registry() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    service
        .mcp_registry_mut()
        .add_server(crate::mcp::McpServerConfig::stdio(
            "fs",
            "filesystem",
            "mcp-fs",
            Vec::new(),
        ))
        .unwrap();
    service
        .mcp_registry_mut()
        .mark_available(
            "fs",
            vec![crate::mcp::McpToolState {
                server_id: String::new(),
                name: "read_file".to_string(),
                available: true,
                blacklisted: false,
                permission_required: true,
                effects: crate::mcp::McpToolEffects::none(),
                approval: crate::mcp::McpApprovalSetting::Inherit,
                description: "read a file".to_string(),
                input_schema_json: "{}".to_string(),
            }],
        )
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"mcp","method":"mcp/list","params":{}}"#,
        &primary,
    );

    assert!(response.contains(r#""id":"fs""#), "{response}");
    assert!(response.contains(r#""id":"fs:read_file""#), "{response}");

    let targeted = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"mcp-targeted","method":"mcp/list","params":{"target":{"default":true}}}"#,
        &primary,
    );
    assert!(targeted.contains(r#""id":"fs""#), "{targeted}");

    let missing_session = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"mcp-missing","method":"mcp/list","params":{"target":{"name":"elsewhere"}}}"#,
        &primary,
    );
    assert!(
        missing_session.contains(r#""mezzanine_code":"not_found""#),
        "{missing_session}"
    );
}

/// Verifies that agent-scoped operations with slash-command equivalents are no
/// longer accepted through the live terminal command prompt. These workflows
/// belong in pane-local agent slash commands, while the terminal command
/// language remains focused on multiplexer/session control.
#[test]
fn runtime_terminal_command_rejects_agent_scoped_slash_duplicates() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let removed = [
        "auth-logout",
        "mcp-list",
        "list-project-trust",
        "trust-project /tmp/project",
        "reject-project /tmp/project",
        "revoke-project-trust /tmp/project",
        "permissions",
        "approval",
        "list-command-rules",
        "allow-command cargo test",
        "deny-command rm",
        "prompt-command git commit",
        "remove-command-rule rule1",
        "bypass-approvals status",
    ];

    for input in removed {
        let error = service
            .execute_terminal_command(&primary, input)
            .unwrap_err();
        assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
        assert!(
            error.message().contains("unknown command"),
            "{input}: {error}"
        );
    }
}

/// Verifies that command-prompt MCP mutation commands update the live runtime
/// configuration layer and reload the runtime-owned MCP registry. MCP listing
/// is handled by the agent `/list-mcp` command, so this test checks registry state
/// directly instead of reintroducing a terminal `mcp-list` alias.
#[tokio::test]
async fn runtime_terminal_command_mcp_add_and_remove_update_runtime_registry() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-terminal-mcp-add-remove");
    let script_path = root.join("mcp-fixture.sh");
    fs::write(&script_path, runtime_mcp_fixture_script(false)).unwrap();

    let add = service
        .execute_terminal_command_async(
            &primary,
            &format!(
                "mcp-add fixture --command /bin/sh --arg {}",
                script_path.display()
            ),
        )
        .await
        .unwrap();

    assert!(add.contains("server=fixture"), "{add}");
    assert!(add.contains("transport=stdio"), "{add}");
    assert!(add.contains("changed=true"), "{add}");
    assert!(add.contains("source=runtime-config"), "{add}");
    assert!(add.contains("status=available"), "{add}");
    assert!(add.contains("tools=1"), "{add}");
    assert_eq!(
        service.mcp_registry().prompt_summary().available_tools[0].tool_name,
        "echo"
    );

    assert_eq!(service.mcp_registry().list_servers().len(), 1);
    assert_eq!(
        service.mcp_registry().prompt_summary().available_tools[0].tool_name,
        "echo"
    );

    let remove = service
        .execute_terminal_command_async(&primary, "mcp-remove fixture")
        .await
        .unwrap();

    assert!(remove.contains("server=fixture"), "{remove}");
    assert!(remove.contains("removed=true"), "{remove}");
    assert!(remove.contains("changed=true"), "{remove}");
    assert!(
        service.mcp_registry().list_servers().is_empty(),
        "{:?}",
        service.mcp_registry().list_servers()
    );
    assert!(service.mcp_registry().list_servers().is_empty());
    let _ = fs::remove_dir_all(root);
}

/// Verifies that live provider information refresh is an explicit terminal
/// command and that the result is cached for later model-list displays.
///
/// Ordinary pane interaction should not fetch provider catalogs on demand; this
/// command is the user-visible refresh path after daemon startup has completed.
#[tokio::test]
async fn runtime_terminal_refresh_provider_info_populates_model_catalog_cache() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\", \"gpt-5.4\"]\ndefault_model = \"gpt-5.5\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();

    let output = service
        .execute_terminal_command_async(&primary, "refresh-provider-info")
        .await
        .unwrap();

    assert!(
        output.contains(r#""command":"refresh-provider-info""#),
        "{output}"
    );
    assert!(
        output.contains("providers=1 refreshed=1 failed=0"),
        "{output}"
    );
    assert!(output.contains("openai source=config"), "{output}");
    assert!(
        output.contains("provider_error=auth-unavailable"),
        "{output}"
    );
    assert!(service.provider_model_catalog_cache.contains_key("openai"));
}

/// Verifies that user-visible MCP retry surfaces clear session blacklisting,
/// drop stale transport state, and rediscover the configured server. This is
/// the recovery path promised by the MCP prompt restriction: blacklisted
/// servers stay hidden from the model until the user explicitly retries them.
#[tokio::test]
async fn runtime_mcp_retry_command_and_control_rediscover_session_blacklisted_server() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-mcp-retry");
    let script_path = root.join("mcp-fixture.sh");
    fs::write(&script_path, runtime_mcp_fixture_script(false)).unwrap();

    service
        .execute_terminal_command_async(
            &primary,
            &format!(
                "mcp-add fixture --command /bin/sh --arg {}",
                script_path.display()
            ),
        )
        .await
        .unwrap();
    service
        .mcp_registry_mut()
        .blacklist_for_session("fixture", "failed handshake")
        .unwrap();
    assert!(
        service
            .mcp_registry()
            .prompt_summary()
            .available_tools
            .is_empty()
    );

    let command_retry = service
        .execute_terminal_command_async(&primary, "mcp-retry fixture")
        .await
        .unwrap();

    assert!(command_retry.contains("server=fixture"), "{command_retry}");
    assert!(
        command_retry.contains("previous_status=blacklisted"),
        "{command_retry}"
    );
    assert!(
        command_retry.contains("status=available"),
        "{command_retry}"
    );
    assert!(
        command_retry.contains("rediscovered=true"),
        "{command_retry}"
    );
    assert_eq!(
        service.mcp_registry().prompt_summary().available_tools[0].tool_name,
        "echo"
    );

    service
        .mcp_registry_mut()
        .blacklist_for_session("fixture", "tool call failed")
        .unwrap();
    let control_retry = service
        .retry_runtime_mcp_server_async("fixture")
        .await
        .unwrap();

    assert!(
        control_retry.previous_status_name() == "blacklisted",
        "{control_retry:?}"
    );
    assert_eq!(
        control_retry.status_name(),
        "available",
        "{control_retry:?}"
    );
    assert!(control_retry.rediscovered, "{control_retry:?}");
    assert_eq!(
        service.mcp_registry().list_servers()[0].status,
        crate::mcp::McpServerStatus::Available
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime control approval methods use runtime owned queue.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_control_approval_methods_use_runtime_owned_queue() {
    let mut service = test_runtime_service();
    let audit_root = temp_root("runtime-approval-audit");
    let audit_path = audit_root.join("audit.jsonl");
    service.set_audit_log(AuditLog::new(crate::audit::AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: false,
        required: true,
    }));
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let approval_id = service
        .queue_blocked_approval(BlockedApprovalRequest {
            id: String::new(),
            requesting_agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            parent_agent_chain: vec!["agent-%1".to_string()],
            action_kind: "shell_command".to_string(),
            action_summary: "cargo test".to_string(),
            declared_effects: vec!["process_control".to_string()],
            matched_rules: vec!["default.prompt".to_string()],
            read_scopes: vec![".".to_string()],
            write_scopes: Vec::new(),
            cooperation_mode: None,
            created_at_unix_seconds: None,
            decided_at_unix_seconds: None,
            decided_by_client_id: None,
            state: crate::permissions::BlockedApprovalState::Approved,
            decision: Some(crate::permissions::ApprovalDecision::Disapprove),
            redirect_instruction: Some("ignored by create".to_string()),
        })
        .unwrap();

    let mut connection = ControlConnectionState::trusted_existing_client(primary.clone());
    let list = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"list","method":"approval/list","params":{}}"#,
    );
    let (list_output, _) = service
        .handle_control_input_for_connection(&list, 4096, &mut connection)
        .unwrap();
    let (list_body, _) = decode_control_frame(&list_output, 4096).unwrap();
    assert!(list_body.contains(&format!(r#""approval_id":"{}""#, approval_id)));
    assert!(list_body.contains(r#""state":"pending""#), "{list_body}");
    assert!(list_body.contains(r#""created_at":""#), "{list_body}");
    assert!(list_body.contains(r#""decided_at":null"#), "{list_body}");

    let decide = encode_control_body(&format!(
        r#"{{"jsonrpc":"2.0","id":"decide","method":"approval/decide","params":{{"approval_id":"{}","decision":"approve","scope":{{"persistence":"session","command_prefix":["cargo","test"]}},"idempotency_key":"approval-decision"}}}}"#,
        approval_id
    ));
    let (decide_output, _) = service
        .handle_control_input_for_connection(&decide, 4096, &mut connection)
        .unwrap();
    let (decide_body, _) = decode_control_frame(&decide_output, 4096).unwrap();
    assert!(
        decide_body.contains(r#""state":"approved""#),
        "{decide_body}"
    );
    assert!(decide_body.contains(r#""decided_at":""#), "{decide_body}");
    assert!(
        decide_body.contains(&format!(r#""decided_by_client_id":"{}""#, primary)),
        "{decide_body}"
    );
    assert_eq!(
        service.blocked_approvals().get(&approval_id).unwrap().state,
        crate::permissions::BlockedApprovalState::Approved
    );
    assert_eq!(
        service
            .permission_policy()
            .evaluate_shell_command("cargo test"),
        RuleDecision::Allow
    );
    assert!(
        service.permission_policy().rules().iter().any(|rule| {
            rule.scope == CommandRuleScope::Session
                && matches!(rule.rule_match, RuleMatch::ExactSha256 { .. })
                && rule.decision == RuleDecision::Allow
        }),
        "approval/decide scope should control persisted exact command rules"
    );

    let approved_list = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"approved-list","method":"approval/list","params":{"state":"approved"}}"#,
    );
    let (approved_output, _) = service
        .handle_control_input_for_connection(&approved_list, 4096, &mut connection)
        .unwrap();
    let (approved_body, _) = decode_control_frame(&approved_output, 4096).unwrap();
    assert!(
        approved_body.contains(&format!(r#""approval_id":"{}""#, approval_id)),
        "{approved_body}"
    );

    let pending_list = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"pending-list","method":"approval/list","params":{"state":"pending"}}"#,
    );
    let (pending_output, _) = service
        .handle_control_input_for_connection(&pending_list, 4096, &mut connection)
        .unwrap();
    let (pending_body, _) = decode_control_frame(&pending_output, 4096).unwrap();
    assert!(pending_body.contains(r#""approvals":[]"#), "{pending_body}");

    let (repeated_output, _) = service
        .handle_control_input_for_connection(&decide, 4096, &mut connection)
        .unwrap();
    let (repeated_body, _) = decode_control_frame(&repeated_output, 4096).unwrap();
    assert_eq!(repeated_body, decide_body);

    let primary_events = service
        .event_log
        .as_ref()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(
        primary_events.iter().any(|event| {
            event.kind == EventKind::ApprovalChanged
                && event
                    .payload
                    .contains(&format!(r#""approval_id":"{}""#, approval_id))
        }),
        "{primary_events:?}"
    );
    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains(r#""event_type":"approval""#), "{audit}");
    assert!(audit.contains(r#""action":"prompt""#), "{audit}");
    assert!(audit.contains(r#""outcome":"prompted""#), "{audit}");
    assert!(audit.contains(r#""outcome":"started""#), "{audit}");
    assert!(audit.contains(r#""outcome":"applied""#), "{audit}");
    assert!(
        audit.contains(&format!(r#""approval_id":"{approval_id}""#)),
        "{audit}"
    );
    let _ = fs::remove_dir_all(audit_root);
}

/// Verifies that project-persistent approval choices create and update the
/// project-local Mezzanine config with exact command rules for the command
/// arguments the user actually reviewed. This keeps the prompt workflow
/// config-driven: allow-forever writes an allow rule, deny writes a deny rule,
/// and future decisions are evaluated from the project overlay rather than a
/// hard-coded command blocklist.
#[test]
fn runtime_control_project_approval_decisions_persist_exact_command_rules() {
    let root = temp_root("runtime-project-approval-rules");
    fs::create_dir_all(root.join(".git")).unwrap();
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let descriptor = service.initial_pane_descriptor().unwrap();
    service
        .start_pane_process_with_start_directory(descriptor, Some("sleep 30"), Some(&root))
        .unwrap();

    let allow_id = service
        .queue_blocked_approval(BlockedApprovalRequest {
            id: String::new(),
            requesting_agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            parent_agent_chain: vec!["agent-%1".to_string()],
            action_kind: "shell_command".to_string(),
            action_summary: "mez-test-command --flag".to_string(),
            declared_effects: vec!["unknown command effects".to_string()],
            matched_rules: vec!["default.prompt".to_string()],
            read_scopes: Vec::new(),
            write_scopes: Vec::new(),
            cooperation_mode: None,
            created_at_unix_seconds: None,
            decided_at_unix_seconds: None,
            decided_by_client_id: None,
            state: crate::permissions::BlockedApprovalState::Pending,
            decision: None,
            redirect_instruction: None,
        })
        .unwrap();
    let deny_id = service
        .queue_blocked_approval(BlockedApprovalRequest {
            id: String::new(),
            requesting_agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            parent_agent_chain: vec!["agent-%1".to_string()],
            action_kind: "shell_command".to_string(),
            action_summary: "mez-test-command --delete".to_string(),
            declared_effects: vec!["unknown command effects".to_string()],
            matched_rules: vec!["default.prompt".to_string()],
            read_scopes: Vec::new(),
            write_scopes: Vec::new(),
            cooperation_mode: None,
            created_at_unix_seconds: None,
            decided_at_unix_seconds: None,
            decided_by_client_id: None,
            state: crate::permissions::BlockedApprovalState::Pending,
            decision: None,
            redirect_instruction: None,
        })
        .unwrap();

    let allow = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"allow-project","method":"approval/decide","params":{{"approval_id":"{}","decision":"approve","scope":{{"persistence":"project"}},"idempotency_key":"allow-project"}}}}"#,
            allow_id
        ),
        &primary,
    );
    assert!(allow.contains(r#""state":"approved""#), "{allow}");

    let deny = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"deny-project","method":"approval/decide","params":{{"approval_id":"{}","decision":"disapprove","scope":{{"persistence":"project"}},"idempotency_key":"deny-project"}}}}"#,
            deny_id
        ),
        &primary,
    );
    assert!(deny.contains(r#""state":"disapproved""#), "{deny}");

    let project_config = root.join(".mezzanine/config.toml");
    let config_text = fs::read_to_string(&project_config).unwrap();
    assert!(
        config_text.contains(r#"approval_policy = "ask""#),
        "{config_text}"
    );
    assert!(
        config_text.contains(r#"match = "exact_sha256""#),
        "{config_text}"
    );
    assert!(
        config_text.contains(r#"decision = "allow""#),
        "{config_text}"
    );
    assert!(
        config_text.contains(r#"decision = "deny""#),
        "{config_text}"
    );
    assert_eq!(
        service
            .permission_policy()
            .evaluate_shell_command("mez-test-command --flag"),
        RuleDecision::Allow
    );
    assert_eq!(
        service
            .permission_policy()
            .evaluate_shell_command("mez-test-command --delete"),
        RuleDecision::Forbid
    );
    assert_eq!(
        service
            .permission_policy()
            .evaluate_shell_command("mez-test-command --flag extra"),
        RuleDecision::Prompt
    );
    assert_eq!(
        service
            .permission_policy()
            .evaluate_shell_command("mez-test-command --delete --dry-run"),
        RuleDecision::Prompt
    );
    assert!(
        service.permission_policy().rules().iter().any(|rule| {
            rule.scope == CommandRuleScope::Project
                && matches!(rule.rule_match, RuleMatch::ExactSha256 { .. })
                && rule.decision == RuleDecision::Allow
        }),
        "project approval should load an exact allow rule into the runtime policy"
    );
    assert!(
        service.permission_policy().rules().iter().any(|rule| {
            rule.scope == CommandRuleScope::Project
                && matches!(rule.rule_match, RuleMatch::ExactSha256 { .. })
                && rule.decision == RuleDecision::Forbid
        }),
        "project approval should load an exact deny rule into the runtime policy"
    );

    service.pane_processes_mut().terminate_all().unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime approval disapproval focuses blocked agent pane.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_approval_disapproval_focuses_blocked_agent_pane() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let blocked_pane = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    service.session.select_pane(&primary, "%1").unwrap();
    service
        .agent_shell_store_mut()
        .ensure_session(blocked_pane.as_str())
        .unwrap();
    let approval_id = service
        .queue_blocked_approval(BlockedApprovalRequest {
            id: String::new(),
            requesting_agent_id: format!("agent-{blocked_pane}"),
            pane_id: blocked_pane.to_string(),
            parent_agent_chain: vec![format!("agent-{blocked_pane}")],
            action_kind: "shell_command".to_string(),
            action_summary: "env".to_string(),
            declared_effects: vec!["approval required".to_string()],
            matched_rules: vec!["runtime.agent_action_blocked".to_string()],
            read_scopes: Vec::new(),
            write_scopes: Vec::new(),
            cooperation_mode: None,
            created_at_unix_seconds: None,
            decided_at_unix_seconds: None,
            decided_by_client_id: None,
            state: crate::permissions::BlockedApprovalState::Pending,
            decision: None,
            redirect_instruction: None,
        })
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"deny","method":"approval/decide","params":{{"approval_id":"{}","decision":"disapprove","idempotency_key":"deny-blocked-agent"}}}}"#,
            approval_id
        ),
        &primary,
    );

    assert!(response.contains(r#""state":"disapproved""#), "{response}");
    assert_eq!(
        service
            .session()
            .active_window()
            .unwrap()
            .active_pane()
            .id
            .as_str(),
        blocked_pane.as_str()
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get(blocked_pane.as_str())
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );
}

/// Verifies runtime applies permission and mcp state from config layers.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_applies_permission_and_mcp_state_from_config_layers() {
    let mut service = test_runtime_service();
    let report = service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[permissions]\napproval_policy = \"full-access\"\nbypass_mode = false\n[[permissions.command_rules]]\npattern = [\"cargo\", \"test\"]\ndecision = \"allow\"\nscope = \"session\"\nmatch = \"prefix\"\n[mcp_servers.fs]\nname = \"filesystem\"\ncommand = \"mcp-fs\"\nargs = [\"--root\", \".\"]\nenv_vars = [\"MEZ_TEST_MISSING_TOKEN\"]\n".to_string(),
        }])
        .unwrap();

    assert_eq!(report.applied_layers, vec!["primary".to_string()]);
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::FullAccess
    );
    assert!(!service.permission_policy().approval_bypass());
    assert_eq!(
        service
            .permission_policy()
            .evaluate_shell_command("cargo test --all-targets"),
        RuleDecision::Allow
    );
    assert_eq!(service.mcp_registry().list_servers().len(), 1);
    assert_eq!(
        service.mcp_registry().prompt_summary().unavailable_servers[0].server_id,
        "fs"
    );
    assert_eq!(report.providers_configured, 1);
    assert_eq!(report.model_profiles_configured, 7);
    assert_eq!(report.default_model_profile.as_deref(), Some("default"));
    let profile = service
        .provider_registry()
        .resolve_profile("default")
        .unwrap();
    assert_eq!(profile.provider, "openai");
    assert_eq!(profile.model, "gpt-5.5");
    assert!(
        service
            .provider_registry()
            .resolve_profile("gpt-5.2")
            .is_ok(),
        "built-in OpenAI model profiles should be available when no provider list is configured"
    );
}

/// Verifies runtime applies explicit host clipboard pipe commands from
/// configuration. Users on systems where the default auto-detection order is
/// wrong need deterministic copy and paste commands without replacing the
/// internal paste-buffer behavior.
#[test]
fn runtime_applies_host_clipboard_pipe_commands_from_config_layers() {
    let root = std::env::temp_dir().join(format!(
        "mez-runtime-clipboard-config-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let copy_path = root.join("copied.txt");
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                "[terminal]\nclipboard_copy_command = [\"sh\", \"-c\", \"cat > '{}'\"]\nclipboard_paste_command = [\"sh\", \"-c\", \"printf configured-paste\"]\n",
                copy_path.display()
            ),
        }])
        .unwrap();

    assert!(service.host_clipboard.copy("configured-copy"));
    assert_eq!(fs::read_to_string(&copy_path).unwrap(), "configured-copy");
    assert_eq!(
        service.host_clipboard.read(),
        Some("configured-paste".to_string())
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies that configured named model profiles populate the full
/// specification-facing profile fields and that configured fallback profiles
/// are filtered through safety, privacy, residency, and approval
/// characteristics before they can be offered after provider failure.
#[test]
fn runtime_applies_named_model_profile_fields_and_safe_fallbacks() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"work\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-work\", \"gpt-safe\", \"gpt-weak\", \"gpt-external\"]\ndefault_model = \"gpt-work\"\n[model_profiles.work]\nprovider = \"openai\"\nmodel = \"gpt-work\"\nreasoning_profile = \"high\"\nlatency_preference = \"balanced\"\nmultimodal_required = true\nsafety_tier = \"high\"\nprivacy_tier = \"strict\"\nresidency = \"us\"\napproval_policy = \"ask\"\nfallback_profiles = [\"safe\", \"weak\", \"external\"]\n[model_profiles.work.provider_options]\nreasoning_effort = \"high\"\n[model_profiles.safe]\nprovider = \"openai\"\nmodel = \"gpt-safe\"\nsafety_tier = \"high\"\nprivacy_tier = \"strict\"\nresidency = \"us\"\napproval_policy = \"ask\"\n[model_profiles.weak]\nprovider = \"openai\"\nmodel = \"gpt-weak\"\nsafety_tier = \"medium\"\nprivacy_tier = \"strict\"\nresidency = \"us\"\napproval_policy = \"ask\"\n[model_profiles.external]\nprovider = \"openai\"\nmodel = \"gpt-external\"\nsafety_tier = \"high\"\nprivacy_tier = \"external\"\nresidency = \"eu\"\napproval_policy = \"full-access\"\n"
                .to_string(),
        }])
        .unwrap();

    let registry = service.provider_registry();
    let profile = registry.resolve_profile("work").unwrap();
    assert_eq!(profile.provider, "openai");
    assert_eq!(profile.model, "gpt-work");
    assert_eq!(profile.reasoning_profile.as_deref(), Some("high"));
    assert_eq!(profile.latency_preference.as_deref(), Some("balanced"));
    assert!(profile.multimodal_required);
    assert_eq!(profile.safety_tier.as_deref(), Some("high"));
    assert_eq!(
        profile
            .provider_options
            .get("reasoning_effort")
            .map(String::as_str),
        Some("high")
    );
    assert_eq!(
        registry.safe_fallback_profiles("work").unwrap(),
        vec!["safe".to_string()]
    );
}

/// Verifies that provider failure reporting only offers configured fallback
/// profiles whose safety, privacy, residency, and approval characteristics are
/// non-weaker than the active model profile.
#[test]
fn runtime_provider_failure_reports_only_safe_model_fallbacks() {
    let mut service = test_runtime_service();
    let transcript_root = temp_root("runtime-provider-safe-fallback-transcript");
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"runtime-fail\"\ndefault_model_profile = \"work\"\n[providers.runtime-fail]\nkind = \"runtime-fail\"\nmodels = [\"primary\", \"safe\", \"weak\"]\ndefault_model = \"primary\"\n[model_profiles.work]\nprovider = \"runtime-fail\"\nmodel = \"primary\"\nsafety_tier = \"high\"\nprivacy_tier = \"strict\"\nresidency = \"us\"\napproval_policy = \"ask\"\nfallback_profiles = [\"safe\", \"weak\"]\n[model_profiles.safe]\nprovider = \"runtime-fail\"\nmodel = \"safe\"\nsafety_tier = \"high\"\nprivacy_tier = \"strict\"\nresidency = \"us\"\napproval_policy = \"ask\"\n[model_profiles.weak]\nprovider = \"runtime-fail\"\nmodel = \"weak\"\nsafety_tier = \"medium\"\nprivacy_tier = \"external\"\nresidency = \"eu\"\napproval_policy = \"full-access\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-provider-safe-fallback","input":"summarize the pane"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    assert_eq!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == "turn-1")
            .map(|turn| turn.model_profile.as_str()),
        Some("work")
    );

    let error = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &RuntimeFailingProvider,
            service.provider_registry().resolve_profile("work").unwrap(),
        )
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    let entries = transcript_store.inspect(&conversation_id).unwrap();
    let failure = entries
        .iter()
        .find(|entry| {
            entry.role == crate::transcript::TranscriptRole::Assistant
                && entry.content.contains("provider_error")
        })
        .unwrap();
    assert!(failure.content.contains("safe_fallback_profiles: safe"));
    assert!(!failure.content.contains("weak"));
    let _ = fs::remove_dir_all(transcript_root);
}

/// Verifies that frame position, style, and visible-field fallback templates
/// are applied from runtime config layers instead of being accepted but ignored.
#[test]
fn runtime_applies_frame_display_options_from_config_layers() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[frames.window]\nenabled = true\nposition = \"bottom\"\nstyle = \"inverse\"\ntemplate = \"\"\nright_status = \"#{datetime.local}\"\nvisible_fields = [\"session.id\", \"window.index\"]\n[frames.pane]\nenabled = true\nposition = \"bottom\"\nstyle = \"bold\"\ntemplate = \"\"\nvisible_fields = [\"pane.index\", \"agent.status\"]\n".to_string(),
        }])
        .unwrap();

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();

    assert!(service.window_frames_enabled);
    assert!(config.window_frames_enabled);
    assert_eq!(
        config.window_frame_position,
        crate::terminal::TerminalFramePosition::Bottom
    );
    assert_eq!(
        config.window_frame_style,
        crate::terminal::TerminalFrameStyle::Inverse
    );
    assert_eq!(
        config.window_frame_template,
        "#{session.id} #{window.index}"
    );
    assert_eq!(
        config.window_frame_visible_fields,
        vec!["session.id".to_string(), "window.index".to_string()]
    );
    assert_eq!(
        service.window_frame_right_status_template,
        "#{datetime.local}"
    );
    assert!(config.pane_frames_enabled);
    assert_eq!(
        config.pane_frame_position,
        crate::terminal::TerminalFramePosition::Bottom
    );
    assert_eq!(
        config.pane_frame_style,
        crate::terminal::TerminalFrameStyle::Bold
    );
    assert_eq!(config.pane_frame_template, "#{pane.index} #{agent.status}");
    assert_eq!(
        config.pane_frame_visible_fields,
        vec!["pane.index".to_string(), "agent.status".to_string()]
    );
}

/// Verifies that terminal cursor presentation settings are parsed from runtime
/// configuration layers and applied to attached-terminal render configuration.
#[test]
fn runtime_applies_cursor_presentation_options_from_config_layers() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[terminal]\ncursor_style = \"bar\"\ncursor_blink = false\ncursor_blink_interval_ms = 250\nresize_debounce_ms = 125\nreduced_motion = true\n"
                .to_string(),
        }])
        .unwrap();

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();

    assert_eq!(
        config.cursor_style,
        crate::terminal::TerminalCursorStyle::Bar
    );
    assert!(!config.cursor_blink);
    assert_eq!(config.cursor_blink_interval_ms, 250);
    assert_eq!(config.resize_debounce_ms, 125);
    assert!(config.frame_context.reduced_motion);
    assert_eq!(config.frame_context.animation_tick_ms, 0);
}
/// Verifies that frame-context animation stays static when no live agent footer
/// is visible in the active window. This keeps idle redraws from paying for
/// animated footer state when agent mode is inactive or quiescent.
#[test]
fn runtime_frame_context_disables_animation_without_live_agent_footer() {
    let service = test_runtime_service();
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    assert_eq!(config.frame_context.animation_tick_ms, 0);
}
/// Verifies that a live agent footer re-enables animated frame ticks so active
/// agent progress indicators keep their motion while work is still running.
#[test]
fn runtime_frame_context_animates_live_agent_footer() {
    let mut service = test_runtime_service();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    service.agent_compacting_panes.insert(pane_id, 1);
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    assert!(config.frame_context.animation_tick_ms > 0);
}
/// Verifies that callers with an already-resolved terminal loop config can
/// render the same primary view without rebuilding frame context and mouse hit
/// regions. This protects the optimized hot path used by control requests that
/// need both config and a rendered frame.
#[test]
fn runtime_render_client_view_with_resolved_config_matches_public_render() {
    let service = test_runtime_service();
    let client_size = Size::new(80, 24).unwrap();
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let direct = service
        .render_client_view(ClientViewRole::Primary, client_size, &config)
        .unwrap();
    let resolved = service
        .render_client_view_with_resolved_config(ClientViewRole::Primary, client_size, &config)
        .unwrap();
    assert_eq!(resolved, direct);
}

/// Verifies that runtime frame context sources `pane.process_name` from the
/// live host process metadata instead of only echoing the configured shell path.
#[cfg(target_os = "linux")]
/// Verifies runtime frame context uses host process name when available.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_frame_context_uses_host_process_name_when_available() {
    let mut service = test_runtime_service();
    service.start_initial_pane_process(Some("sleep 2")).unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();

    let mut process_name = None;
    for _ in 0..10_000 {
        process_name = service.pane_processes().process_name(&pane_id);
        if process_name.as_deref() == Some("sleep") {
            break;
        }
        thread::yield_now();
    }

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(process_name.as_deref(), Some("sleep"));
    assert_eq!(pane_context.process_name.as_deref(), Some("sleep"));
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that frame context renders the real normalized exit status when a
/// non-live pane has known exit metadata. This prevents pane frames from
/// collapsing all exited processes into a generic `exited` placeholder.
#[test]
fn runtime_frame_context_uses_known_pane_exit_status() {
    let mut service = test_runtime_service();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    service
        .session
        .set_pane_live_state(&pane_id, false)
        .unwrap();
    service.pane_exit_records.insert(
        pane_id.clone(),
        PaneExitRecord {
            exit_status: crate::process::PaneExitStatus {
                code: Some(7),
                signal: None,
                success: false,
            },
        },
    );

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(pane_context.exit_status.as_deref(), Some("exit=7"));
}

/// Verifies that a visible pane agent shell publishes the active model profile,
/// reasoning profile, and idle status into pane frame context before any turn
/// has started. The default header relies on these fields for agent mode.
#[test]
fn runtime_frame_context_reports_visible_agent_shell_metadata() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"work\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-work\"]\ndefault_model = \"gpt-work\"\n[model_profiles.work]\nprovider = \"openai\"\nmodel = \"gpt-work\"\nreasoning_profile = \"high\"\n"
                .to_string(),
        }])
        .unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(pane_context.mode.as_deref(), Some("agent"));
    assert_eq!(pane_context.agent_name.as_deref(), Some("manager"));
    assert_eq!(pane_context.agent_status.as_deref(), Some("idle"));
    assert_eq!(pane_context.agent_model.as_deref(), Some("gpt-work"));
    assert_eq!(pane_context.agent_reasoning.as_deref(), Some("high"));
}

/// Verifies that pane-frame runtime context includes the best known current
/// working directory in the compact home-relative form used by the status
/// pill. This keeps the renderer independent from process probing while still
/// giving users location context when shell prompts are hidden or overwritten.
#[test]
fn runtime_frame_context_reports_home_relative_pane_working_directory() {
    let mut service = test_runtime_service();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from);
    let path = home
        .as_ref()
        .map(|home| home.join("Documents/repos/mezzanine"))
        .unwrap_or_else(|| PathBuf::from("/tmp/mezzanine"));
    let expected = home
        .as_ref()
        .map(|_| "~/Documents/repos/mezzanine".to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());
    service
        .pane_current_working_directories
        .insert(pane_id.clone(), path);

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(
        pane_context.current_working_directory.as_deref(),
        Some(expected.as_str())
    );
    assert_eq!(
        config
            .frame_context
            .window_status
            .as_ref()
            .and_then(|status| status.active_pane_working_directory.as_deref()),
        Some(expected.as_str())
    );
}
/// Verifies that frame context leaves unused dynamic right-status fields empty
/// when the configured template only references pane working-directory data.
/// This avoids repeated uptime and datetime formatting work on redraws that do
/// not display those fields.
#[test]
fn runtime_frame_context_skips_unused_dynamic_window_status_fields() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[frames.window]\nright_status = \"#{pane.pwd}\"\n".to_string(),
        }])
        .unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from);
    let path = home
        .as_ref()
        .map(|home| home.join("Documents/repos/mezzanine"))
        .unwrap_or_else(|| PathBuf::from("/tmp/mezzanine"));
    let expected = home
        .as_ref()
        .map(|_| "~/Documents/repos/mezzanine".to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());
    service
        .pane_current_working_directories
        .insert(pane_id, path);
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let status = config.frame_context.window_status.as_ref().unwrap();
    assert_eq!(
        status.active_pane_working_directory.as_deref(),
        Some(expected.as_str())
    );
    assert!(status.system_uptime.is_empty());
    assert!(status.datetime_local.is_empty());
}

/// Verifies that the pane-frame status reports compaction as its own active
/// running substate. Compaction is provider work, but it is distinct enough
/// from ordinary response generation that users need a direct state label.
#[test]
fn runtime_frame_context_reports_agent_compacting_substate() {
    let mut service = test_runtime_service();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    service
        .agent_turn_ledger
        .start_turn(crate::agent::AgentTurnRecord {
            turn_id: "turn-completed".to_string(),
            agent_id: format!("agent-{pane_id}"),
            pane_id: pane_id.clone(),
            trigger: crate::agent::AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 1,
            policy_profile: "default".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            cooperation_mode: None,
            state: AgentTurnState::Queued,
        })
        .unwrap();
    service
        .agent_turn_ledger
        .finish_turn("turn-completed", AgentTurnState::Completed)
        .unwrap();
    service.agent_compacting_panes.insert(pane_id.clone(), 1);

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(pane_context.agent_status.as_deref(), Some("compacting"));
    assert_eq!(
        config
            .frame_context
            .window_agent_active_counts
            .get(service.session().active_window().unwrap().id.as_str())
            .copied(),
        Some(1)
    );
}

/// Verifies that an active agent turn reports the provider model name rather
/// than the selected profile name in pane-frame metadata. The pane status area
/// is constrained, so showing the concrete provider model and keeping reasoning
/// in its own field preserves both accuracy and space.
#[test]
fn runtime_frame_context_reports_running_agent_provider_model_name() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"work\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-work\"]\ndefault_model = \"gpt-work\"\n[model_profiles.work]\nprovider = \"openai\"\nmodel = \"gpt-work\"\nreasoning_profile = \"high\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"frame-provider-model","input":"summarize the pane"}}"#,
        &primary,
    );
    assert!(response.contains(r#""state":"running""#), "{response}");

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(pane_context.agent_status.as_deref(), Some("thinking"));
    assert_eq!(pane_context.agent_name.as_deref(), Some("manager"));
    assert_eq!(pane_context.agent_model.as_deref(), Some("gpt-work"));
    assert_eq!(pane_context.agent_reasoning.as_deref(), Some("high"));
    assert_eq!(pane_context.agent_context_usage, None);
    assert!(
        pane_context
            .agent_display_lines
            .iter()
            .any(|line| line.starts_with("thinking (") && line.contains(" • esc to interrupt")),
        "{pane_context:?}"
    );

    service
        .finish_agent_turn(&pane_id, "turn-1", AgentTurnState::Completed)
        .unwrap();
    let pane_text = service
        .pane_screen(&pane_id)
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("Worked for "), "{pane_text}");
}

/// Verifies that the pane frame reports only the latest provider-backed input
/// context percentage instead of replacing it with a local preflight estimate
/// while another turn is running. This keeps the status pill tied to the same
/// token accounting that the provider returns, while still allowing the runtime
/// to use internal byte estimates for compaction decisions separately.
#[test]
fn runtime_frame_context_reports_last_provider_context_usage() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"work\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-work\"]\ndefault_model = \"gpt-work\"\n[model_profiles.work]\nprovider = \"openai\"\nmodel = \"gpt-work\"\ncontext_window_tokens = 1000\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();

    let initial_config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let initial_pane_context = initial_config.frame_context.panes.get(&pane_id).unwrap();
    assert_eq!(initial_pane_context.agent_context_usage, None);

    service.record_agent_provider_token_usage(
        &pane_id,
        crate::agent::ModelTokenUsage {
            input_tokens: 251,
            output_tokens: 34,
            reasoning_tokens: 9,
            cached_input_tokens: Some(80),
        },
    );
    let recorded_config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let recorded_pane_context = recorded_config.frame_context.panes.get(&pane_id).unwrap();
    assert_eq!(
        recorded_pane_context.agent_context_usage.as_deref(),
        Some("25%")
    );

    let (_, profile) = service
        .active_model_profile_for_pane(&pane_id, &format!("agent-{pane_id}"), None)
        .unwrap();
    service.record_agent_provider_token_usage_with_profile(
        &pane_id,
        crate::agent::ModelTokenUsage {
            input_tokens: 1_200,
            output_tokens: 10,
            reasoning_tokens: 5,
            cached_input_tokens: Some(100),
        },
        crate::agent::ModelTokenUsage {
            input_tokens: 251,
            output_tokens: 10,
            reasoning_tokens: 5,
            cached_input_tokens: Some(80),
        },
        Some(&profile),
    );
    let cumulative_recorded_config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let cumulative_recorded_pane_context = cumulative_recorded_config
        .frame_context
        .panes
        .get(&pane_id)
        .unwrap();
    assert_eq!(
        cumulative_recorded_pane_context
            .agent_context_usage
            .as_deref(),
        Some("25%")
    );

    service.record_agent_provider_token_usage_with_profile(
        &pane_id,
        crate::agent::ModelTokenUsage {
            input_tokens: 1_500,
            output_tokens: 10,
            reasoning_tokens: 5,
            cached_input_tokens: Some(100),
        },
        crate::agent::ModelTokenUsage {
            input_tokens: 1_200,
            output_tokens: 10,
            reasoning_tokens: 5,
            cached_input_tokens: Some(80),
        },
        Some(&profile),
    );
    let saturated_config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let saturated_pane_context = saturated_config.frame_context.panes.get(&pane_id).unwrap();
    assert_eq!(
        saturated_pane_context.agent_context_usage.as_deref(),
        Some("100%")
    );

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt-context-usage","method":"agent/shell/command","params":{"idempotency_key":"agent-prompt-context-usage","input":"summarize the pane"}}"#,
        &primary,
    );
    assert!(response.contains(r#""state":"running""#), "{response}");

    let running_config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let running_pane_context = running_config.frame_context.panes.get(&pane_id).unwrap();
    assert_eq!(
        running_pane_context.agent_context_usage.as_deref(),
        Some("100%")
    );
}

/// Verifies that the agent frame context percentage uses the effective model
/// context-window denominator when a profile omits an explicit token count. This
/// protects the status area from reporting OpenAI GPT-5.5 usage against the
/// small local fallback window instead of the provider model's documented window.
#[test]
fn runtime_frame_context_uses_known_openai_model_context_window() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"work\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\"]\ndefault_model = \"gpt-5.5\"\n[model_profiles.work]\nprovider = \"openai\"\nmodel = \"gpt-5.5\"\n"
                .to_string(),
        }])
        .unwrap();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();

    service.record_agent_provider_token_usage(
        &pane_id,
        crate::agent::ModelTokenUsage {
            input_tokens: 10_500,
            output_tokens: 34,
            reasoning_tokens: 9,
            cached_input_tokens: Some(80),
        },
    );
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(pane_context.agent_context_usage.as_deref(), Some("1%"));
}

/// Verifies that runtime config application fails closed when a layer attempts
/// to enter approval bypass directly. Bypass activation must stay tied to the
/// explicit primary-authorized command path rather than a passive config load
/// or live config reload.
#[test]
fn runtime_rejects_config_enabled_approval_bypass_mode() {
    let mut service = test_runtime_service();
    let error = service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[permissions]\nbypass_mode = true\n".to_string(),
        }])
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Config);
    assert!(
        error
            .message()
            .contains("permissions.bypass_mode cannot be enabled from configuration"),
        "{}",
        error.message()
    );
    assert!(!service.permission_policy().approval_bypass());
}

/// Verifies runtime applies configured lifecycle hooks.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_applies_configured_lifecycle_hooks() {
    let root = temp_root("configured-hooks");
    let payload_path = root.join("attach-payload.json");
    let mut service = test_runtime_service();
    let report = service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                "[hooks.attach]\nevent = \"client_attach\"\nprogram = \"/bin/sh\"\nargs = [\"-c\", \"cat > \\\"$1\\\"\", \"hook\", \"{}\"]\n\n[hooks.focused]\nevent = \"client_attach\"\ncommand = \"printf hook-from-config\"\nagent_hook = true\non_failure = \"warn\"\n",
                payload_path.display()
            ),
        }])
        .unwrap();

    assert_eq!(report.hooks_configured, 2);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();

    let payload = fs::read_to_string(&payload_path).unwrap();
    assert!(payload.contains(r#""client_id":"#), "{payload}");
    assert!(payload.contains(primary.as_str()), "{payload}");
    assert_eq!(service.focused_shell_hook_queue_len(), 1);
    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime config parses hook matcher groups.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_config_parses_hook_matcher_groups() {
    let mut service = test_runtime_service();
    let report = service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[hooks.prompt]\nevent = \"user_prompt_submit\"\nprogram = \"/bin/echo\"\n[hooks.prompt.match.pane_id]\nprefix = \"pane-\"\n[[hooks.prompt.matches]]\npath = \"agent_id\"\nequals = \"agent-1\"\n".to_string(),
        }])
        .unwrap();

    let matching = crate::hooks::plan_event(
        &service.hook_definitions,
        HookEvent::UserPromptSubmit,
        r#"{"pane_id":"pane-2"}"#,
    )
    .unwrap();
    let fallback = crate::hooks::plan_event(
        &service.hook_definitions,
        HookEvent::UserPromptSubmit,
        r#"{"agent_id":"agent-1"}"#,
    )
    .unwrap();
    let filtered = crate::hooks::plan_event(
        &service.hook_definitions,
        HookEvent::UserPromptSubmit,
        r#"{"pane_id":"other","agent_id":"agent-2"}"#,
    )
    .unwrap();

    assert_eq!(report.hooks_configured, 1);
    assert_eq!(service.hook_definitions[0].matcher_groups.len(), 2);
    assert_eq!(matching.plans.len(), 1);
    assert_eq!(fallback.plans.len(), 1);
    assert!(filtered.plans.is_empty());
}

/// Verifies runtime config reload reloads layers and applies live policy.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_config_reload_reloads_layers_and_applies_live_policy() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-config-reload");
    let path = root.join("config.toml");
    fs::write(&path, "[permissions]\napproval_policy = \"full-access\"\n").unwrap();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: Some(path.clone()),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: fs::read_to_string(&path).unwrap(),
        }])
        .unwrap();
    let audit_root = temp_root("runtime-config-audit");
    let audit_path = audit_root.join("audit.jsonl");
    service.set_audit_log(AuditLog::new(crate::audit::AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: false,
        required: true,
    }));
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::FullAccess
    );

    fs::write(
        &path,
        "[permissions]\napproval_policy = \"ask\"\n[[permissions.command_rules]]\npattern = [\"cargo\", \"test\"]\ndecision = \"allow\"\nscope = \"session\"\nmatch = \"prefix\"\n",
    )
    .unwrap();
    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"reload","method":"config/reload","params":{"idempotency_key":"reload-live-config"}}"#,
        &primary,
    );

    assert!(response.contains(r#""operation":"reload""#), "{response}");
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::Ask
    );
    assert_eq!(
        service
            .permission_policy()
            .evaluate_shell_command("cargo test --all-targets"),
        RuleDecision::Allow
    );
    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains(r#""event_type":"configuration""#), "{audit}");
    assert!(audit.contains(r#""action":"reload""#), "{audit}");
    assert!(audit.contains(r#""outcome":"started""#), "{audit}");
    assert!(audit.contains(r#""outcome":"applied""#), "{audit}");
    assert!(audit.contains(r#""event_type":"permission""#), "{audit}");
    assert!(
        audit.contains(r#""permission_id":"permissions.approval_policy""#),
        "{audit}"
    );
    assert!(
        audit.contains(r#""permission_id":"permissions.command_rules""#),
        "{audit}"
    );
    assert!(
        audit.contains(r#""action_kind":"config_reload""#),
        "{audit}"
    );
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(audit_root);
    let _ = fs::remove_dir_all(root);
}

/// Verifies that live runtime `config/set` and `config/unset` requests apply
/// the spec-defined `PersistTarget` vocabulary directly to the running service.
/// This protects the control API from returning offline planning placeholders
/// when a primary client asks for a non-persistent live configuration change.
#[test]
fn runtime_control_config_live_persist_target_mutates_live_override() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let audit_root = temp_root("runtime-live-config-audit");
    let audit_path = audit_root.join("audit.jsonl");
    service.set_audit_log(AuditLog::new(crate::audit::AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: false,
        required: true,
    }));

    let set = r#"{"jsonrpc":"2.0","id":"live-set","method":"config/set","params":{"path":"history.lines","value":5,"persist":{"scope":"live"},"idempotency_key":"live-history"}}"#;
    let first = service.dispatch_runtime_control_body(set, &primary);
    let first_json: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(first_json["result"]["applied"], true, "{first}");
    assert_eq!(first_json["result"]["persisted"], false, "{first}");
    assert_eq!(first_json["result"]["plan"]["scope"], "live", "{first}");
    assert_eq!(
        first_json["result"]["plan"]["target"]["scope"], "live",
        "{first}"
    );
    assert_eq!(service.terminal_history_limit(), 5);
    assert_eq!(service.session.config_generation, 1);

    let second = service.dispatch_runtime_control_body(set, &primary);
    assert_eq!(first, second);
    assert_eq!(service.control_idempotency().len(), 1);
    assert_eq!(service.session.config_generation, 1);

    let conflict = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"live-conflict","method":"config/set","params":{"path":"history.lines","value":6,"persist":{"scope":"live"},"idempotency_key":"live-history"}}"#,
        &primary,
    );
    assert!(
        conflict.contains(r#""mezzanine_code":"conflict""#),
        "{conflict}"
    );
    assert_eq!(service.terminal_history_limit(), 5);
    assert_eq!(service.session.config_generation, 1);

    let null_persist = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"live-null","method":"config/set","params":{"path":"history.lines","value":6,"persist":null,"idempotency_key":"live-null-history"}}"#,
        &primary,
    );
    assert!(
        null_persist.contains(r#""target":{"scope":"live","path":null}"#),
        "{null_persist}"
    );
    assert!(
        null_persist.contains(r#""persisted":false"#),
        "{null_persist}"
    );
    assert_eq!(service.terminal_history_limit(), 6);
    assert_eq!(service.session.config_generation, 2);

    let unset = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"live-unset","method":"config/unset","params":{"path":"history.lines","persist":{"scope":"live"},"idempotency_key":"live-history-unset"}}"#,
        &primary,
    );
    assert!(unset.contains(r#""applied":true"#), "{unset}");
    assert_eq!(service.session.config_generation, 3);
    assert_ne!(service.terminal_history_limit(), 6);

    let primary_scope = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"primary-scope","method":"config/set","params":{"path":"history.lines","value":7,"persist":{"scope":"primary"},"idempotency_key":"primary-scope"}}"#,
        &primary,
    );
    assert!(primary_scope.contains(r#""mezzanine_code":"invalid_params""#));
    assert!(primary_scope.contains("must be live, user, or project"));

    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains(r#""event_type":"configuration""#), "{audit}");
    assert!(audit.contains(r#""action":"set""#), "{audit}");
    assert!(audit.contains(r#""outcome":"started""#), "{audit}");
    assert!(audit.contains(r#""outcome":"applied""#), "{audit}");
    assert!(audit.contains(r#""scope":"live""#), "{audit}");
    let _ = fs::remove_dir_all(audit_root);
}

/// Verifies that runtime user config persistence is confined to the configured
/// private config root or the active primary layer. This prevents control
/// clients from using `scope = user` as a general-purpose file write primitive.
#[test]
fn runtime_control_config_user_persistence_requires_user_private_target() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-user-config-persist");
    let config_root = root.join("config");
    let config_path = config_root.join("config.toml");
    fs::create_dir_all(&config_root).unwrap();
    fs::write(&config_path, "[history]\nlines = 10\n").unwrap();
    service.set_config_root(config_root.clone());
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: Some(config_path.clone()),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: fs::read_to_string(&config_path).unwrap(),
        }])
        .unwrap();

    let outside_path = root.join("outside.toml");
    fs::write(&outside_path, "[history]\nlines = 10\n").unwrap();
    let rejected = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"user-outside","method":"config/set","params":{{"path":"history.lines","value":7,"persist":{{"scope":"user","path":"{}"}},"idempotency_key":"user-outside"}}}}"#,
            json_escape(&outside_path.to_string_lossy())
        ),
        &primary,
    );
    assert!(
        rejected.contains(r#""mezzanine_code":"invalid_params""#),
        "{rejected}"
    );
    assert!(
        rejected.contains("configured user-private config root"),
        "{rejected}"
    );
    assert!(
        fs::read_to_string(&outside_path)
            .unwrap()
            .contains("lines = 10")
    );
    assert_eq!(service.terminal_history_limit(), 10);

    let allowed = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"user-inside","method":"config/set","params":{{"path":"history.lines","value":7,"persist":{{"scope":"user","path":"{}"}},"idempotency_key":"user-inside"}}}}"#,
            json_escape(&config_path.to_string_lossy())
        ),
        &primary,
    );
    assert!(allowed.contains(r#""applied":true"#), "{allowed}");
    assert!(allowed.contains(r#""persisted":true"#), "{allowed}");
    assert_eq!(service.terminal_history_limit(), 7);
    assert!(
        fs::read_to_string(&config_path)
            .unwrap()
            .contains("lines = 7")
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies that runtime project config persistence blocks until the target
/// path is covered by a trusted project-root decision. This keeps project
/// overlays from being written before the primary client has accepted the
/// project trust boundary.
#[test]
fn runtime_control_config_project_persistence_requires_trusted_root() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-project-config-persist");
    fs::create_dir_all(root.join(".git")).unwrap();
    let project_config_dir = root.join(".mezzanine");
    let project_path = project_config_dir.join("config.toml");
    fs::create_dir_all(&project_config_dir).unwrap();
    fs::write(&project_path, "[history]\nlines = 10\n").unwrap();
    service.set_project_trust_store(ProjectTrustStore::default(), None);
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "project".to_string(),
            path: Some(project_path.clone()),
            format: ConfigFormat::Toml,
            scope: ConfigScope::ProjectOverlay,
            trusted: true,
            text: fs::read_to_string(&project_path).unwrap(),
        }])
        .unwrap();

    let pending = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"project-pending","method":"config/set","params":{{"path":"history.lines","value":7,"persist":{{"scope":"project","path":"{}"}},"idempotency_key":"project-pending"}}}}"#,
            json_escape(&project_path.to_string_lossy())
        ),
        &primary,
    );
    assert!(
        pending.contains(r#""mezzanine_code":"conflict""#),
        "{pending}"
    );
    assert!(
        pending.contains("blocked until project trust is decided"),
        "{pending}"
    );
    assert!(
        fs::read_to_string(&project_path)
            .unwrap()
            .contains("lines = 10")
    );

    let mut trust_store = ProjectTrustStore::default();
    trust_store
        .decide_at(root.clone(), TrustDecision::Trusted, None, 42)
        .unwrap();
    service.set_project_trust_store(trust_store, None);
    let trusted = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"project-trusted","method":"config/set","params":{{"path":"history.lines","value":7,"persist":{{"scope":"project","path":"{}"}},"idempotency_key":"project-trusted"}}}}"#,
            json_escape(&project_path.to_string_lossy())
        ),
        &primary,
    );
    assert!(trusted.contains(r#""applied":true"#), "{trusted}");
    assert!(trusted.contains(r#""persisted":true"#), "{trusted}");
    assert_eq!(service.terminal_history_limit(), 7);
    assert!(
        fs::read_to_string(&project_path)
            .unwrap()
            .contains("lines = 7")
    );

    let outside_path = temp_root("runtime-project-config-outside").join("config.toml");
    let outside = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"project-outside","method":"config/set","params":{{"path":"history.lines","value":5,"persist":{{"scope":"project","path":"{}"}},"idempotency_key":"project-outside"}}}}"#,
            json_escape(&outside_path.to_string_lossy())
        ),
        &primary,
    );
    assert!(
        outside.contains(r#""mezzanine_code":"conflict""#),
        "{outside}"
    );
    assert!(
        outside.contains("blocked until project trust is decided"),
        "{outside}"
    );
    let _ = fs::remove_dir_all(outside_path.parent().unwrap());
    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime config reload applies history limit to live screens.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_config_reload_applies_history_limit_to_live_screens() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-history-reload");
    let path = root.join("config.toml");
    fs::write(&path, "[history]\nlines = 4\nrotate_lines = 2\n").unwrap();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: Some(path.clone()),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: fs::read_to_string(&path).unwrap(),
        }])
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(12, 2).unwrap(), 4).unwrap();
    screen.restore_normal_content(
        &["one".to_string(), "two".to_string(), "three".to_string()],
        &[],
    );
    service.pane_screens.insert("%1".to_string(), screen);

    fs::write(&path, "[history]\nlines = 2\nrotate_lines = 3\n").unwrap();
    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"reload","method":"config/reload","params":{"idempotency_key":"reload-history-limit"}}"#,
        &primary,
    );

    assert!(response.contains(r#""operation":"reload""#), "{response}");
    assert_eq!(service.terminal_history_limit(), 2);
    assert_eq!(service.terminal_history_rotate_lines(), 3);
    let screen = service.pane_screen("%1").unwrap();
    assert_eq!(screen.history_limit(), 2);
    assert_eq!(screen.history_rotate_lines(), 3);
    assert_eq!(
        screen.history().lines().collect::<Vec<_>>(),
        vec!["two", "three"]
    );
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime config reload applies agent scheduler limit.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_config_reload_applies_agent_scheduler_limit() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-scheduler-reload");
    let path = root.join("config.toml");
    fs::write(&path, "[agents]\nmax_concurrent_agents = 2\n").unwrap();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: Some(path.clone()),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: fs::read_to_string(&path).unwrap(),
        }])
        .unwrap();
    service
        .agent_scheduler_mut()
        .enqueue(ScheduledWork {
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            pane_id: Some("%1".to_string()),
            kind: ScheduledWorkKind::ShellCapable,
        })
        .unwrap();
    service
        .agent_scheduler_mut()
        .enqueue(ScheduledWork {
            turn_id: "turn-2".to_string(),
            agent_id: "agent-2".to_string(),
            pane_id: Some("%2".to_string()),
            kind: ScheduledWorkKind::ShellCapable,
        })
        .unwrap();
    assert_eq!(
        service.agent_scheduler_mut().start_ready().unwrap().turn_id,
        "turn-1"
    );
    assert_eq!(
        service.agent_scheduler_mut().start_ready().unwrap().turn_id,
        "turn-2"
    );

    fs::write(&path, "[agents]\nmax_concurrent_agents = 1\n").unwrap();
    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"reload","method":"config/reload","params":{"idempotency_key":"reload-scheduler-limit"}}"#,
        &primary,
    );

    assert!(response.contains(r#""operation":"reload""#), "{response}");
    let snapshot = service.agent_scheduler().snapshot();
    assert_eq!(snapshot.max_concurrent_agents, 1);
    assert_eq!(snapshot.running, 2);
    assert!(service.agent_scheduler_mut().start_ready().is_none());
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime config reload applies the model-correction retry budget.
///
/// Action-failure recovery is intentionally bounded so a repeated bad action
/// cannot loop forever, but the bound must be configurable for providers and
/// tasks that need more than the default repair attempts.
#[test]
fn runtime_config_reload_applies_action_failure_retry_limit() {
    let mut service = test_runtime_service();
    assert_eq!(service.agent_action_failure_retry_limit, 5);
    let root = temp_root("runtime-action-failure-retry-limit");
    let path = root.join("config.toml");
    fs::write(&path, "[agents]\naction_failure_retry_limit = 2\n").unwrap();

    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: Some(path.clone()),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: fs::read_to_string(&path).unwrap(),
        }])
        .unwrap();

    assert_eq!(service.agent_action_failure_retry_limit, 2);
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime config reload applies custom system prompts and default
/// personality profiles.
///
/// These values are intentionally runtime-owned preferences: configured system
/// prompt text must enter the provider request as system context, while a
/// default personality profile can supply response-style and planning guidance
/// without requiring a user to run `/personality` in every pane.
#[test]
fn runtime_config_reload_applies_agent_prompt_and_personality_profiles() {
    let mut service = test_runtime_service();
    let root = temp_root("runtime-agent-personality-config");
    let path = root.join("config.toml");
    fs::write(
        &path,
        "[agents]\ncustom_system_prompt = \"Always preserve user work.\"\ndefault_personality = \"careful\"\n[personalities.careful]\nname = \"Careful\"\nsystem_prompt = \"Be exact about evidence.\"\nresponse_style = \"terse\"\nplanning_enabled = true\nauto_reasoning_enabled = true\n",
    )
    .unwrap();

    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: Some(path.clone()),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: fs::read_to_string(&path).unwrap(),
        }])
        .unwrap();

    assert_eq!(
        service.custom_agent_system_prompt.as_deref(),
        Some("Always preserve user work.")
    );
    assert_eq!(
        service.default_agent_personality.as_deref(),
        Some("careful")
    );
    assert_eq!(service.agent_personality_profiles.len(), 1);

    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let started = service
        .start_agent_prompt_turn("%1", "summarize the change")
        .unwrap();
    let context = service
        .agent_turn_contexts
        .get(&started.turn_id)
        .expect("started turn should retain provider context");
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::System
            && block.label == "configured agent system prompt"
            && block.content.contains("Always preserve user work")
    }));
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::System
            && block.label == "agent personality system prompt"
            && block.content.contains("Be exact about evidence")
    }));
    assert!(context.blocks.iter().any(|block| {
        block.label == "agent shell plan mode" && block.content.contains("Planning mode is active")
    }));
    assert!(context.blocks.iter().any(|block| {
        block.label == "agent shell plan mode"
            && block
                .content
                .contains("Do not use a visible plan when the next safe inspection")
    }));
    assert!(!context.blocks.iter().any(|block| {
        block.label == "agent shell plan mode"
            && block.content.contains("Start by presenting a concise")
    }));
    assert!(context.blocks.iter().any(|block| {
        block.label == "agent shell personality"
            && block.content.contains("Response style preference")
            && block.content.contains("terse")
    }));

    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(root);
}

/// Verifies that subagent wait policy is a validated live agent option.
///
/// The default must remain join-and-wait so parent turns do not race ahead of
/// delegated work, while explicit `detach` configuration remains available for
/// workflows that want fire-and-forget delegation. Invalid values must fail
/// config application with a diagnosable error rather than silently changing
/// scheduler semantics.
#[test]
fn runtime_config_reload_applies_subagent_wait_policy() {
    let mut service = test_runtime_service();
    assert_eq!(service.subagent_wait_policy, SubagentWaitPolicy::Join);

    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\nsubagent_wait_policy = \"detach\"\n".to_string(),
        }])
        .unwrap();
    assert_eq!(service.subagent_wait_policy, SubagentWaitPolicy::Detach);

    let error = service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\nsubagent_wait_policy = \"invalid\"\n".to_string(),
        }])
        .unwrap_err();
    assert!(
        error.message().contains("unsupported subagent wait policy"),
        "{error}"
    );
}

/// Verifies that subagent width and depth limits are live agent options.
///
/// Delegation capacity is runtime scheduling policy rather than static config
/// metadata. Reloading these values must update the service immediately so
/// subsequent control and MAAP spawns apply the same current limits without
/// restarting the session.
#[test]
fn runtime_config_reload_applies_subagent_capacity_limits() {
    let mut service = test_runtime_service();

    assert_eq!(service.max_root_subagents, 4);
    assert_eq!(service.max_subagents_per_subagent, 2);
    assert_eq!(service.max_subagent_depth, 2);

    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text:
                "[agents]\nmax_root_subagents = 6\nmax_subagents_per_subagent = 3\nmax_depth = 4\n"
                    .to_string(),
        }])
        .unwrap();

    assert_eq!(service.max_root_subagents, 6);
    assert_eq!(service.max_subagents_per_subagent, 3);
    assert_eq!(service.max_subagent_depth, 4);

    let error = service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\nmax_root_subagents = 0\n".to_string(),
        }])
        .unwrap_err();
    assert!(
        error
            .message()
            .contains("agents.max_root_subagents must be a positive integer"),
        "{error}"
    );
}

/// Verifies the runtime applies compaction threshold and raw-retention config.
///
/// Both values are live agent options: the trigger threshold decides when
/// compaction starts, while the raw-retention percentage decides how much exact
/// recent context remains after compaction.
#[test]
fn runtime_config_reload_applies_compaction_thresholds() {
    let mut service = test_runtime_service();

    assert_eq!(service.agent_auto_compact_threshold, 0.95);
    assert_eq!(service.agent_compaction_raw_retention_percent, 10);

    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text:
                "[agents]\nauto_compact_threshold = 0.80\ncompaction_raw_retention_percent = 25\n"
                    .to_string(),
        }])
        .unwrap();

    assert_eq!(service.agent_auto_compact_threshold, 0.80);
    assert_eq!(service.agent_compaction_raw_retention_percent, 25);
}

/// Verifies that a live config reload starts queued agent work when the new
/// scheduler limit makes that work runnable. Updating the limit without
/// draining newly available scheduler capacity would leave prompt turns queued
/// until some unrelated turn completion nudged the scheduler.
#[test]
fn runtime_config_reload_starts_newly_runnable_agent_turns() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-scheduler-reload-start-ready");
    let path = root.join("config.toml");
    fs::write(&path, "[agents]\nmax_concurrent_agents = 1\n").unwrap();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: Some(path.clone()),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: fs::read_to_string(&path).unwrap(),
        }])
        .unwrap();
    let second_pane = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    service.session.select_pane(&primary, "%1").unwrap();
    for pane_id in ["%1", second_pane.as_str()] {
        let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
        screen.feed(b"ready\n");
        service.pane_screens.insert(pane_id.to_string(), screen);
        service
            .agent_shell_store_mut()
            .enter_or_resume(pane_id)
            .unwrap();
    }

    let first = service.start_agent_prompt_turn("%1", "first").unwrap();
    let second = service
        .start_agent_prompt_turn(second_pane.as_str(), "second")
        .unwrap();
    assert_eq!(first.state, AgentTurnState::Running);
    assert_eq!(second.state, AgentTurnState::Queued);
    assert_eq!(service.agent_scheduler().snapshot().running, 1);
    assert_eq!(service.agent_scheduler().snapshot().queued, 1);

    fs::write(&path, "[agents]\nmax_concurrent_agents = 2\n").unwrap();
    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"reload","method":"config/reload","params":{"idempotency_key":"reload-scheduler-start-ready"}}"#,
        &primary,
    );

    assert!(response.contains(r#""operation":"reload""#), "{response}");
    assert_eq!(service.agent_scheduler().snapshot().running, 2);
    assert_eq!(service.agent_scheduler().snapshot().queued, 0);
    assert_eq!(
        service
            .agent_shell_store()
            .get(second_pane.as_str())
            .and_then(|session| session.running_turn_id.as_deref()),
        Some("turn-2")
    );
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == "turn-2"),
    );
    service.kill_session(&primary, true).unwrap();
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(root);
}

/// Verifies that live prompt submission drains scheduler capacity through the
/// same fairness policy as the scheduler queue. A blocked same-pane turn at
/// the head of the queue must not prevent a later prompt for an independent
/// pane from starting when the global concurrency limit still has capacity.
#[test]
fn runtime_prompt_submission_starts_ready_work_behind_blocked_queue_head() {
    let mut service = test_runtime_service();
    service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(2)
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let second_pane = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    service.session.select_pane(&primary, "%1").unwrap();
    for pane_id in ["%1", second_pane.as_str()] {
        let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
        screen.feed(b"ready\n");
        service.pane_screens.insert(pane_id.to_string(), screen);
        service
            .agent_shell_store_mut()
            .enter_or_resume(pane_id)
            .unwrap();
    }

    let first = service.start_agent_prompt_turn("%1", "first").unwrap();
    let blocked_same_pane = service.start_agent_prompt_turn("%1", "second").unwrap();
    let independent = service
        .start_agent_prompt_turn(second_pane.as_str(), "third")
        .unwrap();

    assert_eq!(first.state, AgentTurnState::Running);
    assert_eq!(blocked_same_pane.state, AgentTurnState::Queued);
    assert_eq!(independent.state, AgentTurnState::Running);
    assert_eq!(service.agent_scheduler().snapshot().running, 2);
    assert_eq!(service.agent_scheduler().snapshot().queued, 1);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        Some("turn-1")
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get(second_pane.as_str())
            .and_then(|session| session.running_turn_id.as_deref()),
        Some("turn-3")
    );
    assert_eq!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == "turn-2")
            .map(|turn| turn.state),
        Some(AgentTurnState::Queued)
    );
    assert_eq!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == "turn-3")
            .map(|turn| turn.state),
        Some(AgentTurnState::Running)
    );
    let pending = service.pending_agent_provider_tasks();
    assert!(pending.iter().any(|task| task.turn_id == "turn-1"));
    assert!(pending.iter().any(|task| task.turn_id == "turn-3"));
    assert!(!pending.iter().any(|task| task.turn_id == "turn-2"));
    service.kill_session(&primary, true).unwrap();
}

/// Verifies that stopping a queued pane-local agent turn does not depend on the
/// pane shell store having that queued turn as the active running turn. This
/// covers the queued cleanup path used when global scheduler capacity is full.
#[test]
fn runtime_stop_agent_turn_cleans_up_queued_turn_without_shell_running_marker() {
    let mut service = test_runtime_service();
    service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(1)
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let second_pane = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    for pane_id in ["%1", second_pane.as_str()] {
        let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
        screen.feed(b"ready\n");
        service.pane_screens.insert(pane_id.to_string(), screen);
        service
            .agent_shell_store_mut()
            .enter_or_resume(pane_id)
            .unwrap();
    }

    let first = service.start_agent_prompt_turn("%1", "first").unwrap();
    let second = service
        .start_agent_prompt_turn(second_pane.as_str(), "second")
        .unwrap();
    assert_eq!(first.state, AgentTurnState::Running);
    assert_eq!(second.state, AgentTurnState::Queued);

    let stopped = service
        .stop_agent_turn_for_pane(second_pane.as_str())
        .unwrap();

    assert_eq!(stopped.turn_id, "turn-2");
    assert!(stopped.scheduler_cancelled);
    assert_eq!(service.agent_scheduler().snapshot().queued, 0);
    assert_eq!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == "turn-2")
            .map(|turn| turn.state),
        Some(AgentTurnState::Interrupted)
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get(second_pane.as_str())
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    service.kill_session(&primary, true).unwrap();
}

/// Verifies that runtime configuration can initialize the audit writer from
/// `[audit]` settings. The path is resolved under the configured Mezzanine
/// config root when relative, and subsequent auditable runtime actions write
/// JSONL records through the configured hash-chain and retention modes.
#[test]
fn runtime_applies_audit_log_from_config_layers() {
    let mut service = test_runtime_service();
    let root = temp_root("runtime-audit-config");
    let config_root = root.join("config");
    service.set_config_root(config_root.clone());
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[audit]\nenabled = true\npath = \"security/audit.jsonl\"\nformat = \"jsonl\"\nretention_days = 1\nhash_chain = true\nrequired = true\n".to_string(),
        }])
        .unwrap();
    let audit_path = config_root.join("security/audit.jsonl");
    assert_eq!(service.audit_log().unwrap().path(), audit_path.as_path());
    fs::create_dir_all(audit_path.parent().unwrap()).unwrap();
    fs::write(
        &audit_path,
        "{\"timestamp\":\"unix:1\",\"action\":\"old\"}\n",
    )
    .unwrap();

    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let output = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"audit-approval","method":"agent/shell/command","params":{"idempotency_key":"audit-approval","input":"/approval full-access"}}"#,
        &primary,
    );

    assert!(output.contains("changed=true"), "{output}");
    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains(r#""event_type":"permission""#), "{audit}");
    assert!(audit.contains(r#""hash":"#), "{audit}");
    assert!(!audit.contains(r#""action":"old""#), "{audit}");
    let _ = fs::remove_dir_all(root);
}

/// Verifies that invalid audit retention configuration fails before replacing
/// the runtime audit writer. A zero-day retention window would immediately
/// discard useful audit history, so the config layer is rejected instead of
/// silently enabling destructive pruning.
#[test]
fn runtime_rejects_invalid_audit_retention_days() {
    let mut service = test_runtime_service();
    let error = service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[audit]\nenabled = true\nretention_days = 0\n".to_string(),
        }])
        .unwrap_err();

    assert!(error.message().contains("audit.retention_days"), "{error}");
    assert!(service.audit_log().is_none());
}

/// Verifies that unknown project-trust method names do not enter the runtime's
/// project-trust dispatcher. Unsupported names must remain ordinary JSON-RPC
/// method-not-found errors rather than reporting a project-trust implementation
/// placeholder, because only the advertised project trust methods are valid.
#[test]
fn runtime_unknown_project_trust_method_uses_generic_method_not_found() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"unknown","method":"project/trust/archive","params":{}}"#,
        &primary,
    );

    assert!(
        response.contains(r#""mezzanine_code":"method_not_found""#),
        "{response}"
    );
    assert!(
        response.contains("unknown control method `project/trust/archive`"),
        "{response}"
    );
    assert!(!response.contains("project trust method"), "{response}");
}

/// Verifies runtime project trust decision applies and removes project overlays.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_project_trust_decision_applies_and_removes_project_overlays() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-project-trust");
    let audit_root = temp_root("runtime-project-trust-audit");
    let audit_path = audit_root.join("audit.jsonl");
    service.set_audit_log(AuditLog::new(crate::audit::AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: false,
        required: true,
    }));
    fs::create_dir_all(root.join(".git")).unwrap();
    let overlay_dir = root.join(".mezzanine");
    fs::create_dir_all(&overlay_dir).unwrap();
    let overlay_path = overlay_dir.join("config.toml");
    fs::write(
        &overlay_path,
        "[history]\nlines = 7\n[permissions]\napproval_policy = \"ask\"\n",
    )
    .unwrap();
    let trust_path = root.join("trust.tsv");
    service.set_project_trust_store(ProjectTrustStore::default(), Some(trust_path.clone()));
    let initial_report = service
        .replace_config_layers(vec![
            ConfigLayer {
                name: "primary".to_string(),
                path: None,
                format: ConfigFormat::Toml,
                scope: ConfigScope::Primary,
                trusted: true,
                text: "[history]\nlines = 3\n".to_string(),
            },
            ConfigLayer {
                name: "project".to_string(),
                path: Some(overlay_path.clone()),
                format: ConfigFormat::Toml,
                scope: ConfigScope::ProjectOverlay,
                trusted: false,
                text: fs::read_to_string(&overlay_path).unwrap(),
            },
        ])
        .unwrap();
    assert_eq!(initial_report.project_trust_prompts_announced, 1);
    assert_eq!(service.terminal_history_limit(), 3);
    let primary_events = service
        .event_log
        .as_ref()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(primary_events.iter().any(|event| {
        event.kind == EventKind::ConfigChanged
            && event.payload.contains(r#""state":"pending""#)
            && event
                .payload
                .contains(r#""blocks_until_primary_decision":true"#)
            && event
                .payload
                .contains(&json_escape(&root.to_string_lossy()))
    }));
    assert_eq!(
        service
            .apply_runtime_config_layers()
            .unwrap()
            .project_trust_prompts_announced,
        0
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let blocked_prompt = service
        .execute_agent_shell_command(&primary, "summarize this project")
        .unwrap();
    assert!(
        blocked_prompt.contains(r#""kind":"display""#)
            && blocked_prompt.contains("agent command error: project trust decision pending")
            && blocked_prompt.contains("(conflict)"),
        "{blocked_prompt}"
    );
    assert!(
        blocked_prompt.contains("project trust decision pending"),
        "{blocked_prompt}"
    );
    assert!(service.agent_turn_ledger.turns().is_empty());

    let trust = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"trust","method":"project/trust/decide","params":{{"project_root":"{}","decision":"trust","idempotency_key":"trust-project"}}}}"#,
            json_escape(&root.to_string_lossy())
        ),
        &primary,
    );

    assert!(trust.contains(r#""state":"trusted""#), "{trust}");
    assert!(trust.contains(r#""trusted_at":""#), "{trust}");
    assert!(
        trust.contains(&format!(r#""decided_by_client_id":"{}""#, primary)),
        "{trust}"
    );
    assert!(!trust.contains(r#""trusted_at":"unix:"#), "{trust}");
    assert!(trust.contains(r#""changed_layers":["project"]"#), "{trust}");
    assert!(
        trust.contains(&json_escape(&overlay_path.to_string_lossy())),
        "{trust}"
    );
    assert!(
        trust.contains(&format!(
            r#""overlay_files":[{{"path":"{}","format":"toml","applied":true,"diagnostics":[]}}]"#,
            json_escape(&overlay_path.to_string_lossy())
        )),
        "{trust}"
    );
    assert!(
        trust.contains(r#""capability_expansion_summary":["permissions"]"#),
        "{trust}"
    );
    assert_eq!(service.terminal_history_limit(), 7);
    assert!(service.config_layers()[1].trusted);
    assert!(trust_path.exists());

    let trusted_list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"trusted-list","method":"project/trust/list","params":{"state":"trusted"}}"#,
        &primary,
    );
    assert!(
        trusted_list.contains(&json_escape(&root.to_string_lossy())),
        "{trusted_list}"
    );

    let pending_list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"pending-list","method":"project/trust/list","params":{"state":"pending"}}"#,
        &primary,
    );
    assert!(
        !pending_list.contains(&json_escape(&root.to_string_lossy())),
        "{pending_list}"
    );

    let invalid_list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"invalid-list","method":"project/trust/list","params":{"state":"unknown"}}"#,
        &primary,
    );
    assert!(
        invalid_list.contains(r#""mezzanine_code":"invalid_params""#),
        "{invalid_list}"
    );

    let revoke = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"revoke","method":"project/trust/revoke","params":{{"project_root":"{}","idempotency_key":"revoke-project"}}}}"#,
            json_escape(&root.to_string_lossy())
        ),
        &primary,
    );

    assert!(revoke.contains(r#""state":"revoked""#), "{revoke}");
    assert!(
        revoke.contains(&format!(r#""decided_by_client_id":"{}""#, primary)),
        "{revoke}"
    );

    let revoked_list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"revoked-list","method":"project/trust/list","params":{"state":"revoked"}}"#,
        &primary,
    );
    assert!(
        revoked_list.contains(&json_escape(&root.to_string_lossy())),
        "{revoked_list}"
    );

    assert_eq!(service.terminal_history_limit(), 3);
    assert!(!service.config_layers()[1].trusted);

    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains(r#""event_type":"configuration""#), "{audit}");
    assert!(audit.contains(r#""scope":"project_trust""#), "{audit}");
    assert!(audit.contains(r#""decision":"trusted""#), "{audit}");
    assert!(audit.contains(r#""decision":"revoked""#), "{audit}");
    assert!(audit.contains(r#""project_root""#), "{audit}");
    let _ = fs::remove_dir_all(audit_root);
    let _ = fs::remove_dir_all(root);
}

/// Verifies agent work refreshes project overlays from the active pane's cwd.
///
/// The daemon may start outside the repository. Before an agent prompt runs,
/// the runtime should discover `.mezzanine/config.*` under the pane project,
/// block for trust, apply the trusted overlay, and expose trusted project
/// skills through the same catalog used by `/list-skills` and `$skill`.
#[test]
fn runtime_agent_prompt_refreshes_project_overlay_and_project_skills_from_pane_cwd() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-project-refresh");
    let config_root = root.join("config-root");
    let project_root = root.join("repo");
    let nested = project_root.join("src");
    let overlay_dir = project_root.join(".mezzanine");
    let skill_dir = overlay_dir.join("skills/review");
    fs::create_dir_all(project_root.join(".git")).unwrap();
    fs::create_dir_all(&nested).unwrap();
    fs::create_dir_all(&skill_dir).unwrap();
    let overlay_path = overlay_dir.join("config.toml");
    fs::write(&overlay_path, "[history]\nlines = 11\n").unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: review\ndescription: Project review workflow\n---\n\nReview this repository.\n",
    )
    .unwrap();
    service.set_config_root(config_root.clone());
    service.set_project_trust_store(
        ProjectTrustStore::default(),
        Some(config_root.join("project-trust.tsv")),
    );
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[history]\nlines = 3\n".to_string(),
        }])
        .unwrap();
    service
        .pane_current_working_directories
        .insert("%1".to_string(), nested.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let blocked_prompt = service
        .execute_agent_shell_command(&primary, "summarize this project")
        .unwrap();
    assert!(
        blocked_prompt.contains("project trust decision pending"),
        "{blocked_prompt}"
    );
    assert!(service.agent_turn_ledger.turns().is_empty());
    assert_eq!(service.terminal_history_limit(), 3);
    assert!(
        service
            .config_layers()
            .iter()
            .any(|layer| layer.path.as_ref() == Some(&overlay_path) && !layer.trusted)
    );

    let trust = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"trust-refresh","method":"project/trust/decide","params":{{"project_root":"{}","decision":"trust","idempotency_key":"trust-refresh"}}}}"#,
            json_escape(&project_root.to_string_lossy())
        ),
        &primary,
    );
    assert!(trust.contains(r#""state":"trusted""#), "{trust}");
    assert_eq!(service.terminal_history_limit(), 11);

    let skills = service
        .execute_agent_shell_command(&primary, "/list-skills")
        .unwrap();
    assert!(skills.contains("Project review workflow"), "{skills}");
    assert!(
        skills.contains("| `$review` | project | Project review workflow |"),
        "{skills}"
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime agent trust command logs and persists project trust request.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_agent_trust_command_logs_and_persists_project_trust_request() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-agent-trust-command");
    let config_root = root.join("config-root");
    let trust_path = config_root.join("project-trust.tsv");
    service.set_config_root(config_root.clone());
    fs::create_dir_all(root.join(".git")).unwrap();
    let overlay_dir = root.join(".mezzanine");
    fs::create_dir_all(&overlay_dir).unwrap();
    let overlay_path = overlay_dir.join("config.toml");
    fs::write(
        &overlay_path,
        "[history]\nlines = 11\n[permissions]\napproval_policy = \"ask\"\n",
    )
    .unwrap();
    service.set_project_trust_store(ProjectTrustStore::default(), None);
    let initial_report = service
        .replace_config_layers(vec![
            ConfigLayer {
                name: "primary".to_string(),
                path: None,
                format: ConfigFormat::Toml,
                scope: ConfigScope::Primary,
                trusted: true,
                text: "[history]\nlines = 3\n".to_string(),
            },
            ConfigLayer {
                name: "project".to_string(),
                path: Some(overlay_path.clone()),
                format: ConfigFormat::Toml,
                scope: ConfigScope::ProjectOverlay,
                trusted: false,
                text: fs::read_to_string(&overlay_path).unwrap(),
            },
        ])
        .unwrap();
    assert_eq!(initial_report.project_trust_prompts_announced, 1);
    let primary_events = service
        .event_log
        .as_ref()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(
        primary_events.iter().any(|event| {
            event.kind == EventKind::ConfigChanged
                && event.payload.contains(r#""trust_command":"/trust "#)
                && event
                    .payload
                    .contains(&json_escape(&root.to_string_lossy()))
        }),
        "{primary_events:?}"
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let blocked_prompt = service
        .execute_agent_shell_command(&primary, "summarize this project")
        .unwrap();
    assert!(
        blocked_prompt.contains(r#""kind":"display""#)
            && blocked_prompt.contains("agent command error: project trust decision pending")
            && blocked_prompt.contains("(conflict)"),
        "{blocked_prompt}"
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("project trust pending:"), "{pane_text}");
    let collapsed_agent_wraps = pane_text.replace("\n▐ ", "");
    assert!(collapsed_agent_wraps.contains("/trust"), "{pane_text}");

    let trust = service
        .execute_agent_shell_command(&primary, "/trust")
        .unwrap();

    assert!(trust.contains(r#""kind":"mutated""#), "{trust}");
    assert!(trust.contains(r#""command":"trust""#), "{trust}");
    assert!(trust.contains("project trust granted"), "{trust}");
    assert!(trust.contains("persisted=true"), "{trust}");
    assert_eq!(service.terminal_history_limit(), 11);
    assert!(service.config_layers()[1].trusted);
    assert!(trust_path.exists());
    let persisted = ProjectTrustStore::load_from_file(&trust_path).unwrap();
    assert_eq!(persisted.get(&root).unwrap().state, TrustDecision::Trusted);
    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime config applies safe terminal term to new panes.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_config_applies_safe_terminal_term_to_new_panes() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[terminal]\nterm = \"screen-256color\"\n".to_string(),
        }])
        .unwrap();
    let output = std::env::temp_dir().join(format!("mez-runtime-term-test-{}", std::process::id()));
    let _ = fs::remove_file(&output);
    let command = format!("printf %s \"$TERM\" > {}", output.display());

    let started = service
        .create_window_with_pane_process(&primary, "term", true, Some(&command))
        .unwrap();
    let updates = poll_until_exit(&mut service);
    let observed = fs::read_to_string(&output).unwrap();

    assert_eq!(service.terminal_term(), "screen-256color");
    assert_eq!(started.pane_id, updates[0].pane_id);
    assert_eq!(observed, "screen-256color");
    let _ = fs::remove_file(output);
}

/// Verifies that a failed new-window process spawn is transactional. The window
/// is inserted before the PTY spawn path runs, so a spawn-layer failure must
/// restore the previous window list and active-window selection instead of
/// leaving a processless pane behind for later rendering or input dispatch.
#[test]
fn runtime_new_window_spawn_failure_rolls_back_window_creation() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let active_window_id = service.session().active_window().unwrap().id.clone();
    service.session.shell = ResolvedShell::new(
        PathBuf::from("/tmp/mez-runtime-missing-shell-new-window"),
        ShellSource::FallbackBinSh,
    );

    let error = service
        .create_window_with_pane_process(&primary, "bad", true, None)
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Io);
    assert_eq!(service.session().windows().len(), 1);
    assert_eq!(
        service.session().active_window().unwrap().id,
        active_window_id
    );
    assert!(service.pane_processes().is_empty());
}

/// Verifies that a failed split process spawn restores the pre-split layout.
/// Existing panes are resized before the new pane process is started, so the
/// rollback must also return the active pane geometry to its original size and
/// leave only the already-running process tracked by the runtime.
#[test]
fn runtime_split_spawn_failure_rolls_back_layout_creation() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let active_pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .clone();
    service.session.shell = ResolvedShell::new(
        PathBuf::from("/tmp/mez-runtime-missing-shell-split"),
        ShellSource::FallbackBinSh,
    );

    let error = service
        .split_pane_with_process(&primary, SplitDirection::Vertical, None)
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Io);
    let window = service.session().active_window().unwrap();
    assert_eq!(window.panes().len(), 1);
    assert_eq!(window.active_pane().id, active_pane_id);
    assert_eq!(window.active_pane().size, Size::new(80, 24).unwrap());
    assert_eq!(service.pane_processes().tracked_pane_ids(), vec!["%1"]);
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that terminal-command splits use the same transactional runtime
/// helper as direct mux/control splits. A failed process spawn must restore the
/// pre-split layout instead of leaving a processless command-created pane with
/// stale geometry behind.
#[test]
fn runtime_terminal_command_split_spawn_failure_rolls_back_layout_creation() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let active_pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .clone();
    service.session.shell = ResolvedShell::new(
        PathBuf::from("/tmp/mez-runtime-missing-shell-command-split"),
        ShellSource::FallbackBinSh,
    );

    let error = service
        .execute_terminal_command(&primary, "split-window")
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Io);
    let window = service.session().active_window().unwrap();
    assert_eq!(window.panes().len(), 1);
    assert_eq!(window.active_pane().id, active_pane_id);
    assert_eq!(window.active_pane().size, Size::new(80, 24).unwrap());
    assert_eq!(service.pane_processes().tracked_pane_ids(), vec!["%1"]);
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies runtime control initialize can reattach primary without existing primary.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_control_initialize_can_reattach_primary_without_existing_primary() {
    let mut service = test_runtime_service();
    let mut connection = ControlConnectionState::new(true, true);
    let initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"requested_role":"primary","requested_version":1,"client_name":"mez-cli","client":{"name":"mez-cli","interactive":true,"terminal":{"columns":100,"rows":40,"term":"xterm-256color"}}}}"#,
    );
    let get =
        encode_control_body(r#"{"jsonrpc":"2.0","id":"get","method":"session/get","params":{}}"#);
    let mut input = initialize;
    input.extend_from_slice(&get);

    let (output, consumed) = service
        .handle_control_input_for_connection(&input, 4096, &mut connection)
        .unwrap();
    let (first_body, first_consumed) = decode_control_frame(&output, 4096).unwrap();
    let (second_body, _) = decode_control_frame(&output[first_consumed..], 4096).unwrap();

    assert_eq!(consumed, input.len());
    assert!(first_body.contains(r#""granted_role":"primary""#));
    assert!(second_body.contains(r#""session_id":"$1""#));
    assert!(connection.caller_client_id().is_some());
    assert!(service.session().primary_client_id().is_some());
    assert_eq!(
        service.session().authoritative_size,
        Size::new(100, 40).unwrap()
    );
    assert!(service.last_attach_at_unix_seconds().is_some());
}

/// Verifies that the live control attach path applies the primary terminal size
/// to an already-started initial pane. The daemon starts the first pane before
/// the CLI sends `control/initialize`, so the initialize side effect must use
/// the same resize/sync path as direct attaches instead of only recording the
/// authoritative size.
#[test]
fn runtime_control_initialize_resizes_started_initial_pane_for_primary_terminal() {
    let mut service = test_runtime_service();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let initial_descriptor = service
        .tracked_pane_descriptors()
        .into_iter()
        .find(|descriptor| descriptor.pane_id.as_str() == "%1")
        .unwrap();
    assert_eq!(initial_descriptor.size, Size::new(80, 22).unwrap());

    let mut connection = ControlConnectionState::new(true, true);
    let initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"requested_role":"primary","requested_version":1,"client_name":"mez-cli","client":{"name":"mez-cli","interactive":true,"terminal":{"columns":100,"rows":40,"term":"xterm-256color"}}}}"#,
    );

    let (output, consumed) = service
        .handle_control_input_for_connection(&initialize, 4096, &mut connection)
        .unwrap();
    let (body, _) = decode_control_frame(&output, 4096).unwrap();

    assert_eq!(consumed, initialize.len());
    assert!(body.contains(r#""granted_role":"primary""#), "{body}");
    assert_eq!(
        service.session().authoritative_size,
        Size::new(100, 40).unwrap()
    );
    assert_eq!(
        service.session().active_window().unwrap().size,
        Size::new(100, 40).unwrap()
    );
    assert_eq!(
        service
            .session()
            .active_window()
            .unwrap()
            .active_pane()
            .size,
        Size::new(100, 40).unwrap()
    );
    let resized_descriptor = service
        .tracked_pane_descriptors()
        .into_iter()
        .find(|descriptor| descriptor.pane_id.as_str() == "%1")
        .unwrap();
    assert_eq!(resized_descriptor.size, Size::new(100, 38).unwrap());
    assert_eq!(
        service.pane_screen("%1").unwrap().size(),
        Size::new(100, 38).unwrap()
    );

    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(100, 40).unwrap(),
            &config,
        )
        .unwrap()
        .unwrap();
    let region = view.agent_prompt_region.unwrap();
    assert_eq!(view.lines.len(), 40);
    assert_eq!(region.columns, 100);
    assert_eq!(region.rows, 38);
    assert!(
        view.cursor_row >= 38,
        "agent prompt cursor should render at attached terminal bottom: {view:?}"
    );

    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies observer `control/initialize` requests are visible immediately.
///
/// The control dispatcher already creates the pending observer record. The
/// runtime side effect must also log the request, write a visible active-pane
/// status line with the request id, and make `:list-observers` usable as the
/// same pager/action surface as `:choose-observer`.
#[test]
fn runtime_control_initialize_observer_logs_and_lists_pending_request() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut connection = ControlConnectionState::new(true, true);
    let initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"requested_role":"observer","requested_version":1,"client_name":"observer-cli","client":{"name":"observer-cli","interactive":true,"terminal":{"columns":100,"rows":40,"term":"xterm-256color"}}}}"#,
    );

    let (output, consumed) = service
        .handle_control_input_for_connection(&initialize, 4096, &mut connection)
        .unwrap();
    let (body, _) = decode_control_frame(&output, 4096).unwrap();
    let observer = service.session().observers().first().unwrap();
    let observer_id = observer.id.to_string();

    assert_eq!(consumed, initialize.len());
    assert!(
        body.contains(r#""granted_role":"pending_observer""#),
        "{body}"
    );
    assert!(body.contains(&observer_id), "{body}");
    let events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(
        events.iter().any(|event| {
            event.kind == EventKind::ObserverRequested && event.payload.contains(&observer_id)
        }),
        "{events:?}"
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .visible_lines()
        .join("\n");
    assert!(
        pane_text.contains(&format!("observer request {observer_id}")),
        "{pane_text}"
    );

    service
        .execute_attached_display_command(&primary, "list-observers")
        .unwrap();
    let overlay = service
        .primary_display_overlay
        .as_ref()
        .expect("list-observers should open a command display overlay");
    assert!(
        overlay
            .selections
            .iter()
            .any(|selection| selection.command == format!("approve-observer -t {observer_id}")),
        "{overlay:?}"
    );
    assert!(
        overlay
            .selections
            .iter()
            .any(|selection| selection.command == format!("reject-observer -t {observer_id}")),
        "{overlay:?}"
    );
}

/// Verifies that the runtime service refreshes the filesystem registry when a
/// control connection claims the primary role. Without this write, `mez list`
/// could advertise a detached session as primary-available after an attach, and
/// default attach resolution could pick that busy session instead of another
/// attachable live daemon.
#[test]
fn runtime_control_initialize_persists_attached_registry_state() {
    let root = std::env::temp_dir().join(format!(
        "mez-runtime-registry-initialize-{}-{:?}",
        std::process::id(),
        thread::current().id()
    ));
    let _ = fs::remove_dir_all(&root);
    let registry = SessionRegistry::new(root.clone(), effective_uid());
    let mut service = test_runtime_service();
    service.set_session_registry(registry.clone());
    service.persist_registry_update().unwrap();
    let mut connection = ControlConnectionState::new(true, true);
    let initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"requested_role":"primary","requested_version":1,"client_name":"mez-cli","client":{"name":"mez-cli","interactive":true,"terminal":{"columns":100,"rows":40,"term":"xterm-256color"}}}}"#,
    );

    service
        .handle_control_input_for_connection(&initialize, 4096, &mut connection)
        .unwrap();

    let records = registry.list().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].session_id, service.session().id.to_string());
    assert_eq!(records[0].state, RegistrySessionState::Running);
    assert!(!records[0].primary_available);
    assert_eq!(records[0].authoritative_columns, 100);
    assert_eq!(records[0].authoritative_rows, 40);
    assert!(records[0].last_attach_at_unix_seconds.is_some());

    let _ = fs::remove_dir_all(root);
}

/// Verifies that primary detach actions issued by the attached terminal loop
/// update the registry immediately. This covers the default prefix escape path,
/// which mutates runtime state outside the framed control request loop and
/// otherwise could leave `mez list` showing the session as still busy.
#[test]
fn attached_terminal_detach_action_persists_available_registry_state() {
    let root = std::env::temp_dir().join(format!(
        "mez-runtime-registry-detach-action-{}-{:?}",
        std::process::id(),
        thread::current().id()
    ));
    let _ = fs::remove_dir_all(&root);
    let registry = SessionRegistry::new(root.clone(), effective_uid());
    let mut service = test_runtime_service();
    service.set_session_registry(registry.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let busy_records = registry.list().unwrap();
    assert_eq!(busy_records.len(), 1);
    assert!(!busy_records[0].primary_available);
    let detach_step = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::ExecuteMux(
            MuxAction::DetachPrimaryClient,
        )],
        output_lines: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };

    service
        .apply_attached_terminal_step_plan(&primary, &detach_step)
        .unwrap();

    let records = registry.list().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].state, RegistrySessionState::Detached);
    assert!(records[0].primary_available);
    assert_eq!(records[0].last_attach_at_unix_seconds, Some(120));

    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime service registry plan preserves authoritative detached size.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_service_registry_plan_preserves_authoritative_detached_size() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();

    service
        .detach_primary(&primary, Size::new(132, 43).unwrap())
        .unwrap();

    let RuntimeRegistryUpdatePlan::Upsert(record) = service.registry_update_plan() else {
        panic!("detached live service must plan a registry upsert");
    };
    assert_eq!(record.state, RegistrySessionState::Detached);
    assert_eq!(record.last_attach_at_unix_seconds, Some(120));
    assert!(record.primary_available);
    assert_eq!(record.authoritative_columns, 132);
    assert_eq!(record.authoritative_rows, 43);
}

/// Verifies runtime service kill requires force and plans registry removal.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_service_kill_requires_force_and_plans_registry_removal() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let error = service.kill_session(&primary, false).unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);

    service.kill_session(&primary, true).unwrap();

    assert_eq!(service.lifecycle_state(), RuntimeLifecycleState::Killed);
    assert!(service.session().windows().is_empty());
    assert!(matches!(
        service.registry_update_plan(),
        RuntimeRegistryUpdatePlan::Remove { .. }
    ));

    let error = service
        .attach_primary("late", true, Size::new(80, 24).unwrap(), 200)
        .unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
}

/// Verifies runtime service owns session memory and clears it on kill.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_service_owns_session_memory_and_clears_it_on_kill() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    service
        .upsert_session_memory(MemoryRecord {
            id: "runtime-note".to_string(),
            scope: crate::memory::MemoryScope::Session {
                session_id: service.session().id.to_string(),
            },
            created_at_unix_seconds: 120,
            updated_at_unix_seconds: 120,
            source: crate::memory::MemorySource::User,
            priority: 20,
            content: "prefer focused regression tests".to_string(),
            explicit_sensitive_consent: false,
        })
        .unwrap();

    assert_eq!(service.memory_records().len(), 1);
    assert_eq!(
        service
            .session_memory()
            .inspect("runtime-note")
            .unwrap()
            .content,
        "prefer focused regression tests"
    );

    service.kill_session(&primary, true).unwrap();

    assert!(service.memory_records().is_empty());
}

/// Verifies runtime service starts initial pane process through resolved shell.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_service_starts_initial_pane_process_through_resolved_shell() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let started = service.start_initial_pane_process(Some("true")).unwrap();

    assert_eq!(started.session_id, service.session().id.to_string());
    assert_eq!(started.window_id, "@1");
    assert_eq!(started.pane_id, "%1");
    assert!(started.primary_pid > 0);
    assert_eq!(
        service.pane_processes().primary_pid("%1"),
        Some(started.primary_pid)
    );
    assert!(matches!(
        started.registry_update,
        RuntimeRegistryUpdatePlan::Upsert(_)
    ));

    let events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(
        events
            .iter()
            .any(|event| event.kind == EventKind::PaneChanged
                && event.payload.contains(r#""process_state":"running""#))
    );
    assert!(
        events
            .iter()
            .any(|event| event.kind == EventKind::Diagnostic
                && event.payload.contains("fell back to /bin/sh"))
    );

    let _ = primary;
    poll_until_exit(&mut service);
}

/// Verifies that runtime services can hand a running pane process to an async
/// owner and restore it if the handoff is cancelled. The service keeps session
/// and terminal metadata while only the process/PTY handle leaves the
/// synchronous manager.
#[test]
fn runtime_service_can_handoff_running_pane_process_to_async_owner() {
    let mut service = test_runtime_service();
    let started = service
        .start_initial_pane_process(Some("sleep 30"))
        .unwrap();

    let process = service
        .take_running_pane_process_for_async_owner(&started.pane_id)
        .unwrap();

    assert!(!service.pane_processes().contains_pane(&started.pane_id));
    let window = service.session().active_window().unwrap();
    let pane_state = service.runtime_control_pane_state_json(window, window.active_pane());
    assert!(
        pane_state.contains(&format!(r#""primary_pid":{}"#, started.primary_pid)),
        "{pane_state}"
    );
    assert!(
        pane_state.contains(r#""process_state":"running""#),
        "{pane_state}"
    );
    service
        .apply_pane_foreground_process_event(
            &started.pane_id,
            "vim",
            started.primary_pid.saturating_add(1),
            Some("/tmp/mez-async-cwd".to_string()),
        )
        .unwrap();
    assert_eq!(
        service
            .pane_current_working_directory(&started.pane_id)
            .as_deref(),
        Some(Path::new("/tmp/mez-async-cwd"))
    );
    assert_eq!(
        service
            .restore_running_pane_process_from_async_owner(&started.pane_id, process)
            .unwrap(),
        started.primary_pid
    );
    assert_eq!(
        service.pane_processes().primary_pid(&started.pane_id),
        Some(started.primary_pid)
    );
    service
        .pane_processes_mut()
        .terminate_pane_with_grace(&started.pane_id, Duration::from_millis(50))
        .unwrap();
}

/// Verifies runtime service restarts restored panes with fresh primary pids.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_service_restarts_restored_panes_with_fresh_primary_pids() {
    let mut original = test_session();
    let primary = original.attach_primary("primary", true).unwrap();
    original
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    let payload = crate::snapshot::SessionSnapshotPayload::from_session(&original);
    let restored = Session::from_snapshot_payload(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        &payload,
    )
    .unwrap();
    assert!(
        restored
            .windows()
            .iter()
            .flat_map(|window| window.panes())
            .all(|pane| !pane.live)
    );
    let mut service = RuntimeSessionService::with_event_log(
        restored,
        PathBuf::from("/tmp/mez-1000/restored.sock"),
        100,
        10,
        1024,
    )
    .unwrap();

    let starts = service
        .restart_restored_pane_processes(Some("true"))
        .unwrap();

    assert_eq!(starts.len(), 2);
    assert!(starts.iter().all(|start| start.primary_pid > 0));
    assert_ne!(starts[0].primary_pid, starts[1].primary_pid);
    assert_eq!(service.pane_processes().len(), 2);
    assert!(
        service
            .session()
            .windows()
            .iter()
            .flat_map(|window| window.panes())
            .all(|pane| pane.live)
    );
    let events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(
        events
            .iter()
            .any(|event| event.payload.contains(r#""restarted":true"#))
    );
    poll_until_exit(&mut service);
}

/// Verifies runtime service starts processes for created windows and panes.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_service_starts_processes_for_created_windows_and_panes() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let window_start = service
        .create_window_with_pane_process(&primary, "build", true, Some("true"))
        .unwrap();
    assert_eq!(window_start.window_id, "@2");
    assert_eq!(window_start.pane_id, "%2");
    assert_eq!(
        service.pane_processes().primary_pid(&window_start.pane_id),
        Some(window_start.primary_pid)
    );

    let split_start = service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("true"))
        .unwrap();
    assert_eq!(split_start.window_id, "@2");
    assert_eq!(split_start.pane_id, "%3");
    assert_eq!(
        service.pane_processes().primary_pid(&split_start.pane_id),
        Some(split_start.primary_pid)
    );

    let mut exited = 0usize;
    for _ in 0..50 {
        let activity_sequences = tracked_pane_activity_sequences(&service);
        exited += service.poll_pane_processes().unwrap().len();
        if exited >= 2 {
            break;
        }
        wait_for_any_tracked_pane_activity_after(
            &service,
            activity_sequences,
            Duration::from_millis(10),
        );
    }
    assert_eq!(exited, 2);
}

/// Verifies runtime applies attached terminal step actions.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_applies_attached_terminal_step_actions() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let step = AttachedTerminalClientStepPlan {
        actions: vec![
            TerminalClientLoopAction::ForwardToPane(b"hello\n".to_vec()),
            TerminalClientLoopAction::ExecuteMux(MuxAction::SplitPaneVertical),
            TerminalClientLoopAction::ExecuteMux(MuxAction::FocusPane(PaneFocusDirection::Left)),
            TerminalClientLoopAction::EnterPrefixKeyMode,
            TerminalClientLoopAction::ExecuteMux(MuxAction::EnterCopyMode),
        ],
        output_lines: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };

    let report = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();

    assert_eq!(report.forwarded_bytes, 6);
    assert_eq!(report.mux_actions_applied, 3);
    assert!(report.view_refresh_required);
    assert!(report.full_redraw_required);
    assert!(report.unsupported_actions.is_empty());
    assert!(!service.active_copy_modes.is_empty());
    assert_eq!(service.session().windows()[0].panes().len(), 2);
    assert_eq!(service.pane_processes().len(), 2);
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies runtime keeps a lone escape key as pending prefix state until the
/// next terminal action consumes it.
///
/// This regression scenario protects the split between entering prefix-key
/// state and explicitly requesting the command prompt through the prefix table.
#[test]
fn runtime_applies_lone_prefix_key_as_pending_state() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();

    let prefix_report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::EnterPrefixKeyMode],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert!(prefix_report.view_refresh_required);
    assert!(service.primary_prefix_key_pending);
    assert!(service.primary_prompt_input.is_none());
    assert!(
        service
            .terminal_client_loop_config(TerminalClientLoopConfig::default())
            .unwrap()
            .prefix_key_pending
    );

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ExecuteMux(
                    MuxAction::EnterCommandPrompt,
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert!(!service.primary_prefix_key_pending);
    assert!(service.primary_prompt_input.is_some());
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies the terminal `copy-mode` command opens over the same live pane
/// viewport height that the attached-terminal copy-mode key path uses.
///
/// The command previously subtracted one row from the pane descriptor before
/// building `CopyMode`, which made the first copy-mode viewport start one line
/// below the live pane when no frame or prompt row was actually present.
#[test]
fn runtime_copy_mode_command_preserves_live_viewport_height() {
    let mut service = test_runtime_service_with_size(Size::new(20, 4).unwrap());
    service.window_frames_enabled = false;
    service.pane_frames_enabled = false;
    let primary = service
        .attach_primary("primary", true, Size::new(20, 4).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"one\ntwo\nthree\nfour");
    service.pane_screens.insert(pane_id.clone(), screen);

    service
        .execute_terminal_command(&primary, "copy-mode")
        .unwrap();

    let visible = service
        .active_copy_modes
        .get(&pane_id)
        .unwrap()
        .visible_lines()
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    assert_eq!(visible, vec!["one", "two", "three", "four"]);
}

/// Verifies that pane split actions which cannot fit inside the active window
/// become transient status-line errors instead of escaping as runtime errors.
/// The failing action must be consumed with no partial pane/process side
/// effects, and the next action while the error is visible must only dismiss
/// the presentational error instead of replaying the same split request.
#[test]
fn runtime_attached_split_error_is_presentational_and_not_replayed_on_dismiss() {
    let mut service = test_runtime_service_with_size(Size::new(3, 8).unwrap());
    let primary = service
        .attach_primary("primary", true, Size::new(3, 8).unwrap(), 120)
        .unwrap();
    let step = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::ExecuteMux(
            MuxAction::SplitPaneVertical,
        )],
        output_lines: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };

    let report = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();

    assert_eq!(report.mux_actions_applied, 0);
    assert!(report.view_refresh_required);
    assert!(report.full_redraw_required);
    assert_eq!(service.session().windows()[0].panes().len(), 1);
    assert!(service.pane_processes().is_empty());
    assert!(
        service
            .primary_error_status_overlay
            .as_deref()
            .is_some_and(|message| message.contains("cannot split vertically")),
        "{:?}",
        service.primary_error_status_overlay
    );

    let dismiss = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();

    assert_eq!(dismiss.mux_actions_applied, 0);
    assert!(dismiss.view_refresh_required);
    assert!(dismiss.full_redraw_required);
    assert_eq!(service.session().windows()[0].panes().len(), 1);
    assert!(service.pane_processes().is_empty());
    assert!(service.primary_error_status_overlay.is_none());

    let retried = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();

    assert_eq!(retried.mux_actions_applied, 0);
    assert!(service.primary_error_status_overlay.is_some());
    assert_eq!(service.session().windows()[0].panes().len(), 1);
    assert!(service.pane_processes().is_empty());
}

/// Verifies that command display output is owned by runtime state instead of a
/// nested terminal loop. The modal overlay must render through the normal
/// primary client view, consume user input while active, and clear on Escape or
/// `q` without forwarding those bytes into the active pane.
#[test]
fn runtime_primary_display_overlay_renders_and_clears_via_terminal_step() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(40, 6).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    service
        .apply_pane_output_bytes(pane_id, b"prompt$ ".to_vec())
        .unwrap();
    let base_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(40, 6).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(base_view.cursor_visible);
    service
        .show_primary_display_overlay(vec![
            "first display line".to_string(),
            "second display line".to_string(),
        ])
        .unwrap();

    let overlay_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(40, 6).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(overlay_view.lines[0].trim_end(), "mezzanine command output");
    assert!(
        overlay_view
            .lines
            .iter()
            .any(|line| line.contains("first display line")),
        "{:?}",
        overlay_view.lines
    );
    assert!(!overlay_view.cursor_visible);

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert!(report.view_refresh_required);
    assert!(report.full_redraw_required);

    let cleared_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(40, 6).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(
        !cleared_view
            .lines
            .iter()
            .any(|line| line.contains("mezzanine command output")),
        "{:?}",
        cleared_view.lines
    );
    assert!(cleared_view.cursor_visible);
    assert_eq!(cleared_view.cursor_row, base_view.cursor_row);
    assert_eq!(cleared_view.cursor_column, base_view.cursor_column);

    service
        .show_primary_display_overlay(vec!["third display line".to_string()])
        .unwrap();
    let quit = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"q".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(quit.forwarded_bytes, 0);
    assert!(quit.view_refresh_required);
    assert!(service.primary_display_overlay.is_none());
}

/// Verifies that command chooser output rendered in the primary overlay is not
/// inert text. Rows that advertise an `action=` command must retain selectable
/// metadata so a mouse click can execute the command through the normal
/// terminal command path and then close or replace the overlay.
#[test]
fn runtime_primary_display_overlay_executes_selectable_command_rows() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .create_window_with_pane_process(&primary, "work", false, None)
        .unwrap();

    service
        .execute_attached_display_command(&primary, "choose-window")
        .unwrap();
    let overlay = service
        .primary_display_overlay
        .as_ref()
        .expect("choose-window should open a command display overlay");
    let work_selection = overlay
        .selections
        .iter()
        .find(|selection| selection.command == "select-window -t @2")
        .expect("work window row should advertise a selectable action");
    let clicked_row = work_selection.line_index.saturating_add(1);

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::SelectDisplayOverlay {
                        position: CopyPosition {
                            line: clicked_row,
                            column: 0,
                        },
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert!(report.view_refresh_required);
    assert!(service.primary_display_overlay.is_none());
    assert_eq!(service.session().active_window().unwrap().name, "work");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies selectable command rows exposed by the primary display overlay can
/// be chosen from the keyboard. Mouse clicks and keyboard Enter must execute the
/// same stored command metadata so chooser output does not depend on scraping
/// the rendered text.
#[test]
fn runtime_primary_display_overlay_executes_keyboard_selected_command_rows() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .create_window_with_pane_process(&primary, "work", false, None)
        .unwrap();

    service
        .execute_attached_display_command(&primary, "choose-window")
        .unwrap();
    assert!(service.primary_display_overlay.is_some());
    assert_eq!(service.session().active_window().unwrap().name, "0");

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"\x1b[B".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"\r".to_vec()),
                ],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert!(report.view_refresh_required);
    assert!(report.full_redraw_required);
    assert!(service.primary_display_overlay.is_none());
    assert_eq!(service.session().active_window().unwrap().name, "work");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies command overlays can expose multiple selectable choices on one row.
/// The user should be able to distinguish routine and destructive choices by
/// color, move between them with selector keys, and execute the active choice
/// without scraping command text out of the rendered row.
#[test]
fn runtime_primary_display_overlay_executes_multiple_action_chips() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .paste_buffers
        .set_with_origin("main", "pasted\n", Some("test".to_string()))
        .unwrap();
    service.active_paste_buffer = Some("main".to_string());

    service
        .execute_attached_display_command(&primary, "choose-buffer")
        .unwrap();
    let overlay = service
        .primary_display_overlay
        .as_ref()
        .expect("choose-buffer should open a command display overlay");
    let paste = overlay
        .selections
        .iter()
        .position(|selection| selection.command == "paste-buffer -b main")
        .expect("buffer row should expose a paste choice");
    let delete = overlay
        .selections
        .iter()
        .position(|selection| selection.command == "delete-buffer main")
        .expect("buffer row should expose a delete choice");
    assert_eq!(delete, paste.saturating_add(1));

    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    let row = view
        .lines
        .iter()
        .position(|line| line.contains("[paste]") && line.contains("[delete]"))
        .expect("overlay should render compact action chips");
    assert!(view.lines[row].contains("[paste]"));
    assert!(view.lines[row].contains("[delete]"));
    assert!(
        view.line_style_spans[row].iter().any(|span| {
            span.length == "[paste]".len()
                && span.rendition.background.is_some_and(|color| {
                    color == service.ui_theme.colors.agent_reasoning.background
                })
        }),
        "{view:?}"
    );
    assert!(
        view.line_style_spans[row].iter().any(|span| {
            span.length == "[delete]".len()
                && span.rendition.background.is_some_and(|color| {
                    color == service.ui_theme.colors.agent_status_failed.background
                })
        }),
        "{view:?}"
    );

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"\x1b[C".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"\r".to_vec()),
                ],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert!(report.view_refresh_required);
    assert!(service.paste_buffers.get("main").is_none());
    assert_eq!(service.active_paste_buffer.as_deref(), None);
    assert!(service.primary_display_overlay.is_none());
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies mouse selection resolves the clicked chip when multiple choices are
/// present on the same display row. This keeps multi-action rows from falling
/// back to ambiguous whole-row execution.
#[test]
fn runtime_primary_display_overlay_mouse_selects_action_chip() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .paste_buffers
        .set_with_origin("main", "pasted\n", Some("test".to_string()))
        .unwrap();
    service.active_paste_buffer = Some("main".to_string());

    service
        .execute_attached_display_command(&primary, "choose-buffer")
        .unwrap();
    let (clicked_line, clicked_column) = service
        .primary_display_overlay
        .as_ref()
        .and_then(|overlay| {
            overlay
                .selections
                .iter()
                .find(|selection| selection.command == "delete-buffer main")
                .map(|selection| {
                    (
                        selection.line_index.saturating_add(1),
                        selection.start_column.saturating_add(2),
                    )
                })
        })
        .expect("delete-buffer choice should be selectable");

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::SelectDisplayOverlay {
                        position: CopyPosition {
                            line: clicked_line,
                            column: clicked_column,
                        },
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert!(service.paste_buffers.get("main").is_none());
    assert_eq!(service.active_paste_buffer.as_deref(), None);
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies observer chooser rows expose the concrete available decisions as
/// compact action chips. The row also contains descriptive `actions=` metadata,
/// but the executable choices must come from the command list so keyboard and
/// mouse selection run real terminal commands.
#[test]
fn runtime_primary_display_overlay_exposes_observer_action_chips() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 24).unwrap(), 120)
        .unwrap();
    let (_observer_client, observer_request) = service
        .session
        .request_observer_with_terminal("observer", None);

    service
        .execute_attached_display_command(&primary, "choose-observer")
        .unwrap();
    let overlay = service
        .primary_display_overlay
        .as_ref()
        .expect("choose-observer should open a command display overlay");
    assert!(
        overlay
            .selections
            .iter()
            .any(|selection| selection.command
                == format!("approve-observer -t {observer_request}")),
        "{overlay:?}"
    );
    assert!(
        overlay
            .selections
            .iter()
            .any(|selection| selection.command == format!("reject-observer -t {observer_request}")),
        "{overlay:?}"
    );
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(100, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(
        view.lines
            .iter()
            .any(|line| line.contains("[approve]") && line.contains("[reject]")),
        "{view:?}"
    );
}

/// Verifies that the primary command prompt is runtime state rather than a
/// nested prompt loop. Submitted input must be consumed by the actor, clear the
/// prompt immediately, execute the terminal command, and render command output
/// through the primary display overlay without forwarding bytes to the pane.
#[test]
fn runtime_primary_command_prompt_submits_and_clears_through_terminal_step() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();
    service.enter_primary_command_prompt("").unwrap();

    let prompt_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(50, 8).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(prompt_view.primary_prompt_active);
    assert_eq!(
        prompt_view.lines.last().map(|line| line.trim_end()),
        Some("▐ :")
    );

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"help\r".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert!(report.view_refresh_required);
    assert!(report.full_redraw_required);
    let display_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(50, 8).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(!display_view.primary_prompt_active);
    assert_eq!(display_view.lines[0].trim_end(), "mezzanine command output");
    assert!(
        display_view
            .lines
            .iter()
            .any(|line| line.contains("mezzanine command help")),
        "{:?}",
        display_view.lines
    );
    assert!(
        display_view
            .lines
            .iter()
            .any(|line| line.contains("agent-shell")),
        "{:?}",
        display_view.lines
    );
}

/// Verifies Ctrl+L clears the live viewport while keeping the terminal command
/// prompt open and preserving prior visible content in pane history. Escape
/// exits that prompt without forwarding bytes.
#[test]
fn runtime_primary_command_prompt_ctrl_l_clears_and_escape_exits() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(50, 8).unwrap(), 120).unwrap();
    screen.feed(b"old output");
    service.pane_screens.insert("%1".to_string(), screen);
    assert!(
        service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("old output")
    );

    service.enter_primary_command_prompt("li").unwrap();
    let clear = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x0c".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(clear.forwarded_bytes, 0);
    assert!(service.primary_prompt_input.is_some());
    assert!(
        !service
            .pane_screen("%1")
            .unwrap()
            .visible_lines()
            .join("\n")
            .contains("old output")
    );
    assert!(
        service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("old output")
    );

    let escape = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(escape.forwarded_bytes, 0);
    assert!(service.primary_prompt_input.is_none());
}

/// Verifies that immediate terminal commands submitted through the command
/// prompt take effect without opening a modal display overlay. Commands like
/// `send-prefix` already have an observable pane effect, so users should not
/// have to press Escape after invoking them from the prompt.
#[test]
fn runtime_primary_command_prompt_immediate_command_does_not_open_overlay() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.enter_primary_command_prompt("").unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"send-prefix\r".to_vec(),
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert!(report.view_refresh_required);
    assert!(service.primary_prompt_input.is_none());
    assert!(service.primary_display_overlay.is_none());
    service.enter_primary_command_prompt("").unwrap();

    let create_buffer = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"create-buffer ack --content hello\r".to_vec(),
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(create_buffer.forwarded_bytes, 0);
    assert!(create_buffer.view_refresh_required);
    assert!(service.primary_prompt_input.is_none());
    assert!(service.primary_display_overlay.is_none());
    assert_eq!(service.paste_buffers.get("ack"), Some("hello"));
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("mez: buffer: ack"), "{pane_text}");
    assert!(pane_text.contains("created=true"), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that the primary command prompt retains submitted commands across
/// prompt openings and exposes them through the same readline Up/Down and
/// Ctrl+R reverse-search behavior used by agent prompts. The command history
/// must remain prompt-local runtime state rather than being forwarded to the
/// pane shell.
#[test]
fn runtime_primary_command_prompt_uses_readline_history_and_reverse_search() {
    let transcript_root = std::env::temp_dir().join(format!(
        "mez-primary-command-history-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&transcript_root);
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();

    service.enter_primary_command_prompt("").unwrap();
    let first = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"help\r".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(first.forwarded_bytes, 0);
    assert!(service.primary_prompt_input.is_none());
    service.clear_primary_display_overlay();

    service.enter_primary_command_prompt("").unwrap();
    let second = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"list-buffers\r".to_vec(),
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(second.forwarded_bytes, 0);
    assert_eq!(
        service.primary_command_prompt_history,
        vec![String::from("help"), String::from("list-buffers")]
    );
    assert_eq!(
        transcript_store.command_prompt_history().unwrap(),
        vec![String::from("help"), String::from("list-buffers")]
    );
    assert!(transcript_store.command_prompt_history_file().exists());
    service.clear_primary_display_overlay();
    service
        .primary_command_prompt_history
        .push("show list-buffers".to_string());

    service.enter_primary_command_prompt("li").unwrap();
    let search = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x12".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(search.forwarded_bytes, 0);
    let prompt = service.primary_prompt_input.as_ref().unwrap();
    assert_eq!(prompt.prompt.buffer.line(), "show list-buffers");

    let restore_draft = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b[B".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(restore_draft.forwarded_bytes, 0);
    let prompt = service.primary_prompt_input.as_ref().unwrap();
    assert_eq!(prompt.prompt.buffer.line(), "li");
    let _ = fs::remove_dir_all(transcript_root);
}

/// Verifies MCP server ids complete in primary `:` commands that address
/// existing MCP configuration. These ids come from the live runtime registry,
/// not the static command table.
#[test]
fn runtime_primary_command_prompt_mcp_retry_autocompletes_configured_server_id() {
    let mut service = test_runtime_service();
    service
        .mcp_registry_mut()
        .add_server(crate::mcp::McpServerConfig::stdio(
            "fixture",
            "Fixture MCP",
            "mcp-fixture",
            Vec::new(),
        ))
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();
    service.enter_primary_command_prompt("").unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"mcp-retry fi".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"\t".to_vec()),
                ],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(
        service
            .primary_prompt_input
            .as_ref()
            .unwrap()
            .prompt
            .buffer
            .line(),
        "mcp-retry fixture "
    );
}

/// Verifies standalone Escape cancels primary command reverse search without
/// closing the prompt itself.
///
/// Reverse search is an in-prompt editing mode. Escape must restore the draft
/// that was present before search started, while a later standalone Escape from
/// normal prompt mode can still close the prompt.
#[test]
fn runtime_primary_command_prompt_escape_cancels_reverse_search() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();
    service.primary_command_prompt_history =
        vec!["list-buffers".to_string(), "show list-buffers".to_string()];

    service.enter_primary_command_prompt("li").unwrap();
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x12".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert!(
        service
            .primary_prompt_input
            .as_ref()
            .unwrap()
            .prompt
            .reverse_search_active()
    );

    let escape = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(escape.forwarded_bytes, 0);
    let prompt = service.primary_prompt_input.as_ref().unwrap();
    assert!(!prompt.prompt.reverse_search_active());
    assert_eq!(prompt.prompt.buffer.line(), "li");
}

/// Verifies that the terminal command prompt accepts encoded Ctrl+R from
/// terminals that use CSI-u for modified printable keys.
///
/// The low-level readline decoder already handles the legacy ASCII control
/// byte. This runtime-level regression keeps the active prompt path wired so
/// command history search works with terminal encodings commonly emitted by
/// modern emulators.
#[test]
fn runtime_primary_command_prompt_accepts_encoded_ctrl_r_history_search() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();
    service.primary_command_prompt_history =
        vec!["list-buffers".to_string(), "show list-buffers".to_string()];

    service.enter_primary_command_prompt("li").unwrap();
    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"\x1b[114;5u".to_vec(),
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    let prompt = service.primary_prompt_input.as_ref().unwrap();
    assert_eq!(prompt.prompt.buffer.line(), "show list-buffers");
}

/// Verifies that recoverable error overlays render as transient status-bar
/// notices. The next input should clear the notice as presentational state
/// without being forwarded or replayed, so repeating an error-causing action
/// does not immediately trigger the same error while dismissing the overlay.
#[test]
fn runtime_primary_error_overlay_dismisses_on_any_input() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(40, 6).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .show_primary_error_overlay(vec!["error: simulated".to_string()])
        .unwrap();

    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(40, 6).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(
        view.lines
            .last()
            .is_some_and(|line| line.contains("simulated")),
        "{:?}",
        view.lines
    );
    assert!(view.cursor_visible);

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"x".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert!(report.view_refresh_required);
    assert!(report.full_redraw_required);
    assert!(service.primary_error_status_overlay.is_none());
    assert!(service.primary_display_overlay.is_none());
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that destructive default prefix bindings open command prompts with
/// the explicit force flag required by live target shutdown semantics. The user
/// still has to submit the prompt, but the generated command no longer fails
/// the confirmation gate for live pane and window targets.
#[test]
fn runtime_destructive_prefix_prompts_include_explicit_force() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let step = AttachedTerminalClientStepPlan {
        actions: vec![
            TerminalClientLoopAction::ExecuteMux(MuxAction::KillWindowAfterConfirmation),
            TerminalClientLoopAction::ExecuteMux(MuxAction::KillPaneAfterConfirmation),
        ],
        output_lines: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };

    let report = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();

    assert_eq!(report.mux_actions_applied, 0);
    assert!(report.view_refresh_required);
    assert!(report.full_redraw_required);
    assert!(report.unsupported_actions.is_empty());
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(
        view.lines
            .last()
            .is_some_and(|line| line.contains("kill-pane --force ")),
        "{:?}",
        view.lines.last()
    );
}

/// Verifies that default prefix mux actions that do not open a command prompt
/// still perform a runtime side effect instead of being reported as unsupported.
#[test]
fn runtime_applies_default_prefix_mux_actions() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("cat"))
        .unwrap();
    service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("cat"))
        .unwrap();
    let active_before = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .clone();

    let cycle_report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ExecuteMux(MuxAction::CyclePane)],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(cycle_report.mux_actions_applied, 1);
    assert_ne!(
        service.session().active_window().unwrap().active_pane().id,
        active_before
    );

    let step = AttachedTerminalClientStepPlan {
        actions: vec![
            TerminalClientLoopAction::ExecuteMux(MuxAction::SendPrefixToPane),
            TerminalClientLoopAction::ExecuteMux(MuxAction::ListKeyBindings),
            TerminalClientLoopAction::ExecuteMux(MuxAction::ShowPaneIndexes),
            TerminalClientLoopAction::ExecuteMux(MuxAction::ShowMessages),
            TerminalClientLoopAction::ExecuteMux(MuxAction::EnterCopyModeAndPageUp),
            TerminalClientLoopAction::ExecuteMux(MuxAction::SwapPaneNext),
            TerminalClientLoopAction::ExecuteMux(MuxAction::SwapPanePrevious),
        ],
        output_lines: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };

    let report = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();

    assert_eq!(report.mux_actions_applied, 7);
    assert!(report.unsupported_actions.is_empty());
    assert!(!service.active_copy_modes.is_empty());
    assert_eq!(service.session().active_window().unwrap().panes().len(), 3);
    let events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(
        events
            .iter()
            .any(|event| event.payload.contains("attached_display_command")),
        "{events:?}"
    );
    let display_panes_event = events
        .iter()
        .find(|event| {
            event
                .payload
                .contains(r#""attached_display_command":"display-panes""#)
        })
        .expect("display-panes binding should emit attached display output");
    assert!(
        display_panes_event
            .payload
            .contains("chooser=select-pane-index"),
        "{display_panes_event:?}"
    );
    assert!(
        display_panes_event
            .payload
            .contains("action=select-pane -t"),
        "{display_panes_event:?}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies runtime attached split mux action focuses new pane.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_attached_split_mux_action_focuses_new_pane() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    assert!(
        service
            .apply_attached_mux_action(&primary, MuxAction::SplitPaneVertical)
            .unwrap()
    );

    let window = &service.session().windows()[0];
    assert_eq!(window.panes().len(), 2);
    assert_eq!(window.active_pane().id.as_str(), "%2");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that the attached-terminal detach binding runs through the runtime
/// lifecycle path rather than mutating session client state directly. The
/// lifecycle helper updates the service state and emits the client-detached
/// event that hooks, registry state, and observers use as the authoritative
/// detach signal.
#[test]
fn runtime_attached_detach_mux_action_emits_lifecycle_state() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    assert!(
        service
            .apply_attached_mux_action(&primary, MuxAction::DetachPrimaryClient)
            .unwrap()
    );

    assert_eq!(service.lifecycle_state(), RuntimeLifecycleState::Detached);
    assert!(service.session().primary_client_id().is_none());
    let events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(
        events
            .iter()
            .any(|event| event.kind == EventKind::ClientDetached
                && event.payload.contains(r#""role":"primary""#)),
        "{events:?}"
    );
}

/// Verifies runtime attached mux action toggles agent shell state.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_attached_mux_action_toggles_agent_shell_state() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let step = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::ExecuteMux(
            MuxAction::ToggleAgentShell,
        )],
        output_lines: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };

    let report = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();
    assert_eq!(report.mux_actions_applied, 1);
    assert!(report.view_refresh_required);
    assert!(report.full_redraw_required);
    assert!(report.unsupported_actions.is_empty());
    let list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"list","method":"agent/list","params":{}}"#,
        &primary,
    );
    assert!(list.contains(r#""visible":true"#), "{list}");

    let report = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();
    assert_eq!(report.mux_actions_applied, 1);
    assert!(report.view_refresh_required);
    assert!(report.full_redraw_required);
    let list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"list2","method":"agent/list","params":{}}"#,
        &primary,
    );
    assert!(list.contains(r#""visible":false"#), "{list}");
}

/// Verifies that opening the pane-local agent prompt resizes the tracked PTY
/// to only the rows available for terminal content, then restores the original
/// size when agent mode exits. This protects cursor placement and terminal
/// application sizing from drifting under the agent input region.
#[test]
fn runtime_agent_shell_toggle_syncs_process_size_with_reserved_prompt_rows() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let step = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::ExecuteMux(
            MuxAction::ToggleAgentShell,
        )],
        output_lines: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };
    let initial_size = service
        .tracked_pane_descriptors()
        .into_iter()
        .find(|descriptor| descriptor.pane_id.as_str() == "%1")
        .unwrap()
        .size;

    let enter_report = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();
    let agent_size = service
        .tracked_pane_descriptors()
        .into_iter()
        .find(|descriptor| descriptor.pane_id.as_str() == "%1")
        .unwrap()
        .size;

    assert_eq!(enter_report.mux_actions_applied, 1);
    assert_eq!(agent_size.columns, initial_size.columns);
    assert!(agent_size.rows < initial_size.rows);
    assert_eq!(service.pane_screens.get("%1").unwrap().size(), agent_size);

    let exit_report = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();
    let restored_size = service
        .tracked_pane_descriptors()
        .into_iter()
        .find(|descriptor| descriptor.pane_id.as_str() == "%1")
        .unwrap()
        .size;

    assert_eq!(exit_report.mux_actions_applied, 1);
    assert_eq!(restored_size, initial_size);
    assert_eq!(service.pane_screens.get("%1").unwrap().size(), initial_size);
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that ordinary pane input is redirected into the pane-local agent
/// prompt while agent mode is active, without entering the older modal prompt
/// loop. Mux actions remain available because only forward-to-pane text is
/// intercepted by the runtime.
#[test]
fn runtime_attached_input_submits_visible_agent_prompt_non_modally() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 10).unwrap(),
    );
    let step = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::ForwardToPane(
            b"summarize\nmore\r".to_vec(),
        )],
        output_lines: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };

    let report = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    assert!(report.view_refresh_required);
    assert!(!report.full_redraw_required);
    assert_eq!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .map(|task| task.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec!["turn-1"]
    );
    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "");
    assert_eq!(
        prompt_state.prompt.buffer.history(),
        &[String::from("summarize\nmore")]
    );
    assert!(prompt_state.display_lines.is_empty());
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("user> summarize"), "{pane_text}");
    assert!(pane_text.contains("more"), "{pane_text}");
    assert!(
        !pane_text.contains("agent: turn turn-1 running"),
        "{pane_text}"
    );
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .unwrap();
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    assert_eq!(turn.state, AgentTurnState::Running);
    assert!(
        context
            .blocks
            .iter()
            .any(|block| block.content.contains("summarize\nmore"))
    );
}

/// Verifies large bracketed-paste agent prompt input is displayed compactly in
/// the pane transcript while the agent turn receives the exact pasted payload.
#[test]
fn runtime_agent_prompt_displays_large_paste_as_compact_block() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 10).unwrap(),
    );

    let payload = "z".repeat(1229);
    let mut input = Vec::new();
    input.extend_from_slice(b"prefix ");
    input.extend_from_slice(b"\x1b[200~");
    input.extend_from_slice(payload.as_bytes());
    input.extend_from_slice(b"\x1b[201~ suffix\r");
    let step = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::ForwardToPane(input)],
        output_lines: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };

    let report = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert_eq!(
        prompt_state.prompt.buffer.history(),
        &[format!("prefix {payload} suffix")]
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("user> prefix [Pasted 1.2 KiB] suffix"),
        "{pane_text}"
    );
    assert!(!pane_text.contains(&payload), "{pane_text}");
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    assert!(
        context
            .blocks
            .iter()
            .any(|block| block.content.contains(&format!("prefix {payload} suffix")))
    );
}

/// Verifies large prompt paste blocks can exceed the visible pane area.
///
/// Bracketed paste payloads may arrive split across terminal reads and contain
/// far more text than can be rendered in the prompt area. The prompt renderer
/// should show one compact block while the submitted turn receives the exact
/// payload.
#[test]
fn runtime_agent_prompt_preserves_large_split_paste_beyond_visible_area() {
    let mut service = test_runtime_service_with_size(Size::new(50, 8).unwrap());
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(50, 8).unwrap(), 10).unwrap(),
    );

    let payload = (0..80)
        .map(|index| format!("line-{index:02}-{}", "x".repeat(36)))
        .collect::<Vec<_>>()
        .join("\n");
    let mut first = Vec::new();
    first.extend_from_slice(b"prefix ");
    first.extend_from_slice(b"\x1b[200~");
    first.extend_from_slice(&payload.as_bytes()[..payload.len() / 2]);
    let mut second = Vec::new();
    second.extend_from_slice(&payload.as_bytes()[payload.len() / 2..]);
    second.extend_from_slice(b"\x1b[201~ suffix\r");

    for input in [first, second] {
        service
            .apply_attached_terminal_step_plan(
                &primary,
                &AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::ForwardToPane(input)],
                    output_lines: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .unwrap();
    }

    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert_eq!(
        prompt_state.prompt.buffer.history(),
        &[format!("prefix {payload} suffix")]
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("user> prefix [Pasted"), "{pane_text}");
    assert!(!pane_text.contains("line-79"), "{pane_text}");
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    assert!(
        context
            .blocks
            .iter()
            .any(|block| { block.content.contains(&format!("prefix {payload} suffix")) })
    );
}

/// Verifies that the pane-local agent prompt accepts encoded Ctrl+R from
/// terminals that use xterm modifyOtherKeys for modified printable keys.
///
/// Agent mode intercepts ordinary pane input before it reaches the PTY. This
/// protects that interception path so encoded reverse-search keys still edit
/// the prompt from its history instead of becoming a no-op escape sequence.
#[test]
fn runtime_agent_prompt_accepts_encoded_ctrl_r_history_search() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    {
        let prompt_state = service.agent_prompt_inputs.get_mut("%1").unwrap();
        prompt_state
            .prompt
            .buffer
            .set_history(vec!["/status".to_string(), "/help".to_string()]);
        prompt_state.prompt.buffer.set_line("/s");
    }

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"\x1b[27;5;114~".to_vec(),
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "/status");
}

/// Verifies standalone Escape cancels pane-local agent reverse search without
/// exiting the agent shell.
///
/// Agent prompts share readline behavior with the primary command prompt, but
/// Escape also has agent-mode exit semantics. This keeps the reverse-search
/// case routed to the prompt before the broader exit handling runs.
#[test]
fn runtime_agent_prompt_escape_cancels_reverse_search() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    {
        let prompt_state = service.agent_prompt_inputs.get_mut("%1").unwrap();
        prompt_state
            .prompt
            .buffer
            .set_history(vec!["/status".to_string(), "/help".to_string()]);
        prompt_state.prompt.buffer.set_line("/s");
    }

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x12".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert!(
        service
            .agent_prompt_inputs
            .get("%1")
            .unwrap()
            .prompt
            .reverse_search_active()
    );

    let escape = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(escape.forwarded_bytes, 0);
    assert_eq!(escape.agent_prompt_inputs_applied, 1);
    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert!(!prompt_state.prompt.reverse_search_active());
    assert_eq!(prompt_state.prompt.buffer.line(), "/s");
    assert!(service.agent_shell_store().get("%1").is_some());
}

/// Verifies Up/Down move through soft-wrapped prompt rows before history.
///
/// Long single-line drafts can occupy multiple visible rows, but ordinary Up
/// and Down keys still operate on the rendered prompt rows before falling back
/// to the submitted-prompt history contract at the first or last row.
#[test]
fn runtime_agent_prompt_up_moves_within_soft_wrapped_draft_before_history() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(24, 8).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(24, 8).unwrap(), 10).unwrap(),
    );
    {
        let prompt_state = service.agent_prompt_inputs.get_mut("%1").unwrap();
        prompt_state.prompt.buffer.set_history(vec![
            "first saved prompt".to_string(),
            "second saved prompt".to_string(),
        ]);
        prompt_state.prompt.buffer.set_line("alpha beta gamma");
    }
    let original_cursor = service
        .agent_prompt_inputs
        .get("%1")
        .unwrap()
        .prompt
        .buffer
        .cursor();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b[A".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "alpha beta gamma");
    assert!(prompt_state.prompt.buffer.cursor() < original_cursor);

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b[B".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "alpha beta gamma");

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b[A".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b[A".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "second saved prompt");
}

/// Verifies pane-local agent prompt navigation uses the rendered pane width
/// after reserving the shared right divider.
///
/// Split-pane agent prompts must wrap and move vertically on the same columns
/// the terminal renderer uses. Otherwise Up can move the cursor sideways on the
/// current visual row instead of to the row above.
#[test]
fn runtime_agent_prompt_navigation_uses_split_pane_render_width() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(30, 8).unwrap(), 120)
        .unwrap();
    service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    service.session.select_pane(&primary, "%1").unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    {
        let prompt_state = service.agent_prompt_inputs.get_mut("%1").unwrap();
        prompt_state.prompt.buffer.set_line("abcde fghij klmno");
    }
    let original_cursor = service
        .agent_prompt_inputs
        .get("%1")
        .unwrap()
        .prompt
        .buffer
        .cursor();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b[A".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "abcde fghij klmno");
    assert!(prompt_state.prompt.buffer.cursor() < original_cursor);
    assert_eq!(prompt_state.prompt.buffer.cursor(), "abcde fghij".len());
}

/// Verifies application-cursor-mode arrows still drive agent prompt navigation.
///
/// PTY applications can leave the pane in application cursor mode, which causes
/// the attached terminal router to forward SS3 arrow sequences. The
/// Mezzanine-owned agent prompt must normalize those bytes before applying
/// readline navigation.
#[test]
fn runtime_agent_prompt_accepts_application_cursor_arrow_sequences() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(24, 8).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(24, 8).unwrap(), 10).unwrap(),
    );
    {
        let prompt_state = service.agent_prompt_inputs.get_mut("%1").unwrap();
        prompt_state.prompt.buffer.set_line("alpha beta gamma");
    }
    let original_cursor = service
        .agent_prompt_inputs
        .get("%1")
        .unwrap()
        .prompt
        .buffer
        .cursor();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1bOA".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "alpha beta gamma");
    assert!(prompt_state.prompt.buffer.cursor() < original_cursor);
}

/// Verifies runtime agent prompts keep Up/Down within explicit multiline draft
/// rows before recalling submitted prompt history.
#[test]
fn runtime_agent_prompt_up_moves_within_multiline_draft_before_history() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(24, 8).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(24, 8).unwrap(), 10).unwrap(),
    );
    {
        let prompt_state = service.agent_prompt_inputs.get_mut("%1").unwrap();
        prompt_state.prompt.buffer.set_history(vec![
            "first saved prompt".to_string(),
            "second saved prompt".to_string(),
        ]);
        prompt_state
            .prompt
            .buffer
            .set_line("first line\nsecond line\nthird line");
    }

    let original_cursor = service
        .agent_prompt_inputs
        .get("%1")
        .unwrap()
        .prompt
        .buffer
        .cursor();
    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b[A".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert_eq!(
        prompt_state.prompt.buffer.line(),
        "first line\nsecond line\nthird line"
    );
    assert!(prompt_state.prompt.buffer.cursor() < original_cursor);
}

/// Verifies that pane-local agent mode does not make the primary client modal.
/// Mux navigation can still focus another pane, and ordinary text input after
/// that focus change must go to the newly active shell instead of being
/// captured by the original pane's agent prompt.
#[test]
fn runtime_agent_prompt_allows_navigation_and_other_pane_input() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let input = b"echo outside agent\n".to_vec();
    let step = AttachedTerminalClientStepPlan {
        actions: vec![
            TerminalClientLoopAction::ExecuteMux(MuxAction::SplitPaneVertical),
            TerminalClientLoopAction::ForwardToPane(input.clone()),
        ],
        output_lines: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };

    let report = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();

    assert_eq!(report.mux_actions_applied, 1);
    assert_eq!(report.forwarded_bytes, input.len());
    assert_eq!(report.agent_prompt_inputs_applied, 0);
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert_eq!(
        service.session().windows()[0].active_pane().id.as_str(),
        "%2"
    );
    assert!(!service.agent_prompt_inputs.contains_key("%2"));
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that agent-mode prompt submissions convert runtime errors into a
/// pane-local error log instead of letting the attached terminal step fail.
/// Invalid-state errors previously bubbled out of this path and could terminate
/// the foreground client instead of leaving the agent prompt usable.
#[test]
fn runtime_attached_agent_prompt_logs_invalid_state_errors_non_modally() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 10).unwrap(),
    );
    let step = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::ForwardToPane(b"/stop\r".to_vec())],
        output_lines: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };

    let report = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    assert!(report.view_refresh_required);
    assert!(!report.full_redraw_required);
    assert!(service.pending_agent_provider_tasks().is_empty());
    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "");
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent command error: agent shell session has no running turn"),
        "{pane_text}"
    );
    let compact_pane_text = pane_text.replace("\n▐ ", "");
    assert!(compact_pane_text.contains("(invalid_state)"), "{pane_text}");
}

/// Verifies that terminal command execution uses live runtime state for the
/// agent shell toggle instead of falling through to the offline no-op command
/// planner. This covers both show and hide transitions for the active pane and
/// verifies transition clears preserve prior visible content in pane history.
#[test]
fn runtime_terminal_command_toggles_agent_shell_state() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();
    screen.feed(b"history line\nvisible before agent");
    service.pane_screens.insert("%1".to_string(), screen);
    let history_before_enter = service.pane_screen("%1").unwrap().history().len();
    assert!(
        service
            .pane_screen("%1")
            .unwrap()
            .visible_lines()
            .join("\n")
            .contains("visible before agent")
    );

    let show = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show.contains(r#""command":"agent-shell""#), "{show}");
    assert!(show.contains(r#""kind":"display""#), "{show}");
    assert!(show.contains("pane=%1"), "{show}");
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    assert!(
        show.contains(&format!("conversation_id={conversation_id}")),
        "{show}"
    );
    assert!(show.contains("visibility=visible"), "{show}");
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );
    let after_enter_screen = service.pane_screen("%1").unwrap();
    assert!(after_enter_screen.history().len() > history_before_enter);
    assert!(
        !after_enter_screen
            .visible_lines()
            .join("\n")
            .contains("visible before agent")
    );
    assert!(
        after_enter_screen
            .normal_content_lines()
            .join("\n")
            .contains("visible before agent")
    );
    let history_before_exit = after_enter_screen.history().len();
    service
        .pane_screens
        .get_mut("%1")
        .unwrap()
        .feed(b"visible inside agent");
    assert!(
        service
            .pane_screen("%1")
            .unwrap()
            .visible_lines()
            .join("\n")
            .contains("visible inside agent")
    );

    let hide = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(hide.contains("visibility=hidden"), "{hide}");
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Hidden)
    );
    let after_exit_screen = service.pane_screen("%1").unwrap();
    assert!(after_exit_screen.history().len() > history_before_exit);
    assert!(
        !after_exit_screen
            .visible_lines()
            .join("\n")
            .contains("visible inside agent")
    );
    assert!(
        after_exit_screen
            .normal_content_lines()
            .join("\n")
            .contains("visible inside agent")
    );

    let show_again = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show_again.contains("visibility=visible"), "{show_again}");
    let after_reentry_screen = service.pane_screen("%1").unwrap();
    assert!(
        !after_reentry_screen
            .visible_lines()
            .join("\n")
            .contains("visible inside agent"),
        "agent reentry should start from a clean viewport, not scroll old agent logs back into view"
    );
    assert!(
        after_reentry_screen
            .normal_content_lines()
            .join("\n")
            .contains("visible inside agent")
    );
}

/// Verifies that showing agent mode starts a pane-local subshell and hiding it
/// exits that subshell instead of sending redraw traffic to the user's original
/// interactive shell. This protects prompt, option, and environment mutations
/// made by agent commands from leaking back to the parent shell, and confirms
/// that retained hidden-render suppression is cleared so the parent prompt
/// repaint can advance the terminal cursor to the end of the prompt line.
#[test]
fn runtime_agent_shell_toggle_enters_and_exits_pane_subshell() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let mut process = service
        .take_running_pane_process_for_async_owner(&pane_id)
        .unwrap();

    let show = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show.contains("visibility=visible"), "{show}");
    let enter_input = service.drain_deferred_pane_inputs();
    assert_eq!(enter_input.len(), 1);
    assert_eq!(enter_input[0].pane_id, pane_id);
    let enter_text = String::from_utf8_lossy(&enter_input[0].bytes);
    assert!(
        enter_text.contains("command env -u BASH_ENV -u ENV -u ZDOTDIR"),
        "{enter_text}"
    );
    assert!(enter_text.contains("HISTFILE=/dev/null"), "{enter_text}");
    assert!(enter_text.contains("'/bin/sh'"), "{enter_text}");
    assert!(service.agent_subshell_panes.contains(&pane_id));
    service.remember_mez_wrapper_filter_command(&pane_id, "MEZ_MARKER_TOKEN='abc'");

    let hide = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(hide.contains("visibility=hidden"), "{hide}");
    let exit_input = service.drain_deferred_pane_inputs();
    assert_eq!(exit_input.len(), 1);
    assert_eq!(exit_input[0].pane_id, pane_id);
    assert_eq!(exit_input[0].bytes, b"\x04");
    assert!(!service.agent_subshell_panes.contains(&pane_id));
    assert!(!service.hidden_shell_render_retention_timer_needed());
    let simple_prompt_repaint = service.visible_pane_output_bytes(&pane_id, b"\r$ ");
    assert_eq!(simple_prompt_repaint, b"\r$ ");
    let prompt_repaint = service.renderable_pane_output_bytes(&pane_id, b"user@host ~/repo $ ");
    assert_eq!(prompt_repaint, b"user@host ~/repo $ ");
    service
        .apply_pane_output_bytes(pane_id.clone(), b"user@host ~/repo $ ".to_vec())
        .unwrap();
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            &TerminalClientLoopConfig {
                window_frames_enabled: false,
                pane_frames_enabled: false,
                ..TerminalClientLoopConfig::default()
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(view.cursor_column, "user@host ~/repo $ ".chars().count());
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies that Ctrl+D from a visible agent prompt restores the parent shell
/// cursor after agent-authored text has been rendered into the pane. The
/// preceding agent output leaves the pane screen on a Mezzanine-rendered line,
/// so the subsequent parent prompt repaint must still advance through the
/// prompt's trailing space instead of landing one cell early.
#[test]
fn runtime_agent_shell_ctrl_d_after_agent_output_restores_prompt_cursor() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let mut process = service
        .take_running_pane_process_for_async_owner(&pane_id)
        .unwrap();

    let show = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show.contains("visibility=visible"), "{show}");
    assert_eq!(service.drain_deferred_pane_inputs().len(), 1);
    service
        .append_agent_assistant_text_to_terminal_buffer(&pane_id, "done")
        .unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x04".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    assert!(report.view_refresh_required);
    assert!(report.full_redraw_required);
    assert_eq!(
        service
            .agent_shell_store()
            .get(&pane_id)
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Hidden),
        "Ctrl+D should hide the agent prompt before the parent prompt repaint"
    );
    let exit_input = service.drain_deferred_pane_inputs();
    assert_eq!(exit_input.len(), 1);
    assert_eq!(exit_input[0].pane_id, pane_id);
    assert_eq!(exit_input[0].bytes, b"\x04");

    let prompt = b"user@host ~/repo $ ";
    let prompt_repaint = service.renderable_pane_output_bytes(&pane_id, prompt);
    assert_eq!(prompt_repaint, prompt);
    service
        .apply_pane_output_bytes(pane_id.clone(), prompt.to_vec())
        .unwrap();
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            &TerminalClientLoopConfig {
                window_frames_enabled: false,
                pane_frames_enabled: false,
                ..TerminalClientLoopConfig::default()
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(view.cursor_column, "user@host ~/repo $ ".chars().count());
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies that the live subshell EOF path also restores the parent prompt
/// cursor after agent-authored text has already moved the pane screen. This
/// covers the Ctrl+D path that exits the child agent shell, waits for the parent
/// shell prompt to repaint, and then presents the attached terminal cursor.
#[test]
fn runtime_agent_shell_ctrl_d_after_agent_output_restores_live_parent_cursor() {
    let shell_path = PathBuf::from("/bin/sh");
    let shell_available = fs::metadata(&shell_path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false);
    if !shell_available {
        eprintln!("skipping live cursor regression because /bin/sh is unavailable");
        return;
    }
    let mut service = RuntimeSessionService::with_event_log(
        Session::new_default(
            ResolvedShell::new(shell_path.clone(), ShellSource::ShellEnv),
            Size::new(80, 24).unwrap(),
        ),
        PathBuf::from("/tmp/mez-1000/default.sock"),
        100,
        10,
        1024,
    )
    .unwrap();
    service.host_clipboard = HostClipboard::disabled();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some(
            "/bin/sh -c 'PS1=\"parent$ \"; export PS1; exec /bin/sh -i'",
        ))
        .unwrap();
    let mut initial_screen = String::new();
    for _ in 0..200 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        initial_screen = service
            .pane_screen("%1")
            .unwrap()
            .visible_lines()
            .join("\n");
        if initial_screen.contains("parent$") {
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(
        initial_screen.contains("parent$"),
        "parent prompt did not arrive: {initial_screen:?}"
    );

    let show = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show.contains("visibility=visible"), "{show}");
    service
        .append_agent_assistant_text_to_terminal_buffer("%1", "done")
        .unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x04".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    assert!(report.full_redraw_required);

    let prompt_column = "parent$ ".chars().count();
    let mut cursor_column = None;
    let mut observed_cursor = None;
    let mut observed_screen = String::new();
    for _ in 0..100 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        let cursor = service.pane_screen("%1").unwrap().cursor_state();
        let screen_text = service
            .pane_screen("%1")
            .unwrap()
            .visible_lines()
            .join("\n");
        observed_cursor = Some(cursor);
        observed_screen = screen_text.clone();
        if screen_text.contains("parent$") && cursor.column == prompt_column {
            cursor_column = Some(cursor.column);
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }

    assert_eq!(
        cursor_column,
        Some(prompt_column),
        "parent prompt cursor should land after the trailing prompt space; observed_cursor={observed_cursor:?}; observed_screen={observed_screen:?}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that `/exit` from the pane-scoped agent prompt performs the same
/// subshell exit as the keyboard toggle while preserving pane-visible content in
/// history. This covers the slash-command path used by Escape, Ctrl+C, Ctrl+D
/// on an empty prompt, `/quit`, and direct `/exit` submissions through the
/// control API.
#[test]
fn runtime_agent_shell_slash_exit_exits_pane_subshell() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let mut process = service
        .take_running_pane_process_for_async_owner(&pane_id)
        .unwrap();
    let show = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show.contains("visibility=visible"), "{show}");
    assert_eq!(service.drain_deferred_pane_inputs().len(), 1);
    assert!(service.agent_subshell_panes.contains(&pane_id));
    service
        .pane_screens
        .get_mut(&pane_id)
        .unwrap()
        .feed(b"slash exit history\nslash exit visible text");
    assert!(
        service
            .pane_screen(&pane_id)
            .unwrap()
            .visible_lines()
            .join("\n")
            .contains("slash exit visible text")
    );

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-exit","method":"agent/shell/command","params":{"idempotency_key":"agent-exit","input":"/exit"}}"#,
        &primary,
    );
    assert!(response.contains(r#""visibility":"hidden""#), "{response}");
    let exit_input = service.drain_deferred_pane_inputs();
    assert_eq!(exit_input.len(), 1);
    assert_eq!(exit_input[0].pane_id, pane_id);
    assert_eq!(exit_input[0].bytes, b"\x04");
    assert!(!service.agent_subshell_panes.contains(&pane_id));
    let after_exit_screen = service.pane_screen(&pane_id).unwrap();
    assert!(
        !after_exit_screen
            .visible_lines()
            .join("\n")
            .contains("slash exit visible text")
    );
    assert!(
        after_exit_screen
            .normal_content_lines()
            .join("\n")
            .contains("slash exit visible text")
    );
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies `/exit` stops an active pane-local turn before hiding agent mode.
/// This protects the exit paths used by slash commands, keyboard shortcuts, and
/// control clients from leaving provider or shell-action work running unseen.
#[test]
fn runtime_agent_shell_slash_exit_stops_running_turn_before_hiding() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-prompt-exit-stop","input":"summarize the pane"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-exit","method":"agent/shell/command","params":{"idempotency_key":"agent-exit-stop","input":"/exit"}}"#,
        &primary,
    );

    assert!(response.contains(r#""command":"exit""#), "{response}");
    assert!(response.contains(r#""visibility":"hidden""#), "{response}");
    assert!(response.contains("stopped_turn=turn-1"), "{response}");
    let session = service.agent_shell_store().get("%1").unwrap();
    assert_eq!(session.visibility, AgentShellVisibility::Hidden);
    assert_eq!(session.running_turn_id, None);
    assert!(!service.agent_turn_is_running("turn-1"));
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("Stopped after"), "{pane_text}");
}

/// Verifies exiting agent mode after interrupting a live shell transaction
/// closes the nested agent subshell with a line command.
///
/// Immediate EOF can be consumed by an interrupted transaction wrapper's read
/// loop, leaving the user inside the child shell after agent mode hides. After
/// a live transaction is interrupted, the runtime should queue Ctrl+C followed
/// by `exit` so the command is read by the shell after the wrapper unwinds.
#[test]
fn runtime_agent_shell_exit_after_shell_transaction_uses_command_exit() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let mut process = service
        .take_running_pane_process_for_async_owner(&pane_id)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    service.agent_subshell_panes.insert(pane_id.clone());
    let started = service
        .start_agent_prompt_turn(&pane_id, "search the file")
        .unwrap();
    service.running_shell_transactions.insert(
        "marker-grep".to_string(),
        RunningShellTransactionRef {
            turn_id: started.turn_id.clone(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "shell-grep".to_string(),
            },
            pane_id: pane_id.clone(),
            command: "grep -n needle file.txt".to_string(),
            started_at_unix_ms: 1_000,
            timeout_ms: Some(10 * 60 * 1000),
            pending_input_payload: Some(b"payload\n".to_vec()),
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-exit","method":"agent/shell/command","params":{"idempotency_key":"agent-exit-live-shell","input":"/exit"}}"#,
        &primary,
    );

    assert!(response.contains(r#""visibility":"hidden""#), "{response}");
    let exit_inputs = service.drain_deferred_pane_inputs();
    assert_eq!(exit_inputs.len(), 2);
    assert_eq!(exit_inputs[0].pane_id, pane_id);
    assert_eq!(exit_inputs[0].bytes, b"\x03");
    assert_eq!(exit_inputs[1].pane_id, pane_id);
    assert_eq!(exit_inputs[1].bytes, b"exit\n");
    assert!(!service.agent_subshell_panes.contains(&pane_id));
    assert!(!service.agent_subshell_command_exit_panes.contains(&pane_id));
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies Escape interrupts active agent work instead of exiting agent mode.
///
/// The pane-local prompt owns Escape while visible. During active work it must
/// follow the same cancellation contract as `/stop`, leaving the shell visible
/// and clearing the running turn rather than hiding the prompt.
#[test]
fn runtime_agent_prompt_escape_interrupts_running_turn() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-prompt-escape-stop","input":"summarize the pane"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    assert!(!service.agent_turn_is_running("turn-1"));
}

/// Verifies Ctrl+C uses the same active-work interruption path as Escape.
///
/// Ctrl+C arrives through readline as a cancellation outcome rather than the
/// direct Escape byte path, so it needs separate coverage to ensure both input
/// routes reuse the same `/stop` behavior.
#[test]
fn runtime_agent_prompt_ctrl_c_interrupts_running_turn() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-prompt-ctrl-c-stop","input":"summarize the pane"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x03".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    assert!(!service.agent_turn_is_running("turn-1"));
}

/// Verifies Escape exits an idle pane-local agent shell.
///
/// Escape is an explicit mode exit key when no turn is active, so it should
/// leave agent mode rather than sending bytes to the pane PTY or manufacturing
/// a `/stop` error.
#[test]
fn runtime_agent_prompt_escape_exits_when_idle() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Hidden)
    );
}

/// Verifies idle Ctrl+C requires confirmation before exiting agent mode.
///
/// Ctrl+C is easy to hit accidentally while editing a prompt. The first press
/// should show a pane-local status message and keep the prompt visible; the
/// second press within the confirmation window exits.
#[test]
fn runtime_agent_prompt_ctrl_c_requires_second_press_when_idle() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let first = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x03".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(first.forwarded_bytes, 0);
    assert_eq!(first.agent_prompt_inputs_applied, 1);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("press ctrl-c again within 3s to exit agent mode"),
        "{pane_text}"
    );

    let second = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x03".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(second.forwarded_bytes, 0);
    assert_eq!(second.agent_prompt_inputs_applied, 1);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Hidden)
    );
}

/// Verifies idle Ctrl+C clears a nonempty pane-local agent prompt before using
/// the double-confirm exit path for an already empty prompt.
#[test]
fn runtime_agent_prompt_ctrl_c_clears_nonempty_buffer_when_idle() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let edit = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"draft text".to_vec(),
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(edit.agent_prompt_inputs_applied, 1);
    assert_eq!(
        service
            .agent_prompt_inputs
            .get("%1")
            .unwrap()
            .prompt
            .buffer
            .line(),
        "draft text"
    );

    let clear = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x03".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(clear.forwarded_bytes, 0);
    assert_eq!(clear.agent_prompt_inputs_applied, 1);
    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "");
    assert!(prompt_state.display_lines.is_empty());
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );

    let confirm = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x03".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(confirm.agent_prompt_inputs_applied, 1);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );
    assert!(
        service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("press ctrl-c again within 3s to exit agent mode")
    );
}

/// Verifies ordinary pane input is consumed while an agent-shell hide request
/// is waiting for the active turn to stop. This prevents user keystrokes from
/// leaking into the parent shell before the `/stop` contract has completed.
#[test]
fn runtime_agent_shell_exit_pending_blocks_foreground_input() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .request_hide_pending_task_completion("%1")
        .unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"leak\r".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: input blocked while agent shell is stopping"),
        "{pane_text}"
    );
}

/// Verifies Ctrl+L clears the live viewport while keeping the pane-local agent
/// prompt available and preserving prior visible content in pane history.
#[test]
fn runtime_agent_prompt_ctrl_l_clears_pane_buffer() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(50, 8).unwrap(), 120).unwrap();
    screen.feed(b"old agent output");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    assert!(
        service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("old agent output")
    );

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x0c".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    assert!(
        !service
            .pane_screen("%1")
            .unwrap()
            .visible_lines()
            .join("\n")
            .contains("old agent output")
    );
    assert!(
        service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("old agent output")
    );
    assert!(service.agent_shell_store().get("%1").is_some());
}

/// Verifies `/resume` completion includes saved conversation ids supplied by
/// the runtime transcript store.
#[test]
fn runtime_agent_prompt_resume_autocompletes_saved_session_uuid() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-resume-complete"));
    transcript_store
        .append(&crate::transcript::TranscriptEntry {
            conversation_id: "018f6b3a-1b2c-7000-9000-cafebabefeed".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: crate::transcript::TranscriptRole::User,
            turn_id: "turn-saved".to_string(),
            agent_id: "agent-%9".to_string(),
            pane_id: "%9".to_string(),
            content: "saved prompt".to_string(),
        })
        .unwrap();
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"/resume 018f".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"\t".to_vec()),
                ],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(
        service
            .agent_prompt_inputs
            .get("%1")
            .unwrap()
            .prompt
            .buffer
            .line(),
        "/resume 018f6b3a-1b2c-7000-9000-cafebabefeed "
    );
}

/// Verifies `/personality` completion includes user-configured personality
/// profile ids.
///
/// Personality profiles have no built-in names, so completion must be sourced
/// from live runtime config rather than from a static candidate list.
#[test]
fn runtime_agent_prompt_personality_autocompletes_configured_profile() {
    let mut service = test_runtime_service();
    let root = temp_root("runtime-agent-personality-complete");
    let path = root.join("config.toml");
    fs::write(
        &path,
        "[personalities.careful]\nname = \"Careful\"\nresponse_style = \"terse\"\n",
    )
    .unwrap();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: Some(path.clone()),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: fs::read_to_string(&path).unwrap(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"/personality car".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"\t".to_vec()),
                ],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(
        service
            .agent_prompt_inputs
            .get("%1")
            .unwrap()
            .prompt
            .buffer
            .line(),
        "/personality careful "
    );
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(root);
}

/// Verifies `/list-mcp` completion includes configured MCP server ids supplied
/// by the live runtime registry. MCP server names are dynamic configuration
/// data, so they must not be limited to static slash-command candidates.
#[test]
fn runtime_agent_prompt_list_mcp_autocompletes_configured_server_id() {
    let mut service = test_runtime_service();
    service
        .mcp_registry_mut()
        .add_server(crate::mcp::McpServerConfig::stdio(
            "fixture",
            "Fixture MCP",
            "mcp-fixture",
            Vec::new(),
        ))
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"/list-mcp fi".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"\t".to_vec()),
                ],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(
        service
            .agent_prompt_inputs
            .get("%1")
            .unwrap()
            .prompt
            .buffer
            .line(),
        "/list-mcp fixture "
    );
}

/// Verifies `/resume <session>` replays saved transcript context into the pane
/// buffer after rebinding the pane-local agent shell. A resumed task should
/// show enough prior conversation content for the user to continue without
/// opening a separate transcript file.
#[test]
fn runtime_agent_prompt_resume_displays_saved_transcript_context() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-resume-display"));
    let conversation_id = "018f6b3a-1b2c-7000-9000-cafebabefeed";
    for (sequence, role, content) in [
        (1, crate::transcript::TranscriptRole::User, "aGVsbG8K"),
        (
            2,
            crate::transcript::TranscriptRole::Assistant,
            "I inspected the repo and started the change",
        ),
        (
            3,
            crate::transcript::TranscriptRole::Tool,
            r#"action_id=action-1 action_type=say status=succeeded content: ignored structured_content: {"kind":"say","status":"final","content_type":"text/plain; charset=utf-8","text":"Implemented the change"}"#,
        ),
    ] {
        transcript_store
            .append(&crate::transcript::TranscriptEntry {
                conversation_id: conversation_id.to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%9".to_string(),
                pane_id: "%9".to_string(),
                content: content.to_string(),
            })
            .unwrap();
    }
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service
        .execute_agent_shell_command(&primary, &format!("/resume {conversation_id}"))
        .unwrap();

    assert!(response.contains("resumed=true"), "{response}");
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("Resumed Agent Session"), "{pane_text}");
    assert!(
        pane_text.contains(&format!("Conversation ID: {conversation_id}")),
        "{pane_text}"
    );
    assert!(pane_text.contains("Entries: 3"), "{pane_text}");
    assert!(pane_text.contains("Resumed:\n▐ yes"), "{pane_text}");
    assert!(pane_text.contains("user> hello"), "{pane_text}");
    assert!(
        pane_text.contains("agent> I inspected the repo and started the change"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("agent: Implemented the change"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("aGVsbG8K"), "{pane_text}");
    assert!(!pane_text.contains("structured_content"), "{pane_text}");
    assert!(!pane_text.contains("[1 turn=turn-1]"), "{pane_text}");
}

/// Verifies that hiding a visible agent shell through terminal command routing
/// stops the in-progress turn before returning control to the pane.
#[test]
fn runtime_terminal_command_hides_running_agent_shell_after_task_completion() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-prompt-hide-stop","input":"summarize the pane"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let hide = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(hide.contains("visibility=hidden"), "{hide}");
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Hidden)
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    assert!(!service.agent_turn_is_running("turn-1"));

    let show = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show.contains("visibility=visible"), "{show}");
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );
}

/// Verifies runtime control dispatches agent shell command for visible shell.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_control_dispatches_agent_shell_command_for_visible_shell() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let step = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::ExecuteMux(
            MuxAction::ToggleAgentShell,
        )],
        output_lines: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };
    service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();
    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-command","method":"agent/shell/command","params":{"idempotency_key":"agent-status","input":"/status"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(response.contains("| Visibility | visible |"), "{response}");
    assert!(response.contains(r#""turn":null"#), "{response}");

    let alias_response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-command-alias","method":"agent/shell/command","params":{"idempotency_key":"agent-command-alias","command":"/status"}}"#,
        &primary,
    );
    assert!(
        alias_response.contains(r#""mezzanine_code":"invalid_params""#),
        "{alias_response}"
    );
    assert!(
        alias_response.contains("agent/shell/command params contains unknown field `command`"),
        "{alias_response}"
    );
}

/// Verifies that invalid runtime-backed slash command arguments are converted
/// into pane-local display responses rather than JSON-RPC errors. This keeps
/// the agent prompt alive for commands whose validation happens in runtime
/// handlers instead of the slash-command registry.
#[test]
fn runtime_control_reports_invalid_runtime_slash_args_as_agent_display() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-command-invalid","method":"agent/shell/command","params":{"idempotency_key":"agent-command-invalid","input":"/model one two three"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(
        response.contains(
            "agent command error: /model accepts at most a model name and optional reasoning level"
        ),
        "{response}"
    );
    assert!(!response.contains(r#""error""#), "{response}");
}

/// Verifies that runtime-state failures from agent slash commands are reported
/// through the agent display channel instead of surfacing as JSON-RPC errors.
/// This keeps agent-mode clients alive when a runtime-backed command hits an
/// invalid state, such as stopping when no turn is running.
#[test]
fn runtime_control_reports_invalid_state_agent_shell_errors_as_display() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-command-invalid-state","method":"agent/shell/command","params":{"idempotency_key":"agent-command-invalid-state","input":"/stop"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(
        response.contains("agent command error: agent shell session has no running turn"),
        "{response}"
    );
    assert!(response.contains("(invalid_state)"), "{response}");
    assert!(!response.contains(r#""error""#), "{response}");
}

/// Verifies that runtime `terminal/command` accepts only the spec-defined
/// `input` field. The legacy `command` alias is rejected at the params schema
/// boundary so clients cannot depend on a non-normative request shape.
#[test]
fn runtime_terminal_command_rejects_legacy_command_alias() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let alias_response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"terminal-command-alias","method":"terminal/command","params":{"idempotency_key":"terminal-command-alias","command":"list-windows"}}"#,
        &primary,
    );

    assert!(
        alias_response.contains(r#""mezzanine_code":"invalid_params""#),
        "{alias_response}"
    );
    assert!(
        alias_response.contains("terminal/command params contains unknown field `command`"),
        "{alias_response}"
    );
}

/// Verifies that an unknown command submitted through the supported
/// `terminal/command` JSON-RPC method is reported as invalid command input, not
/// as JSON-RPC method-not-found. The transport method is implemented; only the
/// command language token is unknown.
#[test]
fn runtime_terminal_command_unknown_input_is_invalid_params() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"terminal-command-unknown","method":"terminal/command","params":{"idempotency_key":"terminal-command-unknown","input":"does-not-exist"}}"#,
        &primary,
    );

    assert!(
        response.contains(r#""mezzanine_code":"invalid_params""#),
        "{response}"
    );
    assert!(
        response.contains("unknown command `does-not-exist`"),
        "{response}"
    );
    assert!(
        !response.contains(r#""mezzanine_code":"method_not_found""#),
        "{response}"
    );
}

/// Verifies that the runtime `agent/shell/command` `/list-mcp` path uses the live
/// MCP registry and exposes unavailable or session-blacklisted details. This
/// protects the spec requirement that agent-shell MCP visibility match control
/// and command surfaces instead of returning a generic runtime placeholder.
#[test]
fn runtime_agent_shell_mcp_command_reports_live_registry_detail() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .mcp_registry_mut()
        .add_server(crate::mcp::McpServerConfig::stdio(
            "fs",
            "filesystem",
            "mcp-fs",
            Vec::new(),
        ))
        .unwrap();
    service
        .mcp_registry_mut()
        .mark_available(
            "fs",
            vec![crate::mcp::McpToolState {
                server_id: String::new(),
                name: "read_file".to_string(),
                available: true,
                blacklisted: false,
                permission_required: true,
                effects: crate::mcp::McpToolEffects::none(),
                approval: crate::mcp::McpApprovalSetting::Inherit,
                description: "read a file".to_string(),
                input_schema_json: "{}".to_string(),
            }],
        )
        .unwrap();
    service
        .mcp_registry_mut()
        .blacklist_for_session("fs", "failed handshake")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-mcp","method":"agent/shell/command","params":{"idempotency_key":"agent-mcp","input":"/list-mcp"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(response.contains(r#""command":"list-mcp""#), "{response}");
    assert!(response.contains("## MCP Servers"), "{response}");
    assert!(response.contains("Servers: 1"), "{response}");
    assert!(response.contains("Tools: 1"), "{response}");
    assert!(response.contains("Source: runtime-mcp"), "{response}");
    assert!(response.contains("### `fs` - filesystem"), "{response}");
    assert!(response.contains("- State: blacklisted"), "{response}");
    assert!(
        response.contains("- Session blacklisted: true"),
        "{response}"
    );
    assert!(response.contains("- Retryable: true"), "{response}");
    assert!(
        response.contains("- Reason: failed handshake"),
        "{response}"
    );
    assert!(
        response.contains("- `read_file`: state=blacklisted"),
        "{response}"
    );
    assert!(!response.contains("requires_runtime"), "{response}");
}

/// Verifies `/list-mcp` starts configured MCP transports after a synchronous
/// config load. Default startup paths apply configuration synchronously, so the
/// user-facing MCP listing must not require a separate config reload before the
/// server becomes available to the agent runtime.
#[test]
fn runtime_agent_shell_list_mcp_lazily_discovers_configured_server() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-agent-list-mcp-lazy-discovery");
    let script_path = root.join("mcp-fixture.sh");
    fs::write(&script_path, runtime_mcp_fixture_script(false)).unwrap();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                "[mcp_servers.fixture]\ncommand = \"/bin/sh\"\nargs = [{}]\napproval = \"allow\"\ntool_timeout_ms = 1000\n",
                toml_string(script_path.to_string_lossy().as_ref())
            ),
        }])
        .unwrap();
    assert_eq!(
        service.mcp_registry().list_servers()[0].status,
        crate::mcp::McpServerStatus::Configured
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-mcp","method":"agent/shell/command","params":{"idempotency_key":"agent-mcp-lazy","input":"/list-mcp"}}"#,
        &primary,
    );

    assert!(response.contains("## MCP Servers"), "{response}");
    assert!(response.contains("Servers: 1"), "{response}");
    assert!(response.contains("Tools: 1"), "{response}");
    assert!(response.contains("### `fixture` - fixture"), "{response}");
    assert!(response.contains("- Status: available"), "{response}");
    assert!(response.contains("- `echo`: state=available"), "{response}");
    assert_eq!(
        service.mcp_registry().prompt_summary().available_tools[0].tool_name,
        "echo"
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies that `/new` is a live agent-shell mutation rather than a generic
/// runtime-required placeholder. A fresh conversation id with zero transcript
/// entries must replace the active pane's completed conversation while keeping
/// the shell visible for the next prompt.
#[test]
fn runtime_agent_shell_new_command_starts_fresh_conversation() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .start_turn("%1", "turn-previous")
        .unwrap();
    service
        .agent_shell_store_mut()
        .finish_turn("%1", "turn-previous")
        .unwrap();
    let old_session = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-new","method":"agent/shell/command","params":{"idempotency_key":"agent-new","input":"/new"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"mutated""#), "{response}");
    assert!(response.contains(r#""command":"new""#), "{response}");
    assert!(response.contains("new=true"), "{response}");
    assert!(response.contains("transcript_entries=0"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
    let session = service.agent_shell_store().get("%1").unwrap();
    assert_ne!(session.session_id, old_session);
    assert_eq!(session.transcript_entries, 0);
    assert_eq!(session.visibility, AgentShellVisibility::Visible);
}

/// Verifies that `/clear` follows the spec-level behavior of clearing the live
/// viewport while preserving pane logs and starting a fresh visible
/// conversation.
#[test]
fn runtime_agent_shell_clear_command_resets_conversation_and_terminal_view() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 3).unwrap(), 10).unwrap();
    screen.feed(b"old visible text");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .start_turn("%1", "turn-previous")
        .unwrap();
    service
        .agent_shell_store_mut()
        .finish_turn("%1", "turn-previous")
        .unwrap();
    let old_session = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-clear","method":"agent/shell/command","params":{"idempotency_key":"agent-clear","input":"/clear"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"mutated""#), "{response}");
    assert!(response.contains(r#""command":"clear""#), "{response}");
    assert!(response.contains("new=true"), "{response}");
    assert!(
        response.contains("terminal_view_cleared=true"),
        "{response}"
    );
    let session = service.agent_shell_store().get("%1").unwrap();
    assert_ne!(session.session_id, old_session);
    assert_eq!(session.transcript_entries, 0);
    assert_eq!(session.visibility, AgentShellVisibility::Visible);
    assert!(
        service
            .pane_screen("%1")
            .unwrap()
            .visible_lines()
            .iter()
            .all(|line| line.trim().is_empty()),
        "{:?}",
        service.pane_screen("%1").unwrap().visible_lines()
    );
    assert!(
        service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("old visible text")
    );
}

/// Verifies that `/status` is backed by live runtime state rather than only
/// the shell session fallback. The status view is a user-visible conformance
/// surface, so it must include model selection, policy, identity, writable
/// scope state, current context tracking, and provider token counters in one
/// response.
#[test]
fn runtime_agent_shell_status_reports_live_runtime_state() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-fast\"]\ndefault_model = \"gpt-fast\"\n\n[permissions]\npreset = \"auto\"\napproval_policy = \"full-access\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.record_agent_provider_token_usage(
        "%1",
        crate::agent::ModelTokenUsage {
            input_tokens: 120,
            output_tokens: 34,
            reasoning_tokens: 9,
            cached_input_tokens: Some(80),
        },
    );
    service.record_agent_provider_token_usage(
        "%1",
        crate::agent::ModelTokenUsage {
            input_tokens: 0,
            output_tokens: 0,
            reasoning_tokens: 0,
            cached_input_tokens: Some(20),
        },
    );
    service
        .subagent_scopes
        .register(
            "agent-%1",
            CooperationMode::OwnedWrite,
            &["src".to_string()],
            None,
        )
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "summarize the pane")
        .unwrap();
    assert_eq!(started.state, AgentTurnState::Running);

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-status","method":"agent/shell/command","params":{"idempotency_key":"agent-status","input":"/status"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(response.contains(r#""command":"status""#), "{response}");
    assert!(
        response.contains(r#""content_type":"text/markdown; charset=utf-8""#),
        "{response}"
    );
    assert!(response.contains("## Agent Status"), "{response}");
    assert!(response.contains("| Field | Value |"), "{response}");
    assert!(response.contains("| Agent id | agent-%1 |"), "{response}");
    assert!(response.contains("| Window id | @1 |"), "{response}");
    assert!(
        response.contains("| Model | gpt-fast via openai (profile: default"),
        "{response}"
    );
    assert!(
        response.contains("| Prompt profile | default v18 |"),
        "{response}"
    );
    assert!(
        response.contains("| Permissions | preset auto, approval full-access"),
        "{response}"
    );
    assert!(
        response.contains("| src | agent-%1 | owned-write |"),
        "{response}"
    );
    assert!(response.contains("| Context | 5 blocks"), "{response}");
    assert!(
        response.contains(
            "| Provider tokens | input=20 raw_input=120 output=34 reasoning=9 cached_input=100 cache_hit=83.33% total=154 |"
        ),
        "{response}"
    );
    assert!(!response.contains("Provider rate limits"), "{response}");
    assert!(!response.contains("### Quota Usage"), "{response}");
    assert!(
        response.contains("| Latest turn | turn-1 (running) |"),
        "{response}"
    );
    assert!(!response.contains("requires_runtime"), "{response}");
}

/// Verifies that `/diff` reads the active pane's Git repository and includes
/// both modified tracked content and untracked files. This covers the spec
/// requirement that the agent shell diff view expose the working tree rather
/// than returning a generic runtime-required placeholder.
#[test]
fn runtime_agent_shell_diff_reports_git_worktree_and_untracked_files() {
    let root = temp_root("runtime-agent-diff");
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init"]);
    fs::write(root.join("tracked.txt"), "before\n").unwrap();
    git(&["add", "tracked.txt"]);
    fs::write(root.join("tracked.txt"), "before\nafter\n").unwrap();
    fs::write(root.join("new.txt"), "untracked\n").unwrap();

    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let descriptor = service.initial_pane_descriptor().unwrap();
    service
        .start_pane_process_with_start_directory(descriptor, Some("sleep 30"), Some(&root))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-diff","method":"agent/shell/command","params":{"idempotency_key":"agent-diff","input":"/diff"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(response.contains(r#""command":"diff""#), "{response}");
    assert!(response.contains("source=runtime-vcs-diff"), "{response}");
    assert!(response.contains("untracked_files=1"), "{response}");
    assert!(response.contains("tracked.txt"), "{response}");
    assert!(response.contains("+after"), "{response}");
    assert!(response.contains("file=new.txt"), "{response}");
    assert!(response.contains("+untracked"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
    service.kill_session(&primary, true).unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies `/list-modified-files` renders compact modified-file rows.
///
/// Agent mutation previews already show `edited path (+N -M)` style summaries;
/// the slash command should expose the tracked aggregate in the same compact
/// form instead of a verbose nested object list.
#[test]
fn runtime_agent_shell_list_modified_files_reports_compact_rows() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_modified_files
        .entry("%1".to_string())
        .or_default()
        .insert(
            "src/lib.rs".to_string(),
            RuntimeAgentModifiedFileSummary {
                path: "src/lib.rs".to_string(),
                added: 12,
                removed: 3,
            },
        );

    let response = service
        .execute_agent_shell_command(&primary, "/list-modified-files")
        .unwrap();

    assert!(response.contains("## modified files"), "{response}");
    assert!(response.contains("edited `src/lib.rs`"), "{response}");
    assert!(
        response.contains(r#"<span class=\"mez-diff-addition\">+12</span>"#),
        "{response}"
    );
    assert!(
        response.contains(r#"<span class=\"mez-diff-deletion\">-3</span>"#),
        "{response}"
    );
    assert!(!response.contains("Added:"), "{response}");
    assert!(!response.contains("Removed:"), "{response}");
    assert!(!response.contains("`summary`"), "{response}");
}

/// Verifies that `/init` creates a project instruction scaffold in the active
/// pane's working directory and leaves an existing scaffold intact. This covers
/// the baseline file-mutation slash command without writing to the repository
/// root used by the test harness.
#[test]
fn runtime_agent_shell_init_creates_project_instruction_scaffold() {
    let root = temp_root("runtime-agent-init");
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let descriptor = service.initial_pane_descriptor().unwrap();
    service
        .start_pane_process_with_start_directory(descriptor, Some("sleep 30"), Some(&root))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-init","method":"agent/shell/command","params":{"idempotency_key":"agent-init","input":"/init"}}"#,
        &primary,
    );

    let scaffold = root.join("AGENTS.md");
    assert!(response.contains(r#""kind":"mutated""#), "{response}");
    assert!(response.contains(r#""command":"init""#), "{response}");
    assert!(response.contains("created=true"), "{response}");
    assert!(response.contains("source=runtime-init"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
    let text = fs::read_to_string(&scaffold).unwrap();
    assert!(text.contains("# Repository Guidelines"), "{text}");
    assert!(
        text.contains("## Build, Test, and Development Commands"),
        "{text}"
    );

    let existing = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-init-existing","method":"agent/shell/command","params":{"idempotency_key":"agent-init-existing","input":"/init"}}"#,
        &primary,
    );

    assert!(existing.contains(r#""kind":"display""#), "{existing}");
    assert!(existing.contains(r#""command":"init""#), "{existing}");
    assert!(existing.contains("created=false"), "{existing}");
    assert!(existing.contains("existing=true"), "{existing}");
    assert!(!existing.contains("requires_runtime"), "{existing}");
    service.kill_session(&primary, true).unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies that `/copy` uses retained model-authored `say` text and supports
/// the same pane, buffer, and clipboard targets as other copy commands.
///
/// The raw provider response can contain transport or protocol scaffolding, so
/// the command must copy the latest explicit `say.text` rather than raw model
/// text or an action-summary substitute.
#[test]
fn runtime_agent_shell_copy_writes_latest_say_text_to_destinations() {
    let _clipboard_guard = TEST_HOST_CLIPBOARD_TEST_LOCK.lock().unwrap();
    TEST_HOST_CLIPBOARD_WRITES.lock().unwrap().clear();
    let mut service = test_runtime_service();
    service.host_clipboard =
        HostClipboard::new(record_host_clipboard_copy, empty_host_clipboard_read);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "produce final answer")
        .unwrap();
    assert_eq!(started.state, AgentTurnState::Running);
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "raw transport envelope should not be copied".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![
                    crate::agent::AgentAction {
                        id: "say-1".to_string(),
                        rationale: "give an earlier answer".to_string(),
                        payload: crate::agent::AgentActionPayload::Say {
                            status: crate::agent::SayStatus::Final,
                            text: "Earlier say text.".to_string(),
                            content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                                .to_string(),
                        },
                    },
                    crate::agent::AgentAction {
                        id: "say-2".to_string(),
                        rationale: "give the answer that should be copied".to_string(),
                        payload: crate::agent::AgentActionPayload::Say {
                            status: crate::agent::SayStatus::Final,
                            text: "Latest say text.".to_string(),
                            content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                                .to_string(),
                        },
                    },
                ],
                final_turn: true,
            }),
        },
    };
    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            ModelProfile {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
        )
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Completed);

    let buffer_response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-copy-buffer","method":"agent/shell/command","params":{"idempotency_key":"agent-copy-buffer","input":"/copy buffer retained-say"}}"#,
        &primary,
    );

    assert!(
        buffer_response.contains(r#""kind":"mutated""#),
        "{buffer_response}"
    );
    assert!(
        buffer_response.contains(r#""command":"copy""#),
        "{buffer_response}"
    );
    assert!(
        buffer_response.contains("destination=buffer"),
        "{buffer_response}"
    );
    assert!(
        buffer_response.contains("source=runtime-agent-say"),
        "{buffer_response}"
    );
    assert_eq!(
        service.paste_buffers.get("retained-say"),
        Some("Latest say text.")
    );
    assert_ne!(
        service.paste_buffers.get("retained-say"),
        Some("raw transport envelope should not be copied")
    );
    let buffers = service.paste_buffers.list();
    assert!(
        buffers.iter().any(|buffer| {
            buffer.name == "retained-say" && buffer.origin.as_deref() == Some("agent:turn-1:say")
        }),
        "{buffers:?}"
    );

    let clipboard_response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-copy-clipboard","method":"agent/shell/command","params":{"idempotency_key":"agent-copy-clipboard","input":"/copy clipboard"}}"#,
        &primary,
    );
    assert!(
        clipboard_response.contains("destination=clipboard"),
        "{clipboard_response}"
    );
    assert_eq!(
        service.paste_buffers.get("clipboard"),
        Some("Latest say text.")
    );
    assert!(
        TEST_HOST_CLIPBOARD_WRITES
            .lock()
            .unwrap()
            .last()
            .is_some_and(|text| text == "Latest say text.")
    );

    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 6).unwrap(), 20).unwrap(),
    );
    let pane_response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-copy-pane","method":"agent/shell/command","params":{"idempotency_key":"agent-copy-pane","input":"/copy"}}"#,
        &primary,
    );
    assert!(
        pane_response.contains("destination=pane"),
        "{pane_response}"
    );
    let pane_text_after = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text_after.contains("Latest say text."),
        "{pane_text_after}"
    );
}

/// Verifies that `/logout` executes through the runtime auth store and removes
/// stored credentials without exposing a duplicate terminal logout command.
#[test]
fn runtime_agent_shell_logout_uses_attached_auth_store() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-agent-logout");
    let secret_file = root.join("openai-key.txt");
    fs::write(&secret_file, "sk-runtime-secret").unwrap();
    service.set_auth_store(AuthStore::new(crate::auth::AuthPaths::under_config_root(
        &root,
    )));
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let login = service
        .execute_terminal_command(
            &primary,
            &format!(
                "auth-login --api-key --credential-store file --api-key-file {} --profile work",
                secret_file.display()
            ),
        )
        .unwrap();
    assert!(login.contains("authenticated=true"), "{login}");

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-logout","method":"agent/shell/command","params":{"idempotency_key":"agent-logout","input":"/logout"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"mutated""#), "{response}");
    assert!(response.contains(r#""command":"logout""#), "{response}");
    assert!(response.contains("logged_out=true"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
    assert!(!response.contains("sk-runtime-secret"), "{response}");
    let status = service
        .execute_terminal_command(&primary, "auth-status")
        .unwrap();
    assert!(status.contains("authenticated=false"), "{status}");
    let _ = fs::remove_dir_all(root);
}

/// Verifies that `/approval` arguments are applied through the live runtime
/// approval-mode command path. The no-argument slash command already displays
/// policy state; this covers mutation through the agent shell surface.
#[test]
fn runtime_agent_shell_approval_command_mutates_live_policy() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-permissions","method":"agent/shell/command","params":{"idempotency_key":"agent-permissions","input":"/approval full-access"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"mutated""#), "{response}");
    assert!(response.contains(r#""command":"approval""#), "{response}");
    assert!(response.contains("field=approval_policy"), "{response}");
    assert!(response.contains("requested=full-access"), "{response}");
    assert!(response.contains("changed=true"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::FullAccess
    );
}

/// Verifies terse slash-command display output is written to the pane instead
/// of opening the modal command-output pager.
///
/// One-line status acknowledgements are part of the agent pane transcript and
/// should not force the user to dismiss a full-screen overlay just to continue
/// typing in the pane-local prompt.
#[test]
fn runtime_agent_shell_single_line_display_logs_to_pane_without_overlay() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"/approval\r".to_vec(),
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    assert!(service.primary_display_overlay.is_none());
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("approval policy: ask"), "{pane_text}");
    assert!(pane_text.contains("source: runtime-policy"), "{pane_text}");
}

/// Verifies an explicit `/approval` choice is stored as a live override and
/// therefore survives unrelated configuration reloads from disk.
///
/// This protects full-access mode from being silently reset when a config
/// reload reapplies an older `permissions.approval_policy` value.
#[test]
fn runtime_agent_shell_approval_command_survives_config_reload() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-approval-live-override");
    let path = root.join("config.toml");
    fs::write(
        &path,
        "[history]\nlines = 7\n[permissions]\napproval_policy = \"ask\"\n",
    )
    .unwrap();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: Some(path.clone()),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: fs::read_to_string(&path).unwrap(),
        }])
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-approval","method":"agent/shell/command","params":{"idempotency_key":"agent-approval-live","input":"/approval full-access"}}"#,
        &primary,
    );

    assert!(response.contains("requested=full-access"), "{response}");
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::FullAccess
    );

    fs::write(
        &path,
        "[history]\nlines = 11\n[permissions]\napproval_policy = \"ask\"\n",
    )
    .unwrap();
    let reload = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"reload-approval","method":"config/reload","params":{"idempotency_key":"reload-approval-live"}}"#,
        &primary,
    );

    assert!(reload.contains(r#""operation":"reload""#), "{reload}");
    assert_eq!(service.terminal_history_limit(), 11);
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::FullAccess
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies that `/statusline` mutates the live pane status-line rendering
/// fields. The command should configure existing frame state instead of
/// returning a runtime-required slash placeholder.
#[test]
fn runtime_agent_shell_statusline_configures_pane_frame_fields() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-statusline","method":"agent/shell/command","params":{"idempotency_key":"agent-statusline","input":"/statusline agent.status agent.model pane.mode"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"mutated""#), "{response}");
    assert!(response.contains(r#""command":"statusline""#), "{response}");
    assert!(response.contains("enabled=true"), "{response}");
    assert!(response.contains("agent.status"), "{response}");
    assert!(response.contains("agent.model"), "{response}");
    assert!(response.contains("pane.mode"), "{response}");
    assert!(response.contains("changed=true"), "{response}");
    assert!(response.contains("source=runtime-statusline"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
    assert!(service.pane_frames_enabled);
    assert_eq!(
        service.pane_frame_visible_fields,
        vec![
            "agent.status".to_string(),
            "agent.model".to_string(),
            "pane.mode".to_string()
        ]
    );
    assert_eq!(
        service.pane_frame_template,
        "#{agent.status} #{agent.model} #{pane.mode}"
    );
}

/// Verifies that `/title` reads and mutates the active runtime window title
/// through the live command path. This covers the agent shell title command
/// without allowing the slash surface to target or rename unrelated windows.
#[test]
fn runtime_agent_shell_title_displays_and_renames_active_window() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let display = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-title-display","method":"agent/shell/command","params":{"idempotency_key":"agent-title-display","input":"/title"}}"#,
        &primary,
    );

    assert!(display.contains(r#""kind":"display""#), "{display}");
    assert!(display.contains(r#""command":"title""#), "{display}");
    assert!(display.contains("source=runtime-title"), "{display}");
    assert!(display.contains("window_id=@1"), "{display}");
    assert!(display.contains("window_title=shell"), "{display}");
    assert!(display.contains("pane=%1"), "{display}");
    assert!(display.contains("pane_title=shell"), "{display}");
    assert!(!display.contains("requires_runtime"), "{display}");

    let rename = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-title-rename","method":"agent/shell/command","params":{"idempotency_key":"agent-title-rename","input":"/title build shell"}}"#,
        &primary,
    );

    assert!(rename.contains(r#""kind":"mutated""#), "{rename}");
    assert!(rename.contains(r#""command":"title""#), "{rename}");
    assert!(rename.contains("source=runtime-title"), "{rename}");
    assert!(rename.contains("changed=true"), "{rename}");
    assert!(rename.contains("window_title=build shell"), "{rename}");
    assert!(!rename.contains("requires_runtime"), "{rename}");
    assert_eq!(
        service.session().active_window().unwrap().name,
        "build shell"
    );
}

/// Verifies that `/debug-config` reports live effective configuration, layer
/// order, and policy diagnostics from runtime state instead of the generic
/// runtime-required slash placeholder.
#[test]
fn runtime_agent_shell_debug_config_reports_live_runtime_config() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[history]\nlines = 7\n[permissions]\npreset = \"auto\"\napproval_policy = \"full-access\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"debug-config","method":"agent/shell/command","params":{"idempotency_key":"debug-config","input":"/debug-config history.lines"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(
        response.contains(r#""command":"debug-config""#),
        "{response}"
    );
    assert!(response.contains("source=runtime-config"), "{response}");
    assert!(response.contains("layers=1"), "{response}");
    assert!(response.contains("applied_layers=1"), "{response}");
    assert!(response.contains("permission_preset=auto"), "{response}");
    assert!(
        response.contains("approval_policy=full-access"),
        "{response}"
    );
    assert!(response.contains("layer=primary"), "{response}");
    assert!(response.contains("scope=primary"), "{response}");
    assert!(response.contains("format=toml"), "{response}");
    assert!(response.contains("value path=history.lines"), "{response}");
    assert!(response.contains("value=7"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
}

/// Verifies runtime agent shell prompt starts live turn lifecycle.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_agent_shell_prompt_starts_live_turn_lifecycle() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-prompt","input":"summarize the pane"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"turn_started""#), "{response}");
    assert!(response.contains(r#""command":null"#), "{response}");
    assert!(response.contains(r#""body":null"#), "{response}");
    assert!(response.contains(r#""state":"running""#), "{response}");
    let response_json: serde_json::Value = serde_json::from_str(&response).unwrap();
    let turn = &response_json["result"]["turn"];
    assert_eq!(turn["id"], "turn-1", "{response}");
    assert_eq!(turn["version"], serde_json::json!(1), "{response}");
    assert_eq!(turn["agent_id"], "agent-%1", "{response}");
    assert_eq!(turn["state"], "running", "{response}");
    assert!(turn["created_at"].as_str().is_some(), "{response}");
    assert!(turn["started_at"].as_str().is_some(), "{response}");
    assert_eq!(turn["finished_at"], serde_json::Value::Null, "{response}");
    assert_eq!(turn["prompt_preview"], "summarize the pane", "{response}");
    assert_eq!(turn["approval_ids"], serde_json::json!([]), "{response}");
    assert_eq!(
        turn["result_summary"],
        serde_json::Value::Null,
        "{response}"
    );
    assert!(
        turn["extensions"]["context_blocks"].as_u64().is_some(),
        "{response}"
    );
    let tasks = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"tasks","method":"agent/task/list","params":{"target":{"pane_id":"%1"}}}"#,
        &primary,
    );
    assert!(tasks.contains(r#""id":"turn-1""#), "{tasks}");
    assert!(tasks.contains(r#""state":"running""#), "{tasks}");
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].turn_id, "turn-1");
    assert_eq!(pending[0].model_profile.provider, "openai");
    assert_eq!(pending[0].model_profile.model, "gpt-5.5");
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("agent: working on"), "{pane_text}");
}

/// Verifies that a user prompt and a non-command agent response are written
/// into the pane's normal terminal buffer instead of a transient prompt
/// overlay. This preserves the Codex-like interaction transcript as copyable
/// terminal text while still retaining terminal style spans for user-facing
/// color. Each injected line keeps the same Mezzanine UI prefix used by the
/// pane-local prompt so message boundaries are visible in the terminal buffer.
#[test]
fn runtime_agent_prompt_and_say_response_are_interleaved_in_pane_buffer() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 12).unwrap(), 100).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-visible-say","input":"summarize visible output"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap say response".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "say-1".to_string(),
                    rationale: "answer in the pane".to_string(),
                    payload: crate::agent::AgentActionPayload::Say {
                        status: crate::agent::SayStatus::Final,
                        text: "The pane is ready.".to_string(),
                        content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                            .to_string(),
                    },
                }],
                final_turn: true,
            }),
        },
    };
    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("user> summarize visible output"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("▐ user> summarize visible output"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("agent> The pane is ready."),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("▐ agent> The pane is ready."),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("agent> answer in the pane"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("agent: turn turn-1"), "{pane_text}");
    let theme = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .ui_theme;
    let assistant_line = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines()
        .into_iter()
        .find(|line| line.text.contains("agent> The pane is ready."))
        .unwrap();
    assert!(assistant_line.text.starts_with("▐ "));
    assert!(!assistant_line.style_spans.is_empty());
    let assistant_body_start = "▐ agent> ".chars().count();
    assert!(
        assistant_line
            .style_spans
            .iter()
            .all(|span| span.start.saturating_add(span.length) <= assistant_body_start),
        "assistant body text should use default terminal color: {:?}",
        assistant_line.style_spans
    );
    assert!(
        assistant_line.style_spans.iter().any(|span| {
            span.start == 0
                && span.rendition.foreground
                    == Some(theme.colors.agent_transcript_assistant.foreground)
                && span.rendition.background.is_none()
                && span.rendition.bold
        }),
        "assistant gutter and label should use themed foreground without a background: {:?}",
        assistant_line.style_spans
    );
    let user_line = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines()
        .into_iter()
        .find(|line| line.text.contains("user> summarize visible output"))
        .unwrap();
    let user_body_start = "▐ user> ".chars().count();
    assert!(
        user_line
            .style_spans
            .iter()
            .all(|span| span.start.saturating_add(span.length) <= user_body_start),
        "user prompt body text should use default terminal color: {:?}",
        user_line.style_spans
    );
    assert!(
        user_line.style_spans.iter().any(|span| {
            span.start == 0
                && span.rendition.foreground == Some(theme.colors.agent_transcript_user.foreground)
                && span.rendition.background.is_none()
                && span.rendition.bold
        }),
        "user gutter and label should use themed foreground without a background: {:?}",
        user_line.style_spans
    );
    service
        .append_agent_error_text_to_terminal_buffer("%1", "agent error: failed")
        .unwrap();
    service
        .append_agent_command_preview_to_terminal_buffer("%1", "ls -la")
        .unwrap();
    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let error_line = styled_lines
        .iter()
        .find(|line| line.text.contains("agent error: failed"))
        .unwrap();
    assert!(
        error_line.style_spans.iter().any(|span| {
            span.start == 0
                && span.rendition.foreground == Some(theme.colors.agent_transcript_error.foreground)
                && span.rendition.background.is_none()
                && span.rendition.bold
        }),
        "error transcript lines should use themed error foreground without a background: {:?}",
        error_line.style_spans
    );
    let command_line = styled_lines
        .iter()
        .find(|line| line.text.contains("$ ls -la"))
        .unwrap();
    assert!(
        command_line.style_spans.iter().any(|span| {
            span.start == 0
                && span.rendition.foreground
                    == Some(theme.colors.agent_transcript_command.foreground)
                && span.rendition.background.is_none()
                && span.rendition.bold
        }),
        "command transcript lines should use themed command foreground without a background: {:?}",
        command_line.style_spans
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies markdown `say` output is rendered as presentation-only styling.
///
/// The display path should remove visual markdown delimiters and add terminal
/// style spans for readability, while copy mode must still return the raw
/// markdown authored by the model. This protects markdown as the first
/// content-type renderer without hard-coding future media types into copy mode.
#[test]
fn runtime_agent_markdown_say_renders_styled_presentation_and_copies_raw_markdown() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-markdown","method":"agent/shell/command","params":{"idempotency_key":"agent-markdown-say","input":"render markdown"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let markdown = "**Important** and <u>underlined</u>\n- first";
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "markdown say response".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "say-1".to_string(),
                    rationale: String::new(),
                    payload: crate::agent::AgentActionPayload::Say {
                        status: crate::agent::SayStatus::Final,
                        text: markdown.to_string(),
                        content_type: crate::agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE
                            .to_string(),
                    },
                }],
                final_turn: true,
            }),
        },
    };

    service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let assistant_line = styled_lines
        .iter()
        .find(|line| line.text.contains("agent> Important and underlined"))
        .unwrap();
    let assistant_index = styled_lines
        .iter()
        .position(|line| line.text == assistant_line.text)
        .unwrap();
    let expected_divider = expected_markdown_block_divider_line(80);
    assert!(
        assistant_index == 0 || styled_lines[assistant_index - 1].text != expected_divider,
        "{styled_lines:?}"
    );
    assert!(
        !assistant_line.text.contains("**") && !assistant_line.text.contains("<u>"),
        "{assistant_line:?}"
    );
    assert!(
        assistant_line
            .style_spans
            .iter()
            .any(|span| span.rendition.bold && span.start >= "▐ agent> ".chars().count()),
        "{assistant_line:?}"
    );
    assert!(
        assistant_line
            .style_spans
            .iter()
            .any(|span| span.rendition.underline && span.start >= "▐ agent> ".chars().count()),
        "{assistant_line:?}"
    );
    assert!(
        styled_lines
            .iter()
            .any(|line| line.text.contains("• first")),
        "{styled_lines:?}"
    );
    assert!(
        styled_lines
            .iter()
            .all(|line| line.text != expected_divider),
        "{styled_lines:?}"
    );

    let copy_mode = service.ensure_active_copy_mode("%1").unwrap();
    let scroll_top = copy_mode.scroll_top();
    let visible_lines = copy_mode.visible_lines();
    let first_line = visible_lines
        .iter()
        .position(|line| line.contains("agent> Important and underlined"))
        .map(|line| line + scroll_top)
        .unwrap();
    let second_line = visible_lines
        .iter()
        .position(|line| line.contains("• first"))
        .map(|line| line + scroll_top)
        .unwrap();
    let second_column = visible_lines[second_line.saturating_sub(scroll_top)]
        .chars()
        .count();
    copy_mode
        .select_range(
            CopyPosition {
                line: first_line,
                column: 0,
            },
            CopyPosition {
                line: second_line,
                column: second_column,
            },
        )
        .unwrap();

    assert_eq!(copy_mode.copy_selection().unwrap(), markdown);
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies wrapped markdown presentation rows still copy as raw markdown.
///
/// Wide Markdown tables can render across several terminal rows in a narrow
/// pane. Copy mode should treat those extra rows as presentation-only so the
/// copied text remains a valid pipe table rather than including display wraps
/// or Unicode table borders.
#[test]
fn runtime_agent_markdown_copy_preserves_raw_table_when_rendered_rows_wrap() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(34, 12).unwrap(), 120)
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(34, 12).unwrap(), 120).unwrap(),
    );
    let markdown = "| Name | Description |\n| --- | --- |\n| alpha | this description is intentionally long enough to wrap in a narrow pane |";

    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            markdown,
            crate::agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
        )
        .unwrap();

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("│"),
        "table should be rendered as terminal presentation: {pane_text}"
    );
    let copy_mode = service.ensure_active_copy_mode("%1").unwrap();
    let visible_lines = copy_mode.visible_lines();
    let last_visible_index = visible_lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .unwrap_or_else(|| visible_lines.len().saturating_sub(1));
    let last_line = copy_mode.scroll_top().saturating_add(last_visible_index);
    let last_column = visible_lines
        .get(last_visible_index)
        .map(|line| line.chars().count())
        .unwrap_or_default();
    copy_mode
        .select_range(
            CopyPosition { line: 0, column: 0 },
            CopyPosition {
                line: last_line,
                column: last_column,
            },
        )
        .unwrap();

    let copied = copy_mode.copy_selection().unwrap();
    assert_eq!(copied, markdown);
    assert!(!copied.contains('│'), "{copied}");
}

/// Verifies plain `agent>` output wraps under the assistant indicator.
///
/// Markdown output already has element-aware continuation indentation. Plain
/// assistant text should use the same transcript geometry instead of relying
/// on terminal soft wrapping, whose continuation starts too far left.
#[test]
fn runtime_agent_plain_say_wraps_under_agent_indicator() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(28, 12).unwrap(), 120).unwrap(),
    );

    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            "alpha beta gamma delta epsilon",
            crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE,
        )
        .unwrap();

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("▐ agent> alpha beta gamma"),
        "{pane_text}"
    );
    assert!(pane_text.contains("▐        delta epsilon"), "{pane_text}");
}

/// Verifies model-authored diff output uses the diff content renderer.
///
/// Diffs are a structured text media type rather than prose. The runtime should
/// parse the unified diff, omit raw diff scaffolding from the visible pane log,
/// and apply file-aware token colors to changed source lines when the file path
/// identifies a supported syntax.
#[test]
fn runtime_agent_diff_say_renders_file_aware_syntax_spans() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-diff","method":"agent/shell/command","params":{"idempotency_key":"agent-diff-say","input":"show diff"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let diff = "diff -- update file\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,1 +1,1 @@\n-fn old() {}\n+fn new() {}";
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "diff say response".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "say-diff".to_string(),
                    rationale: String::new(),
                    payload: crate::agent::AgentActionPayload::Say {
                        status: crate::agent::SayStatus::Final,
                        text: diff.to_string(),
                        content_type: crate::agent::AGENT_OUTPUT_TEXT_DIFF_CONTENT_TYPE.to_string(),
                    },
                }],
                final_turn: true,
            }),
        },
    };

    service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let pane_text = styled_lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        pane_text.contains("• Edited src/main.rs (+1 -1)"),
        "{pane_text}"
    );
    assert!(pane_text.contains("       1 +fn new() {}"), "{pane_text}");
    assert!(!pane_text.contains("diff -- update file"), "{pane_text}");
    let addition_line = styled_lines
        .iter()
        .find(|line| line.text.contains("       1 +fn new() {}"))
        .unwrap();
    let syntax_start = "▐ ".chars().count() + 10;
    assert!(
        addition_line
            .style_spans
            .iter()
            .any(|span| span.start >= syntax_start && span.rendition.foreground.is_some()),
        "{addition_line:?}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies display-only `say` actions can show raw Mezzanine patch examples.
///
/// When a user asks to see a patch, the patch text is ordinary assistant
/// output and must not be parsed as markdown structure, executed as a semantic
/// mutation, or collapsed into a no-output placeholder.
#[test]
fn runtime_agent_markdown_say_displays_raw_mez_patch_examples() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(96, 24).unwrap(), 120)
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(96, 24).unwrap(), 120).unwrap(),
    );
    let patch = "*** Begin Patch\n*** Update File: docs/example.md\n@@\n-old\n+new\n*** End Patch";

    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            patch,
            crate::agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
        )
        .unwrap();

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("agent> *** Begin Patch"), "{pane_text}");
    assert!(
        pane_text.contains("       *** Update File: docs/example.md"),
        "{pane_text}"
    );
    assert!(pane_text.contains("       +new"), "{pane_text}");
    assert!(!pane_text.contains("[mez: no output]"), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies CommonMark block and inline constructs are rendered from a parser
/// instead of the older delimiter scanner.
///
/// This covers features that were not supported by the line-oriented renderer:
/// ordered lists, task markers, block quotes, links, tables, fenced code blocks,
/// emphasis, and strikethrough. The test checks display text and terminal
/// styles so regressions point at both parsing and presentation failures.
#[test]
fn runtime_agent_commonmark_say_renders_rich_markdown_features() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(96, 40).unwrap(), 120)
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(96, 40).unwrap(), 120).unwrap(),
    );
    let markdown = "# Heading\n\n> quoted **bold** text\n\n1. first\n2. second\n\n- [x] done\n\n`code` and *em*\n\n[link](https://example.com)\n\n| Name | Count |\n|:--|--:|\n| alpha | 2 |\n\n```rust\nfn main() {}\n```\n\n~~gone~~\n\nparagraph\n## Later";

    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            markdown,
            crate::agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
        )
        .unwrap();

    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let heading = styled_lines
        .iter()
        .find(|line| line.text.trim_end().ends_with("Heading"))
        .unwrap();
    let heading_index = styled_lines
        .iter()
        .position(|line| line.text == heading.text)
        .unwrap();
    assert!(
        styled_lines
            .iter()
            .all(|line| line.text != expected_markdown_block_divider_line(96)),
        "{styled_lines:?}"
    );
    assert!(!heading.text.contains('#'), "{heading:?}");
    assert!(heading.style_spans.iter().any(|span| {
        span.rendition.bold && span.rendition.underline && span.start >= "▐ agent> ".chars().count()
    }));

    let quote = styled_lines
        .iter()
        .find(|line| line.text.contains("> quoted bold text"))
        .unwrap();
    assert!(quote.style_spans.iter().any(|span| span.rendition.bold));
    assert!(
        styled_lines
            .iter()
            .any(|line| line.text.contains("1. first")),
        "{styled_lines:?}"
    );
    assert!(
        styled_lines
            .iter()
            .any(|line| line.text.contains("2. second")),
        "{styled_lines:?}"
    );
    assert!(
        styled_lines
            .iter()
            .any(|line| line.text.contains("[x] done")),
        "{styled_lines:?}"
    );

    let inline = styled_lines
        .iter()
        .find(|line| line.text.contains("code and em"))
        .unwrap();
    assert!(
        inline.style_spans.iter().any(|span| {
            !span.rendition.inverse
                && span.rendition.background.is_none()
                && span.rendition.foreground == Some(EXPECTED_MARKDOWN_INLINE_CODE_FOREGROUND)
        }),
        "{inline:?}"
    );
    assert!(inline.style_spans.iter().any(|span| span.rendition.italic));

    let link = styled_lines
        .iter()
        .find(|line| line.text.contains("link (https://example.com)"))
        .unwrap();
    assert!(link.style_spans.iter().any(|span| span.rendition.underline));
    assert!(link.style_spans.iter().any(|span| span.rendition.dim));

    assert!(
        styled_lines
            .iter()
            .any(|line| line.text.contains("│ Name") && line.text.contains("Count │")),
        "{styled_lines:?}"
    );
    assert!(
        styled_lines
            .iter()
            .any(|line| line.text.contains("├") && line.text.contains("┼")),
        "{styled_lines:?}"
    );
    let table_row = styled_lines
        .iter()
        .find(|line| line.text.contains("│ alpha") && line.text.contains("2 │"))
        .unwrap();
    assert!(
        table_row.style_spans.iter().any(|span| {
            span.rendition.foreground == Some(EXPECTED_MARKDOWN_TABLE_ALTERNATE_ROW_FOREGROUND)
                && span.rendition.background.is_none()
        }),
        "{table_row:?}"
    );
    assert!(
        styled_lines
            .iter()
            .any(|line| line.text.contains("fn main() {}")
                && line.style_spans.iter().all(|span| !span.rendition.dim)),
        "{styled_lines:?}"
    );
    assert!(
        styled_lines.iter().any(|line| line.text.contains("gone")
            && line
                .style_spans
                .iter()
                .any(|span| span.rendition.strikethrough)),
        "{styled_lines:?}"
    );
    assert!(
        styled_lines
            .iter()
            .skip(heading_index + 1)
            .all(|line| line.text != expected_markdown_block_divider_line(96)),
        "{styled_lines:?}"
    );
    let later_heading_index = styled_lines
        .iter()
        .position(|line| line.text.contains("Later"))
        .unwrap();
    assert!(
        later_heading_index > 0 && styled_lines[later_heading_index - 1].text.trim_end() == "▐",
        "{styled_lines:?}"
    );
}

/// Verifies markdown neutral accents switch to dark greys on light themes.
///
/// Inline code and table alternation are foreground-only presentation accents,
/// so they must derive their lightness from the active theme surface instead of
/// assuming a dark terminal background.
#[test]
fn runtime_agent_markdown_uses_dark_neutral_accents_on_light_theme() {
    let mut service = test_runtime_service();
    service.ui_theme = crate::terminal::resolve_ui_theme(
        "catppuccin_latte",
        crate::terminal::builtin_ui_theme_definition("catppuccin_latte").unwrap(),
    )
    .unwrap();
    service
        .attach_primary("primary", true, Size::new(80, 16).unwrap(), 120)
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 16).unwrap(), 120).unwrap(),
    );

    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            "`code`\n\n| Name | Count |\n|:--|--:|\n| alpha | 2 |",
            crate::agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
        )
        .unwrap();

    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let inline = styled_lines
        .iter()
        .find(|line| line.text.contains("code"))
        .unwrap();
    assert!(
        inline.style_spans.iter().any(|span| {
            span.rendition.foreground == Some(TerminalColor::Rgb(0x42, 0x42, 0x42))
                && span.rendition.background.is_none()
        }),
        "{inline:?}"
    );
    let table_row = styled_lines
        .iter()
        .find(|line| line.text.contains("│ alpha") && line.text.contains("2 │"))
        .unwrap();
    assert!(
        table_row.style_spans.iter().any(|span| {
            span.rendition.foreground == Some(TerminalColor::Rgb(0x5a, 0x5a, 0x5a))
                && span.rendition.background.is_none()
        }),
        "{table_row:?}"
    );
}

/// Verifies markdown presentation wraps at the smaller of pane width or 120
/// cells and indents continuation rows under the rendered list marker.
///
/// Wide panes should not produce unreadably long markdown transcript rows, and
/// continuation rows should retain enough structural indentation to make lists
/// readable after wrapping.
#[test]
fn runtime_agent_markdown_wraps_to_120_cells_and_indents_continuations() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(200, 40).unwrap(), 120)
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(200, 40).unwrap(), 120).unwrap(),
    );
    let markdown = format!("- {}", "alphabet ".repeat(40));

    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            &markdown,
            crate::agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
        )
        .unwrap();

    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    assert!(
        styled_lines
            .iter()
            .all(|line| line.text != expected_markdown_block_divider_line(120)),
        "{styled_lines:?}"
    );
    let continuation_prefix = format!("▐ {}", " ".repeat("agent> • ".chars().count()));
    let wrapped_lines = styled_lines
        .iter()
        .filter(|line| {
            line.text.contains("agent> • alphabet") || line.text.starts_with(&continuation_prefix)
        })
        .collect::<Vec<_>>();

    assert!(wrapped_lines.len() > 1, "{styled_lines:?}");
    assert!(
        wrapped_lines
            .iter()
            .all(|line| line.text.chars().count() <= 120),
        "{wrapped_lines:?}"
    );
    assert!(
        wrapped_lines
            .iter()
            .skip(1)
            .all(|line| line.text.starts_with(&continuation_prefix)),
        "{wrapped_lines:?}"
    );
}

/// Verifies markdown tables keep their row layout on wide terminals.
///
/// Prose markdown is capped at 120 cells for readability, but table rows need
/// to remain horizontally inspectable until they exceed the actual pane width.
#[test]
fn runtime_agent_markdown_tables_wrap_only_at_terminal_width() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(200, 40).unwrap(), 120)
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(200, 40).unwrap(), 120).unwrap(),
    );
    let first_cell = "alpha".repeat(18);
    let second_cell = "beta".repeat(8);
    let markdown = format!("| Long | Other |\n| --- | --- |\n| {first_cell} | {second_cell} |");

    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            &markdown,
            crate::agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
        )
        .unwrap();

    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let data_row = styled_lines
        .iter()
        .find(|line| line.text.contains(&first_cell) && line.text.contains(&second_cell))
        .unwrap();

    assert!(
        data_row.text.chars().count() > 120,
        "table row should exceed the prose cap: {data_row:?}"
    );
    assert!(
        data_row.text.chars().count() <= 200,
        "table row should still fit the terminal width: {data_row:?}"
    );
    assert!(
        data_row.text.contains("│") && data_row.text.contains(&second_cell),
        "{data_row:?}"
    );
}

/// Verifies markdown display bodies from agent slash commands use the shared
/// command-output pager instead of being appended as ordinary pane transcript.
///
/// `/status` emits a markdown table rather than model-authored prose. It should
/// open the same navigable display overlay used by `:` command output, with
/// markdown heading syntax stripped and tables rendered for terminal reading.
#[test]
fn runtime_agent_slash_markdown_display_opens_command_overlay() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 60).unwrap(), 120).unwrap(),
    );

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"/status\r".to_vec(),
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    let overlay = service
        .primary_display_overlay
        .as_ref()
        .expect("/status should open the command display overlay");
    let heading_index = overlay
        .lines
        .iter()
        .position(|line| line.contains("Agent Status"))
        .unwrap();
    let heading_line = &overlay.lines[heading_index];
    assert!(!heading_line.contains("##"), "{heading_line:?}");
    assert!(!heading_line.contains("agent>"), "{heading_line:?}");
    assert_eq!(heading_line, "Agent Status");
    assert!(
        overlay
            .lines
            .iter()
            .any(|line| line.contains("│ Field") && line.contains("Value")),
        "{overlay:?}"
    );
    assert!(
        overlay
            .lines
            .iter()
            .all(|line| !line.contains("Quota Usage")),
        "{overlay:?}"
    );
}

/// Verifies that a provider response containing only a final completion marker
/// still leaves an explicit pane-buffer status. This prevents the default
/// non-verbose view from looking silent when the model forgets to include a
/// user-facing `say` action.
#[test]
fn runtime_agent_complete_without_say_reports_visible_completion_status() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-visible-complete","input":"finish silently"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap complete response".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "the task is complete".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "complete-1".to_string(),
                    rationale: String::new(),
                    payload: crate::agent::AgentActionPayload::Complete,
                }],
                final_turn: true,
            }),
        },
    };
    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: completed without a user-facing response"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("thinking: the task is complete"),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that model-authored thinking text is not rendered a second time
/// when another action in the same response already presents the same text as
/// a `say` action. Models commonly emit a short `say` plus a matching
/// batch-level `thinking:` rationale; the pane should show the user-visible
/// answer once rather than adding a grey duplicate.
#[test]
fn runtime_agent_suppresses_batch_rationale_that_duplicates_say_text() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-duplicate-thinking","input":"respond once"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let visible = "I will handle the next step.";
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap say and complete response".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: format!("thinking: {visible}"),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![
                    crate::agent::AgentAction {
                        id: "say-1".to_string(),
                        rationale: String::new(),
                        payload: crate::agent::AgentActionPayload::Say {
                            status: crate::agent::SayStatus::Final,
                            text: visible.to_string(),
                            content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                                .to_string(),
                        },
                    },
                    crate::agent::AgentAction {
                        id: "complete-1".to_string(),
                        rationale: String::new(),
                        payload: crate::agent::AgentActionPayload::Complete,
                    },
                ],
                final_turn: true,
            }),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(pane_text.matches(visible).count(), 1, "{pane_text}");
    assert!(
        pane_text.contains(&format!("agent> {visible}")),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that normal mode renders shell commands selected by the agent into
/// the same pane terminal buffer before they are sent to the PTY. Users should
/// be able to monitor the exact command stream without enabling raw shell
/// output or wrapper diagnostics.
#[test]
fn runtime_agent_shell_command_is_presented_before_pty_dispatch() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 12).unwrap(), 20).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-visible-command","input":"run a harmless command"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap shell response".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "check shell access".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "shell-1".to_string(),
                    rationale: String::new(),
                    payload: crate::agent::AgentActionPayload::ShellCommand {
                        summary: "Check shell access".to_string(),
                        command: "if true; then echo \"ok\"; fi".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
        },
    };
    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        !pane_text.contains("agent> Check shell access"),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("agent: Check shell access"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("thinking: check shell access"),
        "{pane_text}"
    );
    assert_eq!(
        pane_text.matches("$ if true; then echo \"ok\"; fi").count(),
        1
    );
    let command_line = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines()
        .into_iter()
        .find(|line| line.text.contains("$ if true; then echo \"ok\"; fi"))
        .unwrap();
    let theme = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .ui_theme;
    assert!(command_line.style_spans.iter().any(|span| {
        span.start >= 2
            && span.rendition.foreground.is_some_and(|foreground| {
                foreground != theme.colors.agent_transcript_command.foreground
            })
    }));
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies hidden model shell commands expose a single live latest-output row.
///
/// Normal logging hides raw PTY output, but users still need lightweight
/// progress for long-running commands. The latest cleaned stdout/stderr line
/// should replace the previous transient row and disappear when the next durable
/// agent transcript line is written.
#[test]
fn runtime_hidden_model_shell_command_shows_transient_latest_output_line() {
    let mut service = test_runtime_service();
    let mut screen = TerminalScreen::new(Size::new(80, 12).unwrap(), 20).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service
        .start_agent_prompt_turn("%1", "run a command")
        .unwrap();
    assert_eq!(start.state, AgentTurnState::Running);
    service.pending_agent_provider_tasks.remove("turn-1");
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .cloned()
        .unwrap();
    let action = crate::agent::AgentAction {
        id: "shell-1".to_string(),
        rationale: "run a command".to_string(),
        payload: crate::agent::AgentActionPayload::ShellCommand {
            summary: "Run a command".to_string(),
            command: "sleep 1".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    service.agent_turn_executions.insert(
        "turn-1".to_string(),
        crate::agent::AgentTurnExecution {
            request: crate::agent::ModelRequest {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                reasoning_effort: None,
                prompt_cache_retention: None,
                max_output_tokens: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                available_mcp_tools: Vec::new(),
                interaction_kind: crate::agent::ModelInteractionKind::ActionExecution,
                allowed_actions: crate::agent::AllowedActionSet::for_capability(
                    crate::agent::AgentCapability::Shell,
                ),
                messages: vec![crate::agent::ModelMessage {
                    role: crate::agent::ModelMessageRole::User,
                    source: ContextSourceKind::UserInstruction,
                    content: "run a command".to_string(),
                }],
            },
            response: crate::agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "run shell".to_string(),
                usage: Default::default(),
                quota_usage: Default::default(),
                action_batch: Some(crate::agent::MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "test action batch rationale".to_string(),
                    turn_id: "turn-1".to_string(),
                    agent_id: "agent-%1".to_string(),
                    actions: vec![action.clone()],
                    final_turn: false,
                }),
            },
            latest_response_usage: Default::default(),
            action_results: vec![crate::agent::ActionResult::running(
                &turn,
                &action,
                vec!["shell command accepted for pane execution".to_string()],
                None,
            )],
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        },
    );
    service
        .append_agent_command_preview_to_terminal_buffer("%1", "sleep 1")
        .unwrap();
    service.running_shell_transactions.insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "shell-1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "sleep 1".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    service.record_running_shell_transaction_output("%1", b"first output\n");
    let first_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(first_text.contains("first output"), "{first_text}");

    service.record_running_shell_transaction_output("%1", b"second output\n");
    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let second_text = styled_lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!second_text.contains("first output"), "{second_text}");
    assert!(second_text.contains("second output"), "{second_text}");
    let theme = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .ui_theme;
    let output_line = styled_lines
        .iter()
        .find(|line| line.text.contains("second output"))
        .unwrap();
    assert!(
        output_line.style_spans.iter().any(|span| {
            span.start == 0
                && span.rendition.foreground
                    == Some(theme.colors.agent_transcript_status.foreground)
                && span.rendition.dim
        }),
        "transient shell output should use muted status/thinking style: {:?}",
        output_line.style_spans
    );

    let encoded_tail =
        base64::engine::general_purpose::STANDARD.encode(b"decoded transported output\n");
    let transported_tail = format!(
        "__MEZ_SHELL_OUTPUT_BASE64_BEGIN__\n{encoded_tail}\n__MEZ_SHELL_OUTPUT_BASE64_END__\n"
    );
    service.record_running_shell_transaction_output("%1", transported_tail.as_bytes());
    let decoded_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        decoded_text.contains("decoded transported output"),
        "{decoded_text}"
    );
    assert!(
        !decoded_text.contains("__MEZ_SHELL_OUTPUT_BASE64_BEGIN__"),
        "{decoded_text}"
    );

    service.record_running_shell_transaction_output(
        "%1",
        b"final output\n\x1b]133;D;0;mez_marker=marker-1;mez_turn=turn-1;mez_agent=agent-%1;mez_pane=%1\x1b\\\r\n~/repo > ",
    );
    let final_output_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        final_output_text.contains("final output"),
        "{final_output_text}"
    );
    assert!(
        !final_output_text.contains("~/repo >"),
        "{final_output_text}"
    );
    assert!(
        !final_output_text
            .lines()
            .any(|line| line.trim_end().ends_with(">") && !line.contains("final output")),
        "{final_output_text}"
    );

    service.record_running_shell_transaction_output("%1", b"~/repo > ");
    let prompt_tail_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        prompt_tail_text.contains("final output"),
        "{prompt_tail_text}"
    );
    assert!(!prompt_tail_text.contains("~/repo >"), "{prompt_tail_text}");

    service
        .append_agent_status_text_to_terminal_buffer("%1", "agent: next stage")
        .unwrap();
    let final_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!final_text.contains("second output"), "{final_text}");
    assert!(final_text.contains("agent: next stage"), "{final_text}");
}

/// Verifies that planning-time shell action failures stay visible without
/// exposing the exact command in the default pane buffer. The user still sees
/// the policy failure, while command details remain reserved for verbose or
/// trace mode.
#[test]
fn runtime_agent_shell_planning_failure_hides_command_by_default() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 12).unwrap(), 20).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().add_rule(
        crate::permissions::CommandRule::new(["ls"], RuleDecision::Forbid, RuleMatch::Prefix)
            .unwrap(),
    );

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-failed-command","input":"list files"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap shell response".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "shell-1".to_string(),
                    rationale: "list files".to_string(),
                    payload: crate::agent::AgentActionPayload::ShellCommand {
                        summary: "List files".to_string(),
                        command: "ls".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert_eq!(execution.action_results[0].status, ActionStatus::Denied);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: List files (shell command denied before execution"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("before execution: ls"), "{pane_text}");
    assert!(!pane_text.contains("$ ls"), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that `/log-level verbose` is an explicit opt-in for low-level agent lifecycle
/// chatter. Normal mode keeps the pane buffer focused on prompts, assistant
/// text, concise progress, and errors; verbose mode restores provider,
/// protocol, command, and command-output diagnostics for debugging without
/// enabling thinking.
#[test]
fn runtime_agent_verbose_mode_injects_low_level_status_lines() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 12).unwrap(), 20).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let verbose = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"verbose","method":"agent/shell/command","params":{"idempotency_key":"agent-verbose","input":"/log-level verbose"}}"#,
        &primary,
    );
    assert!(
        verbose.contains("agent log level for pane %1 is now verbose."),
        "{verbose}"
    );

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-verbose-say","input":"summarize visible output"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap say response".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "say-1".to_string(),
                    rationale: "answer in the pane".to_string(),
                    payload: crate::agent::AgentActionPayload::Say {
                        status: crate::agent::SayStatus::Final,
                        text: "The pane is ready.".to_string(),
                        content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                            .to_string(),
                    },
                }],
                final_turn: true,
            }),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: thinking with runtime-batch model test"),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("agent> answer in the pane"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("agent: turn turn-1 completed"),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that `/log-level debug` exposes model introspection and action
/// rationales while still hiding the full shell view that verbose and trace show.
#[test]
fn runtime_agent_thinking_mode_injects_action_rationales() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 12).unwrap(), 100).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let thinking = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"thinking","method":"agent/shell/command","params":{"idempotency_key":"agent-thinking","input":"/log-level debug"}}"#,
        &primary,
    );
    assert!(
        thinking.contains("agent log level for pane %1 is now debug."),
        "{thinking}"
    );

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-thinking-say","input":"summarize visible output"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap say response".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "say-1".to_string(),
                    rationale: "answer in the pane".to_string(),
                    payload: crate::agent::AgentActionPayload::Say {
                        status: crate::agent::SayStatus::Final,
                        text: "The pane is ready.".to_string(),
                        content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                            .to_string(),
                    },
                }],
                final_turn: true,
            }),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent debug: turn turn-1: MAAP action_results"),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("agent> answer in the pane"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("agent> The pane is ready."),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that trace mode exposes the full MAAP exchange in the pane buffer:
/// the model request messages, raw provider response with the parsed action
/// batch, and action results. Summary-only tracing made auto-allow/full-access
/// hangs difficult to diagnose because the user could not copy the actual MAAP
/// messages that drove the state machine.
#[test]
fn runtime_agent_trace_mode_prints_maap_request_response_and_results() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(100, 16).unwrap(), 500).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Trace)
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-trace-maap","input":"trace maap please"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "trace-maap-raw-response".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "say-1".to_string(),
                    rationale: "show trace details".to_string(),
                    payload: crate::agent::AgentActionPayload::Say {
                        status: crate::agent::SayStatus::Final,
                        text: "Trace visible.".to_string(),
                        content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                            .to_string(),
                    },
                }],
                final_turn: true,
            }),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent trace: turn turn-1: MAAP request"),
        "{pane_text}"
    );
    assert!(pane_text.contains(r#""role": "user""#), "{pane_text}");
    assert!(pane_text.contains("trace maap please"), "{pane_text}");
    assert!(
        pane_text.contains("agent trace: turn turn-1: MAAP response"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains(r#""raw_text": "trace-maap-raw-response""#),
        "{pane_text}"
    );
    assert!(pane_text.contains(r#""action_batch""#), "{pane_text}");
    assert!(pane_text.contains(r#""type": "say""#), "{pane_text}");
    assert!(
        pane_text.contains("agent trace: turn turn-1: MAAP action_results"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains(r#""status": "succeeded""#),
        "{pane_text}"
    );
    assert!(pane_text.contains(r#""structured_content""#), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that normal-mode panes retain a bounded hidden trace log that can
/// later be dumped to the pane, an internal paste buffer, or the clipboard.
///
/// This protects post-failure diagnostics: users should not have to predict in
/// advance that trace mode will be needed, but the retained trace remains
/// bounded and explicit to export.
#[test]
fn runtime_agent_copy_trace_log_retains_hidden_trace_and_writes_destinations() {
    let _clipboard_guard = TEST_HOST_CLIPBOARD_TEST_LOCK.lock().unwrap();
    TEST_HOST_CLIPBOARD_WRITES.lock().unwrap().clear();
    let mut service = test_runtime_service();
    service.host_clipboard =
        HostClipboard::new(record_host_clipboard_copy, empty_host_clipboard_read);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-trace-log","input":"trace retention sentinel"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "trace raw sentinel".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "say-1".to_string(),
                    rationale: "retain trace details".to_string(),
                    payload: crate::agent::AgentActionPayload::Say {
                        status: crate::agent::SayStatus::Final,
                        text: "Trace retained.".to_string(),
                        content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                            .to_string(),
                    },
                }],
                final_turn: true,
            }),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let pane_text_before = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        !pane_text_before.contains("agent trace: turn turn-1: MAAP request"),
        "{pane_text_before}"
    );

    let buffer_response = service
        .execute_agent_shell_command(&primary, "/copy-trace-log buffer retained-trace")
        .unwrap();
    assert!(
        buffer_response.contains(r#""command":"copy-trace-log""#),
        "{buffer_response}"
    );
    assert!(
        buffer_response.contains(r#""kind":"mutated""#),
        "{buffer_response}"
    );
    assert!(
        buffer_response.contains("destination=buffer"),
        "{buffer_response}"
    );
    let buffer = service.paste_buffers.get("retained-trace").unwrap();
    assert!(buffer.contains("trace raw sentinel"), "{buffer}");
    assert!(
        buffer.contains("agent trace: turn turn-1: MAAP response"),
        "{buffer}"
    );

    let clipboard_response = service
        .execute_agent_shell_command(&primary, "/copy-trace-log clipboard")
        .unwrap();
    assert!(
        clipboard_response.contains("destination=clipboard"),
        "{clipboard_response}"
    );
    let clipboard = service.paste_buffers.get("clipboard").unwrap();
    assert!(clipboard.contains("trace raw sentinel"), "{clipboard}");
    assert!(
        TEST_HOST_CLIPBOARD_WRITES
            .lock()
            .unwrap()
            .last()
            .is_some_and(|text| text.contains("trace raw sentinel"))
    );

    let pane_response = service
        .execute_agent_shell_command(&primary, "/copy-trace-log pane")
        .unwrap();
    assert!(
        pane_response.contains("destination=pane"),
        "{pane_response}"
    );
    let pane_text_after = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text_after.contains("agent trace log for pane %1"),
        "{pane_text_after}"
    );
    assert!(
        pane_text_after.contains("trace raw sentinel"),
        "{pane_text_after}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies `/copy-context` exports the assembled provider request context
/// through the same pane, buffer, and clipboard targets as the other copy
/// commands.
///
/// The idle path is intentionally covered here because users invoke this
/// diagnostic command when they need to inspect the next prompt's context
/// before a turn is running.
#[test]
fn runtime_agent_copy_context_writes_idle_context_to_destinations() {
    let _clipboard_guard = TEST_HOST_CLIPBOARD_TEST_LOCK.lock().unwrap();
    TEST_HOST_CLIPBOARD_WRITES.lock().unwrap().clear();
    let mut service = test_runtime_service();
    service.host_clipboard =
        HostClipboard::new(record_host_clipboard_copy, empty_host_clipboard_read);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let buffer_response = service
        .execute_agent_shell_command(&primary, "/copy-context buffer retained-context")
        .unwrap();
    assert!(
        buffer_response.contains(r#""command":"copy-context""#),
        "{buffer_response}"
    );
    assert!(
        buffer_response.contains(r#""kind":"mutated""#),
        "{buffer_response}"
    );
    assert!(
        buffer_response.contains("destination=buffer"),
        "{buffer_response}"
    );
    let buffer = service.paste_buffers.get("retained-context").unwrap();
    assert!(
        buffer.contains(r#""kind": "model_request_context_dump""#),
        "{buffer}"
    );
    assert!(buffer.contains("idle-context-preview-%1"), "{buffer}");

    let clipboard_response = service
        .execute_agent_shell_command(&primary, "/copy-context clipboard")
        .unwrap();
    assert!(
        clipboard_response.contains("destination=clipboard"),
        "{clipboard_response}"
    );
    let clipboard = service.paste_buffers.get("clipboard").unwrap();
    assert!(
        clipboard.contains(r#""kind": "model_request_context_dump""#),
        "{clipboard}"
    );
    assert!(
        TEST_HOST_CLIPBOARD_WRITES
            .lock()
            .unwrap()
            .last()
            .is_some_and(|text| text.contains("idle-context-preview-%1"))
    );

    let pane_response = service
        .execute_agent_shell_command(&primary, "/copy-context pane")
        .unwrap();
    assert!(
        pane_response.contains("destination=pane"),
        "{pane_response}"
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("model_request_context_dump"),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies `/copy-patches` exports exact retained patch payloads and statuses
/// through the same pane, buffer, and clipboard targets as `/copy-trace-log`.
///
/// Patch bodies are deliberately omitted from durable transcript summaries, so
/// this command must use the runtime's structured patch ledger rather than
/// scraping rendered pane text or compact transcript entries.
#[test]
fn runtime_agent_copy_patches_writes_retained_patches_to_destinations() {
    let _clipboard_guard = TEST_HOST_CLIPBOARD_TEST_LOCK.lock().unwrap();
    TEST_HOST_CLIPBOARD_WRITES.lock().unwrap().clear();
    let mut service = test_runtime_service();
    service.host_clipboard =
        HostClipboard::new(record_host_clipboard_copy, empty_host_clipboard_read);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let target_rel = format!(
        "target/mez-copy-patches-export-{}-{unique}/note.txt",
        std::process::id()
    );
    let target = PathBuf::from(&target_rel);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    let patch = format!("*** Begin Patch\n*** Add File: {target_rel}\n+alpha\n*** End Patch");

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-copy-patches","input":"create a note"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap patch response".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "patch-1".to_string(),
                    rationale: "write a note".to_string(),
                    payload: crate::agent::AgentActionPayload::ApplyPatch {
                        patch: patch.clone(),
                        strip: None,
                    },
                }],
                final_turn: true,
            }),
        },
    };
    service.pending_agent_provider_tasks.remove("turn-1");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    poll_until_turn_state(&mut service, "turn-1", AgentTurnState::Completed);

    let buffer_response = service
        .execute_agent_shell_command(&primary, "/copy-patches buffer retained-patches")
        .unwrap();
    assert!(
        buffer_response.contains(r#""command":"copy-patches""#),
        "{buffer_response}"
    );
    assert!(
        buffer_response.contains("destination=buffer"),
        "{buffer_response}"
    );
    let buffer = service.paste_buffers.get("retained-patches").unwrap();
    assert!(buffer.contains("agent patches for pane %1"), "{buffer}");
    assert!(
        buffer.contains("patch 1: turn=turn-1 action=patch-1 status=succeeded"),
        "{buffer}"
    );
    assert!(buffer.contains("*** Begin Patch"), "{buffer}");
    assert!(buffer.contains(&target_rel), "{buffer}");
    assert!(buffer.contains("+alpha"), "{buffer}");

    let clipboard_response = service
        .execute_agent_shell_command(&primary, "/copy-patches clipboard")
        .unwrap();
    assert!(
        clipboard_response.contains("destination=clipboard"),
        "{clipboard_response}"
    );
    let clipboard = service.paste_buffers.get("clipboard").unwrap();
    assert!(clipboard.contains("status=succeeded"), "{clipboard}");
    assert!(
        TEST_HOST_CLIPBOARD_WRITES
            .lock()
            .unwrap()
            .last()
            .is_some_and(|text| text.contains(&patch))
    );

    let pane_response = service
        .execute_agent_shell_command(&primary, "/copy-patches pane")
        .unwrap();
    assert!(
        pane_response.contains("destination=pane"),
        "{pane_response}"
    );
    let pane_text_after = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text_after.contains("agent patches for pane %1"),
        "{pane_text_after}"
    );
    assert!(
        pane_text_after.contains("status=succeeded"),
        "{pane_text_after}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies `/copy-patches` keeps every patch attempt for a session even when
/// recovery reuses the same turn id and model-authored action id.
///
/// Patch recovery often happens inside one agent turn, and models frequently
/// reuse simple action ids such as `patch`. The export ledger must therefore
/// treat a new running patch after a settled patch as a new attempt rather than
/// overwriting the earlier failed or successful attempt.
#[test]
fn runtime_agent_copy_patches_retains_reused_action_id_attempts() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    let started = service
        .start_agent_prompt_turn("%1", "patch the file")
        .unwrap();
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");

    let first_patch = "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch";
    let second_patch =
        "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-current\n+updated\n*** End Patch";
    let build_execution =
        |patch: &str, result: crate::agent::ActionResult| crate::agent::AgentTurnExecution {
            request: runtime_model_request_fixture(&turn.turn_id),
            response: crate::agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: format!("patch attempt for {}", result.action_id),
                usage: Default::default(),
                quota_usage: Default::default(),
                action_batch: Some(crate::agent::MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "test action batch rationale".to_string(),
                    turn_id: turn.turn_id.clone(),
                    agent_id: turn.agent_id.clone(),
                    actions: vec![crate::agent::AgentAction {
                        id: result.action_id.clone(),
                        rationale: "apply a source patch".to_string(),
                        payload: crate::agent::AgentActionPayload::ApplyPatch {
                            patch: patch.to_string(),
                            strip: None,
                        },
                    }],
                    final_turn: false,
                }),
            },
            latest_response_usage: Default::default(),
            action_results: vec![result],
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        };
    let action_for_result = |patch: &str| crate::agent::AgentAction {
        id: "patch-retry".to_string(),
        rationale: "apply a source patch".to_string(),
        payload: crate::agent::AgentActionPayload::ApplyPatch {
            patch: patch.to_string(),
            strip: None,
        },
    };

    let first_action = action_for_result(first_patch);
    let first_running = crate::agent::ActionResult::running(
        &turn,
        &first_action,
        vec!["shell command accepted for pane execution".to_string()],
        None,
    );
    service.record_runtime_agent_patch_results_for_turn(
        &turn,
        &build_execution(first_patch, first_running),
    );
    let first_failed = crate::agent::ActionResult::failed(
        &turn,
        &first_action,
        ActionStatus::Failed,
        "shell_command_failed",
        "shell command exited with status 1",
    )
    .unwrap();
    service.record_runtime_agent_patch_results_for_turn(
        &turn,
        &build_execution(first_patch, first_failed),
    );

    let second_action = action_for_result(second_patch);
    let second_running = crate::agent::ActionResult::running(
        &turn,
        &second_action,
        vec!["shell command accepted for pane execution".to_string()],
        None,
    );
    service.record_runtime_agent_patch_results_for_turn(
        &turn,
        &build_execution(second_patch, second_running),
    );
    let second_succeeded = crate::agent::ActionResult::succeeded(
        &turn,
        &second_action,
        vec!["patch applied".to_string()],
        None,
    );
    service.record_runtime_agent_patch_results_for_turn(
        &turn,
        &build_execution(second_patch, second_succeeded),
    );

    let copy_response = service
        .execute_agent_shell_command(&primary, "/copy-patches buffer all-patches")
        .unwrap();
    assert!(
        copy_response.contains(r#""command":"copy-patches""#),
        "{copy_response}"
    );
    assert!(copy_response.contains("patches=2"), "{copy_response}");
    let retained = service.paste_buffers.get("all-patches").unwrap();
    assert!(
        retained.contains("patch 1: turn=turn-1 action=patch-retry status=failed"),
        "{retained}"
    );
    assert!(
        retained.contains("patch 2: turn=turn-1 action=patch-retry status=succeeded"),
        "{retained}"
    );
    assert!(retained.contains("-old"), "{retained}");
    assert!(retained.contains("+new"), "{retained}");
    assert!(retained.contains("-current"), "{retained}");
    assert!(retained.contains("+updated"), "{retained}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that `/log-level debug` exposes MAAP and state-machine diagnostics
/// without exposing the raw shell view. Debug should show the same diagnostic
/// categories as trace and preserve command fields inside MAAP objects, while
/// raw provider text and output previews stay hidden until the pane is
/// explicitly moved to trace.
#[test]
fn runtime_agent_debug_mode_prints_maap_without_shell_view() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Debug)
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-debug-maap","input":"debug maap please"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "debug-maap-raw-response with debug-secret-command".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "shell-1".to_string(),
                    rationale: "run a command for debug redaction".to_string(),
                    payload: crate::agent::AgentActionPayload::ShellCommand {
                        summary: "Run a debug redaction command".to_string(),
                        command: "printf 'debug-secret-command\\n'".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent debug: turn turn-1: MAAP response"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("agent debug: turn turn-1: MAAP request"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("hidden at debug log level; use /log-level trace"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains(r#""command": "printf 'debug-secret-command\\n'""#),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("debug-maap-raw-response"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("$ printf 'debug-secret-command"),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that a shell command selected by the model is monitorable when
/// verbose mode is enabled: the command line is injected before dispatch and
/// transaction output can settle without exposing wrapper internals.
#[test]
fn runtime_agent_shell_command_output_is_visible_in_verbose_mode() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Verbose)
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-visible-output","input":"print a marker"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap shell response".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "shell-1".to_string(),
                    rationale: "print a marker".to_string(),
                    payload: crate::agent::AgentActionPayload::ShellCommand {
                        summary: "Print a marker".to_string(),
                        command: "printf 'agent-visible-%s\\n' output".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: true,
            }),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    for _ in 0..100 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        if service.running_shell_transactions.is_empty() {
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(
        service.running_shell_transactions.is_empty(),
        "agent shell command should settle before checking verbose presentation"
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("$ printf 'agent-visible-%s"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("MEZ_MARKER_TOKEN"), "{pane_text}");
    assert!(!pane_text.contains("MEZ_STATUS"), "{pane_text}");
    assert!(!pane_text.contains("MEZ_COMMAND_"), "{pane_text}");
    assert!(!pane_text.contains("unset MEZ_MARKER_TOKEN"), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that default agent command execution keeps one bounded command
/// preview while routing decoded command output into provider context. Raw
/// shell output may be base64-transported in the pane, but the model-facing
/// action result must still receive the decoded child-command output.
#[test]
fn runtime_agent_shell_command_output_keeps_decoded_context() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-hidden-output","input":"print a hidden marker"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap shell response".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "shell-1".to_string(),
                    rationale: "print a hidden marker".to_string(),
                    payload: crate::agent::AgentActionPayload::ShellCommand {
                        summary: "Print a hidden marker".to_string(),
                        command: "printf 'agent-hidden-%s\\n' output".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
        },
    };
    service.pending_agent_provider_tasks.remove("turn-1");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    for _ in 0..50 {
        let _ = service.poll_pane_outputs(4096).unwrap();
        if service.pending_agent_provider_tasks.contains("turn-1") {
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(service.pending_agent_provider_tasks.contains("turn-1"));
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        !pane_text.contains("agent> Print a hidden marker"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("$ printf 'agent-hidden-%s"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("agent-hidden-output"),
        "decoded command output should be visible as the transient tail line: {pane_text}"
    );
    assert!(
        !pane_text.contains("__MEZ_SHELL_OUTPUT_BASE64_BEGIN__"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("MEZ_MARKER_TOKEN"), "{pane_text}");
    assert!(!pane_text.contains("unset MEZ_MARKER_TOKEN"), "{pane_text}");
    let context_text = service
        .agent_turn_contexts
        .get("turn-1")
        .unwrap()
        .blocks
        .iter()
        .map(|block| block.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        context_text.contains("agent-hidden-output"),
        "{context_text}"
    );
    assert!(context_text.contains("output:\n"), "{context_text}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that a bash-backed pane shell survives the first agent shell
/// transaction after the command is displayed. The user-visible failure mode
/// was the primary pane exiting immediately after an agent command preview, so
/// this test waits through transaction settlement and repeated process polls.
#[test]
fn runtime_bash_agent_shell_transaction_keeps_parent_shell_alive() {
    let Some(bash_path) = bash_path_for_tests() else {
        eprintln!("skipping bash parent-shell regression because bash is unavailable");
        return;
    };
    let mut service = RuntimeSessionService::with_event_log(
        Session::new_default(
            ResolvedShell::new(bash_path, ShellSource::ShellEnv),
            Size::new(80, 24).unwrap(),
        ),
        PathBuf::from("/tmp/mez-1000/default.sock"),
        100,
        10,
        1024,
    )
    .unwrap();
    service.host_clipboard = HostClipboard::disabled();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-bash-survival","input":"run a bash command"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap shell response".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "shell-1".to_string(),
                    rationale: "exercise bash shell survival".to_string(),
                    payload: crate::agent::AgentActionPayload::ShellCommand {
                        summary: "Run a failing bash command and keep the parent shell available"
                            .to_string(),
                        command: "printf 'agent-bash-command-ran\\n'; false".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
        },
    };
    service.pending_agent_provider_tasks.remove("turn-1");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Running);

    for _ in 0..100 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        if service.running_shell_transactions.is_empty() {
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(
        service.running_shell_transactions.is_empty(),
        "agent transaction should have completed before checking parent shell liveness"
    );
    let pane_exits = service.poll_pane_processes().unwrap();
    assert!(pane_exits.is_empty(), "{pane_exits:?}");
    assert!(service.pane_processes().contains_pane("%1"));
    for _ in 0..10 {
        let exits = service.poll_pane_processes().unwrap();
        assert!(exits.is_empty(), "{exits:?}");
        assert!(service.pane_processes().contains_pane("%1"));
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!pane_text.contains("MEZ_MARKER_TOKEN"), "{pane_text}");
    assert!(!pane_text.contains("MEZ_HISTORY_"), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that the bash-backed pane shell also survives an agent shell
/// transaction when strict interactive options are already enabled. Some users
/// set `errexit` and `nounset` in shell startup files, so the transaction
/// prologue must temporarily disable and later restore both without letting a
/// failed agent command close the pane or the enclosing Mez session.
#[test]
fn runtime_bash_agent_shell_transaction_preserves_strict_parent_shell_options() {
    let Some(bash_path) = bash_path_for_tests() else {
        eprintln!("skipping bash strict-option regression because bash is unavailable");
        return;
    };
    let mut service = RuntimeSessionService::with_event_log(
        Session::new_default(
            ResolvedShell::new(bash_path, ShellSource::ShellEnv),
            Size::new(80, 24).unwrap(),
        ),
        PathBuf::from("/tmp/mez-1000/default.sock"),
        100,
        10,
        1024,
    )
    .unwrap();
    service.host_clipboard = HostClipboard::disabled();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .write_input_to_pane(&primary, Some("%1"), b"set -eu\n")
        .unwrap();
    for _ in 0..20 {
        let _ = service.poll_pane_outputs(4096).unwrap();
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-bash-strict-survival","input":"run a bash command"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap shell response".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "shell-1".to_string(),
                    rationale: "exercise bash strict shell survival".to_string(),
                    payload: crate::agent::AgentActionPayload::ShellCommand {
                        summary: "Run a failing bash command and keep strict shell options intact"
                            .to_string(),
                        command: "printf 'agent-bash-strict-command-ran\\n'; false".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
        },
    };
    service.pending_agent_provider_tasks.remove("turn-1");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Running);

    for _ in 0..100 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        if service.running_shell_transactions.is_empty() {
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(service.running_shell_transactions.is_empty());
    let pane_exits = service.poll_pane_processes().unwrap();
    assert!(pane_exits.is_empty(), "{pane_exits:?}");
    assert!(service.pane_processes().contains_pane("%1"));
    if !service.pending_agent_provider_tasks().is_empty() {
        let completion_provider = RuntimeBatchProvider {
            response: crate::agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "done".to_string(),
                usage: Default::default(),
                quota_usage: Default::default(),
                action_batch: Some(runtime_complete_batch("turn-1")),
            },
        };
        let completions = service
            .poll_agent_provider_tasks_with_provider(&completion_provider, 1)
            .unwrap();
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].terminal_state, AgentTurnState::Completed);
    }

    service
        .write_input_to_pane(&primary, Some("%1"), b"case $- in *e*u*|*u*e*) printf 'STRICT_OPTIONS_STILL_SET\\n';; *) printf 'STRICT_OPTIONS_LOST:%s\\n' \"$-\";; esac\n")
        .unwrap();
    let mut pane_text = String::new();
    for _ in 0..50 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        pane_text = service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        if pane_text.contains("STRICT_OPTIONS_STILL_SET") {
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(
        pane_text.contains("STRICT_OPTIONS_STILL_SET"),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that the normal command preview is bounded using the pane's
/// display width. Long generated commands should remain inspectable without
/// flooding the pane buffer or hiding the fact that more wrapped lines exist.
#[test]
fn runtime_agent_shell_command_preview_is_wrapped_and_capped() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(24, 8).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(24, 8).unwrap(), 20).unwrap(),
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-command-preview","input":"run a long command"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let command = "printf 'alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau upsilon phi chi psi omega alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu\\n'";
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap shell response".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "shell-1".to_string(),
                    rationale: "run a long command".to_string(),
                    payload: crate::agent::AgentActionPayload::ShellCommand {
                        summary: "Run a long command".to_string(),
                        command: command.to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
        },
    };
    service.pending_agent_provider_tasks.remove("turn-1");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("▐ $ printf 'alpha"), "{pane_text}");
    assert!(pane_text.contains("▐   ["), "{pane_text}");
    let command_preview_line_count = pane_text
        .lines()
        .skip_while(|line| !line.contains("▐ $ "))
        .take_while(|line| line.contains("▐ $ ") || line.starts_with("▐   "))
        .count();
    assert_eq!(command_preview_line_count, 10, "{pane_text}");
    assert!(
        !pane_text.contains("epsilon zeta eta theta iota kappa lambda mu"),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies command previews on wide panes cap their display width at 120 cells.
///
/// The command preview renderer should avoid pane-width lines that are too long
/// to scan while still preserving the existing `$ ` prompt and continuation
/// indentation.
#[test]
fn runtime_agent_shell_command_preview_caps_wide_panes_at_120_cells() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(200, 24).unwrap(), 120)
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(200, 24).unwrap(), 120).unwrap(),
    );
    service
        .append_agent_command_preview_to_terminal_buffer(
            "%1",
            &format!("printf '{}'", "abcdef ".repeat(40)),
        )
        .unwrap();

    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let command_lines = styled_lines
        .iter()
        .filter(|line| line.text.starts_with("▐ $ ") || line.text.starts_with("▐   "))
        .collect::<Vec<_>>();

    assert!(command_lines.len() > 1, "{styled_lines:?}");
    assert!(
        command_lines
            .iter()
            .all(|line| line.text.chars().count() <= 120),
        "{command_lines:?}"
    );
    assert!(
        command_lines[0].text.starts_with("▐ $ "),
        "{command_lines:?}"
    );
    assert!(
        command_lines
            .iter()
            .skip(1)
            .all(|line| line.text.starts_with("▐   ")),
        "{command_lines:?}"
    );
}

/// Verifies that bootstrap parsing uses the hidden transaction capture rather
/// than the visible pane screen. Bootstrap traffic is normally hidden from the
/// terminal buffer, so parsing only screen history leaves the pane marked as
/// bootstrap-pending and causes a tick-time bootstrap loop.
#[test]
fn runtime_bootstrap_completion_uses_hidden_transaction_output_and_clears_pending() {
    let mut service = test_runtime_service();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_pane_readiness("%1", PaneReadinessState::Busy);
    let marker = "bootstrap-marker";
    let turn_id = "bootstrap-%1-test";
    let output = "env\tos\tLinux\n\
env\tarch\tx86_64\n\
env\tkernel_version\t6.8.0-generic\n\
env\thost\tmyhost\n\
env\tuser\tme\n\
env\tshell_path\t/bin/sh\n\
env\tshell_class\tposix-sh\n\
env\tshell_version\t/bin/sh\n\
env\tpath\t/usr/local/bin:/usr/bin:/bin\n\
env\tcwd\t/home/me/project\n\
env\tproject_root\t/home/me/project\n\
env\tgit_repo\t1\n\
bootstrap\tcomplete\t1714500000\n\
tool\tsed\t1\t/usr/bin/sed\tGNU sed 4.9\tcommand -v sed\t0\t/usr/bin/sed --version\t0\t1714500000\n";
    service.running_shell_transactions.insert(
        marker.to_string(),
        RunningShellTransactionRef {
            turn_id: turn_id.to_string(),
            kind: RunningShellTransactionKind::Bootstrap,
            pane_id: "%1".to_string(),
            command: "bootstrap".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: output.len(),
            observed_output_preview: output.to_string(),
            observed_output_truncated: false,
        },
    );

    let observed = service
        .observe_agent_shell_transaction_end("%1", marker, turn_id, "agent-%1", "%1", 0)
        .unwrap();

    assert_eq!(observed, 1);
    assert!(
        !service.pane_bootstrap_pending.contains("%1"),
        "bootstrap pending should be cleared after one completed attempt"
    );
    let signature = service.pane_environment_signatures.get("%1").unwrap();
    assert_eq!(signature.working_directory, "/home/me/project");
    assert_eq!(signature.project_root.as_deref(), Some("/home/me/project"));
    assert!(
        service
            .tool_discovery_cache
            .get(signature)
            .is_some_and(|inventory| inventory.sed)
    );
    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::Ready
    );
    service.maybe_bootstrap_ready_panes().unwrap();
    assert!(
        service
            .running_shell_transactions
            .values()
            .all(|transaction| transaction.kind != RunningShellTransactionKind::Bootstrap)
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that a completed but unparseable bootstrap attempt is still
/// one-shot. Retrying the same hidden wrapper on every tick floods the pane
/// with Mezzanine-owned shell boilerplate without improving context.
#[test]
fn runtime_bootstrap_unparsed_output_does_not_retry_forever() {
    let mut service = test_runtime_service();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_pane_readiness("%1", PaneReadinessState::Busy);
    let marker = "bootstrap-unparsed-marker";
    let turn_id = "bootstrap-%1-unparsed";
    service.running_shell_transactions.insert(
        marker.to_string(),
        RunningShellTransactionRef {
            turn_id: turn_id.to_string(),
            kind: RunningShellTransactionKind::Bootstrap,
            pane_id: "%1".to_string(),
            command: "bootstrap".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let observed = service
        .observe_agent_shell_transaction_end("%1", marker, turn_id, "agent-%1", "%1", 0)
        .unwrap();

    assert_eq!(observed, 1);
    assert!(!service.pane_bootstrap_pending.contains("%1"));
    assert!(!service.pane_environment_signatures.contains_key("%1"));
    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::PromptCandidate
    );
    service.maybe_bootstrap_ready_panes().unwrap();
    assert!(
        service
            .running_shell_transactions
            .values()
            .all(|transaction| transaction.kind != RunningShellTransactionKind::Bootstrap)
    );
    let events = service
        .event_log
        .as_ref()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(
        events
            .iter()
            .any(|event| event.payload.contains(r#""bootstrap":"unparsed""#)),
        "{events:?}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that runtime model-profile overrides feed both provider execution
/// and the live `agent/list` state surface. The selected profile must remain
/// visible while the turn is running and after the turn completes so clients do
/// not see the generic offline `default` placeholder for a live agent.
#[test]
fn runtime_agent_shell_model_command_overrides_pane_model_profile() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\", \"gpt-5.4\"]\ndefault_model = \"gpt-5.5\"\n\n[providers.openai.options]\nreasoning_effort = \"medium\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let model = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"model","method":"agent/shell/command","params":{"idempotency_key":"model","input":"/model gpt-5.4"}}"#,
        &primary,
    );
    assert!(model.contains("scope=pane:%1"), "{model}");
    assert!(model.contains("profile=gpt-5.4"), "{model}");

    let prompt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"prompt","method":"agent/shell/command","params":{"idempotency_key":"prompt","input":"use the selected model"}}"#,
        &primary,
    );
    assert!(prompt.contains(r#""state":"running""#), "{prompt}");
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].model_profile.model, "gpt-5.4");
    let agents = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agents","method":"agent/list","params":{}}"#,
        &primary,
    );
    assert!(agents.contains(r#""model_profile":"gpt-5.4""#), "{agents}");
    assert!(agents.contains(r#""last_turn_id":"turn-1""#), "{agents}");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &RuntimeEchoProvider,
            ModelProfile {
                provider: "openai".to_string(),
                model: "gpt-5.4".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
        )
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let completed_agents = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agents-after","method":"agent/list","params":{}}"#,
        &primary,
    );
    assert!(
        completed_agents.contains(r#""model_profile":"gpt-5.4""#),
        "{completed_agents}"
    );
}

/// Verifies that clicking pane-frame model and reasoning status pills opens a
/// selector backed by the live provider catalog cache and applies the selected
/// value as a pane-scoped model override. This protects the mouse UI path from
/// drifting away from the `/model` command semantics that provider execution
/// already uses.
#[test]
fn runtime_pane_agent_status_selector_applies_model_and_reasoning() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\", \"gpt-5.4\"]\ndefault_model = \"gpt-5.5\"\n\n[model_profiles.default]\nprovider = \"openai\"\nmodel = \"gpt-5.5\"\nreasoning_profile = \"low\"\n\n[model_profiles.default.provider_options]\nreasoning_effort = \"low\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.cache_provider_model_catalog_for_tests(
        "openai",
        vec![crate::agent::ProviderModelInfo {
            id: "gpt-provider-only".to_string(),
            display_name: Some("Provider Only".to_string()),
            reasoning_levels: vec!["low".to_string(), "high".to_string()],
        }],
        vec!["low".to_string(), "high".to_string()],
    );

    let open_model = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::HandleMouse(
            MouseAction::OpenPaneAgentStatusSelector {
                pane_index: 0,
                field: PaneAgentStatusField::Model,
            },
        )],
        output_lines: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };
    service
        .apply_attached_terminal_step_plan(&primary, &open_model)
        .unwrap();
    let model_index = service
        .pane_agent_status_selector
        .as_ref()
        .and_then(|selector| {
            selector
                .items
                .iter()
                .position(|item| item == "gpt-provider-only")
        })
        .expect("model selector should include live provider catalog models");
    let select_model = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::HandleMouse(
            MouseAction::SelectPaneAgentStatusSelector {
                pane_index: 0,
                field: PaneAgentStatusField::Model,
                item_index: model_index,
            },
        )],
        output_lines: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };
    service
        .apply_attached_terminal_step_plan(&primary, &select_model)
        .unwrap();
    let (_name, model_profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(model_profile.model, "gpt-provider-only");
    assert_eq!(model_profile.reasoning_profile.as_deref(), Some("low"));

    let open_reasoning = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::HandleMouse(
            MouseAction::OpenPaneAgentStatusSelector {
                pane_index: 0,
                field: PaneAgentStatusField::Reasoning,
            },
        )],
        output_lines: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };
    service
        .apply_attached_terminal_step_plan(&primary, &open_reasoning)
        .unwrap();
    let reasoning_items = service
        .pane_agent_status_selector
        .as_ref()
        .map(|selector| selector.items.clone())
        .unwrap_or_default();
    let reasoning_index = reasoning_items
        .iter()
        .position(|item| item == "high")
        .unwrap_or_else(|| {
            panic!(
                "reasoning selector should include configured provider reasoning levels: {reasoning_items:?}"
            )
        });
    let select_reasoning = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::HandleMouse(
            MouseAction::SelectPaneAgentStatusSelector {
                pane_index: 0,
                field: PaneAgentStatusField::Reasoning,
                item_index: reasoning_index,
            },
        )],
        output_lines: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };
    service
        .apply_attached_terminal_step_plan(&primary, &select_reasoning)
        .unwrap();
    let (_name, reasoning_profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(reasoning_profile.model, "gpt-provider-only");
    assert_eq!(reasoning_profile.reasoning_profile.as_deref(), Some("high"));
    assert!(service.pane_agent_status_selector.is_none());

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::ApprovalPolicy,
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    let full_access_index = service
        .pane_agent_status_selector
        .as_ref()
        .and_then(|selector| selector.items.iter().position(|item| item == "full-access"))
        .expect("approval selector should include full-access");
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::SelectPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::ApprovalPolicy,
                        item_index: full_access_index,
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    let (_name, preserved_profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(preserved_profile.model, "gpt-provider-only");
    assert_eq!(preserved_profile.reasoning_profile.as_deref(), Some("high"));
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get("%1").unwrap();
    assert_eq!(
        pane_context.agent_model.as_deref(),
        Some("gpt-provider-only")
    );
    assert_eq!(pane_context.agent_reasoning.as_deref(), Some("high"));
}

/// Verifies that clickable pane-frame agent status pills cover live toggles
/// beyond model selection. Automatic reasoning should apply immediately like a
/// button, while approval policy should open the same selector flow used by
/// model and reasoning choices.
#[test]
fn runtime_pane_agent_status_selector_toggles_auto_and_selects_approval() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.agent_auto_reasoning = false;

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::AutoReasoning,
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert!(service.pane_agent_status_selector.is_none());
    assert_eq!(
        service.agent_auto_reasoning_overrides.get("%1").copied(),
        Some(true)
    );

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::ApprovalPolicy,
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    let full_access_index = service
        .pane_agent_status_selector
        .as_ref()
        .and_then(|selector| {
            assert_eq!(selector.field, PaneAgentStatusField::ApprovalPolicy);
            selector.items.iter().position(|item| item == "full-access")
        })
        .expect("approval selector should include full-access");
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::SelectPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::ApprovalPolicy,
                        item_index: full_access_index,
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert!(service.pane_agent_status_selector.is_none());
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::FullAccess
    );
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let view = service
        .render_client_view(ClientViewRole::Primary, Size::new(80, 24).unwrap(), &config)
        .unwrap()
        .unwrap();
    let rendered = view.lines.join("\n");
    assert!(rendered.contains("full-access"), "{rendered}");
    assert!(rendered.contains("auto:on"), "{rendered}");
    assert!(rendered.contains("gpt"), "{rendered}");
}

/// Verifies that pane-frame agent selectors remain modal until the user makes
/// an explicit selection or cancels them. Escape must close the selector
/// without leaking the escape byte into the active pane.
#[test]
fn runtime_pane_agent_status_selector_esc_closes_without_forwarding() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\", \"gpt-5.4\"]\ndefault_model = \"gpt-5.5\"\n\n[providers.openai.options]\nreasoning_effort = \"medium\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Model,
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert!(service.pane_agent_status_selector.is_some());

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b".to_vec())],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert!(report.view_refresh_required);
    assert!(report.full_redraw_required);
    assert!(service.pane_agent_status_selector.is_none());
}

/// Verifies pane-frame model and reasoning dropdowns support keyboard
/// navigation. The active row should move with arrow input and Enter should
/// apply the same pane-scoped `/model` mutation as mouse selection.
#[test]
fn runtime_pane_agent_status_selector_accepts_keyboard_navigation() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\", \"gpt-5.4\"]\ndefault_model = \"gpt-5.5\"\n\n[providers.openai.options]\nreasoning_effort = \"medium\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Model,
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    let (active_index, target_index) = service
        .pane_agent_status_selector
        .as_ref()
        .map(|selector| {
            (
                selector.active_index,
                selector
                    .items
                    .iter()
                    .position(|item| item == "gpt-5.4")
                    .expect("model selector should include gpt-5.4"),
            )
        })
        .expect("model selector should be open");
    let movement = if target_index < active_index {
        b"\x1b[A".to_vec()
    } else {
        b"\x1b[B".to_vec()
    };

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(movement),
                    TerminalClientLoopAction::ForwardToPane(b"\r".to_vec()),
                ],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert!(report.view_refresh_required);
    assert!(service.pane_agent_status_selector.is_none());
    let (_name, model_profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(model_profile.model, "gpt-5.4");
}

/// Verifies mouse-wheel input over an open pane agent selector scrolls the
/// selector itself rather than falling through to pane scrollback.
#[test]
fn runtime_pane_agent_status_selector_scrolls_only_dropdown_contents() {
    let mut service = test_runtime_service();
    let models = (0..40)
        .map(|index| format!("\"gpt-test-{index:02}\""))
        .collect::<Vec<_>>()
        .join(", ");
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [{models}]\ndefault_model = \"gpt-test-00\"\n\n[model_profiles.default]\nprovider = \"openai\"\nmodel = \"gpt-test-00\"\n"
            ),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 12).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Model,
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(
        service
            .pane_agent_status_selector
            .as_ref()
            .map(|selector| selector.scroll_offset),
        Some(0)
    );

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::ScrollPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Model,
                        lines: 3,
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(
        service
            .pane_agent_status_selector
            .as_ref()
            .map(|selector| selector.scroll_offset),
        Some(3)
    );

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::ScrollPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Model,
                        lines: -30,
                    },
                )],
                output_lines: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(
        service
            .pane_agent_status_selector
            .as_ref()
            .map(|selector| selector.scroll_offset),
        Some(0)
    );
}

/// Verifies that `/model list` uses the active provider catalog surface instead
/// of listing only manually named profiles. In this test there is no auth store
/// attached, so the runtime must fall back to the configured provider model set
/// and clearly label the catalog source while still exposing reasoning choices.
#[tokio::test]
async fn runtime_agent_shell_model_list_displays_provider_model_catalog() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\", \"gpt-5.4\"]\ndefault_model = \"gpt-5.5\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let model_list = service
        .execute_agent_shell_command_async(&primary, "/model list")
        .await
        .unwrap();

    assert!(model_list.contains(r#""kind":"display""#), "{model_list}");
    assert!(model_list.contains(r#""command":"model""#), "{model_list}");
    assert!(
        model_list.contains(r#""content_type":"text/markdown; charset=utf-8""#),
        "{model_list}"
    );
    assert!(model_list.contains("## Model Catalog"), "{model_list}");
    assert!(!model_list.contains("### Active Selection"), "{model_list}");
    assert!(!model_list.contains("### Available Models"), "{model_list}");
    assert!(
        model_list.contains("**Provider catalog unavailable:** `auth-unavailable`"),
        "{model_list}"
    );
    assert!(
        model_list.contains(
            "| Provider | Model | Reasoning levels | Context limit | Source | Active profile |"
        ),
        "{model_list}"
    );
    assert!(
        model_list.contains("| openai | ★ gpt-5.5 |"),
        "{model_list}"
    );
    assert!(model_list.contains("| openai | gpt-5.4 |"), "{model_list}");
    assert!(
        model_list.contains("★ default, low, medium, high, xhigh"),
        "{model_list}"
    );
    assert!(!model_list.contains("### Quota Usage"), "{model_list}");
    assert!(!model_list.contains("provider quota"), "{model_list}");
    assert!(!model_list.contains("**Usage:**"), "{model_list}");
}

/// Verifies that an explicitly empty provider model list still falls back to
/// the provider's built-in code-defined catalog. This protects minimal configs
/// that clear `providers.openai.models` from losing all local model selection
/// when live provider catalog access is unavailable.
#[tokio::test]
async fn runtime_agent_shell_model_list_uses_code_defaults_when_config_models_empty() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = []\ndefault_model = \"gpt-5.5\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let model_list = service
        .execute_agent_shell_command_async(&primary, "/model list")
        .await
        .unwrap();

    for model in [
        "★ gpt-5.5",
        "gpt-5.4",
        "gpt-5.4-mini",
        "gpt-5.3-codex",
        "gpt-5.3-codex-spark",
        "gpt-5.2",
    ] {
        assert!(model_list.contains(model), "{model_list}");
    }
    assert!(!model_list.contains("codex-mini-latest"), "{model_list}");
    assert!(model_list.contains("| config |"), "{model_list}");
}

/// Verifies that live provider model catalogs take precedence over configured
/// fallback models. The configured `providers.openai.models` list should keep
/// the command useful when the provider cannot be reached, but it must not
/// override a successfully populated provider catalog.
#[tokio::test]
async fn runtime_agent_shell_model_list_uses_provider_catalog_over_configured_models() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"configured-only\"]\ndefault_model = \"configured-only\"\n"
                .to_string(),
        }])
        .unwrap();
    service.cache_provider_model_catalog_for_tests(
        "openai",
        vec![crate::agent::ProviderModelInfo {
            id: "provider-only".to_string(),
            display_name: None,
            reasoning_levels: vec!["low".to_string(), "high".to_string()],
        }],
        vec!["low".to_string(), "high".to_string()],
    );
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let model_list = service
        .execute_agent_shell_command_async(&primary, "/model list")
        .await
        .unwrap();

    assert!(
        model_list.contains("| openai | provider-only |"),
        "{model_list}"
    );
    assert!(!model_list.contains("configured-only"), "{model_list}");
    assert!(model_list.contains("| provider |"), "{model_list}");
}

/// Verifies that ChatGPT browser/device credentials do not trigger a fabricated
/// Codex model-catalog HTTP request. The runtime should skip that unsupported
/// live catalog path and fall back to configured provider models without
/// surfacing an OpenAI 400-class provider error in the agent prompt.
#[tokio::test]
async fn runtime_agent_shell_model_list_skips_browser_auth_catalog_request() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\", \"gpt-5.4\"]\ndefault_model = \"gpt-5.5\"\n"
                .to_string(),
        }])
        .unwrap();
    let root = temp_root("runtime-model-list-chatgpt");
    let auth_store = AuthStore::new(crate::auth::AuthPaths::under_config_root(&root));
    let credential_store = auth_store.file_credential_store("openai").unwrap();
    auth_store
        .login_openai_provider_credential(
            "default",
            crate::auth::OpenAiProviderCredential {
                api_key: "chatgpt-access-token".to_string(),
                refresh_token: Some("refresh-token".to_string()),
                account_id: Some("acct_123".to_string()),
                organization_id: None,
                token_expires_at: Some("12345".to_string()),
            },
            &credential_store,
        )
        .unwrap();
    service.set_auth_store(auth_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let model_list = service
        .execute_agent_shell_command_async(&primary, "/model list")
        .await
        .unwrap();

    assert!(model_list.contains(r#""kind":"display""#), "{model_list}");
    assert!(
        model_list.contains("**Provider catalog unavailable:** `browser-auth-catalog-unsupported`"),
        "{model_list}"
    );
    assert!(!model_list.contains("status 400"), "{model_list}");
    assert!(!model_list.contains("Models API returned"), "{model_list}");
    assert!(
        model_list.contains("| openai | ★ gpt-5.5 |"),
        "{model_list}"
    );
    assert!(model_list.contains("| openai | gpt-5.4 |"), "{model_list}");
}

/// Verifies that `/model <model> <reasoning>` creates a pane-scoped runtime
/// model profile from the provider model catalog. This covers the direct model
/// selection UX without requiring users to predefine a named profile for every
/// model and reasoning combination they want to try.
#[test]
fn runtime_agent_shell_model_command_accepts_model_name_with_reasoning() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\", \"gpt-5.4\"]\ndefault_model = \"gpt-5.5\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let model = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"model-reasoning","method":"agent/shell/command","params":{"idempotency_key":"model-reasoning","input":"/model gpt-5.4 high"}}"#,
        &primary,
    );
    assert!(model.contains(r#""kind":"mutated""#), "{model}");
    assert!(model.contains("scope=pane:%1"), "{model}");
    assert!(model.contains("profile=gpt-5.4:high"), "{model}");
    assert!(model.contains("model=gpt-5.4"), "{model}");
    assert!(model.contains("reasoning_profile=high"), "{model}");

    let prompt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"prompt-reasoning","method":"agent/shell/command","params":{"idempotency_key":"prompt-reasoning","input":"use the selected model and reasoning"}}"#,
        &primary,
    );
    assert!(prompt.contains(r#""state":"running""#), "{prompt}");
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].model_profile.model, "gpt-5.4");
    assert_eq!(
        pending[0].model_profile.reasoning_profile.as_deref(),
        Some("high")
    );
    assert_eq!(
        pending[0]
            .model_profile
            .provider_options
            .get("reasoning_effort")
            .map(String::as_str),
        Some("high")
    );
}

/// Verifies that `/model --secondary` updates the auto-sizing router model.
///
/// The secondary model is used for the internal sizing decision before the
/// main provider request. It should be configurable from the same command
/// surface as the primary pane model without changing the active pane model.
#[test]
fn runtime_agent_shell_model_command_sets_secondary_router_profile() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n[agents.auto_sizing]\nrouter_model_profile = \"router\"\nsmall_model_profile = \"default\"\nmedium_model_profile = \"default\"\nlarge_model_profile = \"default\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\", \"gpt-5.4\"]\ndefault_model = \"gpt-5.5\"\n[model_profiles.default]\nprovider = \"openai\"\nmodel = \"gpt-5.5\"\nreasoning_profile = \"medium\"\n[model_profiles.router]\nprovider = \"openai\"\nmodel = \"gpt-5.4-mini\"\nreasoning_profile = \"medium\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let secondary = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"secondary-model","method":"agent/shell/command","params":{"idempotency_key":"secondary-model","input":"/model --secondary gpt-5.4 high"}}"#,
        &primary,
    );

    assert!(secondary.contains(r#""kind":"mutated""#), "{secondary}");
    assert!(secondary.contains("scope=secondary"), "{secondary}");
    assert!(secondary.contains("profile=gpt-5.4:high"), "{secondary}");
    assert!(secondary.contains("model=gpt-5.4"), "{secondary}");
    assert!(secondary.contains("reasoning_profile=high"), "{secondary}");
    assert_eq!(
        service.agent_auto_sizing.router_model_profile,
        "gpt-5.4:high"
    );

    let primary_status = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"primary-model-status","method":"agent/shell/command","params":{"idempotency_key":"primary-model-status","input":"/model show"}}"#,
        &primary,
    );
    assert!(
        primary_status.contains("active_profile=default"),
        "{primary_status}"
    );
    let secondary_status = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"secondary-model-status","method":"agent/shell/command","params":{"idempotency_key":"secondary-model-status","input":"/model --secondary show"}}"#,
        &primary,
    );
    assert!(
        secondary_status.contains("profile=gpt-5.4:high"),
        "{secondary_status}"
    );
    assert!(
        secondary_status.contains("active_model=gpt-5.5"),
        "{secondary_status}"
    );
}

/// Verifies that `/auto-reasoning` stores a pane-local override used by
/// subsequent turns without mutating the global configured default. This covers
/// the command surface for enabling, toggling, and inspecting automatic model
/// sizing.
#[test]
fn runtime_agent_shell_auto_reasoning_command_sets_pane_override() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let enabled = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"auto-reasoning-on","method":"agent/shell/command","params":{"idempotency_key":"auto-reasoning-on","input":"/auto-reasoning on"}}"#,
        &primary,
    );

    assert!(enabled.contains(r#""kind":"mutated""#), "{enabled}");
    assert!(
        enabled.contains(r#""command":"auto-reasoning""#),
        "{enabled}"
    );
    assert!(enabled.contains("enabled=true"), "{enabled}");
    assert!(enabled.contains("default=false"), "{enabled}");
    assert!(enabled.contains("changed=true"), "{enabled}");
    assert_eq!(
        service.agent_auto_reasoning_overrides.get("%1").copied(),
        Some(true)
    );

    let status = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"auto-reasoning-status","method":"agent/shell/command","params":{"idempotency_key":"auto-reasoning-status","input":"/auto-reasoning status"}}"#,
        &primary,
    );
    assert!(status.contains(r#""kind":"display""#), "{status}");
    assert!(status.contains("enabled=true"), "{status}");
    assert!(status.contains("override_present=true"), "{status}");

    let toggled = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"auto-reasoning-toggle","method":"agent/shell/command","params":{"idempotency_key":"auto-reasoning-toggle","input":"/auto-reasoning toggle"}}"#,
        &primary,
    );
    assert!(toggled.contains(r#""kind":"mutated""#), "{toggled}");
    assert!(toggled.contains("enabled=false"), "{toggled}");
    assert!(toggled.contains("changed=true"), "{toggled}");
    assert_eq!(
        service.agent_auto_reasoning_overrides.get("%1").copied(),
        Some(false)
    );
}

/// Verifies that automatic reasoning runs an internal router request before
/// the turn provider request, applies the selected model and reasoning effort,
/// and keeps router prompt/response correspondence out of persisted model
/// context. Only the effective profile and bounded logs survive.
#[test]
fn runtime_agent_turn_auto_reasoning_selects_profile_without_context_leak() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"
[agents]
default_provider = "runtime-batch"
default_model_profile = "default"
auto_reasoning = true

[agents.auto_sizing]
router_model_profile = "router"
small_model_profile = "small"
medium_model_profile = "medium"
large_model_profile = "large"
allowed_reasoning_efforts = ["low", "medium", "high", "xhigh"]
fallback_policy = "use-default-profile"

[providers.runtime-batch]
kind = "openai"
models = ["gpt-router", "gpt-default", "gpt-5.3-codex", "gpt-5.4", "gpt-5.5"]
default_model = "gpt-default"

[model_profiles.default]
provider = "runtime-batch"
model = "gpt-default"
reasoning_profile = "medium"

[model_profiles.router]
provider = "runtime-batch"
model = "gpt-router"
reasoning_profile = "low"

[model_profiles.small]
provider = "runtime-batch"
model = "gpt-5.3-codex"
reasoning_profile = "medium"

[model_profiles.medium]
provider = "runtime-batch"
model = "gpt-5.4"
reasoning_profile = "medium"

[model_profiles.large]
provider = "runtime-batch"
model = "gpt-5.5"
reasoning_profile = "high"
"#
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let prompt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"auto-sized-prompt","method":"agent/shell/command","params":{"idempotency_key":"auto-sized-prompt","input":"implement this"}}"#,
        &primary,
    );
    assert!(prompt.contains(r#""state":"running""#), "{prompt}");
    assert_eq!(service.pending_agent_provider_tasks().len(), 1);
    service
        .agent_turn_contexts
        .get_mut("turn-1")
        .unwrap()
        .blocks
        .extend([
            crate::agent::ContextBlock {
                source: ContextSourceKind::TranscriptAssistant,
                label: "old minified assistant context for pane %1".to_string(),
                content: format!("minified-context:{}", "x".repeat(200 * 1024)),
            },
            crate::agent::ContextBlock {
                source: ContextSourceKind::TranscriptAssistant,
                label: "transcript assistant entry 2 for pane %1".to_string(),
                content: "Recommended next tasks:\n1. Document the model picker.\n2. Clean up stale quota UI.\n3. Implement multi-file runtime auto-sizing.".to_string(),
            },
            crate::agent::ContextBlock {
                source: ContextSourceKind::TranscriptTool,
                label: "previous tool output for pane %1".to_string(),
                content: "tool-only output should not reach the router".to_string(),
            },
            crate::agent::ContextBlock {
                source: ContextSourceKind::Policy,
                label: "policy context".to_string(),
                content: "policy-only context should not reach the router".to_string(),
            },
        ]);

    let provider = RuntimeAutoSizingProvider {
        requests: RefCell::new(Vec::new()),
    };
    let executions = service
        .poll_agent_provider_tasks_with_provider(&provider, 1)
        .unwrap();
    assert_eq!(executions.len(), 1);
    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].interaction_kind,
        crate::agent::ModelInteractionKind::AutoSizing
    );
    assert_eq!(requests[0].model, "gpt-router");
    assert!(requests[0].turn_id.ends_with(":auto-sizing"));
    let router_context = requests[0]
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(router_context.contains("implement this"));
    assert!(router_context.contains("Implement multi-file runtime auto-sizing"));
    assert!(router_context.contains("Latest submitted task"));
    assert!(router_context.contains("Referential prompt detected"));
    assert!(router_context.contains("Do not choose small/low merely because"));
    assert!(router_context.contains("Model size reflects task scope"));
    assert!(router_context.contains("reasoning effort reflects the depth and complexity"));
    assert!(router_context.contains("Small models are only for chat"));
    assert!(router_context.contains("Planning, investigation, complex implementation"));
    assert!(router_context.contains("Never choose low reasoning for coding"));
    assert!(router_context.contains("do not return only a discovery plan"));
    assert!(router_context.contains("[truncated for auto-sizing router]"));
    assert!(
        router_context.len() < 180 * 1024,
        "router context should stay bounded independently of model-window fallback estimates"
    );
    assert!(requests[0].messages.iter().any(|message| {
        message.role == crate::agent::ModelMessageRole::User
            && message.source == ContextSourceKind::UserInstruction
            && message.content.contains("implement this")
    }));
    assert!(requests[0].messages.iter().any(|message| {
        message.role == crate::agent::ModelMessageRole::Assistant
            && message.source == ContextSourceKind::TranscriptAssistant
            && message
                .content
                .contains("Implement multi-file runtime auto-sizing")
    }));
    assert!(
        !router_context.contains("tool-only output should not reach the router"),
        "{router_context}"
    );
    assert!(
        !router_context.contains("policy-only context should not reach the router"),
        "{router_context}"
    );
    assert_eq!(
        requests[1].interaction_kind,
        crate::agent::ModelInteractionKind::CapabilityDecision
    );
    assert_eq!(requests[1].model, "gpt-5.5");
    assert_eq!(requests[1].reasoning_effort.as_deref(), Some("high"));
    assert_eq!(executions[0].request.model, "gpt-5.5");
    assert_eq!(
        executions[0].request.reasoning_effort.as_deref(),
        Some("high")
    );
    let normal_request_context = requests[1]
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!normal_request_context.contains(":auto-sizing"));
    assert!(!normal_request_context.contains("multi-file feature work"));
}

/// Builds the synthetic model response used by compaction completion tests.
fn runtime_test_compaction_response(summary: &str) -> crate::agent::ModelResponse {
    crate::agent::ModelResponse {
        provider: "test".to_string(),
        model: "gpt-compact-test".to_string(),
        raw_text: summary.to_string(),
        usage: Default::default(),
        quota_usage: Vec::new(),
        action_batch: None,
    }
}

/// Applies a queued `/compact` provider result without leaving the actor path.
fn complete_runtime_test_compaction(
    service: &mut RuntimeSessionService,
    pane_id: &str,
    summary: &str,
) {
    let task = service
        .pending_agent_compaction_tasks
        .remove(pane_id)
        .expect("queued compaction task");
    service
        .claimed_agent_compaction_tasks
        .insert(pane_id.to_string(), task);
    assert!(
        service
            .apply_agent_compaction_completed_event(
                pane_id,
                runtime_test_compaction_response(summary)
            )
            .unwrap()
    );
}

/// Verifies that `/compact` converts the active conversation transcript into a
/// bounded pane-scoped memory record, retains a raw recent transcript tail, and
/// feeds both into the next prompt context. This keeps context pressure
/// handling from silently dropping recent exact referents.
#[test]
fn runtime_agent_shell_compact_summarizes_transcript_into_memory_context() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "compact-context-window".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "openai"
default_model_profile = "compact-test"
auto_compact = false
[providers.openai]
kind = "openai"
models = ["gpt-compact-test"]
default_model = "gpt-compact-test"
[model_profiles.compact-test]
provider = "openai"
model = "gpt-compact-test"
context_window_tokens = 4500
"#
            .to_string(),
        }])
        .unwrap();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-compact"));
    for sequence in 1..=12 {
        let (role, content) = match sequence {
            1 => (
                crate::transcript::TranscriptRole::User,
                format!("summarize release plan {}", "summary-word ".repeat(28)),
            ),
            2 => (
                crate::transcript::TranscriptRole::Tool,
                format!(
                    "api_key sk-secret should be hidden {}",
                    "secret-word ".repeat(28)
                ),
            ),
            3 => (
                crate::transcript::TranscriptRole::Assistant,
                format!(
                    "release plan summary is ready {}",
                    "release-word ".repeat(28)
                ),
            ),
            _ if sequence % 2 == 0 => (
                crate::transcript::TranscriptRole::User,
                format!("filler user turn {sequence} {}", "user-word ".repeat(28)),
            ),
            _ => (
                crate::transcript::TranscriptRole::Assistant,
                format!(
                    "filler assistant turn {sequence} {}",
                    "assistant-word ".repeat(28)
                ),
            ),
        };
        transcript_store
            .append(&crate::transcript::TranscriptEntry {
                conversation_id: "as1".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content,
            })
            .unwrap();
    }
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 8).unwrap(), 80).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "as1", 12)
        .unwrap();

    let compact = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compact","method":"agent/shell/command","params":{"idempotency_key":"compact","input":"/compact"}}"#,
        &primary,
    );

    assert!(compact.contains(r#""kind":"mutated""#), "{compact}");
    assert!(compact.contains(r#""command":"compact""#), "{compact}");
    assert!(compact.contains("state=queued"), "{compact}");
    assert!(
        compact.contains("previous_transcript_entries=12"),
        "{compact}"
    );
    assert!(compact.contains("summarized_entries=6"), "{compact}");
    assert!(compact.contains("source=model-compact"), "{compact}");
    assert!(!compact.contains("requires_runtime"), "{compact}");
    assert!(service.agent_compacting_panes.contains_key("%1"));
    assert!(service.pending_agent_compaction_tasks.contains_key("%1"));

    complete_runtime_test_compaction(&mut service, "%1", "summarize release plan\n[redacted]");
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: compacting conversation summary"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("agent: compacted conversation summary"),
        "{pane_text}"
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .transcript_entries,
        6
    );
    let compacted = service
        .memory_records()
        .into_iter()
        .find(|record| record.id == "compact-as1")
        .expect("compacted memory record");
    assert!(
        compacted.content.contains("summarize release plan"),
        "{}",
        compacted.content
    );
    assert!(
        compacted.content.contains("[redacted]"),
        "{}",
        compacted.content
    );
    assert!(
        !compacted.content.contains("sk-secret"),
        "{}",
        compacted.content
    );

    let prompt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compact-prompt","method":"agent/shell/command","params":{"idempotency_key":"compact-prompt","input":"continue after compaction"}}"#,
        &primary,
    );
    assert!(prompt.contains(r#""state":"running""#), "{prompt}");
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::Memory
            && block.label.contains("compact-as1")
            && block.content.contains("summarize release plan")
    }));
    assert!(context.blocks.iter().all(|block| {
        !matches!(
            block.source,
            ContextSourceKind::Transcript
                | ContextSourceKind::TranscriptUser
                | ContextSourceKind::TranscriptAssistant
                | ContextSourceKind::TranscriptTool
        ) || !block.content.contains("release plan summary is ready")
    }));
    assert!(context.blocks.iter().all(|block| {
        !matches!(
            block.source,
            ContextSourceKind::Transcript
                | ContextSourceKind::TranscriptUser
                | ContextSourceKind::TranscriptAssistant
                | ContextSourceKind::TranscriptTool
        ) || !block.content.contains("sk-secret")
    }));
    assert!(context.blocks.iter().all(|block| {
        !matches!(
            block.source,
            ContextSourceKind::Transcript
                | ContextSourceKind::TranscriptUser
                | ContextSourceKind::TranscriptAssistant
                | ContextSourceKind::TranscriptTool
        ) || !block.content.contains("summarize release plan")
    }));
}

/// Verifies prompt submission does not run fallback context accounting before
/// appending prompt-derived state.
///
/// Provider responses and provider context-limit errors are the source of truth
/// for context-size handling, so prompt submission must start the turn even when
/// a local estimate would have crossed the configured threshold.
#[test]
fn runtime_agent_prompt_does_not_preflight_compact_before_context_append() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "compact-preflight-context-window".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "openai"
default_model_profile = "compact-preflight-test"
auto_compact = true
auto_compact_threshold = 0.50
[providers.openai]
kind = "openai"
models = ["gpt-compact-preflight-test"]
default_model = "gpt-compact-preflight-test"
[model_profiles.compact-preflight-test]
provider = "openai"
model = "gpt-compact-preflight-test"
context_window_tokens = 1024
"#
            .to_string(),
        }])
        .unwrap();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-compact-preflight"));
    transcript_store
        .append(&crate::transcript::TranscriptEntry {
            conversation_id: "as-preflight".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: crate::transcript::TranscriptRole::Assistant,
            turn_id: "turn-previous".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: format!("large prior context {}", "context-pressure ".repeat(900)),
        })
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 8).unwrap(), 80).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "as-preflight", 1)
        .unwrap();

    let response = service
        .execute_agent_shell_command(&primary, "continue with the next item")
        .unwrap();

    assert!(response.contains(r#""state":"running""#), "{response}");
    assert!(
        !response.contains(r#""kind":"requires_runtime""#),
        "{response}"
    );
    assert_eq!(service.agent_turn_ledger.turns().len(), 1);
    assert_eq!(
        transcript_store.prompt_history("as-preflight").unwrap(),
        vec!["continue with the next item".to_string()]
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("continue with the next item"),
        "{pane_text}"
    );
}

/// Verifies active-turn provider continuations do not run fallback context
/// accounting before request assembly.
///
/// Runtime-owned action results and steering can append context after the turn
/// has started. The continuation path should still send the exact assembled
/// request first and rely on provider context-limit recovery if the provider
/// rejects it.
#[test]
fn runtime_agent_turn_sends_active_context_before_provider_limit_feedback() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "compact-active-turn-context-window".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "compact-active-turn-test"
auto_compact = true
auto_compact_threshold = 0.50
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
[model_profiles.compact-active-turn-test]
provider = "runtime-batch"
model = "test"
context_window_tokens = 64000
"#
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-active-turn-compact","input":"continue with gathered evidence"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service
        .agent_turn_contexts
        .get_mut("turn-1")
        .unwrap()
        .blocks
        .push(ContextBlock {
            source: ContextSourceKind::ActionResult,
            label: "synthetic in-turn action result".to_string(),
            content: format!(
                "turn-context-pressure- {}",
                "context-pressure ".repeat(10_000)
            ),
        });
    service.pending_agent_provider_tasks.remove("turn-1");
    let provider = RuntimeRecordingProvider {
        provider: "runtime-batch",
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "done".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(runtime_complete_batch("turn-1")),
        },
        last_request: RefCell::new(None),
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            service
                .provider_registry()
                .resolve_profile("compact-active-turn-test")
                .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let request = provider.last_request.borrow().clone().unwrap();
    let request_text = request
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        request_text.contains("[synthetic in-turn action result]"),
        "{request_text}"
    );
    assert!(
        request_text.contains("turn-context-pressure-"),
        "{request_text}"
    );
    assert!(
        !request_text.contains("[context compacted]"),
        "{request_text}"
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        !pane_text.contains("agent: compacted active turn context"),
        "{pane_text}"
    );
}

/// Verifies provider context-limit API errors trigger active-turn compaction
/// and retry before the turn is failed.
///
/// The proactive threshold path can miss provider-specific tokenization or
/// hidden request overhead. When the provider rejects the request anyway, the
/// runtime must compact the stored active-turn context before retrying so the
/// same oversized payload is not sent again.
#[test]
fn runtime_provider_context_limit_error_compacts_context_and_retries() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "provider-context-limit-recovery".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "provider-context-limit-test"
auto_compact = false
auto_compact_threshold = 0.50
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
[model_profiles.provider-context-limit-test]
provider = "runtime-batch"
model = "test"
context_window_tokens = 40000
"#
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-context-limit-recovery","input":"continue with the large observation"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service
        .agent_turn_contexts
        .get_mut("turn-1")
        .unwrap()
        .blocks
        .push(ContextBlock {
            source: ContextSourceKind::ActionResult,
            label: "synthetic provider-rejected action result".to_string(),
            content: format!("provider-context-limit- {}", "cp ".repeat(10_000)),
        });
    service.pending_agent_provider_tasks.remove("turn-1");
    let provider = RuntimeContextLimitThenSuccessProvider {
        requests: RefCell::new(Vec::new()),
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            service
                .provider_registry()
                .resolve_profile("provider-context-limit-test")
                .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 2);
    let first_request_text = requests[0]
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        first_request_text.contains("provider-context-limit-"),
        "{first_request_text}"
    );
    let second_request_text = requests[1]
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        second_request_text.contains("[context compacted]"),
        "{second_request_text}"
    );
    assert!(
        !second_request_text.contains("provider-context-limit-"),
        "{second_request_text}"
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("provider rejected context as too large; compacted active turn context"),
        "{pane_text}"
    );
}

/// Verifies provider context-window wording also triggers active-turn
/// compaction and retry before the turn is failed.
///
/// Some providers report the same rejection without the OpenAI-specific
/// `context_length_exceeded` code. Runtime recovery should still classify the
/// error as a context-limit failure when the diagnostic says the input exceeds
/// the model context window.
#[test]
fn runtime_provider_context_window_error_compacts_context_and_retries() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "provider-context-window-recovery".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "provider-context-window-test"
auto_compact = false
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
[model_profiles.provider-context-window-test]
provider = "runtime-batch"
model = "test"
context_window_tokens = 40000
"#
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-context-window-recovery","input":"continue with the large observation"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service
        .agent_turn_contexts
        .get_mut("turn-1")
        .unwrap()
        .blocks
        .push(ContextBlock {
            source: ContextSourceKind::ActionResult,
            label: "synthetic provider-context-window action result".to_string(),
            content: format!("provider-context-window- {}", "cw ".repeat(10_000)),
        });
    service.pending_agent_provider_tasks.remove("turn-1");
    let provider = RuntimeContextWindowErrorProvider {
        requests: RefCell::new(Vec::new()),
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            service
                .provider_registry()
                .resolve_profile("provider-context-window-test")
                .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 2);
    let first_request_text = requests[0]
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        first_request_text.contains("provider-context-window-"),
        "{first_request_text}"
    );
    let second_request_text = requests[1]
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        second_request_text.contains("[context compacted]"),
        "{second_request_text}"
    );
    assert!(
        !second_request_text.contains("provider-context-window-"),
        "{second_request_text}"
    );
    let retry_notice = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        retry_notice
            .contains("provider rejected context as too large; compacted active turn context"),
        "{retry_notice}"
    );
}

/// Verifies provider output-limit incomplete responses trigger compact retry
/// guidance and max-output escalation without compacting active-turn context.
///
/// Output exhaustion means the provider accepted the input but cut generation
/// off, so the recovery path should ask for a smaller complete response rather
/// than discarding context.
#[test]
fn runtime_provider_output_limit_error_guides_compact_retry_without_compaction() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "provider-output-limit-recovery".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "provider-output-limit-test"
auto_compact = false
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
[model_profiles.provider-output-limit-test]
provider = "runtime-batch"
model = "test"
max_output_tokens = 4096
"#
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-output-limit-recovery","method":"agent/shell/command","params":{"idempotency_key":"agent-output-limit-recovery","input":"continue with the current implementation"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service
        .agent_turn_contexts
        .get_mut("turn-1")
        .unwrap()
        .blocks
        .push(ContextBlock {
            source: ContextSourceKind::ActionResult,
            label: "synthetic retained action result".to_string(),
            content: "output-limit-retained-context".to_string(),
        });
    service.pending_agent_provider_tasks.remove("turn-1");
    let provider = RuntimeOutputLimitThenSuccessProvider {
        requests: RefCell::new(Vec::new()),
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            service
                .provider_registry()
                .resolve_profile("provider-output-limit-test")
                .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].max_output_tokens, Some(4096));
    assert_eq!(requests[1].max_output_tokens, Some(16_384));
    let second_request_text = requests[1]
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        second_request_text.contains("output-limit-retained-context"),
        "{second_request_text}"
    );
    assert!(
        second_request_text.contains("[ephemeral provider output-limit retry]"),
        "{second_request_text}"
    );
    assert!(
        second_request_text.contains("one complete compact MAAP batch"),
        "{second_request_text}"
    );
    assert!(
        !second_request_text.contains("[context compacted]"),
        "{second_request_text}"
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("provider response hit output limit; retrying compactly"),
        "{pane_text}"
    );
}

/// Verifies auto-reasoning context-limit recovery budgets against the smallest
/// possible main-provider target before a router decision has been stored.
///
/// A turn may start with a large default profile while the router is still able
/// to choose a smaller target profile for the first normal request. Provider
/// context-limit recovery must therefore compact against the minimum target
/// window until the synthesized per-turn profile exists.
#[test]
fn runtime_auto_reasoning_context_limit_recovery_uses_minimum_target_window() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "auto-reasoning-context-limit-recovery".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"
[agents]
default_provider = "runtime-batch"
default_model_profile = "default"
auto_reasoning = true
auto_compact = false
auto_compact_threshold = 0.50

[agents.auto_sizing]
router_model_profile = "router"
small_model_profile = "small"
medium_model_profile = "medium"
large_model_profile = "large"
allowed_reasoning_efforts = ["low", "medium", "high", "xhigh"]
fallback_policy = "use-default-profile"

[providers.runtime-batch]
kind = "openai"
models = ["gpt-router", "gpt-default", "gpt-small", "gpt-medium", "gpt-large"]
default_model = "gpt-default"

[model_profiles.default]
provider = "runtime-batch"
model = "gpt-default"
reasoning_profile = "medium"
context_window_tokens = 100000

[model_profiles.router]
provider = "runtime-batch"
model = "gpt-router"
reasoning_profile = "low"
context_window_tokens = 2000

[model_profiles.small]
provider = "runtime-batch"
model = "gpt-small"
reasoning_profile = "medium"
context_window_tokens = 40000

[model_profiles.medium]
provider = "runtime-batch"
model = "gpt-medium"
reasoning_profile = "medium"
context_window_tokens = 100000

[model_profiles.large]
provider = "runtime-batch"
model = "gpt-large"
reasoning_profile = "high"
context_window_tokens = 100000
"#
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-auto-context-limit","method":"agent/shell/command","params":{"idempotency_key":"agent-auto-context-limit","input":"continue with the current findings"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let default_profile = service
        .provider_registry()
        .resolve_profile("default")
        .unwrap();
    service
        .agent_turn_model_profiles
        .insert("turn-1".to_string(), default_profile);
    service
        .agent_turn_contexts
        .get_mut("turn-1")
        .unwrap()
        .blocks
        .push(ContextBlock {
            source: ContextSourceKind::ActionResult,
            label: "synthetic auto-reasoning action result".to_string(),
            content: format!(
                "auto-reasoning-context-pressure- {}",
                "context-pressure ".repeat(50_000)
            ),
        });
    let error = MezError::invalid_state(
        "OpenAI Responses API returned status 400: context length exceeded",
    )
    .with_provider_failure_json(
        r#"{"status_code":400,"error":{"message":"maximum context length exceeded","type":"invalid_request_error","code":"context_length_exceeded"}}"#,
    );

    let recovered = service
        .recover_agent_provider_context_limit_failure(
            &AgentId::opaque("agent-%1").unwrap(),
            "turn-1",
            &error,
            1,
        )
        .unwrap();

    assert!(recovered);
    let stored_context = service
        .agent_turn_contexts
        .get("turn-1")
        .unwrap()
        .blocks
        .iter()
        .map(|block| block.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(stored_context.contains("[context compacted]"));
    assert!(
        stored_context.contains("label=synthetic auto-reasoning action result"),
        "{stored_context}"
    );
    assert!(
        !stored_context.contains("auto-reasoning-context-pressure-"),
        "{stored_context}"
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("provider rejected context as too large; compacted active turn context"),
        "{pane_text}"
    );
}

/// Verifies provider context receives only the active conversation compaction
/// memory automatically.
///
/// Generic session memory should not be injected into every provider request
/// once transcript replay and compaction summaries already represent the active
/// conversation.
#[test]
fn runtime_agent_context_injects_only_active_compact_memory() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "as1", 0)
        .unwrap();
    service
        .upsert_session_memory(MemoryRecord {
            id: "runtime-note".to_string(),
            scope: crate::memory::MemoryScope::Session {
                session_id: service.session().id.to_string(),
            },
            created_at_unix_seconds: 1,
            updated_at_unix_seconds: 1,
            source: crate::memory::MemorySource::User,
            priority: 255,
            content: "generic memory should not be automatic context".to_string(),
            explicit_sensitive_consent: false,
        })
        .unwrap();
    service
        .upsert_session_memory(MemoryRecord {
            id: "compact-other".to_string(),
            scope: crate::memory::MemoryScope::Pane {
                session_id: service.session().id.to_string(),
                pane_id: "%1".to_string(),
            },
            created_at_unix_seconds: 2,
            updated_at_unix_seconds: 2,
            source: crate::memory::MemorySource::Agent,
            priority: 255,
            content: "other compaction should not leak".to_string(),
            explicit_sensitive_consent: false,
        })
        .unwrap();
    service
        .upsert_session_memory(MemoryRecord {
            id: "compact-as1".to_string(),
            scope: crate::memory::MemoryScope::Pane {
                session_id: service.session().id.to_string(),
                pane_id: "%1".to_string(),
            },
            created_at_unix_seconds: 3,
            updated_at_unix_seconds: 3,
            source: crate::memory::MemorySource::Agent,
            priority: 128,
            content: "active compact summary".to_string(),
            explicit_sensitive_consent: false,
        })
        .unwrap();

    let context = service
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    let memory_blocks = context
        .blocks
        .iter()
        .filter(|block| block.source == ContextSourceKind::Memory)
        .collect::<Vec<_>>();

    assert_eq!(memory_blocks.len(), 2, "{memory_blocks:?}");
    assert!(
        memory_blocks
            .iter()
            .any(|block| block.label == "conversation compaction notice"
                && block.content.contains("Conversation compaction occurred")),
        "{memory_blocks:?}"
    );
    assert!(
        memory_blocks
            .iter()
            .any(|block| block.label.contains("compact-as1")
                && block.content.contains("active compact summary")),
        "{memory_blocks:?}"
    );
    assert!(
        context
            .blocks
            .iter()
            .all(|block| !block.content.contains("generic memory"))
    );
    assert!(
        context
            .blocks
            .iter()
            .all(|block| !block.content.contains("other compaction"))
    );
}

/// Verifies explicit `$skill` prompt syntax loads the selected skill into the
/// next turn context and appends trailing prompt text as skill-specific
/// semantic context. The raw prompt remains present so the user's latest input
/// is still the visible turn instruction.
#[test]
fn runtime_agent_context_explicit_skill_prompt_loads_skill_context() {
    let config_root = temp_root("runtime-skill-context");
    let skill_dir = config_root.join("skills/review");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: review\ndescription: Review workflow\n---\n\nCheck tests and risks.\n",
    )
    .unwrap();
    let mut service = test_runtime_service();
    service.set_config_root(config_root);

    let context = service
        .agent_context_for_pane_prompt("%1", "$review focus src/lib.rs", 0)
        .unwrap();
    let skill_block = context
        .blocks
        .iter()
        .find(|block| block.label == "explicit skill review")
        .expect("missing explicit skill context block");
    let prompt_block = context
        .blocks
        .iter()
        .find(|block| block.label == "user prompt")
        .expect("missing raw user prompt block");

    assert_eq!(skill_block.source, ContextSourceKind::UserInstruction);
    assert!(skill_block.content.contains("name: review"));
    assert!(skill_block.content.contains("Check tests and risks."));
    assert!(
        skill_block
            .content
            .contains("## Additional context\n\nfocus src/lib.rs")
    );
    assert_eq!(prompt_block.content, "$review focus src/lib.rs");
}

/// Verifies explicit `$create-skill` prompt syntax loads the built-in skill
/// authoring workflow even when no user or project skills have been installed.
/// This keeps the built-in workflow available as normal skill context instead
/// of requiring a separate command or bootstrap file.
#[test]
fn runtime_agent_context_builtin_create_skill_prompt_loads_builtin_context() {
    let mut service = test_runtime_service();

    let context = service
        .agent_context_for_pane_prompt(
            "%1",
            "$create-skill create a project skill for release notes",
            0,
        )
        .unwrap();
    let skill_block = context
        .blocks
        .iter()
        .find(|block| block.label == "explicit skill create-skill")
        .expect("missing explicit built-in skill context block");

    assert_eq!(skill_block.source, ContextSourceKind::UserInstruction);
    assert!(skill_block.content.contains("Source: builtin"));
    assert!(skill_block.content.contains("name: create-skill"));
    assert!(skill_block.content.contains("Project scope:"));
    assert!(
        skill_block
            .content
            .contains("Invocation state: this skill is already loaded"),
        "{}",
        skill_block.content
    );
    assert!(
        skill_block
            .content
            .contains("## Additional context\n\ncreate a project skill for release notes")
    );
    let invocation_block = context
        .blocks
        .iter()
        .find(|block| block.label == "explicit skill invocation create-skill")
        .expect("missing explicit skill invocation block");
    assert_eq!(invocation_block.source, ContextSourceKind::LocalMessage);
    assert!(
        invocation_block
            .content
            .contains("The selected skill context has already been loaded above"),
        "{}",
        invocation_block.content
    );
}

/// Verifies `$mez-config` includes live schema guidance and current config.
///
/// The config skill should not force the model to rediscover basic setting
/// names before making a config mutation. Its invocation context therefore
/// includes the annotated schema, concrete theme color slots, reset operation,
/// and the pane's current effective config snapshot.
#[test]
fn runtime_agent_context_builtin_mez_config_prompt_includes_current_config() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "default-user".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: crate::config::DEFAULT_CONFIG_TOML.to_string(),
        }])
        .unwrap();

    let context = service
        .agent_context_for_pane_prompt("%1", "$mez-config set the prompt color", 0)
        .unwrap();
    let skill_block = context
        .blocks
        .iter()
        .find(|block| block.label == "explicit skill mez-config")
        .expect("missing explicit mez-config skill context block");

    assert!(
        skill_block
            .content
            .contains("Allowed operations: `set`, `unset`, `reset`")
    );
    assert!(skill_block.content.contains("theme.colors.agent_prompt_bg"));
    assert!(
        skill_block
            .content
            .contains("## Current effective Mezzanine config")
    );
    assert!(skill_block.content.contains("value path=theme.active"));
    assert!(
        skill_block
            .content
            .contains("## Additional context\n\nset the prompt color"),
        "{}",
        skill_block.content
    );
}

/// Verifies persisted skill payloads are not replayed into later model context.
///
/// This covers both newly compact skill-action transcripts and legacy
/// transcripts that may already contain an expanded `SKILL.md` body from an
/// earlier build. The next ordinary prompt should see the raw user request and
/// assistant/tool evidence, not stale skill workflow instructions.
#[test]
fn runtime_agent_context_omits_persisted_skill_payloads_from_replay() {
    let transcript_root = temp_root("runtime-skill-transcript-replay");
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    for (sequence, role, content) in [
        (
            1,
            crate::transcript::TranscriptRole::User,
            "# Skill: review\n\nSource: project\nPath: skills/review/SKILL.md\n\nInvocation state: this skill is already loaded for the current turn.\n\nReview workflow body.",
        ),
        (
            2,
            crate::transcript::TranscriptRole::Tool,
            "action_id=skill-1 action_type=call_skill status=Succeeded\ncontent:\n# Skill: review\n\nReview workflow body.",
        ),
        (
            3,
            crate::transcript::TranscriptRole::Tool,
            "action_id=catalog-1 action_type=request_skills status=Succeeded\ncontent:\nAvailable skills:\n- review (project) - Review workflow body.",
        ),
        (
            4,
            crate::transcript::TranscriptRole::User,
            "$review focus src/lib.rs",
        ),
        (
            5,
            crate::transcript::TranscriptRole::Assistant,
            "I reviewed the requested area.",
        ),
    ] {
        transcript_store
            .append(&crate::transcript::TranscriptEntry {
                conversation_id: conversation_id.clone(),
                sequence,
                created_at_unix_seconds: 100,
                role,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: content.to_string(),
            })
            .unwrap();
    }
    service
        .agent_shell_store_mut()
        .record_transcript_entries("%1", 5)
        .unwrap();

    let context = service
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    let replayed = context
        .blocks
        .iter()
        .filter(|block| {
            matches!(
                block.source,
                ContextSourceKind::TranscriptUser
                    | ContextSourceKind::TranscriptAssistant
                    | ContextSourceKind::TranscriptTool
            )
        })
        .map(|block| block.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    assert!(replayed.contains("$review focus src/lib.rs"), "{replayed}");
    assert!(
        replayed.contains("I reviewed the requested area."),
        "{replayed}"
    );
    assert!(!replayed.contains("# Skill:"), "{replayed}");
    assert!(!replayed.contains("Review workflow body"), "{replayed}");
    assert!(!replayed.contains("Available skills:"), "{replayed}");
    let _ = fs::remove_dir_all(transcript_root);
}

/// Verifies explicit `$skill` prompts do not allow a model to loop by loading
/// the same skill again.
///
/// A `$create-skill ...` prompt has already loaded the built-in skill into the
/// turn context. If the model responds with `call_skill(create-skill)` instead
/// of requesting a concrete execution capability, the strict request surface
/// should reject the action before runtime skill execution can start another
/// successful continuation.
#[test]
fn runtime_explicit_skill_prompt_rejects_redundant_call_skill_loop() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "$create-skill create a review skill")
        .unwrap();
    service
        .pending_agent_provider_tasks
        .remove(&started.turn_id);
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "load create skill again".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "load skill authoring context".to_string(),
                turn_id: started.turn_id.clone(),
                agent_id: started.agent_id.clone(),
                actions: vec![crate::agent::AgentAction {
                    id: "skill-loop".to_string(),
                    rationale: "load the create-skill workflow".to_string(),
                    payload: crate::agent::AgentActionPayload::CallSkill {
                        name: "create-skill".to_string(),
                        additional_context: Some("create a review skill".to_string()),
                    },
                }],
                final_turn: false,
            }),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            &started.turn_id,
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert!(
        !service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == started.turn_id)
    );
    assert!(
        execution
            .response
            .raw_text
            .contains("maap action type call_skill is not allowed"),
        "{}",
        execution.response.raw_text
    );
    assert!(
        execution
            .request
            .allowed_actions
            .action_type_names()
            .contains(&"request_capability")
    );
    assert!(
        !execution
            .request
            .allowed_actions
            .action_type_names()
            .contains(&"call_skill")
    );
    assert!(execution.action_results.is_empty());
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("maap_validation_error"), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies explicit `$skill` prompts do not need an additional skill catalog
/// lookup before acting on the already-loaded workflow.
///
/// The model-facing action surface suppresses `request_skills` once a full
/// skill body is in context. A provider that still emits the forbidden lookup
/// is rejected at MAAP validation rather than handed to the runtime skill
/// executor as another recoverable lookup.
#[test]
fn runtime_explicit_skill_prompt_rejects_redundant_skill_catalog_lookup() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "$create-skill create a review skill")
        .unwrap();
    service
        .pending_agent_provider_tasks
        .remove(&started.turn_id);
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "request skills again".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "check available skill workflows".to_string(),
                turn_id: started.turn_id.clone(),
                agent_id: started.agent_id.clone(),
                actions: vec![crate::agent::AgentAction {
                    id: "skill-catalog-loop".to_string(),
                    rationale: "check available skill workflows".to_string(),
                    payload: crate::agent::AgentActionPayload::RequestSkills,
                }],
                final_turn: false,
            }),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            &started.turn_id,
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert!(
        execution
            .response
            .raw_text
            .contains("maap action type request_skills is not allowed"),
        "{}",
        execution.response.raw_text
    );
    assert!(
        !execution
            .request
            .allowed_actions
            .action_type_names()
            .contains(&"request_skills")
    );
    assert!(execution.action_results.is_empty());
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies `/list-skills` displays the effective pane skill catalog with the
/// same `$skill` invocation syntax accepted by explicit skill prompts. This
/// gives users a discoverable way to see and select available workflows before
/// submitting a prompt.
#[test]
fn runtime_agent_shell_list_skills_displays_effective_catalog() {
    let config_root = temp_root("runtime-list-skills");
    let skill_dir = config_root.join("skills/review");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: review\ndescription: Review workflow\n---\n\nCheck tests and risks.\n",
    )
    .unwrap();
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_config_root(config_root);

    let response = service
        .execute_agent_shell_command(&primary, "/list-skills")
        .unwrap();

    assert!(response.contains("## Skills"), "{response}");
    assert!(response.contains("Start a prompt with `$`"), "{response}");
    assert!(
        response.contains("`$<skill-name> [additional context]`"),
        "{response}"
    );
    assert!(
        response
            .contains("| `$create-skill` | builtin | Create or modify concise Mezzanine skills"),
        "{response}"
    );
    assert!(
        response.contains("| `$review` | user | Review workflow |"),
        "{response}"
    );
}

/// Verifies `/list-skills` shows the built-in skill-authoring workflow when the
/// current pane has no user or trusted-project skills. This makes skill
/// creation discoverable before any external skill directories exist.
#[test]
fn runtime_agent_shell_list_skills_reports_builtin_catalog_without_external_skills() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_config_root(temp_root("runtime-list-skills-empty"));

    let response = service
        .execute_agent_shell_command(&primary, "/list-skills")
        .unwrap();

    assert!(
        response
            .contains("| `$create-skill` | builtin | Create or modify concise Mezzanine skills"),
        "{response}"
    );
    assert!(
        !response.contains("No skills are currently available."),
        "{response}"
    );
    assert!(response.contains("Start a prompt with `$`"), "{response}");
}

/// Verifies overlapping compaction attempts are rejected before they can start
/// another model request for the same pane.
#[test]
fn runtime_agent_shell_compact_rejects_overlapping_pane_compaction() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.agent_compacting_panes.insert("%1".to_string(), 1);

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compact-overlap","method":"agent/shell/command","params":{"idempotency_key":"compact-overlap","input":"/compact"}}"#,
        &primary,
    );

    assert!(response.contains("already compacting"), "{response}");
}

/// Verifies compaction keeps only a bounded raw transcript tail when the active
/// conversation is larger than the exact-reference window.
///
/// The compact memory can summarize older entries, but the next turn needs the
/// recent tail verbatim for prompts like "implement the first item". Older raw
/// messages should not remain in transcript replay after compaction.
#[test]
fn runtime_agent_shell_compact_retains_bounded_recent_transcript_tail() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "compact-tail-context-window".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "openai"
default_model_profile = "compact-tail-test"
auto_compact = false
[providers.openai]
kind = "openai"
models = ["gpt-compact-tail-test"]
default_model = "gpt-compact-tail-test"
[model_profiles.compact-tail-test]
provider = "openai"
model = "gpt-compact-tail-test"
context_window_tokens = 5000
"#
            .to_string(),
        }])
        .unwrap();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-compact-tail"));
    for sequence in 1..=12 {
        let (role, content) = match sequence {
            1 => (
                crate::transcript::TranscriptRole::User,
                format!(
                    "old raw marker should be summary only {}",
                    "old-word ".repeat(28)
                ),
            ),
            8 => (
                crate::transcript::TranscriptRole::Assistant,
                format!(
                    "Recent targets:\n1. Preserve raw tail after compaction.\n2. Keep memory summary. {}",
                    "recent-word ".repeat(28)
                ),
            ),
            _ if sequence % 2 == 0 => (
                crate::transcript::TranscriptRole::User,
                format!("filler user turn {sequence} {}", "tail-user ".repeat(28)),
            ),
            _ => (
                crate::transcript::TranscriptRole::Assistant,
                format!(
                    "filler assistant turn {sequence} {}",
                    "tail-assistant ".repeat(28)
                ),
            ),
        };
        transcript_store
            .append(&crate::transcript::TranscriptEntry {
                conversation_id: "as-tail".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content,
            })
            .unwrap();
    }
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap(),
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "as-tail", 12)
        .unwrap();

    let compact = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compact-tail","method":"agent/shell/command","params":{"idempotency_key":"compact-tail","input":"/compact"}}"#,
        &primary,
    );

    assert!(compact.contains("state=queued"), "{compact}");
    assert!(compact.contains("summarized_entries=5"), "{compact}");
    complete_runtime_test_compaction(&mut service, "%1", "old raw marker should be summary only");
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .transcript_entries,
        7
    );

    let prompt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compact-tail-prompt","method":"agent/shell/command","params":{"idempotency_key":"compact-tail-prompt","input":"Implement the first item"}}"#,
        &primary,
    );
    assert!(prompt.contains(r#""state":"running""#), "{prompt}");
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    let compaction_notice = context
        .blocks
        .iter()
        .find(|block| block.label == "conversation compaction notice")
        .expect("compaction notice should be model-visible after /compact");
    assert!(
        compaction_notice
            .content
            .contains("Older durable transcript entries were summarized"),
        "{compaction_notice:?}"
    );
    let transcript_context = context
        .blocks
        .iter()
        .filter(|block| {
            matches!(
                block.source,
                ContextSourceKind::Transcript
                    | ContextSourceKind::TranscriptUser
                    | ContextSourceKind::TranscriptAssistant
                    | ContextSourceKind::TranscriptTool
            )
        })
        .map(|block| block.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        transcript_context.contains("1. Preserve raw tail after compaction."),
        "{transcript_context}"
    );
    assert!(
        !transcript_context.contains("old raw marker should be summary only"),
        "{transcript_context}"
    );
}

/// Verifies explicit `/compact` is forced even when the entire transcript fits
/// inside the normal retained-tail budget.
///
/// The user command is a direct request to compact now, so it must summarize at
/// least one active durable entry instead of returning a budget-based no-op.
#[test]
fn runtime_agent_shell_compact_forces_summary_when_under_context_budget() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "compact-forced-context-window".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "openai"
default_model_profile = "compact-forced-test"
[providers.openai]
kind = "openai"
models = ["gpt-compact-forced-test"]
default_model = "gpt-compact-forced-test"
[model_profiles.compact-forced-test]
provider = "openai"
model = "gpt-compact-forced-test"
context_window_tokens = 128000
"#
            .to_string(),
        }])
        .unwrap();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-compact-forced"));
    for sequence in 1..=3 {
        transcript_store
            .append(&crate::transcript::TranscriptEntry {
                conversation_id: "as-forced".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role: crate::transcript::TranscriptRole::Assistant,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: format!("forced compact marker {sequence}"),
            })
            .unwrap();
    }
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "as-forced", 3)
        .unwrap();

    let compact = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compact-forced","method":"agent/shell/command","params":{"idempotency_key":"compact-forced","input":"/compact"}}"#,
        &primary,
    );

    assert!(compact.contains(r#""kind":"mutated""#), "{compact}");
    assert!(compact.contains("state=queued"), "{compact}");
    assert!(compact.contains("summarized_entries=1"), "{compact}");
    assert!(
        !compact.contains("within-retained-context-tail"),
        "{compact}"
    );
    complete_runtime_test_compaction(&mut service, "%1", "forced compact marker 1");
    let compacted = service
        .memory_records()
        .into_iter()
        .find(|record| record.id == "compact-as-forced")
        .expect("compacted memory record");
    assert!(
        compacted.content.contains("forced compact marker 1"),
        "{}",
        compacted.content
    );
}

/// Verifies that `/compact` is explicit when there is no transcript content to
/// compact and that the empty path does not create a misleading memory record.
#[test]
fn runtime_agent_shell_compact_reports_empty_transcript_without_memory() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let compact = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compact-empty","method":"agent/shell/command","params":{"idempotency_key":"compact-empty","input":"/compact"}}"#,
        &primary,
    );

    assert!(compact.contains(r#""kind":"display""#), "{compact}");
    assert!(compact.contains(r#""command":"compact""#), "{compact}");
    assert!(
        compact.contains("compacted=false reason=no-transcript-entries"),
        "{compact}"
    );
    assert!(compact.contains("source=model-compact"), "{compact}");
    assert!(service.memory_records().is_empty());
}

/// Verifies that `/personality` mutates live pane-scoped agent preferences and
/// that those preferences are appended to the next prompt context. This makes
/// the slash command affect provider input instead of only acknowledging a
/// runtime placeholder.
#[test]
fn runtime_agent_shell_personality_feeds_prompt_context() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let personality = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"personality","method":"agent/shell/command","params":{"idempotency_key":"personality","input":"/personality concise"}}"#,
        &primary,
    );
    assert!(personality.contains(r#""kind":"mutated""#), "{personality}");
    assert!(
        personality.contains(r#""command":"personality""#),
        "{personality}"
    );
    assert!(personality.contains("style=concise"), "{personality}");
    assert!(
        personality.contains("source=runtime-personality"),
        "{personality}"
    );
    assert!(!personality.contains("requires_runtime"), "{personality}");

    let prompt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"preference-prompt","method":"agent/shell/command","params":{"idempotency_key":"preference-prompt","input":"prepare work"}}"#,
        &primary,
    );
    assert!(prompt.contains(r#""state":"running""#), "{prompt}");
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    assert!(
        context
            .blocks
            .iter()
            .any(|block| block.label == "agent shell personality"
                && block.content.contains("concise"))
    );
}

/// Verifies that saved agent conversations can be listed, resumed into the
/// current pane, exposed to prompt context, and forked while keeping readline
/// prompt history available through the shared prompt-history file.
#[test]
fn runtime_agent_shell_resume_and_fork_manage_saved_conversations() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-resume-fork"));
    let cwd = temp_root("runtime-agent-resume-cwd");
    fs::create_dir_all(&cwd).unwrap();
    transcript_store
        .append(&crate::transcript::TranscriptEntry {
            conversation_id: "saved".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: crate::transcript::TranscriptRole::System,
            turn_id: "turn-old".to_string(),
            agent_id: "agent-%9".to_string(),
            pane_id: "%9".to_string(),
            content: format!("cwd={}", cwd.display()),
        })
        .unwrap();
    transcript_store
        .append(&crate::transcript::TranscriptEntry {
            conversation_id: "saved".to_string(),
            sequence: 2,
            created_at_unix_seconds: 1,
            role: crate::transcript::TranscriptRole::User,
            turn_id: "turn-old".to_string(),
            agent_id: "agent-%9".to_string(),
            pane_id: "%9".to_string(),
            content: "saved prompt".to_string(),
        })
        .unwrap();
    transcript_store
        .append_prompt_history("saved", "find files")
        .unwrap();
    transcript_store
        .append(&crate::transcript::TranscriptEntry {
            conversation_id: "latest".to_string(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: crate::transcript::TranscriptRole::User,
            turn_id: "turn-latest".to_string(),
            agent_id: "agent-%8".to_string(),
            pane_id: "%8".to_string(),
            content: "latest prompt".to_string(),
        })
        .unwrap();
    transcript_store
        .append_presentation(&crate::transcript::AgentPresentationEntry {
            conversation_id: "saved".to_string(),
            sequence: 1,
            created_at_unix_seconds: 3,
            pane_id: "%9".to_string(),
            turn_id: Some("turn-old".to_string()),
            terminal_width: 80,
            style_names: vec!["assistant".to_string(), "status".to_string()],
            display_lines: vec![
                "agent> rendered saved response".to_string(),
                "agent: rendered saved status".to_string(),
            ],
            copy_lines: vec![
                "agent> copy saved response".to_string(),
                "agent: copy saved status".to_string(),
            ],
            ansi_text: Some(
                "\r▐ agent> rendered saved response\r\n▐ agent: rendered saved status\r\n▐ ansi-only replay marker\r\n"
                    .to_string(),
            ),
        })
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap(),
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let picker = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"resume-list","method":"agent/shell/command","params":{"idempotency_key":"resume-list","input":"/resume"}}"#,
        &primary,
    );
    assert!(picker.contains("mez-agent:/resume%20saved"), "{picker}");
    assert!(picker.contains("mez-agent:/resume%20latest"), "{picker}");

    let latest = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"resume-latest","method":"agent/shell/command","params":{"idempotency_key":"resume-latest","input":"/resume --latest"}}"#,
        &primary,
    );
    assert!(latest.contains("conversation_id=latest"), "{latest}");
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.session_id.as_str()),
        Some("latest")
    );

    let resumed = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"resume","method":"agent/shell/command","params":{"idempotency_key":"resume","input":"/resume saved"}}"#,
        &primary,
    );
    assert!(resumed.contains("conversation_id=saved"), "{resumed}");
    assert_eq!(
        service.pane_current_working_directory("%1").as_deref(),
        Some(cwd.as_path())
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.session_id.as_str()),
        Some("saved")
    );
    let resumed_pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        resumed_pane_text.contains("rendered sa") && resumed_pane_text.contains("ved response"),
        "{resumed_pane_text}"
    );
    assert!(
        resumed_pane_text.contains("agent: rendered sa")
            && resumed_pane_text.contains("ved status"),
        "{resumed_pane_text}"
    );
    assert!(
        resumed_pane_text.contains("ansi-only") && resumed_pane_text.contains("arker"),
        "{resumed_pane_text}"
    );
    assert!(
        !resumed_pane_text.contains("Resumed Agent Session"),
        "{resumed_pane_text}"
    );
    assert_eq!(
        service
            .agent_prompt_inputs
            .get("%1")
            .unwrap()
            .prompt
            .buffer
            .history(),
        &[
            String::from("find files"),
            String::from("/resume"),
            String::from("/resume --latest"),
            String::from("/resume saved")
        ]
    );
    let context = service
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    assert!(context.blocks.iter().any(|block| {
        block.source == crate::agent::ContextSourceKind::TranscriptUser
            && block.content.contains("saved prompt")
    }));

    let forked = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"fork","method":"agent/shell/command","params":{"idempotency_key":"fork","input":"/fork saved-fork"}}"#,
        &primary,
    );
    assert!(forked.contains("source=saved"), "{forked}");
    assert!(forked.contains("conversation_id=saved-fork"), "{forked}");
    assert!(forked.contains("source_pane=%1"), "{forked}");
    assert_eq!(transcript_store.inspect("saved-fork").unwrap().len(), 2);
    assert_eq!(
        transcript_store.inspect_presentation("saved-fork").unwrap()[0].display_lines[0],
        "agent> rendered saved response"
    );
    let forked_pane = service
        .agent_shell_store()
        .sessions()
        .find(|session| session.session_id == "saved-fork")
        .map(|session| session.pane_id.clone())
        .expect("forked conversation should be bound to a pane");
    assert_ne!(forked_pane, "%1");
    assert_eq!(
        transcript_store.prompt_history("saved-fork").unwrap(),
        vec![
            String::from("find files"),
            String::from("/resume"),
            String::from("/resume --latest"),
            String::from("/resume saved"),
            String::from("/fork saved-fork")
        ]
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.session_id.as_str()),
        Some("saved")
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get(&forked_pane)
            .map(|session| session.session_id.as_str()),
        Some("saved-fork")
    );
    assert_eq!(
        service
            .agent_prompt_inputs
            .get(&forked_pane)
            .unwrap()
            .prompt
            .buffer
            .line(),
        "/resume saved"
    );
    service.pane_processes_mut().terminate_all().unwrap();
    let _ = fs::remove_dir_all(cwd);
}

/// Verifies that live agent pane rendering writes a separate durable
/// presentation log and does not leak presentation-only text into future model
/// context.
#[test]
fn runtime_agent_presentation_persistence_stays_out_of_model_context() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-presentation"));
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();

    service
        .append_agent_assistant_text_to_terminal_buffer("%1", "visual-only pane replay")
        .unwrap();

    let presentation = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    assert_eq!(presentation.len(), 1);
    assert_eq!(presentation[0].style_names, vec!["assistant"]);
    assert_eq!(
        presentation[0].display_lines,
        vec![String::from("agent> visual-only pane replay")]
    );
    assert!(
        presentation[0]
            .ansi_text
            .as_deref()
            .is_some_and(|text| text.contains("visual-only pane replay"))
    );
    assert!(transcript_store.inspect(&conversation_id).is_err());
    let context = service
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    assert!(
        context
            .blocks
            .iter()
            .all(|block| !block.content.contains("visual-only pane replay"))
    );
}

/// Verifies active agent shell metadata survives a daemon-style restart for the
/// same Mezzanine session without replaying a prompt or requiring a snapshot.
#[test]
fn runtime_restores_active_agent_session_metadata_for_same_session() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-active-restore"));
    let cwd = temp_root("runtime-agent-active-restore-cwd");
    fs::create_dir_all(&cwd).unwrap();
    transcript_store
        .append(&crate::transcript::TranscriptEntry {
            conversation_id: "saved".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: crate::transcript::TranscriptRole::User,
            turn_id: "turn-old".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "saved restart context".to_string(),
        })
        .unwrap();
    transcript_store
        .append_prompt_history("saved", "remember this")
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .pane_current_working_directories
        .insert("%1".to_string(), cwd.clone());

    let resumed = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"restore-resume","method":"agent/shell/command","params":{"idempotency_key":"restore-resume","input":"/resume saved"}}"#,
        &primary,
    );
    assert!(resumed.contains("conversation_id=saved"), "{resumed}");
    service.record_agent_provider_token_usage(
        "%1",
        crate::agent::ModelTokenUsage {
            input_tokens: 321,
            output_tokens: 45,
            reasoning_tokens: 12,
            cached_input_tokens: Some(123),
        },
    );
    let auto_reasoning = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"restore-auto-reasoning","method":"agent/shell/command","params":{"idempotency_key":"restore-auto-reasoning","input":"/auto-reasoning on"}}"#,
        &primary,
    );
    assert!(auto_reasoning.contains("enabled=true"), "{auto_reasoning}");
    let approval = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"restore-approval","method":"agent/shell/command","params":{"idempotency_key":"restore-approval","input":"/approval full-access"}}"#,
        &primary,
    );
    assert!(approval.contains("requested=full-access"), "{approval}");
    let personality = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"restore-personality","method":"agent/shell/command","params":{"idempotency_key":"restore-personality","input":"/personality concise"}}"#,
        &primary,
    );
    assert!(personality.contains("style=concise"), "{personality}");
    let log_level = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"restore-log-level","method":"agent/shell/command","params":{"idempotency_key":"restore-log-level","input":"/log-level trace"}}"#,
        &primary,
    );
    assert!(log_level.contains("now trace"), "{log_level}");
    let saved_metadata = transcript_store
        .load_agent_session_metadata(service.session().id.as_str())
        .unwrap();
    assert_eq!(saved_metadata.len(), 1);
    assert_eq!(
        saved_metadata[0].working_directory.as_deref(),
        Some(cwd.to_string_lossy().as_ref())
    );
    assert_eq!(
        saved_metadata[0].token_usage,
        crate::agent::ModelTokenUsage {
            input_tokens: 321,
            output_tokens: 45,
            reasoning_tokens: 12,
            cached_input_tokens: Some(123),
        }
    );
    assert_eq!(saved_metadata[0].auto_reasoning_enabled, Some(true));
    assert_eq!(
        saved_metadata[0].approval_policy.as_deref(),
        Some("full-access")
    );

    let mut restored = test_runtime_service();
    restored.session.id = service.session().id.clone();
    restored.set_agent_transcript_store(transcript_store.clone());
    let restored_count = restored
        .restore_agent_sessions_from_transcript_store()
        .unwrap();

    let restored_session = restored.agent_shell_store().get("%1").unwrap();
    assert_eq!(restored_count, 1);
    assert_eq!(restored_session.session_id, "saved");
    assert_eq!(restored_session.visibility, AgentShellVisibility::Visible);
    assert_eq!(restored_session.transcript_entries, 1);
    assert_eq!(restored_session.log_level, AgentLogLevel::Trace);
    assert_eq!(
        restored
            .agent_token_usage_by_conversation
            .get("saved")
            .copied(),
        Some(crate::agent::ModelTokenUsage {
            input_tokens: 321,
            output_tokens: 45,
            reasoning_tokens: 12,
            cached_input_tokens: Some(123),
        })
    );
    assert_eq!(
        restored.agent_auto_reasoning_overrides.get("%1").copied(),
        Some(true)
    );
    assert_eq!(
        restored.permission_policy().approval_policy,
        ApprovalPolicy::FullAccess
    );
    assert_eq!(
        restored.pane_current_working_directory("%1").as_deref(),
        Some(cwd.as_path())
    );
    assert_eq!(
        restored.agent_response_styles.get("%1").map(String::as_str),
        Some("concise")
    );
    assert_eq!(
        restored
            .agent_prompt_inputs
            .get("%1")
            .unwrap()
            .prompt
            .buffer
            .history(),
        &[
            String::from("remember this"),
            String::from("/resume saved"),
            String::from("/auto-reasoning on"),
            String::from("/approval full-access"),
            String::from("/personality concise"),
            String::from("/log-level trace"),
        ]
    );
    let context = restored
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    assert!(context.blocks.iter().any(|block| {
        block.source == crate::agent::ContextSourceKind::TranscriptUser
            && block.content.contains("saved restart context")
    }));
    let _ = fs::remove_dir_all(cwd);
}

/// Verifies `/resume` reloads saved provider token totals for the rebound
/// conversation.
///
/// Active-session metadata is the durable source for pane-level provider
/// accounting. A manual resume path must hydrate the same in-memory usage map
/// as daemon startup restore so `/status` does not reset token counts to zero.
#[test]
fn runtime_resume_restores_provider_token_usage_from_session_metadata() {
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-resume-tokens"));
    let mut service = test_runtime_service();
    let mezzanine_session_id = service.session().id.as_str().to_string();
    transcript_store
        .append(&crate::transcript::TranscriptEntry {
            conversation_id: "saved-tokens".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: crate::transcript::TranscriptRole::User,
            turn_id: "turn-old".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "resume with prior token totals".to_string(),
        })
        .unwrap();
    transcript_store
        .save_agent_session_metadata(
            &mezzanine_session_id,
            &[crate::transcript::AgentSessionMetadata {
                mezzanine_session_id: mezzanine_session_id.clone(),
                pane_id: "%1".to_string(),
                conversation_id: "saved-tokens".to_string(),
                visibility: "visible".to_string(),
                running_turn_id: None,
                transcript_entries: 1,
                log_level: "normal".to_string(),
                pane_model_profile: None,
                planning_enabled: false,
                response_style: None,
                auto_reasoning_enabled: Some(true),
                approval_policy: Some("full-access".to_string()),
                working_directory: None,
                project_root: None,
                context_usage: Some("42%".to_string()),
                token_usage: crate::agent::ModelTokenUsage {
                    input_tokens: 900,
                    output_tokens: 80,
                    reasoning_tokens: 33,
                    cached_input_tokens: Some(450),
                },
            }],
        )
        .unwrap();
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let resumed = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"resume-tokens","method":"agent/shell/command","params":{"idempotency_key":"resume-tokens","input":"/resume saved-tokens"}}"#,
        &primary,
    );
    assert!(
        resumed.contains("conversation_id=saved-tokens"),
        "{resumed}"
    );
    assert_eq!(
        service.agent_auto_reasoning_overrides.get("%1").copied(),
        Some(true)
    );
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::FullAccess
    );
    assert_eq!(
        service
            .agent_context_usage_by_conversation
            .get("saved-tokens")
            .map(String::as_str),
        Some("42%")
    );
    let status = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"resume-token-status","method":"agent/shell/command","params":{"idempotency_key":"resume-token-status","input":"/status"}}"#,
        &primary,
    );

    assert!(
        status.contains(
            "| Provider tokens | input=450 raw_input=900 output=80 reasoning=33 cached_input=450 cache_hit=50.00% total=980 |"
        ),
        "{status}"
    );
}

/// Verifies legacy agent-session metadata cannot narrow a configured elevated
/// approval default during restore.
///
/// Older checkpoints recorded the effective policy even when it only came from
/// the default configuration. Restoring those rows must not silently preempt a
/// user's newer `permissions.approval_policy = "full-access"` configuration.
#[test]
fn runtime_agent_session_restore_does_not_narrow_configured_approval_default() {
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-approval-default"));
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[permissions]\napproval_policy = \"full-access\"\n".to_string(),
        }])
        .unwrap();
    let mezzanine_session_id = service.session().id.as_str().to_string();
    transcript_store
        .save_agent_session_metadata(
            &mezzanine_session_id,
            &[crate::transcript::AgentSessionMetadata {
                mezzanine_session_id: mezzanine_session_id.clone(),
                pane_id: "%1".to_string(),
                conversation_id: "legacy-ask".to_string(),
                visibility: "visible".to_string(),
                running_turn_id: None,
                transcript_entries: 1,
                log_level: "normal".to_string(),
                pane_model_profile: None,
                planning_enabled: false,
                response_style: None,
                auto_reasoning_enabled: None,
                approval_policy: Some("ask".to_string()),
                working_directory: None,
                project_root: None,
                context_usage: None,
                token_usage: Default::default(),
            }],
        )
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());

    let restored = service
        .restore_agent_sessions_from_transcript_store()
        .unwrap();

    assert_eq!(restored, 1);
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::FullAccess
    );
    let rewritten = transcript_store
        .load_agent_session_metadata(&mezzanine_session_id)
        .unwrap();
    assert_eq!(rewritten.len(), 1);
    assert_eq!(rewritten[0].approval_policy, None);
}

/// Verifies active agent metadata from a different Mezzanine session id does
/// not auto-bind a fresh runtime pane to a stale conversation.
#[test]
fn runtime_does_not_restore_agent_metadata_for_other_sessions() {
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-other-session"));
    transcript_store
        .save_agent_session_metadata(
            "$foreign",
            &[crate::transcript::AgentSessionMetadata {
                mezzanine_session_id: "$foreign".to_string(),
                pane_id: "%1".to_string(),
                conversation_id: "foreign".to_string(),
                visibility: "visible".to_string(),
                running_turn_id: None,
                transcript_entries: 1,
                log_level: "normal".to_string(),
                pane_model_profile: None,
                planning_enabled: false,
                response_style: None,
                auto_reasoning_enabled: None,
                approval_policy: None,
                working_directory: None,
                project_root: None,
                context_usage: None,
                token_usage: Default::default(),
            }],
        )
        .unwrap();
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store);

    let restored = service
        .restore_agent_sessions_from_transcript_store()
        .unwrap();

    assert_eq!(restored, 0);
    assert!(service.agent_shell_store().get("%1").is_none());
}

/// Verifies crash-recovered active metadata never resumes a previously running
/// turn automatically; it restores the conversation and records the turn as
/// interrupted so retry requires a fresh user action.
#[test]
fn runtime_restored_agent_metadata_marks_running_turn_interrupted() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-active-interrupted"));
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    service
        .start_agent_turn(crate::agent::AgentTurnRecord {
            turn_id: "turn-running-restore".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            trigger: crate::agent::AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 200,
            policy_profile: "runtime".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            cooperation_mode: None,
            state: AgentTurnState::Queued,
        })
        .unwrap();
    assert_eq!(
        transcript_store
            .load_agent_session_metadata(service.session().id.as_str())
            .unwrap()[0]
            .running_turn_id
            .as_deref(),
        Some("turn-running-restore")
    );

    let mut restored = test_runtime_service();
    restored.session.id = service.session().id.clone();
    restored.set_agent_transcript_store(transcript_store);
    let restored_count = restored
        .restore_agent_sessions_from_transcript_store()
        .unwrap();

    let restored_session = restored.agent_shell_store().get("%1").unwrap();
    let restored_turn = restored
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-running-restore")
        .unwrap();
    assert_eq!(restored_count, 1);
    assert_eq!(restored_session.session_id, conversation_id);
    assert_eq!(restored_session.running_turn_id, None);
    assert_eq!(restored_turn.state, AgentTurnState::Interrupted);
}

/// Verifies that `/fork` returns a concrete runtime diagnostic when no
/// transcript store is attached instead of falling back to a generic
/// runtime-required placeholder.
#[test]
fn runtime_agent_shell_fork_reports_missing_transcript_store() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"fork-missing-store","method":"agent/shell/command","params":{"idempotency_key":"fork-missing-store","input":"/fork branch"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(response.contains(r#""command":"fork""#), "{response}");
    assert!(
        response.contains("forked=false reason=transcript-store-unavailable"),
        "{response}"
    );
    assert!(response.contains("source=runtime-fork"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
}

/// Verifies runtime provider execution completes running prompt turn.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_provider_execution_completes_running_prompt_turn() {
    let mut service = test_runtime_service();
    let transcript_root = temp_root("runtime-provider-transcript");
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    service.set_agent_transcript_store(transcript_store.clone());
    let audit_root = temp_root("runtime-provider-audit");
    let audit_path = audit_root.join("audit.jsonl");
    service.set_audit_log(AuditLog::new(crate::audit::AuditConfig {
        enabled: true,
        required: true,
        path: audit_path.clone(),
        hash_chain: false,
    }));
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-provider-turn","input":"summarize the pane"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &RuntimeEchoProvider,
            ModelProfile {
                provider: "runtime-echo".to_string(),
                model: "echo-model".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert!(execution.final_turn);
    assert_eq!(execution.response.raw_text, "done");
    assert!(
        execution
            .request
            .messages
            .iter()
            .any(|message| message.content.contains("summarize the pane"))
    );
    assert_eq!(service.agent_scheduler().snapshot().running, 0);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    assert_eq!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == "turn-1")
            .map(|turn| turn.state),
        Some(AgentTurnState::Completed)
    );
    let entries = transcript_store.inspect(&conversation_id).unwrap();
    assert!(entries.iter().any(
        |entry| entry.role == crate::transcript::TranscriptRole::Assistant
            && entry.content == "done"
    ));
    assert!(
        entries
            .iter()
            .any(|entry| entry.content.contains("summarize the pane"))
    );
    assert_eq!(
        service.pane_transcript_refs.get("%1"),
        Some(&vec![format!("transcript:%1:{conversation_id}")])
    );
    let tasks = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"tasks","method":"agent/task/list","params":{"target":{"pane_id":"%1"}}}"#,
        &primary,
    );
    assert!(tasks.contains(r#""state":"completed""#), "{tasks}");
    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(
        audit.contains(r#""event_type":"external_integration""#),
        "{audit}"
    );
    assert!(audit.contains(r#""action":"provider_request""#), "{audit}");
    assert!(audit.contains(r#""outcome":"started""#), "{audit}");
    assert!(audit.contains(r#""outcome":"succeeded""#), "{audit}");
    assert!(audit.contains(r#""provider":"runtime-echo""#), "{audit}");
    assert!(audit.contains(r#""model":"echo-model""#), "{audit}");
    assert!(audit.contains(r#""turn_id":"turn-1""#), "{audit}");

    let _ = fs::remove_dir_all(transcript_root);
    let _ = fs::remove_dir_all(audit_root);
}

/// Verifies runtime treats a same-pane prompt submitted mid-turn as steering.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_prompt_during_running_turn_becomes_steering_context() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let first = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt-1","method":"agent/shell/command","params":{"idempotency_key":"agent-provider-turn-1","input":"first prompt"}}"#,
        &primary,
    );
    assert!(first.contains(r#""state":"running""#), "{first}");
    let second = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt-2","method":"agent/shell/command","params":{"idempotency_key":"agent-provider-turn-2","input":"second prompt"}}"#,
        &primary,
    );
    assert!(second.contains(r#""kind":"mutated""#), "{second}");
    assert!(second.contains(r#""command":"prompt""#), "{second}");
    assert!(second.contains("injected_user_input=true"), "{second}");
    assert_eq!(service.agent_turn_ledger.turns().len(), 1);
    assert_eq!(service.agent_scheduler().snapshot().queued, 0);
    assert_eq!(service.agent_scheduler().snapshot().running, 1);
    let provider = RuntimeRecordingProvider {
        provider: "runtime-batch",
        response: runtime_say_response("turn-1", "Acknowledged.", true),
        last_request: RefCell::new(None),
    };

    service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    let request = provider.last_request.borrow().clone().unwrap();
    let request_context = request
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        request_context.contains("second prompt"),
        "{request_context}"
    );
    assert!(
        request_context.contains("[user steering input during active turn]"),
        "{request_context}"
    );
    assert!(
        !service
            .agent_turn_ledger
            .turns()
            .iter()
            .any(|turn| turn.turn_id == "turn-2")
    );
}

/// Verifies that the live runtime scheduler applies the starvation-bound
/// fairness rule after a running turn finishes: a queued runnable turn from a
/// different agent starts before a same-agent follow-up when capacity is one.
#[test]
fn runtime_scheduler_prefers_other_runnable_agent_after_completion() {
    let mut service = test_runtime_service();
    service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(1)
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let pane2 = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    for pane in ["%1", pane2.as_str()] {
        service
            .agent_shell_store_mut()
            .enter_or_resume(pane)
            .unwrap();
        let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
        screen.feed(b"ready\n");
        service.pane_screens.insert(pane.to_string(), screen);
    }

    service.start_agent_prompt_turn("%1", "first").unwrap();
    service.start_agent_prompt_turn("%1", "second").unwrap();
    service
        .start_agent_prompt_turn(pane2.as_str(), "third")
        .unwrap();
    assert_eq!(service.agent_scheduler().snapshot().running, 1);
    assert_eq!(service.agent_scheduler().snapshot().queued, 2);

    service.agent_scheduler_mut().complete("turn-1").unwrap();
    service
        .finish_agent_turn("%1", "turn-1", AgentTurnState::Completed)
        .unwrap();

    assert_eq!(
        service
            .agent_scheduler()
            .running_turns()
            .map(|running| running.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec!["turn-3"]
    );
    assert_eq!(
        service
            .agent_scheduler()
            .queued_turns()
            .map(|queued| queued.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec!["turn-2"]
    );
}

/// Verifies terminal failures without a pane-local running shell marker still
/// drain scheduler capacity.
///
/// Some runtime failure paths settle a turn after its pane shell session was
/// already detached or removed. Those paths still release a global scheduler
/// slot, so they must immediately start queued independent work instead of
/// leaving it parked until unrelated input arrives.
#[test]
fn runtime_no_shell_session_provider_failure_starts_queued_turn() {
    let mut service = test_runtime_service();
    service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(1)
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    let pane2 = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    for pane in ["%1", pane2.as_str()] {
        service
            .agent_shell_store_mut()
            .enter_or_resume(pane)
            .unwrap();
        let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
        screen.feed(b"ready\n");
        service.pane_screens.insert(pane.to_string(), screen);
    }

    service.start_agent_prompt_turn("%1", "first").unwrap();
    service
        .start_agent_prompt_turn(pane2.as_str(), "second")
        .unwrap();
    assert_eq!(service.agent_scheduler().snapshot().running, 1);
    assert_eq!(service.agent_scheduler().snapshot().queued, 1);
    service.agent_shell_store_mut().remove_session("%1");

    let error = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &RuntimeBatchFailingProvider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert_eq!(
        service
            .agent_scheduler()
            .running_turns()
            .map(|running| running.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec!["turn-2"]
    );
    assert_eq!(service.agent_scheduler().snapshot().queued, 0);
    assert_eq!(
        service
            .agent_shell_store()
            .get(pane2.as_str())
            .and_then(|session| session.running_turn_id.as_deref()),
        Some("turn-2")
    );
}

/// Verifies joined child completion drains the scheduler when other joined
/// children are queued behind a low concurrency limit.
///
/// A blocked parent releases its global scheduler slot while it waits for
/// joined subagents. When the first running child finishes, the next queued
/// child must start immediately so the parent is not left waiting for a child
/// turn that is ready but never launched.
#[test]
fn runtime_joined_child_completion_starts_next_queued_child() {
    let mut service = test_runtime_service();
    service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(1)
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(120, 40).unwrap(), 120)
        .unwrap();
    let child_one_pane = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    let child_two_pane = service
        .session
        .split_active_pane(&primary, SplitDirection::Horizontal)
        .unwrap();
    for pane in ["%1", child_one_pane.as_str(), child_two_pane.as_str()] {
        service
            .agent_shell_store_mut()
            .enter_or_resume(pane)
            .unwrap();
        let mut screen = TerminalScreen::new(Size::new(24, 5).unwrap(), 10).unwrap();
        screen.feed(b"ready\n");
        service.pane_screens.insert(pane.to_string(), screen);
    }

    let parent = service.start_agent_prompt_turn("%1", "parent").unwrap();
    let child_one = service
        .start_agent_prompt_turn(child_one_pane.as_str(), "child one")
        .unwrap();
    let child_two = service
        .start_agent_prompt_turn(child_two_pane.as_str(), "child two")
        .unwrap();
    let parent_turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == parent.turn_id)
        .cloned()
        .unwrap();
    let spawn_one = runtime_spawn_agent_action("spawn-one", "child one");
    let spawn_two = runtime_spawn_agent_action("spawn-two", "child two");
    service.agent_turn_executions.insert(
        parent.turn_id.clone(),
        crate::agent::AgentTurnExecution {
            request: runtime_model_request_fixture_for_agent(&parent.turn_id, &parent.agent_id),
            response: crate::agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "spawn children".to_string(),
                usage: Default::default(),
                quota_usage: Default::default(),
                action_batch: Some(crate::agent::MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "test action batch rationale".to_string(),
                    turn_id: parent.turn_id.clone(),
                    agent_id: parent.agent_id.clone(),
                    actions: vec![spawn_one.clone(), spawn_two.clone()],
                    final_turn: false,
                }),
            },
            latest_response_usage: Default::default(),
            action_results: vec![
                crate::agent::ActionResult::running(
                    &parent_turn,
                    &spawn_one,
                    vec!["waiting for child one".to_string()],
                    None,
                ),
                crate::agent::ActionResult::running(
                    &parent_turn,
                    &spawn_two,
                    vec!["waiting for child two".to_string()],
                    None,
                ),
            ],
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        },
    );
    service.joined_subagent_dependencies.insert(
        child_one.turn_id.clone(),
        JoinedSubagentDependency {
            parent_turn_id: parent.turn_id.clone(),
            parent_action_id: "spawn-one".to_string(),
            child_turn_id: child_one.turn_id.clone(),
            child_agent_id: child_one.agent_id.clone(),
            child_display_name: Some("child one".to_string()),
        },
    );
    service.joined_subagent_dependencies.insert(
        child_two.turn_id.clone(),
        JoinedSubagentDependency {
            parent_turn_id: parent.turn_id.clone(),
            parent_action_id: "spawn-two".to_string(),
            child_turn_id: child_two.turn_id.clone(),
            child_agent_id: child_two.agent_id.clone(),
            child_display_name: Some("child two".to_string()),
        },
    );
    service.pending_agent_provider_tasks.remove(&parent.turn_id);
    service
        .agent_scheduler_mut()
        .block_running(&parent.turn_id)
        .unwrap();
    service
        .agent_turn_ledger
        .finish_turn(&parent.turn_id, AgentTurnState::Blocked)
        .unwrap();
    service.start_ready_agent_turns().unwrap();
    assert_eq!(
        service
            .agent_scheduler()
            .running_turns()
            .map(|running| running.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec![child_one.turn_id.as_str()]
    );
    assert_eq!(
        service
            .agent_scheduler()
            .queued_turns()
            .map(|queued| queued.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec![child_two.turn_id.as_str()]
    );

    let child_provider = RuntimeBatchProvider {
        response: runtime_say_response_for_agent(
            &child_one.turn_id,
            &child_one.agent_id,
            "child one done",
            true,
        ),
    };
    service
        .execute_agent_turn_with_provider(
            &child_one.turn_id,
            &child_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(
        service
            .agent_scheduler()
            .running_turns()
            .map(|running| running.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec![child_two.turn_id.as_str()]
    );
    assert_eq!(service.agent_scheduler().snapshot().queued, 0);
    assert!(
        !service
            .joined_subagent_dependencies
            .contains_key(&child_one.turn_id)
    );
    assert!(
        service
            .joined_subagent_dependencies
            .contains_key(&child_two.turn_id)
    );
    assert_eq!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == parent.turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Blocked)
    );
}

/// Verifies a stale running `spawn_agent` result without a live joined child is
/// not treated as a runtime progress path.
///
/// The recovery loop must be able to fail or repair an orphaned parent turn
/// instead of considering any running `spawn_agent` result sufficient evidence
/// that a child can still complete.
#[test]
fn runtime_stale_joined_spawn_result_is_unreachable_progress() {
    let mut service = test_runtime_service();
    let _primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(24, 5).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    let parent = service.start_agent_prompt_turn("%1", "parent").unwrap();
    let parent_turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == parent.turn_id)
        .cloned()
        .unwrap();
    let spawn = runtime_spawn_agent_action("spawn-stale", "missing child");
    service.agent_turn_executions.insert(
        parent.turn_id.clone(),
        crate::agent::AgentTurnExecution {
            request: runtime_model_request_fixture_for_agent(&parent.turn_id, &parent.agent_id),
            response: crate::agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "spawn child".to_string(),
                usage: Default::default(),
                quota_usage: Default::default(),
                action_batch: Some(crate::agent::MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "test action batch rationale".to_string(),
                    turn_id: parent.turn_id.clone(),
                    agent_id: parent.agent_id.clone(),
                    actions: vec![spawn.clone()],
                    final_turn: false,
                }),
            },
            latest_response_usage: Default::default(),
            action_results: vec![crate::agent::ActionResult::running(
                &parent_turn,
                &spawn,
                vec!["waiting for missing child".to_string()],
                None,
            )],
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        },
    );
    service.pending_agent_provider_tasks.remove(&parent.turn_id);

    assert!(service.unreachable_running_agent_turn_timer_needed());
    assert_eq!(service.reconcile_agent_runtime_progress_paths().unwrap(), 1);
    assert_eq!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == parent.turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Failed)
    );
    assert!(!service.agent_turn_executions.contains_key(&parent.turn_id));
}

/// Verifies runtime provider failure persists and finishes turn.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_provider_failure_persists_and_finishes_turn() {
    let mut service = test_runtime_service();
    let transcript_root = temp_root("runtime-provider-failure-transcript");
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    service.set_agent_transcript_store(transcript_store.clone());
    let audit_root = temp_root("runtime-provider-failure-audit");
    let audit_path = audit_root.join("audit.jsonl");
    service.set_audit_log(AuditLog::new(crate::audit::AuditConfig {
        enabled: true,
        required: true,
        path: audit_path.clone(),
        hash_chain: false,
    }));
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-provider-fail","input":"summarize the pane"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let error = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &RuntimeFailingProvider,
            ModelProfile {
                provider: "runtime-fail".to_string(),
                model: "failing-model".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
        )
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert_eq!(service.agent_scheduler().snapshot().running, 0);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    assert_eq!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == "turn-1")
            .map(|turn| turn.state),
        Some(AgentTurnState::Failed)
    );
    let entries = transcript_store.inspect(&conversation_id).unwrap();
    assert!(entries.iter().any(|entry| {
        entry.role == crate::transcript::TranscriptRole::Assistant
            && entry.content.contains("provider_error")
    }));
    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains(r#""outcome":"started""#), "{audit}");
    assert!(audit.contains(r#""outcome":"failed""#), "{audit}");
    assert!(audit.contains(r#""provider":"runtime-fail""#), "{audit}");
    assert!(audit.contains(r#""model":"failing-model""#), "{audit}");
    assert!(audit.contains(r#""error_kind":"invalid_state""#), "{audit}");
    assert!(
        audit.contains(r#""error_message":"provider API request failed""#),
        "{audit}"
    );
    let failed_audit_record = audit
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|record| record["outcome"] == "failed")
        .unwrap();
    let provider_failure_json = failed_audit_record["metadata"]["provider_failure_json"]
        .as_str()
        .unwrap();
    let provider_failure: serde_json::Value = serde_json::from_str(provider_failure_json).unwrap();
    assert_eq!(provider_failure["status_code"], 400);
    assert_eq!(
        provider_failure["error"]["message"],
        "stream must be set to true"
    );
    assert_eq!(provider_failure["error"]["type"], "invalid_request_error");
    assert_eq!(
        provider_failure["error"]["code"],
        "missing_required_parameter"
    );
    assert!(
        failed_audit_record["metadata"]["provider_failure_json_bytes"]
            .as_str()
            .is_some_and(|value| value.parse::<usize>().unwrap() > 0),
        "{failed_audit_record}"
    );
    assert!(
        failed_audit_record["metadata"]["provider_failure_json_sha256"]
            .as_str()
            .is_some_and(|value| value.len() == 64),
        "{failed_audit_record}"
    );
    let _ = fs::remove_dir_all(transcript_root);
    let _ = fs::remove_dir_all(audit_root);
}

/// Verifies that provider errors carrying malformed raw output preserve that
/// output in the failed assistant transcript entry. This covers provider-native
/// MAAP parse failures that happen before the provider can build a
/// `ModelResponse`.
#[test]
fn runtime_provider_parse_failure_persists_raw_provider_text() {
    let mut service = test_runtime_service();
    let transcript_root = temp_root("runtime-provider-parse-failure-transcript");
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    service.set_agent_transcript_store(transcript_store.clone());
    let audit_root = temp_root("runtime-provider-parse-failure-audit");
    let audit_path = audit_root.join("audit.jsonl");
    service.set_audit_log(AuditLog::new(crate::audit::AuditConfig {
        enabled: true,
        required: true,
        path: audit_path.clone(),
        hash_chain: false,
    }));
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-provider-parse-fail","input":"produce malformed maap"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let error = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &RuntimeProviderRawTextFailingProvider,
            ModelProfile {
                provider: "runtime-raw-fail".to_string(),
                model: "failing-model".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
        )
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    let entries = transcript_store.inspect(&conversation_id).unwrap();
    assert!(entries.iter().any(|entry| {
        entry.role == crate::transcript::TranscriptRole::Assistant
            && entry
                .content
                .contains("{\"protocol\":\"maap/1\",\"actions\":[]}")
            && entry.content.contains("provider_error")
            && entry.content.contains("missing turn_id")
    }));
    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains(r#""outcome":"failed""#), "{audit}");
    assert!(audit.contains(r#""provider_raw_text_bytes":"#), "{audit}");
    assert!(audit.contains(r#""provider_raw_text_sha256":"#), "{audit}");
    assert!(audit.contains(r#""provider_failure_json":"#), "{audit}");
    assert!(audit.contains(r#"malformed_model_output"#), "{audit}");
    assert!(
        !audit.contains(r#""protocol":"maap/1","actions":[]"#),
        "{audit}"
    );
    let _ = fs::remove_dir_all(transcript_root);
    let _ = fs::remove_dir_all(audit_root);
}

/// Verifies that batch-level MAAP validation failures are recorded as failed
/// agent turns with the provider's raw text and the validation diagnostic in
/// the persisted assistant transcript entry. This guards against treating
/// malformed provider output as an opaque provider failure after a response has
/// already been parsed into a `ModelResponse`.
#[test]
fn runtime_maap_validation_failure_persists_provider_response_detail() {
    let mut service = test_runtime_service();
    let transcript_root = temp_root("runtime-maap-validation-transcript");
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    service.set_agent_transcript_store(transcript_store.clone());
    let audit_root = temp_root("runtime-maap-validation-audit");
    let audit_path = audit_root.join("audit.jsonl");
    service.set_audit_log(AuditLog::new(crate::audit::AuditConfig {
        enabled: true,
        required: true,
        path: audit_path.clone(),
        hash_chain: false,
    }));
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .mcp_registry_mut()
        .add_server(crate::mcp::McpServerConfig::stdio(
            "state",
            "state",
            "mcp-state",
            Vec::new(),
        ))
        .unwrap();
    service
        .mcp_registry_mut()
        .mark_available(
            "state",
            vec![crate::mcp::McpToolState {
                server_id: String::new(),
                name: "list".to_string(),
                available: true,
                blacklisted: false,
                permission_required: false,
                effects: crate::mcp::McpToolEffects::none(),
                approval: crate::mcp::McpApprovalSetting::Allow,
                description: "list state".to_string(),
                input_schema_json: "{}".to_string(),
            }],
        )
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-maap-validation-fail","input":"call unavailable tool"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "bad maap action".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "mcp-1".to_string(),
                    rationale: "call missing tool".to_string(),
                    payload: crate::agent::AgentActionPayload::McpCall {
                        server: "missing".to_string(),
                        tool: "read".to_string(),
                        arguments_json: "{}".to_string(),
                    },
                }],
                final_turn: false,
            }),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            ModelProfile {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert_eq!(service.agent_scheduler().snapshot().running, 0);
    let entries = transcript_store.inspect(&conversation_id).unwrap();
    assert!(entries.iter().any(|entry| {
        entry.role == crate::transcript::TranscriptRole::Assistant
            && entry.content.contains("bad maap action")
            && entry.content.contains("maap_validation_error")
            && entry.content.contains("unavailable server")
    }));
    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains(r#""outcome":"succeeded""#), "{audit}");
    assert!(audit.contains(r#""outcome":"failed""#), "{audit}");
    assert!(audit.contains(r#""provider_raw_text_bytes":"#), "{audit}");
    assert!(audit.contains(r#""provider_failure_json":"#), "{audit}");
    let failed_audit_record = audit
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|record| record["outcome"] == "failed")
        .unwrap();
    let failure_json = failed_audit_record["metadata"]["provider_failure_json"]
        .as_str()
        .unwrap();
    let failure: serde_json::Value = serde_json::from_str(failure_json).unwrap();
    assert_eq!(failure["type"], "agent_turn_execution_failure");
    assert_eq!(failure["stage"], "maap_validation");
    assert_eq!(failure["response"]["action_batch_present"], true);
    assert_eq!(failure["response"]["action_count"], 1);
    assert!(
        failure["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("unavailable server")),
        "{failure}"
    );
    let _ = fs::remove_dir_all(transcript_root);
    let _ = fs::remove_dir_all(audit_root);
}

/// Verifies runtime executes accepted stdio mcp action and audits call.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test]
async fn runtime_executes_accepted_stdio_mcp_action_and_audits_call() {
    let mut service = test_runtime_service();
    let audit_root = temp_root("runtime-mcp-audit");
    let audit_path = audit_root.join("audit.jsonl");
    service.set_audit_log(AuditLog::new(crate::audit::AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: false,
        required: true,
    }));
    let script = runtime_mcp_fixture_script(false);
    service
        .replace_config_layers_async(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                "[mcp_servers.fixture]\ncommand = \"/bin/sh\"\nargs = [\"-c\", {}]\napproval = \"allow\"\ntool_timeout_ms = 1000\n",
                toml_string(&script)
            ),
        }])
        .await
        .unwrap();
    assert_eq!(
        service.mcp_registry().prompt_summary().available_tools[0].tool_name,
        "echo"
    );
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-mcp-turn","input":"call echo tool"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "calling mcp".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "m1".to_string(),
                    rationale: "call mcp".to_string(),
                    payload: crate::agent::AgentActionPayload::McpCall {
                        server: "fixture".to_string(),
                        tool: "echo".to_string(),
                        arguments_json: r#"{"message":"hello"}"#.to_string(),
                    },
                }],
                final_turn: true,
            }),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider_async(
            "turn-1",
            &provider,
            ModelProfile {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert!(execution.request.messages.iter().any(|message| {
        message.source == ContextSourceKind::Configuration
            && message.content.contains("[mcp integrations]")
            && message.content.contains("available_tool=fixture/echo")
    }));
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    assert!(
        execution.action_results[0]
            .content_text()
            .contains("hello from mcp")
    );
    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(
        audit.contains(r#""event_type":"external_integration""#),
        "{audit}"
    );
    assert!(audit.contains(r#""action":"mcp_call""#), "{audit}");
    assert!(audit.contains(r#""outcome":"started""#), "{audit}");
    assert!(audit.contains(r#""outcome":"succeeded""#), "{audit}");
    assert!(audit.contains(r#""server_id":"fixture""#), "{audit}");
    let _ = fs::remove_dir_all(audit_root);
}

/// Verifies full-access mode satisfies MCP tool prompt approval while still
/// executing the call through the normal MCP registry and transport path.
///
/// This prevents `approval = "prompt"` MCP tools from creating blocked action
/// approvals after the user has explicitly selected full-access mode.
#[tokio::test]
async fn runtime_full_access_executes_prompt_stdio_mcp_action() {
    let mut service = test_runtime_service();
    service.permission_policy_mut().approval_policy = ApprovalPolicy::FullAccess;
    let script = runtime_mcp_fixture_script(false);
    service
        .replace_config_layers_async(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                "[mcp_servers.fixture]\ncommand = \"/bin/sh\"\nargs = [\"-c\", {}]\napproval = \"prompt\"\ntool_timeout_ms = 1000\n",
                toml_string(&script)
            ),
        }])
        .await
        .unwrap();
    service.permission_policy_mut().approval_policy = ApprovalPolicy::FullAccess;
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-mcp-full-access","input":"call echo tool"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "calling mcp".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "m1".to_string(),
                    rationale: "call mcp".to_string(),
                    payload: crate::agent::AgentActionPayload::McpCall {
                        server: "fixture".to_string(),
                        tool: "echo".to_string(),
                        arguments_json: r#"{"message":"hello"}"#.to_string(),
                    },
                }],
                final_turn: true,
            }),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider_async(
            "turn-1",
            &provider,
            ModelProfile {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    assert!(service.blocked_approvals().pending().is_empty());
    assert!(
        execution.action_results[0]
            .content_text()
            .contains("hello from mcp")
    );
}

/// Runs the execute runtime send message action operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn execute_runtime_send_message_action(
    content_type: &str,
    payload: &str,
) -> (
    RuntimeSessionService,
    crate::agent::AgentTurnExecution,
    AgentId,
) {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let target_agent = AgentId::opaque("agent-%2").unwrap();
    service
        .message_service_mut()
        .ensure_agent_identity(
            SenderIdentity {
                agent_id: target_agent.clone(),
                pane_id: None,
                window_id: None,
                role: Some("worker".to_string()),
                capabilities: Vec::new(),
            },
            0,
        )
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 100).unwrap(),
    );
    service.pane_screens.get_mut("%1").unwrap().feed(b"ready\n");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-message-turn","input":"send local message"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "send message".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "msg-1".to_string(),
                    rationale: "coordinate with another local agent".to_string(),
                    payload: crate::agent::AgentActionPayload::SendMessage {
                        recipient: "agent:agent-%2".to_string(),
                        content_type: content_type.to_string(),
                        payload: payload.to_string(),
                    },
                }],
                final_turn: true,
            }),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            ModelProfile {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
        )
        .unwrap();

    (service, execution, target_agent)
}

/// Verifies that MAAP `send_message` still reaches the shared message queue
/// when its media metadata is valid. This protects the accepted text path while
/// invalid media handling is tightened to match MMP transport validation.
#[test]
fn runtime_executes_send_message_action_through_message_service() {
    let (service, execution, target_agent) =
        execute_runtime_send_message_action("text/plain; charset=utf-8", "hello worker");

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    assert!(
        execution.action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap_or_default()
            .contains(r#""delivery_status":"accepted""#)
    );
    let messages = service
        .message_service()
        .receive_for(&target_agent, u64::MAX);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content_type, "text/plain; charset=utf-8");
    assert_eq!(messages[0].payload, "hello worker");
}

/// Verifies that MAAP `send_message` canonicalizes the common model-emitted
/// `text/plain` shorthand before MMP delivery. The transport endpoint remains
/// strict, but model-produced coordination messages should not fail a subagent
/// turn when the payload is otherwise valid UTF-8 text.
#[test]
fn runtime_canonicalizes_send_message_text_plain_alias() {
    let (service, execution, target_agent) =
        execute_runtime_send_message_action("text/plain", "hello worker");

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    let messages = service
        .message_service()
        .receive_for(&target_agent, u64::MAX);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content_type, "text/plain; charset=utf-8");
    assert_eq!(messages[0].payload, "hello worker");
}

/// Verifies that MAAP `send_message` uses the same text, JSON, and binary
/// payload metadata validation as the MMP transport endpoint. Rejected actions
/// must not enqueue messages because the agent-facing action result is the
/// durable protocol feedback for the failed local delivery.
#[test]
fn runtime_rejects_send_message_action_with_invalid_mmp_payload_metadata() {
    let cases = [
        (
            "text/markdown",
            "hello worker",
            "MMP text payloads require content_type text/plain; charset=utf-8",
        ),
        (
            "application/json",
            "not-json",
            "MMP JSON payload must be valid JSON",
        ),
        (
            "application/octet-stream",
            "AQID",
            "MMP binary payloads require payload_encoding base64",
        ),
    ];

    for (content_type, payload, expected_message) in cases {
        let (service, execution, target_agent) =
            execute_runtime_send_message_action(content_type, payload);

        assert_eq!(execution.terminal_state, AgentTurnState::Running);
        let result = &execution.action_results[0];
        assert_eq!(result.status, ActionStatus::Failed);
        assert!(result.is_error);
        assert_eq!(
            result.error.as_ref().map(|error| error.code.as_str()),
            Some("invalid_message_payload")
        );
        assert_eq!(
            result.error.as_ref().map(|error| error.message.as_str()),
            Some(expected_message)
        );
        let structured = result.structured_content_json.as_deref().unwrap();
        assert!(structured.contains(r#""delivery_status":"rejected""#));
        assert!(structured.contains(r#""code":"invalid_params""#));
        assert!(structured.contains(expected_message), "{structured}");
        assert!(
            service
                .message_service()
                .receive_for(&target_agent, u64::MAX)
                .is_empty()
        );
        assert!(
            service
                .pending_agent_provider_tasks()
                .iter()
                .any(|task| task.turn_id == "turn-1")
        );
        let context = service.agent_turn_contexts.get("turn-1").unwrap();
        assert!(context.blocks.iter().any(|block| {
            block.source == ContextSourceKind::ActionResult
                && block
                    .content
                    .contains("[action_result msg-1 send_message failed]")
                && block.content.contains("invalid_message_payload")
        }));
        assert!(context.blocks.iter().any(|block| {
            block.source == ContextSourceKind::LocalMessage
                && block.content.contains("Message recovery")
                && block.content.contains("Next step:")
                && block.content.contains("content_type and payload shape")
        }));
    }
}

/// Verifies that MAAP `send_message` accepts valid JSON payloads through the
/// same shared validator. This catches accidental text-only validation when the
/// action path is kept in sync with MMP transport dispatch.
#[test]
fn runtime_accepts_send_message_action_with_valid_json_payload() {
    let (service, execution, target_agent) =
        execute_runtime_send_message_action("application/json", r#"{"status":"ok"}"#);

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    let messages = service
        .message_service()
        .receive_for(&target_agent, u64::MAX);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content_type, "application/json");
    assert_eq!(messages[0].payload, r#"{"status":"ok"}"#);
}

/// Verifies runtime nonfinal mcp action queues provider continuation.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test]
async fn runtime_nonfinal_mcp_action_queues_provider_continuation() {
    let mut service = test_runtime_service();
    let script = runtime_mcp_fixture_script(false);
    service
        .replace_config_layers_async(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                "[mcp_servers.fixture]\ncommand = \"/bin/sh\"\nargs = [\"-c\", {}]\napproval = \"allow\"\ntool_timeout_ms = 1000\n",
                toml_string(&script)
            ),
        }])
        .await
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-mcp-nonfinal","input":"call echo and continue"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let first_provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "calling mcp".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "m1".to_string(),
                    rationale: "call mcp".to_string(),
                    payload: crate::agent::AgentActionPayload::McpCall {
                        server: "fixture".to_string(),
                        tool: "echo".to_string(),
                        arguments_json: r#"{"message":"hello"}"#.to_string(),
                    },
                }],
                final_turn: false,
            }),
        },
    };
    let execution = service
        .execute_agent_turn_with_provider_async(
            "turn-1",
            &first_provider,
            ModelProfile {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].turn_id, "turn-1");

    let second_provider = RuntimeRecordingProvider {
        provider: "runtime-batch",
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "done".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(runtime_complete_batch("turn-1")),
        },
        last_request: RefCell::new(None),
    };
    let executions = service
        .poll_agent_provider_tasks_with_provider(&second_provider, 1)
        .unwrap();

    assert_eq!(executions.len(), 1);
    let request = second_provider.last_request.borrow().clone().unwrap();
    assert!(request.messages.iter().any(|message| {
        message.source == ContextSourceKind::ActionResult
            && message
                .content
                .contains("[action_result m1 mcp_call succeeded]")
    }));
}

/// Verifies that a nonzero shell action is fed back as ordinary model-visible
/// command evidence instead of consuming semantic-action recovery budget.
///
/// Nonzero shell exits are real command results. The model should always see
/// stdout/stderr and the exit status in the next request so it can decide
/// whether to retry, inspect, or report the failure.
#[test]
fn runtime_shell_action_nonzero_exit_queues_model_visible_result() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-failure-feedback","input":"run a command and recover"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.pending_agent_provider_tasks.remove("turn-1");
    let first_provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "failing shell".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![
                    crate::agent::AgentAction {
                        id: "shell-fail".to_string(),
                        rationale: "exercise failure feedback".to_string(),
                        payload: crate::agent::AgentActionPayload::ShellCommand {
                            summary: "Run a command that will need correction".to_string(),
                            command: "false".to_string(),
                            interactive: false,
                            stateful: false,
                            timeout_ms: None,
                        },
                    },
                    crate::agent::AgentAction {
                        id: "shell-next".to_string(),
                        rationale: "should wait for model after nonzero shell exit".to_string(),
                        payload: crate::agent::AgentActionPayload::ShellCommand {
                            summary: "Run a command after the failing command".to_string(),
                            command: "echo should wait".to_string(),
                            interactive: false,
                            stateful: false,
                            timeout_ms: None,
                        },
                    },
                ],
                final_turn: false,
            }),
        },
    };
    let first = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &first_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(first.terminal_state, AgentTurnState::Running);
    let marker = service
        .running_shell_transactions
        .iter()
        .find_map(|(marker, transaction)| match &transaction.kind {
            RunningShellTransactionKind::AgentAction { action_id } if action_id == "shell-fail" => {
                Some(marker.clone())
            }
            _ => None,
        })
        .unwrap();
    let encoded_failure_output = base64::engine::general_purpose::STANDARD
        .encode(b"model-visible failure output\n\x1b]133;D;0;mez_marker=spoof\x1b\\\n");
    let encoded_transport = format!(
        "__MEZ_SHELL_OUTPUT_BASE64_BEGIN__\n{encoded_failure_output}\n__MEZ_SHELL_OUTPUT_BASE64_END__\n"
    );
    let transaction = service.running_shell_transactions.get_mut(&marker).unwrap();
    transaction.observed_output_bytes = encoded_transport.len();
    transaction.observed_output_preview = encoded_transport;

    service
        .observe_agent_shell_transaction_start("%1", &marker, "turn-1", "agent-%1", "%1")
        .unwrap();
    service
        .observe_agent_shell_transaction_end("%1", &marker, "turn-1", "agent-%1", "%1", 2)
        .unwrap();

    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].turn_id, "turn-1");
    assert!(
        !service
            .running_shell_transactions
            .values()
            .any(|transaction| matches!(
                &transaction.kind,
                RunningShellTransactionKind::AgentAction { action_id } if action_id == "shell-next"
            ))
    );
    assert!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .any(|turn| turn.turn_id == "turn-1" && turn.state == AgentTurnState::Running)
    );
    assert!(service.agent_turn_executions.contains_key("turn-1"));
    assert!(service.agent_turn_failure_feedback_attempts.is_empty());
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result shell-fail shell_command succeeded]")
            && block.content.contains("exit_code: 2")
            && block.content.contains("model-visible failure output")
            && !block.content.contains("mez_marker=spoof")
            && !block.content.contains("__MEZ_SHELL_OUTPUT_BASE64_BEGIN__")
    }));
    assert!(!context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::LocalMessage
            && block.content.contains("action failure feedback")
    }));

    let second_provider = RuntimeRecordingProvider {
        provider: "runtime-batch",
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "corrected".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(runtime_complete_batch("turn-1")),
        },
        last_request: RefCell::new(None),
    };
    let executions = service
        .poll_agent_provider_tasks_with_provider(&second_provider, 1)
        .unwrap();

    assert_eq!(executions.len(), 1);
    assert_eq!(executions[0].terminal_state, AgentTurnState::Completed);
    let request = second_provider.last_request.borrow().clone().unwrap();
    assert!(request.messages.iter().any(|message| {
        message.source == ContextSourceKind::ActionResult
            && message
                .content
                .contains("[action_result shell-fail shell_command succeeded]")
    }));
    assert!(request.messages.iter().any(|message| {
        message.source == ContextSourceKind::ActionResult
            && message
                .content
                .contains("[action_result shell-next shell_command succeeded]")
            && message
                .content
                .contains("shell command not run because `shell-fail` exited with status 2")
    }));
    assert!(service.agent_turn_failure_feedback_attempts.is_empty());
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies provider failure after a nonzero shell command does not reuse stale
/// running execution state for final diagnostics.
///
/// Nonzero shell commands are ordinary model-visible observations. If the
/// follow-up provider request then fails, the final failure must describe the
/// provider boundary cleanly instead of reporting the impossible state
/// `turn state is running, not failed`.
#[test]
fn runtime_provider_failure_after_nonzero_shell_result_does_not_report_running_recovery_state() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-shell-provider-fail","input":"run a command and recover"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.pending_agent_provider_tasks.remove("turn-1");
    let first_provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "failing shell".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "shell-fail".to_string(),
                    rationale: "exercise failure feedback".to_string(),
                    payload: crate::agent::AgentActionPayload::ShellCommand {
                        summary: "Run a command that will need correction".to_string(),
                        command: "false".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
        },
    };
    service
        .execute_agent_turn_with_provider(
            "turn-1",
            &first_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    let marker = service
        .running_shell_transactions
        .iter()
        .find_map(|(marker, transaction)| match &transaction.kind {
            RunningShellTransactionKind::AgentAction { action_id } if action_id == "shell-fail" => {
                Some(marker.clone())
            }
            _ => None,
        })
        .unwrap();

    service
        .observe_agent_shell_transaction_start("%1", &marker, "turn-1", "agent-%1", "%1")
        .unwrap();
    service
        .observe_agent_shell_transaction_end("%1", &marker, "turn-1", "agent-%1", "%1", 127)
        .unwrap();

    let error = service
        .poll_agent_provider_tasks_with_provider(&RuntimeBatchFailingProvider, 1)
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        !pane_text.contains("turn state is running, not failed"),
        "{pane_text}"
    );
    assert!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .any(|turn| turn.turn_id == "turn-1" && turn.state == AgentTurnState::Failed)
    );
    assert!(!service.agent_turn_executions.contains_key("turn-1"));
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Dispatches a simple shell action and returns its pane and transaction marker.
///
/// Protocol-invariant tests need a real runtime-owned shell transaction so the
/// strict start-marker bookkeeping is populated the same way it is in normal
/// agent execution.
fn dispatch_protocol_test_shell_action(
    service: &mut RuntimeSessionService,
    primary: &crate::ids::ClientId,
    action_id: &str,
) -> (String, String) {
    mark_test_pane_ready(service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{{"idempotency_key":"agent-protocol-{action_id}","input":"run a shell command"}}}}"#
        ),
        primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.pending_agent_provider_tasks.remove("turn-1");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "shell".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: action_id.to_string(),
                    rationale: "run a shell command".to_string(),
                    payload: crate::agent::AgentActionPayload::ShellCommand {
                        summary: "Run a command".to_string(),
                        command: "true".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
        },
    };
    service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    let marker = service
        .running_shell_transactions
        .iter()
        .find_map(|(marker, transaction)| match &transaction.kind {
            RunningShellTransactionKind::AgentAction {
                action_id: candidate,
            } if candidate == action_id => Some(marker.clone()),
            _ => None,
        })
        .unwrap();
    assert!(
        service
            .shell_transaction_require_start_markers
            .contains(&marker)
    );
    ("%1".to_string(), marker)
}

/// Verifies mismatched shell-transaction markers fail the live action promptly.
///
/// A terminal OSC marker can be malformed, delayed, or spoofed. The runtime must
/// validate marker metadata against the retained transaction state and fail the
/// action instead of leaving the turn to wait for a later timeout.
#[test]
fn runtime_shell_transaction_metadata_mismatch_fails_live_action() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-marker-mismatch","input":"run a command"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.pending_agent_provider_tasks.remove("turn-1");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "shell".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "shell-1".to_string(),
                    rationale: "run a shell command".to_string(),
                    payload: crate::agent::AgentActionPayload::ShellCommand {
                        summary: "Run a command".to_string(),
                        command: "true".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
        },
    };
    service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    let marker = service
        .running_shell_transactions
        .iter()
        .find_map(|(marker, transaction)| match &transaction.kind {
            RunningShellTransactionKind::AgentAction { action_id } if action_id == "shell-1" => {
                Some(marker.clone())
            }
            _ => None,
        })
        .unwrap();

    let observed = service
        .observe_agent_shell_transaction_end("%2", &marker, "turn-1", "agent-%1", "%1", 0)
        .unwrap();

    assert_eq!(observed, 1);
    assert!(!service.running_shell_transactions.contains_key(&marker));
    assert!(
        !service
            .shell_transaction_require_start_markers
            .contains(&marker)
    );
    assert!(!service.shell_transaction_started_markers.contains(&marker));
    assert!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .any(|turn| turn.turn_id == "turn-1" && turn.state == AgentTurnState::Failed)
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text
            .contains("shell transaction marker metadata does not match runtime dispatch state"),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies a duplicate start marker fails the live shell action.
///
/// The wrapper start marker is the handoff boundary for deferred command
/// payloads. Seeing it twice for one marker means the in-band control stream is
/// no longer well framed, so the action should fail instead of waiting for a
/// later timeout.
#[test]
fn runtime_shell_transaction_duplicate_start_marker_fails_live_action() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let (pane_id, marker) =
        dispatch_protocol_test_shell_action(&mut service, &primary, "shell-duplicate-start");

    service
        .observe_agent_shell_transaction_start(&pane_id, &marker, "turn-1", "agent-%1", &pane_id)
        .unwrap();
    assert!(service.shell_transaction_started_markers.contains(&marker));
    let observed = service
        .observe_agent_shell_transaction_start(&pane_id, &marker, "turn-1", "agent-%1", &pane_id)
        .unwrap();

    assert_eq!(observed, 1);
    assert!(!service.running_shell_transactions.contains_key(&marker));
    assert!(
        !service
            .shell_transaction_require_start_markers
            .contains(&marker)
    );
    assert!(!service.shell_transaction_started_markers.contains(&marker));
    assert!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .any(|turn| turn.turn_id == "turn-1" && turn.state == AgentTurnState::Failed)
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("shell transaction emitted a duplicate start marker"),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies an end marker before the start marker fails the live shell action.
///
/// Runtime-dispatched wrappers must emit a start marker before any end marker.
/// An end marker first means the parser missed a control boundary or command
/// output spoofed the frame, either of which should fail fast with diagnostics.
#[test]
fn runtime_shell_transaction_end_before_start_marker_fails_live_action() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let (pane_id, marker) =
        dispatch_protocol_test_shell_action(&mut service, &primary, "shell-end-before-start");

    let observed = service
        .observe_agent_shell_transaction_end(&pane_id, &marker, "turn-1", "agent-%1", &pane_id, 0)
        .unwrap();

    assert_eq!(observed, 1);
    assert!(!service.running_shell_transactions.contains_key(&marker));
    assert!(
        !service
            .shell_transaction_require_start_markers
            .contains(&marker)
    );
    assert!(!service.shell_transaction_started_markers.contains(&marker));
    assert!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .any(|turn| turn.turn_id == "turn-1" && turn.state == AgentTurnState::Failed)
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("shell transaction end marker arrived before the start marker"),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies async pane write failures settle shell-backed file actions.
///
/// File mutations are sent through the pane shell as generated transactions. If
/// the async pane worker cannot write that transaction input, the action must
/// become a failed action result and queue model recovery instead of remaining
/// in the running-transaction table forever.
#[test]
fn runtime_pane_write_failure_fails_running_file_action() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let mut process = service
        .take_running_pane_process_for_async_owner(&pane_id)
        .unwrap();
    mark_test_pane_ready(&mut service, &pane_id);
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-write-failure","input":"create a note"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.pending_agent_provider_tasks.remove("turn-1");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "write file".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "patch-fail".to_string(),
                    rationale: "write a note".to_string(),
                    payload: crate::agent::AgentActionPayload::ApplyPatch {
                        patch: "*** Begin Patch\n*** Add File: note.txt\n+note\n*** End Patch"
                            .to_string(),
                        strip: None,
                    },
                }],
                final_turn: false,
            }),
        },
    };

    let first = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(first.terminal_state, AgentTurnState::Running);
    assert_eq!(service.drain_deferred_pane_inputs().len(), 1);
    assert!(
        service
            .running_shell_transactions
            .values()
            .any(|transaction| matches!(
                transaction.kind,
                RunningShellTransactionKind::AgentAction { ref action_id }
                    if action_id == "patch-fail"
            ))
    );

    assert!(
        service
            .apply_pane_write_failure_event(&pane_id, "synthetic PTY write failure")
            .unwrap()
    );

    assert!(
        service
            .running_shell_transactions
            .values()
            .all(|transaction| !matches!(
                transaction.kind,
                RunningShellTransactionKind::AgentAction { .. }
            ))
    );
    assert_eq!(service.pending_agent_provider_tasks().len(), 1);
    assert!(!service.agent_turn_executions.contains_key("turn-1"));
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result patch-fail apply_patch failed]")
            && block.content.contains("pane input write failed")
    }));

    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies shell transaction payload bytes are deferred until the wrapper
/// receiver emits its start marker.
///
/// Large generated file-action scripts must not be sent as part of the initial
/// shell wrapper. Waiting for the start marker proves the shell has reached the
/// read loop that treats following bytes as payload data instead of shell
/// source.
#[test]
fn runtime_shell_transaction_start_streams_deferred_payload() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let mut process = service
        .take_running_pane_process_for_async_owner(&pane_id)
        .unwrap();
    mark_test_pane_ready(&mut service, &pane_id);
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-stream-payload","input":"run command"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.pending_agent_provider_tasks.remove("turn-1");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "shell action".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "shell-stream".to_string(),
                    rationale: "run payload command".to_string(),
                    payload: crate::agent::AgentActionPayload::ShellCommand {
                        summary: "Run payload command".to_string(),
                        command: "printf '%s\\n' payload-marker".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    let deferred_wrapper = service.drain_deferred_pane_inputs();
    assert_eq!(deferred_wrapper.len(), 1);
    let wrapper_text = String::from_utf8_lossy(&deferred_wrapper[0].bytes);
    assert!(wrapper_text.contains("__mez_tx_"), "{wrapper_text}");
    assert!(!wrapper_text.contains("payload-marker"), "{wrapper_text}");
    let (marker, transaction) = service
        .running_shell_transactions
        .iter()
        .find(|(_, transaction)| {
            matches!(
                transaction.kind,
                RunningShellTransactionKind::AgentAction { ref action_id }
                    if action_id == "shell-stream"
            )
        })
        .map(|(marker, transaction)| (marker.clone(), transaction.clone()))
        .unwrap();
    assert!(transaction.pending_input_payload.is_some());

    service
        .observe_agent_shell_transaction_start(&pane_id, &marker, "turn-1", "agent-%1", &pane_id)
        .unwrap();

    let deferred_payload = service.drain_deferred_pane_inputs();
    assert_eq!(deferred_payload.len(), 1);
    let payload_text = String::from_utf8_lossy(&deferred_payload[0].bytes);
    let encoded = payload_text
        .lines()
        .take_while(|line| !line.starts_with("__MEZ_COMMAND_PAYLOAD_END_"))
        .collect::<String>();
    let decoded = String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(encoded.as_bytes())
            .unwrap(),
    )
    .unwrap();
    assert!(decoded.contains("payload-marker"), "{decoded}");
    assert!(
        service
            .running_shell_transactions
            .get(&marker)
            .unwrap()
            .pending_input_payload
            .is_none()
    );
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies pending payload handoff uses a short start-marker deadline.
///
/// Non-stateful shell actions wait for an OSC start marker before sending the
/// encoded command body. If that marker is lost or the wrapper never reaches
/// the receiver loop, the transaction should time out quickly instead of
/// occupying the pane until the full command timeout expires.
#[test]
fn runtime_shell_transaction_pending_payload_uses_short_start_timer() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = "%1".to_string();
    let mut process = service
        .take_running_pane_process_for_async_owner(&pane_id)
        .unwrap();
    service.running_shell_transactions.insert(
        "marker-start".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "shell-1".to_string(),
            },
            pane_id: pane_id.clone(),
            command: "grep -n needle file.txt".to_string(),
            started_at_unix_ms: 1_000,
            timeout_ms: Some(10 * 60 * 1000),
            pending_input_payload: Some(b"payload\n".to_vec()),
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let timer = service
        .running_shell_transaction_timers()
        .into_iter()
        .find(|timer| timer.marker == "marker-start")
        .unwrap();

    assert_eq!(timer.timeout_ms, 30_000);

    service
        .observe_agent_shell_transaction_start(
            &pane_id,
            "marker-start",
            "turn-1",
            "agent-%1",
            &pane_id,
        )
        .unwrap();
    let timer = service
        .running_shell_transaction_timers()
        .into_iter()
        .find(|timer| timer.marker == "marker-start")
        .unwrap();
    assert_eq!(timer.timeout_ms, 10 * 60 * 1000);
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies runtime shell dispatch honors per-action shell timeouts.
///
/// The MAAP parser and semantic lowering preserve `timeout_ms`; the runtime
/// must carry that bound into the live shell transaction instead of replacing it
/// with the enclosing turn's full timeout budget.
#[test]
fn runtime_shell_command_dispatch_uses_action_timeout() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let mut process = service
        .take_running_pane_process_for_async_owner(&pane_id)
        .unwrap();
    mark_test_pane_ready(&mut service, &pane_id);
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-timeout","input":"run bounded grep"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.pending_agent_provider_tasks.remove("turn-1");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "shell action".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "shell-timeout".to_string(),
                    rationale: "run a bounded command".to_string(),
                    payload: crate::agent::AgentActionPayload::ShellCommand {
                        summary: "Run bounded grep".to_string(),
                        command: "grep -n needle file.txt".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: Some(1500),
                    },
                }],
                final_turn: false,
            }),
        },
    };

    service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    let transaction = service
        .running_shell_transactions
        .values()
        .find(|transaction| {
            matches!(
                transaction.kind,
                RunningShellTransactionKind::AgentAction { ref action_id }
                    if action_id == "shell-timeout"
            )
        })
        .unwrap();

    assert_eq!(transaction.timeout_ms, Some(1500));
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies timed-out shell actions receive bounded model recovery.
///
/// A file mutation can time out if the pane PTY stops accepting the generated
/// shell transaction. Treating timeout action results as non-recoverable leaves
/// the turn failed even though the model can choose a smaller or different
/// mutation strategy after seeing the timeout diagnostic.
#[test]
fn runtime_shell_action_timeout_queues_model_self_correction() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "write a file")
        .unwrap();
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.pending_agent_provider_tasks.remove(&turn.turn_id);

    let action = crate::agent::AgentAction {
        id: "patch-timeout".to_string(),
        rationale: "write a file through the pane shell".to_string(),
        payload: crate::agent::AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** Add File: note.txt\n+hello\n*** End Patch".to_string(),
            strip: None,
        },
    };
    let timed_out = crate::agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::TimedOut,
        "shell_timeout",
        "shell command timed out after 30000 ms",
    )
    .unwrap();
    let mut execution = crate::agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "write file timed out".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![action],
                final_turn: false,
            }),
        },
        latest_response_usage: Default::default(),
        action_results: vec![timed_out],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };

    let queued = service
        .queue_agent_failure_feedback_for_correction(
            &turn,
            &mut execution,
            "shell_timeout_recovery",
        )
        .unwrap();

    assert!(queued);
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == turn.turn_id)
    );
    let context = service.agent_turn_contexts.get(&turn.turn_id).unwrap();
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result patch-timeout apply_patch timed_out]")
            && block
                .content
                .contains("shell command timed out after 30000 ms")
    }));
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies an `apply_patch` validation failure is eligible for model
/// correction.
///
/// Malformed Mezzanine patch payloads are model-correctable input errors and
/// must not end the turn before the model sees the failed action result.
#[test]
fn runtime_apply_patch_invalid_params_queues_model_self_correction() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "patch the file")
        .unwrap();
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.pending_agent_provider_tasks.remove(&turn.turn_id);

    let action = crate::agent::AgentAction {
        id: "patch-invalid".to_string(),
        rationale: "apply an invalid patch".to_string(),
        payload: crate::agent::AgentActionPayload::ApplyPatch {
            patch: "*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch".to_string(),
            strip: None,
        },
    };

    let mut failed = crate::agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::Failed,
        "invalid_params",
        "apply_patch requires Mezzanine patch blocks starting with *** Begin Patch; use shell_command with git apply for raw unified diffs",
    )
    .unwrap();
    failed.structured_content_json = Some(
        serde_json::json!({
            "state": "dispatch_failed",
            "stage": "local_action_plan",
            "error": {
                "kind": "invalid_params",
                "message": "apply_patch requires Mezzanine patch blocks starting with *** Begin Patch; use shell_command with git apply for raw unified diffs"
            }
        })
        .to_string(),
    );
    let mut execution = crate::agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "invalid patch".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![action],
                final_turn: false,
            }),
        },
        latest_response_usage: Default::default(),
        action_results: vec![failed],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };

    let queued = service
        .queue_agent_failure_feedback_for_correction(
            &turn,
            &mut execution,
            "apply_patch_validation_failed",
        )
        .unwrap();

    assert!(queued);
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == turn.turn_id)
    );
    assert_eq!(
        service
            .agent_turn_failure_feedback_attempts
            .values()
            .copied()
            .collect::<Vec<_>>(),
        vec![1]
    );
    let context = service.agent_turn_contexts.get(&turn.turn_id).unwrap();
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result patch-invalid apply_patch failed]")
            && block.content.contains("Mezzanine patch blocks starting")
    }));
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::LocalMessage
            && block.content.contains("action failure feedback")
    }));
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: action failed; asking model to recover"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("Failed after"), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies pre-execution `apply_patch` transport failures are model
/// correctable.
///
/// A pane input write timeout means the runtime could not deliver the generated
/// write command, not that the user request is impossible. The model should
/// receive bounded correction feedback so it can retry with a smaller or
/// different file action instead of failing through immediately.
#[test]
fn runtime_apply_patch_pane_input_failure_queues_model_self_correction() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "write the file")
        .unwrap();
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.pending_agent_provider_tasks.remove(&turn.turn_id);

    let action = crate::agent::AgentAction {
        id: "patch-transport".to_string(),
        rationale: "write a source file".to_string(),
        payload: crate::agent::AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** Add File: src/generated.rs\n+content\n*** End Patch"
                .to_string(),
            strip: None,
        },
    };
    let failed = crate::agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::Failed,
        "pane_input_write_failed",
        "pane input write failed while sending shell action",
    )
    .unwrap();
    let mut execution = crate::agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "write transport failure".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![action],
                final_turn: false,
            }),
        },
        latest_response_usage: Default::default(),
        action_results: vec![failed],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };

    let queued = service
        .queue_agent_failure_feedback_for_correction(
            &turn,
            &mut execution,
            "apply_patch_transport_failed",
        )
        .unwrap();

    assert!(queued);
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == turn.turn_id)
    );
    let context = service.agent_turn_contexts.get(&turn.turn_id).unwrap();
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result patch-transport apply_patch failed]")
            && block.content.contains("pane_input_write_failed")
    }));
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies `apply_patch` hunk mismatches receive specific recovery guidance.
///
/// A generic "action failed" continuation is not enough for patch hunk
/// mismatches because replaying the same patch will deterministically fail.
/// The model should be steered to inspect the current file and generate a fresh
/// Mezzanine patch block instead.
#[test]
fn runtime_apply_patch_hunk_mismatch_recovery_guides_context_refresh() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "patch the file")
        .unwrap();
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.pending_agent_provider_tasks.remove(&turn.turn_id);

    let action = crate::agent::AgentAction {
        id: "patch-hunk".to_string(),
        rationale: "apply a source patch".to_string(),
        payload: crate::agent::AgentActionPayload::ApplyPatch {
            patch:
                "*** Begin Patch\n*** Update File: src/driver/mod.rs\n@@\n-old\n+new\n*** End Patch"
                    .to_string(),
            strip: None,
        },
    };
    let mut failed = crate::agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::Failed,
        "shell_command_failed",
        "shell command exited with status 1",
    )
    .unwrap();
    failed.structured_content_json = Some(
        serde_json::json!({
            "command": "\"$MEZ_PYTHON\" \"$MEZ_PATCH_SCRIPT\" \"$MEZ_PATCH\"",
            "terminal_observation": {
                "exit_code": 1,
                "combined_output_preview": "apply_patch: hunk did not match: src/driver/mod.rs\napply_patch: patch failed",
                "combined_output_bytes": 91,
                "output_truncated": false
            }
        })
        .to_string(),
    );
    let mut execution = crate::agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "hunk mismatch patch".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![action],
                final_turn: false,
            }),
        },
        latest_response_usage: Default::default(),
        action_results: vec![failed],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };

    let queued = service
        .queue_agent_failure_feedback_for_correction(
            &turn,
            &mut execution,
            "apply_patch_hunk_mismatch",
        )
        .unwrap();

    assert!(queued);
    let context = service.agent_turn_contexts.get(&turn.turn_id).unwrap();
    let feedback = context
        .blocks
        .iter()
        .find(|block| {
            block.source == ContextSourceKind::LocalMessage
                && block.label == "action failure feedback"
        })
        .expect("feedback block should be present");
    assert!(feedback.content.contains("max=5"), "{}", feedback.content);
    assert!(
        feedback.content.contains("Mutation-evidence rule"),
        "{}",
        feedback.content
    );
    assert!(
        feedback
            .content
            .contains("no successful mutation has occurred"),
        "{}",
        feedback.content
    );
    assert!(
        feedback
            .content
            .contains("Reads, git status, and git diff after a failed mutation"),
        "{}",
        feedback.content
    );
    assert!(
        feedback
            .content
            .contains("the current file/diff shows that state"),
        "{}",
        feedback.content
    );
    assert!(
        feedback
            .content
            .contains("Do not retry substantially the same patch"),
        "{}",
        feedback.content
    );
    assert!(
        feedback
            .content
            .contains("Next step: first inspect the affected path(s) with a bounded shell_command"),
        "{}",
        feedback.content
    );
    assert!(
        feedback.content.contains("reported line number(s)"),
        "{}",
        feedback.content
    );
    assert!(
        feedback
            .content
            .contains("not necessarily a stale-file condition"),
        "{}",
        feedback.content
    );
    assert!(
        feedback.content.contains("fresh Mezzanine"),
        "{}",
        feedback.content
    );
    assert!(
        feedback.content.contains("emit a smaller fresh Mezzanine"),
        "{}",
        feedback.content
    );
    assert!(
        feedback.content.contains("src/driver/mod.rs"),
        "{}",
        feedback.content
    );
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("apply_patch: hunk did not match: src/driver/mod.rs")
    }));
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: action failed; asking model to recover"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("(1/5, patch hunk mismatch)"),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies real `apply_patch` write-phase hunk failures enter model recovery.
///
/// `apply_patch` runs through a read transaction followed by a generated write
/// transaction. Direct recovery-unit tests do not prove the shell-transaction
/// observer routes write-phase hunk mismatches back into the correction loop,
/// so this covers the user-visible path that emits the final patch diagnostic.
#[test]
fn runtime_apply_patch_write_phase_hunk_mismatch_queues_model_recovery() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-patch-write-phase-recovery","input":"patch the file"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.pending_agent_provider_tasks.remove("turn-1");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "patch response".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "patch-write".to_string(),
                    rationale: "apply a source patch".to_string(),
                    payload: crate::agent::AgentActionPayload::ApplyPatch {
                        patch: "*** Begin Patch\n*** Update File: tests/standard_config_consumer_test.rs\n@@\n-old\n+new\n*** End Patch"
                            .to_string(),
                        strip: None,
                    },
                }],
                final_turn: false,
            }),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(service.running_shell_transactions.len(), 1);
    let marker = service
        .running_shell_transactions
        .keys()
        .next()
        .cloned()
        .unwrap();
    let transaction = service.running_shell_transactions.get_mut(&marker).unwrap();
    transaction.command = "# __MEZ_APPLY_PATCH_WRITE_PHASE__".to_string();
    transaction.observed_output_preview =
        "apply_patch: hunk did not match: tests/standard_config_consumer_test.rs\n\
         apply_patch: exact hunk context was not found in the current file"
            .to_string();
    transaction.observed_output_bytes = transaction.observed_output_preview.len();

    service
        .observe_agent_shell_transaction_start("%1", &marker, "turn-1", "agent-%1", "%1")
        .unwrap();
    service
        .observe_agent_shell_transaction_end("%1", &marker, "turn-1", "agent-%1", "%1", 1)
        .unwrap();

    assert_eq!(service.pending_agent_provider_tasks().len(), 1);
    assert!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .any(|turn| turn.turn_id == "turn-1" && turn.state == AgentTurnState::Running)
    );
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    let feedback = context
        .blocks
        .iter()
        .rev()
        .find(|block| {
            block.source == ContextSourceKind::LocalMessage
                && block.label == "action failure feedback"
        })
        .expect("feedback block should be present");
    assert!(
        feedback.content.contains("Apply-patch recovery"),
        "{}",
        feedback.content
    );
    assert!(
        feedback
            .content
            .contains("tests/standard_config_consumer_test.rs"),
        "{}",
        feedback.content
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: action failed; asking model to recover"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("(1/5, patch hunk mismatch)"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("recovery unavailable"), "{pane_text}");
    let copy_response = service
        .execute_agent_shell_command(&primary, "/copy-patches buffer failed-patches")
        .unwrap();
    assert!(
        copy_response.contains(r#""command":"copy-patches""#),
        "{copy_response}"
    );
    assert!(copy_response.contains("patches=written"), "{copy_response}");
    assert!(
        copy_response.contains("destination=buffer"),
        "{copy_response}"
    );
    let failed_patches = service.paste_buffers.get("failed-patches").unwrap();
    assert!(
        failed_patches.contains("patch 1: turn=turn-1 action=patch-write status=failed"),
        "{failed_patches}"
    );
    assert!(
        failed_patches
            .contains("apply_patch: hunk did not match: tests/standard_config_consumer_test.rs"),
        "{failed_patches}"
    );
    assert!(
        failed_patches.contains("*** Update File: tests/standard_config_consumer_test.rs"),
        "{failed_patches}"
    );
    assert!(failed_patches.contains("-old"), "{failed_patches}");
    assert!(failed_patches.contains("+new"), "{failed_patches}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies repeated identical `apply_patch` hunk mismatches share one bounded
/// recovery budget.
///
/// Provider wording and generated action ids can vary while the model repeats
/// the same bad patch. The retry key should therefore follow the failed action
/// signature and diagnostic rather than provider prose.
#[test]
fn runtime_apply_patch_hunk_mismatch_retry_key_ignores_provider_prose() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "patch the file")
        .unwrap();
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.pending_agent_provider_tasks.remove(&turn.turn_id);

    let build_execution = |raw_text: &str, action_id: &str| {
        let action = crate::agent::AgentAction {
            id: action_id.to_string(),
            rationale: "apply a source patch".to_string(),
            payload: crate::agent::AgentActionPayload::ApplyPatch {
                patch:
                    "*** Begin Patch\n*** Update File: src/main.rs\n@@\n-old\n+new\n*** End Patch"
                        .to_string(),
                strip: None,
            },
        };
        let mut failed = crate::agent::ActionResult::failed(
            &turn,
            &action,
            ActionStatus::Failed,
            "shell_command_failed",
            "shell command exited with status 1",
        )
        .unwrap();
        failed.structured_content_json = Some(
            serde_json::json!({
                "command": "apply_patch",
                "terminal_observation": {
                    "exit_code": 1,
                    "combined_output_preview": "apply_patch: hunk did not match: src/main.rs\napply_patch: patch failed",
                    "combined_output_bytes": 75,
                    "output_truncated": false
                }
            })
            .to_string(),
        );
        crate::agent::AgentTurnExecution {
            request: runtime_model_request_fixture(&turn.turn_id),
            response: crate::agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: raw_text.to_string(),
                usage: Default::default(),
                quota_usage: Default::default(),
                action_batch: Some(crate::agent::MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "test action batch rationale".to_string(),
                    turn_id: turn.turn_id.clone(),
                    agent_id: turn.agent_id.clone(),
                    actions: vec![action],
                    final_turn: false,
                }),
            },
            latest_response_usage: Default::default(),
            action_results: vec![failed],
            final_turn: false,
            terminal_state: AgentTurnState::Failed,
        }
    };

    let mut first_execution = build_execution("first provider wording", "patch-a");
    assert!(
        service
            .queue_agent_failure_feedback_for_correction(
                &turn,
                &mut first_execution,
                "apply_patch_hunk_mismatch",
            )
            .unwrap()
    );
    let mut second_execution = build_execution("different provider wording", "patch-b");
    assert!(
        service
            .queue_agent_failure_feedback_for_correction(
                &turn,
                &mut second_execution,
                "apply_patch_hunk_mismatch",
            )
            .unwrap()
    );

    assert_eq!(
        service
            .agent_turn_failure_feedback_attempts
            .values()
            .copied()
            .collect::<Vec<_>>(),
        vec![2]
    );
    let context = service.agent_turn_contexts.get(&turn.turn_id).unwrap();
    let feedback = context
        .blocks
        .iter()
        .rev()
        .find(|block| {
            block.source == ContextSourceKind::LocalMessage
                && block.label == "action failure feedback"
        })
        .expect("second feedback block should be present");
    assert!(
        feedback.content.contains("Repeated apply-patch recovery"),
        "{}",
        feedback.content
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies unsafe `apply_patch` paths receive CWD-relative recovery guidance.
///
/// Mezzanine patch headers are intentionally restricted to paths relative to
/// the pane current working directory. When a model emits an absolute path, the
/// corrective continuation should include the rejected path, the best-known CWD,
/// and a clear note that this restriction is specific to `apply_patch` headers.
#[test]
fn runtime_apply_patch_unsafe_path_recovery_guides_relative_headers() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.pane_current_working_directories.insert(
        "%1".to_string(),
        PathBuf::from("/home/neil/Documents/repos/chimera"),
    );
    let started = service
        .start_agent_prompt_turn("%1", "patch the file")
        .unwrap();
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.pending_agent_provider_tasks.remove(&turn.turn_id);

    let unsafe_path = "/home/neil/Documents/repos/chimera/src/conf/document.rs";
    let action = crate::agent::AgentAction {
        id: "patch-absolute".to_string(),
        rationale: "apply a source patch".to_string(),
        payload: crate::agent::AgentActionPayload::ApplyPatch {
            patch: format!(
                "*** Begin Patch\n*** Update File: {unsafe_path}\n@@\n-old\n+new\n*** End Patch"
            ),
            strip: None,
        },
    };
    let mut failed = crate::agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::Failed,
        "shell_command_failed",
        "shell command exited with status 1",
    )
    .unwrap();
    failed.structured_content_json = Some(
        serde_json::json!({
            "command": "\"$MEZ_PYTHON\" \"$MEZ_PATCH_SCRIPT\" \"$MEZ_PATCH\"",
            "terminal_observation": {
                "exit_code": 1,
                "combined_output_preview": format!("apply_patch: unsafe patch path: {unsafe_path}\n"),
                "combined_output_bytes": 96,
                "output_truncated": false
            }
        })
        .to_string(),
    );
    let mut execution = crate::agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "absolute path patch".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![action],
                final_turn: false,
            }),
        },
        latest_response_usage: Default::default(),
        action_results: vec![failed],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };

    let queued = service
        .queue_agent_failure_feedback_for_correction(
            &turn,
            &mut execution,
            "apply_patch_unsafe_path",
        )
        .unwrap();

    assert!(queued);
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == turn.turn_id)
    );
    let context = service.agent_turn_contexts.get(&turn.turn_id).unwrap();
    let feedback = context
        .blocks
        .iter()
        .find(|block| {
            block.source == ContextSourceKind::LocalMessage
                && block.label == "action failure feedback"
        })
        .expect("feedback block should be present");
    assert!(
        feedback.content.contains("unsafe patch path"),
        "{}",
        feedback.content
    );
    assert!(
        feedback.content.contains(unsafe_path),
        "{}",
        feedback.content
    );
    assert!(
        feedback
            .content
            .contains("Current pane working directory: /home/neil/Documents/repos/chimera"),
        "{}",
        feedback.content
    );
    assert!(
        feedback
            .content
            .contains("relative to the pane current working directory"),
        "{}",
        feedback.content
    );
    assert!(
        feedback.content.contains("`src/conf/document.rs`"),
        "{}",
        feedback.content
    );
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("apply_patch: unsafe patch path: /home/neil/Documents/repos/chimera/src/conf/document.rs")
    }));
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies agent-authored heredoc shell commands fail before pane dispatch.
///
/// MAAP validation rejects heredocs before runtime execution. This protects the
/// pane from receiving an unterminated shell construct and ensures that a fixed
/// provider response surfaces a repairable diagnostic instead of attempting to
/// execute the invalid command.
#[test]
fn runtime_shell_command_heredoc_is_rejected_before_pane_dispatch() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-heredoc-feedback","input":"write a file"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.pending_agent_provider_tasks.remove("turn-1");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "heredoc shell".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "shell-heredoc".to_string(),
                    rationale: "write a file with a heredoc".to_string(),
                    payload: crate::agent::AgentActionPayload::ShellCommand {
                        summary: "Write a file with a heredoc".to_string(),
                        command: "cat > /tmp/mez-heredoc.rs <<'EOF'\nfn main() {}\nEOF".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert!(service.running_shell_transactions.is_empty());
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert!(
        execution
            .response
            .raw_text
            .contains("maap_validation_error"),
        "{}",
        execution.response.raw_text
    );
    assert!(
        execution
            .response
            .raw_text
            .contains("heredoc redirection is disabled"),
        "{}",
        execution.response.raw_text
    );
    assert!(
        execution.response.raw_text.contains("apply_patch"),
        "{}",
        execution.response.raw_text
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!pane_text.contains("MEZ_COMMAND_"), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies network research failure feedback is scoped per action batch and
/// that mixed successful results are sent back with the failures.
///
/// Broken documentation links and 404s are normal web-research evidence. A
/// previous single turn-wide failure-feedback budget let an earlier bad URL
/// consume budget for a later batch of different URLs. The network budget should
/// instead be per batch and controlled by the configured action-failure limit.
#[test]
fn runtime_network_action_failures_get_additional_model_feedback_budget() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-network-failure-feedback","input":"research docs"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.pending_agent_provider_tasks.remove("turn-1");
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .cloned()
        .unwrap();
    let success_action = crate::agent::AgentAction {
        id: "fetch-good".to_string(),
        rationale: "capture one usable source".to_string(),
        payload: crate::agent::AgentActionPayload::FetchUrl {
            url: "https://example.test/ok".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let failed_action = crate::agent::AgentAction {
        id: "fetch-missing".to_string(),
        rationale: "try a moved source".to_string(),
        payload: crate::agent::AgentActionPayload::FetchUrl {
            url: "https://example.test/missing".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let mut failed_result = crate::agent::ActionResult::failed(
        &turn,
        &failed_action,
        ActionStatus::Failed,
        "network_http_error",
        "network request returned HTTP 404",
    )
    .unwrap();
    failed_result.structured_content_json = Some(
        serde_json::json!({
            "kind": "fetch_url",
            "response": {
                "url": "https://example.test/missing",
                "status_code": 404
            }
        })
        .to_string(),
    );
    let mut execution = crate::agent::AgentTurnExecution {
        request: crate::agent::ModelRequest {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            reasoning_effort: None,
            prompt_cache_retention: None,
            max_output_tokens: None,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-%1".to_string(),
            available_mcp_tools: Vec::new(),
            interaction_kind: crate::agent::ModelInteractionKind::ActionExecution,
            allowed_actions: crate::agent::AllowedActionSet::for_capability(
                crate::agent::AgentCapability::NetworkFetch,
            ),
            messages: vec![crate::agent::ModelMessage {
                role: crate::agent::ModelMessageRole::User,
                source: ContextSourceKind::UserInstruction,
                content: "research docs".to_string(),
            }],
        },
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "mixed network fetches".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![success_action.clone(), failed_action.clone()],
                final_turn: false,
            }),
        },
        latest_response_usage: Default::default(),
        action_results: vec![
            crate::agent::ActionResult::succeeded(
                &turn,
                &success_action,
                vec!["usable source body".to_string()],
                None,
            ),
            failed_result,
        ],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };
    let previous_key = "turn-1:previous-network-batch".to_string();
    service
        .agent_turn_failure_feedback_attempts
        .insert(previous_key.clone(), 3);
    service
        .present_agent_action_outcomes_to_terminal_buffer(&turn.pane_id, &execution)
        .unwrap();
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent warning: URL fetch failed (HTTP 404)"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("model received the response detail")
            && pane_text.contains("for recovery"),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("https://example.test/missing"),
        "{pane_text}"
    );

    let queued = service
        .queue_agent_failure_feedback_for_correction(
            &turn,
            &mut execution,
            "network_research_failed_action",
        )
        .unwrap();

    assert!(queued);
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(
        service
            .agent_turn_failure_feedback_attempts
            .get(&previous_key)
            .copied(),
        Some(3)
    );
    let mut attempt_values = service
        .agent_turn_failure_feedback_attempts
        .values()
        .copied()
        .collect::<Vec<_>>();
    attempt_values.sort_unstable();
    assert_eq!(attempt_values, vec![1, 3]);
    assert!(service.pending_agent_provider_tasks.contains("turn-1"));
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result fetch-good fetch_url succeeded]")
            && block.content.contains("usable source body")
    }));
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result fetch-missing fetch_url failed]")
            && block.content.contains("network request returned HTTP 404")
    }));
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::LocalMessage && block.content.contains("attempt=1 max=5")
    }));
    assert!(context.blocks.iter().all(|block| {
        block.source != ContextSourceKind::LocalMessage
            || !block.content.contains("Mutation-evidence rule")
    }));
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies failure-feedback accounting is per failed action, not per batch.
///
/// A single model response may contain multiple correctable action failures.
/// Each failed action should receive its own bounded retry counter so one bad
/// action does not amortize away another action's correction opportunity.
#[test]
fn runtime_action_failure_retry_budget_is_per_failed_action() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-per-action-retry","input":"research docs"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.pending_agent_provider_tasks.remove("turn-1");
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .cloned()
        .unwrap();
    let first_action = crate::agent::AgentAction {
        id: "fetch-first".to_string(),
        rationale: "try first source".to_string(),
        payload: crate::agent::AgentActionPayload::FetchUrl {
            url: "https://example.test/first".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let second_action = crate::agent::AgentAction {
        id: "fetch-second".to_string(),
        rationale: "try second source".to_string(),
        payload: crate::agent::AgentActionPayload::FetchUrl {
            url: "https://example.test/second".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let first_result = crate::agent::ActionResult::failed(
        &turn,
        &first_action,
        ActionStatus::Failed,
        "network_http_error",
        "network request returned HTTP 404 for first source",
    )
    .unwrap();
    let second_result = crate::agent::ActionResult::failed(
        &turn,
        &second_action,
        ActionStatus::Failed,
        "network_http_error",
        "network request returned HTTP 404 for second source",
    )
    .unwrap();
    let mut execution = crate::agent::AgentTurnExecution {
        request: runtime_model_request_fixture("turn-1"),
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "two failed network fetches".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![first_action, second_action],
                final_turn: false,
            }),
        },
        latest_response_usage: Default::default(),
        action_results: vec![first_result, second_result],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };

    let queued = service
        .queue_agent_failure_feedback_for_correction(
            &turn,
            &mut execution,
            "network_research_failed_actions",
        )
        .unwrap();

    assert!(queued);
    let mut attempt_values = service
        .agent_turn_failure_feedback_attempts
        .values()
        .copied()
        .collect::<Vec<_>>();
    attempt_values.sort_unstable();
    assert_eq!(attempt_values, vec![1, 1]);
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that intentionally terminal model actions do not use the automatic
/// failure-feedback path. Cancellations and denials represent user or policy
/// boundaries rather than correctable execution evidence, so they must end the
/// turn without queuing another provider request.
#[test]
fn runtime_cancelled_action_does_not_queue_failure_feedback() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-cancel-no-feedback","input":"stop"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.pending_agent_provider_tasks.remove("turn-1");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "abort".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "abort-1".to_string(),
                    rationale: "abort the turn".to_string(),
                    payload: crate::agent::AgentActionPayload::Abort {
                        reason: "cannot continue".to_string(),
                    },
                }],
                final_turn: true,
            }),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert!(service.agent_turn_failure_feedback_attempts.is_empty());
    assert!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .any(|turn| turn.turn_id == "turn-1" && turn.state == AgentTurnState::Failed)
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies unrecovered `apply_patch` failures render their captured terminal
/// diagnostic when the turn is actually ending failed.
///
/// While the model still has a recovery attempt, normal logging does not need
/// to show the patch stderr/stdout. Once recovery is unavailable or exhausted,
/// the user needs enough final context to understand why the patch action
/// failed, so the renderer should surface the bounded terminal observation
/// before the failed-turn footer.
#[test]
fn runtime_unrecovered_apply_patch_failure_logs_terminal_observation() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-unrecovered-patch-failure","input":"patch the file"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .cloned()
        .expect("started turn should be recorded");
    let action = crate::agent::AgentAction {
        id: "patch-fail".to_string(),
        rationale: "apply a source patch".to_string(),
        payload: crate::agent::AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch"
                .to_string(),
            strip: None,
        },
    };
    let mut result = crate::agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::Failed,
        "shell_command_failed",
        "shell command exited with status 1",
    )
    .unwrap();
    result.structured_content_json = Some(
        serde_json::json!({
            "kind": "apply_patch",
            "terminal_observation": {
                "combined_output_preview": "\n\n∙ MEZ_PATCH=$(mktemp) || exit 1\n∙ printf %s '*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch' > \"$MEZ_PATCH\"\n∙ \"$MEZ_PYTHON\" \"$MEZ_PATCH_SCRIPT\" \"$MEZ_PATCH\"\nMEZ_RESTORE_NOUNSET_NOW=$MEZ_RESTORE_NOUNSET\napply_patch: hunk did not match: src/lib.rs\napply_patch: patch failed\n",
                "combined_output_bytes": 298,
                "output_truncated": false
            }
        })
        .to_string(),
    );
    let execution = crate::agent::AgentTurnExecution {
        request: runtime_model_request_fixture("turn-1"),
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "failed patch".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![action],
                final_turn: true,
            }),
        },
        latest_response_usage: Default::default(),
        action_results: vec![result],
        final_turn: true,
        terminal_state: AgentTurnState::Failed,
    };
    service
        .agent_turn_executions
        .insert("turn-1".to_string(), execution);

    service
        .finish_agent_turn("%1", "turn-1", AgentTurnState::Failed)
        .unwrap();

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("failed; recovery unavailable: correction budget remained"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("apply_patch: hunk did not match: src/lib.rs"),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("MEZ_RESTORE_NOUNSET_NOW"),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("[mez: failure output truncated for pane display]"),
        "{pane_text}"
    );
    assert!(pane_text.contains("Failed after"), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies unrecovered failures explain when recovery is unavailable because
/// a sibling action has not settled.
///
/// The runtime cannot feed a partial batch back to the model without risking a
/// correction prompt that ignores still-running or blocked actions. The final
/// failure line should make that blocker explicit instead of using a bare
/// "recovery unavailable" suffix.
#[test]
fn runtime_unrecovered_failure_with_pending_sibling_explains_blocker() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "patch and inspect")
        .unwrap();
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.pending_agent_provider_tasks.remove(&turn.turn_id);

    let patch_action = crate::agent::AgentAction {
        id: "patch-fail".to_string(),
        rationale: "apply a source patch".to_string(),
        payload: crate::agent::AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch"
                .to_string(),
            strip: None,
        },
    };
    let read_action = crate::agent::AgentAction {
        id: "read-pending".to_string(),
        rationale: "read the target file".to_string(),
        payload: crate::agent::AgentActionPayload::ShellCommand {
            summary: "Read the target file".to_string(),
            command: "sed -n '1,120p' src/lib.rs".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    let mut failed = crate::agent::ActionResult::failed(
        &turn,
        &patch_action,
        ActionStatus::Failed,
        "shell_command_failed",
        "shell command exited with status 1",
    )
    .unwrap();
    failed.structured_content_json = Some(
        serde_json::json!({
            "kind": "apply_patch",
            "terminal_observation": {
                "combined_output_preview": "apply_patch: hunk did not match: src/lib.rs",
                "combined_output_bytes": 44,
                "output_truncated": false
            }
        })
        .to_string(),
    );
    let pending = crate::agent::ActionResult::running(
        &turn,
        &read_action,
        vec!["local action accepted for pane execution".to_string()],
        None,
    );
    let execution = crate::agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "partial batch".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![patch_action, read_action],
                final_turn: false,
            }),
        },
        latest_response_usage: Default::default(),
        action_results: vec![failed, pending],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };
    service
        .agent_turn_executions
        .insert(turn.turn_id.clone(), execution);

    service
        .finish_agent_turn("%1", &turn.turn_id, AgentTurnState::Failed)
        .unwrap();

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("recovery unavailable: action result(s) are still pend"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("read-pending shell_command running no_error_code"),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies unrecovered failures explain when the failed result is outside the
/// model-correction path.
///
/// Policy/user-boundary outcomes must not be retried by the model. The final
/// failure line should still identify the non-correctable result so the user
/// can distinguish that boundary from a missing retry loop.
#[test]
fn runtime_unrecovered_non_correctable_failure_explains_boundary() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "write the file")
        .unwrap();
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.pending_agent_provider_tasks.remove(&turn.turn_id);

    let action = crate::agent::AgentAction {
        id: "patch-denied".to_string(),
        rationale: "write a source file".to_string(),
        payload: crate::agent::AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch"
                .to_string(),
            strip: None,
        },
    };
    let denied = crate::agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::Denied,
        "approval_denied",
        "user denied the action",
    )
    .unwrap();
    let execution = crate::agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "denied write".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![action],
                final_turn: false,
            }),
        },
        latest_response_usage: Default::default(),
        action_results: vec![denied],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };
    service
        .agent_turn_executions
        .insert(turn.turn_id.clone(), execution);

    service
        .finish_agent_turn("%1", &turn.turn_id, AgentTurnState::Failed)
        .unwrap();

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("recovery unavailable: no model-correctable"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("patch-denied apply_patch denied"),
        "{pane_text}"
    );
    assert!(pane_text.contains("approval_denied"), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies unrecovered `apply_patch` failures do not expose shell-wrapper
/// fragments when no actionable diagnostic survived capture.
///
/// Some failed patch commands can echo a partially quoted generated command as
/// isolated glyphs or words after the shell wrapper has already been stripped.
/// Those fragments are confusing to users and do not help model recovery, so a
/// final failed turn should prefer a concise generic diagnostic when no real
/// `apply_patch:` or error line is available.
#[test]
fn runtime_unrecovered_apply_patch_failure_uses_generic_line_for_fragments() {
    let action = crate::agent::AgentAction {
        id: "patch-fragment".to_string(),
        rationale: "apply a source patch".to_string(),
        payload: crate::agent::AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch"
                .to_string(),
            strip: None,
        },
    };
    let lines = super::agent::runtime_unrecovered_failure_output_lines(
        &action,
        "\n∙\nb\ngal(&mut\ncomma\nd\nS\ne\nu\nl\nE\nR\nMEZ_RESTORE_NOUNSET_NOW=$MEZ_RESTORE_NOUNSET\n",
    );

    assert_eq!(
        lines,
        vec![
            "apply_patch failed without an actionable patch diagnostic. Next step: inspect the current target file with a bounded shell_command, then retry with a smaller fresh Mezzanine *** Begin Patch block."
                .to_string()
        ]
    );
}
