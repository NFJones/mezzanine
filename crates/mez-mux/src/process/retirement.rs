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

use std::io;
use std::io::Write;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use portable_pty::{Child, ExitStatus};
use rustix::process::Signal;

use super::signals::send_signal_to_retained_pane_group as send_signal_to_pane_process_group;
use crate::{MuxError as MezError, Result};

/// Session-scoped owner retained independently of abortable pane workers.
#[derive(Clone, Debug, Default)]
pub struct PaneProcessRetirementOwner {
    inner: Arc<Mutex<RetirementState>>,
}

/// Preallocated slots contain raw resources only, never a guard/owner cycle.
#[derive(Debug, Default)]
struct RetirementState {
    slots: Vec<Option<ChildSlot>>,
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
    group: Option<i32>,
    /// Successful root signal is not a wait receipt, but avoids repeat signals.
    force_requested: bool,
    progress: NativeTerminationProgress,
    failure_latched: bool,
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
        for slot in &mut self.slots {
            if let Some(ChildSlot::Returned(raw)) = slot {
                // Poll before any numeric group signal; an already reaped PID
                // must never authorize signaling a reused group.
                if matches!(raw.child.try_wait(), Ok(None)) {
                    if let Some(group) = raw.group {
                        let _ = send_signal_to_pane_process_group(group, Signal::KILL);
                    }
                    let _ = raw.child.kill();
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

    /// Serializes bounded drains without holding a mutex across native I/O.
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
        })
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
        if let Some(index) = index {
            self.owner.state().slots[index] = Some(ChildSlot::Leased);
        }
        RetainedPaneChild {
            raw: Some(RawPaneChild {
                child,
                group,
                force_requested: false,
                progress: NativeTerminationProgress::default(),
                failure_latched: false,
            }),
            reservation: index.map(|index| (self.owner.clone(), index)),
            status: None,
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
}

impl PaneProcessRetirementDrain {
    /// Claims one exact returned child using its existing reservation.
    pub fn take_returned(&mut self) -> Option<RetainedPaneChild> {
        let mut state = self.owner.state();
        let len = state.slots.len();
        for offset in 0..len {
            let index = (self.cursor + offset) % len;
            let slot = &mut state.slots[index];
            if matches!(slot, Some(ChildSlot::Returned(_))) {
                if let Some(ChildSlot::Returned(raw)) = slot.take() {
                    *slot = Some(ChildSlot::Draining);
                    self.cursor = (index + 1) % len;
                    return Some(RetainedPaneChild {
                        raw: Some(raw),
                        reservation: Some((self.owner.clone(), index)),
                        status: None,
                    });
                }
            }
        }
        None
    }
}

impl Drop for PaneProcessRetirementDrain {
    /// Releases drain serialization, not native ownership or admission sealing.
    fn drop(&mut self) {
        let mut state = self.owner.state();
        state.drain_cursor = self.cursor;
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
            owner.state().slots[index] = Some(ChildSlot::Leased);
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
                group,
                force_requested: false,
                progress: NativeTerminationProgress::default(),
                failure_latched: false,
            }),
            reservation: None,
            status: None,
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
        if self.try_wait()?.is_none() {
            if let Err(error) = self.raw_mut()?.child.kill() {
                // An authoritative exact-child wait can win an already-exited
                // root race; never classify benignness by error text.
                if let Ok(Some(status)) = self.raw_mut()?.child.try_wait() {
                    self.record_wait(status);
                    return Ok(());
                }
                self.record_failure("native root signal", &error);
                return Err(error);
            }
        }
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

    /// Starts/continues the same exact-child timeline across every handoff.
    pub(super) fn begin_termination(&mut self, force: bool) -> Result<Instant> {
        let shared = self
            .reservation
            .as_ref()
            .and_then(|(owner, _)| owner.state().shutdown);
        let raw = self.raw_mut()?;
        let plan = raw.progress.plan.get_or_insert_with(|| {
            shared.unwrap_or_else(|| {
                let started = Instant::now();
                NativeShutdownPlan {
                    started,
                    deadline: started + Duration::from_secs(if force { 1 } else { 2 }),
                    force,
                }
            })
        });
        if let Some(shared) = shared {
            plan.started = plan.started.min(shared.started);
            plan.deadline = plan.deadline.min(shared.deadline);
            plan.force |= shared.force;
        }
        plan.force |= force;
        Ok(plan.deadline)
    }

    /// Records signals issued by the synchronous process path without replay.
    pub(super) fn record_signal(&mut self, signal: Signal) -> Result<()> {
        self.begin_termination(signal == Signal::KILL)?;
        let progress = &mut self.raw_mut()?.progress;
        if signal == Signal::HUP {
            progress.hup_sent = true;
        }
        if signal == Signal::TERM {
            progress.term_sent = true;
        }
        Ok(())
    }

    /// Advances HUP/TERM/KILL without sleeps, preserving prior sent stages.
    /// A successful native wait settles the slot; a signal never does.
    pub fn poll_termination(&mut self, force: bool) -> Result<Option<ExitStatus>> {
        if let Some(status) = self.try_wait()? {
            return Ok(Some(status));
        }
        self.begin_termination(force)?;
        let raw = self.raw_mut()?;
        // A live guard's begin_termination always installs this plan.
        let plan = raw
            .progress
            .plan
            .ok_or_else(|| MezError::invalid_state("native termination plan is missing"))?;
        let elapsed = plan.started.elapsed();
        if plan.force || elapsed >= Duration::from_secs(1) {
            self.force_retirement()?;
        } else {
            let signal = if !raw.progress.hup_sent {
                raw.progress.hup_sent = true;
                Some(Signal::HUP)
            } else if elapsed >= Duration::from_millis(500) && !raw.progress.term_sent {
                raw.progress.term_sent = true;
                Some(Signal::TERM)
            } else {
                None
            };
            if let (Some(signal), Some(group)) = (signal, raw.group) {
                if let Err(error) = send_signal_to_pane_process_group(group, signal) {
                    self.record_failure("native group signal", &error);
                    return Err(error);
                }
            }
        }
        self.try_wait().map_err(Into::into)
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
        let group = self.raw_mut()?.group;
        let group_result = group.map_or(Ok(()), |group| {
            send_signal_to_pane_process_group(group, Signal::KILL)
        });
        // Root polling may release this slot immediately. Preserve an actual
        // group failure before that winning wait, not only in the caller future.
        if let Err(error) = &group_result {
            self.record_failure("native group signal", error);
        }
        let root_result = self.kill().map_err(MezError::from);
        if root_result.is_ok() {
            self.raw_mut()?.force_requested = true;
        }
        let result = match (group_result, root_result) {
            (Err(group), Err(root)) => Err(MezError::invalid_state(format!(
                "{group}; owned root cleanup also failed: {root}"
            ))),
            (Err(error), _) | (_, Err(error)) => Err(error),
            _ => Ok(()),
        };
        if let Err(error) = &result {
            self.record_failure("native group/root escalation", error);
        }
        result
    }
}

impl Drop for RetainedPaneChild {
    /// Returns raw resources to the already allocated exact slot, without I/O.
    fn drop(&mut self) {
        if let Some((owner, index)) = self.reservation.take() {
            if let Some(raw) = self.raw.take() {
                owner.state().slots[index] = Some(ChildSlot::Returned(raw));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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

    /// Missing PID does not invalidate exact cleanup authority. A reservation
    /// made before seal still attaches; native wait releases it immediately,
    /// repeated polls use cached status, and terminal seal never reopens.
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
        child.force_retirement().unwrap();
        assert!(child.try_wait().unwrap().is_some());
        let polls = state.polls.load(Ordering::SeqCst);
        assert!(child.try_wait().unwrap().is_some());
        assert_eq!(state.polls.load(Ordering::SeqCst), polls);
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
                .attach(Box::new(ControlledChild(state)), None),
        );
        let mut first = owner.begin_drain().unwrap();
        let child = first.take_returned().unwrap();
        drop(first);
        assert!(owner.begin_drain().is_err());
        assert_eq!(owner.outstanding(), 1);
        drop(child);
        let mut retry = owner.begin_drain().unwrap();
        let mut child = retry.take_returned().unwrap();
        child.force_retirement().unwrap();
        assert!(child.try_wait().unwrap().is_some());
        assert_eq!(owner.outstanding(), 0);
    }

    /// Group failure cannot short-circuit exact-root kill and wait. The failed
    /// group is an invalid synthetic identifier, so no host process is targeted.
    #[test]
    fn group_failure_still_attempts_owned_root_and_releases_only_after_wait() {
        let owner = PaneProcessRetirementOwner::default();
        let state = Arc::new(ChildState::default());
        let mut child = owner
            .reserve()
            .unwrap()
            .attach(Box::new(ControlledChild(state.clone())), Some(-1));
        assert!(child.force_retirement().is_err());
        assert_eq!(state.kills.load(Ordering::SeqCst), 1);
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
        assert!(child.poll_cleanup().unwrap().is_none());
        assert_eq!(state.kills.load(Ordering::SeqCst), 0);
        assert!(child.raw.as_ref().unwrap().progress.hup_sent);
        drop(child);
        owner.request_shutdown(true);
        assert_eq!(owner.cleanup_deadline(), original);
        let mut drain = owner.begin_drain().unwrap();
        let mut child = drain.take_returned().unwrap();
        assert!(child.poll_cleanup().unwrap().is_some());
        assert_eq!(state.kills.load(Ordering::SeqCst), 1);
        assert_eq!(owner.outstanding(), 0);
    }

    /// Final-owner abandonment with a known live child and invalid group must
    /// return a typed group error internally, still attempt the exact root and
    /// destroy resources without panicking or forming a cycle. This tests only
    /// best-effort disposal, deliberately not a successful native wait receipt.
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
        assert_eq!(state.kills.load(Ordering::SeqCst), 1);
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

    /// The same nonblocking path used by the async backend must kill its exact
    /// root even when group metadata names another exclusively test-owned root.
    /// Both owned children are waited/reaped; no process discovery is involved.
    #[test]
    fn retained_native_escalation_kills_exact_root_with_different_owned_group() {
        let mut manager = super::super::PaneProcessManager::new();
        let environment = super::super::PaneProcessEnvironment {
            mez: "native-test".into(),
            session: "native-test".into(),
            window: "@native".into(),
            pane: "%native".into(),
            term: "xterm".into(),
        };
        let launch = super::super::PaneProcessLaunch::new("/bin/sh".into());
        let size = mez_terminal::TerminalSize::new(80, 24).unwrap();
        let root_pid = manager
            .spawn_for_pane("%root", &launch, Some("sleep 5"), &environment, size)
            .unwrap();
        let other_pid = manager
            .spawn_for_pane("%other", &launch, Some("sleep 5"), &environment, size)
            .unwrap();
        let owner = manager.retirement_owner();
        let mut root = manager.take_running_pane_process("%root").unwrap();
        let mut other = manager.take_running_pane_process("%other").unwrap();
        root.child.raw.as_mut().unwrap().group = Some(i32::try_from(other_pid).unwrap());
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if root.poll_native_termination(true).unwrap().is_some() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "exact native root did not retire"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        loop {
            if other.poll_exit().unwrap().is_some() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "owned signal target did not retire"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        for pid in [root_pid, other_pid] {
            let pid = rustix::process::Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
            assert_eq!(
                rustix::process::test_kill_process(pid),
                Err(rustix::io::Errno::SRCH)
            );
        }
        assert_eq!(
            owner.outstanding(),
            0,
            "native wait releases slots before PTY owners are dropped"
        );
    }

    /// This real child encounters an injected invalid cached group at the
    /// production signal selector. The group stage fails, but owned-root KILL
    /// still runs. Drop that caller before wait and keep a duplicate master so
    /// kernel PTY hangup cannot supply the retirement proof. The same retained
    /// child must be waited/reaped, without erasing the earlier error receipt.
    #[test]
    fn real_native_group_failure_survives_caller_drop_and_zero_pending_reap() {
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
        let pid =
            rustix::process::Pid::from_raw(i32::try_from(process.primary_pid()).unwrap()).unwrap();
        let held_master = process.duplicate_master_fd().unwrap();
        process.child.raw.as_mut().unwrap().group = Some(-1);
        let original = process.poll_native_termination(true);
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
