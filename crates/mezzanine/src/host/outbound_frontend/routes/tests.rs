//! Pure route availability tests, without dialing lookup or relay services.
//!
//! Endpoint identity stays pinned; preflight describes possible configured routes,
//! not their reachability or application authorization.

use super::*;

/// Direct, relay and lookup evidence may independently permit shared startup.
/// Outbound veto always wins; disabled transports and absent route evidence do
/// not authorize dialing or a replacement endpoint. Port mapping is orthogonal.
#[test]
fn outbound_routes_follow_retained_policy_and_protected_evidence() {
    let id = iroh::SecretKey::generate().public();
    let empty = iroh::EndpointAddr::new(id);
    let direct = empty
        .clone()
        .with_ip_addr("127.0.0.1:43210".parse().unwrap());
    let relay = empty
        .clone()
        .with_relay_url("http://127.0.0.1:9".parse().unwrap());
    let mut policy = RuntimeIrohTransportPolicy::default();
    assert!(available(&policy, &direct));
    assert!(!available(&policy, &empty));
    assert!(!available(&policy, &relay));
    policy.port_mapping = true;
    assert!(available(&policy, &direct));
    policy.direct_connections = false;
    assert!(!available(&policy, &direct));
    for configured in [
        RuntimeIrohRelayPolicy::Public,
        RuntimeIrohRelayPolicy::Custom {
            urls: vec!["http://127.0.0.1:9".into()],
        },
    ] {
        policy.relay = configured;
        assert!(available(&policy, &relay));
        assert!(!available(&policy, &empty));
    }
    policy.relay = RuntimeIrohRelayPolicy::Disabled;
    // Lookup discovers routes for an enabled data transport; it is not itself
    // a transport. Match the production parser's valid-policy requirement.
    policy.direct_connections = true;
    for lookup in [
        RuntimeIrohAddressLookupPolicy::N0Dns,
        RuntimeIrohAddressLookupPolicy::CustomDns {
            domain: "localhost".into(),
        },
    ] {
        policy.address_lookup = lookup;
        assert!(available(&policy, &empty));
        policy.outbound_enabled = false;
        assert!(!available(&policy, &direct));
        assert!(!available(&policy, &relay));
        assert!(!available(&policy, &empty));
        policy.outbound_enabled = true;
    }
    policy.address_lookup = RuntimeIrohAddressLookupPolicy::Local;
    assert!(!available(&policy, &empty));
    assert_eq!(direct.id, id);
    assert_eq!(relay.id, id);
}
