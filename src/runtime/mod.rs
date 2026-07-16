//! Runtime service model, runtime directory helpers, and pane environment helpers.
//!
//! The live session service will eventually own Unix-domain sockets, server
//! locks, and peer credential checks. This module keeps the path and environment
//! rules separate from that transport code so they can be tested before the
//! daemon exists. In particular, it validates the private socket directory
//! invariant, defines the `MEZ` pane-environment format used by in-pane `mez`
//! commands, and provides an owned in-memory runtime service that coordinates
//! session lifecycle state without requiring a long-running daemon.

use mez_mux::presentation::{ClientViewRole, RenderedClientView};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rustix::fd::BorrowedFd;
use rustix::net::sockopt::socket_peercred;
use rustix::process::geteuid;
use serde_json::Value;

use crate::config::{
    ConfigFormat, ConfigLayer, ConfigMutation, ConfigMutationOperation, ConfigMutationValue,
    ConfigPaths, ConfigScope, EffectiveConfig, compose_effective_config, persist_config_text,
    plan_config_mutation, validate_config_text,
};
#[cfg(test)]
use crate::control::handle_control_frames_for_connection;
use crate::control::{
    ApprovalDecisionScopePersistence, ControlConnectionState, ControlIdempotencyCache,
    PaneCaptureSource, approval_decide_scope_persistence, decode_control_frame,
    destination_target_checked_resolved, dispatch_control_request_cached,
    dispatch_control_request_for_client_with_agent_state,
    dispatch_control_request_for_client_with_agent_state_and_model_profiles,
    dispatch_control_request_for_client_with_config,
    dispatch_control_request_for_client_with_config_and_audit,
    dispatch_control_request_for_client_with_snapshot_context,
    dispatch_control_request_for_connection, dispatch_control_request_with_approvals,
    dispatch_control_request_with_approvals_and_audit, dispatch_control_request_with_captures,
    dispatch_control_request_with_mcp, dispatch_event_list_request,
    dispatch_snapshot_request_with_context_async, encode_control_body,
    frame_read_json_with_context, layout_state_json, observer_json, observers_json,
    pane_target_checked_resolved, parse_json_rpc_request, project_trust_state_filter_from_params,
    session_state_name, snapshot_id_for_idempotency_key, source_pane_target_checked_resolved,
    state_request_pane_list_window_ids, state_request_session_target_matches,
    unix_seconds_to_rfc3339, window_target_checked_resolved,
};
use crate::error::{MezError, Result};
use crate::host::terminal::{
    AttachedTerminalClientStepPlan, CopyMode, HostClipboard, MouseAction,
    MouseWindowActionFrameCell, TerminalClientLoopAction, TerminalClientLoopConfig,
    TerminalFrameContext, WindowFrameAction, agent_prompt_reserved_line_count,
    render_attached_client_view, rendered_pane_geometries, route_client_input_actions,
    window_frame_action_pillbox_cells, window_frame_pillbox_cells,
};
use crate::integrations::agent::actions::{
    execute_mcp_action_through_runtime, execute_mcp_action_through_runtime_async,
    next_transcript_sequence,
};
use crate::integrations::agent::context::assemble_model_request;
use crate::integrations::agent::network::execute_network_action_with_transport_async;
use crate::integrations::agent::provider::{
    AsyncModelProvider, DeepSeekChatCompletionsProvider, OpenAiCompatibleChatCompletionsProvider,
    OpenAiResponsesProvider, ReqwestProviderHttpTransport,
};
use crate::integrations::agent::slash::{
    AgentShellCommandOutcome, AgentShellRuntimeContext, execute_agent_shell_command_with_context,
};
use crate::integrations::agent::subagent::SUBAGENT_FRIENDLY_NAMES;
use crate::integrations::hooks::{
    FocusedShellExecutor, FocusedShellHookDispatch, FocusedShellHookDispatchStatus,
    FocusedShellHookOutput, HookDefinition, HookEvent, HookExecutionPlan, HookExecutionResult,
    HookExecutionStatus, HookFailure, HookFailureDecision, HookFailureKind, HookOnFailure,
    decide_hook_failure, execute_focused_shell_hook, execute_program_hook,
    hook_execution_audit_record, plan_event,
};
use crate::integrations::mcp::{
    McpStdioConnection, discover_streamable_http_mcp_server_with_auth_token,
    execute_streamable_http_exchange, spawn_stdio_mcp_connection,
};
use crate::protocol::event::{
    EventAudience, EventKind, EventLog, EventVisibility, VisibleEvent, encode_event_notification,
    event_type_name,
};
use crate::protocol::message::{decode_mmp_frame, encode_mmp_body, handle_mmp_frame};
use crate::security::audit::{AuditActor, AuditDeferredWrite, AuditLog, AuditRecord};
use crate::security::auth::AuthStore;
use crate::security::project::{
    ProjectTrustStore, TrustDecision, default_trust_database_path, discover_existing_overlays,
    discover_project_root,
};
use crate::storage::registry::{SessionRecord, SessionRegistry};
use crate::storage::snapshot::{
    SessionSnapshotPayload, SnapshotAgentSession, SnapshotApprovalGrantMetadata,
    SnapshotApprovalRequestMetadata, SnapshotConfigDiagnostic, SnapshotConfigLayerMetadata,
    SnapshotCreationContext, SnapshotFrameSettings, SnapshotFrameState,
    SnapshotMcpExternalCapability, SnapshotMcpServerState, SnapshotMcpToolEffects,
    SnapshotMcpToolState, SnapshotPaneCapture, SnapshotRepository, SnapshotState,
};
use crate::storage::transcript::AgentTranscriptStore;
use crate::ui::command::{
    CommandOutcome, bind_key_args, binding_config_key, execute_auth_command, execute_command,
    execute_mark_pane_ready_command, key_chord_notation,
};
use crate::ui::readline::{ReadlineInputDecoder, ReadlinePrompt, ReadlinePromptKind};
use mez_agent::mcp::{
    McpApprovalSetting, McpExternalCapability, McpRegistry, McpServerKind, McpServerStatus,
    McpStartupPlan, McpStartupTransportPlan, McpToolCallPlan, McpToolCallRequest,
    McpToolCallResponse, McpToolEffects, McpToolState, mcp_tools_call_operation,
};
use mez_agent::memory::{MemoryRecord, MemoryScope, MemorySource, SessionMemoryStore};
use mez_agent::messaging::{
    Envelope, MessageConnection, MessageService, MessageServiceSnapshot, Recipient, SenderIdentity,
    TaskResultPayload, TaskState, TaskStatusPayload, delivery_batch_json,
    validate_mmp_payload_metadata,
};
use mez_agent::parse_slash_command;
use mez_agent::permissions::{
    ApprovalDecision, ApprovalGrant, ApprovalScope, ArgumentPolicy, BlockedApprovalQueue,
    BlockedApprovalRequest, BlockedApprovalState, CommandRule, CommandRuleScope,
    DEFAULT_COMMAND_SHELL_CLASSIFICATION, PathScopes, PermissionAuthorityChange, PermissionPolicy,
    RuleMatch, SessionApprovalStore, compare_approval_policy_authority,
    compare_permission_preset_authority, exact_command_sha256, normalize_exact_command_text,
};
use mez_agent::transcript::{AgentSessionMetadata, TranscriptEntry, TranscriptRole};
use mez_agent::{
    AGENT_PROMPT_PROFILE_NAME, AGENT_PROMPT_PROFILE_VERSION, ActionContentBlock, ActionResult,
    ActionStatus, AgentAction, AgentActionPayload, AgentContext, AgentLogLevel, AgentShellSession,
    AgentShellStore, AgentShellVisibility, AgentTurnExecution, AgentTurnLedger, AgentTurnRecord,
    AgentTurnState, AgentTurnTrigger, ContextBlock, ContextSourceKind, McpExecutionRequest,
    McpExecutionResponse, ModelProfile, ModelProfileOverrides, ModelRequest, ModelResponse,
    ModelTokenUsage, ModelTokenUsageKey, PaneReadinessState, ReadinessOverrideRevocation,
    action_result_context_content, compact_model_context_for_budget_with_retained_tail_percent,
    decode_shell_output_transport_with_diagnostics, local_action_plan, network_action_plan,
    postprocess_shell_action_success_output, select_model_profile, shell_command_result_content,
    transcript_entries_for_execution,
};
use mez_agent::{AgentScheduler, DEFAULT_MAX_CONCURRENT_AGENTS, ScheduledWork, ScheduledWorkKind};
use mez_agent::{ApprovalPolicy, PermissionPreset, RuleDecision};
use mez_agent::{AsyncMcpActionExecutor, McpActionExecutor};
use mez_agent::{
    CooperationMode, SubagentProfile, SubagentScopeDeclaration, SubagentSpawnRequest,
    builtin_subagent_profiles,
};
use mez_agent::{
    EnvironmentSignature, MarkerToken, ShellClassification, ShellTransaction,
    ShellTransactionOutputTransport, agent_subshell_enter_command,
};
use mez_core::ids::{AgentId, ClientId, PaneId, SessionId, WindowId};
use mez_mux::command::{CommandInvocation, parse_command_sequence};
use mez_mux::copy::CopyModeKeyAction;
use mez_mux::copy::{CopyPosition, SearchDirection};
use mez_mux::input::{
    KeyBindings, KeyChord, KeyCode, MuxAction, PaneFocusDirection, WindowFocusTarget,
    key_chord_input_bytes,
};
use mez_mux::layout::{
    MIN_PANE_COLUMNS, MIN_PANE_ROWS, PaneGeometry, PaneNavigationDirection, PaneSizeSpec,
    ResizeAxis, ResizeDirection, Size, SplitDirection,
};
use mez_mux::paste::{PasteBuffer, PasteBuffers};
use mez_mux::process::{
    ExitedPaneProcess, PaneExitStatus, PaneProcessManager, PaneProcessOutput,
    shell_command_from_argv,
};
use mez_mux::readline::ReadlineOutcome;
use mez_mux::session::{ClientRole, ClientState, ObserverDecisionState, Session};
use mez_mux::theme::{UiThemeDefinition, builtin_ui_theme_definition, resolve_ui_theme};
use mez_terminal::DEFAULT_PANE_TERM;
use mez_terminal::{TerminalOscEvent, TerminalScreen};

