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
use crate::terminal::{
    compose_client_presentation_with_styles, max_viewport_column, max_viewport_row,
};
use mez_mux::presentation::TerminalCursorStyle;
use mez_mux::theme::{UiColorPair, UiTheme};
use mez_terminal::{GraphicRendition, TerminalColor, TerminalStyleSpan};

// Runtime JSON serialization, parsing, and name mapping helpers.

/// Runs the runtime terminal step result json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_terminal_step_result_json(
    input_bytes: usize,
    application: &AttachedClientStepApplication,
    view: Option<&RenderedClientView>,
) -> String {
    let unsupported = application
        .unsupported_actions
        .iter()
        .map(|action| format!(r#""{}""#, json_escape(action)))
        .collect::<Vec<_>>();
    let view_json = view
        .map(rendered_client_view_json)
        .unwrap_or_else(|| "null".to_string());
    let ui_theme = view
        .map(|view| ui_theme_json(&view.ui_theme))
        .unwrap_or_else(|| "null".to_string());
    format!(
        r#"{{"input_bytes":{},"application":{{"forwarded_bytes":{},"mux_actions_applied":{},"mouse_actions_reported":{},"agent_prompt_inputs_applied":{},"view_refresh_required":{},"full_redraw_required":{},"unsupported_actions":[{}]}},"view":{},"ui_theme":{}}}"#,
        input_bytes,
        application.forwarded_bytes,
        application.mux_actions_applied,
        application.mouse_actions_reported,
        application.agent_prompt_inputs_applied,
        application.view_refresh_required,
        application.full_redraw_required,
        unsupported.join(","),
        view_json,
        ui_theme
    )
}

/// Runs the runtime agent shell command response json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_agent_shell_command_response_json(
    pane_id: &str,
    input: &str,
    outcome: Option<&AgentShellCommandOutcome>,
) -> String {
    match outcome {
        Some(AgentShellCommandOutcome::Display { command, body }) => format!(
            r#"{{"pane_id":"{}","input":"{}","kind":"display","command":"{}","content_type":"text/markdown; charset=utf-8","body":"{}","turn":null}}"#,
            json_escape(pane_id),
            json_escape(input),
            json_escape(command),
            json_escape(body)
        ),
        Some(AgentShellCommandOutcome::Mutated {
            command,
            body,
            visibility,
        }) => format!(
            r#"{{"pane_id":"{}","input":"{}","kind":"mutated","command":"{}","visibility":"{}","body":"{}","turn":null}}"#,
            json_escape(pane_id),
            json_escape(input),
            json_escape(command),
            agent_shell_visibility_json_name(*visibility),
            json_escape(body)
        ),
        Some(AgentShellCommandOutcome::RequiresRuntime { command, reason }) => format!(
            r#"{{"pane_id":"{}","input":"{}","kind":"requires_runtime","command":"{}","body":"{}","turn":null}}"#,
            json_escape(pane_id),
            json_escape(input),
            json_escape(command),
            json_escape(reason)
        ),
        None => format!(
            r#"{{"pane_id":"{}","input":"{}","kind":"requires_runtime","command":"prompt","body":"live model-loop task execution is pending","turn":null}}"#,
            json_escape(pane_id),
            json_escape(input)
        ),
    }
}

/// Runs the runtime agent shell prompt turn response json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_agent_shell_prompt_turn_response_json(
    pane_id: &str,
    input: &str,
    started: &RuntimeAgentPromptTurnStart,
) -> String {
    let turn = runtime_agent_turn_state_json(started);
    format!(
        r#"{{"pane_id":"{}","input":"{}","kind":"turn_started","command":null,"body":null,"turn":{}}}"#,
        json_escape(pane_id),
        json_escape(input),
        turn
    )
}

/// Runs the runtime agent turn state json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_agent_turn_state_json(started: &RuntimeAgentPromptTurnStart) -> String {
    let approval_ids = started
        .approval_ids
        .iter()
        .map(|id| format!(r#""{}""#, json_escape(id)))
        .collect::<Vec<_>>()
        .join(",");
    let result_summary = started
        .result_summary
        .as_deref()
        .map(|summary| format!(r#""{}""#, json_escape(summary)))
        .unwrap_or_else(|| "null".to_string());
    format!(
        r#"{{"id":"{}","version":1,"agent_id":"{}","state":"{}","created_at":{},"started_at":{},"finished_at":{},"prompt_preview":"{}","approval_ids":[{}],"result_summary":{},"extensions":{{"context_blocks":{}}}}}"#,
        json_escape(&started.turn_id),
        json_escape(&started.agent_id),
        runtime_agent_turn_state_name(started.state),
        runtime_timestamp_json(started.created_at_unix_seconds),
        runtime_optional_timestamp_json(started.started_at_unix_seconds),
        runtime_optional_timestamp_json(started.finished_at_unix_seconds),
        json_escape(&started.prompt_preview),
        approval_ids,
        result_summary,
        started.context_blocks
    )
}

/// Runs the runtime subagent state json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_subagent_state_json(
    session: &Session,
    pane: &mez_mux::layout::Pane,
    agent_id: &str,
    display_name: &str,
    spawn: &SubagentSpawnRequest,
    turn: Option<&RuntimeAgentPromptTurnStart>,
    model_profile: Option<&str>,
) -> String {
    let model = model_profile.unwrap_or("default");
    let visible = matches!(
        spawn.placement.as_str(),
        "visible" | "focused" | "new-pane" | "new-window"
    );
    format!(
        r#"{{"id":"{}","version":1,"display_name":"{}","session_id":"{}","pane_id":"{}","status":"{}","visible":{},"conversation_id":"{}","model_profile":"{}","cooperation_mode":"{}","read_scopes":{},"write_scopes":{},"last_turn_id":"{}","role":"{}","parent_agent_id":"{}"}}"#,
        json_escape(agent_id),
        json_escape(display_name),
        json_escape(session.id.as_str()),
        json_escape(pane.id.as_str()),
        runtime_agent_status_name(turn.map_or(AgentTurnState::Queued, |t| t.state)),
        visible,
        json_escape(agent_id),
        json_escape(model),
        runtime_cooperation_mode_name(spawn.cooperation_mode),
        runtime_string_array_json(&spawn.read_scopes),
        runtime_string_array_json(&spawn.write_scopes),
        json_escape(turn.map_or("", |t| t.turn_id.as_str())),
        json_escape(&spawn.requested_role),
        json_escape(&spawn.parent_agent_id)
    )
}

/// Runs the runtime agent shell stop response json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_agent_shell_stop_response_json(
    pane_id: &str,
    input: &str,
    stopped: &RuntimeAgentTurnStop,
) -> String {
    format!(
        r#"{{"pane_id":"{}","input":"{}","kind":"mutated","command":"stop","visibility":"{}","body":"turn_id={} state=cancelled scheduler_cancelled={} interrupted_shell_transactions={}"}}"#,
        json_escape(pane_id),
        json_escape(input),
        agent_shell_visibility_json_name(stopped.visibility),
        json_escape(&stopped.turn_id),
        stopped.scheduler_cancelled,
        stopped.interrupted_shell_transactions
    )
}

/// Runs the runtime agent turn state name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_agent_turn_state_name(state: AgentTurnState) -> &'static str {
    match state {
        AgentTurnState::Queued => "queued",
        AgentTurnState::Running => "running",
        AgentTurnState::Blocked => "waiting_approval",
        AgentTurnState::Completed => "completed",
        AgentTurnState::Failed => "failed",
        AgentTurnState::Interrupted => "interrupted",
    }
}

