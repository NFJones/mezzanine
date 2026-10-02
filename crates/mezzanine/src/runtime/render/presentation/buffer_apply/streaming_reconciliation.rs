//! Retention of validated streaming prefixes during completion fallback.
//!
//! Only exact accepted sources from the installed response generation can
//! become durable. The existing service owns all state and preserves ordinal
//! ordering; fallback never promotes provisional execution evidence.

use super::*;

/// Reports whether any provisional field lacks its source-closure receipt.
/// This classifies rejection diagnostics only, never authorizes publication.
pub(super) fn streaming_source_is_incomplete(
    presentation: &RuntimeStreamingSayPresentation,
) -> bool {
    presentation
        .rationale
        .as_ref()
        .is_some_and(|source| !source.complete)
        || presentation.actions.values().any(|source| !source.complete)
        || presentation
            .outbound_messages
            .values()
            .any(|source| !source.complete)
        || presentation
            .shell_commands
            .values()
            .any(|source| !source.complete)
        || presentation
            .shell_summaries
            .values()
            .any(|source| !source.complete)
}

/// Checks complete provisional fields against their exact validated ordinals.
/// Receipt alone does not authorize promotion; source, media type and action
/// payload must all agree with the accepted batch.
pub(super) fn streaming_sources_match_batch(
    presentation: &RuntimeStreamingSayPresentation,
    batch: &mez_agent::MaapBatch,
) -> bool {
    presentation
        .rationale
        .as_ref()
        .is_none_or(|streamed| streamed.complete && streamed.text == batch.rationale)
        && presentation
            .outbound_messages
            .iter()
            .all(|(action_index, streamed)| {
                let Some(mez_agent::AgentActionPayload::SendMessage {
                    recipient,
                    content_type,
                    payload,
                    ..
                }) = batch
                    .actions
                    .get(*action_index)
                    .map(|action| &action.payload)
                else {
                    return false;
                };
                streamed.complete
                    && streamed.recipient == *recipient
                    && streamed.text == *payload
                    && streamed.content_type
                        == mez_agent::normalize_maap_message_content_type(content_type)
            })
        && presentation.actions.iter().all(|(action_index, streamed)| {
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
                    == mez_agent::normalize_agent_output_content_type(Some(content_type))
        })
        && presentation
            .shell_commands
            .iter()
            .all(|(action_index, streamed)| {
                let Some(mez_agent::AgentActionPayload::ShellCommand { command, .. }) = batch
                    .actions
                    .get(*action_index)
                    .map(|action| &action.payload)
                else {
                    return false;
                };
                streamed.complete && streamed.text == *command
            })
        && presentation
            .shell_summaries
            .iter()
            .all(|(action_index, streamed)| {
                let Some(mez_agent::AgentActionPayload::ShellCommand { summary, .. }) = batch
                    .actions
                    .get(*action_index)
                    .map(|action| &action.payload)
                else {
                    return false;
                };
                streamed.complete && streamed.text == *summary
            })
        && presentation.action_headers.iter().all(|(index, header)| {
            let Some(action) = batch.actions.get(*index) else {
                return false;
            };
            match (header, &action.payload) {
                (
                    mez_agent::StreamingActionHeader::WebSearch { query: streamed },
                    mez_agent::AgentActionPayload::WebSearch {
                        query: accepted, ..
                    },
                ) => streamed == accepted,
                (
                    mez_agent::StreamingActionHeader::FetchUrl { url: streamed },
                    mez_agent::AgentActionPayload::FetchUrl { url: accepted, .. },
                ) => streamed == accepted,
                (mez_agent::StreamingActionHeader::Action { action: streamed }, accepted) => {
                    &streamed.payload == accepted
                }
                _ => false,
            }
        })
}

impl RuntimeSessionService {
    /// Reconciles live source and reports whether the installed screen survived.
    pub(crate) fn reconcile_agent_streaming_say_completion_with_render_intent(
        &mut self,
        pane_id: &str,
        turn_id: &str,
        execution: &mez_agent::AgentTurnExecution,
    ) -> Result<crate::runtime::render::RuntimeStreamingSayCompletionReconciliation> {
        self.reconcile_captured_streaming_presentation(pane_id, turn_id, execution)
    }

