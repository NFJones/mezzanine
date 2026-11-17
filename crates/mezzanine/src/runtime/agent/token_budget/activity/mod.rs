//! Native-binding-qualified root inhibition at the existing actor/worker boundary.
//!
//! One cancellable read per root observes coherent policy and known expense off
//! actor. Sealed outcomes can update only their original live membership capture;
//! stale reads never transfer to a replacement conversation or repository. This
//! descriptive projection is NOT a grant. The native_claim leaf separately
//! claims one exact shell or approved-external offer inside the accounting writer order;
//! actor transfer still checks original permission/ownership boundaries.
//! Other effect families, production assembly and automatic release remain staged.

use super::{BudgetMembership, RuntimeSessionService};
use crate::error::{MezError, Result};
use crate::storage::token_usage::budget::admission::BudgetAdmissionWaitState;
use std::sync::{Arc, Weak};

mod native_claim;
pub(crate) use native_claim::{
    RuntimeBudgetEffectClaim, RuntimeBudgetFilesystemClaimPreparation, RuntimeBudgetNativeClaim,
    RuntimeBudgetNativeClaimPreparation, RuntimeBudgetNativeClaimWork,
};

pub(super) type RootKey = (String, String);

#[derive(Debug)]
pub(super) struct CaptureFailure {
    identity: Weak<()>,
    // Original cause is retained locally, never sent through shared service
    // failure or rendered with private path/payload details.
    _cause: MezError,
}

/// Only Disabled permits the unchanged non-budget path. Refused withholds this
/// claim without converting a root-local qualification failure into service loss.
#[derive(Debug)]
pub(crate) enum RuntimeBudgetActivityPreparation {
    Disabled,
    Refused,
    Read(Box<RuntimeBudgetActivityWork>),
}

/// Weak exact-query witness for a joined reader failure, not read/send authority.
#[derive(Debug)]
pub(crate) struct RuntimeBudgetActivityFailure {
    conversation: String,
    root: RootKey,
    identity: Weak<()>,
    token: Weak<()>,
}

#[derive(Debug, Default)]
pub(super) struct RootActivity {
    pending: Option<Weak<()>>,
    observed: Option<BudgetAdmissionWaitState>,
    failure: Option<MezError>,
}

impl RootActivity {
    fn exhausted(&self) -> bool {
        self.observed.as_ref().is_some_and(|state| {
            state.limits.is_some_and(|limits| {
                limits.input.exhausted(state.known.input)
                    || limits.output.exhausted(state.known.output)
            })
        })
    }

    fn operationally_inhibited(&self) -> bool {
        self.pending.is_some()
            || self.failure.is_some()
            || self
                .observed
                .as_ref()
                .is_none_or(|state| state.limits.is_none())
    }
}

fn key(member: &BudgetMembership) -> RootKey {
    (member.namespace.clone(), member.root.as_str().to_owned())
}

/// Owned native read, not serializable/cloneable policy or effect authority.
/// Its strong token bounds one outstanding query; dropping a borrowed observer
/// cannot let another query replace a still-running original read.
#[derive(Debug)]
pub(crate) struct RuntimeBudgetActivityWork {
    owner: super::request::RuntimeBudgetRequestOwner,
    turn: mez_agent::AgentTurnRecord,
    identity: Weak<()>,
    token: Arc<()>,
    #[cfg(test)]
    panic_reader: bool,
}

#[derive(Debug)]
pub(crate) struct RuntimeBudgetActivityRead {
    work: Box<RuntimeBudgetActivityWork>,
    observed: Result<BudgetAdmissionWaitState>,
}

impl RuntimeBudgetActivityWork {
    pub(crate) fn failure_capture(&self) -> RuntimeBudgetActivityFailure {
        RuntimeBudgetActivityFailure {
            conversation: self.turn.conversation_id.clone(),
            root: key(&self.owner.member),
            identity: self.identity.clone(),
            token: Arc::downgrade(&self.token),
        }
    }

    /// Blocking-worker only. No policy mutation, receipt, attempt or effect is
    /// created. The original kernel lease remains owned through the read.
    #[cfg(test)]
    pub(crate) fn execute(self: Box<Self>) -> Box<RuntimeBudgetActivityRead> {
        #[cfg(test)]
        if self.panic_reader {
            panic!("private budget activity reader fixture panic");
        }
        let observed = self.owner.store.budget_admission_wait_state(
            &self.owner.member,
            &self.owner.binding_owner,
            &self.owner.lease,
        );
        Box::new(RuntimeBudgetActivityRead {
            work: self,
            observed,
        })
    }
}

impl RuntimeSessionService {
    #[cfg(test)]
    pub(crate) fn inject_budget_activity_fault(&mut self, turn: String, panic_reader: bool) {
        self.agent.budget_memberships.activity_fault = Some((turn, panic_reader));
    }

