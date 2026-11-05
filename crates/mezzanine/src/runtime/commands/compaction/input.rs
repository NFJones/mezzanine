//! Compactor input separates physical publication proof from model-visible rows.
//!
//! Selected selective ranges render once at their original sequence anchors.
//! Covered raw content and unselected range summaries never enter that request;
//! physical rows remain available to the caller for count and publication proof.
//! Accounted system rows contribute only their semantic event. Their occurrence,
//! costs and stable snapshots remain in the physical proof; malformed reserved
//! records abort request construction instead of becoming model-visible text.
//! Terminal handoff certificates are audit proof, including nested original
//! records, and never contribute semantic compactor input.
//! This is an ephemeral request projection, not a new durable transcript codec.

use std::borrow::Cow;

use super::*;
use crate::storage::transcript::AgentCompactionRange;

/// Borrowed, qualified physical source and its selected summary replacements.
#[derive(Debug, Clone, Copy)]
pub(in crate::runtime::commands) struct CompactionTranscriptSource<'a> {
    /// Physical source rows, including rows represented by selected summaries.
    pub(super) rows: &'a [TranscriptEntry],
    /// Whole selected ranges in the same frozen physical source.
    ranges: &'a [AgentCompactionRange],
    /// An authoritative selective snapshot replaces cached range memory blocks,
    /// including when every range lies after this selected prefix.
    selective_history: bool,
}

impl<'a> CompactionTranscriptSource<'a> {
    /// Retains ordinary nonselective compactor input without changing its shape.
    pub(in crate::runtime::commands) fn raw(rows: &'a [TranscriptEntry]) -> Self {
        Self {
            rows,
            ranges: &[],
            selective_history: false,
        }
    }

    /// Qualifies whole selected ranges against their physical source. A partial,
    /// overlapping, absent or reversed range is an error before model dispatch.
    pub(super) fn projected(
        rows: &'a [TranscriptEntry],
        ranges: &'a [AgentCompactionRange],
    ) -> Result<Self> {
        let mut previous = 0;
        for range in ranges {
            if range.first_sequence <= previous
                || range.first_sequence > range.through_sequence
                || range.summary.trim().is_empty()
                || !rows.iter().any(|row| row.sequence == range.first_sequence)
                || !rows
                    .iter()
                    .any(|row| row.sequence == range.through_sequence)
            {
                return Err(MezError::invalid_state(
                    "compactor input splits or loses a selective range",
                ));
            }
            previous = range.through_sequence;
        }
        Ok(Self {
            rows,
            ranges,
            selective_history: true,
        })
    }

    /// Removes raw replay and cached selective summaries before rendering the
    /// selected replacements inline. The earlier prefix summary and unrelated
    /// exact references preserve their original metadata and ordering.
    pub(super) fn prepare_context(self, context: AgentContext) -> Result<AgentContext> {
        let mut context = runtime_compaction_context_without_transcript_blocks(context)?;
        if self.selective_history {
            context.retain_blocks_with_metadata(|block, _| {
                !(block.source == ContextSourceKind::CompactionSelective
                    || (block.source == ContextSourceKind::Memory
                        && block.label == "context compaction summary"))
            })?;
        }
        Ok(context.revalidate()?)
    }

