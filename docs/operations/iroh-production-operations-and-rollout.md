# Iroh production operations and rollout

## Purpose

Operate the optional Iroh client/server transport without weakening local Unix
recovery, hiding network metadata exposure, or treating public development
infrastructure as a production dependency.

This page is an operator gate, not evidence that a production relay or lookup
service has been selected. Iroh remains disabled by default.

## Current production infrastructure decision

No production relay or endpoint-address lookup service is approved by this
repository. `relay_mode = "public"` and `address_lookup = "n0_dns"` are suitable
only for development and controlled evaluation. They must not become an
implicit dependency of a supported release.

Before a production rollout can advance, the release owner must record and
approve all of the following outside secret-bearing configuration:

| Required record | Release requirement |
| --- | --- |
| Service owner and incident contact | Named team and escalation path for relay and lookup services. |
| Domains and regions | Exact production domains, deployment regions, and residency constraints. |
| Capacity and rate limits | Tested concurrent endpoints, sessions, bandwidth, connection churn, and enforced client limits. |
| Availability and recovery targets | Availability objective, detection interval, recovery target, and documented maintenance behavior. |
| Authentication and CA policy | Relay authentication where supported, certificate issuer, trust-store policy, and rotation procedure. |
| Metadata retention | Retained endpoint IDs, addresses, timing, connection-pair, and byte-count metadata, including deletion policy. |
| Upgrade and rollback owner | Version policy, compatibility window, canary process, and rollback authority. |

A missing or unapproved row blocks supported production use. Do not substitute
public n0 service behavior for these records.

## Choose one explicit network policy

Use primary-user configuration and restart the daemon after changes. Project
overlays and model-authored changes cannot enable or retarget Iroh.

### Direct only

```toml
[transport.iroh]
enabled = true
relay_mode = "disabled"
address_lookup = "disabled"
direct_connections = true
port_mapping = false
```

Use only where clients receive a pinned endpoint address through a confidential
invitation or profile. Direct peers can observe each other IP addresses. Enable
port mapping only after the controlled network plan measures the exposure and
benefit.

### Relay required

```toml
[transport.iroh]
enabled = true
relay_mode = "custom"
relay_urls = ["https://relay.example.invalid"]
direct_connections = false
address_lookup = "custom_dns"
address_lookup_domain = "iroh.example.invalid"
system_ca_store = true
```

Replace example domains only with approved infrastructure. This mode fails
closed if no relay route exists. Proxy and CA settings must match the approved
relay and lookup deployment.

### Controlled direct plus relay

```toml
[transport.iroh]
enabled = true
relay_mode = "custom"
relay_urls = ["https://relay.example.invalid"]
direct_connections = true
address_lookup = "custom_dns"
address_lookup_domain = "iroh.example.invalid"
```

Use this mode only when direct-path metadata exposure and relay fallback are
both accepted. Verify path selection and migration in the controlled network
matrix; do not infer either from configuration alone.

The runtime rejects contradictory route, lookup-domain, relay-URL, type, and
numeric-bound combinations even when configuration is composed
programmatically.

## Enable and verify

1. Confirm the private Unix control socket works and retain a local primary
   administration path.
2. Validate configuration before restart.
3. Start one canary session with Iroh enabled.
4. Run `mez --json remote status` through local Unix control.
5. Confirm `enabled`, `listener_active`, configured route flags, and the bound
   endpoint address match the intended policy.
6. Inspect `show-metrics` locally. The `[iroh transport]` section reports only
   aggregate listener, setup, connection, shutdown, and path counters.
7. Pair one role-limited test device, exercise attach and detach, revoke it, and
   verify future initialization fails.
8. Confirm a local Unix client can still inspect, revoke, detach, and stop the
   session during and after remote failures.

`remote/status` includes endpoint identity and dialable endpoint address because
local administration needs them. Aggregate `show-metrics` output intentionally
omits endpoint IDs, peer addresses, invitations, credentials, private keys,
payloads, and trust records.