/// Runs the runtime agent status name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_agent_status_name(state: AgentTurnState) -> &'static str {
    match state {
        AgentTurnState::Queued | AgentTurnState::Completed => "idle",
        AgentTurnState::Running => "running",
        AgentTurnState::Blocked => "waiting_approval",
        AgentTurnState::Failed => "failed",
        AgentTurnState::Interrupted => "stopped",
    }
}

/// Runs the runtime timestamp json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_timestamp_json(value: u64) -> String {
    format!(r#""{}""#, unix_seconds_to_rfc3339(value))
}

/// Runs the runtime optional timestamp json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_optional_timestamp_json(value: Option<u64>) -> String {
    value
        .map(runtime_timestamp_json)
        .unwrap_or_else(|| "null".to_string())
}

/// Runs the runtime pane readiness state name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_pane_readiness_state_name(state: PaneReadinessState) -> &'static str {
    match state {
        PaneReadinessState::Unknown => "unknown",
        PaneReadinessState::PromptCandidate => "prompt-candidate",
        PaneReadinessState::Probing => "probing",
        PaneReadinessState::Ready => "ready",
        PaneReadinessState::Busy => "busy",
        PaneReadinessState::Degraded => "degraded",
        PaneReadinessState::InteractiveBlocked
        | PaneReadinessState::FullScreen
        | PaneReadinessState::PasswordPrompt => "interactive-blocked",
    }
}

/// Runs the runtime hook execution status name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_hook_execution_status_name(status: HookExecutionStatus) -> &'static str {
    match status {
        HookExecutionStatus::Succeeded => "succeeded",
        HookExecutionStatus::Failed => "failed",
        HookExecutionStatus::TimedOut => "timed_out",
        HookExecutionStatus::Queued => "queued",
    }
}

/// Runs the runtime execution ready for provider continuation operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_execution_ready_for_provider_continuation(
    execution: &AgentTurnExecution,
) -> bool {
    execution.terminal_state == AgentTurnState::Running
        && !execution.final_turn
        && execution.action_results.iter().all(|result| {
            !matches!(result.status, ActionStatus::Running | ActionStatus::Blocked)
                && !result.is_error
        })
}

/// Runs the agent shell visibility json name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn agent_shell_visibility_json_name(visibility: AgentShellVisibility) -> &'static str {
    match visibility {
        AgentShellVisibility::Hidden => "hidden",
        AgentShellVisibility::Visible => "visible",
        AgentShellVisibility::HidePendingTaskCompletion => "hide-pending-task-completion",
    }
}

/// Runs the runtime command outcomes json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_command_outcomes_json(outcomes: &[CommandOutcome]) -> String {
    let outcomes = outcomes
        .iter()
        .map(runtime_command_outcome_json)
        .collect::<Vec<_>>();
    format!(
        r#"{{"executed":{},"outcomes":[{}]}}"#,
        outcomes.len(),
        outcomes.join(",")
    )
}

/// Runs the runtime command outcome json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_command_outcome_json(outcome: &CommandOutcome) -> String {
    match outcome {
        CommandOutcome::Noop { command } => {
            format!(r#"{{"command":"{}","kind":"noop"}}"#, json_escape(command))
        }
        CommandOutcome::Mutated { command } => format!(
            r#"{{"command":"{}","kind":"mutated"}}"#,
            json_escape(command)
        ),
        CommandOutcome::MutatedWithPaneCommand {
            command,
            shell_command,
            start_directory,
        } => format!(
            r#"{{"command":"{}","kind":"mutated_with_pane_command","shell_command":"{}","start_directory":{}}}"#,
            json_escape(command),
            json_escape(shell_command),
            optional_string_json(start_directory.as_deref())
        ),
        CommandOutcome::Display { command, body } => format!(
            r#"{{"command":"{}","kind":"display","body":"{}"}}"#,
            json_escape(command),
            json_escape(body)
        ),
        CommandOutcome::LayoutSave { command, name } => format!(
            r#"{{"command":"{}","kind":"layout_save","name":{},"body":"runtime layout repository required"}}"#,
            json_escape(command),
            optional_string_json(name.as_deref())
        ),
        CommandOutcome::LayoutLoad { command, selector } => format!(
            r#"{{"command":"{}","kind":"layout_load","selector":{},"body":"runtime layout repository required"}}"#,
            json_escape(command),
            runtime_layout_load_selector_json(selector)
        ),
    }
}

