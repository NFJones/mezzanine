//! Runtime service mutation for overlays, record browsers, and selections.

use super::display_content::*;
use super::product_content::*;
use super::selection_adapter::*;
use crate::runtime::render::*;

impl RuntimeSessionService {
    /// Executes one command selected from the primary display overlay.
    pub(crate) fn execute_primary_display_overlay_selection_command(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        command: &str,
    ) -> Result<bool> {
        if command.trim_start().starts_with('/') {
            let pane_id = self.active_pane_id()?.to_string();
            let record_browser_stack =
                self.presentation
                    .primary_display_overlay
                    .as_ref()
                    .and_then(|overlay| {
                        let target_command = record_browser_command_name(command)?;
                        let record_browser = overlay.record_browser.as_ref()?;
                        let mut stack = record_browser.stack.clone();
                        stack.push(
                            crate::runtime::service_state::RuntimeRecordBrowserOverlayFrame {
                                command: record_browser.command.clone(),
                                source: record_browser.source.clone(),
                                browser: record_browser.browser.clone(),
                                scroll_offset: overlay.scroll_offset,
                                active_selection_index: overlay.active_selection_index,
                            },
                        );
                        Some((target_command, stack))
                    });
            self.presentation.primary_display_overlay = None;
            let body = self.execute_agent_shell_command(primary_client_id, command)?;
            if let Some((target_command, stack)) = record_browser_stack {
                self.presentation
                    .pending_record_browser_overlay_stacks
                    .insert((pane_id.clone(), target_command), stack);
            }
            let display_output =
                runtime_agent_shell_display_output(&body, &self.presentation.settings.ui_theme)?;
            self.set_agent_prompt_display_output(&pane_id, display_output)?;
            if runtime_agent_shell_visibility(&body).as_deref() == Some("hidden") {
                self.presentation.agent_prompt_inputs.remove(&pane_id);
            }
            return Ok(true);
        }
        self.presentation.primary_display_overlay = None;
        let content = self
            .execute_terminal_command(primary_client_id, command)
            .and_then(|body| {
                runtime_command_display_overlay_content(&body, &self.presentation.settings.ui_theme)
            })?;
        self.present_runtime_command_display_content(content)?;
        Ok(true)
    }

    /// Applies mouse-wheel scrolling to the primary display overlay.
    pub(crate) fn apply_primary_display_overlay_scroll(&mut self, lines: isize) -> Result<bool> {
        let Some(overlay) = self.presentation.primary_display_overlay.as_mut() else {
            return Ok(false);
        };
        Ok(apply_overlay_scroll_delta(
            overlay,
            lines,
            self.session.authoritative_size,
        ))
    }

