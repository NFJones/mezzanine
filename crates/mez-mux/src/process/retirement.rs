//! Exact native-child ownership across manager, adapter and cleanup lifetimes.
//!
//! Each managed spawn reserves one return slot before creating a child. The
//! child guard, not an async task, carries that reservation through handoffs.
//! Dropping an unreaped guard fills its existing slot without allocating or
//! spawning work. A live session owner can subsequently claim and settle that
//! same child, including after actor closure or cancellation before first poll.
//!
//! Slots are cardinality-bounded by admitted unreaped children, not a new pane
//! limit. The first returned/unproven child fences new managed admission until
//! cleanup settles; existing children keep their reserved slots and I/O. Sealing
//! is monotonic and distinct from that recoverable admission fence. No completed
//! child history is retained. Native wait success alone releases a slot.
//!
//! Numeric signal authority is the immutable birth group minted at proven
//! native UNIX spawn boundaries, not cached/live PTY foreground metadata.
//! An unreaped exact std Child pins that root PID/group identity; every send
//! checks that binding again. Unknown child types fail closed. Root force uses
//! inherent std Child::kill, never portable-pty's sleeping HUP implementation.

use std::io;
use std::io::Write;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use portable_pty::{Child, ExitStatus};
use rustix::process::Signal;

use super::signals::send_signal_to_retained_pane_group as send_signal_to_pane_process_group;
use crate::{MuxError as MezError, Result};

mod attempt;
use attempt::{CleanupAttemptState, CleanupBinding};
pub use attempt::{
    PaneProcessCleanupAttempt, PaneProcessCleanupIdentity, PreparedPaneProcessCleanupRetry,
};

/// Session-scoped owner retained independently of abortable pane workers.
#[derive(Clone, Debug, Default)]
pub struct PaneProcessRetirementOwner {
    inner: Arc<Mutex<RetirementState>>,
}

/// Preallocated slots contain raw resources only, never a guard/owner cycle.
#[derive(Debug, Default)]
struct RetirementState {
    slots: Vec<Option<ChildSlot>>,
    /// Bounded source-incarnation fence; no completed child history is kept.
    source_epoch: u64,
    /// Initial registration has no shared budget; retry budgets are explicit.
    attempt: Option<CleanupAttemptState>,
    sealed: bool,
    draining: bool,
    drain_cursor: usize,
    shutdown: Option<NativeShutdownPlan>,
    failed_children: usize,
    first_failure: Option<String>,
}

/// One original absolute budget; a force upgrade never restarts its clock.
#[derive(Debug, Clone, Copy)]
struct NativeShutdownPlan {
    started: Instant,
    deadline: Instant,
    force: bool,
}

/// Native signal progress travels with raw ownership through cancellation.
#[derive(Debug, Default)]
struct NativeTerminationProgress {
    plan: Option<NativeShutdownPlan>,
    hup_sent: bool,
    term_sent: bool,
}

/// One admitted obligation, including reservations and not-yet-polled workers.
#[derive(Debug)]
enum ChildSlot {
    Reserved,
    Leased,
    Returned(RawPaneChild),
    Draining,
}

/// Native resources sufficient for exact root/group signaling and reaping.
#[derive(Debug)]
struct RawPaneChild {
    child: Box<dyn Child + Send + Sync>,
    /// Captured scalar authority, never a reference back into the owner slots.
    binding: CleanupBinding,
    /// Boundary-minted birth-root PGID; immutable throughout production cleanup.
    group: Option<i32>,
    /// Successful root signal is not a wait receipt, but avoids repeat signals.
    force_requested: bool,
    progress: NativeTerminationProgress,
    failure_latched: bool,
}

impl RawPaneChild {
    /// Effective effect budget. Initial continuations keep their own raw plan
    /// cap; only a validated explicit retry binding can replace that cap.
    fn signal_deadline(&self) -> Option<Instant> {
        if self.binding.epoch != 0 {
            return self.binding.deadline;
        }
        [
            self.binding.deadline,
            self.progress.plan.map(|plan| plan.deadline),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Fences new effects after the retained deadline, including direct signal
    /// methods and final-owner best effort. Status observation remains allowed.
    fn signal_budget_remaining(&self) -> bool {
        (self.binding.epoch == 0 || self.binding.deadline.is_some())
            && (self.binding.deadline.is_none()
                || self.binding.started.is_some()
                || self.progress.plan.is_some())
            && self
                .signal_deadline()
                .is_none_or(|deadline| Instant::now() < deadline)
    }

    /// Checks birth authority against the still-owned native child, not a PID
    /// lookup. Callers must first poll the exact child and suppress every send
    /// after a successful wait. Missing/invalid/mismatched authority is typed
    /// failure and must not issue a numeric group signal.
    fn signal_group(&mut self, signal: Signal) -> Result<()> {
        if !self.signal_budget_remaining() {
            return Err(MezError::invalid_state(
                "native termination deadline expired; exact ownership retained",
            ));
        }
        let native = self
            .child
            .as_ref()
            .as_any()
            .downcast_ref::<std::process::Child>()
            .ok_or_else(|| {
                MezError::invalid_state("native group authority has an unsupported child type")
            })?;
        let root = i32::try_from(native.id())
            .ok()
            .filter(|pid| *pid > 0)
            .ok_or_else(|| {
                MezError::invalid_state("native root pid is invalid for group authority")
            })?;
        let group = self.group.filter(|group| *group > 0).ok_or_else(|| {
            MezError::invalid_state("native birth process group authority is missing or invalid")
        })?;
        if group != root {
            return Err(MezError::invalid_state(
                "native birth process group authority does not match its owned root",
            ));
        }
        send_signal_to_pane_process_group(group, signal)
    }

    /// Issues only inherent nonblocking exact-root KILL for the known native
    /// type. Opaque implementations retain unproven ownership; invoking their
    /// portable killer could sleep, reap internally or have unknown semantics.
    fn kill_root(&mut self) -> io::Result<()> {
        if !self.signal_budget_remaining() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "native root signal deadline expired",
            ));
        }
        let native = self
            .child
            .as_mut()
            .as_any_mut()
            .downcast_mut::<std::process::Child>()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    "native root force requires the owned std::process::Child",
                )
            })?;
        std::process::Child::kill(native)
    }
}

impl Drop for RetirementState {
    /// Last-owner destruction is best effort, never a retirement receipt.
    fn drop(&mut self) {
        if self.slots.iter().any(Option::is_some) {
            let _ = writeln!(
                io::stderr().lock(),
                "mezzanine: native child retirement remains unproven at final owner destruction"
            );
        }
        let source = self.source_epoch;
        let epoch = self.attempt.map_or(0, |attempt| attempt.epoch);
        let shared = self.shutdown;
        for slot in &mut self.slots {
            if let Some(ChildSlot::Returned(raw)) = slot {
                if raw.binding.source != source || raw.binding.epoch != epoch {
                    continue;
                }
                if epoch == 0
                    && let Some(shared) = shared
                {
                    match &mut raw.progress.plan {
                        Some(plan) => {
                            plan.started = plan.started.min(shared.started);
                            plan.deadline = plan.deadline.min(shared.deadline);
                            plan.force |= shared.force;
                        }
                        None => raw.progress.plan = Some(shared),
                    }
                }
                // Poll before any numeric group signal; an already reaped PID
                // must never authorize signaling a reused group.
                if matches!(raw.child.try_wait(), Ok(None))
                    && raw.binding.source == source
                    && raw.binding.epoch == epoch
                {
                    let _ = raw.signal_group(Signal::KILL);
                    let _ = raw.kill_root();
                }
            }
        }
    }
}

