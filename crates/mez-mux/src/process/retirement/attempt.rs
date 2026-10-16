//! Purpose-scoped cleanup authority for exact already-owned native children.
//!
//! Initial registration is identity only: it never globalizes a pane-local
//! deadline. Only a single-use explicitly prepared retry can advance a sealed,
//! quiescent owner after every remaining real initial authority has expired.
//! Tokens capture immutable budgets and source/attempt identities. They live
//! outside returned raw resources, so no owner/slot/guard cycle is introduced.

use super::{ChildSlot, Instant, MezError, NativeShutdownPlan, PaneProcessRetirementOwner, Result};
use std::fmt;

/// Current per-source registration or explicitly admitted retry budget.
#[derive(Debug, Clone, Copy)]
pub(super) struct CleanupAttemptState {
    /// Zero registers initial identity without introducing a shared budget.
    pub(super) epoch: u64,
    /// Only a genuinely new explicit retry installs an owner-wide budget.
    pub(super) deadline: Option<Instant>,
    /// Explicit retry upgrades force monotonically, never resets grace.
    pub(super) force: bool,
}

/// Scalar authority carried by a claim, never an Arc inside returned slots.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct CleanupBinding {
    pub(super) source: u64,
    pub(super) epoch: u64,
    pub(super) deadline: Option<Instant>,
    pub(super) force: bool,
    /// Creation time of this operation, used only for a first local plan.
    pub(super) started: Option<Instant>,
}

/// Immutable continuation authority for one owner/source/attempt and budget.
/// Fields cannot be forged or changed; cloning never extends the deadline.
#[derive(Clone)]
pub struct PaneProcessCleanupAttempt {
    pub(super) owner: PaneProcessRetirementOwner,
    pub(super) binding: CleanupBinding,
    deadline: Instant,
}

/// Read-only preparation captured synchronously before an explicit future is
/// polled. Admission is single-use and must follow caller runtime validation.
///
/// Long-lived runtimes use a separate identity-only capture so a later genuine
/// initial shutdown does not start its finite clock at session startup.
pub struct PreparedPaneProcessCleanupRetry {
    owner: PaneProcessRetirementOwner,
    source: u64,
    observed: Option<u64>,
    deadline: Instant,
    started: Instant,
}

impl PaneProcessCleanupAttempt {
    /// Binds a scope captured before any native admission to this owner's FIRST
    /// initial source only. Source0-to1 is not idle reuse; later sources and all
    /// retry epochs remain stale. Budget/start are copied, never recomputed.
    pub fn bind_first_source(&self, owner: &PaneProcessRetirementOwner) -> Result<Self> {
        if !std::sync::Arc::ptr_eq(&self.owner.inner, &owner.inner) {
            return Err(MezError::invalid_state(
                "native cleanup attempt belongs to another owner",
            ));
        }
        let mut state = owner.state();
        let epoch = state.attempt.map_or(0, |attempt| attempt.epoch);
        if self.binding.source == state.source_epoch && self.binding.epoch == epoch {
            return Ok(self.clone());
        }
        if self.binding.source != 0
            || self.binding.epoch != 0
            || state.source_epoch != 1
            || epoch != 0
        {
            return Err(MezError::invalid_state("native cleanup attempt is stale"));
        }
        let occupied = state.slots.iter().any(Option::is_some);
        if occupied && self.deadline <= Instant::now() {
            return Err(MezError::invalid_state(
                "native cleanup initial source budget expired",
            ));
        }
        if occupied && state.attempt.is_none() {
            state.attempt = Some(CleanupAttemptState {
                epoch: 0,
                deadline: None,
                force: false,
            });
        }
        let mut bound = self.clone();
        bound.binding.source = 1;
        Ok(bound)
    }

    /// Returns the same identity without changing its captured effect budget.
    pub fn identity(&self) -> PaneProcessCleanupIdentity {
        PaneProcessCleanupIdentity {
            owner: self.owner.clone(),
            source: self.binding.source,
            epoch: self.binding.epoch,
        }
    }

    /// Returns the captured finite deadline without reading a newer attempt.
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Validates effect admission, not merely status observation. This never
    /// changes the captured budget or any native bookkeeping.
    pub fn validate_effects(&self, owner: &PaneProcessRetirementOwner) -> Result<()> {
        self.validate(owner)?;
        if Instant::now() >= self.deadline {
            return Err(MezError::invalid_state(
                "native cleanup effect deadline expired",
            ));
        }
        Ok(())
    }

    /// Verifies exact owner/source/attempt identity before a claim or quantum.
    /// A stale or foreign token never changes resources or progress.
    pub fn validate(&self, owner: &PaneProcessRetirementOwner) -> Result<()> {
        if !std::sync::Arc::ptr_eq(&self.owner.inner, &owner.inner) {
            return Err(MezError::invalid_state(
                "native cleanup attempt belongs to another owner",
            ));
        }
        let state = owner.state();
        if self.binding.source != state.source_epoch
            || self.binding.epoch != state.attempt.map_or(0, |attempt| attempt.epoch)
        {
            return Err(MezError::invalid_state("native cleanup attempt is stale"));
        }
        Ok(())
    }
}