/// Renders a layout load selector for runtime command JSON diagnostics.
fn runtime_layout_load_selector_json(selector: &crate::command::LayoutLoadSelector) -> String {
    match selector {
        crate::command::LayoutLoadSelector::Name(name) => {
            format!(r#"{{"kind":"name","name":"{}"}}"#, json_escape(name))
        }
        crate::command::LayoutLoadSelector::Latest => r#"{"kind":"latest"}"#.to_string(),
    }
}

/// Runs the optional string json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn optional_string_json(value: Option<&str>) -> String {
    value
        .map(|value| format!(r#""{}""#, json_escape(value)))
        .unwrap_or_else(|| "null".to_string())
}

/// Runs the optional path json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn optional_path_json(value: Option<&Path>) -> String {
    value
        .map(|value| format!(r#""{}""#, json_escape(&value.to_string_lossy())))
        .unwrap_or_else(|| "null".to_string())
}

/// Runs the rendered client view json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn rendered_client_view_json(view: &RenderedClientView) -> String {
    let (presentation_lines, presentation_spans) =
        compose_client_presentation_with_styles(view, None);
    let lines = presentation_lines
        .iter()
        .map(|line| format!(r#""{}""#, json_escape(line)))
        .collect::<Vec<_>>();
    let line_style_spans = terminal_line_style_spans_json(&presentation_spans);
    let agent_prompt_region = view
        .agent_prompt_region
        .map(|region| {
            format!(
                r#"{{"row":{},"column":{},"columns":{},"rows":{}}}"#,
                region.row, region.column, region.columns, region.rows
            )
        })
        .unwrap_or_else(|| "null".to_string());
    format!(
        r#"{{"role":"{}","authoritative_size":{{"columns":{},"rows":{}}},"client_size":{{"columns":{},"rows":{}}},"requires_client_scroll":{},"viewport":{{"row":{},"column":{},"max_row":{},"max_column":{}}},"cursor":{{"row":{},"column":{},"visible":{},"style":"{}","blink":{},"blink_interval_ms":{}}},"output_modes":{{"application_keypad":{},"bracketed_paste":{},"host_mouse_reporting":{},"animation_refresh_interval_ms":{}}},"agent_prompt_region":{},"lines":[{}],"line_style_spans":{}}}"#,
        client_view_role_name(view.role),
        view.authoritative_size.columns,
        view.authoritative_size.rows,
        view.client_size.columns,
        view.client_size.rows,
        view.requires_client_scroll,
        view.viewport_row,
        view.viewport_column,
        max_viewport_row(view),
        max_viewport_column(view),
        view.cursor_row,
        view.cursor_column,
        view.cursor_visible,
        terminal_cursor_style_name(view.cursor_style),
        view.cursor_blink,
        view.cursor_blink_interval_ms,
        view.application_keypad,
        view.bracketed_paste,
        view.host_mouse_reporting,
        view.animation_refresh_interval_ms,
        agent_prompt_region,
        lines.join(","),
        line_style_spans
    )
}

/// Runs the ui theme json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn ui_theme_json(theme: &UiTheme) -> String {
    format!(
        r#"{{"name":"{}","colors":{},"prompt":{},"agent_prompt":{},"display_overlay":{}}}"#,
        json_escape(&theme.name),
        ui_theme_colors_json(theme),
        ui_color_pair_json(theme.colors.prompt),
        ui_color_pair_json(theme.colors.agent_prompt),
        ui_color_pair_json(theme.colors.display_overlay)
    )
}

/// Runs the ui theme colors json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn ui_theme_colors_json(theme: &UiTheme) -> String {
    format!(
        r#"{{"window_frame":{},"window_active":{},"window_inactive":{},"pane_frame_active":{},"pane_frame_inactive":{},"pane_border_active":{},"pane_border_inactive":{},"pane_divider":{},"frame_fill":{},"scroll_indicator":{},"pane_pwd":{},"window_status_uptime":{},"window_status_datetime":{},"prompt":{},"agent_prompt":{},"agent_transcript_user":{},"agent_transcript_assistant":{},"agent_transcript_status":{},"agent_transcript_error":{},"agent_transcript_command":{},"agent_model":{},"agent_reasoning":{},"agent_status_idle":{},"agent_status_running":{},"agent_status_blocked":{},"agent_status_failed":{},"display_overlay":{},"copy_selection":{}}}"#,
        ui_color_pair_json(theme.colors.window_frame),
        ui_color_pair_json(theme.colors.window_active),
        ui_color_pair_json(theme.colors.window_inactive),
        ui_color_pair_json(theme.colors.pane_frame_active),
        ui_color_pair_json(theme.colors.pane_frame_inactive),
        ui_color_pair_json(theme.colors.pane_border_active),
        ui_color_pair_json(theme.colors.pane_border_inactive),
        ui_color_pair_json(theme.colors.pane_divider),
        ui_color_pair_json(theme.colors.frame_fill),
        ui_color_pair_json(theme.colors.scroll_indicator),
        ui_color_pair_json(theme.colors.pane_pwd),
        ui_color_pair_json(theme.colors.window_status_uptime),
        ui_color_pair_json(theme.colors.window_status_datetime),
        ui_color_pair_json(theme.colors.prompt),
        ui_color_pair_json(theme.colors.agent_prompt),
        ui_color_pair_json(theme.colors.agent_transcript_user),
        ui_color_pair_json(theme.colors.agent_transcript_assistant),
        ui_color_pair_json(theme.colors.agent_transcript_status),
        ui_color_pair_json(theme.colors.agent_transcript_error),
        ui_color_pair_json(theme.colors.agent_transcript_command),
        ui_color_pair_json(theme.colors.agent_model),
        ui_color_pair_json(theme.colors.agent_reasoning),
        ui_color_pair_json(theme.colors.agent_status_idle),
        ui_color_pair_json(theme.colors.agent_status_running),
        ui_color_pair_json(theme.colors.agent_status_blocked),
        ui_color_pair_json(theme.colors.agent_status_failed),
        ui_color_pair_json(theme.colors.display_overlay),
        ui_color_pair_json(theme.colors.copy_selection)
    )
}

/// Runs the ui color pair json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn ui_color_pair_json(pair: UiColorPair) -> String {
    format!(
        r#"{{"foreground":{},"background":{}}}"#,
        terminal_color_json(Some(pair.foreground)),
        terminal_color_json(Some(pair.background))
    )
}

/// Runs the terminal cursor style name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn terminal_cursor_style_name(style: TerminalCursorStyle) -> &'static str {
    match style {
        TerminalCursorStyle::Block => "block",
        TerminalCursorStyle::Underline => "underline",
        TerminalCursorStyle::Bar => "bar",
    }
}

