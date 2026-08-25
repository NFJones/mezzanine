# Remote pairing and recovery

## Purpose

Explain the application trust boundary for optional Iroh control, the local
pairing and revocation workflow, and recovery when a device or server identity
is lost.

## Prerequisites

Keep the user-private Unix control socket available. Iroh transport is disabled
by default. Enable `[transport.iroh]` only in primary-user configuration and
restart the owning daemon. A per-session identity listener runs alongside Unix
control; failure to bind an explicitly enabled per-session endpoint fails
startup rather than silently removing remote service. A host-scoped identity is
owned only by `mez host serve`: direct `mez` and `mez serve` continue with Unix
control and do not attempt to bind it. Unix remains the administration and
recovery path.

## Understand the identities

An Iroh endpoint ID proves possession of a network key. It does not grant a
Mezzanine role. Mezzanine authority comes from a durable trust record that
binds the current server endpoint ID, the authenticated client endpoint ID, a
stable record ID, a maximum `observer` or `primary` role, revocation state, and
a verifier for a device credential. Persistent-host trust is shared across the
host; direct-session compatibility trust remains isolated to that session.

The persistent host key and `trust.json` live below `remote/host` in the
primary configuration root. Legacy direct-session keys and trust remain below
hashed `remote/sessions` directories. The client endpoint key, profile
metadata, and separate device-credential files live under `remote/client`.
Files are owner-only, bounded, opened without following symlinks, and updated
with atomic replacement. Live server and client endpoint keys retain exclusive
locks, and client profile database access is serialized under a protected
lock. Do not edit these files directly.

## Pair a device

Use only the local Unix control path:

```console
mez remote status
mez remote invite --role observer
```

`status` reports the public persistent-host endpoint identity and, while the listener
is bound, its current dialable endpoint address. `invite` returns a short-lived
bearer token once together with that pinned address, role, expiry, and profile
name. The versioned invitation envelope currently uses format version 1. Its
lifetime defaults to `transport.iroh.invitation_ttl_seconds` (600
seconds by default); `--expires SECONDS` explicitly overrides that lifetime
within the supported 30 through 86,400 second range. Transfer the JSON through
a confidential channel. Prefer secure no-overwrite output:

```console
mez remote invite --role primary --output mez-invite.json
```

The output file is created mode `0600`, existing files and symlinks are refused,
and the terminal receives only the created path. `mez remote invitation inspect
mez-invite.json` reports only expiry, role, an abbreviated server fingerprint,
and direct/relay route counts; it never prints the token.

The one-shot local administration connection releases a primary that it
created when the request completes, so the invited remote primary can redeem
without a manual detach. If an interactive primary already existed, the
administration request may reuse its authority but does not detach or replace
that primary when the request closes.

The invitation is server-bound, role-limited, and claimable by one authenticated
client endpoint. A remote client selects it explicitly with
`--iroh-invite-file`; no failed remote attempt falls back to Unix. The
invitation is consumed only after remote `control/initialize` succeeds. The
recipient does not need to enable an inbound Iroh listener: explicit outbound
use is enabled separately by `transport.iroh.outbound_enabled` and uses only
the invitation's pinned direct or relay routes without implicit lookup or port
mapping. Administrators can set that field to false to block outbound Iroh.
The successful response returns a device credential, and the client atomically
publishes a protected profile only after receiving that success. If the
response is lost or local profile persistence fails, retry the same invitation
from the same client endpoint before it expires. The server returns the same
credential and reuses the existing trust record; another endpoint cannot claim
or resume it. This retry does not replay a later control or terminal request.
Use a human-readable client-local alias while pairing or attaching:

```console
mez remote pair --invite-file mez-invite.json --name home-mez
# Or pair and immediately attach:
mez --iroh-invite-file mez-invite.json --save-as home-mez attach
```

Aliases are display and reconnect metadata only. They do not grant authority or
replace endpoint identity, role, or credential checks. If an alias already
belongs to a different server endpoint, the client fails before dialing or
redeeming the invitation and preserves the existing profile. Reusing an alias
for the same endpoint may refresh authenticated routes and credentials.
`remote pair` authenticates temporarily as an observer and self-detaches, so it
does not displace a primary or leave a pending observer request.
Foreign-machine invitations include only a relay route or a non-loopback direct
route on a configured non-zero `transport.iroh.bind_port`. Direct-only
deployments must configure that stable port and the network must make it
reachable. Relay-backed routes survive direct-port changes. If address lookup
is explicitly configured, a paired profile may resolve and persist refreshed
route hints only after the connection authenticates the same pinned server ID
and device credential.
Later control and interactive attach use `--iroh-profile PROFILE`. Inspect and
manage local reconnect profiles without exposing credentials:

```console
mez remote profile list
mez remote profile show home-mez
mez remote profile rename home-mez workstation
mez remote profile check workstation
mez remote profile remove workstation
```