    /// Applies one input chunk to a retained record-browser overlay, when one
    /// is active.
    fn apply_primary_record_browser_overlay_input(&mut self, input: &[u8]) -> Result<Option<bool>> {
        let display_width = usize::from(self.session.authoritative_size.columns)
            .min(self.presentation.settings.terminal_agent_wrap_column_cap)
            .max(1);
        let Some(overlay) = self.presentation.primary_display_overlay.as_ref() else {
            return Ok(Some(false));
        };
        let Some(record_browser) = overlay.record_browser.as_ref() else {
            return Ok(None);
        };
        if record_browser.browser.prompt().is_some() {
            return self
                .apply_primary_record_browser_prompt_input(input)
                .map(Some);
        }
        let Some(overlay) = self.presentation.primary_display_overlay.as_mut() else {
            return Ok(Some(false));
        };
        let Some(record_browser) = overlay.record_browser.as_mut() else {
            return Ok(None);
        };
        let action = match input {
            b"k" => Some(mez_mux::record_browser::RecordBrowserAction::StartFilter(
                mez_mux::record_browser::RecordBrowserFilterField::Kind,
            )),
            b"p" => Some(mez_mux::record_browser::RecordBrowserAction::StartFilter(
                mez_mux::record_browser::RecordBrowserFilterField::ProjectGlob,
            )),
            b"x" => Some(mez_mux::record_browser::RecordBrowserAction::StartFilter(
                mez_mux::record_browser::RecordBrowserFilterField::Text,
            )),
            b"s" => Some(mez_mux::record_browser::RecordBrowserAction::StartSave),
            _ if matches!(
                runtime_selector_input_action(input),
                RuntimeSelectorInputAction::Select
            ) =>
            {
                Some(mez_mux::record_browser::RecordBrowserAction::OpenActive)
            }
            _ if matches!(
                runtime_selector_input_action(input),
                RuntimeSelectorInputAction::Exit
            ) =>
            {
                if let Some(frame) = record_browser.stack.pop() {
                    record_browser.command = frame.command;
                    record_browser.source = frame.source;
                    record_browser.browser = frame.browser;
                    let scroll_offset = frame.scroll_offset;
                    let active_selection_index = frame.active_selection_index;
                    let changed = render_record_browser_overlay(
                        overlay,
                        &self.presentation.settings.ui_theme,
                        display_width,
                    );
                    overlay.scroll_offset = scroll_offset.min(modal_overlay_max_scroll(
                        overlay.lines.len(),
                        self.session.authoritative_size,
                    ));
                    overlay.active_selection_index = active_selection_index
                        .filter(|index| *index < overlay.selections.len())
                        .or_else(|| (!overlay.selections.is_empty()).then_some(0));
                    return Ok(Some(changed));
                }
                let outcome = record_browser
                    .browser
                    .apply_action(mez_mux::record_browser::RecordBrowserAction::BackToList)?;
                if matches!(
                    outcome,
                    mez_mux::record_browser::RecordBrowserOutcome::Updated
                ) {
                    return Ok(Some(render_record_browser_overlay(
                        overlay,
                        &self.presentation.settings.ui_theme,
                        display_width,
                    )));
                }
                return Ok(None);
            }
            _ => None,
        };
        let Some(action) = action else {
            return Ok(None);
        };
        let outcome = record_browser.browser.apply_action(action)?;
        if matches!(
            outcome,
            mez_mux::record_browser::RecordBrowserOutcome::Ignored
        ) {
            return Ok(None);
        }
        Ok(Some(render_record_browser_overlay(
            overlay,
            &self.presentation.settings.ui_theme,
            display_width,
        )))
    }

