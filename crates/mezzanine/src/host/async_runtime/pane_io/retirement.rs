//! Bounded settlement of exact native children returned by cancelled owners.
//!
//! This is not a process manager or a detached reaper. The existing session
//! owner retains every child and preallocated return slot. One explicit drain
//! claims at most a fixed burst of those exact handles, signals their owned
//! groups/roots, and observes native waits. Cancellation drops only claim
//! guards, returning resources to the same owner for retry. Outstanding leased
//! and pre-spawn reservations participate in the barrier as well as queued
//! children, so joining an outer task cannot falsely certify nested cleanup.

use mez_mux::process::PaneProcessRetirementOwner;

use super::{Duration, Instant, MezError, Result, sleep};

/// Existing forced-native termination budget; no global shutdown extension.
pub(crate) const NATIVE_PANE_RETIREMENT_TIMEOUT: Duration = Duration::from_secs(1);
/// Bound native work per executor turn independently of admitted pane count.
const NATIVE_RETIREMENT_BURST: usize = 16;

/// Drains exact returned child handles by one finite absolute deadline.
///
/// The caller seals only for final session shutdown. A live owner may also
/// drain cancelled handoffs without sealing and resume admission once all
/// returned obligations settle. Errors retain unreaped children in `owner`;
/// even after one signal/wait failure, all other safely owned children are
/// attempted. Signal errors remain errors even if the root is later reaped.
pub(crate) async fn drain_native_pane_retirement(
    owner: &PaneProcessRetirementOwner,
    deadline: Instant,
) -> Result<()> {
    drain_native_pane_retirement_scope(owner, deadline, true).await
}

/// Settles returned failures without waiting for healthy sibling lifetimes.
pub(super) async fn drain_returned_native_panes(
    owner: &PaneProcessRetirementOwner,
    deadline: Instant,
) -> Result<()> {
    drain_native_pane_retirement_scope(owner, deadline, false).await
}

/// Performs one bounded native quantum without waiting for another owner.
/// `false` is pending/busy, never completion; every exact guard is returned
/// before yielding so host round-robin cleanup can progress owner seventeen.
pub(crate) fn advance_native_pane_retirement(
    owner: &PaneProcessRetirementOwner,
    deadline: Instant,
) -> Result<bool> {
    if owner.outstanding() == 0 {
        return retained_native_failure(owner).map_or(Ok(true), Err);
    }
    if Instant::now() >= deadline {
        return Err(MezError::invalid_state(format!(
            "native pane retirement deadline expired with {} exact owned children",
            owner.outstanding()
        )));
    }
    let Some(mut drain) = owner.try_begin_drain() else {
        return Ok(false);
    };
    let mut first_error = None;
    for _ in 0..owner.outstanding().min(NATIVE_RETIREMENT_BURST) {
        if Instant::now() >= deadline {
            break;
        }
        let Some(mut child) = drain.take_returned() else {
            break;
        };
        if let Err(error) = child.poll_cleanup() {
            first_error.get_or_insert_with(|| error.into());
        }
    }
    // Failure publication precedes the final native slot release. Observe
    // completion first, then read its monotonic receipt before healthy success.
    let completed = owner.outstanding() == 0;
    match first_error.or_else(|| retained_native_failure(owner)) {
        Some(error) => Err(error),
        None => Ok(completed),
    }
}

/// Shares bounded exact-handle I/O; only final shutdown uses the all-owner barrier.
async fn drain_native_pane_retirement_scope(
    owner: &PaneProcessRetirementOwner,
    deadline: Instant,
    all_owners: bool,
) -> Result<()> {
    drain_native_with_failure_reader(owner, deadline, all_owners, || {
        retained_native_failure(owner)
    })
    .await
}

