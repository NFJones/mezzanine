//! Durable line-oriented snapshot payload decoding.

use super::helpers::*;
use super::*;

impl SessionSnapshotPayload {
    /// Runs the decode operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::snapshot) fn decode(data: &str) -> Result<Self> {
        let mut lines = data.lines();
        let first_line = lines
            .next()
            .ok_or_else(|| MezError::invalid_args("snapshot payload is empty"))?;
        let first_fields = split_fields(first_line)?;
        let session_fields = if first_fields.first().map(String::as_str) == Some("payload_version")
        {
            if first_fields.len() != 2 {
                return Err(MezError::invalid_args(
                    "invalid snapshot payload version field count",
                ));
            }
            let version = parse_u32(&first_fields[1])?;
            if !(MIN_SUPPORTED_SNAPSHOT_PAYLOAD_FORMAT_VERSION..=SNAPSHOT_PAYLOAD_FORMAT_VERSION)
                .contains(&version)
            {
                return Err(MezError::invalid_args(
                    "unsupported snapshot payload format version",
                ));
            }
            let session_line = lines
                .next()
                .ok_or_else(|| MezError::invalid_args("snapshot payload session header missing"))?;
            split_fields(session_line)?
        } else {
            first_fields
        };
        if session_fields.len() != 7 || session_fields[0] != "session" {
            return Err(MezError::invalid_args(
                "invalid snapshot session payload header",
            ));
        }

        let mut payload = Self {
            session_id: session_fields[1].clone(),
            name: session_fields[2].clone(),
            state: SnapshotSessionState::parse(&session_fields[3])?,
            authoritative_columns: parse_u16(&session_fields[4])?,
            authoritative_rows: parse_u16(&session_fields[5])?,
            active_window_id: non_empty_string(&session_fields[6]),
            shell: SnapshotShellMetadata::default(),
            active_config_layers: Vec::new(),
            frame_state: SnapshotFrameState::default(),
            agent_sessions: Vec::new(),
            approval_grants: Vec::new(),
            approval_requests: Vec::new(),
            message_state: None,
            mcp_servers: Vec::new(),
            window_groups: Vec::new(),
            windows: Vec::new(),
        };

