//! Runtime Types implementation.
//!
//! This module owns the runtime types boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    ActionStatus, AgentAction, AgentActionPayload, AgentContext, AgentId, AgentScheduler,
    AgentShellStore, AgentShellVisibility, AgentTranscriptStore, AgentTurnExecution,
    AgentTurnLedger, AgentTurnRecord, AgentTurnState, AuditDeferredWrite, AuditLog, AuthStore,
    BTreeMap, BTreeSet, BlockedApprovalQueue, ChatCompletionsProvider, ConfigLayer, ConfigScope,
    ControlIdempotencyCache, CopyMode, DiscoveredInstructionFile, Duration, EnvironmentSignature,
    EventAudience, EventLog, File, FocusedShellHookDispatch, FocusedShellHookQueue, HookDefinition,
    HookEvent, HookExecutionPlan, HookExecutionResult, HookFailureKind, HostClipboard, KeyBindings,
    KeyChord, McpRegistry, McpServerStatus, McpStartupPlan, McpStdioConnection, McpToolCallPlan,
    McpToolCallResponse, MessageService, MezError, ModelProfile, ModelRequest, ModelResponse,
    ModelTokenUsage, ModelTokenUsageKey, OpenAiResponsesProvider, OpenOptions, OsString,
    PaneExitStatus, PaneGeometry, PaneId, PaneProcessManager, PaneReadinessOverrideStore,
    PaneReadinessState, PasteBuffers, Path, PathBuf, PathScopes, PermissionPolicy,
    ProjectTrustStore, ProviderQuotaUsage, ReqwestProviderHttpTransport, Result, ScopeRegistry,
    Session, SessionApprovalStore, SessionMemoryStore, SessionRecord, SessionRegistry, Size,
    SplitDirection, Stdio, SubagentProfile, SubagentScopeDeclaration, TerminalCursorStyle,
    TerminalFramePosition, TerminalFrameStyle, TerminalScreen, ToolDiscoveryCache, TranscriptEntry,
    UiTheme, VisibleEvent, WindowFrameAction, WindowId, Write, delivery_batch_json, effective_uid,
    encode_control_body, encode_event_notification, encode_mmp_body,
    execute_streamable_http_exchange, parse_mcp_tools_call_response,
};
use crate::mcp::McpPromptTool;
use crate::readline::{ReadlineInputDecoder, ReadlinePrompt};
use crate::terminal::{CopyPosition, PaneAgentStatusField, TerminalStyleSpan};
use tokio::io::AsyncWriteExt;

// Runtime data types, connection tables, and provider/MCP registries.

/// Defines the MEZ ENV FIELD SEPARATOR const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const MEZ_ENV_FIELD_SEPARATOR: char = '\x1f';
/// Defines the DEFAULT SOCKET NAME const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const DEFAULT_SOCKET_NAME: &str = "default.sock";
/// Defines the DEFAULT PTY READ LIMIT BYTES const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const DEFAULT_PTY_READ_LIMIT_BYTES: usize = 64 * 1024;
/// Default number of subagent panes that may share one subagent window.
pub const DEFAULT_MAX_SUBAGENT_PANES_PER_WINDOW: usize = 4;
/// Default number of direct subagents a root pane agent may spawn.
pub const DEFAULT_MAX_ROOT_SUBAGENTS: usize = 4;
/// Default number of direct subagents a child subagent may spawn.
pub const DEFAULT_MAX_SUBAGENTS_PER_SUBAGENT: usize = 2;
/// Default maximum delegation depth for nested subagents.
pub const DEFAULT_MAX_SUBAGENT_DEPTH: usize = 2;
/// Default policy for parent turns after spawning child subagents.
pub const DEFAULT_SUBAGENT_WAIT_POLICY: SubagentWaitPolicy = SubagentWaitPolicy::Join;
/// Default percent of the active model context retained as uncompacted raw tail.
pub const DEFAULT_AGENT_COMPACTION_RAW_RETENTION_PERCENT: usize = 10;
/// Whether agent turns use automatic model and reasoning sizing by default.
pub const DEFAULT_AGENT_ROUTING: bool = false;
/// Default bounded retry budget for model-correctable action failures.
pub const DEFAULT_AGENT_ACTION_FAILURE_RETRY_LIMIT: usize = 5;
/// Default number of successive successful shell commands before nudging implementation.
pub const DEFAULT_AGENT_IMPLEMENTATION_PRESSURE_AFTER_SHELL_ACTIONS: usize = 5;
/// Default router profile for automatic model and reasoning sizing.
pub const DEFAULT_AUTO_SIZING_ROUTER_PROFILE: &str = "auto-size-router";
/// Default small target profile for automatic model and reasoning sizing.
pub const DEFAULT_AUTO_SIZING_SMALL_PROFILE: &str = "auto-size-small";
/// Default medium target profile for automatic model and reasoning sizing.
pub const DEFAULT_AUTO_SIZING_MEDIUM_PROFILE: &str = "auto-size-medium";
/// Default large target profile for automatic model and reasoning sizing.
pub const DEFAULT_AUTO_SIZING_LARGE_PROFILE: &str = "auto-size-large";
/// Default fallback policy for failed automatic model sizing decisions.
pub const DEFAULT_AUTO_SIZING_FALLBACK_POLICY: &str = "use-default-profile";
/// Defines the COMMAND PANE PIPE QUEUE CAPACITY const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const COMMAND_PANE_PIPE_QUEUE_CAPACITY: usize = 256;
/// Defines the COMMAND PANE PIPE CLOSE TIMEOUT const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const COMMAND_PANE_PIPE_CLOSE_TIMEOUT: Duration = Duration::from_millis(250);
/// Runtime-owned diagnostics for provider, prompt-cache, turn, and shell work.
///
/// The async runtime actor records serialized actor activity separately. This
/// snapshot covers the higher-level runtime service path so inspection commands
/// can debug agent/provider behavior without parsing trace logs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct RuntimeMetricsSnapshot {
    /// Number of agent turns started by the runtime service.
    pub(super) agent_turns_started: u64,
    /// Number of agent turns that ended completed.
    pub(super) agent_turns_completed: u64,
    /// Number of agent turns that ended failed.
    pub(super) agent_turns_failed: u64,
    /// Number of agent turns that ended interrupted.
    pub(super) agent_turns_interrupted: u64,
    /// Number of agent turns that ended blocked waiting for approval or child work.
    pub(super) agent_turns_blocked: u64,
    /// Number of provider request shapes recorded from runtime executions.
    pub(super) provider_requests_started: u64,
    /// Number of recorded provider requests in capability-decision mode.
    pub(super) provider_request_capability_decision: u64,
    /// Number of recorded provider requests in action-execution mode.
    pub(super) provider_request_action_execution: u64,
    /// Number of recorded provider requests in repair mode.
    pub(super) provider_request_repair: u64,
    /// Number of recorded provider requests in auto-sizing mode.
    pub(super) provider_request_auto_sizing: u64,
    /// Number of provider executions that returned a usable response.
    pub(super) provider_responses_succeeded: u64,
    /// Number of provider executions that failed before a usable response.
    pub(super) provider_responses_failed: u64,
    /// Number of request shapes with available prompt-cache diagnostics.
    pub(super) provider_prompt_cache_diagnostics_available: u64,
    /// Number of request shapes whose prompt-cache diagnostics could not be built.
    pub(super) provider_prompt_cache_diagnostics_failed: u64,
    /// Number of provider responses that reported cached input tokens.
    pub(super) provider_cached_input_reports: u64,
    /// Number of provider responses that did not report cached input tokens.
    pub(super) provider_cached_input_unknown: u64,
    /// Number of provider responses that reported zero cached input tokens.
    pub(super) provider_cached_input_zero_hits: u64,
    /// Accumulated provider input tokens.
    pub(super) provider_input_tokens: u64,
    /// Accumulated provider output tokens.
    pub(super) provider_output_tokens: u64,
    /// Accumulated provider reasoning tokens.
    pub(super) provider_reasoning_tokens: u64,
    /// Accumulated provider cached input tokens when reported.
    pub(super) provider_cached_input_tokens: u64,
    /// Accumulated provider input tokens not reported as cache hits.
    pub(super) provider_billed_input_tokens: u64,
    /// Accumulated provider token usage grouped by provider/model.
    pub(super) provider_token_usage_by_model: BTreeMap<ModelTokenUsageKey, ModelTokenUsage>,
    /// Number of shell action dispatch attempts that reached dispatch accounting.
    pub(super) shell_action_batches: u64,
    /// Number of shell-backed agent actions dispatched to panes.
    pub(super) shell_actions_dispatched: u64,
    /// Number of shell transactions observed to completion.
    pub(super) shell_transactions_observed: u64,
    /// Number of shell transactions that exited successfully.
    pub(super) shell_transactions_succeeded: u64,
    /// Number of shell transactions that exited non-zero.
    pub(super) shell_transactions_failed: u64,
    /// Number of shell transaction marker protocol violations.
    pub(super) shell_transaction_protocol_violations: u64,
    /// Histogram of provider request message counts.
    pub(super) provider_request_message_counts: crate::async_runtime::RuntimeHistogram,
    /// Histogram of total provider request message bytes.
    pub(super) provider_request_message_bytes: crate::async_runtime::RuntimeHistogram,
    /// Histogram of OpenAI instruction bytes in cache diagnostics.
    pub(super) provider_prompt_instructions_bytes: crate::async_runtime::RuntimeHistogram,
    /// Histogram of OpenAI response-format bytes in cache diagnostics.
    pub(super) provider_prompt_response_format_bytes: crate::async_runtime::RuntimeHistogram,
    /// Histogram of OpenAI tool schema bytes in cache diagnostics.
    pub(super) provider_prompt_tools_bytes: crate::async_runtime::RuntimeHistogram,
    /// Histogram of OpenAI tool-choice bytes in cache diagnostics.
    pub(super) provider_prompt_tool_choice_bytes: crate::async_runtime::RuntimeHistogram,
    /// Histogram of stable input bytes in cache diagnostics.
    pub(super) provider_prompt_stable_input_bytes: crate::async_runtime::RuntimeHistogram,
    /// Histogram of volatile input bytes in cache diagnostics.
    pub(super) provider_prompt_volatile_input_bytes: crate::async_runtime::RuntimeHistogram,
    /// Histogram of stable prompt-prefix bytes in cache diagnostics.
    pub(super) provider_prompt_stable_prefix_bytes: crate::async_runtime::RuntimeHistogram,
    /// Histogram of provider request-shape bytes tracked outside the prompt prefix.
    pub(super) provider_request_shape_bytes: crate::async_runtime::RuntimeHistogram,
    /// Histogram of stable observable cacheable prefix bytes.
    pub(super) provider_prompt_cacheable_prefix_bytes: crate::async_runtime::RuntimeHistogram,
    /// Histogram of latest response input tokens.
    pub(super) provider_input_tokens_per_response: crate::async_runtime::RuntimeHistogram,
    /// Histogram of latest response output tokens.
    pub(super) provider_output_tokens_per_response: crate::async_runtime::RuntimeHistogram,
    /// Histogram of latest response cached input tokens.
    pub(super) provider_cached_input_tokens_per_response: crate::async_runtime::RuntimeHistogram,
    /// Histogram of latest response cache-hit ratios in basis points.
    pub(super) provider_cached_input_hit_ratio_basis_points: crate::async_runtime::RuntimeHistogram,
    /// Histogram of MAAP action counts per provider response.
    pub(super) provider_response_action_counts: crate::async_runtime::RuntimeHistogram,
    /// Histogram of shell actions dispatched per dispatch pass.
    pub(super) shell_actions_dispatched_per_batch: crate::async_runtime::RuntimeHistogram,
    /// Histogram of shell transaction elapsed milliseconds.
    pub(super) shell_transaction_duration_ms: crate::async_runtime::RuntimeHistogram,
    /// Histogram of shell transaction model-visible output bytes.
    pub(super) shell_transaction_output_bytes: crate::async_runtime::RuntimeHistogram,
    /// Most recent provider identifier observed by runtime metrics.
    pub(super) last_provider: Option<String>,
    /// Most recent provider model observed by runtime metrics.
    pub(super) last_model: Option<String>,
    /// Most recent provider interaction kind observed by runtime metrics.
    pub(super) last_interaction_kind: Option<String>,
    /// Most recent allowed action surface observed by runtime metrics.
    pub(super) last_allowed_actions: Option<String>,
    /// Most recent prompt-cache key observed by runtime metrics.
    pub(super) last_prompt_cache_key: Option<String>,
    /// Most recent stable prompt-prefix digest observed by runtime metrics.
    pub(super) last_stable_prompt_prefix_sha256: Option<String>,
    /// Most recent provider request-shape digest observed by runtime metrics.
    pub(super) last_provider_request_shape_sha256: Option<String>,
    /// Most recent tool-choice digest observed by runtime metrics.
    pub(super) last_tool_choice_sha256: Option<String>,
}

impl RuntimeMetricsSnapshot {
    /// Records that one runtime-owned agent turn started execution.
    pub(super) fn record_agent_turn_started(&mut self) {
        self.agent_turns_started = self.agent_turns_started.saturating_add(1);
    }

    /// Records one terminal or blocked turn outcome.
    pub(super) fn record_agent_turn_finished(&mut self, state: AgentTurnState) {
        match state {
            AgentTurnState::Completed => {
                self.agent_turns_completed = self.agent_turns_completed.saturating_add(1);
            }
            AgentTurnState::Failed => {
                self.agent_turns_failed = self.agent_turns_failed.saturating_add(1);
            }
            AgentTurnState::Interrupted => {
                self.agent_turns_interrupted = self.agent_turns_interrupted.saturating_add(1);
            }
            AgentTurnState::Blocked => {
                self.agent_turns_blocked = self.agent_turns_blocked.saturating_add(1);
            }
            AgentTurnState::Queued | AgentTurnState::Running => {}
        }
    }

