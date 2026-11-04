//! Pure canonical chronology preceding an original MMP canonical occurrence.
//!
//! Projection runs against the actual planned context before its live commit.
//! It preserves source order and execution occurrences, including witnessed
//! compaction sources, without creating terminal witnesses or execution work.
//! The original transcript lease later removes already committed identities and
//! selects physical positions. Neither projection nor dedup grants ACK or sender-consumption rights.

use super::*;

impl RuntimeSessionService {
    /// Encodes the exact new chronology before `before_event_sequence`. Imported
    /// history already belongs to its archive; runtime-only hints have no row.
    /// Invalid canonical events fail before the caller changes its live context.
    pub(in crate::runtime) fn project_canonical_message_prefix(
        &self,
        turn: &AgentTurnRecord,
        context: &mez_agent::AgentContext,
        imported_high_water: u64,
        before_event_sequence: u64,
    ) -> Result<Vec<TranscriptEntry>> {
        let chronology = Self::terminal_compaction_source_chronology_from_source(
            context,
            self.agent.agent_turn_compaction_handoffs.get(&turn.turn_id),
        )?;
        let occurrences = Self::terminal_execution_occurrences(&chronology)?;
        let native_owned = self.native_owned_peer_events(&turn.turn_id);
        let mut rows = Vec::new();
        for event in chronology.iter().filter(|event| {
            event.sequence().get() > imported_high_water
                && event.sequence().get() < before_event_sequence
        }) {
            let block = event.block();
            if block.source == ContextSourceKind::PeerMessage
                && native_owned.contains(&event.sequence().get())
            {
                continue;
            }
            let canonical = match block.source {
                ContextSourceKind::UserInstruction => TranscriptContextEvent::user_event(
                    event.sequence().get(),
                    block.label.clone(),
                    block.content.clone(),
                ),
                ContextSourceKind::Configuration if block.label == "task environment snapshot" => {
                    TranscriptContextEvent::environment_snapshot(block.content.clone())
                }
                ContextSourceKind::McpCatalogSnapshot => {
                    TranscriptContextEvent::mcp_catalog_snapshot(
                        block.content.clone(),
                        event.sequence().get(),
                    )
                }
                ContextSourceKind::SkillInstruction
                | ContextSourceKind::LocalMessage
                | ContextSourceKind::PeerMessage
                | ContextSourceKind::Policy
                | ContextSourceKind::Configuration => {
                    TranscriptContextEvent::prompt_boundary_with_event_sequence(
                        block.source,
                        event.sequence().get(),
                        block.label.clone(),
                        block.content.clone(),
                    )
                }
                ContextSourceKind::CommittedEvidence
                | ContextSourceKind::TranscriptAssistant
                | ContextSourceKind::TranscriptTool
                | ContextSourceKind::ActionResult
                | ContextSourceKind::McpServerReference
                | ContextSourceKind::McpServerSearchResult
                | ContextSourceKind::McpRetrievedManifest => {
                    if event.execution_group_id().is_some() {
                        occurrences.get(&event.sequence().get()).cloned()
                    } else {
                        TranscriptContextEvent::execution_block(
                            block.source,
                            block.label.clone(),
                            block.content.clone(),
                        )
                    }
                }
                _ => continue,
            }
            .ok_or_else(|| {
                MezError::invalid_state("MMP preceding canonical event cannot be encoded")
            })?;
            if rows.len() >= crate::storage::transcript::custody::MAX_CUSTODY_APPEND_ROWS - 1 {
                return Err(MezError::invalid_state("MMP canonical prefix row capacity"));
            }
            rows.push(TranscriptEntry {
                conversation_id: turn.conversation_id.clone(),
                sequence: rows.len() as u64 + 1,
                created_at_unix_seconds: turn.started_at_unix_seconds,
                role: TranscriptRole::System,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                pane_id: turn.pane_id.clone(),
                content: canonical.to_transcript_content(),
            });
        }
        Ok(rows)
    }
}
