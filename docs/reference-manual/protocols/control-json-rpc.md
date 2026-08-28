# Mezzanine control JSON-RPC reference

## Purpose

Describe the implemented `mezctl/2` independent-primary endpoint, the
`mezctl/3` persistent-host front door, and the unsupported `mezctl/1`
predecessor. This page summarizes the implementer contract; the [control
endpoint in `SPEC.md`](../../../SPEC.md#13-control-endpoint) is normative.

## Version 2 cutover contract

`mezctl/2` allocates a fresh non-resumable client ID for every attachment and
supports at most 16 equal-authority attached primaries. Each primary owns
independent navigation and transient presentation. One layout owner controls
canonical PTY geometry. V2 initialization returns the exact client and
advertises `multiple_primaries`, `client_local_focus`, `layout_owner`, and
`client_bound_events`; v2 session state reports primary IDs/count/capacity,
owner, canonical size, and caller-relative navigation rather than singular
primary/focus fields.

V2 removes `session/attach` and `client/select_primary`. `client/detach`
defaults to the exact caller, and `client/set_layout_owner` atomically targets
another attached interactive primary. A v2 server rejects requested version 1
with `unsupported_version`.

V2 event delivery binds every stream to an exact initialized client. Shared
events target all primaries, private presentation events target one client,
and observers receive only their source primary's live session view at or after
the attachment cutoff. Snapshot payload v5 persists shared topology, canonical size,
and landing navigation, but never live clients, owner, event credentials, or
transient presentation.

## Transport, framing, and initialization

The default transport is a user-private Unix-domain socket. TCP is optional,
loopback-only by default, and remote TCP is disabled unless explicitly
configured. Unix clients should use peer credentials and the private socket
path. TCP clients must authenticate with an unguessable bearer token or a
stronger configured mechanism before receiving session data or mutating state.

The opt-in Iroh adapter uses ALPN `mezzanine/transport/1` and carries bounded
control frames on exactly one long-lived, client-opened bidirectional control
stream. The server accepts no client-opened unidirectional streams and lowers
each connection to one concurrent bidirectional control stream. Primaries
attempt event-stream versions `3 → 2 → 1`; observers attempt `3 → 1`.
Downgrade occurs only for the structured unsupported-event-version result or
the exact legacy equivalent, never for authentication, authorization,
malformed initialization, transport, or post-initialization failures. After
the initialize response is flushed, the server may open one unidirectional
stream with preface `mezzanine/events/1\n`, `mezzanine/events/2\n`, or
`mezzanine/events/3\n`, matching the negotiated version. Version 3 is the
boundary for pushed rendered-state updates. A primary or observer v3 stream
sends an authoritative `render/snapshot` immediately after its preface and
sends another complete snapshot for later actionable presentation changes.
Each snapshot contains a stream-local revision, `event_cutoff`,
`invalidate_output`, and a complete exact-client `RenderedClientView`. The
client validates the entire frame, including its negotiated role, before
replacing its retained logical view and then reuses the normal local ANSI
differential renderer. Later non-invalidating updates may use
`render/delta` with `base_revision`, a greater `revision`, complete non-row view
metadata, and unique whole-row text/style replacements. The framed
`terminal/step` path publishes an exact-client render invalidation for
presentation changes not represented by an inline view. In particular,
pane-local agent-prompt edits have no PTY echo and therefore wake the pushed
render stream directly instead of waiting for unrelated output or status
activity.

The server retains the last successfully flushed complete view, suppresses
identical views, and sends
a snapshot when the base is unsafe, geometry or row count changes, output must
be invalidated, or the uncompressed delta is not smaller. The client validates
and reconstructs the complete candidate atomically; a stale or malformed delta
fails the stream without partially changing retained state, and reattachment
begins with a fresh snapshot.

Observer push ownership is two-sided for compatibility. The observer client
opts in with `client.metadata.pushed_render_updates: true`, and the server
confirms support with `capabilities.features.pushed_render_updates: true`.
When either signal is absent, observer v3 remains notification-plus-fetch.
Primary v3 push ownership remains version-defined.

The first render update is sent immediately. Only one encoded render update is
written at a time; while that write is backpressured, the runtime retains
bounded redraw triggers rather than rendered frames. After the write completes,
it drains all currently ready event slices and exact-client render
invalidations, renders latest state once, and computes from the last
successfully flushed base. Unsafe or safety-bound trigger ranges force an
invalidating snapshot, while failed writes do not advance revision/base state.
This is latest-state backpressure coalescing, not timer-based batching.
The client continues consuming and presenting authoritative v3 updates while a
primary `terminal/step` acknowledgement is outstanding, so the independent
render stream is not held behind the control RTT. The control response remains
the ordered mutation acknowledgement and is still awaited exactly once.
Presentation advances in bounded output passes: an incomplete or backpressured
terminal frame cannot prevent acknowledgement polling or capture of follow-on
stdin. Captured input remains buffered until the preceding acknowledgement is
decoded, preserving stop-and-wait mutation ordering without leaving keystrokes
stuck behind physical-terminal output.
The event decoder continues applying revisioned snapshots and deltas in order
while presentation is busy, but its handoff is latest-state rather than an
eight-frame FIFO. It keeps one consumer-visible wakeup and one decoder-local
coalesced wakeup, carries any skipped output invalidation onto the newest
complete frame, and lets the terminal adapter finish an already-started ANSI
frame before presenting only that newest deferred view. Sustained pane-buffer
or pager scrolling therefore does not replay every reconstructed viewport.
Connection-local status exposes content-free coalesced-trigger, suppressed
update, snapshot-fallback, maximum-ready-depth, and render-write-wait metrics.

Each observer v3 stream renders with terminal dimensions retained for that
exact authenticated observer. `terminal/resize` updates only the caller's
observer geometry and triggers an exact-client pushed render; it cannot mutate
primary geometry, another observer, or canonical pane layout. Version 2 is
primary-only; primary versions 2 and 3 support negotiated client-local
clipboard writes, while observer v3 does not. Setup, idle operation, writes,
and teardown are bounded; wrong ALPNs, excess streams, malformed frames,
stalled setup, and one failed connection are isolated from later clients and
from the Unix listener.

Schema v71 defines two compressed application-framing ALPNs:
`mezzanine/transport/2/zstd` and
`mezzanine/transport/2/lz4`. This is not Iroh or QUIC compression. On either
ALPN, each complete existing control or event frame is independently wrapped
in a fixed 16-byte `MZC2` envelope containing flags, an encoded length, and the
exact decoded length. Unknown flags, non-zero reserved bytes, zero or excessive
lengths, truncation, trailing bytes, decode failures, and decoded-length
mismatches are protocol errors local to the offending connection. A frame
below `compression_min_bytes`, or one whose compressed representation would
expand, uses an identity v2 envelope without changing the negotiated codec.

`control/initialize` requests and responses, including invitation-issued
`device_credential` values, must use identity envelopes. Compression becomes
eligible only after successful initialization has been flushed. Clients choose
configured codecs in order and may try another ALPN only after connection or
ALPN failure before opening a stream or writing initialization data. They must
never downgrade or replay after application bytes have been sent. The selected
codec is immutable for the connection and applies to later control frames in
both directions and to event frames after the unchanged event-stream preface.

Each connection maintains non-sensitive application-frame counters for wire
bytes, decoded bytes, compressed frames, and identity frames. The status sampler
publishes only interval deltas for the exact initialized client and resets its
baseline with the connection-local codec context. A zero-frame interval is
insufficient data. Decode, limit, unsupported-codec, and malformed-envelope
failures remain connection-local and diagnostics must identify only the failure
class, never credentials, topology, payload bytes, or payload-derived samples.

An Iroh endpoint ID proves possession of a transport key only; it grants no
Mezzanine authority by itself. Before any other method, the peer must call
`control/initialize` for role `primary` or `observer` with either a single-endpoint-use
`extension:iroh_invitation` token or an endpoint-bound
`extension:iroh_device` credential. Agent and automation initialization are
rejected on this remote pairing path. Invitation initialization returns an
endpoint-bound `device_credential`; the client persists it only after
successful initialization and uses `extension:iroh_device` on reconnect. If
the initialize response or local profile save is lost, the same authenticated
endpoint may retry that invitation until expiry and receives the same
credential without creating another trust record. Another endpoint cannot
resume the redemption.

For graceful one-shot control, the client finishes its send half after the
final request, reads exactly the final framed response, drains response EOF,
and waits boundedly for acknowledgement before closing. The server finishes its
response half and likewise waits boundedly before connection teardown. Abrupt
EOF, reset, decode, dispatch, write, and flush failures run the same idempotent
connection-disconnect cleanup, so a detach-on-disconnect primary is removed at
most once. One-shot administrative clients request that cleanup, but the server
arms it only when the connection creates the primary. Reusing a same-named
interactive primary does not transfer its ownership to the one-shot request.
Neither side silently replays an application request after an ambiguous
failure.

Interactive Iroh attach retains that initialized stream instead of opening a
stream per request. The client serializes each resize, `terminal/step`, and
`terminal/view` operation behind exactly one response before sending the next
operation. Its negotiated event stream carries only authorized `event/*`
notifications and wakes the client to request a fresh rendered view; it never
carries terminal input or control responses. Terminal input is non-replayable:
after a write, read, timeout, reset, or connection failure that leaves its
outcome ambiguous, the client must fail visibly, close the channel, and require
reattach without retrying buffered input.

Each stream frame is UTF-8 JSON preceded by this ASCII header block. The
decimal `Content-Length` is the JSON body's octet length.

```text
Content-Length: <decimal-octet-length>\r\n
Content-Type: application/vnd.mezzanine.control+json; version=1\r\n
\r\n
```

Unknown headers are ignored. Missing, invalid, negative, or oversized lengths
are rejected. The body is a JSON-RPC 2.0 request, response, or notification:

- Requests contain `jsonrpc: "2.0"`, a non-null string or integer `id`, a
  `method`, and optional object `params`.
- Notifications contain `jsonrpc: "2.0"` and `method`, but no `id`.
- Responses repeat the request ID and contain exactly one of `result` or
  `error`.

Unless an outer transport has already authenticated and negotiated a version,
the first request is `control/initialize`.

```json
{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"client_name":"example-ui","client_version":"1.0.0","requested_version":2,"requested_role":"primary","client":{"name":"example-ui","terminal":{"columns":120,"rows":40,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}
```

The implemented direct-session endpoint accepts `mezctl/2`. The persistent
host front door accepts `mezctl/3` and adds an explicit `session_intent` before
any connection is bound to a session. Its `host_only` authentication path is
implemented; session-routing intents are completed by the host router:

| Intent | Target and idempotency contract |
| --- | --- |
| `create` | Omit `session_target`; require a non-empty client-generated `idempotency_key`. |
| `attach` | Require exactly one `session_target`; omit `idempotency_key`. |
| `default` | Omit both fields and select an existing attachable default; never create. |
| `host_only` | Omit both fields and expose only authorized host methods; never resolve or create a session. |

Every v3 initialize request includes one intent. V2 requests omit the v3
fields, and a direct session endpoint rejects v3. The host authenticates and
authorizes the paired device before target lookup, lease reservation, runtime
allocation, or session disclosure. `host_only` returns null `session` and
`lease` and permits no session method. After successful session routing, the
connection is permanently bound to one session actor and later targets must
continue to match it.

Create idempotency is scoped to the authenticated host principal and normalized
creation inputs. Replaying the same key returns the committed lease/session;
reusing it with different inputs is a conflict. Pairing, invitation redemption,
profile checks, and host administration use `host_only`, so those operations
cannot accidentally provision a session.

The implemented host RPC catalog is `host/get`, `host/shutdown`,
`host/reconcile`, `host/session/list`, `host/session/create`, and
`host/session/resolve`. The lease catalog is `lease/list`, `lease/get`,
`lease/checkpoint`, `lease/recover`, `lease/release`, `lease/revoke`, and
`lease/gc`. Local Unix administration is authoritative by default; remote
attach/create authority never implies lease administration. Lease targets may
be exact lease IDs, session IDs, or unambiguous names. Active release/revoke
requests require `terminate=true`; GC is a preview unless `apply=true` and can
remove only released, revoked, or failed tombstones. Results omit create
idempotency keys and creation fingerprints. Configured audit logging records
the local host administrator, method, outcome, lease identity, and generation
without request reasons, credentials, or other secret-bearing fields.

The result contains `selected_version`, a secret-free `server` identity, the
granted role, negotiated `capabilities`, the attached `client`, and `session`
state. Observer initialization attaches a read-only client immediately.
Capabilities list available methods, event types, roles, transports, limits,
and feature flags. Treat this advertised set—not this page—as the available
surface for the connection.

A successful invitation redemption adds `device_credential` to the initialize
result. The invitation is not consumed until ordinary initialization can
succeed, so malformed client data, role conflicts, or version errors leave it
reusable. After redemption, only the same authenticated endpoint may repeat
that initialize until invitation expiry; the server returns the same credential
and reuses the same trust record so response loss or client profile-save failure
is recoverable. This idempotency applies only to pairing initialization, not to
subsequent application requests. Reconnect with the returned credential and
the same authenticated endpoint ID. Credentials are matched to their exact
trust record and bound to the current server endpoint identity and role ceiling;
wrong endpoints, bad proofs, revoked historical credentials, server identity
replacement, and role escalation fail closed. Redeeming a valid new invitation
for an endpoint with active trust atomically revokes the previous record as
superseded and activates the replacement; initialization rollback restores the
previous record. Re-pairing a revoked endpoint likewise creates a new active
record without allowing old history to shadow it. Never log or copy invitation
or device credentials into diagnostics.

## Roles, authorization, and idempotency

Requested roles are `primary`, `observer`, `agent`, and `automation`. Under
v1, one interactive primary owns terminal input. Under v2, every attached
primary may submit actor-ordered input against its own navigation, while only
the layout owner's resize changes canonical geometry. A primary request always
requires a verifiable interactive terminal; a client descriptor alone is not
sufficient when the transport does not trust that assertion.

Observer initialization immediately creates an attached read-only `observer`
bound to the current layout-owner primary. It fails with `conflict` and leaves
no client residue when no layout owner is attached. Observers receive only
rendered views and permitted events at or after their attachment cutoff, may
detach themselves through `client/detach`, and cannot mutate the session.
Primary-only operations fail with `not_primary` or `forbidden` for other callers.
Agent calls remain subject to the active permission policy.

Every non-idempotent mutation requires an `idempotency_key` in its params.
Results are replayed for a repeated key with the same caller, method, and
parameters; reuse with changed method or parameters is a `conflict`. Read-only
and explicitly naturally-idempotent methods may omit the key.

## Errors and common objects

Use standard JSON-RPC parse, invalid-request, method-not-found,
invalid-params, and internal-error codes for JSON-RPC failures. Application
errors use `-32000` through `-32012` with `error.data.mezzanine_code`:

| Code | Stable name |
| ---: | --- |
| -32000 | `internal_error` |
| -32001 | `unauthorized` |
| -32002 | `forbidden` |
| -32003 | `unsupported_version` |
| -32004 | `invalid_state` |
| -32005 | `not_found` |
| -32006 | `conflict` |
| -32007 | `not_primary` |
| -32008 | `policy_denied` |
| -32009 | `approval_required` |
| -32010 | `timeout` |
| -32011 | `rate_limited` |
| -32012 | `cancelled` |

All params and results are objects. IDs are opaque. Time values are RFC 3339
with an offset. Undefined fields belong under `extensions`. Target objects use
one unambiguous identity form: exact IDs take precedence, indexes are
non-negative, ambiguity is `conflict`, and a missing object is `not_found`.
`SessionTarget` selects exactly one `session_id`, `name`, or `default: true`;
`WindowTarget`, `PaneTarget`, and `AgentTarget` refine those identities as
defined in the [normative target contract](../../../SPEC.md#13-control-endpoint).

State results use versioned objects such as `SessionState`, `WindowState`,
`PaneState`, `LayoutState`, `AgentState`, `ApprovalState`, `SnapshotState`, and
MCP server/tool state. State records include opaque `id` and `version`; clients
must preserve unknown extensions and refetch rather than reconstructing state.

## Current version 2 method catalog

This table summarizes the direct-session `mezctl/2` surface. “RO” means
read-only and naturally idempotent. Every other entry requires
`idempotency_key` unless its note says otherwise. The parameter and result
object schemas are specified in the [baseline method table in
`SPEC.md`](../../../SPEC.md#13-control-endpoint). The unsupported v1
predecessor additionally exposed `session/attach` and `client/select_primary`;
v2 removes those methods and adds `client/set_layout_owner`.

| Namespace | Methods | Access and purpose |
| --- | --- | --- |
| Control | `control/initialize`, `control/shutdown`, `control/cancel` | Negotiate a connection, close it, or cancel an owned request. Shutdown is naturally idempotent. |
| Session | `session/list`, `session/get`, `session/rename`, `session/kill` | Inspect, rename, or terminate sessions. List/get are RO. |
| Client | `client/list`, `client/detach`, `client/set_layout_owner` | Inspect clients, detach a client, or atomically select an attached interactive primary as layout owner. |
| Window | `window/list`, `window/create`, `window/rename`, `window/select`, `window/close`, `window/layout`, `window/rebalance` | Inspect, create, name, select, close, or arrange windows. List is RO; rename is naturally idempotent when unchanged. Layout and rebalance are primary-only presentation mutations. |
| Pane | `pane/list`, `pane/create`, `pane/select`, `pane/resize`, `pane/move`, `pane/swap`, `pane/break`, `pane/join`, `pane/close`, `pane/rename`, `pane/zoom`, `pane/input-sync`, `pane/attention`, `pane/status`, `pane/notice`, `pane/capture` | Inspect panes, mutate layout and presentation, control synchronized input, completion attention, source-owned status, or bounded notices, or capture pane content. List is RO; capture is RO when policy permits. Status and notices are available to primary and automation clients; rename, zoom, and input synchronization are primary-only. |
| Buffer | `buffer/list`, `buffer/create`, `buffer/read`, `buffer/delete` | Primary-only bounded internal paste-buffer inspection and mutation. List/read are RO; create requires explicit replacement for existing names. |
| Frame | `frame/read` | Read rendered frame fields and text (RO). |
| Terminal | `terminal/view`, `terminal/step`, `terminal/command` | Render a client view, submit bytes/resize, or invoke a terminal command. Primary-only mutation applies to step and command. |
| Agent | `agent/list`, `agent/task/list`, `agent/spawn`, `agent/shell/show`, `agent/shell/hide`, `agent/shell/command` | Inspect agents/tasks (RO), manage an agent shell, start prompt work, or spawn an agent. |
| Approval | `approval/list`, `approval/decide` | Inspect pending approvals (RO) or make a primary decision. |
| Configuration | `config/get`, `config/set`, `config/unset`, `config/reload`, `config/validate` | Inspect or validate config (RO), or mutate/reload it. |
| Project trust | `project/trust/list`, `project/trust/inspect`, `project/trust/decide`, `project/trust/revoke` | Inspect or decide project trust. |
| Remote trust | `remote/status`, `remote/invite`, `remote/client/list`, `remote/client/rename`, `remote/client/revoke` | Inspect or mutate paired-device trust. These methods require an initialized primary over authenticated local Unix control; even a paired Iroh primary is rejected. Invite, rename, and revoke require idempotency keys. |
| Snapshots | `snapshot/list`, `snapshot/create`, `snapshot/resume`, `snapshot/delete` | Inspect snapshots (RO) or persist, load, or delete layouts. |
| MCP | `mcp/list`, `mcp/retry` | Inspect configured server/tool availability (RO) or retry a server. |
| Events | `event/list` | Replay retained, authorized events after `after_event_id`; RO. |

## Terminal frontend contract

An alternative interactive frontend is a primary client. Obtain the initial
render with `terminal/view`:

```json
{"jsonrpc":"2.0","id":2,"method":"terminal/view","params":{"client_size":{"columns":120,"rows":40}}}
```

The result is `{ "view": RenderedClientView | null, "event_cutoff": integer }`.
`event_cutoff` is the latest ordered server event whose applied state is
represented when the authoritative view is rendered. A view includes its role;
authoritative and client size; viewport and scroll bounds; cursor state;
input/output modes; an optional agent-prompt region; textual `lines`; and
`line_style_spans`. A frontend renders this projection, respecting cursor,
styles, scroll responsibility, bracketed paste, mouse reporting, and any
animation refresh interval.

Send user input through `terminal/step`, with bytes as integers in `0..255`.
Include `client_size` whenever geometry changes and set `render` false only
when the caller deliberately wants no unconditional immediate view. A caller
that can consume conditional inline views may retain `render: false` and add
`extensions: {"render_mode":"if_changed"}`. The runtime then renders only
when the applied step requires a presentation refresh. This extension placement
lets older strict servers ignore the hint safely; their null view causes the
client to perform one ordinary `terminal/view` fallback. Unsupported mode
values are rejected by servers that implement the extension.

The result reports input count, forwarded bytes,
multiplexer/agent/mouse actions, redraw requirements, unsupported actions,
optional `view`, optional `event_cutoff`, UI theme, acknowledged client detach,
and session termination. When an inline view is present, `event_cutoff` is from
the same authoritative render boundary and can cover queued ordinary redraw
wakeups. A true `client_detached` ends that attach loop cleanly without implying
that the durable session was terminated.

```json
{"jsonrpc":"2.0","id":3,"method":"terminal/step","params":{"idempotency_key":"ui-step-0001","client_size":{"columns":120,"rows":40},"render":true,"input_bytes":[108,115,13]}}
```

Iroh primary input uses the compatible conditional form to avoid a second
request/response RTT when input changes the rendered presentation:

```json
{"jsonrpc":"2.0","id":3,"method":"terminal/step","params":{"idempotency_key":"ui-step-0001","client_size":{"columns":120,"rows":40},"render":false,"extensions":{"render_mode":"if_changed"},"input_bytes":[108,115,13]}}
```

Use `terminal/command` for explicit terminal command text, not for arbitrary
JSON-RPC method aliases:

```json
{"jsonrpc":"2.0","id":4,"method":"terminal/command","params":{"idempotency_key":"ui-command-0001","input":"list-windows"}}
```

Automation and primary clients can set or clear the existing flashing
completion-attention pill for a pane with `pane/attention`. This is useful for
agent-harness hooks that need to signal completion without moving focus. Omit
`target` to use the active pane, or provide any standard `PaneTarget`:

```json
{"jsonrpc":"2.0","id":5,"method":"pane/attention","params":{"target":{"pane_id":"%2"},"attention":true,"idempotency_key":"hook-attention-0001"}}
```

Primary clients can also use explicit presentation controls instead of
synthesizing command-prompt input. `pane/rename` pins a pane title,
`pane/zoom` accepts the desired boolean state, and `pane/input-sync` enables or
disables synchronized input for a target window. `window/layout` selects one
of `tiled`, `even-vertical`, `even-horizontal`, or `even-grid`, while
`window/rebalance` reapplies the selected policy. Targets default to the active
pane or window, and targeted operations do not change focus.

Harness hooks can publish richer state with `pane/status`. Each entry is owned
by the calling client plus its bounded `source`, so clearing one source does not
remove another source status. Supported states are `running`, `waiting`,
`blocked`, `failed`, and `complete`; a null state clears that owner. Optional
text is bounded and appears through the `pane.status` frame field. `pane/notice`
appends a structured, bounded `message` event with `info`, `warning`, `error`,
or `success` severity without writing into the PTY or stealing focus.

Primary clients can also stage bounded internal handoffs with `buffer/create`,
`buffer/list`, `buffer/read`, and `buffer/delete`. Existing names are preserved
unless create explicitly requests replacement; buffer APIs do not access the
host clipboard.

The recommended legacy loop is initialize, fetch a view, render it, pass
physical input and size updates via `terminal/step`, then apply the returned
view or request a fresh `terminal/view`. Local clients use the Unix event
socket. Iroh primary input requests a conditional inline view and falls back to
one `terminal/view` when an older server returns no view. Iroh primaries
negotiate `3 → 2 → 1`; observers negotiate `3 → 1`, using only explicit
unsupported-version initialization results to continue to the next candidate.
A primary or observer v3 client renders the initial and subsequent pushed
snapshots or deltas without issuing steady-state `terminal/view` requests.
Primary control responses acknowledge input and resize mutations; observer v3
uses `terminal/resize` to acknowledge only its client-local geometry change.
Legacy event streams retain notification-plus-fetch behavior. This is
rendered-view/input-step control, not raw PTY export; specialized frontends
should design around the supplied view model.

## Events and replay

Server notifications use `event/*` methods. Params contain ordered `event_id`,
`time`, `event_type`, `object`, and `session_id` when the recipient is allowed
to know it; state changes should include `previous` when available. Baseline
events cover client and observer changes, window/pane changes, agent task
changes, approvals, config, snapshots, and MCP availability.

Per-connection event order is preserved. The Iroh writer requests at most 64
visible events per actor batch, advances its cursor only after a bounded stream
flush, and uses QUIC flow control plus a bounded client wakeup channel. A slow
receiver therefore backpressures or times out its own event task without
blocking the serialized runtime actor or another connection. Reconnect with
`event/list` and a known `after_event_id`; replay can be refused once retention
has elapsed, so attach clients refetch the current rendered view after any gap.
The capabilities limits expose retention. Interactive Iroh clients collapse
already-ready redraw wakeups into one authoritative view fetch, discard
ordinary numbered redraw wakeups at or below the returned `event_cutoff`, and
schedule animation-only view refreshes from the interval advertised by the last
rendered view. Newer events, unnumbered wakeups, invalidating actions,
disconnects, and errors remain actionable. This removes redundant round trips
after an in-flight view without delaying the first redraw, so compression does
not turn a burst into a queue of stale renders or suppress local animation
cadence.

Every Iroh event batch re-resolves the live session client before projection.
Attached observers see only `SessionView` events at or after their atomic
attachment cutoff; primary-only, agent, automation, pre-attachment, and
cross-session payloads are omitted. Source detach, self-detach, control
completion, reset, or connection shutdown closes the
event stream. Transport endpoint authentication alone never authorizes events.

## Related pages

- [Protocol conventions](common-conventions.md)
- [`maap/1` action protocol](maap.md)
- [`mmp/1` local messages](mmp.md)
- [Normative control contract](../../../SPEC.md#13-control-endpoint)