        for line in lines {
            let fields = split_fields(line)?;
            match fields.first().map(String::as_str) {
                Some("window_group") => {
                    if fields.len() != 7 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot window group field count",
                        ));
                    }
                    payload.window_groups.push(WindowGroupSnapshotPayload {
                        group_id: fields[1].clone(),
                        index: parse_usize(&fields[2])?,
                        name: fields[3].clone(),
                        window_ids: Vec::new(),
                        active: parse_bool(&fields[4])?,
                        active_window_id: non_empty_string(&fields[5]),
                        last_active_window_id: non_empty_string(&fields[6]),
                    });
                }
                Some("window_group_window") => {
                    if fields.len() != 3 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot window group window field count",
                        ));
                    }
                    let group = payload_window_group_mut(&mut payload, &fields[1])?;
                    group.window_ids.push(fields[2].clone());
                }
                Some("window") => {
                    if fields.len() != 8 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot window payload field count",
                        ));
                    }
                    payload.windows.push(WindowSnapshotPayload {
                        window_id: fields[1].clone(),
                        index: parse_usize(&fields[2])?,
                        name: fields[3].clone(),
                        active: parse_bool(&fields[4])?,
                        columns: parse_u16(&fields[5])?,
                        rows: parse_u16(&fields[6])?,
                        layout_policy: fields[7].clone(),
                        layout_root: None,
                        panes: Vec::new(),
                    });
                }
                Some("window_layout") => {
                    if fields.len() != 3 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot window layout field count",
                        ));
                    }
                    let window = payload_window_mut(&mut payload, &fields[1])?;
                    if window.layout_root.is_some() {
                        return Err(MezError::invalid_args(
                            "snapshot window layout appeared more than once",
                        ));
                    }
                    window.layout_root = Some(serde_json::from_str(&fields[2]).map_err(|_| {
                        MezError::invalid_args("invalid snapshot window layout JSON")
                    })?);
                }
                Some("shell") => {
                    if fields.len() != 4 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot shell payload field count",
                        ));
                    }
                    payload.shell = SnapshotShellMetadata {
                        path: fields[1].clone(),
                        source: fields[2].clone(),
                        used_fallback: parse_bool(&fields[3])?,
                    };
                }
                Some("config_layer") => {
                    if fields.len() != 8 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot config layer field count",
                        ));
                    }
                    payload
                        .active_config_layers
                        .push(SnapshotConfigLayerMetadata {
                            id: fields[1].clone(),
                            layer_type: fields[2].clone(),
                            precedence: parse_usize(&fields[3])?,
                            path: non_empty_string(&fields[4]),
                            trusted: parse_bool(&fields[5])?,
                            applied: parse_bool(&fields[6])?,
                            schema_version: parse_u32(&fields[7])?,
                            diagnostics: Vec::new(),
                        });
                }
                Some("config_diagnostic") => {
                    if fields.len() != 4 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot config diagnostic field count",
                        ));
                    }
                    payload_config_layer_mut(&mut payload, &fields[1])?
                        .diagnostics
                        .push(SnapshotConfigDiagnostic {
                            path: fields[2].clone(),
                            message: fields[3].clone(),
                        });
                }
                Some("frame") => {
                    if fields.len() != 6 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot frame payload field count",
                        ));
                    }
                    *payload_frame_settings_mut(&mut payload, &fields[1])? =
                        SnapshotFrameSettings {
                            enabled: parse_bool(&fields[2])?,
                            position: fields[3].clone(),
                            style: fields[4].clone(),
                            template: fields[5].clone(),
                            visible_fields: Vec::new(),
                        };
                }
                Some("frame_visible") => {
                    if fields.len() != 3 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot frame visible-field count",
                        ));
                    }
                    payload_frame_settings_mut(&mut payload, &fields[1])?
                        .visible_fields
                        .push(fields[2].clone());
                }
                Some("agent_session") => {
                    if fields.len() != 6 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot agent session field count",
                        ));
                    }
                    payload.agent_sessions.push(SnapshotAgentSession {
                        pane_id: fields[1].clone(),
                        conversation_id: fields[2].clone(),
                        visibility: fields[3].clone(),
                        running_turn_id: non_empty_string(&fields[4]),
                        transcript_entries: parse_u64(&fields[5])?,
                    });
                }
                Some("approval_grant") => {
                    if fields.len() != 4 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot approval grant field count",
                        ));
                    }
                    payload.approval_grants.push(SnapshotApprovalGrantMetadata {
                        id: fields[1].clone(),
                        command_prefix: Vec::new(),
                        scope: fields[2].clone(),
                        decision: fields[3].clone(),
                    });
                }
                Some("approval_grant_prefix") => {
                    if fields.len() != 3 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot approval grant prefix field count",
                        ));
                    }
                    payload_approval_grant_mut(&mut payload, &fields[1])?
                        .command_prefix
                        .push(fields[2].clone());
                }
                Some("approval_request") => {
                    if fields.len() != 12 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot approval request field count",
                        ));
                    }
                    payload
                        .approval_requests
                        .push(SnapshotApprovalRequestMetadata {
                            id: fields[1].clone(),
                            requesting_agent_id: fields[2].clone(),
                            pane_id: fields[3].clone(),
                            parent_agent_chain: Vec::new(),
                            action_kind: fields[4].clone(),
                            action_summary: fields[5].clone(),
                            declared_effects: Vec::new(),
                            matched_rules: Vec::new(),
                            read_scopes: Vec::new(),
                            write_scopes: Vec::new(),
                            created_at_unix_seconds: parse_optional_u64(&fields[8])?,
                            decided_at_unix_seconds: parse_optional_u64(&fields[9])?,
                            decided_by_client_id: non_empty_string(&fields[10]),
                            state: fields[6].clone(),
                            decision: non_empty_string(&fields[7]),
                            redirect_instruction: non_empty_string(&fields[11]),
                        });
                }
                Some("approval_request_parent") => {
                    if fields.len() != 3 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot approval request parent field count",
                        ));
                    }
                    payload_approval_request_mut(&mut payload, &fields[1])?
                        .parent_agent_chain
                        .push(fields[2].clone());
                }
                Some("approval_request_effect") => {
                    if fields.len() != 3 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot approval request effect field count",
                        ));
                    }
                    payload_approval_request_mut(&mut payload, &fields[1])?
                        .declared_effects
                        .push(fields[2].clone());
                }
                Some("approval_request_rule") => {
                    if fields.len() != 3 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot approval request rule field count",
                        ));
                    }
                    payload_approval_request_mut(&mut payload, &fields[1])?
                        .matched_rules
                        .push(fields[2].clone());
                }
                Some("approval_request_read_scope") => {
                    if fields.len() != 3 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot approval request read-scope field count",
                        ));
                    }
                    payload_approval_request_mut(&mut payload, &fields[1])?
                        .read_scopes
                        .push(fields[2].clone());
                }
                Some("approval_request_write_scope") => {
                    if fields.len() != 3 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot approval request write-scope field count",
                        ));
                    }
                    payload_approval_request_mut(&mut payload, &fields[1])?
                        .write_scopes
                        .push(fields[2].clone());
                }
                Some("message_state") => {
                    if fields.len() != 2 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot message state field count",
                        ));
                    }
                    if payload.message_state.is_some() {
                        return Err(MezError::invalid_args(
                            "snapshot message state appeared more than once",
                        ));
                    }
                    payload.message_state =
                        Some(serde_json::from_str(&fields[1]).map_err(|_| {
                            MezError::invalid_args("invalid snapshot message state JSON")
                        })?);
                }
                Some("mcp_state") => {
                    if fields.len() != 2 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot MCP state field count",
                        ));
                    }
                    if !payload.mcp_servers.is_empty() {
                        return Err(MezError::invalid_args(
                            "snapshot MCP state appeared more than once",
                        ));
                    }
                    payload.mcp_servers = serde_json::from_str(&fields[1])
                        .map_err(|_| MezError::invalid_args("invalid snapshot MCP state JSON"))?;
                }
                Some("pane") => {
                    if fields.len() != 8 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot pane payload field count",
                        ));
                    }
                    let Some(window) = payload.windows.last_mut() else {
                        return Err(MezError::invalid_args(
                            "snapshot pane payload appeared before any window",
                        ));
                    };
                    let live_at_snapshot = parse_bool(&fields[5])?;
                    window.panes.push(PaneSnapshotPayload {
                        pane_id: fields[1].clone(),
                        index: parse_usize(&fields[2])?,
                        title: fields[3].clone(),
                        active: parse_bool(&fields[4])?,
                        live_at_snapshot,
                        columns: parse_u16(&fields[6])?,
                        rows: parse_u16(&fields[7])?,
                        primary_pid: None,
                        process_state: default_pane_process_state(live_at_snapshot).to_string(),
                        current_working_directory: None,
                        readiness_state: "unknown".to_string(),
                        exit_status: None,
                        geometry: None,
                        terminal_modes: TerminalModeState::default(),
                        terminal_saved_state: TerminalSavedState::default(),
                        terminal_history: Vec::new(),
                        terminal_history_line_style_spans: Vec::new(),
                        visible_lines: Vec::new(),
                        visible_line_style_spans: Vec::new(),
                        alternate_screen_active: false,
                        transcript_refs: Vec::new(),
                    });
                }
                Some("pane_shell") => {
                    if fields.len() != 6 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot pane shell metadata field count",
                        ));
                    }
                    let pane = payload_pane_mut(&mut payload, &fields[1])?;
                    pane.primary_pid = parse_optional_u32_field(&fields[2])?;
                    pane.process_state = fields[3].clone();
                    pane.current_working_directory = non_empty_string(&fields[4]);
                    pane.readiness_state = fields[5].clone();
                }
                Some("pane_alternate_screen") => {
                    if fields.len() != 3 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot pane alternate-screen field count",
                        ));
                    }
                    payload_pane_mut(&mut payload, &fields[1])?.alternate_screen_active =
                        parse_bool(&fields[2])?;
                }
                Some("pane_exit_status") => {
                    if fields.len() != 5 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot pane exit-status field count",
                        ));
                    }
                    let pane = payload_pane_mut(&mut payload, &fields[1])?;
                    if pane.exit_status.is_some() {
                        return Err(MezError::invalid_args(
                            "snapshot pane exit status appeared more than once",
                        ));
                    }
                    pane.exit_status = Some(PaneExitStatus {
                        code: parse_optional_i32_field(&fields[2])?,
                        signal: parse_optional_i32_field(&fields[3])?,
                        success: parse_bool(&fields[4])?,
                    });
                }
                Some("pane_geometry") => {
                    if fields.len() != 6 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot pane geometry field count",
                        ));
                    }
                    let pane = payload_pane_mut(&mut payload, &fields[1])?;
                    if pane.geometry.is_some() {
                        return Err(MezError::invalid_args(
                            "snapshot pane geometry appeared more than once",
                        ));
                    }
                    pane.geometry = Some(SnapshotPaneGeometry {
                        column: parse_u16(&fields[2])?,
                        row: parse_u16(&fields[3])?,
                        columns: parse_u16(&fields[4])?,
                        rows: parse_u16(&fields[5])?,
                    });
                }
                Some("pane_terminal_modes") => {
                    if fields.len() != 9
                        && fields.len() != 10
                        && fields.len() != 11
                        && fields.len() != 12
                        && fields.len() != 13
                    {
                        return Err(MezError::invalid_args(
                            "invalid snapshot pane terminal modes field count",
                        ));
                    }
                    let (has_cursor_visible, has_independent_mouse_modes, has_autowrap_mode) =
                        match fields.len() {
                            9 => (false, false, false),
                            10 => (true, false, false),
                            11 => (false, true, false),
                            12 => (true, true, false),
                            13 => (true, true, true),
                            _ => unreachable!("validated pane terminal mode field count"),
                        };
                    let mode_offset = usize::from(has_cursor_visible);
                    let pane = payload_pane_mut(&mut payload, &fields[1])?;
                    let (
                        normal_mouse_tracking_enabled,
                        button_event_mouse_tracking_enabled,
                        any_event_mouse_tracking_enabled,
                        sgr_mode_index,
                    ) = if has_independent_mouse_modes {
                        (
                            parse_bool(&fields[3 + mode_offset])?,
                            parse_bool(&fields[4 + mode_offset])?,
                            parse_bool(&fields[5 + mode_offset])?,
                            6 + mode_offset,
                        )
                    } else {
                        let mouse_tracking_enabled = parse_bool(&fields[3 + mode_offset])?;
                        (
                            mouse_tracking_enabled,
                            mouse_tracking_enabled,
                            mouse_tracking_enabled,
                            4 + mode_offset,
                        )
                    };
                    pane.terminal_modes = TerminalModeState {
                        cursor_visible: if has_cursor_visible {
                            parse_bool(&fields[2])?
                        } else {
                            true
                        },
                        bracketed_paste_enabled: parse_bool(&fields[2 + mode_offset])?,
                        normal_mouse_tracking_enabled,
                        button_event_mouse_tracking_enabled,
                        any_event_mouse_tracking_enabled,
                        sgr_mouse_enabled: parse_bool(&fields[sgr_mode_index])?,
                        application_cursor_enabled: parse_bool(&fields[sgr_mode_index + 1])?,
                        origin_mode_enabled: false,
                        autowrap_enabled: if has_autowrap_mode {
                            parse_bool(&fields[sgr_mode_index + 2])?
                        } else {
                            true
                        },
                        application_keypad_enabled: parse_bool(
                            &fields[sgr_mode_index + 2 + usize::from(has_autowrap_mode)],
                        )?,
                        focus_events_enabled: parse_bool(
                            &fields[sgr_mode_index + 3 + usize::from(has_autowrap_mode)],
                        )?,
                        title: non_empty_string(
                            &fields[sgr_mode_index + 4 + usize::from(has_autowrap_mode)],
                        ),
                    };
                }
                Some("pane_terminal_saved_cursor") => {
                    if fields.len() != 4 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot pane saved cursor field count",
                        ));
                    }
                    let pane = payload_pane_mut(&mut payload, &fields[1])?;
                    if pane.terminal_saved_state.saved_cursor.is_some() {
                        return Err(MezError::invalid_args(
                            "snapshot pane saved cursor appeared more than once",
                        ));
                    }
                    pane.terminal_saved_state.saved_cursor = Some(TerminalCursorState {
                        row: parse_usize(&fields[2])?,
                        column: parse_usize(&fields[3])?,
                    });
                }
                Some("pane_terminal_saved_dec_private_mode") => {
                    if fields.len() != 4 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot pane saved DEC private mode field count",
                        ));
                    }
                    let pane = payload_pane_mut(&mut payload, &fields[1])?;
                    pane.terminal_saved_state.saved_dec_private_modes.push(
                        TerminalSavedDecPrivateMode {
                            mode: parse_u16(&fields[2])?,
                            enabled: parse_bool(&fields[3])?,
                        },
                    );
                }
                Some("pane_history") => {
                    if fields.len() != 3 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot pane history field count",
                        ));
                    }
                    payload_pane_mut(&mut payload, &fields[1])?
                        .terminal_history
                        .push(fields[2].clone());
                }
                Some("pane_history_style") => {
                    if fields.len() != 15 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot pane history style field count",
                        ));
                    }
                    let line_index = parse_usize(&fields[2])?;
                    let pane = payload_pane_mut(&mut payload, &fields[1])?;
                    if line_index >= pane.terminal_history.len() {
                        return Err(MezError::invalid_args(
                            "snapshot pane history style references unknown history line",
                        ));
                    }
                    while pane.terminal_history_line_style_spans.len() <= line_index {
                        pane.terminal_history_line_style_spans.push(Vec::new());
                    }
                    pane.terminal_history_line_style_spans[line_index].push(TerminalStyleSpan {
                        start: parse_usize(&fields[3])?,
                        length: parse_usize(&fields[4])?,
                        rendition: GraphicRendition {
                            bold: parse_bool(&fields[5])?,
                            dim: parse_bool(&fields[6])?,
                            italic: parse_bool(&fields[7])?,
                            underline: parse_bool(&fields[8])?,
                            double_underline: parse_bool(&fields[9])?,
                            strikethrough: parse_bool(&fields[10])?,
                            inverse: parse_bool(&fields[11])?,
                            hidden: parse_bool(&fields[12])?,
                            foreground: parse_snapshot_terminal_color(&fields[13])?,
                            background: parse_snapshot_terminal_color(&fields[14])?,
                        },
                    });
                }
                Some("pane_visible") => {
                    if fields.len() != 3 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot pane visible field count",
                        ));
                    }
                    payload_pane_mut(&mut payload, &fields[1])?
                        .visible_lines
                        .push(fields[2].clone());
                }
                Some("pane_visible_style") => {
                    if fields.len() != 15 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot pane visible style field count",
                        ));
                    }
                    let line_index = parse_usize(&fields[2])?;
                    let pane = payload_pane_mut(&mut payload, &fields[1])?;
                    if line_index >= pane.visible_lines.len() {
                        return Err(MezError::invalid_args(
                            "snapshot pane visible style references unknown visible line",
                        ));
                    }
                    while pane.visible_line_style_spans.len() <= line_index {
                        pane.visible_line_style_spans.push(Vec::new());
                    }
                    pane.visible_line_style_spans[line_index].push(TerminalStyleSpan {
                        start: parse_usize(&fields[3])?,
                        length: parse_usize(&fields[4])?,
                        rendition: GraphicRendition {
                            bold: parse_bool(&fields[5])?,
                            dim: parse_bool(&fields[6])?,
                            italic: parse_bool(&fields[7])?,
                            underline: parse_bool(&fields[8])?,
                            double_underline: parse_bool(&fields[9])?,
                            strikethrough: parse_bool(&fields[10])?,
                            inverse: parse_bool(&fields[11])?,
                            hidden: parse_bool(&fields[12])?,
                            foreground: parse_snapshot_terminal_color(&fields[13])?,
                            background: parse_snapshot_terminal_color(&fields[14])?,
                        },
                    });
                }
                Some("pane_transcript") => {
                    if fields.len() != 3 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot pane transcript field count",
                        ));
                    }
                    payload_pane_mut(&mut payload, &fields[1])?
                        .transcript_refs
                        .push(fields[2].clone());
                }
                _ => return Err(MezError::invalid_args("unknown snapshot payload record")),
            }
        }

        normalize_payload_visible_line_style_spans(&mut payload);
        payload.validate()?;
        Ok(payload)
    }
}
