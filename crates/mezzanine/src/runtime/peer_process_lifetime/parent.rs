//! Native direct-parent lifetime evidence for a live Unix socket-origin helper.
//!
//! The helper's socket-original lifetime anchors its numeric identity. Its
//! paired native parent/start record—not a payload PID—selects a parent. Parent
//! birth/UID and both helper relationship observations bracket pidfd capture;
//! parent lifetime/birth/UID are rechecked before return. This is not an origin
//! pidfd fallback: numeric opening is allowed only for this independently fenced
//! parent relationship. Capture runs off actor with a cooperative elapsed budget.
//! A retained parent may be observed after the short-lived helper exits, but no
//! vendor attestation, client/session association, pane authority or enrollment
//! follows. Consumers must separately reject pane-root/shell producer selection.
//! Unsupported platforms fail closed; no subprocess/credential mutation occurs.
//! Production uses its fixed capture budget. Linux test builds can explicitly
//! select one fixture's finite allowance and conservative elapsed offset, while
//! executing this same native algorithm and all original identity fences.

use super::*;

/// Cooperative budget; synchronous native reads cannot be interrupted mid-call.
#[cfg(target_os = "linux")]
const PARENT_BUDGET: std::time::Duration = std::time::Duration::from_millis(100);

/// Exact parent instance observed through a live socket-origin relationship.
#[allow(
    dead_code,
    reason = "qualified short-lived hook consumer is unfinished"
)]
#[derive(Debug)]
pub(crate) struct UnixParentProcess {
    /// Bounded same-user identity; no environment or vendor payload supplies it.
    pub(super) uid: u32,
    /// Paired native PID/parent/start evidence at original capture.
    pub(crate) identity: ProcessParentIdentity,
    /// Owned close-on-exec parent pidfd, independent of helper transport lifetime.
    pub(super) lifetime: OwnedFd,
}

impl UnixParentProcess {
    /// Returns the native captured user, never a registration capability.
    #[allow(
        dead_code,
        reason = "qualified short-lived hook consumer is unfinished"
    )]
    pub(crate) fn uid(&self) -> u32 {
        self.uid
    }

    /// Nonblocking parent death check for a future bounded coordinator.
    #[allow(
        dead_code,
        reason = "qualified short-lived hook consumer is unfinished"
    )]
    pub(crate) fn is_live(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            require_live_origin(&self.lifetime).is_ok()
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }

    /// Rechecks retained-parent birth/relationship and UID within its lifetime;
    /// helper survival is not required and PID reuse can never replace the fd.
    #[allow(
        dead_code,
        reason = "qualified short-lived hook consumer is unfinished"
    )]
    pub(crate) fn reobserve(&self) -> io::Result<ProcessParentIdentity> {
        #[cfg(target_os = "linux")]
        {
            let start = std::time::Instant::now();
            let observed = observe_live_origin(&self.lifetime, self.identity.process_id)?;
            if observed != self.identity || parent_uid(observed.process_id)? != self.uid {
                return Err(unavailable());
            }
            require_live_origin(&self.lifetime)?;
            if start.elapsed() >= PARENT_BUDGET {
                return Err(unavailable());
            }
            Ok(observed)
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Unix parent lifetime lookup unsupported",
            ))
        }
    }
}

impl UnixOriginProcess {
    /// Derives an actual parent only while this original helper remains live.
    /// No caller process id is accepted, no current-writer/vendor authority is
    /// granted, and parent reparenting/replacement/credential changes fail closed.
    #[allow(
        dead_code,
        reason = "qualified short-lived hook consumer is unfinished"
    )]
    pub(crate) fn capture_parent(&self) -> io::Result<UnixParentProcess> {
        #[cfg(target_os = "linux")]
        {
            let start = std::time::Instant::now();
            self.capture_parent_with_expiry(|| start.elapsed() >= PARENT_BUDGET)
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Unix parent lifetime lookup unsupported",
            ))
        }
    }

    /// Applies the original native bracketing with one caller-owned expiry
    /// predicate. UID, birth, relationship and retained-lifetime checks remain
    /// identical; no deadline can substitute for that evidence.
    #[cfg(target_os = "linux")]
    fn capture_parent_with_expiry(
        &self,
        expired: impl FnMut() -> bool,
    ) -> io::Result<UnixParentProcess> {
        let (identity, lifetime) = capture_parent_with(
            self.uid(),
            || self.reobserve(),
            |pid| mez_mux::process::process_parent_identity_for_pid(pid).ok_or_else(unavailable),
            parent_uid,
            open_parent,
            require_live_origin,
            expired,
        )?;
        Ok(UnixParentProcess {
            uid: self.uid(),
            identity,
            lifetime,
        })
    }

    /// Executes every cold native check under this fixture's explicit finite
    /// allowance, falling back to the unchanged production budget. The fixed
    /// positive clock offset cannot rewind or manufacture native evidence.
    /// Policy/offset are captured once; expiry and native failures propagate
    /// through the original bracketing. Production never calls this method.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn capture_parent_with_test_budget(
        &self,
        budget: &super::ancestry::UnixAncestryBudget,
    ) -> io::Result<UnixParentProcess> {
        let start = std::time::Instant::now();
        let allowance = budget
            .capture_budget_override_for_tests()
            .unwrap_or(PARENT_BUDGET);
        let additional_elapsed = budget.parent_capture_elapsed_for_tests();
        self.capture_parent_with_expiry(|| {
            start.elapsed().saturating_add(additional_elapsed) >= allowance
        })
    }
}