Rename changes only the alias. Remove deletes only the local profile; it does
not revoke the server trust record. Check authenticates through a temporary
observer and self-detaches. Server-side revocation remains the local Unix
`remote revoke` workflow below. The direct
Iroh CLI surface supports `attach`, `kill`, and `detach`. For example, use `mez
--iroh-invite-file mez-invite.json attach` for first pairing or `mez
--iroh-profile PROFILE attach` for reconnect; add `--observer` to request
observer access. An observer-limited invitation or profile cannot attach as
primary.

Configuration schema 73 provides a persistent-host Iroh owner with one stable
endpoint identity and host trust database. Pairing remains mandatory once per
client device, but invitation redemption and profile checks use protocol-v3
`host_only` initialization and cannot create, select, or attach a session.
After pairing, authorized create and attach operations do not repeat the
pairing flow. A host endpoint ID remains transport evidence only; the
host-scoped trust record carries application authority and revocation state.

Protected client profiles now carry an explicit scope: `host` or
`legacy_session`. Profiles written before scope metadata existed are loaded as
`legacy_session`; they are never silently broadened to host authority. A host
profile uses `host_only` for pairing and health checks. Omitted-target `attach`
and `new`, explicit lease attachment, and `attach --default` route through the
persistent host. Lease administration remains local-only: active release or
revocation requires explicit runtime termination, while device trust revocation
continues to affect that device across leases. Runtime kill, lease release,
lease revocation, and device trust revocation remain separate operations.

Setup timeout errors name the failed stage and deadline, summarize pinned
direct/relay route counts, and state when no Mezzanine authentication occurred.
This diagnostic is safe to share because it omits route addresses, tokens,
credentials, and private keys.

Interactive attach retains one initialized control stream and orders each
resize, terminal-input, and view request behind its response. It explicitly
negotiates one server-opened event stream only after authenticated
initialization. Event authorization is rechecked for every bounded batch:
pending observers receive no stream before approval, approved observers receive
only post-approval session-view events, and revocation or detach closes the
stream. Endpoint identity alone cannot reveal events. If terminal input may have
been written when the connection fails, the client reports an unknown outcome,
does not replay the input or reconnect automatically, and requires a new
explicit attach. Unix control remains available concurrently for recovery.

Invitations, device credentials, private endpoint keys, and persisted verifiers
are omitted from client lists, diagnostics, debug output, and audit records.
Only the explicitly requested invitation response/file and successful or
same-endpoint recovery pairing response contain their respective secret.

## Inspect, rename, and revoke

```console
mez remote clients
mez remote rename remote-RECORD-ID "work laptop"
mez remote revoke remote-RECORD-ID --reason "device retired"
```

These commands are primary-only and local-Unix-only. A paired Iroh primary
cannot create invitations or inspect, rename, or revoke trust. Revocation makes
future device-proof initialization fail closed. Existing Unix control remains
available concurrently and is the recovery path after revocation, malformed
remote traffic, setup timeout, ALPN failure, abrupt loss, or listener failure.
To pair the same persistent client endpoint again after revocation, create and
transfer a new invitation. The new credential resolves against the new active
trust record; the old credential remains rejected and the revoked record stays
available as audit-safe history.

## Back up or recover identity

Stop the session daemon before copying remote identity state so the endpoint
and trust database remain a consistent pair. Preserve owner-only permissions
and protect backups as credentials. Restoring only one side is not sufficient:
trust records and outstanding invitations are bound to the server endpoint ID.

If the endpoint key is lost, corrupted, or intentionally replaced, prior
invitations and device credentials no longer authenticate to the new server
identity. Keep using local Unix control, inspect status, and pair devices again.
If the trust database is lost or corrupted, do not reconstruct verifiers by
hand; preserve or replace the failed state offline and re-pair through local
control.

## Privacy and rollout

Application payloads are encrypted by the Iroh connection, but networking can
still expose metadata. Direct peers can observe peer IP addresses. Relays and
lookup infrastructure can observe endpoint IDs, addresses, timing, connection
relationships, and relayed byte counts even though they cannot read encrypted
application data. Select lookup, relay, direct connection, port mapping, proxy,
and CA policies independently and deliberately.

Public n0 relay and lookup services are development dependencies, not an
approved Mezzanine production service. A supported deployment requires named
relay and lookup ownership, metadata retention and incident policy, controlled
network evidence, staged stop thresholds, and a tested Unix-only rollback. See
[Iroh production operations and rollout](../operations/iroh-production-operations-and-rollout.md).

## Related pages

- [Configuration reference](../configuration/reference.md)
- [Control JSON-RPC](../reference-manual/protocols/control-json-rpc.md)
- [Audit and diagnostics](audit-and-diagnostics.md)
- [Lifecycle, detach, and recovery](../operations/lifecycle-detach-and-recovery.md)
