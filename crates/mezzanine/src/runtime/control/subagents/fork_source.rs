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
        let snapshot = self
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