/// Read-only source/attempt identity for same-generation host admission checks.
/// It cannot bind another budget or admit a retry epoch.
#[derive(Clone)]
pub struct PaneProcessCleanupIdentity {
    owner: PaneProcessRetirementOwner,
    source: u64,
    epoch: u64,
}

impl fmt::Debug for PaneProcessCleanupAttempt {
    /// Retained owners/handles and diagnostics are deliberately opaque.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PaneProcessCleanupAttempt")
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for PreparedPaneProcessCleanupRetry {
    /// Preparation does not reveal its retained owner or private native state.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedPaneProcessCleanupRetry")
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for PaneProcessCleanupIdentity {
    /// Identity formatting never traverses native ownership internals.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PaneProcessCleanupIdentity")
            .finish_non_exhaustive()
    }
}

impl PaneProcessRetirementOwner {
    /// Atomically seals and captures a genuine initial runtime shutdown after
    /// long-lived services settle. This cannot adopt any explicit retry epoch.
    /// Existing child-local plans still cap initial0 effects; zero resources
    /// need no effect token. No fallback clock is used as prior-expiry evidence.
    pub fn initial_shutdown_attempt(
        &self,
        force: bool,
    ) -> Result<Option<PaneProcessCleanupAttempt>> {
        let mut state = self.state();
        let occupied = state.slots.iter().any(Option::is_some);
        if occupied && state.attempt.is_some_and(|attempt| attempt.epoch != 0) {
            return Err(MezError::invalid_state(
                "initial native shutdown cannot adopt explicit retry authority",
            ));
        }
        state.sealed = true;
        if !occupied {
            return Ok(None);
        }
        if state.attempt.is_none() {
            state.attempt = Some(CleanupAttemptState {
                epoch: 0,
                deadline: None,
                force: false,
            });
        }
        let started = Instant::now();
        let plan = state.shutdown.get_or_insert(NativeShutdownPlan {
            started,
            deadline: started + std::time::Duration::from_secs(if force { 1 } else { 2 }),
            force,
        });
        plan.force |= force;
        let plan = *plan;
        let binding = CleanupBinding {
            source: state.source_epoch,
            epoch: 0,
            deadline: Some(plan.deadline),
            force: plan.force,
            started: Some(plan.started),
        };
        Ok(Some(PaneProcessCleanupAttempt {
            owner: self.clone(),
            binding,
            deadline: plan.deadline,
        }))
    }

    /// Captures an initial/continuation operation synchronously. Registration
    /// changes None to initial0 but creates no shared initial deadline. Each
    /// raw child's existing plan still caps its effects independently.
    pub fn capture_cleanup_attempt(&self, deadline: Instant) -> Result<PaneProcessCleanupAttempt> {
        let mut state = self.state();
        let occupied = state.slots.iter().any(Option::is_some);
        if occupied && deadline <= Instant::now() {
            return Err(MezError::invalid_state(
                "native cleanup operation deadline expired",
            ));
        }
        if occupied && state.attempt.is_none() {
            state.attempt = Some(CleanupAttemptState {
                epoch: 0,
                deadline: None,
                force: false,
            });
        }
        let attempt = state.attempt;
        let deadline = attempt
            .and_then(|attempt| attempt.deadline)
            .map_or(deadline, |current| current.min(deadline));
        Ok(PaneProcessCleanupAttempt {
            owner: self.clone(),
            deadline,
            binding: CleanupBinding {
                source: state.source_epoch,
                epoch: attempt.map_or(0, |attempt| attempt.epoch),
                deadline: Some(deadline),
                force: attempt.is_some_and(|attempt| attempt.force),
                started: Some(Instant::now()),
            },
        })
    }

    /// Captures only initial authority for a generation admitted after a host
    /// scope was created. A late retry epoch cannot be silently adopted.
    pub fn capture_initial_cleanup_attempt(
        &self,
        deadline: Instant,
    ) -> Result<PaneProcessCleanupAttempt> {
        let state = self.state();
        if state.attempt.is_some_and(|attempt| attempt.epoch != 0) {
            return Err(MezError::invalid_state(
                "late native owner already has retry authority",
            ));
        }
        drop(state);
        let token = self.capture_cleanup_attempt(deadline)?;
        if token.binding.epoch != 0 {
            return Err(MezError::invalid_state(
                "late native owner retry authority changed",
            ));
        }
        Ok(token)
    }