## Interpret diagnostics

| Field | Meaning and response |
| --- | --- |
| `listener_active` | The supervised Iroh listener is currently accepting work. False while enabled requires startup or service investigation. |
| `active_remote_connections` / `active_connections` | Current accepted connection tasks. Sustained saturation at `max_connections` requires abuse or capacity review. |
| `connections_accepted` and `connections_rejected` | Aggregate setup outcomes. Rejection spikes require ALPN, relay, lookup, certificate, timeout, or abuse investigation. |
| `setup_successes` and `setup_failures` | Completed transport setup attempts. Failures include rejected and timed-out setup before application authority. |
| `setup_latency_average_millis` and `setup_latency_max_millis` | Bounded aggregate setup latency. Compare with the configured timeout and controlled baseline. |
| `connections_completed` and `connections_failed` | Control-task outcomes after accepted setup. Failures are aggregate and disclose no peer identity. |
| `shutdown_aborts` | Connection tasks aborted after bounded listener drain expired. Any nonzero clean-shutdown value requires investigation. |
| `last_connection_path` and path counts | Aggregate selected direct, relay, custom, or unknown path evidence. It is diagnostic evidence, not a route guarantee. |

Counters are process-lifetime aggregates and can race with an in-flight state
transition. Correlate them with lifecycle state and bounded audit events, not
with secret-bearing application payloads.

## Outage response

### Relay or lookup outage

1. Keep Unix administration available.
2. Confirm the configured policy and infrastructure health outside Mezzanine.
3. Inspect setup failures, latency, path counts, and listener state.
4. Do not silently change an explicit relay-required target to direct or Unix.
5. For direct-plus-relay policy, verify an allowed direct path through a
   controlled test rather than assuming fallback occurred.
6. If the outage crosses the rollout stop threshold, disable Iroh and restart.

### Proxy or CA failure

Verify the daemon environment, approved proxy scope, certificate chain, system
CA policy, and hostname. Never disable certificate verification as a recovery
shortcut. If the approved CA path is unavailable, stop remote rollout and use
Unix control.

### Malformed traffic or connection flood

The listener bounds accepted connections, streams, setup, frame sizes, queues,
idle work, event batches, and shutdown drain. Rejected work must not gain
application authority. Sustained saturation, rising setup failures, or repeated
shutdown aborts is a stop condition; preserve aggregate diagnostics and disable
Iroh while investigating.

## Key loss, rotation, and emergency revocation

Use local Unix control for all trust administration.

- Revoke a lost client immediately with `mez remote revoke` and a reason.
- Stop the session before backing up or replacing server endpoint identity and
  trust state.
- Treat endpoint key, trust database, client key, profile, and device credential
  backups as credentials with owner-only access.
- Replacing a server endpoint key invalidates endpoint-bound invitations and
  device trust. Re-pair clients; do not rewrite trust records by hand.
- If trust state is corrupt, preserve it offline for diagnosis, replace it as a
  unit, and re-pair through Unix control.
- After any rotation, verify old profiles fail and the new server endpoint ID is
  delivered through a confidential channel.

## Disable and return to Unix only

1. Establish and test a local Unix primary connection.
2. Set `transport.iroh.enabled = false` in primary-user configuration.
3. Restart the daemon so the supervised listener and network activity stop.
4. Confirm Unix attach and `mez remote status` still work, `enabled` and
   `listener_active` are false, active remote connections are zero, and no bound
   Iroh endpoint address is published.
5. Confirm explicit remote targets fail visibly and do not fall back to Unix.
6. Preserve remote identity and trust state for later re-enable, or remove it
   only during a deliberate offline credential-retirement procedure.

Rollback needs no session-data migration. Disabling the transport does not
change local session storage or remove the Unix recovery path.

## Staged rollout and stop thresholds

Advance only in this order:

