//! Sender-side presentation acceptance and provisional message retirement.
//!
//! Message-service acceptance remains the authority for sender rows. This owner
//! shares the existing conversation screen and source renderer with replay;
//! rejected siblings are removed without erasing accepted predecessors.

use super::*;

impl RuntimeSessionService {
    /// Appends one accepted sender-side message row and retains its strict
    /// sent-only source for replay at a later geometry or theme.
    pub(super) fn append_accepted_outbound_message_presentation(
        &mut self,
        pane_id: &str,
        action_identity: &str,
        source: &RuntimeStreamingMessageSource,
    ) -> Result<()> {
        let projection = streaming_outbound_message_projection_with_theme(
            source,
            self.agent_terminal_markdown_frame_width(pane_id)?,
            self.agent_terminal_markdown_terminal_width(pane_id)?,
            &self.presentation.settings.ui_theme,
        );
        let persisted_source = sent_peer_message_presentation_source(
            action_identity,
            source.recipient_label.as_str(),
            source.text.as_str(),
            source.content_type.as_str(),
            source.direct_parent,
        );
        let entry = source.entry.clone().unwrap_or_else(|| {
            self.presentation_entry(
                pane_id,
                mez_core::presentation::PresentationOccurrenceBasis::LocalAcceptance,
            )
        });
        self.with_presentation_entry(pane_id, entry, |service| {
            service.append_agent_terminal_rendered_lines_with_sources_to_buffer(
                pane_id,
                projection.style,
                &projection.rendered_lines,
                &projection.copy_lines,
                Some((
                    persisted_source.as_str(),
                    AGENT_PRESENTATION_PEER_MESSAGE_CONTENT_TYPE,
                )),
                if runtime_peer_message_presentation_is_markdown(Some(&source.content_type)) {
                    crate::host::terminal::source_copy::CopySourcePlan::WholeEntry(
                        source.text.trim_end_matches(['\r', '\n']),
                    )
                } else {
                    crate::host::terminal::source_copy::CopySourcePlan::Declared
                },
            )
        })
    }

    /// Persists an accepted sender-side source without writing another live row.
    fn persist_accepted_outbound_message_presentation(
        &mut self,
        pane_id: &str,
        action_identity: &str,
        source: &RuntimeStreamingMessageSource,
    ) -> Result<()> {
        let projection = streaming_outbound_message_projection_with_theme(
            source,
            self.agent_terminal_markdown_frame_width(pane_id)?,
            self.agent_terminal_markdown_terminal_width(pane_id)?,
            &self.presentation.settings.ui_theme,
        );
        let persisted_source = sent_peer_message_presentation_source(
            action_identity,
            source.recipient_label.as_str(),
            source.text.as_str(),
            source.content_type.as_str(),
            source.direct_parent,
        );
        self.persist_streamed_presentation(
            pane_id,
            source.entry.clone(),
            (
                vec![
                    projection.style.persistence_name().to_string();
                    projection.rendered_lines.len()
                ],
                projection
                    .rendered_lines
                    .iter()
                    .map(|line| line.display.clone())
                    .collect(),
                projection.copy_lines,
            ),
            Some((
                persisted_source.as_str(),
                AGENT_PRESENTATION_PEER_MESSAGE_CONTENT_TYPE,
            )),
        );
        Ok(())
    }

    /// Settles one accepted model-authored outbound message presentation.
    ///
    /// Message-service acceptance is the sole authority for this sender row.
    /// An exact streamed source keeps its installed screen in place; an
    /// accepted non-streaming action appends the same live-only rendition.
    /// Neither path persists a sender source or changes receiver receipts.
    pub(crate) fn settle_accepted_outbound_message_preview(
        &mut self,
        pane_id: &str,
        turn_id: &str,
        action_index: usize,
        action: &AgentAction,
    ) -> Result<()> {
        let AgentActionPayload::SendMessage {
            recipient,
            content_type,
            payload,
            ..
        } = &action.payload
        else {
            return Err(MezError::invalid_args(
                "outbound message settlement requires a send_message action",
            ));
        };
        let content_type = mez_agent::normalize_maap_message_content_type(content_type);
        let direct_parent = crate::runtime::runtime_message_recipient(recipient)
            .ok()
            .is_some_and(|recipient| {
                self.runtime_outbound_recipient_is_direct_parent(
                    &format!("agent-{pane_id}"),
                    &recipient,
                )
            });
        let fallback_recipient_label = crate::runtime::runtime_message_recipient(recipient)
            .ok()
            .map(|parsed_recipient| {
                self.runtime_outbound_recipient_display_label(&parsed_recipient, recipient)
            })
            .unwrap_or_else(|| recipient.clone());
        if !runtime_peer_message_presentation_is_visible(
            self.agent_peer_message_log_mode(),
            Some(&content_type),
        ) {
            return Ok(());
        }
        let identity = (pane_id.to_string(), turn_id.to_string(), action.id.clone());
        if self
            .presentation
            .agent_settled_outbound_message_actions
            .contains(&identity)
        {
            return Ok(());
        }
        let streamed_source = self
            .presentation
            .agent_streaming_say_presentations
            .get(pane_id)
            .and_then(|presentation| {
                (presentation.turn_id == turn_id)
                    .then(|| presentation.outbound_messages.get(&action_index))
                    .flatten()
            })
            .filter(|source| {
                source.complete
                    && source.recipient == *recipient
                    && source.content_type == content_type
                    && source.text == *payload
            })
            .cloned();
        if let Some(source) = streamed_source {
            // Completion verified the complete provider source. Keep the
            // cumulative owner until every outbound action from this response
            // settles, so a later rejected sibling can still restore the
            // shared baseline instead of leaving a partial sender transcript.
            if self
                .presentation
                .agent_streaming_say_presentations
                .get(pane_id)
                .is_some_and(|presentation| presentation.layout_retired)
            {
                self.append_accepted_outbound_message_presentation(
                    pane_id,
                    action.id.as_str(),
                    &source,
                )?;
            } else {
                self.persist_accepted_outbound_message_presentation(
                    pane_id,
                    action.id.as_str(),
                    &source,
                )?;
            }
            self.presentation
                .agent_settled_outbound_message_actions
                .insert(identity);
            return Ok(());
        }
        let retained_entry = self
            .agent_turn_executions()
            .get(turn_id)
            .filter(|execution| {
                execution
                    .response
                    .action_batch
                    .as_ref()
                    .and_then(|batch| batch.actions.get(action_index))
                    == Some(action)
            })
            .map(|execution| {
                self.retained_provider_component_entry(
                    pane_id,
                    execution,
                    Some(action_index),
                    "message",
                )
            })
            .transpose()?
            .flatten();
        let source = RuntimeStreamingMessageSource {
            recipient: recipient.clone(),
            entry: Some(retained_entry.unwrap_or_else(|| {
                self.presentation_entry(
                    pane_id,
                    mez_core::presentation::PresentationOccurrenceBasis::LocalAcceptance,
                )
            })),
            recipient_label: fallback_recipient_label,
            direct_parent,
            content_type,
            text: payload.clone(),
            complete: true,
        };
        self.append_accepted_outbound_message_presentation(pane_id, action.id.as_str(), &source)?;
        self.presentation
            .agent_settled_outbound_message_actions
            .insert(identity);
        Ok(())
    }

