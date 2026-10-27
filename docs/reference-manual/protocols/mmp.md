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

For native pane agents, registered role and populated capabilities describe the
stable native registration, not the currently resumed conversation kind. An
independently resumed root conversation can therefore have discovery kind
`primary` while its original role remains `worker` or `explorer`. Trusted generic
runtime refresh preserves that exact routing metadata only for the matching
actor-owned pane/session; explicit creation conflicts and project/pane/window
checks remain strict. These labels grant no parent, cleanup, scope or approval
authority. Resume neither clears the directory/cursors nor creates another ID.

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

### Task payloads

`task_status` and `task_result` use the full envelope described above, with
`content_type: "application/json"` and an object payload. Their required fields
are validated before acceptance:

| Type | Payload fields |
| --- | --- |
| `task_status` | Non-blank string `task_id`, string `state`, and string `summary`. States are `queued`, `running`, `blocked`, `succeeded`, `failed`, or `cancelled`. Optional `progress_percent` is null or an integer from 0 through 100. |
| `task_result` | Non-blank string `task_id`, boolean `success`, string `summary`, and string `output`. Both strings are required even when empty. |

For example, these are payload objects, not complete requests:

```json
{"task_id":"task-1","state":"running","progress_percent":25,"summary":"Checking protocol examples."}
```

```json
{"task_id":"task-1","success":true,"summary":"Checks passed.","output":"Two focused tests passed."}
```

Malformed payloads produce `invalid_envelope` without queueing the message.
Acceptance acknowledges a report, not independent verification of the reported
task outcome and not authority to execute work.

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

Acceptance also freezes the exact eligible recipient registrations. A late
registration, previously offline peer, or newly matching sparse metadata cannot
acquire old fanout. Reusing an opaque ID after retirement starts a new interval;
neither live nor historical reads inherit the old owner's mail. Current
availability, selector and audience checks still apply, so acceptance does not
grant permission or bypass project isolation.

Retiring a reusable/native identity removes its discovery, presence and
subscription, but preserves shared canonical envelopes, acceptance receipts and
other recipients' offer/cursor evidence. In particular, retirement of a fast
fanout recipient cannot delete a slow recipient's unread copy. The existing
delivery floor and frozen registration bindings prevent a replacement owner or
late matching registration from acquiring old mail. Direct-only retained mail
may remain as bounded evidence even after its recipient retires; it is not
evidence of delivery or a terminal disposition. Unsatisfied expired/retired
obligations remain charged until explicit terminal maintenance settles them,
never silently treated as a successful ACK.

The lower `MessageService::settle_delivery_outcomes_at` API records one typed
`expired` or `recipient_retired` disposition in each pre-reserved recipient slot.
Expiry uses the original strict TTL boundary; retirement means the exact
registration interval ended, not merely offline presence. First reason/time
wins, and clock rollback cannot revive terminal mail. Terminal slots never
become recipient ACKs, even through a later cumulative ACK. A genuine later
offered endpoint can advance across a terminal prefix without consuming its
outcome.

`delivery_outcomes_for` exposes at most 100 payload-free receipts per call to
the authenticated original sender interval. `acknowledge_delivery_outcome`
releases only an exact receipt after caller-owned handoff commits. Reused IDs
cannot consume old receipts; unknown legacy sender intervals remain unclaimable.
Outcomes remain reserved for ten minutes from first service observation, then
explicit maintenance releases them. Retries/rollback do not renew the horizon.
Shared custody releases only after every recipient ACKs or its terminal receipt
is consumed/ages out; optional replay may retain terminal tombstones afterward.
This is wall-clock-based, not monotonic elapsed-time or crash-durability proof.

**Integration boundary:** the product timer invokes this maintenance after
native commit reconciliation. Only an exact original live accepted action can
consume its handoff, as described below; other owners retain the bounded
receipt until expiry. No raw wire action, ordinary mail, model wake, task retry
or fabricated successful delivery is introduced by this contract.

