//! Runtime service model, runtime directory helpers, and pane environment helpers.
//!
//! The live session service will eventually own Unix-domain sockets, server
//! locks, and peer credential checks. This module keeps the path and environment
//! rules separate from that transport code so they can be tested before the
//! daemon exists. In particular, it validates the private socket directory
//! invariant, defines the `MEZ` pane-environment format used by in-pane `mez`
//! commands, and provides an owned in-memory runtime service that coordinates
//! session lifecycle state without requiring a long-running daemon.

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

#[cfg(test)]
pub use crate::agent::ModelProvider;
use crate::agent::{
    AGENT_PROMPT_PROFILE_NAME, AGENT_PROMPT_PROFILE_VERSION, ActionContentBlock, ActionResult,
    ActionStatus, AgentAction, AgentActionPayload, AgentContext, AgentLogLevel,
    AgentShellCommandOutcome, AgentShellRuntimeContext, AgentShellSession, AgentShellStore,
    AgentShellVisibility, AgentTurnExecution, AgentTurnLedger, AgentTurnRecord, AgentTurnState,
    AgentTurnTrigger, AsyncMcpActionExecutor, AsyncModelProvider, ContextBlock, ContextSourceKind,
    DEFAULT_PROVIDER_TIMEOUT_MS, DeepSeekChatCompletionsProvider, EnvironmentSignature,
    MarkerToken, McpActionExecutor, ModelMessage, ModelMessageRole, ModelProfile,
    ModelProfileOverrides, ModelRequest, ModelResponse, ModelTokenUsage, ModelTokenUsageKey,
    OpenAiCompatibleChatCompletionsProvider, OpenAiResponsesProvider, PaneReadinessOverrideStore,
    PaneReadinessState, ProviderQuotaUsage, ReadinessOverrideRevocation,
    ReqwestProviderHttpTransport, ShellClassification, ShellTransaction,
    ShellTransactionOutputTransport, ToolDiscoveryCache, action_result_context_content,
    agent_subshell_enter_command, append_mcp_context, append_memory_context,
    append_permission_policy_context, append_scheduler_context,
    assemble_model_request_with_retained_tail_percent,
    compact_model_context_for_budget_with_retained_tail_percent, decode_shell_output_transport,
    decode_shell_output_transport_with_diagnostics, execute_agent_shell_command_with_context,
    execute_mcp_action_through_runtime, execute_mcp_action_through_runtime_async,
    execute_network_action_with_transport_async, local_action_plan, local_action_summary,
    network_action_plan, network_action_summary, next_transcript_sequence,
    openai_default_reasoning_levels_for_model, parse_slash_command,
    postprocess_shell_action_success_output, select_model_profile, set_project_guidance_context,
    shell_command_result_content, shell_command_structured_content_json,
    transcript_entries_for_execution,
};
use crate::audit::{
    AuditActor, AuditConfig, AuditDeferredWrite, AuditLog, AuditRecord, AuditRetentionPolicy,
};
use crate::auth::AuthStore;
use crate::command::{
    CommandInvocation, CommandOutcome, bind_key_args, binding_config_key, execute_auth_command,
    execute_command, execute_mark_pane_ready_command, key_chord_notation, new_window_name,
    new_window_shell_command, parse_command_sequence, resize_spec_from_invocation,
    split_window_selects_new_pane, split_window_shell_command,
};
use crate::config::{
    ConfigDiagnostic, ConfigFormat, ConfigLayer, ConfigMutation, ConfigMutationOperation,
    ConfigMutationValue, ConfigPaths, ConfigScope, EffectiveConfig, compose_effective_config,
    persist_config_text, plan_config_mutation, validate_config_text,
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
use crate::event::{
    EventAudience, EventKind, EventLog, EventVisibility, VisibleEvent, encode_event_notification,
    event_type_name,
};
use crate::hooks::{
    FocusedShellExecutor, FocusedShellHookDispatch, FocusedShellHookDispatchStatus,
    FocusedShellHookOutput, FocusedShellHookQueue, HookDefinition, HookEvent, HookExecutionPlan,
    HookExecutionResult, HookExecutionStatus, HookFailure, HookFailureDecision, HookFailureKind,
    HookInvocation, HookMatcherGroup, HookMatcherOperator, HookMatcherPredicate, HookOnFailure,
    decide_hook_failure, execute_focused_shell_hook, execute_program_hook,
    hook_execution_audit_record, plan_event,
};
use crate::ids::{AgentId, ClientId, PaneId, SessionId, WindowId};
use crate::instructions::DiscoveredInstructionFile;
use crate::layout::{
    MIN_PANE_COLUMNS, MIN_PANE_ROWS, PaneGeometry, PaneNavigationDirection, PaneSizeSpec,
    ResizeAxis, ResizeDirection, Size, SplitDirection,
};
use crate::mcp::{
    McpApprovalSetting, McpExternalCapability, McpRegistry, McpServerConfig, McpServerKind,
    McpServerStatus, McpStartupPlan, McpStartupTransportPlan, McpStdioConnection, McpToolCallPlan,
    McpToolCallRequest, McpToolCallResponse, McpToolEffects, McpToolState,
    discover_streamable_http_mcp_server_with_auth_token, execute_streamable_http_exchange,
    mcp_tools_call_operation, spawn_stdio_mcp_connection,
};
use crate::memory::{MemoryRecord, MemoryScope, MemorySource, SessionMemoryStore};
use crate::message::{
    Envelope, MessageConnection, MessageService, MessageServiceSnapshot, Recipient, SenderIdentity,
    TaskResultPayload, TaskState, TaskStatusPayload, decode_mmp_frame, delivery_batch_json,
    encode_mmp_body, handle_mmp_frame, validate_mmp_payload_metadata,
};
#[cfg(test)]
use crate::message::{MessageFanoutSink, flush_message_fanout_for};
use crate::permissions::{
    ApprovalDecision, ApprovalGrant, ApprovalPolicy, ApprovalScope, ArgumentPolicy,
    BlockedApprovalQueue, BlockedApprovalRequest, BlockedApprovalState, CommandRule,
    CommandRuleScope, DEFAULT_COMMAND_SHELL_CLASSIFICATION, PathScopes, PermissionAuthorityChange,
    PermissionPolicy, PermissionPreset, RuleDecision, RuleMatch, SessionApprovalStore,
    compare_approval_policy_authority, compare_permission_preset_authority, exact_command_sha256,
    normalize_exact_command_text,
};
use crate::process::{
    ExitedPaneProcess, PaneExitStatus, PaneProcessManager, PaneProcessOutput,
    shell_command_from_argv,
};
use crate::project::{
    ProjectTrustRecord, ProjectTrustStore, TrustDecision, default_trust_database_path,
    discover_existing_overlays, discover_project_root,
};
use crate::readline::{ReadlineInputDecoder, ReadlineOutcome, ReadlinePrompt, ReadlinePromptKind};
use crate::registry::{SessionRecord, SessionRegistry};
use crate::scheduler::{
    AgentScheduler, DEFAULT_MAX_CONCURRENT_AGENTS, ScheduledWork, ScheduledWorkKind,
};
use crate::session::{ClientRole, ClientState, ObserverDecisionState, Session};
use crate::snapshot::{
    SessionSnapshotPayload, SnapshotAgentSession, SnapshotApprovalGrantMetadata,
    SnapshotApprovalRequestMetadata, SnapshotConfigDiagnostic, SnapshotConfigLayerMetadata,
    SnapshotCreationContext, SnapshotFrameSettings, SnapshotFrameState,
    SnapshotMcpExternalCapability, SnapshotMcpServerState, SnapshotMcpToolEffects,
    SnapshotMcpToolState, SnapshotPaneCapture, SnapshotRepository, SnapshotState,
};
use crate::subagent::{
    CooperationMode, SUBAGENT_FRIENDLY_NAMES, ScopeRegistry, SubagentProfile,
    SubagentScopeDeclaration, SubagentSpawnRequest, builtin_subagent_profiles,
};
use crate::terminal::{
    AttachedTerminalClientStepPlan, ClientViewRole, CopyMode, CopyModeKeyAction, CopyPosition,
    DEFAULT_HISTORY_LIMIT, DEFAULT_HISTORY_ROTATE_LINES, DEFAULT_PANE_TERM, DEFAULT_UI_THEME_NAME,
    HostClipboard, HostClipboardCommand, KeyBindings, KeyChord, KeyCode, MouseAction,
    MouseBorderCell, MousePaneRegion, MouseWindowActionFrameCell, MouseWindowFrameCell, MuxAction,
    PaneFocusDirection, PasteBuffer, PasteBufferTarget, PasteBuffers, RenderedClientView,
    SearchDirection, TerminalClientLoopAction, TerminalClientLoopConfig, TerminalCursorStyle,
    TerminalFrameContext, TerminalFramePosition, TerminalFrameStyle, TerminalOscEvent,
    TerminalPaneFrameContext, TerminalScreen, TerminalWindowFrameContext,
    TerminalWindowStatusContext, UiTheme, UiThemeDefinition, WindowFocusTarget, WindowFrameAction,
    agent_prompt_reserved_line_count, builtin_ui_theme_definition, key_chord_input_bytes,
    pane_border_cells_for_geometries, pane_content_size_for_geometry,
    pane_frame_merges_into_divider, pane_render_region_size_for_geometry,
    render_attached_client_view, rendered_pane_geometries, rendered_window_body_size,
    resolve_ui_theme, route_client_input_actions, valid_color_alias_name,
    window_frame_action_pillbox_cells, window_frame_pillbox_cells,
};
use crate::transcript::{
    AgentSessionMetadata, AgentTranscriptStore, TranscriptEntry, TranscriptRole,
};

/// Exposes the agent module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod agent;
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
/// Exposes the json module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod json;
/// Exposes the lifecycle module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod lifecycle;
/// Exposes the processes module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod processes;
/// Exposes the render module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod render;
/// Exposes the service module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod service;
/// Exposes the sockets module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod sockets;
/// Exposes the types module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod types;

#[cfg(test)]
pub use sockets::{
    accept_one_control_connection, accept_one_message_connection,
    flush_runtime_event_wakeups_to_stream, serve_control_connection, serve_control_listener,
    serve_message_connection, serve_message_listener, serve_runtime_control_connection,
    serve_runtime_control_connection_with_state, serve_runtime_control_listener,
};
pub use sockets::{
    apply_registry_update, apply_registry_update_async, authorize_unix_peer,
    authorize_unix_peer_raw_fd, authorize_unix_peer_uid, auxiliary_socket_path_for_control_socket,
    bind_control_socket, current_effective_uid, default_socket_directory,
    ensure_private_socket_directory, pane_environment, pane_environment_with_term,
    prune_stale_socket_files_in_directory, remove_stale_socket_file_if_unserved,
    socket_path_for_name,
};
pub use types::{
    AttachedClientStepApplication, AuxiliarySocketKind, DEFAULT_AGENT_ACTION_FAILURE_RETRY_LIMIT,
    DEFAULT_AGENT_COMPACTION_RAW_RETENTION_PERCENT,
    DEFAULT_AGENT_IMPLEMENTATION_PRESSURE_AFTER_SHELL_ACTIONS, DEFAULT_AGENT_LOOP_LIMIT,
    DEFAULT_AGENT_ROUTING, DEFAULT_AUTO_SIZING_FALLBACK_POLICY, DEFAULT_AUTO_SIZING_LARGE_PROFILE,
    DEFAULT_AUTO_SIZING_MEDIUM_PROFILE, DEFAULT_AUTO_SIZING_ROUTER_PROFILE,
    DEFAULT_AUTO_SIZING_SMALL_PROFILE, DEFAULT_MAX_ROOT_SUBAGENTS, DEFAULT_MAX_SUBAGENT_DEPTH,
    DEFAULT_MAX_SUBAGENT_PANES_PER_WINDOW, DEFAULT_MAX_SUBAGENTS_PER_SUBAGENT,
    DEFAULT_PTY_READ_LIMIT_BYTES, DEFAULT_SOCKET_NAME, DEFAULT_SUBAGENT_WAIT_POLICY,
    DeferredAgentPromptHistoryWrite, DeferredAgentTranscriptWrite,
    DeferredCommandPromptHistoryWrite, DeferredConfigFileWrite, DeferredPaneInput,
    DeferredPanePipeWrite, DeferredPaneResize, DeferredPaneTermination, DeferredProgramHook,
    DeferredProjectConfigWrite, DeferredProjectInstructionWrite, MEZ_ENV_FIELD_SEPARATOR,
    PaneEnvironment, PaneExitUpdate, PaneInputDispatch, PaneOutputUpdate, PaneProcessStart,
    PaneResizeUpdate, RuntimeAgentCompactionDispatch, RuntimeAgentCompactionTask,
    RuntimeAgentLoopState, RuntimeAgentLoopTurn, RuntimeAgentLoopTurnKind,
    RuntimeAgentPromptTurnStart, RuntimeAgentProviderDispatch,
    RuntimeAgentProviderDispatchProvider, RuntimeAgentProviderTask, RuntimeAgentRememberDispatch,
    RuntimeAgentRememberTask, RuntimeAgentTurnStop, RuntimeAutoSizingConfig,
    RuntimeAutoSizingDecision, RuntimeAutoSizingDispatch, RuntimeAutoSizingFallbackPolicy,
    RuntimeAutoSizingTargetProfile, RuntimeConfigApplyReport, RuntimeEnv, RuntimeEventConnection,
    RuntimeEventConnectionTable, RuntimeEventFanoutSink, RuntimeEventWakeup,
    RuntimeFocusedShellHookRun, RuntimeLifecycleState, RuntimeMemorySidecarDispatch,
    RuntimeMessageConnection, RuntimeMessageConnectionTable, RuntimeMessageFanoutSink,
    RuntimeMessageWakeup, RuntimeModelPreset, RuntimePresetRegistry, RuntimeProviderConfig,
    RuntimeProviderRegistry, RuntimeRegistryUpdatePlan, RuntimeSessionService,
    RuntimeShellTransactionTimerKind, RuntimeShellTransactionTimerRef, SocketDirectory,
    SocketDirectorySource, SubagentWaitPolicy, flush_runtime_event_wakeup,
    flush_runtime_event_wakeups, flush_runtime_message_wakeup, flush_runtime_message_wakeups,
};
use types::{
    JoinedSubagentDependency, RuntimeAgentCopyOutput, RuntimeAgentModifiedFileSummary,
    RuntimeAgentPromptInput, RuntimeAgentTurnSteering, RuntimeCommandBinding,
    RuntimeSubagentLineage,
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
    runtime_terminal_clipboard_from_config, runtime_terminal_cursor_blink_from_config,
    runtime_terminal_cursor_blink_interval_ms_from_config,
    runtime_terminal_cursor_style_from_config, runtime_terminal_emoji_width_from_config,
    runtime_terminal_reduced_motion_from_config,
    runtime_terminal_render_rate_limit_fps_from_config,
    runtime_terminal_resize_debounce_ms_from_config, runtime_terminal_term_from_config,
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
    runtime_agent_turn_state_from_action_results, runtime_agent_turn_state_json,
    runtime_agent_turn_state_name, runtime_command_outcomes_json, runtime_cooperation_mode,
    runtime_cooperation_mode_name, runtime_copy_position_for_view,
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
use sockets::{
    effective_uid, ensure_absolute, ensure_no_mez_separator, validate_pane_size_for_resize,
};
use types::{
    ActivePanePipe, BlockedAgentApprovalRef, MouseResizeDragState, MouseSelectionDragState,
    PaneDescriptor, PaneExitRecord, PendingFocusedShellHookContinuation,
    PendingFocusedShellHookTransaction, RunningShellTransactionKind, RunningShellTransactionRef,
    RuntimeAgentPersonalityProfile, RuntimeAgentPreShellHookCompletion, RuntimeHookPipelineBlock,
    RuntimeHookPipelineDecision, RuntimeHttpMcpTransportState, RuntimeMcpRetryReport,
    RuntimeMcpTransportSet, RuntimeModelProfileOverrideScope, RuntimeModelProfileOverrideStore,
    RuntimeShellTransactionActionFailure, RuntimeSubagentPlacement, StoppedPanePipe,
};

pub(crate) use types::{
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