/// Runs the terminal line style spans json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn terminal_line_style_spans_json(line_spans: &[Vec<TerminalStyleSpan>]) -> String {
    let lines = line_spans
        .iter()
        .map(|spans| terminal_style_spans_json(spans))
        .collect::<Vec<_>>();
    format!("[{}]", lines.join(","))
}

/// Runs the terminal style spans json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn terminal_style_spans_json(spans: &[TerminalStyleSpan]) -> String {
    let spans = spans
        .iter()
        .map(|span| {
            format!(
                r#"{{"start":{},"length":{},"rendition":{}}}"#,
                span.start,
                span.length,
                terminal_rendition_json(span.rendition)
            )
        })
        .collect::<Vec<_>>();
    format!("[{}]", spans.join(","))
}

/// Runs the terminal rendition json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn terminal_rendition_json(rendition: GraphicRendition) -> String {
    format!(
        r#"{{"bold":{},"dim":{},"italic":{},"underline":{},"double_underline":{},"strikethrough":{},"inverse":{},"hidden":{},"foreground":{},"background":{}}}"#,
        rendition.bold,
        rendition.dim,
        rendition.italic,
        rendition.underline,
        rendition.double_underline,
        rendition.strikethrough,
        rendition.inverse,
        rendition.hidden,
        terminal_color_json(rendition.foreground),
        terminal_color_json(rendition.background)
    )
}

/// Runs the terminal color json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn terminal_color_json(color: Option<TerminalColor>) -> String {
    match color {
        Some(TerminalColor::Indexed(index)) => {
            format!(r#"{{"kind":"indexed","index":{index}}}"#)
        }
        Some(TerminalColor::Rgb(red, green, blue)) => {
            format!(r#"{{"kind":"rgb","red":{red},"green":{green},"blue":{blue}}}"#)
        }
        None => "null".to_string(),
    }
}

/// Runs the client view role name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn client_view_role_name(role: ClientViewRole) -> &'static str {
    match role {
        ClientViewRole::Primary => "primary",
        ClientViewRole::Observer => "observer",
        ClientViewRole::PendingObserver => "pending_observer",
    }
}

/// Runs the runtime pane by id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_pane_by_id<'a>(
    session: &'a Session,
    pane_id: &str,
) -> Result<(&'a mez_mux::layout::Window, &'a mez_mux::layout::Pane)> {
    session
        .windows()
        .iter()
        .find_map(|window| {
            window
                .panes()
                .iter()
                .find(|pane| pane.id.as_str() == pane_id)
                .map(|pane| (window, pane))
        })
        .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "pane not found"))
}

/// Runs the runtime mutating method operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_mutating_method(method: &str) -> bool {
    matches!(
        method,
        "window/create"
            | "window/close"
            | "pane/create"
            | "pane/resize"
            | "pane/swap"
            | "pane/break"
            | "pane/join"
            | "pane/move"
            | "pane/close"
            | "observer/approve"
            | "observer/reject"
            | "observer/revoke"
            | "terminal/step"
            | "terminal/command"
            | "agent/shell/command"
            | "agent/spawn"
            | "project/trust/decide"
            | "project/trust/revoke"
            | "mcp/retry"
            | "session/kill"
    )
}

/// Runs the agent state control method operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn agent_state_control_method(method: &str) -> bool {
    matches!(
        method,
        "agent/list" | "agent/task/list" | "agent/shell/show" | "agent/shell/hide"
    )
}

/// Runs the runtime split direction operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_split_direction(value: &str) -> Result<SplitDirection> {
    match value {
        "vertical" | "right" | "left" => Ok(SplitDirection::Vertical),
        "horizontal" | "above" | "below" | "up" | "down" => Ok(SplitDirection::Horizontal),
        _ => Err(MezError::invalid_args("unsupported pane split direction")),
    }
}