    /// Records one provider request shape and prompt-cache diagnostic snapshot.
    pub(super) fn record_provider_request_shape(
        &mut self,
        request: &ModelRequest,
        diagnostics: Option<&crate::agent::OpenAiPromptCacheDiagnostics>,
        diagnostics_failed: bool,
    ) {
        self.provider_requests_started = self.provider_requests_started.saturating_add(1);
        match request.interaction_kind.as_str() {
            "capability_decision" => {
                self.provider_request_capability_decision =
                    self.provider_request_capability_decision.saturating_add(1);
            }
            "action_execution" => {
                self.provider_request_action_execution =
                    self.provider_request_action_execution.saturating_add(1);
            }
            "repair" => {
                self.provider_request_repair = self.provider_request_repair.saturating_add(1);
            }
            "auto_sizing" => {
                self.provider_request_auto_sizing =
                    self.provider_request_auto_sizing.saturating_add(1);
            }
            _ => {}
        }
        self.provider_request_message_counts
            .record(request.messages.len() as u64);
        let message_bytes = request.messages.iter().fold(0u64, |sum, message| {
            sum.saturating_add(message.content.len() as u64)
        });
        self.provider_request_message_bytes.record(message_bytes);
        self.last_provider = Some(request.provider.clone());
        self.last_model = Some(request.model.clone());
        self.last_interaction_kind = Some(request.interaction_kind.as_str().to_string());
        self.last_allowed_actions = Some(request.allowed_actions.action_type_names().join(","));
        if let Some(diagnostics) = diagnostics {
            self.provider_prompt_cache_diagnostics_available = self
                .provider_prompt_cache_diagnostics_available
                .saturating_add(1);
            self.provider_prompt_instructions_bytes
                .record(diagnostics.instructions_bytes as u64);
            self.provider_prompt_response_format_bytes
                .record(diagnostics.response_format_bytes as u64);
            self.provider_prompt_tools_bytes
                .record(diagnostics.tools_bytes as u64);
            self.provider_prompt_tool_choice_bytes
                .record(diagnostics.tool_choice_bytes as u64);
            self.provider_prompt_stable_input_bytes
                .record(diagnostics.stable_input_bytes as u64);
            self.provider_prompt_volatile_input_bytes
                .record(diagnostics.volatile_input_bytes as u64);
            self.provider_prompt_stable_prefix_bytes
                .record(diagnostics.stable_prompt_prefix_bytes as u64);
            self.provider_request_shape_bytes
                .record(diagnostics.provider_request_shape_bytes as u64);
            self.provider_prompt_cacheable_prefix_bytes
                .record(diagnostics.cacheable_prefix_bytes as u64);
            self.last_prompt_cache_key = Some(diagnostics.prompt_cache_key.clone());
            self.last_stable_prompt_prefix_sha256 =
                Some(diagnostics.stable_prompt_prefix_sha256.clone());
            self.last_provider_request_shape_sha256 =
                Some(diagnostics.provider_request_shape_sha256.clone());
            self.last_tool_choice_sha256 = Some(diagnostics.tool_choice_sha256.clone());
        } else if diagnostics_failed {
            self.provider_prompt_cache_diagnostics_failed = self
                .provider_prompt_cache_diagnostics_failed
                .saturating_add(1);
        }
    }

    /// Records one successful provider execution and its response shape.
    pub(super) fn record_provider_response(
        &mut self,
        response: &ModelResponse,
        latest_usage: ModelTokenUsage,
        model_key: &ModelTokenUsageKey,
    ) {
        self.provider_responses_succeeded = self.provider_responses_succeeded.saturating_add(1);
        self.provider_response_action_counts.record(
            response
                .action_batch
                .as_ref()
                .map(|batch| batch.actions.len() as u64)
                .unwrap_or(0),
        );
        self.record_provider_token_usage(response.usage, latest_usage, model_key);
    }

    /// Records one provider request that failed before yielding a usable response.
    pub(super) fn record_provider_failure(&mut self) {
        self.provider_responses_failed = self.provider_responses_failed.saturating_add(1);
    }

    /// Records provider token counters and per-response token histograms.
    pub(super) fn record_provider_token_usage(
        &mut self,
        usage: ModelTokenUsage,
        latest_usage: ModelTokenUsage,
        model_key: &ModelTokenUsageKey,
    ) {
        self.provider_input_tokens = self
            .provider_input_tokens
            .saturating_add(usage.input_tokens);
        self.provider_output_tokens = self
            .provider_output_tokens
            .saturating_add(usage.output_tokens);
        self.provider_reasoning_tokens = self
            .provider_reasoning_tokens
            .saturating_add(usage.reasoning_tokens);
        self.provider_cached_input_tokens = self
            .provider_cached_input_tokens
            .saturating_add(usage.cached_input_tokens.unwrap_or(0));
        self.provider_billed_input_tokens = self
            .provider_billed_input_tokens
            .saturating_add(usage.billed_input_tokens());
        if !usage.is_zero() {
            self.provider_token_usage_by_model
                .entry(model_key.clone())
                .or_default()
                .add_assign(usage);
        }
        self.provider_input_tokens_per_response
            .record(latest_usage.input_tokens);
        self.provider_output_tokens_per_response
            .record(latest_usage.output_tokens);
        if let Some(cached) = latest_usage.cached_input_tokens {
            self.provider_cached_input_reports =
                self.provider_cached_input_reports.saturating_add(1);
            if cached == 0 {
                self.provider_cached_input_zero_hits =
                    self.provider_cached_input_zero_hits.saturating_add(1);
            }
            self.provider_cached_input_tokens_per_response
                .record(cached);
            if let Some(ratio) = latest_usage.cached_input_hit_ratio_basis_points() {
                self.provider_cached_input_hit_ratio_basis_points
                    .record(u64::from(ratio));
            }
        } else {
            self.provider_cached_input_unknown =
                self.provider_cached_input_unknown.saturating_add(1);
        }
    }

    /// Records the number of shell-backed actions dispatched in one pass.
    pub(super) fn record_shell_action_batch(&mut self, dispatched: usize) {
        self.shell_action_batches = self.shell_action_batches.saturating_add(1);
        self.shell_actions_dispatched = self
            .shell_actions_dispatched
            .saturating_add(dispatched as u64);
        self.shell_actions_dispatched_per_batch
            .record(dispatched as u64);
    }

    /// Records one completed shell transaction and its result payload size.
    pub(super) fn record_shell_transaction_completion(
        &mut self,
        started_at_unix_ms: u64,
        finished_at_unix_ms: u64,
        output_bytes: usize,
        exit_code: i32,
    ) {
        self.shell_transactions_observed = self.shell_transactions_observed.saturating_add(1);
        if exit_code == 0 {
            self.shell_transactions_succeeded = self.shell_transactions_succeeded.saturating_add(1);
        } else {
            self.shell_transactions_failed = self.shell_transactions_failed.saturating_add(1);
        }
        self.shell_transaction_duration_ms
            .record(finished_at_unix_ms.saturating_sub(started_at_unix_ms));
        self.shell_transaction_output_bytes
            .record(output_bytes as u64);
    }

    /// Records one shell wrapper marker protocol violation.
    pub(super) fn record_shell_transaction_protocol_violation(&mut self) {
        self.shell_transaction_protocol_violations =
            self.shell_transaction_protocol_violations.saturating_add(1);
    }
}

/// One retained `apply_patch` attempt emitted by the current pane agent session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RuntimeAgentPatchRecord {
    /// Turn id that contained the patch action.
    pub turn_id: String,
    /// Action id assigned by the model to the patch action.
    pub action_id: String,
    /// Lowercase action status observed by the runtime.
    pub status: String,
    /// Patch body exactly as emitted in the MAAP action payload.
    pub patch: String,
    /// Optional `strip` value supplied with the patch payload.
    pub strip: Option<u64>,
    /// Optional structured error code recorded for a failed patch.
    pub error_code: Option<String>,
    /// Optional human-readable error or patch diagnostic for a failed patch.
    pub error_message: Option<String>,
}

/// Carries Auxiliary Socket Kind state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuxiliarySocketKind {
    /// Represents the Message case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Message,
    /// Represents the Event case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Event,
}

/// Carries Socket Directory Source state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketDirectorySource {
    /// Represents the Mez Tmpdir case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    MezTmpdir,
    /// Represents the Xdg Runtime Dir case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    XdgRuntimeDir,
    /// Represents the Tmp case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Tmp,
}

/// Carries Runtime Env state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeEnv {
    /// Stores the mez tmpdir value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub mez_tmpdir: Option<OsString>,
    /// Stores the xdg runtime dir value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub xdg_runtime_dir: Option<OsString>,
    /// Stores the uid value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub uid: u32,
}

impl RuntimeEnv {
    /// Runs the from process operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn from_process() -> Self {
        Self {
            mez_tmpdir: std::env::var_os("MEZ_TMPDIR"),
            xdg_runtime_dir: std::env::var_os("XDG_RUNTIME_DIR"),
            uid: effective_uid(),
        }
    }
}

/// Carries Socket Directory state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocketDirectory {
    /// Stores the path value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub path: PathBuf,
    /// Stores the source value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub source: SocketDirectorySource,
}

/// Carries Pane Environment state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneEnvironment {
    /// Stores the mez value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub mez: String,
    /// Stores the session value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub session: String,
    /// Stores the window value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub window: String,
    /// Stores the pane value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane: String,
    /// Stores the term value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub term: String,
}

/// Carries Runtime Lifecycle State state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeLifecycleState {
    /// Represents the Running case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Running,
    /// Represents the Detached case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Detached,
    /// Represents the Stopping case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Stopping,
    /// Represents the Killed case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Killed,
    /// Represents the Failed case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Failed,
}

/// Carries Runtime Registry Update Plan state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeRegistryUpdatePlan {
    /// Represents the Upsert case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Upsert(SessionRecord),
    /// Represents the Remove case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Remove {
        /// Stores the session id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        session_id: String,
    },
}

/// Carries Pane Process Start state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneProcessStart {
    /// Stores the session id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub session_id: String,
    /// Stores the window id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub window_id: String,
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_id: String,
    /// Stores the primary pid value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub primary_pid: u32,
    /// Stores the size value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub size: Size,
    /// Stores the registry update value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub registry_update: RuntimeRegistryUpdatePlan,
}

/// Carries Pane Resize Update state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneResizeUpdate {
    /// Stores the session id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub session_id: String,
    /// Stores the window id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub window_id: String,
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_id: String,
    /// Stores the primary pid value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub primary_pid: u32,
    /// Stores the size value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub size: Size,
    /// Stores the registry update value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub registry_update: RuntimeRegistryUpdatePlan,
}

/// Carries Pane Input Dispatch state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneInputDispatch {
    /// Stores the session id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub session_id: String,
    /// Stores the window id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub window_id: String,
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_id: String,
    /// Stores the primary pid value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub primary_pid: u32,
    /// Stores the bytes written value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub bytes_written: usize,
}

/// Carries Deferred Pane Input state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredPaneInput {
    /// Pane whose PTY should receive the bytes.
    pub pane_id: String,
    /// Bytes to write to the pane PTY.
    pub bytes: Vec<u8>,
    /// Whether the input must overtake already queued pane input.
    ///
    /// Transaction payloads use this to stay directly behind the wrapper whose
    /// receiver has just announced that it is ready to drain payload data.
    pub priority: bool,
}

/// Owned snapshot creation context captured by the actor before repository I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeSnapshotOwnedCreationContext {
    /// Live pane terminal/process captures.
    pub pane_captures: Vec<crate::snapshot::SnapshotPaneCapture>,
    /// Active config layers at capture time.
    pub active_config_layers: Vec<crate::snapshot::SnapshotConfigLayerMetadata>,
    /// Live terminal frame state at capture time.
    pub frame_state: crate::snapshot::SnapshotFrameState,
    /// Agent sessions to include in the snapshot payload.
    pub agent_sessions: Vec<crate::snapshot::SnapshotAgentSession>,
    /// Approval grants to include in the snapshot payload.
    pub approval_grants: Vec<crate::snapshot::SnapshotApprovalGrantMetadata>,
    /// Approval requests to include in the snapshot payload.
    pub approval_requests: Vec<crate::snapshot::SnapshotApprovalRequestMetadata>,
    /// Message-service state to include in the snapshot payload.
    pub message_state: crate::message::MessageServiceSnapshot,
    /// MCP server state to include in the snapshot payload.
    pub mcp_servers: Vec<crate::snapshot::SnapshotMcpServerState>,
}

impl RuntimeSnapshotOwnedCreationContext {
    /// Borrows the owned context as the snapshot repository creation context.
    pub(crate) fn as_creation_context(&self) -> crate::snapshot::SnapshotCreationContext<'_> {
        crate::snapshot::SnapshotCreationContext::new(
            &self.pane_captures,
            &self.active_config_layers,
            &self.frame_state,
            &self.agent_sessions,
        )
        .with_approvals(&self.approval_grants, &self.approval_requests)
        .with_message_state(&self.message_state)
        .with_mcp_servers(&self.mcp_servers)
    }
}

/// Snapshot control operation that can perform repository I/O off the actor.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeSnapshotControlAsyncWork {
    /// Parsed JSON-RPC request.
    pub request: crate::control::JsonRpcRequest,
    /// Client authorized to make the request.
    pub caller_client_id: crate::ids::ClientId,
    /// Operation-specific repository work.
    pub kind: RuntimeSnapshotControlAsyncWorkKind,
}

/// Repository work shape for actor-deferred snapshot control operations.
#[derive(Debug, Clone)]
pub(crate) enum RuntimeSnapshotControlAsyncWorkKind {
    /// Snapshot list/create/delete or plan-only resume dispatch.
    Dispatch {
        /// Session snapshot captured before the repository operation.
        session: Box<crate::session::Session>,
        /// Owned snapshot context captured before the repository operation.
        context: Box<RuntimeSnapshotOwnedCreationContext>,
    },
    /// Live snapshot resume that must return payload metadata for actor apply.
    Resume {
        /// Shell to seed restored panes with.
        shell: crate::shell::ResolvedShell,
    },
}

/// Repository result returned to the actor after async snapshot control work.
#[derive(Debug)]
pub(crate) enum RuntimeSnapshotControlAsyncOutcome {
    /// JSON result body produced by the snapshot dispatcher.
    Dispatch(Result<String>),
    /// Live resume payload plus restored session state.
    Resume(
        Box<
            Result<(
                crate::snapshot::SessionSnapshotPayload,
                crate::snapshot::SnapshotRestoreResult,
            )>,
        >,
    ),
}

/// Pane resize operation deferred for an async pane process owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeferredPaneResize {
    /// Latest pane PTY size requested by runtime layout state.
    pub size: Size,
}

/// Pane termination operation deferred for an async pane process owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeferredPaneTermination {
    /// Whether the pane termination was requested as a forceful kill.
    pub force: bool,
}

/// File-backed pane pipe write deferred for async persistence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredPanePipeWrite {
    /// Pane whose rendered output should be piped.
    pub pane_id: String,
    /// File target configured by `pipe-pane -o`.
    pub path: PathBuf,
    /// Output bytes to append.
    pub bytes: Vec<u8>,
}