    /// Prepares an explicit retry without sealing, minting or native effects.
    /// Capture uses std Instant and is valid outside an entered Tokio runtime.
    pub fn prepare_explicit_cleanup_retry(
        &self,
        deadline: Instant,
    ) -> Result<PreparedPaneProcessCleanupRetry> {
        if deadline <= Instant::now() {
            return Err(MezError::invalid_args(
                "explicit native retry deadline must be in the future",
            ));
        }
        let state = self.state();
        Ok(PreparedPaneProcessCleanupRetry {
            owner: self.clone(),
            source: state.source_epoch,
            observed: state.attempt.map(|attempt| attempt.epoch),
            deadline,
            started: Instant::now(),
        })
    }
}

impl PaneProcessCleanupIdentity {
    /// Validates identity only, without registering or binding a new clock.
    pub fn validate(&self, owner: &PaneProcessRetirementOwner) -> Result<()> {
        if !std::sync::Arc::ptr_eq(&self.owner.inner, &owner.inner) {
            return Err(MezError::invalid_state(
                "native cleanup identity belongs to another owner",
            ));
        }
        let state = owner.state();
        if state.source_epoch != self.source
            || state.attempt.map_or(0, |attempt| attempt.epoch) != self.epoch
        {
            return Err(MezError::invalid_state("native cleanup identity is stale"));
        }
        Ok(())
    }
}

impl PreparedPaneProcessCleanupRetry {
    /// Checks exact preparation identity before an adapter cancels a captured
    /// runtime. This never seals, mints, changes progress or takes resources.
    pub fn validate(&self, owner: &PaneProcessRetirementOwner) -> Result<()> {
        if !std::sync::Arc::ptr_eq(&self.owner.inner, &owner.inner) {
            return Err(MezError::invalid_state(
                "prepared native retry belongs to another owner",
            ));
        }
        let state = owner.state();
        if state.source_epoch != self.source
            || state.attempt.map(|attempt| attempt.epoch) != self.observed
        {
            return Err(MezError::invalid_state("prepared native retry is stale"));
        }
        Ok(())
    }

    /// Atomically admits this genuine explicit caller's finite retry. All
    /// rejection checks precede mutation. No-plan/live/mixed initial children
    /// continue initial0 with their original caps; only all real expired plans
    /// authorize a distinct retry epoch. Vacant sparse slots are harmless.
    pub fn admit(self) -> Result<PaneProcessCleanupAttempt> {
        let mut state = self.owner.state();
        if state.source_epoch != self.source
            || state.attempt.map(|attempt| attempt.epoch) != self.observed
        {
            return Err(MezError::invalid_state("prepared native retry is stale"));
        }
        if self.deadline <= Instant::now() {
            return Err(MezError::invalid_args(
                "prepared native retry deadline expired",
            ));
        }
        if !state.sealed {
            return Err(MezError::invalid_state(
                "explicit native retry requires sealed admission",
            ));
        }
        if state.draining
            || state.slots.iter().any(|slot| {
                matches!(
                    slot,
                    Some(ChildSlot::Reserved | ChildSlot::Leased | ChildSlot::Draining)
                )
            })
        {
            return Err(MezError::invalid_state(
                "explicit native retry is busy with leased native ownership",
            ));
        }
        let mut any = false;
        let now = Instant::now();
        let retry_budget = state
            .attempt
            .filter(|attempt| attempt.epoch != 0)
            .and_then(|attempt| attempt.deadline);
        let all_expired = state.slots.iter().flatten().all(|slot| {
            let ChildSlot::Returned(raw) = slot else {
                return false;
            };
            any = true;
            let prior = retry_budget.or_else(|| {
                let local = raw.progress.plan.map(|plan| plan.deadline);
                let shared = state.shutdown.map(|plan| plan.deadline);
                match (local, shared, raw.binding.deadline) {
                    (None, None, _) => None,
                    (local, shared, bound) => [local, shared, bound].into_iter().flatten().min(),
                }
            });
            prior.is_some_and(|deadline| deadline <= now)
        });
        if self.deadline <= Instant::now() {
            return Err(MezError::invalid_args(
                "prepared native retry deadline expired during admission",
            ));
        }
        let old_epoch = state.attempt.map_or(0, |attempt| attempt.epoch);
        let epoch = if any && all_expired {
            old_epoch.checked_add(1).ok_or_else(|| {
                MezError::invalid_state("native cleanup attempt generation exhausted")
            })?
        } else {
            old_epoch
        };
        let mut current = state.attempt;
        if any && all_expired {
            current = Some(CleanupAttemptState {
                epoch,
                deadline: Some(self.deadline),
                force: true,
            });
        } else if any && current.is_none() {
            current = Some(CleanupAttemptState {
                epoch: 0,
                deadline: None,
                force: false,
            });
        }
        let deadline = current
            .and_then(|attempt| attempt.deadline)
            .map_or(self.deadline, |deadline| deadline.min(self.deadline));
        let binding = CleanupBinding {
            source: self.source,
            epoch,
            deadline: Some(deadline),
            force: current.is_some_and(|attempt| attempt.force),
            started: Some(self.started),
        };
        state.attempt = current;
        drop(state);
        Ok(PaneProcessCleanupAttempt {
            owner: self.owner,
            binding,
            deadline,
        })
    }
}
