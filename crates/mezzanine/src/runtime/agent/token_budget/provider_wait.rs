//! Scheduling custody for an unissued hold or a settled ORIGINAL response.
//!
//! This port does no I/O and grants no send permission. It keeps the ledger and
//! original claim live while the existing scheduler releases/reacquires capacity.
//! Reacquisition never queues a replacement provider continuation. The host must
//! adopt the returned timer transition before resuming the original future and
//! freshly reconsidering admission or reobserving completion policy. Settled
//! captures borrow the result's original future and never mint an inference
//! hold epoch. Root-wide effect admission and activation remain staged.

use super::{RuntimeSessionService, admission_work::RuntimeBudgetHeldAttempt};
use crate::error::{MezError, Result};
use crate::runtime::{RuntimeSideEffect, RuntimeTimerKey, RuntimeTimerKind, RuntimeTransition};
use mez_agent::{AgentTurnRecord, AgentTurnState};
use mez_core::ids::AgentId;

/// Sealed reasons to retain the same original provider claim. Neither variant
/// grants a send or effect; completion custody is never an inference Held epoch.
#[derive(Debug, Clone)]
pub(crate) enum RuntimeBudgetProviderCapture {
    Inference(RuntimeBudgetHeldAttempt),
    Settled(super::request::RuntimeBudgetSettledResponse),
}

impl From<RuntimeBudgetHeldAttempt> for RuntimeBudgetProviderCapture {
    fn from(value: RuntimeBudgetHeldAttempt) -> Self {
        Self::Inference(value)
    }
}
impl From<&RuntimeBudgetHeldAttempt> for RuntimeBudgetProviderCapture {
    fn from(value: &RuntimeBudgetHeldAttempt) -> Self {
        Self::Inference(value.clone())
    }
}
impl From<super::request::RuntimeBudgetSettledResponse> for RuntimeBudgetProviderCapture {
    fn from(value: super::request::RuntimeBudgetSettledResponse) -> Self {
        Self::Settled(value)
    }
}
impl From<&RuntimeBudgetProviderCapture> for RuntimeBudgetProviderCapture {
    fn from(value: &RuntimeBudgetProviderCapture) -> Self {
        value.clone()
    }
}

impl RuntimeBudgetProviderCapture {
    fn matches_owner(&self, owner: &super::request::RuntimeBudgetRequestOwner) -> bool {
        match self {
            Self::Inference(c) => c.matches_owner(owner),
            Self::Settled(c) => c.matches_owner(owner),
        }
    }
    fn matches_claim(&self, agent: &str, turn: &str, generation: u64) -> bool {
        match self {
            Self::Inference(c) => c.matches_claim(agent, turn, generation),
            Self::Settled(c) => c.matches_claim(agent, turn, generation),
        }
    }
    fn same_capture(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Inference(a), Self::Inference(b)) => a.same_capture(b),
            (Self::Settled(a), Self::Settled(b)) => a.same_capture(b),
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Waiting,
    Reacquiring,
    Ready,
    Active,
}

#[derive(Debug)]
pub(super) struct ProviderWait {
    capture: RuntimeBudgetProviderCapture,
    generation: u64,
    started_at_ms: u64,
    phase: Phase,
    // First pump failure remains with this bounded original wait, not a new
    // recovery source. Failed pumping never permits native reconsideration.
    pump_failure: Option<MezError>,
}

impl RuntimeSessionService {
    /// Fixed local presentation only, after current hold qualification and
    /// before wait mutation. No payload, policy authority or provider work.
    /// A refused append leaves the original claim/receipt owned; callers must
    /// not bypass admission. Duplicate parked captures return before this call.
    pub(super) fn append_budget_hold_notice(&mut self, pane: &str) -> Result<()> {
        self.append_agent_status_text_to_terminal_buffer(
            pane,
            "agent: session token budget hold; original request/result retained; waiting for budget, accounting, or fair admission; Stop remains available",
        )
    }

