//! Immutable compacted-parent fork source captured on a blocking worker.
//!
//! The actor freezes identity, logical cutoff and pending reservations without
//! reading storage. The worker captures the authoritative epoch and checked rows
//! in one mutation lease, rejects summaries that cross its cutoff, and returns
//! owned data. Freshness checks use the original actor binding independently of
//! archived/display counts. This module does not publish child history or grant
//! execution authority; child persistence owns translation and durable admission.

use super::{MezError, Result, RuntimeSessionService, pane_id_from_runtime_agent_id};
use crate::storage::transcript::ConversationTranscriptRead;

/// Captures the bounded durable parent history copied into one forked child.
///
/// The copied records preserve their original role and authorship while the
/// target conversation receives independent sequence ownership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeSubagentForkSnapshot {
    /// Parent cache lineage retained by the child’s inherited prompt prefix.
    pub(super) prompt_cache_lineage_id: String,
    /// Ordered parent records visible to the child at spawn time.
    pub(crate) entries: Vec<mez_agent::TranscriptEntry>,
    /// Source authority captured with the rows; this is not child publication.
    pub(crate) compaction_epoch: Option<crate::storage::transcript::AgentCompactionEpoch>,
    /// Frozen parent logical frontier, independent of raw-tail cardinality.
    pub(crate) source_high_water: u64,
}

/// Immutable parent-history read input captured before child pane allocation.
/// Only the actor may select the parent conversation, retained count and pending rows.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeSubagentForkReadWork {
    store: crate::storage::transcript::AgentTranscriptStore,
    #[cfg(test)]
    worker_started: Option<std::sync::Arc<tokio::sync::Notify>>,
    pub(super) parent_pane_id: String,
    parent_conversation_id: String,
    parent_compaction_epoch: u64,
    source_conversation_id: String,
    source_entries: u64,
    source_high_water: u64,
    ephemeral_source: bool,
    pending: Vec<mez_agent::TranscriptEntry>,
    prompt_cache_lineage_id: String,
}

impl PartialEq for RuntimeSubagentForkReadWork {
    fn eq(&self, other: &Self) -> bool {
        self.store == other.store
            && self.parent_pane_id == other.parent_pane_id
            && self.parent_conversation_id == other.parent_conversation_id
            && self.parent_compaction_epoch == other.parent_compaction_epoch
            && self.source_conversation_id == other.source_conversation_id
            && self.source_entries == other.source_entries
            && self.source_high_water == other.source_high_water
            && self.ephemeral_source == other.ephemeral_source
            && self.pending == other.pending
            && self.prompt_cache_lineage_id == other.prompt_cache_lineage_id
    }
}

impl Eq for RuntimeSubagentForkReadWork {}

impl RuntimeSubagentForkReadWork {
    /// Signals that the blocking reader has started in actor tests.
    #[cfg(test)]
    pub(crate) fn signal_worker_started(&self) {
        if let Some(started) = &self.worker_started {
            started.notify_one();
        }
    }

    /// Rejects a prepared snapshot after its parent identity, replay position,
    /// compaction generation or configured transcript store changes.
    pub(crate) fn check_owner(&self, service: &RuntimeSessionService) -> Result<()> {
        if service
            .agent_shell_store()
            .get(&self.parent_pane_id)
            .is_none_or(|session| {
                session.session_id != self.parent_conversation_id
                    || session.prompt_cache_lineage_id != self.prompt_cache_lineage_id
                    || session
                        .ephemeral_transcript_source_conversation_id
                        .as_deref()
                        != self
                            .ephemeral_source
                            .then_some(self.source_conversation_id.as_str())
                    || (if self.ephemeral_source {
                        session.ephemeral_transcript_source_entries
                    } else {
                        session.transcript_entries
                    }) != self.source_entries
                    || (!self.ephemeral_source
                        && session.transcript_high_water != self.source_high_water)
            })
            || service.agent_compaction_epoch(&self.parent_pane_id) != self.parent_compaction_epoch
            || service.persistence.cloned_transcript_store().as_ref() != Some(&self.store)
        {
            return Err(MezError::invalid_state(
                "fork parent conversation changed or history advanced before snapshot acceptance",
            ));
        }
        Ok(())
    }

