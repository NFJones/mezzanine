//! Immutable checked raw history before its first model-authored head.
//!
//! This capture is descriptive source, not another epoch or append journal.
//! Preparation requalifies original row images and cached projection records
//! under the existing conversation lease. The ordinary publisher owns the only
//! head, receipt and replacement; legacy native groups gain no completion right.

use super::*;
use mez_agent::TranscriptEntry;
use std::sync::Arc;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ReplayRawHistorySource {
    root: std::path::PathBuf,
    body: AgentContext,
    rows: Arc<[TranscriptEntry]>,
}

impl std::fmt::Debug for ReplayRawHistorySource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ReplayRawHistorySource")
    }
}

impl ReplayRawHistorySource {
    /// Freezes the actual renderer's owned zero-head body and original window.
    /// Pending rows retain descriptive source only; publication requires their
    /// exact physical/index/cost evidence and cannot substitute another store.
    pub(crate) fn capture(
        store: &AgentTranscriptStore,
        owner: ReplayEpochContext,
        rows: Vec<TranscriptEntry>,
    ) -> Result<Self> {
        if owner.head().is_some()
            || owner.revision().get() != 0
            || rows.is_empty()
            || rows
                .iter()
                .any(|row| row.conversation_id != owner.conversation_id().as_str())
            || rows
                .windows(2)
                .any(|pair| pair[0].sequence.checked_add(1) != Some(pair[1].sequence))
        {
            return Err(invalid(
                "raw source capture changed its original projection",
            ));
        }
        Ok(Self {
            root: store.root.clone(),
            body: owner.into_owned_context(),
            rows: rows.into(),
        })
    }

    /// Called with the publication's held conversation lease. Original legacy
    /// images establish source custody only, never an ordinary append ACK.
    pub(super) fn prepare(
        &self,
        store: &AgentTranscriptStore,
        source: &ReplayEpochContext,
    ) -> Result<(Vec<escrow::EscrowOccurrence>, cursor::ArchiveCut)> {
        let conversation = source.conversation_id().as_str();
        if store.root != self.root
            || source.head().is_some()
            || source.revision().get() != 0
            || store.has_replay_manifest(conversation)?
            || store.compaction_epoch_locked(conversation)?.is_some()
        {
            return Err(MezError::conflict("raw normalization authority changed"));
        }
        let original = ReplayEpochContext::from_owned_context(
            ReplayConversationId::new(conversation)?,
            self.body.clone(),
        )?;
        source.validate_source_extension(&original)?;
        let current = store.inspect_validated_transcript(conversation)?;
        let first = self
            .rows
            .first()
            .ok_or_else(|| invalid("raw source window is missing"))?
            .sequence;
        let through = self
            .rows
            .last()
            .ok_or_else(|| invalid("raw source frontier is missing"))?
            .sequence;
        let actual = current
            .iter()
            .filter(|row| (first..=through).contains(&row.sequence));
        if !actual.eq(self.rows.iter()) {
            return Err(MezError::conflict(
                "raw normalization original rows changed",
            ));
        }
        let prefix = current
            .iter()
            .take_while(|row| row.sequence <= through)
            .cloned()
            .collect::<Vec<_>>();
        store.capture_legacy_prefix_images_locked(conversation, &prefix)?;
        let qualified = escrow::legacy_prefix_source(
            store,
            conversation,
            first.saturating_sub(1),
            through,
            &original,
            &self.rows,
            || Err(invalid("raw dependency is outside its original projection")),
        )?;
        let cut = cursor::ArchiveCut::for_copied_archive(store, conversation, through)?;
        Ok((qualified.items, cut))
    }
}
