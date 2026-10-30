//! Accepted local-summary source witnesses and terminal transcript capture.
//!
//! Private staging retains witnesses without granting publication authority.
//! Acceptance transfers them to the turn; bookkeeping captures original live
//! groups and an independently validated terminal witness before cleanup. No
//! group identity is invented for legacy or unowned context. Certificates and
//! transcript rows share a full-chronology occurrence map across witnesses;
//! a consumed group frontier does not permanently seal later canonical events.

use super::*;
use crate::runtime::agent_state::{RuntimeCompactionWitness, RuntimeTurnCompactionHandoff};
use crate::storage::transcript::compaction_handoff::{
    TerminalCompactionHandoff, TerminalCompactionReplacement,
};

impl RuntimeSessionService {
    /// Builds prospective witnesses and expands previously accepted local
    /// summaries by anchor and exact content, never by repeated text alone.
    pub(super) fn compaction_handoff_witnesses(
        &self,
        task: &RuntimeAgentCompactionTask,
        turn_id: &str,
        context: &AgentContext,
        plan: &mez_agent::ModelContextCompactionPlan,
        summary: &str,
    ) -> Vec<RuntimeCompactionWitness> {
        let mut witnesses = self
            .agent
            .agent_turn_compaction_handoffs
            .get(turn_id)
            .map_or_else(Vec::new, |handoff| handoff.witnesses.clone());
        if let RuntimeAgentCompactionTarget::ActiveTurn {
            staged: Some(staged),
            ..
        } = &task.target
        {
            witnesses = staged.handoff_witnesses.clone();
        }
        let mut source = Vec::new();
        for sequence in plan.replacement_event_sequences() {
            let Some(event) = context
                .chronology()
                .iter()
                .find(|event| event.sequence() == *sequence)
            else {
                continue;
            };
            if let Some(index) = witnesses.iter().position(|witness| {
                witness.anchor == sequence.get()
                    && (event.block().source == ContextSourceKind::CompactionSelective
                        || (event.block().source == ContextSourceKind::Memory
                            && event.block().label == "context compaction summary"))
                    && event.block().content == witness.summary
            }) {
                source.extend(witnesses.remove(index).source);
            } else {
                source.push(event.clone());
            }
        }
        if let Some(first) = source.first() {
            witnesses.push(RuntimeCompactionWitness {
                anchor: first.sequence().get(),
                source,
                summary: summary.to_string(),
            });
        }
        witnesses.sort_by_key(|witness| witness.anchor);
        witnesses
    }

    /// Accepts a distinct terminal witness only after the ordinary retry fits.
    /// Failure to map legacy source is reported; raw evidence is still retained.
    pub(super) fn retain_compaction_handoff(
        &mut self,
        task: &RuntimeAgentCompactionTask,
        turn_id: &str,
        witnesses: Vec<RuntimeCompactionWitness>,
    ) -> Result<()> {
        let baseline = self
            .agent
            .agent_turn_compaction_handoffs
            .get(turn_id)
            .map(|handoff| handoff.baseline.clone())
            .map(Ok)
            .unwrap_or_else(|| self.private_compaction_epoch_baseline(task))?;
        let supported = witnesses.iter().all(|witness| {
            witness
                .source
                .iter()
                .all(|event| event.execution_group_id().is_some())
        });
        self.agent.agent_turn_compaction_handoffs.insert(
            turn_id.to_string(),
            RuntimeTurnCompactionHandoff {
                baseline,
                witnesses,
            },
        );
        self.append_agent_trace_turn_event(&task.pane_id, turn_id, &format!(
            "context_compaction terminal_handoff_witness accepted=true typed_source={supported} operation_generation={}", task.task_generation,
        ))?;
        if !supported {
            self.append_agent_status_text_to_terminal_buffer(&task.pane_id,
                "agent: summary is turn-local; terminal handoff is unsupported for source without typed occurrence ownership")?;
        }
        Ok(())
    }

