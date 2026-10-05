//! Durable presentation reconstruction and transient restoration after resize.
//!
//! Semantic sources are replayed through the ordinary renderers on the existing
//! conversation screen. Reconstruction preserves exact transient lineage and
//! restores the prior screen and ownership maps if any replay step fails.

use super::*;

impl RuntimeSessionService {
    /// Replays persisted presentation entries into the pane terminal buffer.
    pub(crate) fn replay_agent_presentation_entries_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        entries: &[AgentPresentationEntry],
    ) -> Result<bool> {
        if entries.is_empty() {
            return Ok(false);
        }
        self.validate_agent_presentation_replay_target(pane_id, entries)?;
        self.presentation
            .agent_presentation_replay_panes
            .insert(pane_id.to_string());
        let result = (|| -> Result<bool> {
            let mut sorted_entries = entries.iter().collect::<Vec<_>>();
            sorted_entries.sort_by_key(|entry| entry.sequence);
            for entry in sorted_entries {
                if entry.source_content_type.as_deref()
                    == Some(crate::storage::transcript::steering::CONTENT_TYPE)
                {
                    let source = crate::storage::transcript::steering::Source::decode(
                        entry.source_text.as_deref().unwrap_or_default(),
                    )?;
                    if source.conversation_id != entry.conversation_id
                        || source.receipt.turn_id != entry.turn_id
                    {
                        return Err(MezError::invalid_args("steering replay ownership changed"));
                    }
                    let key = (
                        pane_id.to_string(),
                        source.conversation_id.clone(),
                        source.receipt.id.clone(),
                    );
                    self.append_settled_steering_source(pane_id, &source)?;
                    self.presentation.presented_steering_receipts.insert(key);
                    continue;
                }
                let activity = if entry.source_content_type.as_deref()
                    == Some(crate::storage::transcript::activity::ACTIVITY_CONTENT_TYPE)
                {
                    let source = crate::storage::transcript::activity::ActivitySource::decode(
                        entry.source_text.as_deref().unwrap_or_default(),
                    )?;
                    if source.conversation_id != entry.conversation_id
                        || entry.turn_id.as_deref() != Some(source.turn_id.as_str())
                    {
                        return Err(MezError::invalid_args(
                            "activity replay identity differs from presentation owner",
                        ));
                    }
                    Some(source)
                } else {
                    None
                };
                if let (Some(source_text), Some(source_content_type)) = (
                    activity
                        .as_ref()
                        .map(|source| source.preview_source.as_deref().unwrap_or(&source.source))
                        .or(entry.source_text.as_deref()),
                    activity
                        .as_ref()
                        .map(|source| source.content_type.as_str())
                        .or(entry.source_content_type.as_deref()),
                ) {
                    if source_content_type == AGENT_PRESENTATION_USER_PROMPT_CONTENT_TYPE {
                        self.append_agent_user_prompt_to_terminal_buffer(pane_id, source_text)?;
                        continue;
                    }
                    if source_content_type == AGENT_PRESENTATION_PARENT_PROMPT_CONTENT_TYPE {
                        self.append_agent_parent_prompt_to_terminal_buffer(pane_id, source_text)?;
                        continue;
                    }
                    if source_content_type == AGENT_PRESENTATION_PEER_MESSAGE_CONTENT_TYPE {
                        // A record that does not decode is dropped rather than
                        // rendered as assistant text: this content type is only
                        // ever written by the peer echo writer, so a malformed
                        // or oversized source is corrupt log state, not model
                        // output, and printing its raw bytes would invent a
                        // transcript line that never existed.
                        if let Some(encoded) = decoded_peer_message_presentation_source(source_text)
                        {
                            // Replayed peer sources use the same renderer as live
                            // presentation: normal mode admits only exact canonical
                            // plaintext, verbose mode renders every bounded raw
                            // payload, and a pre-persistence suppression leaves no
                            // presentation record for replay to resurrect.
                            if encoded.direction == "sent" {
                                let source = RuntimeStreamingMessageSource {
                                    recipient: encoded.peer.clone(),
                                    recipient_label: encoded.peer.clone(),
                                    direct_parent: encoded.direct_parent,
                                    content_type: encoded
                                        .content_type
                                        .as_deref()
                                        .unwrap_or_default()
                                        .to_string(),
                                    text: encoded.payload.clone(),
                                    complete: true,
                                };
                                self.append_accepted_outbound_message_presentation(
                                    pane_id,
                                    encoded.action_identity.as_deref().unwrap_or_default(),
                                    &source,
                                )?;
                            } else {
                                self.append_agent_peer_message_to_terminal_buffer(
                                    pane_id,
                                    &PeerMessagePresentation {
                                        receive_identity: encoded.receive_identity.as_deref(),
                                        peer_label: encoded.peer.as_str(),
                                        content_type: encoded.content_type.as_deref(),
                                        payload: encoded.payload.as_str(),
                                        direct_parent: encoded.direct_parent,
                                        presentation_eligible: encoded
                                            .presentation_eligible
                                            .unwrap_or(false),
                                    },
                                )?;
                            }
                        }
                        continue;
                    }
                    if source_content_type == AGENT_PRESENTATION_THINKING_CONTENT_TYPE {
                        self.append_agent_thinking_text_to_terminal_buffer(pane_id, source_text)?;
                        continue;
                    }
                    if source_content_type == AGENT_PRESENTATION_MACRO_LIFECYCLE_CONTENT_TYPE
                        && let Some((macro_name, step_index, total_steps, status, is_error)) =
                            macro_lifecycle_presentation_source(source_text)
                    {
                        if is_error {
                            self.append_agent_macro_error_to_terminal_buffer(
                                pane_id,
                                &macro_name,
                                step_index.unwrap_or_default(),
                                total_steps,
                                &status,
                            )?;
                        } else {
                            self.append_agent_macro_status_to_terminal_buffer(
                                pane_id,
                                &macro_name,
                                step_index,
                                total_steps,
                                &status,
                            )?;
                        }
                        continue;
                    }
                    if source_content_type == AGENT_PRESENTATION_COMMAND_PREVIEW_CONTENT_TYPE {
                        self.append_agent_command_preview_to_terminal_buffer(pane_id, source_text)?;
                        continue;
                    }
                    if source_content_type
                        == AGENT_PRESENTATION_TRUNCATED_COMMAND_PREVIEW_CONTENT_TYPE
                    {
                        self.append_agent_command_preview_source_to_terminal_buffer(
                            pane_id,
                            source_text,
                            true,
                        )?;
                        continue;
                    }
                    if source_content_type == AGENT_PRESENTATION_ACTION_HEADER_CONTENT_TYPE {
                        let rendered_line = agent_action_execution_rendered_line(
                            source_text,
                            &self.presentation.settings.ui_theme,
                        );
                        self.append_agent_terminal_log_rendered_lines_to_buffer(
                            pane_id,
                            AgentTerminalPresentationStyle::Status,
                            &[rendered_line],
                            Some((source_text, source_content_type)),
                        )?;
                        continue;
                    }
                    if source_content_type == AGENT_PRESENTATION_STYLED_LINES_CONTENT_TYPE
                        && let Some(styled_lines) =
                            styled_agent_presentation_source_lines(source_text)
                        && !styled_lines.is_empty()
                    {
                        self.append_agent_terminal_styled_lines_to_buffer(pane_id, &styled_lines)?;
                        continue;
                    }
                    self.append_agent_assistant_content_to_terminal_buffer(
                        pane_id,
                        source_text,
                        source_content_type,
                    )?;
                    continue;
                }
                if let Some(ansi_text) = entry.ansi_text.as_deref() {
                    self.ensure_current_agent_presentation_screen(pane_id)?;
                    self.retire_agent_streaming_say_before_pane_write(pane_id)?;
                    let (conversation_id, mut screen, preview_presentation) =
                        self.agent_shell_preview_write_base(pane_id)?;
                    Self::feed_agent_terminal_screen(
                        &mut screen,
                        ansi_text.as_bytes(),
                        "replaying persisted agent presentation",
                    )?;
                    if !entry.copy_lines.is_empty() {
                        screen
                            .set_recent_normal_copy_texts(&entry.copy_lines, AGENT_COPY_SKIP_LINE);
                    }
                    self.install_agent_shell_preview_write(
                        pane_id,
                        &conversation_id,
                        screen,
                        preview_presentation,
                    )?;
                    continue;
                }
                let styled_lines = entry
                    .display_lines
                    .iter()
                    .enumerate()
                    .map(|(index, line)| {
                        let style = entry
                            .style_names
                            .get(index)
                            .and_then(|name| {
                                AgentTerminalPresentationStyle::from_persistence_name(name)
                            })
                            .unwrap_or(AgentTerminalPresentationStyle::Status);
                        (style, line.clone())
                    })
                    .collect::<Vec<_>>();
                self.append_agent_terminal_styled_lines_to_buffer(pane_id, &styled_lines)?;
                if !entry.copy_lines.is_empty()
                    && let Some(screen) = self.agent_pane_screen_mut(pane_id)
                {
                    screen.set_recent_normal_copy_texts(&entry.copy_lines, AGENT_COPY_SKIP_LINE);
                }
            }
            let state = self
                .presentation
                .agent_prompt_inputs
                .entry(pane_id.to_string())
                .or_insert_with(|| default_runtime_agent_prompt_input().into());
            state.display_lines.clear();
            Ok(true)
        })();
        self.presentation
            .agent_presentation_replay_panes
            .remove(pane_id);
        result
    }

    /// Replays synthesized transcript fallback lines without persisting them as new presentation.
    pub(crate) fn replay_agent_transcript_fallback_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        display_lines: Vec<String>,
    ) -> Result<()> {
        self.presentation
            .agent_presentation_replay_panes
            .insert(pane_id.to_string());
        let result = self.set_agent_prompt_display_lines(pane_id, display_lines);
        self.presentation
            .agent_presentation_replay_panes
            .remove(pane_id);
        result
    }

    /// Rebuilds a resized agent pane from complete durable presentation source.
    ///
    /// The rebuild is intentionally limited to histories that contain semantic
    /// source. Snapshot-only histories retain ordinary terminal resize behavior
    /// because their saved rows cannot reproduce renderer-level layout.
    #[cfg(test)]
    pub(crate) fn rebuild_agent_presentation_after_resize(
        &mut self,
        pane_id: &str,
        size: Size,
    ) -> Result<bool> {
        if self
            .agent_pane_screen(pane_id)
            .is_some_and(TerminalScreen::normal_viewport_detached_from_history)
        {
            return Ok(false);
        }
        let Some(session) = self.agent_shell_store().get(pane_id) else {
            return Ok(false);
        };
        if session.visibility != AgentShellVisibility::Visible {
            return Ok(false);
        }
        if session.ephemeral {
            return Ok(false);
        }
        let session_id = session.session_id.clone();
        if self
            .presentation
            .agent_presentation_projection_cache
            .get(pane_id)
            .is_some_and(|(cached_session_id, projection_size)| {
                cached_session_id == &session_id && *projection_size == size
            })
        {
            return Ok(false);
        }
        let Some(store) = self.persistence.transcript_store() else {
            return Ok(false);
        };
        let entries = store.inspect_presentation(&session_id)?;
        self.rebuild_agent_presentation_after_resize_from_entries(pane_id, size, &entries)
    }

    /// Rebuilds a resized agent pane from already-decoded durable presentation source.
    pub(super) fn rebuild_agent_presentation_after_resize_from_entries(
        &mut self,
        pane_id: &str,
        size: Size,
        entries: &[AgentPresentationEntry],
    ) -> Result<bool> {
        let Some(session_id) = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.session_id.clone())
        else {
            return Ok(false);
        };
        if !entries.iter().any(|entry| entry.source_text.is_some()) {
            return Ok(false);
        }
        let previous_screen = self.agent_pane_screen(pane_id).cloned();
        let previous_lineage = self.agent_pane_screen_lineage(pane_id, &session_id);
        let previous_steering = self.snapshot_steering_presentation_surface(pane_id);
        let previous_preview = self
            .presentation
            .agent_shell_output_previews
            .remove(pane_id);
        let previous_streaming = self
            .presentation
            .agent_streaming_say_presentations
            .remove(pane_id);
        let preview_to_restore = previous_preview.clone().filter(|preview| {
            preview.conversation_id == session_id
                && previous_lineage == Some(preview.installed_lineage)
        });
        let streaming_to_restore = previous_streaming.clone().filter(|streaming| {
            streaming.conversation_id == session_id
                && previous_lineage == Some(streaming.installed_lineage)
        });
        let rebuild_result = (|| -> Result<()> {
            let rebuilt = TerminalScreen::new_with_history_config(
                size,
                self.terminal_history_limit(),
                self.terminal_history_rotate_lines(),
            )?;
            self.set_agent_pane_screen(pane_id.to_string(), session_id.clone(), rebuilt);
            self.replay_agent_presentation_entries_to_terminal_buffer(pane_id, entries)?;
            let durable_screen = self
                .agent_screen_without_pending_steering(pane_id, &session_id)
                .ok_or_else(|| {
                    MezError::invalid_state("resized agent presentation screen disappeared")
                })?;
            let durable_lineage = self
                .agent_pane_screen_lineage(pane_id, &session_id)
                .ok_or_else(|| {
                    MezError::invalid_state("resized agent presentation lineage disappeared")
                })?;
            if let Some(mut preview) = preview_to_restore {
                preview.installed_lineage = durable_lineage;
                preview.baseline_screen = std::sync::Arc::new(durable_screen.clone());
                self.presentation
                    .agent_shell_output_previews
                    .insert(pane_id.to_string(), preview);
            }
            if let Some(mut streaming) = streaming_to_restore {
                streaming.installed_lineage = durable_lineage;
                streaming.baseline_screen = std::sync::Arc::new(durable_screen.clone());
                streaming.provider_screen = std::sync::Arc::new(durable_screen.clone());
                streaming.projected_revision = None;
                streaming.projected_context = None;
                streaming.projected_actions = None;
                streaming.projected_rationale = None;
                streaming.projected_lineage = None;
                self.presentation
                    .agent_streaming_say_presentations
                    .insert(pane_id.to_string(), streaming);
            }
            if self
                .presentation
                .agent_shell_output_previews
                .contains_key(pane_id)
                || self
                    .presentation
                    .agent_streaming_say_presentations
                    .contains_key(pane_id)
            {
                self.update_agent_streaming_screen(pane_id, &session_id, durable_screen.clone())?;
            }
            if let Some(work) = self.take_agent_streaming_say_projection_work(
                pane_id,
                self.presentation
                    .agent_streaming_say_presentations
                    .get(pane_id)
                    .map(|streaming| streaming.turn_id.as_str())
                    .unwrap_or(""),
            )? {
                let projection = Self::build_agent_streaming_say_projection(work)?;
                if !self.apply_agent_streaming_say_projection_result(projection)? {
                    return Err(MezError::invalid_state(
                        "resized streaming presentation projection was rejected",
                    ));
                }
            }
            Ok(())
        })();
        if let Err(error) = rebuild_result {
            self.presentation
                .agent_shell_output_previews
                .remove(pane_id);
            self.presentation
                .agent_streaming_say_presentations
                .remove(pane_id);
            if let Some(previous) = previous_screen {
                self.set_agent_pane_screen(pane_id.to_string(), session_id.clone(), previous);
            } else {
                self.remove_agent_pane_screen(pane_id);
            }
            if let Some(preview) = previous_preview {
                self.presentation
                    .agent_shell_output_previews
                    .insert(pane_id.to_string(), preview);
            }
            if let Some(streaming) = previous_streaming {
                self.presentation
                    .agent_streaming_say_presentations
                    .insert(pane_id.to_string(), streaming);
            }
            self.restore_steering_presentation_surface(pane_id, previous_steering);
            return Err(error);
        }
        self.presentation
            .agent_presentation_projection_cache
            .insert(pane_id.to_string(), (session_id, size));
        Ok(true)
    }
}