impl PaneProcessRetirementOwner {
    /// Locks private bookkeeping; poison cannot justify discarding a child.
    fn state(&self) -> MutexGuard<'_, RetirementState> {
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Reserves a no-allocation return path before any managed native spawn.
    ///
    /// Failure leaves existing owners untouched. Returned/draining children
    /// reject new admission, but do not cap or interrupt already-live panes.
    pub fn reserve(&self) -> Result<PaneProcessChildReservation> {
        let mut state = self.state();
        if state.sealed
            || state.failed_children != 0
            || state
                .slots
                .iter()
                .any(|slot| matches!(slot, Some(ChildSlot::Returned(_) | ChildSlot::Draining)))
        {
            return Err(MezError::invalid_state(
                "managed native process admission is fenced by pending retirement",
            ));
        }
        let idle = state.slots.iter().all(Option::is_none);
        let next_source = if idle {
            state.source_epoch.checked_add(1).ok_or_else(|| {
                MezError::invalid_state("native cleanup source generation exhausted")
            })?
        } else {
            state.source_epoch
        };
        let index = match state.slots.iter().position(Option::is_none) {
            Some(index) => index,
            None => {
                state.slots.try_reserve(1).map_err(|error| {
                    MezError::invalid_state(format!(
                        "cannot reserve native child retirement ownership: {error}"
                    ))
                })?;
                state.slots.push(None);
                state.slots.len() - 1
            }
        };
        state.slots[index] = Some(ChildSlot::Reserved);
        if idle {
            // Publish the new source only after allocation and Reserved commit.
            state.source_epoch = next_source;
            state.attempt = None;
            state.drain_cursor = 0;
        }
        Ok(PaneProcessChildReservation {
            owner: self.clone(),
            index: Some(index),
        })
    }

    /// Forbids all future admission, including after successful cleanup retry.
    pub fn seal(&self) {
        self.state().sealed = true;
    }

    /// Records graceful/force intent before actor or service cancellation.
    /// Repeated requests can only upgrade force, never renew the original budget.
    pub fn request_shutdown(&self, force: bool) {
        let mut state = self.state();
        if state.attempt.is_none() && state.slots.iter().any(Option::is_some) {
            state.attempt = Some(CleanupAttemptState {
                epoch: 0,
                deadline: None,
                force: false,
            });
        }
        if let Some(attempt) = &mut state.attempt {
            attempt.force |= force;
        }
        match &mut state.shutdown {
            Some(plan) => plan.force |= force,
            None => {
                let started = Instant::now();
                state.shutdown = Some(NativeShutdownPlan {
                    started,
                    deadline: started + Duration::from_secs(if force { 1 } else { 2 }),
                    force,
                });
            }
        }
    }

    /// Returns the original shutdown deadline or the existing forced wait budget.
    pub fn cleanup_deadline(&self) -> Instant {
        self.state().shutdown.map_or_else(
            || Instant::now() + Duration::from_secs(1),
            |plan| plan.deadline,
        )
    }

    /// Returns one bounded private failure receipt, independent of pending count.
    pub fn cleanup_failure(&self) -> Option<String> {
        self.state().first_failure.clone()
    }

    /// Queries native failure without allocating diagnostic text on status reads.
    pub fn has_cleanup_failure(&self) -> bool {
        self.state().first_failure.is_some()
    }

    /// Reports monotonic closure independently of current pending cardinality.
    pub fn is_sealed(&self) -> bool {
        self.state().sealed
    }

    /// Counts every unreaped obligation, not merely queued return records.
    pub fn outstanding(&self) -> usize {
        self.state()
            .slots
            .iter()
            .filter(|slot| slot.is_some())
            .count()
    }

    /// Counts unproven returns without including still-live sibling workers.
    pub fn returned_count(&self) -> usize {
        self.state()
            .slots
            .iter()
            .filter(|slot| matches!(slot, Some(ChildSlot::Returned(_) | ChildSlot::Draining)))
            .count()
    }

    /// Claims wait-only observation without effect authority. Use a captured
    /// attempt for any signaling; this never adopts a current retry budget.
    pub fn begin_drain(&self) -> Result<PaneProcessRetirementDrain> {
        self.try_begin_drain().ok_or_else(|| {
            MezError::invalid_state("native child retirement drain is already active")
        })
    }

    /// Returns typed contention without mistaking expected overlap for failure.
    /// Async callers wait within their original deadline and retry this claim;
    /// no child ownership or native operation is replayed while it is busy.
    pub fn try_begin_drain(&self) -> Option<PaneProcessRetirementDrain> {
        let mut state = self.state();
        if state.draining
            || state
                .slots
                .iter()
                .any(|slot| matches!(slot, Some(ChildSlot::Draining)))
        {
            return None;
        }
        state.draining = true;
        Some(PaneProcessRetirementDrain {
            owner: self.clone(),
            cursor: state.drain_cursor,
            observation_only: true,
            binding: CleanupBinding {
                source: state.source_epoch,
                epoch: state.attempt.map_or(0, |attempt| attempt.epoch),
                deadline: state.attempt.and_then(|attempt| attempt.deadline),
                force: state.attempt.is_some_and(|attempt| attempt.force),
                started: None,
            },
        })
    }

    /// Claims serialization for the exact captured owner/source/attempt. Both
    /// identity and busy checks precede every resource/bookkeeping mutation.
    pub fn try_begin_drain_for_attempt(
        &self,
        attempt: &PaneProcessCleanupAttempt,
    ) -> Result<Option<PaneProcessRetirementDrain>> {
        if !Arc::ptr_eq(&self.inner, &attempt.owner.inner) {
            return Err(MezError::invalid_state(
                "native cleanup attempt belongs to another owner",
            ));
        }
        let mut state = self.state();
        if state.source_epoch != attempt.binding.source
            || state.attempt.map_or(0, |current| current.epoch) != attempt.binding.epoch
        {
            return Err(MezError::invalid_state("native cleanup attempt is stale"));
        }
        if state.draining
            || state
                .slots
                .iter()
                .any(|slot| matches!(slot, Some(ChildSlot::Draining)))
        {
            return Ok(None);
        }
        state.draining = true;
        Ok(Some(PaneProcessRetirementDrain {
            owner: self.clone(),
            cursor: state.drain_cursor,
            binding: attempt.binding,
            observation_only: false,
        }))
    }

    /// Releases only a completed native wait or a reservation with no child.
    fn complete(&self, index: usize) {
        self.complete_native(index, false);
    }

    /// Releases failure admission fencing only after this exact native wait.
    fn complete_native(&self, index: usize, failed: bool) {
        let mut state = self.state();
        state.slots[index] = None;
        if failed {
            state.failed_children = state.failed_children.saturating_sub(1);
        }
        // Occupied slot indices never move. Vacant trailing entries are not
        // history, and the backing allocation is reclaimed at complete idle.
        // While older live indices remain, spare capacity retains its admitted
        // high-water allocation; it never grows merely from completed churn.
        while state.slots.last().is_some_and(Option::is_none) {
            state.slots.pop();
        }
        if state.slots.is_empty() {
            state.slots = Vec::new();
        }
    }
}

/// A pre-spawn reservation is included in the final cleanup barrier.
#[derive(Debug)]
pub struct PaneProcessChildReservation {
    owner: PaneProcessRetirementOwner,
    index: Option<usize>,
}

impl PaneProcessChildReservation {
    /// Immediately guards a newly created child, before PID/config fallibles.
    /// A reservation acquired before sealing remains valid after sealing.
    pub(super) fn attach(
        mut self,
        child: Box<dyn Child + Send + Sync>,
        group: Option<i32>,
    ) -> RetainedPaneChild {
        let index = self.index.take();
        let binding = {
            let mut state = self.owner.state();
            if let Some(index) = index {
                state.slots[index] = Some(ChildSlot::Leased);
            }
            CleanupBinding {
                source: state.source_epoch,
                epoch: state.attempt.map_or(0, |attempt| attempt.epoch),
                deadline: None,
                force: false,
                started: None,
            }
        };
        RetainedPaneChild {
            raw: Some(RawPaneChild {
                child,
                binding,
                group,
                force_requested: false,
                progress: NativeTerminationProgress::default(),
                failure_latched: false,
            }),
            reservation: index.map(|index| (self.owner.clone(), index)),
            status: None,
            observation_only: false,
        }
    }
}

