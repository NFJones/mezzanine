# Iroh production operations and rollout

## Purpose

Operate the optional Iroh client/server transport without weakening local Unix
recovery, hiding network metadata exposure, or treating public development
infrastructure as a production dependency.

This page is an operator gate, not evidence that a production relay or lookup
service has been selected. Iroh remains disabled by default.

## Prerequisites

- Deploy and verify the [persistent multi-session host](persistent-host.md)
  with a working local Unix administration path.
- Understand the pairing, identity, and revocation boundaries in [Remote
  pairing and recovery](../safety-and-trust/remote-pairing-and-recovery.md).
- Assign an operator who can approve infrastructure ownership, collect the
  release evidence below, and perform a tested Unix-only rollback.

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
bind_port = 4242
relay_mode = "disabled"
address_lookup = "disabled"
direct_connections = true
port_mapping = false
```

Replace `4242` with an approved, firewall-permitted UDP port. Direct-only
foreign-machine invitations require a non-zero stable bind port and at least
one non-loopback route; ephemeral or loopback-only routes are rejected. Direct
peers can observe each other IP addresses. Enable port mapping only after the
controlled network plan measures the exposure and benefit.

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

Relay addresses are restart-safe route hints. Explicitly configured lookup may
refresh a paired profile after a route change, but the authenticated server
endpoint ID remains pinned and cannot be replaced by lookup results.

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

## Compression policy foundation

Schema v71 adds ordered `compression_codecs`, `compression_min_bytes`, and
`compression_zstd_level` settings. Compression is applied to complete
Mezzanine application frames, not by Iroh or QUIC. The v2 frame foundation
supports bounded Zstandard and LZ4 payloads plus per-frame identity fallback.
The runtime advertises and attempts streaming codecs first, preserving
configured preference within the streaming and non-streaming classes. It does
so only before opening an application stream, then keeps the selected codec
fixed for control, event, and negotiated X11 traffic until that connection
closes. An interleaved preference list is therefore intentionally reordered by
codec class.

Schema v75 additionally accepts `lz4-stream` and `zstd-stream` for opt-in
low-latency trials. These v3 variants reuse bounded direction-local history and
flush every logical frame; they do not use `compression_min_bytes` bypasses.
Initialization and event prefaces remain raw, and sensitive frames reset codec
history. A canary preference list may place a streaming codec first while
retaining existing fallbacks, for example:

```toml
[transport.iroh]
compression_codecs = ["lz4-stream", "zstd-stream", "zstd", "lz4", "none"]
```

Do not make a streaming variant the fleet default without direct and relayed
input-to-visible p50/p95/p99 latency, CPU, memory, wire-byte, loss, cold-context,
and maximum-concurrency measurements. Compare `lz4-stream` first for a
latency-first profile and `zstd-stream` where relay bandwidth dominates.

During staged rollout, retain `none` in canary preference lists for old-peer
compatibility and verify negotiation before reviewing CPU or bandwidth
effects. Immediate rollback is:

```toml
[transport.iroh]
compression_codecs = ["none"]
```

Restart the daemon after changing this policy. Never retry a different codec
after a stream is opened or initialization bytes are written; an ambiguous
application outcome must fail visibly instead of being replayed.

### Reproduce compression measurements

Run the single-threaded release harness from the repository root:

```text
just iroh-compression-bench
```

It writes `target/iroh-compression-bench.json` by default; override the path
with `MEZ_IROH_COMPRESSION_BENCH_REPORT`. Keep point-in-time measurements in
generated reports rather than this operational runbook.

### Reproduce render-update measurements

Run the content-safe, single-threaded release harness from the repository root:

```text
just iroh-render-bench
```

It writes `target/iroh-render-bench.json` by default; override the path with
`MEZ_IROH_RENDER_BENCH_REPORT`. The report measures snapshot/delta selection,
changed rows, selected and candidate bytes, codec cadence, and a clearly
labelled serialized-request RTT model. It does not claim to measure a real
direct or relay path.

## Enable and verify

1. Confirm the private Unix control socket works and retain a local primary
   administration path.
2. Run `mez config validate` before restart.
3. Start one canary persistent host with Iroh enabled by running `mez host
   serve` under the deployment's service manager.
4. Run `mez --json remote status` through local Unix control.
5. Confirm `enabled` and `endpoint_id`. This response does not prove listener
   health or expose route policy, counters, or a dialable address; verify the
   configured route policy separately and treat the running host plus a
   successful paired connection as the listener-health check.
6. Inspect `show-metrics` in the routed canary session when session-local Iroh
   diagnostics are needed. It is not a host-wide preflight surface.
7. From the paired remote client, run `show-iroh-status` in the command prompt
   to inspect its selected path, RTT, traffic, negotiated codec, interval wire
   savings, snapshot/delta counts, changed rows, selected/candidate bytes,
   coalescing, write wait, recent loss/congestion, and quality rating without
   exposing endpoint, route, credential, or payload data.
8. Pair one role-limited test device, exercise attach and detach, revoke it, and
   verify future initialization fails.
9. Confirm a local Unix client can still inspect, revoke, detach, and stop the
   session after ordinary remote connection failures. Also verify that the
   service manager restarts the host after a listener-task failure, because
   the current listener supervisor stops the paired Unix listener too.
10. From a negotiated remote primary, copy a non-sensitive test value and verify
    the server internal buffer, server-host clipboard attempt, and attaching
    client clipboard independently. Confirm observers, other devices, and v1
    clients receive no client-local effect.

Persistent-host `remote/status` includes only `enabled` and `endpoint_id`.
Session-local `show-metrics` output intentionally omits endpoint IDs, peer
addresses, invitations, credentials, private keys, payloads, and trust records.

## Interpret diagnostics

The fields below belong to routed session-local diagnostics such as
`show-metrics`; they are not fields in persistent-host `remote/status`.

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

The client-local `show-iroh-status` codec and compression rows are not part of
the process-lifetime aggregate table. They reset with each connection-local
codec context. `insufficient sample` means no complete frame has crossed on the
current connection; a ratio below 1.00× or an `expansion` label warrants
workload review but does not change the route quality label. Compression and
render counters cover the connection lifetime, while recent loss and
congestion values are interval deltas.

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
3. Restart the host through the deployment's service manager so the supervised
   listener and network activity stop.
4. Confirm Unix attach and `mez remote status` still work and that `enabled` is
   false with no endpoint ID. The current host status response does not expose
   listener or active-connection counters.
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

Point-in-time build, benchmark, and acceptance samples belong in generated or
versioned release evidence, not in this runbook. The matrix below states what
the repository currently demonstrates and what each release must still collect
on supported platforms and approved infrastructure.

| Gate | Current repository evidence | Required release evidence | Status |
| --- | --- | --- | --- |
| Disabled default and Unix recovery | Configuration and coexistence regressions preserve Unix control; isolated host restart and lease recovery pass. | Packaged daemon rollback drill. | Locally verified; packaged drill pending. |
| Policy validation | Schema and effective runtime reject contradictory route, relay, lookup, type, and bound combinations. | Validate approved production configuration. | Locally verified. |
| Direct local path | Direct Iroh control, events, reconnect, malformed traffic, timeout, abrupt loss, and stream limits have focused tests. | Native Linux and macOS controlled runs. | Local Linux environment only; macOS pending. |
| Relay-required and direct-plus-relay | Explicit configuration paths exist. | Approved custom relay, outage, migration, latency, throughput, and reconnect runs. | Pending; no production relay approved. |
| Lookup | Disabled, local, n0 DNS, and custom DNS policy are explicit. | Approved custom lookup ownership, DNS loss, retention, and recovery run. | Pending. |
| Network diversity | No repository unit test proves real NAT, IPv6, proxy, captive, loss, or reordering behavior. | LAN, representative NAT, IPv4 and IPv6, proxy and CA, latency, loss, and reordering matrix. | Pending. |
| Abuse and bounds | Connection, stream, frame, queue, setup, idle, slow-consumer, and shutdown bounds have local regressions. | Descriptor, memory, CPU, and connection-flood measurements. | Functional bounds verified; measurements pending. |
| Privacy | Aggregate status and metrics have redaction regressions; documentation states direct and relay metadata exposure. | Production telemetry schema and retention review. | Local redaction verified; production review pending. |
| Performance and package impact | Local report-only release workload records throughput, RSS, and PTY/input/render latency; isolated direct create, detach, reconnect, and revocation paths pass. | Cold startup, memory, CPU, release binary and package size, direct and relay latency and throughput, reconnect, observer fan-out, and concurrent-session baseline. | Local sample recorded; packaged and network baselines pending. |
| Compatibility | ALPN and protocol version are fixed and explicit targets never fall back. | Supported client/server upgrade and rollback matrix. | Protocol behavior verified; packaged upgrade matrix pending. |
| Client clipboard | Negotiated v2 routing, exact-client isolation, bounded assembly, malformed/timeout handling, local command ownership, and v1 fallback have focused integration regressions. | Native Linux and macOS desktop/headless runs plus packaged old/new client-server matrix. | Repository behavior verified; native/platform matrix pending. |

Do not label the Iroh transport supported for production while any pending gate
lacks an approved exception and a tracker record with an owner and due date.

## Related pages

- [Remote pairing and recovery](../safety-and-trust/remote-pairing-and-recovery.md)
- [Configuration reference](../configuration/reference.md)
- [Control JSON-RPC](../reference-manual/protocols/control-json-rpc.md)
- [Lifecycle, detach, and recovery](lifecycle-detach-and-recovery.md)

## Next step

For a development or controlled-evaluation deployment, return to [Remote
pairing and recovery](../safety-and-trust/remote-pairing-and-recovery.md) and
create the narrowest invitation that fits the test. For production, stop until
every infrastructure decision and release-readiness gate above has an approved
owner and evidence.