    /// Finalizes each completed outbound preview after all message actions settle.
    ///
    /// Rejected sources are removed before rebuilding the cumulative screen, so
    /// an accepted sibling remains visible. This remains presentation-only and
    /// does not create sender persistence. An accepted unchanged projection is
    /// successful settlement, not a delivery failure; only genuinely rejected
    /// work fails this boundary, and no outcome repeats the settled send.
    pub(crate) fn finalize_settled_outbound_message_previews(
        &mut self,
        pane_id: &str,
        turn_id: &str,
        execution: &mez_agent::AgentTurnExecution,
    ) -> Result<()> {
        let Some(presentation) = self
            .presentation
            .agent_streaming_say_presentations
            .get(pane_id)
            .filter(|presentation| presentation.turn_id == turn_id)
        else {
            return Ok(());
        };
        if presentation.outbound_messages.is_empty() {
            return Ok(());
        }
        let settled = execution
            .response
            .action_batch
            .as_ref()
            .is_some_and(|batch| {
                presentation.outbound_messages.keys().all(|action_index| {
                    batch.actions.get(*action_index).is_some_and(|action| {
                        matches!(action.payload, AgentActionPayload::SendMessage { .. })
                            && execution
                                .action_results
                                .iter()
                                .any(|result| result.action_id == action.id && result.is_terminal())
                    })
                })
            });
        if !settled {
            return Ok(());
        }
        let rejected = execution
            .response
            .action_batch
            .as_ref()
            .map_or_else(Vec::new, |batch| {
                presentation
                    .outbound_messages
                    .keys()
                    .filter(|action_index| {
                        batch.actions.get(**action_index).is_some_and(|action| {
                            execution.action_results.iter().any(|result| {
                                result.action_id == action.id
                                    && result.status != mez_agent::ActionStatus::Succeeded
                            })
                        })
                    })
                    .copied()
                    .collect::<Vec<_>>()
            });
        if !rejected.is_empty() {
            let presentation = self
                .presentation
                .agent_streaming_say_presentations
                .get_mut(pane_id)
                .ok_or_else(|| {
                    MezError::invalid_state("streaming outbound presentation disappeared")
                })?;
            for action_index in rejected {
                presentation.outbound_messages.remove(&action_index);
            }
            presentation.revision = presentation.revision.wrapping_add(1);
            presentation.projected_revision = None;
        }
        let has_source = self
            .presentation
            .agent_streaming_say_presentations
            .get(pane_id)
            .is_some_and(|presentation| {
                presentation.rationale.is_some()
                    || !presentation.actions.is_empty()
                    || !presentation.outbound_messages.is_empty()
                    || !presentation.shell_commands.is_empty()
                    || !presentation.shell_summaries.is_empty()
                    || !presentation.action_headers.is_empty()
            });
        if !has_source {
            self.discard_agent_streaming_say_presentation(pane_id, Some(turn_id))?;
            return Ok(());
        }
        if let Some(work) = self.take_agent_streaming_say_projection_work(pane_id, turn_id)? {
            let result = Self::build_agent_streaming_say_projection(work)?;
            if !self
                .apply_agent_streaming_say_projection_outcome(result)?
                .is_accepted()
            {
                return Err(MezError::invalid_state(
                    "settled outbound preview projection was rejected",
                ));
            }
        }
        self.finalize_agent_streaming_say_presentation(pane_id, Some(turn_id))?;
        Ok(())
    }
}
