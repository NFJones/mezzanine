//! Suspend only an original positively nonwriting compaction exchange. The
//! actor adopts timer transitions before replying; no SQLite, new task, ordinary
//! turn, native send permission or renewed timeout allowance is created here.
//! A paced watchdog retains supervision while the original future is parked.

use super::{RuntimeSessionService, admission_work::RuntimeBudgetHeldAttempt};
use crate::error::{MezError, Result};
use crate::runtime::{RuntimeSideEffect, RuntimeTimerKey, RuntimeTimerKind, RuntimeTransition};

#[derive(Debug, Clone)]
pub(crate) struct CompactionLease {
    deadline_ms: u64,
    wait: Option<CompactionWait>,
    // One exact rearm obligation, destroyed with this actual allocation.
    rearm_pending: bool,
}

#[derive(Debug, Clone)]
struct CompactionWait {
    capture: RuntimeBudgetHeldAttempt,
    waiter: std::sync::Weak<()>,
    remaining_ms: u64,
    resumed: bool,
}

fn timers(pane: &str, generation: u64, delay_ms: u64) -> RuntimeTransition {
    let key = RuntimeTimerKey::new(RuntimeTimerKind::CompactionClaim, pane, generation);
    RuntimeTransition {
        applied: true,
        side_effects: vec![
            RuntimeSideEffect::CancelTimer { key: key.clone() },
            RuntimeSideEffect::ScheduleTimer { key, delay_ms },
        ],
    }
}

impl RuntimeSessionService {
    /// Record only the lease actually adopted by the actor for this allocation.
    /// Duplicate bookkeeping never restarts its original active allowance.
    pub(crate) fn note_compaction_budget_lease(
        &mut self,
        pane: &str,
        generation: u64,
        now_ms: u64,
        timeout_ms: u64,
    ) {
        if let Some(claim) = self
            .agent
            .claimed_agent_compaction_tasks
            .get_mut(&(pane.into(), generation))
        {
            claim.budget_lease.get_or_insert(CompactionLease {
                deadline_ms: now_ms.saturating_add(timeout_ms),
                wait: None,
                rearm_pending: false,
            });
        }
    }

    pub(super) fn budget_compaction_subject(
        &self,
        capture: &RuntimeBudgetHeldAttempt,
    ) -> Result<(String, u64)> {
        let (pane, generation) = capture
            .compaction_subject()
            .ok_or_else(|| MezError::conflict("original compaction hold unavailable"))?;
        if !self.agent_compaction_task_is_current(pane, generation)
            || self.agent_shell_store().get(pane).is_none()
        {
            return Err(MezError::conflict("original compaction owner changed"));
        }
        let claim = self
            .agent
            .claimed_agent_compaction_tasks
            .get(&(pane.into(), generation))
            .ok_or_else(|| MezError::conflict("original compaction claim retired"))?;
        let owner = self
            .claim_runtime_budget_compaction(&claim.task)?
            .ok_or_else(|| MezError::conflict("original compaction membership unavailable"))?;
        if !capture.matches_live_auxiliary_owner(&owner) {
            return Err(MezError::conflict(
                "original compaction hold or allocation changed",
            ));
        }
        Ok((pane.into(), generation))
    }

    pub(crate) fn budget_compaction_claim_is_quiescent(&self, pane: &str, generation: u64) -> bool {
        self.agent
            .claimed_agent_compaction_tasks
            .get(&(pane.into(), generation))
            .and_then(|claim| claim.budget_lease.as_ref())
            .and_then(|lease| lease.wait.as_ref())
            .is_some_and(|wait| {
                !wait.resumed
                    && wait.waiter.upgrade().is_some()
                    && self.budget_compaction_subject(&wait.capture).is_ok()
            })
    }

    /// Early callbacks from a replaced same-key timer must not expire the
    /// resumed lease. Dead/revoked parked futures are not granted an extension.
    pub(crate) fn compaction_budget_timer_delay(
        &self,
        pane: &str,
        generation: u64,
        now_ms: u64,
    ) -> Option<u64> {
        let lease = self
            .agent
            .claimed_agent_compaction_tasks
            .get(&(pane.into(), generation))?
            .budget_lease
            .as_ref()?;
        // The original unsuspended timer retains its existing authoritative
        // expiry semantics. Only park/resume can create stale same-key ticks.
        let wait = lease.wait.as_ref()?;
        if !wait.resumed {
            return self
                .budget_compaction_claim_is_quiescent(pane, generation)
                .then_some(1000);
        }
        lease
            .deadline_ms
            .checked_sub(now_ms)
            .filter(|remaining| *remaining > 0)
    }

    /// A due physical timer no longer exists. Retain its rearm with this claim
    /// until actual queue adoption, including under exhausted committed reserve.
    pub(crate) fn compaction_budget_timer_offer_delay(
        &self,
        pane: &str,
        generation: u64,
        now_ms: u64,
    ) -> Option<u64> {
        let claim = self
            .agent
            .claimed_agent_compaction_tasks
            .get(&(pane.into(), generation))?;
        claim.budget_lease.as_ref()?.wait.as_ref()?;
        if self.replay_epoch_publication(pane, generation).is_some() {
            return None;
        }
        Some(
            self.compaction_budget_timer_delay(pane, generation, now_ms)
                .unwrap_or(1),
        )
    }

