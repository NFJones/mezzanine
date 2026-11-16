//! Nonwriting suspension of the original optional-title claim. This stores no
//! runnable queue and grants no native permission. Current claim and membership
//! checks are repeated before lease exclusion or resuming the same future.

use super::{
    RuntimeSessionService, admission_work::RuntimeBudgetHeldAttempt,
    auxiliary::RuntimeBudgetAuxiliaryClaim,
};
use crate::error::{MezError, Result};

#[derive(Debug, Clone)]
pub(crate) struct TitleWait {
    capture: RuntimeBudgetHeldAttempt,
    // Only the original waiting future owns the strong token. The retained
    // hold/journal alone cannot suppress expiry after producer destruction.
    waiter: std::sync::Weak<()>,
    started_at_ms: u64,
    resumed: bool,
    ready_order: Option<u64>,
}

impl RuntimeSessionService {
    /// Pending and active titles reserve slots. Only an exact live nonwriting
    /// hold releases execution capacity, not its original task/expense custody.
    pub(in crate::runtime::agent) fn budget_title_active_capacity(&self) -> usize {
        let parked = self
            .agent
            .claimed_agent_session_title_tasks
            .keys()
            .filter(|conversation| self.budget_title_claim_is_quiescent(conversation))
            .count();
        self.agent
            .session_title_tasks
            .in_flight()
            .saturating_sub(parked)
    }

    /// Readiness is requested only after the original future observes policy
    /// progress. Paused, retired and revoked waiters cannot block healthy work.
    pub(in crate::runtime::agent) fn oldest_ready_budget_title(&self) -> Option<&str> {
        self.agent
            .claimed_agent_session_title_tasks
            .iter()
            .filter_map(|(conversation, claim)| {
                let wait = claim.budget_wait.as_ref()?;
                let order = wait.ready_order?;
                (!wait.resumed
                    && !self.agent_is_human_paused(&claim.task.pane_id)
                    && self.budget_title_claim_is_quiescent(conversation))
                .then_some((order, conversation.as_str()))
            })
            .min_by_key(|(order, _)| *order)
            .map(|(_, conversation)| conversation)
    }

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
    pub(super) fn budget_title_subject(
        &self,
        capture: &RuntimeBudgetHeldAttempt,
    ) -> Result<(String, String)> {
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
        let (conversation, pane) = self.budget_title_subject(&capture)?;
        let claim = self
            .agent
            .claimed_agent_session_title_tasks
            .get(&conversation)
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
        self.append_budget_hold_notice(&pane)?;
        let claim = self
            .agent
            .claimed_agent_session_title_tasks
            .get_mut(&conversation)
            .ok_or_else(|| MezError::conflict("original title claim unavailable"))?;
        claim.budget_wait = Some(TitleWait {
            capture,
            waiter,
            started_at_ms: now_ms,
            resumed: false,
            ready_order: None,
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
        let wait = self
            .agent
            .claimed_agent_session_title_tasks
            .get(&conversation)
            .and_then(|claim| claim.budget_wait.as_ref())
            .filter(|wait| wait.capture.same_capture(capture) && wait.waiter.upgrade().is_some())
            .ok_or_else(|| MezError::conflict("original title wait unavailable"))?;
        if wait.resumed {
            return Ok(true);
        }
        if wait.ready_order.is_none() {
            let next = self
                .agent
                .budget_memberships
                .next_title_ready_order
                .checked_add(1)
                .ok_or_else(|| MezError::invalid_state("title readiness order exhausted"))?;
            self.agent.budget_memberships.next_title_ready_order = next;
            if let Some(wait) = self
                .agent
                .claimed_agent_session_title_tasks
                .get_mut(&conversation)
                .and_then(|claim| claim.budget_wait.as_mut())
            {
                wait.ready_order = Some(next);
            }
        }
        if self.budget_title_active_capacity()
            >= super::super::session_titles::MAX_CONCURRENT_SESSION_TITLE_TASKS
            || self.oldest_ready_budget_title() != Some(conversation.as_str())
        {
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
