//! Source-proven terminal handoffs, independent of active compaction staging.
//!
//! A terminal system row travels through ordinary exact append receipts. It is
//! audit data, never a floating Memory block. Only complete committed typed
//! groups can support its selective replay epoch. Publication compares the
//! captured baseline and exact source under the epoch writer's conversation
//! lock; append retries and restart recovery can repeat the transition safely.
//! Fork capture uses the same source-proof derivation without publishing against
//! the parent. Prospective admission rejects genuine occurrence conflicts while
//! classifying sound split-group layouts as unsupported raw-authoritative history.
//! Exact lookup diagnostics expose only phase and numeric indexes/counts.
//! Accepted authority travels in the child epoch; copied audit rows
//! use a separate inert marker and cannot replay against renumbered child rows.

use mez_agent::{
    TranscriptContextEvent,
    transcript::{TranscriptEntry, TranscriptRole},
};
use serde::{Deserialize, Serialize};

use super::{AgentCompactionEpoch, AgentCompactionRange, AgentTranscriptStore};
use crate::error::{MezError, Result};

/// Reserved system-row marker, excluded by ordinary transcript replay.
pub(crate) const MARKER: &str = "[mez-terminal-compaction-handoff/v1]\n";
/// Inherited audit evidence is inert; its accepted projection travels separately.
pub(crate) const INHERITED_MARKER: &str = "[mez-inherited-compaction-audit/v1]\n";
const MAX_BYTES: usize = 32 * 1024 * 1024;

#[cfg(test)]
mod tests;

/// Checks prospective complete logical source before admitting a certificate.
/// Unique but interleaved occurrences are unsupported, not corrupt. Missing,
/// duplicated or reordered occurrence-bearing source is conflicting evidence.
pub(crate) fn terminal_handoff_source_is_contiguous(
    content: &str,
    entries: &[TranscriptEntry],
) -> Result<bool> {
    let json = content
        .strip_prefix(MARKER)
        .ok_or_else(|| MezError::invalid_args("terminal handoff marker missing"))?;
    let handoff: TerminalCompactionHandoff = serde_json::from_str(json).map_err(|error| {
        MezError::invalid_state(format!(
            "terminal compaction handoff decode failed: {error}"
        ))
    })?;
    handoff.validate()?;
    // Decode each row once: inspecting each group separately would repeatedly
    // allocate large private payloads in multi-witness handoffs.
    let mut group_history = std::collections::BTreeMap::<_, Vec<_>>::new();
    for (index, row) in entries.iter().enumerate() {
        if let Some(TranscriptContextEvent::ExecutionBlock {
            execution_group_id: Some(group),
            ordinal: Some(ordinal),
            ..
        }) = TranscriptContextEvent::from_transcript_content(&row.content)
        {
            group_history
                .entry(group)
                .or_default()
                .push((index, ordinal));
        }
    }
    let mut contiguous = true;
    for (witness_index, replacement) in handoff.replacements.iter().enumerate() {
        let mut indexes = Vec::new();
        for (source_index, source) in replacement.sources.iter().enumerate() {
            let matches = entries
                .iter()
                .enumerate()
                .filter(|(_, row)| row.role == TranscriptRole::System && row.content == *source)
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            if matches.len() != 1 {
                return Err(source_occurrence_conflict(
                    "preappend",
                    witness_index,
                    source_index,
                    replacement.sources.len(),
                    matches.len(),
                ));
            }
            indexes.push(matches[0]);
        }
        if indexes.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(MezError::conflict(
                "terminal compaction source order changed",
            ));
        }
        contiguous &= indexes
            .windows(2)
            .all(|pair| pair[0].checked_add(1) == Some(pair[1]));
        // Every range must own complete groups. A valid late same-group segment
        // across an exact barrier is not representable by the selective epoch;
        // reject malformed ordinal history, but classify a sound split as
        // unsupported before any publication certificate is appended.
        let selected = indexes
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        let groups = replacement
            .sources
            .iter()
            .filter_map(
                |source| match TranscriptContextEvent::from_transcript_content(source) {
                    Some(TranscriptContextEvent::ExecutionBlock {
                        execution_group_id, ..
                    }) => execution_group_id,
                    _ => None,
                },
            )
            .collect::<std::collections::BTreeSet<_>>();
        for group in groups {
            let mut expected = Some(1_u64);
            if let Some(history) = group_history.get(&group) {
                for &(index, ordinal) in history {
                    if Some(ordinal) != expected {
                        return Err(MezError::conflict(format!(
                            "terminal compaction source group ordinals are incomplete or reordered: phase=preappend witness_index={witness_index} source_count={}",
                            replacement.sources.len()
                        )));
                    }
                    expected = ordinal.checked_add(1);
                    contiguous &= selected.contains(&index);
                }
            }
        }
    }
    Ok(contiguous)
}