/// Agent transcript entries deferred for async persistence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredAgentTranscriptWrite {
    /// Filesystem-backed transcript store that owns encoding and permissions.
    pub store: AgentTranscriptStore,
    /// Transcript file path used for persistence diagnostics.
    pub path: PathBuf,
    /// Entries to append in sequence order.
    pub entries: Vec<TranscriptEntry>,
}

/// Agent prompt-history append deferred for async persistence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredAgentPromptHistoryWrite {
    /// Filesystem-backed transcript store that owns the shared history file.
    pub store: AgentTranscriptStore,
    /// Destination prompt-history file used for persistence diagnostics.
    pub path: PathBuf,
    /// Conversation identity used for validation and future scoping.
    pub conversation_id: String,
    /// Prompt text to append to the bounded shared history.
    pub prompt: String,
}

/// Primary command prompt history append deferred for async persistence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredCommandPromptHistoryWrite {
    /// Filesystem-backed transcript store that owns the command history file.
    pub store: AgentTranscriptStore,
    /// Destination command prompt history file used for persistence diagnostics.
    pub path: PathBuf,
    /// Command text to append to the bounded shared history.
    pub command: String,
}

/// Project instruction scaffold write deferred for async persistence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredProjectInstructionWrite {
    /// Destination instruction file path, normally `AGENTS.md` in the pane CWD.
    pub path: PathBuf,
    /// Complete scaffold bytes to create at the destination.
    pub bytes: Vec<u8>,
}

/// Project configuration write deferred for async persistence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredProjectConfigWrite {
    /// Destination project configuration file.
    pub path: PathBuf,
    /// Complete validated config text to replace at the destination.
    pub text: String,
}

/// User or project configuration write deferred for async persistence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredConfigFileWrite {
    /// Destination configuration file.
    pub path: PathBuf,
    /// Configuration scope that determines the persistence file policy.
    pub scope: ConfigScope,
    /// Complete validated config text to replace at the destination.
    pub text: String,
}

/// Describes whether a parent turn waits for spawned subagents before it can
/// continue provider execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentWaitPolicy {
    /// Spawned subagents are joined: the parent waits for their task results.
    Join,
    /// Spawned subagents are detached: the parent can continue after spawn.
    Detach,
}

/// Tracks one spawned child turn that a parent turn is waiting to join.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct JoinedSubagentDependency {
    /// Parent turn that emitted the MAAP `spawn_agent` action.
    pub parent_turn_id: String,
    /// Parent action that should receive the child task result.
    pub parent_action_id: String,
    /// Child turn created for the spawned subagent.
    pub child_turn_id: String,
    /// Child agent created for the spawned subagent.
    pub child_agent_id: String,
    /// Human-readable display name assigned to the child subagent.
    pub child_display_name: Option<String>,
}

/// Tracks runtime delegation lineage for an active spawned subagent.
///
/// Regular pane agents are roots at depth zero and therefore do not need stored
/// entries. Only active spawned children are tracked so width and depth limits
/// reflect currently running delegation state rather than historical turns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RuntimeSubagentLineage {
    /// Direct parent agent that spawned this child.
    pub parent_agent_id: String,
    /// Root pane agent that owns this delegation tree.
    pub root_agent_id: String,
    /// Depth of this subagent below the root pane agent.
    pub depth: usize,
    /// Human-readable display name assigned while the subagent is active.
    pub display_name: String,
}

/// Program hook execution deferred for an async hook worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredProgramHook {
    /// Fully planned program hook invocation.
    pub plan: HookExecutionPlan,
    /// Whether this hook was triggered by a completed lifecycle event.
    pub triggering_event_completed: bool,
}

/// Carries Attached Client Step Application state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachedClientStepApplication {
    /// Stores the forwarded bytes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub forwarded_bytes: usize,
    /// Stores the mux actions applied value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub mux_actions_applied: usize,
    /// Stores the mouse actions reported value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub mouse_actions_reported: usize,
    /// Stores the unsupported actions value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub unsupported_actions: Vec<String>,
    /// Stores the agent prompt inputs applied value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub agent_prompt_inputs_applied: usize,
    /// Stores the view refresh required value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub view_refresh_required: bool,
    /// Stores the full redraw required value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub full_redraw_required: bool,
}

/// Actor-owned full-window display overlay for command output, help text, and
/// recoverable foreground errors.
///
/// The overlay is modal for the primary client: normal pane input is suspended
/// while it is present, and subsequent input scrolls or closes this state
/// through the runtime actor before rendering the next frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RuntimeDisplayOverlay {
    /// Stores the lines value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) lines: Vec<String>,
    /// Visible terminal styles for `lines`, indexed by rendered line.
    pub(super) line_style_spans: Vec<Vec<TerminalStyleSpan>>,
    /// Stores the scroll offset value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) scroll_offset: usize,
    /// Selectable line-to-command mappings rendered inside this overlay.
    ///
    /// Lines without an entry remain inert. The line index is measured against
    /// `lines`, before paging offsets are applied by the renderer.
    pub(super) selections: Vec<RuntimeDisplayOverlaySelection>,
    /// Active selectable row for keyboard navigation.
    ///
    /// This index addresses `selections`, not `lines`, so multiple commands can
    /// share the same rendered view without requiring text scraping.
    pub(super) active_selection_index: Option<usize>,
    /// Stores the dismiss on any input value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) dismiss_on_any_input: bool,
}

/// One selectable command-output overlay line.
///
/// Command chooser output is still represented as ordinary command display
/// text for control clients. The primary TUI stores this companion metadata so
/// mouse clicks can execute the advertised command without scraping the
/// already-rendered text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RuntimeDisplayOverlaySelection {
    /// Zero-based line in `RuntimeDisplayOverlay::lines` that activates this
    /// selection.
    pub(super) line_index: usize,
    /// Zero-based display column where the interactive choice starts before
    /// the overlay renderer adds row selector gutters.
    pub(super) start_column: usize,
    /// Display-cell width of the interactive choice.
    pub(super) width: usize,
    /// Terminal command to execute when the line is selected.
    pub(super) command: String,
    /// Visual importance of this selectable action.
    pub(super) kind: RuntimeDisplayOverlaySelectionKind,
}

/// Visual category for one command-output overlay choice.
///
/// The category lets command overlays use theme-aware colors to distinguish
/// routine navigation from secondary actions and potentially destructive or
/// authority-changing choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RuntimeDisplayOverlaySelectionKind {
    /// Routine primary action, such as selecting a pane or approving a request.
    Primary,
    /// Secondary action, such as pasting a buffer.
    Secondary,
    /// Destructive or disruptive action, such as deleting, detaching, or
    /// rejecting.
    Danger,
}

/// Pane-local drop-down selector for agent model and reasoning status pills.
///
/// The selector is actor-owned UI state: mouse routing receives cell hits from
/// the terminal client loop, while rendering uses this record to draw the
/// current list and highlight the row under the pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RuntimePaneAgentStatusSelector {
    /// Stable pane identity targeted by the selector.
    pub(super) pane_id: String,
    /// Pane index targeted by rendered mouse cells.
    pub(super) pane_index: usize,
    /// Status field being selected.
    pub(super) field: PaneAgentStatusField,
    /// Available values in display and selection order.
    pub(super) items: Vec<String>,
    /// Item currently highlighted by hover or initial active value.
    pub(super) active_index: usize,
    /// First item currently visible in the drop-down viewport.
    pub(super) scroll_offset: usize,
    /// Column of the source pill used to place the drop-down.
    pub(super) anchor_column: u16,
    /// Row of the source pill used to place the drop-down.
    pub(super) anchor_row: u16,
    /// Width of the source pill used as a minimum drop-down width.
    pub(super) anchor_width: u16,
}

/// Carries Pane Output Update state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneOutputUpdate {
    /// Stores the session id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub session_id: String,
    /// Stores the window id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub window_id: String,
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_id: String,
    /// Stores the primary pid value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub primary_pid: u32,
    /// Stores the bytes read value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub bytes_read: usize,
    /// Stores the activity events value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub activity_events: u64,
    /// Stores the bell events value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub bell_events: u64,
    /// Stores the background value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub background: bool,
}

/// Carries Active Pane Pipe state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug)]
pub(super) struct ActivePanePipe {
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_id: String,
    /// Stores the target value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) target: ActivePanePipeTarget,
    /// Stores the bytes written value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) bytes_written: usize,
}

/// Carries Active Pane Pipe Target state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug)]
pub(super) enum ActivePanePipeTarget {
    /// Represents the File case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    File {
        /// Stores the path value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        path: PathBuf,
        /// Stores the file value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        file: Option<File>,
    },
    /// Represents the Command case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Command {
        /// Stores the command value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        command: String,
        /// Stores the writer value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        writer: CommandPanePipeWriter,
    },
}

/// Bounded Tokio-backed writer for command-backed pane pipes.
///
/// The runtime state keeps only the bounded sender and worker status. The pipe
/// command process and stdin are owned by a small Tokio runtime so pane-output
/// application does not block on the pipe command reading from stdin.
#[derive(Debug)]
pub(super) struct CommandPanePipeWriter {
    /// Stores the sender value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    sender: tokio::sync::mpsc::Sender<Vec<u8>>,
    /// Stores the status value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    status: CommandPanePipeWorkerState,
}

/// Carries Command Pane Pipe Worker Status state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Default)]
struct CommandPanePipeWorkerStatus {
    /// Stores the completed value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    completed: bool,
    /// Stores the failure value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    failure: Option<String>,
}

/// Defines the Command Pane Pipe Worker State type used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
type CommandPanePipeWorkerState = std::sync::Arc<std::sync::Mutex<CommandPanePipeWorkerStatus>>;

/// Carries Command Pane Pipe Status Snapshot state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CommandPanePipeStatusSnapshot {
    /// Stores the completed value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) completed: bool,
    /// Stores the failure value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) failure: Option<String>,
}

/// Carries Stopped Pane Pipe state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StoppedPanePipe {
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_id: String,
    /// Stores the mode value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) mode: &'static str,
    /// Stores the target value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) target: String,
    /// Stores the bytes written value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) bytes_written: usize,
    /// Stores the failure value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) failure: Option<String>,
}

impl ActivePanePipe {
    /// Runs the file operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn file(pane_id: String, path: PathBuf) -> Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            pane_id,
            target: ActivePanePipeTarget::File {
                path,
                file: Some(file),
            },
            bytes_written: 0,
        })
    }

    /// Runs the deferred file operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn deferred_file(pane_id: String, path: PathBuf) -> Self {
        Self {
            pane_id,
            target: ActivePanePipeTarget::File { path, file: None },
            bytes_written: 0,
        }
    }

    /// Runs the command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn command(pane_id: String, shell_path: &Path, command: String) -> Result<Self> {
        Ok(Self {
            pane_id,
            target: ActivePanePipeTarget::Command {
                writer: CommandPanePipeWriter::spawn(shell_path, command.clone())?,
                command,
            },
            bytes_written: 0,
        })
    }

    /// Runs the deferred command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn deferred_command(
        pane_id: String,
        shell_path: &Path,
        command: String,
    ) -> Result<Self> {
        Ok(Self {
            pane_id,
            target: ActivePanePipeTarget::Command {
                writer: CommandPanePipeWriter::spawn_without_startup_wait(
                    shell_path,
                    command.clone(),
                )?,
                command,
            },
            bytes_written: 0,
        })
    }

    /// Runs the write output operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn write_output(&mut self, bytes: &[u8]) -> Result<()> {
        match &mut self.target {
            ActivePanePipeTarget::File {
                file: Some(file), ..
            } => file.write_all(bytes)?,
            ActivePanePipeTarget::File { file: None, .. } => {
                return Err(MezError::invalid_state(
                    "deferred file pane pipe cannot write inline",
                ));
            }
            ActivePanePipeTarget::Command { writer, .. } => writer.write(bytes)?,
        }
        self.bytes_written = self.bytes_written.saturating_add(bytes.len());
        Ok(())
    }

    /// Runs the file target path operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn file_target_path(&self) -> Option<PathBuf> {
        match &self.target {
            ActivePanePipeTarget::File { path, .. } => Some(path.clone()),
            ActivePanePipeTarget::Command { .. } => None,
        }
    }

    /// Runs the record deferred output operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn record_deferred_output(&mut self, bytes: usize) {
        self.bytes_written = self.bytes_written.saturating_add(bytes);
    }

    /// Runs the mode operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn mode(&self) -> &'static str {
        match &self.target {
            ActivePanePipeTarget::File { .. } => "file",
            ActivePanePipeTarget::Command { .. } => "command",
        }
    }

    /// Runs the target label operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn target_label(&self) -> String {
        match &self.target {
            ActivePanePipeTarget::File { path, .. } => path.display().to_string(),
            ActivePanePipeTarget::Command { command, .. } => command.clone(),
        }
    }

    /// Runs the stop operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn stop(self) -> StoppedPanePipe {
        let mode = self.mode();
        let target = self.target_label();
        let failure = match self.target {
            ActivePanePipeTarget::Command { writer, .. } => writer.close(),
            ActivePanePipeTarget::File { .. } => None,
        };
        StoppedPanePipe {
            pane_id: self.pane_id,
            mode,
            target,
            bytes_written: self.bytes_written,
            failure,
        }
    }

    /// Runs the command status operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn command_status(&self) -> Result<Option<CommandPanePipeStatusSnapshot>> {
        match &self.target {
            ActivePanePipeTarget::Command { writer, .. } => writer.status().map(Some),
            ActivePanePipeTarget::File { .. } => Ok(None),
        }
    }
}