    /// Restores witnessed raw source for canonical persistence, not execution.
    /// Immutable chronological identities keep barriers and native groups ordered.
    /// Duplicate witness sequences and changed live source fail before capture;
    /// only the exact accepted summary at its own anchor may replace raw source.
    pub(in crate::runtime) fn terminal_compaction_source_chronology(
        &self,
        turn_id: &str,
        context: &AgentContext,
    ) -> Result<Vec<mez_agent::ConversationEvent>> {
        let mut events = context
            .chronology()
            .iter()
            .cloned()
            .map(|event| (event.sequence(), event))
            .collect::<std::collections::BTreeMap<_, _>>();
        if let Some(handoff) = self.agent.agent_turn_compaction_handoffs.get(turn_id) {
            let mut witnessed = std::collections::BTreeSet::new();
            for (witness_index, witness) in handoff.witnesses.iter().enumerate() {
                for (source_index, event) in witness.source.iter().enumerate() {
                    if !witnessed.insert(event.sequence()) {
                        return Err(MezError::conflict(format!(
                            "duplicate or conflicting terminal compaction source: phase=capture witness_index={witness_index} source_index={source_index} match_count=2"
                        )));
                    }
                    if let Some(current) = events.get(&event.sequence()) {
                        let accepted_summary = event.sequence().get() == witness.anchor
                            && (current.block().source == ContextSourceKind::CompactionSelective
                                || (current.block().source == ContextSourceKind::Memory
                                    && current.block().label == "context compaction summary"))
                            && current.block().content == witness.summary;
                        if current != event && !accepted_summary {
                            return Err(MezError::conflict(format!(
                                "changed terminal compaction source: phase=capture witness_index={witness_index} source_index={source_index}"
                            )));
                        }
                    }
                    events.insert(event.sequence(), event.clone());
                }
            }
        }
        Ok(events.into_values().collect())
    }

    /// Encodes typed execution occurrences once over the complete restored
    /// chronology, including imported predecessors. Witness boundaries never
    /// reset a group's ordinal. Both certificates and newly persisted rows use
    /// this map; legacy/unowned blocks remain outside its typed authority.
    /// Invalid payloads and exhausted ordinals fail before certificate admission.
    pub(in crate::runtime) fn terminal_execution_occurrences(
        chronology: &[mez_agent::ConversationEvent],
    ) -> Result<std::collections::BTreeMap<u64, mez_agent::TranscriptContextEvent>> {
        let mut ordinals = std::collections::BTreeMap::<_, u64>::new();
        let mut occurrences = std::collections::BTreeMap::new();
        for event in chronology {
            let block = event.block();
            if !matches!(
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
            }
            let Some(group) = event.execution_group_id() else {
                continue;
            };
            let ordinal = ordinals.entry(group.clone()).or_default();
            *ordinal = ordinal.checked_add(1).ok_or_else(|| {
                MezError::invalid_state("terminal execution occurrence ordinal exhausted")
            })?;
            let source = mez_agent::TranscriptContextEvent::execution_block_with_metadata(
                block.source,
                block.label.clone(),
                block.content.clone(),
                group.clone(),
                *ordinal,
                event.provider_owner().cloned(),
            )
            .ok_or_else(|| {
                MezError::invalid_state("terminal execution occurrence cannot be encoded")
            })?;
            occurrences.insert(event.sequence().get(), source);
        }
        Ok(occurrences)
    }

    /// Encodes only accepted anchored summaries whose complete source has typed
    /// ownership. Unknown/legacy coverage leaves the original epoch authoritative.
    pub(in crate::runtime) fn terminal_compaction_handoff_content(
        &self,
        turn_id: &str,
    ) -> Result<Option<String>> {
        let Some(handoff) = self.agent.agent_turn_compaction_handoffs.get(turn_id) else {
            return Ok(None);
        };
        let context = self.agent_turn_contexts().get(turn_id).ok_or_else(|| {
            MezError::conflict("accepted terminal compaction context is unavailable")
        })?;
        let chronology = self.terminal_compaction_source_chronology(turn_id, context)?;
        let occurrences = Self::terminal_execution_occurrences(&chronology)?;
        let mut replacements = Vec::new();
        for witness in &handoff.witnesses {
            if self
                .agent_turn_contexts()
                .get(turn_id)
                .is_none_or(|context| {
                    !context.chronology().iter().any(|event| {
                        event.sequence().get() == witness.anchor
                            && (event.block().source == ContextSourceKind::CompactionSelective
                                || (event.block().source == ContextSourceKind::Memory
                                    && event.block().label == "context compaction summary"))
                            && event.block().content == witness.summary
                    })
                })
            {
                return Err(MezError::conflict(
                    "accepted terminal compaction summary anchor changed",
                ));
            }
            let mut sources = Vec::new();
            for event in &witness.source {
                if event.execution_group_id().is_none() {
                    return Ok(None);
                }
                let source = occurrences.get(&event.sequence().get()).ok_or_else(||
                    MezError::conflict("terminal compaction source occurrence is unavailable in canonical chronology"))?;
                sources.push(source.to_transcript_content());
            }
            replacements.push(TerminalCompactionReplacement {
                sources,
                summary: witness.summary.clone(),
            });
        }
        TerminalCompactionHandoff {
            baseline: handoff.baseline.clone(),
            replacements,
        }
        .to_content()
        .map(Some)
    }