/// Describes a failed exact occurrence lookup without exposing source payloads,
/// opaque group/provider labels, summaries, or private context. Numeric indexes
/// and counts distinguish candidate admission from committed publication.
fn source_occurrence_conflict(
    phase: &str,
    witness_index: usize,
    source_index: usize,
    source_count: usize,
    match_count: usize,
) -> MezError {
    MezError::conflict(format!(
        "terminal compaction source occurrence is missing or ambiguous: phase={phase} witness_index={witness_index} source_index={source_index} source_count={source_count} match_count={match_count}"
    ))
}

/// Accepted model replacement and its exact, occurrence-bearing source rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TerminalCompactionReplacement {
    /// Canonical typed execution payloads, including group and ordinal.
    pub(crate) sources: Vec<String>,
    /// Bounded accepted model summary, anchored at the first covered row.
    pub(crate) summary: String,
}

/// Distinct terminal publication witness, not an abandoned staged operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TerminalCompactionHandoff {
    /// Prior authoritative epoch captured when local recovery was accepted.
    pub(crate) baseline: Option<AgentCompactionEpoch>,
    /// Ordered replacements whose source identities were retained at acceptance.
    pub(crate) replacements: Vec<TerminalCompactionReplacement>,
}

impl TerminalCompactionHandoff {
    /// Rebinds only a validated conversation-dependent baseline for a fork.
    /// Typed occurrence payloads and model summaries remain byte-for-byte
    /// identical. Foreign baselines are errors, not evidence to launder into
    /// target ownership; publication still independently validates copied rows.
    pub(super) fn fork_content(
        content: &str,
        source_conversation_id: &str,
        target_conversation_id: &str,
    ) -> Result<Option<String>> {
        let Some(json) = content.strip_prefix(MARKER) else {
            return Ok(None);
        };
        if json.len() > MAX_BYTES {
            return Err(MezError::invalid_state(
                "terminal compaction handoff exceeds its bounded size",
            ));
        }
        let mut handoff: Self = serde_json::from_str(json).map_err(|error| {
            MezError::invalid_state(format!(
                "terminal compaction handoff decode failed: {error}"
            ))
        })?;
        handoff.validate()?;
        if let Some(baseline) = &mut handoff.baseline {
            if baseline.conversation_id != source_conversation_id {
                return Err(MezError::conflict(
                    "terminal compaction handoff has foreign baseline identity",
                ));
            }
            baseline.conversation_id = target_conversation_id.to_string();
        }
        handoff.to_content().map(Some)
    }

    /// Decodes one complete bounded witness, or identifies unrelated content.
    fn from_content(content: &str) -> Result<Option<Self>> {
        let Some(json) = content.strip_prefix(MARKER) else {
            return Ok(None);
        };
        if json.len() > MAX_BYTES {
            return Err(MezError::invalid_state(
                "terminal compaction handoff exceeds its bounded size",
            ));
        }
        let handoff: Self = serde_json::from_str(json).map_err(|error| {
            MezError::invalid_state(format!(
                "terminal compaction handoff decode failed: {error}"
            ))
        })?;
        handoff.validate()?;
        Ok(Some(handoff))
    }

    /// Encodes bounded audit evidence; unsupported source cannot be serialized.
    pub(crate) fn to_content(&self) -> Result<String> {
        self.validate()?;
        let json = serde_json::to_string(self).map_err(|error| {
            MezError::invalid_state(format!(
                "terminal compaction handoff encode failed: {error}"
            ))
        })?;
        if json.len() > MAX_BYTES {
            return Err(MezError::invalid_state(
                "terminal compaction handoff exceeds its bounded size",
            ));
        }
        Ok(format!("{MARKER}{json}"))
    }

