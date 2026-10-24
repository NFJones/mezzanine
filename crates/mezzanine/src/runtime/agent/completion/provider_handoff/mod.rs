//! Exact completion custody across provisional provider lease admission.
//!
//! A prepared permit is not worker admission and is never a refusal by itself.
//! Only the actor's actual rejected-lease or post-lease queue boundary may arm
//! negative settlement. The existing group keeps its source and typed failures;
//! no historical group map, payload queue, timer retry or new authority exists.
//! Positive consumption is nonfallible and identity-qualified after all actor
//! admission work. Merely dropping a permit never releases original custody.

use super::retirement::{
    CompletionCapture, CompletionFailedBacking, CompletionMilestone, CompletionTranscriptTarget,
};
use super::*;
use crate::runtime::RuntimeAgentProviderDispatch;
use sha2::{Digest, Sha256};
use std::sync::Arc;

mod loop_restore;
use loop_restore::OwnLoopRestore;

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
    pub(super) routed: CompletionMilestone,
    pub(super) routed_result: Option<bool>,
    pub(super) source_frozen: bool,
    own_loop_restore: Option<Arc<OwnLoopRestore>>,
    #[cfg(test)]
    consumer_probe: Option<(bool, bool, u64)>,
}

/// Read-only phase facts for isolated real-actor refusal controls.
#[cfg(test)]
pub(crate) struct ProviderRefusalTestFacts {
    pub(crate) live_before: bool,
    pub(crate) live_after: bool,
    pub(crate) consumer_entries: u64,
    pub(crate) secondary: Vec<MezError>,
    pub(crate) task_entered: bool,
    pub(crate) task_accepted: bool,
    pub(crate) scheduler_accepted: bool,
    pub(crate) settlement_terminal: bool,
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
    /// The common correction facade calls this after actual queue insertion,
    /// before any legacy caller can discard its live execution slot. Move the
    /// actual original source into the same admitted group, without a clone.
    pub(in crate::runtime::agent) fn retain_queued_completion_correction_source(
        &mut self,
        turn: &AgentTurnRecord,
    ) -> Result<()> {
        let Some(group) = self.agent.completion_settlements.groups.get(&turn.turn_id) else {
            return Ok(());
        };
        if group.correction_execution.as_ref().is_some() {
            return Ok(());
        }
        if matches!(
            group.correction_execution,
            CompletionCapture::Captured(None)
        ) {
            return Err(MezError::invalid_state(
                "correction source was already transferred; recapture prohibited",
            ));
        }
        let execution = self
            .agent_turn_executions()
            .get(&turn.turn_id)
            .ok_or_else(|| {
                MezError::invalid_state("queued correction original execution is unavailable")
            })?;
        self.ensure_completion_settlement_source_current(turn, execution)?;
        let execution = self.agent_turn_executions_mut().remove(&turn.turn_id);
        if let Some(group) = self
            .agent
            .completion_settlements
            .groups
            .get_mut(&turn.turn_id)
        {
            group.correction_execution.capture_once(|| execution);
        }
        Ok(())
    }

    /// Captures exact phase/secondary evidence without treating observations as
    /// production custody or manufacturing a successful consumer disposition.
    #[cfg(test)]
    pub(crate) fn provider_refusal_test_facts(
        &self,
        turn_id: &str,
        expected_group: &str,
    ) -> Option<ProviderRefusalTestFacts> {
        let group = self.agent.completion_settlements.groups.get(turn_id)?;
        if group.group.as_str() != expected_group {
            return None;
        }
        let owner = group.provider_handoff.as_ref()?;
        let (live_before, live_after, consumer_entries) =
            owner.consumer_probe.unwrap_or((false, false, 0));
        Some(ProviderRefusalTestFacts {
            live_before,
            live_after,
            consumer_entries,
            secondary: group
                .secondary_failures
                .iter()
                .map(|failure| (*failure.error).clone())
                .collect(),
            task_entered: group.task_result == CompletionMilestone::Entered,
            task_accepted: group.task_result == CompletionMilestone::Accepted,
            scheduler_accepted: group.scheduler == CompletionMilestone::Accepted,
            settlement_terminal: owner.settlement == CompletionMilestone::Accepted,
        })
    }

