//! Shared payload lookup, normalization, encoding, and typed validation helpers.

use super::{
    GraphicRendition, LayoutPolicy, MessageService, MessageServiceSnapshot, MezError,
    PaneSnapshotPayload, Result, Session, SessionSnapshotPayload, SessionState,
    SnapshotAgentSession, SnapshotApprovalGrantMetadata, SnapshotApprovalRequestMetadata,
    SnapshotConfigDiagnostic, SnapshotConfigLayerMetadata, SnapshotFrameSettings,
    SnapshotFrameState, SnapshotMcpExternalCapability, SnapshotMcpServerState,
    SnapshotMcpToolEffects, SnapshotMcpToolState, SnapshotPaneGeometry, SnapshotSessionState,
    SnapshotShellMetadata, TerminalColor, TerminalModeState, TerminalSavedState, TerminalStyleSpan,
    WindowGroupSnapshotPayload, WindowSnapshotPayload, escape_field, parse_u64,
    tracked_dec_private_mode,
};

/// Runs the payload config layer mut operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn payload_config_layer_mut<'a>(
    payload: &'a mut SessionSnapshotPayload,
    layer_id: &str,
) -> Result<&'a mut SnapshotConfigLayerMetadata> {
    payload
        .active_config_layers
        .iter_mut()
        .find(|layer| layer.id == layer_id)
        .ok_or_else(|| {
            MezError::invalid_args("snapshot config diagnostic references unknown layer")
        })
}

/// Runs the payload window mut operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn payload_window_mut<'a>(
    payload: &'a mut SessionSnapshotPayload,
    window_id: &str,
) -> Result<&'a mut WindowSnapshotPayload> {
    payload
        .windows
        .iter_mut()
        .find(|window| window.window_id == window_id)
        .ok_or_else(|| MezError::invalid_args("snapshot window layout references unknown window"))
}

/// Returns the mutable snapshot window group with the requested stable id.
pub(super) fn payload_window_group_mut<'a>(
    payload: &'a mut SessionSnapshotPayload,
    group_id: &str,
) -> Result<&'a mut WindowGroupSnapshotPayload> {
    payload
        .window_groups
        .iter_mut()
        .find(|group| group.group_id == group_id)
        .ok_or_else(|| MezError::invalid_args("snapshot window group references unknown group"))
}

/// Runs the payload approval grant mut operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn payload_approval_grant_mut<'a>(
    payload: &'a mut SessionSnapshotPayload,
    grant_id: &str,
) -> Result<&'a mut SnapshotApprovalGrantMetadata> {
    payload
        .approval_grants
        .iter_mut()
        .find(|grant| grant.id == grant_id)
        .ok_or_else(|| {
            MezError::invalid_args("snapshot approval grant prefix references unknown grant")
        })
}

/// Runs the payload approval request mut operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn payload_approval_request_mut<'a>(
    payload: &'a mut SessionSnapshotPayload,
    request_id: &str,
) -> Result<&'a mut SnapshotApprovalRequestMetadata> {
    payload
        .approval_requests
        .iter_mut()
        .find(|request| request.id == request_id)
        .ok_or_else(|| {
            MezError::invalid_args("snapshot approval request extension references unknown request")
        })
}

/// Runs the shell metadata from session operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn shell_metadata_from_session(session: &Session) -> SnapshotShellMetadata {
    SnapshotShellMetadata {
        path: session.shell.path().display().to_string(),
        source: session.shell.source_name().to_string(),
        used_fallback: session.shell.used_fallback(),
    }
}

/// Runs the default pane process state operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn default_pane_process_state(live_at_snapshot: bool) -> &'static str {
    if live_at_snapshot {
        "starting"
    } else {
        "exited"
    }
}

/// Runs the parse optional u64 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_optional_u64(value: &str) -> Result<Option<u64>> {
    if value.is_empty() {
        Ok(None)
    } else {
        parse_u64(value).map(Some)
    }
}