impl Drop for PaneProcessChildReservation {
    /// Failed spawn releases its empty reservation; no native child is lost.
    fn drop(&mut self) {
        if let Some(index) = self.index.take() {
            self.owner.complete(index);
        }
    }
}

/// Exclusive drain lease; cancellation leaves all native children owned.
#[derive(Debug)]
pub struct PaneProcessRetirementDrain {
    owner: PaneProcessRetirementOwner,
    cursor: usize,
    binding: CleanupBinding,
    observation_only: bool,
}

impl PaneProcessRetirementDrain {
    /// Claims one exact returned child using its existing reservation.
    pub fn take_returned(&mut self) -> Option<RetainedPaneChild> {
        let mut state = self.owner.state();
        if state.source_epoch != self.binding.source
            || state.attempt.map_or(0, |attempt| attempt.epoch) != self.binding.epoch
        {
            return None;
        }
        let shared = state.shutdown;
        let len = state.slots.len();
        for offset in 0..len {
            let index = (self.cursor + offset) % len;
            let slot = &mut state.slots[index];
            if matches!(slot, Some(ChildSlot::Returned(_)))
                && let Some(ChildSlot::Returned(mut raw)) = slot.take()
            {
                *slot = Some(ChildSlot::Draining);
                if !self.observation_only {
                    let mut binding = self.binding;
                    if raw.binding.source == binding.source && raw.binding.epoch == binding.epoch {
                        binding.deadline = [raw.binding.deadline, binding.deadline]
                            .into_iter()
                            .flatten()
                            .min();
                        binding.started = raw.binding.started.or(binding.started);
                        binding.force |= raw.binding.force;
                    }
                    raw.binding = binding;
                }
                let binding = raw.binding;
                if !self.observation_only
                    && raw.progress.plan.is_none()
                    && let Some(deadline) = binding.deadline
                {
                    if binding.epoch == 0 {
                        if let Some(started) = binding.started.or(shared.map(|plan| plan.started)) {
                            let mut plan = shared.unwrap_or(NativeShutdownPlan {
                                started,
                                deadline,
                                force: true,
                            });
                            plan.deadline = plan.deadline.min(deadline);
                            plan.force |= binding.force;
                            raw.progress.plan = Some(plan);
                        }
                    } else if let Some(shared) = shared {
                        raw.progress.plan = Some(shared);
                    }
                }
                self.cursor = (index + 1) % len;
                return Some(RetainedPaneChild {
                    raw: Some(raw),
                    reservation: Some((self.owner.clone(), index)),
                    status: None,
                    observation_only: self.observation_only,
                });
            }
        }
        None
    }
}

impl Drop for PaneProcessRetirementDrain {
    /// Releases drain serialization, not native ownership or admission sealing.
    fn drop(&mut self) {
        let mut state = self.owner.state();
        if state.source_epoch == self.binding.source
            && state.attempt.map_or(0, |attempt| attempt.epoch) == self.binding.epoch
        {
            state.drain_cursor = self.cursor;
        }
        state.draining = false;
    }
}

/// Exact child guard shared by normal process methods and bounded cleanup.
///
/// `raw` is Some for every callable method and removed only during Drop. Native
/// status is cached separately from presentation/output lifecycle metadata.
#[derive(Debug)]
pub struct RetainedPaneChild {
    raw: Option<RawPaneChild>,
    reservation: Option<(PaneProcessRetirementOwner, usize)>,
    status: Option<ExitStatus>,
    observation_only: bool,
}

impl RetainedPaneChild {
    /// Adopts a standalone fixture/embedding handle without accepting a child
    /// owned by another session. Managed production spawns reserve earlier.
    pub(super) fn adopt(&mut self, owner: &PaneProcessRetirementOwner) -> Result<()> {
        if let Some((current, _)) = &self.reservation {
            return if Arc::ptr_eq(&current.inner, &owner.inner) {
                Ok(())
            } else {
                Err(MezError::invalid_state(
                    "native child belongs to a different retirement owner",
                ))
            };
        }
        if self.status.is_some() {
            return Err(MezError::invalid_state(
                "reaped native child cannot be adopted as running",
            ));
        }
        let mut reservation = owner.reserve()?;
        if let Some(index) = reservation.index.take() {
            let mut state = owner.state();
            state.slots[index] = Some(ChildSlot::Leased);
            self.raw_mut()?.binding = CleanupBinding {
                source: state.source_epoch,
                epoch: state.attempt.map_or(0, |attempt| attempt.epoch),
                deadline: None,
                force: false,
                started: None,
            };
            self.reservation = Some((owner.clone(), index));
        }
        Ok(())
    }

    /// Wraps standalone callers that do not participate in a managed session.
    /// Managed production spawns must instead use a pre-spawn reservation.
    pub(super) fn standalone(child: Box<dyn Child + Send + Sync>, group: Option<i32>) -> Self {
        Self {
            raw: Some(RawPaneChild {
                child,
                binding: CleanupBinding::default(),
                group,
                force_requested: false,
                progress: NativeTerminationProgress::default(),
                failure_latched: false,
            }),
            reservation: None,
            status: None,
            observation_only: false,
        }
    }

    /// Borrows the exact child while this guard is live.
    fn raw_mut(&mut self) -> io::Result<&mut RawPaneChild> {
        self.raw
            .as_mut()
            .ok_or_else(|| io::Error::other("native child guard has been consumed"))
    }

    /// Returns the original owned PID, including absence during spawn failure.
    pub fn process_id(&self) -> Option<u32> {
        self.raw.as_ref().and_then(|raw| raw.child.process_id())
    }

    /// Caches successful native wait and immediately releases its slot.
    fn record_wait(&mut self, status: ExitStatus) -> ExitStatus {
        if let Some((owner, index)) = self.reservation.take() {
            owner.complete_native(
                index,
                self.raw.as_ref().is_some_and(|raw| raw.failure_latched),
            );
        }
        self.status = Some(status.clone());
        status
    }

