//! Runtime service application of projected agent presentation rows.

use super::actions::{
    agent_action_execution_display_header, agent_action_execution_rendered_line,
    agent_action_model_thinking_lines, agent_action_result_display_header,
    agent_macro_lifecycle_display_lines_for_width, agent_thinking_display_lines_for_width,
    bounded_agent_action_result_display_lines,
};
use super::diff::{
    agent_action_result_uses_diff_preview, cleaned_agent_diff_source_lines,
    readable_agent_diff_display_lines_for_width, streaming_agent_diff_display_lines_for_width,
};
use super::style::{
    AGENT_PROMPT_TEXT_PREFIX, AGENT_TERMINAL_MESSAGE_PREFIX, AgentTerminalPresentationStyle,
};
use super::text::{
    agent_say_text_is_displayed_patch_block, agent_terminal_label_rendition,
    append_styled_agent_terminal_line, append_styled_agent_terminal_rendered_line,
    bounded_agent_terminal_presentation_columns, bounded_command_preview_source,
    command_preview_terminal_rendered_lines, fit_agent_terminal_text_width,
    render_agent_markdown_body_lines, sanitized_agent_terminal_line,
    wrapped_prefixed_agent_terminal_lines,
};
use super::{
    AGENT_COPY_SKIP_LINE, AgentAction, RichTextLine, TerminalStyleSpan, UnicodeWidthStr,
    diff_section_path, frame_markdown_lines, parse_unified_diff_sections, prefix_rich_text_lines,
    wrap_rich_text_lines_to_width,
};
use crate::runtime::render::{
    ActionResult, AgentPresentationEntry, MezError, Result, RuntimeSessionService,
    RuntimeStreamingSayAction, RuntimeStreamingSayPresentation,
    RuntimeStreamingSayProjectionContext, Size, TerminalScreen, current_unix_seconds,
    default_runtime_agent_prompt_input,
};
use mez_agent::{
    AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE, AgentShellVisibility, agent_output_content_type_is_diff,
    agent_output_content_type_is_markdown,
};
use mez_mux::render::markdown_block_copy_lines;

/// Content type for width-independent styled agent presentation records.
const AGENT_PRESENTATION_STYLED_LINES_CONTENT_TYPE: &str =
    "application/vnd.mezzanine.agent-presentation.styled-lines+json; charset=utf-8";
/// Content type for a raw user prompt that must be wrapped at replay geometry.
const AGENT_PRESENTATION_USER_PROMPT_CONTENT_TYPE: &str =
    "application/vnd.mezzanine.agent-presentation.user-prompt+text; charset=utf-8";
/// Content type for a shell command preview rendered at replay geometry.
const AGENT_PRESENTATION_COMMAND_PREVIEW_CONTENT_TYPE: &str =
    "application/vnd.mezzanine.agent-presentation.command-preview+text; charset=utf-8";
/// Content type for a bounded command preview whose source omitted a tail.
const AGENT_PRESENTATION_TRUNCATED_COMMAND_PREVIEW_CONTENT_TYPE: &str =
    "application/vnd.mezzanine.agent-presentation.command-preview-truncated+text; charset=utf-8";
/// Content type for one action-execution header rendered at replay geometry.
const AGENT_PRESENTATION_ACTION_HEADER_CONTENT_TYPE: &str =
    "application/vnd.mezzanine.agent-presentation.action-header+text; charset=utf-8";
/// Content type for a parent-supplied subagent prompt rendered at replay geometry.
const AGENT_PRESENTATION_PARENT_PROMPT_CONTENT_TYPE: &str =
    "application/vnd.mezzanine.agent-presentation.parent-prompt+text; charset=utf-8";
/// Content type for rationale text rendered at replay geometry.
const AGENT_PRESENTATION_THINKING_CONTENT_TYPE: &str =
    "application/vnd.mezzanine.agent-presentation.thinking+text; charset=utf-8";
/// Content type for structured macro lifecycle rows rendered at replay geometry.
const AGENT_PRESENTATION_MACRO_LIFECYCLE_CONTENT_TYPE: &str =
    "application/vnd.mezzanine.agent-presentation.macro-lifecycle+json; charset=utf-8";

/// One media-type-specific projection of accumulated streamed `say` source.
struct StreamingSayProjection {
    style: AgentTerminalPresentationStyle,
    rendered_lines: Vec<RichTextLine>,
    copy_lines: Vec<String>,
}

/// Decodes one typed styled-line presentation record for geometry-aware replay.
fn styled_agent_presentation_source_lines(
    source_text: &str,
) -> Option<Vec<(AgentTerminalPresentationStyle, String)>> {
    let encoded = serde_json::from_str::<Vec<(String, String)>>(source_text).ok()?;
    (!encoded.is_empty()).then(|| {
        encoded
            .into_iter()
            .filter_map(|(style, text)| {
                AgentTerminalPresentationStyle::from_persistence_name(&style)
                    .map(|style| (style, text))
            })
            .collect()
    })
}

/// Decodes one structured macro lifecycle row for geometry-aware replay.
fn macro_lifecycle_presentation_source(
    source_text: &str,
) -> Option<(String, Option<usize>, usize, String, bool)> {
    serde_json::from_str(source_text).ok()
}

/// Runs one terminal presentation operation while containing parser panics.
///
/// A contained panic still becomes an explicit runtime error so callers do not
/// report a dropped presentation batch as successfully rendered.
fn catch_agent_terminal_presentation_panic(context: &str, operation: impl FnOnce()) -> Result<()> {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation)).is_err() {
        return Err(MezError::invalid_state(format!(
            "agent terminal presentation feed panicked while {context}"
        )));
    }
    Ok(())
}