/// Runs the optional u32 field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn optional_u32_field(value: Option<u32>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

/// Runs the parse optional u32 field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_optional_u32_field(value: &str) -> Result<Option<u32>> {
    if value.is_empty() {
        Ok(None)
    } else {
        value
            .parse::<u32>()
            .map(Some)
            .map_err(|_| MezError::invalid_args("invalid integer in snapshot pane shell metadata"))
    }
}

/// Runs the optional i32 field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn optional_i32_field(value: Option<i32>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

/// Runs the parse optional i32 field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_optional_i32_field(value: &str) -> Result<Option<i32>> {
    if value.is_empty() {
        Ok(None)
    } else {
        value
            .parse::<i32>()
            .map(Some)
            .map_err(|_| MezError::invalid_args("invalid integer in snapshot pane exit status"))
    }
}

/// Runs the validate non empty collection operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_non_empty_collection(
    values: &[String],
    message: &'static str,
) -> Result<()> {
    if values.iter().any(|value| value.trim().is_empty()) {
        return Err(MezError::invalid_args(message));
    }
    Ok(())
}

/// Runs the validate approval decision name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_approval_decision_name(value: &str) -> Result<()> {
    match value {
        "approve" | "disapprove" | "redirect" => Ok(()),
        _ => Err(MezError::invalid_args(
            "snapshot approval decision must be approve, disapprove, or redirect",
        )),
    }
}

/// Runs the validate message snapshot state operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_message_snapshot_state(state: &MessageServiceSnapshot) -> Result<()> {
    MessageService::from_snapshot_state(state)
        .map(|_| ())
        .map_err(Into::into)
}

/// Runs the validate mcp name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_mcp_name(value: &str, message: &'static str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(MezError::invalid_args(message));
    }
    Ok(())
}

/// Runs the validate mcp json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_mcp_json(value: &str) -> Result<()> {
    serde_json::from_str::<serde_json::Value>(value)
        .map(|_| ())
        .map_err(|_| MezError::invalid_args("snapshot MCP input schema must be valid JSON"))
}

/// Runs the normalized line style spans operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn normalized_line_style_spans(
    source: &[Vec<TerminalStyleSpan>],
    line_count: usize,
) -> Vec<Vec<TerminalStyleSpan>> {
    let mut spans = source.iter().take(line_count).cloned().collect::<Vec<_>>();
    while spans.len() < line_count {
        spans.push(Vec::new());
    }
    spans
}

/// Runs the normalize payload visible line style spans operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn normalize_payload_visible_line_style_spans(payload: &mut SessionSnapshotPayload) {
    for pane in payload
        .windows
        .iter_mut()
        .flat_map(|window| window.panes.iter_mut())
    {
        pane.terminal_history_line_style_spans = normalized_line_style_spans(
            &pane.terminal_history_line_style_spans,
            pane.terminal_history.len(),
        );
        pane.visible_line_style_spans =
            normalized_line_style_spans(&pane.visible_line_style_spans, pane.visible_lines.len());
    }
}

/// Runs the snapshot terminal color name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn snapshot_terminal_color_name(color: Option<TerminalColor>) -> String {
    match color {
        Some(TerminalColor::Indexed(index)) => format!("indexed:{index}"),
        Some(TerminalColor::Rgb(red, green, blue)) => format!("rgb:{red},{green},{blue}"),
        None => String::new(),
    }
}