    /// Captures one provisional response for exact-source settlement on the actor.
    ///
    /// Removing ownership happens before reconciliation, as in the original
    /// settlement path. No worker or competing presentation store can publish it.
    pub(super) fn reconcile_captured_streaming_presentation(
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
        self.settle_captured_streaming_presentation(pane_id, turn_id, execution, presentation)
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

    /// Builds a private generation with deferred final says after accepted siblings.
    /// The synthetic projection positions affect only screen order; action ids,
    /// source indices, and durable records keep their validated identities.
    pub(super) fn build_ordered_pending_final_screen(
        mut work: crate::runtime::RuntimeStreamingSayProjectionWork,
        pending_indices: &std::collections::BTreeSet<usize>,
    ) -> Result<TerminalScreen> {
        let mut last_index = work
            .actions
            .keys()
            .chain(work.outbound_messages.keys())
            .chain(work.shell_commands.keys())
            .chain(work.shell_summaries.keys())
            .chain(work.action_headers.keys())
            .copied()
            .max()
            .unwrap_or(0);
        for index in pending_indices {
            if let Some(source) = work.actions.remove(index) {
                last_index = last_index.checked_add(1).ok_or_else(|| {
                    MezError::invalid_state("deferred final projection index overflow")
                })?;
                work.actions.insert(last_index, source);
            }
        }
        Ok(Self::build_agent_streaming_say_projection(work)?.screen)
    }

    /// Retains the exact installed rationale and accepted action prefix when a
    /// later component falls back to ordinary completion presentation. Command
    /// intent is retained independently of readiness or execution settlement;
    /// only already projected, validated source can become a permanent row.
    pub(super) fn retain_validated_streaming_rationale_for_fallback(
        &mut self,
        pane_id: &str,
        turn_id: &str,
        presentation: &RuntimeStreamingSayPresentation,
        batch: &mez_agent::MaapBatch,
        execution: &mez_agent::AgentTurnExecution,
    ) -> Result<bool> {
        let rationale = presentation.rationale.as_ref().filter(|source| {
            source.complete && !source.text.trim().is_empty() && source.text == batch.rationale
        });
        if presentation.turn_id != turn_id
            || self
                .agent_shell_store()
                .get(pane_id)
                .is_none_or(|session| session.session_id != presentation.conversation_id)
            || self.agent_pane_screen_lineage(pane_id, &presentation.conversation_id)
                != Some(presentation.installed_lineage)
            || presentation.projected_revision != Some(presentation.revision)
            || presentation.projected_lineage != Some(presentation.installed_lineage)
        {
            return Ok(false);
        }
        let Ok(context) = self.agent_streaming_say_projection_context(pane_id) else {
            return Ok(false);
        };
        if presentation.projected_context.as_ref() != Some(&context) {
            return Ok(false);
        }
        let mut retained_actions = presentation
            .actions
            .iter()
            .filter(|(index, source)| {
                source.complete
                    && source.status == mez_agent::SayStatus::Progress
                    && presentation.projected_actions.as_ref().is_some_and(|rows| {
                        rows.iter().any(|row| {
                            row.action_index == **index
                                && row.kind
                                    == crate::runtime::render::RuntimeStreamingSayProjectedActionKind::Say
                        })
                    })
                    && batch.actions.get(**index).is_some_and(|action| {
                        matches!(&action.payload, AgentActionPayload::Say {
                            status: mez_agent::SayStatus::Progress,
                            text,
                            content_type,
                        } if text == &source.text
                            && mez_agent::normalize_agent_output_content_type(Some(content_type))
                                == source.content_type)
                            && execution.action_results.iter().any(|result| {
                                result.action_id == action.id
                                    && result.status == mez_agent::ActionStatus::Succeeded
                            })
                    })
            })
            .map(|(index, source)| (*index, source.clone()))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut retained_commands = presentation
            .shell_commands
            .iter()
            .filter(|(index, source)| {
                source.complete
                    && presentation.projected_actions.as_ref().is_some_and(|rows| {
                        rows.iter().any(|row| {
                            row.action_index == **index
                                && matches!(
                                    row.kind,
                                    crate::runtime::render::RuntimeStreamingSayProjectedActionKind::ShellCommand {
                                        truncated: false,
                                    }
                                )
                        })
                    })
                    && batch.actions.get(**index).is_some_and(|action| {
                        matches!(&action.payload, AgentActionPayload::ShellCommand {
                            command, summary, ..
                        } if command == &source.text
                            && presentation.shell_summaries.get(index).is_none_or(|source| {
                                source.complete && &source.text == summary
                            }))
                            && execution.action_results.iter().any(|result| {
                                result.action_id == action.id
                                    && result.status != mez_agent::ActionStatus::Rejected
                            })
                    })
            })
            .map(|(index, source)| (*index, source.clone()))
            .collect::<std::collections::BTreeMap<_, _>>();
        // Retention cannot jump over an unprojected or unaccepted ordinal. In
        // particular, receipt of later commands does not prove they occupied
        // the installed screen or authorize promotion ahead of the first one.
        let first_unpromotable = (0..batch.actions.len())
            .find(|index| {
                !retained_actions.contains_key(index) && !retained_commands.contains_key(index)
            })
            .unwrap_or(batch.actions.len());
        retained_actions.retain(|index, _| *index < first_unpromotable);
        retained_commands.retain(|index, _| *index < first_unpromotable);
        let retained_summaries = presentation
            .shell_summaries
            .iter()
            .filter(|(index, _)| retained_commands.contains_key(index))
            .map(|(index, source)| (*index, source.clone()))
            .collect();
        let work = crate::runtime::RuntimeStreamingSayProjectionWork {
            pane_id: pane_id.to_string(),
            turn_id: turn_id.to_string(),
            response_index: presentation.response_index,
            conversation_id: presentation.conversation_id.clone(),
            revision: presentation.revision,
            installed_lineage: presentation.installed_lineage,
            baseline_screen: presentation.baseline_screen.clone(),
            rationale: rationale.filter(|_| context.thinking_enabled).cloned(),
            actions: retained_actions,
            outbound_messages: std::collections::BTreeMap::new(),
            shell_commands: retained_commands,
            shell_summaries: retained_summaries,
            action_headers: std::collections::BTreeMap::new(),
            thinking_enabled: context.thinking_enabled,
            shell_classification: context.shell_classification,
            presentation_columns: context.presentation_columns,
            frame_width: context.frame_width,
            table_width: context.table_width,
            ui_theme: context.ui_theme,
            screen_size: context.screen_size,
        };
        let projection = Self::build_agent_streaming_say_projection(work)?;
        if projection.projected_rationale.is_none() && projection.projected_actions.is_empty() {
            return Ok(false);
        }
        self.update_agent_streaming_screen(
            pane_id,
            &presentation.conversation_id,
            projection.screen,
        )?;
        let mut promoted = std::collections::BTreeSet::new();
        if let (Some(rationale), Some(row)) = (rationale, projection.projected_rationale) {
            self.persist_activity_rationale_projection(
                pane_id,
                execution,
                (row.style, row.rendered_lines, row.copy_lines),
                &rationale.text,
            )?;
            promoted.insert(STREAMED_RATIONALE_PRESENTED_MARKER);
            self.integration
                .runtime_metrics_mut()
                .record_agent_streaming_settled_component("rationale");
        }
        for projected in projection.projected_actions {
            let (source, content_type, component) = match projected.kind {
                crate::runtime::render::RuntimeStreamingSayProjectedActionKind::Say => {
                    let Some(source) = presentation.actions.get(&projected.action_index) else {
                        continue;
                    };
                    (source.text.as_str(), source.content_type.as_str(), "say")
                }
                crate::runtime::render::RuntimeStreamingSayProjectedActionKind::ShellCommand {
                    truncated: false,
                } => {
                    let Some(source) = presentation.shell_commands.get(&projected.action_index)
                    else {
                        continue;
                    };
                    if context.thinking_enabled
                        && let Some(summary) =
                            presentation.shell_summaries.get(&projected.action_index)
                        && rationale.map(|source| source.text.as_str())
                            != Some(summary.text.as_str())
                    {
                        let lines = agent_thinking_display_lines_for_width(
                            &summary.text,
                            context.frame_width,
                        );
                        self.persist_agent_presentation_entry(
                            pane_id,
                            vec![
                                AgentTerminalPresentationStyle::Status
                                    .persistence_name()
                                    .to_string();
                                lines.len()
                            ],
                            lines,
                            Vec::new(),
                            String::new(),
                            Some((
                                summary.text.as_str(),
                                AGENT_PRESENTATION_THINKING_CONTENT_TYPE,
                            )),
                        );
                    }
                    (
                        source.text.as_str(),
                        AGENT_PRESENTATION_COMMAND_PREVIEW_CONTENT_TYPE,
                        "command",
                    )
                }
                _ => continue,
            };
            self.persist_agent_presentation_entry(
                pane_id,
                vec![projected.style.clone(); projected.rendered_lines.len()],
                projected.rendered_lines,
                projected.copy_lines,
                String::new(),
                Some((source, content_type)),
            );
            promoted.insert(projected.action_index);
            self.integration
                .runtime_metrics_mut()
                .record_agent_streaming_settled_component(component);
        }
        self.presentation
            .agent_promoted_streaming_say_actions
            .insert((pane_id.to_string(), turn_id.to_string()), promoted);
        self.integration
            .runtime_metrics_mut()
            .record_agent_streaming_settlement_screen_change(true);
        Ok(true)
    }
}