/// Coordinates the seven private application runtime components.
///
/// The coordinator itself owns no domain state. Each field is private to this
/// module and therefore visible only to runtime descendants under Rust's
/// normal module privacy rules; external callers interact through typed
/// service operations rather than a crate-visible state bag.
#[derive(Debug)]
pub struct RuntimeSessionService {
    /// Terminal presentation and attached-client interaction ownership.
    presentation: RuntimePresentationComponent,
    /// Pane process, terminal state, and shell-transaction ownership.
    process: RuntimeProcessComponent,
    /// Application-side agent execution ownership.
    agent: RuntimeAgentComponent,
    /// Repository and deferred external-effect ownership.
    persistence: RuntimePersistenceComponent,
    /// Control replay, messaging, and event-fanout ownership.
    control: RuntimeControlComponent,
    /// Concrete config, security, provider, trust, and hook bindings.
    integration: RuntimeIntegrationComponent,
    /// Canonical mux session and application lifecycle metadata.
    session: RuntimeSessionComponent,
}

/// Exposes the agent module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod agent;
pub(crate) use agent::RuntimeAgentComponent;
/// Exposes runtime agent provider dispatch and loop state records.
///
/// The nested module keeps provider-backed agent worker records out of the
/// central runtime service state.
mod agent_state;
/// Exposes the auto sizing module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod auto_sizing;
/// Exposes the commands module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod commands;
/// Exposes the commands support module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod commands_support;
/// Exposes the config module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod config;
/// Exposes the control module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod control;
pub(crate) use control::RuntimeControlComponent;
/// Exposes deferred runtime side-effect value types.
///
/// The nested module keeps side-effect planning records out of the central
/// runtime service state.
mod deferred;
/// Exposes runtime environment and pane environment value types.
///
/// The nested module keeps socket-directory and pane-environment contracts out
/// of the central runtime service state.
mod env;
/// Exposes runtime message and event fanout connection tables.
///
/// The nested module keeps socket delivery bookkeeping out of the central
/// runtime service state.
mod fanout;
/// Exposes the hook pipeline module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod hook_pipeline;
/// Exposes the hook support module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod hook_support;
/// Exposes the hooks module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod hooks;
/// Exposes concrete product integration state ownership.
mod integration;
/// Exposes the json module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod json;
pub(crate) use integration::RuntimeIntegrationComponent;
/// Exposes the lifecycle module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod lifecycle;
/// Exposes the pane_io module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod pane_io;
mod persistence;
pub(crate) use persistence::RuntimePersistenceComponent;
/// Exposes the processes module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod processes;
pub(crate) use processes::RuntimeProcessComponent;
/// Exposes runtime provider registry and model preset records.
///
/// The nested module keeps provider configuration records out of the central
/// runtime service state.
/// Exposes reusable pager state for record-oriented agent-shell browsers.
///
/// The module keeps list/detail navigation, prompt state, and save payloads
/// independent from issue and memory adapters.
/// Exposes the render module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod render;
pub(crate) use render::{RuntimePresentationComponent, RuntimePresentationSettings};
/// Exposes the service module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod service;
#[cfg(test)]
pub(crate) use service::coalesce_config_persistence_effects;
/// Exposes the types module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod service_state;
/// Exposes mux-session and runtime lifecycle metadata ownership.
mod session;
pub(crate) use session::RuntimeSessionComponent;
/// Exposes the sockets module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod sockets;
/// Exposes command-backed window status pill scheduling.
///
/// The nested module keeps status-polling configuration and cache state out of
/// the central runtime service state.
mod status_pills;
/// Exposes transport-neutral runtime event, transition, and side-effect types.
mod transitions;