    /// Checks the captured history without accessing live runtime state.
    pub(crate) fn execute(&self) -> Result<RuntimeSubagentForkSnapshot> {
        let pending_count = self
            .pending
            .iter()
            .filter(|row| row.sequence <= self.source_high_water)
            .map(|row| row.sequence)
            .collect::<std::collections::BTreeSet<_>>()
            .len() as u64;
        let mut snapshot = self
            .store
            .conversation_replay_snapshot(&self.source_conversation_id, &self.pending, |epoch| {
                if self.source_high_water > 0 {
                    Some((
                        ConversationTranscriptRead::Through(self.source_high_water),
                        self.source_high_water > pending_count,
                    ))
                } else {
                    // A zero-boundary inherited summary still owns a real empty
                    // archive. With no epoch or rows there is nothing to decode.
                    epoch.map(|_| (ConversationTranscriptRead::All, true))
                }
            })
            .map_err(|error| {
                // An existing epoch reads its archive frontier before the row-view
                // policy runs. Preserve the required-history diagnostic for that
                // earlier failure as well as the ordinary checked-row read.
                if error.kind() == crate::error::MezErrorKind::NotFound {
                    MezError::invalid_state(format!(
                        "required transcript archive is missing: {}",
                        error.message()
                    ))
                } else {
                    error
                }
            })?;
        let mut compaction_epoch = snapshot.epoch;
        if let Some(epoch) = compaction_epoch.as_mut() {
            if epoch.through_sequence > self.source_high_water {
                return Err(MezError::conflict(
                    "fork source compaction prefix exceeds its captured logical cutoff",
                ));
            }
            if epoch.ranges.iter().any(|range| {
                range.first_sequence <= self.source_high_water
                    && self.source_high_water < range.through_sequence
            }) {
                return Err(MezError::conflict(
                    "fork source selective summary crosses its captured logical cutoff",
                ));
            }
            epoch
                .ranges
                .retain(|range| range.through_sequence <= self.source_high_water);
            if epoch.summary.trim().is_empty() && epoch.ranges.is_empty() {
                compaction_epoch = None;
            }
        }
        // A zero-frontier epoch needs an actual empty-archive qualification,
        // but its read may also observe later rows. Bound both evidence domains
        // before interpreting terminal witnesses, just as nonzero reads do.
        snapshot
            .view
            .committed
            .retain(|row| row.sequence <= self.source_high_water);
        snapshot
            .view
            .logical
            .retain(|row| row.sequence <= self.source_high_water);
        for row in &snapshot.view.logical {
            if row.role == mez_agent::TranscriptRole::System
                && row
                    .content
                    .starts_with(crate::storage::transcript::compaction_handoff::MARKER)
                && !snapshot.view.committed.contains(row)
            {
                return Err(MezError::conflict(
                    "fork terminal handoff must settle before source capture",
                ));
            }
        }
        compaction_epoch =
            crate::storage::transcript::compaction_handoff::fork_terminal_projection(
                &self.source_conversation_id,
                &snapshot.view.committed,
                compaction_epoch,
            )?;
        let mut entries = snapshot.view.logical;
        // Limit by the actor-captured owner before selecting the retained tail.
        if self.source_high_water > 0
            && entries
                .last()
                .is_none_or(|entry| entry.sequence < self.source_high_water)
        {
            return Err(MezError::invalid_state(
                "fork source transcript is missing its captured high-water row",
            ));
        }
        entries.retain(|entry| entry.sequence <= self.source_high_water);
        if !self.ephemeral_source {
            let retained_entries = usize::try_from(self.source_entries).unwrap_or(usize::MAX);
            let first_retained = entries.len().saturating_sub(retained_entries);
            entries.drain(..first_retained);
        }
        Ok(RuntimeSubagentForkSnapshot {
            prompt_cache_lineage_id: self.prompt_cache_lineage_id.clone(),
            entries,
            compaction_epoch,
            source_high_water: self.source_high_water,
        })
    }
}

