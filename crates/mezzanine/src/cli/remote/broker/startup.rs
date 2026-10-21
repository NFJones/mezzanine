//! Bounded startup orchestration over protected election and authenticated readiness.
//!
//! This owner accepts a concrete launcher from its caller; it does not select an
//! executable or enable ordinary CLI routing. Only missing/refused discovery can
//! trigger election. Permission, protocol and negotiation failures are terminal,
//! never reasons to replace an existing owner. One election guard survives the
//! launch/readiness sequence and is revalidated before every startup effect.
//! The endpoint's independent exclusive lifetime lock remains mandatory.
//!
//! Authenticated readiness negotiation runs beside exact launched-child exit
//! observation under the one existing absolute deadline. An exited child is
//! reported promptly and distinctly rather than as a generic readiness timeout,
//! yet a broker that authenticates during the readiness/exit race stays
//! authoritative. Observation also stays active during an in-flight readiness
//! probe and precedes any deadline-exhaustion classification, and a startup
//! election held by another launcher is reported as contention rather than as an
//! unready child. An observed exit settles an already-connected readiness probe
//! instead of cancelling it and reconnecting; only a probe that settled as
//! missing or refused discovery takes one bounded post-exit recheck. Observation
//! reaps only the caller's exact spawned child; no
//! discovered, sibling or unknown process is signaled, adopted or replaced, and
//! no discovery, lock or key entry is deleted to bypass contention.

use std::path::Path;
use std::process::ExitStatus;
use std::time::Duration;

use super::election::StartupElection;
use crate::error::{MezError, Result};
use crate::host::outbound_frontend::client::OutboundFrontendClient;