    /// Known negative failure inhibits NEW work; it must not abort mandatory
    /// checkpoint/cleanup suffixes after actual Failed ledger bookkeeping. No
    /// admission gate is cleared or bypassed to start any task: the count is zero.
    pub(in crate::runtime::agent) fn start_ready_after_completion_cleanup(
        &mut self,
        turn: &AgentTurnRecord,
    ) -> Result<usize> {
        if self.provider_refusal_is_armed(&turn.turn_id) {
            self.ensure_provider_refusal_failed_source(turn, AgentTurnState::Failed)?;
            if !self
                .agent_turn_ledger()
                .turn(&turn.turn_id)
                .is_some_and(|turn| turn.state == AgentTurnState::Failed)
            {
                return Err(MezError::invalid_state(
                    "negative cleanup has not committed Failed ledger state",
                ));
            }
            return Ok(0);
        }
        // A pending fork/new loop iteration may bind its next conversation only
        // after the exact old running-turn slot has been released. A failed
        // continuation can queue a terminal pane close after the earlier close
        // checkpoint; settle that close in this same ordinary cleanup pass.
        // The negative-refusal branch above must never start this work.
        self.start_deferred_agent_loop_continuation(&turn.turn_id)?;
        self.close_terminal_subagent_pane_if_pending(turn)?;
        self.start_ready_agent_turns()
    }

    /// Selects one inert fault in an isolated actor fixture, not a live policy.
    #[cfg(test)]
    pub(crate) fn set_provider_handoff_fault_for_tests(&mut self, fault: &'static str) {
        self.agent
            .completion_settlements
            .provider_handoff_fault_for_tests = Some(fault);
    }

    /// Consumes only the named boundary; an unrelated path cannot heal it.
    #[cfg(test)]
    pub(crate) fn take_provider_handoff_fault_for_tests(&mut self, fault: &'static str) -> bool {
        if self
            .agent
            .completion_settlements
            .provider_handoff_fault_for_tests
            == Some(fault)
        {
            self.agent
                .completion_settlements
                .provider_handoff_fault_for_tests = None;
            true
        } else {
            false
        }
    }

