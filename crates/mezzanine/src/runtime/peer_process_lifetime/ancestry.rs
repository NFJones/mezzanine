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

/// Exact retained lifetimes through the pane root. Socket-backed witnesses rely
/// on their original origin owner; parent-backed witnesses own the source too.
#[derive(Debug)]
pub(crate) struct UnixAncestryWitness {
    /// Parent-backed capture owns an exact duplicate of the verified source;
    /// socket-backed capture keeps source lifetime in its original origin owner.
    source: Option<UnixParentProcess>,
    parents: Vec<UnixParentProcess>,
    // Keep this last so descriptor destruction precedes budget release.
    _reservation: AncestryReservation,
}

impl UnixAncestryWitness {
    /// Test-only exact ancestor completion probe. Returns None for an uncaptured
    /// native record rather than inferring lifetime from a supplied numeric PID;
    /// fixture reparenting alone is not the retained pidfd completion boundary.
    #[cfg(test)]
    pub(crate) fn test_ancestor_is_live(&self, identity: ProcessParentIdentity) -> Option<bool> {
        self.parents
            .iter()
            .find(|parent| parent.identity == identity)
            .map(UnixParentProcess::is_live)
    }

    /// Verifies a parent-backed witness retains the exact code-owned producer
    /// UID/birth/relationship record, not merely a living root/ancestor chain.
    /// This selector comparison grants no sender or enrollment authority.
    pub(crate) fn source_matches(&self, uid: u32, identity: ProcessParentIdentity) -> bool {
        self.source
            .as_ref()
            .is_some_and(|source| source.uid() == uid && source.identity == identity)
    }

    /// Nonblocking kernel polls only. Any retained ancestor exit invalidates the
    /// whole relationship even if root, producer and immediate parent survive.
    pub(crate) fn is_live(&self) -> bool {
        self.source.as_ref().is_none_or(UnixParentProcess::is_live)
            && !self.parents.is_empty()
            && self.parents.iter().all(UnixParentProcess::is_live)
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
            capture_retained_ancestry(self.uid(), || self.reobserve(), None, root, budget)
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

impl UnixParentProcess {
    /// Captures this already-native-qualified parent's source/root chain off
    /// actor, independent of helper survival. The original descriptor is never
    /// reopened numerically; a CLOEXEC duplicate and every ancestor share the
    /// finite budget. No sender/vendor/client/session or enrollment authority is
    /// inferred. Source-as-root, changed/unreadable evidence or unsupported OS fail.
    pub(crate) fn capture_ancestry(
        &self,
        root: ProcessParentIdentity,
        budget: &Arc<UnixAncestryBudget>,
    ) -> io::Result<UnixAncestryWitness> {
        #[cfg(target_os = "linux")]
        {
            capture_retained_ancestry(self.uid(), || self.reobserve(), Some(self), root, budget)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (root, budget);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Unix parent ancestry capture unsupported",
            ))
        }
    }
}

