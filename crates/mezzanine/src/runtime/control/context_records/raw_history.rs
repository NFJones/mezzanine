//! Initial ownership from original accounted raw history before a first head.
//!
//! The shared leased history renderer supplies the source rows and exact model
//! projection. Explicit task admissions reconstruct instruction lifecycle only;
//! imported native groups remain protected. Legacy/mixed source uses original
//! committed row images and cannot borrow another task's identity or provenance.

use super::*;
use std::collections::BTreeMap;

impl RuntimeContextRecords {
    /// Qualifies an unsummarized projection without manufacturing publication,
    /// append acknowledgment, execution closure, or a summary identity. Pending
    /// images remain descriptive custody; the publication worker independently
    /// requires their exact original appends to settle before normalization.
    pub(in crate::runtime::control) fn qualify_raw_history(
        &mut self,
        conversation: &str,
        blocks: Vec<ContextBlock>,
        execution: &[mez_agent::ImportedExecutionEvent],
        rows: &[TranscriptEntry],
    ) -> Result<Option<Vec<ContextBlock>>> {
        self.qualify_execution_controls(None, conversation, rows)?;
        self.ensure_valid()?;
        if self.replay.is_some() {
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
        if self.records.len() != blocks.len() {
            return Err(invalid());
        }
        let legacy = self.task_admissions.is_empty()
            || self
                .records
                .iter()
                .any(|(_, record)| !authored.contains_key(&record.occurrence_id()));
        let framing = if legacy {
            Some(self.legacy_raw_framing(conversation, &blocks, execution)?)
        } else {
            None
        };
        let conversation = mez_agent::ReplayConversationId::new(conversation)?;
        let owner = replay_arrivals::bootstrap(
            conversation.clone(),
            blocks,
            self,
            framing.as_deref().unwrap_or(execution),
        )?;
        let memory =
            if legacy {
                let mut records = self.row_cache.clone();
                records.extend(self.rendered_row_records()?);
                Some(std::sync::Arc::new(
                crate::storage::transcript::replay_epoch::ReplayMemorySource::capture_legacy_raw(
                    &owner, rows, &self.row_origins, &records,
                )?,
            ))
            } else {
                None
            };
        let (mut records, blocks) = Self::from_replay_owner(conversation, owner, None)?;
        records.replay.as_mut().ok_or_else(invalid)?.memory = memory;
        self.records = records.records;
        self.replay = records.replay;
        self.legacy_raw = legacy;
        Ok(Some(blocks))
    }

    /// Describes grouping once over the whole original raw projection, keeping
    /// its actual task partitions. Explicit wire framing wins; inferred names
    /// cannot alias it. The ordinary import port protects every resulting group
    /// and only independently qualified original controls can refine lifecycle.
    fn legacy_raw_framing(
        &self,
        conversation: &str,
        blocks: &[ContextBlock],
        execution: &[mez_agent::ImportedExecutionEvent],
    ) -> Result<Vec<mez_agent::ImportedExecutionEvent>> {
        // This provisional pure import is only a framing view. Flattening the
        // raw blocks would turn several retired task roots into active prompts.
        // The final owner is built with the checked whole-source framing below;
        // no provisional owner, source seal or execution handle escapes here.
        let conversation = mez_agent::ReplayConversationId::new(conversation)?;
        let imported = replay_arrivals::bootstrap(conversation, blocks.to_vec(), self, execution)?;
        let context = imported.context();
        let explicit = execution
            .iter()
            .map(|event| {
                Ok((
                    event.record().ok_or_else(invalid)?.occurrence_id(),
                    event.ordinal(),
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let frames = context.legacy_import_framing(&explicit.keys().copied().collect())?;
        let mut ordinals = BTreeMap::<mez_agent::ContextExecutionGroupId, u64>::new();
        context
            .chronology()
            .iter()
            .zip(frames)
            .filter_map(|(event, frame)| {
                frame.execution_group_id().map(|group| {
                    let ordinal = ordinals.entry(group.clone()).or_default();
                    *ordinal = match explicit.get(&event.record().occurrence_id()) {
                        Some(original) => *original,
                        None => ordinal.checked_add(1).ok_or_else(invalid)?,
                    };
                    Ok(mez_agent::ImportedExecutionEvent::new(
                        event.block().clone(),
                        group.clone(),
                        *ordinal,
                        frame.provider_owner().cloned(),
                    )?
                    .with_record(event.record())?)
                })
            })
            .collect()
    }
}
