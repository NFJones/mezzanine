//! Accepted summary occurrence ownership across runtime compaction previews.
//!
//! An active summary takes the exact record created by canonical replacement;
//! a prefix proposal allocates once after the final bounded model output.
//! Source selection and output qualify reuse of that retained proposal. Pure
//! hypothetical previews may create their own small, unpublished record, but
//! durable prefix publication requires the actual accepted proposal.

use super::*;
use crate::runtime::agent_state::RuntimeCompactionSummaryProposal;
use mez_agent::{ContextBlockRecord, ModelContextCompactionPlan};

impl RuntimeAgentCompactionTask {
    /// Captures the actual canonical replacement by its exact ordering slot,
    /// validating the expected source and body without searching equal text.
    pub(super) fn retain_selective_summary_record(
        &mut self,
        compacted: &AgentContext,
        plan: &ModelContextCompactionPlan,
        summary: &str,
    ) -> Result<()> {
        let anchor = plan.replacement_event_sequences().first().ok_or_else(|| {
            MezError::invalid_state("accepted compaction summary has no replacement owner")
        })?;
        let event = compacted
            .chronology()
            .iter()
            .find(|event| event.sequence() == *anchor)
            .filter(|event| {
                event.block().source == ContextSourceKind::CompactionSelective
                    && event.block().content == summary
            })
            .ok_or_else(|| {
                MezError::invalid_state("accepted canonical compaction summary is unavailable")
            })?;
        self.summary_proposal = Some(RuntimeCompactionSummaryProposal {
            summary: summary.to_string(),
            source_sequences: plan.replacement_event_sequences().to_vec(),
            record: event.record(),
        });
        Ok(())
    }

    /// Captures one final prefix proposal before sizing/publication. Each new
    /// model output is a distinct proposed occurrence even with equal bytes.
    pub(super) fn retain_prefix_summary_record(&mut self, summary: &str) -> Result<()> {
        self.summary_proposal = Some(RuntimeCompactionSummaryProposal {
            summary: summary.to_string(),
            source_sequences: Vec::new(),
            record: ContextBlockRecord::allocate(&self.prefix_summary_block(summary))?,
        });
        Ok(())
    }

    /// Reuses an accepted selection/output record, or creates an unpublished
    /// hypothetical record for a different proposed selection or sizing body.
    pub(super) fn selective_summary_record_for_preview(
        &self,
        plan: &ModelContextCompactionPlan,
        summary: &str,
    ) -> Result<ContextBlockRecord> {
        if let Some(proposal) = &self.summary_proposal
            && proposal.summary == summary
            && proposal.source_sequences.as_slice() == plan.replacement_event_sequences()
        {
            return Ok(proposal.record);
        }
        Ok(ContextBlockRecord::allocate(
            &crate::storage::transcript::selective_summary_block(summary),
        )?)
    }

    /// Builds one prospective prefix record. Normal completion already retains
    /// it; callers previewing an unaccepted output own only a temporary proposal.
    pub(super) fn prefix_summary_record_for_preview(
        &self,
        summary: &str,
    ) -> Result<ContextBlockRecord> {
        if let Some(record) = self.retained_prefix_summary_record(summary) {
            return Ok(record);
        }
        Ok(ContextBlockRecord::allocate(
            &self.prefix_summary_block(summary),
        )?)
    }

    /// Requires the exact accepted prefix proposal at durable publication.
    pub(super) fn accepted_prefix_summary_record(
        &self,
        summary: &str,
    ) -> Result<ContextBlockRecord> {
        self.retained_prefix_summary_record(summary).ok_or_else(|| {
            MezError::invalid_state("accepted prefix compaction summary record is unavailable")
        })
    }

    /// Checks the actual retained proposal; body equality validates ownership
    /// rather than assigning an identity to a matching text occurrence.
    fn retained_prefix_summary_record(&self, summary: &str) -> Option<ContextBlockRecord> {
        self.summary_proposal
            .as_ref()
            .filter(|proposal| proposal.source_sequences.is_empty() && proposal.summary == summary)
            .map(|proposal| proposal.record)
    }

    /// Renders the exact framed prefix used by candidate replay and storage.
    fn prefix_summary_block(&self, summary: &str) -> ContextBlock {
        crate::storage::transcript::prefix_summary_block(
            &self.conversation_id,
            &runtime_model_compact_memory_content(
                &self.pane_id,
                &self.conversation_id,
                self.transcript_entries,
                self.summarized_entries,
                &self.model_profile_name,
                &self.model_profile,
                summary,
            ),
        )
    }
}
