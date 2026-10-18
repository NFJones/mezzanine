//! Exact completion custody across provisional provider lease admission.
//!
//! A prepared permit is not worker admission and is never a refusal by itself.
//! Only the actor's actual rejected-lease or post-lease queue boundary may arm
//! negative settlement. The existing group keeps its source and typed failures;
//! no historical group map, payload queue, timer retry or new authority exists.
//! Positive consumption is nonfallible and identity-qualified after all actor
//! admission work. Merely dropping a permit never releases original custody.

use super::retirement::{CompletionMilestone, CompletionTranscriptTarget};
use super::*;
use crate::runtime::RuntimeAgentProviderDispatch;
use std::sync::Arc;

#[cfg(test)]
mod tests;

/// Actual actor boundary that withheld an otherwise prepared dispatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProviderHandoffRejection {
    LeaseRejected,
    AdmittedLeaseQueueRejected,
}

/// Existing-group metadata only; never a copy of the completion payload.
pub(super) struct ProviderHandoffOwner {
    pub(super) identity: Arc<()>,
    pub(super) generation: u64,
    /// The real accepted original projection before a failure invalidates ack.
    pub(super) original_snapshot:
        Option<crate::runtime::persistence::RuntimeCompletionCandidateWitness>,
    pub(super) rejection: Option<ProviderHandoffRejection>,
    pub(super) settlement: CompletionMilestone,
    pub(super) context_consumers_open: bool,
    pub(super) task_disposition: Option<crate::runtime::agent::TerminalResultDisposition>,
}

/// Opaque original-group/attempt permit, distinct from a public turn label.
pub(crate) struct CompletionProviderHandoff {
    turn: AgentTurnRecord,
    generation: u64,
    disposition: CompletionProviderClaimDisposition,
    original: Option<(Arc<()>, Arc<()>, mez_agent::ContextExecutionGroupId)>,
}

impl CompletionProviderHandoff {
    /// Compares only immutable owner identity, not a mutable terminal state.
    fn matches_turn(&self, turn: &AgentTurnRecord) -> bool {
        self.turn.turn_id == turn.turn_id
            && self.turn.agent_id == turn.agent_id
            && self.turn.pane_id == turn.pane_id
            && self.turn.conversation_id == turn.conversation_id
    }
}

impl RuntimeSessionService {
    /// Freezes a nonauthorizing permit before provisional claim mutation.
    /// All fallible source/gate checks precede installation. Assessment keeps
    /// its unchanged dedicated owner and receives no ordinary refusal permit.
    pub(in crate::runtime::agent) fn prepare_completion_provider_handoff(
        &mut self,
        dispatch: &RuntimeAgentProviderDispatch,
        generation: u64,
    ) -> Result<CompletionProviderHandoff> {
        self.ensure_completion_provider_dispatch_ready(dispatch)?;
        let disposition = self.completion_provider_claim_disposition(&dispatch.turn.turn_id)?;
        let mut permit = CompletionProviderHandoff {
            turn: dispatch.turn.clone(),
            generation,
            disposition,
            original: None,
        };
        if disposition == CompletionProviderClaimDisposition::RetainedManagedAssessment {
            return Ok(permit);
        }
        let Some(group) = self
            .agent
            .completion_settlements
            .groups
            .get(&dispatch.turn.turn_id)
        else {
            return Ok(permit);
        };
        if group.provider_handoff.is_some() {
            return Err(MezError::invalid_state(
                "completion already owns a provisional provider handoff",
            ));
        }
        let execution = self
            .agent_turn_executions()
            .get(&dispatch.turn.turn_id)
            .ok_or_else(|| {
                MezError::invalid_state("provider handoff original execution is unavailable")
            })?;
        self.ensure_completion_settlement_source_current(&dispatch.turn, execution)?;
        let original_snapshot = match &group.transcript {
            CompletionTranscriptTarget::Original { .. } => Some(
                group
                    .handoff
                    .as_ref()
                    .and_then(|handoff| handoff.witness.clone())
                    .ok_or_else(|| {
                        MezError::invalid_state(
                            "provider handoff has no accepted original snapshot",
                        )
                    })?,
            ),
            CompletionTranscriptTarget::NoPersistencePolicy(_) => None,
        };
        let identity = Arc::new(());
        permit.original = Some((
            group.identity.clone(),
            identity.clone(),
            group.group.clone(),
        ));
        // No fallible operation follows source qualification and installation.
        if let Some(group) = self
            .agent
            .completion_settlements
            .groups
            .get_mut(&dispatch.turn.turn_id)
        {
            group.provider_handoff = Some(ProviderHandoffOwner {
                identity,
                generation,
                original_snapshot,
                rejection: None,
                settlement: CompletionMilestone::NotEntered,
                context_consumers_open: false,
                task_disposition: None,
            });
        }
        Ok(permit)
    }

