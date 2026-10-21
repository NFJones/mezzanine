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

#[cfg(test)]
use std::sync::atomic::AtomicU64;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
#[cfg(any(target_os = "linux", test))]
use std::time::Duration;
#[cfg(target_os = "linux")]
use std::time::Instant;

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
    /// Test-only per-instance capture budget in microseconds; zero uses the
    /// production 100 ms policy so other fixtures and production stay strict.
    #[cfg(test)]
    #[allow(dead_code)]
    capture_budget_us: AtomicU64,
    /// Test-only checkpoint number at which the next capture must expire.
    #[cfg(test)]
    #[allow(dead_code)]
    capture_expiry_check: AtomicUsize,
    /// Test-only checkpoint that actually forced expiry, zero when none did.
    #[cfg(test)]
    #[allow(dead_code)]
    capture_expiry_reached: AtomicUsize,
    /// Test-only live reservation count observed at forced expiry.
    #[cfg(test)]
    #[allow(dead_code)]
    capture_expiry_reserved_at: AtomicUsize,
}

impl UnixAncestryBudget {
    /// Reserves the complete descriptor count before any opens. Exhaustion is
    /// nonblocking and cannot evict a current run or partially admit a chain.
    #[cfg(any(target_os = "linux", test))]
    fn reserve(self: &Arc<Self>, count: usize) -> io::Result<AncestryReservation> {
        if count == 0 {
            return Err(unavailable());
        }
        let mut used = self.reserved.load(Ordering::SeqCst);
        loop {
            let next = used
                .checked_add(count)
                .filter(|next| *next <= MAX_RETAINED_ANCESTORS)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "Unix ancestry capacity unavailable",
                    )
                })?;
            match self.reserved.compare_exchange_weak(
                used,
                next,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(current) => used = current,
            }
        }
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

    /// Overrides only this test-owned budget instance; production and every
    /// other budget keep the fixed capture deadline.
    #[cfg(test)]
    pub(crate) fn set_capture_budget_for_tests(&self, budget: Duration) {
        self.capture_budget_us.store(
            u64::try_from(budget.as_micros()).unwrap_or(u64::MAX),
            Ordering::SeqCst,
        );
    }

    /// Returns the production capture deadline unless this test instance opted
    /// into a finite override.
    #[cfg(target_os = "linux")]
    fn active_capture_budget(&self) -> Duration {
        #[cfg(test)]
        {
            let micros = self.capture_budget_us.load(Ordering::SeqCst);
            if micros > 0 {
                return Duration::from_micros(micros);
            }
        }
        CAPTURE_BUDGET
    }

    /// Forces the next real capture to expire at one known checkpoint, so a
    /// control does not depend on native syscalls happening to exceed a clock.
    #[cfg(test)]
    pub(crate) fn force_capture_expiry_at_check_for_tests(&self, check: usize) {
        self.capture_expiry_check.store(check, Ordering::SeqCst);
        self.capture_expiry_reached.store(0, Ordering::SeqCst);
        self.capture_expiry_reserved_at.store(0, Ordering::SeqCst);
    }

    /// Returns the checkpoint that forced expiry and the reservation count seen
    /// there; zeroes mean no forced expiry was observed.
    #[cfg(test)]
    pub(crate) fn capture_expiry_observation_for_tests(&self) -> (usize, usize) {
        (
            self.capture_expiry_reached.load(Ordering::SeqCst),
            self.capture_expiry_reserved_at.load(Ordering::SeqCst),
        )
    }

    /// Evaluates one numbered capture checkpoint, honoring the test-only forced
    /// checkpoint before the real elapsed budget.
    #[cfg(target_os = "linux")]
    fn capture_expired(&self, started: Instant, check: usize, reserved: usize) -> bool {
        #[cfg(test)]
        {
            if self.capture_expiry_check.load(Ordering::SeqCst) == check {
                self.capture_expiry_reached.store(check, Ordering::SeqCst);
                self.capture_expiry_reserved_at
                    .store(reserved, Ordering::SeqCst);
                return true;
            }
        }
        started.elapsed() >= self.active_capture_budget()
    }

    /// Deterministic full-capacity injection exercises admission cleanup without
    /// exhausting unrelated process descriptors in a parallel test suite.
    #[cfg(all(test, target_os = "linux"))]
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
    #[cfg(all(test, target_os = "linux"))]
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
    if budget.capture_expired(started, 1, 0) {
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
        if budget.capture_expired(started, 2, count) {
            return Err(unavailable());
        }
        duplicate.reobserve()?;
        if budget.capture_expired(started, 3, count) {
            return Err(unavailable());
        }
        if observe()? != origin || budget.capture_expired(started, 4, count) {
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
            || budget.capture_expired(started, 5, count),
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
        if budget.capture_expired(started, 6, count) {
            return Err(unavailable());
        }
    }
    if observe()? != origin || budget.capture_expired(started, 7, count) {
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
    if !witness.is_live() || budget.capture_expired(started, 8, count) {
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

    /// Competing whole-chain reservations cannot overbook the aggregate budget.
    /// Keep every successful guard until all workers finish, then prove each
    /// charge is released exactly once and the entire capacity is reusable.
    #[test]
    fn unix_ancestry_budget_concurrent_reservations_never_overbook() {
        let budget = Arc::new(UnixAncestryBudget::default());
        let barrier = std::sync::Barrier::new(16);
        let reservations = std::thread::scope(|scope| {
            let workers = (0..16)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        budget.reserve(64).ok()
                    })
                })
                .collect::<Vec<_>>();
            workers
                .into_iter()
                .filter_map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(reservations.len(), 8);
        assert_eq!(budget.reserved(), MAX_RETAINED_ANCESTORS);
        drop(reservations);
        assert_eq!(budget.reserved(), 0);
        drop(budget.reserve(MAX_RETAINED_ANCESTORS).unwrap());
        assert_eq!(budget.reserved(), 0);
    }

    /// Test overrides are instance-local; production and every other budget
    /// instance retain the fixed 100 ms capture deadline.
    #[test]
    #[cfg(target_os = "linux")]
    fn unix_ancestry_capture_budget_override_is_instance_local() {
        let budget = Arc::new(UnixAncestryBudget::default());
        assert_eq!(budget.active_capture_budget(), CAPTURE_BUDGET);
        budget.set_capture_budget_for_tests(Duration::from_secs(10));
        assert_eq!(budget.active_capture_budget(), Duration::from_secs(10));
        let other = Arc::new(UnixAncestryBudget::default());
        assert_eq!(other.active_capture_budget(), CAPTURE_BUDGET);
        assert_eq!(other.reserved(), 0);
    }

    /// Forced checkpoint expiry is deterministic: the pre-reservation checkpoint
    /// rejects without a charge, and the post-reservation checkpoint proves the
    /// acquired real reservation is released on return.
    #[test]
    #[cfg(target_os = "linux")]
    fn unix_ancestry_checkpoint_expiry_distinguishes_pre_and_post_reservation() {
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
        budget.force_capture_expiry_at_check_for_tests(1);
        let early =
            capture_retained_ancestry(uid, || source.reobserve(), Some(&source), root, &budget);
        assert_eq!(early.unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(budget.capture_expiry_observation_for_tests(), (1, 0));
        assert_eq!(budget.reserved(), 0);
        budget.force_capture_expiry_at_check_for_tests(2);
        let late =
            capture_retained_ancestry(uid, || source.reobserve(), Some(&source), root, &budget);
        assert_eq!(late.unwrap_err().kind(), io::ErrorKind::NotFound);
        let (check, reserved_at) = budget.capture_expiry_observation_for_tests();
        assert_eq!(check, 2);
        assert!(
            reserved_at > 0,
            "post-reservation expiry must observe a live charge"
        );
        assert_eq!(budget.reserved(), 0);
    }
}