    /// A retained claim is quiescent only while its exact positive nonwriting
    /// capture remains live and it has not resumed. This is not permission to
    /// send, retire the worker, or ignore independent issued effects.
    pub(crate) fn budget_provider_claim_is_quiescent(&self, turn: &str) -> bool {
        let Some(wait) = self.agent.budget_memberships.provider_waits.get(turn) else {
            return false;
        };
        let Some(record) = self.agent_turn_ledger().turn(turn) else {
            return false;
        };
        let Some(agent) = AgentId::opaque(record.agent_id.clone()) else {
            return false;
        };
        wait.phase != Phase::Active
            && self
                .budget_wait_turn(&agent, turn, wait.generation, &wait.capture)
                .is_ok()
    }

    /// Human pause may arrive after fair selection but before the original
    /// future resumes. Release that selected capacity without terminalizing its
    /// ledger or replacing its capture/first inactive clock.
    pub(in crate::runtime::agent) fn pause_quiescent_budget_provider_claim(
        &mut self,
        turn: &str,
    ) -> Result<()> {
        if !self.budget_provider_claim_is_quiescent(turn) {
            return Err(MezError::conflict(
                "original nonwriting budget hold changed",
            ));
        }
        if self
            .agent
            .agent_scheduler
            .running_turns()
            .any(|work| work.turn_id == turn)
        {
            self.agent.agent_scheduler.wait_running(turn)?;
            if let Some(wait) = self.agent.budget_memberships.provider_waits.get_mut(turn) {
                wait.phase = Phase::Waiting;
            }
        }
        Ok(())
    }

    pub(super) fn budget_wait_turn(
        &self,
        agent: &AgentId,
        turn: &str,
        generation: u64,
        capture: impl Into<RuntimeBudgetProviderCapture>,
    ) -> Result<AgentTurnRecord> {
        let capture = capture.into();
        if !self.agent_provider_claim_matches(agent, turn, generation) {
            return Err(MezError::conflict("original budget provider claim changed"));
        }
        let record = self
            .agent_turn_ledger()
            .turn(turn)
            .filter(|record| record.state == AgentTurnState::Running)
            .ok_or_else(|| MezError::conflict("original budget provider turn unavailable"))?;
        let claim = &self.agent.claimed_agent_provider_tasks[turn];
        let session = self
            .agent_shell_store()
            .get(&record.pane_id)
            .ok_or_else(|| MezError::conflict("original budget provider pane unavailable"))?;
        if claim.conversation_id != record.conversation_id
            || session.session_id != record.conversation_id
            || session.running_turn_id.as_deref() != Some(turn)
            || self.agent_provider_task_is_pending(turn)
        {
            return Err(MezError::conflict(
                "original budget provider custody changed",
            ));
        }
        let owner = self
            .runtime_budget_request_owner(record)?
            .ok_or_else(|| MezError::conflict("original budget membership unavailable"))?;
        if !capture.matches_owner(&owner)
            || !capture.matches_claim(agent.as_str(), turn, generation)
        {
            return Err(MezError::conflict("original budget hold changed"));
        }
        Ok(record.clone())
    }

    /// Release only scheduler capacity after the original nonwriting Held result.
    /// Duplicate capture retains its original clock; another attempt cannot replace
    /// a parked claim. Ledger Running preserves original monitor/Stop semantics.
    pub(crate) fn park_budget_provider_claim(
        &mut self,
        agent: &AgentId,
        turn: &str,
        generation: u64,
        capture: impl Into<RuntimeBudgetProviderCapture>,
        now_ms: u64,
    ) -> Result<RuntimeTransition> {
        let capture = capture.into();
        let record = self.budget_wait_turn(agent, turn, generation, &capture)?;
        if let Some(wait) = self.agent.budget_memberships.provider_waits.get(turn) {
            if wait.generation == generation
                && wait.capture.same_capture(&capture)
                && wait.phase != Phase::Active
            {
                return Ok(RuntimeTransition::default());
            }
            if wait.phase != Phase::Active {
                return Err(MezError::conflict("original budget wait already retained"));
            }
            if wait.capture.same_capture(&capture) {
                return Err(MezError::conflict("original budget wait already resumed"));
            }
        }
        // One entry per existing finite claim, never a separate work queue.
        let claim = &self.agent.claimed_agent_provider_tasks[turn];
        if now_ms.saturating_sub(claim.claimed_at_unix_ms) >= claim.timeout_ms {
            return Err(MezError::conflict(
                "original budget provider lease exhausted",
            ));
        }
        if self.agent.budget_memberships.provider_waits.len() >= 4096
            && !self
                .agent
                .budget_memberships
                .provider_waits
                .contains_key(turn)
        {
            return Err(MezError::invalid_state(
                "budget provider wait capacity exhausted",
            ));
        }
        self.append_budget_hold_notice(&record.pane_id)?;
        self.agent.agent_scheduler.wait_running(turn)?;
        self.agent.budget_memberships.provider_waits.insert(
            turn.into(),
            ProviderWait {
                capture,
                generation,
                started_at_ms: now_ms,
                phase: Phase::Waiting,
                pump_failure: None,
            },
        );
        self.park_agent_turn_deadline(turn, now_ms);
        Ok(RuntimeTransition {
            applied: true,
            side_effects: vec![RuntimeSideEffect::CancelTimer {
                key: RuntimeTimerKey::new(RuntimeTimerKind::ProviderClaim, turn, generation),
            }],
        })
    }

