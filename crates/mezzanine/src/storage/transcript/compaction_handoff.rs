//! Source-proven terminal handoffs, independent of active compaction staging.
//!
//! A terminal system row travels through ordinary exact append receipts. It is
//! audit data, never a floating Memory block. Only complete committed typed
//! groups can support its selective replay epoch. Publication compares the
//! captured baseline and exact source under the epoch writer's conversation
//! lock; append retries and restart recovery can repeat the transition safely.

use mez_agent::{
    TranscriptContextEvent,
    transcript::{TranscriptEntry, TranscriptRole},
};
use serde::{Deserialize, Serialize};

use super::{AgentCompactionEpoch, AgentCompactionRange, AgentTranscriptStore};
use crate::error::{MezError, Result};

/// Reserved system-row marker, excluded by ordinary transcript replay.
pub(crate) const MARKER: &str = "[mez-terminal-compaction-handoff/v1]\n";
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
    let mut contiguous = true;
    for replacement in handoff.replacements {
        let mut indexes = Vec::new();
        for source in &replacement.sources {
            let matches = entries
                .iter()
                .enumerate()
                .filter(|(_, row)| row.role == TranscriptRole::System && row.content == *source)
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            if matches.len() != 1 {
                return Err(MezError::conflict(
                    "terminal compaction source occurrence is missing or ambiguous",
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
    }
    Ok(contiguous)
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
        let mut additions = Vec::new();
        let mut frozen = Vec::new();
        for replacement in &handoff.replacements {
            let starts = entries
                .iter()
                .enumerate()
                .filter(|(_, row)| {
                    row.role == TranscriptRole::System && row.content == replacement.sources[0]
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            if starts.len() != 1 {
                return Err(MezError::conflict(
                    "terminal compaction source occurrence is missing or ambiguous",
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
            return Ok(());
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
        self.save_terminal_compaction_ranges_with_proof(epoch, &frozen, &previous)
    }
}
