//! Agent, shell session, task list, and model-profile state projection.

use super::{
    AgentShellCommandOutcome, AgentShellSession, AgentShellStore, AgentShellVisibility,
    AgentTurnLedger, AgentTurnState, BTreeMap, JsonRpcRequest, MezError, Result, Session, Window,
    execute_agent_shell_command, json_escape, json_optional_string, json_string_field, pane_by_id,
    pane_target_checked_resolved, parse_json_object_value, require_idempotency_key,
    require_session_target_matches_value, state_request_session_target_matches,
    target_or_active_pane, unix_seconds_to_rfc3339,
};

/// Runs the agents json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn agents_json(session: &Session) -> String {
    let agents = session
        .windows()
        .iter()
        .flat_map(|window| {
            window
                .panes()
                .iter()
                .map(|pane| agent_state_json(session.id.as_str(), window, pane, false))
        })
        .collect::<Vec<_>>();
    format!("[{}]", agents.join(","))
}

/// Runs the agents json for params operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn agents_json_for_params(
    session: &Session,
    params: Option<&str>,
) -> Result<String> {
    state_request_session_target_matches(session, params, "agent/list params")?;
    Ok(agents_json(session))
}

/// Serializes `agent/list` with optional pane-keyed model profile names.
///
/// The optional map lets live runtime dispatch replace the generic offline
/// `default` profile placeholder with the selected profile from runtime state
/// while keeping generic control fixtures unchanged.
pub(in crate::control) fn dispatch_agent_list_with_store_and_model_profiles(
    request: &JsonRpcRequest,
    session: &Session,
    agent_store: &AgentShellStore,
    model_profiles_by_pane: Option<&BTreeMap<String, String>>,
) -> Result<String> {
    state_request_session_target_matches(session, request.params.as_deref(), "agent/list params")?;
    let agents = session
        .windows()
        .iter()
        .flat_map(|window| {
            window.panes().iter().map(|pane| {
                let model_profile = model_profiles_by_pane
                    .and_then(|profiles| profiles.get(pane.id.as_str()))
                    .map(String::as_str)
                    .unwrap_or("default");
                agent_store.get(pane.id.as_str()).map_or_else(
                    || {
                        agent_state_json_with_model_profile(
                            session.id.as_str(),
                            window,
                            pane,
                            false,
                            model_profile,
                        )
                    },
                    |agent_session| {
                        agent_state_json_with_shell_session_and_model_profile(
                            session.id.as_str(),
                            pane,
                            agent_session,
                            model_profile,
                        )
                    },
                )
            })
        })
        .collect::<Vec<_>>();
    Ok(format!(r#"{{"agents":[{}]}}"#, agents.join(",")))
}

/// Runs the dispatch agent shell visibility with store operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn dispatch_agent_shell_visibility_with_store(
    request: &JsonRpcRequest,
    session: &Session,
    agent_store: &mut AgentShellStore,
) -> Result<String> {
    let params = request.params.as_deref().ok_or_else(|| {
        MezError::invalid_args(format!("{} requires a params object", request.method))
    })?;
    require_idempotency_key(params)?;
    let target = pane_target_checked_resolved(session, params)?;
    let (_window, pane) = target_or_active_pane(session, target.as_deref())?;
    let agent_session = if request.method == "agent/shell/show" {
        agent_store.enter_or_resume(pane.id.as_str())?
    } else {
        agent_store.request_exit(pane.id.as_str())?
    };
    let visible = !matches!(agent_session.visibility, AgentShellVisibility::Hidden);
    Ok(format!(
        r#"{{"agent":{},"visible":{}}}"#,
        agent_state_json_with_shell_session(session.id.as_str(), pane, agent_session),
        visible
    ))
}

/// Runs the dispatch agent shell command with store operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn dispatch_agent_shell_command_with_store(
    request: &JsonRpcRequest,
    session: &Session,
    agent_store: &mut AgentShellStore,
) -> Result<String> {
    let params = request
        .params
        .as_deref()
        .ok_or_else(|| MezError::invalid_args("agent/shell/command requires a params object"))?;
    require_idempotency_key(params)?;
    let input = json_string_field(params, "input")
        .ok_or_else(|| MezError::invalid_args("agent/shell/command requires input"))?;
    let window = session
        .active_window()
        .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
    let pane = window.active_pane();
    let visible = agent_store
        .get(pane.id.as_str())
        .is_some_and(|agent| agent.visibility == AgentShellVisibility::Visible);
    if !visible {
        return Err(MezError::invalid_state(
            "agent shell command requires a visible agent shell session",
        ));
    }
    let outcome = execute_agent_shell_command(agent_store, pane.id.as_str(), &input)?;
    Ok(agent_shell_command_response_json(
        pane.id.as_str(),
        &input,
        outcome.as_ref(),
    ))
}