impl RuntimeSessionService {
    /// Captures the exact bounded parent history visible to a forked child.
    ///
    /// The snapshot follows the same retained-tail semantics used for ordinary
    /// durable context assembly. A later parent append cannot enter the child
    /// because records are copied into the child conversation at spawn time.
    pub(super) fn capture_subagent_fork_snapshot(
        &self,
        parent_agent_id: &str,
    ) -> Result<RuntimeSubagentForkSnapshot> {
        match self.prepare_subagent_fork_read_work(parent_agent_id)? {
            Some(work) => {
                let snapshot = work.execute()?;
                work.check_owner(self)?;
                Ok(snapshot)
            }
            None => {
                let parent_pane_id = pane_id_from_runtime_agent_id(parent_agent_id)
                    .ok_or_else(|| MezError::invalid_args("subagent parent agent id is invalid"))?;
                let session = self
                    .agent_shell_store()
                    .get(parent_pane_id.as_str())
                    .ok_or_else(|| {
                        MezError::invalid_state("subagent parent session is unavailable")
                    })?;
                Ok(RuntimeSubagentForkSnapshot {
                    prompt_cache_lineage_id: session.prompt_cache_lineage_id.clone(),
                    entries: Vec::new(),
                    compaction_epoch: None,
                    source_high_water: 0,
                })
            }
        }
    }

    /// Captures parent identity, cutoff and pending rows without storage I/O.
    /// A configured store requires worker qualification even with no raw rows,
    /// because an authoritative nonempty prefix may remain. Only an empty
    /// store-free parent can bypass that source worker.
    pub(crate) fn prepare_subagent_fork_read_work(
        &self,
        parent_agent_id: &str,
    ) -> Result<Option<RuntimeSubagentForkReadWork>> {
        let parent_pane_id = pane_id_from_runtime_agent_id(parent_agent_id)
            .ok_or_else(|| MezError::invalid_args("subagent parent agent id is invalid"))?;
        let parent_session = self
            .agent_shell_store()
            .get(parent_pane_id.as_str())
            .cloned()
            .ok_or_else(|| MezError::invalid_state("subagent parent session is unavailable"))?;
        let source_conversation_id = parent_session
            .ephemeral_transcript_source_conversation_id
            .clone()
            .unwrap_or_else(|| parent_session.session_id.clone());
        if self
            .persistence
            .bookkeeping_pending(&source_conversation_id)
        {
            return Err(MezError::conflict(
                "conversation bookkeeping must settle before fork capture",
            ));
        }
        let source_entries = if parent_session.ephemeral
            && parent_session
                .ephemeral_transcript_source_conversation_id
                .is_some()
        {
            parent_session.ephemeral_transcript_source_entries
        } else {
            parent_session.transcript_entries
        };
        let source_high_water = if parent_session.ephemeral
            && parent_session
                .ephemeral_transcript_source_conversation_id
                .is_some()
        {
            source_entries
        } else {
            parent_session.transcript_high_water
        };
        let Some(store) = self.persistence.cloned_transcript_store() else {
            if source_entries == 0 && source_high_water == 0 {
                return Ok(None);
            }
            return Err(MezError::invalid_state(
                "forked subagent sessions require the parent durable transcript store",
            ));
        };
        let pending = self
            .persistence
            .pending_transcript_entries(&source_conversation_id);
        Ok(Some(RuntimeSubagentForkReadWork {
            store,
            #[cfg(test)]
            worker_started: self.integration.fork_history_started(),
            parent_pane_id: parent_pane_id.to_string(),
            parent_conversation_id: parent_session.session_id.clone(),
            parent_compaction_epoch: self.agent_compaction_epoch(parent_pane_id.as_str()),
            source_conversation_id,
            source_entries,
            source_high_water,
            ephemeral_source: parent_session.ephemeral
                && parent_session
                    .ephemeral_transcript_source_conversation_id
                    .is_some(),
            pending,
            prompt_cache_lineage_id: parent_session.prompt_cache_lineage_id,
        }))
    }
}

