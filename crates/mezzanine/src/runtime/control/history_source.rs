//! Original replay ownership retained beside the actual imported chronology.
//!
//! The history worker captures this value from the same leased read that built
//! model history. Clones share physical source images rather than rediscovering
//! a store, pane binding, pending append, or row after asynchronous work. Images
//! establish the frozen proof only: publication still validates the original
//! archive under its mutation lease. They never enter provider messages.

use super::{RuntimeAgentHistoryEpochWork, RuntimeAgentTranscriptContext};
use crate::{MezError, Result};
#[cfg(test)]
use mez_agent::ContextBlockOccurrenceId;
use mez_agent::TranscriptRole;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// Immutable original store, replay inputs, epoch and accepted physical rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeAgentHistorySource {
    /// Store and pending rows captured before the original history read.
    pub(super) work: Option<RuntimeAgentHistoryEpochWork>,
    /// Exact authoritative epoch read with these rows, including absence.
    pub(in crate::runtime) epoch: Option<crate::storage::transcript::AgentCompactionEpoch>,
    /// Typed source and committed parent captured in the same actual read.
    pub(in crate::runtime) replay:
        Option<Arc<crate::storage::transcript::replay_epoch::ReplayEpochSnapshot>>,
    /// Exact memory-only source of an imported historical prefix. Live NoStore
    /// continuation must keep this read's producers after flat transport ends.
    pub(super) memory: Option<Arc<crate::storage::transcript::replay_epoch::ReplayMemorySource>>,
    /// Original raw projection, distinct from a committed typed parent.
    pub(in crate::runtime) raw:
        Option<Arc<crate::storage::transcript::replay_epoch::ReplayRawHistorySource>>,
    /// Original checked logical window, shared through retries/interruption.
    rows: Arc<[mez_agent::TranscriptEntry]>,
    /// Original legacy positions from the same leased row read.
    row_origins: crate::storage::transcript::TranscriptRowOrigins,
    /// Target policy captured at turn admission, independent of source window.
    target_ephemeral: Option<bool>,
}

impl RuntimeAgentHistorySource {
    /// Requires saved archive projection to retain this read's accepted head.
    /// Append-only work may advance raw source, but a locally valid unpublished
    /// rewrite cannot replace the acknowledged publication. Restoring this
    /// descriptive snapshot performs no I/O and grants no execution rights.
    /// Memory-only rewrites have no durable publication base to acknowledge.
    pub(in crate::runtime) fn validate_published_replay_head(
        &self,
        conversation: &str,
        context: &mez_agent::AgentContext,
    ) -> Result<()> {
        let Some(snapshot) = self
            .replay
            .as_ref()
            .filter(|_| self.ephemeral() != Some(true))
        else {
            return Ok(());
        };
        let conversation = mez_agent::ReplayConversationId::new(conversation)?;
        let checkpoint = snapshot.checkpoint().clone();
        let checkpoint = if snapshot.conversation_id() == conversation.as_str() {
            checkpoint
        } else {
            checkpoint.for_conversation_copy(conversation.clone())?
        };
        let accepted = mez_agent::ReplayEpochContext::from_checkpoint(
            conversation.clone(),
            checkpoint,
            mez_agent::AgentContext::empty(),
        )?;
        let current =
            mez_agent::ReplayEpochContext::from_owned_context(conversation, context.clone())?;
        if current.revision() != accepted.revision()
            || current.head() != accepted.head()
            || (accepted.head().is_some()
                && current.context().chronology().first()
                    != accepted.context().chronology().first())
        {
            return Err(MezError::conflict(
                "transcript projection differs from original accepted replay head",
            ));
        }
        Ok(())
    }