    /// Appends ordered source lines with summaries replacing covered raw rows.
    /// Summary bytes use the same credential redaction as ordinary source text.
    /// Invalid accounted system records fail before a request can be returned;
    /// non-system marker text remains literal user or assistant source.
    pub(super) fn append_lines(self, lines: &mut Vec<String>) -> Result<()> {
        if self.selective_history {
            let raw = self
                .rows
                .iter()
                .filter(|entry| {
                    !self.ranges.iter().any(|range| {
                        (range.first_sequence..=range.through_sequence).contains(&entry.sequence)
                    })
                })
                .count();
            lines.push(format!("Raw entries supplied for compaction: {raw}"));
            lines.push(format!(
                "Selective summaries supplied for compaction: {}",
                self.ranges.len()
            ));
        }
        for entry in self.rows {
            if let Some(range) = self
                .ranges
                .iter()
                .find(|range| range.first_sequence == entry.sequence)
            {
                lines.push(format!(
                    "summary_range={}..={} content={}",
                    range.first_sequence,
                    range.through_sequence,
                    runtime_model_compaction_entry_content(&range.summary)
                ));
            }
            if self.ranges.iter().any(|range| {
                (range.first_sequence..=range.through_sequence).contains(&entry.sequence)
            }) {
                continue;
            }
            if entry.role == TranscriptRole::System
                && (entry
                    .content
                    .starts_with("[mez-terminal-compaction-handoff/")
                    || entry
                        .content
                        .starts_with("[mez-inherited-compaction-audit/"))
            {
                continue;
            }
            let content = if entry.role == TranscriptRole::System {
                mez_agent::transcript::TranscriptContextRecord::from_transcript_content(
                    &entry.content,
                )
                .map_err(|error| {
                    MezError::invalid_state(format!(
                        "invalid compactor input at transcript sequence {}: {error}",
                        entry.sequence
                    ))
                })?
                .map_or(Cow::Borrowed(entry.content.as_str()), |record| {
                    Cow::Owned(record.into_event().to_transcript_content())
                })
            } else {
                Cow::Borrowed(entry.content.as_str())
            };
            lines.push(format!(
                "entry={} role={} turn={} pane={} content={}",
                entry.sequence,
                runtime_transcript_role_name(entry.role),
                entry.turn_id,
                entry.pane_id,
                runtime_model_compaction_entry_content(&content)
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Actual terminal certificates embed accounted source records for exact
    /// publication proof. Neither those records nor the certificate's own
    /// summary may leak into a later auxiliary body. Inherited audit is equally
    /// inert, while a user quoting the reserved marker remains authored text.
    #[test]
    fn compactor_request_excludes_terminal_and_inherited_accounting_audit() {
        use crate::storage::transcript::compaction_handoff::{
            INHERITED_MARKER, MARKER, TerminalCompactionHandoff, TerminalCompactionReplacement,
        };
        let block = ContextBlock::assistant_event("audit source", "AUDIT_ONLY_SOURCE");
        let record = mez_agent::ContextBlockRecord::allocate(&block).unwrap();
        let event = mez_agent::TranscriptContextEvent::execution_block_with_metadata(
            block.source,
            block.label,
            block.content,
            mez_agent::ContextExecutionGroupId::new("audit-source").unwrap(),
            1,
            None,
        )
        .unwrap();
        let encoded = mez_agent::transcript::TranscriptContextRecord::new(event, record, None)
            .unwrap()
            .to_transcript_content()
            .unwrap();
        let certificate = TerminalCompactionHandoff {
            baseline: None,
            replacements: vec![TerminalCompactionReplacement {
                summary_record: None,
                sources: vec![encoded],
                summary: "AUDIT_ONLY_SUMMARY".to_string(),
            }],
        }
        .to_content()
        .unwrap();
        let occurrence: String = record.occurrence_id().into();
        assert!(certificate.contains(&occurrence));
        let profile = ModelProfile {
            provider: "test".to_string(),
            model: "test".to_string(),
            ..Default::default()
        };
        let context = AgentContext::new_durable(vec![ContextBlock {
            source: ContextSourceKind::System,
            placement: mez_agent::ContextPlacement::StablePrefix,
            label: "compactor guidance".to_string(),
            content: "Summarize supplied semantic source.".to_string(),
        }])
        .unwrap();
        let visible = TranscriptEntry {
            conversation_id: "audit-source".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::Assistant,
            turn_id: "source-turn".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "VISIBLE_COMPACTOR_SOURCE".to_string(),
        };
        for content in [
            certificate.clone(),
            certificate.replacen(MARKER, INHERITED_MARKER, 1),
        ] {
            let mut rows = vec![
                visible.clone(),
                TranscriptEntry {
                    sequence: 2,
                    role: TranscriptRole::System,
                    content,
                    ..visible.clone()
                },
            ];
            let build = |rows: &[TranscriptEntry]| {
                let request = runtime_model_compaction_request(
                    &profile,
                    "%1",
                    "audit-source",
                    rows.len() as u64,
                    CompactionTranscriptSource::raw(rows),
                    &context,
                    AllowedActionSet::from_actions([mez_agent::AllowedAction::Say]),
                )
                .unwrap();
                mez_agent::openai_responses_request_body(&request).unwrap()
            };
            let body = build(&rows);
            assert!(body.contains("VISIBLE_COMPACTOR_SOURCE"));
            for hidden in [
                occurrence.as_str(),
                "AUDIT_ONLY_SOURCE",
                "AUDIT_ONLY_SUMMARY",
                "rendered_sha256",
                "mez-transcript-context-record/",
            ] {
                assert!(!body.contains(hidden), "{hidden}");
            }
            rows[1].role = TranscriptRole::User;
            assert!(build(&rows).contains(rows[1].content.lines().next().unwrap()));
        }
    }

    /// A malformed or future reserved system record must prevent construction
    /// of the entire auxiliary request. The identical bytes in an ordinary
    /// user row are authored text and must survive with no metadata authority.
    #[test]
    fn compactor_request_rejects_invalid_accounting_and_preserves_literal_user_text() {
        let profile = ModelProfile::default();
        let context = AgentContext::new_durable(vec![ContextBlock {
            source: ContextSourceKind::System,
            placement: mez_agent::ContextPlacement::StablePrefix,
            label: "compactor guidance".to_string(),
            content: "Summarize the supplied source.".to_string(),
        }])
        .unwrap();
        for content in [
            "[mez-transcript-context-record/v1]\n{}",
            "[mez-transcript-context-record/v999]\n{}",
        ] {
            let mut entry = TranscriptEntry {
                conversation_id: "invalid-accounting".to_string(),
                sequence: 1,
                created_at_unix_seconds: 1,
                role: TranscriptRole::System,
                turn_id: "source-turn".to_string(),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: content.to_string(),
            };
            let build = |entry: &TranscriptEntry| {
                runtime_model_compaction_request(
                    &profile,
                    "%1",
                    "invalid-accounting",
                    1,
                    CompactionTranscriptSource::raw(std::slice::from_ref(entry)),
                    &context,
                    AllowedActionSet::from_actions([mez_agent::AllowedAction::Say]),
                )
            };
            let error = build(&entry).unwrap_err();
            assert!(error.message().contains("invalid compactor input"));
            entry.role = TranscriptRole::User;
            let request = build(&entry).unwrap();
            assert!(request.messages.last().unwrap().content.contains(content));
        }
    }
}