impl CommandPanePipeWriter {
    /// Runs the spawn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn spawn(shell_path: &Path, command: String) -> Result<Self> {
        Self::spawn_async(shell_path, command)
    }

    /// Runs the spawn without startup wait operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn spawn_without_startup_wait(shell_path: &Path, command: String) -> Result<Self> {
        Self::spawn_async(shell_path, command)
    }

    /// Runs the spawn async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn spawn_async(shell_path: &Path, command: String) -> Result<Self> {
        let (sender, receiver) = tokio::sync::mpsc::channel(COMMAND_PANE_PIPE_QUEUE_CAPACITY);
        let status =
            std::sync::Arc::new(std::sync::Mutex::new(CommandPanePipeWorkerStatus::default()));
        let worker_status = status.clone();
        let shell_path = shell_path.to_path_buf();
        let worker_command = command.clone();
        let handle = tokio::runtime::Handle::try_current().map_err(|_| {
            MezError::invalid_state("command pane pipe requires an active Tokio runtime")
        })?;
        handle.spawn(run_command_pane_pipe_writer_async(
            shell_path,
            worker_command,
            receiver,
            worker_status,
        ));
        Ok(Self { sender, status })
    }

    /// Runs the write operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn write(&self, bytes: &[u8]) -> Result<()> {
        let status = self.status()?;
        if let Some(failure) = status.failure {
            return Err(MezError::invalid_state(format!(
                "pipe command writer failed: {failure}"
            )));
        }
        if status.completed {
            return Err(MezError::invalid_state(
                "pipe command writer completed before accepting output",
            ));
        }
        self.sender.try_send(bytes.to_vec()).map_err(|error| {
            let message = match error {
                tokio::sync::mpsc::error::TrySendError::Full(_) => {
                    "pipe command writer queue is full".to_string()
                }
                tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                    "pipe command writer is closed".to_string()
                }
            };
            MezError::invalid_state(message)
        })?;
        let status = self.status()?;
        if let Some(failure) = status.failure {
            return Err(MezError::invalid_state(format!(
                "pipe command writer failed: {failure}"
            )));
        }
        Ok(())
    }

    /// Runs the close operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn close(self) -> Option<String> {
        let failure = self.status().ok().and_then(|status| status.failure);
        drop(self.sender);
        failure
    }

    /// Runs the status operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn status(&self) -> Result<CommandPanePipeStatusSnapshot> {
        self.status
            .lock()
            .map(|status| CommandPanePipeStatusSnapshot {
                completed: status.completed,
                failure: status.failure.clone(),
            })
            .map_err(|_| MezError::invalid_state("pipe command writer status lock is poisoned"))
    }
}

/// Runs the run command pane pipe writer async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn run_command_pane_pipe_writer_async(
    shell_path: PathBuf,
    command: String,
    mut receiver: tokio::sync::mpsc::Receiver<Vec<u8>>,
    status: CommandPanePipeWorkerState,
) {
    let mut child = match tokio::process::Command::new(&shell_path)
        .arg("-c")
        .arg(command.as_str())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            let message = format!("failed to spawn pipe command `{command}`: {error}");
            record_command_pane_pipe_failure(&status, message.clone());
            mark_command_pane_pipe_completed(&status);
            return;
        }
    };
    let mut stdin = match child.stdin.take() {
        Some(stdin) => stdin,
        None => {
            let message = "pipe command stdin was not captured".to_string();
            record_command_pane_pipe_failure(&status, message.clone());
            mark_command_pane_pipe_completed(&status);
            return;
        }
    };

    loop {
        tokio::select! {
            child_status = child.wait() => {
                record_command_pane_pipe_child_status(&status, child_status);
                mark_command_pane_pipe_completed(&status);
                return;
            }
            maybe_bytes = receiver.recv() => {
                let Some(bytes) = maybe_bytes else {
                    drop(stdin);
                    wait_for_command_pane_pipe_child(&mut child, &status).await;
                    mark_command_pane_pipe_completed(&status);
                    return;
                };
                if let Err(error) = stdin.write_all(&bytes).await {
                    record_command_pane_pipe_failure(&status, format!("stdin write failed: {error}"));
                    drop(stdin);
                    wait_for_command_pane_pipe_child(&mut child, &status).await;
                    mark_command_pane_pipe_completed(&status);
                    return;
                }
            }
        }
    }
}

/// Runs the wait for command pane pipe child operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn wait_for_command_pane_pipe_child(
    child: &mut tokio::process::Child,
    status: &CommandPanePipeWorkerState,
) {
    match tokio::time::timeout(COMMAND_PANE_PIPE_CLOSE_TIMEOUT, child.wait()).await {
        Ok(child_status) => record_command_pane_pipe_child_status(status, child_status),
        Err(_) => {
            record_command_pane_pipe_failure(status, command_pane_pipe_close_timeout_message());
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
    }
}

/// Runs the record command pane pipe child status operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn record_command_pane_pipe_child_status(
    status: &CommandPanePipeWorkerState,
    child_status: std::io::Result<std::process::ExitStatus>,
) {
    match child_status {
        Ok(exit_status) if exit_status.success() => {}
        Ok(exit_status) => {
            record_command_pane_pipe_failure(status, format!("child exited with {exit_status}"));
        }
        Err(error) => {
            record_command_pane_pipe_failure(status, format!("child status check failed: {error}"));
        }
    }
}

/// Runs the mark command pane pipe completed operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn mark_command_pane_pipe_completed(status: &CommandPanePipeWorkerState) {
    if let Ok(mut status) = status.lock() {
        status.completed = true;
    }
}

/// Runs the record command pane pipe failure operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn record_command_pane_pipe_failure(status: &CommandPanePipeWorkerState, message: String) {
    if let Ok(mut status) = status.lock() {
        status.failure.get_or_insert(message);
    }
}

/// Runs the command pane pipe close timeout message operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn command_pane_pipe_close_timeout_message() -> String {
    format!(
        "child did not exit within {}ms after pipe close",
        COMMAND_PANE_PIPE_CLOSE_TIMEOUT.as_millis()
    )
}

/// Carries Pane Exit Update state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneExitUpdate {
    /// Stores the session id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub session_id: String,
    /// Stores the window id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub window_id: String,
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_id: String,
    /// Stores the primary pid value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub primary_pid: u32,
    /// Stores the exit status value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub exit_status: PaneExitStatus,
    /// Stores the closed window value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub closed_window: bool,
    /// Stores the session empty value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub session_empty: bool,
    /// Stores the registry update value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub registry_update: RuntimeRegistryUpdatePlan,
}

/// Carries Pane Exit Record state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PaneExitRecord {
    /// Stores the exit status value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub exit_status: PaneExitStatus,
}

/// Carries Runtime Message Connection state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeMessageConnection {
    /// Stores the connection id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub connection_id: String,
    /// Stores the agent id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub agent_id: AgentId,
    /// Stores the writable value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub writable: bool,
}

/// Carries Runtime Message Wakeup state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeMessageWakeup {
    /// Stores the connection id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub connection_id: String,
    /// Stores the agent id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub agent_id: AgentId,
    /// Stores the pending messages value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pending_messages: usize,
}

/// Defines the Runtime Message Fanout Sink behavior contract for this subsystem.
///
/// Implementors provide the concrete I/O or state transition boundary
/// consumed by higher-level orchestration code.
pub trait RuntimeMessageFanoutSink {
    /// Runs the send frame operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn send_frame(&mut self, connection_id: &str, frame: &[u8]) -> Result<()>;
}

/// Carries Runtime Event Connection state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeEventConnection {
    /// Stores the connection id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub connection_id: String,
    /// Stores the audience value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub audience: EventAudience,
    /// Stores the writable value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub writable: bool,
    /// Stores the last delivered event id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub last_delivered_event_id: u64,
}

/// Carries Runtime Event Wakeup state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeEventWakeup {
    /// Stores the connection id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub connection_id: String,
    /// Stores the events value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub events: Vec<VisibleEvent>,
}

/// Defines the Runtime Event Fanout Sink behavior contract for this subsystem.
///
/// Implementors provide the concrete I/O or state transition boundary
/// consumed by higher-level orchestration code.
pub trait RuntimeEventFanoutSink {
    /// Runs the send frame operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn send_frame(&mut self, connection_id: &str, frame: &[u8]) -> Result<()>;
}

/// Carries Runtime Focused Shell Hook Run state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeFocusedShellHookRun {
    /// Stores the enqueued value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub enqueued: Vec<u64>,
    /// Stores the dispatches value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub dispatches: Vec<FocusedShellHookDispatch>,
    /// Stores the pending hooks value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pending_hooks: usize,
}

/// Carries Runtime Message Connection Table state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Default)]
pub struct RuntimeMessageConnectionTable {
    /// Stores the connections value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) connections: Vec<RuntimeMessageConnection>,
}

impl RuntimeMessageConnectionTable {
    /// Runs the attach operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn attach(
        &mut self,
        connection_id: impl Into<String>,
        agent_id: AgentId,
        writable: bool,
    ) -> Result<()> {
        let connection_id = connection_id.into();
        if connection_id.trim().is_empty() {
            return Err(MezError::invalid_args(
                "message connection id must not be empty",
            ));
        }
        if self
            .connections
            .iter()
            .any(|connection| connection.connection_id == connection_id)
        {
            return Err(MezError::conflict("message connection id already exists"));
        }
        self.connections.push(RuntimeMessageConnection {
            connection_id,
            agent_id,
            writable,
        });
        Ok(())
    }

    /// Runs the detach operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn detach(&mut self, connection_id: &str) -> bool {
        let before = self.connections.len();
        self.connections
            .retain(|connection| connection.connection_id != connection_id);
        self.connections.len() != before
    }

    /// Runs the set writable operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn set_writable(&mut self, connection_id: &str, writable: bool) -> Result<()> {
        let connection = self
            .connections
            .iter_mut()
            .find(|connection| connection.connection_id == connection_id)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "message connection not found",
                )
            })?;
        connection.writable = writable;
        Ok(())
    }

    /// Runs the wakeups operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn wakeups(
        &self,
        service: &MessageService,
        now_ms: u64,
        limit_per_connection: usize,
    ) -> Vec<RuntimeMessageWakeup> {
        self.connections
            .iter()
            .filter(|connection| connection.writable)
            .filter_map(|connection| {
                let batch = service
                    .fanout_ready_for(&connection.agent_id, now_ms, limit_per_connection)
                    .ok()??;
                Some(RuntimeMessageWakeup {
                    connection_id: connection.connection_id.clone(),
                    agent_id: connection.agent_id.clone(),
                    pending_messages: batch.batch.messages.len(),
                })
            })
            .collect()
    }

    /// Runs the len operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn len(&self) -> usize {
        self.connections.len()
    }

    /// Runs the is empty operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn is_empty(&self) -> bool {
        self.connections.is_empty()
    }
}

/// Runs the flush runtime message wakeup operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn flush_runtime_message_wakeup<S>(
    service: &mut MessageService,
    wakeup: &RuntimeMessageWakeup,
    now_ms: u64,
    limit: usize,
    sink: &mut S,
) -> Result<usize>
where
    S: RuntimeMessageFanoutSink,
{
    let Some(batch) = service.fanout_ready_for(&wakeup.agent_id, now_ms, limit)? else {
        return Ok(0);
    };
    let body = delivery_batch_json(&batch.batch);
    let frame = encode_mmp_body(&body);
    sink.send_frame(&wakeup.connection_id, &frame)?;
    service.acknowledge_fanout_batch(&batch)?;
    Ok(batch.batch.messages.len())
}

/// Runs the flush runtime message wakeups operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn flush_runtime_message_wakeups<S>(
    service: &mut MessageService,
    wakeups: &[RuntimeMessageWakeup],
    now_ms: u64,
    limit_per_connection: usize,
    sink: &mut S,
) -> Result<usize>
where
    S: RuntimeMessageFanoutSink,
{
    if limit_per_connection == 0 {
        return Err(MezError::invalid_args(
            "runtime message wakeup limit must be greater than zero",
        ));
    }
    let mut delivered = 0usize;
    for wakeup in wakeups {
        delivered +=
            flush_runtime_message_wakeup(service, wakeup, now_ms, limit_per_connection, sink)?;
    }
    Ok(delivered)
}

/// Carries Runtime Event Connection Table state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Default)]
pub struct RuntimeEventConnectionTable {
    /// Stores the connections value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) connections: Vec<RuntimeEventConnection>,
}

impl RuntimeEventConnectionTable {
    /// Runs the attach operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn attach(
        &mut self,
        connection_id: impl Into<String>,
        audience: EventAudience,
        writable: bool,
        last_delivered_event_id: u64,
    ) -> Result<()> {
        let connection_id = connection_id.into();
        if connection_id.trim().is_empty() {
            return Err(MezError::invalid_args(
                "event connection id must not be empty",
            ));
        }
        if self
            .connections
            .iter()
            .any(|connection| connection.connection_id == connection_id)
        {
            return Err(MezError::conflict("event connection id already exists"));
        }
        if matches!(audience, EventAudience::PendingObserver { .. }) {
            return Err(MezError::forbidden(
                "pending observer event streams are not allowed before approval",
            ));
        }
        self.connections.push(RuntimeEventConnection {
            connection_id,
            audience,
            writable,
            last_delivered_event_id,
        });
        Ok(())
    }

    /// Runs the detach operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn detach(&mut self, connection_id: &str) -> bool {
        let before = self.connections.len();
        self.connections
            .retain(|connection| connection.connection_id != connection_id);
        self.connections.len() != before
    }

    /// Runs the set writable operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn set_writable(&mut self, connection_id: &str, writable: bool) -> Result<()> {
        let connection = self
            .connections
            .iter_mut()
            .find(|connection| connection.connection_id == connection_id)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "event connection not found",
                )
            })?;
        connection.writable = writable;
        Ok(())
    }

    /// Runs the mark delivered operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn mark_delivered(&mut self, connection_id: &str, event_id: u64) -> Result<()> {
        let connection = self
            .connections
            .iter_mut()
            .find(|connection| connection.connection_id == connection_id)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "event connection not found",
                )
            })?;
        connection.last_delivered_event_id = connection.last_delivered_event_id.max(event_id);
        Ok(())
    }

    /// Runs the wakeups operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn wakeups(
        &self,
        event_log: Option<&EventLog>,
        limit_per_connection: usize,
    ) -> Vec<RuntimeEventWakeup> {
        let Some(event_log) = event_log else {
            return Vec::new();
        };
        self.connections
            .iter()
            .filter(|connection| connection.writable)
            .filter_map(|connection| {
                let events = event_log.replay_after_for(
                    &connection.audience,
                    connection.last_delivered_event_id,
                    limit_per_connection,
                );
                if events.is_empty() {
                    None
                } else {
                    Some(RuntimeEventWakeup {
                        connection_id: connection.connection_id.clone(),
                        events,
                    })
                }
            })
            .collect()
    }

    /// Runs the len operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn len(&self) -> usize {
        self.connections.len()
    }

    /// Runs the is empty operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn is_empty(&self) -> bool {
        self.connections.is_empty()
    }
}

