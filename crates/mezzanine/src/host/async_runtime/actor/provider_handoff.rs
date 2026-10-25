//! Source-owned provider-lane dispatch transfer under transient pressure.
//!
//! The service retains provider, external, native, compaction and remember work;
//! this owner retains only refused/offered identities, never payloads or rights.
//! Eligibility is separate from ownership: temporarily blocked external work
//! must not lose recovery metadata while waiting for a persistence barrier or
//! another action. Normal poll/retry scheduling and strict producer admission
//! remain unchanged. Offers last until actual preparation/claim settlement.
//! Native identity includes the existing immutable transaction marker: an old
//! phase refusal cannot accelerate a later phase of the same logical action.

use super::{AsyncRuntimeSessionActor, Result, RuntimeSideEffect};
use crate::runtime::RuntimeSessionService;

/// Existing service source identity, not a claim or permission to execute.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SourceDispatch {
    /// One pending provider turn; its claim captures the actual generation.
    Provider(String),
    /// One approved action; its claim allocates the actual external attempt.
    External(String, String),
    /// One exact native phase, fenced by its existing transaction marker.
    Native(String, String, String),
    /// One queued compactor, fenced by its existing task generation.
    Compaction(String, u64),
    /// One memory request, fenced by its existing observation identity.
    Remember(String, String),
}

impl SourceDispatch {
    /// Extracts only supported source-owned descriptor families.
    fn from_effect(effect: &RuntimeSideEffect, service: &RuntimeSessionService) -> Option<Self> {
        match effect {
            RuntimeSideEffect::DispatchAgentProvider { turn_id, .. } => {
                Some(Self::Provider(turn_id.clone()))
            }
            RuntimeSideEffect::DispatchApprovedExternalAction { turn_id, action_id } => {
                Some(Self::External(turn_id.clone(), action_id.clone()))
            }
            RuntimeSideEffect::DispatchNativeShellAction { turn_id, action_id } => service
                .pending_native_shell_action_marker(turn_id, action_id)
                .map(|marker| Self::Native(turn_id.clone(), action_id.clone(), marker.to_string())),
            RuntimeSideEffect::DispatchAgentCompaction {
                pane_id,
                task_generation,
            } => Some(Self::Compaction(pane_id.clone(), *task_generation)),
            RuntimeSideEffect::DispatchAgentRemember {
                pane_id,
                observation_id,
            } => Some(Self::Remember(pane_id.clone(), observation_id.clone())),
            _ => None,
        }
    }

    /// Tests unclaimed source custody independently of temporary readiness.
    fn is_owned(&self, service: &RuntimeSessionService) -> bool {
        match self {
            Self::Provider(turn_id) => service.agent_provider_task_is_pending(turn_id),
            Self::External(turn_id, action_id) => {
                service.approved_external_action_is_unclaimed(turn_id, action_id)
            }
            Self::Native(turn_id, action_id, marker) => {
                service.pending_native_shell_action_marker(turn_id, action_id)
                    == Some(marker.as_str())
            }
            Self::Compaction(pane, generation) => {
                service.pending_agent_compaction_task_generation(pane) == Some(*generation)
                    && service.agent_compaction_task_is_current(pane, *generation)
            }
            Self::Remember(pane, observation) => {
                service.pending_agent_remember_observation_id(pane) == Some(observation.as_str())
            }
        }
    }
}

impl AsyncRuntimeSessionActor {
    /// Retains identities only after committed admission refuses descriptors.
    /// Ordinary pending tasks keep their established poll/retry scheduling.
    pub(super) fn record_refused_provider_sources(&mut self, effects: &[RuntimeSideEffect]) {
        self.refused_dispatch_sources
            .retain(|source| source.is_owned(&self.service));
        for source in effects
            .iter()
            .filter_map(|effect| SourceDispatch::from_effect(effect, &self.service))
        {
            if source.is_owned(&self.service) {
                self.refused_dispatch_sources.insert(source);
            }
        }
    }

    /// Releases a provider offer at its real preparation/claim boundary.
    pub(super) fn settle_provider_offer(&mut self, turn_id: &str) {
        self.settle_source_offer(SourceDispatch::Provider(turn_id.to_string()));
    }

    /// Releases offered native phases after the service's actual claim attempt.
    /// A successful claim excludes its marker; a no-work claim may retain the
    /// same unclaimed phase. Retired/replaced markers cannot follow a new phase.
    pub(super) fn settle_native_offer(&mut self, turn_id: &str, action_id: &str) {
        let sources: Vec<_> = self.offered_dispatch_sources.iter()
            .filter(|source| matches!(source, SourceDispatch::Native(owner, action, _) if owner == turn_id && action == action_id))
            .cloned().collect();
        for source in sources {
            self.settle_source_offer(source);
        }
    }

    /// Returns still-owned work to its source; successful claims remain excluded
    /// by the existing service readiness checks, not by guessed worker outcomes.
    pub(super) fn settle_source_offer(&mut self, source: SourceDispatch) {
        if self.offered_dispatch_sources.remove(&source) && source.is_owned(&self.service) {
            self.refused_dispatch_sources.insert(source);
        }
    }

    /// Fences recovered descriptors before drain-triggered refill can see their
    /// still-pending service sources. Arbitrary adapter records cannot grow it.
    pub(super) fn record_provider_offers(&mut self, effects: &[RuntimeSideEffect]) {
        for source in effects
            .iter()
            .filter_map(|effect| SourceDispatch::from_effect(effect, &self.service))
        {
            if self.refused_dispatch_sources.remove(&source) && source.is_owned(&self.service) {
                self.offered_dispatch_sources.insert(source);
            }
        }
    }

    /// Selects a fitting prefix using ordinary service eligibility/deduplication.
    /// Only genuinely refused sources participate; no work payload is removed
    /// from the service and no action/provider/event is replayed.
    pub(super) fn queue_fitting_provider_sources(&mut self) -> Result<usize> {
        self.offered_dispatch_sources
            .retain(|source| source.is_owned(&self.service));
        self.refused_dispatch_sources
            .retain(|source| source.is_owned(&self.service));
        if self.refused_dispatch_sources.is_empty()
            || self.available_committed_side_effect_capacity() == 0
        {
            return Ok(0);
        }
        let effects: Vec<_> = self
            .pending_provider_dispatch_side_effects()?
            .into_iter()
            .filter(|effect| {
                SourceDispatch::from_effect(effect, &self.service)
                    .is_some_and(|source| self.refused_dispatch_sources.contains(&source))
            })
            .take(self.available_committed_side_effect_capacity())
            .collect();
        let count = effects.len();
        if count > 0 {
            self.admit_committed_side_effects(effects)?;
        }
        Ok(count)
    }
}