    /// A dependency observation only permits requesting fair reacquisition, not
    /// another inference. Queue refusal preserves the original Waiting entry.
    #[cfg(test)]
    pub(crate) fn requeue_budget_provider_claim(
        &mut self,
        agent: &AgentId,
        turn: &str,
        generation: u64,
        capture: impl Into<RuntimeBudgetProviderCapture>,
    ) -> Result<()> {
        if self.try_requeue_budget_provider_claim(agent, turn, generation, capture)? {
            Ok(())
        } else {
            Err(MezError::invalid_state(
                "original budget scheduler remains unavailable",
            ))
        }
    }

    /// False preserves the same wait under positively unmutated queue pressure
    /// or a retained pump refusal. Ownership errors remain errors, not retries.
    pub(crate) fn try_requeue_budget_provider_claim(
        &mut self,
        agent: &AgentId,
        turn: &str,
        generation: u64,
        capture: impl Into<RuntimeBudgetProviderCapture>,
    ) -> Result<bool> {
        let capture = capture.into();
        self.budget_wait_turn(agent, turn, generation, &capture)?;
        let wait = self
            .agent
            .budget_memberships
            .provider_waits
            .get(turn)
            .filter(|wait| wait.generation == generation && wait.capture.same_capture(&capture))
            .ok_or_else(|| MezError::conflict("original budget wait unavailable"))?;
        if wait.phase == Phase::Waiting {
            if let Err(error) = self.agent.agent_scheduler.requeue_waiting(turn) {
                if error.kind() == mez_agent::SchedulerErrorKind::QueueFull {
                    return Ok(false);
                }
                return Err(error.into());
            }
            // Exact entry remains present; no external work can run on the actor.
            if let Some(wait) = self.agent.budget_memberships.provider_waits.get_mut(turn) {
                wait.phase = Phase::Reacquiring;
            }
        }
        Ok(self.pump_budget_provider_wait(turn))
    }

    /// Sibling startup can fail after mutating its own scheduler state. Keep
    /// that first cause and refuse this wait's resume until pumping succeeds;
    /// never drop an already-produced claim-timer cancellation for it.
    pub(crate) fn pump_budget_provider_wait(&mut self, turn: &str) -> bool {
        match self.start_ready_agent_turns() {
            Ok(_) => true,
            Err(error) => {
                if let Some(wait) = self.agent.budget_memberships.provider_waits.get_mut(turn)
                    && wait.pump_failure.is_none()
                {
                    eprintln!(
                        "mezzanine: retained budget scheduler pump refused ({:?})",
                        error.kind()
                    );
                    wait.pump_failure = Some(error);
                }
                false
            }
        }
    }

    /// Called only by the existing scheduler AFTER it selected this work. Return
    /// true only for the exact retained Running claim, never ordinary new work.
    pub(crate) fn reacquire_budget_provider_claim(
        &mut self,
        turn: &AgentTurnRecord,
    ) -> Result<bool> {
        let Some(wait) = self
            .agent
            .budget_memberships
            .provider_waits
            .get(&turn.turn_id)
        else {
            return Ok(false);
        };
        if turn.state != AgentTurnState::Running || wait.phase != Phase::Reacquiring {
            return Ok(false);
        }
        let agent = AgentId::opaque(turn.agent_id.clone())
            .ok_or_else(|| MezError::invalid_state("budget provider agent invalid"))?;
        self.budget_wait_turn(&agent, &turn.turn_id, wait.generation, &wait.capture)?;
        if let Some(wait) = self
            .agent
            .budget_memberships
            .provider_waits
            .get_mut(&turn.turn_id)
        {
            wait.phase = Phase::Ready;
        }
        Ok(true)
    }