/// Runs the flush runtime event wakeup operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn flush_runtime_event_wakeup<S>(
    connections: &mut RuntimeEventConnectionTable,
    wakeup: &RuntimeEventWakeup,
    sink: &mut S,
) -> Result<usize>
where
    S: RuntimeEventFanoutSink,
{
    let mut delivered = 0usize;
    for event in &wakeup.events {
        let notification = encode_event_notification(event);
        let frame = encode_control_body(&notification);
        sink.send_frame(&wakeup.connection_id, &frame)?;
        connections.mark_delivered(&wakeup.connection_id, event.id)?;
        delivered += 1;
    }
    Ok(delivered)
}

/// Runs the flush runtime event wakeups operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn flush_runtime_event_wakeups<S>(
    connections: &mut RuntimeEventConnectionTable,
    wakeups: &[RuntimeEventWakeup],
    sink: &mut S,
) -> Result<usize>
where
    S: RuntimeEventFanoutSink,
{
    let mut delivered = 0usize;
    for wakeup in wakeups {
        delivered += flush_runtime_event_wakeup(connections, wakeup, sink)?;
    }
    Ok(delivered)
}

/// Carries Pane Descriptor state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PaneDescriptor {
    /// Stores the window id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) window_id: WindowId,
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_id: PaneId,
    /// Stores the size value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) size: Size,
}

/// Carries Blocked Agent Approval Ref state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BlockedAgentApprovalRef {
    /// Stores the turn id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) turn_id: String,
    /// Stores the action id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) action_id: String,
}

/// Carries Running Shell Transaction Ref state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RunningShellTransactionRef {
    /// Stores the turn id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) turn_id: String,
    /// Stores the kind value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) kind: RunningShellTransactionKind,
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_id: String,
    /// Stores the command value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) command: String,
    /// Stores the started at unix ms value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) started_at_unix_ms: u64,
    /// Stores the timeout ms value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) timeout_ms: Option<u64>,
    /// Pane input payload that must be sent after the transaction start marker.
    ///
    /// Large generated command bodies are streamed after the wrapper receiver
    /// starts so they are consumed as data rather than parsed as shell source.
    pub(super) pending_input_payload: Option<Vec<u8>>,
    /// Stores the observed output bytes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) observed_output_bytes: usize,
    /// Stores the observed output preview value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) observed_output_preview: String,
    /// Stores the observed output truncated value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) observed_output_truncated: bool,
}

/// Carries Running Shell Transaction Kind state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RunningShellTransactionKind {
    /// Represents the Agent Action case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    AgentAction {
        /// Stores the action id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        action_id: String,
    },
    /// Represents the Readiness Probe case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ReadinessProbe,
    /// Represents the Bootstrap case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Bootstrap,
}

/// Timer-visible kind for a live shell transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeShellTransactionTimerKind {
    /// Agent shell command action timeout.
    AgentAction,
    /// Readiness probe timeout.
    ReadinessProbe,
    /// Pane bootstrap timeout.
    Bootstrap,
    /// Focused-shell hook marker timeout.
    FocusedShellHook,
}

/// Timer-visible snapshot of a live shell transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeShellTransactionTimerRef {
    /// Unique transaction marker used as the timer owner identity.
    pub marker: String,
    /// Timeout family to schedule.
    pub kind: RuntimeShellTransactionTimerKind,
    /// Unix timestamp in milliseconds when the transaction started.
    pub started_at_unix_ms: u64,
    /// Timeout duration in milliseconds.
    pub timeout_ms: u64,
}

/// Runtime-owned failure payload used to settle a shell action whose external
/// shell transaction could not complete normally.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct RuntimeShellTransactionActionFailure {
    /// Runtime action id for the MAAP shell command being failed.
    pub(super) action_id: String,
    /// Terminal action status to report to the MAAP action result.
    pub(super) status: ActionStatus,
    /// Stable machine-readable failure code for the action error object.
    pub(super) code: String,
    /// User-facing failure message rendered into the pane and transcript.
    pub(super) message: String,
    /// Whether the shell command itself was sent to the pane before failure.
    pub(super) sent_to_pane: bool,
    /// Structured timeout or observation data attached to the action result.
    pub(super) terminal_observation: serde_json::Value,
    /// Trace-level reason used for state-transition diagnostics.
    pub(super) trace_reason: String,
}

/// Carries Pending Focused Shell Hook Transaction state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PendingFocusedShellHookTransaction {
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_id: String,
    /// Stores the plan value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) plan: HookExecutionPlan,
    /// Stores the started at unix ms value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) started_at_unix_ms: u64,
    /// Stores the timeout ms value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) timeout_ms: u64,
    /// Stores the continuation value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) continuation: Option<PendingFocusedShellHookContinuation>,
}

/// Agent shell action suspended behind a blocking focused-shell pre-action hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PendingFocusedShellHookContinuation {
    /// Turn that owns the shell action waiting on the hook result.
    pub(super) turn_id: String,
    /// Action to resume or deny after the hook result is known.
    pub(super) action_id: String,
}

/// Completed pre-shell hook identity for a running action.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct RuntimeAgentPreShellHookCompletion {
    /// Turn whose pending action ran the hook.
    pub(super) turn_id: String,
    /// Shell action guarded by the hook.
    pub(super) action_id: String,
    /// Hook that has already completed for this action.
    pub(super) hook_id: String,
}

/// Outcome of evaluating pre-action hooks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RuntimeHookPipelineDecision {
    /// No blocking hook prevented the caller from continuing immediately.
    Continue,
    /// A hook failure policy blocked the action.
    Block(RuntimeHookPipelineBlock),
    /// A focused-shell hook was queued and the caller must resume later.
    Pending,
}

/// Carries Runtime Model Profile Override Store state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct RuntimeModelProfileOverrideStore {
    /// Stores the session profile value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) session_profile: Option<String>,
    /// Stores the window profiles value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) window_profiles: BTreeMap<String, String>,
    /// Stores the pane profiles value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_profiles: BTreeMap<String, String>,
    /// Stores the agent profiles value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) agent_profiles: BTreeMap<String, String>,
    /// Stores the subagent profiles value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) subagent_profiles: BTreeMap<String, String>,
}

/// User-defined pane personality profile.
///
/// Personality profiles are optional named overlays for pane-local agent
/// preferences. They never replace Mezzanine's built-in system prompt; instead
/// they append user-configured instructions and selected agent preferences.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct RuntimeAgentPersonalityProfile {
    /// Stable profile id from configuration.
    pub(super) id: String,
    /// Optional human-readable profile name.
    pub(super) name: Option<String>,
    /// Optional system-level instruction text appended after Mezzanine's base
    /// system prompt.
    pub(super) system_prompt: Option<String>,
    /// Optional response style preference.
    pub(super) response_style: Option<String>,
    /// Optional model profile override.
    pub(super) model_profile: Option<String>,
    /// Optional planning-mode override.
    pub(super) planning_enabled: Option<bool>,
    /// Optional routing override.
    pub(super) routing_enabled: Option<bool>,
}

/// Carries Runtime Model Profile Override Scope state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RuntimeModelProfileOverrideScope {
    /// Represents the Session case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Session,
    /// Represents the Window case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Window(String),
    /// Represents the Pane case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Pane(String),
    /// Represents the Agent case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Agent(String),
    /// Represents the Subagent case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Subagent(String),
}

/// Carries Runtime Mcp Transport Set state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Default)]
pub(super) struct RuntimeMcpTransportSet {
    /// Stores the transports value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) transports: BTreeMap<String, RuntimeMcpTransport>,
}

/// Carries Runtime Mcp Retry Report state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RuntimeMcpRetryReport {
    /// Stores the server id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) server_id: String,
    /// Stores the previous status value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) previous_status: McpServerStatus,
    /// Stores the status value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) status: McpServerStatus,
    /// Stores the retryable before retry value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) retryable_before_retry: bool,
    /// Stores the rediscovered value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) rediscovered: bool,
    /// Stores the tools value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) tools: usize,
    /// Stores the reason value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) reason: Option<String>,
}

impl RuntimeMcpRetryReport {
    /// Runs the previous status name operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn previous_status_name(&self) -> &'static str {
        runtime_mcp_status_name(self.previous_status)
    }

    /// Runs the status name operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn status_name(&self) -> &'static str {
        runtime_mcp_status_name(self.status)
    }
}

/// Carries Runtime Mcp Transport state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
pub(super) enum RuntimeMcpTransport {
    /// Represents the Stdio case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Stdio(McpStdioConnection),
    /// Represents the Streamable Http case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    StreamableHttp(RuntimeHttpMcpTransportState),
}

/// Carries Runtime Http Mcp Transport State state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RuntimeHttpMcpTransportState {
    /// Stores the startup plan value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) startup_plan: McpStartupPlan,
    /// Stores the session id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) session_id: Option<String>,
    /// Stores the next request id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) next_request_id: u64,
}

/// Carries Runtime Hook Pipeline Block state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RuntimeHookPipelineBlock {
    /// Stores the hook id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) hook_id: String,
    /// Stores the event value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) event: HookEvent,
    /// Stores the failure kind value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) failure_kind: HookFailureKind,
    /// Stores the message value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) message: String,
}

impl RuntimeMcpTransportSet {
    /// Runs the clear operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn clear(&mut self) {
        self.transports.clear();
    }

    /// Runs the clear counted operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn clear_counted(&mut self) -> usize {
        let count = self.transports.len();
        self.clear();
        count
    }

    /// Runs the insert stdio operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn insert_stdio(&mut self, server_id: String, connection: McpStdioConnection) {
        self.transports
            .insert(server_id, RuntimeMcpTransport::Stdio(connection));
    }

    /// Runs the insert streamable http operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn insert_streamable_http(
        &mut self,
        server_id: String,
        state: RuntimeHttpMcpTransportState,
    ) {
        self.transports
            .insert(server_id, RuntimeMcpTransport::StreamableHttp(state));
    }

    /// Runs the remove operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn remove(&mut self, server_id: &str) {
        self.transports.remove(server_id);
    }

    /// Runs the call tool operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn call_tool(
        &mut self,
        plan: &McpToolCallPlan,
        _environment: &BTreeMap<String, String>,
    ) -> Result<McpToolCallResponse> {
        Err(MezError::invalid_state(format!(
            "MCP server `{}` requires the async runtime tool execution path",
            plan.server_id
        )))
    }

    /// Runs the call tool async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) async fn call_tool_async(
        &mut self,
        plan: &McpToolCallPlan,
        environment: &BTreeMap<String, String>,
    ) -> Result<McpToolCallResponse> {
        let transport = self.transports.get_mut(&plan.server_id).ok_or_else(|| {
            MezError::invalid_state(format!(
                "MCP server `{}` has no owned runtime transport",
                plan.server_id
            ))
        })?;
        match transport {
            RuntimeMcpTransport::Stdio(connection) => connection.call_tool(plan).await,
            RuntimeMcpTransport::StreamableHttp(state) => {
                let request_id = state.next_request_id;
                state.next_request_id = state.next_request_id.saturating_add(1);
                let request = plan.json_rpc_request(request_id)?;
                let response = execute_streamable_http_exchange(
                    &state.startup_plan,
                    environment,
                    &request,
                    Some(request_id),
                    plan.timeout_ms,
                    state.session_id.as_deref(),
                )
                .await?;
                if response.session_id.is_some() {
                    state.session_id = response.session_id.clone();
                }
                parse_mcp_tools_call_response(&response.protocol_body, request_id)
            }
        }
    }
}

/// Runs the runtime mcp status name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_mcp_status_name(status: McpServerStatus) -> &'static str {
    match status {
        McpServerStatus::Configured => "configured",
        McpServerStatus::Starting => "starting",
        McpServerStatus::Available => "available",
        McpServerStatus::Unavailable => "unavailable",
        McpServerStatus::Blacklisted => "blacklisted",
        McpServerStatus::Failed => "failed",
    }
}

impl std::fmt::Debug for RuntimeMcpTransportSet {
    /// Runs the fmt operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeMcpTransportSet")
            .field("server_count", &self.transports.len())
            .finish()
    }
}

/// Carries Runtime Provider Registry state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeProviderRegistry {
    /// Stores the default profile value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) default_profile: Option<String>,
    /// Stores the providers value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) providers: BTreeMap<String, RuntimeProviderConfig>,
    /// Stores the profiles value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) profiles: BTreeMap<String, ModelProfile>,
    /// Stores the fallback profiles value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) fallback_profiles: BTreeMap<String, Vec<String>>,
}

/// Carries Runtime Provider Config state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeProviderConfig {
    /// Stores the provider id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub provider_id: String,
    /// Stores the kind value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub kind: String,
    /// Stores the auth profile value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub auth_profile: String,
    /// Stores the base url value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub base_url: Option<String>,
    /// Stores the models value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub models: Vec<String>,
    /// Stores the default model value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub default_model: Option<String>,
    /// Stores the options value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub options: BTreeMap<String, String>,
}

impl RuntimeProviderRegistry {
    /// Runs the default profile name operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn default_profile_name(&self) -> Option<&str> {
        self.default_profile.as_deref()
    }

    /// Runs the provider operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn provider(&self, provider_id: &str) -> Option<&RuntimeProviderConfig> {
        self.providers.get(provider_id)
    }

    /// Runs the profile operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn profile(&self, profile_name: &str) -> Option<&ModelProfile> {
        self.profiles.get(profile_name)
    }

    /// Runs the resolve profile operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn resolve_profile(&self, profile_name: &str) -> Result<ModelProfile> {
        self.profile(profile_name).cloned().ok_or_else(|| {
            MezError::config(format!("model profile `{profile_name}` is not configured"))
        })
    }

    /// Runs the providers operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn providers(&self) -> &BTreeMap<String, RuntimeProviderConfig> {
        &self.providers
    }

    /// Runs the profiles operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn profiles(&self) -> &BTreeMap<String, ModelProfile> {
        &self.profiles
    }

    /// Runs the safe fallback profiles operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn safe_fallback_profiles(&self, profile_name: &str) -> Result<Vec<String>> {
        let preferred = self.resolve_profile(profile_name)?;
        let Some(fallbacks) = self.fallback_profiles.get(profile_name) else {
            return Ok(Vec::new());
        };
        let mut safe = Vec::new();
        for fallback_name in fallbacks {
            let fallback = self.resolve_profile(fallback_name)?;
            if preferred.failover_safe(&fallback) {
                safe.push(fallback_name.clone());
            }
        }
        Ok(safe)
    }
}

/// Carries Runtime Model Preset state for this subsystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeModelPreset {
    /// Primary model profile to use.
    pub default_model_profile: String,
    /// Auto-sizing router model profile.
    pub auto_sizing_router_model_profile: String,
    /// Auto-sizing small model profile.
    pub auto_sizing_small_model_profile: String,
    /// Auto-sizing medium model profile.
    pub auto_sizing_medium_model_profile: String,
    /// Auto-sizing large model profile.
    pub auto_sizing_large_model_profile: String,
    /// Reasoning efforts allowed for auto-sizing.
    pub allowed_reasoning_efforts: Vec<String>,
}

