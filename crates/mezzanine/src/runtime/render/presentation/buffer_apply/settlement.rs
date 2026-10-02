//! Validated response settlement, isolated from ordinary pane writes.
//!
//! The service remains the sole state owner; exact source and lineage checks
//! gate promotion, rollback, and durable action-order publication.

use super::*;

impl RuntimeSessionService {
    /// Publishes or rolls back one response captured by the reconciliation owner.
    pub(super) fn settle_captured_streaming_presentation(
        &mut self,
        pane_id: &str,
        turn_id: &str,
        execution: &mez_agent::AgentTurnExecution,
        presentation: RuntimeStreamingSayPresentation,
    ) -> Result<crate::runtime::render::RuntimeStreamingSayCompletionReconciliation> {
        let conversation_matches = self
            .agent_shell_store()
            .get(pane_id)
            .is_some_and(|session| session.session_id == presentation.conversation_id);
        let screen_is_owned = self
            .agent_pane_screen_lineage(pane_id, &presentation.conversation_id)
            == Some(presentation.installed_lineage);
        let batch = execution.response.action_batch.as_ref();
        // A complete action preview may arrive without optional rationale
        // fragments. Do not promote it ahead of a validated rationale: restore
        // only our provisional screen and let the ordinary presenter append
        // the complete batch in rationale-then-action order.
        if presentation.rationale.is_none()
            && batch.is_some_and(|batch| !batch.rationale.trim().is_empty())
        {
            if presentation.turn_id == turn_id && conversation_matches && screen_is_owned {
                self.update_agent_streaming_screen(
                    pane_id,
                    &presentation.conversation_id,
                    presentation.baseline_screen.as_ref().clone(),
                )?;
                self.integration
                    .runtime_metrics_mut()
                    .record_agent_streaming_settlement_screen_change(false);
            }
            return Ok(Default::default());
        }
        let incomplete_source =
            streaming_reconciliation::streaming_source_is_incomplete(&presentation);
        let matches = presentation.turn_id == turn_id
            && conversation_matches
            && batch.is_some_and(|batch| {
                streaming_reconciliation::streaming_sources_match_batch(&presentation, batch)
            });
        let rejected_header = presentation.turn_id == turn_id
            && conversation_matches
            && presentation.action_headers.len() == 1
            && batch.is_some_and(|batch| {
                presentation
                    .action_headers
                    .keys()
                    .next()
                    .is_some_and(|index| {
                        batch.actions.get(*index).is_some_and(|action| {
                            execution.action_results.iter().any(|result| {
                                result.action_id == action.id
                                    && !matches!(
                                        result.status,
                                        mez_agent::ActionStatus::Running
                                            | mez_agent::ActionStatus::Succeeded
                                    )
                            })
                        })
                    })
            });
        if !matches || rejected_header {
            if !matches {
                if incomplete_source {
                    self.integration
                        .runtime_metrics_mut()
                        .record_agent_streaming_settlement_incomplete_source();
                } else {
                    self.integration
                        .runtime_metrics_mut()
                        .record_agent_streaming_settlement_rejection(
                            !conversation_matches
                                || !screen_is_owned
                                || presentation.turn_id != turn_id,
                        );
                }
            }
            // Replace a changed header, or remove a rejected one, privately
            // while retaining every exact rationale/say sibling in action order.
            let context = self.agent_streaming_say_projection_context(pane_id).ok();
            if presentation.turn_id == turn_id
                && conversation_matches
                && screen_is_owned
                && presentation.projected_revision == Some(presentation.revision)
                && presentation.projected_context == context
                && presentation.projected_lineage == Some(presentation.installed_lineage)
                && (!presentation.actions.is_empty() || presentation.projected_rationale.is_some())
                && presentation.rationale.as_ref().is_none_or(|source| {
                    source.complete && batch.is_some_and(|batch| source.text == batch.rationale)
                })
                && presentation.action_headers.len() == 1
                && presentation.outbound_messages.is_empty()
                && presentation.shell_commands.is_empty()
                && presentation.shell_summaries.is_empty()
                && let Some(batch) = batch
                && batch.actions.len() == presentation.actions.len() + 1
                && let Some((&header_index, _)) = presentation.action_headers.iter().next()
                && presentation.actions.iter().all(|(index, source)| {
                    batch.actions.get(*index).is_some_and(|action| {
                        matches!(&action.payload, mez_agent::AgentActionPayload::Say {
                            status,
                            text,
                            content_type,
                        } if source.complete
                            && status == &source.status
                            && text == &source.text
                            && mez_agent::normalize_agent_output_content_type(Some(content_type))
                                == source.content_type
                            && (source.status == mez_agent::SayStatus::Progress
                                || (source.status == mez_agent::SayStatus::Final
                                    && *index > header_index)))
                            && execution.action_results.iter().any(|result| {
                                result.action_id == action.id
                                    && result.status == mez_agent::ActionStatus::Succeeded
                            })
                    })
                })
                && let Some(accepted_action) = batch.actions.get(header_index)
                && let Some(accepted_header) =
                    agent_action_execution_display_header(accepted_action)
                && execution.action_results.iter().any(|result| {
                    result.action_id == accepted_action.id
                        && (rejected_header
                            || matches!(
                                result.status,
                                mez_agent::ActionStatus::Running
                                    | mez_agent::ActionStatus::Succeeded
                            ))
                })
                && let Some(context) = context
            {
                let work = crate::runtime::RuntimeStreamingSayProjectionWork {
                    pane_id: pane_id.to_string(),
                    turn_id: turn_id.to_string(),
                    response_index: presentation.response_index,
                    conversation_id: presentation.conversation_id.clone(),
                    revision: presentation.revision,
                    installed_lineage: presentation.installed_lineage,
                    baseline_screen: presentation.baseline_screen.clone(),
                    rationale: presentation.rationale.clone(),
                    actions: presentation.actions.clone(),
                    outbound_messages: std::collections::BTreeMap::new(),
                    shell_commands: std::collections::BTreeMap::new(),
                    shell_summaries: std::collections::BTreeMap::new(),
                    action_headers: if rejected_header {
                        std::collections::BTreeMap::new()
                    } else {
                        std::collections::BTreeMap::from([(
                            header_index,
                            mez_agent::StreamingActionHeader::Action {
                                action: Box::new(accepted_action.clone()),
                            },
                        )])
                    },
                    thinking_enabled: context.thinking_enabled,
                    shell_classification: context.shell_classification,
                    presentation_columns: context.presentation_columns,
                    frame_width: context.frame_width,
                    table_width: context.table_width,
                    ui_theme: context.ui_theme.clone(),
                    screen_size: context.screen_size,
                };
                let pending_indices = presentation
                    .actions
                    .iter()
                    .filter(|(_, source)| {
                        source.status == mez_agent::SayStatus::Final
                            && execution.terminal_state != mez_agent::AgentTurnState::Completed
                    })
                    .map(|(index, _)| *index)
                    .collect::<std::collections::BTreeSet<_>>();
                let without_final_screen = if pending_indices.is_empty() {
                    None
                } else {
                    let mut without_final = work.clone();
                    without_final
                        .actions
                        .retain(|index, _| !pending_indices.contains(index));
                    Some(Self::build_agent_streaming_say_projection(without_final)?.screen)
                };
                let ordered_screen = (!pending_indices.is_empty()
                    && batch.actions.iter().enumerate().any(|(index, _)| {
                        !pending_indices.contains(&index)
                            && pending_indices.iter().any(|pending| *pending < index)
                    }))
                .then(|| Self::build_ordered_pending_final_screen(work.clone(), &pending_indices))
                .transpose()?;
                let replacement = Self::build_agent_streaming_say_projection(work)?;
                let installed_lineage = self.update_agent_streaming_screen(
                    pane_id,
                    &presentation.conversation_id,
                    ordered_screen.unwrap_or(replacement.screen),
                )?;
                self.integration
                    .runtime_metrics_mut()
                    .record_agent_streaming_settlement_screen_change(true);
                let mut promoted = std::collections::BTreeSet::new();
                if let (Some(source), Some(row)) = (
                    presentation.rationale.as_ref(),
                    replacement.projected_rationale.as_ref(),
                ) {
                    self.persist_agent_presentation_entry(
                        pane_id,
                        vec![row.style.clone(); row.rendered_lines.len()],
                        row.rendered_lines.clone(),
                        row.copy_lines.clone(),
                        String::new(),
                        Some((
                            source.text.as_str(),
                            AGENT_PRESENTATION_THINKING_CONTENT_TYPE,
                        )),
                    );
                    promoted.insert(STREAMED_RATIONALE_PRESENTED_MARKER);
                    self.integration
                        .runtime_metrics_mut()
                        .record_agent_streaming_settled_component("rationale");
                }
                if !rejected_header {
                    self.presentation.agent_accepted_streaming_headers.insert(
                        (
                            pane_id.to_string(),
                            turn_id.to_string(),
                            accepted_action.id.clone(),
                        ),
                        accepted_header.clone(),
                    );
                }
                let rendered_lines = wrap_rich_text_line_to_width_with_source_ranges_hard(
                    agent_action_execution_rendered_line(
                        &accepted_header,
                        &self.presentation.settings.ui_theme,
                    ),
                    context.frame_width,
                )
                .into_iter()
                .map(|wrapped| wrapped.line.display)
                .collect::<Vec<_>>();
                for index in 0..batch.actions.len() {
                    if index == header_index {
                        if !rejected_header {
                            self.persist_agent_presentation_entry(
                                pane_id,
                                vec![
                                    AgentTerminalPresentationStyle::Status
                                        .persistence_name()
                                        .to_string();
                                    rendered_lines.len()
                                ],
                                rendered_lines.clone(),
                                Vec::new(),
                                String::new(),
                                Some((
                                    &accepted_header,
                                    AGENT_PRESENTATION_ACTION_HEADER_CONTENT_TYPE,
                                )),
                            );
                            self.integration
                                .runtime_metrics_mut()
                                .record_agent_streaming_settled_component("header");
                        }
                    } else if let (Some(source), Some(row)) = (
                        presentation.actions.get(&index),
                        replacement
                            .projected_actions
                            .iter()
                            .find(|row| row.action_index == index),
                    ) {
                        if pending_indices.contains(&index) {
                            continue;
                        }
                        self.persist_agent_presentation_entry(
                            pane_id,
                            vec![row.style.clone(); row.rendered_lines.len()],
                            row.rendered_lines.clone(),
                            row.copy_lines.clone(),
                            String::new(),
                            Some((source.text.as_str(), source.content_type.as_str())),
                        );
                        promoted.insert(index);
                        self.integration
                            .runtime_metrics_mut()
                            .record_agent_streaming_settled_component("say");
                    }
                }
                self.presentation
                    .agent_promoted_streaming_say_actions
                    .insert((pane_id.to_string(), turn_id.to_string()), promoted.clone());
                if let Some(without_final_screen) = without_final_screen {
                    let finals = pending_indices
                        .iter()
                        .filter_map(|index| {
                            let source = presentation.actions.get(index)?;
                            let row = replacement
                                .projected_actions
                                .iter()
                                .find(|row| row.action_index == *index)?;
                            Some((*index, source.clone(), row.clone()))
                        })
                        .collect();
                    self.presentation.agent_pending_final_say_previews.insert(
                        pane_id.to_string(),
                        crate::runtime::render::RuntimePendingFinalSayPreview {
                            turn_id: turn_id.to_string(),
                            conversation_id: presentation.conversation_id.clone(),
                            finals,
                            installed_lineage,
                            without_final_screen: std::sync::Arc::new(without_final_screen),
                        },
                    );
                }
                return Ok(
                    crate::runtime::render::RuntimeStreamingSayCompletionReconciliation {
                        promoted_action_indices: promoted
                            .into_iter()
                            .filter(|index| *index != STREAMED_RATIONALE_PRESENTED_MARKER)
                            .collect(),
                    },
                );
            }
            self.presentation
                .agent_promoted_streaming_say_actions
                .remove(&(pane_id.to_string(), turn_id.to_string()));
            if conversation_matches && screen_is_owned {
                self.update_agent_streaming_screen(
                    pane_id,
                    &presentation.conversation_id,
                    presentation.baseline_screen.as_ref().clone(),
                )?;
                self.integration
                    .runtime_metrics_mut()
                    .record_agent_streaming_settlement_screen_change(false);
            }
            return Ok(Default::default());
        }

        let projection_context_is_current = self
            .agent_streaming_say_projection_context(pane_id)
            .is_ok_and(|context| presentation.projected_context.as_ref() == Some(&context));
        let current_projected_actions = presentation.projected_actions.as_ref().filter(|_| {
            presentation.projected_revision == Some(presentation.revision)
                && projection_context_is_current
                && presentation.projected_lineage == Some(presentation.installed_lineage)
        });
        if !screen_is_owned {
            self.integration
                .runtime_metrics_mut()
                .record_agent_streaming_settlement_rejection(true);
        } else if current_projected_actions.is_none() {
            self.integration
                .runtime_metrics_mut()
                .record_agent_streaming_settlement_projection_miss();
        }
        // Validated command intent is a permanent log component, not proof of
        // shell readiness or dispatch. Preserve its exact installed projection
        // independently of whether execution is pending or already succeeded.
        let command_can_promote = screen_is_owned && batch.is_some_and(|batch| {
            presentation.rationale.as_ref().is_none_or(|rationale| {
                rationale.complete && rationale.text == batch.rationale
            })
                && presentation.actions.values().all(|source| {
                    source.complete && source.status == mez_agent::SayStatus::Progress
                })
                && !presentation.shell_commands.is_empty()
                && presentation.outbound_messages.is_empty()
                && presentation.action_headers.iter().all(|(index, header)| {
                    batch.actions.get(*index).is_some_and(|action| {
                        agent_action_execution_display_header(action).is_some_and(|accepted| {
                            streaming_action_execution_display_header(header) == accepted
                        }) && execution.action_results.iter().any(|result| {
                            result.action_id == action.id
                                && matches!(result.status, mez_agent::ActionStatus::Running | mez_agent::ActionStatus::Succeeded)
                        })
                    })
                })
                && !self.agent_verbose_enabled(pane_id)
                && batch.actions.len()
                    == presentation.shell_commands.len()
                        + presentation.actions.len()
                        + presentation.action_headers.len()
                && execution.action_results.len() == batch.actions.len()
                && presentation.actions.iter().all(|(action_index, source)| {
                    batch.actions.get(*action_index).is_some_and(|action| {
                        matches!(&action.payload, mez_agent::AgentActionPayload::Say {
                            status: mez_agent::SayStatus::Progress,
                            text,
                            ..
                        } if text == &source.text)
                    }) && execution.action_results.get(*action_index).is_some_and(|result| {
                        result.action_id == batch.actions[*action_index].id
                            && result.status == mez_agent::ActionStatus::Succeeded
                    })
                })
                && presentation.shell_commands.iter().all(|(action_index, source)| {
                    source.complete
                        && !bounded_command_preview_source(&source.text).truncated
                        && batch.actions.get(*action_index).is_some_and(|action| {
                            matches!(
                                &action.payload,
                                mez_agent::AgentActionPayload::ShellCommand { command, .. }
                                    if command == &source.text
                            )
                        })
                        && execution.action_results.get(*action_index).is_some_and(|result| {
                            result.action_id == batch.actions[*action_index].id
                                && matches!(result.status, mez_agent::ActionStatus::Running | mez_agent::ActionStatus::Succeeded)
                        })
                })
                && presentation
                    .shell_summaries
                    .iter()
                    .all(|(action_index, source)| {
                        source.complete
                            && batch.actions.get(*action_index).is_some_and(|action| {
                                matches!(
                                    &action.payload,
                                    mez_agent::AgentActionPayload::ShellCommand { summary, .. }
                                        if summary == &source.text
                                )
                            })
                    })
                && current_projected_actions.is_some_and(|projected| {
                    projected.len() + presentation.action_headers.len() == batch.actions.len()
                        && projected.iter().all(|projection| {
                            match projection.kind {
                                crate::runtime::render::RuntimeStreamingSayProjectedActionKind::ShellCommand { truncated: false } =>
                                    presentation.shell_commands.contains_key(&projection.action_index),
                                crate::runtime::render::RuntimeStreamingSayProjectedActionKind::Say =>
                                    presentation.actions.contains_key(&projection.action_index),
                                _ => false,
                            }
                        })
                })
                && presentation.projected_context.as_ref().is_some_and(|context| {
                    !context.thinking_enabled || presentation.projected_rationale.is_some()
                })
        });

        // A rationale with no other streamed components already occupies its
        // final rows. Preserve that exact projection through completion and
        // record only its semantic source; Complete has no action display row.
        let rationale_only_can_promote = screen_is_owned
            && current_projected_actions.is_some_and(Vec::is_empty)
            && presentation.projected_rationale.is_some()
            && presentation
                .rationale
                .as_ref()
                .is_some_and(|source| source.complete)
            && presentation.actions.is_empty()
            && presentation.outbound_messages.is_empty()
            && presentation.shell_commands.is_empty()
            && presentation.shell_summaries.is_empty()
            && presentation.action_headers.is_empty()
            && batch.is_some_and(|batch| {
                !batch.actions.is_empty()
                    && batch.actions.iter().all(|action| {
                        matches!(action.payload, mez_agent::AgentActionPayload::Complete)
                    })
            });
        if rationale_only_can_promote
            && let (Some(rationale), Some(projection)) = (
                presentation.rationale.as_ref(),
                presentation.projected_rationale.as_ref(),
            )
        {
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
            self.presentation
                .agent_promoted_streaming_say_actions
                .insert(
                    (pane_id.to_string(), turn_id.to_string()),
                    std::collections::BTreeSet::from([STREAMED_RATIONALE_PRESENTED_MARKER]),
                );
            self.integration
                .runtime_metrics_mut()
                .record_agent_streaming_settled_component("rationale");
            return Ok(Default::default());
        }

        // An accepted action can claim its exact rendered header. The
        // provider preview alone is not proof of execution: keep the handoff
        // separate from action-index promotion until the execution presenter
        // consumes it and persists the accepted header.
        let settled_header = screen_is_owned
            && current_projected_actions.is_some_and(|projected| {
                projected.len() == presentation.actions.len()
                    && projected.iter().all(|row| {
                        matches!(
                            row.kind,
                            crate::runtime::render::RuntimeStreamingSayProjectedActionKind::Say
                        ) && presentation.actions.contains_key(&row.action_index)
                    })
            })
            && presentation.actions.iter().all(|(index, source)| {
                source.complete
                    && (source.status == mez_agent::SayStatus::Progress
                        || (source.status == mez_agent::SayStatus::Final
                            && presentation
                                .action_headers
                                .keys()
                                .next()
                                .is_some_and(|header_index| index > header_index)))
            })
            && presentation.outbound_messages.is_empty()
            && presentation.shell_commands.is_empty()
            && presentation.shell_summaries.is_empty()
            && !presentation.action_headers.is_empty()
            && batch.is_some_and(|batch| {
                batch.actions.len()
                    == presentation.actions.len() + presentation.action_headers.len()
            });
        if settled_header
            && let Some(batch) = batch
            && presentation.action_headers.iter().all(|(index, header)| {
                batch.actions.get(*index).is_some_and(|action| {
                    agent_action_execution_display_header(action).is_some_and(|static_header| {
                        streaming_action_execution_display_header(header) == static_header
                    }) && execution.action_results.iter().any(|result| {
                        result.action_id == action.id
                            && matches!(
                                result.status,
                                mez_agent::ActionStatus::Running
                                    | mez_agent::ActionStatus::Succeeded
                            )
                    })
                })
            })
            && presentation.actions.iter().all(|(index, _)| {
                batch.actions.get(*index).is_some_and(|action| {
                    matches!(action.payload, mez_agent::AgentActionPayload::Say { .. })
                        && execution.action_results.iter().any(|result| {
                            result.action_id == action.id
                                && result.status == mez_agent::ActionStatus::Succeeded
                        })
                })
            })
        {
            let pending_finals = presentation
                .actions
                .iter()
                .filter(|(_, source)| {
                    source.status == mez_agent::SayStatus::Final
                        && execution.terminal_state != mez_agent::AgentTurnState::Completed
                })
                .map(|(&index, source)| {
                    let projected = current_projected_actions
                        .and_then(|rows| rows.iter().find(|row| row.action_index == index))
                        .ok_or_else(|| {
                            MezError::invalid_state("pending final projection is unavailable")
                        })?;
                    Ok((index, source.clone(), projected.clone()))
                })
                .collect::<Result<Vec<_>>>()?;
            let pending_final_preview = if !pending_finals.is_empty() {
                let context = presentation.projected_context.as_ref().ok_or_else(|| {
                    MezError::invalid_state("pending final render context is unavailable")
                })?;
                let mut actions = presentation.actions.clone();
                for (index, _, _) in &pending_finals {
                    actions.remove(index);
                }
                let work = crate::runtime::RuntimeStreamingSayProjectionWork {
                    pane_id: pane_id.to_string(),
                    turn_id: turn_id.to_string(),
                    response_index: presentation.response_index,
                    conversation_id: presentation.conversation_id.clone(),
                    revision: presentation.revision,
                    installed_lineage: presentation.installed_lineage,
                    baseline_screen: presentation.baseline_screen.clone(),
                    rationale: presentation.rationale.clone(),
                    actions,
                    outbound_messages: presentation.outbound_messages.clone(),
                    shell_commands: presentation.shell_commands.clone(),
                    shell_summaries: presentation.shell_summaries.clone(),
                    action_headers: presentation.action_headers.clone(),
                    thinking_enabled: context.thinking_enabled,
                    shell_classification: context.shell_classification,
                    presentation_columns: context.presentation_columns,
                    frame_width: context.frame_width,
                    table_width: context.table_width,
                    ui_theme: context.ui_theme.clone(),
                    screen_size: context.screen_size,
                };
                let pending_indices = pending_finals
                    .iter()
                    .map(|(index, _, _)| *index)
                    .collect::<std::collections::BTreeSet<_>>();
                let ordered_screen = if batch.actions.iter().enumerate().any(|(index, _)| {
                    !pending_indices.contains(&index)
                        && pending_indices.iter().any(|pending| *pending < index)
                }) {
                    let mut ordered_work = work.clone();
                    ordered_work.actions = presentation.actions.clone();
                    Some(Self::build_ordered_pending_final_screen(
                        ordered_work,
                        &pending_indices,
                    )?)
                } else {
                    None
                };
                let without_final_screen = Self::build_agent_streaming_say_projection(work)?.screen;
                let installed_lineage = if let Some(screen) = ordered_screen {
                    self.update_agent_streaming_screen(
                        pane_id,
                        &presentation.conversation_id,
                        screen,
                    )?
                } else {
                    presentation.installed_lineage
                };
                Some(crate::runtime::render::RuntimePendingFinalSayPreview {
                    turn_id: turn_id.to_string(),
                    conversation_id: presentation.conversation_id.clone(),
                    finals: pending_finals,
                    installed_lineage,
                    without_final_screen: std::sync::Arc::new(without_final_screen),
                })
            } else {
                None
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
            if presentation.projected_rationale.is_some() {
                promoted.insert(STREAMED_RATIONALE_PRESENTED_MARKER);
                self.integration
                    .runtime_metrics_mut()
                    .record_agent_streaming_settled_component("rationale");
            }
            // Persist in the projector's action-index order, not grouped by
            // component kind; replay must reproduce the installed screen.
            for index in 0..batch.actions.len() {
                if presentation.action_headers.contains_key(&index) {
                    let action = &batch.actions[index];
                    let static_header =
                        agent_action_execution_display_header(action).ok_or_else(|| {
                            MezError::invalid_state("validated streaming header disappeared")
                        })?;
                    let frame_width = self.agent_terminal_markdown_frame_width(pane_id)?;
                    let rendered_lines = wrap_rich_text_line_to_width_with_source_ranges_hard(
                        agent_action_execution_rendered_line(
                            &static_header,
                            &self.presentation.settings.ui_theme,
                        ),
                        frame_width,
                    )
                    .into_iter()
                    .map(|wrapped| wrapped.line.display)
                    .collect::<Vec<_>>();
                    self.persist_agent_presentation_entry(
                        pane_id,
                        vec![
                            AgentTerminalPresentationStyle::Status
                                .persistence_name()
                                .to_string();
                            rendered_lines.len()
                        ],
                        rendered_lines.clone(),
                        Vec::new(),
                        String::new(),
                        Some((
                            &static_header,
                            AGENT_PRESENTATION_ACTION_HEADER_CONTENT_TYPE,
                        )),
                    );
                    self.integration
                        .runtime_metrics_mut()
                        .record_agent_streaming_settled_component("header");
                    self.presentation.agent_accepted_streaming_headers.insert(
                        (pane_id.to_string(), turn_id.to_string(), action.id.clone()),
                        static_header,
                    );
                } else if let Some(row) = current_projected_actions
                    .and_then(|rows| rows.iter().find(|row| row.action_index == index))
                    && let Some(source) = presentation.actions.get(&index)
                {
                    if pending_final_preview.as_ref().is_some_and(|preview| {
                        preview
                            .finals
                            .iter()
                            .any(|(candidate, _, _)| *candidate == index)
                    }) {
                        continue;
                    }
                    self.persist_agent_presentation_entry(
                        pane_id,
                        vec![row.style.clone(); row.rendered_lines.len()],
                        row.rendered_lines.clone(),
                        row.copy_lines.clone(),
                        String::new(),
                        Some((source.text.as_str(), source.content_type.as_str())),
                    );
                    promoted.insert(index);
                    self.integration
                        .runtime_metrics_mut()
                        .record_agent_streaming_settled_component("say");
                }
            }
            self.presentation
                .agent_promoted_streaming_say_actions
                .insert((pane_id.to_string(), turn_id.to_string()), promoted.clone());
            if let Some(preview) = pending_final_preview {
                self.presentation
                    .agent_pending_final_say_previews
                    .insert(pane_id.to_string(), preview);
            }
            return Ok(
                crate::runtime::render::RuntimeStreamingSayCompletionReconciliation {
                    promoted_action_indices: promoted
                        .into_iter()
                        .filter(|index| *index != STREAMED_RATIONALE_PRESENTED_MARKER)
                        .collect(),
                },
            );
        }

        // Shell commands and rationale-only responses have additional
        // completion-time ordering rules. Exact rationale-plus-say projections
        // already use the static renderers and can promote both components.
        let rationale_requires_static = presentation.rationale.is_some()
            && presentation.actions.is_empty()
            && presentation.shell_commands.is_empty();
        if rationale_requires_static
            || (!presentation.action_headers.is_empty() && !command_can_promote)
            || (!presentation.shell_commands.is_empty() && !command_can_promote)
        {
            let retained_rationale = if let Some(batch) = batch {
                self.retain_validated_streaming_rationale_for_fallback(
                    pane_id,
                    turn_id,
                    &presentation,
                    batch,
                    execution,
                )?
            } else {
                false
            };
            if !retained_rationale {
                self.presentation
                    .agent_promoted_streaming_say_actions
                    .remove(&(pane_id.to_string(), turn_id.to_string()));
            }
            if screen_is_owned && !retained_rationale {
                self.update_agent_streaming_screen(
                    pane_id,
                    &presentation.conversation_id,
                    presentation.baseline_screen.as_ref().clone(),
                )?;
                self.integration
                    .runtime_metrics_mut()
                    .record_agent_streaming_settlement_screen_change(false);
            }
            return Ok(Default::default());
        }

        let Some(projected_actions) = current_projected_actions else {
            let retained_rationale = if let Some(batch) = batch {
                self.retain_validated_streaming_rationale_for_fallback(
                    pane_id,
                    turn_id,
                    &presentation,
                    batch,
                    execution,
                )?
            } else {
                false
            };
            if !retained_rationale {
                self.presentation
                    .agent_promoted_streaming_say_actions
                    .remove(&(pane_id.to_string(), turn_id.to_string()));
            }
            if screen_is_owned && !retained_rationale {
                self.update_agent_streaming_screen(
                    pane_id,
                    &presentation.conversation_id,
                    presentation.baseline_screen.as_ref().clone(),
                )?;
                self.integration
                    .runtime_metrics_mut()
                    .record_agent_streaming_settlement_screen_change(false);
            }
            return Ok(Default::default());
        };
        let batch = batch
            .ok_or_else(|| MezError::invalid_state("validated streaming batch disappeared"))?;
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
            self.integration
                .runtime_metrics_mut()
                .record_agent_streaming_settled_component("rationale");
        }
        let mut promoted = std::collections::BTreeSet::new();
        for index in 0..batch.actions.len() {
            if presentation.action_headers.contains_key(&index) {
                let action = &batch.actions[index];
                let header = agent_action_execution_display_header(action).ok_or_else(|| {
                    MezError::invalid_state("validated streaming header disappeared")
                })?;
                let frame_width = self.agent_terminal_markdown_frame_width(pane_id)?;
                let rendered_lines = wrap_rich_text_line_to_width_with_source_ranges_hard(
                    agent_action_execution_rendered_line(
                        &header,
                        &self.presentation.settings.ui_theme,
                    ),
                    frame_width,
                )
                .into_iter()
                .map(|wrapped| wrapped.line.display)
                .collect::<Vec<_>>();
                self.persist_agent_presentation_entry(
                    pane_id,
                    vec![
                        AgentTerminalPresentationStyle::Status
                            .persistence_name()
                            .to_string();
                        rendered_lines.len()
                    ],
                    rendered_lines,
                    Vec::new(),
                    String::new(),
                    Some((&header, AGENT_PRESENTATION_ACTION_HEADER_CONTENT_TYPE)),
                );
                self.presentation.agent_accepted_streaming_headers.insert(
                    (pane_id.to_string(), turn_id.to_string(), action.id.clone()),
                    header,
                );
                self.integration
                    .runtime_metrics_mut()
                    .record_agent_streaming_settled_component("header");
                continue;
            }
            let Some(projection) = projected_actions
                .iter()
                .find(|row| row.action_index == index)
            else {
                continue;
            };
            if presentation
                .projected_context
                .as_ref()
                .is_some_and(|context| context.thinking_enabled)
                && let Some(source) = presentation.shell_summaries.get(&projection.action_index)
                && presentation
                    .rationale
                    .as_ref()
                    .map(|rationale| rationale.text.as_str())
                    != Some(source.text.as_str())
            {
                let frame_width = presentation
                    .projected_context
                    .as_ref()
                    .map(|context| context.frame_width)
                    .unwrap_or_default();
                let rendered_lines =
                    agent_thinking_display_lines_for_width(&source.text, frame_width);
                self.persist_agent_presentation_entry(
                    pane_id,
                    vec![
                        AgentTerminalPresentationStyle::Status
                            .persistence_name()
                            .to_string();
                        rendered_lines.len()
                    ],
                    rendered_lines,
                    Vec::new(),
                    String::new(),
                    Some((
                        source.text.as_str(),
                        AGENT_PRESENTATION_THINKING_CONTENT_TYPE,
                    )),
                );
            }
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
            self.integration
                .runtime_metrics_mut()
                .record_agent_streaming_settled_component(match projection.kind {
                crate::runtime::render::RuntimeStreamingSayProjectedActionKind::Say => "say",
                crate::runtime::render::RuntimeStreamingSayProjectedActionKind::ShellCommand {
                    ..
                } => "command",
            });
            promoted.insert(projection.action_index);
        }
        self.presentation
            .agent_promoted_streaming_say_actions
            .insert((pane_id.to_string(), turn_id.to_string()), promoted.clone());
        let mut presentation_indices = promoted.clone();
        presentation_indices.extend(presentation.outbound_messages.keys().copied());
        self.presentation
            .agent_promoted_streaming_say_actions
            .insert(
                (pane_id.to_string(), turn_id.to_string()),
                presentation_indices,
            );
        if !presentation.outbound_messages.is_empty() {
            // The message remains provisional, but the already persisted
            // rationale and action rows are now the immutable prefix. Keep
            // them in the rollback baseline, not in the mutable source maps:
            // resizing or rejecting a message must neither erase nor replay
            // the settled siblings.
            let context = presentation.projected_context.as_ref().ok_or_else(|| {
                MezError::invalid_state("settled streaming projection context disappeared")
            })?;
            let settled_actions = presentation
                .actions
                .iter()
                .filter(|(index, _)| promoted.contains(index))
                .map(|(index, source)| (*index, source.clone()))
                .collect();
            let settled_commands = presentation
                .shell_commands
                .iter()
                .filter(|(index, _)| promoted.contains(index))
                .map(|(index, source)| (*index, source.clone()))
                .collect();
            let prefix = Self::build_agent_streaming_say_projection(
                crate::runtime::RuntimeStreamingSayProjectionWork {
                    pane_id: pane_id.to_string(),
                    turn_id: turn_id.to_string(),
                    response_index: presentation.response_index,
                    conversation_id: presentation.conversation_id.clone(),
                    revision: presentation.revision,
                    installed_lineage: presentation.installed_lineage,
                    baseline_screen: presentation.baseline_screen.clone(),
                    rationale: presentation
                        .projected_rationale
                        .as_ref()
                        .and(presentation.rationale.as_ref())
                        .cloned(),
                    actions: settled_actions,
                    outbound_messages: std::collections::BTreeMap::new(),
                    shell_commands: settled_commands,
                    shell_summaries: presentation
                        .shell_summaries
                        .iter()
                        .filter(|(index, _)| promoted.contains(index))
                        .map(|(index, source)| (*index, source.clone()))
                        .collect(),
                    action_headers: presentation.action_headers.clone(),
                    thinking_enabled: context.thinking_enabled,
                    shell_classification: context.shell_classification,
                    presentation_columns: context.presentation_columns,
                    frame_width: context.frame_width,
                    table_width: context.table_width,
                    ui_theme: context.ui_theme.clone(),
                    screen_size: context.screen_size,
                },
            )?;
            let mut presentation = presentation;
            presentation.baseline_screen = std::sync::Arc::new(prefix.screen);
            presentation.rationale = None;
            presentation.actions.clear();
            presentation.shell_commands.clear();
            presentation.shell_summaries.clear();
            presentation.action_headers.clear();
            presentation
                .received_actions
                .retain(|index| presentation.outbound_messages.contains_key(index));
            presentation.revision = presentation.revision.wrapping_add(1);
            presentation.projected_revision = None;
            presentation.projected_actions = None;
            presentation.projected_rationale = None;
            self.presentation
                .agent_streaming_say_presentations
                .insert(pane_id.to_string(), presentation);
        }
        Ok(
            crate::runtime::render::RuntimeStreamingSayCompletionReconciliation {
                promoted_action_indices: promoted,
            },
        )
    }
}