Accepted same-runtime custody is independent of optional replay history. The
pending and replay references share the original immutable occurrence; replay
FIFO eviction cannot discard unread direct mail or a slow fanout recipient's
obligation, acceptance receipt, offer evidence or required sender provenance.
Sequenced receive/fanout returns custody-only mail in original acceptance order.
Unsequenced replay/history views remain optional bounded history. Subscribing
late cannot skip accepted unread mail; repeated subscriptions preserve their
cursor. Explicit reconstruction can inspect committed retained evidence without
redelivering it as new work.

Internal pending ceilings are 1000 occurrences, 1 MiB unique payload, 4096 frozen
recipient slots and 256 slots per exact recipient interval, independently of
the replay window. Fanout charges the shared payload once; committed slots are
conservatively charged until the last recipient commits or its terminal receipt
is released. The last exact offered ACK releases pending custody, not a remaining replay
reference. Capacity is reserved before assigning sequence/success. These limits
do not add a config knob, disk spool, fair deferred-send scheduler, or durable
acceptance. The existing one-second product delivery timer also maintains
future TTL expiry, exact recipient retirement and unconsumed terminal horizons
when live fanout is empty. TTL-free merely offline mail and already released
slots do not independently keep terminal maintenance ticking.

For an exact original live `send_message` action, the timer commits one
payload-free, summarizable runtime reference before consuming its receipt.
Compaction can retire settled facts without recreating their consumed receipts.
Original sender registration, generated turn/action ID, acceptance sequence,
task context and current conversation must agree. Ownership is captured at
actual native acceptance, survives provider batch replacement and is bounded
by at most 1000 canonical pending occurrences. It stores original identities
and occurrence references, not payloads, and does not restore task authority
after restart. The original action remains accepted. This fact does not act
as peer mail, resume a wait, wake a provider or retry effects.
Ended/replaced/unprovable owners retain the bounded receipt until ten minutes
from first observation, then maintenance releases it without handing it to
another conversation. Exact eligible occurrence queries inspect at most 100
outcomes per sender/sweep; earlier unowned receipts cannot hide a later live
owner's handoff. Request-aware runnable recovery, fairness and crash-durable
acceptance remain separate work.

Native receive receipts, including invisible media commit proofs, share the
1,024-entry live bound. Admission reserves the whole batch before canonical
context mutation; overflow leaves the delivery unacknowledged.

Native receive admission seals same-runtime proof only after canonical context
storage. That proof survives Stop, context cleanup and failed-backing moves;
restored display projections and historical labels cannot claim it. Retiring a
pane/conversation suppresses its old display while retaining unacknowledged
proof until transport settlement. It cannot repaint a reopened conversation.

A later native input cannot cumulatively ACK an earlier offered input that
never committed. Proven input waiting on a prefix is protected from false TTL
failure and excluded from native fanout/prompt construction, so it neither
replays as new work nor hides genuinely unread mail behind a fanout limit.
Only a reconcilable prefix or finite terminal work keeps the maintenance timer
active. At exact native registration retirement, only proven own offers receive
positive ACKs; genuinely unsatisfied recipients keep their terminal obligations.
This adds no wire ACK action, task restart authority or acceptance-atomic crash
persistence. Unacknowledged same-runtime proof is not restored as task authority
from a snapshot's presentation outbox.

Trusted in-process callers can narrow each custody budget using
`PendingDeliveryLimits` with `MessageService::with_pending_limits`. Values must
be positive and cannot exceed the ceilings above. No wire/config control is
introduced. Policy is destination-owned, not saved in schema8 message state:
`from_snapshot_state` explicitly uses production defaults, while a narrowed
destination must use `from_snapshot_state_with_pending_limits` with its policy.
Over-cap current or reconstructed legacy custody is refused, never truncated.
Replay limits and payload validation remain independent; a pending byte budget
does not enlarge the maximum individually acceptable envelope.