/// Carries Runtime Preset Registry state for this subsystem.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimePresetRegistry {
    /// Named model presets keyed by preset identity.
    pub presets: BTreeMap<String, RuntimeModelPreset>,
}

impl RuntimePresetRegistry {
    /// Returns true when at least one preset is defined.
    pub fn has_presets(&self) -> bool {
        !self.presets.is_empty()
    }

    /// Resolves a preset by name.
    pub fn resolve(&self, name: &str) -> Option<&RuntimeModelPreset> {
        self.presets.get(name)
    }
}

/// Carries Runtime Agent Provider Task state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeAgentProviderTask {
    /// Stores the turn id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub turn_id: String,
    /// Stores the agent id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub agent_id: String,
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_id: String,
    /// Stores the model profile value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub model_profile: ModelProfile,
}

/// Runtime fallback behavior for automatic turn model sizing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeAutoSizingFallbackPolicy {
    /// Continue with the ordinary active profile when the router cannot produce
    /// a valid decision.
    UseDefaultProfile,
}

impl RuntimeAutoSizingFallbackPolicy {
    /// Returns the stable configuration name for this fallback policy.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UseDefaultProfile => DEFAULT_AUTO_SIZING_FALLBACK_POLICY,
        }
    }
}

/// Configured profile names used by automatic turn model sizing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeAutoSizingConfig {
    /// Model profile used for the internal routing decision.
    pub router_model_profile: String,
    /// Model profile used when the router chooses the small bucket.
    pub small_model_profile: String,
    /// Model profile used when the router chooses the medium bucket.
    pub medium_model_profile: String,
    /// Model profile used when the router chooses the large bucket.
    pub large_model_profile: String,
    /// Reasoning efforts the router may select.
    pub allowed_reasoning_efforts: Vec<String>,
    /// Fallback behavior used when routing fails.
    pub fallback_policy: RuntimeAutoSizingFallbackPolicy,
}

impl Default for RuntimeAutoSizingConfig {
    /// Returns the generated automatic sizing defaults.
    fn default() -> Self {
        Self {
            router_model_profile: DEFAULT_AUTO_SIZING_ROUTER_PROFILE.to_string(),
            small_model_profile: DEFAULT_AUTO_SIZING_SMALL_PROFILE.to_string(),
            medium_model_profile: DEFAULT_AUTO_SIZING_MEDIUM_PROFILE.to_string(),
            large_model_profile: DEFAULT_AUTO_SIZING_LARGE_PROFILE.to_string(),
            allowed_reasoning_efforts: vec![
                "low".to_string(),
                "medium".to_string(),
                "high".to_string(),
                "xhigh".to_string(),
            ],
            fallback_policy: RuntimeAutoSizingFallbackPolicy::UseDefaultProfile,
        }
    }
}

/// Resolved target profile metadata included in an automatic sizing dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeAutoSizingTargetProfile {
    /// Size bucket name visible to the router.
    pub size: String,
    /// Configured model profile identity for this bucket.
    pub profile_name: String,
    /// Resolved model profile copied when the bucket is chosen.
    pub profile: ModelProfile,
    /// Reasoning efforts known to be valid for this model, when configured.
    pub supported_reasoning_efforts: Vec<String>,
}

/// Bounded internal routing context carried to the provider worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeAutoSizingDispatch {
    /// Router profile identity.
    pub router_profile_name: String,
    /// Router model profile used for the internal decision request.
    pub router_profile: ModelProfile,
    /// Ordinary active profile identity to use when routing is disabled or
    /// falls back.
    pub default_profile_name: String,
    /// Ordinary active model profile to use when routing is disabled or
    /// falls back.
    pub default_profile: ModelProfile,
    /// Small target profile.
    pub small: RuntimeAutoSizingTargetProfile,
    /// Medium target profile.
    pub medium: RuntimeAutoSizingTargetProfile,
    /// Large target profile.
    pub large: RuntimeAutoSizingTargetProfile,
    /// Optional bounded turn metadata, such as subagent scope and lineage.
    pub turn_metadata: Option<String>,
    /// Reasoning efforts the router may select.
    pub allowed_reasoning_efforts: Vec<String>,
    /// Fallback behavior used when routing fails.
    pub fallback_policy: RuntimeAutoSizingFallbackPolicy,
}

/// Parsed automatic sizing decision returned by the router model.
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeAutoSizingDecision {
    /// Chosen size bucket.
    pub size: String,
    /// Chosen reasoning effort.
    pub reasoning_effort: String,
    /// Router confidence in the decision.
    pub confidence: f64,
    /// Short non-secret explanation suitable for logs.
    pub rationale: String,
}

/// Tracks a provider task after the async actor has claimed it from the queue.
///
/// Provider workers run outside the serialized runtime actor. This record gives
/// the actor a finite lease it can enforce if the worker never submits a
/// completion or failure event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RuntimeAgentProviderClaim {
    /// Runtime turn owned by the claimed provider worker.
    pub turn_id: String,
    /// Agent identity that owns the turn.
    pub agent_id: String,
    /// Timer generation associated with the current claim lease.
    pub generation: u64,
    /// Unix timestamp, in milliseconds, when the provider task was claimed.
    pub claimed_at_unix_ms: u64,
    /// Maximum lease duration before the runtime fails the turn.
    pub timeout_ms: u64,
}

/// Carries Runtime Agent Provider Dispatch Provider state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone)]
pub enum RuntimeAgentProviderDispatchProvider {
    /// Represents the Open Ai case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    OpenAi(OpenAiResponsesProvider<ReqwestProviderHttpTransport>),
    /// Represents the Deep Seek case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    DeepSeek(ChatCompletionsProvider<ReqwestProviderHttpTransport>),
    /// Represents a named OpenAI-compatible Chat Completions provider.
    ///
    /// Callers use this variant for configured provider instances that share
    /// the Chat Completions wire contract without inheriting native OpenAI
    /// Responses semantics.
    OpenAiCompatible(ChatCompletionsProvider<ReqwestProviderHttpTransport>),
}

impl RuntimeAgentProviderDispatchProvider {
    /// Runs the provider id operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn provider_id(&self) -> &'static str {
        match self {
            Self::OpenAi(_) => "openai",
            Self::DeepSeek(_) => "deepseek",
            Self::OpenAiCompatible(_) => "openai-compatible",
        }
    }
}

/// Carries Runtime Agent Provider Dispatch state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone)]
pub struct RuntimeAgentProviderDispatch {
    /// Stores the turn value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub turn: AgentTurnRecord,
    /// Stores the context value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub context: AgentContext,
    /// Stores the model profile value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub model_profile: ModelProfile,
    /// Optional automatic sizing context for the worker's first provider step.
    pub auto_sizing: Option<RuntimeAutoSizingDispatch>,
    /// Optional router provider for auto-sizing when different from the main
    /// turn provider. When set, auto-sizing requests use this provider.
    pub auto_sizing_provider: Option<RuntimeAgentProviderDispatchProvider>,
    /// Stores the provider value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub provider: RuntimeAgentProviderDispatchProvider,
    /// Stores the permission policy value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub permission_policy: PermissionPolicy,
    /// Stores the session approvals value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub session_approvals: SessionApprovalStore,
    /// Stores the path scopes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub path_scopes: Option<PathScopes>,
    /// Stores the subagent scope value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub subagent_scope: Option<SubagentScopeDeclaration>,
    /// Stores the available mcp servers value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub available_mcp_servers: Vec<String>,
    /// Stores the available mcp tools value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub available_mcp_tools: Vec<McpPromptTool>,
}

/// Provider-backed conversation compaction queued outside the actor.
///
/// The actor keeps pane state serialized while provider I/O runs in a worker.
/// This task carries the deterministic request and transcript-retention
/// metadata needed to finish compaction once a model response returns.
#[derive(Debug, Clone)]
pub struct RuntimeAgentCompactionTask {
    /// Pane whose visible status should remain `compacting`.
    pub pane_id: String,
    /// Conversation being summarized.
    pub conversation_id: String,
    /// User-visible source such as `manual` or `auto`.
    pub source: String,
    /// Transcript entry count before compaction.
    pub transcript_entries: u64,
    /// Raw recent transcript entries to retain after summary insertion.
    pub retained_transcript_entries: u64,
    /// Durable entries supplied to the model compactor.
    pub summarized_entries: usize,
    /// Active model profile name used for the compactor request.
    pub model_profile_name: String,
    /// Active model profile copied for completion metadata.
    pub model_profile: ModelProfile,
    /// Provider request submitted by the async compaction worker.
    pub request: ModelRequest,
}

/// Claimed model compaction dispatch owned by an async provider worker.
#[derive(Debug, Clone)]
pub struct RuntimeAgentCompactionDispatch {
    /// Compaction task metadata and provider request.
    pub task: RuntimeAgentCompactionTask,
    /// Provider used to execute the compaction request.
    pub provider: RuntimeAgentProviderDispatchProvider,
}