/// Runs the agent shell command response json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn agent_shell_command_response_json(
    pane_id: &str,
    input: &str,
    outcome: Option<&AgentShellCommandOutcome>,
) -> String {
    match outcome {
        Some(AgentShellCommandOutcome::Display { command, body }) => format!(
            r#"{{"pane_id":"{}","input":"{}","kind":"display","command":"{}","body":"{}","turn":null}}"#,
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
            r#"{{"pane_id":"{}","input":"{}","kind":"requires_runtime","command":"prompt","body":"live model-loop task execution requires the runtime service","turn":null}}"#,
            json_escape(pane_id),
            json_escape(input)
        ),
    }
}

/// Runs the agent shell visibility json name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn agent_shell_visibility_json_name(visibility: AgentShellVisibility) -> &'static str {
    match visibility {
        AgentShellVisibility::Visible => "visible",
        AgentShellVisibility::Hidden => "hidden",
        AgentShellVisibility::HidePendingTaskCompletion => "hide-pending-task-completion",
    }
}

/// Runs the dispatch agent task list with ledger operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn dispatch_agent_task_list_with_ledger(
    request: &JsonRpcRequest,
    session: &Session,
    turn_ledger: &AgentTurnLedger,
    approval_ids_by_turn: Option<&BTreeMap<String, Vec<String>>>,
) -> Result<String> {
    let filter = AgentTaskListFilter::from_params(session, request.params.as_deref())?;
    let tasks = turn_ledger
        .turns()
        .iter()
        .filter(|turn| {
            filter
                .agent_id
                .as_deref()
                .is_none_or(|agent_id| turn.agent_id == agent_id)
                && filter
                    .pane_id
                    .as_deref()
                    .is_none_or(|pane_id| turn.pane_id == pane_id)
        })
        .map(|turn| {
            agent_task_state_json(
                turn,
                approval_ids_by_turn
                    .and_then(|approval_ids| approval_ids.get(&turn.turn_id))
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    Ok(format!(r#"{{"tasks":[{}]}}"#, tasks.join(",")))
}

/// Runs the validate agent task list params operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn validate_agent_task_list_params(
    session: &Session,
    params: Option<&str>,
) -> Result<()> {
    let _ = AgentTaskListFilter::from_params(session, params)?;
    Ok(())
}

/// Carries Agent Task List Filter state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct AgentTaskListFilter {
    /// Stores the agent id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    agent_id: Option<String>,
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pane_id: Option<String>,
}

impl AgentTaskListFilter {
    /// Runs the from params operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn from_params(session: &Session, params: Option<&str>) -> Result<Self> {
        let Some(params) = params else {
            return Ok(Self::default());
        };
        let value = parse_json_object_value(params, "agent/task/list params")?;
        let object = value
            .as_object()
            .ok_or_else(|| MezError::invalid_args("agent/task/list params must be an object"))?;
        let target_filter = object
            .get("target")
            .map(|target| Self::from_target_value(session, target))
            .transpose()?;
        let inline_filter = Self::from_inline_fields(session, params)?;
        match (target_filter, inline_filter) {
            (Some(target), Some(inline)) if target != inline => Err(MezError::invalid_args(
                "agent/task/list target conflicts with top-level agent filters",
            )),
            (Some(target), _) => Ok(target),
            (None, Some(inline)) => Ok(inline),
            (None, None) => Ok(Self::default()),
        }
    }

    /// Runs the from target value operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn from_target_value(session: &Session, target: &serde_json::Value) -> Result<Self> {
        let object = target
            .as_object()
            .ok_or_else(|| MezError::invalid_args("agent/task/list target must be an object"))?;
        let has_agent_id = object.contains_key("agent_id");
        let has_pane_id = object.contains_key("pane_id");
        let has_session_selector = object.contains_key("session_id")
            || object.contains_key("name")
            || object.contains_key("default");
        if (has_agent_id || has_pane_id) && has_session_selector {
            return Err(MezError::invalid_args(
                "AgentTarget must not be combined with SessionTarget fields",
            ));
        }
        if has_agent_id || has_pane_id {
            return Self::from_agent_target_fields(session, target);
        }
        require_session_target_matches_value(session, target)?;
        Ok(Self::default())
    }

    /// Runs the from inline fields operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn from_inline_fields(session: &Session, params: &str) -> Result<Option<Self>> {
        let value = parse_json_object_value(params, "agent/task/list params")?;
        let object = value
            .as_object()
            .ok_or_else(|| MezError::invalid_args("agent/task/list params must be an object"))?;
        if !object.contains_key("agent_id") && !object.contains_key("pane_id") {
            return Ok(None);
        }
        Ok(Some(Self::from_agent_target_fields(session, &value)?))
    }

    /// Runs the from agent target fields operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn from_agent_target_fields(session: &Session, value: &serde_json::Value) -> Result<Self> {
        let object = value
            .as_object()
            .ok_or_else(|| MezError::invalid_args("AgentTarget must be an object"))?;
        let agent_id =
            match object.get("agent_id") {
                Some(value) => Some(value.as_str().ok_or_else(|| {
                    MezError::invalid_args("AgentTarget agent_id must be a string")
                })?),
                None => None,
            };
        let pane_id =
            match object.get("pane_id") {
                Some(value) => Some(value.as_str().ok_or_else(|| {
                    MezError::invalid_args("AgentTarget pane_id must be a string")
                })?),
                None => None,
            };
        match (agent_id, pane_id) {
            (Some(_), Some(_)) => Err(MezError::invalid_args(
                "AgentTarget must use exactly one of agent_id or pane_id",
            )),
            (Some(agent_id), None) => {
                if let Some(pane_id) = agent_id.strip_prefix("agent-") {
                    pane_by_id(session, pane_id)?;
                }
                Ok(Self {
                    agent_id: Some(agent_id.to_string()),
                    pane_id: None,
                })
            }
            (None, Some(pane_id)) => {
                pane_by_id(session, pane_id)?;
                Ok(Self {
                    agent_id: None,
                    pane_id: Some(pane_id.to_string()),
                })
            }
            (None, None) => Err(MezError::invalid_args(
                "AgentTarget must use exactly one of agent_id or pane_id",
            )),
        }
    }
}