    /// Checks a mixed inherited/new group against original captured images and
    /// escrow before ordinary bookkeeping claims this producer. No store read,
    /// effect receipt or source-copy permission is inferred from matching text.
    pub(in crate::runtime) fn qualify_execution_control_append(
        &self,
        entries: &[mez_agent::TranscriptEntry],
        control: &mez_agent::TranscriptEntry,
    ) -> Result<()> {
        let mut rows = self.rows.to_vec();
        rows.extend_from_slice(entries);
        rows.push(control.clone());
        crate::storage::transcript::replay_epoch::qualify_execution_controls(
            self.replay.as_deref(),
            &control.conversation_id,
            &rows,
        )?;
        Ok(())
    }

    /// Advances only the acknowledged replay base while keeping original row
    /// images, store and admitted target policy. This descriptor does not adopt
    /// a cold owner into the actor's live model context or grant append rights.
    pub(in crate::runtime) fn with_replay_publication(
        original: Option<&Arc<Self>>,
        store: crate::storage::transcript::AgentTranscriptStore,
        pane: &str,
        snapshot: Arc<crate::storage::transcript::replay_epoch::ReplayEpochSnapshot>,
        ephemeral: bool,
    ) -> Result<Arc<Self>> {
        let mut source = original.cloned().unwrap_or_else(Self::absent);
        let source_mut = Arc::make_mut(&mut source);
        if let Some(work) = source_mut.work.as_ref() {
            if work.store != store || work.inputs.conversation_id != snapshot.conversation_id() {
                return Err(MezError::conflict(
                    "acknowledged replay history has a foreign original store",
                ));
            }
        } else {
            source_mut.work = Some(RuntimeAgentHistoryEpochWork {
                pending_capture_error: None,
                store,
                inputs: super::RuntimeAgentHistoryEpochInputs {
                    pane_id: pane.to_owned(),
                    conversation_id: snapshot.conversation_id().to_owned(),
                    ephemeral_source_entries: None,
                    active_entries: Some(0),
                    pending_entries: Default::default(),
                },
            });
        }
        source_mut.epoch = None;
        source_mut.replay = Some(snapshot);
        source_mut.memory = None;
        source_mut.raw = None;
        source_mut.target_ephemeral = Some(ephemeral);
        Ok(source)
    }

    /// Clones the original store handle without sampling installation state.
    pub(in crate::runtime) fn store(
        &self,
    ) -> Option<crate::storage::transcript::AgentTranscriptStore> {
        self.work.as_ref().map(|work| work.store.clone())
    }

    /// Original checked wire images, retained independently of later stores.
    pub(in crate::runtime) fn original_rows(&self) -> &[mez_agent::TranscriptEntry] {
        &self.rows
    }

    /// Identifies the archive that actually supplied these imported events.
    #[cfg(test)]
    pub(in crate::runtime) fn conversation_id(&self) -> Option<&str> {
        self.work
            .as_ref()
            .map(|work| work.inputs.conversation_id.as_str())
    }

    /// Actual target save policy captured at admission, not the source pane's
    /// replay-window policy (a persistent child may inherit ephemeral history).
    pub(in crate::runtime) fn ephemeral(&self) -> Option<bool> {
        self.target_ephemeral
    }

    /// Shares unchanged captures; a distinct target policy owns only a new
    /// descriptor while retaining the same original physical source images.
    pub(in crate::runtime) fn with_target_policy(
        mut source: Arc<Self>,
        ephemeral: bool,
    ) -> Arc<Self> {
        if source.target_ephemeral != Some(ephemeral) {
            Arc::make_mut(&mut source).target_ephemeral = Some(ephemeral);
        }
        source
    }

    /// Freezes genuine source absence at the original prompt boundary.
    pub(in crate::runtime) fn absent() -> Arc<Self> {
        Arc::new(Self {
            work: None,
            epoch: None,
            replay: None,
            memory: None,
            raw: None,
            rows: Arc::from([]),
            row_origins: BTreeMap::new(),
            target_ephemeral: None,
        })
    }