    /// Rejects floating summaries, missing occurrence identity, or empty evidence.
    fn validate(&self) -> Result<()> {
        if self.replacements.is_empty()
            || self.replacements.iter().any(|replacement| {
                replacement.summary.trim().is_empty()
                    || replacement.summary.len()
                        > mez_agent::http::DEFAULT_PROVIDER_MAX_RESPONSE_BYTES
                    || replacement.sources.is_empty()
                    || replacement.sources.iter().any(|source| {
                        !matches!(
                            TranscriptContextEvent::from_transcript_content(source),
                            Some(TranscriptContextEvent::ExecutionBlock {
                                execution_group_id: Some(_),
                                ordinal: Some(_),
                                ..
                            })
                        )
                    })
            })
        {
            return Err(MezError::invalid_state(
                "terminal compaction handoff lacks typed source ownership",
            ));
        }
        Ok(())
    }
}

impl AgentTranscriptStore {
    /// Reconciles committed terminal witnesses before admitting a new replay.
    /// Pending rows are not publication proof. Stale or conflicting evidence
    /// leaves the old epoch authoritative and returns an explicit error.
    pub(crate) fn publish_terminal_compaction_handoffs(&self, conversation_id: &str) -> Result<()> {
        let _conversation_lock = self.acquire_conversation_lock(conversation_id)?;
        if self.active_delete_pending(conversation_id)? {
            return Err(MezError::invalid_state(
                "conversation publication or deletion is incomplete",
            ));
        }
        self.publish_terminal_compaction_handoffs_locked(conversation_id)
    }