impl RuntimeSessionService {
    /// Returns the active conversation and layout size for one presentation target.
    fn agent_presentation_target(&self, pane_id: &str) -> Result<(String, Size)> {
        let descriptor = self.find_pane_descriptor(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "agent terminal presentation target pane not found",
            )
        })?;
        let conversation_id = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.session_id.clone())
            .ok_or_else(|| {
                MezError::invalid_state("agent terminal presentation target session not found")
            })?;
        Ok((conversation_id, descriptor.size))
    }

    /// Ensures the agent destination is bound to the pane's active conversation.
    fn ensure_current_agent_presentation_screen(
        &mut self,
        pane_id: &str,
    ) -> Result<&mut TerminalScreen> {
        if self.agent_shell_store().get(pane_id).is_none() {
            self.agent_shell_store_mut().ensure_session(pane_id)?;
        }
        let (conversation_id, size) = self.agent_presentation_target(pane_id)?;
        let binding_changed = self
            .agent_pane_screen_state(pane_id)
            .is_some_and(|screen| screen.conversation_id() != conversation_id);
        if binding_changed {
            self.presentation
                .agent_shell_output_status_lines
                .remove(pane_id);
            self.presentation
                .agent_streaming_say_presentations
                .remove(pane_id);
            self.presentation
                .agent_presentation_projection_cache
                .remove(pane_id);
        }
        self.ensure_agent_pane_screen(pane_id, &conversation_id, size)
    }

    /// Validates that replay records belong to the pane's active conversation.
    fn validate_agent_presentation_replay_target(
        &self,
        pane_id: &str,
        entries: &[AgentPresentationEntry],
    ) -> Result<String> {
        let (conversation_id, _size) = self.agent_presentation_target(pane_id)?;
        if entries
            .iter()
            .any(|entry| entry.conversation_id != conversation_id)
        {
            return Err(MezError::invalid_state(
                "agent presentation replay target does not match the active conversation",
            ));
        }
        Ok(conversation_id)
    }

    /// Runs the append agent user prompt to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn append_agent_user_prompt_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        prompt: &str,
    ) -> Result<()> {
        let display_width = self.agent_terminal_markdown_frame_width(pane_id)?;
        let rendered_lines = wrapped_prefixed_agent_terminal_lines("user> ", prompt, display_width);
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane_id,
            AgentTerminalPresentationStyle::UserPrompt,
            rendered_lines.as_slice(),
            &[],
            Some((prompt, AGENT_PRESENTATION_USER_PROMPT_CONTENT_TYPE)),
        )
    }

    /// Appends the parent-supplied prompt at the top of a spawned subagent pane.
    ///
    /// Subagent pane logs should expose the exact parent instruction that
    /// started the child turn so follow-up inspection does not require looking
    /// back through the parent pane.
    pub(crate) fn append_agent_parent_prompt_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        prompt: &str,
    ) -> Result<()> {
        let display_width = self.agent_terminal_markdown_frame_width(pane_id)?;
        let rendered_lines =
            wrapped_prefixed_agent_terminal_lines("parent> ", prompt, display_width);
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane_id,
            AgentTerminalPresentationStyle::UserPrompt,
            rendered_lines.as_slice(),
            &[],
            Some((prompt, AGENT_PRESENTATION_PARENT_PROMPT_CONTENT_TYPE)),
        )
    }

    /// Runs the append agent assistant text to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn append_agent_assistant_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        text: &str,
    ) -> Result<()> {
        self.append_agent_assistant_content_to_terminal_buffer(
            pane_id,
            text,
            AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE,
        )
    }

    /// Appends assistant output using its declared presentation media type.
    pub(crate) fn append_agent_assistant_content_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        text: &str,
        content_type: &str,
    ) -> Result<()> {
        if agent_output_content_type_is_markdown(content_type)
            && !agent_say_text_is_displayed_patch_block(text)
        {
            return self.append_agent_assistant_markdown_to_terminal_buffer(
                pane_id,
                text,
                content_type,
            );
        }
        if agent_output_content_type_is_diff(content_type) {
            return self.append_agent_assistant_diff_to_terminal_buffer(
                pane_id,
                text,
                content_type,
            );
        }
        let display_width = self.agent_terminal_markdown_frame_width(pane_id)?;
        let rendered_lines = wrapped_prefixed_agent_terminal_lines("mez> ", text, display_width);
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane_id,
            AgentTerminalPresentationStyle::Assistant,
            rendered_lines.as_slice(),
            &[],
            Some((text, content_type)),
        )
    }

    /// Returns the display cells available after the agent transcript gutter.
    fn agent_terminal_markdown_frame_width(&self, pane_id: &str) -> Result<usize> {
        let columns = self.agent_terminal_presentation_columns(pane_id)?;
        Ok(bounded_agent_terminal_presentation_columns(columns)
            .saturating_sub(UnicodeWidthStr::width(AGENT_TERMINAL_MESSAGE_PREFIX))
            .max(1))
    }

    /// Returns display cells available after the agent transcript gutter.
    fn agent_terminal_markdown_terminal_width(&self, pane_id: &str) -> Result<usize> {
        let columns = self.agent_terminal_presentation_columns(pane_id)?;
        Ok(columns
            .saturating_sub(UnicodeWidthStr::width(AGENT_TERMINAL_MESSAGE_PREFIX))
            .max(1))
    }

    /// Returns display cells available for editable pane-local prompt text.
    ///
    /// This width mirrors the terminal renderer, which draws the editable text
    /// after both the agent transcript gutter and the `agent>` prompt marker.
    ///
    /// # Parameters
    /// - `pane_id`: Pane whose current presentation width bounds the prompt.
    pub(crate) fn agent_prompt_editable_body_width(&self, pane_id: &str) -> Result<usize> {
        let columns = self.agent_terminal_presentation_columns(pane_id)?;
        let prompt_prefix_width = UnicodeWidthStr::width(AGENT_TERMINAL_MESSAGE_PREFIX)
            .saturating_add(UnicodeWidthStr::width(AGENT_PROMPT_TEXT_PREFIX));
        Ok(columns.saturating_sub(prompt_prefix_width).max(1))
    }

    /// Returns the current pane presentation width in terminal display cells.
    fn agent_terminal_presentation_columns(&self, pane_id: &str) -> Result<usize> {
        let descriptor = self.find_pane_descriptor(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "agent terminal presentation target pane not found",
            )
        })?;
        if let Some(columns) = self.agent_terminal_render_region_columns(pane_id) {
            return Ok(columns);
        }
        let columns = self
            .agent_pane_screen(pane_id)
            .map(|screen| screen.size().columns)
            .unwrap_or(descriptor.size.columns);
        Ok(usize::from(columns))
    }

    /// Returns the pane-local render width used by the terminal compositor.
    fn agent_terminal_render_region_columns(&self, pane_id: &str) -> Option<usize> {
        let window = self.session.active_window()?;
        let pane = window
            .panes()
            .iter()
            .find(|pane| pane.id.as_str() == pane_id)?;
        let plan = self.window_presentation_plan(window)?;
        Some(usize::from(plan.pane(pane.index)?.content_size.columns))
    }

    /// Returns the pane width to persist with one agent presentation entry.
    fn agent_presentation_terminal_width(&self, pane_id: &str) -> Option<u16> {
        self.agent_pane_screen(pane_id)
            .map(|screen| screen.size().columns)
            .or_else(|| {
                self.find_pane_descriptor(pane_id)
                    .map(|descriptor| descriptor.size.columns)
            })
    }

    /// Persists one durable user-visible agent presentation entry.
    fn persist_agent_presentation_entry(
        &mut self,
        pane_id: &str,
        style_names: Vec<String>,
        display_lines: Vec<String>,
        copy_lines: Vec<String>,
        ansi_text: String,
        source: Option<(&str, &str)>,
    ) {
        if self
            .presentation
            .agent_presentation_replay_panes
            .contains(pane_id)
            || display_lines.is_empty()
            || style_names.len() != display_lines.len()
        {
            return;
        }
        self.presentation
            .agent_presentation_projection_cache
            .remove(pane_id);
        let Some(session) = self.agent_shell_store().get(pane_id) else {
            return;
        };
        if session.ephemeral {
            return;
        }
        let Some(store) = self.persistence.transcript_store() else {
            return;
        };
        let Some(terminal_width) = self.agent_presentation_terminal_width(pane_id) else {
            return;
        };
        let Ok(sequence) = store.next_presentation_sequence(&session.session_id) else {
            return;
        };
        let entry = AgentPresentationEntry {
            conversation_id: session.session_id.clone(),
            sequence,
            created_at_unix_seconds: current_unix_seconds().max(1),
            pane_id: pane_id.to_string(),
            turn_id: session.running_turn_id.clone(),
            terminal_width,
            style_names,
            display_lines,
            copy_lines,
            ansi_text: (!ansi_text.is_empty()).then_some(ansi_text),
            source_text: source.map(|(text, _content_type)| text.to_string()),
            source_content_type: source.map(|(_text, content_type)| content_type.to_string()),
        };
        let _ = store.append_presentation(&entry);
    }

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
                if let (Some(source_text), Some(source_content_type)) = (
                    entry.source_text.as_deref(),
                    entry.source_content_type.as_deref(),
                ) {
                    if source_content_type == AGENT_PRESENTATION_USER_PROMPT_CONTENT_TYPE {
                        self.append_agent_user_prompt_to_terminal_buffer(pane_id, source_text)?;
                        continue;
                    }
                    if source_content_type == AGENT_PRESENTATION_PARENT_PROMPT_CONTENT_TYPE {
                        self.append_agent_parent_prompt_to_terminal_buffer(pane_id, source_text)?;
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
                        self.append_agent_terminal_rendered_lines_to_buffer(
                            pane_id,
                            AgentTerminalPresentationStyle::Status,
                            &[rendered_line],
                            &[],
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
                    self.clear_agent_shell_output_status_line(pane_id)?;
                    let screen = self.agent_pane_screen_mut(pane_id).ok_or_else(|| {
                        MezError::invalid_state(
                            "agent terminal presentation screen was not initialized",
                        )
                    })?;
                    Self::feed_agent_terminal_screen(
                        screen,
                        ansi_text.as_bytes(),
                        "replaying persisted agent presentation",
                    )?;
                    if !entry.copy_lines.is_empty() {
                        screen
                            .set_recent_normal_copy_texts(&entry.copy_lines, AGENT_COPY_SKIP_LINE);
                    }
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
                .or_insert_with(default_runtime_agent_prompt_input);
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
        if !entries.iter().any(|entry| entry.source_text.is_some()) {
            return Ok(false);
        }
        let previous = self.agent_pane_screen(pane_id).cloned();
        let rebuilt = TerminalScreen::new_with_history_config(
            size,
            self.terminal_history_limit(),
            self.terminal_history_rotate_lines(),
        )?;
        self.set_agent_pane_screen(pane_id.to_string(), session_id.clone(), rebuilt);
        if let Err(error) =
            self.replay_agent_presentation_entries_to_terminal_buffer(pane_id, &entries)
        {
            if let Some(previous) = previous {
                self.set_agent_pane_screen(pane_id.to_string(), session_id.clone(), previous);
            } else {
                self.remove_agent_pane_screen(pane_id);
            }
            return Err(error);
        }
        self.presentation
            .agent_presentation_projection_cache
            .insert(pane_id.to_string(), (session_id, size));
        Ok(true)
    }

    /// Appends markdown assistant output as styled presentation lines.
    fn append_agent_assistant_markdown_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        markdown: &str,
        content_type: &str,
    ) -> Result<()> {
        let frame_width = self.agent_terminal_markdown_frame_width(pane_id)?;
        let table_width = self.agent_terminal_markdown_terminal_width(pane_id)?;
        let body_rendered_lines = wrap_rich_text_lines_to_width(
            render_agent_markdown_body_lines(
                markdown,
                &self.presentation.settings.ui_theme,
                table_width,
            ),
            frame_width,
            table_width,
        );
        let body_rendered_count = body_rendered_lines.len();
        let rendered_lines = frame_markdown_lines(body_rendered_lines, frame_width);
        let trimmed_markdown = markdown.trim_end_matches(['\r', '\n']);
        let raw_copy_lines = if trimmed_markdown.is_empty() {
            vec![String::new()]
        } else {
            trimmed_markdown
                .split('\n')
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        let copy_lines = markdown_block_copy_lines(
            rendered_lines.as_slice(),
            body_rendered_count,
            raw_copy_lines,
            AGENT_TERMINAL_MESSAGE_PREFIX,
        );
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane_id,
            AgentTerminalPresentationStyle::Assistant,
            rendered_lines.as_slice(),
            &copy_lines,
            Some((markdown, content_type)),
        )
    }

    /// Runs the append agent status text to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn append_agent_status_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        text: &str,
    ) -> Result<()> {
        let lines = text
            .trim_end_matches(['\r', '\n'])
            .lines()
            .map(sanitized_agent_terminal_line)
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>();
        self.append_agent_terminal_lines_to_buffer(
            pane_id,
            &lines,
            AgentTerminalPresentationStyle::Status,
        )
    }

    /// Appends one bounded sandbox-mapping warning to the retained agent log.
    ///
    /// Warning identities are deduplicated for the lifetime of the active pane
    /// environment. The warning is visible regardless of verbose-mode state.
    pub(crate) fn append_sandbox_mapping_warning_once(
        &mut self,
        pane_id: &str,
        warning_id: &str,
        detail: &str,
    ) -> Result<()> {
        let identity = format!(
            "{pane_id}\0{}\0{warning_id}",
            self.session.config_generation
        );
        if !self
            .process
            .sandbox_mapping_warnings_emitted
            .insert(identity)
        {
            return Ok(());
        }
        let detail = detail
            .chars()
            .filter(|character| !character.is_control())
            .take(512)
            .collect::<String>();
        self.append_agent_status_text_to_terminal_buffer(
            pane_id,
            &format!(
                "agent warning: sandbox omitted unavailable host mapping: {detail}. The sandbox remains active with reduced access."
            ),
        )
    }

    /// Runs the append agent verbose status text to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn append_agent_verbose_status_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        text: &str,
    ) -> Result<()> {
        if self.agent_verbose_enabled(pane_id) {
            self.append_agent_status_text_to_terminal_buffer(pane_id, text)?;
        }
        Ok(())
    }

    /// Appends transient PTY diagnostics without granting terminal controls
    /// authority over the retained agent transcript surface.
    pub(crate) fn append_agent_pty_diagnostic_bytes_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        bytes: &[u8],
    ) -> Result<()> {
        let lines = String::from_utf8_lossy(bytes)
            .trim_end_matches(['\r', '\n'])
            .lines()
            .map(sanitized_agent_terminal_line)
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>();
        if lines.is_empty() {
            return Ok(());
        }
        self.ensure_current_agent_presentation_screen(pane_id)?;
        self.retire_agent_streaming_say_before_pane_write(pane_id)?;
        self.clear_agent_shell_output_status_line(pane_id)?;
        let ui_theme = self.presentation.settings.ui_theme.clone();
        let screen = self.agent_pane_screen_mut(pane_id).ok_or_else(|| {
            MezError::invalid_state("agent terminal presentation screen was not initialized")
        })?;
        let mut rendered = String::new();
        let cursor = screen.cursor_state();
        let current_line_has_content = screen
            .visible_lines()
            .get(cursor.row)
            .is_some_and(|line| !line.trim().is_empty());
        if cursor.column == 0 && !current_line_has_content {
            rendered.push('\r');
        } else {
            rendered.push_str("\r\n");
        }
        for line in lines {
            append_styled_agent_terminal_line(
                &mut rendered,
                AgentTerminalPresentationStyle::Status,
                &line,
                &ui_theme,
            );
            rendered.push_str("\x1b[0m\r\n");
        }
        Self::feed_agent_terminal_screen(
            screen,
            rendered.as_bytes(),
            "appending transient agent PTY diagnostics",
        )
    }

    /// Runs the append agent thinking text to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn append_agent_thinking_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        text: &str,
    ) -> Result<()> {
        if self.agent_thinking_enabled(pane_id) {
            let columns = self.agent_terminal_presentation_columns(pane_id)?;
            let rendition = agent_terminal_label_rendition(
                AgentTerminalPresentationStyle::Status,
                &self.presentation.settings.ui_theme,
            );
            let rendered_lines = agent_thinking_display_lines_for_width(text, columns)
                .into_iter()
                .map(|display| {
                    let length = UnicodeWidthStr::width(display.as_str());
                    RichTextLine {
                        display,
                        style_spans: vec![TerminalStyleSpan {
                            start: 0,
                            length,
                            rendition,
                        }],
                        copy_text: None,
                        kind: mez_mux::render::RichTextLineKind::Normal,
                    }
                })
                .collect::<Vec<_>>();
            self.append_agent_terminal_rendered_lines_to_buffer(
                pane_id,
                AgentTerminalPresentationStyle::Status,
                &rendered_lines,
                &[],
                Some((text, AGENT_PRESENTATION_THINKING_CONTENT_TYPE)),
            )?;
        }
        Ok(())
    }

    /// Appends one structured macro lifecycle transition in the parent pane.
    pub(crate) fn append_agent_macro_status_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        macro_name: &str,
        step_index: Option<usize>,
        total_steps: usize,
        status: &str,
    ) -> Result<()> {
        let columns = self.agent_terminal_presentation_columns(pane_id)?;
        let rendered_lines = agent_macro_lifecycle_display_lines_for_width(
            macro_name,
            step_index,
            total_steps,
            status,
            columns,
        )
        .into_iter()
        .map(|display| RichTextLine {
            display,
            style_spans: Vec::new(),
            copy_text: None,
            kind: mez_mux::render::RichTextLineKind::Normal,
        })
        .collect::<Vec<_>>();
        let source = serde_json::to_string(&(macro_name, step_index, total_steps, status, false))
            .map_err(|error| {
            MezError::invalid_state(format!("macro lifecycle source encoding failed: {error}"))
        })?;
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane_id,
            AgentTerminalPresentationStyle::Status,
            &rendered_lines,
            &[],
            Some((&source, AGENT_PRESENTATION_MACRO_LIFECYCLE_CONTENT_TYPE)),
        )
    }

    /// Appends one failed macro lifecycle transition in the parent pane.
    pub(crate) fn append_agent_macro_error_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        macro_name: &str,
        step_index: usize,
        total_steps: usize,
        status: &str,
    ) -> Result<()> {
        let columns = self.agent_terminal_presentation_columns(pane_id)?;
        let rendered_lines = agent_macro_lifecycle_display_lines_for_width(
            macro_name,
            Some(step_index),
            total_steps,
            status,
            columns,
        )
        .into_iter()
        .map(|display| RichTextLine {
            display,
            style_spans: Vec::new(),
            copy_text: None,
            kind: mez_mux::render::RichTextLineKind::Normal,
        })
        .collect::<Vec<_>>();
        let source =
            serde_json::to_string(&(macro_name, Some(step_index), total_steps, status, true))
                .map_err(|error| {
                    MezError::invalid_state(format!(
                        "macro lifecycle source encoding failed: {error}"
                    ))
                })?;
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane_id,
            AgentTerminalPresentationStyle::Error,
            &rendered_lines,
            &[],
            Some((&source, AGENT_PRESENTATION_MACRO_LIFECYCLE_CONTENT_TYPE)),
        )
    }

    /// Runs the append agent error text to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn append_agent_error_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        text: &str,
    ) -> Result<()> {
        let lines = text
            .trim_end_matches(['\r', '\n'])
            .lines()
            .map(sanitized_agent_terminal_line)
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>();
        self.append_agent_terminal_lines_to_buffer(
            pane_id,
            &lines,
            AgentTerminalPresentationStyle::Error,
        )
    }

    /// Runs the append agent command preview to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn append_agent_command_preview_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        command: &str,
    ) -> Result<()> {
        self.append_agent_command_preview_source_to_terminal_buffer(pane_id, command, false)
    }

    /// Appends one bounded command source with replay-supplied omission state.
    fn append_agent_command_preview_source_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        command: &str,
        source_was_truncated: bool,
    ) -> Result<()> {
        /// Defines the MAX AGENT COMMAND PREVIEW LINES const used by this subsystem.
        ///
        /// Keeping this value documented makes the contract explicit at the module
        /// boundary and avoids relying on call-site inference.
        const MAX_AGENT_COMMAND_PREVIEW_LINES: usize = 10;
        let columns = self
            .agent_pane_screen(pane_id)
            .map(|screen| usize::from(screen.size().columns))
            .or_else(|| {
                self.find_pane_descriptor(pane_id)
                    .map(|descriptor| usize::from(descriptor.size.columns))
            })
            .unwrap_or(80);
        let display_columns = bounded_agent_terminal_presentation_columns(columns);
        let prefix_width =
            UnicodeWidthStr::width(AGENT_TERMINAL_MESSAGE_PREFIX) + UnicodeWidthStr::width("$ ");
        let content_columns = display_columns.saturating_sub(prefix_width).max(1);
        let mut source = bounded_command_preview_source(command);
        source.truncated |= source_was_truncated;
        let rendered_lines = command_preview_terminal_rendered_lines(
            &source.text,
            source.truncated,
            content_columns,
            MAX_AGENT_COMMAND_PREVIEW_LINES,
            self.shell_classification_for_pane(pane_id),
            &self.presentation.settings.ui_theme,
        );
        let copy_lines = rendered_lines
            .iter()
            .map(|line| line.display.clone())
            .collect::<Vec<_>>();
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane_id,
            AgentTerminalPresentationStyle::Command,
            &rendered_lines,
            &copy_lines,
            Some((
                &source.text,
                if source.truncated {
                    AGENT_PRESENTATION_TRUNCATED_COMMAND_PREVIEW_CONTENT_TYPE
                } else {
                    AGENT_PRESENTATION_COMMAND_PREVIEW_CONTENT_TYPE
                },
            )),
        )
    }

    /// Runs the append agent terminal lines to buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn append_agent_terminal_lines_to_buffer(
        &mut self,
        pane_id: &str,
        lines: &[String],
        style: AgentTerminalPresentationStyle,
    ) -> Result<()> {
        let styled_lines = lines
            .iter()
            .map(|line| (style, line.clone()))
            .collect::<Vec<_>>();
        self.append_agent_terminal_styled_lines_to_buffer(pane_id, &styled_lines)
    }

    /// Feeds agent-owned presentation bytes into a terminal screen.
    ///
    /// Agent presentation content is model-authored, so terminal rendering must
    /// contain parser defects to the presentation batch instead of allowing a
    /// panic to cross the runtime state boundary.
    ///
    /// # Parameters
    /// - `screen`: The pane screen receiving rendered bytes.
    /// - `bytes`: The already-sanitized terminal bytes to feed.
    /// - `context`: A short description of the presentation operation.
    fn feed_agent_terminal_screen(
        screen: &mut TerminalScreen,
        bytes: &[u8],
        context: &str,
    ) -> Result<()> {
        screen.set_wrap_continuation_prefix(AGENT_TERMINAL_MESSAGE_PREFIX);
        catch_agent_terminal_presentation_panic(context, || screen.feed(bytes))
    }

    /// Retires provisional provider output before an unrelated pane write.
    ///
    /// Streaming owns only the exact screen generation it installed. Ordinary
    /// status, prompt, and completion writes remove that provisional generation
    /// first so later worker projections cannot replace the unrelated output.
    fn retire_agent_streaming_say_before_pane_write(&mut self, pane_id: &str) -> Result<()> {
        if self
            .presentation
            .agent_streaming_say_presentations
            .contains_key(pane_id)
        {
            self.discard_agent_streaming_say_presentation(pane_id, None)?;
        }
        Ok(())
    }

    /// Appends agent terminal lines with per-line presentation styles.
    ///
    /// Diff previews need additions, deletions, headers, and context to carry
    /// different colors while still flowing through the same pane-buffer gutter
    /// logic as normal agent transcript entries.
    pub(crate) fn append_agent_terminal_styled_lines_to_buffer(
        &mut self,
        pane_id: &str,
        styled_lines: &[(AgentTerminalPresentationStyle, String)],
    ) -> Result<()> {
        if styled_lines.is_empty() {
            return Ok(());
        }
        self.ensure_current_agent_presentation_screen(pane_id)?;
        self.retire_agent_streaming_say_before_pane_write(pane_id)?;
        self.clear_agent_shell_output_status_line(pane_id)?;
        let ui_theme = self.presentation.settings.ui_theme.clone();
        let ansi_text = {
            let screen = self.agent_pane_screen_mut(pane_id).ok_or_else(|| {
                MezError::invalid_state("agent terminal presentation screen was not initialized")
            })?;
            let mut bytes = String::new();
            let cursor = screen.cursor_state();
            let current_line_has_content = screen
                .visible_lines()
                .get(cursor.row)
                .is_some_and(|line| !line.trim().is_empty());
            if cursor.column == 0 && !current_line_has_content {
                bytes.push('\r');
            } else {
                bytes.push_str("\r\n");
            }
            for (style, line) in styled_lines {
                append_styled_agent_terminal_line(&mut bytes, *style, line, &ui_theme);
                bytes.push_str("\x1b[0m\r\n");
            }
            Self::feed_agent_terminal_screen(
                screen,
                bytes.as_bytes(),
                "appending styled agent lines",
            )?;
            bytes
        };
        self.persist_agent_presentation_entry(
            pane_id,
            styled_lines
                .iter()
                .map(|(style, _line)| style.persistence_name().to_string())
                .collect(),
            styled_lines
                .iter()
                .map(|(_style, line)| line.clone())
                .collect(),
            styled_lines
                .iter()
                .map(|(_style, line)| line.clone())
                .collect(),
            ansi_text,
            serde_json::to_string(
                &styled_lines
                    .iter()
                    .map(|(style, line)| (style.persistence_name(), line))
                    .collect::<Vec<_>>(),
            )
            .ok()
            .as_deref()
            .map(|source| (source, AGENT_PRESENTATION_STYLED_LINES_CONTENT_TYPE)),
        );
        Ok(())
    }

    /// Appends transformed assistant display lines while preserving raw copy text.
    fn append_agent_terminal_rendered_lines_to_buffer(
        &mut self,
        pane_id: &str,
        style: AgentTerminalPresentationStyle,
        rendered_lines: &[RichTextLine],
        copy_lines: &[String],
        source: Option<(&str, &str)>,
    ) -> Result<()> {
        if rendered_lines.is_empty() {
            return Ok(());
        }
        self.ensure_current_agent_presentation_screen(pane_id)?;
        self.retire_agent_streaming_say_before_pane_write(pane_id)?;
        self.clear_agent_shell_output_status_line(pane_id)?;
        let ui_theme = self.presentation.settings.ui_theme.clone();
        let ansi_text = {
            let screen = self.agent_pane_screen_mut(pane_id).ok_or_else(|| {
                MezError::invalid_state("agent terminal presentation screen was not initialized")
            })?;
            let mut bytes = String::new();
            let cursor = screen.cursor_state();
            let current_line_has_content = screen
                .visible_lines()
                .get(cursor.row)
                .is_some_and(|line| !line.trim().is_empty());
            if cursor.column == 0 && !current_line_has_content {
                bytes.push('\r');
            } else {
                bytes.push_str("\r\n");
            }
            for line in rendered_lines {
                append_styled_agent_terminal_rendered_line(&mut bytes, style, line, &ui_theme);
                bytes.push_str("\x1b[0m\r\n");
            }
            Self::feed_agent_terminal_screen(
                screen,
                bytes.as_bytes(),
                "appending rendered agent lines",
            )?;
            screen.set_recent_normal_copy_texts(copy_lines, AGENT_COPY_SKIP_LINE);
            bytes
        };
        self.persist_agent_presentation_entry(
            pane_id,
            vec![style.persistence_name().to_string(); rendered_lines.len()],
            rendered_lines
                .iter()
                .map(|line| line.display.clone())
                .collect(),
            copy_lines.to_vec(),
            ansi_text,
            source,
        );
        Ok(())
    }

    /// Applies one ordered provider `say` event to source-backed pane state.
    ///
    /// Source stays actor-owned while cumulative snapshots are rendered against
    /// a private baseline clone and replace the visible screen atomically.
    pub(crate) fn apply_agent_streaming_say_event_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        turn_id: &str,
        event: &mez_agent::StreamingSayEvent,
    ) -> Result<()> {
        match event {
            mez_agent::StreamingSayEvent::ResponseStarted { response_index } => {
                self.ensure_current_agent_presentation_screen(pane_id)?;
                self.clear_agent_shell_output_status_line(pane_id)?;
                let conversation_id = self
                    .agent_shell_store()
                    .get(pane_id)
                    .map(|session| session.session_id.clone())
                    .ok_or_else(|| {
                        MezError::invalid_state(
                            "streaming response presentation has no active conversation",
                        )
                    })?;
                let stale = self
                    .presentation
                    .agent_streaming_say_presentations
                    .get(pane_id)
                    .is_some_and(|presentation| {
                        presentation.turn_id != turn_id
                            || presentation.conversation_id != conversation_id
                            || presentation.response_index < *response_index
                    });
                if stale {
                    self.discard_agent_streaming_say_presentation(pane_id, None)?;
                }
                if self
                    .presentation
                    .agent_streaming_say_presentations
                    .get(pane_id)
                    .is_some_and(|presentation| presentation.response_index >= *response_index)
                {
                    return Ok(());
                }
                let baseline_screen =
                    self.agent_pane_screen(pane_id).cloned().ok_or_else(|| {
                        MezError::invalid_state(
                            "streaming response presentation screen was not initialized",
                        )
                    })?;
                let installed_screen = std::sync::Arc::new(baseline_screen.clone());
                self.presentation
                    .agent_promoted_streaming_say_actions
                    .remove(&(pane_id.to_string(), turn_id.to_string()));
                self.presentation.agent_streaming_say_presentations.insert(
                    pane_id.to_string(),
                    RuntimeStreamingSayPresentation {
                        turn_id: turn_id.to_string(),
                        response_index: *response_index,
                        conversation_id,
                        baseline_screen: std::sync::Arc::new(baseline_screen),
                        installed_screen,
                        rationale: None,
                        actions: std::collections::BTreeMap::new(),
                        shell_commands: std::collections::BTreeMap::new(),
                        revision: 1,
                        projected_revision: None,
                        projected_context: None,
                        projected_actions: None,
                        projected_rationale: None,
                        projected_screen: None,
                    },
                );
            }
            mez_agent::StreamingSayEvent::Started {
                action_index,
                status,
                content_type,
            } => {
                self.ensure_current_agent_presentation_screen(pane_id)?;
                self.clear_agent_shell_output_status_line(pane_id)?;
                let conversation_id = self
                    .agent_shell_store()
                    .get(pane_id)
                    .map(|session| session.session_id.clone())
                    .ok_or_else(|| {
                        MezError::invalid_state(
                            "streaming say presentation has no active conversation",
                        )
                    })?;
                let replace = self
                    .presentation
                    .agent_streaming_say_presentations
                    .get(pane_id)
                    .is_some_and(|presentation| {
                        presentation.turn_id != turn_id
                            || presentation.conversation_id != conversation_id
                    });
                if replace {
                    self.discard_agent_streaming_say_presentation(pane_id, None)?;
                }
                if !self
                    .presentation
                    .agent_streaming_say_presentations
                    .contains_key(pane_id)
                {
                    let baseline_screen =
                        self.agent_pane_screen(pane_id).cloned().ok_or_else(|| {
                            MezError::invalid_state(
                                "streaming say presentation screen was not initialized",
                            )
                        })?;
                    self.presentation
                        .agent_promoted_streaming_say_actions
                        .remove(&(pane_id.to_string(), turn_id.to_string()));
                    self.presentation.agent_streaming_say_presentations.insert(
                        pane_id.to_string(),
                        RuntimeStreamingSayPresentation {
                            turn_id: turn_id.to_string(),
                            response_index: 0,
                            conversation_id,
                            baseline_screen: std::sync::Arc::new(baseline_screen),
                            installed_screen: std::sync::Arc::new(
                                self.agent_pane_screen(pane_id).cloned().ok_or_else(|| {
                                    MezError::invalid_state(
                                        "streaming say presentation screen was not initialized",
                                    )
                                })?,
                            ),
                            rationale: None,
                            actions: std::collections::BTreeMap::new(),
                            shell_commands: std::collections::BTreeMap::new(),
                            revision: 1,
                            projected_revision: None,
                            projected_context: None,
                            projected_actions: None,
                            projected_rationale: None,
                            projected_screen: None,
                        },
                    );
                }
                let presentation = self
                    .presentation
                    .agent_streaming_say_presentations
                    .get_mut(pane_id)
                    .ok_or_else(|| {
                        MezError::invalid_state("streaming say presentation state is unavailable")
                    })?;
                presentation
                    .actions
                    .entry(*action_index)
                    .or_insert_with(|| RuntimeStreamingSayAction {
                        status: *status,
                        content_type: content_type.clone(),
                        text: String::new(),
                        complete: false,
                    });
                presentation.revision = presentation.revision.wrapping_add(1);
                presentation.projected_revision = None;
                self.append_agent_streaming_say_started(pane_id)?;
            }
            mez_agent::StreamingSayEvent::TextDelta { action_index, text } => {
                let action_exists = self
                    .presentation
                    .agent_streaming_say_presentations
                    .get(pane_id)
                    .filter(|presentation| presentation.turn_id == turn_id)
                    .is_some_and(|presentation| presentation.actions.contains_key(action_index));
                if !action_exists {
                    return Err(MezError::invalid_state(
                        "streaming say text arrived before its start event",
                    ));
                }
                let presentation = self
                    .presentation
                    .agent_streaming_say_presentations
                    .get_mut(pane_id)
                    .ok_or_else(|| {
                        MezError::invalid_state("streaming say presentation state disappeared")
                    })?;
                let action = presentation.actions.get_mut(action_index).ok_or_else(|| {
                    MezError::invalid_state(
                        "streaming say text state disappeared during presentation",
                    )
                })?;
                action.text.push_str(text);
                if !text.is_empty() {
                    presentation.revision = presentation.revision.wrapping_add(1);
                    presentation.projected_revision = None;
                }
            }
            mez_agent::StreamingSayEvent::TextComplete { action_index } => {
                let presentation = self
                    .presentation
                    .agent_streaming_say_presentations
                    .get_mut(pane_id)
                    .filter(|presentation| presentation.turn_id == turn_id)
                    .ok_or_else(|| {
                        MezError::invalid_state(
                            "streaming say completion arrived before its start event",
                        )
                    })?;
                let action = presentation.actions.get_mut(action_index).ok_or_else(|| {
                    MezError::invalid_state(
                        "streaming say completion arrived before its start event",
                    )
                })?;
                action.complete = true;
            }
            mez_agent::StreamingSayEvent::RationaleStarted => {
                self.ensure_agent_streaming_presentation(pane_id, turn_id)?;
                let presentation = self
                    .presentation
                    .agent_streaming_say_presentations
                    .get_mut(pane_id)
                    .ok_or_else(|| {
                        MezError::invalid_state("streaming rationale presentation is unavailable")
                    })?;
                presentation.rationale.get_or_insert_with(Default::default);
                presentation.revision = presentation.revision.wrapping_add(1);
                presentation.projected_revision = None;
            }
            mez_agent::StreamingSayEvent::RationaleTextDelta { text } => {
                let presentation = self
                    .presentation
                    .agent_streaming_say_presentations
                    .get_mut(pane_id)
                    .filter(|presentation| presentation.turn_id == turn_id)
                    .ok_or_else(|| {
                        MezError::invalid_state(
                            "streaming rationale arrived before its start event",
                        )
                    })?;
                let rationale = presentation.rationale.as_mut().ok_or_else(|| {
                    MezError::invalid_state("streaming rationale arrived before its start event")
                })?;
                rationale.text.push_str(text);
                if !text.is_empty() {
                    presentation.revision = presentation.revision.wrapping_add(1);
                    presentation.projected_revision = None;
                }
            }
            mez_agent::StreamingSayEvent::RationaleTextComplete => {
                let presentation = self
                    .presentation
                    .agent_streaming_say_presentations
                    .get_mut(pane_id)
                    .filter(|presentation| presentation.turn_id == turn_id)
                    .ok_or_else(|| {
                        MezError::invalid_state(
                            "streaming rationale completion arrived before its start event",
                        )
                    })?;
                let rationale = presentation.rationale.as_mut().ok_or_else(|| {
                    MezError::invalid_state(
                        "streaming rationale completion arrived before its start event",
                    )
                })?;
                rationale.complete = true;
            }
            mez_agent::StreamingSayEvent::ShellCommandStarted { action_index } => {
                self.ensure_agent_streaming_presentation(pane_id, turn_id)?;
                let presentation = self
                    .presentation
                    .agent_streaming_say_presentations
                    .get_mut(pane_id)
                    .ok_or_else(|| {
                        MezError::invalid_state("streaming command presentation is unavailable")
                    })?;
                presentation
                    .shell_commands
                    .entry(*action_index)
                    .or_insert_with(Default::default);
                presentation.revision = presentation.revision.wrapping_add(1);
                presentation.projected_revision = None;
                self.append_agent_streaming_plain_started(
                    pane_id,
                    AgentTerminalPresentationStyle::Command,
                    "$ ",
                    "starting streaming command source",
                )?;
            }
            mez_agent::StreamingSayEvent::ShellCommandTextDelta { action_index, text } => {
                let exists = self
                    .presentation
                    .agent_streaming_say_presentations
                    .get(pane_id)
                    .filter(|presentation| presentation.turn_id == turn_id)
                    .is_some_and(|presentation| {
                        presentation.shell_commands.contains_key(action_index)
                    });
                if !exists {
                    return Err(MezError::invalid_state(
                        "streaming command arrived before its start event",
                    ));
                }
                let presentation = self
                    .presentation
                    .agent_streaming_say_presentations
                    .get_mut(pane_id)
                    .ok_or_else(|| {
                        MezError::invalid_state("streaming command presentation disappeared")
                    })?;
                let command = presentation
                    .shell_commands
                    .get_mut(action_index)
                    .ok_or_else(|| {
                        MezError::invalid_state("streaming command source disappeared")
                    })?;
                command.text.push_str(text);
                if !text.is_empty() {
                    presentation.revision = presentation.revision.wrapping_add(1);
                    presentation.projected_revision = None;
                }
            }
            mez_agent::StreamingSayEvent::ShellCommandTextComplete { action_index } => {
                let presentation = self
                    .presentation
                    .agent_streaming_say_presentations
                    .get_mut(pane_id)
                    .filter(|presentation| presentation.turn_id == turn_id)
                    .ok_or_else(|| {
                        MezError::invalid_state(
                            "streaming command completion arrived before its start event",
                        )
                    })?;
                let command = presentation
                    .shell_commands
                    .get_mut(action_index)
                    .ok_or_else(|| {
                        MezError::invalid_state(
                            "streaming command completion arrived before its start event",
                        )
                    })?;
                command.complete = true;
            }
        }
        Ok(())
    }

    /// Initializes response-scoped provisional presentation for any source kind.
    fn ensure_agent_streaming_presentation(&mut self, pane_id: &str, turn_id: &str) -> Result<()> {
        self.ensure_current_agent_presentation_screen(pane_id)?;
        self.clear_agent_shell_output_status_line(pane_id)?;
        let conversation_id = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.session_id.clone())
            .ok_or_else(|| {
                MezError::invalid_state("streaming presentation has no active conversation")
            })?;
        let replace = self
            .presentation
            .agent_streaming_say_presentations
            .get(pane_id)
            .is_some_and(|presentation| {
                presentation.turn_id != turn_id || presentation.conversation_id != conversation_id
            });
        if replace {
            self.discard_agent_streaming_say_presentation(pane_id, None)?;
        }
        if !self
            .presentation
            .agent_streaming_say_presentations
            .contains_key(pane_id)
        {
            let baseline_screen = self.agent_pane_screen(pane_id).cloned().ok_or_else(|| {
                MezError::invalid_state("streaming presentation screen was not initialized")
            })?;
            self.presentation
                .agent_promoted_streaming_say_actions
                .remove(&(pane_id.to_string(), turn_id.to_string()));
            self.presentation.agent_streaming_say_presentations.insert(
                pane_id.to_string(),
                RuntimeStreamingSayPresentation {
                    turn_id: turn_id.to_string(),
                    response_index: 0,
                    conversation_id,
                    baseline_screen: std::sync::Arc::new(baseline_screen),
                    installed_screen: std::sync::Arc::new(
                        self.agent_pane_screen(pane_id).cloned().ok_or_else(|| {
                            MezError::invalid_state(
                                "streaming presentation screen was not initialized",
                            )
                        })?,
                    ),
                    rationale: None,
                    actions: std::collections::BTreeMap::new(),
                    shell_commands: std::collections::BTreeMap::new(),
                    revision: 1,
                    projected_revision: None,
                    projected_context: None,
                    projected_actions: None,
                    projected_rationale: None,
                    projected_screen: None,
                },
            );
        }
        Ok(())
    }

    /// Installs one same-conversation streaming screen without disturbing the reader's viewport.
    fn update_agent_streaming_screen(
        &mut self,
        pane_id: &str,
        conversation_id: &str,
        screen: TerminalScreen,
    ) -> Result<()> {
        let installed_screen = std::sync::Arc::new(screen.clone());
        if self.update_agent_pane_screen_preserving_interaction(pane_id, conversation_id, screen) {
            if let Some(presentation) = self
                .presentation
                .agent_streaming_say_presentations
                .get_mut(pane_id)
                .filter(|presentation| presentation.conversation_id == conversation_id)
            {
                presentation.installed_screen = installed_screen;
            }
            return Ok(());
        }
        Err(MezError::invalid_state(
            "streaming presentation screen conversation changed",
        ))
    }

    /// Appends one existing styled prefix for a provisional plain-text source.
    fn append_agent_streaming_plain_started(
        &mut self,
        pane_id: &str,
        style: AgentTerminalPresentationStyle,
        prefix: &str,
        context: &str,
    ) -> Result<()> {
        let presentation = self
            .presentation
            .agent_streaming_say_presentations
            .get(pane_id)
            .ok_or_else(|| MezError::invalid_state("streaming presentation is unavailable"))?;
        let conversation_id = presentation.conversation_id.clone();
        let mut candidate = self.agent_pane_screen(pane_id).cloned().ok_or_else(|| {
            MezError::invalid_state("streaming presentation screen was not initialized")
        })?;
        let cursor = candidate.cursor_state();
        let current_line_has_content = candidate
            .visible_lines()
            .get(cursor.row)
            .is_some_and(|line| !line.trim().is_empty());
        let mut bytes = if cursor.column == 0 && !current_line_has_content {
            "\r".to_string()
        } else {
            "\r\n".to_string()
        };
        append_styled_agent_terminal_line(
            &mut bytes,
            style,
            prefix,
            &self.presentation.settings.ui_theme,
        );
        Self::feed_agent_terminal_screen(&mut candidate, bytes.as_bytes(), context)?;
        self.update_agent_streaming_screen(pane_id, &conversation_id, candidate)
    }

    /// Atomically appends the literal assistant label for a newly started action.
    fn append_agent_streaming_say_started(&mut self, pane_id: &str) -> Result<()> {
        let presentation = self
            .presentation
            .agent_streaming_say_presentations
            .get(pane_id)
            .ok_or_else(|| MezError::invalid_state("streaming say presentation is unavailable"))?;
        let conversation_id = presentation.conversation_id.clone();
        let mut candidate = self.agent_pane_screen(pane_id).cloned().ok_or_else(|| {
            MezError::invalid_state("streaming say presentation screen was not initialized")
        })?;
        let cursor = candidate.cursor_state();
        let current_line_has_content = candidate
            .visible_lines()
            .get(cursor.row)
            .is_some_and(|line| !line.trim().is_empty());
        let mut bytes = if cursor.column == 0 && !current_line_has_content {
            "\r".to_string()
        } else {
            "\r\n".to_string()
        };
        append_styled_agent_terminal_line(
            &mut bytes,
            AgentTerminalPresentationStyle::Assistant,
            "mez> ",
            &self.presentation.settings.ui_theme,
        );
        Self::feed_agent_terminal_screen(
            &mut candidate,
            bytes.as_bytes(),
            "starting streaming say literal source",
        )?;
        self.update_agent_streaming_screen(pane_id, &conversation_id, candidate)
    }

    /// Captures one immutable dirty generation for an external renderer.
    pub(crate) fn take_agent_streaming_say_projection_work(
        &self,
        pane_id: &str,
        turn_id: &str,
    ) -> Result<Option<crate::runtime::RuntimeStreamingSayProjectionWork>> {
        let Some(presentation) = self
            .presentation
            .agent_streaming_say_presentations
            .get(pane_id)
            .filter(|presentation| presentation.turn_id == turn_id)
        else {
            return Ok(None);
        };
        let has_source = presentation.rationale.is_some()
            || !presentation.actions.is_empty()
            || !presentation.shell_commands.is_empty();
        if !has_source {
            return Ok(None);
        }
        if self.agent_pane_screen(pane_id) != Some(presentation.installed_screen.as_ref()) {
            return Ok(None);
        }
        let projected_context = self.agent_streaming_say_projection_context(pane_id)?;
        if presentation.projected_revision == Some(presentation.revision)
            && presentation.projected_context.as_ref() == Some(&projected_context)
        {
            return Ok(None);
        }
        Ok(Some(crate::runtime::RuntimeStreamingSayProjectionWork {
            pane_id: pane_id.to_string(),
            turn_id: turn_id.to_string(),
            response_index: presentation.response_index,
            conversation_id: presentation.conversation_id.clone(),
            revision: presentation.revision,
            baseline_screen: presentation.baseline_screen.clone(),
            rationale: presentation.rationale.clone(),
            actions: presentation.actions.clone(),
            shell_commands: presentation.shell_commands.clone(),
            thinking_enabled: projected_context.thinking_enabled,
            shell_classification: projected_context.shell_classification,
            presentation_columns: projected_context.presentation_columns,
            frame_width: projected_context.frame_width,
            table_width: projected_context.table_width,
            ui_theme: projected_context.ui_theme,
            screen_size: projected_context.screen_size,
        }))
    }

    /// Captures every non-source input that determines a streaming projection.
    fn agent_streaming_say_projection_context(
        &self,
        pane_id: &str,
    ) -> Result<RuntimeStreamingSayProjectionContext> {
        let screen_size = self
            .agent_pane_screen(pane_id)
            .ok_or_else(|| {
                MezError::invalid_state("streaming say presentation screen is unavailable")
            })?
            .size();
        Ok(RuntimeStreamingSayProjectionContext {
            thinking_enabled: self.agent_thinking_enabled(pane_id),
            shell_classification: self.shell_classification_for_pane(pane_id),
            presentation_columns: self.agent_terminal_presentation_columns(pane_id)?,
            frame_width: self.agent_terminal_markdown_frame_width(pane_id)?,
            table_width: self.agent_terminal_markdown_terminal_width(pane_id)?,
            ui_theme: self.presentation.settings.ui_theme.clone(),
            screen_size,
        })
    }

    /// Builds a complete private screen generation from immutable source.
    pub(crate) fn build_agent_streaming_say_projection(
        work: crate::runtime::RuntimeStreamingSayProjectionWork,
    ) -> Result<crate::runtime::RuntimeStreamingSayProjectionResult> {
        let say_projections = work
            .actions
            .iter()
            .map(|(action_index, action)| {
                (
                    *action_index,
                    Self::streaming_say_projection_with_theme(
                        action,
                        work.frame_width,
                        work.table_width,
                        &work.ui_theme,
                    ),
                )
            })
            .collect::<Vec<_>>();
        let rationale_projection = work.rationale.as_ref().and_then(|source| {
            work.thinking_enabled.then(|| {
                let rendition = agent_terminal_label_rendition(
                    AgentTerminalPresentationStyle::Status,
                    &work.ui_theme,
                );
                StreamingSayProjection {
                    style: AgentTerminalPresentationStyle::Status,
                    rendered_lines: agent_thinking_display_lines_for_width(
                        &source.text,
                        work.presentation_columns,
                    )
                    .into_iter()
                    .map(|display| {
                        let length = UnicodeWidthStr::width(display.as_str());
                        RichTextLine {
                            display,
                            style_spans: vec![TerminalStyleSpan {
                                start: 0,
                                length,
                                rendition,
                            }],
                            copy_text: None,
                            kind: mez_mux::render::RichTextLineKind::Normal,
                        }
                    })
                    .collect(),
                    copy_lines: Vec::new(),
                }
            })
        });
        let command_content_columns =
            bounded_agent_terminal_presentation_columns(usize::from(work.screen_size.columns))
                .saturating_sub(
                    UnicodeWidthStr::width(AGENT_TERMINAL_MESSAGE_PREFIX)
                        .saturating_add(UnicodeWidthStr::width("$ ")),
                )
                .max(1);
        let command_projections = work
            .shell_commands
            .iter()
            .map(|(action_index, source)| {
                let bounded = bounded_command_preview_source(&source.text);
                let rendered_lines = command_preview_terminal_rendered_lines(
                    &bounded.text,
                    bounded.truncated,
                    command_content_columns,
                    10,
                    work.shell_classification,
                    &work.ui_theme,
                );
                (
                    *action_index,
                    StreamingSayProjection {
                        style: AgentTerminalPresentationStyle::Command,
                        copy_lines: rendered_lines
                            .iter()
                            .map(|line| line.display.clone())
                            .collect(),
                        rendered_lines,
                    },
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut candidate = work.baseline_screen.as_ref().clone();
        let mut bytes = String::new();
        let cursor = candidate.cursor_state();
        let current_line_has_content = candidate
            .visible_lines()
            .get(cursor.row)
            .is_some_and(|line| !line.trim().is_empty());
        if cursor.column == 0 && !current_line_has_content {
            bytes.push('\r');
        } else {
            bytes.push_str("\r\n");
        }
        let mut first_line = true;
        if let Some(projection) = rationale_projection.as_ref() {
            for line in &projection.rendered_lines {
                if !first_line {
                    bytes.push_str("\r\n");
                }
                append_styled_agent_terminal_rendered_line(
                    &mut bytes,
                    projection.style,
                    line,
                    &work.ui_theme,
                );
                bytes.push_str("\x1b[0m");
                first_line = false;
            }
        }
        let mut projection_copy_lines = Vec::new();
        let mut has_projection_copy_lines = false;
        let action_indices = work
            .actions
            .keys()
            .chain(work.shell_commands.keys())
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        for action_index in action_indices {
            let projection = say_projections
                .iter()
                .find_map(|(candidate, projection)| {
                    (*candidate == action_index).then_some(projection)
                })
                .or_else(|| command_projections.get(&action_index));
            let Some(projection) = projection else {
                continue;
            };
            for line in &projection.rendered_lines {
                if !first_line {
                    bytes.push_str("\r\n");
                }
                append_styled_agent_terminal_rendered_line(
                    &mut bytes,
                    projection.style,
                    line,
                    &work.ui_theme,
                );
                bytes.push_str("\x1b[0m");
                first_line = false;
            }
            if projection.copy_lines.is_empty() {
                projection_copy_lines.extend(std::iter::repeat_n(
                    AGENT_COPY_SKIP_LINE.to_string(),
                    projection.rendered_lines.len(),
                ));
            } else {
                has_projection_copy_lines = true;
                projection_copy_lines.extend(projection.copy_lines.iter().cloned());
            }
        }
        Self::feed_agent_terminal_screen(
            &mut candidate,
            bytes.as_bytes(),
            "projecting streaming say output",
        )?;
        if has_projection_copy_lines {
            candidate.set_recent_normal_copy_texts(&projection_copy_lines, AGENT_COPY_SKIP_LINE);
        }
        let mut projected_actions = say_projections
            .iter()
            .map(|(action_index, projection)| {
                crate::runtime::render::RuntimeStreamingSayProjectedAction {
                    action_index: *action_index,
                    kind: crate::runtime::render::RuntimeStreamingSayProjectedActionKind::Say,
                    style: projection.style.persistence_name().to_string(),
                    rendered_lines: projection
                        .rendered_lines
                        .iter()
                        .map(|line| line.display.clone())
                        .collect(),
                    copy_lines: projection.copy_lines.clone(),
                }
            })
            .collect::<Vec<_>>();
        projected_actions.extend(command_projections.iter().map(
            |(action_index, projection)| {
                let truncated = work
                    .shell_commands
                    .get(action_index)
                    .is_some_and(|source| bounded_command_preview_source(&source.text).truncated);
                crate::runtime::render::RuntimeStreamingSayProjectedAction {
                    action_index: *action_index,
                    kind: crate::runtime::render::RuntimeStreamingSayProjectedActionKind::ShellCommand {
                        truncated,
                    },
                    style: projection.style.persistence_name().to_string(),
                    rendered_lines: projection
                        .rendered_lines
                        .iter()
                        .map(|line| line.display.clone())
                        .collect(),
                    copy_lines: projection.copy_lines.clone(),
                }
            },
        ));
        projected_actions.sort_by_key(|projection| projection.action_index);
        let projected_rationale = rationale_projection.as_ref().map(|projection| {
            crate::runtime::render::RuntimeStreamingSayProjectedRationale {
                style: projection.style.persistence_name().to_string(),
                rendered_lines: projection
                    .rendered_lines
                    .iter()
                    .map(|line| line.display.clone())
                    .collect(),
                copy_lines: projection.copy_lines.clone(),
            }
        });
        Ok(crate::runtime::RuntimeStreamingSayProjectionResult {
            pane_id: work.pane_id,
            turn_id: work.turn_id,
            response_index: work.response_index,
            conversation_id: work.conversation_id,
            revision: work.revision,
            thinking_enabled: work.thinking_enabled,
            shell_classification: work.shell_classification,
            presentation_columns: work.presentation_columns,
            frame_width: work.frame_width,
            table_width: work.table_width,
            ui_theme: work.ui_theme,
            screen_size: work.screen_size,
            projected_actions,
            projected_rationale,
            screen: candidate,
        })
    }

    /// Atomically installs one complete current projection generation.
    pub(crate) fn apply_agent_streaming_say_projection_result(
        &mut self,
        result: crate::runtime::RuntimeStreamingSayProjectionResult,
    ) -> Result<bool> {
        let current = self
            .presentation
            .agent_streaming_say_presentations
            .get(&result.pane_id)
            .is_some_and(|presentation| {
                presentation.turn_id == result.turn_id
                    && presentation.response_index == result.response_index
                    && presentation.conversation_id == result.conversation_id
                    && presentation.revision == result.revision
            });
        let screen_lineage_current = self
            .presentation
            .agent_streaming_say_presentations
            .get(&result.pane_id)
            .is_some_and(|presentation| {
                self.agent_pane_screen(&result.pane_id)
                    == Some(presentation.installed_screen.as_ref())
            });
        let conversation_current = self
            .agent_shell_store()
            .get(&result.pane_id)
            .is_some_and(|session| session.session_id == result.conversation_id);
        if !current
            || !screen_lineage_current
            || !conversation_current
            || self
                .agent_pane_screen(&result.pane_id)
                .is_none_or(|screen| screen.size() != result.screen_size)
            || self.agent_thinking_enabled(&result.pane_id) != result.thinking_enabled
            || self.shell_classification_for_pane(&result.pane_id) != result.shell_classification
            || self.agent_terminal_presentation_columns(&result.pane_id)?
                != result.presentation_columns
            || self.agent_terminal_markdown_frame_width(&result.pane_id)? != result.frame_width
            || self.agent_terminal_markdown_terminal_width(&result.pane_id)? != result.table_width
            || self.presentation.settings.ui_theme != result.ui_theme
        {
            return Ok(false);
        }
        let projected_screen = std::sync::Arc::new(result.screen.clone());
        self.update_agent_streaming_screen(
            &result.pane_id,
            &result.conversation_id,
            result.screen,
        )?;
        let presentation = self
            .presentation
            .agent_streaming_say_presentations
            .get_mut(&result.pane_id)
            .ok_or_else(|| MezError::invalid_state("streaming say presentation disappeared"))?;
        presentation.projected_revision = Some(result.revision);
        presentation.projected_context = Some(RuntimeStreamingSayProjectionContext {
            thinking_enabled: result.thinking_enabled,
            shell_classification: result.shell_classification,
            presentation_columns: result.presentation_columns,
            frame_width: result.frame_width,
            table_width: result.table_width,
            ui_theme: result.ui_theme,
            screen_size: result.screen_size,
        });
        presentation.projected_actions = Some(result.projected_actions);
        presentation.projected_rationale = result.projected_rationale;
        presentation.projected_screen = Some(projected_screen);
        Ok(true)
    }

    /// Builds one live or persisted projection through the ordinary say renderers.
    fn streaming_say_projection(
        &self,
        action: &RuntimeStreamingSayAction,
        frame_width: usize,
        table_width: usize,
    ) -> StreamingSayProjection {
        Self::streaming_say_projection_with_theme(
            action,
            frame_width,
            table_width,
            &self.presentation.settings.ui_theme,
        )
    }

    /// Builds one projection against an immutable worker-owned theme.
    fn streaming_say_projection_with_theme(
        action: &RuntimeStreamingSayAction,
        frame_width: usize,
        table_width: usize,
        ui_theme: &mez_mux::theme::UiTheme,
    ) -> StreamingSayProjection {
        if agent_output_content_type_is_markdown(&action.content_type)
            && !agent_say_text_is_displayed_patch_block(&action.text)
        {
            let body = wrap_rich_text_lines_to_width(
                render_agent_markdown_body_lines(&action.text, ui_theme, table_width),
                frame_width,
                table_width,
            );
            let body_count = body.len();
            let rendered_lines = frame_markdown_lines(body, frame_width);
            let raw_lines = if action.text.is_empty() {
                vec![String::new()]
            } else {
                action.text.split('\n').map(str::to_string).collect()
            };
            let copy_lines = markdown_block_copy_lines(
                &rendered_lines,
                body_count,
                raw_lines,
                AGENT_TERMINAL_MESSAGE_PREFIX,
            );
            return StreamingSayProjection {
                style: AgentTerminalPresentationStyle::Assistant,
                rendered_lines,
                copy_lines,
            };
        }
        if agent_output_content_type_is_diff(&action.content_type) {
            let rendered_lines = streaming_agent_diff_display_lines_for_width(
                &action.text,
                ui_theme,
                frame_width
                    .saturating_sub(UnicodeWidthStr::width("mez> "))
                    .max(1),
            );
            if rendered_lines.is_empty() {
                return StreamingSayProjection {
                    style: AgentTerminalPresentationStyle::Assistant,
                    rendered_lines: wrapped_prefixed_agent_terminal_lines(
                        "mez> ",
                        &action.text,
                        frame_width,
                    ),
                    copy_lines: Vec::new(),
                };
            }
            return StreamingSayProjection {
                style: AgentTerminalPresentationStyle::Assistant,
                rendered_lines: prefix_rich_text_lines(rendered_lines, "mez> ", "     "),
                copy_lines: Vec::new(),
            };
        }
        StreamingSayProjection {
            style: AgentTerminalPresentationStyle::Assistant,
            rendered_lines: wrapped_prefixed_agent_terminal_lines(
                "mez> ",
                &action.text,
                frame_width,
            ),
            copy_lines: Vec::new(),
        }
    }

    /// Retires one unvalidated live response and conditionally restores its baseline.
    ///
    /// The baseline is restored only while the pane still exactly matches the
    /// screen installed by this presentation. Any intervening pane mutation
    /// revokes streaming ownership and must survive retirement.
    pub(crate) fn discard_agent_streaming_say_presentation(
        &mut self,
        pane_id: &str,
        expected_turn_id: Option<&str>,
    ) -> Result<bool> {
        let Some(presentation) = self
            .presentation
            .agent_streaming_say_presentations
            .remove(pane_id)
        else {
            return Ok(false);
        };
        if expected_turn_id.is_some_and(|expected| expected != presentation.turn_id) {
            self.presentation
                .agent_streaming_say_presentations
                .insert(pane_id.to_string(), presentation);
            return Ok(false);
        }
        self.presentation
            .agent_promoted_streaming_say_actions
            .remove(&(pane_id.to_string(), presentation.turn_id.clone()));
        if self
            .agent_shell_store()
            .get(pane_id)
            .is_some_and(|session| session.session_id == presentation.conversation_id)
            && self.agent_pane_screen(pane_id) == Some(presentation.installed_screen.as_ref())
        {
            self.update_agent_streaming_screen(
                pane_id,
                &presentation.conversation_id,
                presentation.baseline_screen.as_ref().clone(),
            )?;
        }
        Ok(true)
    }

    /// Discards every live presentation owned by one provider turn.
    pub(crate) fn discard_agent_streaming_say_presentations_for_turn(
        &mut self,
        turn_id: &str,
    ) -> Result<usize> {
        let pane_ids = self
            .presentation
            .agent_streaming_say_presentations
            .iter()
            .filter(|(_pane_id, presentation)| presentation.turn_id == turn_id)
            .map(|(pane_id, _presentation)| pane_id.clone())
            .collect::<Vec<_>>();
        let mut discarded = 0usize;
        for pane_id in pane_ids {
            if self.discard_agent_streaming_say_presentation(&pane_id, Some(turn_id))? {
                discarded = discarded.saturating_add(1);
            }
        }
        Ok(discarded)
    }

    /// Discards every provisional provider-output presentation.
    pub(crate) fn discard_all_agent_streaming_say_presentations(&mut self) -> Result<usize> {
        let pane_ids = self
            .presentation
            .agent_streaming_say_presentations
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut discarded = 0usize;
        for pane_id in pane_ids {
            if self.discard_agent_streaming_say_presentation(&pane_id, None)? {
                discarded = discarded.saturating_add(1);
            }
        }
        Ok(discarded)
    }

    /// Reports whether completion already promoted one streamed action in place.
    pub(crate) fn agent_streaming_say_action_is_promoted(
        &self,
        pane_id: &str,
        turn_id: &str,
        action_index: usize,
    ) -> bool {
        self.presentation
            .agent_promoted_streaming_say_actions
            .get(&(pane_id.to_string(), turn_id.to_string()))
            .is_some_and(|indices| indices.contains(&action_index))
    }

    /// Reconciles live source with one validated provider execution.
    ///
    /// Every streamed action must be complete and exactly match its validated
    /// action index, status, normalized media type, and source text. A mismatch
    /// restores the pre-stream pane and lets ordinary completion presentation
    /// append the authoritative batch. Exact matches retain the current rows,
    /// persist their semantic source once, and mark the indices as presented.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "compatibility entry point used by focused tests")
    )]
    pub(crate) fn reconcile_agent_streaming_say_completion(
        &mut self,
        pane_id: &str,
        turn_id: &str,
        execution: &mez_agent::AgentTurnExecution,
    ) -> Result<std::collections::BTreeSet<usize>> {
        Ok(self
            .reconcile_agent_streaming_say_completion_with_render_intent(
                pane_id, turn_id, execution,
            )?
            .promoted_action_indices)
    }

    /// Reconciles live source and reports whether the installed screen survived.
    pub(crate) fn reconcile_agent_streaming_say_completion_with_render_intent(
        &mut self,
        pane_id: &str,
        turn_id: &str,
        execution: &mez_agent::AgentTurnExecution,
    ) -> Result<crate::runtime::render::RuntimeStreamingSayCompletionReconciliation> {
        let Some(presentation) = self
            .presentation
            .agent_streaming_say_presentations
            .remove(pane_id)
        else {
            return Ok(Default::default());
        };
        let conversation_matches = self
            .agent_shell_store()
            .get(pane_id)
            .is_some_and(|session| session.session_id == presentation.conversation_id);
        let screen_is_owned =
            self.agent_pane_screen(pane_id) == Some(presentation.installed_screen.as_ref());
        let batch = execution.response.action_batch.as_ref();
        let matches =
            presentation.turn_id == turn_id
                && conversation_matches
                && batch.is_some_and(|batch| {
                    presentation.rationale.as_ref().is_none_or(|streamed| {
                        streamed.complete && streamed.text == batch.rationale
                    }) && presentation.actions.iter().all(|(action_index, streamed)| {
                        let Some(mez_agent::AgentActionPayload::Say {
                            status,
                            text,
                            content_type,
                        }) = batch
                            .actions
                            .get(*action_index)
                            .map(|action| &action.payload)
                        else {
                            return false;
                        };
                        streamed.complete
                            && streamed.status == *status
                            && streamed.text == *text
                            && streamed.content_type
                                == mez_agent::normalize_agent_output_content_type(Some(
                                    content_type,
                                ))
                    }) && presentation
                        .shell_commands
                        .iter()
                        .all(|(action_index, streamed)| {
                            let Some(mez_agent::AgentActionPayload::ShellCommand {
                                command, ..
                            }) = batch
                                .actions
                                .get(*action_index)
                                .map(|action| &action.payload)
                            else {
                                return false;
                            };
                            streamed.complete && streamed.text == *command
                        })
                });
        if !matches {
            self.presentation
                .agent_promoted_streaming_say_actions
                .remove(&(pane_id.to_string(), turn_id.to_string()));
            if conversation_matches && screen_is_owned {
                self.update_agent_streaming_screen(
                    pane_id,
                    &presentation.conversation_id,
                    presentation.baseline_screen.as_ref().clone(),
                )?;
            }
            return Ok(Default::default());
        }

        let projection_context_is_current = self
            .agent_streaming_say_projection_context(pane_id)
            .is_ok_and(|context| presentation.projected_context.as_ref() == Some(&context));
        let current_projected_actions = presentation.projected_actions.as_ref().filter(|_| {
            presentation.projected_revision == Some(presentation.revision)
                && projection_context_is_current
                && presentation.projected_screen.as_deref() == self.agent_pane_screen(pane_id)
        });
        let command_can_promote = batch.is_some_and(|batch| {
            presentation.rationale.as_ref().is_some_and(|rationale| {
                rationale.complete && rationale.text == batch.rationale
            })
                && presentation.actions.is_empty()
                && presentation.shell_commands.len() == 1
                && !self.agent_verbose_enabled(pane_id)
                && self.pane_readiness_state(pane_id) == mez_agent::PaneReadinessState::Ready
                && batch.actions.len() == 1
                && execution.action_results.len() == 1
                && presentation.shell_commands.iter().all(|(action_index, source)| {
                    *action_index == 0
                        && source.complete
                        && !bounded_command_preview_source(&source.text).truncated
                        && batch.actions.get(*action_index).is_some_and(|action| {
                            action.rationale.trim().is_empty()
                                && matches!(
                                    &action.payload,
                                    mez_agent::AgentActionPayload::ShellCommand { command, .. }
                                        if command == &source.text
                                )
                        })
                        && execution.action_results.first().is_some_and(|result| {
                            result.action_id == batch.actions[*action_index].id
                                && result.status == mez_agent::ActionStatus::Running
                        })
                })
                && current_projected_actions.is_some_and(|projected| {
                    projected.len() == 1
                        && projected.first().is_some_and(|projection| {
                            projection.action_index == 0
                                && matches!(
                                    projection.kind,
                                    crate::runtime::render::RuntimeStreamingSayProjectedActionKind::ShellCommand {
                                        truncated: false
                                    }
                                )
                        })
                })
                && presentation.projected_context.as_ref().is_some_and(|context| {
                    !context.thinking_enabled || presentation.projected_rationale.is_some()
                })
        });

        // Thinking and non-promotable command rows have additional
        // completion-time ordering rules. Restore the shared baseline and let
        // the static pipeline settle every uncertain surface.
        if (presentation.rationale.is_some() || !presentation.shell_commands.is_empty())
            && !command_can_promote
        {
            self.presentation
                .agent_promoted_streaming_say_actions
                .remove(&(pane_id.to_string(), turn_id.to_string()));
            if screen_is_owned {
                self.update_agent_streaming_screen(
                    pane_id,
                    &presentation.conversation_id,
                    presentation.baseline_screen.as_ref().clone(),
                )?;
            }
            return Ok(Default::default());
        }

        let Some(projected_actions) = current_projected_actions else {
            if screen_is_owned {
                self.update_agent_streaming_screen(
                    pane_id,
                    &presentation.conversation_id,
                    presentation.baseline_screen.as_ref().clone(),
                )?;
            }
            return Ok(Default::default());
        };
        if let (Some(rationale), Some(projection)) = (
            presentation.rationale.as_ref(),
            presentation.projected_rationale.as_ref(),
        ) {
            self.persist_agent_presentation_entry(
                pane_id,
                vec![projection.style.clone(); projection.rendered_lines.len()],
                projection.rendered_lines.clone(),
                projection.copy_lines.clone(),
                String::new(),
                Some((
                    rationale.text.as_str(),
                    AGENT_PRESENTATION_THINKING_CONTENT_TYPE,
                )),
            );
        }
        let mut promoted = std::collections::BTreeSet::new();
        for projection in projected_actions {
            let source = match projection.kind {
                crate::runtime::render::RuntimeStreamingSayProjectedActionKind::Say => {
                    let Some(action) = presentation.actions.get(&projection.action_index) else {
                        continue;
                    };
                    Some((action.text.as_str(), action.content_type.as_str()))
                }
                crate::runtime::render::RuntimeStreamingSayProjectedActionKind::ShellCommand {
                    truncated,
                } => {
                    let Some(source) = presentation.shell_commands.get(&projection.action_index)
                    else {
                        continue;
                    };
                    Some((
                        source.text.as_str(),
                        if truncated {
                            AGENT_PRESENTATION_TRUNCATED_COMMAND_PREVIEW_CONTENT_TYPE
                        } else {
                            AGENT_PRESENTATION_COMMAND_PREVIEW_CONTENT_TYPE
                        },
                    ))
                }
            };
            self.persist_agent_presentation_entry(
                pane_id,
                vec![projection.style.clone(); projection.rendered_lines.len()],
                projection.rendered_lines.clone(),
                projection.copy_lines.clone(),
                String::new(),
                source,
            );
            promoted.insert(projection.action_index);
        }
        self.presentation
            .agent_promoted_streaming_say_actions
            .insert((pane_id.to_string(), turn_id.to_string()), promoted.clone());
        Ok(
            crate::runtime::render::RuntimeStreamingSayCompletionReconciliation {
                promoted_action_indices: promoted,
                preserved_installed_screen: true,
            },
        )
    }

    /// Clears validated-promotion bookkeeping after deferred presentation settles.
    pub(crate) fn clear_promoted_agent_streaming_say_actions(
        &mut self,
        pane_id: &str,
        turn_id: &str,
    ) {
        self.presentation
            .agent_promoted_streaming_say_actions
            .remove(&(pane_id.to_string(), turn_id.to_string()));
    }

    /// Updates the transient status rows for a hidden running shell command.
    ///
    /// The preview intentionally has no trailing newline after its final row.
    /// Later output replaces it in place, while the next durable agent
    /// transcript append clears it before writing normal log content.
    pub(crate) fn append_agent_shell_output_status_lines_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        lines: &[String],
    ) -> Result<()> {
        if self.agent_shell_view_enabled(pane_id) || lines.is_empty() {
            return Ok(());
        }
        self.ensure_current_agent_presentation_screen(pane_id)?;
        self.retire_agent_streaming_say_before_pane_write(pane_id)?;
        let columns = usize::from(
            self.agent_pane_screen(pane_id)
                .ok_or_else(|| {
                    MezError::invalid_state(
                        "agent terminal presentation screen was not initialized",
                    )
                })?
                .size()
                .columns,
        );
        let content_columns = columns
            .saturating_sub(UnicodeWidthStr::width(AGENT_TERMINAL_MESSAGE_PREFIX))
            .max(1);
        let lines = lines
            .iter()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                fit_agent_terminal_text_width(&sanitized_agent_terminal_line(line), content_columns)
            })
            .collect::<Vec<_>>();
        if lines.is_empty() {
            return Ok(());
        }
        let previous_line_count = self
            .presentation
            .agent_shell_output_status_lines
            .get(pane_id)
            .map(Vec::len)
            .unwrap_or(0);
        let ui_theme = self.presentation.settings.ui_theme.clone();
        let screen = self.agent_pane_screen_mut(pane_id).ok_or_else(|| {
            MezError::invalid_state("agent terminal presentation screen was not initialized")
        })?;
        let mut bytes = String::new();
        if previous_line_count > 0 {
            for index in 0..previous_line_count {
                if index > 0 {
                    bytes.push_str("\x1b[1A");
                }
                bytes.push_str("\r\x1b[2K");
            }
        } else {
            let cursor = screen.cursor_state();
            let current_line_has_content = screen
                .visible_lines()
                .get(cursor.row)
                .is_some_and(|line| !line.trim().is_empty());
            if cursor.column == 0 && !current_line_has_content {
                bytes.push('\r');
            } else {
                bytes.push_str("\r\n");
            }
        }
        for (index, line) in lines.iter().enumerate() {
            if index > 0 {
                bytes.push_str("\r\n");
            }
            append_styled_agent_terminal_line(
                &mut bytes,
                AgentTerminalPresentationStyle::Status,
                line,
                &ui_theme,
            );
            bytes.push_str("\x1b[0m");
        }
        Self::feed_agent_terminal_screen(screen, bytes.as_bytes(), "updating shell output status")?;
        self.presentation
            .agent_shell_output_status_lines
            .insert(pane_id.to_string(), lines);
        Ok(())
    }

    /// Clears transient shell-output status rows for one pane.
    pub(crate) fn clear_agent_shell_output_status_line(&mut self, pane_id: &str) -> Result<()> {
        let line_count = self
            .presentation
            .agent_shell_output_status_lines
            .remove(pane_id)
            .map_or(0, |lines| lines.len());
        if line_count == 0 {
            return Ok(());
        }
        if let Some(screen) = self.agent_pane_screen_mut(pane_id) {
            let mut bytes = String::new();
            for index in 0..line_count {
                if index > 0 {
                    bytes.push_str("\x1b[1A");
                }
                bytes.push_str("\r\x1b[2K");
            }
            Self::feed_agent_terminal_screen(
                screen,
                bytes.as_bytes(),
                "clearing shell output status",
            )?;
        }
        Ok(())
    }

    /// Appends model-authored action summary text as normal-mode thinking logs.
    pub(crate) fn append_agent_action_model_thinking_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        action: &AgentAction,
    ) -> Result<bool> {
        let thinking_lines = agent_action_model_thinking_lines(action);
        if thinking_lines.is_empty() {
            return Ok(false);
        }
        self.append_agent_thinking_text_to_terminal_buffer(pane_id, &thinking_lines.join("\n"))?;
        Ok(true)
    }

    /// Appends a sanitized mutating-action diff preview to the pane buffer.
    ///
    /// The source text is the cleaned shell observation captured from the hidden
    /// transaction, so this path never exposes shell prompts or Mezzanine wrapper
    /// traffic while still giving users a copyable summary of filesystem changes.
    pub(crate) fn append_agent_diff_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        text: &str,
    ) -> Result<()> {
        let display_width = self.agent_terminal_markdown_frame_width(pane_id)?;
        let rendered_lines = readable_agent_diff_display_lines_for_width(
            text,
            &self.presentation.settings.ui_theme,
            display_width,
        );
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane_id,
            AgentTerminalPresentationStyle::DiffContext,
            &rendered_lines,
            &[],
            Some((text, "text/x-diff; charset=utf-8")),
        )
    }

    /// Appends unbounded model-authored diff output as assistant presentation.
    fn append_agent_assistant_diff_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        text: &str,
        content_type: &str,
    ) -> Result<()> {
        let frame_width = self.agent_terminal_markdown_frame_width(pane_id)?;
        let table_width = self.agent_terminal_markdown_terminal_width(pane_id)?;
        let action = RuntimeStreamingSayAction {
            status: mez_agent::SayStatus::Final,
            content_type: content_type.to_string(),
            text: text.to_string(),
            complete: true,
        };
        let projection = self.streaming_say_projection(&action, frame_width, table_width);
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane_id,
            projection.style,
            &projection.rendered_lines,
            &projection.copy_lines,
            Some((text, content_type)),
        )
    }

    /// Records successful patch diffs for `/list-modified-files`.
    ///
    /// The source text is the same cleaned shell observation used for the
    /// normal diff preview, so counts are derived from the semantic patch diff
    /// rather than from shell echo or wrapper traffic.
    pub(crate) fn record_agent_modified_files_from_diff(&mut self, pane_id: &str, text: &str) {
        let source_lines = cleaned_agent_diff_source_lines(text);
        for section in parse_unified_diff_sections(&source_lines) {
            let path = diff_section_path(&section).to_string();
            if path.is_empty() || path == "/dev/null" {
                continue;
            }
            let added = section
                .lines
                .iter()
                .filter(|line| line.marker == '+')
                .count();
            let removed = section
                .lines
                .iter()
                .filter(|line| line.marker == '-')
                .count();
            self.record_agent_modified_file_delta(pane_id, path, added, removed);
        }
    }

    /// Appends a single human-readable action execution line to the pane.
    ///
    /// Semantic file/search and runtime URL actions should be legible in normal
    /// mode without dumping generated commands or result payloads. The line
    /// uses span-level styling so the action remains salient without forcing
    /// arguments to inherit the same visual weight.
    pub(crate) fn append_agent_action_execution_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        action: &AgentAction,
    ) -> Result<bool> {
        let Some(header) = agent_action_execution_display_header(action) else {
            return Ok(false);
        };
        self.append_agent_action_execution_header_to_terminal_buffer(pane_id, action, &header)?;
        Ok(true)
    }

    /// Appends one action execution row using a runtime-selected header.
    ///
    /// Multi-transaction actions use this entry point when the active
    /// transaction has a more precise display target than the model action.
    pub(crate) fn append_agent_action_execution_header_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        action: &AgentAction,
        header: &str,
    ) -> Result<()> {
        let thinking_lines = agent_action_model_thinking_lines(action);
        if !thinking_lines.is_empty() && self.agent_thinking_enabled(pane_id) {
            self.append_agent_thinking_text_to_terminal_buffer(
                pane_id,
                &thinking_lines.join("\n"),
            )?;
        }
        let rendered_line =
            agent_action_execution_rendered_line(header, &self.presentation.settings.ui_theme);
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane_id,
            AgentTerminalPresentationStyle::Status,
            &[rendered_line],
            &[],
            Some((header, AGENT_PRESENTATION_ACTION_HEADER_CONTENT_TYPE)),
        )?;
        Ok(())
    }

    /// Appends a bounded, human-readable action result preview to the pane.
    ///
    /// Normal mode uses this renderer for mutating semantic action diffs. Other
    /// result previews remain reserved for elevated log levels.
    pub(crate) fn append_agent_action_result_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        action: &AgentAction,
        result: &ActionResult,
        text: &str,
    ) -> Result<()> {
        if agent_action_result_uses_diff_preview(action) {
            return self.append_agent_diff_text_to_terminal_buffer(pane_id, text);
        }
        if result.is_error {
            return Ok(());
        }
        let Some(header) = agent_action_result_display_header(action) else {
            return Ok(());
        };
        let mut styled_lines = vec![(AgentTerminalPresentationStyle::Command, header)];
        styled_lines.extend(
            bounded_agent_action_result_display_lines(text)
                .into_iter()
                .map(|line| (AgentTerminalPresentationStyle::Status, line)),
        );
        self.append_agent_terminal_styled_lines_to_buffer(pane_id, &styled_lines)
    }

    /// Returns whether a cleaned action result preview should render in normal
    /// logging mode.
    pub(crate) fn agent_action_result_renders_in_normal_mode(&self, action: &AgentAction) -> bool {
        agent_action_result_uses_diff_preview(action)
    }

    /// Runs the agent verbose enabled operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn agent_verbose_enabled(&self, pane_id: &str) -> bool {
        self.agent_shell_store()
            .get(pane_id)
            .is_some_and(|session| session.log_level.shows_verbose_status())
    }

    /// Runs the agent thinking enabled operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn agent_thinking_enabled(&self, pane_id: &str) -> bool {
        self.agent_shell_store()
            .get(pane_id)
            .is_some_and(|session| session.log_level.shows_thinking())
    }

    /// Runs the agent debug enabled operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn agent_debug_enabled(&self, pane_id: &str) -> bool {
        self.agent_shell_store()
            .get(pane_id)
            .is_some_and(|session| session.log_level.shows_debug())
    }

    /// Runs the agent trace enabled operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn agent_trace_enabled(&self, pane_id: &str) -> bool {
        self.agent_shell_store()
            .get(pane_id)
            .is_some_and(|session| session.log_level.shows_trace())
    }

    /// Runs the agent shell view enabled operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn agent_shell_view_enabled(&self, pane_id: &str) -> bool {
        self.agent_shell_store()
            .get(pane_id)
            .is_some_and(|session| session.log_level.shows_shell_view())
    }

    /// Runs the agent diagnostic level name operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn agent_diagnostic_level_name(&self, pane_id: &str) -> Option<&'static str> {
        if self.agent_trace_enabled(pane_id) {
            Some("trace")
        } else if self.agent_debug_enabled(pane_id) {
            Some("debug")
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{catch_agent_terminal_presentation_panic, styled_agent_presentation_source_lines};

    /// Verifies typed styled presentation source preserves valid style and text
    /// pairs while rejecting malformed payloads before replay reaches a pane.
    #[test]
    fn styled_agent_presentation_source_lines_decodes_valid_typed_records() {
        let decoded = styled_agent_presentation_source_lines(
            r#"[["user-prompt","user> restore me"],["status","agent: restored"]]"#,
        )
        .expect("valid typed styled presentation source should decode");

        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].1, "user> restore me");
        assert_eq!(decoded[1].1, "agent: restored");
        assert!(styled_agent_presentation_source_lines("not json").is_none());
    }

    /// Verifies a contained terminal parser panic becomes a contextual runtime
    /// error rather than reporting the dropped presentation batch as success.
    #[test]
    fn contained_agent_terminal_presentation_panic_propagates_contextual_error() {
        let error = catch_agent_terminal_presentation_panic("testing panic propagation", || {
            panic!("controlled terminal parser panic");
        })
        .expect_err("contained presentation panic must return an error");

        assert!(
            error.message().contains(
                "agent terminal presentation feed panicked while testing panic propagation"
            ),
            "{error:?}"
        );
    }
}