/// Runs the parse snapshot terminal color operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_snapshot_terminal_color(value: &str) -> Result<Option<TerminalColor>> {
    if value.is_empty() {
        return Ok(None);
    }
    if let Some(index) = value.strip_prefix("indexed:") {
        let index = parse_u8_component(index, "snapshot terminal indexed color is invalid")?;
        return Ok(Some(TerminalColor::Indexed(index)));
    }
    if let Some(rgb) = value.strip_prefix("rgb:") {
        let parts = rgb.split(',').collect::<Vec<_>>();
        if parts.len() != 3 {
            return Err(MezError::invalid_args(
                "snapshot terminal RGB color is invalid",
            ));
        }
        return Ok(Some(TerminalColor::Rgb(
            parse_u8_component(parts[0], "snapshot terminal RGB color is invalid")?,
            parse_u8_component(parts[1], "snapshot terminal RGB color is invalid")?,
            parse_u8_component(parts[2], "snapshot terminal RGB color is invalid")?,
        )));
    }
    Err(MezError::invalid_args(
        "snapshot terminal color must be empty, indexed, or rgb",
    ))
}

/// Runs the parse u8 component operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_u8_component(value: &str, message: &'static str) -> Result<u8> {
    let parsed = value
        .parse::<u16>()
        .map_err(|_| MezError::invalid_args(message))?;
    u8::try_from(parsed).map_err(|_| MezError::invalid_args(message))
}

/// Runs the payload frame settings mut operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn payload_frame_settings_mut<'a>(
    payload: &'a mut SessionSnapshotPayload,
    target: &str,
) -> Result<&'a mut SnapshotFrameSettings> {
    match target {
        "window" => Ok(&mut payload.frame_state.window),
        "pane" => Ok(&mut payload.frame_state.pane),
        _ => Err(MezError::invalid_args(
            "snapshot frame payload references unknown target",
        )),
    }
}

/// Runs the payload pane mut operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn payload_pane_mut<'a>(
    payload: &'a mut SessionSnapshotPayload,
    pane_id: &str,
) -> Result<&'a mut PaneSnapshotPayload> {
    payload
        .windows
        .iter_mut()
        .flat_map(|window| window.panes.iter_mut())
        .find(|pane| pane.pane_id == pane_id)
        .ok_or_else(|| MezError::invalid_args("snapshot pane extension references unknown pane"))
}

/// Runs the encode frame settings operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn encode_frame_settings(
    target: &str,
    settings: &SnapshotFrameSettings,
    output: &mut String,
) {
    output.push_str(&format!(
        "frame\t{}\t{}\t{}\t{}\t{}\n",
        target,
        settings.enabled,
        escape_field(&settings.position),
        escape_field(&settings.style),
        escape_field(&settings.template)
    ));
    for field in &settings.visible_fields {
        output.push_str(&format!(
            "frame_visible\t{}\t{}\n",
            target,
            escape_field(field)
        ));
    }
}

/// Runs the encode terminal modes operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn encode_terminal_modes(pane_id: &str, modes: &TerminalModeState, output: &mut String) {
    output.push_str(&format!(
        "pane_terminal_modes\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
        escape_field(pane_id),
        modes.cursor_visible,
        modes.bracketed_paste_enabled,
        modes.normal_mouse_tracking_enabled,
        modes.button_event_mouse_tracking_enabled,
        modes.any_event_mouse_tracking_enabled,
        modes.sgr_mouse_enabled,
        modes.application_cursor_enabled,
        modes.autowrap_enabled,
        modes.application_keypad_enabled,
        modes.focus_events_enabled,
        escape_field(modes.title.as_deref().unwrap_or(""))
    ));
}

/// Runs the encode terminal saved state operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn encode_terminal_saved_state(
    pane_id: &str,
    saved_state: &TerminalSavedState,
    output: &mut String,
) {
    if let Some(cursor) = saved_state.saved_cursor {
        output.push_str(&format!(
            "pane_terminal_saved_cursor\t{}\t{}\t{}\n",
            escape_field(pane_id),
            cursor.row,
            cursor.column
        ));
    }
    for saved_mode in &saved_state.saved_dec_private_modes {
        output.push_str(&format!(
            "pane_terminal_saved_dec_private_mode\t{}\t{}\t{}\n",
            escape_field(pane_id),
            saved_mode.mode,
            saved_mode.enabled
        ));
    }
}