/// Carries Runtime Session Service state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug)]
pub struct RuntimeSessionService {
    /// Stores the session value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) session: Session,
    /// Stores the window created at unix seconds value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) window_created_at_unix_seconds: BTreeMap<String, u64>,
    /// Stores the config layers value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) config_layers: Vec<ConfigLayer>,
    /// Stores the config root value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) config_root: Option<PathBuf>,
    /// Stores the control idempotency value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) control_idempotency: ControlIdempotencyCache,
    /// Stores the message service value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) message_service: MessageService,
    /// Stores the pane processes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_processes: PaneProcessManager,
    /// Stores the async owned pane processes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) async_owned_pane_processes: BTreeMap<String, u32>,
    /// Stores the latest async runtime actor metrics snapshot when available.
    ///
    /// The actor-owned command path updates this snapshot before rendering
    /// `show-metrics` so runtime display helpers can present metrics without
    /// taking a direct dependency on actor internals.
    pub(super) async_runtime_metrics: Option<crate::async_runtime::AsyncRuntimeActorMetrics>,
    /// Stores runtime-owned agent, provider, and shell diagnostics.
    ///
    /// These counters and histograms are updated from the serialized runtime
    /// service path so `show-metrics` can expose prompt-cache shape, provider
    /// usage, turn lifecycle, and shell-transaction behavior without parsing
    /// trace logs.
    pub(super) runtime_metrics: RuntimeMetricsSnapshot,
    /// Stores the pane current working directories value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_current_working_directories: BTreeMap<String, PathBuf>,
    /// Stores the deferred pane inputs value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) deferred_pane_inputs: Vec<DeferredPaneInput>,
    /// Stores the deferred pane resizes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) deferred_pane_resizes: BTreeMap<String, DeferredPaneResize>,
    /// Stores the deferred pane terminations value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) deferred_pane_terminations: BTreeMap<String, DeferredPaneTermination>,
    /// Stores the deferred pane pipe writes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) deferred_pane_pipe_writes: Vec<DeferredPanePipeWrite>,
    /// Stores the deferred audit writes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) deferred_audit_writes: Vec<AuditDeferredWrite>,
    /// Stores the deferred agent transcript writes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) deferred_agent_transcript_writes: Vec<DeferredAgentTranscriptWrite>,
    /// Stores the deferred agent prompt history writes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) deferred_agent_prompt_history_writes: Vec<DeferredAgentPromptHistoryWrite>,
    /// Stores the deferred command prompt history writes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) deferred_command_prompt_history_writes: Vec<DeferredCommandPromptHistoryWrite>,
    /// Stores the deferred config file writes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) deferred_config_file_writes: Vec<DeferredConfigFileWrite>,
    /// Stores the deferred project config writes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) deferred_project_config_writes: Vec<DeferredProjectConfigWrite>,
    /// Stores the deferred project instruction writes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) deferred_project_instruction_writes: Vec<DeferredProjectInstructionWrite>,
    /// Stores the deferred transcript next sequences value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) deferred_transcript_next_sequences: BTreeMap<String, u64>,
    /// Stores the pane screens value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_screens: BTreeMap<String, TerminalScreen>,
    /// Stores the pane transaction osc screens value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_transaction_osc_screens: BTreeMap<String, TerminalScreen>,
    /// Stores hidden agent-shell OSC parser fragments for each pane.
    ///
    /// Hidden agent-shell output is command data, not user-visible terminal
    /// traffic. The runtime keeps only bounded fragments that may contain a
    /// split Mezzanine transaction marker so large file-read bodies never have
    /// to pass through the full terminal-screen parser.
    pub(super) pane_transaction_osc_pending: BTreeMap<String, Vec<u8>>,
    /// Stores the pane mez wrapper filter pending value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_mez_wrapper_filter_pending: BTreeMap<String, Vec<u8>>,
    /// Stores the pane mez wrapper filter recent commands value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_mez_wrapper_filter_recent_commands: BTreeMap<String, Vec<String>>,
    /// Stores the pane mez wrapper filter recent polls value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_mez_wrapper_filter_recent_polls: BTreeMap<String, usize>,
    /// Stores the pane hidden shell render recent polls value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_hidden_shell_render_recent_polls: BTreeMap<String, usize>,
    /// Stores the foreground title idle sync polls value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) foreground_title_idle_sync_polls: usize,
    /// Stores the pane exit records value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_exit_records: BTreeMap<String, PaneExitRecord>,
    /// Stores the active pane pipes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) active_pane_pipes: BTreeMap<String, ActivePanePipe>,
    /// Stores the defer file pane pipe writes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) defer_file_pane_pipe_writes: bool,
    /// Stores the defer command pane pipe startup value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) defer_command_pane_pipe_startup: bool,
    /// Stores the paste buffers value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) paste_buffers: PasteBuffers,
    /// Stores the active paste buffer value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) active_paste_buffer: Option<String>,
    /// Stores the host clipboard value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) host_clipboard: HostClipboard,
    /// Stores the active copy modes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) active_copy_modes: BTreeMap<String, CopyMode>,
    /// Panes currently using copy-mode storage as transient mouse scrollback.
    ///
    /// Keyboard copy-mode is an explicit modal workflow; mouse-wheel
    /// scrollback is only a temporary viewport offset and should return to the
    /// live pane on the next key press.
    pub(super) scrollback_copy_mode_panes: BTreeSet<String>,
    /// Stores the mouse resize drag state value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) mouse_resize_drag_state: Option<MouseResizeDragState>,
    /// Stores the mouse selection drag state value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) mouse_selection_drag_state: Option<MouseSelectionDragState>,
    /// Stores the pressed window status-bar action value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pressed_window_action: Option<WindowFrameAction>,
    /// Stores the pane transcript refs value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_transcript_refs: BTreeMap<String, Vec<String>>,
    /// Stores the terminal history limit value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) terminal_history_limit: usize,
    /// Stores the terminal history rotation line count value for this data
    /// structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) terminal_history_rotate_lines: usize,
    /// Stores the terminal term value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) terminal_term: String,
    /// Stores the window frames enabled value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) window_frames_enabled: bool,
    /// Stores the window frame template value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) window_frame_template: String,
    /// Stores the window frame right status template value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) window_frame_right_status_template: String,
    /// Stores the window frame position value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) window_frame_position: TerminalFramePosition,
    /// Stores the window frame style value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) window_frame_style: TerminalFrameStyle,
    /// Stores the window frame visible fields value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) window_frame_visible_fields: Vec<String>,
    /// Stores the pane frames enabled value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_frames_enabled: bool,
    /// Stores the pane frame template value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_frame_template: String,
    /// Stores the pane frame position value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_frame_position: TerminalFramePosition,
    /// Stores the pane frame style value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_frame_style: TerminalFrameStyle,
    /// Stores the pane frame visible fields value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_frame_visible_fields: Vec<String>,
    /// Stores the terminal cursor style value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) terminal_cursor_style: TerminalCursorStyle,
    /// Stores the terminal cursor blink value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) terminal_cursor_blink: bool,
    /// Stores the terminal cursor blink interval ms value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) terminal_cursor_blink_interval_ms: u64,
    /// Stores the terminal resize debounce ms value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) terminal_resize_debounce_ms: u64,
    /// Stores the terminal render rate limit fps value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) terminal_render_rate_limit_fps: u64,
    /// Stores whether optional terminal animations should be disabled.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) terminal_reduced_motion: bool,
    /// Stores the terminal clipboard value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) terminal_clipboard: String,
    /// Stores the ui theme value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) ui_theme: UiTheme,
    /// Stores the key bindings value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) key_bindings: KeyBindings,
    /// Stores the command bindings value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) command_bindings: BTreeMap<KeyChord, RuntimeCommandBinding>,
    /// Stores the permission policy value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) permission_policy: PermissionPolicy,
    /// Stores an explicit live approval-bypass override selected by the user.
    ///
    /// Configuration is intentionally unable to enable approval bypass, so
    /// explicit runtime activation must survive unrelated configuration
    /// reloads without being encoded into normal config layers.
    pub(super) live_approval_bypass_override: Option<bool>,
    /// Stores an explicit live approval-policy override selected by the user.
    ///
    /// Runtime approval changes are session choices. They must survive unrelated
    /// configuration reloads without being erased by persistent config changes.
    pub(super) live_approval_policy_override: Option<crate::permissions::ApprovalPolicy>,
    /// Stores the blocked approvals value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) blocked_approvals: BlockedApprovalQueue,
    /// Stores the session approvals value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) session_approvals: SessionApprovalStore,
    /// Stores the session memory value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) session_memory: SessionMemoryStore,
    /// Stores the mcp registry value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) mcp_registry: McpRegistry,
    /// Stores the mcp transports value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) mcp_transports: RuntimeMcpTransportSet,
    /// Stores the provider registry value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) provider_registry: RuntimeProviderRegistry,
    /// Stores the preset registry value for this data structure.
    pub(super) preset_registry: RuntimePresetRegistry,
    /// Stores the subagent profiles value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) subagent_profiles: BTreeMap<String, SubagentProfile>,
    /// User-defined pane personality profiles.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) agent_personality_profiles: BTreeMap<String, RuntimeAgentPersonalityProfile>,
    /// Configured default personality profile id, when one exists.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) default_agent_personality: Option<String>,
    /// User-configured system prompt text appended after the built-in prompt.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) custom_agent_system_prompt: Option<String>,
    /// Pane-local selected personality profile ids.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) agent_personality_selections: BTreeMap<String, String>,
    /// Stores the model profile overrides value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) model_profile_overrides: RuntimeModelProfileOverrideStore,
    /// Stores the auth store value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) auth_store: Option<AuthStore>,
    /// Stores the audit log value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) audit_log: Option<AuditLog>,
    /// Stores the defer audit writes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) defer_audit_writes: bool,
    /// Stores the agent scheduler value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) agent_scheduler: AgentScheduler,
    /// Stores the agent shell store value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) agent_shell_store: AgentShellStore,
    /// Retains bounded per-pane agent trace lines independent of visible log level.
    ///
    /// The runtime uses this ring as a diagnostics escape hatch so normal-mode
    /// sessions can later dump recent trace context without enabling noisy
    /// trace logging up front.
    pub(super) agent_pane_trace_logs: BTreeMap<String, Vec<String>>,
    /// Retained patch payloads keyed by pane-local agent session id.
    ///
    /// This lets `/copy-patches` export exact patch attempts and outcomes from
    /// the current session without depending on rendered pane text or compact
    /// transcript summaries.
    pub(super) agent_session_patch_records: BTreeMap<String, Vec<RuntimeAgentPatchRecord>>,
    /// Tracks panes whose visible agent shell is scoped to a child shell.
    ///
    /// The runtime uses this ephemeral set to exit the child shell cleanly when
    /// agent mode hides, while keeping prompt and environment mutations away
    /// from the user's original interactive shell.
    pub(super) agent_subshell_panes: BTreeSet<String>,
    /// Tracks agent subshells that should exit with a command line after an
    /// interrupted shell transaction.
    ///
    /// EOF can be consumed by a transaction wrapper that is still unwinding
    /// after Ctrl+C. These panes use a line-oriented `exit` command instead so
    /// the parent shell is restored when the interrupted wrapper returns.
    pub(super) agent_subshell_command_exit_panes: BTreeSet<String>,
    /// Stores the agent turn ledger value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) agent_turn_ledger: AgentTurnLedger,
    /// Stores the agent turn contexts value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) agent_turn_contexts: BTreeMap<String, AgentContext>,
    /// Stores the agent turn executions value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) agent_turn_executions: BTreeMap<String, AgentTurnExecution>,
    /// User steering prompts waiting to be incorporated into an active turn.
    ///
    /// Input submitted while a turn is already running cannot alter a provider
    /// request that has already been dispatched. These entries are drained into
    /// the next provider-bound context so the same turn can incorporate the new
    /// instruction before taking further action.
    pub(super) agent_turn_pending_steering: BTreeMap<String, Vec<RuntimeAgentTurnSteering>>,
    /// Counts bounded model self-correction attempts after real action failures.
    ///
    /// Failure feedback is scoped to a turn so provider continuations can give
    /// the model one chance to recover from command/tool failures without
    /// creating an unbounded retry loop.
    pub(super) agent_turn_failure_feedback_attempts: BTreeMap<String, usize>,
    /// Stores the configured retry budget for model-correctable action failures.
    ///
    /// The value is applied per stable failed-action signature so identical
    /// failures cannot loop forever while distinct failures in the same turn
    /// still receive bounded correction opportunities.
    pub(super) agent_action_failure_retry_limit: usize,
    /// Stores the configured successful shell-command streak that triggers a
    /// soft action-pressure hint during one active turn.
    ///
    /// The runtime uses this as advisory context only; it must not block shell
    /// execution because legitimate audits can require long inspection runs.
    pub(super) agent_implementation_pressure_after_shell_actions: usize,
    /// Stores the agent turn shell dispatch history value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) agent_turn_shell_dispatch_history:
        BTreeMap<String, RuntimeAgentShellDispatchHistory>,
    /// Stores the agent turn network action history value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) agent_turn_network_action_history:
        BTreeMap<String, RuntimeAgentNetworkActionHistory>,
    /// Stores the agent pre shell hook completions value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) agent_pre_shell_hook_completions: BTreeSet<RuntimeAgentPreShellHookCompletion>,
    /// Stores the latest agent say output retained for copy commands.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) agent_copy_outputs: BTreeMap<String, RuntimeAgentCopyOutput>,
    /// File modifications observed from successful agent patch actions.
    ///
    /// The outer key is the pane id, and the inner map is keyed by repository
    /// relative display path so `/list-modified-files` can summarize the
    /// current conversation without re-reading the working tree.
    pub(super) agent_modified_files:
        BTreeMap<String, BTreeMap<String, RuntimeAgentModifiedFileSummary>>,
    /// Submitted primary command-prompt history retained across prompt openings.
    ///
    /// The `Ctrl+A :` command prompt uses this cache for readline navigation and
    /// reverse search without mixing mux commands into agent prompt history.
    pub(super) primary_command_prompt_history: Vec<String>,
    /// Stores the primary prompt input value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) primary_prompt_input: Option<RuntimePrimaryPromptInput>,
    /// Whether the primary client's next key should use the prefix table.
    ///
    /// This transient state is set by a lone escape key and consumed by the
    /// next attached-terminal input action.
    pub(super) primary_prefix_key_pending: bool,
    /// Stores the agent prompt inputs value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) agent_prompt_inputs: BTreeMap<String, RuntimeAgentPromptInput>,
    /// Stores the primary display overlay value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) primary_display_overlay: Option<RuntimeDisplayOverlay>,
    /// Stores the transient primary error status overlay value.
    ///
    /// Recoverable foreground errors use this one-line notice instead of the
    /// modal display overlay so the user's next input can both dismiss the
    /// notice and continue to the active pane or mux action.
    pub(super) primary_error_status_overlay: Option<String>,
    /// Stores the open pane agent status selector value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_agent_status_selector: Option<RuntimePaneAgentStatusSelector>,
    /// Stores the agent turn model profiles value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) agent_turn_model_profiles: BTreeMap<String, ModelProfile>,
    /// Stores the agent planning modes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) agent_planning_modes: BTreeSet<String>,
    /// Stores the agent response styles value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) agent_response_styles: BTreeMap<String, String>,
    /// Percent of the active model context retained as uncompacted raw tail.
    pub(super) agent_compaction_raw_retention_percent: usize,
    /// Panes currently running model-backed context compaction, keyed by start
    /// time for timer rendering.
    pub(super) agent_compacting_panes: BTreeMap<String, u64>,
    /// Model-backed compaction tasks waiting for async provider dispatch.
    pub(super) pending_agent_compaction_tasks: BTreeMap<String, RuntimeAgentCompactionTask>,
    /// Model-backed compaction tasks claimed by async provider workers.
    pub(super) claimed_agent_compaction_tasks: BTreeMap<String, RuntimeAgentCompactionTask>,
    /// Whether new agent turns use routing model and reasoning sizing by default.
    pub(super) agent_routing: bool,
    /// Pane-local routing overrides. Missing entries inherit the
    /// configured default.
    pub(super) agent_routing_overrides: BTreeMap<String, bool>,
    /// Automatic sizing profile and fallback configuration.
    pub(super) agent_auto_sizing: RuntimeAutoSizingConfig,
    /// Pane-local automatic sizing profile overrides selected through model
    /// presets. Missing entries inherit the configured default.
    pub(super) agent_auto_sizing_overrides: BTreeMap<String, RuntimeAutoSizingConfig>,
    /// Cumulative provider token usage keyed by conversation and provider/model.
    pub(super) agent_token_usage_by_conversation:
        BTreeMap<String, BTreeMap<ModelTokenUsageKey, ModelTokenUsage>>,
    /// Latest provider-response input context usage percentage keyed by
    /// conversation id for terminal rendering and legacy persistence.
    pub(super) agent_context_usage_by_conversation: BTreeMap<String, String>,
    /// Latest provider-response request-context snapshots keyed by
    /// conversation id.
    pub(super) agent_context_usage_snapshot_by_conversation:
        BTreeMap<String, crate::agent::AgentContextUsageSnapshot>,
    /// Latest provider quota usage percentages keyed by agent conversation id.
    pub(super) agent_quota_usage_by_conversation: BTreeMap<String, Vec<ProviderQuotaUsage>>,
    /// Latest live provider model catalogs keyed by provider id.
    pub(super) provider_model_catalog_cache: BTreeMap<String, super::commands::RuntimeModelCatalog>,
    /// Stores the pending agent provider tasks value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pending_agent_provider_tasks: BTreeSet<String>,
    /// Stores provider tasks claimed by async workers but not yet settled.
    ///
    /// Claimed tasks are removed from `pending_agent_provider_tasks`, so this
    /// map keeps running turns observable and gives the actor a timeout path if
    /// a worker fails to deliver a provider event.
    pub(super) claimed_agent_provider_tasks: BTreeMap<String, RuntimeAgentProviderClaim>,
    /// Stores the subagent task routes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) subagent_task_routes: BTreeMap<String, String>,
    /// Tracks windows reserved for subagent panes.
    ///
    /// Subagent windows are runtime-only placement buckets. They remain in the
    /// same user-visible group as the controlling pane, use an even layout, and
    /// are pruned from this set whenever the backing window disappears.
    pub(super) subagent_window_ids: BTreeSet<String>,
    /// Panes that should close once their terminal subagent turn fully
    /// finishes its normal terminal cleanup.
    pub(super) pending_terminal_subagent_pane_closes: BTreeSet<String>,
    /// Maximum number of subagent panes to place in one subagent window.
    ///
    /// This bound keeps helper panes readable by forcing a fresh background
    /// window once the configured bucket size is reached.
    pub(super) max_subagent_panes_per_window: usize,
    /// Maximum number of direct subagents a root pane agent may spawn.
    ///
    /// This caps the delegation width available to the user-facing root agent
    /// independently from scheduler concurrency and subagent window bucket
    /// capacity.
    pub(super) max_root_subagents: usize,
    /// Maximum number of direct subagents a child subagent may spawn.
    ///
    /// Child delegation uses a narrower branching factor so nested helper
    /// trees remain bounded even when the parent task is allowed to delegate.
    pub(super) max_subagents_per_subagent: usize,
    /// Maximum depth at which spawned subagents may create more children.
    ///
    /// Root pane agents are depth zero. A subagent at this depth may continue
    /// its own work but cannot spawn another generation.
    pub(super) max_subagent_depth: usize,
    /// Policy controlling whether parent turns wait for spawned subagents.
    ///
    /// Joined parents move to blocked scheduler state until all spawned child
    /// task results are available, preventing scheduler deadlocks while keeping
    /// provider continuation ordered after child output.
    pub(super) subagent_wait_policy: SubagentWaitPolicy,
    /// Child turns currently joined by parent `spawn_agent` actions.
    ///
    /// The map is keyed by child turn id so task-result delivery can resolve
    /// the exact parent action result that was waiting.
    pub(super) joined_subagent_dependencies: BTreeMap<String, JoinedSubagentDependency>,
    /// Stores the subagent scope declarations value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) subagent_scope_declarations: BTreeMap<String, SubagentScopeDeclaration>,
    /// Runtime-only delegation lineage for active spawned subagents.
    ///
    /// Entries are keyed by child agent id. Root pane agents are inferred when
    /// absent from this map, which keeps limits scoped to active child work.
    pub(super) subagent_lineage: BTreeMap<String, RuntimeSubagentLineage>,
    /// Stores the blocked agent approval refs value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) blocked_agent_approval_refs: BTreeMap<String, BlockedAgentApprovalRef>,
    /// Stores the running shell transactions value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) running_shell_transactions: BTreeMap<String, RunningShellTransactionRef>,
    /// Tracks live shell transactions whose wrapper start marker is mandatory.
    ///
    /// Runtime-dispatched transactions are sequenced by explicit wrapper start
    /// and end markers. Tests and migration fixtures may still construct
    /// transactions directly, so this set defines which live marker entries are
    /// subject to strict start-before-end validation.
    pub(super) shell_transaction_require_start_markers: BTreeSet<String>,
    /// Tracks live shell transactions whose wrapper start marker was observed.
    ///
    /// This state is intentionally separate from pending command payloads:
    /// stateful commands have no deferred payload but still must emit exactly
    /// one wrapper start marker before they can complete.
    pub(super) shell_transaction_started_markers: BTreeSet<String>,
    /// Tracks pane-local transient shell-output status rows for hidden agent
    /// shell commands.
    ///
    /// The row is display-only progress feedback: each new command-output line
    /// replaces the prior row, and the next durable agent transcript line clears
    /// it before writing its own content.
    pub(super) agent_shell_output_status_lines: BTreeMap<String, String>,
    /// Panes currently replaying durable agent presentation entries.
    ///
    /// Replay writes use the same terminal append primitives as live agent
    /// output, so this set prevents restored rows from being persisted again.
    pub(super) agent_presentation_replay_panes: BTreeSet<String>,
    /// Stores the pane readiness states value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_readiness_states: BTreeMap<String, PaneReadinessState>,
    /// Stores the pane readiness overrides value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_readiness_overrides: PaneReadinessOverrideStore,
    /// Stores the pane environment signatures value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_environment_signatures: BTreeMap<String, EnvironmentSignature>,
    /// Stores the pane bootstrap pending value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_bootstrap_pending: BTreeSet<String>,
    /// Stores the tool discovery cache value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) tool_discovery_cache: ToolDiscoveryCache,
    /// Stores the pane instruction files value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_instruction_files: BTreeMap<String, Vec<DiscoveredInstructionFile>>,
    /// Stores the pane closing value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_closing: BTreeSet<String>,
    /// Stores the agent transcript store value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) agent_transcript_store: Option<AgentTranscriptStore>,
    /// Stores the defer agent transcript writes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) defer_agent_transcript_writes: bool,
    /// Stores the defer config file writes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) defer_config_file_writes: bool,
    /// Stores the defer project config writes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) defer_project_config_writes: bool,
    /// Stores the defer project instruction writes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) defer_project_instruction_writes: bool,
    /// Stores the subagent scopes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) subagent_scopes: ScopeRegistry,
    /// Stores the project trust store value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) project_trust_store: Option<ProjectTrustStore>,
    /// Stores the project trust database path value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) project_trust_database_path: Option<PathBuf>,
    /// Stores the announced project trust roots value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) announced_project_trust_roots: BTreeSet<PathBuf>,
    /// Stores the hook definitions value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) hook_definitions: Vec<HookDefinition>,
    /// Stores the defer program hooks value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) defer_program_hooks: bool,
    /// Stores the deferred program hooks value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) deferred_program_hooks: Vec<DeferredProgramHook>,
    /// Stores the focused shell hooks value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) focused_shell_hooks: FocusedShellHookQueue,
    /// Stores the next focused shell hook marker value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) next_focused_shell_hook_marker: u64,
    /// Stores the focused shell hook transactions value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) focused_shell_hook_transactions:
        BTreeMap<String, PendingFocusedShellHookTransaction>,
    /// Stores the focused shell hook results value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) focused_shell_hook_results: Vec<HookExecutionResult>,
    /// Stores the event log value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) event_log: Option<EventLog>,
    /// Stores the lifecycle state value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) lifecycle_state: RuntimeLifecycleState,
    /// Stores the session registry value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) session_registry: Option<SessionRegistry>,
    /// Stores the defer registry updates value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) defer_registry_updates: bool,
    /// Stores the deferred registry update value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) deferred_registry_update: Option<RuntimeRegistryUpdatePlan>,
    /// Stores the socket path value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) socket_path: PathBuf,
    /// Stores the created at unix seconds value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) created_at_unix_seconds: u64,
    /// Stores the last attach at unix seconds value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) last_attach_at_unix_seconds: Option<u64>,
}