/// Returns one retained authenticated frontend, invoking at most one launcher.
/// The root must already be provisioned privately by the policy-aware caller.
/// A total async deadline bounds election contention, readiness negotiation and
/// exact-child exit observation; synchronous filesystem/launcher calls remain
/// subject to host I/O availability. Launch failure, observed child exit or
/// timeout never retries application work or unlinks ownership.
pub(super) async fn connect_with_launcher<L>(
    config_root: &Path,
    budget: Duration,
    launcher: &mut L,
) -> Result<OutboundFrontendClient>
where
    L: StartupLauncher + ?Sized,
{
    if !(Duration::from_millis(100)..=Duration::from_secs(120)).contains(&budget) {
        return Err(MezError::invalid_args(
            "outbound startup deadline unavailable",
        ));
    }
    let deadline = tokio::time::Instant::now() + budget;
    let mut elected = None;
    let mut election_contended = false;
    loop {
        if let Some(guard) = &elected {
            let guard: &StartupElection = guard;
            guard.validate()?;
        }
        // Observe the exact child before classifying the deadline: an exit that
        // happened during the last sleep or negotiation must still be reported
        // as an exit instead of as a generic readiness timeout.
        if let Some(status) = launcher.observe_exit()? {
            return exit_outcome(config_root, deadline, &elected, status).await;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining < Duration::from_millis(100) {
            return Err(startup_timeout_error(elected.is_some(), election_contended));
        }
        // Authenticated readiness and exact-child exit observation run beside
        // each other under the one absolute deadline. An exit never cancels a
        // possibly connected handshake: the same probe is settled first, so its
        // authenticated success or terminal failure keeps precedence and no
        // blind reconnect occurs. Only a probe that actually settled as missing
        // or refused discovery may take the single bounded post-exit recheck.
        let mut readiness = Box::pin(probe_readiness(config_root, deadline));
        let mut exit_status = None;
        let mut observing = true;
        loop {
            tokio::select! {
                biased;
                settled = readiness.as_mut() => {
                    match settled? {
                        Some(client) => {
                            if let Some(guard) = &elected {
                                guard.validate()?;
                            }
                            return Ok(client);
                        }
                        None => {
                            if let Some(status) = exit_status {
                                return exit_outcome(config_root, deadline, &elected, status).await;
                            }
                            break;
                        }
                    }
                },
                status = observe_exit_until(launcher, deadline), if observing && exit_status.is_none() => {
                    match status? {
                        Some(status) => exit_status = Some(status),
                        None => observing = false,
                    }
                },
            }
        }
        if elected.is_none() {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining < Duration::from_millis(100) {
                return Err(startup_timeout_error(false, election_contended));
            }
            match StartupElection::acquire(config_root)? {
                Some(guard) => {
                    // Reprobe after election: another owner may have published
                    // between the first observation and lock acquisition.
                    guard.validate()?;
                    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                    if remaining < Duration::from_millis(100) {
                        return Err(readiness_timeout_error());
                    }
                    if let Some(client) = probe_readiness(config_root, deadline).await? {
                        guard.validate()?;
                        return Ok(client);
                    }
                    guard.validate()?;
                    launcher.launch(&guard)?;
                    elected = Some(guard);
                    election_contended = false;
                }
                None => election_contended = true,
            }
        }
        tokio::time::sleep_until(
            (tokio::time::Instant::now() + Duration::from_millis(20)).min(deadline),
        )
        .await;
    }
}

/// Launcher-side capability consumed by bounded startup orchestration.
///
/// Implementations launch at most one exact child under a validated election and
/// observe only that caller-owned child through a nonblocking exact handle. They
/// never discover, signal, adopt or replace another process, and a synchronous
/// launch failure propagates as its original error without retry or fallback.
pub(super) trait StartupLauncher {
    /// Invokes this startup attempt's single launch under a valid election.
    fn launch(&mut self, election: &StartupElection) -> Result<()>;

    /// Returns the exact launched child's status once it has exited, reaping only
    /// that child. `None` means no child was launched or it is still running.
    fn observe_exit(&mut self) -> Result<Option<ExitStatus>>;
}

/// Polls the exact child's exit beside one in-flight readiness negotiation.
///
/// It yields `Some(status)` as soon as the caller's exact child exits and `None`
/// only when the absolute deadline passes without an exit. It never awaits,
/// signals or discovers a process other than the retained exact child and never
/// extends the caller's deadline.
async fn observe_exit_until<L>(
    launcher: &mut L,
    deadline: tokio::time::Instant,
) -> Result<Option<ExitStatus>>
where
    L: StartupLauncher + ?Sized,
{
    loop {
        if let Some(status) = launcher.observe_exit()? {
            return Ok(Some(status));
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        tokio::time::sleep_until(
            (tokio::time::Instant::now() + Duration::from_millis(10)).min(deadline),
        )
        .await;
    }
}

/// Resolves one observed exact-child exit against authenticated readiness.
///
/// A broker that authenticates wins the readiness/exit race: the failed child is
/// not authoritative, no launch is attempted and no application work is replayed.
/// Otherwise the bounded exit classification is returned. The single recheck
/// stays inside the caller's absolute deadline and never unlinks ownership.
async fn exit_outcome(
    config_root: &Path,
    deadline: tokio::time::Instant,
    elected: &Option<StartupElection>,
    status: ExitStatus,
) -> Result<OutboundFrontendClient> {
    if let Some(client) = probe_readiness(config_root, deadline).await? {
        if let Some(guard) = elected {
            guard.validate()?;
        }
        return Ok(client);
    }
    Err(child_exit_error(status))
}

/// Runs one bounded authenticated readiness probe.
///
/// `Ok(None)` reports missing or refused discovery, the only outcomes that may
/// elect startup or inspect an exact child. A ready client or any connected
/// failure propagates unchanged so permission, protocol and negotiation errors
/// stay terminal and keep their original classification. The probe never creates
/// discovery state and never exceeds the caller's absolute deadline.
async fn probe_readiness(
    config_root: &Path,
    deadline: tokio::time::Instant,
) -> Result<Option<OutboundFrontendClient>> {
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining < Duration::from_millis(100) {
        return Ok(None);
    }
    match OutboundFrontendClient::connect(config_root, remaining.min(Duration::from_millis(500)))
        .await
    {
        Ok(client) => Ok(Some(client)),
        Err(error)
            if matches!(
                error.io_kind(),
                Some(std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused)
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// Reports a live-or-absent startup that exhausted the retained deadline.
///
/// This stays distinct from an observed child exit: the caller's exact child may
/// still be running and may still become ready, so no ownership inference,
/// termination or retry is attached to this classification.
fn readiness_timeout_error() -> MezError {
    MezError::invalid_state("outbound broker did not become ready before timeout")
}

/// Classifies an exhausted startup deadline without conflating stages.
///
/// A launched attempt reports live-but-not-ready readiness; an attempt that never
/// acquired election because another launcher held it reports contention. Neither
/// classification authorizes stealing, unlinking or extending ownership.
fn startup_timeout_error(launched: bool, contended: bool) -> MezError {
    if !launched && contended {
        MezError::invalid_state(
            "outbound startup election remained held by another process before timeout",
        )
    } else {
        readiness_timeout_error()
    }
}

/// Classifies one observed exact-child exit without reading diagnostics or
/// reflecting broker output.
///
/// Only the bounded exit status and the static "before local readiness" stage are
/// reported. The retained endpoint identity may still belong to another live
/// owner, so the message points at owner inspection rather than lock or key
/// deletion, and the caller keeps the exact child evidence for its own disposal.
fn child_exit_error(status: ExitStatus) -> MezError {
    if status.success() {
        MezError::invalid_state(format!(
            "outbound broker child exited successfully before local readiness ({status}); \
             an existing broker may still own the endpoint identity, so inspect the retained \
             owner and diagnostics instead of deleting lock or key files"
        ))
    } else {
        MezError::invalid_state(format!(
            "outbound broker child exited before local readiness ({status}); \
             an existing broker may still own the endpoint identity, so inspect the retained \
             owner and diagnostics instead of deleting lock or key files"
        ))
    }
}

#[cfg(test)]
mod tests;
