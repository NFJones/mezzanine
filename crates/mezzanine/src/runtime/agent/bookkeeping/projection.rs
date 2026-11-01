//! Explicit canonical transcript projection without ambient runtime ownership.
//!
//! Live callers capture one source view; failed completion groups retain the
//! actual moved context and its imported-history/compaction/routing metadata.
//! The same encoding/occurrence logic serves both paths, preserving original
//! witness anchors and full-chronology ordinals. Projection neither installs
//! runtime maps nor accepts archive writes, continuations or producer transfer.

use super::*;
use crate::runtime::agent_state::RuntimeTurnCompactionHandoff;

/// Non-context metadata needed to project the same original canonical source.
pub(in crate::runtime::agent) struct RuntimeTranscriptProjectionMetadata {
    /// Imported occurrences are restored for ordinal assignment, not reappended.
    imported_history_high_water: u64,
    /// Exact accepted witnesses and prior epoch, independent of later live maps.
    compaction: Option<RuntimeTurnCompactionHandoff>,
    /// Original routed parent policy, not a replacement workflow lookup.
    routed_presentation: bool,
    /// Forced original archive needs a witness even while effect slots remain
    /// running; this never certifies worker termination or custody transfer.
    terminal_archive: bool,
}

/// Borrowed immutable canonical source; absence never adopts another context.
pub(in crate::runtime::agent) struct RuntimeTranscriptProjection<'a> {
    /// Completion requires actual context; legacy interrupted projections may lack it.
    pub(super) context: Option<&'a mez_agent::AgentContext>,
    /// Imported boundary from the same actual original source.
    imported_history_high_water: u64,
    /// Borrowed witnesses avoid cloning raw source during live projection.
    compaction: Option<&'a RuntimeTurnCompactionHandoff>,
    /// Original routed-parent policy for this projection.
    routed_presentation: bool,
    /// Original forced-archive intent, never a worker termination receipt.
    terminal_archive: bool,
}

impl RuntimeSessionService {
    /// Moves original projection metadata in the same synchronous retirement as
    /// its actual context. Repeated cleanup cannot sample replacement values.
    pub(in crate::runtime::agent) fn take_transcript_projection_metadata(
        &mut self,
        turn_id: &str,
    ) -> RuntimeTranscriptProjectionMetadata {
        let routed_presentation = self.routed_presentation_turn(turn_id);
        RuntimeTranscriptProjectionMetadata {
            imported_history_high_water: self
                .agent
                .agent_turn_imported_history_sequence_high_water
                .remove(turn_id)
                .unwrap_or(0),
            compaction: self.agent.agent_turn_compaction_handoffs.remove(turn_id),
            routed_presentation,
            terminal_archive: true,
        }
    }
}

impl<'a> RuntimeTranscriptProjection<'a> {
    /// Binds metadata to the caller's actual retained context without lookup.
    pub(in crate::runtime::agent) fn new(
        context: Option<&'a mez_agent::AgentContext>,
        metadata: &'a RuntimeTranscriptProjectionMetadata,
    ) -> Self {
        Self {
            context,
            imported_history_high_water: metadata.imported_history_high_water,
            compaction: metadata.compaction.as_ref(),
            routed_presentation: metadata.routed_presentation,
            terminal_archive: metadata.terminal_archive,
        }
    }

    /// Borrows live context/witnesses once at the explicit source boundary.
    /// Subsequent row encoding performs no ambient lookup or witness cloning.
    pub(super) fn live(service: &'a RuntimeSessionService, turn_id: &str) -> Self {
        Self {
            context: service.agent_turn_contexts().get(turn_id),
            imported_history_high_water: service
                .agent_turn_imported_history_sequence_high_water(turn_id),
            compaction: service.agent.agent_turn_compaction_handoffs.get(turn_id),
            routed_presentation: service.routed_presentation_turn(turn_id),
            terminal_archive: false,
        }
    }

