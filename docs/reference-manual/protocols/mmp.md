# `mmp/1` local message protocol reference

## Purpose

Specify Mezzanine Message Protocol version 1 (`mmp/1`), the local service for
agent discovery, direct/group messaging, presence, correlation, and task
status. It is separate from the `mezctl/2` direct-session and `mezctl/3`
persistent-host control endpoints: MMP never creates panes, changes layout, or
performs multiplexer control. The [MMP section of `SPEC.md`](../../../SPEC.md#12-local-message-passing-protocol)
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
The physical header has a separate 8192-byte maximum including its final
`\r\n\r\n`; unterminated headers fail when a terminator can no longer fit.
Body bytes and subsequent buffered frames are not part of this header budget.

## Envelope and identity

The normative versioned message envelope is an object with these fields:

| Field | Meaning |
| --- | --- |
| `protocol` | Exactly `mmp/1`. |
| `id` | Globally unique message ID in the session. Recipients treat it as an idempotency key. |
| `type` | One of the message types accepted by the endpoint below. |
| `time` | RFC 3339 or documented monotonic time. |
| `sender` | Authenticated sender identity. Registered agents include `agent_id`; pane, window, role, capabilities, and the bounded generated `objective` may be present. |
| `recipient` | Agent, pane, window, session, role, capability query, or group target. |
| `scope` | Optional top-level audience: `project`, `session`, or `null`; omitted or null means `project`. This is separate from the recipient selector. |
| `correlation_id` | Related request ID, or `null`. |
| `ttl_ms` | Time-to-live in milliseconds, or `null`. |
| `content_type` | Payload media type. |
| `payload` | The application payload. |

The current endpoint enforces this full shape for `send`, `task_status`, and
`task_result`. Its service operations use reduced, operation-specific request
objects instead. Registration `hello` contains `protocol`, `type`, an optional
non-empty `role`, optional `capabilities`, and optional top-level `objective`;
an omitted role defaults to `default`. The service assigns the effective
identity in `welcome`. Raw `hello` assigns neither pane/window identity nor
trusted project membership. Client-supplied project fields cannot establish
that membership; the trusted runtime registration/rebinding path owns it.

Runtime-authored subagent lifecycle envelopes use IDs shaped as
`<turn-id>:task_status:<state>:<sequence>` and
`<turn-id>:task_result:final:<sequence>`. The final component is the durable
MMP acceptance sequence. It keeps lifecycle IDs unique across snapshot restore
and distinguishes repeated occurrences of the same task state.

These reduced service-operation shapes are an implementation conformance gap.
`SPEC.md` requires every message envelope to carry the full envelope and only
allows a pre-registration `hello` sender to omit `agent_id` or use a
provisional ID. The current full-envelope parser also requires `time` to be a
non-empty string but does not yet validate the normative RFC 3339 or documented
monotonic-time grammar.

After registration, the service—not the sender—validates identity against the
authenticated connection. Mismatched sender claims are rejected unless a
documented trusted bridge rewrites them. MMP currently preserves accepted
non-reserved extension fields at the top level, as required for forwarding by
the MMP contract; this is an explicit exception to the shared
`extensions`-object convention.

Registered sender identities may project an optional `objective`: the agent's
bounded, model-generated statement of current work for peer discovery. Raw
`hello` and `presence` requests publish it at the **top level**, not inside a
`sender` object. The field is additive to `mmp/1`; it does not change the
protocol version or non-reserved extension forwarding. A supplied string must
normalize to non-empty text of at most 2097152 bytes (the agent-shell prompt
ingestion bound), whitespace-collapsed to one line and free of control
characters. Multi-line text is collapsed rather than rejected solely for
spanning lines. Missing or null objective does not clear a published value.
An objective-only refresh with no value is a no-op, but a raw `presence`
request still updates status and its timestamp even when objective is absent.
`welcome.identity` and `discover_result.agents` project the registered
objective; raw `presence` returns an `ack`, not an identity projection.

### Registration and presence request shapes

```json
{"protocol":"mmp/1","type":"hello","role":"worker","capabilities":["docs"],"objective":"Checking protocol documentation."}
```

The response is `{"protocol":"mmp/1","type":"welcome","identity":{...}}`.
Use the assigned `identity.agent_id` in later full envelopes. On that same
registered connection, a presence update can be:

```json
{"protocol":"mmp/1","type":"presence","id":"presence-1","status":"busy","objective":"Validating protocol examples."}
```

Accepted statuses are `available`, `busy`, `blocked`, and `offline`; omitted
status defaults to `available`. Dispatch updates status and `updated_at_ms`
using service time, then applies any supplied objective. It does **not** update
capabilities; those are registration metadata. The response is:

```json
{"protocol":"mmp/1","type":"ack","message_id":"presence-1","queued_recipients":0}
```

The request ID is optional; without it `message_id` is null. A heartbeat also
returns this ack shape and updates liveness time without changing status.

## Message types

| Type | Direction and meaning |
| --- | --- |
| `hello` | Client registers with the service. |
| `welcome` | Service confirms registration and assigned identity, including the registered `objective` when published. |
| `discover` | Query agents by identity, pane, window, role, status, or capabilities. |
| `discover_result` | Discovery response carrying matched sender identities with their published `objective` values. |
| `send` | Submit application payload for a recipient or scope. |
| `mmp.receive` | Poll a subscribed recipient for a delivery batch; optional `limit` defaults to 100. |
| `transport/receive` | Compatibility alias for `mmp.receive`. |
| `deliver` | Service delivers a batch containing `cursor` and sequenced `messages`. |
| `ack` | Service response acknowledging sender-side acceptance, or recipient request advancing a subscription through `sequence` (or compatibility field `last_sequence`). |
| `error` | Structured protocol or delivery failure. |
| `presence` | Update status and presence time, optionally publishing a top-level `objective`; returns an ack. It does not update capabilities. |
| `heartbeat` | Prove connection liveness. |
| `task_status` | Report task state. |
| `task_result` | Report task completion. |

Types outside the baseline list must use a reverse-DNS or URI-like namespace.
The current endpoint recognizes that namespace grammar during validation but
does not dispatch extension types, so it rejects them as unsupported endpoint
operations. Namespace syntax alone does not advertise extension support.

## Delivery audience and recipient selection

Raw `send`, `task_status`, and `task_result` envelopes accept optional top-level
`scope: "project" | "session" | null`. Omission or null resolves to `project`;
other values are invalid. Project delivery requires the authenticated sender's
trusted project membership and reaches only matching identities. A sender
without membership fails closed, not by widening to the session. A
cross-project direct recipient is indistinguishable from an absent or
unavailable target. `session` is an explicit visibility widening request, not
authorization to execute work. The service records the resolved audience at
acceptance; later runtime membership changes do not rewrite queued audiences.

`recipient` is an object with exactly one supported selector: `agent_id`,
`pane_id`, `window_id`, `role`, `capability`, `group`, or `session: true`.
Multiple independent selectors are rejected. A selector such as
`{"session":true}` or `{"group":"session"}` does not itself widen audience;
with omitted scope it still selects only the sender's project peers.

For a raw client registered by `hello`, project discovery shows only self
when no additional discovery filters exclude self, until the trusted runtime
supplies membership. Default/project sends fail
without that membership. Explicit `scope: "session"` allows session-wide
discovery or delivery within the local service. For example, after `hello`,
substitute its assigned agent ID in this complete request:

```json
{"protocol":"mmp/1","id":"docs-handoff-1","type":"send","time":"2026-01-01T00:00:00Z","sender":{"agent_id":"a1"},"recipient":{"role":"worker"},"scope":"session","correlation_id":null,"ttl_ms":null,"content_type":"text/plain; charset=utf-8","payload":"Protocol documentation is ready for integration."}
```

The caller's own `sender.agent_id` must match the registered connection; the
service uses the canonical registered identity, not sender-supplied membership
or objective claims. Both nullable `correlation_id` and `ttl_ms` are required
in these full raw envelopes even when null. Discovery uses a reduced request,
for example `{"protocol":"mmp/1","type":"discover","scope":"session"}`;
its result reports the resolved `scope` and filtered `agents` identities.

## Delivery, expiry, and errors

MMP preserves acceptance order for one sender, one recipient, and one logical
channel. Explicit receive returns a `deliver` object shaped as
`{"protocol":"mmp/1","type":"deliver","cursor":{"recipient":"...","last_sequence":N},"messages":[{"sequence":N,"envelope":{...}}]}`.
An `ack` advances the durable subscription cursor through the supplied
sequence. The current automatic fanout path, however, advances its server-side
cursor after writing a delivery frame rather than after a recipient `ack`.
This is a known implementation conformance gap against the normative
at-least-once requirement in `SPEC.md`: automatic fanout is currently
connection-oriented best effort. A disconnect after the server write but
before application consumption can lose that unconsumed delivery. Integrators
must not treat the current automatic fanout path as an end-to-end receipt.

The sender receives an `ack` with `message_id`, `queued_recipients`, and
`status` when a message is accepted for delivery. A recipient's cursor-advance
`ack` instead supplies `sequence` (or `last_sequence`) and receives the new
`last_sequence`. Body-level failures use
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
separate wire protocols. After message-service acceptance, eligible
model-authored sends may produce one durable `${recipient}< {payload}` sender
presentation row; recipient rows remain separate `{sender}> {payload}` records
created only by recipient commit. Sender retention is operator-visible evidence
of acceptance or queueing, not delivery confirmation, acknowledgment, agreement,
or completion, and never participates in receiver receipts, cursors, recovery,
or MMP routing.
Both sender and recipient pane-log continuations start five display spaces
after the `▐ ` gutter, independent of endpoint-label width; Markdown structural
indentation is additive, and copied source excludes presentation-only padding.
Ordinary concrete-agent labels use the bounded, sanitized runtime-owned
subagent display name when available, otherwise the canonical raw agent id;
mutable pane, window, or conversation titles are never used as agent identity.
Receiver receipts preserve their resolved labels for retry and replay, while
non-agent selector expressions remain the explicitly addressed label.

## Related pages

- [Protocol conventions](common-conventions.md)
- [`maap/1` action protocol](maap.md)
- [Subagents and messaging](../../agent/subagents-and-messaging.md)
- [Normative MMP contract](../../../SPEC.md#12-local-message-passing-protocol)
