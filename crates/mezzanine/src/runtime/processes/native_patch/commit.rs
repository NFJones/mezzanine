//! Nonblocking actor cancellation and exclusive native patch commit admission.
//!
//! One coordinator is explicitly shared by conflicting runtime writers. Each
//! admitted action owns a fresh fence, deadline and cancellation identity. The
//! actor closes a fence without waiting for a filesystem operation; a live guard
//! is reported as in flight, never as stopped. Workers recheck cancellation
//! after announcing their guard, so an acknowledged fence cannot race a later
//! commit admission. This is scheduling, not filesystem or policy authority.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::error::{MezError, Result};
use crate::runtime::processes::native_cancellation::NativeActionCancellation;

/// Explicit session-local writer exclusion, without a hidden global lock.
#[derive(Clone, Default)]
pub(crate) struct NativePatchWriteCoordinator(Arc<AtomicBool>);

/// Exact action's immutable deadline and monotonic commit fence.
#[derive(Clone)]
pub(crate) struct NativePatchCommitFence {
    /// Shared actor/worker cancellation for this exact admitted dispatch.
    cancellation: NativeActionCancellation,
    /// Finite patch budget, independently capped by the owning turn deadline.
    deadline: Instant,
    /// Positive admission evidence retained until the commit guard is dropped.
    in_flight: Arc<AtomicBool>,
}

/// Cancellation observation, intentionally distinct from mutation certainty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativePatchFenceStatus {
    /// No commit guard is active and later admissions are fenced out.
    Fenced,
    /// An admitted commit still needs its actual result; it may already write.
    InFlight,
}

/// Exclusive scheduling lease; permission and file checks remain mandatory.
pub(crate) struct NativePatchCommitGuard {
    /// Exact action whose in-flight observation this guard owns.
    in_flight: Arc<AtomicBool>,
    /// Explicit writer coordinator held until actual commit processing ends.
    writer: Arc<AtomicBool>,
}

impl NativePatchCommitFence {
    /// Creates a fresh action fence with a maximum thirty-second patch budget.
    /// A prior turn deadline is preserved; an elapsed deadline denies admission.
    pub(crate) fn new(
        cancellation: NativeActionCancellation,
        turn_deadline: Option<Instant>,
    ) -> Result<Self> {
        let patch_deadline = Instant::now()
            .checked_add(Duration::from_secs(30))
            .ok_or_else(|| MezError::invalid_state("native patch deadline overflow"))?;
        Ok(Self {
            cancellation,
            deadline: turn_deadline.map_or(patch_deadline, |turn| turn.min(patch_deadline)),
            in_flight: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Checks bounded work between snapshot, planning and staging steps.
    /// This cannot interrupt an already-blocked filesystem operation.
    pub(crate) fn check(&self) -> Result<()> {
        if self.cancellation.flag().load(Ordering::SeqCst) {
            return Err(MezError::conflict("native patch action cancelled"));
        }
        if Instant::now() >= self.deadline {
            return Err(MezError::conflict("native patch deadline elapsed"));
        }
        Ok(())
    }

    /// Tries to enter an exclusive commit without waiting on another writer.
    /// The caller must retain the guard across current authority, physical
    /// preimage revalidation and publication, and record the actual outcome.
    /// A guard itself grants no permission and never certifies a mutation.
    pub(crate) fn try_begin(
        &self,
        coordinator: &NativePatchWriteCoordinator,
    ) -> Result<NativePatchCommitGuard> {
        self.check()?;
        coordinator
            .0
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| MezError::conflict("native patch writer is busy"))?;
        if self
            .in_flight
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            coordinator.0.store(false, Ordering::SeqCst);
            return Err(MezError::conflict("native patch action already committing"));
        }
        let guard = NativePatchCommitGuard {
            in_flight: Arc::clone(&self.in_flight),
            writer: Arc::clone(&coordinator.0),
        };
        // Announce before the last cancellation check. If the actor previously
        // observed Fenced, this check rejects us. Otherwise it must see a live
        // guard and report InFlight instead of acknowledging stopped effects.
        self.check()?;
        Ok(guard)
    }

    /// Binds an actor-issued guard to this exact action fence allocation.
    pub(crate) fn owns_guard(&self, guard: &NativePatchCommitGuard) -> bool {
        Arc::ptr_eq(&self.in_flight, &guard.in_flight)
    }

    /// Monotonically fences future commits without blocking the actor.
    /// InFlight requires later actual completion evidence; Fenced says nothing
    /// about effects confirmed before this call or about snapshot worker exit.
    pub(crate) fn cancel(&self) -> NativePatchFenceStatus {
        self.cancellation.cancel();
        if self.in_flight.load(Ordering::SeqCst) {
            NativePatchFenceStatus::InFlight
        } else {
            NativePatchFenceStatus::Fenced
        }
    }
}

impl Drop for NativePatchCommitGuard {
    /// Releases only this admitted guard, even on an error or worker unwind.
    fn drop(&mut self) {
        self.in_flight.store(false, Ordering::SeqCst);
        self.writer.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests;