/// Shares one bracketed native walk between two already-retained evidence owners.
/// UID/reobservation closures are code-owned, not payload claims. Source duplication
/// is charged before opening and preserves the exact original kernel process.
#[cfg(target_os = "linux")]
fn capture_retained_ancestry(
    uid: u32,
    observe: impl Fn() -> io::Result<ProcessParentIdentity>,
    retained_source: Option<&UnixParentProcess>,
    root: ProcessParentIdentity,
    budget: &Arc<UnixAncestryBudget>,
) -> io::Result<UnixAncestryWitness> {
    let started = Instant::now();
    let origin = observe()?;
    if origin.process_id == root.process_id {
        return Err(unavailable());
    }
    let chain = mez_mux::process::process_ancestry(origin, root).map_err(|_| unavailable())?;
    if started.elapsed() >= CAPTURE_BUDGET {
        return Err(unavailable());
    }
    let count = chain
        .chain
        .len()
        .checked_sub(1)
        .and_then(|count| count.checked_add(usize::from(retained_source.is_some())))
        .ok_or_else(unavailable)?;
    let reservation = budget.reserve(count)?;
    let source = if let Some(retained) = retained_source {
        if retained.identity != origin || retained.uid() != uid {
            return Err(unavailable());
        }
        let duplicate = UnixParentProcess {
            uid,
            identity: origin,
            lifetime: retained.lifetime.try_clone()?,
        };
        if started.elapsed() >= CAPTURE_BUDGET {
            return Err(unavailable());
        }
        duplicate.reobserve()?;
        if started.elapsed() >= CAPTURE_BUDGET {
            return Err(unavailable());
        }
        if observe()? != origin || started.elapsed() >= CAPTURE_BUDGET {
            return Err(unavailable());
        }
        Some(duplicate)
    } else {
        None
    };
    let mut parents: Vec<UnixParentProcess> = Vec::new();
    for expected in chain.chain.iter().skip(1) {
        let (identity, lifetime) = parent::capture_parent_with(
            uid,
            || {
                parents
                    .last()
                    .map_or_else(&observe, UnixParentProcess::reobserve)
            },
            |pid| mez_mux::process::process_parent_identity_for_pid(pid).ok_or_else(unavailable),
            parent::parent_uid,
            parent::open_parent,
            super::require_live_origin,
            || started.elapsed() >= CAPTURE_BUDGET,
        )?;
        if identity != *expected {
            return Err(unavailable());
        }
        parents.push(UnixParentProcess {
            uid,
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
    if observe()? != origin || started.elapsed() >= CAPTURE_BUDGET {
        return Err(unavailable());
    }
    if let Some(retained) = &source {
        retained.reobserve()?;
    }
    let witness = UnixAncestryWitness {
        source,
        parents,
        _reservation: reservation,
    };
    if !witness.is_live() || started.elapsed() >= CAPTURE_BUDGET {
        return Err(unavailable());
    }
    Ok(witness)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A code-owned current-process fence permits deterministic read failures
    /// after duplication and midway through bracketing without racing a fixture.
    /// This test-only numeric open is not a production origin fallback. Every
    /// failed capture releases its exact charge; success duplicates CLOEXEC and
    /// survives dropping the caller's original descriptor until last witness drop.
    #[test]
    #[cfg(target_os = "linux")]
    fn unix_parent_ancestry_failure_after_duplication_reclaims_budget() {
        use std::os::fd::AsFd;
        let identity =
            mez_mux::process::process_parent_identity_for_pid(std::process::id()).unwrap();
        let root =
            mez_mux::process::process_parent_identity_for_pid(identity.parent_process_id).unwrap();
        let uid = parent::parent_uid(identity.process_id).unwrap();
        let source = UnixParentProcess {
            uid,
            identity,
            lifetime: parent::open_parent(identity.process_id).unwrap(),
        };
        let budget = Arc::new(UnixAncestryBudget::default());
        for fail_at in [2, 3, 4] {
            let reads = std::cell::Cell::new(0);
            let result = capture_retained_ancestry(
                uid,
                || {
                    reads.set(reads.get() + 1);
                    if reads.get() == fail_at {
                        return Err(unavailable());
                    }
                    source.reobserve()
                },
                Some(&source),
                root,
                &budget,
            );
            assert!(result.is_err());
            assert_eq!(budget.reserved(), 0);
        }
        let witness = source.capture_ancestry(root, &budget).unwrap();
        assert_eq!(budget.reserved(), 2);
        let duplicate = witness.source.as_ref().unwrap();
        assert!(
            rustix::io::fcntl_getfd(duplicate.lifetime.as_fd())
                .unwrap()
                .contains(rustix::io::FdFlags::CLOEXEC)
        );
        assert_eq!(duplicate.identity, source.identity);
        drop(source);
        assert!(witness.is_live());
        drop(witness);
        assert_eq!(budget.reserved(), 0);
    }

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
