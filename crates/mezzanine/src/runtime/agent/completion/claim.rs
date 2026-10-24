//! Purpose-qualified claim integration for existing completion custody.
//!
//! The managed-shell owner can retain a completed transaction for its existing
//! bounded sandbox-failure assessment before the canonical action is terminal.
//! That internal request is not ordinary execution continuation. This module
//! derives a nonauthorizing disposition from the existing actor-owned record,
//! original transaction/action, current binding and actual dispatch request.
//! It does not create assessments, approve effects, interpret model responses,
//! change sandbox policy, or retire completion backing for an assessment.

use super::*;
use crate::runtime::RuntimeAgentProviderDispatch;

/// A derived lifetime disposition, never a caller-supplied capability or label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CompletionProviderClaimDisposition {
    /// Ordinary execution continuation must transfer terminal group obligations.
    Ordinary,
    /// Existing exact analysis owner keeps the preterminal group and backing.
    RetainedManagedAssessment,
}

impl RuntimeSessionService {
    /// Revalidates the existing exact assessment owner; request presence alone
    /// cannot bypass ordinary completion milestones. Invalid/stale records fail
    /// closed instead of being reclassified as ordinary provider continuation.
    pub(super) fn completion_provider_claim_disposition(
        &self,
        turn_id: &str,
    ) -> Result<CompletionProviderClaimDisposition> {
        let Some(pending) = self.agent.sandbox_failure_assessments.get(turn_id) else {
            return Ok(CompletionProviderClaimDisposition::Ordinary);
        };
        let turn = self
            .agent_turn_ledger()
            .turn(turn_id)
            .ok_or_else(|| MezError::invalid_state("assessment completion turn is unavailable"))?;
        let session = self.agent_shell_store().get(&turn.pane_id).ok_or_else(|| {
            MezError::invalid_state("assessment completion binding is unavailable")
        })?;
        if turn.state != AgentTurnState::Running
            || self.agent_turn_is_human_paused(turn_id)
            || session.session_id != turn.conversation_id
            || session.running_turn_id.as_deref() != Some(turn_id)
            || pending.exit_code == 0
            || pending.transaction.turn_id != turn_id
            || pending.transaction.pane_id != turn.pane_id
            || pending.request.turn_id != turn_id
            || pending.request.agent_id != turn.agent_id
            || !matches!(&pending.transaction.kind,
                crate::runtime::RunningShellTransactionKind::AgentAction { action_id } if action_id == &pending.action_id)
            || pending.marker.is_empty()
            || self.running_shell_transaction(&pending.marker).is_some()
            || self
                .agent
                .pending_native_shell_dispatches
                .keys()
                .any(|(owner, _)| owner == turn_id)
            || self
                .agent
                .claimed_native_shell_dispatches
                .keys()
                .any(|(owner, _)| owner == turn_id)
        {
            return Err(MezError::invalid_state(
                "assessment no longer owns its original completion binding",
            ));
        }
        let execution = self.agent_turn_executions().get(turn_id).ok_or_else(|| {
            MezError::invalid_state("assessment completion execution is unavailable")
        })?;
        let (ordinal, action) = execution
            .response
            .action_batch
            .as_ref()
            .and_then(|batch| {
                batch
                    .actions
                    .iter()
                    .enumerate()
                    .find(|(_, action)| action.id == pending.action_id)
            })
            .ok_or_else(|| MezError::invalid_state("assessment original action is unavailable"))?;
        let result = execution
            .action_results
            .iter()
            .find(|result| result.action_id == action.id)
            .ok_or_else(|| {
                MezError::invalid_state("assessment original result slot is unavailable")
            })?;
        if execution.request.turn_id != turn_id
            || execution.request.agent_id != turn.agent_id
            || result.turn_id != turn_id
            || result.agent_id != turn.agent_id
            || result.action_type != action.action_type()
            || result.status != ActionStatus::Running
            || !result
                .permission_evaluation
                .as_deref()
                .is_some_and(|evaluation| {
                    evaluation.decision == mez_agent::permissions::RuleDecision::Allow
                })
        {
            return Err(MezError::invalid_state(
                "assessment original action or permission evidence changed",
            ));
        }
        if let Some(group) = self.agent.completion_settlements.groups.get(turn_id)
            && (group.paint_revoked
                || group.turn.turn_id != turn_id
                || group.turn.agent_id != turn.agent_id
                || group.turn.pane_id != turn.pane_id
                || group.turn.conversation_id != turn.conversation_id
                || !group.actions.iter().any(|(id, kind, index)| {
                    id == &action.id && kind == action.action_type() && *index == ordinal
                })
                || self
                    .agent
                    .agent_execution_groups_by_turn
                    .get(turn_id)
                    .and_then(|groups| groups.get(&action.id))
                    != Some(&group.group)
                || super::super::provider_execution::provider_log_execution_group_id(execution)?
                    != group.group)
        {
            return Err(MezError::invalid_state(
                "assessment differs from original completion group",
            ));
        }
        // The unchanged managed-shell issuer alone creates this record after
        // validating backend status and the original Allow/nonzero outcome.
        // Its retained Ref, not marker text, owns the already-retired transaction.
        Ok(CompletionProviderClaimDisposition::RetainedManagedAssessment)
    }