    /// Validates the distinct negative disposition. This is failure bookkeeping
    /// with retained original custody, never success, archive acceptance, or
    /// permission for another provider/action/assessment continuation.
    fn ensure_provider_refusal_failed_source(
        &self,
        turn: &AgentTurnRecord,
        state: AgentTurnState,
    ) -> Result<()> {
        let group = self
            .agent
            .completion_settlements
            .groups
            .get(&turn.turn_id)
            .ok_or_else(|| MezError::invalid_state("negative completion group is missing"))?;
        let owner = group
            .provider_handoff
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("negative completion owner is missing"))?;
        let backing = group.failed_backing.as_ref().ok_or_else(|| {
            MezError::invalid_state("negative completion has no retained original backing")
        })?;
        let execution = backing.execution.as_ref().ok_or_else(|| {
            MezError::invalid_state("negative completion has no captured original execution")
        })?;
        if state != AgentTurnState::Failed
            || owner.rejection.is_none()
            || owner.settlement != CompletionMilestone::Entered
            || !owner.source_frozen
            || group.primary_failure.is_none()
            || group.turn.agent_id != turn.agent_id
            || group.turn.pane_id != turn.pane_id
            || group.turn.conversation_id != turn.conversation_id
            || !self
                .agent_turn_ledger()
                .turn(&turn.turn_id)
                .is_some_and(|current| {
                    current.agent_id == group.turn.agent_id
                        && current.pane_id == group.turn.pane_id
                        && current.conversation_id == group.turn.conversation_id
                })
            || self
                .agent_shell_store()
                .get(&group.turn.pane_id)
                .is_some_and(|_| !self.provider_refusal_shell_binding_owned(&turn.turn_id))
            || crate::runtime::agent::provider_execution::provider_log_execution_group_id(
                execution,
            )? != group.group
            || (!self.provider_refusal_context_is_live(&turn.turn_id)
                && backing.context.as_ref().is_none())
            || (self.provider_refusal_context_is_live(&turn.turn_id)
                && !self.agent_turn_contexts().contains_key(&turn.turn_id))
        {
            return Err(MezError::invalid_state(
                "negative completion does not own its original Failed cleanup disposition",
            ));
        }
        if matches!(
            group.transcript,
            CompletionTranscriptTarget::Original { .. }
        ) && owner.original_snapshot.is_none()
        {
            return Err(MezError::invalid_state(
                "negative completion lost original archive qualification",
            ));
        }
        Ok(())
    }

    /// Revalidates the exact negative target before the existing nonfallible
    /// cancellation prefix. No capture, signal or replacement lookup is performed.
    pub(in crate::runtime::agent) fn validate_provider_refusal_cleanup_before_cancellation(
        &self,
        turn_id: &str,
    ) -> Result<()> {
        let turn = self
            .completion_original_turn(turn_id)
            .ok_or_else(|| MezError::invalid_state("negative cleanup original turn is missing"))?;
        let state = self
            .agent_turn_ledger()
            .turn(turn_id)
            .map(|turn| turn.state)
            .ok_or_else(|| MezError::invalid_state("negative cleanup ledger is missing"))?;
        self.ensure_provider_refusal_failed_source(&turn, state)
    }

    /// Ordinary terminal retirement keeps its existing gates. Only the exact
    /// negative Failed disposition may retain backing instead of discharging
    /// those positive gates. It cannot authorize Completed or Interrupted work.
    pub(in crate::runtime::agent) fn prepare_completion_terminal_cleanup(
        &mut self,
        turn: &AgentTurnRecord,
        state: AgentTurnState,
    ) -> Result<()> {
        if !self.provider_refusal_is_armed(&turn.turn_id) {
            return self.ensure_completion_terminal_retirement(&turn.turn_id);
        }
        self.ensure_provider_refusal_failed_source(turn, state)?;
        self.detach_failed_completion_backing(&turn.turn_id);
        self.ensure_provider_refusal_failed_source(turn, state)
    }

    /// Completes only the same negative task/scheduler suffix. The original
    /// required failure remains primary; no positive completion gate is cleared
    /// or marked accepted. Ambiguous effect entry remains bounded, not replayed.
    pub(in crate::runtime::agent) fn finalize_completion_provider_refusal(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
        reason: &str,
    ) -> Result<()> {
        self.ensure_provider_refusal_failed_source(turn, execution.terminal_state)?;
        self.emit_provider_error_terminal_result(turn, execution)?;
        let scheduler = self
            .agent
            .completion_settlements
            .groups
            .get(&turn.turn_id)
            .ok_or_else(|| MezError::invalid_state("negative scheduler owner is missing"))?
            .scheduler;
        match scheduler {
            CompletionMilestone::Entered => {
                return Err(MezError::invalid_state(
                    "negative scheduler result is ambiguous; replay prohibited",
                ));
            }
            CompletionMilestone::Accepted => return Ok(()),
            CompletionMilestone::NotEntered => {}
        }
        if let Some(group) = self
            .agent
            .completion_settlements
            .groups
            .get_mut(&turn.turn_id)
        {
            group.scheduler = CompletionMilestone::Entered;
        }
        self.complete_running_agent_turn_and_start_ready(turn, AgentTurnState::Failed, reason)?;
        if let Some(group) = self
            .agent
            .completion_settlements
            .groups
            .get_mut(&turn.turn_id)
        {
            group.scheduler = CompletionMilestone::Accepted;
        }
        self.start_ready_after_completion_cleanup(turn)?;
        Ok(())
    }

    /// Enters the exact actual-refusal scope once. Generic provider failures,
    /// stale permits and a merely prepared marker cannot enter this path.
    pub(in crate::runtime::agent) fn begin_completion_provider_refusal(
        &mut self,
        turn_id: &str,
    ) -> Result<Option<CompletionProviderHandoff>> {
        let Some(group) = self.agent.completion_settlements.groups.get(turn_id) else {
            return Ok(None);
        };
        let Some(owner) = group
            .provider_handoff
            .as_ref()
            .filter(|owner| owner.rejection.is_some())
        else {
            return Ok(None);
        };
        let permit = CompletionProviderHandoff {
            turn: group.turn.clone(),
            generation: owner.generation,
            disposition: CompletionProviderClaimDisposition::Ordinary,
            original: Some((
                group.identity.clone(),
                owner.identity.clone(),
                group.group.clone(),
            )),
        };
        if owner.settlement != CompletionMilestone::NotEntered
            || !self.provider_handoff_claim_matches(&permit)
        {
            return Err(self.completion_group_failure(turn_id).unwrap_or_else(|| {
                MezError::invalid_state(
                    "provider refusal is already entered or no longer owns its claim",
                )
            }));
        }
        if let Some(owner) = self
            .agent
            .completion_settlements
            .groups
            .get_mut(turn_id)
            .and_then(|group| group.provider_handoff.as_mut())
        {
            owner.settlement = CompletionMilestone::Entered;
            owner.context_consumers_open = true;
        }
        Ok(Some(permit))
    }

    /// Qualifies the accepted original projection before overwrite or failure
    /// evidence restaging. A witness is producer ownership, not disk durability.
    pub(in crate::runtime::agent) fn freeze_completion_provider_refusal_source(
        &mut self,
        permit: &CompletionProviderHandoff,
    ) -> Result<()> {
        if !self.provider_handoff_group_matches(permit) {
            return Err(MezError::invalid_state(
                "provider refusal original group changed",
            ));
        }
        let group = self
            .agent
            .completion_settlements
            .groups
            .get(&permit.turn.turn_id)
            .ok_or_else(|| MezError::invalid_state("provider refusal source is unavailable"))?;
        let owner = group
            .provider_handoff
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("provider refusal witness is unavailable"))?;
        if let CompletionTranscriptTarget::Original { .. } = &group.transcript {
            let witness = owner.original_snapshot.as_ref().ok_or_else(|| {
                MezError::invalid_state("provider refusal original archive witness is missing")
            })?;
            let candidate = &group
                .handoff
                .as_ref()
                .ok_or_else(|| {
                    MezError::invalid_state("provider refusal original candidate is missing")
                })?
                .candidate;
            let digest: String = Sha256::digest(candidate.completion_snapshot_bytes()?)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            if digest != witness.snapshot_id() {
                return Err(MezError::invalid_state(
                    "provider refusal original snapshot changed before capture",
                ));
            }
        }
        if let Some(owner) = self
            .agent
            .completion_settlements
            .groups
            .get_mut(&permit.turn.turn_id)
            .and_then(|group| group.provider_handoff.as_mut())
        {
            owner.source_frozen = true;
        }
        Ok(())
    }

    /// Captures actual original execution immediately before provider-error
    /// insertion. Context stays in its original live scope for loop/routed
    /// consumers; no removal/rebind or clone is used to satisfy those consumers.
    pub(in crate::runtime::agent) fn capture_completion_before_provider_error(
        &mut self,
        turn: &AgentTurnRecord,
    ) -> Result<()> {
        if !self.provider_refusal_is_armed(&turn.turn_id) {
            return Ok(());
        }
        let group = self
            .agent
            .completion_settlements
            .groups
            .get(&turn.turn_id)
            .ok_or_else(|| {
                MezError::invalid_state("provider refusal capture group is unavailable")
            })?;
        let owner = group.provider_handoff.as_ref().ok_or_else(|| {
            MezError::invalid_state("provider refusal capture owner is unavailable")
        })?;
        if owner.settlement != CompletionMilestone::Entered
            || !owner.context_consumers_open
            || !owner.source_frozen
            || group.turn.agent_id != turn.agent_id
            || group.turn.pane_id != turn.pane_id
            || group.turn.conversation_id != turn.conversation_id
        {
            return Err(MezError::invalid_state(
                "provider refusal capture is not in its original consumer scope",
            ));
        }
        if group
            .failed_backing
            .as_ref()
            .is_some_and(|backing| !matches!(backing.execution, CompletionCapture::NotCaptured))
        {
            return Err(MezError::invalid_state(
                "provider refusal execution capture already occurred; overwrite replay prohibited",
            ));
        }
        let original = self
            .agent_turn_executions()
            .get(&turn.turn_id)
            .or_else(|| group.correction_execution.as_ref())
            .ok_or_else(|| {
                MezError::invalid_state("provider refusal lost original execution before overwrite")
            })?;
        if crate::runtime::agent::provider_execution::provider_log_execution_group_id(original)?
            != group.group
            || !self.agent_turn_contexts().contains_key(&turn.turn_id)
        {
            return Err(MezError::invalid_state(
                "provider refusal cannot capture replacement or missing context",
            ));
        }
        let from_correction = group.correction_execution.as_ref().is_some();
        let execution = if from_correction {
            self.agent
                .completion_settlements
                .groups
                .get_mut(&turn.turn_id)
                .and_then(|group| {
                    match std::mem::replace(
                        &mut group.correction_execution,
                        CompletionCapture::Captured(None),
                    ) {
                        CompletionCapture::Captured(execution) => execution,
                        CompletionCapture::NotCaptured => None,
                    }
                })
        } else {
            self.agent_turn_executions_mut().remove(&turn.turn_id)
        };
        if let Some(group) = self
            .agent
            .completion_settlements
            .groups
            .get_mut(&turn.turn_id)
        {
            group
                .failed_backing
                .get_or_insert_with(|| Box::new(CompletionFailedBacking::default()))
                .execution
                .capture_once(|| execution);
        }
        // Capture remaining non-context components before any nested cleanup.
        // The original context-open flag is honored inside every detach entry.
        self.detach_failed_completion_backing(&turn.turn_id);
        Ok(())
    }

    /// Closes the same strong scope on every return and completes each remaining
    /// component once. Actual settlement errors stay secondary to the refusal;
    /// a duplicate entered scope never replays audit, task, routed or loop work.
    pub(in crate::runtime::agent) fn finish_completion_provider_refusal(
        &mut self,
        permit: CompletionProviderHandoff,
        result: Result<()>,
    ) -> Result<()> {
        if !self.provider_handoff_group_matches(&permit) {
            return result.and_then(|()| {
                Err(MezError::invalid_state(
                    "provider refusal original scope changed before close",
                ))
            });
        }
        let turn_id = permit.turn.turn_id.as_str();
        if !self
            .agent_turn_ledger()
            .turn(turn_id)
            .is_some_and(|turn| permit.matches_turn(turn))
            || self
                .agent_shell_store()
                .get(&permit.turn.pane_id)
                .is_some_and(|_| !self.provider_refusal_shell_binding_owned(turn_id))
        {
            let error =
                MezError::invalid_state("provider refusal binding changed before component close");
            self.record_completion_required_failure(
                turn_id,
                "provider_refusal_binding",
                None,
                None,
                &error,
            );
            return Err(self.completion_group_failure(turn_id).unwrap_or(error));
        }
        if let Err(error) = &result {
            self.record_completion_required_failure(
                turn_id,
                "provider_refusal_settlement",
                None,
                None,
                error,
            );
        }
        let terminal = self
            .agent_turn_ledger()
            .turn(turn_id)
            .is_some_and(|turn| turn.state == AgentTurnState::Failed)
            && self
                .agent
                .completion_settlements
                .groups
                .get(turn_id)
                .is_some_and(|group| {
                    group.task_result == CompletionMilestone::Accepted
                        && group.scheduler == CompletionMilestone::Accepted
                });
        if let Some(owner) = self
            .agent
            .completion_settlements
            .groups
            .get_mut(turn_id)
            .and_then(|group| group.provider_handoff.as_mut())
        {
            owner.context_consumers_open = false;
            if result.is_ok() && terminal {
                owner.settlement = CompletionMilestone::Accepted;
            }
        }
        // These are only this undispatched claim and original group; never a
        // current-pane scan or replacement claim cancellation.
        if self.provider_handoff_claim_matches(&permit) {
            self.agent.claimed_agent_provider_tasks.remove(turn_id);
        }
        self.agent.pending_agent_provider_tasks.remove(turn_id);
        self.detach_failed_completion_backing(turn_id);
        match result {
            Ok(()) if terminal => Ok(()),
            Ok(()) => Err(self.completion_group_failure(turn_id).unwrap_or_else(|| {
                MezError::invalid_state(
                    "negative provider settlement remains retained, not terminal",
                )
            })),
            Err(error) => Err(self.completion_group_failure(turn_id).unwrap_or(error)),
        }
    }

    /// Enters the FIRST existing task-result emission before any loop/routed
    /// effects and caches its real disposition. Entered-but-unaccepted is never
    /// retried. Ordinary/provider failures without this exact scope are unchanged.
    pub(in crate::runtime::agent) fn emit_provider_error_terminal_result(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> Result<crate::runtime::agent::TerminalResultDisposition> {
        if !self.provider_refusal_is_armed(&turn.turn_id) {
            return self.emit_subagent_task_result_for_execution(turn, execution);
        }
        let group = self
            .agent
            .completion_settlements
            .groups
            .get(&turn.turn_id)
            .ok_or_else(|| MezError::invalid_state("provider refusal task owner is missing"))?;
        match group.task_result {
            CompletionMilestone::Accepted => {
                return group
                    .provider_handoff
                    .as_ref()
                    .and_then(|owner| owner.task_disposition)
                    .ok_or_else(|| {
                        MezError::invalid_state("provider refusal task disposition is missing")
                    });
            }
            CompletionMilestone::Entered => {
                return Err(MezError::invalid_state(
                    "provider refusal task result is ambiguous; replay prohibited",
                ));
            }
            CompletionMilestone::NotEntered => {}
        }
        if let Some(group) = self
            .agent
            .completion_settlements
            .groups
            .get_mut(&turn.turn_id)
        {
            group.task_result = CompletionMilestone::Entered;
        }
        #[cfg(test)]
        if self.take_provider_handoff_fault_for_tests("task_consumer") {
            let live_before = self.provider_refusal_context_is_live(&turn.turn_id)
                && self.agent_turn_contexts().contains_key(&turn.turn_id);
            self.detach_failed_completion_backing(&turn.turn_id);
            let live_after = self.provider_refusal_context_is_live(&turn.turn_id)
                && self.agent_turn_contexts().contains_key(&turn.turn_id);
            if let Some(owner) = self
                .agent
                .completion_settlements
                .groups
                .get_mut(&turn.turn_id)
                .and_then(|group| group.provider_handoff.as_mut())
            {
                let count = owner.consumer_probe.map_or(0, |probe| probe.2) + 1;
                owner.consumer_probe = Some((live_before, live_after, count));
            }
            if !live_before || !live_after {
                return Err(MezError::invalid_state(
                    "nested refusal capture lost the original live consumer context",
                ));
            }
            return Err(MezError::invalid_state(
                "injected required refusal context consumer failure",
            ));
        }
        let disposition = self.emit_subagent_task_result_for_execution(turn, execution)?;
        if let Some(group) = self
            .agent
            .completion_settlements
            .groups
            .get_mut(&turn.turn_id)
        {
            group.task_result = CompletionMilestone::Accepted;
            if let Some(owner) = group.provider_handoff.as_mut() {
                owner.task_disposition = Some(disposition);
            }
        }
        Ok(disposition)
    }

    /// Keeps the later mutable routed-context consumer in the SAME live scope,
    /// separately recording entry before effects. No retained-context rebinding.
    pub(in crate::runtime::agent) fn complete_provider_error_routed_presentation(
        &mut self,
        turn_id: &str,
        state: AgentTurnState,
    ) -> Result<bool> {
        if !self.provider_refusal_is_armed(turn_id) {
            return self.complete_routed_presentation(turn_id, state);
        }
        let owner = self
            .agent
            .completion_settlements
            .groups
            .get(turn_id)
            .and_then(|group| group.provider_handoff.as_ref())
            .ok_or_else(|| MezError::invalid_state("provider refusal routed owner is missing"))?;
        match owner.routed {
            CompletionMilestone::Accepted => {
                return owner.routed_result.ok_or_else(|| {
                    MezError::invalid_state("provider refusal routed disposition is missing")
                });
            }
            CompletionMilestone::Entered => {
                return Err(MezError::invalid_state(
                    "provider refusal routed result is ambiguous; replay prohibited",
                ));
            }
            CompletionMilestone::NotEntered => {}
        }
        if let Some(owner) = self
            .agent
            .completion_settlements
            .groups
            .get_mut(turn_id)
            .and_then(|group| group.provider_handoff.as_mut())
        {
            owner.routed = CompletionMilestone::Entered;
        }
        let result = self.complete_routed_presentation(turn_id, state)?;
        if let Some(owner) = self
            .agent
            .completion_settlements
            .groups
            .get_mut(turn_id)
            .and_then(|group| group.provider_handoff.as_mut())
        {
            owner.routed = CompletionMilestone::Accepted;
            owner.routed_result = Some(result);
        }
        Ok(result)
    }

    /// Called only by the actual actor lease-rejection boundary, before its
    /// existing failure settlement removes the undispatched claim.
    pub(crate) fn reject_completion_provider_lease(
        &mut self,
        permit: &CompletionProviderHandoff,
        error: &MezError,
    ) -> bool {
        self.reject_completion_provider_handoff(
            permit,
            ProviderHandoffRejection::LeaseRejected,
            error,
        )
    }

    /// Distinguishes an admitted timer followed by pre-dispatch queue failure
    /// from timer refusal. The old timer is not restarted or treated as a worker.
    pub(crate) fn reject_completion_provider_queue(
        &mut self,
        permit: &CompletionProviderHandoff,
        error: &MezError,
    ) -> bool {
        let rejected = self.reject_completion_provider_handoff(
            permit,
            ProviderHandoffRejection::AdmittedLeaseQueueRejected,
            error,
        );
        #[cfg(test)]
        if rejected {
            self.observe_provider_lease_refusal_for_tests(
                &permit.turn.turn_id,
                permit.generation,
                error,
            );
        }
        rejected
    }

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
            .or_else(|| group.correction_execution.as_ref())
            .ok_or_else(|| {
                MezError::invalid_state("provider handoff original execution is unavailable")
            })?;
        if self
            .agent_turn_executions()
            .contains_key(&dispatch.turn.turn_id)
        {
            self.ensure_completion_settlement_source_current(&dispatch.turn, execution)?;
        } else if group.correction_execution.as_ref().is_none()
            || execution.request.turn_id != group.turn.turn_id
            || execution.request.agent_id != group.turn.agent_id
            || crate::runtime::agent::provider_execution::provider_log_execution_group_id(
                execution,
            )? != group.group
            || !self
                .agent_turn_contexts()
                .contains_key(&dispatch.turn.turn_id)
            || group.turn.agent_id != dispatch.turn.agent_id
            || group.turn.pane_id != dispatch.turn.pane_id
            || group.turn.conversation_id != dispatch.turn.conversation_id
        {
            return Err(MezError::invalid_state(
                "provider correction handoff lost its exact transferred source",
            ));
        }
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
                routed: CompletionMilestone::NotEntered,
                routed_result: None,
                source_frozen: false,
                own_loop_restore: None,
                #[cfg(test)]
                consumer_probe: None,
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
        if !self
            .agent_turn_ledger()
            .turn(&permit.turn.turn_id)
            .is_some_and(|turn| permit.matches_turn(turn) && turn.state == AgentTurnState::Running)
            || !self
                .agent_shell_store()
                .get(&permit.turn.pane_id)
                .is_some_and(|session| {
                    session.session_id == permit.turn.conversation_id
                        && session.running_turn_id.as_deref() == Some(permit.turn.turn_id.as_str())
                })
        {
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
                    && !group.paint_revoked
                    && group.scope.is_none()
                    && group.failed_backing.is_none()
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