impl SnapshotShellMetadata {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn validate(&self) -> Result<()> {
        if self.path.trim().is_empty() {
            return Err(MezError::invalid_args(
                "snapshot shell path must not be empty",
            ));
        }
        match self.source.as_str() {
            "shell-env" | "fallback-bin-sh" => {}
            _ => {
                return Err(MezError::invalid_args(
                    "snapshot shell source must be shell-env or fallback-bin-sh",
                ));
            }
        }
        if self.used_fallback != (self.source == "fallback-bin-sh") {
            return Err(MezError::invalid_args(
                "snapshot shell fallback flag must match shell source",
            ));
        }
        Ok(())
    }
}

impl SnapshotAgentSession {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn validate(&self) -> Result<()> {
        if self.pane_id.trim().is_empty() || self.conversation_id.trim().is_empty() {
            return Err(MezError::invalid_args(
                "snapshot agent session identity fields must not be empty",
            ));
        }
        match self.visibility.as_str() {
            "hidden" | "visible" | "hide-pending-task-completion" => {}
            _ => {
                return Err(MezError::invalid_args(
                    "snapshot agent session visibility is invalid",
                ));
            }
        }
        if self
            .running_turn_id
            .as_ref()
            .is_some_and(|turn_id| turn_id.trim().is_empty())
        {
            return Err(MezError::invalid_args(
                "snapshot agent session running turn id must not be empty",
            ));
        }
        if self.visibility == "hide-pending-task-completion" && self.running_turn_id.is_none() {
            return Err(MezError::invalid_args(
                "snapshot pending-hide agent sessions must include a running turn",
            ));
        }
        Ok(())
    }
}

impl SnapshotApprovalGrantMetadata {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty() {
            return Err(MezError::invalid_args(
                "snapshot approval grant id must not be empty",
            ));
        }
        if self.command_prefix.is_empty() {
            return Err(MezError::invalid_args(
                "snapshot approval grant command prefix must not be empty",
            ));
        }
        validate_non_empty_collection(
            &self.command_prefix,
            "snapshot approval grant command prefix tokens must not be empty",
        )?;
        match self.scope.as_str() {
            "session" | "global" => {}
            _ => {
                return Err(MezError::invalid_args(
                    "snapshot approval grant scope must be session or global",
                ));
            }
        }
        validate_approval_decision_name(&self.decision)
    }
}

impl SnapshotApprovalRequestMetadata {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty()
            || self.requesting_agent_id.trim().is_empty()
            || self.pane_id.trim().is_empty()
            || self.action_kind.trim().is_empty()
            || self.action_summary.trim().is_empty()
        {
            return Err(MezError::invalid_args(
                "snapshot approval request identity fields must not be empty",
            ));
        }
        validate_non_empty_collection(
            &self.parent_agent_chain,
            "snapshot approval request parent chain entries must not be empty",
        )?;
        validate_non_empty_collection(
            &self.declared_effects,
            "snapshot approval request declared effects must not be empty",
        )?;
        validate_non_empty_collection(
            &self.matched_rules,
            "snapshot approval request matched rules must not be empty",
        )?;
        validate_non_empty_collection(
            &self.read_scopes,
            "snapshot approval request read scopes must not be empty",
        )?;
        validate_non_empty_collection(
            &self.write_scopes,
            "snapshot approval request write scopes must not be empty",
        )?;
        let expected_decision = match self.state.as_str() {
            "approved" => "approve",
            "disapproved" => "disapprove",
            "redirected" => "redirect",
            _ => {
                return Err(MezError::invalid_args(
                    "snapshot approval request state must be approved, disapproved, or redirected",
                ));
            }
        };
        let Some(decision) = self.decision.as_deref() else {
            return Err(MezError::invalid_args(
                "snapshot decided approval request must include a decision",
            ));
        };
        validate_approval_decision_name(decision)?;
        if decision != expected_decision {
            return Err(MezError::invalid_args(
                "snapshot approval request decision must match its state",
            ));
        }
        if self
            .decided_by_client_id
            .as_ref()
            .is_some_and(|client_id| client_id.trim().is_empty())
        {
            return Err(MezError::invalid_args(
                "snapshot approval request deciding client id must not be empty",
            ));
        }
        match self.redirect_instruction.as_deref() {
            Some(instruction) if instruction.trim().is_empty() => Err(MezError::invalid_args(
                "snapshot approval request redirect instruction must not be empty",
            )),
            Some(_) if decision != "redirect" => Err(MezError::invalid_args(
                "snapshot approval request redirect instruction requires redirect decision",
            )),
            None if decision == "redirect" => Err(MezError::invalid_args(
                "snapshot redirected approval request must include redirect instruction",
            )),
            _ => Ok(()),
        }
    }
}

