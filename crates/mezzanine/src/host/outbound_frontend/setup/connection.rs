//! Pinned direct transport connection for one consumed prepared frontend.
//!
//! No application stream, authentication request or session creation is sent.
//! The connection lease retains the shared endpoint and closes only this peer
//! connection on drop; the prepared frontend retains its local stream/capacity.
//! This initial path qualifies direct pinned routes only, refusing endpoint-wide
//! discovery, relay or port-mapping policy rather than changing shared policy or
//! silently binding another endpoint. Later policy-qualified routes require
//! their own acceptance. Codec fallback is allowed only before stream creation.

use super::*;
use crate::host::outbound_endpoint::OutboundConnectionLease;
use crate::runtime::{
    IrohCompressionPolicy, RuntimeIrohAddressLookupPolicy, RuntimeIrohRelayPolicy,
};

/// Connected transport ownership, not remote application authority.
/// Fields remain internal until exact initialization/stream ownership is added.
pub(crate) struct ConnectedFrontend {
    prepared: PreparedFrontend,
    connection: OutboundConnectionLease,
    compression: IrohCompressionPolicy,
}

mod initialize;

impl ConnectedFrontend {
    /// Reports the validated initialization intent without exposing profile proof
    /// or mutable setup fields. Initialization still enforces host-only authority.
    pub(crate) fn host_only_requested(&self) -> Result<bool> {
        let params = initialize_params_from_json(&self.prepared.initialize.to_string())?;
        Ok(params.session_intent == Some(SessionIntent::HostOnly))
    }
}

impl PreparedFrontend {
    /// Connects only to this owner-resolved profile's pinned direct address.
    /// A single total deadline bounds all pre-stream codec attempts. Failure or
    /// cancellation disposes the consumed frontend, never retrying creation.
    pub(crate) async fn connect_pinned(self) -> Result<ConnectedFrontend> {
        let policy = self.frontend._endpoint.transport_policy().clone();
        if !policy.outbound_enabled
            || !policy.direct_connections
            || policy.port_mapping
            || !matches!(
                policy.address_lookup,
                RuntimeIrohAddressLookupPolicy::Disabled | RuntimeIrohAddressLookupPolicy::Local
            )
            || !matches!(policy.relay, RuntimeIrohRelayPolicy::Disabled)
            || self.profile.server_addr.ip_addrs().next().is_none()
            || self.profile.server_addr.relay_urls().next().is_some()
        {
            return Err(MezError::forbidden(
                "outbound pinned route policy unsupported",
            ));
        }
        let codecs = IrohCompressionPolicy::negotiation_codecs(&policy.compression_codecs)
            .map(|codec| {
                IrohCompressionPolicy::new(
                    codec,
                    policy.compression_min_bytes,
                    policy.compression_zstd_level,
                    1024 * 1024 + 1024,
                )
                .map(|compression| (codec, compression))
            })
            .collect::<Result<Vec<_>>>()?;
        if codecs.is_empty() {
            return Err(MezError::invalid_args("outbound codec policy unavailable"));
        }
        tokio::time::timeout(policy.setup_timeout, async move {
            for (codec, compression) in codecs {
                self.frontend._endpoint.frontend_config_root()?;
                let attempt = self
                    .frontend
                    ._endpoint
                    .connect(self.profile.server_addr.clone(), codec.alpn())
                    .await;
                let connection = match attempt {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == MezErrorKind::RateLimited => return Err(error),
                    Err(_) => continue,
                };
                if connection.connection().remote_id() != self.profile.server_addr.id {
                    return Err(MezError::forbidden("outbound server identity mismatch"));
                }
                self.frontend._endpoint.frontend_config_root()?;
                return Ok(ConnectedFrontend {
                    prepared: self,
                    connection,
                    compression,
                });
            }
            Err(MezError::invalid_state(
                "outbound pinned connection unavailable",
            ))
        })
        .await
        .map_err(|_| MezError::invalid_state("outbound pinned connection timed out"))?
    }
}

#[cfg(test)]
mod tests;
