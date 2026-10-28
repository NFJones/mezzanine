//! Optional host transport construction without releasing security ownership.
//!
//! The parent validates and retains host identity/trust before entering this
//! network-only boundary. A completed safe network failure keeps its original
//! policy/error without an endpoint. Configuration and uncertain cleanup remain
//! fatal. There is no retry timer, dummy endpoint, key replacement or replay.

use crate::error::{MezError, Result};
use crate::runtime::{
    RuntimeIrohBindError, RuntimeIrohEndpoint, RuntimeIrohTransportPolicy,
    bind_runtime_iroh_endpoint,
};

use super::HostIrohServeError;

/// Exactly one live endpoint or settled unavailable construction outcome.
#[derive(Debug)]
pub(super) enum HostIrohTransport {
    /// Heap ownership bounds the host composition future independently of SDK size.
    Bound(Box<RuntimeIrohEndpoint>),
    /// Retains configuration and causal evidence, without invented endpoint state.
    Unavailable(Box<UnavailableTransport>),
}

/// Original network-only failure and policy retained for the host lifetime.
#[derive(Debug)]
pub(super) struct UnavailableTransport {
    /// Configured policy remains inspectable while transport is unavailable.
    policy: RuntimeIrohTransportPolicy,
    /// Complete original error, including its typed evidence.
    error: MezError,
}

impl HostIrohTransport {
    /// Attempts construction once after parent security validation. Returns a
    /// critical error for invalid configuration or unproven construction/cleanup;
    /// safely unavailable networking remains an owned, enabled host transport.
    pub(super) async fn bind(
        policy: RuntimeIrohTransportPolicy,
        secret_key: iroh::SecretKey,
    ) -> Result<Self> {
        match bind_runtime_iroh_endpoint(policy.clone(), secret_key).await {
            Ok(Some(endpoint)) => Ok(Self::Bound(Box::new(endpoint))),
            Ok(None) => Err(MezError::invalid_state(
                "enabled host Iroh endpoint was not bound",
            )),
            Err(RuntimeIrohBindError::TransportUnavailable(error)) => {
                Ok(Self::Unavailable(Box::new(UnavailableTransport {
                    policy,
                    error,
                })))
            }
            Err(RuntimeIrohBindError::Critical(error)) => Err(error),
        }
    }

    /// Returns only an actually constructed endpoint, never a placeholder.
    pub(super) fn endpoint(&self) -> Option<&RuntimeIrohEndpoint> {
        match self {
            Self::Bound(endpoint) => Some(endpoint),
            Self::Unavailable(_) => None,
        }
    }

    /// Returns the original configured policy for either construction outcome.
    pub(super) fn policy(&self) -> &RuntimeIrohTransportPolicy {
        match self {
            Self::Bound(endpoint) => endpoint.policy(),
            Self::Unavailable(unavailable) => &unavailable.policy,
        }
    }

    /// Supplies the actual listener endpoint or the retained scoped failure.
    /// Repeated observation never retries construction or releases identity.
    pub(super) fn listener_endpoint(
        &self,
    ) -> std::result::Result<&RuntimeIrohEndpoint, HostIrohServeError> {
        match self {
            Self::Bound(endpoint) => Ok(endpoint),
            Self::Unavailable(unavailable) => Err(HostIrohServeError::TransportUnavailable(
                unavailable.error.clone(),
            )),
        }
    }
}
