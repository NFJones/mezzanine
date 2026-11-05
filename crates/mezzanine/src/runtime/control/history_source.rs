//! Original replay ownership retained beside the actual imported chronology.
//!
//! The history worker captures this value from the same leased read that built
//! model history. Clones share physical source images rather than rediscovering
//! a store, pane binding, pending append, or row after asynchronous work. Images
//! establish the frozen proof only: publication still validates the original
//! archive under its mutation lease. They never enter provider messages.

use super::{RuntimeAgentHistoryEpochWork, RuntimeAgentTranscriptContext};
use crate::{MezError, Result};
use mez_agent::{ContextBlockOccurrenceId, TranscriptRole};
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
    /// Original checked logical window, shared through retries/interruption.
    rows: Arc<[mez_agent::TranscriptEntry]>,
    /// Target policy captured at turn admission, independent of source window.
    target_ephemeral: Option<bool>,
}

impl RuntimeAgentHistorySource {
    /// Clones the original store handle without sampling installation state.
    pub(in crate::runtime) fn store(
        &self,
    ) -> Option<crate::storage::transcript::AgentTranscriptStore> {
        self.work.as_ref().map(|work| work.store.clone())
    }

    /// Identifies the archive that actually supplied these imported events.
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
    pub(super) fn absent() -> Arc<Self> {
        Arc::new(Self {
            work: None,
            epoch: None,
            rows: Arc::from([]),
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
        history.source = Some(Arc::new(Self {
            work: Some(work),
            epoch,
            rows: rows.into(),
            target_ephemeral: None,
        }));
    }

    /// Resolves only actual selected occurrences in the frozen original window.
    /// Legacy ordinary rows are explicitly present without typed ownership.
    /// Missing, duplicated or malformed typed sources fail without consulting a
    /// replacement archive or accepting content equality as occurrence identity.
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
                None => ContextBlockOccurrenceId::for_legacy_archive_row(
                    &row.conversation_id,
                    row.sequence,
                    0,
                )?,
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
}