    /// Attaches source from the same actual read that produced this history.
    pub(super) fn attach(
        history: &mut RuntimeAgentTranscriptContext,
        work: RuntimeAgentHistoryEpochWork,
        epoch: Option<crate::storage::transcript::AgentCompactionEpoch>,
        rows: Vec<mez_agent::TranscriptEntry>,
    ) {
        let row_origins = history.records.source_row_origins().clone();
        let replay = history.records.replay_snapshot();
        let memory = history.records.replay_memory_source();
        let raw = if history.records.legacy_raw {
            let capture = history.replay_owner().and_then(|owner| {
                crate::storage::transcript::replay_epoch::ReplayRawHistorySource::capture(
                    &work.store,
                    owner.ok_or_else(|| {
                        MezError::invalid_state("raw source lost its canonical owner")
                    })?,
                    rows.clone(),
                )
            });
            match capture {
                Ok(source) => Some(Arc::new(source)),
                Err(error) => {
                    history.records.reject_source(&error);
                    None
                }
            }
        } else {
            None
        };
        history.source = Some(Arc::new(Self {
            work: Some(work),
            epoch,
            replay,
            memory,
            raw,
            rows: rows.into(),
            row_origins,
            target_ephemeral: None,
        }));
    }

    /// Resolves only actual selected occurrences in the frozen original window.
    /// Legacy ordinary rows are explicitly present without typed ownership.
    /// Missing, duplicated or malformed typed sources fail without consulting a
    /// replacement archive or accepting content equality as occurrence identity.
    #[cfg(test)]
    pub(in crate::runtime) fn selected_images(
        &self,
        needed: &BTreeSet<ContextBlockOccurrenceId>,
    ) -> Result<BTreeMap<ContextBlockOccurrenceId, Option<String>>> {
        let mut captured = BTreeMap::new();
        for row in self.rows.iter() {
            let record = if row.role == TranscriptRole::System {
                mez_agent::transcript::TranscriptContextRecord::from_transcript_content(
                    &row.content,
                )?
            } else {
                None
            };
            let origin = match record {
                Some(record) => record.record().occurrence_id(),
                None => match self.row_origins.get(&row.sequence) {
                    Some(origin) => *origin,
                    None => ContextBlockOccurrenceId::for_legacy_archive_row(
                        &row.conversation_id,
                        row.sequence,
                        0,
                    )?,
                },
            };
            if needed.contains(&origin)
                && captured
                    .insert(
                        origin,
                        (row.role == TranscriptRole::System).then(|| row.content.clone()),
                    )
                    .is_some()
            {
                return Err(MezError::conflict(
                    "imported compaction source occurrence is duplicated",
                ));
            }
        }
        if captured.len() != needed.len() {
            return Err(MezError::conflict(
                "imported compaction source occurrence is unavailable",
            ));
        }
        Ok(captured)
    }

    /// Retains original authored images for a first head over checked raw
    /// history. Only current members are selected; display/audit rows do not
    /// enter escrow. New live members keep their actual turn's provenance.
    pub(in crate::runtime) fn raw_replay_rows(
        &self,
        owner: &mez_agent::ReplayEpochContext,
    ) -> Result<Vec<mez_agent::TranscriptEntry>> {
        if self.replay.is_some() || self.epoch.is_some() {
            return Err(MezError::conflict(
                "raw replay capture already has a summary epoch",
            ));
        }
        let needed = owner
            .context()
            .chronology()
            .iter()
            .map(|event| event.record().occurrence_id())
            .collect::<BTreeSet<_>>();
        let mut found = BTreeSet::new();
        let mut result = Vec::new();
        for row in self
            .rows
            .iter()
            .filter(|row| row.role == TranscriptRole::System)
        {
            if let Some(record) =
                mez_agent::transcript::TranscriptContextRecord::from_transcript_content(
                    &row.content,
                )?
                && needed.contains(&record.record().occurrence_id())
            {
                if !found.insert(record.record().occurrence_id()) {
                    return Err(MezError::conflict(
                        "raw replay capture repeats an original occurrence",
                    ));
                }
                result.push(row.clone());
            }
        }
        Ok(result)
    }
}
