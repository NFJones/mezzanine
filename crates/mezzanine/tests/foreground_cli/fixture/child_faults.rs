//! Instance-local failures around the public child handle's cleanup operations.
//!
//! The wrapped process is always a freshly generated synthetic fixture. Errors
//! alter only observations or a single termination request; retries retain the
//! same native handle, with no guessed PID, blocking wait, or repeated setup.

use super::*;
use portable_pty::{ChildKiller, ExitStatus};
use std::io;

/// Native child with one explicitly selected cleanup operation failure.
#[derive(Debug)]
struct FaultChild {
    /// Exact generated process; all successful operations delegate to this owner.
    inner: Box<dyn PtyChild + Send + Sync>,
    /// Observation count at which to inject one transient error.
    fail_poll: Option<usize>,
    /// Number of observations already attempted.
    polls: usize,
    /// Whether the next kill request fails without signaling anything.
    fail_kill: bool,
}

impl ChildKiller for FaultChild {
    /// Fails the selected request once; otherwise signals only the owned child.
    fn kill(&mut self) -> io::Result<()> {
        if std::mem::take(&mut self.fail_kill) {
            return Err(io::Error::other("injected owned-child kill failure"));
        }
        self.inner.kill()
    }

    /// Preserves the actual native killer when an independent owner is requested.
    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        self.inner.clone_killer()
    }
}

impl PtyChild for FaultChild {
    /// Injects the selected nonblocking observation error without reaping elsewhere.
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.polls += 1;
        if self.fail_poll == Some(self.polls) {
            return Err(io::Error::other("injected owned-child poll failure"));
        }
        self.inner.try_wait()
    }

    /// Forbids a blocking wait so this fixture cannot hide an unbounded cleanup path.
    fn wait(&mut self) -> io::Result<ExitStatus> {
        panic!("foreground retirement must never perform a blocking wait")
    }

    /// Returns native identity for the existing foreground interrupt operation.
    fn process_id(&self) -> Option<u32> {
        self.inner.process_id()
    }
}

/// Polling, termination, and post-kill reap failures must retain storage and
/// identify the failed operation. A later attempt uses the very same child and
/// may discharge its obligation after affirmative reaping, without replaying a
/// frontend launch or treating the first termination request as completion.
#[test]
fn foreground_fixture_retirement_reports_and_retries_owned_child_failures() {
    for (name, poll, kill, expected) in [
        (
            "initial-poll",
            Some(1),
            false,
            "child state observation failed",
        ),
        ("kill", None, true, "child termination request failed"),
        ("reap-poll", Some(2), false, "child reap observation failed"),
    ] {
        let root = test_root(name);
        let (mut fixture, slave) = retirement_fixture_with_child(&root, "exec sleep 30", |child| {
            Box::new(FaultChild {
                inner: child,
                fail_poll: poll,
                polls: 0,
                fail_kill: kill,
            })
        });
        drop(slave);
        let outcome = fixture.retire();
        assert!(
            matches!(outcome, FixtureCleanup::Unsettled(ref reason) if reason.contains(expected)),
            "{name}: {outcome:?}"
        );
        assert!(
            root.exists(),
            "{name}: uncertain cleanup must retain storage"
        );
        assert_eq!(fixture.retire(), FixtureCleanup::Retired, "{name}");
        assert!(!root.exists());
    }
}