/// Carries Mouse Selection Drag State state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MouseSelectionDragState {
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_id: String,
    /// Stores the position value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub position: CopyPosition,
    /// Stores the origin position value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub origin_position: CopyPosition,
    /// Stores the autoscroll position value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub autoscroll_position: Option<CopyPosition>,
}

/// Carries Mouse Resize Drag State state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum MouseResizeDragState {
    /// Represents the Vertical case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Vertical {
        /// Stores the min column value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        min_column: u16,
        /// Stores the max column value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        max_column: u16,
        /// Stores the left indices value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        left_indices: Vec<usize>,
        /// Stores the right indices value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        right_indices: Vec<usize>,
        /// Stores the geometries value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        geometries: Vec<PaneGeometry>,
    },
    /// Represents the Horizontal case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Horizontal {
        /// Stores the min row value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        min_row: u16,
        /// Stores the max row value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        max_row: u16,
        /// Stores the row offset value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        row_offset: u16,
        /// Stores the top indices value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        top_indices: Vec<usize>,
        /// Stores the bottom indices value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        bottom_indices: Vec<usize>,
        /// Stores the geometries value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        geometries: Vec<PaneGeometry>,
    },
}

/// Carries Runtime Command Binding state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RuntimeCommandBinding {
    /// Stores the notation value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub notation: String,
    /// Stores the command value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub command: String,
    /// Stores the source layer value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub source_layer: String,
}

/// Carries Runtime Config Apply Report state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfigApplyReport {
    /// Stores the applied layers value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub applied_layers: Vec<String>,
    /// Stores the skipped layers value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub skipped_layers: Vec<String>,
    /// Stores the terminal history limit value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub terminal_history_limit: usize,
    /// Stores the terminal history rotation line count value for this data
    /// structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub terminal_history_rotate_lines: usize,
    /// Stores the terminal term value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub terminal_term: String,
    /// Stores the window frames enabled value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub window_frames_enabled: bool,
    /// Stores the pane frames enabled value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_frames_enabled: bool,
    /// Stores the max concurrent agents value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub max_concurrent_agents: usize,
    /// Stores the permission policy applied value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub permission_policy_applied: bool,
    /// Stores the mcp servers configured value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub mcp_servers_configured: usize,
    /// Stores the mcp servers blacklisted value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub mcp_servers_blacklisted: Vec<String>,
    /// Stores the providers configured value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub providers_configured: usize,
    /// Stores the model profiles configured value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub model_profiles_configured: usize,
    /// Stores the default model profile value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub default_model_profile: Option<String>,
    /// Stores the hooks configured value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub hooks_configured: usize,
    /// Stores the project trust prompts announced value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub project_trust_prompts_announced: usize,
    /// Stores the ui theme value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub ui_theme: String,
}

/// Carries Runtime Agent Prompt Turn Start state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeAgentPromptTurnStart {
    /// Stores the turn id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub turn_id: String,
    /// Stores the agent id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub agent_id: String,
    /// Stores the state value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub state: AgentTurnState,
    /// Stores the created at unix seconds value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub created_at_unix_seconds: u64,
    /// Stores the started at unix seconds value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub started_at_unix_seconds: Option<u64>,
    /// Stores the finished at unix seconds value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub finished_at_unix_seconds: Option<u64>,
    /// Stores the prompt preview value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub prompt_preview: String,
    /// Stores the approval ids value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub approval_ids: Vec<String>,
    /// Stores the result summary value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub result_summary: Option<String>,
    /// Stores the context blocks value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub context_blocks: usize,
}

/// User-authored steering input submitted while an agent turn is running.
///
/// The runtime stores these records until the next provider request for the
/// same turn. Keeping the original text separate from the templated model
/// context lets the pane log remain copyable while still giving the model clear
/// instructions about how to treat mid-turn input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RuntimeAgentTurnSteering {
    /// Original user prompt text submitted through the pane-local agent shell.
    pub input: String,
    /// Unix timestamp in seconds when the steering prompt was accepted.
    pub submitted_at_unix_seconds: u64,
}

/// Carries Runtime Agent Turn Stop state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeAgentTurnStop {
    /// Stores the turn id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub turn_id: String,
    /// Stores the scheduler cancelled value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub scheduler_cancelled: bool,
    /// Stores the interrupted shell transactions value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub interrupted_shell_transactions: usize,
    /// Stores the visibility value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub visibility: AgentShellVisibility,
}

/// Latest model-authored `say` action retained for user-facing copy operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RuntimeAgentCopyOutput {
    /// Stores the turn id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) turn_id: String,
    /// Raw `say.text` payload that should be copied without rendered prefixes.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) output: String,
    /// Declared `say.content_type` for pane-target rendering.
    ///
    /// Clipboard and paste-buffer targets use `output` directly, while pane
    /// output reuses the regular assistant renderer so markdown and diff
    /// content behaves like the original say action.
    pub(super) content_type: String,
}

/// Aggregated file-modification counts for one pane-local agent conversation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct RuntimeAgentModifiedFileSummary {
    /// Relative path presented to users.
    pub(super) path: String,
    /// Number of added lines observed across successful patch diffs.
    pub(super) added: usize,
    /// Number of removed lines observed across successful patch diffs.
    pub(super) removed: usize,
}

/// Runtime-local shell dispatch history for one active agent turn.
///
/// The provider may require several shell continuations before it can complete
/// a task, but the runtime must retain enough turn-local state to suppress
/// exact command loops before they become unbounded pane input.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct RuntimeAgentShellDispatchHistory {
    /// Stores the commands value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) commands: Vec<String>,
    /// Shell commands that reached a successful transaction boundary.
    pub(super) succeeded_commands: Vec<String>,
    /// Consecutive successful model-authored `shell_command` actions in this turn.
    pub(super) consecutive_successful_shell_commands: usize,
    /// Whether a file mutation succeeded during this active turn.
    pub(super) successful_file_mutation_this_turn: bool,
    /// Whether a validation command succeeded after the latest file mutation.
    pub(super) successful_validation_after_file_mutation: bool,
}

impl RuntimeAgentShellDispatchHistory {
    /// Returns how many model-selected shell commands this turn dispatched.
    pub(super) fn dispatched_count(&self) -> usize {
        self.commands.len()
    }

    /// Returns how many times the exact command text succeeded this turn.
    pub(super) fn exact_success_count(&self, command: &str) -> usize {
        self.succeeded_commands
            .iter()
            .filter(|existing| existing.as_str() == command)
            .count()
    }

    /// Records a successfully dispatched shell command.
    pub(super) fn record(&mut self, command: impl Into<String>) {
        self.commands.push(command.into());
    }

    /// Records a shell command that completed successfully.
    pub(super) fn record_success(
        &mut self,
        command: impl Into<String>,
        action: &AgentAction,
        command_is_validation: bool,
    ) {
        self.succeeded_commands.push(command.into());
        match action.payload {
            AgentActionPayload::ShellCommand { .. } => {
                if command_is_validation && self.successful_file_mutation_this_turn {
                    self.consecutive_successful_shell_commands = 0;
                    self.successful_validation_after_file_mutation = true;
                } else {
                    self.consecutive_successful_shell_commands =
                        self.consecutive_successful_shell_commands.saturating_add(1);
                }
            }
            AgentActionPayload::ApplyPatch { .. } => {
                self.consecutive_successful_shell_commands = 0;
                self.successful_file_mutation_this_turn = true;
                self.successful_validation_after_file_mutation = false;
            }
            _ => {}
        }
    }

    /// Resets the successful inspection streak after a non-shell runtime effect.
    pub(super) fn reset_successive_shell_commands(&mut self) {
        self.consecutive_successful_shell_commands = 0;
    }
}

/// Runtime-local network action history for one active agent turn.
///
/// Network actions execute outside the pane shell, so this records the
/// automatic network dispatches in one turn for traceability. Concrete network
/// actions perform their own URL, policy, and response-size validation before
/// returning results.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct RuntimeAgentNetworkActionHistory {
    /// Network requests executed by this turn.
    pub(super) requests: Vec<String>,
}

impl RuntimeAgentNetworkActionHistory {
    /// Records an executed network request.
    pub(super) fn record(&mut self, request: impl Into<String>) {
        self.requests.push(request.into());
    }
}

/// Runtime-local editable prompt and display state for one pane's agent shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RuntimeAgentPromptInput {
    /// Stores the prompt value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) prompt: ReadlinePrompt,
    /// Stores the decoder value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) decoder: ReadlineInputDecoder,
    /// Stores the display lines value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) display_lines: Vec<String>,
    /// First idle Ctrl+C timestamp waiting for the second confirmation press.
    ///
    /// Ctrl+C is easy to hit accidentally in a pane-local prompt. Idle prompt
    /// exit therefore requires a second Ctrl+C within a short window while
    /// active turns still use Ctrl+C as an immediate interrupt.
    pub(super) pending_ctrl_c_exit_at_unix_ms: Option<u64>,
}

/// Runtime-local editable prompt state for the primary command surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RuntimePrimaryPromptInput {
    /// Stores the prompt value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) prompt: ReadlinePrompt,
    /// Stores the decoder value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) decoder: ReadlineInputDecoder,
}

/// Carries Runtime Subagent Placement state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RuntimeSubagentPlacement {
    /// Represents the New Pane case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    NewPane {
        /// Stores the direction value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        direction: SplitDirection,
        /// Stores the select value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        select: bool,
    },
    /// Represents the New Window case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    NewWindow {
        /// Stores the name value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        name: String,
        /// Stores the select value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        select: bool,
    },
}
