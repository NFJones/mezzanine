//! Immutable manual request rendering from the canonical source owner.
//!
//! Actor admission selects one frozen prefix and captures policy, model and
//! expense origin. This worker only renders that selection; it cannot fall back
//! to row counts, infer execution closure or produce an old-format task.

use super::*;
use std::sync::Arc;

/// Original canonical source and request inputs for one manual operation.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeManualCompactionRequestWork {
    /// Original generation and finite source-preparation permit.
    pub(crate) owner: RuntimeManualCompactionPreparation,
    profile_name: String,
    profile: ModelProfile,
    source: Arc<RuntimeCompactionReplaySource>,
    allowed_actions: AllowedActionSet,
    origin: crate::storage::token_usage::AccountingOrigin,
    /// Independent request-rendering barrier, absent from production work.
    #[cfg(test)]
    pub(crate) probe: Option<(
        Arc<tokio::sync::Notify>,
        Arc<tokio::sync::Notify>,
        Arc<tokio::sync::Notify>,
    )>,
}

impl RuntimeManualCompactionRequestWork {
    /// Captures already selected source and current model/action policy. The
    /// required source retains its own history/store; no alternate producer or
    /// physical-row selection can replace it during request construction.
    pub(in crate::runtime::commands::compaction) fn capture(
        owner: RuntimeManualCompactionPreparation,
        profile_name: String,
        profile: ModelProfile,
        source: Arc<RuntimeCompactionReplaySource>,
        allowed_actions: AllowedActionSet,
        origin: crate::storage::token_usage::AccountingOrigin,
    ) -> Self {
        Self {
            owner,
            profile_name,
            profile,
            source,
            allowed_actions,
            origin,
            #[cfg(test)]
            probe: None,
        }
    }

    /// Adds a fixture gate without changing immutable inputs or ownership.
    #[cfg(test)]
    pub(crate) fn with_probe(
        mut self,
        probe: Option<(
            Arc<tokio::sync::Notify>,
            Arc<tokio::sync::Notify>,
            Arc<tokio::sync::Notify>,
        )>,
    ) -> Self {
        self.probe = probe;
        self
    }

    /// Accepts only this worker's original source/model. Matching scalar owner
    /// fields cannot let another operation's rendered result enter this slot.
    pub(super) fn validate_task(&self, task: &RuntimeAgentCompactionTask) -> Result<()> {
        if !task
            .replay_source
            .as_ref()
            .is_some_and(|source| Arc::ptr_eq(source, &self.source))
            || !task
                .source_owner
                .as_ref()
                .is_some_and(|owner| Arc::ptr_eq(owner, &self.source.original))
            || task.model_profile != self.profile
            || task.model_profile_name != self.profile_name
            || task.source != "manual"
            || task.resume_turn_id.is_some()
            || !matches!(&task.target, RuntimeAgentCompactionTarget::Conversation)
        {
            return Err(MezError::conflict(
                "manual rendered request source/model changed",
            ));
        }
        Ok(())
    }

    /// Renders the same canonical manual task as inline admission. No actor
    /// lookup, I/O, provider call, publication or ordinary append ACK occurs.
    pub(crate) fn execute_request(&self) -> Result<RuntimeAgentCompactionTask> {
        replay_epoch::manual_replay_task(
            &self.owner.pane_id,
            &self.profile_name,
            &self.profile,
            self.source.clone(),
            self.owner.compaction_epoch,
            self.owner.transcript_entries,
            self.origin.clone(),
            self.allowed_actions.clone(),
        )
    }
}