The retained recipient set is bounded independently of payload bytes: at most
4096 records and 1 MiB logical metadata, charging 192 bytes plus each UTF-8 ID's
length. New sends reserve the whole fanout before sequence allocation. Capacity
rejection (`code: "capacity"`, `status: "rejected"`, no accepted receipt) leaves
the prior accepted set unchanged. Only fully settled history that the ordinary
replay count/byte policy actually evicts may release metadata for the new send.
Binding/offer/commit/terminal/sender metadata counts the deduplicated
replay/custody union.
The socket write-versus-application-commit limitation below remains unchanged.

`recipient` is an object with exactly one supported selector: `agent_id`,
`pane_id`, `window_id`, `role`, `capability`, `group`, or `session: true`.
Multiple independent selectors are rejected. A selector such as
`{"session":true}` or `{"group":"session"}` does not itself widen audience;
with omitted scope it still selects only the sender's project peers.
For `agent_id`, use the exact nonblank, control-free ID returned by discovery.
Generated IDs such as `a1` and opaque native IDs such as `agent-%1` are both
direct addresses. The parser does not trim or reinterpret them as pane IDs,
roles or display labels, and addressing an ID never registers it. Exact current
registration, availability and audience checks still apply; missing, retired
and audience-ineligible targets remain indistinguishably unavailable. Pane/window
selector grammar, sender binding and framing limits are unchanged.

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
An `ack` advances the subscription cursor only through an occurrence selected
for that exact acceptance-time recipient registration. Every earlier retained
own occurrence above the committed cursor must also have been offered, unless
expired at the endpoint's observed offer time. Foreign global sequence gaps
are harmless; unoffered, foreign and future ACKs reject without mutation, and
duplicate/regressive ACKs are inert. Explicit sequenced receive and automatic
fanout share this evidence; historical and unsequenced reads do not authorize
ACKs. Native context commit still precedes acknowledgement.

The current automatic fanout path, however, advances its server-side
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

Malformed framing (including invalid UTF-8 or content type), oversized input,
EOF/reset and response or fanout socket failures close only the offending
connection. The same listener and runtime actor continue serving replacements;
other panes and agents are unaffected. Framing is not resynchronized and no
request or delivery write is replayed. Body-level protocol rejection still uses
the error response above when a valid frame can be decoded.

Raw `hello` allocates a fresh observational identity, not a binding to an
existing pane identity. Repeating `hello` retires the previous raw identity;
connection exit/cancellation retires its latest identity and subscription.
Stale cleanup cannot retire another socket's fresh identity. Shared retained
envelopes and acceptance receipts remain intact, and a failed fanout write does
not advance its cursor. Continuous reaping releases active slots after cleanup;
final drain uses the same containment rules. Diagnostics log fixed reason
categories and aggregate counts at power-of-two failures, not peer data.

Actor ingress, audit/shared-state and listener accept failures still escalate;
task JoinErrors retain the existing critical policy. This isolation is based on
typed operation provenance, not blanket suppression of I/O or state errors.
Socket ownership is published before the hello reply, including when an audit
error follows registration. Final actor cleanup owns any raw lease still live
at shutdown; independently registered pane-less identities remain untouched.
Cleanup ownership corruption is critical and reaches completion after pane
termination, rather than being reported as a successful peer closure.

Private MMP snapshot schema8 retains schema4's distinction of fresh raw sender provenance from
reusable delivery floors. It preserves the exact objective versions in retained
authored mail, including when the current objective changed again without a
send. Every version must be referenced by canonical mail and agree on immutable
agent/project/pane/window/role/capabilities; mail, receipts and audience are not
rewritten. Releasing the final replay/custody reference removes unreferenced
versions, then the empty raw floor. This bounds version count by canonical authored occurrences, not by the
payload-only byte budget. Schemas1–3 reject the fresh-version field even when
empty. Active/reusable-owner exact validation is unchanged; this is not general
live-owner objective-history repair or a raw reconnect/rebind operation.
Snapshot restoration also reserves exact recipient binding IDs still referenced by
retained mail, including raw non-senders and senders whose authored floor has
already evicted. A fresh generated owner cannot acquire that recipient's old
direct or fanout mail through historical reads. This uses existing bounded mail rather
than restoring idle registrations or keeping extra identity tombstones.

