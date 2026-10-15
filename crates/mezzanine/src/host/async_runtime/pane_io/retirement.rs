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
    match first_error.or_else(|| retained_native_failure(owner)) {
        Some(error) => Err(error),
        None => Ok(owner.outstanding() == 0),
    }
}

/// Shares bounded exact-handle I/O; only final shutdown uses the all-owner barrier.
async fn drain_native_pane_retirement_scope(
    owner: &PaneProcessRetirementOwner,
    deadline: Instant,
    all_owners: bool,
) -> Result<()> {
    let mut first_error = retained_native_failure(owner);
    loop {
        let pending = if all_owners {
            owner.outstanding()
        } else {
            owner.returned_count()
        };
        if pending == 0 {
            return first_error.map_or(Ok(()), Err);
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
            return first_error.map_or(Ok(()), Err);
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
