//! Nonwriting suspension of the original optional-title claim. This stores no
//! runnable queue and grants no native permission. Current claim and membership
//! checks are repeated before lease exclusion or resuming the same future.

use super::{
    RuntimeSessionService, admission_work::RuntimeBudgetHeldAttempt,
    auxiliary::RuntimeBudgetAuxiliaryClaim,
};
use crate::error::{MezError, Result};

/// The actual publication claim owns the strong fence. Expense/request clones
/// retain only a Weak reference. The final atomic read, after native binding
/// validation, orders send admission against retirement without actor I/O/locks.
#[derive(Debug, Clone)]
pub(crate) struct TitleClaimFence(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl TitleClaimFence {
    pub(crate) fn new() -> Self {
        Self(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
            true,
        )))
    }

    pub(crate) fn capture(&self) -> std::sync::Weak<std::sync::atomic::AtomicBool> {
        std::sync::Arc::downgrade(&self.0)
    }

    pub(crate) fn retire(&self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TitleWait {
    capture: RuntimeBudgetHeldAttempt,
    // Only the original waiting future owns the strong token. The retained
    // hold/journal alone cannot suppress expiry after producer destruction.
    waiter: std::sync::Weak<()>,
    started_at_ms: u64,
    resumed: bool,
}

impl RuntimeSessionService {
    /// Revoke before removing publication custody. Existing cloned lease views
    /// and late accounting owners cannot keep the old send boundary open.
    pub(in crate::runtime::agent) fn take_agent_session_title_claim(
        &mut self,
        conversation: &str,
    ) -> Option<crate::runtime::RuntimeAgentSessionTitleClaim> {
        if let Some(claim) = self
            .agent
            .claimed_agent_session_title_tasks
            .get(conversation)
        {
            claim.budget_fence.retire();
        }
        self.agent
            .claimed_agent_session_title_tasks
            .remove(conversation)
    }

    /// Returns only the actual current publication claim, never its separately
    /// retained late-expense owner. A cancelled title cannot regain authority.
    fn budget_title_subject(&self, capture: &RuntimeBudgetHeldAttempt) -> Result<(String, String)> {
        let Some(RuntimeBudgetAuxiliaryClaim::Title {
            conversation,
            dispatch,
            attempt,
        }) = capture.auxiliary_claim()
        else {
            return Err(MezError::conflict(
                "original title budget claim unavailable",
            ));
        };
        let claim = self
            .agent
            .claimed_agent_session_title_tasks
            .get(conversation)
            .filter(|claim| {
                claim.task.dispatch_id == *dispatch && claim.task.attempt_id == *attempt
            })
            .ok_or_else(|| MezError::conflict("original title task retired or changed"))?;
        let owner = self
            .claim_runtime_budget_title(&claim.task)?
            .ok_or_else(|| MezError::conflict("original title membership unavailable"))?;
        if !capture.matches_owner(&owner) {
            return Err(MezError::conflict("original title budget hold changed"));
        }
        Ok((conversation.clone(), claim.task.pane_id.clone()))
    }

    /// A positive current hold is quiescent for pause and lease expiry only
    /// before resume. Revoked epochs cannot hide an issued request.
    pub(crate) fn budget_title_claim_is_quiescent(&self, conversation: &str) -> bool {
        self.agent
            .claimed_agent_session_title_tasks
            .get(conversation)
            .and_then(|claim| claim.budget_wait.as_ref())
            .is_some_and(|wait| {
                !wait.resumed
                    && wait.waiter.upgrade().is_some()
                    && self.budget_title_subject(&wait.capture).is_ok()
            })
    }

    pub(crate) fn park_budget_title_claim(
        &mut self,
        capture: RuntimeBudgetHeldAttempt,
        waiter: std::sync::Weak<()>,
        now_ms: u64,
    ) -> Result<bool> {
        if waiter.upgrade().is_none() {
            return Err(MezError::conflict("original title waiter retired"));
        }
        let (conversation, _) = self.budget_title_subject(&capture)?;
        let claim = self
            .agent
            .claimed_agent_session_title_tasks
            .get_mut(&conversation)
            .ok_or_else(|| MezError::conflict("original title claim unavailable"))?;
        if let Some(wait) = &claim.budget_wait {
            if wait.capture.same_capture(&capture) && !wait.resumed && wait.waiter.ptr_eq(&waiter) {
                return Ok(true);
            }
            if !wait.resumed || wait.capture.same_capture(&capture) {
                return Err(MezError::conflict(
                    "original title wait already retained or resumed",
                ));
            }
        }
        if now_ms.saturating_sub(claim.claimed_at_unix_ms) >= claim.timeout_ms {
            return Err(MezError::conflict(
                "original title lease exhausted before park",
            ));
        }
        claim.budget_wait = Some(TitleWait {
            capture,
            waiter,
            started_at_ms: now_ms,
            resumed: false,
        });
        Ok(true)
    }

    /// Pause remains orthogonal. Duplicate adoption cannot extend the lease a
    /// second time; an exact native authorization is still required afterward.
    pub(crate) fn resume_budget_title_claim(
        &mut self,
        capture: &RuntimeBudgetHeldAttempt,
        now_ms: u64,
    ) -> Result<bool> {
        let (conversation, pane) = self.budget_title_subject(capture)?;
        if self.agent_is_human_paused(&pane) {
            return Ok(false);
        }
        let claim = self
            .agent
            .claimed_agent_session_title_tasks
            .get_mut(&conversation)
            .ok_or_else(|| MezError::conflict("original title claim unavailable"))?;
        let wait = claim
            .budget_wait
            .as_mut()
            .filter(|wait| wait.capture.same_capture(capture) && wait.waiter.upgrade().is_some())
            .ok_or_else(|| MezError::conflict("original title wait unavailable"))?;
        if !wait.resumed {
            claim.claimed_at_unix_ms = claim
                .claimed_at_unix_ms
                .saturating_add(now_ms.saturating_sub(wait.started_at_ms));
            wait.resumed = true;
            self.agent
                .session_title_tasks
                .note_claim(&conversation, claim.claimed_at_unix_ms);
        }
        Ok(true)
    }
}