    pub(crate) fn retain_compaction_budget_watchdog(&mut self, pane: &str, generation: u64) {
        if let Some(lease) = self
            .agent
            .claimed_agent_compaction_tasks
            .get_mut(&(pane.into(), generation))
            .and_then(|claim| claim.budget_lease.as_mut())
        {
            lease.rearm_pending = true;
        }
    }

    pub(crate) fn compaction_budget_watchdog_pending(&self, pane: &str, generation: u64) -> bool {
        self.agent
            .claimed_agent_compaction_tasks
            .get(&(pane.into(), generation))
            .and_then(|claim| claim.budget_lease.as_ref())
            .is_some_and(|lease| lease.rearm_pending)
    }

    /// Recompute the remaining active deadline at recovery, never restart it.
    /// Invalid/dead holds receive a prompt expiry callback, not another hold.
    pub(crate) fn pending_compaction_budget_watchdogs(
        &self,
        now_ms: u64,
    ) -> Vec<(String, u64, u64)> {
        self.agent
            .claimed_agent_compaction_tasks
            .iter()
            .filter_map(|((pane, generation), claim)| {
                claim
                    .budget_lease
                    .as_ref()
                    .filter(|lease| lease.rearm_pending)?;
                Some((
                    pane.clone(),
                    *generation,
                    self.compaction_budget_timer_delay(pane, *generation, now_ms)
                        .unwrap_or(1),
                ))
            })
            .collect()
    }

    pub(crate) fn adopt_compaction_budget_watchdog(&mut self, pane: &str, generation: u64) {
        if let Some(lease) = self
            .agent
            .claimed_agent_compaction_tasks
            .get_mut(&(pane.into(), generation))
            .and_then(|claim| claim.budget_lease.as_mut())
        {
            lease.rearm_pending = false;
        }
    }

    pub(crate) fn park_budget_compaction_claim(
        &mut self,
        capture: RuntimeBudgetHeldAttempt,
        waiter: std::sync::Weak<()>,
        now_ms: u64,
    ) -> Result<RuntimeTransition> {
        let (pane, generation) = self.budget_compaction_subject(&capture)?;
        if waiter.upgrade().is_none() {
            return Err(MezError::conflict("original compaction waiter retired"));
        }
        let lease = self
            .agent
            .claimed_agent_compaction_tasks
            .get(&(pane.clone(), generation))
            .and_then(|claim| claim.budget_lease.as_ref())
            .ok_or_else(|| MezError::conflict("original compaction lease unavailable"))?;
        if let Some(wait) = &lease.wait {
            if wait.capture.same_capture(&capture) && !wait.resumed && wait.waiter.ptr_eq(&waiter) {
                return Ok(RuntimeTransition::default());
            }
            if !wait.resumed || wait.capture.same_capture(&capture) {
                return Err(MezError::conflict(
                    "original compaction wait retained or resumed",
                ));
            }
        }
        let remaining_ms = lease.deadline_ms.saturating_sub(now_ms);
        if remaining_ms == 0 {
            return Err(MezError::conflict(
                "original compaction lease exhausted before park",
            ));
        }
        self.append_budget_hold_notice(&pane)?;
        let lease = self
            .agent
            .claimed_agent_compaction_tasks
            .get_mut(&(pane.clone(), generation))
            .and_then(|claim| claim.budget_lease.as_mut())
            .ok_or_else(|| MezError::conflict("original compaction lease unavailable"))?;
        lease.wait = Some(CompactionWait {
            capture,
            waiter,
            remaining_ms,
            resumed: false,
        });
        lease.rearm_pending = false;
        Ok(timers(&pane, generation, 1000))
    }

    /// Pause is orthogonal. Adopt this returned timer before waking the same
    /// future; it must still obtain fresh native authorization and final fence.
    pub(crate) fn resume_budget_compaction_claim(
        &mut self,
        capture: &RuntimeBudgetHeldAttempt,
        now_ms: u64,
    ) -> Result<Option<RuntimeTransition>> {
        let (pane, generation) = self.budget_compaction_subject(capture)?;
        if self.agent_is_human_paused(&pane) {
            return Ok(None);
        }
        let lease = self
            .agent
            .claimed_agent_compaction_tasks
            .get_mut(&(pane.clone(), generation))
            .and_then(|claim| claim.budget_lease.as_mut())
            .ok_or_else(|| MezError::conflict("original compaction lease unavailable"))?;
        let wait = lease
            .wait
            .as_mut()
            .filter(|wait| wait.capture.same_capture(capture) && wait.waiter.upgrade().is_some())
            .ok_or_else(|| MezError::conflict("original compaction wait unavailable"))?;
        if wait.resumed {
            return Ok(Some(RuntimeTransition::default()));
        }
        lease.deadline_ms = now_ms.saturating_add(wait.remaining_ms);
        wait.resumed = true;
        lease.rearm_pending = false;
        Ok(Some(timers(&pane, generation, wait.remaining_ms)))
    }
}