    /// Checks the complete logical archive before accepting terminal audit
    /// evidence. Interleaved display rows cannot enter the existing selective
    /// epoch contract; explicitly retain raw replay instead of committing a
    /// permanently unreconcilable witness. Conflicting source remains an error.
    pub(in crate::runtime) fn check_terminal_compaction_handoff_admission(
        &mut self,
        turn: &AgentTurnRecord,
        history: &[TranscriptEntry],
        entries: &mut [TranscriptEntry],
    ) -> Result<()> {
        let mut logical = history.to_vec();
        logical.extend_from_slice(entries);
        for entry in entries {
            if Self::admit_terminal_compaction_handoff_entry(entry, &logical)? {
                if self
                    .agent_shell_store()
                    .get(&turn.pane_id)
                    .is_some_and(|session| session.session_id == turn.conversation_id)
                {
                    self.append_agent_status_text_to_terminal_buffer(&turn.pane_id,
                        "agent: terminal compaction handoff unsupported for interleaved or split execution-group archive source; raw replay remains authoritative")?;
                    self.append_agent_trace_turn_event(&turn.pane_id, &turn.turn_id,
                        "context_compaction terminal_handoff supported=false reason=noncontiguous_archive_source")?;
                } else {
                    self.append_lifecycle_event(crate::runtime::EventKind::Diagnostic, serde_json::json!({
                        "kind": "terminal_compaction_handoff", "conversation_id": turn.conversation_id,
                        "turn_id": turn.turn_id, "supported": false, "reason": "noncontiguous_archive_source",
                    }).to_string())?;
                }
            }
        }
        Ok(())
    }

    /// Checks detached original-archive evidence without pane or diagnostic I/O.
    /// Required malformed/conflicting source errors are unchanged; unsupported
    /// interleaving retains the exact existing raw-authority marker rewrite.
    pub(in crate::runtime) fn check_detached_terminal_compaction_handoff_admission(
        history: &[TranscriptEntry],
        entries: &mut [TranscriptEntry],
    ) -> Result<()> {
        let mut logical = history.to_vec();
        logical.extend_from_slice(entries);
        for entry in entries {
            Self::admit_terminal_compaction_handoff_entry(entry, &logical)?;
        }
        Ok(())
    }

    /// Pure shared validation/transformation for one prospective certificate.
    /// Returns true only when this exact entry becomes the unsupported marker.
    /// The complete prospective archive is required even for per-entry callers;
    /// no live pane identity, presentation, trace, or lifecycle diagnostic enters
    /// this decision. Legacy callers project each returned fact independently.
    fn admit_terminal_compaction_handoff_entry(
        entry: &mut TranscriptEntry,
        logical: &[TranscriptEntry],
    ) -> Result<bool> {
        use crate::storage::transcript::compaction_handoff::{
            MARKER, terminal_handoff_source_is_contiguous,
        };
        if entry.role != TranscriptRole::System || !entry.content.starts_with(MARKER) {
            return Ok(false);
        }
        if terminal_handoff_source_is_contiguous(&entry.content, logical)? {
            return Ok(false);
        }
        entry.content =
            "[mez-terminal-compaction-handoff/unsupported]\nreason=noncontiguous_archive_source"
                .to_string();
        Ok(true)
    }
}
