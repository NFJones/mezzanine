//! Actor acceptance of worker-rendered provisional response generations.
//!
//! A candidate can replace the existing conversation screen only while response,
//! source revision, installed lineage, geometry and rendering policy still match.
//! Rejected and identical candidates retain the existing screen and interaction.

use super::*;

impl RuntimeSessionService {
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
        let outbound_message_projections = work
            .outbound_messages
            .iter()
            .map(|(action_index, message)| {
                (
                    *action_index,
                    streaming_outbound_message_projection_with_theme(
                        message,
                        work.frame_width,
                        work.table_width,
                        &work.ui_theme,
                    ),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
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
                        work.frame_width,
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
        let command_content_columns = work
            .frame_width
            .saturating_sub(UnicodeWidthStr::width("$ "))
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
        let rationale_text = work.rationale.as_ref().map(|source| source.text.as_str());
        let summary_projections = if work.thinking_enabled {
            work.shell_summaries
                .iter()
                .filter(|(_action_index, source)| Some(source.text.as_str()) != rationale_text)
                .map(|(action_index, source)| {
                    let rendition = agent_terminal_label_rendition(
                        AgentTerminalPresentationStyle::Status,
                        &work.ui_theme,
                    );
                    let rendered_lines =
                        agent_thinking_display_lines_for_width(&source.text, work.frame_width)
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
                            .collect();
                    (
                        *action_index,
                        StreamingSayProjection {
                            style: AgentTerminalPresentationStyle::Status,
                            rendered_lines,
                            copy_lines: Vec::new(),
                        },
                    )
                })
                .collect::<std::collections::BTreeMap<_, _>>()
        } else {
            std::collections::BTreeMap::new()
        };
        let header_projections = work
            .action_headers
            .iter()
            .map(|(action_index, header)| {
                let header = streaming_action_execution_display_header(header);
                let rendered_lines = wrap_rich_text_line_to_width_with_source_ranges_hard(
                    agent_action_execution_rendered_line(&header, &work.ui_theme),
                    work.frame_width,
                )
                .into_iter()
                .map(|wrapped| wrapped.line)
                .collect();
                (
                    *action_index,
                    StreamingSayProjection {
                        style: AgentTerminalPresentationStyle::Status,
                        rendered_lines,
                        copy_lines: Vec::new(),
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
            .chain(work.outbound_messages.keys())
            .chain(work.shell_summaries.keys())
            .chain(work.action_headers.keys())
            .chain(work.shell_commands.keys())
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        for action_index in action_indices {
            let say = say_projections.iter().find_map(|(candidate, projection)| {
                (*candidate == action_index).then_some(projection)
            });
            for projection in say
                .into_iter()
                .chain(outbound_message_projections.get(&action_index))
                .chain(summary_projections.get(&action_index))
                .chain(header_projections.get(&action_index))
                .chain(command_projections.get(&action_index))
            {
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
            installed_lineage: work.installed_lineage,
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
            || !presentation.outbound_messages.is_empty()
            || !presentation.shell_commands.is_empty()
            || !presentation.shell_summaries.is_empty()
            || !presentation.action_headers.is_empty();
        if !has_source {
            return Ok(None);
        }
        if self.agent_pane_screen_lineage(pane_id, &presentation.conversation_id)
            != Some(presentation.installed_lineage)
        {
            return Ok(None);
        }
        let projected_context = self.agent_streaming_say_projection_context(pane_id)?;
        if presentation.projected_revision == Some(presentation.revision)
            && presentation.projected_context.as_ref() == Some(&projected_context)
        {
            return Ok(None);
        }
        // Preserve later source for reconciliation, but do not publish it while
        // an earlier component is still receiving its direct field. The worker
        // sees one immutable ordered prefix rather than the entire response.
        let first_open_action = presentation
            .actions
            .iter()
            .filter(|(_, source)| !source.complete)
            .map(|(index, _)| *index)
            .chain(
                presentation
                    .outbound_messages
                    .iter()
                    .filter(|(_, source)| !source.complete)
                    .map(|(index, _)| *index),
            )
            .chain(
                presentation
                    .shell_commands
                    .iter()
                    .filter(|(_, source)| !source.complete)
                    .map(|(index, _)| *index),
            )
            .chain(
                presentation
                    .shell_summaries
                    .iter()
                    .filter(|(_, source)| !source.complete)
                    .map(|(index, _)| *index),
            )
            .min();
        let first_unreceived_action = (!presentation.received_actions.is_empty())
            .then(|| {
                presentation
                    .actions
                    .keys()
                    .chain(presentation.outbound_messages.keys())
                    .chain(presentation.shell_commands.keys())
                    .chain(presentation.shell_summaries.keys())
                    .chain(presentation.action_headers.keys())
                    .filter(|index| !presentation.received_actions.contains(index))
                    .copied()
                    .min()
            })
            .flatten();
        // Receipt of an action with no previewable field does not determine
        // whether validation will supply a header or runtime result for it.
        let first_no_preview_action = presentation
            .received_actions
            .iter()
            .filter(|index| {
                !presentation.actions.contains_key(index)
                    && !presentation.outbound_messages.contains_key(index)
                    && !presentation.shell_commands.contains_key(index)
                    && !presentation.shell_summaries.contains_key(index)
                    && !presentation.action_headers.contains_key(index)
            })
            .copied()
            .min();
        let first_pending_action = first_open_action
            .into_iter()
            .chain(first_unreceived_action)
            .chain(first_no_preview_action)
            // Receipt establishes an ordinal, not an accepted or finalized
            // component. Keep later provisional source buffered until the
            // validated completion hands it to the ordinary presenter.
            .chain(presentation.received_actions.iter().copied().min())
            .min();
        let visible = |index: &usize| first_pending_action.is_none_or(|pending| *index <= pending);
        let visible = |index: &usize| {
            presentation
                .rationale
                .as_ref()
                .is_none_or(|source| source.complete)
                && visible(index)
        };
        Ok(Some(crate::runtime::RuntimeStreamingSayProjectionWork {
            pane_id: pane_id.to_string(),
            turn_id: turn_id.to_string(),
            response_index: presentation.response_index,
            conversation_id: presentation.conversation_id.clone(),
            revision: presentation.revision,
            installed_lineage: presentation.installed_lineage,
            baseline_screen: presentation.baseline_screen.clone(),
            rationale: presentation.rationale.clone(),
            actions: presentation
                .actions
                .iter()
                .filter(|(index, _)| visible(index))
                .map(|(index, source)| (*index, source.clone()))
                .collect(),
            outbound_messages: presentation
                .outbound_messages
                .iter()
                .filter(|(index, _)| visible(index))
                .map(|(index, source)| (*index, source.clone()))
                .collect(),
            shell_commands: presentation
                .shell_commands
                .iter()
                .filter(|(index, _)| visible(index))
                .map(|(index, source)| (*index, source.clone()))
                .collect(),
            shell_summaries: presentation
                .shell_summaries
                .iter()
                .filter(|(index, _)| visible(index))
                .map(|(index, source)| (*index, source.clone()))
                .collect(),
            action_headers: presentation
                .action_headers
                .iter()
                .filter(|(index, _)| visible(index))
                .map(|(index, header)| (*index, header.clone()))
                .collect(),
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
    pub(super) fn agent_streaming_say_projection_context(
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
            .agent_pane_screen_lineage(&result.pane_id, &result.conversation_id)
            == Some(result.installed_lineage);
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
            self.integration
                .runtime_metrics_mut()
                .record_agent_streaming_projection_result(false, !screen_lineage_current);
            return Ok(false);
        }
        let screen_is_unchanged = !self
            .presentation
            .agent_shell_output_previews
            .contains_key(&result.pane_id)
            && self
                .agent_pane_screen(&result.pane_id)
                .is_some_and(|screen| screen == &result.screen);
        let installed_lineage = if screen_is_unchanged {
            result.installed_lineage
        } else {
            self.update_agent_streaming_screen(
                &result.pane_id,
                &result.conversation_id,
                result.screen,
            )?
        };
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
        presentation.projected_lineage = Some(installed_lineage);
        if screen_is_unchanged {
            self.integration
                .runtime_metrics_mut()
                .record_agent_streaming_projection_noop();
        } else {
            self.integration
                .runtime_metrics_mut()
                .record_agent_streaming_projection_result(true, false);
        }
        Ok(!screen_is_unchanged)
    }
}
