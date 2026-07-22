//! Runtime service mutation for overlays, record browsers, and selections.

use super::display_content::*;
use super::product_content::*;
use super::selection_adapter::*;
use crate::runtime::render::*;
use crate::ui::selector::record_browser_save_path_candidates;

impl RuntimeSessionService {
    /// Reflows an active record browser after terminal geometry changes.
    ///
    /// The browser retains raw Markdown and structured navigation state, so it
    /// can be rendered again without treating previously wrapped physical rows
    /// as source content. Other overlays keep their already-rendered payload.
    pub(crate) fn reflow_primary_record_browser_overlay(&mut self) -> bool {
        let terminal_width = usize::from(self.session.authoritative_size.columns).max(1);
        let prose_width = terminal_width
            .min(self.presentation.settings.terminal_agent_wrap_column_cap)
            .max(1);
        let Some(overlay) = self.presentation.primary_display_overlay.as_mut() else {
            return false;
        };
        if overlay.record_browser.is_none() {
            return false;
        }
        let changed = render_record_browser_overlay(
            overlay,
            &self.presentation.settings.ui_theme,
            terminal_width,
            prose_width,
        );
        clamp_overlay_scroll(overlay, self.session.authoritative_size);
        changed
    }

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
            let display_output = runtime_agent_shell_display_output(
                &body,
                &self.presentation.settings.ui_theme,
                usize::from(self.session.authoritative_size.columns),
                self.presentation.settings.terminal_agent_wrap_column_cap,
            )?;
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
                runtime_command_display_overlay_content(
                    &body,
                    &self.presentation.settings.ui_theme,
                    usize::from(self.session.authoritative_size.columns),
                    self.presentation.settings.terminal_agent_wrap_column_cap,
                )
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
    fn apply_primary_record_browser_overlay_input(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        input: &[u8],
    ) -> Result<Option<bool>> {
        let terminal_width = usize::from(self.session.authoritative_size.columns).max(1);
        let prose_width = terminal_width
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
        if matches!(
            record_browser.source,
            Some(RuntimeRecordBrowserOverlaySource::Approvals)
        ) && matches!(input, b"a" | b"d")
        {
            let active_index = overlay
                .active_selection_index
                .unwrap_or_else(|| record_browser.browser.active_index());
            let mut selected = record_browser.browser.clone();
            selected.set_active_index(active_index);
            let Some(approval_id) = selected.active_record_id().map(str::to_string) else {
                return Ok(Some(false));
            };
            let decision = if input == b"a" {
                "approve"
            } else {
                "disapprove"
            };
            let scope = if input == b"a" {
                r#", "scope":{"persistence":"once"}"#
            } else {
                ""
            };
            let request = format!(
                r#"{{"jsonrpc":"2.0","id":"approval-browser","method":"approval/decide","params":{{"approval_id":"{}","decision":"{}"{},"idempotency_key":"approval-browser-{}-{}"}}}}"#,
                json_escape(&approval_id),
                decision,
                scope,
                json_escape(&approval_id),
                current_unix_seconds()
            );
            let response = self.dispatch_runtime_control_body(&request, primary_client_id);
            let error = serde_json::from_str::<serde_json::Value>(&response)
                .ok()
                .and_then(|value| value.get("error").cloned())
                .and_then(|error| {
                    error
                        .get("message")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                });
            let mut browser = self.approval_record_browser()?;
            browser.set_active_index(active_index);
            browser.set_error(error);
            let Some(overlay) = self.presentation.primary_display_overlay.as_mut() else {
                return Ok(Some(false));
            };
            let Some(record_browser) = overlay.record_browser.as_mut() else {
                return Ok(None);
            };
            record_browser.browser = browser;
            return Ok(Some(render_record_browser_overlay(
                overlay,
                &self.presentation.settings.ui_theme,
                terminal_width,
                prose_width,
            )));
        }
        if input == b"a" {
            let source = record_browser.source.clone();
            if let Some(source) = source {
                let source = self.record_browser_source_toggled_scope(&source);
                let browser = self.refresh_record_browser_overlay_source(&source)?;
                let Some(overlay) = self.presentation.primary_display_overlay.as_mut() else {
                    return Ok(Some(false));
                };
                let Some(record_browser) = overlay.record_browser.as_mut() else {
                    return Ok(None);
                };
                record_browser.source = Some(source);
                record_browser.browser = browser;
                return Ok(Some(render_record_browser_overlay(
                    overlay,
                    &self.presentation.settings.ui_theme,
                    terminal_width,
                    prose_width,
                )));
            }
        }
        if input == b"d" && record_browser.browser.deletion_enabled() {
            let source = record_browser.source.clone().ok_or_else(|| {
                MezError::invalid_state("deletable record browser is missing its backend source")
            })?;
            let active_index = overlay
                .active_selection_index
                .unwrap_or_else(|| record_browser.browser.active_index());
            let outcome = {
                let Some(overlay) = self.presentation.primary_display_overlay.as_mut() else {
                    return Ok(Some(false));
                };
                let Some(record_browser) = overlay.record_browser.as_mut() else {
                    return Ok(None);
                };
                record_browser.browser.set_active_index(active_index);
                record_browser
                    .browser
                    .apply_action(mez_mux::record_browser::RecordBrowserAction::DeleteActive)?
            };
            let mez_mux::record_browser::RecordBrowserOutcome::DeleteSubmitted { id } = outcome
            else {
                return Ok(None);
            };
            let browser = match self.delete_record_browser_entry(&source, &id, active_index) {
                Ok(browser) => browser,
                Err(error) => {
                    let Some(overlay) = self.presentation.primary_display_overlay.as_mut() else {
                        return Ok(Some(false));
                    };
                    let Some(record_browser) = overlay.record_browser.as_mut() else {
                        return Ok(None);
                    };
                    record_browser
                        .browser
                        .set_error(Some(error.message().to_string()));
                    return Ok(Some(render_record_browser_overlay(
                        overlay,
                        &self.presentation.settings.ui_theme,
                        terminal_width,
                        prose_width,
                    )));
                }
            };
            let Some(overlay) = self.presentation.primary_display_overlay.as_mut() else {
                return Ok(Some(false));
            };
            let Some(record_browser) = overlay.record_browser.as_mut() else {
                return Ok(None);
            };
            record_browser.browser = browser;
            return Ok(Some(render_record_browser_overlay(
                overlay,
                &self.presentation.settings.ui_theme,
                terminal_width,
                prose_width,
            )));
        }
        let active_selection_index = overlay.active_selection_index;
        let Some(overlay) = self.presentation.primary_display_overlay.as_mut() else {
            return Ok(Some(false));
        };
        let Some(record_browser) = overlay.record_browser.as_mut() else {
            return Ok(None);
        };
        if let Some(active_index) = active_selection_index {
            record_browser.browser.set_active_index(active_index);
        }
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
            b"s" => {
                self.presentation.record_browser_save_completion = None;
                Some(mez_mux::record_browser::RecordBrowserAction::StartSave)
            }
            _ if matches!(selector_input_action(input), SelectorInputAction::Select) => {
                Some(mez_mux::record_browser::RecordBrowserAction::OpenActive)
            }
            _ if matches!(selector_input_action(input), SelectorInputAction::Exit) => {
                if let Some(frame) = record_browser.stack.pop() {
                    record_browser.command = frame.command;
                    record_browser.source = frame.source;
                    record_browser.browser = frame.browser;
                    let scroll_offset = frame.scroll_offset;
                    let active_selection_index = frame.active_selection_index;
                    let changed = render_record_browser_overlay(
                        overlay,
                        &self.presentation.settings.ui_theme,
                        terminal_width,
                        prose_width,
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
                        terminal_width,
                        prose_width,
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
            terminal_width,
            prose_width,
        )))
    }

