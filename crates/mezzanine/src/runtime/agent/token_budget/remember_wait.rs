//! Nonwriting parking of the original remembering claim. No ordinary turn,
//! provider retry, lease renewal or second runnable queue is created here.
//! Membership, positive hold epoch and actual claim are checked on every read.

use super::{
    RuntimeSessionService, admission_work::RuntimeBudgetHeldAttempt,
    auxiliary::RuntimeBudgetAuxiliaryClaim,
};
use crate::error::{MezError, Result};

#[derive(Debug, Clone)]
pub(crate) struct RememberWait {
    capture: RuntimeBudgetHeldAttempt,
    waiter: std::sync::Weak<()>,
    resumed: bool,
}

impl RuntimeSessionService {
    pub(super) fn budget_remember_subject(
        &self,
        capture: &RuntimeBudgetHeldAttempt,
    ) -> Result<String> {
        let Some(RuntimeBudgetAuxiliaryClaim::Remember { pane, observation }) =
            capture.auxiliary_claim()
        else {
            return Err(MezError::conflict(
                "original memory budget claim unavailable",
            ));
        };
        let claim = self
            .agent
            .claimed_agent_remember_tasks
            .get(pane)
            .filter(|claim| claim.task.observation_id == *observation)
            .ok_or_else(|| MezError::conflict("original memory task retired or changed"))?;
        let owner = self
            .runtime_budget_remember_owner(&claim.task)?
            .ok_or_else(|| MezError::conflict("original memory membership unavailable"))?;
        if !capture.matches_owner(&owner) {
            return Err(MezError::conflict("original memory budget hold changed"));
        }
        Ok(pane.clone())
    }

    /// Only the live original future and current positive hold are quiescent.
    /// Late expense custody cannot keep a dead or replaced worker parked.
    pub(crate) fn budget_remember_claim_is_quiescent(&self, pane: &str) -> bool {
        self.agent
            .claimed_agent_remember_tasks
            .get(pane)
            .and_then(|claim| claim.budget_wait.as_ref())
            .is_some_and(|wait| {
                !wait.resumed
                    && wait.waiter.upgrade().is_some()
                    && self.budget_remember_subject(&wait.capture).is_ok()
            })
    }

    /// Title and memory claims share the existing nonwriting actor protocol,
    /// not their lease policy. Other auxiliary kinds deliberately refuse.
    pub(crate) fn park_budget_auxiliary_claim(
        &mut self,
        capture: RuntimeBudgetHeldAttempt,
        waiter: std::sync::Weak<()>,
        now_ms: u64,
    ) -> Result<bool> {
        if capture.is_title() {
            return self.park_budget_title_claim(capture, waiter, now_ms);
        }
        if waiter.upgrade().is_none() {
            return Err(MezError::conflict("original memory waiter retired"));
        }
        let pane = self.budget_remember_subject(&capture)?;
        let claim = self
            .agent
            .claimed_agent_remember_tasks
            .get_mut(&pane)
            .ok_or_else(|| MezError::conflict("original memory claim unavailable"))?;
        if let Some(wait) = &claim.budget_wait {
            if wait.capture.same_capture(&capture) && !wait.resumed && wait.waiter.ptr_eq(&waiter) {
                return Ok(true);
            }
            if !wait.resumed || wait.capture.same_capture(&capture) {
                return Err(MezError::conflict(
                    "original memory wait already retained or resumed",
                ));
            }
        }
        claim.budget_wait = Some(RememberWait {
            capture,
            waiter,
            resumed: false,
        });
        Ok(true)
    }

    /// Human Pause is independent. Resuming this same future supplies no send
    /// permit; native policy and the claim-local fence are checked afterward.
    pub(crate) fn resume_budget_auxiliary_claim(
        &mut self,
        capture: &RuntimeBudgetHeldAttempt,
        now_ms: u64,
    ) -> Result<bool> {
        if capture.is_title() {
            return self.resume_budget_title_claim(capture, now_ms);
        }
        let pane = self.budget_remember_subject(capture)?;
        if self.agent_is_human_paused(&pane) {
            return Ok(false);
        }
        let wait = self
            .agent
            .claimed_agent_remember_tasks
            .get_mut(&pane)
            .and_then(|claim| claim.budget_wait.as_mut())
            .filter(|wait| wait.capture.same_capture(capture) && wait.waiter.upgrade().is_some())
            .ok_or_else(|| MezError::conflict("original memory wait unavailable"))?;
        wait.resumed = true;
        Ok(true)
    }
}