    /// Exposes the read-only gate to isolated ordinary negative-control tests;
    /// it cannot insert a request, create a purpose owner or record a claim.
    #[cfg(test)]
    pub(crate) fn validate_completion_claim_readiness_for_tests(
        &self,
        turn_id: &str,
    ) -> Result<()> {
        self.ensure_completion_provider_claim_ready(turn_id)
    }

    /// Revalidates after preparation against the actual request selected for
    /// wire dispatch. No caller label, bool, request presence or alternate router
    /// can manufacture the retained-assessment disposition. The unchanged worker
    /// still applies its current session catalog; this is not a wire-byte or
    /// durable-storage witness.
    pub(in crate::runtime::agent) fn ensure_completion_provider_dispatch_ready(
        &self,
        dispatch: &RuntimeAgentProviderDispatch,
    ) -> Result<()> {
        self.ensure_completion_admission_open()?;
        self.ensure_completion_required_gates_ready(&dispatch.turn.turn_id)?;
        self.ensure_completion_provider_claim_ready(&dispatch.turn.turn_id)?;
        match self.completion_provider_claim_disposition(&dispatch.turn.turn_id)? {
            CompletionProviderClaimDisposition::Ordinary => {
                if dispatch.sandbox_failure_assessment_request.is_some() {
                    return Err(MezError::invalid_state(
                        "assessment dispatch has no exact retained owner",
                    ));
                }
            }
            CompletionProviderClaimDisposition::RetainedManagedAssessment => {
                let pending = self
                    .agent
                    .sandbox_failure_assessments
                    .get(&dispatch.turn.turn_id)
                    .ok_or_else(|| {
                        MezError::invalid_state("assessment dispatch owner disappeared")
                    })?;
                let turn = self
                    .agent_turn_ledger()
                    .turn(&dispatch.turn.turn_id)
                    .ok_or_else(|| {
                        MezError::invalid_state("assessment dispatch turn disappeared")
                    })?;
                if dispatch.turn.state != AgentTurnState::Running
                    || dispatch.turn.agent_id != turn.agent_id
                    || dispatch.turn.pane_id != turn.pane_id
                    || dispatch.turn.conversation_id != turn.conversation_id
                    || dispatch.auto_sizing.is_some()
                    || dispatch.macro_judge_request.is_some()
                    || dispatch.sandbox_failure_assessment_request.as_ref()
                        != Some(&pending.request)
                    || dispatch.model_profile.provider != pending.request.provider
                    || dispatch.model_profile.model != pending.request.model
                {
                    return Err(MezError::invalid_state(
                        "dispatch differs from exact retained assessment request",
                    ));
                }
            }
        }
        Ok(())
    }
}