/// Keeps the native resource algorithm identical while allowing a private,
/// per-call receipt reader in deterministic verdict-order tests. Production
/// always reads the same owner's durable native receipt; no global hook or
/// child ownership/failure mutator is exposed.
async fn drain_native_with_failure_reader(
    owner: &PaneProcessRetirementOwner,
    deadline: Instant,
    all_owners: bool,
    mut read_failure: impl FnMut() -> Option<MezError>,
) -> Result<()> {
    let mut first_error = read_failure();
    loop {
        let pending = if all_owners {
            owner.outstanding()
        } else {
            owner.returned_count()
        };
        if pending == 0 {
            // A competing worker may have published failure and released its
            // last slot during our await. Never trust the pre-await snapshot.
            return first_error.or_else(&mut read_failure).map_or(Ok(()), Err);
        }
        if Instant::now() >= deadline {
            let diagnostic = first_error
                .map(|error| format!("; first cleanup failure: {error}"))
                .unwrap_or_default();
            return Err(MezError::invalid_state(format!(
                "native pane retirement is unproven for {pending} exact owned children{diagnostic}"
            )));
        }
        if let Err(error) = advance_native_pane_retirement(owner, deadline) {
            first_error.get_or_insert(error);
        }
        let outstanding = if all_owners {
            owner.outstanding()
        } else {
            owner.returned_count()
        };
        if outstanding == 0 {
            // Route every completed path through the same fresh receipt read.
            continue;
        }
        if Instant::now() >= deadline {
            let diagnostic = first_error
                .map(|error| format!("; first cleanup failure: {error}"))
                .unwrap_or_default();
            return Err(MezError::invalid_state(format!(
                "native pane retirement is unproven for {outstanding} exact owned children{diagnostic}"
            )));
        }
        // Includes late cancellation of nested, not-yet-polled workers.
        tokio::task::yield_now().await;
        sleep(Duration::from_millis(10).min(deadline.saturating_duration_since(Instant::now())))
            .await;
    }
}

