# Remote pairing and recovery

## Purpose

Explain the application trust boundary for optional Iroh control, the local
pairing and revocation workflow, and recovery when a device or server identity
is lost.

## Prerequisites

Keep the user-private Unix control socket available. Iroh transport is disabled
by default. Enable `[transport.iroh]` only in primary-user configuration and
restart the daemon. The enabled listener runs alongside Unix control; failure
to bind an explicitly enabled endpoint fails startup rather than silently
removing remote service. Unix remains the administration and recovery path.

## Understand the identities

An Iroh endpoint ID proves possession of a network key. It does not grant a
Mezzanine role. Mezzanine authority comes from a durable per-session trust
record that binds the current server endpoint ID, the authenticated client
endpoint ID, a stable record ID, a maximum `observer` or `primary` role,
revocation state, and a verifier for a device credential.

Server endpoint keys and `trust.json` live below the primary configuration
root under a hashed `remote/sessions` directory. The client endpoint key,
profile metadata, and separate device-credential files live under
`remote/client`. Files are owner-only, bounded, opened without following
symlinks, and updated with atomic replacement. Live server and client endpoint
keys retain exclusive locks, and client profile database access is serialized
under a protected lock. Do not edit these files directly.

## Pair a device

Use only the local Unix control path:

```console
mez remote status
mez remote invite --role observer --expires 600
```

`status` reports the public per-session endpoint identity and, while the listener
is bound, its current dialable endpoint address. `invite` returns a short-lived
bearer token once together with that pinned address, role, expiry, and profile
name. Transfer the JSON through a confidential channel and store it in an
owner-only file:

```console
umask 077
mez --json remote invite --role primary --expires 600 > mez-invite.json
```

The invitation is server-bound, role-limited, and single-use. A remote client
selects it explicitly with `--iroh-invite-file`; no failed remote attempt falls
back to Unix. The invitation is consumed only after remote
`control/initialize` succeeds. The successful response returns a device
credential once, and the client atomically publishes a protected profile only
after receiving that success. Later control and interactive attach use
`--iroh-profile PROFILE`. The direct Iroh CLI surface supports `attach`, `kill`,
and `detach`. For example, use `mez --iroh-invite-file mez-invite.json attach`
for first pairing or `mez --iroh-profile PROFILE attach` for reconnect; add
`--observer` to request observer access. An observer-limited invitation or
profile cannot attach as primary.

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
Only the explicitly requested invitation response/file and successful pairing
response contain their respective one-time secret.

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

## Related pages

- [Configuration reference](../configuration/reference.md)
- [Control JSON-RPC](../reference-manual/protocols/control-json-rpc.md)
- [Audit and diagnostics](audit-and-diagnostics.md)
- [Lifecycle, detach, and recovery](../operations/lifecycle-detach-and-recovery.md)