/// Runs the runtime subagent spawn request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_subagent_spawn_request(
    params: &str,
    caller_is_primary: bool,
) -> Result<SubagentSpawnRequest> {
    let value = runtime_json_value(params)?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args("agent/spawn params must be an object"))?;
    let parent_agent_id = match object.get("parent_agent") {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Object(target)) => target
            .get("agent_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| MezError::invalid_args("agent/spawn parent_agent requires agent_id"))?,
        _ => {
            return Err(MezError::invalid_args("agent/spawn requires parent_agent"));
        }
    };
    let requested_role = object
        .get("role")
        .and_then(Value::as_str)
        .map(runtime_subagent_role_name)
        .transpose()?
        .ok_or_else(|| MezError::invalid_args("agent/spawn requires role"))?;
    let placement = runtime_subagent_placement_mode(params)?.name().to_string();
    let cooperation_mode_defaulted = !object.contains_key("cooperation_mode");
    let cooperation_mode = object
        .get("cooperation_mode")
        .and_then(Value::as_str)
        .map(runtime_cooperation_mode)
        .transpose()?
        .unwrap_or(CooperationMode::ExploreOnly);
    let read_scopes_defaulted = !object.contains_key("read_scopes");
    let read_scopes = runtime_value_string_array(object.get("read_scopes"), "read_scopes")?;
    let write_scopes_defaulted = !object.contains_key("write_scopes");
    let write_scopes = runtime_value_string_array(object.get("write_scopes"), "write_scopes")?;
    let task_prompt = object
        .get("prompt")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| MezError::invalid_args("agent/spawn requires prompt"))?;
    Ok(SubagentSpawnRequest {
        parent_agent_id,
        requested_role,
        placement,
        cooperation_mode,
        cooperation_mode_defaulted,
        read_scopes,
        read_scopes_defaulted,
        write_scopes,
        write_scopes_defaulted,
        task_prompt,
        explicit_user_approval: caller_is_primary,
        skip_initial_turn: object
            .get("skip_initial_turn")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

/// Runs the runtime subagent placement mode operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_subagent_placement_mode(params: &str) -> Result<RuntimeSubagentPlacement> {
    let value = runtime_json_value(params)?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args("agent/spawn params must be an object"))?;
    let placement = object
        .get("placement")
        .ok_or_else(|| MezError::invalid_args("agent/spawn requires placement"))?;
    match placement {
        Value::String(mode) => runtime_subagent_placement_from_fields(mode, None),
        Value::Object(fields) => {
            let mode = fields
                .get("mode")
                .and_then(Value::as_str)
                .ok_or_else(|| MezError::invalid_args("agent/spawn placement requires mode"))?;
            runtime_subagent_placement_from_fields(mode, Some(fields))
        }
        _ => Err(MezError::invalid_args(
            "agent/spawn placement must be a string or object",
        )),
    }
}

/// Runs the runtime subagent placement from fields operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_subagent_placement_from_fields(
    mode: &str,
    fields: Option<&serde_json::Map<String, Value>>,
) -> Result<RuntimeSubagentPlacement> {
    match mode {
        "new-pane" => {
            let direction = fields
                .and_then(|fields| fields.get("split").or_else(|| fields.get("direction")))
                .and_then(Value::as_str)
                .map(runtime_split_direction)
                .transpose()?
                .unwrap_or(SplitDirection::Vertical);
            let select = fields
                .and_then(|fields| fields.get("select"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Ok(RuntimeSubagentPlacement::NewPane { direction, select })
        }
        "new-window" => {
            let name = fields
                .and_then(|fields| fields.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("agent")
                .to_string();
            let select = fields
                .and_then(|fields| fields.get("select"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Ok(RuntimeSubagentPlacement::NewWindow { name, select })
        }
        _ => Err(MezError::invalid_args(
            "agent/spawn placement mode must be new-pane or new-window",
        )),
    }
}

impl RuntimeSubagentPlacement {
    /// Runs the name operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::NewPane { .. } => "new-pane",
            Self::NewWindow { .. } => "new-window",
        }
    }
}

/// Runs the runtime subagent role name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_subagent_role_name(value: &str) -> Result<String> {
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        return Err(MezError::invalid_args("subagent role is invalid"));
    }
    Ok(value.to_string())
}

/// Runs the runtime cooperation mode operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_cooperation_mode(value: &str) -> Result<CooperationMode> {
    match value {
        "safety" | "scope" | "scoped" => Ok(CooperationMode::ExploreOnly),
        "explore-only" | "explore_only" => Ok(CooperationMode::ExploreOnly),
        "read-only" | "read_only" | "readonly" | "read" => Ok(CooperationMode::ExploreOnly),
        "parallel" | "parallel-read" | "parallel_read" => Ok(CooperationMode::ExploreOnly),
        "owned-write" | "owned_write" => Ok(CooperationMode::OwnedWrite),
        "coordinated-write" | "coordinated_write" => Ok(CooperationMode::CoordinatedWrite),
        "serial-write" | "serial_write" => Ok(CooperationMode::SerialWrite),
        "unrestricted" => Ok(CooperationMode::Unrestricted),
        _ => Err(MezError::invalid_args(
            "unsupported subagent cooperation mode",
        )),
    }
}

/// Runs the runtime cooperation mode name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_cooperation_mode_name(mode: CooperationMode) -> &'static str {
    match mode {
        CooperationMode::ExploreOnly => "explore-only",
        CooperationMode::OwnedWrite => "owned-write",
        CooperationMode::CoordinatedWrite => "coordinated-write",
        CooperationMode::SerialWrite => "serial-write",
        CooperationMode::Unrestricted => "unrestricted",
    }
}

/// Runs the runtime value string array operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_value_string_array(
    value: Option<&Value>,
    field: &str,
) -> Result<Vec<String>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| MezError::invalid_args(format!("agent/spawn {field} must be an array")))?;
    values
        .iter()
        .map(|value| {
            value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                MezError::invalid_args(format!("agent/spawn {field} values must be strings"))
            })
        })
        .collect()
}

/// Runs the pane navigation direction operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn pane_navigation_direction(direction: PaneFocusDirection) -> PaneNavigationDirection {
    match direction {
        PaneFocusDirection::Up => PaneNavigationDirection::Up,
        PaneFocusDirection::Down => PaneNavigationDirection::Down,
        PaneFocusDirection::Left => PaneNavigationDirection::Left,
        PaneFocusDirection::Right => PaneNavigationDirection::Right,
    }
}