    /// Applies editing keys while a retained record-browser modal prompt is open.
    fn apply_primary_record_browser_prompt_input(&mut self, input: &[u8]) -> Result<bool> {
        let display_width = usize::from(self.session.authoritative_size.columns)
            .min(self.presentation.settings.terminal_agent_wrap_column_cap)
            .max(1);
        let prompt_has_selector = self
            .presentation
            .primary_display_overlay
            .as_ref()
            .and_then(|overlay| overlay.record_browser.as_ref())
            .and_then(|record_browser| record_browser.browser.prompt_selection())
            .is_some();
        let prompt_text = self
            .presentation
            .primary_display_overlay
            .as_ref()
            .and_then(|overlay| overlay.record_browser.as_ref())
            .and_then(|record_browser| record_browser.browser.prompt())
            .map(record_browser_prompt_text)
            .unwrap_or_default();
        let action = if prompt_has_selector {
            match runtime_selector_input_action(input) {
                RuntimeSelectorInputAction::Exit => {
                    mez_mux::record_browser::RecordBrowserAction::BackToList
                }
                RuntimeSelectorInputAction::Select => {
                    mez_mux::record_browser::RecordBrowserAction::SubmitPrompt
                }
                RuntimeSelectorInputAction::Previous => {
                    mez_mux::record_browser::RecordBrowserAction::MovePromptSelection(-1)
                }
                RuntimeSelectorInputAction::Next => {
                    mez_mux::record_browser::RecordBrowserAction::MovePromptSelection(1)
                }
                RuntimeSelectorInputAction::First => {
                    mez_mux::record_browser::RecordBrowserAction::SelectPromptFirst
                }
                RuntimeSelectorInputAction::Last => {
                    mez_mux::record_browser::RecordBrowserAction::SelectPromptLast
                }
                RuntimeSelectorInputAction::Ignore => return Ok(false),
            }
        } else {
            match runtime_display_overlay_input_action(input) {
                RuntimeDisplayOverlayInputAction::Exit => {
                    mez_mux::record_browser::RecordBrowserAction::BackToList
                }
                RuntimeDisplayOverlayInputAction::SelectActive => {
                    mez_mux::record_browser::RecordBrowserAction::SubmitPrompt
                }
                RuntimeDisplayOverlayInputAction::EditSearchBackspace => {
                    let mut text = prompt_text;
                    text.pop();
                    mez_mux::record_browser::RecordBrowserAction::EditPrompt(text)
                }
                RuntimeDisplayOverlayInputAction::EditSearchText => {
                    let Ok(input) = std::str::from_utf8(input) else {
                        return Ok(false);
                    };
                    let mut text = prompt_text;
                    text.push_str(input);
                    mez_mux::record_browser::RecordBrowserAction::EditPrompt(text)
                }
                RuntimeDisplayOverlayInputAction::StartSearch
                | RuntimeDisplayOverlayInputAction::SelectPrevious
                | RuntimeDisplayOverlayInputAction::SelectNext
                | RuntimeDisplayOverlayInputAction::SelectFirst
                | RuntimeDisplayOverlayInputAction::SelectLast
                | RuntimeDisplayOverlayInputAction::ScrollBy(_)
                | RuntimeDisplayOverlayInputAction::Ignore => return Ok(false),
            }
        };
        let outcome = {
            let Some(overlay) = self.presentation.primary_display_overlay.as_mut() else {
                return Ok(false);
            };
            let Some(record_browser) = overlay.record_browser.as_mut() else {
                return Ok(false);
            };
            record_browser.browser.apply_action(action)?
        };
        match outcome {
            mez_mux::record_browser::RecordBrowserOutcome::Ignored => Ok(false),
            mez_mux::record_browser::RecordBrowserOutcome::FilterSubmitted { field, value } => {
                let source = self
                    .presentation
                    .primary_display_overlay
                    .as_ref()
                    .and_then(|overlay| overlay.record_browser.as_ref())
                    .and_then(|record_browser| record_browser.source.clone());
                if let Some(source) = source {
                    let source = self.record_browser_source_with_filter(&source, field, &value)?;
                    let browser = self.refresh_record_browser_overlay_source(&source)?;
                    let Some(overlay) = self.presentation.primary_display_overlay.as_mut() else {
                        return Ok(false);
                    };
                    let Some(record_browser) = overlay.record_browser.as_mut() else {
                        return Ok(false);
                    };
                    record_browser.source = Some(source);
                    record_browser.browser = browser;
                }
                let Some(overlay) = self.presentation.primary_display_overlay.as_mut() else {
                    return Ok(false);
                };
                Ok(render_record_browser_overlay(
                    overlay,
                    &self.presentation.settings.ui_theme,
                    display_width,
                ))
            }
            mez_mux::record_browser::RecordBrowserOutcome::SaveSubmitted { path, markdown } => {
                let pane_id = self
                    .presentation
                    .primary_display_overlay
                    .as_ref()
                    .and_then(|overlay| overlay.record_browser.as_ref())
                    .map(|record_browser| record_browser.pane_id.clone());
                if let Some(pane_id) = pane_id {
                    self.save_record_browser_overlay_markdown(&pane_id, &path, markdown)?;
                }
                let Some(overlay) = self.presentation.primary_display_overlay.as_mut() else {
                    return Ok(false);
                };
                Ok(render_record_browser_overlay(
                    overlay,
                    &self.presentation.settings.ui_theme,
                    display_width,
                ))
            }
            _ => {
                let Some(overlay) = self.presentation.primary_display_overlay.as_mut() else {
                    return Ok(false);
                };
                Ok(render_record_browser_overlay(
                    overlay,
                    &self.presentation.settings.ui_theme,
                    display_width,
                ))
            }
        }
    }

