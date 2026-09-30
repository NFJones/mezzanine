//! Durable presentation reconstruction and transient restoration after resize.
//!
//! Semantic sources are replayed through the ordinary renderers on the existing
//! conversation screen. Reconstruction preserves exact transient lineage and
//! restores the prior screen and ownership maps if any replay step fails.

use super::*;

impl RuntimeSessionService {
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
            let durable_screen = self.agent_pane_screen(pane_id).cloned().ok_or_else(|| {
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
            return Err(error);
        }
        self.presentation
            .agent_presentation_projection_cache
            .insert(pane_id.to_string(), (session_id, size));
        Ok(true)
    }
}