pub use agent_state::{
    RuntimeAgentCompactionDispatch, RuntimeAgentCompactionTask, RuntimeAgentLoopState,
    RuntimeAgentLoopTurn, RuntimeAgentLoopTurnKind, RuntimeAgentProviderDispatch,
    RuntimeAgentProviderDispatchProvider, RuntimeAgentProviderTask, RuntimeAgentRememberDispatch,
    RuntimeAgentRememberTask,
};
pub use deferred::AttachedClientStepApplication;
pub use env::{
    AuxiliarySocketKind, DEFAULT_SOCKET_NAME, MEZ_ENV_FIELD_SEPARATOR, RuntimeEnv, SocketDirectory,
    SocketDirectorySource,
};
#[cfg(test)]
pub use fanout::{
    RuntimeEventConnection, RuntimeEventFanoutSink, RuntimeMessageConnection,
    RuntimeMessageConnectionTable, RuntimeMessageFanoutSink, RuntimeMessageWakeup,
    flush_runtime_event_wakeup, flush_runtime_event_wakeups, flush_runtime_message_wakeup,
    flush_runtime_message_wakeups,
};
pub use fanout::{RuntimeEventConnectionTable, RuntimeEventWakeup, RuntimeFocusedShellHookRun};
#[cfg(test)]
use mez_agent::AutoSizingDecision as RuntimeAutoSizingDecision;
use mez_agent::{
    AutoSizingConfig as RuntimeAutoSizingConfig, AutoSizingDispatch as RuntimeAutoSizingDispatch,
    AutoSizingFallbackPolicy as RuntimeAutoSizingFallbackPolicy,
    AutoSizingTargetProfile as RuntimeAutoSizingTargetProfile, DEFAULT_AUTO_SIZING_FALLBACK_POLICY,
    DEFAULT_AUTO_SIZING_ROUTER_PROFILE, ModelPreset as RuntimeModelPreset,
    ProviderConfig as RuntimeProviderConfig, ProviderRegistry as RuntimeProviderRegistry,
};
use mez_mux::process::PaneProcessEnvironment as PaneEnvironment;
use pane_io::{ActivePanePipe, PaneExitRecord, StoppedPanePipe};
pub use pane_io::{
    PaneExitUpdate, PaneInputDispatch, PaneOutputUpdate, PaneProcessStart, PaneResizeUpdate,
};
pub use service_state::{
    DEFAULT_AGENT_ACTION_FAILURE_RETRY_LIMIT, DEFAULT_AGENT_COMPACTION_RAW_RETENTION_PERCENT,
    DEFAULT_AGENT_IMPLEMENTATION_PRESSURE_AFTER_SHELL_ACTIONS, DEFAULT_AGENT_LOOP_LIMIT,
    DEFAULT_AGENT_ROUTING, DEFAULT_MAX_ROOT_SUBAGENTS, DEFAULT_MAX_SUBAGENT_DEPTH,
    DEFAULT_MAX_SUBAGENT_PANES_PER_WINDOW, DEFAULT_MAX_SUBAGENTS_PER_SUBAGENT,
    DEFAULT_PTY_READ_LIMIT_BYTES, DEFAULT_SUBAGENT_WAIT_POLICY, RuntimeAgentPromptTurnStart,
    RuntimeAgentTurnStop, RuntimeConfigApplyReport, RuntimeLifecycleState,
    RuntimeRegistryUpdatePlan, RuntimeShellTransactionTimerKind, RuntimeShellTransactionTimerRef,
    SubagentWaitPolicy,
};
use service_state::{
    JoinedSubagentDependency, RuntimeAgentCopyOutput, RuntimeAgentModifiedFileSummary,
    RuntimeAgentPromptInput, RuntimeCommandBinding, RuntimeSubagentLineage,
};
pub use sockets::{
    apply_registry_update, apply_registry_update_async, authorize_unix_peer_raw_fd,
    auxiliary_socket_path_for_control_socket, bind_control_socket, current_effective_uid,
    default_socket_directory, ensure_private_socket_directory, pane_environment_with_term,
    prune_stale_socket_files_in_directory, socket_path_for_name,
};
#[cfg(test)]
pub use sockets::{
    authorize_unix_peer, authorize_unix_peer_uid, pane_environment,
    remove_stale_socket_file_if_unserved, serve_control_connection,
    serve_runtime_control_connection, serve_runtime_control_connection_with_state,
};
use status_pills::{
    RuntimeStatusPillCache, RuntimeStatusPillDefinition,
    runtime_status_pill_definitions_from_config,
};
pub use transitions::{
    AgentCompactionEvent, AgentProviderEvent, AgentRememberEvent, AsyncHookEvent, ClientEvent,
    PaneEvent, PersistenceEvent, PersistenceTarget, PersistenceWriteMode, ProcessEvent,
    RenderInvalidationReason, RuntimeEvent, RuntimeEventBatch, RuntimeEventIngressReport,
    RuntimeSideEffect, RuntimeTimerKey, RuntimeTimerKind, RuntimeTransition, ShutdownEvent,
    TimerEvent,
};