    /// Runs the apply primary display overlay input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn apply_primary_display_overlay_input(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        input: &[u8],
    ) -> Result<bool> {
        let Some(overlay) = self.presentation.primary_display_overlay.as_ref() else {
            return Ok(false);
        };
        if overlay.dismiss_on_any_input && !input.is_empty() {
            self.presentation.primary_display_overlay = None;
            return Ok(true);
        }
        if overlay.search_input.is_some() {
            return self.apply_primary_display_overlay_search_input(input);
        }
        if let Some(changed) = self.apply_primary_record_browser_overlay_input(input)? {
            return Ok(changed);
        }
        match runtime_display_overlay_input_action(input) {
            RuntimeDisplayOverlayInputAction::Exit => {
                self.presentation.primary_display_overlay = None;
                Ok(true)
            }
            RuntimeDisplayOverlayInputAction::StartSearch => {
                let Some(overlay) = self.presentation.primary_display_overlay.as_mut() else {
                    return Ok(false);
                };
                overlay.search_input = Some(String::new());
                overlay.search_status = None;
                Ok(true)
            }
            RuntimeDisplayOverlayInputAction::EditSearchText
            | RuntimeDisplayOverlayInputAction::EditSearchBackspace => Ok(false),
            RuntimeDisplayOverlayInputAction::SelectActive => {
                let size = self.session.authoritative_size;
                let command = self
                    .presentation
                    .primary_display_overlay
                    .as_ref()
                    .and_then(|overlay| {
                        let index = overlay.active_selection_index?;
                        overlay_selection_index_is_visible(overlay, index, size)
                            .then(|| overlay.selections.get(index))
                            .flatten()
                    })
                    .map(|selection| selection.command.clone());
                if let Some(command) = command {
                    self.execute_primary_display_overlay_selection_command(
                        primary_client_id,
                        &command,
                    )
                } else {
                    Ok(false)
                }
            }
            RuntimeDisplayOverlayInputAction::SelectPrevious => {
                self.move_primary_display_overlay_selection(-1)
            }
            RuntimeDisplayOverlayInputAction::SelectNext => {
                self.move_primary_display_overlay_selection(1)
            }
            RuntimeDisplayOverlayInputAction::SelectFirst => {
                self.set_primary_display_overlay_selection_index(0)
            }
            RuntimeDisplayOverlayInputAction::SelectLast => {
                let Some(overlay) = self.presentation.primary_display_overlay.as_ref() else {
                    return Ok(false);
                };
                self.set_primary_display_overlay_selection_index(
                    overlay.selections.len().saturating_sub(1),
                )
            }
            RuntimeDisplayOverlayInputAction::ScrollBy(delta) if delta < 0 => {
                let Some(overlay) = self.presentation.primary_display_overlay.as_mut() else {
                    return Ok(false);
                };
                Ok(apply_overlay_scroll_delta(
                    overlay,
                    delta,
                    self.session.authoritative_size,
                ))
            }
            RuntimeDisplayOverlayInputAction::ScrollBy(delta) => {
                let Some(overlay) = self.presentation.primary_display_overlay.as_mut() else {
                    return Ok(false);
                };
                Ok(apply_overlay_scroll_delta(
                    overlay,
                    delta,
                    self.session.authoritative_size,
                ))
            }
            RuntimeDisplayOverlayInputAction::Ignore => Ok(false),
        }
    }

    /// Applies one input chunk while the command-output pager search prompt is active.
    pub(crate) fn apply_primary_display_overlay_search_input(
        &mut self,
        input: &[u8],
    ) -> Result<bool> {
        match runtime_display_overlay_input_action(input) {
            RuntimeDisplayOverlayInputAction::Exit => {
                let Some(overlay) = self.presentation.primary_display_overlay.as_mut() else {
                    return Ok(false);
                };
                overlay.search_input = None;
                overlay.search_status = None;
                Ok(true)
            }
            RuntimeDisplayOverlayInputAction::SelectActive => {
                self.submit_primary_display_overlay_search()
            }
            RuntimeDisplayOverlayInputAction::EditSearchBackspace => {
                let Some(overlay) = self.presentation.primary_display_overlay.as_mut() else {
                    return Ok(false);
                };
                let Some(search_input) = overlay.search_input.as_mut() else {
                    return Ok(false);
                };
                let changed = search_input.pop().is_some();
                Ok(changed)
            }
            RuntimeDisplayOverlayInputAction::EditSearchText => {
                let Ok(text) = std::str::from_utf8(input) else {
                    return Ok(false);
                };
                let Some(overlay) = self.presentation.primary_display_overlay.as_mut() else {
                    return Ok(false);
                };
                let Some(search_input) = overlay.search_input.as_mut() else {
                    return Ok(false);
                };
                search_input.push_str(text);
                Ok(!text.is_empty())
            }
            RuntimeDisplayOverlayInputAction::StartSearch
            | RuntimeDisplayOverlayInputAction::SelectPrevious
            | RuntimeDisplayOverlayInputAction::SelectNext
            | RuntimeDisplayOverlayInputAction::SelectFirst
            | RuntimeDisplayOverlayInputAction::SelectLast
            | RuntimeDisplayOverlayInputAction::ScrollBy(_)
            | RuntimeDisplayOverlayInputAction::Ignore => Ok(false),
        }
    }

    /// Submits the active command-output pager search query.
    pub(crate) fn submit_primary_display_overlay_search(&mut self) -> Result<bool> {
        let Some(overlay) = self.presentation.primary_display_overlay.as_mut() else {
            return Ok(false);
        };
        let submitted = overlay.search_input.take().unwrap_or_default();
        let query = if submitted.is_empty() {
            let Some(query) = overlay.search_query.clone() else {
                overlay.search_status = Some("search: enter a query".to_string());
                return Ok(true);
            };
            query
        } else {
            overlay.search_query = Some(submitted.clone());
            submitted
        };
        let start_line = overlay
            .search_match
            .map(|search_match| search_match.line_index)
            .or_else(|| overlay.scroll_offset.checked_sub(1))
            .unwrap_or(overlay.scroll_offset);
        let Some(search_match) = overlay_next_search_match(overlay, &query, start_line) else {
            overlay.search_status = Some(format!("pattern not found: {query}"));
            return Ok(true);
        };
        overlay.search_match = Some(search_match);
        overlay.scroll_offset = search_match.line_index;
        clamp_overlay_scroll(overlay, self.session.authoritative_size);
        overlay.search_status = None;
        Ok(true)
    }

    /// Moves the active command overlay selection and keeps it visible.
    pub(crate) fn move_primary_display_overlay_selection(&mut self, delta: isize) -> Result<bool> {
        let Some(overlay) = self.presentation.primary_display_overlay.as_mut() else {
            return Ok(false);
        };
        if overlay.selections.is_empty() {
            return Ok(apply_overlay_scroll_delta(
                overlay,
                delta,
                self.session.authoritative_size,
            ));
        }
        let previous = overlay.active_selection_index.unwrap_or(0);
        let next = runtime_selector_step_index(previous, overlay.selections.len(), delta);
        overlay.active_selection_index = Some(next);
        if let Some(line_index) = overlay
            .selections
            .get(next)
            .map(|selection| selection.line_index)
        {
            scroll_overlay_to_line(overlay, line_index, self.session.authoritative_size);
        }
        Ok(next != previous)
    }

    /// Sets the active command overlay selection and keeps it visible.
    pub(crate) fn set_primary_display_overlay_selection_index(
        &mut self,
        index: usize,
    ) -> Result<bool> {
        let Some(overlay) = self.presentation.primary_display_overlay.as_mut() else {
            return Ok(false);
        };
        if overlay.selections.is_empty() {
            let next = if index == 0 {
                0
            } else {
                modal_overlay_max_scroll(overlay.lines.len(), self.session.authoritative_size)
            };
            let changed = next != overlay.scroll_offset;
            overlay.scroll_offset = next;
            return Ok(changed);
        }
        let previous = overlay.active_selection_index.unwrap_or(0);
        let next = index.min(overlay.selections.len().saturating_sub(1));
        overlay.active_selection_index = Some(next);
        if let Some(line_index) = overlay
            .selections
            .get(next)
            .map(|selection| selection.line_index)
        {
            scroll_overlay_to_line(overlay, line_index, self.session.authoritative_size);
        }
        Ok(next != previous)
    }
}