    /// Projects ordinary rows and exact canonical occurrences from this source.
    pub(super) fn entries_for_execution(
        &self,
        conversation_id: &str,
        first_sequence: u64,
        created_at_unix_seconds: u64,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> Result<Vec<TranscriptEntry>> {
        let sequence = first_sequence;
        let mut execution_entries = transcript_entries_for_execution(
            conversation_id,
            sequence,
            created_at_unix_seconds,
            turn,
            execution,
        )?;
        // Canonical chronology owns every user-event occurrence. Request
        // projections cannot distinguish identical prompt and steering text,
        // so exclude them rather than backdating or coalescing user events.
        execution_entries.retain(|entry| entry.role != TranscriptRole::User);
        self.insert_initial_user_transcript_event(
            &mut execution_entries,
            conversation_id,
            sequence,
            created_at_unix_seconds,
            turn,
        )?;
        self.insert_prompt_boundary_transcript_events(
            &mut execution_entries,
            conversation_id,
            created_at_unix_seconds,
            turn,
        )?;
        self.append_exact_execution_block_transcript_events(
            &mut execution_entries,
            conversation_id,
            sequence,
            created_at_unix_seconds,
            turn,
        )?;
        if execution.terminal_state == mez_agent::AgentTurnState::Completed
            && self.routed_presentation
            && let Some(content) = self.routed_handoff_transcript_content()
        {
            Self::insert_routed_handoff_transcript_event(
                &mut execution_entries,
                conversation_id,
                created_at_unix_seconds,
                turn,
                content,
            )?;
        }
        if self.terminal_archive
            || execution.final_turn
            || matches!(
                execution.terminal_state,
                mez_agent::AgentTurnState::Completed
                    | mez_agent::AgentTurnState::Failed
                    | mez_agent::AgentTurnState::Interrupted
            )
        {
            self.append_terminal_compaction_handoff_entry(
                &mut execution_entries,
                conversation_id,
                sequence,
                created_at_unix_seconds,
                turn,
            )?;
        }
        Ok(execution_entries)
    }

    /// Inserts the canonical initial prompt before its display projection.
    fn insert_initial_user_transcript_event(
        &self,
        entries: &mut Vec<TranscriptEntry>,
        conversation_id: &str,
        sequence: u64,
        created_at_unix_seconds: u64,
        turn: &AgentTurnRecord,
    ) -> Result<()> {
        let Some(event) = self.context.and_then(|context| {
            context.chronology().iter().find(|event| {
                event.block().source == ContextSourceKind::UserInstruction
                    && event.block().label == "user prompt"
            })
        }) else {
            return Ok(());
        };
        let transcript_event = TranscriptContextEvent::user_event(
            event.sequence().get(),
            event.block().label.clone(),
            event.block().content.clone(),
        )
        .ok_or_else(|| MezError::invalid_state("turn initial user event is invalid"))?;
        let entry = TranscriptEntry {
            conversation_id: conversation_id.to_string(),
            sequence,
            created_at_unix_seconds,
            role: TranscriptRole::System,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            pane_id: turn.pane_id.clone(),
            content: transcript_event.to_transcript_content(),
        };
        entry.validate()?;
        entries.insert(0, entry);
        Ok(())
    }

    /// Inserts exact prompt-boundary context before its owning user entry.
    fn insert_prompt_boundary_transcript_events(
        &self,
        entries: &mut Vec<TranscriptEntry>,
        conversation_id: &str,
        created_at_unix_seconds: u64,
        turn: &AgentTurnRecord,
    ) -> Result<()> {
        let Some(insertion_index) = entries.iter().position(|entry| {
            entry.role == TranscriptRole::User
                || matches!(
                    TranscriptContextEvent::from_transcript_content(&entry.content),
                    Some(TranscriptContextEvent::UserEvent { .. })
                )
        }) else {
            return Ok(());
        };
        let first_sequence = entries[insertion_index].sequence;
        let prompt_entries = self.prompt_boundary_transcript_entries_for_turn(
            conversation_id,
            first_sequence,
            created_at_unix_seconds,
            turn,
        )?;
        if prompt_entries.is_empty() {
            return Ok(());
        }
        let inserted = prompt_entries.len() as u64;
        for entry in &mut entries[insertion_index..] {
            entry.sequence = entry.sequence.saturating_add(inserted);
        }
        entries.splice(insertion_index..insertion_index, prompt_entries);
        Ok(())
    }

    /// Builds exact durable events for newly introduced context before a user prompt.
    pub(super) fn prompt_boundary_transcript_entries_for_turn(
        &self,
        conversation_id: &str,
        first_sequence: u64,
        created_at_unix_seconds: u64,
        turn: &AgentTurnRecord,
    ) -> Result<Vec<TranscriptEntry>> {
        let Some(context) = self.context else {
            return Ok(Vec::new());
        };
        let imported_history_sequence_high_water = self.imported_history_high_water;
        let mut sequence = first_sequence;
        let mut entries = Vec::new();
        for event in context
            .chronology()
            .iter()
            .filter(|event| event.sequence().get() > imported_history_sequence_high_water)
        {
            let block = event.block();
            if block.source == ContextSourceKind::UserInstruction && block.label == "user prompt" {
                break;
            }
            let event = if block.source == ContextSourceKind::Configuration
                && block.label == "task environment snapshot"
            {
                TranscriptContextEvent::environment_snapshot(block.content.clone()).ok_or_else(
                    || MezError::invalid_state("turn environment snapshot is empty or oversized"),
                )?
            } else if block.source == ContextSourceKind::McpCatalogSnapshot
                && block.label == mez_agent::MCP_CATALOG_SNAPSHOT_CONTEXT_LABEL
            {
                TranscriptContextEvent::mcp_catalog_snapshot(
                    block.content.clone(),
                    event.sequence().get(),
                )
                .ok_or_else(|| {
                    MezError::invalid_state("turn MCP catalog snapshot is empty or oversized")
                })?
            } else if matches!(
                block.source,
                ContextSourceKind::SkillInstruction
                    | ContextSourceKind::LocalMessage
                    | ContextSourceKind::PeerMessage
                    | ContextSourceKind::Policy
                    | ContextSourceKind::Configuration
            ) {
                TranscriptContextEvent::prompt_boundary_with_event_sequence(
                    block.source,
                    event.sequence().get(),
                    block.label.clone(),
                    block.content.clone(),
                )
                .ok_or_else(|| {
                    MezError::invalid_state(
                        "turn prompt-boundary context is empty, oversized, or unsupported",
                    )
                })?
            } else {
                continue;
            };
            let entry = TranscriptEntry {
                conversation_id: conversation_id.to_string(),
                sequence,
                created_at_unix_seconds,
                role: TranscriptRole::System,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                pane_id: turn.pane_id.clone(),
                content: event.to_transcript_content(),
            };
            entry.validate()?;
            entries.push(entry);
            sequence = sequence.saturating_add(1);
        }
        Ok(entries)
    }

    /// Appends canonical user events and exact execution blocks after the
    /// ordinary display transcript projection for one completed turn.
    pub(super) fn append_exact_execution_block_transcript_events(
        &self,
        entries: &mut Vec<TranscriptEntry>,
        conversation_id: &str,
        first_sequence: u64,
        created_at_unix_seconds: u64,
        turn: &AgentTurnRecord,
    ) -> Result<()> {
        let Some(context) = self.context else {
            return Ok(());
        };
        let imported_history_sequence_high_water = self.imported_history_high_water;
        let mut sequence = entries
            .last()
            .map_or(first_sequence, |entry| entry.sequence.saturating_add(1));
        let mut active_user_seen = false;
        let chronology = RuntimeSessionService::terminal_compaction_source_chronology_from_source(
            context,
            self.compaction,
        )?;
        let occurrences = RuntimeSessionService::terminal_execution_occurrences(&chronology)?;
        for event in chronology
            .iter()
            .filter(|event| event.sequence().get() > imported_history_sequence_high_water)
        {
            let block = event.block();
            if block.source == ContextSourceKind::UserInstruction {
                active_user_seen = true;
                if block.label == "user prompt" {
                    continue;
                }
                let transcript_event = TranscriptContextEvent::user_event(
                    event.sequence().get(),
                    block.label.clone(),
                    block.content.clone(),
                )
                .ok_or_else(|| {
                    MezError::invalid_state("turn user event is empty, oversized, or unsupported")
                })?;
                let entry = TranscriptEntry {
                    conversation_id: conversation_id.to_string(),
                    sequence,
                    created_at_unix_seconds,
                    role: TranscriptRole::System,
                    turn_id: turn.turn_id.clone(),
                    agent_id: turn.agent_id.clone(),
                    pane_id: turn.pane_id.clone(),
                    content: transcript_event.to_transcript_content(),
                };
                entry.validate()?;
                entries.push(entry);
                sequence = sequence.saturating_add(1);
                continue;
            }
            let transcript_event = if block.source == ContextSourceKind::McpCatalogSnapshot
                && active_user_seen
            {
                TranscriptContextEvent::mcp_catalog_snapshot(
                    block.content.clone(),
                    event.sequence().get(),
                )
            } else if active_user_seen
                && matches!(
                    block.source,
                    ContextSourceKind::LocalMessage
                        | ContextSourceKind::PeerMessage
                        | ContextSourceKind::Policy
                        | ContextSourceKind::Configuration
                )
            {
                TranscriptContextEvent::prompt_boundary_with_event_sequence(
                    block.source,
                    event.sequence().get(),
                    block.label.clone(),
                    block.content.clone(),
                )
            } else if !matches!(
                block.source,
                ContextSourceKind::CommittedEvidence
                    | ContextSourceKind::TranscriptAssistant
                    | ContextSourceKind::TranscriptTool
                    | ContextSourceKind::ActionResult
                    | ContextSourceKind::McpServerReference
                    | ContextSourceKind::McpServerSearchResult
                    | ContextSourceKind::McpRetrievedManifest
            ) {
                continue;
            } else if event.execution_group_id().is_some() {
                occurrences.get(&event.sequence().get()).cloned()
            } else {
                TranscriptContextEvent::execution_block(
                    block.source,
                    block.label.clone(),
                    block.content.clone(),
                )
            }
            .ok_or_else(|| {
                MezError::invalid_state("turn execution block is empty, oversized, or unsupported")
            })?;
            let entry = TranscriptEntry {
                conversation_id: conversation_id.to_string(),
                sequence,
                created_at_unix_seconds,
                role: TranscriptRole::System,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                pane_id: turn.pane_id.clone(),
                content: transcript_event.to_transcript_content(),
            };
            entry.validate()?;
            entries.push(entry);
            sequence = sequence.saturating_add(1);
        }
        Ok(())
    }

    /// Captures terminal source proof into the ordinary append-receipt pipeline
    /// before turn cleanup. This audit row is never directly rendered to models.
    pub(super) fn append_terminal_compaction_handoff_entry(
        &self,
        entries: &mut Vec<TranscriptEntry>,
        conversation_id: &str,
        first_sequence: u64,
        created_at_unix_seconds: u64,
        turn: &AgentTurnRecord,
    ) -> Result<()> {
        let Some(handoff) = self.compaction else {
            return Ok(());
        };
        let context = self.context.ok_or_else(|| {
            MezError::invalid_state("terminal projection lost original compaction context")
        })?;
        let Some(content) =
            RuntimeSessionService::terminal_compaction_handoff_from_source(context, handoff)?
        else {
            return Ok(());
        };
        let entry = TranscriptEntry {
            conversation_id: conversation_id.to_string(),
            sequence: entries
                .last()
                .map_or(first_sequence, |row| row.sequence.saturating_add(1)),
            created_at_unix_seconds,
            role: TranscriptRole::System,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            pane_id: turn.pane_id.clone(),
            content,
        };
        entry.validate()?;
        entries.push(entry);
        Ok(())
    }

    /// Returns the summarized routed handoff selected for durable replay.
    ///
    /// The exact worker output and presentation-only instructions use different
    /// labels and are deliberately excluded. The summary block exists only on
    /// the parent presentation turn while transcript persistence is running.
    fn routed_handoff_transcript_content(&self) -> Option<String> {
        self.context?
            .blocks()
            .iter()
            .rev()
            .find(|block| {
                block.source == ContextSourceKind::RoutedHandoff
                    && block.label == "routed worker handoff context"
                    && !block.content.trim().is_empty()
            })
            .map(|block| block.content.clone())
    }

    /// Inserts one typed routed-handoff event immediately before the visible
    /// parent assistant entry and advances later sequence numbers.
    fn insert_routed_handoff_transcript_event(
        entries: &mut Vec<TranscriptEntry>,
        conversation_id: &str,
        created_at_unix_seconds: u64,
        turn: &AgentTurnRecord,
        content: String,
    ) -> Result<()> {
        let assistant_index = entries
            .iter()
            .position(|entry| entry.role == TranscriptRole::Assistant)
            .ok_or_else(|| {
                MezError::invalid_state(
                    "routed presentation transcript is missing its assistant entry",
                )
            })?;
        let sequence = entries[assistant_index].sequence;
        for entry in &mut entries[assistant_index..] {
            entry.sequence = entry.sequence.saturating_add(1);
        }
        let entry = TranscriptEntry {
            conversation_id: conversation_id.to_string(),
            sequence,
            created_at_unix_seconds,
            role: TranscriptRole::System,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            pane_id: turn.pane_id.clone(),
            content: TranscriptContextEvent::RoutedHandoff { content }.to_transcript_content(),
        };
        entry.validate()?;
        entries.insert(assistant_index, entry);
        Ok(())
    }
}