#[cfg(test)]
pub(crate) use auto_sizing::runtime_execute_auto_sizing_with_provider;
pub(crate) use auto_sizing::{
    runtime_apply_auto_sizing_execution_profile, runtime_auto_sizing_reasoning_levels_for_profile,
    runtime_execute_auto_sizing_with_async_provider,
};

use commands_support::{
    execute_runtime_command_sequence, execute_runtime_command_sequence_async,
    runtime_add_command_rule, runtime_append_auth_logout_audit,
    runtime_append_observer_decision_audit, runtime_apply_persisted_config_mutation_batch,
    runtime_approval_command, runtime_approval_policy_name, runtime_bypass_approvals_command,
    runtime_list_command_rules_display, runtime_mcp_retry_event_payload,
    runtime_parse_approval_policy, runtime_paste_bytes, runtime_permission_preset_name,
    runtime_permissions_command, runtime_remove_command_rule, runtime_set_theme_command,
    runtime_write_agent_context_for_pane, runtime_write_agent_copy_output_for_pane,
    runtime_write_agent_patches_for_pane, runtime_write_agent_trace_log_for_pane,
};
use config::{
    RUNTIME_LATENCY_PREFERENCES, json_escape, optional_i32_json,
    runtime_agent_action_failure_retry_limit_from_config, runtime_agent_auto_sizing_from_config,
    runtime_agent_compaction_raw_retention_percent_from_config,
    runtime_agent_custom_system_prompt_from_config,
    runtime_agent_implementation_pressure_after_shell_actions_from_config,
    runtime_agent_loop_limit_from_config, runtime_agent_personality_profiles_from_config,
    runtime_agent_routing_from_config, runtime_agent_turn_start_hook_payload,
    runtime_approval_decision_name_to_kind, runtime_audit_config_present,
    runtime_audit_log_from_config, runtime_blocked_approval_request,
    runtime_command_bindings_from_effective, runtime_config_apply_event_payload,
    runtime_config_method_applies_to_live_service, runtime_default_agent_personality_from_config,
    runtime_default_models_for_provider, runtime_fit_status_line,
    runtime_history_limit_from_config, runtime_history_rotate_lines_from_config,
    runtime_hook_definitions_from_config, runtime_hook_target_pane_id,
    runtime_host_clipboard_from_config, runtime_key_bindings_from_config,
    runtime_marker_for_action, runtime_max_concurrent_agents_from_config,
    runtime_max_root_subagents_from_config, runtime_max_subagent_depth_from_config,
    runtime_max_subagent_panes_per_window_from_config,
    runtime_max_subagents_per_subagent_from_config, runtime_mcp_error_code,
    runtime_mcp_registry_from_config, runtime_message_recipient, runtime_model_command_args,
    runtime_model_override_scope_for_args, runtime_model_override_scope_name,
    runtime_model_profile_display, runtime_pane_frame_position_from_config,
    runtime_pane_frame_style_from_config, runtime_pane_frame_template_from_config,
    runtime_pane_frame_visible_fields_from_config, runtime_pane_frames_enabled_from_config,
    runtime_path_under_project_root, runtime_permission_decision_hook_payload,
    runtime_permission_policy_from_config, runtime_permission_request_hook_payload,
    runtime_post_mcp_hook_payload, runtime_post_shell_hook_payload, runtime_pre_mcp_hook_payload,
    runtime_pre_shell_hook_payload, runtime_preset_registry_from_config,
    runtime_project_root_param, runtime_project_trust_record_json,
    runtime_provider_auth_refresh_leeway_seconds_from_config,
    runtime_provider_registry_from_config, runtime_random_marker_token,
    runtime_saved_agent_session_limit_from_config, runtime_string_array_json,
    runtime_subagent_profiles_from_config, runtime_subagent_wait_policy_from_config,
    runtime_terminal_agent_wrap_column_cap_from_config, runtime_terminal_clipboard_from_config,
    runtime_terminal_cursor_blink_from_config,
    runtime_terminal_cursor_blink_interval_ms_from_config,
    runtime_terminal_cursor_style_from_config, runtime_terminal_emoji_width_from_config,
    runtime_terminal_reduced_motion_from_config,
    runtime_terminal_render_rate_limit_fps_from_config,
    runtime_terminal_resize_debounce_ms_from_config,
    runtime_terminal_shell_output_preview_lines_from_config, runtime_terminal_term_from_config,
    runtime_trust_decision_name, runtime_trust_decision_param, runtime_user_prompt_hook_payload,
    runtime_validate_latency_preference, runtime_window_frame_position_from_config,
    runtime_window_frame_right_status_template_from_config, runtime_window_frame_style_from_config,
    runtime_window_frame_template_from_config, runtime_window_frame_visible_fields_from_config,
    runtime_window_frames_enabled_from_config,
};
pub use config::{runtime_effective_config_value, runtime_ui_theme_from_config};
use hook_support::{
    RuntimeFocusedShellPaneExecutor, RuntimeMcpActionExecutor,
    focused_shell_pre_action_failed_result, focused_shell_pre_action_timeout_result,
    runtime_hook_event_for_lifecycle, runtime_hook_event_name,
};
use json::{
    agent_shell_visibility_json_name, agent_state_control_method, current_unix_millis,
    current_unix_seconds, mouse_action_name, mux_action_command_prompt_prefill, mux_action_name,
    optional_path_json, optional_string_json, pane_navigation_direction, rendered_client_view_json,
    runtime_agent_shell_command_response_json, runtime_agent_shell_prompt_turn_response_json,
    runtime_agent_shell_stop_response_json, runtime_agent_turn_duration_display,
    runtime_agent_turn_state_json, runtime_agent_turn_state_name, runtime_command_outcomes_json,
    runtime_cooperation_mode, runtime_cooperation_mode_name, runtime_copy_position_for_view,
    runtime_execution_ready_for_provider_continuation, runtime_hook_execution_status_name,
    runtime_initialize_requested_observer, runtime_initialize_requested_primary,
    runtime_initialize_terminal_size, runtime_json_bool_field, runtime_json_creation_command,
    runtime_json_input_bytes, runtime_json_optional_client_size, runtime_json_optional_size_field,
    runtime_json_optional_view_offset, runtime_json_rpc_error, runtime_json_size,
    runtime_json_start_directory, runtime_json_string_field, runtime_json_value,
    runtime_mezzanine_error_code, runtime_mutating_method, runtime_pane_by_id,
    runtime_pane_readiness_state_name, runtime_split_direction, runtime_subagent_placement_mode,
    runtime_subagent_spawn_request, runtime_subagent_state_json, runtime_terminal_step_result_json,
};
use mez_agent::turn_state_from_action_results as runtime_agent_turn_state_from_action_results;
use service_state::{
    BlockedAgentApprovalRef, MouseResizeDragState, MouseSelectionDragState, PaneDescriptor,
    PendingFocusedShellHookContinuation, PendingFocusedShellHookTransaction,
    RunningShellTransactionKind, RunningShellTransactionRef, RuntimeAgentPersonalityProfile,
    RuntimeAgentPreShellHookCompletion, RuntimeHookPipelineBlock, RuntimeHookPipelineDecision,
    RuntimeHttpMcpTransportState, RuntimeMcpRetryReport, RuntimeMcpTransportSet,
    RuntimeModelProfileOverrideScope, RuntimeShellTransactionActionFailure,
    RuntimeSubagentPlacement,
};
#[cfg(test)]
use sockets::effective_uid;
use sockets::{ensure_absolute, ensure_no_mez_separator, validate_pane_size_for_resize};

pub(crate) use service_state::{
    RuntimeSnapshotControlAsyncOutcome, RuntimeSnapshotControlAsyncWork,
    RuntimeSnapshotControlAsyncWorkKind, RuntimeSnapshotOwnedCreationContext,
};

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use tests::effective_uid_for_tests;