/// Runs the agent state json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn agent_state_json(
    session_id: &str,
    window: &Window,
    pane: &mez_mux::layout::Pane,
    visible: bool,
) -> String {
    agent_state_json_with_model_profile(session_id, window, pane, visible, "default")
}

/// Serializes an idle agent state with an explicit model profile name.
pub(in crate::control) fn agent_state_json_with_model_profile(
    session_id: &str,
    _window: &Window,
    pane: &mez_mux::layout::Pane,
    visible: bool,
    model_profile: &str,
) -> String {
    format!(
        r#"{{"id":"agent-{}","version":1,"session_id":"{}","pane_id":"{}","status":"idle","visible":{},"conversation_id":"agent-{}","model_profile":"{}","cooperation_mode":"user-directed","read_scopes":[],"write_scopes":[],"last_turn_id":null}}"#,
        json_escape(pane.id.as_str()),
        json_escape(session_id),
        json_escape(pane.id.as_str()),
        visible,
        json_escape(pane.id.as_str()),
        json_escape(model_profile)
    )
}

/// Runs the agent state json with shell session operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn agent_state_json_with_shell_session(
    session_id: &str,
    pane: &mez_mux::layout::Pane,
    agent_session: &AgentShellSession,
) -> String {
    agent_state_json_with_shell_session_and_model_profile(
        session_id,
        pane,
        agent_session,
        "default",
    )
}