/// Converts only the owner's bounded native-stage receipt, never arbitrary
/// actor/provider errors or raw process output, into the cleanup result.
fn retained_native_failure(owner: &PaneProcessRetirementOwner) -> Option<MezError> {
    owner.cleanup_failure().map(|failure| {
        MezError::invalid_state(format!("retained native cleanup failure: {failure}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exact native root force must not block a current-thread executor on
    /// portable-pty's HUP grace. Four roots prove trap installation before the
    /// timed operation; an already-polled timer measures executor responsiveness.
    /// Held masters prevent closure from killing fixtures, and production drain
    /// plus strict native waits/SRCH settles every root before assertions.
    #[tokio::test(flavor = "current_thread")]
    async fn native_root_force_preserves_current_thread_heartbeat() {
        let owner = PaneProcessRetirementOwner::default();
        let mut processes = Vec::new();
        let mut masters = Vec::new();
        let mut pids = Vec::new();
        for _ in 0..4 {
            let mut process = mez_mux::process::spawn_retained_argv_pty_process(
                std::path::Path::new("/bin/sh"),
                &["-c".into(), "trap '' HUP TERM; printf 'heartbeat-ready\\n'; while IFS= read -r line; do :; done".into()],
                &[], super::super::Size::new(80, 24).unwrap(), None, owner.reserve().unwrap()).unwrap();
            masters.push(process.duplicate_master_fd().unwrap());
            pids.push(
                rustix::process::Pid::from_raw(i32::try_from(process.primary_pid()).unwrap())
                    .unwrap(),
            );
            let deadline = Instant::now() + Duration::from_secs(1);
            let mut output = Vec::new();
            while Instant::now() < deadline
                && !String::from_utf8_lossy(&output).contains("heartbeat-ready")
            {
                output.extend(process.read_available_output(4096).unwrap());
                sleep(Duration::from_millis(5)).await;
            }
            assert!(
                String::from_utf8_lossy(&output).contains("heartbeat-ready"),
                "HUP resistance was not installed"
            );
            processes.push(process);
        }
        let started = Instant::now();
        let heartbeat = async {
            sleep(Duration::from_millis(10)).await;
            started.elapsed()
        };
        let force = async {
            // The heartbeat branch is polled before any synchronous native I/O.
            tokio::task::yield_now().await;
            processes
                .iter_mut()
                .map(|process| process.kill_owned_root())
                .collect::<Vec<_>>()
        };
        let (heartbeat_elapsed, force_results) = tokio::join!(biased; heartbeat, force);
        drop(processes);
        let settlement =
            drain_native_pane_retirement(&owner, Instant::now() + Duration::from_secs(1)).await;
        let all_absent = pids
            .into_iter()
            .all(|pid| rustix::process::test_kill_process(pid) == Err(rustix::io::Errno::SRCH));
        drop(masters);
        assert!(
            settlement.is_ok() && owner.outstanding() == 0 && all_absent,
            "exact fixture reaping failed: {settlement:?}"
        );
        assert!(force_results.iter().all(std::result::Result::is_ok));
        assert!(
            heartbeat_elapsed < Duration::from_millis(500),
            "native force blocked executor heartbeat: {heartbeat_elapsed:?}"
        );
    }

    /// Supplies only a labelled receipt-classification fault. Native ownership,
    /// signaling and wait/reap below remain the actual production operations.
    fn competing_receipt_reader(
        published: &std::sync::atomic::AtomicBool,
        mut entered: Option<tokio::sync::oneshot::Sender<()>>,
        earlier: bool,
    ) -> impl FnMut() -> Option<MezError> + '_ {
        move || {
            if let Some(entered) = entered.take() {
                let _ = entered.send(());
            }
            if published.load(std::sync::atomic::Ordering::Acquire) {
                Some(MezError::invalid_state(
                    "injected competing native-stage receipt",
                ))
            } else if earlier {
                Some(MezError::invalid_state("earlier native cleanup cause"))
            } else {
                None
            }
        }
    }

    /// Drain A starts without a new receipt while B exclusively owns the REAL
    /// returned child. B publishes the labelled stage receipt BEFORE exact root
    /// signaling/native wait releases the last slot. A must reread failure at
    /// zero, in both all-owner and returned-only modes, including cancellation
    /// and same-owner retry. Earlier local causes keep precedence. A duplicated
    /// master excludes kernel hangup, and strict ESRCH is proved by B's actual
    /// native wait—not by the injected receipt or the task/operation joins.
    #[tokio::test]
    async fn competing_native_reap_cannot_hide_late_failure_at_zero_pending() {
        use std::sync::atomic::{AtomicBool, Ordering};
        for all_owners in [false, true] {
            for cancel_first in [false, true] {
                for earlier in [false, true] {
                    let owner = PaneProcessRetirementOwner::default();
                    let process = mez_mux::process::spawn_retained_argv_pty_process(
                        std::path::Path::new("/bin/cat"),
                        &[],
                        &[],
                        super::super::Size::new(80, 24).unwrap(),
                        None,
                        owner.reserve().unwrap(),
                    )
                    .unwrap();
                    let pid = rustix::process::Pid::from_raw(
                        i32::try_from(process.primary_pid()).unwrap(),
                    )
                    .unwrap();
                    let start =
                        mez_mux::process::process_start_token_for_pid(process.primary_pid());
                    let held_master = process.duplicate_master_fd().unwrap();
                    drop(process);
                    owner.seal();
                    let mut claim = owner.begin_drain().unwrap();
                    let mut child = claim.take_returned().unwrap();
                    let published = AtomicBool::new(false);
                    let (entered, observed) = tokio::sync::oneshot::channel();
                    let (release, released) = tokio::sync::oneshot::channel();
                    let deadline = Instant::now() + Duration::from_secs(1);
                    let waiting = async {
                        if cancel_first {
                            let first_was_pending = {
                                let first = drain_native_with_failure_reader(
                                    &owner,
                                    deadline,
                                    all_owners,
                                    competing_receipt_reader(&published, Some(entered), earlier),
                                );
                                tokio::pin!(first);
                                tokio::time::timeout(Duration::from_millis(30), &mut first)
                                    .await
                                    .is_err()
                            };
                            assert!(first_was_pending);
                            release.send(()).unwrap();
                            drain_native_with_failure_reader(
                                &owner,
                                deadline,
                                all_owners,
                                competing_receipt_reader(&published, None, earlier),
                            )
                            .await
                        } else {
                            release.send(()).unwrap();
                            drain_native_with_failure_reader(
                                &owner,
                                deadline,
                                all_owners,
                                competing_receipt_reader(&published, Some(entered), earlier),
                            )
                            .await
                        }
                    };
                    let competing = async {
                        observed.await.unwrap();
                        released.await.unwrap();
                        published.store(true, Ordering::Release);
                        child.force_retirement().unwrap();
                        loop {
                            if child.try_wait().unwrap().is_some() {
                                break;
                            }
                            assert!(
                                Instant::now() < deadline,
                                "exact native wait did not settle {pid:?} {start:?}"
                            );
                            sleep(Duration::from_millis(5)).await;
                        }
                        drop(child);
                        drop(claim);
                    };
                    let (verdict, ()) = tokio::join!(waiting, competing);
                    let error = verdict.unwrap_err();
                    assert!(error.message().contains(if earlier {
                        "earlier native cleanup cause"
                    } else {
                        "competing native-stage receipt"
                    }));
                    assert_eq!(owner.outstanding(), 0);
                    assert_eq!(
                        rustix::process::test_kill_process(pid),
                        Err(rustix::io::Errno::SRCH),
                        "{start:?}"
                    );
                    drop(held_master);
                }
            }
        }
    }

    /// Two final/worker drains may overlap normally. A reserved-startup gate
    /// keeps the first drain active while the second waits; both must observe
    /// the same exact child completion within one unchanged deadline.
    #[tokio::test]
    async fn concurrent_native_drains_share_exact_completion_without_false_failure() {
        let owner = PaneProcessRetirementOwner::default();
        let gate = owner.reserve().unwrap();
        let process = mez_mux::process::spawn_retained_argv_pty_process(
            std::path::Path::new("/bin/cat"),
            &[],
            &[],
            super::super::Size::new(80, 24).unwrap(),
            None,
            owner.reserve().unwrap(),
        )
        .unwrap();
        let pid =
            rustix::process::Pid::from_raw(i32::try_from(process.primary_pid()).unwrap()).unwrap();
        drop(process);
        owner.seal();
        let deadline = Instant::now() + Duration::from_secs(1);
        let release = async {
            sleep(Duration::from_millis(30)).await;
            drop(gate);
        };
        let (first, second, ()) = tokio::join!(
            drain_native_pane_retirement(&owner, deadline),
            drain_native_pane_retirement(&owner, deadline),
            release
        );
        first.unwrap();
        second.unwrap();
        assert_eq!(owner.outstanding(), 0);
        assert_eq!(
            rustix::process::test_kill_process(pid),
            Err(rustix::io::Errno::SRCH)
        );
    }

    /// An expired operation cannot acquire native children or signal them.
    /// Pending ownership remains available to a distinct explicit retry.
    #[tokio::test]
    async fn expired_native_deadline_retains_exact_child_without_success() {
        let owner = PaneProcessRetirementOwner::default();
        let process = mez_mux::process::spawn_retained_argv_pty_process(
            std::path::Path::new("/bin/cat"),
            &[],
            &[],
            super::super::Size::new(80, 24).unwrap(),
            None,
            owner.reserve().unwrap(),
        )
        .unwrap();
        let pid =
            rustix::process::Pid::from_raw(i32::try_from(process.primary_pid()).unwrap()).unwrap();
        drop(process);
        owner.seal();
        assert!(
            drain_native_pane_retirement(&owner, Instant::now())
                .await
                .is_err()
        );
        assert_eq!(owner.outstanding(), 1);
        drain_native_pane_retirement(&owner, Instant::now() + Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(
            rustix::process::test_kill_process(pid),
            Err(rustix::io::Errno::SRCH)
        );
    }
}