impl SnapshotMcpServerState {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn validate(&self) -> Result<()> {
        validate_mcp_name(&self.id, "snapshot MCP server id must not be empty")?;
        validate_mcp_name(&self.name, "snapshot MCP server name must not be empty")?;
        match self.kind.as_str() {
            "stdio" | "streamable_http" => {}
            _ => {
                return Err(MezError::invalid_args(
                    "snapshot MCP server kind must be stdio or streamable_http",
                ));
            }
        }
        match self.status.as_str() {
            "configured" | "starting" | "available" | "unavailable" | "blacklisted" | "failed" => {}
            _ => {
                return Err(MezError::invalid_args(
                    "snapshot MCP server status is invalid",
                ));
            }
        }
        if self
            .blacklist_reason
            .as_ref()
            .is_some_and(|reason| reason.trim().is_empty())
        {
            return Err(MezError::invalid_args(
                "snapshot MCP blacklist reason must not be empty",
            ));
        }
        self.external_capability.validate()?;
        for tool in &self.tools {
            tool.validate(&self.id)?;
        }
        Ok(())
    }
}

impl SnapshotMcpExternalCapability {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn validate(&self) -> Result<()> {
        let requires_purpose = self.mutates_filesystem_outside_shell
            || self.executes_processes_outside_shell
            || self.accesses_credentials_outside_shell;
        if requires_purpose && self.purpose.trim().is_empty() {
            return Err(MezError::invalid_args(
                "snapshot MCP external capability purpose must not be empty",
            ));
        }
        Ok(())
    }
}

impl SnapshotMcpToolState {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn validate(&self, server_id: &str) -> Result<()> {
        validate_mcp_name(
            &self.server_id,
            "snapshot MCP tool server id must not be empty",
        )?;
        validate_mcp_name(&self.name, "snapshot MCP tool name must not be empty")?;
        if self.server_id != server_id {
            return Err(MezError::invalid_args(
                "snapshot MCP tool server id must match containing server",
            ));
        }
        match self.approval.as_str() {
            "inherit" | "prompt" | "allow" | "deny" => {}
            _ => {
                return Err(MezError::invalid_args(
                    "snapshot MCP tool approval is invalid",
                ));
            }
        }
        self.effects.validate()?;
        validate_mcp_json(&self.input_schema_json)
    }
}

impl SnapshotMcpToolEffects {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn validate(&self) -> Result<()> {
        let _ = self;
        Ok(())
    }
}

impl SnapshotFrameState {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn validate(&self) -> Result<()> {
        self.window.validate("window")?;
        self.pane.validate("pane")
    }
}

