//! Runtime service application of projected agent presentation rows.

use super::actions::{
    agent_action_execution_display_header, agent_action_execution_rendered_line,
    agent_action_model_thinking_lines, agent_action_result_display_header,
    agent_macro_lifecycle_display_lines_for_width, agent_thinking_display_lines_for_width,
    bounded_agent_action_result_display_lines,
};
use super::diff::{
    agent_action_result_uses_diff_preview, cleaned_agent_diff_source_lines,
    readable_agent_diff_display_lines_for_width,
};
use super::style::{
    AGENT_PROMPT_TEXT_PREFIX, AGENT_TERMINAL_MESSAGE_PREFIX, AgentTerminalPresentationStyle,
};
use super::text::{
    agent_say_text_is_displayed_patch_block, append_styled_agent_terminal_line,
    append_styled_agent_terminal_rendered_line, bounded_agent_terminal_presentation_columns,
    command_preview_terminal_rendered_lines, fit_agent_terminal_text_width,
    render_agent_markdown_body_lines, sanitized_agent_terminal_line,
    wrapped_prefixed_agent_terminal_lines,
};
use super::{
    AGENT_COPY_SKIP_LINE, AgentAction, RichTextLine, UnicodeWidthStr, diff_section_path,
    frame_markdown_lines, parse_unified_diff_sections, wrap_rich_text_lines_to_width,
};
use crate::runtime::render::{
    ActionResult, AgentPresentationEntry, MezError, Result, RuntimeSessionService, Size,
    TerminalScreen, current_unix_seconds, default_runtime_agent_prompt_input,
};
use mez_agent::{
    AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE, agent_output_content_type_is_diff,
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
/// Content type for one action-execution header rendered at replay geometry.
const AGENT_PRESENTATION_ACTION_HEADER_CONTENT_TYPE: &str =
    "application/vnd.mezzanine.agent-presentation.action-header+text; charset=utf-8";
/// Content type for a parent-supplied subagent prompt rendered at replay geometry.
const AGENT_PRESENTATION_PARENT_PROMPT_CONTENT_TYPE: &str =
    "application/vnd.mezzanine.agent-presentation.parent-prompt+text; charset=utf-8";

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

impl RuntimeSessionService {
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
            return self.append_agent_diff_text_to_terminal_buffer(pane_id, text);
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
            .pane_screen(pane_id)
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
        self.pane_screen(pane_id)
            .map(|screen| screen.size().columns)
            .or_else(|| {
                self.find_pane_descriptor(pane_id)
                    .map(|descriptor| descriptor.size.columns)
            })
    }

    /// Persists one durable user-visible agent presentation entry.
    fn persist_agent_presentation_entry(
        &self,
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
        let Some(store) = self.persistence.transcript_store() else {
            return;
        };
        let Some(session) = self.agent_shell_store().get(pane_id) else {
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
                    if source_content_type == AGENT_PRESENTATION_COMMAND_PREVIEW_CONTENT_TYPE {
                        self.append_agent_command_preview_to_terminal_buffer(pane_id, source_text)?;
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
                    let descriptor = self.find_pane_descriptor(pane_id).ok_or_else(|| {
                        MezError::new(
                            crate::error::MezErrorKind::NotFound,
                            "agent terminal presentation target pane not found",
                        )
                    })?;
                    if self.pane_screen(pane_id).is_none() {
                        let screen = TerminalScreen::new_with_history_config(
                            descriptor.size,
                            self.terminal_history_limit(),
                            self.terminal_history_rotate_lines(),
                        )?;
                        self.set_pane_screen(pane_id.to_string(), screen);
                    }
                    self.clear_agent_shell_output_status_line(pane_id)?;
                    let screen = self.pane_screen_mut(pane_id).ok_or_else(|| {
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
                    && let Some(screen) = self.pane_screen_mut(pane_id)
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

    /// Rebuilds a resized agent pane from bounded durable presentation source.
    ///
    /// The rebuild is intentionally limited to histories that contain semantic
    /// source. Snapshot-only histories retain ordinary terminal resize behavior
    /// because their saved rows cannot reproduce renderer-level layout.
    pub(crate) fn rebuild_agent_presentation_after_resize(
        &mut self,
        pane_id: &str,
        size: Size,
    ) -> Result<bool> {
        const MAX_PRESENTATION_REPLAY_ENTRIES: usize = 200;

        let Some(session) = self.agent_shell_store().get(pane_id) else {
            return Ok(false);
        };
        let Some(store) = self.persistence.transcript_store() else {
            return Ok(false);
        };
        let entries = store.inspect_presentation_replay_tail(
            &session.session_id,
            MAX_PRESENTATION_REPLAY_ENTRIES,
        )?;
        if !entries.iter().any(|entry| entry.source_text.is_some()) {
            return Ok(false);
        }
        let previous = self.pane_screen(pane_id).cloned();
        let rebuilt = TerminalScreen::new_with_history_config(
            size,
            self.terminal_history_limit(),
            self.terminal_history_rotate_lines(),
        )?;
        self.set_pane_screen(pane_id.to_string(), rebuilt);
        if let Err(error) =
            self.replay_agent_presentation_entries_to_terminal_buffer(pane_id, &entries)
        {
            if let Some(previous) = previous {
                self.set_pane_screen(pane_id.to_string(), previous);
            }
            return Err(error);
        }
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
            self.append_agent_terminal_lines_to_buffer(
                pane_id,
                &agent_thinking_display_lines_for_width(text, columns),
                AgentTerminalPresentationStyle::Status,
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
        self.append_agent_terminal_lines_to_buffer(
            pane_id,
            &agent_macro_lifecycle_display_lines_for_width(
                macro_name,
                step_index,
                total_steps,
                status,
                columns,
            ),
            AgentTerminalPresentationStyle::Status,
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
        self.append_agent_terminal_lines_to_buffer(
            pane_id,
            &agent_macro_lifecycle_display_lines_for_width(
                macro_name,
                Some(step_index),
                total_steps,
                status,
                columns,
            ),
            AgentTerminalPresentationStyle::Error,
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
        /// Defines the MAX AGENT COMMAND PREVIEW LINES const used by this subsystem.
        ///
        /// Keeping this value documented makes the contract explicit at the module
        /// boundary and avoids relying on call-site inference.
        const MAX_AGENT_COMMAND_PREVIEW_LINES: usize = 10;
        let columns = self
            .pane_screen(pane_id)
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
        let rendered_lines = command_preview_terminal_rendered_lines(
            command,
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
            Some((command, AGENT_PRESENTATION_COMMAND_PREVIEW_CONTENT_TYPE)),
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
        _context: &str,
    ) -> Result<()> {
        screen.set_wrap_continuation_prefix(AGENT_TERMINAL_MESSAGE_PREFIX);
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| screen.feed(bytes))).is_err() {
            return Ok(());
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
        let descriptor = self.find_pane_descriptor(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "agent terminal presentation target pane not found",
            )
        })?;
        if self.pane_screen(pane_id).is_none() {
            let screen = TerminalScreen::new_with_history_config(
                descriptor.size,
                self.terminal_history_limit(),
                self.terminal_history_rotate_lines(),
            )?;
            self.set_pane_screen(pane_id.to_string(), screen);
        }
        self.clear_agent_shell_output_status_line(pane_id)?;
        let ui_theme = self.presentation.settings.ui_theme.clone();
        let ansi_text = {
            let screen = self.pane_screen_mut(pane_id).ok_or_else(|| {
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
        let descriptor = self.find_pane_descriptor(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "agent terminal presentation target pane not found",
            )
        })?;
        if self.pane_screen(pane_id).is_none() {
            let screen = TerminalScreen::new_with_history_config(
                descriptor.size,
                self.terminal_history_limit(),
                self.terminal_history_rotate_lines(),
            )?;
            self.set_pane_screen(pane_id.to_string(), screen);
        }
        self.clear_agent_shell_output_status_line(pane_id)?;
        let ui_theme = self.presentation.settings.ui_theme.clone();
        let ansi_text = {
            let screen = self.pane_screen_mut(pane_id).ok_or_else(|| {
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
        let descriptor = self.find_pane_descriptor(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "agent terminal presentation target pane not found",
            )
        })?;
        if self.pane_screen(pane_id).is_none() {
            let screen = TerminalScreen::new_with_history_config(
                descriptor.size,
                self.terminal_history_limit(),
                self.terminal_history_rotate_lines(),
            )?;
            self.set_pane_screen(pane_id.to_string(), screen);
        }
        let columns = self
            .pane_screen(pane_id)
            .map(|screen| usize::from(screen.size().columns))
            .unwrap_or_else(|| usize::from(descriptor.size.columns));
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
        let screen = self.pane_screen_mut(pane_id).ok_or_else(|| {
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

    /// Clears transient shell-output status rows if one is active for a pane.
    fn clear_agent_shell_output_status_line(&mut self, pane_id: &str) -> Result<()> {
        let Some(lines) = self
            .presentation
            .agent_shell_output_status_lines
            .remove(pane_id)
        else {
            return Ok(());
        };
        if let Some(screen) = self.pane_screen_mut(pane_id) {
            let mut bytes = String::new();
            for index in 0..lines.len() {
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
        let columns = self.agent_terminal_presentation_columns(pane_id)?;
        self.append_agent_terminal_lines_to_buffer(
            pane_id,
            &agent_thinking_display_lines_for_width(&thinking_lines.join("\n"), columns),
            AgentTerminalPresentationStyle::Status,
        )?;
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
            let columns = self.agent_terminal_presentation_columns(pane_id)?;
            self.append_agent_terminal_lines_to_buffer(
                pane_id,
                &agent_thinking_display_lines_for_width(&thinking_lines.join("\n"), columns),
                AgentTerminalPresentationStyle::Status,
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
    use super::styled_agent_presentation_source_lines;

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
}
