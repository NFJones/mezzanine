//! Runtime Json implementation.
//!
//! This module owns the runtime json boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    ActionStatus, AgentShellCommandOutcome, AgentShellVisibility, AgentTurnExecution,
    AgentTurnState, AttachedClientStepApplication, ClientViewRole, CommandOutcome, CooperationMode,
    CopyMode, CopyPosition, HookExecutionStatus, MezError, MouseAction, MuxAction,
    PaneFocusDirection, PaneNavigationDirection, PaneReadinessState, PaneSizeSpec, Path, PathBuf,
    RenderedClientView, ResizeAxis, ResizeDirection, Result, RuntimeAgentPromptTurnStart,
    RuntimeAgentTurnStop, RuntimeSubagentPlacement, Session, Size, SplitDirection,
    SubagentSpawnRequest, SystemTime, UNIX_EPOCH, Value, WindowFocusTarget, json_escape,
    runtime_string_array_json, shell_command_from_argv, unix_seconds_to_rfc3339,
};
use mez_mux::presentation::{
    TerminalCursorStyle, compose_client_presentation_with_styles, max_viewport_column,
    max_viewport_row,
};
use mez_mux::theme::{UiColorPair, UiTheme};
use mez_terminal::{GraphicRendition, TerminalColor, TerminalStyleSpan};

mod actions;
mod command;
mod parse;
mod presentation;
mod status;

pub(super) use actions::{
    agent_state_control_method, mouse_action_name, mux_action_command_prompt_prefill,
    mux_action_name, pane_navigation_direction, runtime_cooperation_mode,
    runtime_cooperation_mode_name, runtime_copy_position_for_view, runtime_mutating_method,
    runtime_pane_by_id, runtime_split_direction, runtime_subagent_placement_mode,
    runtime_subagent_spawn_request,
};
pub(super) use command::{optional_path_json, optional_string_json, runtime_command_outcomes_json};
pub(super) use parse::{
    current_unix_millis, current_unix_seconds, runtime_agent_turn_duration_display,
    runtime_initialize_requested_observer, runtime_initialize_requested_primary,
    runtime_initialize_terminal_size, runtime_json_bool_field, runtime_json_creation_command,
    runtime_json_input_bytes, runtime_json_optional_client_size, runtime_json_optional_size_field,
    runtime_json_optional_view_offset, runtime_json_rpc_error, runtime_json_size,
    runtime_json_start_directory, runtime_json_string_field, runtime_json_value,
    runtime_mezzanine_error_code,
};
pub(super) use presentation::rendered_client_view_json;
pub(super) use status::{
    agent_shell_visibility_json_name, runtime_agent_shell_command_response_json,
    runtime_agent_shell_prompt_turn_response_json, runtime_agent_shell_stop_response_json,
    runtime_agent_turn_state_json, runtime_agent_turn_state_name,
    runtime_execution_ready_for_provider_continuation, runtime_hook_execution_status_name,
    runtime_pane_readiness_state_name, runtime_subagent_state_json,
    runtime_terminal_step_result_json,
};
