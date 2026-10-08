//! Retained lifetime evidence for a natively observed pane ancestry chain.
//!
//! A successful two-pass ancestry walk alone expires when an intermediate parent
//! exits. Capture each selected parent through the same bracketing used for a
//! direct helper parent, retaining exact kernel lifetimes rather than reopening
//! numeric PIDs at settlement. Actor/lifecycle checks poll these bounded anchors
//! without walking procfs. This detects ancestor exit/reparenting on Linux; it
//! does not provide an atomic process-tree snapshot, executable attestation or
//! macOS admission. Pending work and live registrations share one explicit finite
//! descriptor budget, with RAII release on failed capture, retirement and drop.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use super::parent;
#[cfg(any(target_os = "linux", test))]
use super::unavailable;
use super::{UnixOriginProcess, io, parent::UnixParentProcess};
use mez_mux::process::ProcessParentIdentity;

/// Aggregate upper bound on ancestry descriptors in one runtime, not per run.
#[cfg(any(target_os = "linux", test))]
const MAX_RETAINED_ANCESTORS: usize = 512;
/// Cooperative total capture budget, including the initial native chain walk.
#[cfg(target_os = "linux")]
const CAPTURE_BUDGET: Duration = Duration::from_millis(100);

/// Explicit shared accounting for pending and committed ancestry witnesses.
#[derive(Debug, Default)]
pub(crate) struct UnixAncestryBudget {
    reserved: AtomicUsize,
}

impl UnixAncestryBudget {
    /// Reserves the complete descriptor count before any opens. Exhaustion is
    /// nonblocking and cannot evict a current run or partially admit a chain.
    #[cfg(any(target_os = "linux", test))]
    fn reserve(self: &Arc<Self>, count: usize) -> io::Result<AncestryReservation> {
        if count == 0 {
            return Err(unavailable());
        }
        self.reserved
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |used| {
                used.checked_add(count)
                    .filter(|next| *next <= MAX_RETAINED_ANCESTORS)
            })
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "Unix ancestry capacity unavailable",
                )
            })?;
        Ok(AncestryReservation {
            budget: self.clone(),
            count,
        })
    }

    /// Reports exact live test-owned reservations, without opening descriptors.
    #[cfg(test)]
    pub(crate) fn reserved(&self) -> usize {
        self.reserved.load(Ordering::SeqCst)
    }

    /// Deterministic full-capacity injection exercises admission cleanup without
    /// exhausting unrelated process descriptors in a parallel test suite.
    #[cfg(test)]
    pub(crate) fn reserve_for_tests(
        self: &Arc<Self>,
        count: usize,
    ) -> io::Result<AncestryReservation> {
        self.reserve(count)
    }
}

/// Releases one complete successful reservation exactly once at final drop.
#[derive(Debug)]
pub(crate) struct AncestryReservation {
    budget: Arc<UnixAncestryBudget>,
    count: usize,
}

impl Drop for AncestryReservation {
    /// Each guard owns the count it atomically reserved, so subtraction cannot
    /// underflow; clones share the witness, not duplicate reservation guards.
    fn drop(&mut self) {
        self.budget.reserved.fetch_sub(self.count, Ordering::SeqCst);
    }
}

/// Exact parent lifetimes through the pane root; the socket owns origin lifetime.
#[derive(Debug)]
pub(crate) struct UnixAncestryWitness {
    parents: Vec<UnixParentProcess>,
    // Keep this last so descriptor destruction precedes budget release.
    _reservation: AncestryReservation,
}

impl UnixAncestryWitness {
    /// Nonblocking kernel polls only. Any retained ancestor exit invalidates the
    /// whole relationship even if root, producer and immediate parent survive.
    pub(crate) fn is_live(&self) -> bool {
        !self.parents.is_empty() && self.parents.iter().all(UnixParentProcess::is_live)
    }
}

impl UnixOriginProcess {
    /// Captures every ancestor through exact native child-parent relationships
    /// off actor. Original socket-origin proof remains required independently;
    /// numeric opens are only for already bracketed parent relationships. Missing
    /// evidence, identity/UID change, bounds, capacity or unsupported OS reject.
    pub(crate) fn capture_ancestry(
        &self,
        root: ProcessParentIdentity,
        budget: &Arc<UnixAncestryBudget>,
    ) -> io::Result<UnixAncestryWitness> {
        #[cfg(target_os = "linux")]
        {
            let started = Instant::now();
            let origin = self.reobserve()?;
            if origin.process_id == root.process_id {
                return Err(unavailable());
            }
            let chain =
                mez_mux::process::process_ancestry(origin, root).map_err(|_| unavailable())?;
            if started.elapsed() >= CAPTURE_BUDGET {
                return Err(unavailable());
            }
            let reservation =
                budget.reserve(chain.chain.len().checked_sub(1).ok_or_else(unavailable)?)?;
            let mut parents: Vec<UnixParentProcess> = Vec::new();
            for expected in chain.chain.iter().skip(1) {
                let (identity, lifetime) = parent::capture_parent_with(
                    self.uid(),
                    || {
                        parents
                            .last()
                            .map_or_else(|| self.reobserve(), UnixParentProcess::reobserve)
                    },
                    |pid| {
                        mez_mux::process::process_parent_identity_for_pid(pid)
                            .ok_or_else(unavailable)
                    },
                    parent::parent_uid,
                    parent::open_parent,
                    super::require_live_origin,
                    || started.elapsed() >= CAPTURE_BUDGET,
                )?;
                if identity != *expected {
                    return Err(unavailable());
                }
                parents.push(UnixParentProcess {
                    uid: self.uid(),
                    identity,
                    lifetime,
                });
            }
            // Root-first native reobservation brackets all retained descriptors,
            // then the original socket fence checks the producer once more.
            for retained in parents.iter().rev() {
                retained.reobserve()?;
                if started.elapsed() >= CAPTURE_BUDGET {
                    return Err(unavailable());
                }
            }
            self.reobserve()?;
            let witness = UnixAncestryWitness {
                parents,
                _reservation: reservation,
            };
            if !witness.is_live() || started.elapsed() >= CAPTURE_BUDGET {
                return Err(unavailable());
            }
            Ok(witness)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (root, budget);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Unix ancestry lifetime capture unsupported",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The budget is aggregate, nonblocking and checked for overflow. Dropping
    /// one reservation restores only its own capacity; no current grant is evicted.
    #[test]
    fn unix_ancestry_budget_is_bounded_and_releases_exact_reservations() {
        let budget = Arc::new(UnixAncestryBudget::default());
        assert!(budget.reserve(0).is_err());
        let first = budget.reserve(128).unwrap();
        let second = budget.reserve(384).unwrap();
        assert_eq!(budget.reserved(), MAX_RETAINED_ANCESTORS);
        assert_eq!(
            budget.reserve(1).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(budget.reserve(usize::MAX).is_err());
        drop(first);
        assert_eq!(budget.reserved(), 384);
        let replacement = budget.reserve(128).unwrap();
        drop(second);
        assert_eq!(budget.reserved(), 128);
        drop(replacement);
        assert_eq!(budget.reserved(), 0);
    }
}