/// Serializes a live agent shell session with an explicit model profile name.
pub(in crate::control) fn agent_state_json_with_shell_session_and_model_profile(
    session_id: &str,
    pane: &mez_mux::layout::Pane,
    agent_session: &AgentShellSession,
    model_profile: &str,
) -> String {
    let visible = !matches!(agent_session.visibility, AgentShellVisibility::Hidden);
    let status = if agent_session.running_turn_id.is_some() {
        "running"
    } else {
        "idle"
    };
    format!(
        r#"{{"id":"agent-{}","version":1,"session_id":"{}","pane_id":"{}","status":"{}","visible":{},"conversation_id":"{}","model_profile":"{}","cooperation_mode":"user-directed","read_scopes":[],"write_scopes":[],"last_turn_id":{},"transcript_entries":{}}}"#,
        json_escape(pane.id.as_str()),
        json_escape(session_id),
        json_escape(pane.id.as_str()),
        status,
        visible,
        json_escape(&agent_session.session_id),
        json_escape(model_profile),
        json_optional_string(agent_session.running_turn_id.as_deref()),
        agent_session.transcript_entries
    )
}

/// Runs the agent task state json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn agent_task_state_json(
    turn: &mez_agent::AgentTurnRecord,
    approval_ids: &[String],
) -> String {
    let time = unix_seconds_to_rfc3339(turn.started_at_unix_seconds);
    let approval_ids = approval_ids
        .iter()
        .map(|approval_id| format!(r#""{}""#, json_escape(approval_id)))
        .collect::<Vec<_>>()
        .join(",");
    let result_summary = if turn.state == AgentTurnState::Interrupted {
        r#""interrupted by snapshot resume; explicit user confirmation is required before retrying non-idempotent actions""#.to_string()
    } else {
        "null".to_string()
    };
    format!(
        r#"{{"id":"{}","version":1,"agent_id":"{}","state":"{}","created_at":"{}","started_at":"{}","finished_at":{},"prompt_preview":"{}","approval_ids":[{}],"result_summary":{},"pane_id":"{}","policy_profile":"{}","model_profile":"{}"}}"#,
        json_escape(&turn.turn_id),
        json_escape(&turn.agent_id),
        agent_turn_state_name(turn.state, !approval_ids.is_empty()),
        json_escape(&time),
        json_escape(&time),
        if matches!(turn.state, AgentTurnState::Running | AgentTurnState::Queued) {
            "null".to_string()
        } else {
            format!(r#""{}""#, json_escape(&time))
        },
        json_escape(agent_turn_trigger_name(turn.trigger)),
        approval_ids,
        result_summary,
        json_escape(&turn.pane_id),
        json_escape(&turn.policy_profile),
        json_escape(&turn.model_profile)
    )
}

/// Runs the agent turn state name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn agent_turn_state_name(
    state: AgentTurnState,
    waiting_for_approval: bool,
) -> &'static str {
    match state {
        AgentTurnState::Queued => "queued",
        AgentTurnState::Running => "running",
        AgentTurnState::Blocked if waiting_for_approval => "waiting_approval",
        AgentTurnState::Blocked => "waiting",
        AgentTurnState::Completed => "completed",
        AgentTurnState::Failed => "failed",
        AgentTurnState::Interrupted => "interrupted",
    }
}

/// Runs the agent turn trigger name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn agent_turn_trigger_name(
    trigger: mez_agent::AgentTurnTrigger,
) -> &'static str {
    match trigger {
        mez_agent::AgentTurnTrigger::UserPrompt => "user prompt",
        mez_agent::AgentTurnTrigger::LocalMessage => "local message",
        mez_agent::AgentTurnTrigger::ScheduledTask => "scheduled task",
        mez_agent::AgentTurnTrigger::SubagentEvent => "subagent event",
        mez_agent::AgentTurnTrigger::ApprovedContinuation => "approved continuation",
    }
}
