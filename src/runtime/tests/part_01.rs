// Regression coverage for the runtime tests subsystem.
//
// These tests describe the behavior protected by the repository
// specification and workflow guidance. Keeping the scenarios documented
// makes failures easier to map back to the user-visible contract.

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
use crate::session::{Session, SessionState};
use crate::snapshot::SnapshotRepository;
use crate::subagent::SubagentSpawnRequest;
use crate::terminal::{
    AttachedTerminalClientStepPlan, ClientViewRole, CopyPosition, DEFAULT_PANE_TERM, HostClipboard,
    MouseAction, MuxAction, PaneAgentStatusField, PaneFocusDirection, TerminalClientLoopAction,
    TerminalClientLoopConfig, TerminalColor, TerminalOscEvent, TerminalScreen, TerminalStyledLine,
    UI_COLOR_SLOT_NAMES,
};
use crate::test_support::runtime::{RuntimeServiceFixture, SessionFixture};
use crate::transcript::{AgentTranscriptStore, TranscriptEntry, TranscriptRole};
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
    SessionFixture::new().build()
}

/// Runs the test runtime service operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn test_runtime_service() -> RuntimeSessionService {
    RuntimeServiceFixture::new().build()
}

/// Runs the test runtime service with size operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn test_runtime_service_with_size(size: Size) -> RuntimeSessionService {
    RuntimeServiceFixture::new().size(size).build()
}