/// Runs the mux action name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mux_action_name(action: MuxAction) -> &'static str {
    match action {
        MuxAction::SendPrefixToPane => "send-prefix",
        MuxAction::EnterCommandPrompt => "command-prompt",
        MuxAction::ListKeyBindings => "list-keys",
        MuxAction::DetachPrimaryClient => "detach-client",
        MuxAction::ChooseClientOrObserverToDetach => "choose-client",
        MuxAction::NewWindow => "new-window",
        MuxAction::NewGroup => "new-group",
        MuxAction::RenameWindow => "rename-window",
        MuxAction::KillWindowAfterConfirmation => "kill-window",
        MuxAction::FocusWindow(_) => "focus-window",
        MuxAction::FocusGroup(_) => "focus-group",
        MuxAction::SplitPaneVertical => "split-pane-vertical",
        MuxAction::SplitPaneHorizontal => "split-pane-horizontal",
        MuxAction::FocusPane(_) => "focus-pane",
        MuxAction::CyclePane => "cycle-pane",
        MuxAction::FocusLastPane => "last-pane",
        MuxAction::ShowPaneIndexes => "display-panes",
        MuxAction::TogglePaneZoom => "zoom-pane",
        MuxAction::CycleLayouts => "next-layout",
        MuxAction::KillPaneAfterConfirmation => "kill-pane",
        MuxAction::BreakPaneToNewWindow => "break-pane",
        MuxAction::SwapPanePrevious => "swap-pane-previous",
        MuxAction::SwapPaneNext => "swap-pane-next",
        MuxAction::EnterCopyMode => "copy-mode",
        MuxAction::EnterCopyModeAndPageUp => "copy-mode-page-up",
        MuxAction::PasteBuffer(_) => "paste-buffer",
        MuxAction::ListPasteBuffers => "list-buffers",
        MuxAction::DeleteMostRecentPasteBuffer => "delete-buffer",
        MuxAction::ChoosePendingObservers => "choose-observer",
        MuxAction::ShowMessages => "show-messages",
        MuxAction::ToggleAgentShell => "agent-shell",
    }
}

/// Runs the mux action command prompt prefill operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mux_action_command_prompt_prefill(action: MuxAction) -> Option<&'static str> {
    match action {
        MuxAction::EnterCommandPrompt => Some(""),
        MuxAction::FocusWindow(WindowFocusTarget::PromptForIndex) => Some("select-window "),
        MuxAction::FocusWindow(WindowFocusTarget::PromptForNewIndex) => Some("move-window -t "),
        MuxAction::RenameWindow => Some("rename-window "),
        MuxAction::KillWindowAfterConfirmation => Some("kill-window --force "),
        MuxAction::KillPaneAfterConfirmation => Some("kill-pane --force "),
        _ => None,
    }
}

/// Runs the mouse action name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mouse_action_name(action: MouseAction) -> &'static str {
    match action {
        MouseAction::Ignore => "ignore",
        MouseAction::ForwardToPane => "forward-to-pane",
        MouseAction::FocusWindow { .. } => "focus-window",
        MouseAction::FocusGroup { .. } => "focus-group",
        MouseAction::PressWindowAction { .. } => "press-window-action",
        MouseAction::ReleaseWindowAction { .. } => "release-window-action",
        MouseAction::CancelWindowAction => "cancel-window-action",
        MouseAction::OpenPaneAgentStatusSelector { .. } => "open-pane-agent-status-selector",
        MouseAction::HoverPaneAgentStatusSelector { .. } => "hover-pane-agent-status-selector",
        MouseAction::SelectPaneAgentStatusSelector { .. } => "select-pane-agent-status-selector",
        MouseAction::ScrollPaneAgentStatusSelector { .. } => "scroll-pane-agent-status-selector",
        MouseAction::ClosePaneAgentStatusSelector => "close-pane-agent-status-selector",
        MouseAction::BeginDisplayOverlaySelection { .. } => "begin-display-overlay-selection",
        MouseAction::UpdateDisplayOverlaySelection { .. } => "update-display-overlay-selection",
        MouseAction::FinishDisplayOverlaySelection { .. } => "finish-display-overlay-selection",
        MouseAction::SelectDisplayOverlay { .. } => "select-display-overlay",
        MouseAction::ScrollDisplayOverlay { .. } => "scroll-display-overlay",
        MouseAction::FocusPane(_) => "focus-pane",
        MouseAction::FocusPaneOnly(_) => "focus-pane-only",
        MouseAction::PasteClipboard(_) => "paste-clipboard",
        MouseAction::ShowWindowChooser { .. } => "show-window-chooser",
        MouseAction::ResizePane { .. } => "resize-pane",
        MouseAction::FinishResizePane => "finish-resize-pane",
        MouseAction::CopySelectionStart(_) => "copy-selection-start",
        MouseAction::CopyWord(_) => "copy-word",
        MouseAction::CopySelectionUpdate(_) => "copy-selection-update",
        MouseAction::CopySelectionFinish(_) => "copy-selection-finish",
        MouseAction::ScrollHistory { .. } => "scroll-history",
    }
}

/// Runs the runtime copy position for view operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_copy_position_for_view(
    copy_mode: &CopyMode,
    position: CopyPosition,
) -> CopyPosition {
    copy_mode.clamp_position(CopyPosition {
        line: copy_mode.scroll_top().saturating_add(position.line),
        column: position.column,
    })
}

/// Runs the runtime json rpc error operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_json_rpc_error(
    id: &str,
    kind: crate::error::MezErrorKind,
    message: &str,
) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{},"error":{{"code":{},"message":"{}","data":{{"mezzanine_code":"{}"}}}}}}"#,
        id,
        runtime_error_code(kind),
        json_escape(message),
        runtime_mezzanine_error_code(kind)
    )
}