    /// Polls/reaps the owned child; signals and synthetic events cannot settle it.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = &self.status {
            return Ok(Some(status.clone()));
        }
        let status = match self.raw_mut()?.child.try_wait() {
            Ok(status) => status,
            Err(error) => {
                self.record_failure("native wait", &error);
                return Err(error);
            }
        };
        Ok(status.map(|status| self.record_wait(status)))
    }

    /// Retains the synchronous process API, caching its real wait result.
    pub(super) fn wait(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = &self.status {
            return Ok(status.clone());
        }
        let status = match self.raw_mut()?.child.wait() {
            Ok(status) => status,
            Err(error) => {
                self.record_failure("native wait", &error);
                return Err(error);
            }
        };
        Ok(self.record_wait(status))
    }

    /// Signals the exact owned root, never a replacement found by PID lookup.
    pub fn kill(&mut self) -> io::Result<()> {
        if self.try_wait()?.is_some() {
            return Ok(());
        }
        self.validate_cleanup_binding().map_err(io::Error::other)?;
        if self.raw_mut()?.force_requested {
            return Ok(());
        }
        if let Err(error) = self.raw_mut()?.kill_root() {
            // An authoritative exact-child wait can win an already-exited
            // root race; never classify benignness by error text.
            if let Ok(Some(status)) = self.raw_mut()?.child.try_wait() {
                self.record_wait(status);
                return Ok(());
            }
            self.record_failure("native root signal", &error);
            return Err(error);
        }
        self.raw_mut()?.force_requested = true;
        Ok(())
    }

    /// Records one bounded native-stage failure before any future can yield.
    /// The raw flag travels with the child, so retry/cancel cannot double-count.
    pub(super) fn record_failure(&mut self, stage: &'static str, error: &impl std::fmt::Display) {
        let Some((owner, _)) = &self.reservation else {
            return;
        };
        let Some(raw) = self.raw.as_mut() else {
            return;
        };
        let first_for_child = !raw.failure_latched;
        raw.failure_latched = true;
        let mut state = owner.state();
        state.failed_children += usize::from(first_for_child);
        if state.first_failure.is_none() {
            let mut diagnostic = format!("{stage}: {error}");
            let mut limit = diagnostic.len().min(4096);
            while !diagnostic.is_char_boundary(limit) {
                limit -= 1;
            }
            diagnostic.truncate(limit);
            state.first_failure = Some(diagnostic);
        }
    }

    /// Checks the scalar binding without updating source, attempt, budget or
    /// failure state. Leased/Draining ownership excludes retry replacement
    /// between this check and syscall. Cached native status remains inert.
    fn validate_cleanup_binding(&self) -> Result<()> {
        if self.observation_only {
            return Err(MezError::invalid_state(
                "wait-only native claim cannot issue cleanup effects",
            ));
        }
        let raw = self
            .raw
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("native child guard has been consumed"))?;
        if let Some((owner, _)) = &self.reservation {
            let state = owner.state();
            if raw.binding.source != state.source_epoch
                || raw.binding.epoch != state.attempt.map_or(0, |attempt| attempt.epoch)
            {
                return Err(MezError::invalid_state(
                    "native child cleanup binding is stale",
                ));
            }
        }
        if raw.binding.epoch != 0 && raw.binding.deadline.is_none() {
            return Err(MezError::invalid_state(
                "native retry binding has no finite budget",
            ));
        }
        Ok(())
    }

    /// Starts/continues the same exact-child timeline across every handoff.
    pub(super) fn begin_termination(&mut self, force: bool) -> Result<Instant> {
        self.validate_cleanup_binding()?;
        let shared = if let Some((owner, _)) = &self.reservation {
            let mut state = owner.state();
            if state.attempt.is_none() {
                // Registration is visible to Prepared(None), but deliberately
                // has no owner-wide child-local deadline.
                state.attempt = Some(CleanupAttemptState {
                    epoch: 0,
                    deadline: None,
                    force: false,
                });
            }
            state.shutdown
        } else {
            None
        };
        let raw = self.raw_mut()?;
        let binding = raw.binding;
        if binding.epoch != 0 && raw.progress.plan.is_none() && shared.is_none() {
            return Err(MezError::invalid_state(
                "native retry has no original child or owner plan",
            ));
        }
        let plan = raw.progress.plan.get_or_insert_with(|| {
            let mut plan = shared.unwrap_or_else(|| {
                let started = Instant::now();
                NativeShutdownPlan {
                    started,
                    deadline: started + Duration::from_secs(if force { 1 } else { 2 }),
                    force,
                }
            });
            if binding.epoch == 0
                && let Some(deadline) = binding.deadline
            {
                plan.deadline = plan.deadline.min(deadline);
            }
            plan
        });
        if binding.epoch == 0
            && let Some(shared) = shared
        {
            plan.started = plan.started.min(shared.started);
            plan.deadline = plan.deadline.min(shared.deadline);
            plan.force |= shared.force;
        }
        plan.force |= force || binding.force;
        raw.signal_deadline().ok_or_else(|| {
            MezError::invalid_state("native termination has no finite effect deadline")
        })
    }

    /// Records an explicit signal stage once, honoring force upgrades and the
    /// original deadline. False suppresses repeated/obsolete graceful stages.
    pub(super) fn record_signal(&mut self, signal: Signal) -> Result<bool> {
        self.begin_termination(signal == Signal::KILL)?;
        let raw = self.raw_mut()?;
        if !raw.signal_budget_remaining() {
            return Err(MezError::invalid_state(
                "native termination deadline expired; exact ownership retained",
            ));
        }
        let progress = &mut raw.progress;
        if matches!(signal, Signal::HUP | Signal::TERM)
            && progress.plan.is_some_and(|plan| plan.force)
        {
            return Ok(false);
        }
        if signal == Signal::HUP {
            if progress.hup_sent || progress.term_sent {
                return Ok(false);
            }
            progress.hup_sent = true;
        }
        if signal == Signal::TERM {
            if progress.term_sent {
                return Ok(false);
            }
            progress.term_sent = true;
        }
        Ok(true)
    }

    /// Sends only checked birth-group signals while the exact root is unreaped.
    /// A failed signal is latched before any later wait can release the slot.
    pub(super) fn signal_group(&mut self, signal: Signal) -> Result<()> {
        if self.try_wait()?.is_some() {
            return Ok(());
        }
        self.validate_cleanup_binding()?;
        let result = self.raw_mut()?.signal_group(signal);
        if let Err(error) = &result {
            self.record_failure("native group signal", error);
        }
        result
    }

    /// Advances HUP/TERM/KILL without sleeps, preserving prior sent stages.
    /// A successful native wait settles the slot; a signal never does.
    pub fn poll_termination(&mut self, force: bool) -> Result<Option<ExitStatus>> {
        self.poll_termination_with_grace(force, Duration::from_millis(500))
    }

    /// Shares stage selection with synchronous callers. Custom grace is the
    /// absolute HUP-to-TERM offset from the original start, capped at the
    /// default 500ms to reserve the established force/wait tail. KILL occurs at
    /// twice that offset, under the existing deadline. This never creates fresh
    /// per-call grace or replays a stage sent before handoff.
    pub(super) fn poll_termination_with_grace(
        &mut self,
        force: bool,
        grace: Duration,
    ) -> Result<Option<ExitStatus>> {
        if let Some(status) = self.try_wait()? {
            return Ok(Some(status));
        }
        self.begin_termination(force)?;
        let grace = grace.min(Duration::from_millis(500));
        let raw = self.raw_mut()?;
        // A live guard's begin_termination always installs this plan.
        let plan = raw
            .progress
            .plan
            .ok_or_else(|| MezError::invalid_state("native termination plan is missing"))?;
        if !raw.signal_budget_remaining() {
            let error = MezError::invalid_state(
                "native termination deadline expired; exact ownership retained",
            );
            self.record_failure("native termination deadline", &error);
            return Err(error);
        }
        let elapsed = plan.started.elapsed();
        let signal_result = if plan.force || elapsed >= grace.saturating_mul(2) {
            self.force_retirement()
        } else {
            let signal = if !raw.progress.hup_sent && !raw.progress.term_sent {
                raw.progress.hup_sent = true;
                Some(Signal::HUP)
            } else if elapsed >= grace && !raw.progress.term_sent {
                raw.progress.term_sent = true;
                Some(Signal::TERM)
            } else {
                None
            };
            signal.map_or(Ok(()), |signal| self.signal_group(signal))
        };
        // A group failure cannot skip the independent exact native wait. Keep
        // the first typed signal error, even if this wait releases the slot.
        let wait_result = self.try_wait().map_err(MezError::from);
        signal_result.and(wait_result)
    }

    /// Continues graceful intent after cancellation; abandoned local failures
    /// without a shutdown request use the existing forced cleanup policy.
    pub fn poll_cleanup(&mut self) -> Result<Option<ExitStatus>> {
        let force = self
            .reservation
            .as_ref()
            .and_then(|(owner, _)| owner.state().shutdown)
            .or_else(|| self.raw.as_ref().and_then(|raw| raw.progress.plan))
            .is_none_or(|plan| plan.force);
        self.poll_termination(force)
    }

    /// Attempts group and exact-root cleanup independently, preserving failures.
    /// Polling precedes numeric group signaling to fence already-reaped groups.
    pub fn force_retirement(&mut self) -> Result<()> {
        if self.try_wait()?.is_some() {
            return Ok(());
        }
        self.begin_termination(true)?;
        if self.raw_mut()?.force_requested {
            return Ok(());
        }
        let group_result = self.raw_mut()?.signal_group(Signal::KILL);
        // Root polling may release this slot immediately. Preserve an actual
        // group failure before that winning wait, not only in the caller future.
        if let Err(error) = &group_result {
            self.record_failure("native group signal", error);
        }
        let root_result = self.kill().map_err(MezError::from);
        if root_result.is_ok() {
            self.raw_mut()?.force_requested = true;
        }
        // Both operations have run. Preserve the first error's concrete kind;
        // root failure has independently been latched by kill, not formatted
        // into a replacement InvalidState error that erases the first cause.
        let result = group_result.and(root_result);
        if let Err(error) = &result {
            self.record_failure("native group/root escalation", error);
        }
        result
    }
}

