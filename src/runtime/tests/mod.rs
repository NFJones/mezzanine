//! Purpose-named runtime regression tests.
//!
//! Shared fixtures remain at parent scope while bounded child modules own
//! behavior-specific tests. Numbered chunks and flattened includes are prohibited.

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
    RuntimeSessionService, RuntimeSideEffect, RuntimeSubagentLineage, RuntimeSubagentPlacement,
    SenderIdentity, SocketDirectorySource, SplitDirection, SubagentWaitPolicy, TrustDecision,
    UnixStream, authorize_unix_peer, authorize_unix_peer_uid,
    auxiliary_socket_path_for_control_socket, bind_control_socket, default_socket_directory,
    effective_uid, ensure_private_socket_directory, fs, json_escape, pane_environment,
    pane_environment_with_term, prune_stale_socket_files_in_directory, runtime_cooperation_mode,
    runtime_hook_event_for_lifecycle, runtime_hook_event_name, runtime_marker_for_action,
    socket_path_for_name,
};
use crate::MezError;
use crate::agent::{AgentLogLevel, AgentShellCommandOutcome};
use crate::scheduler::{ScheduledWork, ScheduledWorkKind};
use crate::snapshot::SnapshotRepository;
use crate::subagent::SubagentSpawnRequest;
use crate::terminal::{
    AttachedTerminalClientStepPlan, ClientViewRole, CopyPosition, DEFAULT_PANE_TERM, HostClipboard,
    MouseAction, PaneAgentStatusField, TerminalClientLoopAction, TerminalClientLoopConfig,
    TerminalColor, TerminalOscEvent, TerminalScreen, TerminalStyledLine, UI_COLOR_SLOT_NAMES,
};
use crate::test_support::runtime::{RuntimeServiceFixture, SessionFixture};
use crate::transcript::{AgentTranscriptStore, TranscriptEntry, TranscriptRole};
use base64::Engine;
use mez_mux::input::{MuxAction, PaneFocusDirection};
use mez_mux::session::{Session, SessionState};
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
) -> mez_terminal::GraphicRendition {
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
use crate::registry::{RegistrySessionState, SessionRegistry};
use crate::shell::{ResolvedShell, ShellSource};
use mez_mux::layout::Size;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

/// Exposes pane-input fields from canonical runtime side effects in assertions.
trait RuntimeSideEffectTestExt {
    /// Returns pane id, bytes, and priority for a pane-input side effect.
    fn pane_input_parts(&self) -> (&str, &[u8], bool);
}

impl RuntimeSideEffectTestExt for RuntimeSideEffect {
    fn pane_input_parts(&self) -> (&str, &[u8], bool) {
        match self {
            RuntimeSideEffect::WritePaneInput { pane_id, bytes } => (pane_id, bytes, false),
            RuntimeSideEffect::WritePaneInputPriority { pane_id, bytes } => (pane_id, bytes, true),
            effect => panic!("expected pane-input side effect, got {effect:?}"),
        }
    }
}

/// Returns only pane-input effects from a transition effect sequence.
fn pane_input_effects(effects: &[RuntimeSideEffect]) -> Vec<&RuntimeSideEffect> {
    effects
        .iter()
        .filter(|effect| {
            matches!(
                effect,
                RuntimeSideEffect::WritePaneInput { .. }
                    | RuntimeSideEffect::WritePaneInputPriority { .. }
            )
        })
        .collect()
}

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
            id: "say-1".to_string(),
            rationale: "report completion".to_string(),
            payload: crate::agent::AgentActionPayload::Say {
                status: crate::agent::SayStatus::Final,
                text: "Done.".to_string(),
                content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            },
        }],
        final_turn: true,
    }
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
            crate::agent::AgentActionPayload::IssueAdd { .. }
            | crate::agent::AgentActionPayload::IssueUpdate { .. }
            | crate::agent::AgentActionPayload::IssueQuery { .. }
            | crate::agent::AgentActionPayload::IssueDelete { .. } => {
                Some(crate::agent::AgentCapability::Issues)
            }
            crate::agent::AgentActionPayload::Say { .. }
            | crate::agent::AgentActionPayload::RequestCapability { .. }
            | crate::agent::AgentActionPayload::RequestSkills
            | crate::agent::AgentActionPayload::CallSkill { .. }
            | crate::agent::AgentActionPayload::Complete
            | crate::agent::AgentActionPayload::Abort { .. }
            | crate::agent::AgentActionPayload::MemorySearch { .. }
            | crate::agent::AgentActionPayload::MemoryStore { .. } => None,
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
        memory_actions_enabled: false,
        issue_actions_enabled: true,
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

/// Fails the first two provider requests with output-limit incomplete responses
/// and succeeds after runtime recovery applies both retry stages.
struct RuntimeOutputLimitThenSuccessProvider {
    /// Requests observed by the test provider.
    requests: RefCell<Vec<crate::agent::ModelRequest>>,
}

impl ModelProvider for RuntimeOutputLimitThenSuccessProvider {
    /// Returns the provider id used by the output-limit recovery test.
    fn provider_id(&self) -> &str {
        "runtime-batch"
    }

    /// Returns two output-limit errors, then a successful completion response.
    fn send_request(
        &self,
        request: &crate::agent::ModelRequest,
    ) -> Result<crate::agent::ModelResponse> {
        let mut requests = self.requests.borrow_mut();
        requests.push(request.clone());
        if requests.len() <= 2 {
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
                    cache_write_input_tokens: None,
                },
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: None,
                provider_transcript_events: Vec::new(),
            });
        }
        let mut response = runtime_say_response(&request.turn_id, "auto-sized response", true);
        response.usage = crate::agent::ModelTokenUsage {
            input_tokens: 150,
            output_tokens: 40,
            reasoning_tokens: 12,
            cached_input_tokens: Some(50),
            cache_write_input_tokens: None,
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
  *'"method":"initialize"'*)
printf '%s\n' '{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"2025-11-25","capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"fixture","version":"1.0.0"}}}}}}'
;;
  *'"method":"notifications/initialized"'*)
;;
  *'"method":"tools/list"'*)
printf '%s\n' '{{"jsonrpc":"2.0","id":2,"result":{{"tools":[{{"name":"echo","description":"Echo a message","inputSchema":{{"type":"object","properties":{{"message":{{"type":"string"}}}}}}}}]}}}}'
;;
  *'"method":"tools/call"'*)
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
    let deadline = Instant::now() + Duration::from_secs(15);
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
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
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

/// Builds the synthetic model response used by compaction completion tests.
fn runtime_test_compaction_response(summary: &str) -> crate::agent::ModelResponse {
    crate::agent::ModelResponse {
        provider: "test".to_string(),
        model: "gpt-compact-test".to_string(),
        raw_text: summary.to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Vec::new(),
        action_batch: None,
        provider_transcript_events: Vec::new(),
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
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
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
            provider_transcript_events: Vec::new(),
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
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
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
            provider_transcript_events: Vec::new(),
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

mod actions;
mod agent;
mod config;
mod events;
mod session;
mod terminal;
