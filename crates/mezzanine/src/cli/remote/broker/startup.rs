//! Bounded startup orchestration over protected election and authenticated readiness.
//!
//! This owner accepts a concrete launcher from its caller; it does not select an
//! executable or enable ordinary CLI routing. Only missing/refused discovery can
//! trigger election. Permission, protocol and negotiation failures are terminal,
//! never reasons to replace an existing owner. One election guard survives the
//! launch/readiness sequence and is revalidated before every startup effect.
//! The endpoint's independent exclusive lifetime lock remains mandatory.

use std::path::Path;
use std::time::Duration;

use super::election::StartupElection;
use crate::error::{MezError, Result};
use crate::host::outbound_frontend::client::OutboundFrontendClient;

/// Returns one retained authenticated frontend, invoking at most one launcher.
/// The root must already be provisioned privately by the policy-aware caller.
/// A total async deadline bounds election contention and readiness negotiation;
/// synchronous filesystem/launcher calls remain subject to host I/O availability.
/// Launch failure or timeout never retries application work or unlinks ownership.
pub(super) async fn connect_with_launcher<F>(
    config_root: &Path,
    budget: Duration,
    launch: F,
) -> Result<OutboundFrontendClient>
where
    F: FnOnce(&StartupElection) -> Result<()>,
{
    if !(Duration::from_millis(100)..=Duration::from_secs(120)).contains(&budget) {
        return Err(MezError::invalid_args(
            "outbound startup deadline unavailable",
        ));
    }
    let deadline = tokio::time::Instant::now() + budget;
    let mut elected = None;
    let mut launch = Some(launch);
    loop {
        if let Some(guard) = &elected {
            let guard: &StartupElection = guard;
            guard.validate()?;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining < Duration::from_millis(100) {
            return Err(MezError::invalid_state(
                "outbound broker did not become ready before timeout",
            ));
        }
        match OutboundFrontendClient::connect(
            config_root,
            remaining.min(Duration::from_millis(500)),
        )
        .await
        {
            Ok(client) => {
                if let Some(guard) = &elected {
                    guard.validate()?;
                }
                return Ok(client);
            }
            Err(error)
                if matches!(
                    error.io_kind(),
                    Some(std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused)
                ) => {}
            Err(error) => return Err(error),
        }
        if elected.is_none()
            && let Some(guard) = StartupElection::acquire(config_root)?
        {
            // Reprobe after election: another owner may have published
            // between the first observation and lock acquisition.
            guard.validate()?;
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining < Duration::from_millis(100) {
                return Err(MezError::invalid_state(
                    "outbound broker did not become ready before timeout",
                ));
            }
            match OutboundFrontendClient::connect(
                config_root,
                remaining.min(Duration::from_millis(500)),
            )
            .await
            {
                Ok(client) => {
                    guard.validate()?;
                    return Ok(client);
                }
                Err(error)
                    if matches!(
                        error.io_kind(),
                        Some(std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused)
                    ) => {}
                Err(error) => return Err(error),
            }
            guard.validate()?;
            let launcher = launch.take().ok_or_else(|| {
                MezError::invalid_state("outbound startup launcher already consumed")
            })?;
            launcher(&guard)?;
            elected = Some(guard);
        }
        tokio::time::sleep_until(
            (tokio::time::Instant::now() + Duration::from_millis(20)).min(deadline),
        )
        .await;
    }
}

#[cfg(test)]
mod tests;
