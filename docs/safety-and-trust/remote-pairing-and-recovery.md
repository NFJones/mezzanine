# Remote pairing and recovery

## Purpose

Explain the application trust boundary for optional Iroh control, the local
pairing and revocation workflow, and recovery when a device or server identity
is lost.

## Prerequisites

Keep the user-private Unix control socket available. Iroh transport is disabled
by default, and the listener and connector are a separate rollout surface from
the pairing foundation described here.

## Understand the identities

An Iroh endpoint ID proves possession of a network key. It does not grant a
Mezzanine role. Mezzanine authority comes from a durable per-session trust
record that binds the current server endpoint ID, the authenticated client
endpoint ID, a stable record ID, a maximum `observer` or `primary` role,
revocation state, and a verifier for a device credential.

Endpoint keys and `trust.json` live below the primary configuration root under
a hashed `remote/sessions` directory. Files are owner-only, bounded, opened
without following symlinks, and updated under locks with atomic replacement.
Do not edit these files directly.

## Pair a device

Use only the local Unix control path:

```console
mez remote status
mez remote invite --role observer --expires 600
```

`status` ensures the per-session endpoint identity exists and prints only its
public endpoint ID. `invite` prints a short-lived bearer token once. Transfer
it through a confidential channel. The invitation is server-bound,
role-limited, and single-use. It is consumed only after the remote
`control/initialize` request succeeds. The successful first response returns a
device credential once; later reconnects use that credential with the same
Iroh endpoint ID.

Invitations, device credentials, private endpoint keys, and persisted verifiers
are omitted from client lists, status, errors, debug output, and audit records.

## Inspect, rename, and revoke

```console
mez remote clients
mez remote rename remote-RECORD-ID "work laptop"
mez remote revoke remote-RECORD-ID --reason "device retired"
```

These commands are primary-only and local-Unix-only. A paired Iroh primary
cannot create invitations or inspect, rename, or revoke trust. Revocation makes
future device-proof initialization fail closed. Existing Unix control remains
the recovery path after revocation or remote transport failure.

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
