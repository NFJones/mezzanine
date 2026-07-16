//! Runtime target lookup, action naming, subagent parsing, and copy-position mapping.

use super::*;

/// Runs the runtime pane by id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_pane_by_id<'a>(
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
pub(in crate::runtime) fn runtime_mutating_method(method: &str) -> bool {
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
pub(in crate::runtime) fn agent_state_control_method(method: &str) -> bool {
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
pub(in crate::runtime) fn runtime_split_direction(value: &str) -> Result<SplitDirection> {
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
pub(in crate::runtime) fn runtime_subagent_spawn_request(
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
pub(in crate::runtime) fn runtime_subagent_placement_mode(
    params: &str,
) -> Result<RuntimeSubagentPlacement> {
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
pub(in crate::runtime) fn runtime_subagent_placement_from_fields(
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
    fn name(&self) -> &'static str {
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
pub(in crate::runtime) fn runtime_subagent_role_name(value: &str) -> Result<String> {
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
pub(in crate::runtime) fn runtime_cooperation_mode(value: &str) -> Result<CooperationMode> {
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
pub(in crate::runtime) fn runtime_cooperation_mode_name(mode: CooperationMode) -> &'static str {
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
pub(in crate::runtime) fn runtime_value_string_array(
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
pub(in crate::runtime) fn pane_navigation_direction(
    direction: PaneFocusDirection,
) -> PaneNavigationDirection {
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
pub(in crate::runtime) fn mux_action_name(action: MuxAction) -> &'static str {
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
pub(in crate::runtime) fn mux_action_command_prompt_prefill(
    action: MuxAction,
) -> Option<&'static str> {
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
pub(in crate::runtime) fn mouse_action_name(action: MouseAction) -> &'static str {
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
pub(in crate::runtime) fn runtime_copy_position_for_view(
    copy_mode: &CopyMode,
    position: CopyPosition,
) -> CopyPosition {
    copy_mode.clamp_position(CopyPosition {
        line: copy_mode.scroll_top().saturating_add(position.line),
        column: position.column,
    })
}