impl SnapshotFrameSettings {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn validate(&self, target: &str) -> Result<()> {
        match self.position.as_str() {
            "top" | "bottom" | "border" => {}
            _ => {
                return Err(MezError::invalid_args(format!(
                    "snapshot {target} frame position must be top, bottom, or border"
                )));
            }
        }
        match self.style.as_str() {
            "default" | "bold" | "underline" | "inverse" | "reverse" => {}
            _ => {
                return Err(MezError::invalid_args(format!(
                    "snapshot {target} frame style must be default, bold, underline, inverse, or reverse"
                )));
            }
        }
        if self.template.trim().is_empty() {
            return Err(MezError::invalid_args(format!(
                "snapshot {target} frame template must not be empty"
            )));
        }
        if self.visible_fields.is_empty()
            || self
                .visible_fields
                .iter()
                .any(|field| field.trim().is_empty())
        {
            return Err(MezError::invalid_args(format!(
                "snapshot {target} frame visible fields must not be empty"
            )));
        }
        Ok(())
    }
}

impl WindowGroupSnapshotPayload {
    /// Validates saved window-group topology before session reconstruction.
    pub(super) fn validate(&self) -> Result<()> {
        if self.group_id.is_empty() || self.name.is_empty() {
            return Err(MezError::invalid_args(
                "snapshot window group identity fields must not be empty",
            ));
        }
        if self.window_ids.is_empty() {
            return Err(MezError::invalid_args(
                "snapshot window group must contain at least one window",
            ));
        }
        if self.window_ids.iter().any(|window_id| window_id.is_empty()) {
            return Err(MezError::invalid_args(
                "snapshot window group window ids must not be empty",
            ));
        }
        Ok(())
    }
}

/// Validates that saved groups reference saved windows and identify one active group.
pub(super) fn validate_snapshot_window_groups(payload: &SessionSnapshotPayload) -> Result<()> {
    if payload.window_groups.is_empty() {
        return Ok(());
    }
    let active_count = payload
        .window_groups
        .iter()
        .filter(|group| group.active)
        .count();
    if active_count != 1 {
        return Err(MezError::invalid_args(
            "snapshot payload must contain exactly one active window group",
        ));
    }
    for group in &payload.window_groups {
        for window_id in &group.window_ids {
            if !payload
                .windows
                .iter()
                .any(|window| window.window_id == *window_id)
            {
                return Err(MezError::invalid_args(
                    "snapshot window group references an unknown window",
                ));
            }
        }
        for active_window_id in [&group.active_window_id, &group.last_active_window_id]
            .into_iter()
            .flatten()
        {
            if !group
                .window_ids
                .iter()
                .any(|window_id| window_id == active_window_id)
            {
                return Err(MezError::invalid_args(
                    "snapshot window group active window references an unknown group window",
                ));
            }
        }
    }
    Ok(())
}

impl WindowSnapshotPayload {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn validate(&self) -> Result<()> {
        if self.window_id.is_empty() || self.name.is_empty() {
            return Err(MezError::invalid_args(
                "snapshot window identity fields must not be empty",
            ));
        }
        if self.columns == 0 || self.rows == 0 {
            return Err(MezError::invalid_args(
                "snapshot window size must be non-zero",
            ));
        }
        if LayoutPolicy::from_name(&self.layout_policy).is_none() {
            return Err(MezError::invalid_args(
                "snapshot window layout policy is invalid",
            ));
        }
        if self.panes.is_empty() {
            return Err(MezError::invalid_args(
                "snapshot window must contain at least one pane",
            ));
        }
        for pane in &self.panes {
            pane.validate()?;
        }
        if let Some(layout_root) = &self.layout_root {
            let layout_root = layout_root.to_layout_node(&self.panes)?;
            layout_root.validate_pane_indices(self.panes.len())?;
        }
        validate_window_pane_geometries(self)?;
        Ok(())
    }
}

impl SnapshotConfigLayerMetadata {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn validate(&self) -> Result<()> {
        if self.id.is_empty() || self.layer_type.is_empty() {
            return Err(MezError::invalid_args(
                "snapshot config layer identity fields must not be empty",
            ));
        }
        if self.schema_version == 0 {
            return Err(MezError::invalid_args(
                "snapshot config layer schema version must be non-zero",
            ));
        }
        for diagnostic in &self.diagnostics {
            diagnostic.validate()?;
        }
        Ok(())
    }
}