1. Internal development with non-production infrastructure.
2. Controlled opt-in preview with named operators and test devices.
3. Supported opt-in after every release gate below is signed off.

Stop or roll back immediately for any authority leak, secret in telemetry,
remote-to-Unix fallback, Unix recovery failure, cross-session disclosure,
revocation bypass, unbounded resource growth, or data corruption. Pause a
canary for investigation when setup failures exceed 5 percent over 15 minutes,
setup latency reaches 80 percent of the configured timeout, connection capacity
is sustained for 5 minutes, or any clean shutdown records an abort. Production
owners may adopt stricter thresholds after controlled baselines; relaxing them
requires a recorded release exception.

## Release-readiness evidence matrix

Repository tests are not substitutes for native platform, production service,
or representative network evidence.

### Local integrated build sample (not a release gate)

One bounded WSL2 x86_64 release build on Linux 6.18 with Rust/Cargo 1.97.1
completed in 90.53 seconds with 3,901,536 KiB peak build RSS. The resulting
`mez` binary was 75,896,528 bytes. The existing shared `target/release`
directory occupied 15,246,428,797 bytes, and `cargo tree -p mezzanine --locked`
contained 1,014 unique rendered lines.

These values are a local integrated sample only. The target directory included
unrelated existing workspace artifacts, the build was not paired with a
same-commit non-Iroh binary, and WSL2 is not a supported-platform packaging
result. Do not use these numbers as package impact, acceptance thresholds, or
native Linux/macOS evidence.

| Gate | Current repository evidence | Required release evidence | Status |
| --- | --- | --- | --- |
| Disabled default and Unix recovery | Configuration and coexistence regressions preserve Unix control. | Packaged daemon rollback drill. | Locally verified; packaged drill pending. |
| Policy validation | Schema and effective runtime reject contradictory route, relay, lookup, type, and bound combinations. | Validate approved production configuration. | Locally verified. |
| Direct local path | Direct Iroh control, events, reconnect, malformed traffic, timeout, abrupt loss, and stream limits have focused tests. | Native Linux and macOS controlled runs. | Local Linux environment only; macOS pending. |
| Relay-required and direct-plus-relay | Explicit configuration paths exist. | Approved custom relay, outage, migration, latency, throughput, and reconnect runs. | Pending; no production relay approved. |
| Lookup | Disabled, local, n0 DNS, and custom DNS policy are explicit. | Approved custom lookup ownership, DNS loss, retention, and recovery run. | Pending. |
| Network diversity | No repository unit test proves real NAT, IPv6, proxy, captive, loss, or reordering behavior. | LAN, representative NAT, IPv4 and IPv6, proxy and CA, latency, loss, and reordering matrix. | Pending. |
| Abuse and bounds | Connection, stream, frame, queue, setup, idle, slow-consumer, and shutdown bounds have local regressions. | Descriptor, memory, CPU, and connection-flood measurements. | Functional bounds verified; measurements pending. |
| Privacy | Aggregate status and metrics have redaction regressions; documentation states direct and relay metadata exposure. | Production telemetry schema and retention review. | Local redaction verified; production review pending. |
| Performance and package impact | Focused tests provide behavior evidence only. | Cold startup, memory, CPU, release binary and package size, direct and relay latency and throughput, reconnect, observer fan-out, and concurrent-session baseline. | Pending. |
| Compatibility | ALPN and protocol version are fixed and explicit targets never fall back. | Supported client/server upgrade and rollback matrix. | Protocol behavior verified; packaged upgrade matrix pending. |

Do not label the Iroh transport supported for production while any pending gate
lacks an approved exception and a tracker record with an owner and due date.

## Related pages

- [Remote pairing and recovery](../safety-and-trust/remote-pairing-and-recovery.md)
- [Configuration reference](../configuration/reference.md)
- [Control JSON-RPC](../reference-manual/protocols/control-json-rpc.md)
- [Lifecycle, detach, and recovery](lifecycle-detach-and-recovery.md)
