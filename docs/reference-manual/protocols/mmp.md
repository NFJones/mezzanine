# `mmp/1` local message protocol reference

## Purpose

Specify Mezzanine Message Protocol version 1 (`mmp/1`), the local service for
agent discovery, direct/group messaging, presence, correlation, and task
status. It is separate from `mezctl/1`: MMP never creates panes, changes
layout, or performs multiplexer control. The [MMP section of `SPEC.md`](../../../SPEC.md#12-local-message-passing-protocol)
is normative.

## Transport and framing

MMP is local to a Mezzanine session by default. Implementations provide a
reliable, ordered local transport—normally a Unix-domain socket in a
user-private runtime directory. Loopback TCP is optional, must be protected by
an unguessable session capability, and is disabled for remote access by
default.

Frames are UTF-8 JSON values preceded by an ASCII header block:

```text
Content-Length: <decimal-octet-length>\r\n
Content-Type: application/vnd.mezzanine.mmp+json; version=1\r\n
\r\n
```

`Content-Length` is the JSON body's octet length. Receivers reject missing,
invalid, negative, or oversized values and ignore unknown headers.

## Envelope and identity

Every envelope is an object with these fields:

| Field | Meaning |
| --- | --- |
| `protocol` | Exactly `mmp/1`. |
| `id` | Globally unique message ID in the session. Recipients treat it as an idempotency key. |
| `type` | One of the message types accepted by the endpoint below. |
| `time` | RFC 3339 or documented monotonic time. |
| `sender` | Authenticated sender identity. Registered agents include `agent_id`; pane, window, role, and capabilities may be present. |
| `recipient` | Agent, pane, window, session, role, capability query, or group target. |
| `correlation_id` | Related request ID, or `null`. |
| `ttl_ms` | Time-to-live in milliseconds, or `null`. |
| `content_type` | Payload media type. |
| `payload` | The application payload. |

The registration `hello` is the bootstrap exception to this full envelope: it
contains `protocol`, `type`, an optional non-empty `role`, and optional
`capabilities`. An omitted role defaults to `default`. It does not carry a
registered sender identity, recipient, delivery metadata, or application
payload. The service assigns the effective identity in `welcome`. After
registration, the service—not the sender—validates identity against the
authenticated connection. Mismatched sender claims are rejected unless a
documented trusted bridge rewrites them. MMP currently preserves accepted
non-reserved extension fields at the top level; this is an explicit exception
to the shared `extensions`-object convention.

## Message types

| Type | Direction and meaning |
| --- | --- |
| `hello` | Client registers with the service. |
| `welcome` | Service confirms registration and assigned identity. |
| `discover` | Query agents by identity, pane, window, role, status, or capabilities. |
| `discover_result` | Discovery response. |
| `send` | Submit application payload for a recipient or scope. |
| `mmp.receive` | Poll a subscribed recipient for a delivery batch; optional `limit` defaults to 100. |
| `transport/receive` | Compatibility alias for `mmp.receive`. |
| `deliver` | Service delivers a batch containing `cursor` and sequenced `messages`. |
| `ack` | Advance the recipient subscription through `sequence` (or compatibility field `last_sequence`). |
| `error` | Structured protocol or delivery failure. |
| `presence` | Announce status or capability changes. |
| `heartbeat` | Prove connection liveness. |
| `task_status` | Report task state. |
| `task_result` | Report task completion. |

Unknown message types, including otherwise well-formed namespaced types, are
rejected by the current endpoint.

## Delivery, expiry, and errors

MMP preserves acceptance order for one sender, one recipient, and one logical
channel. Explicit receive returns a `deliver` object shaped as
`{"protocol":"mmp/1","type":"deliver","cursor":{"recipient":"...","last_sequence":N},"messages":[{"sequence":N,"envelope":{...}}]}`.
An `ack` advances the durable subscription cursor through the supplied
sequence. The current automatic fanout path, however, advances its server-side
cursor after writing a delivery frame rather than after a recipient `ack`.
Therefore it is connection-oriented best effort, not an end-to-end
at-least-once guarantee: a disconnect after the server write but before
application consumption can lose that unconsumed delivery.

The sender receives `ack` when a message is accepted for delivery. Body-level
failures use
`{"protocol":"mmp/1","type":"error","error":{"code":"...","message":"...","retryable":false,"delivery_status":"..."}}`.
Current dispatch can emit `unsupported_protocol`, `payload_too_large`,
`expired`, `invalid_envelope`, `not_found`, `unauthorized`, `undeliverable`, and
`internal_error`, depending on the underlying failure. Framing failures such as
malformed or oversized frames can terminate the framed request before an MMP
error body exists.

## Payloads and MAAP bridge

Text payloads use `text/plain; charset=utf-8`; JSON uses
`application/json`. Binary data is base64 text with `payload_encoding` set to
`base64`. Receivers enforce configured payload limits.

A MAAP `send_message` action is one way an agent asks Mezzanine to deliver an
MMP message. Its `text/plain` shorthand is normalized to the canonical MMP
text media type. MAAP action results report recipient identity, message ID when
assigned, delivery status, and MMP protocol errors; MAAP and MMP remain
separate wire protocols.

## Related pages

- [Protocol conventions](common-conventions.md)
- [`maap/1` action protocol](maap.md)
- [Subagents and messaging](../../agent/subagents-and-messaging.md)
- [Normative MMP contract](../../../SPEC.md#12-local-message-passing-protocol)