impl Drop for RetainedPaneChild {
    /// Returns raw resources to the already allocated exact slot, without I/O.
    fn drop(&mut self) {
        if let Some((owner, index)) = self.reservation.take()
            && let Some(raw) = self.raw.take()
        {
            owner.state().slots[index] = Some(ChildSlot::Returned(raw));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    mod cleanup_attempts;

    /// Spawns an exclusively owned root that reports received graceful signals
    /// and child-generated probe replies, without relying on terminal echo.
    fn signal_observer(owner: &PaneProcessRetirementOwner) -> super::super::PaneProcess {
        signal_observer_with_environment(
            owner,
            &[("MEZ_TEST_SIGNAL_RECEIPT".into(), String::new())],
        )
    }

    /// Adds only fake fixture environment, including an optional private trap
    /// receipt independent of PTY output consumed by synchronous termination.
    fn signal_observer_with_environment(
        owner: &PaneProcessRetirementOwner,
        environment: &[(String, String)],
    ) -> super::super::PaneProcess {
        let mut process = super::super::spawn_retained_argv_pty_process(
            std::path::Path::new("/bin/sh"),
            &["-c".into(), "trap 'printf observed-hup; if test -n \"$MEZ_TEST_SIGNAL_RECEIPT\"; then printf hup >> \"$MEZ_TEST_SIGNAL_RECEIPT\"; fi' HUP; trap 'printf observed-term; if test -n \"$MEZ_TEST_SIGNAL_RECEIPT\"; then printf term >> \"$MEZ_TEST_SIGNAL_RECEIPT\"; fi' TERM; printf 'observer-ready\\n'; while :; do if IFS= read -r line; then printf 'observer-reply:%s\\n' \"$line\"; fi; done".into()],
            environment, mez_terminal::TerminalSize::new(80, 24).unwrap(), None, owner.reserve().unwrap()).unwrap();
        assert!(observer_output(&mut process, "observer-ready").contains("observer-ready"));
        process
    }

    /// Reads bounded fixture output until a marker or a short probe deadline.
    fn observer_output(process: &mut super::super::PaneProcess, marker: &str) -> String {
        let mut output = Vec::new();
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline {
            match process.read_available_output(4096) {
                Ok(bytes) => output.extend(bytes),
                Err(_) => break,
            }
            if String::from_utf8_lossy(&output).contains(marker) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        String::from_utf8_lossy(&output).into_owned()
    }

    /// Synchronous advancement after handoff must not repeat already-issued
    /// graceful stages or renew their grace. Flags are injected as retained
    /// handoff state; a real root reports any accidental
    /// replay; native wait/SRCH, not output closure, proves final settlement.
    #[test]
    fn synchronous_termination_preserves_sent_stages_and_original_start() {
        for term_sent in [false, true] {
            let owner = PaneProcessRetirementOwner::default();
            use std::os::unix::fs::OpenOptionsExt;
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let receipt = std::env::temp_dir().join(format!(
                "mez-native-stage-{}-{nonce}-{term_sent}",
                std::process::id()
            ));
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&receipt)
                .unwrap();
            drop(file);
            let mut process = signal_observer_with_environment(
                &owner,
                &[(
                    "MEZ_TEST_SIGNAL_RECEIPT".into(),
                    receipt.to_string_lossy().into_owned(),
                )],
            );
            let master = process.duplicate_master_fd().unwrap();
            process.child.begin_termination(false).unwrap();
            let raw = process.child.raw.as_mut().unwrap();
            raw.progress.hup_sent = true;
            raw.progress.term_sent = term_sent;
            raw.progress.plan.as_mut().unwrap().started =
                Instant::now() - Duration::from_millis(150);
            let original = raw.progress.plan.unwrap();
            let result = process.terminate(Duration::from_millis(100));
            let output = std::fs::read_to_string(&receipt);
            let pid = rustix::process::Pid::from_raw(i32::try_from(process.primary_pid()).unwrap())
                .unwrap();
            let absent = rustix::process::test_kill_process(pid) == Err(rustix::io::Errno::SRCH);
            let final_plan = process.child.raw.as_ref().unwrap().progress.plan.unwrap();
            drop(master);
            let removed = std::fs::remove_file(&receipt);
            assert!(removed.is_ok(), "could not remove owned trap receipt");
            let output = output.unwrap();
            assert!(result.is_ok() && absent && owner.outstanding() == 0);
            assert!(!output.contains("hup"), "HUP was replayed: {output}");
            if term_sent {
                assert!(!output.contains("term"), "TERM was replayed: {output}");
            }
            assert_eq!(final_plan.started, original.started);
            assert_eq!(final_plan.deadline, original.deadline);
        }
    }

    /// Long custom grace must reserve force/wait within the original two-second
    /// timeline. A real signal-resistant root stays under held-master ownership
    /// until actual KILL/wait/SRCH, without renewing its established deadline.
    #[test]
    fn long_synchronous_grace_reserves_native_force_and_wait_tail() {
        for grace in [Duration::from_secs(1), Duration::from_secs(30)] {
            let owner = PaneProcessRetirementOwner::default();
            let mut process = signal_observer(&owner);
            let master = process.duplicate_master_fd().unwrap();
            let original = process.child.begin_termination(false).unwrap();
            let result = process.terminate(grace);
            let pid = rustix::process::Pid::from_raw(i32::try_from(process.primary_pid()).unwrap())
                .unwrap();
            let absent = rustix::process::test_kill_process(pid) == Err(rustix::io::Errno::SRCH);
            let same_deadline = process
                .child
                .raw
                .as_ref()
                .unwrap()
                .progress
                .plan
                .unwrap()
                .deadline
                == original;
            // Independently settle an owned fixture if the desired operation
            // failed; this direct exact handle is not product completion proof.
            if !absent {
                let native = process
                    .child
                    .raw
                    .as_mut()
                    .unwrap()
                    .child
                    .as_mut()
                    .as_any_mut()
                    .downcast_mut::<std::process::Child>()
                    .unwrap();
                let _ = std::process::Child::kill(native);
                let _ = process.wait();
            }
            drop(master);
            assert!(
                result.is_ok() && absent && owner.outstanding() == 0,
                "long grace skipped native force: {result:?}"
            );
            assert!(same_deadline, "long grace renewed the original deadline");
        }
    }

    /// Explicit TERM must make subsequent HUP obsolete, just as shared stage
    /// selection does. This synthetic state control exercises the private
    /// recorder without authorizing any real group or opaque killer signal.
    #[test]
    fn explicit_term_suppresses_later_hup_stage() {
        let state = Arc::new(ChildState::default());
        let mut child =
            RetainedPaneChild::standalone(Box::new(ControlledChild(state.clone())), None);
        assert!(child.record_signal(Signal::TERM).unwrap());
        assert!(!child.record_signal(Signal::HUP).unwrap());
        state.exited.store(true, Ordering::SeqCst);
        assert!(child.try_wait().unwrap().is_some());
        assert_eq!(state.kills.load(Ordering::SeqCst), 0);
    }

    /// A force upgrade recorded by the owner survives synchronous handoff:
    /// no HUP/TERM is introduced and custom grace cannot defer exact force.
    #[test]
    fn synchronous_termination_honors_retained_force_upgrade() {
        let owner = PaneProcessRetirementOwner::default();
        let mut process = signal_observer(&owner);
        let master = process.duplicate_master_fd().unwrap();
        owner.request_shutdown(false);
        let original = process.child.begin_termination(false).unwrap();
        owner.request_shutdown(true);
        let started = Instant::now();
        let result = process.terminate(Duration::from_millis(500));
        let elapsed = started.elapsed();
        let output = observer_output(&mut process, "never-produced-marker");
        let progress = &process.child.raw.as_ref().unwrap().progress;
        let no_graceful = !progress.hup_sent && !progress.term_sent;
        let same_deadline = progress.plan.unwrap().deadline == original;
        drop(master);
        assert!(result.is_ok() && owner.outstanding() == 0);
        assert!(no_graceful && same_deadline && !output.contains("observed-"));
        assert!(
            elapsed < Duration::from_millis(500),
            "force upgrade restarted grace: {elapsed:?}"
        );
    }

    /// Expired retained plans permit only native observation, not fresh group
    /// or root signals through synchronous, async, or direct managed methods.
    /// Require a live poll and fresh child response before independently using
    /// this fixture's exact std handle for cleanup outside the expired operation.
    #[test]
    fn expired_retained_plan_cannot_signal_a_live_native_root() {
        let owner = PaneProcessRetirementOwner::default();
        let mut process = signal_observer(&owner);
        let master = process.duplicate_master_fd().unwrap();
        process.child.begin_termination(false).unwrap();
        process
            .child
            .raw
            .as_mut()
            .unwrap()
            .progress
            .plan
            .as_mut()
            .unwrap()
            .deadline = Instant::now() - Duration::from_millis(1);
        let sync = process.terminate(Duration::from_millis(10));
        let asynchronous = process.poll_native_termination(true);
        let group = process.send_signal_to_process_group(Signal::KILL);
        let root = process.kill_owned_root();
        let live = matches!(process.poll_exit(), Ok(None));
        let write = process.write_input(b"expired-plan-probe\n");
        let output = observer_output(&mut process, "observer-reply:expired-plan-probe");
        let native = process
            .child
            .raw
            .as_mut()
            .unwrap()
            .child
            .as_mut()
            .as_any_mut()
            .downcast_mut::<std::process::Child>()
            .unwrap();
        let cleanup = std::process::Child::kill(native);
        let waited = process.wait();
        let pid =
            rustix::process::Pid::from_raw(i32::try_from(process.primary_pid()).unwrap()).unwrap();
        let absent = rustix::process::test_kill_process(pid) == Err(rustix::io::Errno::SRCH);
        drop(master);
        assert!(cleanup.is_ok() && waited.is_ok() && absent && owner.outstanding() == 0);
        assert!(sync.is_err() && asynchronous.is_err() && group.is_err() && root.is_err());
        assert!(live && write.is_ok() && output.contains("observer-reply:expired-plan-probe"));
        assert!(
            !output.contains("observed-"),
            "expired cleanup signaled live root: {output}"
        );
    }

    /// Controllable exact child: no PID/group lookup or real process signaling.
    #[derive(Debug, Default)]
    struct ChildState {
        exited: AtomicBool,
        wait_error: AtomicBool,
        kills: AtomicUsize,
        polls: AtomicUsize,
        drops: AtomicUsize,
    }

    /// Implements only the native handle operations used by the real guard.
    #[derive(Debug)]
    struct ControlledChild(Arc<ChildState>);

    impl portable_pty::ChildKiller for ControlledChild {
        fn kill(&mut self) -> io::Result<()> {
            self.0.kills.fetch_add(1, Ordering::SeqCst);
            self.0.exited.store(true, Ordering::SeqCst);
            Ok(())
        }
        fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
            Box::new(Self(self.0.clone()))
        }
    }

    impl Child for ControlledChild {
        fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            self.0.polls.fetch_add(1, Ordering::SeqCst);
            if self.0.wait_error.load(Ordering::SeqCst) {
                return Err(io::Error::other("injected exact native wait failure"));
            }
            Ok(self
                .0
                .exited
                .load(Ordering::SeqCst)
                .then(|| ExitStatus::with_exit_code(0)))
        }
        fn wait(&mut self) -> io::Result<ExitStatus> {
            self.try_wait()?
                .ok_or_else(|| io::Error::other("fixture native wait still gated"))
        }
        fn process_id(&self) -> Option<u32> {
            None
        }
    }

