//! Route availability under the retained outbound endpoint's immutable policy.
//!
//! This is preflight evidence, not successful reachability or remote authority.
//! The existing endpoint binder owns direct transport, relay and lookup policy;
//! callers neither broaden that policy nor bind a replacement endpoint. Protected
//! addresses retain their pinned endpoint ID even when routing uses discovery.
//! Local lookup currently installs no network lookup and is treated as disabled.

use crate::runtime::{
    RuntimeIrohAddressLookupPolicy, RuntimeIrohRelayPolicy, RuntimeIrohTransportPolicy,
};

/// Reports whether configured transports have a possible route to this pinned
/// peer. Missing routes reject before dialing or elected startup. Availability
/// does not promise connection success and never permits application replay.
/// Callers supply a validated policy with direct or relay data transport enabled;
/// network lookup discovers addresses rather than supplying a transport itself.
pub(crate) fn available(policy: &RuntimeIrohTransportPolicy, address: &iroh::EndpointAddr) -> bool {
    if !policy.outbound_enabled {
        return false;
    }
    let direct = policy.direct_connections && address.ip_addrs().next().is_some();
    let relay = !matches!(policy.relay, RuntimeIrohRelayPolicy::Disabled)
        && address.relay_urls().next().is_some();
    let lookup = !matches!(
        policy.address_lookup,
        RuntimeIrohAddressLookupPolicy::Disabled | RuntimeIrohAddressLookupPolicy::Local
    );
    direct || relay || lookup
}

#[cfg(test)]
mod tests;