/// Reuses bounded complete Linux credential evidence and its existing fail-closed
/// FSUID/GID guards; this grants no filesystem rights and never changes credentials.
#[cfg(target_os = "linux")]
pub(super) fn parent_uid(pid: u32) -> io::Result<u32> {
    mez_mux::process::filesystem_credentials_for_pid(pid)
        .map(|credentials| credentials.user_id)
        .ok_or_else(unavailable)
}

/// Opens only the native selected parent; caller-supplied origin PID fallback is
/// forbidden. The full capture owner verifies pre/post birth and relationship.
#[cfg(target_os = "linux")]
pub(super) fn open_parent(pid: u32) -> io::Result<OwnedFd> {
    use std::os::fd::FromRawFd;
    let pid = libc::pid_t::try_from(pid).map_err(|_| unavailable())?;
    // SAFETY: pidfd_open has scalar arguments and returns a newly owned fd.
    // The syscall does not signal, trace, alter credentials or mutate the parent.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0_u32) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = libc::c_int::try_from(fd).map_err(|_| unavailable())?;
    // SAFETY: successful pidfd_open transferred this new descriptor exactly once.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: owned remains live and F_GETFD only reads descriptor flags.
    let flags = unsafe { libc::fcntl(owned.as_raw_fd(), libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if flags & libc::FD_CLOEXEC == 0 {
        return Err(unavailable());
    }
    Ok(owned)
}

/// Injected capture boundary proves relationship/UID/birth/deadline rejection.
/// All reads are individually bracketed by elapsed fences; native lifetime
/// polling surrounds parent observations and helper observation surrounds capture.
#[cfg(any(target_os = "linux", test))]
#[allow(
    dead_code,
    reason = "qualified short-lived hook consumer is unfinished"
)]
pub(super) fn capture_parent_with<F>(
    uid: u32,
    mut helper: impl FnMut() -> io::Result<ProcessParentIdentity>,
    mut parent: impl FnMut(u32) -> io::Result<ProcessParentIdentity>,
    mut credentials: impl FnMut(u32) -> io::Result<u32>,
    mut open: impl FnMut(u32) -> io::Result<F>,
    mut live: impl FnMut(&F) -> io::Result<()>,
    mut expired: impl FnMut() -> bool,
) -> io::Result<(ProcessParentIdentity, F)> {
    if expired() {
        return Err(unavailable());
    }
    let child = helper()?;
    if expired() || child.parent_process_id <= 1 || child.parent_process_id == child.process_id {
        return Err(unavailable());
    }
    let observed = parent(child.parent_process_id)?;
    if expired() || observed.process_id != child.parent_process_id {
        return Err(unavailable());
    }
    if credentials(observed.process_id)? != uid || expired() {
        return Err(unavailable());
    }
    let lifetime = open(observed.process_id)?;
    if expired() {
        return Err(unavailable());
    }
    live(&lifetime)?;
    let again = parent(observed.process_id)?;
    live(&lifetime)?;
    if expired() || again != observed {
        return Err(unavailable());
    }
    if credentials(observed.process_id)? != uid || expired() {
        return Err(unavailable());
    }
    if helper()? != child || expired() {
        return Err(unavailable());
    }
    live(&lifetime)?;
    if expired() {
        return Err(unavailable());
    }
    Ok((observed, lifetime))
}

#[cfg(test)]
mod tests;
