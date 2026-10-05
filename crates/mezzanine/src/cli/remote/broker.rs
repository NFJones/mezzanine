//! Internal foreground outbound owner composition, not public attach activation.
//!
//! This process entry holds one protected endpoint and private listener, drives
//! the reviewed bounded snapshot pipelines, then disposes publication before
//! completing retained endpoint shutdown. Normal CLI consumers still use their
//! existing transport; election, events/input/X11 and full renderer migration
//! remain unfinished. No session is created merely by starting this owner.

use super::{CliEnv, Result};
use crate::host::outbound_endpoint::OutboundEndpointOwner;
use crate::host::outbound_frontend::OutboundFrontendListener;

#[allow(
    dead_code,
    reason = "frontend startup integration follows election qualification"
)]
mod election;

#[allow(
    dead_code,
    reason = "ordinary frontend activation follows startup orchestration qualification"
)]
mod startup;

#[allow(
    dead_code,
    reason = "production startup integration follows launcher qualification"
)]
mod launch;

/// Runs one foreground owner until cancellation, with explicit teardown on
/// normal/error return. Cancellation of this entire future remains fail-closed
/// under the resource quarantine contract, not proof of graceful shutdown.
pub(super) async fn run<C>(env: &CliEnv, cancellation: C) -> Result<u64>
where
    C: std::future::Future<Output = ()>,
{
    let paths = env.config_paths()?;
    let layers = crate::cli::load_runtime_config_layers(&paths)?;
    let structured = crate::runtime::runtime_effective_config_value(&layers)?;
    let policy = crate::runtime::runtime_iroh_transport_policy_from_config(&structured)?;
    // Configuration setup supplies parent directories; policy veto must precede
    // identity or listener creation, even when no user configuration exists.
    if !policy.outbound_enabled {
        return Err(crate::error::MezError::forbidden(
            "Iroh outbound transport is disabled",
        ));
    }
    paths.ensure_default_config()?;
    let endpoint = OutboundEndpointOwner::bind(paths.root(), &policy).await?;
    let listener = match OutboundFrontendListener::bind(
        endpoint.clone(),
        policy.max_connections,
        policy.setup_timeout,
    ) {
        Ok(listener) => listener,
        Err(error) => {
            // Do not hide uncertain teardown behind the publication error.
            endpoint.retire_and_shutdown().await?.finish().await?;
            return Err(error);
        }
    };
    let served = listener.serve(cancellation).await;
    drop(listener);
    endpoint.retire_and_shutdown().await?.finish().await?;
    served
}

/// Handles ordinary foreground termination signals without another owner task.
pub(super) async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    if let (Ok(mut interrupt), Ok(mut terminate), Ok(mut hangup)) = (
        signal(SignalKind::interrupt()),
        signal(SignalKind::terminate()),
        signal(SignalKind::hangup()),
    ) {
        tokio::select! {
            _ = interrupt.recv() => {},
            _ = terminate.recv() => {},
            _ = hangup.recv() => {},
        }
    } else {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests;