/// Returns a selector row rendition, highlighting the hovered item.
pub(crate) fn runtime_pane_agent_selector_rendition(
    field: PaneAgentStatusField,
    active: bool,
    ui_theme: &mez_mux::theme::UiTheme,
) -> mez_terminal::GraphicRendition {
    let pair = if active {
        match field {
            PaneAgentStatusField::Model => ui_theme.colors.agent_model,
            PaneAgentStatusField::Reasoning => ui_theme.colors.agent_reasoning,
            PaneAgentStatusField::Thinking => ui_theme.colors.agent_reasoning,
            PaneAgentStatusField::Routing => ui_theme.colors.agent_reasoning,
            PaneAgentStatusField::ApprovalPolicy => ui_theme.colors.agent_status_blocked,
            PaneAgentStatusField::Latency => ui_theme.colors.agent_reasoning,
            PaneAgentStatusField::Preset => ui_theme.colors.agent_model,
        }
    } else {
        ui_theme.colors.display_overlay
    };
    let pair_rendition = pair.rendition();
    mez_terminal::GraphicRendition {
        foreground: pair_rendition.foreground,
        background: pair_rendition.background,
        bold: active,
        ..mez_terminal::GraphicRendition::default()
    }
}

impl RuntimeSessionService {
    /// Shows or clears the primary-client command display overlay.
    ///
    /// Non-empty line sets are rendered as a modal full-window view on the next
    /// primary render pass. An empty line set clears any active overlay. This
    /// fails when the runtime is no longer live.
    pub fn show_primary_display_overlay(&mut self, lines: Vec<String>) -> Result<()> {
        let wrap_columns = usize::from(self.session.authoritative_size.columns)
            .min(self.presentation.settings.terminal_agent_wrap_column_cap)
            .max(1);
        let lines = lines
            .into_iter()
            .flat_map(|line| wrap_agent_terminal_text(&line, wrap_columns))
            .collect::<Vec<_>>();
        let line_style_spans = vec![Vec::new(); lines.len()];
        self.show_primary_display_overlay_inner(lines, line_style_spans, Vec::new(), false)
    }

