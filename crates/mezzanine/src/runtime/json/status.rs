//! Runtime status, terminal-step, and agent-shell JSON projections.

use super::{
    ActionStatus, AgentShellCommandOutcome, AgentShellVisibility, AgentTurnExecution,
    AgentTurnState, AttachedClientStepApplication, HookExecutionStatus, PaneReadinessState,
    RenderedClientView, RuntimeAgentPromptTurnStart, RuntimeAgentTurnStop, Session,
    SubagentSpawnRequest, json_escape, rendered_client_view_json, runtime_cooperation_mode_name,
    runtime_string_array_json, unix_seconds_to_rfc3339,
};

// Runtime JSON serialization, parsing, and name mapping helpers.

/// Runs the runtime terminal step result json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_terminal_step_result_json(
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
        .map(|view| super::presentation::ui_theme_json(&view.ui_theme))
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
pub(crate) fn runtime_agent_shell_command_response_json(
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
pub(crate) fn runtime_agent_shell_prompt_turn_response_json(
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
pub(crate) fn runtime_agent_turn_state_json(started: &RuntimeAgentPromptTurnStart) -> String {
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
pub(crate) fn runtime_subagent_state_json(
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
pub(crate) fn runtime_agent_shell_stop_response_json(
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
pub(crate) fn runtime_agent_turn_state_name(state: AgentTurnState) -> &'static str {
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
pub(crate) fn runtime_pane_readiness_state_name(state: PaneReadinessState) -> &'static str {
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
pub(crate) fn runtime_hook_execution_status_name(status: HookExecutionStatus) -> &'static str {
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
pub(crate) fn runtime_execution_ready_for_provider_continuation(
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
pub(crate) fn agent_shell_visibility_json_name(visibility: AgentShellVisibility) -> &'static str {
    match visibility {
        AgentShellVisibility::Hidden => "hidden",
        AgentShellVisibility::Visible => "visible",
        AgentShellVisibility::HidePendingTaskCompletion => "hide-pending-task-completion",
    }
}
