//! Initial ownership from original accounted raw history before a first head.
//!
//! The shared leased history renderer supplies the source rows and exact model
//! projection. Explicit task admissions reconstruct instruction lifecycle only;
//! imported native groups remain protected. Legacy/mixed source needs its own
//! migration and cannot borrow an accounted task's identity or provenance.

use super::*;
use std::collections::BTreeMap;

impl RuntimeContextRecords {
    /// Qualifies an unsummarized projection without manufacturing publication,
    /// append acknowledgment, execution closure, or a summary identity. Returns
    /// None for legacy source lacking complete authored occurrence custody.
    pub(in crate::runtime::control) fn qualify_raw_history(
        &mut self,
        conversation: &str,
        blocks: Vec<ContextBlock>,
        execution: &[mez_agent::ImportedExecutionEvent],
        rows: &[TranscriptEntry],
    ) -> Result<Option<Vec<ContextBlock>>> {
        self.qualify_execution_controls(None, conversation, rows)?;
        self.ensure_valid()?;
        if self.replay.is_some() || self.task_admissions.is_empty() {
            if !self.control_sequences.is_empty() {
                return Err(invalid());
            }
            return Ok(None);
        }
        let mut authored = BTreeMap::new();
        for row in rows {
            if row.conversation_id != conversation {
                return Err(invalid());
            }
            if row.role == mez_agent::transcript::TranscriptRole::System
                && let Some(record) =
                    TranscriptContextRecord::from_transcript_content(&row.content)?
                && authored
                    .insert(record.record().occurrence_id(), row)
                    .is_some()
            {
                return Err(invalid());
            }
        }
        for (root, start) in &self.task_admissions {
            let root = authored.get(root).ok_or_else(invalid)?;
            let start = authored.get(start).ok_or_else(invalid)?;
            if start.sequence > root.sequence
                || start.turn_id != root.turn_id
                || start.agent_id != root.agent_id
                || start.pane_id != root.pane_id
            {
                return Err(invalid());
            }
        }
        if self.records.len() != blocks.len()
            || self
                .records
                .iter()
                .any(|(_, record)| !authored.contains_key(&record.occurrence_id()))
        {
            if !self.control_sequences.is_empty() {
                return Err(invalid());
            }
            return Ok(None);
        }
        let conversation = mez_agent::ReplayConversationId::new(conversation)?;
        let owner =
            mez_agent::ReplayEpochContext::new(conversation.clone(), AgentContext::empty())?;
        let owner = replay_arrivals::append(owner, conversation.clone(), blocks, self, execution)?;
        let (records, blocks) = Self::from_replay_owner(conversation, owner, None)?;
        self.records = records.records;
        self.replay = records.replay;
        Ok(Some(blocks))
    }
}