    /// Shows or clears the primary-client recoverable error status overlay.
    ///
    /// Error overlays render over the window status bar and are dismissed by
    /// the next user action without consuming that action. This keeps runtime
    /// errors visible without turning them into modal state.
    pub fn show_primary_error_overlay(&mut self, lines: Vec<String>) -> Result<()> {
        self.require_live()?;
        self.presentation.primary_error_status_overlay = lines
            .into_iter()
            .find(|line| !line.trim().is_empty())
            .map(|line| runtime_primary_error_status_text(&line));
        Ok(())
    }

    /// Shows or clears the primary-client transient success notice overlay.
    ///
    /// Notice overlays share the status-bar dismissal lifecycle with
    /// recoverable errors while keeping successful command acknowledgements out
    /// of pane transcripts.
    pub fn show_primary_notice_overlay(&mut self, lines: Vec<String>) -> Result<()> {
        self.require_live()?;
        self.presentation.primary_error_status_overlay = lines
            .into_iter()
            .find(|line| !line.trim().is_empty())
            .map(|line| runtime_primary_notice_status_text(&line));
        Ok(())
    }

    /// Runs the show primary display overlay inner operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn show_primary_display_overlay_inner(
        &mut self,
        lines: Vec<String>,
        mut line_style_spans: Vec<Vec<TerminalStyleSpan>>,
        selections: Vec<OverlaySelection>,
        dismiss_on_any_input: bool,
    ) -> Result<()> {
        self.require_live()?;
        self.presentation.primary_display_overlay = if lines.is_empty() {
            None
        } else {
            line_style_spans.truncate(lines.len());
            line_style_spans.resize(lines.len(), Vec::new());
            let active_selection_index = (!selections.is_empty()).then_some(0);
            Some(RuntimeDisplayOverlay {
                lines,
                line_style_spans,
                scroll_offset: 0,
                search_input: None,
                search_query: None,
                search_match: None,
                search_status: None,
                mouse_selection: None,
                selections,
                active_selection_index,
                dismiss_on_any_input,
                record_browser: None,
            })
        };
        Ok(())
    }

    /// Clears the primary-client command display overlay.
    ///
    /// Returns true when an overlay was active before the call.
    #[cfg(test)]
    pub fn clear_primary_display_overlay(&mut self) -> bool {
        self.presentation.primary_display_overlay.take().is_some()
    }

    /// Appends terminal-command display output to the active pane buffer.
    ///
    /// Short acknowledgement-style command output should remain in the pane
    /// transcript instead of forcing a modal command-output overlay. The bytes
    /// are fed through the same pane-screen ingestion path as process output so
    /// rendering state, scrollback, and observers stay consistent.
    fn append_runtime_command_display_lines_to_active_pane(
        &mut self,
        lines: &[String],
    ) -> Result<()> {
        let visible_lines = lines
            .iter()
            .map(|line| sanitized_agent_terminal_line(line))
            .filter(|line| !line.trim().is_empty())
            .take(200)
            .collect::<Vec<_>>();
        if visible_lines.is_empty() {
            return Ok(());
        }
        let pane_id = self.active_pane_id()?.to_string();
        let mut bytes = Vec::new();
        for line in visible_lines {
            bytes.extend_from_slice(b"\r\nmez: ");
            bytes.extend_from_slice(line.as_bytes());
        }
        bytes.extend_from_slice(b"\r\n");
        self.apply_pane_output_bytes(pane_id, bytes)?;
        Ok(())
    }

    /// Presents terminal command display content according to its feedback policy.
    pub(crate) fn present_runtime_command_display_content(
        &mut self,
        content: RuntimeCommandDisplayOverlayContent,
    ) -> Result<()> {
        let should_open_overlay = runtime_command_display_should_open_overlay(&content);
        if should_open_overlay {
            let wrap_columns = usize::from(self.session.authoritative_size.columns)
                .min(self.presentation.settings.terminal_agent_wrap_column_cap)
                .max(1);
            let content = wrap_runtime_command_display_overlay_content(content, wrap_columns);
            return self.show_primary_display_overlay_inner(
                content.lines,
                content.line_style_spans,
                content.selections,
                false,
            );
        }
        if let Some(line) = runtime_command_display_transient_status_line(&content) {
            return self.show_primary_notice_overlay(vec![line]);
        }
        self.append_runtime_command_display_lines_to_active_pane(&content.lines)
    }

    /// Runs the apply primary display overlay terminal action operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn apply_primary_display_overlay_terminal_action(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        action: &TerminalClientLoopAction,
    ) -> Result<bool> {
        match action {
            TerminalClientLoopAction::ForwardToPane(input)
            | TerminalClientLoopAction::ForwardMouseToPane { input, .. } => {
                self.apply_primary_display_overlay_input(primary_client_id, input)
            }
            TerminalClientLoopAction::HandleMouse(MouseAction::SelectDisplayOverlay {
                position,
            }) => self.apply_primary_display_overlay_selection(primary_client_id, *position),
            TerminalClientLoopAction::HandleMouse(MouseAction::BeginDisplayOverlaySelection {
                position,
            }) => self.begin_primary_display_overlay_mouse_selection(*position),
            TerminalClientLoopAction::HandleMouse(MouseAction::UpdateDisplayOverlaySelection {
                position,
            }) => self.update_primary_display_overlay_mouse_selection(*position),
            TerminalClientLoopAction::HandleMouse(MouseAction::FinishDisplayOverlaySelection {
                position,
            }) => self.finish_primary_display_overlay_mouse_selection(primary_client_id, *position),
            TerminalClientLoopAction::HandleMouse(MouseAction::ScrollDisplayOverlay { lines }) => {
                self.apply_primary_display_overlay_scroll(*lines)
            }
            TerminalClientLoopAction::ExecuteMux(_)
            | TerminalClientLoopAction::ExecuteCommand(_)
            | TerminalClientLoopAction::HandleMouse(_)
            | TerminalClientLoopAction::HandleCopyMode(_)
            | TerminalClientLoopAction::EnterPrefixKeyMode
            | TerminalClientLoopAction::ReportUnboundPrefix(_) => Ok(false),
        }
    }

    /// Reports whether one primary display overlay action should invalidate the
    /// attached client's retained output frame.
    ///
    /// Keyboard and mouse-wheel navigation only move the overlay viewport or
    /// active row, so the next rendered view can be applied through the normal
    /// diff renderer. Exiting the modal overlay or executing a selected row can
    /// expose a different underlying view or run a command, so those paths keep
    /// the stronger redraw signal.
    pub(crate) fn primary_display_overlay_action_requires_full_redraw(
        &self,
        action: &TerminalClientLoopAction,
    ) -> bool {
        match action {
            TerminalClientLoopAction::ForwardToPane(input)
            | TerminalClientLoopAction::ForwardMouseToPane { input, .. } => {
                if self
                    .presentation
                    .primary_display_overlay
                    .as_ref()
                    .is_some_and(|overlay| overlay.dismiss_on_any_input && !input.is_empty())
                {
                    return true;
                }
                matches!(
                    runtime_display_overlay_input_action(input),
                    RuntimeDisplayOverlayInputAction::Exit
                        | RuntimeDisplayOverlayInputAction::SelectActive
                ) && self
                    .presentation
                    .primary_display_overlay
                    .as_ref()
                    .is_none_or(|overlay| overlay.search_input.is_none())
            }
            TerminalClientLoopAction::HandleMouse(MouseAction::SelectDisplayOverlay { .. }) => true,
            TerminalClientLoopAction::HandleMouse(MouseAction::ScrollDisplayOverlay { .. }) => {
                false
            }
            TerminalClientLoopAction::ExecuteMux(_)
            | TerminalClientLoopAction::ExecuteCommand(_)
            | TerminalClientLoopAction::HandleMouse(_)
            | TerminalClientLoopAction::HandleCopyMode(_)
            | TerminalClientLoopAction::EnterPrefixKeyMode
            | TerminalClientLoopAction::ReportUnboundPrefix(_) => false,
        }
    }

    /// Executes the selectable command row under a primary display overlay
    /// mouse click.
    fn apply_primary_display_overlay_selection(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        position: CopyPosition,
    ) -> Result<bool> {
        let Some(overlay) = self.presentation.primary_display_overlay.as_ref() else {
            return Ok(false);
        };
        if position.line == 0 {
            return Ok(false);
        }
        let display_line_index = overlay
            .scroll_offset
            .saturating_add(position.line.saturating_sub(1));
        let selection_index =
            overlay_selection_index_at_position(overlay, display_line_index, position.column);
        let Some(command) = selection_index
            .and_then(|index| overlay.selections.get(index))
            .map(|selection| selection.command.clone())
        else {
            return Ok(false);
        };
        if let Some(overlay) = self.presentation.primary_display_overlay.as_mut() {
            overlay.active_selection_index = selection_index;
        }
        self.execute_primary_display_overlay_selection_command(primary_client_id, &command)
    }

    /// Starts a mouse text selection in the primary command-output overlay.
    fn begin_primary_display_overlay_mouse_selection(
        &mut self,
        position: CopyPosition,
    ) -> Result<bool> {
        let Some(selection_position) = self.primary_display_overlay_position_for_mouse(position)
        else {
            return Ok(false);
        };
        if let Some(overlay) = self.presentation.primary_display_overlay.as_mut() {
            overlay.mouse_selection = Some((selection_position, selection_position));
        }
        Ok(true)
    }

    /// Extends a mouse text selection in the primary command-output overlay.
    fn update_primary_display_overlay_mouse_selection(
        &mut self,
        position: CopyPosition,
    ) -> Result<bool> {
        let Some(selection_position) = self.primary_display_overlay_position_for_mouse(position)
        else {
            return Ok(false);
        };
        if let Some(overlay) = self.presentation.primary_display_overlay.as_mut() {
            let start = overlay
                .mouse_selection
                .map(|(start, _)| start)
                .unwrap_or(selection_position);
            overlay.mouse_selection = Some((start, selection_position));
        }
        Ok(true)
    }

    /// Finishes a mouse text selection in the primary command-output overlay and copies it.
    fn finish_primary_display_overlay_mouse_selection(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        position: CopyPosition,
    ) -> Result<bool> {
        let Some(selection_position) = self.primary_display_overlay_position_for_mouse(position)
        else {
            return Ok(false);
        };
        let copied = if let Some(overlay) = self.presentation.primary_display_overlay.as_mut() {
            let start = overlay
                .mouse_selection
                .map(|(start, _)| start)
                .unwrap_or(selection_position);
            overlay.mouse_selection = Some((start, selection_position));
            overlay_copy_selection(overlay)
        } else {
            None
        };
        if let Some(copied) = copied.filter(|text| !text.is_empty()) {
            self.copy_text_to_buffer_and_host_clipboard(
                "mouse",
                copied,
                "display-overlay:mouse".to_string(),
            )?;
            return Ok(true);
        }
        self.apply_primary_display_overlay_selection(primary_client_id, position)
    }

    /// Converts one terminal mouse cell to overlay-content coordinates.
    fn primary_display_overlay_position_for_mouse(
        &self,
        position: CopyPosition,
    ) -> Option<CopyPosition> {
        let overlay = self.presentation.primary_display_overlay.as_ref()?;
        let line = position.line.checked_sub(1)?;
        let line = overlay.scroll_offset.saturating_add(line);
        let text = overlay.lines.get(line)?;
        let prefix_columns = overlay_line_prefix_columns(overlay, line);
        let column = position.column.saturating_sub(prefix_columns);
        let column = column.min(terminal_text_width(text));
        Some(CopyPosition { line, column })
    }
}