/// Verifies recent message-log detail rows wrap to the pane width with an
/// indented continuation row.
///
/// The `show-messages` command renders diagnostics and lifecycle events in a
/// modal display. Long payloads should stay readable in narrow panes instead
/// of depending on host-terminal soft wrapping, and continuation rows should be
/// visually tied to the original log line.
#[test]
fn runtime_show_messages_wraps_logged_rows_with_indented_continuations() {
    let mut service = test_runtime_service_with_size(Size::new(48, 24).unwrap());
    service
        .append_runtime_diagnostic_event(
            "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu".to_string(),
        )
        .unwrap();

    let body = super::commands_support::runtime_show_messages_display(&service);
    let detail_lines = body.lines().skip(1).collect::<Vec<_>>();

    assert!(
        detail_lines.iter().any(|line| line.starts_with("    ")),
        "expected an indented continuation row in {body:?}"
    );
    assert!(
        detail_lines.iter().all(|line| UnicodeWidthStr::width(*line) <= 48),
        "message rows should fit the pane width: {body:?}"
    );
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

/// Verifies terminal snapshot commands use the live runtime snapshot repository.
///
/// The command prompt should no longer return command-layer placeholders for
/// `snapshot-session` or `resume-session` when a daemon has configured snapshot
/// storage. This protects the bridge from parsed colon commands to the same
/// runtime control paths used by JSON-RPC snapshot clients.
#[test]
fn runtime_terminal_snapshot_commands_create_and_resume_snapshots() {
    let root = temp_root("terminal-snapshot-commands");
    let snapshots = SnapshotRepository::new(root.join("snapshots"));
    let mut service = test_runtime_service();
    service.set_snapshot_repository(snapshots);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let create = service
        .execute_terminal_command(&primary, "snapshot-session --name checkpoint")
        .unwrap();
    assert!(create.contains(r#""command":"snapshot-session""#), "{create}");
    assert!(create.contains(r#""kind":"display""#), "{create}");
    assert!(create.contains(r#"\"snapshot\""#), "{create}");
    assert!(create.contains(r#"\"name\":\"checkpoint\""#), "{create}");

    let resume = service
        .execute_terminal_command(&primary, "resume-session --latest")
        .unwrap();
    assert!(resume.contains(r#""command":"resume-session""#), "{resume}");
    assert!(resume.contains(r#"\"resumed\":true"#), "{resume}");
    assert!(resume.contains(r#"\"primary_client_id\":"#), "{resume}");
    assert!(resume.contains(r#"\"restarted_panes\":0"#), "{resume}");

    let _ = fs::remove_dir_all(root);
}

/// Verifies unscoped terminal snapshot resume selects the newest restorable snapshot.
///
/// A user-visible `:resume-session --latest` command should be able to restore
/// the snapshot most recently created by `:snapshot-session` even when the live
/// daemon has a different session id after restart. Scoping `--latest` to the
/// current session id made the command unable to find persisted snapshots from
/// previous daemon sessions, so this regression uses two runtime services that
/// share one repository root.
#[test]
fn runtime_terminal_snapshot_resume_latest_uses_repository_latest_across_sessions() {
    let root = temp_root("terminal-snapshot-latest-cross-session");
    let snapshots = SnapshotRepository::new(root.join("snapshots"));
    let mut creating_service = test_runtime_service();
    creating_service.set_snapshot_repository(snapshots.clone());
    let creating_primary = creating_service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let create = creating_service
        .execute_terminal_command(&creating_primary, "snapshot-session --name restart-point")
        .unwrap();
    assert!(create.contains(r#"\"name\":\"restart-point\""#), "{create}");

    let mut resuming_service = test_runtime_service();
    resuming_service.set_snapshot_repository(snapshots);
    let resuming_primary = resuming_service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let resume = resuming_service
        .execute_terminal_command(&resuming_primary, "resume-session --latest")
        .unwrap();
    assert!(resume.contains(r#"\"resumed\":true"#), "{resume}");
    assert!(resume.contains(r#"\"primary_client_id\":"#), "{resume}");

    let _ = fs::remove_dir_all(root);
}

/// Verifies `:resume-session --latest` revives a detached snapshot into a live
/// running session before restored pane restart begins.
///
/// Snapshot payloads preserve detached lifecycle state so users can resume a
/// saved detached daemon later. The live resume path must still mark the
/// restored session running before it restarts panes, otherwise the hierarchy
/// installs and the first restart step crashes on the live-session guard.
#[test]
fn runtime_terminal_snapshot_resume_latest_revives_detached_snapshot_session() {
    let root = temp_root("terminal-snapshot-resume-detached-state");
    let snapshots = SnapshotRepository::new(root.join("snapshots"));
    let mut creating_service = test_runtime_service();
    creating_service.set_snapshot_repository(snapshots.clone());
    let creating_primary = creating_service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let create = creating_service
        .execute_terminal_command(&creating_primary, "snapshot-session --name detached-restart")
        .unwrap();
    assert!(create.contains(r#"\"name\":\"detached-restart\""#), "{create}");

    creating_service
        .detach_primary(&creating_primary, Size::new(80, 24).unwrap())
        .unwrap();

    let mut resuming_service = test_runtime_service();
    resuming_service.set_snapshot_repository(snapshots);
    let resuming_primary = resuming_service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let resume = resuming_service
        .execute_terminal_command(&resuming_primary, "resume-session --latest")
        .unwrap();
    assert!(resume.contains(r#"\"resumed\":true"#), "{resume}");
    assert!(resume.contains(r#"\"primary_client_id\":"#), "{resume}");
    assert_eq!(resuming_service.session.state, SessionState::Running);

    let _ = fs::remove_dir_all(root);
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
        thought: None,
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
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![action],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
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
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![action],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
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
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![action],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
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
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: None,
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
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
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: Vec::new(),
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
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
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(runtime_complete_batch(request.turn_id.clone())),
            provider_transcript_events: Vec::new(),
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
            latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(crate::agent::MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "test action batch rationale".to_string(),
            thought: None,
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
        provider_transcript_events: Vec::new(),
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
            latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(crate::agent::MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "test action batch rationale".to_string(),
            thought: None,
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
        provider_transcript_events: Vec::new(),
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
        thinking_enabled: None,
        latency_preference: None,
        prompt_cache_retention: None,
        max_output_tokens: None,
        temperature: None,
        stop: None,
        prompt_cache_session_id: None,
                prompt_cache_lineage_id: None,
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
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(runtime_complete_batch(request.turn_id.clone())),
            provider_transcript_events: Vec::new(),
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
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(runtime_complete_batch(request.turn_id.clone())),
            provider_transcript_events: Vec::new(),
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
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(runtime_complete_batch(request.turn_id.clone())),
            provider_transcript_events: Vec::new(),
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
                usage: crate::agent::ModelTokenUsage {
                    input_tokens: 90,
                    output_tokens: 10,
                    reasoning_tokens: 3,
                    cached_input_tokens: Some(30),
                },
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: None,
                provider_transcript_events: Vec::new(),
            });
        }
        let mut response = runtime_say_response(
            &request.turn_id,
            "auto-sized response",
            true,
        );
        response.usage = crate::agent::ModelTokenUsage {
            input_tokens: 150,
            output_tokens: 40,
            reasoning_tokens: 12,
            cached_input_tokens: Some(50),
        };
        Ok(response)
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

/// Waits until host process metadata reports that the pane primary shell owns
/// the foreground process group.
///
/// # Parameters
/// - `service`: The runtime service whose pane process metadata is queried.
/// - `pane_id`: The pane expected to have an idle foreground shell.
fn wait_until_primary_shell_foreground(service: &mut RuntimeSessionService, pane_id: &str) {
    for _ in 0..50 {
        if service.pane_foreground_primary_shell_state(pane_id) == Some(true) {
            return;
        }
        let _ = service.poll_pane_outputs(4096).unwrap();
        wait_for_pane_process_activity(service, pane_id, Duration::from_millis(10));
    }
    panic!("pane primary shell did not become foreground before test timeout");
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
    let output = vec![b'x'; 300_000];

    service.record_running_shell_transaction_output("%1", &output);

    let transaction = service.running_shell_transactions.get("marker-1").unwrap();
    assert_eq!(transaction.observed_output_bytes, 300_001);
    assert_eq!(transaction.observed_output_preview.len(), 262_144);
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
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
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
            provider_transcript_events: Vec::new(),
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
    assert!(pane_text.contains("--- /dev/null"), "{pane_text}");
    assert!(pane_text.contains("+++ ") && pane_text.contains("note.txt"), "{pane_text}");
    assert!(pane_text.contains("@@ -0,0 +1,2 @@"), "{pane_text}");
    assert!(pane_text.contains("            1 +alpha"), "{pane_text}");
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
        .find(|line| line.text.contains("            1 +alpha"))
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
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
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
            provider_transcript_events: Vec::new(),
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
    let diff_index = pane_text.find("@@ -0,0 +1,2 @@").unwrap_or(usize::MAX);
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

/// Verifies MCP tool calls log a compact normal-mode action line with the
/// invoked server, tool, and compact JSON arguments.
///
/// MCP actions do not execute through the pane shell, but operators still need
/// a first-class execution row that makes the tool target and arguments visible
/// without waiting for verbose mode or failure output.
#[test]
fn runtime_mcp_call_logs_styled_action_line_in_normal_mode() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let action = crate::agent::AgentAction {
        id: "mcp-1".to_string(),
        rationale: String::new(),
        payload: crate::agent::AgentActionPayload::McpCall {
            server: "github".to_string(),
            tool: "search_issues".to_string(),
            arguments_json: r#"{ "query": "prompt cache", "limit": 5 }"#.to_string(),
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
    assert!(pane_text.contains("agent: mcp call: github/search_issues"));
    assert!(pane_text.contains("args={"));
    assert!(pane_text.contains(r#""query":"prompt cache""#));
    assert!(pane_text.contains(r#""limit":5"#));
    let action_line = styled_lines
        .iter()
        .find(|line| line.text.contains("agent: mcp call:"))
        .unwrap();
    let theme = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .ui_theme;
    let prefix_column = display_column_for_fragment(&action_line.text, "agent:");
    let action_column = display_column_for_fragment(&action_line.text, "mcp call");
    let argument_column = display_column_for_fragment(&action_line.text, "github/search_issues");
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

/// Verifies skill catalog lookup logs a compact normal-mode action line.
///
/// Non-effecting skill discovery still needs the same execution visibility as
/// other runtime actions so the pane shows that the agent performed a catalog
/// lookup instead of silently continuing provider turns.
#[test]
fn runtime_skill_lookup_logs_styled_action_line_in_normal_mode() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let action = crate::agent::AgentAction {
        id: "skill-catalog-1".to_string(),
        rationale: String::new(),
        payload: crate::agent::AgentActionPayload::RequestSkills,
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
        .find(|line| line.text.contains("agent: skill lookup:"))
        .unwrap();
    assert!(
        action_line
            .text
            .contains("agent: skill lookup: available skills"),
        "{action_line:?}"
    );
    let theme = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .ui_theme;
    let prefix_column = display_column_for_fragment(&action_line.text, "agent:");
    let action_column = display_column_for_fragment(&action_line.text, "skill lookup");
    let prefix_rendition = styled_line_rendition_at(action_line, prefix_column);
    let action_rendition = styled_line_rendition_at(action_line, action_column);
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
}

/// Verifies skill loading logs the selected skill name and appended task
/// context in a compact normal-mode action line.
///
/// Loaded skills can materially change the next provider step, so the pane
/// should expose both the invoked skill and the extra context that shaped the
/// load request.
#[test]
fn runtime_skill_load_logs_styled_action_line_in_normal_mode() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let action = crate::agent::AgentAction {
        id: "skill-load-1".to_string(),
        rationale: String::new(),
        payload: crate::agent::AgentActionPayload::CallSkill {
            name: "review".to_string(),
            additional_context: Some("focus on context replay churn".to_string()),
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
    assert!(pane_text.contains("agent: skill load: review"));
    assert!(pane_text.contains("context=focus on context replay churn"));
    let action_line = styled_lines
        .iter()
        .find(|line| line.text.contains("agent: skill load:"))
        .unwrap();
    let theme = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .ui_theme;
    let prefix_column = display_column_for_fragment(&action_line.text, "agent:");
    let action_column = display_column_for_fragment(&action_line.text, "skill load");
    let argument_column = display_column_for_fragment(&action_line.text, "review");
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
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "set every terminal theme color".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions,
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
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
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "change the requested live configuration".to_string(),
                thought: None,
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
            provider_transcript_events: Vec::new(),
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

/// Verifies subagents inherit the live parent pane routing decision.
///
/// Auto-reasoning is a pane-local agent behavior, not just a global default.
/// Child agents should continue with the parent pane's effective setting so a
/// user does not have to re-toggle it after spawning helpers.
#[test]
fn runtime_subagent_routing_inherits_parent_pane_setting() {
    let mut service = test_runtime_service();
    service.agent_routing = false;
    service
        .agent_routing_overrides
        .insert("%1".to_string(), true);

    assert_eq!(
        service.inherited_routing_for_child_agent("agent-%1"),
        Some(true)
    );

    service.agent_routing_overrides.remove("%1");
    service.agent_routing = true;
    assert_eq!(
        service.inherited_routing_for_child_agent("agent-%1"),
        Some(true)
    );
}

/// Verifies subagents inherit the live parent pane auto-sizing configuration.
///
/// Auto-sizing uses pane-local model profile names for router and bucket
/// selection. Child agents must inherit that configuration with the parent
/// model profile so a DeepSeek parent pane does not spawn children that use the
/// global OpenAI sizing defaults.
#[test]
fn runtime_subagent_auto_sizing_inherits_parent_pane_setting() {
    let mut service = test_runtime_service();
    let mut parent_auto_sizing = service.agent_auto_sizing.clone();
    parent_auto_sizing.router_model_profile = "deepseek-fast".to_string();
    parent_auto_sizing.small_model_profile = "deepseek-fast".to_string();
    parent_auto_sizing.medium_model_profile = "deepseek-default".to_string();
    parent_auto_sizing.large_model_profile = "deepseek-default".to_string();
    parent_auto_sizing.allowed_reasoning_efforts = vec!["high".to_string(), "xhigh".to_string()];
    service
        .agent_auto_sizing_overrides
        .insert("%1".to_string(), parent_auto_sizing.clone());

    assert_eq!(
        service.inherited_auto_sizing_for_child_agent("agent-%1"),
        Some(parent_auto_sizing)
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
                output_line_style_spans: Vec::new(),
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
                output_line_style_spans: Vec::new(),
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
        output.contains("provider_error=none"),
        "{output}"
    );
    assert!(service.provider_model_catalog_cache.contains_key("openai"));
}
/// Verifies that the async terminal command path refreshes provider metadata
/// through the live-pane runtime entrypoint instead of relying on a nested
/// sync-to-async bridge inside command dispatch.
#[tokio::test(flavor = "multi_thread")]
async fn runtime_terminal_refresh_provider_info_async_command_refreshes_provider_metadata() {
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
        output.contains("provider_error=none"),
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