    impl Drop for ControlledChild {
        fn drop(&mut self) {
            self.0.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Missing PID does not discard exact wait ownership. A reservation made
    /// before seal still attaches; a manually released native wait gate frees
    /// it immediately, repeated polls cache status, and seal never reopens.
    #[test]
    fn reserved_child_survives_seal_missing_pid_and_cached_native_wait() {
        let owner = PaneProcessRetirementOwner::default();
        let reservation = owner.reserve().unwrap();
        assert_eq!(owner.outstanding(), 1);
        owner.seal();
        let state = Arc::new(ChildState::default());
        let child = reservation.attach(Box::new(ControlledChild(state.clone())), None);
        assert_eq!(child.process_id(), None);
        drop(child);
        assert_eq!(owner.returned_count(), 1);
        assert!(owner.reserve().is_err());
        let mut drain = owner.begin_drain().unwrap();
        let mut child = drain.take_returned().unwrap();
        state.exited.store(true, Ordering::SeqCst);
        assert!(child.try_wait().unwrap().is_some());
        let polls = state.polls.load(Ordering::SeqCst);
        assert!(child.try_wait().unwrap().is_some());
        assert!(child.signal_group(Signal::HUP).is_ok());
        assert!(child.force_retirement().is_ok());
        assert!(child.kill().is_ok());
        assert_eq!(state.polls.load(Ordering::SeqCst), polls);
        assert_eq!(state.kills.load(Ordering::SeqCst), 0);
        assert_eq!(owner.outstanding(), 0);
        drop(child);
        assert!(owner.reserve().is_err());
        assert_eq!(owner.state().slots.len(), 0);
        assert_eq!(owner.state().slots.capacity(), 0);
    }

    /// Losing a drain future/lease cannot release or duplicate its claimed
    /// exact child. A second drain is rejected until that guard has returned.
    #[test]
    fn cancelled_drain_returns_same_slot_and_preserves_serialization() {
        let owner = PaneProcessRetirementOwner::default();
        let state = Arc::new(ChildState::default());
        drop(
            owner
                .reserve()
                .unwrap()
                .attach(Box::new(ControlledChild(state.clone())), None),
        );
        let mut first = owner.begin_drain().unwrap();
        let child = first.take_returned().unwrap();
        drop(first);
        assert!(owner.begin_drain().is_err());
        assert_eq!(owner.outstanding(), 1);
        drop(child);
        let mut retry = owner.begin_drain().unwrap();
        let mut child = retry.take_returned().unwrap();
        state.exited.store(true, Ordering::SeqCst);
        assert!(child.try_wait().unwrap().is_some());
        assert_eq!(owner.outstanding(), 0);
    }

    /// Opaque child types must reject group and root force without calling the
    /// unknown portable killer. Both typed failure receipts are attempted, the
    /// first group cause survives cancellation, and only its actual wait gate
    /// can settle the slot. No host process or numeric fallback is targeted.
    #[test]
    fn group_failure_still_attempts_owned_root_and_releases_only_after_wait() {
        let owner = PaneProcessRetirementOwner::default();
        let state = Arc::new(ChildState::default());
        let mut child = owner
            .reserve()
            .unwrap()
            .attach(Box::new(ControlledChild(state.clone())), Some(-1));
        let error = child.force_retirement().unwrap_err();
        assert_eq!(error.kind(), crate::MuxErrorKind::InvalidState);
        assert!(error.message().contains("unsupported child type"));
        assert_eq!(child.kill().unwrap_err().kind(), io::ErrorKind::Unsupported);
        assert_eq!(state.kills.load(Ordering::SeqCst), 0);
        assert_eq!(owner.outstanding(), 1);
        let failure = owner.cleanup_failure().unwrap();
        assert!(
            owner.reserve().is_err(),
            "unproven native error must fence admission"
        );
        // Drop the failed caller before wait: the same raw resources and first
        // failure receipt must survive, rather than depending on its future.
        drop(child);
        let mut retry = owner.begin_drain().unwrap();
        let mut child = retry.take_returned().unwrap();
        state.exited.store(true, Ordering::SeqCst);
        assert!(child.try_wait().unwrap().is_some());
        assert_eq!(owner.outstanding(), 0);
        assert_eq!(owner.cleanup_failure().as_deref(), Some(failure.as_str()));
        assert!(
            owner.has_cleanup_failure(),
            "zero pending does not erase native failure"
        );
    }

    /// Explicit force retry upgrades a pending graceful owner immediately while
    /// preserving its original absolute deadline and sent-stage information.
    #[test]
    fn pending_graceful_native_owner_force_upgrade_keeps_original_deadline() {
        let owner = PaneProcessRetirementOwner::default();
        let state = Arc::new(ChildState::default());
        let mut child = owner
            .reserve()
            .unwrap()
            .attach(Box::new(ControlledChild(state.clone())), None);
        owner.request_shutdown(false);
        let original = owner.cleanup_deadline();
        assert!(
            child.poll_cleanup().is_err(),
            "opaque group authority fails closed"
        );
        assert_eq!(state.kills.load(Ordering::SeqCst), 0);
        assert!(child.raw.as_ref().unwrap().progress.hup_sent);
        drop(child);
        owner.request_shutdown(true);
        assert_eq!(owner.cleanup_deadline(), original);
        let authority = owner.capture_cleanup_attempt(original).unwrap();
        let mut drain = owner
            .try_begin_drain_for_attempt(&authority)
            .unwrap()
            .unwrap();
        let mut child = drain.take_returned().unwrap();
        assert!(
            child.poll_cleanup().is_err(),
            "opaque root force fails closed"
        );
        assert!(child.raw.as_ref().unwrap().progress.plan.unwrap().force);
        assert_eq!(
            child.raw.as_ref().unwrap().progress.plan.unwrap().deadline,
            original
        );
        assert_eq!(state.kills.load(Ordering::SeqCst), 0);
        state.exited.store(true, Ordering::SeqCst);
        assert!(child.try_wait().unwrap().is_some());
        assert_eq!(owner.outstanding(), 0);
    }

    /// Final-owner abandonment cannot invoke an opaque portable killer even
    /// with a live wait result. It destroys the synthetic resources without
    /// panicking/cycling, not with a fabricated native completion receipt.
    #[test]
    fn last_owner_live_child_invalid_group_does_not_panic_and_attempts_root() {
        let owner = PaneProcessRetirementOwner::default();
        let weak = Arc::downgrade(&owner.inner);
        let state = Arc::new(ChildState::default());
        drop(
            owner
                .reserve()
                .unwrap()
                .attach(Box::new(ControlledChild(state.clone())), Some(-1)),
        );
        drop(owner);
        assert!(weak.upgrade().is_none());
        assert_eq!(state.kills.load(Ordering::SeqCst), 0);
        assert_eq!(state.drops.load(Ordering::SeqCst), 1);
    }

    /// Poll failure is not evidence authorizing numeric group/root signaling.
    /// Final-owner disposal is explicitly unproven and must not panic or cycle.
    #[test]
    fn last_owner_with_unproven_wait_does_not_signal_or_form_cycle() {
        let owner = PaneProcessRetirementOwner::default();
        let weak = Arc::downgrade(&owner.inner);
        let state = Arc::new(ChildState::default());
        state.wait_error.store(true, Ordering::SeqCst);
        drop(
            owner
                .reserve()
                .unwrap()
                .attach(Box::new(ControlledChild(state.clone())), Some(-1)),
        );
        drop(owner);
        assert!(weak.upgrade().is_none());
        assert_eq!(state.kills.load(Ordering::SeqCst), 0);
        assert_eq!(state.drops.load(Ordering::SeqCst), 1);
    }

    /// Corrupt cleanup metadata must never authorize signaling another root.
    /// Hold both masters open so PTY hangup cannot explain root retirement.
    /// Require exact root wait/SRCH and a fresh child-generated reply from the
    /// other root before independently cleaning it up. No PID discovery/reuse
    /// forcing is involved, and even the red assertion follows fixture cleanup.
    #[test]
    fn retained_native_escalation_kills_exact_root_with_different_owned_group() {
        let owner = PaneProcessRetirementOwner::default();
        let size = mez_terminal::TerminalSize::new(80, 24).unwrap();
        let mut root = super::super::spawn_retained_argv_pty_process(
            std::path::Path::new("/bin/cat"),
            &[],
            &[],
            size,
            None,
            owner.reserve().unwrap(),
        )
        .unwrap();
        let mut other = super::super::spawn_retained_argv_pty_process(
            std::path::Path::new("/bin/sh"),
            &[
                "-c".into(),
                "while IFS= read -r line; do printf 'child-reply:%s\\n' \"$line\"; done".into(),
            ],
            &[],
            size,
            None,
            owner.reserve().unwrap(),
        )
        .unwrap();
        let root_pid = root.primary_pid();
        let other_pid = other.primary_pid();
        let held_root = root.duplicate_master_fd().unwrap();
        let held_other = other.duplicate_master_fd().unwrap();
        root.child.raw.as_mut().unwrap().group = Some(i32::try_from(other_pid).unwrap());
        // A rejected authority is still an error, but independent root cleanup
        // must run. Poll the actual child even if the signal operation failed.
        let signal_result = root.poll_native_termination(true);
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut root_reaped = false;
        while Instant::now() < deadline {
            if matches!(root.poll_exit(), Ok(Some(_))) {
                root_reaped = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let pid = rustix::process::Pid::from_raw(i32::try_from(root_pid).unwrap()).unwrap();
        let root_absent = rustix::process::test_kill_process(pid) == Err(rustix::io::Errno::SRCH);
        let other_live = matches!(other.poll_exit(), Ok(None));
        let input_ok = other.write_input(b"fresh-authority-probe\n").is_ok();
        let mut output = Vec::new();
        let deadline = Instant::now() + Duration::from_millis(500);
        while input_ok && Instant::now() < deadline {
            let Ok(bytes) = other.read_available_output(4096) else {
                break;
            };
            output.extend(bytes);
            if String::from_utf8_lossy(&output).contains("child-reply:fresh-authority-probe") {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let responded =
            String::from_utf8_lossy(&output).contains("child-reply:fresh-authority-probe");
        let root_cleanup = root.terminate(Duration::from_millis(10));
        let other_cleanup = other.terminate(Duration::from_millis(10));
        drop((held_root, held_other));
        assert!(
            root_cleanup.is_ok() && other_cleanup.is_ok(),
            "fixture cleanup failed"
        );
        assert!(
            root_reaped && root_absent,
            "exact root was not waited/reaped before sibling probe"
        );
        assert!(
            other_live && responded,
            "cleanup signaled the other owned group; live={other_live}, response={responded}"
        );
        assert_eq!(
            signal_result.unwrap_err().kind(),
            crate::MuxErrorKind::InvalidState
        );
        assert_eq!(
            owner.outstanding(),
            0,
            "native wait releases slots before PTY owners are dropped"
        );
    }

    /// This real child encounters an injected invalid birth group at the
    /// production signal selector. The group stage fails, but owned-root KILL
    /// still runs. Drop that caller before wait and keep a duplicate master so
    /// kernel PTY hangup cannot supply the retirement proof. The same retained
    /// child must be waited/reaped, without erasing the earlier error receipt.
    #[test]
    fn real_native_group_failure_survives_caller_drop_and_zero_pending_reap() {
        for group in [None, Some(-1), Some(0)] {
            let owner = PaneProcessRetirementOwner::default();
            let mut process = super::super::spawn_retained_argv_pty_process(
                std::path::Path::new("/bin/cat"),
                &[],
                &[],
                mez_terminal::TerminalSize::new(80, 24).unwrap(),
                None,
                owner.reserve().unwrap(),
            )
            .unwrap();
            let pid = rustix::process::Pid::from_raw(i32::try_from(process.primary_pid()).unwrap())
                .unwrap();
            let held_master = process.duplicate_master_fd().unwrap();
            process.child.raw.as_mut().unwrap().group = group;
            let original = process.child.force_retirement();
            assert!(
                original.is_err(),
                "original group failure must remain a failed operation: {original:?}"
            );
            let failure = owner.cleanup_failure().unwrap();
            drop(process);
            assert_eq!(owner.outstanding(), 1);
            let mut drain = owner.begin_drain().unwrap();
            let mut child = drain.take_returned().unwrap();
            let deadline = Instant::now() + Duration::from_secs(1);
            loop {
                if child.try_wait().unwrap().is_some() {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "exact root signal did not settle"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(
                rustix::process::test_kill_process(pid),
                Err(rustix::io::Errno::SRCH)
            );
            assert_eq!(owner.outstanding(), 0);
            assert_eq!(owner.cleanup_failure().as_deref(), Some(failure.as_str()));
            assert!(owner.has_cleanup_failure());
            drop(held_master);
        }
    }

    /// Manager failure keeps the original process record even if independent
    /// root force/wait settles it. A second manager termination consumes only
    /// cached native status, not a renewed numeric group signal. This resides
    /// beside private authority so tests need no production mutation API.
    #[test]
    fn pane_process_manager_retains_process_when_termination_fails() {
        let mut manager = super::super::PaneProcessManager::new();
        let environment = super::super::PaneProcessEnvironment {
            mez: "native-test".into(),
            session: "native-test".into(),
            window: "@native".into(),
            pane: "%native".into(),
            term: "xterm".into(),
        };
        let pid = manager
            .spawn_for_pane(
                "%root",
                &super::super::PaneProcessLaunch::new("/bin/sh".into()),
                Some("cat"),
                &environment,
                mez_terminal::TerminalSize::new(80, 24).unwrap(),
            )
            .unwrap();
        let mut process = manager.take_running_pane_process("%root").unwrap();
        let held_master = process.duplicate_master_fd().unwrap();
        process.child.raw.as_mut().unwrap().group = None;
        manager
            .insert_running_pane_process("%root", process)
            .unwrap();
        let error = manager
            .terminate_pane_with_grace("%root", Duration::from_millis(10))
            .unwrap_err();
        let retained = manager.contains_pane("%root") && manager.primary_pid("%root") == Some(pid);
        let retry = manager.terminate_pane_with_grace("%root", Duration::from_millis(10));
        drop(held_master);
        assert_eq!(error.kind(), crate::MuxErrorKind::InvalidState);
        assert!(error.message().contains("authority is missing or invalid"));
        assert!(retained, "failed termination discarded its manager record");
        assert_eq!(retry.unwrap().unwrap().primary_pid, pid);
        assert!(manager.is_empty());
        assert_eq!(manager.retirement_owner().outstanding(), 0);
        let pid = rustix::process::Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
        assert_eq!(
            rustix::process::test_kill_process(pid),
            Err(rustix::io::Errno::SRCH)
        );
    }

    /// Four real native roots install a visible HUP trap before exact-root
    /// force. Inherent KILL emits no trap and does not incur portable-pty's
    /// four 50ms sleeps per child. Held masters and strict waits/SRCH exclude
    /// PTY closure and mere signal delivery as successful cleanup evidence.
    #[test]
    fn native_root_force_does_not_run_portable_hup_grace() {
        let owner = PaneProcessRetirementOwner::default();
        let mut processes = Vec::new();
        let mut masters = Vec::new();
        for _ in 0..4 {
            let mut process = super::super::spawn_retained_argv_pty_process(
                std::path::Path::new("/bin/sh"),
                &["-c".into(), "trap 'printf unexpected-hup\\\\n' HUP; trap '' TERM; printf 'force-ready\\n'; while IFS= read -r line; do :; done".into()],
                &[], mez_terminal::TerminalSize::new(80, 24).unwrap(), None, owner.reserve().unwrap()).unwrap();
            masters.push(process.duplicate_master_fd().unwrap());
            let deadline = Instant::now() + Duration::from_secs(1);
            let mut output = Vec::new();
            while Instant::now() < deadline
                && !String::from_utf8_lossy(&output).contains("force-ready")
            {
                output.extend(process.read_available_output(4096).unwrap());
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(
                String::from_utf8_lossy(&output).contains("force-ready"),
                "HUP trap was not installed"
            );
            processes.push(process);
        }
        let started = Instant::now();
        let force_results = processes
            .iter_mut()
            .map(|process| process.kill_owned_root())
            .collect::<Vec<_>>();
        let force_elapsed = started.elapsed();
        let mut reaped = true;
        let mut hup_seen = false;
        for process in &mut processes {
            let deadline = Instant::now() + Duration::from_secs(1);
            let mut waited = false;
            while Instant::now() < deadline {
                if matches!(process.poll_exit(), Ok(Some(_))) {
                    waited = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let pid = rustix::process::Pid::from_raw(i32::try_from(process.primary_pid()).unwrap())
                .unwrap();
            reaped &=
                waited && rustix::process::test_kill_process(pid) == Err(rustix::io::Errno::SRCH);
            hup_seen |= String::from_utf8_lossy(&process.read_available_output(4096).unwrap())
                .contains("unexpected-hup");
            let _ = process.terminate(Duration::from_millis(10));
        }
        drop(masters);
        assert!(force_results.iter().all(Result::is_ok));
        assert!(
            reaped && owner.outstanding() == 0,
            "native waits did not settle every root"
        );
        assert!(
            !hup_seen,
            "exact force invoked portable HUP rather than inherent KILL"
        );
        assert!(
            force_elapsed < Duration::from_millis(500),
            "exact force blocked for portable grace: {force_elapsed:?}"
        );
    }

    /// Failed pre-spawn work and settled historical peaks leave no completed
    /// slot metadata/allocation. Reuse is independent of previous child count.
    #[test]
    fn empty_reservations_and_high_water_cleanup_reclaim_slots() {
        let owner = PaneProcessRetirementOwner::default();
        let reservations = (0..64)
            .map(|_| owner.reserve().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(owner.outstanding(), 64);
        drop(reservations);
        assert_eq!(owner.outstanding(), 0);
        assert_eq!(owner.state().slots.capacity(), 0);
        drop(owner.reserve().unwrap());
        assert_eq!(owner.outstanding(), 0);
    }
}