    /// Reconciles through the ordinary owner under an already-held mutation
    /// lock. Fork uses this only on its new target, never on the source archive.
    pub(super) fn publish_terminal_compaction_handoffs_locked(
        &self,
        conversation_id: &str,
    ) -> Result<()> {
        let entries = match self.inspect(conversation_id) {
            Ok(entries) => entries,
            Err(error) if error.kind() == crate::error::MezErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        for entry in &entries {
            if entry.role != TranscriptRole::System {
                continue;
            }
            let Some(json) = entry.content.strip_prefix(MARKER) else {
                continue;
            };
            if json.len() > MAX_BYTES {
                return Err(MezError::invalid_state(
                    "terminal compaction handoff exceeds its bounded size",
                ));
            }
            let handoff: TerminalCompactionHandoff =
                serde_json::from_str(json).map_err(|error| {
                    MezError::invalid_state(format!(
                        "terminal compaction handoff decode failed: {error}"
                    ))
                })?;
            handoff.validate()?;
            self.publish_terminal_compaction_handoff(
                conversation_id,
                entry.sequence,
                &entries,
                handoff,
            )?;
        }
        Ok(())
    }

    /// Maps exact frozen payloads to one contiguous committed occurrence and
    /// publishes only a complete, baseline-fenced projection.
    fn publish_terminal_compaction_handoff(
        &self,
        conversation_id: &str,
        terminal_sequence: u64,
        entries: &[TranscriptEntry],
        handoff: TerminalCompactionHandoff,
    ) -> Result<()> {
        let previous = self.compaction_epoch(conversation_id)?;
        if let Some((epoch, frozen)) = project_terminal_handoff(
            conversation_id,
            terminal_sequence,
            entries,
            handoff,
            previous.clone(),
        )? {
            self.save_terminal_compaction_ranges_with_proof_locked(epoch, &frozen, &previous)?;
        }
        Ok(())
    }
}

/// Derives the same exact baseline-fenced transition without storage mutation.
/// Source occurrence, ordering and coverage checks are shared by live
/// publication and immutable fork capture; range validation remains caller-owned.
fn project_terminal_handoff(
    conversation_id: &str,
    terminal_sequence: u64,
    entries: &[TranscriptEntry],
    handoff: TerminalCompactionHandoff,
    previous: Option<AgentCompactionEpoch>,
) -> Result<Option<(AgentCompactionEpoch, Vec<TranscriptEntry>)>> {
    let mut additions = Vec::new();
    let mut frozen = Vec::new();
    for (witness_index, replacement) in handoff.replacements.iter().enumerate() {
        let starts = entries
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                row.role == TranscriptRole::System && row.content == replacement.sources[0]
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if starts.len() != 1 {
            return Err(source_occurrence_conflict(
                "committed-publication",
                witness_index,
                0,
                replacement.sources.len(),
                starts.len(),
            ));
        }
        let rows = entries
            .get(starts[0]..starts[0].saturating_add(replacement.sources.len()))
            .ok_or_else(|| MezError::conflict("terminal compaction source is incomplete"))?;
        if rows.iter().zip(&replacement.sources).any(|(row, source)| {
            row.role != TranscriptRole::System
                || row.content != *source
                || row.sequence >= terminal_sequence
        }) {
            return Err(MezError::conflict(
                "terminal compaction frozen source changed",
            ));
        }
        let range = AgentCompactionRange {
            first_sequence: rows[0].sequence,
            through_sequence: rows[rows.len() - 1].sequence,
            summary: replacement.summary.clone(),
        };
        // Exact repeat or a subsequently compacted prefix is already durable.
        if previous.as_ref().is_some_and(|epoch| {
            range.through_sequence <= epoch.through_sequence
                || epoch.ranges.iter().any(|later| {
                    later.first_sequence <= range.first_sequence
                        && later.through_sequence >= range.through_sequence
                })
        }) {
            continue;
        }
        additions.push(range);
        frozen.extend_from_slice(rows);
    }
    if additions.is_empty() {
        return Ok(None);
    }
    if previous != handoff.baseline {
        return Err(MezError::conflict(
            "terminal compaction epoch baseline changed",
        ));
    }
    let mut epoch = previous.clone().unwrap_or_else(|| AgentCompactionEpoch {
        version: 2,
        conversation_id: conversation_id.to_string(),
        through_sequence: 0,
        summary: String::new(),
        ranges: Vec::new(),
    });
    epoch.version = 2;
    epoch.ranges.extend(additions);
    epoch.ranges.sort_by_key(|range| range.first_sequence);
    frozen.sort_by_key(|row| row.sequence);
    Ok(Some((epoch, frozen)))
}

/// Projects committed terminal witnesses into a frozen source without changing
/// the parent. The caller supplies its checked full committed prefix and leased
/// epoch, already limited to the captured frontier. Complete groups, baseline,
/// ordering and encoded sidecar bounds match actual publication; pending rows
/// never supply this authority.
pub(crate) fn fork_terminal_projection(
    conversation_id: &str,
    entries: &[TranscriptEntry],
    mut epoch: Option<AgentCompactionEpoch>,
) -> Result<Option<AgentCompactionEpoch>> {
    for entry in entries {
        if entry.conversation_id != conversation_id {
            return Err(MezError::conflict(
                "fork terminal source has foreign conversation identity",
            ));
        }
        if entry.role != TranscriptRole::System {
            continue;
        }
        let Some(handoff) = TerminalCompactionHandoff::from_content(&entry.content)? else {
            continue;
        };
        if handoff
            .baseline
            .as_ref()
            .is_some_and(|baseline| baseline.conversation_id != conversation_id)
        {
            return Err(MezError::conflict(
                "fork terminal source has foreign baseline identity",
            ));
        }
        if let Some((proposed, _)) = project_terminal_handoff(
            conversation_id,
            entry.sequence,
            entries,
            handoff,
            epoch.clone(),
        )? {
            super::store::validate_compaction_ranges(
                &proposed.ranges,
                proposed.through_sequence,
                entries
                    .last()
                    .map_or(1, |row| row.sequence.saturating_add(1)),
            )?;
            super::store::validate_compaction_range_sources(&proposed.ranges, entries)?;
            let encoded = serde_json::to_vec(&proposed).map_err(|error| {
                MezError::invalid_state(format!("fork terminal projection encode failed: {error}"))
            })?;
            if encoded.len().saturating_add(1) as u64 > super::store::COMPACTION_EPOCH_MAX_BYTES {
                return Err(MezError::invalid_state(
                    "fork terminal projection exceeds accepted sidecar size",
                ));
            }
            epoch = Some(proposed);
        }
    }
    Ok(epoch)
}

/// Retains a validated source witness as inert audit provenance after its
/// accepted authority has been folded into the child's initial projection.
/// Original JSON bytes remain unchanged and cannot publish a second transition.
pub(crate) fn inherited_terminal_audit(content: &str) -> Result<Option<String>> {
    if TerminalCompactionHandoff::from_content(content)?.is_none() {
        return Ok(None);
    }
    Ok(content
        .strip_prefix(MARKER)
        .map(|json| format!("{INHERITED_MARKER}{json}")))
}
