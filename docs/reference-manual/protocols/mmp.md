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
| `type` | One of the message types below, or a namespaced extension. |
| `time` | RFC 3339 or documented monotonic time. |
| `sender` | Authenticated sender identity. Registered agents include `agent_id`; pane, window, role, and capabilities may be present. |
| `recipient` | Agent, pane, window, session, role, capability query, or group target. |
| `correlation_id` | Related request ID, or `null`. |
| `ttl_ms` | Time-to-live in milliseconds, or `null`. |
| `content_type` | Payload media type. |
| `payload` | The application payload. |

Before registration, `hello` may omit `sender.agent_id` or use a provisional
ID. The service assigns the effective identity in `welcome`. The service—not
the sender—validates identity against the authenticated connection. Mismatched
sender claims are rejected unless a documented trusted bridge rewrites them.
Forwarders preserve the effective identity and unknown envelope fields unless
policy removes them.

## Message types

| Type | Direction and meaning |
| --- | --- |
| `hello` | Client registers with the service. |
| `welcome` | Service confirms registration and assigned identity. |
| `discover` | Query agents by identity, pane, window, role, status, or capabilities. |
| `discover_result` | Discovery response. |
| `send` | Submit application payload for a recipient or scope. |
| `deliver` | Service delivers application payload. |
| `ack` | Acknowledge accepted handling or delivery. |
| `error` | Structured protocol or delivery failure. |
| `presence` | Announce status or capability changes. |
| `heartbeat` | Prove connection liveness. |
| `task_status` | Report task state. |
| `task_result` | Report task completion. |

Extension message types must use a reverse-DNS or URI-like namespace.

## Delivery, expiry, and errors

MMP provides at-least-once delivery to registered local recipients while they
remain available and the message has not expired. The service continues trying
an open writable recipient connection without requiring another request frame.
For one sender, one recipient, and one logical channel, delivery order matches
acceptance order.

The sender receives `ack` when a message is accepted for delivery. It receives
`error` when the message is rejected, undeliverable, or expires first. An error
payload contains `code`, `message`, and `retryable`, with non-secret `details`
when useful. Baseline codes are `unsupported_protocol`, `malformed_frame`,
`invalid_envelope`, `unauthorized`, `not_found`, `expired`,
`payload_too_large`, `rate_limited`, `policy_denied`, and `internal_error`.

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
