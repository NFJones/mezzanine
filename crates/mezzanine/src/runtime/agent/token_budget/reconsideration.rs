//! Order already-held native reconsideration across the existing claim families.
//! No request payload, native permit, runnable task or expense owner lives here.
//! Weak original-future tokens bound retention; a separate weak selected window
//! lasts through native authorization even when that writer revokes its hold
//! epoch. Family capacity/timer adoption and native policy remain independent.
//! Fresh initial attempts and cross-runtime fairness are not governed here.

use super::{
    RuntimeSessionService,
    admission_work::{RuntimeBudgetHeldAttempt, WeakHeldAttempt},
    auxiliary::RuntimeBudgetAuxiliaryClaim,
};
use crate::error::{MezError, Result};
use mez_agent::token_budget::BudgetRootId;
use std::{collections::BTreeMap, sync::Weak};

const MAX_READY: usize = 4096;

#[derive(Debug)]
struct Ready {
    capture: WeakHeldAttempt,
    future: Weak<()>,
    order: u64,
}

#[derive(Debug, Default)]
pub(super) struct ReconsiderationOrder {
    ready: Vec<Ready>,
    selected: BTreeMap<BudgetRootId, Weak<()>>,
    next: u64,
}

impl ReconsiderationOrder {
    fn next(&mut self) -> Result<u64> {
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| MezError::invalid_state("budget reconsideration order exhausted"))?;
        Ok(self.next)
    }
}

impl RuntimeSessionService {
    /// Exact claim eligibility, not native permission. Paused/dead/retired heads
    /// cannot strand another family's original waiter.
    fn budget_reconsideration_eligible(&self, capture: &RuntimeBudgetHeldAttempt) -> Result<bool> {
        let (pane, quiescent) = match capture.auxiliary_claim() {
            Some(RuntimeBudgetAuxiliaryClaim::Title { .. }) => {
                let (conversation, pane) = self.budget_title_subject(capture)?;
                (pane, self.budget_title_claim_is_quiescent(&conversation))
            }
            Some(RuntimeBudgetAuxiliaryClaim::Remember { .. }) => {
                let pane = self.budget_remember_subject(capture)?;
                let quiescent = self.budget_remember_claim_is_quiescent(&pane);
                (pane, quiescent)
            }
            Some(RuntimeBudgetAuxiliaryClaim::Compaction { .. }) => {
                let (pane, generation) = self.budget_compaction_subject(capture)?;
                let quiescent = self.budget_compaction_claim_is_quiescent(&pane, generation);
                (pane, quiescent)
            }
            None => {
                let (agent, turn, generation) = capture.ordinary_subject().ok_or_else(|| {
                    MezError::conflict("original reconsideration subject unavailable")
                })?;
                let agent = mez_core::ids::AgentId::opaque(agent.to_owned()).ok_or_else(|| {
                    MezError::conflict("original reconsideration agent unavailable")
                })?;
                let record = self.budget_wait_turn(&agent, turn, generation, capture)?;
                (
                    record.pane_id,
                    self.budget_provider_claim_is_quiescent(turn),
                )
            }
        };
        Ok(quiescent && !self.agent_is_human_paused(&pane))
    }

    /// Call only after a descriptive native observation permits reconsidering
    /// a finite root. A selected window is short-lived caller ownership, NOT a
    /// native grant. Dropping it after a refused family resume lets another
    /// fitting original proceed; selection rotates that candidate to the back.
    pub(crate) fn select_budget_reconsideration(
        &mut self,
        capture: &RuntimeBudgetHeldAttempt,
        future: Weak<()>,
        window: Weak<()>,
    ) -> Result<bool> {
        if future.upgrade().is_none() || window.upgrade().is_none() {
            return Err(MezError::conflict(
                "original reconsideration future retired",
            ));
        }
        let eligible = self.budget_reconsideration_eligible(capture)?;
        let pending = std::mem::take(&mut self.agent.budget_memberships.reconsideration.ready);
        let ready = pending
            .into_iter()
            .filter(|entry| {
                entry.future.upgrade().is_some()
                    && entry.capture.upgrade().is_some_and(|held| {
                        self.budget_reconsideration_eligible(&held).unwrap_or(false)
                    })
            })
            .collect();
        let ordering = &mut self.agent.budget_memberships.reconsideration;
        ordering.ready = ready;
        ordering
            .selected
            .retain(|_, owner| owner.upgrade().is_some());
        if !eligible {
            return Ok(false);
        }
        let root = capture.root().clone();
        let existing = ordering.ready.iter().position(|entry| {
            entry
                .capture
                .upgrade()
                .is_some_and(|held| held.same_capture(capture))
        });
        let index = match existing {
            Some(index) => {
                if !ordering.ready[index].future.ptr_eq(&future) {
                    return Err(MezError::conflict(
                        "original reconsideration waiter changed",
                    ));
                }
                index
            }
            None => {
                if ordering.ready.len() >= MAX_READY {
                    return Ok(false);
                }
                let order = ordering.next()?;
                ordering.ready.push(Ready {
                    capture: capture.downgrade(),
                    future,
                    order,
                });
                ordering.ready.len() - 1
            }
        };
        if let Some(selected) = ordering.selected.get(&root) {
            return Ok(selected.ptr_eq(&window));
        }
        let oldest = ordering
            .ready
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                entry
                    .capture
                    .upgrade()
                    .is_some_and(|held| held.root() == &root)
            })
            .min_by_key(|(_, entry)| entry.order)
            .map(|(i, _)| i);
        if oldest != Some(index) || ordering.selected.len() >= MAX_READY {
            return Ok(false);
        }
        let order = ordering.next()?;
        ordering.ready[index].order = order;
        ordering.selected.insert(root, window);
        Ok(true)
    }
}