impl SnapshotConfigDiagnostic {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn validate(&self) -> Result<()> {
        if self.path.is_empty() || self.message.is_empty() {
            return Err(MezError::invalid_args(
                "snapshot config diagnostics must not be empty",
            ));
        }
        Ok(())
    }
}

impl PaneSnapshotPayload {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn validate(&self) -> Result<()> {
        if self.pane_id.is_empty() || self.title.is_empty() {
            return Err(MezError::invalid_args(
                "snapshot pane identity fields must not be empty",
            ));
        }
        if self.columns == 0 || self.rows == 0 {
            return Err(MezError::invalid_args(
                "snapshot pane size must be non-zero",
            ));
        }
        validate_snapshot_pane_process_state(&self.process_state)?;
        validate_snapshot_pane_readiness_state(&self.readiness_state)?;
        if self
            .current_working_directory
            .as_deref()
            .is_some_and(|directory| directory.trim().is_empty())
        {
            return Err(MezError::invalid_args(
                "snapshot pane current working directory must not be empty",
            ));
        }
        if let Some(geometry) = &self.geometry {
            geometry.validate(self.columns, self.rows)?;
        }
        validate_terminal_saved_state(
            &self.terminal_saved_state,
            usize::from(self.rows),
            usize::from(self.columns),
        )?;
        if self.terminal_history_line_style_spans.len() > self.terminal_history.len() {
            return Err(MezError::invalid_args(
                "snapshot pane history style spans must align to history lines",
            ));
        }
        for spans in &self.terminal_history_line_style_spans {
            validate_terminal_style_spans(spans, usize::from(self.columns))?;
        }
        if self.visible_line_style_spans.len() > self.visible_lines.len() {
            return Err(MezError::invalid_args(
                "snapshot pane visible style spans must align to visible lines",
            ));
        }
        for spans in &self.visible_line_style_spans {
            validate_terminal_style_spans(spans, usize::from(self.columns))?;
        }
        if self
            .transcript_refs
            .iter()
            .any(|transcript_ref| transcript_ref.trim().is_empty())
        {
            return Err(MezError::invalid_args(
                "snapshot pane transcript references must not be empty",
            ));
        }
        Ok(())
    }
}

/// Runs the validate snapshot pane process state operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_snapshot_pane_process_state(process_state: &str) -> Result<()> {
    match process_state {
        "starting" | "running" | "exited" | "closing" | "failed" => Ok(()),
        _ => Err(MezError::invalid_args(
            "snapshot pane process state is invalid",
        )),
    }
}

/// Runs the validate snapshot pane readiness state operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_snapshot_pane_readiness_state(readiness_state: &str) -> Result<()> {
    match readiness_state {
        "unknown"
        | "prompt-candidate"
        | "probing"
        | "ready"
        | "busy"
        | "degraded"
        | "interactive-blocked" => Ok(()),
        _ => Err(MezError::invalid_args(
            "snapshot pane readiness state is invalid",
        )),
    }
}

impl SnapshotPaneGeometry {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn validate(&self, pane_columns: u16, pane_rows: u16) -> Result<()> {
        if self.columns == 0 || self.rows == 0 {
            return Err(MezError::invalid_args(
                "snapshot pane geometry size must be non-zero",
            ));
        }
        if self.columns != pane_columns || self.rows != pane_rows {
            return Err(MezError::invalid_args(
                "snapshot pane geometry size must match pane size",
            ));
        }
        Ok(())
    }
}

/// Runs the validate window pane geometries operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_window_pane_geometries(window: &WindowSnapshotPayload) -> Result<()> {
    let mut geometries = Vec::new();
    for pane in &window.panes {
        let Some(geometry) = pane.geometry.as_ref() else {
            return Ok(());
        };
        if geometry.column.saturating_add(geometry.columns) > window.columns
            || geometry.row.saturating_add(geometry.rows) > window.rows
        {
            return Err(MezError::invalid_args(
                "snapshot pane geometry must fit inside the window",
            ));
        }
        geometries.push(geometry);
    }

    for (left_index, left) in geometries.iter().enumerate() {
        for right in geometries.iter().skip(left_index.saturating_add(1)) {
            if snapshot_pane_geometries_overlap(left, right) {
                return Err(MezError::invalid_args(
                    "snapshot pane geometries must not overlap",
                ));
            }
        }
    }
    Ok(())
}