/// Runs the runtime error code operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_error_code(kind: crate::error::MezErrorKind) -> i32 {
    match kind {
        crate::error::MezErrorKind::InvalidArgs => -32602,
        crate::error::MezErrorKind::InvalidState => -32004,
        crate::error::MezErrorKind::Conflict => -32006,
        crate::error::MezErrorKind::NotFound => -32005,
        crate::error::MezErrorKind::Forbidden => -32002,
        crate::error::MezErrorKind::NotImplemented => -32601,
        crate::error::MezErrorKind::Config | crate::error::MezErrorKind::Io => -32000,
    }
}

/// Runs the runtime mezzanine error code operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_mezzanine_error_code(kind: crate::error::MezErrorKind) -> &'static str {
    match kind {
        crate::error::MezErrorKind::InvalidArgs => "invalid_params",
        crate::error::MezErrorKind::InvalidState => "invalid_state",
        crate::error::MezErrorKind::Conflict => "conflict",
        crate::error::MezErrorKind::NotFound => "not_found",
        crate::error::MezErrorKind::Forbidden => "forbidden",
        crate::error::MezErrorKind::NotImplemented => "method_not_found",
        crate::error::MezErrorKind::Config | crate::error::MezErrorKind::Io => "internal_error",
    }
}

/// Runs the runtime json string field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_json_string_field(body: &str, field: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .as_object()?
        .get(field)?
        .as_str()
        .map(ToOwned::to_owned)
}

/// Runs the runtime json bool field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_json_bool_field(body: &str, field: &str) -> Option<bool> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .as_object()?
        .get(field)?
        .as_bool()
}

/// Runs the runtime json creation command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_json_creation_command(body: &str) -> Result<Option<String>> {
    let value = runtime_json_value(body)?;
    let Some(command) = value
        .as_object()
        .and_then(|object| object.get("shell_command"))
    else {
        return Ok(None);
    };
    match command {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(command) => Ok(Some(command.clone())),
        serde_json::Value::Array(values) => {
            let mut argv = Vec::with_capacity(values.len());
            for value in values {
                let argument = value.as_str().ok_or_else(|| {
                    MezError::invalid_args("shell_command array must contain only strings")
                })?;
                argv.push(argument.to_string());
            }
            Ok(shell_command_from_argv(&argv).map(Some)?)
        }
        _ => Err(MezError::invalid_args(
            "shell_command must be a string, string array, or null",
        )),
    }
}

/// Runs the runtime json start directory operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_json_start_directory(body: &str) -> Result<Option<PathBuf>> {
    let value = runtime_json_value(body)?;
    let Some(start_directory) = value
        .as_object()
        .and_then(|object| object.get("start_directory"))
    else {
        return Ok(None);
    };
    match start_directory {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(path) if path.is_empty() => {
            Err(MezError::invalid_args("start_directory must not be empty"))
        }
        serde_json::Value::String(path) => Ok(Some(PathBuf::from(path))),
        _ => Err(MezError::invalid_args(
            "start_directory must be a string or null",
        )),
    }
}

/// Runs the runtime json optional size field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_json_optional_size_field(
    body: &str,
    field: &str,
) -> Result<Option<PaneSizeSpec>> {
    let value = runtime_json_value(body)?;
    let Some(size) = value.as_object().and_then(|object| object.get(field)) else {
        return Ok(None);
    };
    if size.is_null() {
        return Ok(None);
    }
    parse_runtime_size_spec(size, "pane size").map(Some)
}

/// Runs the runtime initialize terminal size operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_initialize_terminal_size(
    request: &crate::control::JsonRpcRequest,
) -> Option<Size> {
    let params = request.params.as_ref()?;
    let value = serde_json::from_str::<serde_json::Value>(params).ok()?;
    let terminal = value
        .as_object()?
        .get("client")?
        .as_object()?
        .get("terminal")?
        .as_object()?;
    let columns = terminal.get("columns")?.as_u64()?;
    let rows = terminal.get("rows")?.as_u64()?;
    Size::new(u16::try_from(columns).ok()?, u16::try_from(rows).ok()?).ok()
}

/// Runs the runtime initialize requested primary operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_initialize_requested_primary(
    request: &crate::control::JsonRpcRequest,
) -> bool {
    request
        .params
        .as_ref()
        .and_then(|params| serde_json::from_str::<serde_json::Value>(params).ok())
        .and_then(|value| {
            value
                .as_object()?
                .get("requested_role")?
                .as_str()
                .map(ToOwned::to_owned)
        })
        .as_deref()
        == Some("primary")
}

/// Returns whether one initialize request is asking for a pending observer role.
pub(super) fn runtime_initialize_requested_observer(
    request: &crate::control::JsonRpcRequest,
) -> bool {
    request
        .params
        .as_ref()
        .and_then(|params| serde_json::from_str::<serde_json::Value>(params).ok())
        .and_then(|value| {
            value
                .as_object()?
                .get("requested_role")?
                .as_str()
                .map(ToOwned::to_owned)
        })
        .as_deref()
        == Some("observer")
}

/// Runs the current unix seconds operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// Formats an elapsed duration for human-facing agent status lines.
pub(super) fn runtime_agent_turn_duration_display(elapsed_seconds: u64) -> String {
    let hours = elapsed_seconds / 3600;
    let minutes = (elapsed_seconds % 3600) / 60;
    let seconds = elapsed_seconds % 60;
    if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

/// Returns the current Unix timestamp in milliseconds, saturating when the
/// host clock cannot fit the millisecond count into the runtime representation.
pub(super) fn current_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Runs the runtime json size operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_json_size(body: &str) -> Result<PaneSizeSpec> {
    let value = runtime_json_value(body)?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args("runtime control params must be an object"))?;
    let size = object
        .get("size")
        .ok_or_else(|| MezError::invalid_args("pane/resize requires size"))?;
    parse_runtime_size_spec(size, "pane size")
}

