//! Actor-owned initialization and source accumulation for provisional responses.
//!
//! Source ordinals and response identity remain in the existing presentation
//! state. This module owns initialization, while projection and accepted
//! settlement retain their separate rendering and freshness boundaries.

use super::*;

impl RuntimeSessionService {
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
                    self.agent_streaming_base_screen(pane_id, &conversation_id)?;
                self.presentation
                    .agent_promoted_streaming_say_actions
                    .remove(&(pane_id.to_string(), turn_id.to_string()));
                self.presentation.agent_streaming_say_presentations.insert(
                    pane_id.to_string(),
                    RuntimeStreamingSayPresentation {
                        turn_id: turn_id.to_string(),
                        response_index: *response_index,
                        conversation_id: conversation_id.clone(),
                        installed_lineage: self
                            .agent_pane_screen_lineage(pane_id, &conversation_id)
                            .ok_or_else(|| {
                                MezError::invalid_state(
                                    "streaming presentation lineage was unavailable",
                                )
                            })?,
                        baseline_screen: std::sync::Arc::new(baseline_screen.clone()),
                        provider_screen: std::sync::Arc::new(baseline_screen),
                        rationale: None,
                        actions: std::collections::BTreeMap::new(),
                        outbound_messages: std::collections::BTreeMap::new(),
                        shell_commands: std::collections::BTreeMap::new(),
                        shell_summaries: std::collections::BTreeMap::new(),
                        action_headers: std::collections::BTreeMap::new(),
                        received_actions: std::collections::BTreeSet::new(),
                        revision: 1,
                        projected_revision: None,
                        projected_context: None,
                        projected_actions: None,
                        projected_rationale: None,
                        projected_lineage: None,
                    },
                );
            }
            mez_agent::StreamingSayEvent::Started {
                action_index,
                status,
                content_type,
            } => {
                self.ensure_current_agent_presentation_screen(pane_id)?;
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
                        self.agent_streaming_base_screen(pane_id, &conversation_id)?;
                    self.presentation
                        .agent_promoted_streaming_say_actions
                        .remove(&(pane_id.to_string(), turn_id.to_string()));
                    self.presentation.agent_streaming_say_presentations.insert(
                        pane_id.to_string(),
                        RuntimeStreamingSayPresentation {
                            turn_id: turn_id.to_string(),
                            response_index: 0,
                            conversation_id: conversation_id.clone(),
                            installed_lineage: self
                                .agent_pane_screen_lineage(pane_id, &conversation_id)
                                .ok_or_else(|| {
                                    MezError::invalid_state(
                                        "streaming presentation lineage was unavailable",
                                    )
                                })?,
                            baseline_screen: std::sync::Arc::new(baseline_screen.clone()),
                            provider_screen: std::sync::Arc::new(baseline_screen),
                            rationale: None,
                            actions: std::collections::BTreeMap::new(),
                            outbound_messages: std::collections::BTreeMap::new(),
                            shell_commands: std::collections::BTreeMap::new(),
                            shell_summaries: std::collections::BTreeMap::new(),
                            action_headers: std::collections::BTreeMap::new(),
                            received_actions: std::collections::BTreeSet::new(),
                            revision: 1,
                            projected_revision: None,
                            projected_context: None,
                            projected_actions: None,
                            projected_rationale: None,
                            projected_lineage: None,
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
                if presentation.actions.contains_key(action_index) {
                    return Ok(());
                }
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
                let has_predecessor = presentation
                    .actions
                    .keys()
                    .chain(presentation.outbound_messages.keys())
                    .chain(presentation.shell_commands.keys())
                    .chain(presentation.shell_summaries.keys())
                    .chain(presentation.action_headers.keys())
                    .chain(presentation.received_actions.iter())
                    .any(|index| index < action_index)
                    || presentation.rationale.is_some();
                if !has_predecessor {
                    self.append_agent_streaming_say_started(pane_id)?;
                }
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
                if action.complete {
                    return Ok(());
                }
                action.complete = true;
                // Closure may release source buffered behind this ordinal even
                // though the completed field adds no display characters.
                release_later_streaming_action(presentation, *action_index);
            }
            mez_agent::StreamingSayEvent::MessageStarted {
                action_index,
                recipient,
                content_type,
            } => {
                if !runtime_peer_message_presentation_is_visible(
                    self.agent_peer_message_log_mode(),
                    Some(content_type),
                ) {
                    return Ok(());
                }
                let direct_parent = crate::runtime::runtime_message_recipient(recipient)
                    .ok()
                    .is_some_and(|recipient| {
                        self.runtime_outbound_recipient_is_direct_parent(
                            &format!("agent-{pane_id}"),
                            &recipient,
                        )
                    });
                let recipient_label = crate::runtime::runtime_message_recipient(recipient)
                    .ok()
                    .map(|parsed_recipient| {
                        self.runtime_outbound_recipient_display_label(&parsed_recipient, recipient)
                    })
                    .unwrap_or_else(|| recipient.clone());
                self.ensure_agent_streaming_presentation(pane_id, turn_id)?;
                let presentation = self
                    .presentation
                    .agent_streaming_say_presentations
                    .get_mut(pane_id)
                    .ok_or_else(|| {
                        MezError::invalid_state(
                            "streaming outbound message presentation is unavailable",
                        )
                    })?;
                if presentation.outbound_messages.contains_key(action_index) {
                    return Ok(());
                }
                presentation
                    .outbound_messages
                    .entry(*action_index)
                    .or_insert_with(|| RuntimeStreamingMessageSource {
                        recipient: recipient.clone(),
                        recipient_label,
                        direct_parent,
                        content_type: content_type.clone(),
                        text: String::new(),
                        complete: false,
                    });
                presentation.revision = presentation.revision.wrapping_add(1);
                presentation.projected_revision = None;
            }
            mez_agent::StreamingSayEvent::MessagePayloadDelta { action_index, text } => {
                let exists = self
                    .presentation
                    .agent_streaming_say_presentations
                    .get(pane_id)
                    .filter(|presentation| presentation.turn_id == turn_id)
                    .is_some_and(|presentation| {
                        presentation.outbound_messages.contains_key(action_index)
                    });
                if !exists {
                    return Ok(());
                }
                let presentation = self
                    .presentation
                    .agent_streaming_say_presentations
                    .get_mut(pane_id)
                    .ok_or_else(|| {
                        MezError::invalid_state(
                            "streaming outbound message presentation disappeared",
                        )
                    })?;
                let message = presentation
                    .outbound_messages
                    .get_mut(action_index)
                    .ok_or_else(|| {
                        MezError::invalid_state("streaming outbound message source disappeared")
                    })?;
                message.text.push_str(text);
                if !text.is_empty() {
                    presentation.revision = presentation.revision.wrapping_add(1);
                    presentation.projected_revision = None;
                }
            }
            mez_agent::StreamingSayEvent::MessagePayloadComplete { action_index } => {
                let Some(presentation) = self
                    .presentation
                    .agent_streaming_say_presentations
                    .get_mut(pane_id)
                    .filter(|presentation| presentation.turn_id == turn_id)
                else {
                    return Ok(());
                };
                if let Some(message) = presentation.outbound_messages.get_mut(action_index) {
                    if message.complete {
                        return Ok(());
                    }
                    message.complete = true;
                    release_later_streaming_action(presentation, *action_index);
                }
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
                if presentation.rationale.is_some() {
                    return Ok(());
                }
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
                if rationale.complete {
                    return Ok(());
                }
                rationale.complete = true;
                if !presentation.actions.is_empty()
                    || !presentation.action_headers.is_empty()
                    || !presentation.shell_commands.is_empty()
                    || !presentation.outbound_messages.is_empty()
                {
                    presentation.revision = presentation.revision.wrapping_add(1);
                    presentation.projected_revision = None;
                }
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
                if presentation.shell_commands.contains_key(action_index) {
                    return Ok(());
                }
                presentation
                    .shell_commands
                    .entry(*action_index)
                    .or_insert_with(Default::default);
                presentation.revision = presentation.revision.wrapping_add(1);
                presentation.projected_revision = None;
                let has_predecessor = presentation
                    .actions
                    .keys()
                    .chain(presentation.outbound_messages.keys())
                    .chain(presentation.shell_commands.keys())
                    .chain(presentation.shell_summaries.keys())
                    .chain(presentation.action_headers.keys())
                    .chain(presentation.received_actions.iter())
                    .any(|index| index < action_index)
                    || presentation.rationale.is_some();
                if !has_predecessor {
                    self.append_agent_streaming_plain_started(
                        pane_id,
                        AgentTerminalPresentationStyle::Command,
                        "$ ",
                        "starting streaming command source",
                    )?;
                }
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
                if command.complete {
                    return Ok(());
                }
                command.complete = true;
                release_later_streaming_action(presentation, *action_index);
            }
            mez_agent::StreamingSayEvent::ShellCommandSummaryStarted { action_index } => {
                self.ensure_agent_streaming_presentation(pane_id, turn_id)?;
                let presentation = self
                    .presentation
                    .agent_streaming_say_presentations
                    .get_mut(pane_id)
                    .ok_or_else(|| {
                        MezError::invalid_state(
                            "streaming shell summary presentation is unavailable",
                        )
                    })?;
                if presentation.shell_summaries.contains_key(action_index) {
                    return Ok(());
                }
                presentation
                    .shell_summaries
                    .entry(*action_index)
                    .or_insert_with(Default::default);
                presentation.revision = presentation.revision.wrapping_add(1);
                presentation.projected_revision = None;
            }
            mez_agent::StreamingSayEvent::ShellCommandSummaryTextDelta { action_index, text } => {
                let presentation = self
                    .presentation
                    .agent_streaming_say_presentations
                    .get_mut(pane_id)
                    .filter(|presentation| presentation.turn_id == turn_id)
                    .ok_or_else(|| {
                        MezError::invalid_state(
                            "streaming shell summary arrived before its start event",
                        )
                    })?;
                let summary = presentation
                    .shell_summaries
                    .get_mut(action_index)
                    .ok_or_else(|| {
                        MezError::invalid_state(
                            "streaming shell summary arrived before its start event",
                        )
                    })?;
                summary.text.push_str(text);
                if !text.is_empty() {
                    presentation.revision = presentation.revision.wrapping_add(1);
                    presentation.projected_revision = None;
                }
            }
            mez_agent::StreamingSayEvent::ShellCommandSummaryTextComplete { action_index } => {
                let presentation = self
                    .presentation
                    .agent_streaming_say_presentations
                    .get_mut(pane_id)
                    .filter(|presentation| presentation.turn_id == turn_id)
                    .ok_or_else(|| {
                        MezError::invalid_state(
                            "streaming shell summary completion arrived before its start event",
                        )
                    })?;
                let summary = presentation
                    .shell_summaries
                    .get_mut(action_index)
                    .ok_or_else(|| {
                        MezError::invalid_state(
                            "streaming shell summary completion arrived before its start event",
                        )
                    })?;
                if summary.complete {
                    return Ok(());
                }
                summary.complete = true;
                release_later_streaming_action(presentation, *action_index);
            }
            mez_agent::StreamingSayEvent::ActionHeader {
                action_index,
                header,
            } => {
                self.ensure_agent_streaming_presentation(pane_id, turn_id)?;
                let presentation = self
                    .presentation
                    .agent_streaming_say_presentations
                    .get_mut(pane_id)
                    .ok_or_else(|| {
                        MezError::invalid_state(
                            "streaming action header presentation is unavailable",
                        )
                    })?;
                if presentation
                    .action_headers
                    .insert(*action_index, *header.clone())
                    != Some(*header.clone())
                {
                    presentation.revision = presentation.revision.wrapping_add(1);
                    presentation.projected_revision = None;
                }
            }
            // A whole-action receipt is an ordering barrier, not validation or
            // execution. A closed field alone cannot release later ordinals.
            mez_agent::StreamingSayEvent::ActionComplete { action_index } => {
                self.ensure_agent_streaming_presentation(pane_id, turn_id)?;
                if let Some(presentation) = self
                    .presentation
                    .agent_streaming_say_presentations
                    .get_mut(pane_id)
                    .filter(|presentation| presentation.turn_id == turn_id)
                    && presentation.received_actions.insert(*action_index)
                {
                    release_later_streaming_action(presentation, *action_index);
                }
            }
        }
        Ok(())
    }

    /// Initializes response-scoped provisional presentation for any source kind.
    pub(super) fn ensure_agent_streaming_presentation(
        &mut self,
        pane_id: &str,
        turn_id: &str,
    ) -> Result<()> {
        self.ensure_current_agent_presentation_screen(pane_id)?;
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
            let baseline_screen = self.agent_streaming_base_screen(pane_id, &conversation_id)?;
            self.presentation
                .agent_promoted_streaming_say_actions
                .remove(&(pane_id.to_string(), turn_id.to_string()));
            self.presentation.agent_streaming_say_presentations.insert(
                pane_id.to_string(),
                RuntimeStreamingSayPresentation {
                    turn_id: turn_id.to_string(),
                    response_index: 0,
                    conversation_id: conversation_id.clone(),
                    installed_lineage: self
                        .agent_pane_screen_lineage(pane_id, &conversation_id)
                        .ok_or_else(|| {
                            MezError::invalid_state(
                                "streaming presentation lineage was unavailable",
                            )
                        })?,
                    baseline_screen: std::sync::Arc::new(baseline_screen.clone()),
                    provider_screen: std::sync::Arc::new(baseline_screen),
                    rationale: None,
                    actions: std::collections::BTreeMap::new(),
                    outbound_messages: std::collections::BTreeMap::new(),
                    shell_commands: std::collections::BTreeMap::new(),
                    shell_summaries: std::collections::BTreeMap::new(),
                    action_headers: std::collections::BTreeMap::new(),
                    received_actions: std::collections::BTreeSet::new(),
                    revision: 1,
                    projected_revision: None,
                    projected_context: None,
                    projected_actions: None,
                    projected_rationale: None,
                    projected_lineage: None,
                },
            );
        }
        Ok(())
    }
}