    /// Adopt fair selection before resuming the SAME producer. The returned timer
    /// is mandatory host bookkeeping, not a grant. Exact duplicate adoption is
    /// inert; the same captured hold cannot park this claim again afterward.
    pub(crate) fn resume_budget_provider_claim(
        &mut self,
        agent: &AgentId,
        turn: &str,
        generation: u64,
        capture: impl Into<RuntimeBudgetProviderCapture>,
        now_ms: u64,
    ) -> Result<RuntimeTransition> {
        let capture = capture.into();
        self.budget_wait_turn(agent, turn, generation, &capture)?;
        if self.agent_turn_is_human_paused(turn) {
            return Err(MezError::conflict("budget provider remains human paused"));
        }
        let wait = self
            .agent
            .budget_memberships
            .provider_waits
            .get(turn)
            .filter(|wait| wait.generation == generation && wait.capture.same_capture(&capture))
            .ok_or_else(|| MezError::conflict("original budget wait unavailable"))?;
        if wait.phase == Phase::Active {
            return Ok(RuntimeTransition::default());
        }
        if wait.phase != Phase::Ready {
            return Err(MezError::conflict(
                "original budget claim has not fairly reacquired",
            ));
        }
        let excluded = now_ms.saturating_sub(wait.started_at_ms);
        let claim = self
            .agent
            .claimed_agent_provider_tasks
            .get_mut(turn)
            .ok_or_else(|| MezError::conflict("original budget provider claim unavailable"))?;
        claim.claimed_at_unix_ms = claim.claimed_at_unix_ms.saturating_add(excluded);
        let remaining = claim
            .timeout_ms
            .saturating_sub(now_ms.saturating_sub(claim.claimed_at_unix_ms));
        if let Some(wait) = self.agent.budget_memberships.provider_waits.get_mut(turn) {
            wait.phase = Phase::Active;
        }
        self.resume_agent_turn_deadline(turn, now_ms)?;
        Ok(RuntimeTransition {
            applied: true,
            side_effects: vec![RuntimeSideEffect::ScheduleTimer {
                key: RuntimeTimerKey::new(RuntimeTimerKind::ProviderClaim, turn, generation),
                delay_ms: remaining,
            }],
        })
    }

    /// Descriptive fair-selection state, after the caller validated the original
    /// capture with requeue. This never grants native send authority.
    pub(crate) fn budget_provider_claim_can_resume(&self, turn: &str, generation: u64) -> bool {
        !self.agent_turn_is_human_paused(turn)
            && self
                .agent
                .budget_memberships
                .provider_waits
                .get(turn)
                .is_some_and(|wait| {
                    wait.generation == generation
                        && matches!(wait.phase, Phase::Ready | Phase::Active)
                })
    }

    pub(crate) fn budget_provider_claim_is_parked(&self, turn: &str, generation: u64) -> bool {
        self.agent
            .budget_memberships
            .provider_waits
            .get(turn)
            .is_some_and(|wait| wait.generation == generation && wait.phase != Phase::Active)
    }

    /// A pre-park callback may already be queued when cancellation is adopted.
    /// After resume its original key cannot shorten the extended deadline; the
    /// actor re-arms the remaining interval instead of consuming the new lease.
    pub(crate) fn budget_provider_claim_timer_remaining(
        &self,
        turn: &str,
        generation: u64,
        now_ms: u64,
    ) -> Option<u64> {
        let wait = self.agent.budget_memberships.provider_waits.get(turn)?;
        let claim = self.agent.claimed_agent_provider_tasks.get(turn)?;
        if wait.generation != generation
            || claim.generation != generation
            || wait.phase != Phase::Active
        {
            return None;
        }
        let remaining = claim
            .timeout_ms
            .saturating_sub(now_ms.saturating_sub(claim.claimed_at_unix_ms));
        (remaining > 0).then_some(remaining)
    }

    pub(crate) fn retire_budget_provider_wait(&mut self, turn: &str) {
        self.agent.budget_memberships.provider_waits.remove(turn);
    }
}