impl RuntimeSubagentForkSnapshot {
    /// Rebinds a checked source into independently sequenced child ownership.
    /// Prefix-covered raw rows stay excluded; complete selective anchors shift
    /// with the physical tail. Terminal witnesses become inert provenance only
    /// after source capture has projected their committed accepted authority.
    pub(super) fn child_batch(
        &self,
        child_cid: &str,
        created_at: u64,
    ) -> Result<Option<crate::storage::transcript::TranscriptAppendBatch>> {
        let prefix = self
            .compaction_epoch
            .as_ref()
            .map_or(0, |epoch| epoch.through_sequence);
        if let Some(epoch) = &self.compaction_epoch {
            if self
                .entries
                .iter()
                .any(|row| row.conversation_id != epoch.conversation_id)
            {
                return Err(MezError::conflict(
                    "fork source rows changed conversation identity",
                ));
            }
            let expected = self.source_high_water.checked_sub(prefix).ok_or_else(|| {
                MezError::conflict("fork source prefix exceeds captured frontier")
            })?;
            let actual = self
                .entries
                .iter()
                .filter(|row| row.sequence > prefix && row.sequence <= self.source_high_water)
                .count() as u64;
            if actual != expected {
                return Err(MezError::conflict(
                    "fork source is missing retained physical rows",
                ));
            }
        }
        let mut rows = Vec::new();
        for source in self
            .entries
            .iter()
            .filter(|row| row.sequence > prefix && row.sequence <= self.source_high_water)
        {
            let mut child = source.clone();
            child.conversation_id = child_cid.into();
            child.sequence = (rows.len() as u64)
                .checked_add(1)
                .ok_or_else(|| MezError::invalid_state("fork child sequence is exhausted"))?;
            child.created_at_unix_seconds = created_at;
            if child.role == mez_agent::TranscriptRole::System
                && let Some(content) =
                    crate::storage::transcript::compaction_handoff::inherited_terminal_audit(
                        &child.content,
                    )?
            {
                child.content = content;
            }
            rows.push(child);
        }
        if let Some(mut epoch) = self.compaction_epoch.clone() {
            epoch.conversation_id = child_cid.into();
            epoch.through_sequence = 0;
            for range in &mut epoch.ranges {
                range.first_sequence =
                    range.first_sequence.checked_sub(prefix).ok_or_else(|| {
                        MezError::conflict("fork selective start precedes raw source")
                    })?;
                range.through_sequence = range
                    .through_sequence
                    .checked_sub(prefix)
                    .ok_or_else(|| MezError::conflict("fork selective end precedes raw source"))?;
            }
            crate::storage::transcript::TranscriptAppendBatch::initial_projection(epoch, rows)
                .map(Some)
        } else if rows.is_empty() {
            Ok(None)
        } else {
            crate::storage::transcript::TranscriptAppendBatch::rows(rows).map(Some)
        }
    }
}

/// Renders initial model history from the same captured payload that admission
/// will persist. This pure projection performs no archive I/O on the actor.
pub(super) fn child_history(
    pane_id: &str,
    batch: &crate::storage::transcript::TranscriptAppendBatch,
) -> crate::runtime::control::RuntimeAgentTranscriptContext {
    super::super::context::runtime_agent_history_epoch_from_snapshot(
        super::super::context::RuntimeAgentHistoryEpochInputs {
            pane_id: pane_id.into(),
            conversation_id: batch.conversation_id().into(),
            ephemeral_source_entries: None,
            active_entries: Some(batch.entries().len()),
            pending_entries: Vec::new(),
        },
        batch.initial_compaction_epoch().cloned(),
        batch.entries().to_vec(),
    )
}
