//! Durable line-oriented snapshot payload encoding.

use super::helpers::*;
use super::*;

impl SessionSnapshotPayload {
    /// Runs the encode operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::snapshot) fn encode(&self) -> Result<String> {
        let mut output = String::new();
        output.push_str(&format!(
            "payload_version\t{SNAPSHOT_PAYLOAD_FORMAT_VERSION}\n"
        ));
        output.push_str(&format!(
            "session\t{}\t{}\t{}\t{}\t{}\t{}\n",
            escape_field(&self.session_id),
            escape_field(&self.name),
            self.state.as_str(),
            self.authoritative_columns,
            self.authoritative_rows,
            escape_field(self.active_window_id.as_deref().unwrap_or(""))
        ));
        output.push_str(&format!(
            "shell\t{}\t{}\t{}\n",
            escape_field(&self.shell.path),
            escape_field(&self.shell.source),
            self.shell.used_fallback
        ));
        for layer in &self.active_config_layers {
            output.push_str(&format!(
                "config_layer\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                escape_field(&layer.id),
                escape_field(&layer.layer_type),
                layer.precedence,
                escape_field(layer.path.as_deref().unwrap_or("")),
                layer.trusted,
                layer.applied,
                layer.schema_version
            ));
            for diagnostic in &layer.diagnostics {
                output.push_str(&format!(
                    "config_diagnostic\t{}\t{}\t{}\n",
                    escape_field(&layer.id),
                    escape_field(&diagnostic.path),
                    escape_field(&diagnostic.message)
                ));
            }
        }
        encode_frame_settings("window", &self.frame_state.window, &mut output);
        encode_frame_settings("pane", &self.frame_state.pane, &mut output);
        for agent_session in &self.agent_sessions {
            output.push_str(&format!(
                "agent_session\t{}\t{}\t{}\t{}\t{}\n",
                escape_field(&agent_session.pane_id),
                escape_field(&agent_session.conversation_id),
                escape_field(&agent_session.visibility),
                escape_field(agent_session.running_turn_id.as_deref().unwrap_or("")),
                agent_session.transcript_entries
            ));
        }
        for grant in &self.approval_grants {
            output.push_str(&format!(
                "approval_grant\t{}\t{}\t{}\n",
                escape_field(&grant.id),
                escape_field(&grant.scope),
                escape_field(&grant.decision)
            ));
            for token in &grant.command_prefix {
                output.push_str(&format!(
                    "approval_grant_prefix\t{}\t{}\n",
                    escape_field(&grant.id),
                    escape_field(token)
                ));
            }
        }
        for request in &self.approval_requests {
            output.push_str(&format!(
                "approval_request\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                escape_field(&request.id),
                escape_field(&request.requesting_agent_id),
                escape_field(&request.pane_id),
                escape_field(&request.action_kind),
                escape_field(&request.action_summary),
                escape_field(&request.state),
                escape_field(request.decision.as_deref().unwrap_or("")),
                request
                    .created_at_unix_seconds
                    .map(|seconds| seconds.to_string())
                    .unwrap_or_default(),
                request
                    .decided_at_unix_seconds
                    .map(|seconds| seconds.to_string())
                    .unwrap_or_default(),
                escape_field(request.decided_by_client_id.as_deref().unwrap_or("")),
                escape_field(request.redirect_instruction.as_deref().unwrap_or(""))
            ));
            for agent_id in &request.parent_agent_chain {
                output.push_str(&format!(
                    "approval_request_parent\t{}\t{}\n",
                    escape_field(&request.id),
                    escape_field(agent_id)
                ));
            }
            for effect in &request.declared_effects {
                output.push_str(&format!(
                    "approval_request_effect\t{}\t{}\n",
                    escape_field(&request.id),
                    escape_field(effect)
                ));
            }
            for rule in &request.matched_rules {
                output.push_str(&format!(
                    "approval_request_rule\t{}\t{}\n",
                    escape_field(&request.id),
                    escape_field(rule)
                ));
            }
            for scope in &request.read_scopes {
                output.push_str(&format!(
                    "approval_request_read_scope\t{}\t{}\n",
                    escape_field(&request.id),
                    escape_field(scope)
                ));
            }
            for scope in &request.write_scopes {
                output.push_str(&format!(
                    "approval_request_write_scope\t{}\t{}\n",
                    escape_field(&request.id),
                    escape_field(scope)
                ));
            }
        }
        if let Some(message_state) = &self.message_state {
            let encoded = serde_json::to_string(message_state)
                .map_err(|_| MezError::invalid_state("snapshot MMP state could not be encoded"))?;
            output.push_str(&format!("message_state\t{}\n", escape_field(&encoded)));
        }
        if !self.mcp_servers.is_empty() {
            let encoded = serde_json::to_string(&self.mcp_servers)
                .map_err(|_| MezError::invalid_state("snapshot MCP state could not be encoded"))?;
            output.push_str(&format!("mcp_state\t{}\n", escape_field(&encoded)));
        }
        for group in &self.window_groups {
            output.push_str(&format!(
                "window_group\t{}\t{}\t{}\t{}\t{}\t{}\n",
                escape_field(&group.group_id),
                group.index,
                escape_field(&group.name),
                group.active,
                escape_field(group.active_window_id.as_deref().unwrap_or("")),
                escape_field(group.last_active_window_id.as_deref().unwrap_or(""))
            ));
            for window_id in &group.window_ids {
                output.push_str(&format!(
                    "window_group_window\t{}\t{}\n",
                    escape_field(&group.group_id),
                    escape_field(window_id)
                ));
            }
        }
        for window in &self.windows {
            output.push_str(&format!(
                "window\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                escape_field(&window.window_id),
                window.index,
                escape_field(&window.name),
                window.active,
                window.columns,
                window.rows,
                escape_field(&window.layout_policy)
            ));
            if let Some(layout_root) = &window.layout_root {
                let encoded = serde_json::to_string(layout_root).map_err(|_| {
                    MezError::invalid_state("snapshot layout tree could not be encoded")
                })?;
                output.push_str(&format!(
                    "window_layout\t{}\t{}\n",
                    escape_field(&window.window_id),
                    escape_field(&encoded)
                ));
            }
            for pane in &window.panes {
                output.push_str(&format!(
                    "pane\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                    escape_field(&pane.pane_id),
                    pane.index,
                    escape_field(&pane.title),
                    pane.active,
                    pane.live_at_snapshot,
                    pane.columns,
                    pane.rows
                ));
                output.push_str(&format!(
                    "pane_shell\t{}\t{}\t{}\t{}\t{}\n",
                    escape_field(&pane.pane_id),
                    optional_u32_field(pane.primary_pid),
                    escape_field(&pane.process_state),
                    escape_field(pane.current_working_directory.as_deref().unwrap_or("")),
                    escape_field(&pane.readiness_state)
                ));
                if let Some(exit_status) = pane.exit_status {
                    output.push_str(&format!(
                        "pane_exit_status\t{}\t{}\t{}\t{}\n",
                        escape_field(&pane.pane_id),
                        optional_i32_field(exit_status.code),
                        optional_i32_field(exit_status.signal),
                        exit_status.success
                    ));
                }
                if pane.alternate_screen_active {
                    output.push_str(&format!(
                        "pane_alternate_screen\t{}\ttrue\n",
                        escape_field(&pane.pane_id)
                    ));
                }
                if let Some(geometry) = &pane.geometry {
                    output.push_str(&format!(
                        "pane_geometry\t{}\t{}\t{}\t{}\t{}\n",
                        escape_field(&pane.pane_id),
                        geometry.column,
                        geometry.row,
                        geometry.columns,
                        geometry.rows
                    ));
                }
                encode_terminal_modes(&pane.pane_id, &pane.terminal_modes, &mut output);
                encode_terminal_saved_state(&pane.pane_id, &pane.terminal_saved_state, &mut output);
                for line in &pane.terminal_history {
                    output.push_str(&format!(
                        "pane_history\t{}\t{}\n",
                        escape_field(&pane.pane_id),
                        escape_field(line)
                    ));
                }
                for (line_index, style_spans) in
                    pane.terminal_history_line_style_spans.iter().enumerate()
                {
                    for span in style_spans {
                        output.push_str(&format!(
                            "pane_history_style\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                            escape_field(&pane.pane_id),
                            line_index,
                            span.start,
                            span.length,
                            span.rendition.bold,
                            span.rendition.dim,
                            span.rendition.italic,
                            span.rendition.underline,
                            span.rendition.double_underline,
                            span.rendition.strikethrough,
                            span.rendition.inverse,
                            span.rendition.hidden,
                            escape_field(&snapshot_terminal_color_name(span.rendition.foreground)),
                            escape_field(&snapshot_terminal_color_name(span.rendition.background))
                        ));
                    }
                }
                for line in &pane.visible_lines {
                    output.push_str(&format!(
                        "pane_visible\t{}\t{}\n",
                        escape_field(&pane.pane_id),
                        escape_field(line)
                    ));
                }
                for (line_index, style_spans) in pane.visible_line_style_spans.iter().enumerate() {
                    for span in style_spans {
                        output.push_str(&format!(
                            "pane_visible_style\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                            escape_field(&pane.pane_id),
                            line_index,
                            span.start,
                            span.length,
                            span.rendition.bold,
                            span.rendition.dim,
                            span.rendition.italic,
                            span.rendition.underline,
                            span.rendition.double_underline,
                            span.rendition.strikethrough,
                            span.rendition.inverse,
                            span.rendition.hidden,
                            escape_field(&snapshot_terminal_color_name(span.rendition.foreground)),
                            escape_field(&snapshot_terminal_color_name(span.rendition.background))
                        ));
                    }
                }
                for transcript_ref in &pane.transcript_refs {
                    output.push_str(&format!(
                        "pane_transcript\t{}\t{}\n",
                        escape_field(&pane.pane_id),
                        escape_field(transcript_ref)
                    ));
                }
            }
        }
        Ok(output)
    }
}