    /// Checks an actually installed claim against frozen attribution/generation.
    fn provider_handoff_claim_matches(&self, permit: &CompletionProviderHandoff) -> bool {
        self.agent
            .claimed_agent_provider_tasks
            .get(&permit.turn.turn_id)
            .is_some_and(|claim| {
                claim.generation == permit.generation
                    && claim.turn_id == permit.turn.turn_id
                    && claim.agent_id == permit.turn.agent_id
                    && claim.conversation_id == permit.turn.conversation_id
            })
    }

    /// Checks both strong allocations; reused turn/group labels cannot match.
    fn provider_handoff_group_matches(&self, permit: &CompletionProviderHandoff) -> bool {
        let Some((group_identity, handoff_identity, group_id)) = &permit.original else {
            return false;
        };
        self.agent
            .completion_settlements
            .groups
            .get(&permit.turn.turn_id)
            .is_some_and(|group| {
                Arc::ptr_eq(&group.identity, group_identity)
                    && &group.group == group_id
                    && permit.matches_turn(&group.turn)
                    && group.provider_handoff.as_ref().is_some_and(|owner| {
                        owner.generation == permit.generation
                            && Arc::ptr_eq(&owner.identity, handoff_identity)
                    })
            })
    }

    /// Commits positive custody only after true lease admission and every
    /// remaining fallible actor step. Mismatch retains without error or replay.
    pub(crate) fn accept_completion_provider_handoff(
        &mut self,
        permit: &CompletionProviderHandoff,
    ) -> bool {
        if !self.provider_handoff_claim_matches(permit) {
            return false;
        }
        if permit.disposition == CompletionProviderClaimDisposition::RetainedManagedAssessment {
            return true;
        }
        if permit.original.is_none() {
            return !self
                .agent
                .completion_settlements
                .groups
                .contains_key(&permit.turn.turn_id);
        }
        if !self.provider_handoff_group_matches(permit) {
            return false;
        }
        let ready = self
            .agent
            .completion_settlements
            .groups
            .get(&permit.turn.turn_id)
            .is_some_and(|group| {
                group.primary_failure.is_none()
                    && group
                        .provider_handoff
                        .as_ref()
                        .is_some_and(|owner| owner.rejection.is_none())
            });
        if !ready {
            return false;
        }
        let Some((_, _, group_id)) = &permit.original else {
            return false;
        };
        self.agent
            .completion_settlements
            .external
            .retain(|_, receipt| {
                receipt.owner.turn_id != permit.turn.turn_id || &receipt.owner.group != group_id
            });
        self.agent
            .completion_settlements
            .native
            .retain(|_, receipt| {
                receipt.owner.turn_id != permit.turn.turn_id || &receipt.owner.group != group_id
            });
        self.agent
            .completion_settlements
            .groups
            .remove(&permit.turn.turn_id);
        true
    }

    /// Arms ONLY an actual actor negative event on the exact undispatched
    /// ordinary claim. Generic preparation errors and timer expiry cannot infer
    /// this disposition from a prepared marker or an error's message/kind.
    pub(crate) fn reject_completion_provider_handoff(
        &mut self,
        permit: &CompletionProviderHandoff,
        rejection: ProviderHandoffRejection,
        error: &MezError,
    ) -> bool {
        if permit.disposition != CompletionProviderClaimDisposition::Ordinary
            || !self.provider_handoff_group_matches(permit)
            || !self.provider_handoff_claim_matches(permit)
        {
            return false;
        }
        let turn_id = permit.turn.turn_id.as_str();
        let Some(group) = self.agent.completion_settlements.groups.get(turn_id) else {
            return false;
        };
        let Some(owner) = group.provider_handoff.as_ref() else {
            return false;
        };
        if owner.rejection.is_some()
            || !self
                .agent_turn_ledger()
                .turn(turn_id)
                .is_some_and(|turn| permit.matches_turn(turn))
        {
            return false;
        }
        if let Some(group) = self.agent.completion_settlements.groups.get_mut(turn_id)
            && let Some(owner) = group.provider_handoff.as_mut()
        {
            owner.rejection = Some(rejection);
        }
        self.record_completion_required_failure(
            turn_id,
            "provider_handoff_rejected",
            None,
            None,
            error,
        );
        true
    }

    /// A still-prepared permit is never negative cleanup authority.
    pub(in crate::runtime::agent) fn provider_refusal_is_armed(&self, turn_id: &str) -> bool {
        self.agent
            .completion_settlements
            .groups
            .get(turn_id)
            .and_then(|group| group.provider_handoff.as_ref())
            .is_some_and(|owner| owner.rejection.is_some())
    }

    /// Protects the same original live context through nested forced cleanup;
    /// callers must use this on every detach/removal path, not just direct clear.
    pub(in crate::runtime::agent) fn provider_refusal_context_is_live(
        &self,
        turn_id: &str,
    ) -> bool {
        self.agent
            .completion_settlements
            .groups
            .get(turn_id)
            .and_then(|group| group.provider_handoff.as_ref())
            .is_some_and(|owner| owner.rejection.is_some() && owner.context_consumers_open)
    }
}