/// Runs the runtime json optional client size operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_json_optional_client_size(body: &str) -> Result<Option<Size>> {
    let value = runtime_json_value(body)?;
    let Some(size) = value
        .as_object()
        .and_then(|object| object.get("client_size").or_else(|| object.get("size")))
    else {
        return Ok(None);
    };
    let size = size
        .as_object()
        .ok_or_else(|| MezError::invalid_args("terminal/step client_size must be an object"))?;
    let columns = size
        .get("columns")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| MezError::invalid_args("terminal/step client_size requires columns"))?;
    let rows = size
        .get("rows")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| MezError::invalid_args("terminal/step client_size requires rows"))?;
    let columns = u16::try_from(columns)
        .map_err(|_| MezError::invalid_args("terminal/step client_size columns is out of range"))?;
    let rows = u16::try_from(rows)
        .map_err(|_| MezError::invalid_args("terminal/step client_size rows is out of range"))?;
    Ok(Some(Size::new(columns, rows)?))
}

/// Runs the runtime json optional view offset operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_json_optional_view_offset(body: &str) -> Result<Option<(usize, usize)>> {
    let value = runtime_json_value(body)?;
    let Some(offset) = value
        .as_object()
        .and_then(|object| object.get("view_offset").or_else(|| object.get("viewport")))
    else {
        return Ok(None);
    };
    let offset = offset
        .as_object()
        .ok_or_else(|| MezError::invalid_args("terminal/view view_offset must be an object"))?;
    let row = offset
        .get("row")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let column = offset
        .get("column")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let row = usize::try_from(row)
        .map_err(|_| MezError::invalid_args("terminal/view view_offset row is out of range"))?;
    let column = usize::try_from(column)
        .map_err(|_| MezError::invalid_args("terminal/view view_offset column is out of range"))?;
    Ok(Some((row, column)))
}

/// Runs the runtime json input bytes operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_json_input_bytes(body: &str) -> Result<Vec<u8>> {
    let value = runtime_json_value(body)?;
    let Some(input) = value
        .as_object()
        .and_then(|object| object.get("input_bytes"))
    else {
        return Ok(Vec::new());
    };
    let input = input
        .as_array()
        .ok_or_else(|| MezError::invalid_args("terminal/step input_bytes must be an array"))?;
    input
        .iter()
        .map(|value| {
            let byte = value.as_u64().ok_or_else(|| {
                MezError::invalid_args("terminal/step input_bytes entries must be integers")
            })?;
            u8::try_from(byte).map_err(|_| {
                MezError::invalid_args("terminal/step input_bytes entries must be bytes")
            })
        })
        .collect()
}

/// Runs the runtime json value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_json_value(body: &str) -> Result<serde_json::Value> {
    serde_json::from_str::<serde_json::Value>(body).map_err(|error| {
        MezError::invalid_args(format!("runtime control params are invalid JSON: {error}"))
    })
}

/// Runs the parse runtime size spec operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_runtime_size_spec(value: &serde_json::Value, context: &str) -> Result<PaneSizeSpec> {
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args(format!("{context} must be an object")))?;
    let mode = object
        .get("mode")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| MezError::invalid_args(format!("{context} requires mode")))?;
    match mode {
        "cells" => {
            let columns = optional_runtime_size_u16(object.get("columns"), "columns")?;
            let rows = optional_runtime_size_u16(object.get("rows"), "rows")?;
            if columns.is_none() && rows.is_none() {
                return Err(MezError::invalid_args(
                    "cells size requires columns or rows",
                ));
            }
            Ok(PaneSizeSpec::Cells { columns, rows })
        }
        "percent" => {
            let percent = required_runtime_size_u16(object.get("percent"), "percent")?;
            let axis = match object
                .get("axis")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("both")
            {
                "columns" | "horizontal" => ResizeAxis::Columns,
                "rows" | "vertical" => ResizeAxis::Rows,
                "both" => ResizeAxis::Both,
                _ => return Err(MezError::invalid_args("percent size axis is invalid")),
            };
            Ok(PaneSizeSpec::Percent { percent, axis })
        }
        "delta" => {
            let direction = object
                .get("direction")
                .and_then(serde_json::Value::as_str)
                .and_then(ResizeDirection::from_name)
                .ok_or_else(|| MezError::invalid_args("delta size direction is invalid"))?;
            let amount = required_runtime_size_u16(object.get("amount"), "amount")?;
            Ok(PaneSizeSpec::Delta { direction, amount })
        }
        "edge" => {
            let edge = object
                .get("edge")
                .and_then(serde_json::Value::as_str)
                .and_then(ResizeDirection::from_name)
                .ok_or_else(|| MezError::invalid_args("edge size edge is invalid"))?;
            let amount = required_runtime_size_u16(object.get("amount"), "amount")?;
            Ok(PaneSizeSpec::Edge { edge, amount })
        }
        _ => Err(MezError::invalid_args("size mode is invalid")),
    }
}

/// Runs the optional runtime size u16 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_runtime_size_u16(
    value: Option<&serde_json::Value>,
    field: &'static str,
) -> Result<Option<u16>> {
    value
        .map(|value| required_runtime_size_u16(Some(value), field))
        .transpose()
}

/// Runs the required runtime size u16 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn required_runtime_size_u16(
    value: Option<&serde_json::Value>,
    field: &'static str,
) -> Result<u16> {
    let value = value
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| MezError::invalid_args(format!("size {field} must be a number")))?;
    u16::try_from(value)
        .map_err(|_| MezError::invalid_args(format!("size {field} is out of range")))
}