    #[cfg(test)]
    pub(crate) fn budget_activity_fault_pending_for_tests(&self) -> bool {
        self.agent.budget_memberships.activity_fault.is_some()
    }

    /// Effect-claim ingress contains ONLY errors from this local capture owner.
    /// Transport/actor errors are outside this result and remain independent.
    pub(crate) fn prepare_budget_activity_claim(
        &mut self,
        turn_id: &str,
    ) -> RuntimeBudgetActivityPreparation {
        if !self.agent.budget_memberships.enabled {
            return RuntimeBudgetActivityPreparation::Disabled;
        }
        match self.prepare_budget_activity_read(turn_id) {
            Ok(Some(work)) => RuntimeBudgetActivityPreparation::Read(work),
            Ok(None) => RuntimeBudgetActivityPreparation::Refused,
            Err(error) => {
                if let Some(turn) = self.agent_turn_ledger().turn(turn_id)
                    && let Some(entry) = self
                        .agent
                        .budget_memberships
                        .entries
                        .get(&turn.conversation_id)
                {
                    let conversation = turn.conversation_id.clone();
                    let identity = Arc::downgrade(&entry.identity);
                    let budgets = &mut self.agent.budget_memberships;
                    budgets.activity_capture_failures.retain(|cid, failure| {
                        budgets.entries.get(cid).is_some_and(|entry| {
                            failure.identity.ptr_eq(&Arc::downgrade(&entry.identity))
                        })
                    });
                    budgets.activity_capture_failures.insert(
                        conversation,
                        CaptureFailure {
                            identity,
                            _cause: error,
                        },
                    );
                }
                RuntimeBudgetActivityPreparation::Refused
            }
        }
    }

    /// A reader JoinError retires no effect and grants no retry. Install the
    /// operational refusal only for its original still-current query, without
    /// allowing an old panic callback to overwrite a newer successful read.
    pub(crate) fn fail_budget_activity_read(
        &mut self,
        failure: RuntimeBudgetActivityFailure,
    ) -> bool {
        let budgets = &mut self.agent.budget_memberships;
        if budgets
            .entries
            .get(&failure.conversation)
            .is_none_or(|entry| !failure.identity.ptr_eq(&Arc::downgrade(&entry.identity)))
        {
            return false;
        }
        let Some(state) = budgets.activity.get_mut(&failure.root) else {
            return false;
        };
        if state
            .pending
            .as_ref()
            .is_none_or(|pending| !pending.ptr_eq(&failure.token))
        {
            return false;
        }
        state.pending = None;
        state.failure = Some(MezError::invalid_state(
            "budget activity reader unavailable",
        ));
        true
    }

    /// Captures an existing live account only; this cannot allocate membership.
    /// None coalesces an outstanding original query or preserves disabled
    /// assembly. A cancelled query leaves inhibition until a fresh read settles.
    pub(crate) fn prepare_budget_activity_read(
        &mut self,
        turn_id: &str,
    ) -> Result<Option<Box<RuntimeBudgetActivityWork>>> {
        if !self.agent.budget_memberships.enabled {
            return Ok(None);
        }
        #[cfg(test)]
        if self
            .agent
            .budget_memberships
            .activity_fault
            .as_ref()
            .is_some_and(|(turn, panic)| turn == turn_id && !panic)
        {
            self.agent.budget_memberships.activity_fault = None;
            return Err(MezError::invalid_state(
                "injected budget membership capture unavailable",
            ));
        }
        let Some(turn) = self.agent_turn_ledger().turn(turn_id).cloned() else {
            return Ok(None);
        };
        if self
            .agent_shell_store()
            .get(&turn.pane_id)
            .is_none_or(|session| session.session_id != turn.conversation_id)
        {
            return Ok(None);
        }
        let Some(owner) = self.runtime_budget_request_owner(&turn)? else {
            return Ok(None);
        };
        let identity =
            Arc::downgrade(&self.agent.budget_memberships.entries[&turn.conversation_id].identity);
        let root = key(&owner.member);
        if !self.agent.budget_memberships.activity.contains_key(&root)
            && self.agent.budget_memberships.activity.len()
                >= super::RUNTIME_BUDGET_MEMBERSHIP_LIMIT
        {
            return Err(MezError::invalid_state(
                "budget activity capture capacity unavailable",
            ));
        }
        let state = self
            .agent
            .budget_memberships
            .activity
            .entry(root)
            .or_default();
        if state.pending.as_ref().and_then(Weak::upgrade).is_some() {
            return Ok(None);
        }
        let token = Arc::new(());
        state.pending = Some(Arc::downgrade(&token));
        #[cfg(test)]
        let panic_reader = self
            .agent
            .budget_memberships
            .activity_fault
            .take_if(|(turn, panic)| turn == turn_id && *panic)
            .is_some();
        Ok(Some(Box::new(RuntimeBudgetActivityWork {
            owner,
            turn,
            identity,
            token,
            #[cfg(test)]
            panic_reader,
        })))
    }