    /// Applies editing keys while a retained record-browser modal prompt is open.
    fn apply_primary_record_browser_prompt_input(&mut self, input: &[u8]) -> Result<bool> {
        let terminal_width = usize::from(self.session.authoritative_size.columns).max(1);
        let prose_width = terminal_width
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
        let save_prompt_pane_id = self
            .presentation
            .primary_display_overlay
            .as_ref()
            .and_then(|overlay| overlay.record_browser.as_ref())
            .and_then(|record_browser| {
                matches!(
                    record_browser.browser.prompt(),
                    Some(mez_mux::record_browser::RecordBrowserPrompt::Save { .. })
                )
                .then(|| record_browser.pane_id.clone())
            });
        if let Some(pane_id) = save_prompt_pane_id {
            if matches!(input, b"\t" | b"\x1b[Z") {
                let completion = self.presentation.record_browser_save_completion.take();
                let (candidates, selected_index) = match completion {
                    Some(completion)
                        if completion.base_input == prompt_text
                            || completion
                                .candidates
                                .get(completion.selected_index)
                                .is_some_and(|candidate| candidate == &prompt_text) =>
                    {
                        let selected_index = if input == b"\t" {
                            completion.selected_index.saturating_add(1)
                                % completion.candidates.len().max(1)
                        } else if completion.selected_index == 0 {
                            completion.candidates.len().saturating_sub(1)
                        } else {
                            completion.selected_index.saturating_sub(1)
                        };
                        (completion.candidates, selected_index)
                    }
                    _ => {
                        let candidates = record_browser_save_path_candidates(
                            &prompt_text,
                            self.pane_current_working_directory(&pane_id).as_deref(),
                        )
                        .into_iter()
                        .map(|candidate| candidate.value)
                        .collect::<Vec<_>>();
                        (candidates, 0)
                    }
                };
                let Some(selected) = candidates.get(selected_index).cloned() else {
                    return Ok(false);
                };
                self.presentation.record_browser_save_completion =
                    Some(RuntimeRecordBrowserSaveCompletion {
                        base_input: prompt_text,
                        candidates,
                        selected_index,
                    });
                let Some(overlay) = self.presentation.primary_display_overlay.as_mut() else {
                    return Ok(false);
                };
                let Some(record_browser) = overlay.record_browser.as_mut() else {
                    return Ok(false);
                };
                record_browser.browser.apply_action(
                    mez_mux::record_browser::RecordBrowserAction::EditPrompt(selected),
                )?;
                return Ok(render_record_browser_overlay(
                    overlay,
                    &self.presentation.settings.ui_theme,
                    terminal_width,
                    prose_width,
                ));
            }
            if !matches!(input, b"\r" | b"\n") {
                self.presentation.record_browser_save_completion = None;
            }
        } else {
            self.presentation.record_browser_save_completion = None;
        }
        let action = if prompt_has_selector {
            match selector_input_action(input) {
                SelectorInputAction::Exit => {
                    mez_mux::record_browser::RecordBrowserAction::BackToList
                }
                SelectorInputAction::Select => {
                    mez_mux::record_browser::RecordBrowserAction::SubmitPrompt
                }
                SelectorInputAction::Previous => {
                    mez_mux::record_browser::RecordBrowserAction::MovePromptSelection(-1)
                }
                SelectorInputAction::Next => {
                    mez_mux::record_browser::RecordBrowserAction::MovePromptSelection(1)
                }
                SelectorInputAction::First => {
                    mez_mux::record_browser::RecordBrowserAction::SelectPromptFirst
                }
                SelectorInputAction::Last => {
                    mez_mux::record_browser::RecordBrowserAction::SelectPromptLast
                }
                SelectorInputAction::Ignore => return Ok(false),
            }
        } else {
            match overlay_input_action(input) {
                OverlayInputAction::Exit => {
                    mez_mux::record_browser::RecordBrowserAction::BackToList
                }
                OverlayInputAction::SelectActive => {
                    mez_mux::record_browser::RecordBrowserAction::SubmitPrompt
                }
                OverlayInputAction::EditSearchBackspace => {
                    let mut text = prompt_text;
                    text.pop();
                    mez_mux::record_browser::RecordBrowserAction::EditPrompt(text)
                }
                OverlayInputAction::EditSearchText => {
                    let Ok(input) = std::str::from_utf8(input) else {
                        return Ok(false);
                    };
                    let mut text = prompt_text;
                    text.push_str(input);
                    mez_mux::record_browser::RecordBrowserAction::EditPrompt(text)
                }
                OverlayInputAction::StartSearch
                | OverlayInputAction::SelectPrevious
                | OverlayInputAction::SelectNext
                | OverlayInputAction::SelectFirst
                | OverlayInputAction::SelectLast
                | OverlayInputAction::ScrollBy(_)
                | OverlayInputAction::Ignore => return Ok(false),
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
                    terminal_width,
                    prose_width,
                ))
            }
            mez_mux::record_browser::RecordBrowserOutcome::SaveSubmitted { path, markdown } => {
                let pane_id = self
                    .presentation
                    .primary_display_overlay
                    .as_ref()
                    .and_then(|overlay| overlay.record_browser.as_ref())
                    .map(|record_browser| record_browser.pane_id.clone());
                self.presentation.record_browser_save_completion = None;
                if let Some(pane_id) = pane_id {
                    self.save_record_browser_overlay_markdown(&pane_id, &path, markdown)?;
                }
                let Some(overlay) = self.presentation.primary_display_overlay.as_mut() else {
                    return Ok(false);
                };
                Ok(render_record_browser_overlay(
                    overlay,
                    &self.presentation.settings.ui_theme,
                    terminal_width,
                    prose_width,
                ))
            }
            _ => {
                let Some(overlay) = self.presentation.primary_display_overlay.as_mut() else {
                    return Ok(false);
                };
                Ok(render_record_browser_overlay(
                    overlay,
                    &self.presentation.settings.ui_theme,
                    terminal_width,
                    prose_width,
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
        if overlay.search_input.is_none()
            && let Some(changed) =
                self.apply_primary_record_browser_overlay_input(primary_client_id, input)?
        {
            return Ok(changed);
        }
        let action = overlay_input_action(input);
        let input_text = matches!(action, OverlayInputAction::EditSearchText)
            .then(|| std::str::from_utf8(input).ok())
            .flatten();
        let outcome = {
            let Some(overlay) = self.presentation.primary_display_overlay.as_mut() else {
                return Ok(false);
            };
            apply_overlay_input(
                overlay,
                action,
                input_text,
                !input.is_empty(),
                self.session.authoritative_size,
            )
        };
        match outcome {
            OverlayInputOutcome::Close => {
                self.presentation.primary_display_overlay = None;
                Ok(true)
            }
            OverlayInputOutcome::Invoke { command } => {
                self.execute_primary_display_overlay_selection_command(primary_client_id, &command)
            }
            OverlayInputOutcome::Updated => Ok(true),
            OverlayInputOutcome::Unchanged | OverlayInputOutcome::Ignored => Ok(false),
        }
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
        let line_style_spans = vec![Vec::new(); lines.len()];
        let line_copy_texts = vec![None; lines.len()];
        self.show_primary_display_overlay_inner(
            lines,
            line_style_spans,
            line_copy_texts,
            Vec::new(),
            false,
        )
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
        mut line_copy_texts: Vec<Option<String>>,
        selections: Vec<OverlaySelection>,
        dismiss_on_any_input: bool,
    ) -> Result<()> {
        self.require_live()?;
        self.presentation.primary_display_overlay = if lines.is_empty() {
            None
        } else {
            line_style_spans.truncate(lines.len());
            line_style_spans.resize(lines.len(), Vec::new());
            line_copy_texts.truncate(lines.len());
            line_copy_texts.resize(lines.len(), None);
            let active_selection_index = (!selections.is_empty()).then_some(0);
            Some(RuntimeDisplayOverlay {
                lines,
                line_style_spans,
                line_copy_texts,
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
        mut content: RuntimeCommandDisplayOverlayContent,
    ) -> Result<()> {
        let should_open_overlay = runtime_command_display_should_open_overlay(&content);
        if should_open_overlay {
            let available_width = runtime_command_overlay_available_width(
                usize::from(self.session.authoritative_size.columns),
                !content.selections.is_empty(),
            );
            content = wrap_runtime_command_display_overlay_content(
                content,
                available_width,
                available_width,
            );
            return self.show_primary_display_overlay_inner(
                content.lines,
                content.line_style_spans,
                content.line_copy_texts,
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
                    overlay_input_action(input),
                    OverlayInputAction::Exit | OverlayInputAction::SelectActive
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