/// Runs the snapshot pane geometries overlap operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn snapshot_pane_geometries_overlap(
    left: &SnapshotPaneGeometry,
    right: &SnapshotPaneGeometry,
) -> bool {
    let left_right = left.column.saturating_add(left.columns);
    let left_bottom = left.row.saturating_add(left.rows);
    let right_right = right.column.saturating_add(right.columns);
    let right_bottom = right.row.saturating_add(right.rows);
    left.column < right_right
        && right.column < left_right
        && left.row < right_bottom
        && right.row < left_bottom
}

/// Runs the validate terminal style spans operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_terminal_style_spans(
    spans: &[TerminalStyleSpan],
    columns: usize,
) -> Result<()> {
    let mut previous_end = 0usize;
    for span in spans {
        if span.length == 0 {
            return Err(MezError::invalid_args(
                "snapshot terminal style span length must be non-zero",
            ));
        }
        if span.rendition == GraphicRendition::default() {
            return Err(MezError::invalid_args(
                "snapshot terminal style span rendition must be non-default",
            ));
        }
        let end = span
            .start
            .checked_add(span.length)
            .ok_or_else(|| MezError::invalid_args("snapshot terminal style span is too large"))?;
        if span.start < previous_end {
            return Err(MezError::invalid_args(
                "snapshot terminal style spans must be sorted and non-overlapping",
            ));
        }
        if end > columns {
            return Err(MezError::invalid_args(
                "snapshot terminal style span exceeds pane width",
            ));
        }
        previous_end = end;
    }
    Ok(())
}

/// Runs the validate terminal saved state operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_terminal_saved_state(
    saved_state: &TerminalSavedState,
    rows: usize,
    columns: usize,
) -> Result<()> {
    if let Some(cursor) = saved_state.saved_cursor
        && (cursor.row >= rows || cursor.column >= columns)
    {
        return Err(MezError::invalid_args(
            "snapshot pane saved cursor must fit pane size",
        ));
    }
    let mut seen_modes = Vec::new();
    for saved_mode in &saved_state.saved_dec_private_modes {
        if !tracked_dec_private_mode(saved_mode.mode) {
            return Err(MezError::invalid_args(
                "snapshot pane saved DEC private mode is not tracked",
            ));
        }
        if seen_modes.contains(&saved_mode.mode) {
            return Err(MezError::invalid_args(
                "snapshot pane saved DEC private modes must not repeat",
            ));
        }
        seen_modes.push(saved_mode.mode);
    }
    Ok(())
}

impl SnapshotSessionState {
    /// Runs the from session state operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn from_session_state(state: SessionState) -> Self {
        match state {
            SessionState::Running => Self::Running,
            SessionState::Detached => Self::Detached,
            SessionState::Empty => Self::Empty,
            SessionState::Stopping => Self::Stopping,
            SessionState::Failed => Self::Failed,
        }
    }

    /// Runs the as str operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Detached => "detached",
            Self::Empty => "empty",
            Self::Stopping => "stopping",
            Self::Failed => "failed",
        }
    }

    /// Runs the parse operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn parse(value: &str) -> Result<Self> {
        match value {
            "running" => Ok(Self::Running),
            "detached" => Ok(Self::Detached),
            "empty" => Ok(Self::Empty),
            "stopping" => Ok(Self::Stopping),
            "failed" => Ok(Self::Failed),
            _ => Err(MezError::invalid_args("unknown snapshot session state")),
        }
    }
}