    /// Adopts only the original query under its current native capture. Neither
    /// success nor WithinKnownLimits is effect permission. Errors retain their
    /// exact cause and a separate operational hold, never fabricated zero usage.
    pub(crate) fn apply_budget_activity_read(&mut self, read: RuntimeBudgetActivityRead) -> bool {
        let RuntimeBudgetActivityRead { work, observed } = read;
        let Some(entry) = self
            .agent
            .budget_memberships
            .entries
            .get(&work.turn.conversation_id)
        else {
            return false;
        };
        if !work.identity.ptr_eq(&Arc::downgrade(&entry.identity))
            || self
                .runtime_budget_request_owner(&work.turn)
                .ok()
                .flatten()
                .is_none_or(|current| {
                    current.member != work.owner.member
                        || current.binding_owner != work.owner.binding_owner
                        || current.store != work.owner.store
                })
        {
            return false;
        }
        let root = key(&work.owner.member);
        let Some(state) = self.agent.budget_memberships.activity.get_mut(&root) else {
            return false;
        };
        if state
            .pending
            .as_ref()
            .is_none_or(|pending| !pending.ptr_eq(&Arc::downgrade(&work.token)))
        {
            return false;
        }
        state.pending = None;
        match observed {
            Ok(current) => {
                if state.observed.as_ref().is_some_and(|old| {
                    current.revisions.projection < old.revisions.projection
                        || current.revisions.root_policy < old.revisions.root_policy
                        || current.revisions.accounting < old.revisions.accounting
                        || current.known.input < old.known.input
                        || current.known.output < old.known.output
                }) {
                    state.failure =
                        Some(MezError::conflict("budget activity observation regressed"));
                } else {
                    state.observed = Some(current);
                    state.failure = None;
                    self.agent
                        .budget_memberships
                        .activity_capture_failures
                        .remove(&work.turn.conversation_id);
                }
            }
            Err(error) => state.failure = Some(error),
        }
        let exhausted = state.exhausted();
        let turns = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .filter(|turn| self.budget_activity_key_for_turn(&turn.turn_id).as_ref() == Some(&root))
            .map(|turn| turn.turn_id.clone())
            .collect::<Vec<_>>();
        for turn in turns {
            self.agent
                .agent_scheduler
                .set_budget_exhausted(&turn, exhausted);
        }
        true
    }

    fn budget_activity_key_for_turn(&self, turn_id: &str) -> Option<RootKey> {
        let turn = self.agent_turn_ledger().turn(turn_id)?;
        let entry = self
            .agent
            .budget_memberships
            .entries
            .get(&turn.conversation_id)?;
        if let Some(member) = &entry.member {
            return Some(key(member));
        }
        // A genuine delegated constructor freezes the activated parent before
        // the child becomes runnable. Depth/labels/current pane cannot supply it.
        match &entry.source {
            super::Source::Inherited { parent, .. } => Some(key(parent)),
            _ => None,
        }
    }

    /// Local deny-only projection. Absence leaves staged callers unchanged;
    /// installed pending/unavailable roots are distinct from numeric exhaustion.
    pub(crate) fn budget_activity_inhibits_turn(&self, turn_id: &str) -> bool {
        self.budget_activity_operationally_inhibits_turn(turn_id)
            || self
                .budget_activity_key_for_turn(turn_id)
                .and_then(|key| self.agent.budget_memberships.activity.get(&key))
                .is_some_and(RootActivity::exhausted)
    }

    /// A native claim may predate a numeric crossing, never an unresolved
    /// membership/read/integrity refusal. Adoption acknowledgment is not proof
    /// that these independently retained operational gates were cleared.
    pub(in crate::runtime::agent) fn budget_activity_operationally_inhibits_turn(
        &self,
        turn_id: &str,
    ) -> bool {
        if let Some(turn) = self.agent_turn_ledger().turn(turn_id)
            && let Some(failure) = self
                .agent
                .budget_memberships
                .activity_capture_failures
                .get(&turn.conversation_id)
            && self
                .agent
                .budget_memberships
                .entries
                .get(&turn.conversation_id)
                .is_some_and(|entry| failure.identity.ptr_eq(&Arc::downgrade(&entry.identity)))
        {
            return true;
        }
        self.budget_activity_key_for_turn(turn_id)
            .and_then(|key| self.agent.budget_memberships.activity.get(&key))
            .is_some_and(RootActivity::operationally_inhibited)
    }
}