Schema5 records each active registration's acceptance sequence floor and each
retained occurrence's exact accepted recipient intervals. A floor is the last
accepted sequence before registration; all mail for an earlier registration is
at or below a replacement's floor. Capture/restore preserves these facts, not
arbitrary reconnect authority. Schemas2–4 with retained mail are explicitly
refused as unknown acceptance-time recipients; the service does not guess a
recipient set from current registrations, nor rewrite the input snapshot.
Empty-mail legacy snapshots still restore, while schema1 retains its existing
audience-less-mail discard policy. This format version does not promise an
acceptance-atomic disk write or transparent workflow restart.

Schema6 additionally preserves exact offered-recipient subsets and their
selection clock high-waters. Missing, duplicate, foreign and TTL-inconsistent
offer records reject before reconstruction. Schema5 mail restores its known
recipients with no offer evidence; a fresh receive/fanout selection is required
before new cursor advancement. One optional clock slot is preallocated per
accepted binding within the existing metadata reservation, not an unbounded ACK
history. Offers remain tied to the original registration through retirement and
opaque-ID reuse, and cannot establish application consumption or reconnect
rights.

Schema7 adds custody-only sources and exact committed-recipient subsets. Each
source appears once, in either replay or custody-only rows; indexes and pending
references are rebuilt from it. Missing/duplicate/foreign sources, forged commits
without genuine offers, commits beyond a still-exact live cursor, inconsistent
receipts and over-cap custody reject before adoption.
An active exact cursor cannot hide explicitly uncommitted live mail. Legitimate
unoffered expired prefixes retain a bounded ACK-endpoint expiry observation,
even after that endpoint leaves replay. This is neither a recipient offer nor
a commit or terminal outcome; invalid TTL/exclusion evidence rejects.
Snapshot round trips retain
fast-recipient commits even after that recipient retires, leaving slow recipients
independent. Schema5/6 can reconstruct only provable retained obligations, not
unknown evicted history. Unknown commits remain conservatively pending only
where the cursor permits selection. An advanced exact schema5/6 cursor without
the retained recipient's own offer evidence explicitly refuses restoration,
rather than inventing commits, rewinding or adopting inaccessible pending mail.
Over-cap legacy state also refuses rather than dropping source. Snapshot support
is not an acceptance-atomic disk write or transparent application/workflow restart.

Schema8 adds exact sender registration intervals (or explicit legacy unknown)
and fixed terminal slots in binding order, including released tombstones in
optional replay. Missing/wrong-sized slots, overlapping ACKs, pre-acceptance
observations, invalid TTL/retirement reasons and invalid sender intervals reject
before adoption. New fields under older schemas reject as well. For schemas5–7,
only a still-current sender interval covering the occurrence is provable;
retired/replaced legacy owners cannot gain handoff authority. Restored terminal
mail cannot become live again on clock rollback. The ten-minute horizon and
explicit product-integration boundary above apply equally after round trip.

## Payloads and MAAP bridge

Idle model-mail receive admission waits for checked bookkeeping of the exact
currently bound conversation, including terminal compaction append receipts.
Accepted envelopes remain behind their durable cursor until history is safe;
deferral does not create a turn or spend the peer-turn loop budget. Existing
checked-settlement and append-completion wakes resume delivery without resending
the envelope. Blocked required history stays observable and fail-closed without
failing healthy timer services or other recipients. Original TTL filtering,
bridge nonwake rules, human inhibition and exact-owned child-failure authority
remain unchanged; neither mail nor settlement authorizes effect replay.
Pending work retains the existing one-second maintenance timer and fair fanout
budgets. A deferred sweep checks the exact history fence without retrying the
archive read or constructing a receive context.

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
