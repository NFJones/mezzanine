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

Control request decoding rejects duplicate object keys at every nested level,
including escaped aliases, before method/producer selectors are interpreted or
parameters canonicalized. Valid unique JSON retains ordinary value semantics;
errors omit raw keys/values and parser excerpts. Ordinary enrollment also checks
its original metadata directly before reserving native work. Pi observation keeps
valid original parameter bytes for typed bounds; ambiguous nested fields reject
at the common boundary rather than being overwritten.

### Base transports and Iroh event streams

The implemented transports are a user-private Unix-domain socket and the
opt-in Iroh adapter. Unix clients use peer credentials and the private socket
path. TCP is only an optional transport contract in `SPEC.md`, not an
implemented listener or CLI switch: current control capabilities advertise
`tcp: false`. The spec's loopback-default and bearer-token requirements for a
future TCP adapter do not enable TCP in this implementation.

The opt-in Iroh adapter uses ALPN `mezzanine/transport/1` and carries bounded
control frames on exactly one long-lived, client-opened bidirectional control
stream. The server accepts no client-opened unidirectional streams and lowers
each connection to one concurrent bidirectional control stream. Primaries
attempt event-stream versions `5 → 4 → 3 → 2 → 1`; observers attempt
`5 → 4 → 3 → 1`. Unix and non-negotiating Iroh clients use version 1.
Downgrade occurs only for the structured unsupported-event-version result or
the exact legacy equivalent, never for authentication, authorization,
malformed initialization, transport, or post-initialization failures.
Plaintext event frames distinguish incomplete input from permanent framing
errors: malformed complete frames terminate reception immediately, without
waiting for EOF or idle timeout or skipping bytes to reach a later event.
After the initialize response is flushed, the server may open one unidirectional
stream with the exact preface `mezzanine/events/1\n`, `mezzanine/events/2\n`,
`mezzanine/events/3\n`, `mezzanine/events/4\n`, or `mezzanine/events/5\n`,
matching the negotiated version. Event-stream versions are independent of the
`mezctl/2` or `mezctl/3` control version and compression ALPN. Version 3 is the
boundary for pushed rendered-state updates. A primary or observer v3–v5 stream
sends an authoritative `render/snapshot` immediately after its preface and
sends revisioned render updates for later actionable presentation changes.
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

Version 4 retains the v3 pushed-render contract and adds bounded fragmentation.
A complete framed `render/snapshot` or `render/delta` larger than 512 KiB may
be replaced by ordered `render/chunk` notifications. Each chunk carries the
target revision, zero-based contiguous index, total chunk count, total
encoded-frame bytes, and base64 data. A transfer is limited to 8 MiB and 16
chunks. Reject unnegotiated, interrupted, out-of-order, oversized, or incomplete
transfers; validate the reassembled original frame atomically before changing
retained state. A frame exceeding 8 MiB produces a visible bounded-transfer
error, not a bypass of the limit.

Version 5 retains v4 snapshots, whole-row deltas, and fragment bounds, and may
send `render/sparse` when its complete decoded framed candidate is strictly
smaller than both a whole-row delta and a snapshot. It carries `kind: "sparse"`,
exact `base_revision`, greater `revision`, `event_cutoff`,
`invalidate_output: false`, unchanged `line_count`, a `view` map of changed
non-row metadata, a `remove` array of deleted metadata keys, and unique
in-range `rows`. Each row has an index and at least one of `line` or
`style_spans`; omitted row fields retain their base values. Omitted metadata
retains its base value, explicit null replaces it, and `remove` deletes it.
Role and row arrays cannot be changed through metadata or removal. Reject
stale bases, invalid keys, duplicate/out-of-range rows, malformed text/styles,
or invalid reconstructed views without changing the retained revision or view.
Sparse frames are never sent on v3/v4 streams; fragmented sparse frames use
the same v4 atomic transfer bounds. Selection compares decoded framed bytes,
not compressed wire savings, and speculative candidates must not advance
stateful codec history.

Logical recomposition and physical terminal-output-cache validity are separate.
An ordinary pane, window, configuration, overlay, layout, or presentation
change—and a snapshot selected only because it is more efficient than a
delta—preserves the attached client’s retained output frame. The client uses
that frame as the normal ANSI differential-rendering base. `invalidate_output`
is reserved for physical uncertainty such as first presentation, the exact
client’s resize, decoder recovery, or uncertain partially committed output.

Observer push ownership is two-sided for compatibility. The observer client
opts in with `client.metadata.pushed_render_updates: true`, and the server
confirms support with `capabilities.features.pushed_render_updates: true`.
When either signal is absent, observer v3–v5 remains notification-plus-fetch.
Primary v3–v5 push ownership remains version-defined.

The first render update is sent immediately. Only one encoded render update is
written at a time; while that write is backpressured, the runtime retains
bounded redraw triggers rather than rendered frames. After the write completes,
it drains all currently ready event slices and exact-client render
invalidations, renders latest state once, and computes from the last
successfully flushed base. Unsafe or safety-bound trigger ranges force an
invalidating snapshot, while failed writes do not advance revision/base state.
This is latest-state backpressure coalescing, not timer-based batching.
The client continues consuming and presenting authoritative v3–v5 updates while a
primary `terminal/step` acknowledgement is outstanding, so the independent
render stream is not held behind the control RTT. The control response remains
the ordered mutation acknowledgement and is still awaited exactly once.
Presentation advances in bounded output passes: an incomplete or backpressured
terminal frame cannot prevent acknowledgement polling or capture of follow-on
stdin. Captured input remains buffered until the preceding acknowledgement is
decoded, preserving stop-and-wait mutation ordering without leaving keystrokes
stuck behind physical-terminal output.
The event decoder continues applying revisioned render updates in order
while presentation is busy, but its handoff is latest-state rather than an
eight-frame FIFO. It keeps one consumer-visible wakeup and one decoder-local
coalesced wakeup, carries any skipped output invalidation onto the newest
complete frame, and lets the terminal adapter finish an already-started ANSI
frame before presenting only that newest deferred view. Sustained pane-buffer
or pager scrolling therefore does not replay every reconstructed viewport.
Connection-local status exposes content-free coalesced-trigger, suppressed
update, snapshot-fallback, maximum-ready-depth, and render-write-wait metrics.

Each observer v3–v5 stream renders with terminal dimensions retained for that
exact authenticated observer. `terminal/resize` updates only the caller's
observer geometry and triggers an exact-client pushed render; it cannot mutate
primary geometry, another observer, or canonical pane layout. Version 2 is
primary-only; primary versions 2–5 support negotiated client-local
clipboard writes, while observer streams do not. Setup, idle operation, writes,
and teardown are bounded; wrong ALPNs, excess streams, malformed frames,
stalled setup, and one failed connection are isolated from later clients and
from the Unix listener.

X11 forwarding is negotiated only by an authenticated Iroh primary through an
optional version-2 `x11_forwarding` initialize offer. The offer contains the
requested `untrusted` or explicitly policy-gated `trusted` mode, exact
`MIT-MAGIC-COOKIE-1` protocol name, a random 16-byte fake cookie encoded as
base64, and an explicit takeover flag. Success advertises
`capabilities.features.x11_forwarding` and returns the exact mode, a nonzero
session-local generation, and a random route token. It never returns the real
local cookie, local display target, or local authority path. Unsupported peers,
policy denial, ownership conflict, malformed metadata, or trust-mode changes
fail initialization visibly without retrying without X11 or falling back to
trusted mode.

Each accepted server proxy socket maps to one server-opened bidirectional Iroh
stream on the exact owning control connection. Its raw fixed 56-byte preface
contains `MZX11STR`, version 2, seven zero reserved bytes, the generation as a
big-endian `u64`, and the 32-byte route token. The preface is excluded from
compression history and accounting. The server validates the fake cookie; the
client validates the preface, decodes one identity/reset setup record, rewrites
only that cookie to its client-local real value, and connects to the selected
pre-resolved X endpoint.

After the preface, `none` preserves raw X11 bytes, version-2 codecs use `MZC2`
records, and version-3 codecs use compact streaming records. The setup request
is one identity record that cannot seed reusable compression history. Later
reads become immediately flushed records of at most 64 KiB. Each direction and
X11 stream has independent codec state. X11 records use neither JSON,
Content-Length, nor event framing, and malformed records fail only their X11
stream. Successful X11 records are included in the owning connection's
aggregate compression metrics.

The server's one client-opened bidirectional control-stream limit remains
unchanged. Only a successfully negotiated client grants bounded credit for
host-opened X11 streams. One generation-fenced route owner is bound to the
session, attached client, authenticated endpoint and principal, and exact Iroh
connection. Explicit takeover invalidates the old authority and cancels its
streams before publishing a new generation. Detach, transport loss, trust or
lease revocation, and session shutdown perform the same exact-generation
cleanup. Reattach preserves the remote `DISPLAY` and `XAUTHORITY` paths but
rotates credentials and does not migrate existing X connections.

A terminal request that reports `session_terminated = true` is flushed before
the connection-owned X11 route is revoked and before terminal lifecycle state
is published to service supervision. This response fence is drop-safe: write
failure, transport loss, or task cancellation still releases teardown rather
than leaving the session services alive indefinitely.

### Iroh compression

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

Schema v75 adds two opt-in stateful ALPNs:
`mezzanine/transport/3/zstd-stream` and
`mezzanine/transport/3/lz4-stream`. Each control direction owns a fresh codec
context after its raw initialization frame, and the event stream owns another
fresh context after its raw preface. Zstandard is limited to a 64 KiB window;
LZ4 retains at most 64 KiB of linked history. Every complete inner
Content-Length frame is carried in one compact record with encoded and decoded
lengths and is codec-flushed and Iroh-flushed immediately. Records containing
credentials, tokens, invitations, passwords, secrets, or clipboard effects
are sent as identity reset records, clearing history before and after the
payload. Contexts are never shared across direction, stream, connection,
client, or reconnect. `compression_min_bytes` does not bypass a stateful codec
because doing so would desynchronize encoder and decoder history.

Each connection maintains non-sensitive application-frame counters for wire
bytes, decoded bytes, compressed frames, and identity frames. The status sampler
publishes only interval deltas for the exact initialized client and resets its
baseline with the connection-local codec context. A zero-frame interval is
insufficient data. Decode, limit, unsupported-codec, and malformed-envelope
failures remain connection-local and diagnostics must identify only the failure
class, never credentials, topology, payload bytes, or payload-derived samples.

### Iroh authentication and connection lifecycle

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

Routed initialization retains exact attachment cleanup from actor response
creation through response delivery and control-loop handoff. Early errors or
cancelled futures transfer teardown to the actor's cancellation queue, not an
untracked background task. The exact X11 route is invalidated before bounded
cleanup acknowledgement. A failed acknowledgement does not prove detachment.
Committed session creation remains idempotently reusable after response loss;
attachment cleanup does not delete that session or replay input.

Interactive Iroh attach retains that initialized stream instead of opening a
stream per request. The client serializes each resize, `terminal/step`, and
`terminal/view` operation behind exactly one response before sending the next
operation. Orderly event-stream FIN ends the receiver even while control stays
live. Clean EOF yields one disconnect; incomplete frames or render-fragment
transfers yield a terminal error. Clipboard-transfer expiry is nonterminal
housekeeping, and dropping the presentation consumer releases the receiver.
Both per-session and host-routed servers supervise the event worker alongside
live control. Required event-stream loss ends only that attachment with exact
disconnect/X11 cleanup. Worker errors, peer stop, panic classification (without
panic payloads), and teardown deadlines remain visible; sibling connections and
the shared session stay live. Failed writes do not advance render bases or receipts.
Iroh view and presentation-acknowledgement RPCs use the effective request timeout
for the entire write, flush, and response wait, not one budget per phase. A
timeout leaves the outcome unknown and ends attachment through existing
terminal and transport cleanup; it does not replay the request or confirm a
receipt. An empty presentation-ID set sends no acknowledgement RPC. Unix primary
attachment retains its existing policy without a supplied RPC deadline.
Unix and legacy Iroh v1 and v2 event streams carry authorized
`event/*` notifications that wake the client to request a fresh rendered view.
The bound Unix stream can also carry a non-durable owner-scoped
`render/wakeup` notification. Its `invalidate_output` flag tells the client
whether to discard its physical-output diff base before that view pull; live
pane-divider drags set it to `false`. Version 3 additionally carries
authoritative `render/snapshot` and `render/delta` notifications, which the
client renders without steady-state view fetches; v4 adds `render/chunk` and
v5 adds `render/sparse`. No event-stream version
carries terminal input or control responses. Terminal input is non-replayable:
after a write, read, timeout, reset, or connection failure that leaves its
outcome ambiguous, the client must fail visibly, close the channel, and require
reattach without retrying buffered input.

### Control framing and initialization

Each stream frame is UTF-8 JSON preceded by this ASCII header block. The
decimal `Content-Length` is the JSON body's octet length.

```text
Content-Length: <decimal-octet-length>\r\n
Content-Type: application/vnd.mezzanine.control+json; version=1\r\n
\r\n
```

Unknown headers are ignored. Missing, invalid, negative, or oversized lengths
are rejected. Headers have a separate 8192-byte maximum including the final
`\r\n\r\n`. Unterminated or over-budget headers fail before Iroh bridge raw
forwarding, also during stateful compression initialization. Bodies and later
buffered frames do not count toward this header budget.
The body is a JSON-RPC 2.0 request, response, or notification:

- Requests contain `jsonrpc: "2.0"`, a non-null string or integer `id`, a
  `method`, and optional object `params`.
- Notifications contain `jsonrpc: "2.0"` and `method`, but no `id`.
- Responses repeat the request ID and contain exactly one of `result` or
  `error`.

Unless an outer transport has already authenticated and negotiated a version,
the first request is `control/initialize`.

Terminal descriptors and `client_size` geometry require positive axes no larger
than 4096 cells each and a product no larger than 262144 visible cells. Excessive
sizes are rejected before attachment, layout mutation, PTY resize scheduling, or
screen allocation. The same safety budget applies to local/direct screen APIs
and restored geometry; dimensions are not silently clamped.

```json
{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"client_name":"example-ui","client_version":"1.0.0","requested_version":2,"requested_role":"primary","event_stream_version":1,"client":{"name":"example-ui","requested_role":"primary","interactive":true,"terminal":{"columns":120,"rows":40,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}
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
| `resolve_or_create` | Omit `session_target`; require a non-empty client-generated `idempotency_key`; atomically select the principal's attachable default or create an authorized lease-backed session. |
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

The local host administration RPC catalog is `host/get`, `host/shutdown`,
`host/reconcile`, `host/session/list`, `host/session/create`, and
`host/session/resolve`. The remote `host_only` path advertises only methods
granted to the authenticated principal; its implemented remote operations are
`host/session/list` and `host/session/kill`. Remote kill requires separately
granted force-kill authority, `force=true`, an idempotency key, and an explicit
target. The lease catalog is `lease/list`, `lease/get`, `lease/release`,
`lease/revoke`, and `lease/gc`. Local Unix
administration is authoritative by default; remote attach/create authority
never implies lease administration. Lease targets may be exact lease IDs,
session IDs, or unambiguous names. Active release/revoke requests require
`terminate=true`; GC is a preview unless `apply=true` and can remove only
released, revoked, or failed tombstones. Results omit create idempotency keys
and creation fingerprints. Configured audit logging records the local host
administrator, method, outcome, lease identity, and generation without request
reasons, credentials, or other secret-bearing fields.

The result contains `selected_version`, a secret-free `server` identity, the
granted role, negotiated `capabilities`, the attached `client`, and `session`
state. Version 3 additionally returns a secret-free `host` summary and a
`lease` summary when session-bound; `host_only` returns null `lease`, `session`,
and `client` values. Observer initialization attaches a read-only client
immediately. Capabilities list available methods, event types, roles,
transports, limits, and feature flags. Treat this advertised set—not this
page—as the available surface for the connection.

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

Authoritative resizes validate every window before updating client descriptors
or canonical geometry. Rejected requests and identical retries leave layout,
ownership, revisions, and resize effects unchanged. Explicit ownership transfer
and restored-layout loading use the same failure-atomic boundary. On automatic
owner election, a replacement terminal too small for the split trees retains
the previous canonical geometry; the departed client still detaches and the new
owner can submit a valid resize later. Post-commit PTY I/O errors are separate
from domain rejection and do not promise cross-process rollback.

Observer initialization immediately creates an attached read-only `observer`
bound to the current layout-owner primary. It fails with `conflict` and leaves
no client residue when no layout owner is attached. Observers receive only
rendered views and permitted events at or after their attachment cutoff, may
detach themselves through `client/detach`, and cannot mutate the session.
Primary-only operations fail with `not_primary` or `forbidden` for other callers.
Agent calls remain subject to the active permission policy.

Every non-idempotent mutation requires an `idempotency_key` in its params.
Results are replayed for a repeated key with the same caller, method, and
parameters; reuse with changed method or parameters is a `conflict`. This is
method-specific, not a requirement to add a key to every non-read-only call.
Read-only methods and connection operations with schemas that omit the key
must not receive it. `control/initialize` uses the intent-specific rules above;
`control/shutdown` takes no params, and `control/cancel` accepts only
`request_id`. Current cancel dispatch validates a string `request_id` and
returns `{"cancel_requested":false}`; it does not cancel an in-flight request.

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
read-only and naturally idempotent. Mutation methods have method-specific
`idempotency_key` requirements; connection operations are not blanket keyed
mutations. Use only fields accepted by the method's parameter schema; the
capabilities advertise availability, not those schemas. The parameter and result
object schemas are specified in the [baseline method table in
`SPEC.md`](../../../SPEC.md#13-control-endpoint). The unsupported v1
predecessor additionally exposed `session/attach` and `client/select_primary`;
v2 removes those methods and adds `client/set_layout_owner`.

| Namespace | Methods | Access and purpose |
| --- | --- | --- |
| Control | `control/initialize`, `control/shutdown`, `control/cancel` | Negotiate a connection or close it. Cancel validates a string `request_id` and currently returns `cancel_requested: false`, without cancelling work. Shutdown takes no params; cancel accepts only `request_id`, not an idempotency key. Initialize follows intent-specific rules. |
| Session | `session/list`, `session/get`, `session/rename`, `session/kill` | Inspect, rename, or terminate sessions. List/get are RO. |
| Client | `client/list`, `client/detach`, `client/set_layout_owner` | Inspect clients, detach a client, or atomically select an attached interactive primary as layout owner. |
| Window | `window/list`, `window/create`, `window/rename`, `window/select`, `window/close`, `window/layout`, `window/rebalance` | Inspect, create, name, select, close, or arrange windows. List is RO; rename is naturally idempotent when unchanged. Layout and rebalance are primary-only presentation mutations. |
| Pane | `pane/list`, `pane/create`, `pane/select`, `pane/resize`, `pane/move`, `pane/swap`, `pane/break`, `pane/join`, `pane/close`, `pane/rename`, `pane/zoom`, `pane/input-sync`, `pane/attention`, `pane/status`, `pane/notice`, `pane/capture` | Inspect panes, mutate layout and presentation, control synchronized input, completion attention, source-owned status, or bounded notices, or capture pane content. List is RO; capture is RO when policy permits. Status and notices are available to primary and automation clients; rename, zoom, and input synchronization are primary-only. |
| Buffer | `buffer/list`, `buffer/create`, `buffer/read`, `buffer/delete` | Primary-only bounded internal paste-buffer inspection and mutation. List/read are RO; create requires explicit replacement for existing names. |
| Frame | `frame/read` | Read rendered frame fields and text (RO). |
| Terminal | `terminal/view`, `terminal/presentation/acknowledge`, `terminal/step`, `terminal/resize`, `terminal/command` | Render a client view, acknowledge receipt-bearing local frame commits, submit bytes/primary size, update exact-client observer geometry, or invoke a terminal command. Presentation acknowledgement is available to primary and observer clients; primary-only mutation applies to step and command; resize is observer-only and never changes primary or canonical geometry. Negotiated observer v3–v5 uses the resulting pushed render instead of fetching another view. |
| Agent | `agent/list`, `agent/task/list`, `agent/spawn`, `agent/shell/show`, `agent/shell/hide`, `agent/shell/command` | Inspect agents/tasks (RO), manage an agent shell, start prompt work, or spawn an agent. |
| External agent | `agent/external/launch`, `agent/external/register`, `agent/external/renew`, `agent/external/deregister`, `agent/external/presentation`, `agent/external/usage` | Additive `external-agent/1` launch capability, observational identity/presentation lease and durable usage reports. Launch issuance requires an attached primary; hook requests require capability-only authenticated Unix ingress, not an initialized client role. |
| Approval | `approval/list`, `approval/decide` | Inspect pending approvals (RO) or make a primary decision. |
| Configuration | `config/get`, `config/set`, `config/unset`, `config/reload`, `config/validate` | Inspect or validate config (RO), or mutate/reload it. |
| Project trust | `project/trust/list`, `project/trust/inspect`, `project/trust/decide`, `project/trust/revoke` | Inspect or decide project trust. |
| Remote trust | `remote/status`, `remote/invite`, `remote/client/list`, `remote/client/rename`, `remote/client/revoke` | Inspect or mutate paired-device trust. These methods require an initialized primary over authenticated local Unix control; even a paired Iroh primary is rejected. Invite, rename, and revoke require idempotency keys. |
| Snapshots | `snapshot/list`, `snapshot/create`, `snapshot/resume`, `snapshot/delete` | Inspect snapshots (RO) or persist, load, or delete layouts. |
| MCP | `mcp/list`, `mcp/retry` | Inspect configured server/tool availability (RO) or retry a server. |
| Events | `event/list` | Replay retained, authorized events after `after_event_id`; RO. |

## Terminal frontend contract

### Restricted external harness registration

An attached primary calls `agent/external/launch` with `pane_id`, lowercase
`harness`, and `version`. The response returns a random `launch_token`, monotonic
`generation`, a 120-second launch deadline and a 60-second renewal lease. The
launcher must deliver the token privately; do not put it in argv, logs or config.
Launch issuance deliberately bypasses the generic request replay cache. A lost
issuance reply requires a new capability; the unused binding expires.

Optional positive `root_generation` narrows this attached-primary issuance to
the retained predecessor's same UID, harness, pane and kernel root incarnation.
It is numeric provenance, not a credential or authorization to initialize a
primary. Missing/expired witnesses and replaced roots reject before allocation;
no unfenced fallback is allowed. A retired registration's finite tombstone may
supply provenance, but its old capability remains retired. This is an explicit
primary API constraint, not a vendor-launch wrapper or ordinary enrollment path;
vendor callbacks must not receive general primary authority.
Internal ordinary-enrollment groundwork now supports native connection-origin
UID/PID lookup on Linux and macOS with exact socket-option result bounds and
positive PID checks. Those primitives alone are not admission authority:
ordinary control still uses its established user/role gates. An inherited/passed
endpoint retains origin evidence rather than attesting its current writer.
Native ancestry groundwork pairs parent and creation token in one bounded
Linux procfs or macOS libproc record. The worker-oriented resolver retains at
most 128 links, checks a cooperative 100ms budget and reobserves every link;
missing, changed, unrelated, cyclic and over-budget evidence fails closed.
It does not read argv/environment or invoke helpers. Two passes are not an
atomic tree snapshot, executable attestation, current-writer proof or vendor
client/session association. The persistent-producer admission slice below
adds separate root/producer/sender fences; the primitives alone grant no new
authority. Only Linux execution is qualified; macOS runtime behavior requires
its own validation.

Socket origin PIDs can remain visible after their process exits, so a fresh
PID/start lookup alone cannot exclude reuse before the first observation.
Linux-only lifetime groundwork uses `SO_PEERPIDFD`, not `pidfd_open(pid)`, and
checks the exact socket-origin process is alive around each native record read.
Its retained descriptor is private and close-on-exec. Dead origins and changed
records reject; older kernels without the option and other platforms fail
closed. macOS requires a reviewed native lifetime/version equivalent before
this boundary can support admission there. Ordinary UID-only control is
unchanged, and lifetime evidence still does not attest a current writer.

Ordinary Unix runtime connection adapters now retain optional lifetime anchors
before frame handling. Capture runs off the runtime actor with an owned socket
duplicate, so cancellation cannot recycle the worker's descriptor. Connection
clones share the same anchor; unbound/remote/wrong-user/initialized connections
cannot attach fresh evidence, and recapture cannot replace an existing anchor.
Unsupported capture leaves ordinary UID-only control usable with no origin
evidence. Hosted sessions publish their own control socket through these shared
runtime adapters; the host administration front door is not a telemetry route.
Retaining an anchor alone grants no role or vendor registration.

Linux direct-parent lifetime groundwork derives the parent from a live socket
origin's native PID/parent/start record, never a supplied PID. It checks parent
birth and same-user evidence before/after pidfd capture and rechecks the helper's
exact birth/relationship. The retained private CLOEXEC parent fd remains distinct
from the helper socket-origin anchor, with zero-timeout death polling and a
cooperative100ms budget. Helper exit does not imply parent exit; socket EOF does
not prove either. Dead/reused/reparented/unreadable/cross-user/init/self or late
evidence rejects. Other platforms remain unsupported without numeric fallback.
This captures provenance only: there is no hook admission consumer, vendor
attestation, actual client/session association or pane authority. Consumers must
reject a surviving pane shell as producer and require separate writer/ancestry/
actor commit fences; two observations do not form an atomic process-tree snapshot.

#### Ordinary persistent-producer enrollment (implemented slice)

`agent/external/enroll` accepts `pane_id`, `harness` (`pi` or `opencode`),
`version`, `external_session_id`, `display_name`, bounded `observer_instance` and
`observer_kind: "persistent"` over an uninitialized same-user Unix connection.
First enrollment omits `predecessor_generation`; replacement supplies the exact
positive generation returned for its current predecessor. Neither field grants
authority or replaces the native root/producer/writer evidence.
The pane hint selects a candidate; native ancestry must prove it. No supplied
PID, parent PID, launch token, idempotency key or general control target is accepted.
The producer must be a distinct descendant of the pane root. A sender at the
root's own PID is rejected before admission reservation and by native observation,
even if the root shell has replaced its executable with `exec`. This boundary is
not an executable-name or vendor-attestation check.
Linux sender credentials are collected with the same read that consumes bytes;
all segments must name the retained live socket origin. Inherited writers,
missing/truncated/foreign ancillary data and passed descriptors cannot qualify.
Received descriptors are closed; ordinary UID-only control bytes are unchanged.
Linux listeners enable collection before accept for immediate first-frame sends.

Native observation runs off actor with at most 32 pending admissions, an 8192-byte
frame/4096-byte metadata bound, and a cooperative 2s admission deadline. Native
syscalls are not hard-cancelled: reservations remain held until work completes,
and late results reject. Actor settlement rechecks exact root/producer/connection
and releases reservations even if the response owner disconnected.

Success returns one private `launch_token`, `generation`, stable `run_id`,
`observer_epoch`, `observer_instance`, `agent_id`, exact
external session, `registered: true`, `controls: []`, and the existing 60s lease.
Identical live same-instance retry returns the same handle/identity with the same
original predecessor witness. A replacement instance against the current
generation receives a fresh handle/generation/observer epoch and resets its
presentation sequence owner, while run/agent/accounting provenance stay frozen.
Old handles cannot affect the replacement. Up to 128 instance identities remain
fenced for the live run; exhaustion rejects new replacement, never evicts stale
IDs or disables current-instance retries. Competing replacements cannot both
publish from one predecessor, and retired instances cannot reclaim the run.
Metadata conflict rejects. The lifecycle/presentation RPCs also require the same
kernel-qualified producer and sender. Handle possession alone is insufficient.
Producer exit retires telemetry independently of its surviving pane shell.
Ordinary leases renew from the existing daemon idle-maintenance owner while a
verified observer socket and producer both remain live; no callback heartbeat
frequency is required. Concrete adapter EOF/Drop releases observer health exactly
once, independently of retained connection clones. Up to 16 weak qualified
observer endpoints permit reconnect without holding socket descriptors; old
connection closure cannot erase a healthy observer. When every observer is lost,
automatic renewal stops and the last lease can expire even with a live producer.
Explicit primary-launched leases keep their existing caller-renewal contract.
`usage: "unavailable-source-continuity"` is explicit: ordinary usage RPCs are
rejected until durable source continuity exists, not billed under fresh owners.

Private shared client artifacts are now included by Pi/OpenCode manifests.
They open no resources at loading; genuine adapter starts must call the client.
The client discovers only nonsecret MEZ route hints and verifies socket/private
directory metadata plus actual native same-user peer UID before sending directly
from the producer. Its fixed hidden `mez harness-peer` subprocess only inspects a
borrowed socket descriptor and emits a captured bounded acknowledgment. It reads
no callback/credential data, performs no socket I/O, initializes no role, and
launches no vendor. The helper runs before HOME/config/runtime discovery with a
cleared environment. It is not an inherited vendor observer-channel prerequisite.
The client is bounded, unreferenced, allowlisted and neutral on loss. It exposes
typed lifecycle delivery, not ledger commitment or fabricated usage.

Private installation pins the installing binary, not a PATH/launcher helper
variable. Pi revision5 refreshes shared client bytes; OpenCode revision4 installs
a TUI-only default `{id,tui}` entry and one owned `tui.json` `/plugin` array member.
Exact Pi2/Pi3/Pi4 and OpenCode1/OpenCode2/OpenCode3 receipts use independently frozen
old client bytes; unknown historical helper references remain non-destructive errors.
Other historical variants, recovery/default-root/dry-run UX remain installer work.
Ordinary installed Pi entry callbacks now use this verified transport, with a
bounded process lease/queue and genuine sessionManager selection. Same-session
reload rotates observer credentials; new/resume/fork retire the exact prior
session before creating a fresh one. Duplicate/child/stale callbacks are inert,
and a lost handoff preserves its exact attempted instance for later recovery.
No vendor observer descriptor, launcher helper environment or forced session flag
is required for this lifecycle path. Vendor trust/disabled-extension policy is
not bypassed; supported Pi callback shapes remain best-effort observations.

`agent/external/pi-observation` is restricted to ordinary Pi enrollment. It accepts
the private token/generation/session, contiguous positive `sequence`, and one
existing typed Pi `event` (maximum metadata4096/event1024 bytes). The original
nested JSON bytes remain intact for typed bounds; duplicate fields reject before
generic parsing can overwrite them. The daemon reuses the Pi
LifecycleOwner reducer, not a second JavaScript outcome state machine. Exact
latest reply-loss replay is inert; gaps/conflicts/content and mixed generic
presentation reject. Replies carry `accepted`, exact `sequence` and `retired`;
these acknowledge lifecycle delivery only, not incurred-usage storage.

The OpenCode TUI entry uses the actual local frontend's `route.current` and cached
root session metadata, not a server plugin's first root event. This permits local
client/session association even when several clients receive the same server bus.
It only reads selected metadata/status/wait IDs and subscribes to scoped events;
it calls no navigation, approval, input or remote client/transcript API. Foreign,
child, home and unknown sessions cannot borrow the active source. A scoped,
unreferenced 250ms cache sampler detects silent selection changes. Exact old-source
retirement is serialized; status may coalesce, but wait identities are not evicted.
Unknown underlying state is not reported as idle. Transport recovery is bounded
to four same-instance attempts spaced at least5s, with locally observed wait
history retained. TUI scope disposal queues one finalizer without returning a
network promise to the vendor. The legacy server entry remains inert without its
old private fixture channel; it does not attribute server-wide activity.

TUI registration owns only its exact strict-JSON array member, preserving authored
plugin order/options/settings; uninstall leaves the shared document. Ambiguous
duplicate keys/members or edits conflict. JSONC merge/general bootstrap UX remain
separate unfinished installer work. The inspected local binary's embedded Bun
runtime qualifies client/native-sender fixtures, not a live interactive UI or all
vendor releases. Vendor disabled/pure/trust policy is not overridden.

Additional vendor callbacks, short-lived helper association and macOS
lifetime/sender equivalents remain unfinished. Unsupported platforms/kernels
retain ordinary control without enrollment; no vendor relaunch/fallback occurs.

The exact canonical harness `gemini` is retired: new launch and usage admission
are rejected before capability or accounting-work allocation. Existing expense,
history and replay tombstones are not deleted or relabelled. Provider names and
Gemini-named models under native or other harnesses are unaffected. Retirement
does not filter completion of already-admitted work or install a replacement.

Hooks use fresh authenticated Unix connections **without** `control/initialize`:

- `agent/external/register`: `launch_token`, `generation`, `external_session_id`,
  `display_name`, optional bounded `objective`. Identical retry returns the same
  `agent_id`; changed registration metadata is rejected.
- `agent/external/renew`: token, generation and exact external session ID.
- `agent/external/deregister`: the same fields. Repeated retirement is a no-op
  during the 300-second tombstone horizon; after pruning credentials are rejected.

The server derives pane, window and trusted project scope, never accepts those
as hook claims. Multiple explicit launches can coexist in a pane. List metadata
distinguishes harness/version and marks native controls unsupported (`controls:
[]`). EOF does not retire a lease; missing renewal means telemetry unavailable,
not process death. Root replacement, pane close and runtime restart invalidate
registrations. Restart requires a fresh launch rather than reviving snapshot
identities. This is same-OS-user bearer authority, not executable attestation;
vendor hooks and bootstrap remain separately certified integrations.

### Registration-owned presentation

`agent/external/presentation` accepts the launch token, generation, exact external
session ID, positive `sequence`, `state`, and optional `title` (null clears this
source's title). States are `ready`, `running`, `approval-wait`, `input-wait`,
`complete`, `interrupted`, `failed`, and `background`. Titles are inert text up to
128 bytes; controls and bidi overrides are rejected. Identical sequence/payload
replay is inert; older or conflicting observations fail. The event source must
serialize its sequence, not derive authority from a vendor timestamp.

Presentation belongs to the renewable registration, not the hook connection.
EOF leaves it intact; deregistration, expiry, replacement, closure or restart
retire only that owner. Titles never mutate stored mux provenance. Explicit pins
and visible native primary identity take precedence. Multiple launch suggestions
use launch-generation precedence; ordinary shell/program titles reappear when
the last suggestion retires. Status is externally reported, not independently
verified execution state. Presentation updates do not renew the lease: adapters
must send explicit renewal during long idle sessions.

`mez harness-event` is a fixed normalized helper, not an upstream hook parser.
See the [CLI reference](../cli.md) for its bounded stdin envelope and limits.

### Durable external usage reports

`agent/external/usage` takes the current `launch_token`, `generation` and
`external_session_id`, plus `epoch`, stable `event_id`, positive `sequence`,
`mode` (`delta` or `cumulative`), UTC `observed_at`, `provider`, `model`, and
`counters`. Each request must contain one control frame. Counters require
`input_tokens` and `output_tokens`; optional `reasoning_tokens`,
`cached_input_tokens` and `cache_write_input_tokens` are unknown when omitted.
Input includes cache subsets, and output includes reasoning. Invalid subsets,
negative values, overflow and unknown payload fields are rejected.

Delta sequences start at 1 and are contiguous. Submit missing observations before
advancing after a gap. Cumulative attachment requires `baseline: true` on its
first sample; that sample is uncharged. Later samples omit baseline and apply
only the difference. A final sample uses the same stream, never a second aggregate
source. A new model, mode, counter-availability pattern or reset requires a new
epoch. Do not use a fresh epoch to replay already accepted expense.

Receipts, checkpoints and normalized deltas commit atomically on a storage worker.
The result reports `accepted`, `durable`, `applied`, `revision`, and reasoning
coverage. Identical retry adds nothing and returns the current absolute checkpoint;
conflicting accepted event IDs fail. Lost transport replies do not undo admitted
accounting. Reports must be within the 91-day horizon and not future-dated;
pruned old sequences cannot become new deltas. High-water checkpoint tombstones
remain after raw events and receipts are pruned.

Concurrent first opens may contend while enabling SQLite WAL. The ledger retries
only that idempotent setup step within a finite contention budget, before usage
transactions begin. It does not automatically replay accounting transactions;
persistent contention returns an explicit error and leaves replay identity intact.
Already-current schema inspection does not acquire migration writer ownership;
actual migrations still recheck the version inside their immediate transaction.

SQLite schema v2 migrates legacy rows to harness `mez` without counter backfill.
The ledger stores harness/model counters and opaque stream identities, not prompt,
transcript or pane paths. Native latest-request samples remain separate. Pane
reset changes only the external view baseline; session and durable expense remain.
Native `agent/list` reports the existing `compacting` status whenever an exact
compaction operation owns the pane's current conversation. Manual preparation
may therefore report `compacting` with `last_turn_id: null`; it does not invent
an ordinary turn. Completion/cancellation removes that current projection.
The pane footer and `/status` use the same ownership. `/status` includes preparing,
queued or claimed phase, logical epoch/elapsed time and existing human-pause detail;
claimed describes a runtime claim, not proof of provider execution. Elapsed time
belongs to the logical operation, not each chunk/retry's lease. No new control
record field or configuration setting is introduced by this projection repair.

`agent/list` exposes separate `external_token_usage` runtime-instance telemetry.
The harness-aware `/status` reader includes native and external events in rolling
history and its oldest-event boundary. Tables retain harness and unknown-reasoning
coverage separately, never silently merging native same-model totals. The legacy
native-only storage reader remains isolated. New stream slots are bounded independently
of workers; updates and retries of existing streams remain eligible at capacity.
External-only reports do not allocate native conversations. Usage is
externally reported, not independently verified invoice data. After registration
expiry or restart, new admission needs a fresh launch; already admitted reports
can settle without reviving a registration.

### Accounting project identity foundation

Telemetry schema v3 adds a separate private canonical-root mapping. Root bytes
are lossless and mapped to opaque IDs independently of trust and MMP audiences.
Worker preparation uses registered trust records, including zero-use and revoked
projects. Provider dispatch captures the currently eligible deepest trusted root
against that qualified mapping; unavailable cwd, store or mapping is explicitly
unattributed. Changing cwd or revoking trust does not rewrite an issued request's
origin. Stale preparation inventory cannot replace current mapping evidence.

Canonical aliases share a mapping. A detected directory-object replacement at
the same path creates a new ID and leaves historical mappings intact; relocation
is not guessed from Git metadata or titles. Directory-object evidence is not an
execution permission or a portable guarantee against filesystem inode reuse.
Telemetry schema v4 adds nullable project attribution to events and external
checkpoints, preserving legacy rows as unattributed. External launches freeze
server-resolved project origins; later cwd changes cannot repartition the stream.
Identical event replay compares project identity too. Grouped history retains
project/harness/model identity and category coverage in one read snapshot with
one UTC time and oldest-event boundary. Scans are bounded and fail rather than
return partial totals. Legacy status still selects native events only, now using
the consistent snapshot reader. An earlier write-gap diagnostic survives later
successful writes because they do not establish recovery of missing expense.
Native producer accounting retains frozen project partitions beside overall
counters. Exact issued requests may settle reported expense after content is
stale or cancelled, without reviving the task or charging a replacement pane.
Conversation metadata v2 preserves partitions; legacy totals remain unattributed.
Restore and failed-resume rollback emit no new usage events. Pane reset changes
only its view. Scoped `/status` reporting remains separate integration work;
legacy expense is never backfilled from cwd or transcript totals.

An alternative interactive frontend is a primary client. Obtain the initial
render with `terminal/view`:

```json
{"jsonrpc":"2.0","id":2,"method":"terminal/view","params":{"client_size":{"columns":120,"rows":40}}}
```

The result is `{ "view": RenderedClientView | null, "presentation_ids":
[integer], "view_identity": string, "event_cutoff": integer,
"render_rate_limit_fps": integer }`.
`event_cutoff` is the latest ordered server event whose applied state is
represented when the authoritative view is rendered. A view includes its role;
authoritative and client size; viewport and scroll bounds; cursor state;
input/output modes; an optional agent-prompt region; textual `lines`; and
`line_style_spans`. A frontend renders this projection, respecting cursor,
styles, scroll responsibility, bracketed paste, mouse reporting, and any
animation refresh interval.

An optional `iroh_status_slot` identifies the row, column, width, and quality
renditions reserved for client-local Iroh status composition. The built-in
attach client overlays padded `up` or `dn` text there and selects the rendition
from its connection-local quality sample; a view without a slot receives no
overlay. This is an implementation conformance gap: `SPEC.md` requires textual
`good`, `degraded`, `poor`, or `unknown` labels and omission for ended
connections, rather than the current connection-state labels. Do not infer
textual quality from the current pill; use `show-iroh-status` for that value.

For a conditional fetch, send `if_view_identity` with the server-issued lowercase
SHA-256 identity of the last completely committed exact-client view. The identity
also covers pending presentation receipt IDs and effective render cadence. When
all of these still match, the server may instead return
`{ "not_modified": true, "view_identity": string, "event_cutoff": integer,
"render_rate_limit_fps": integer }`, without `view` or `presentation_ids`.
Reject this response unless its identity matches the committed base; otherwise
retain that frame and advance the event cutoff. A null `view` is not a
not-modified response. Omitting `if_view_identity` retains the complete-view
response, and older peers may return a complete view even when it is supplied.

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
optional `view`, `presentation_ids`, optional `event_cutoff`, UI theme,
acknowledged client detach, and session termination. When an inline view is
present, `event_cutoff` and `presentation_ids` are from the same authoritative
render boundary. After completely writing a local frame, a control or legacy
attach frontend sends those IDs through the authenticated idempotent
`terminal/presentation/acknowledge` method. Matching pending zen focus labels
then start their configured lifetime; malformed IDs are rejected and stale or
duplicate IDs do not renew it. Pushed Iroh rendering retains its stream-flush
acknowledgement and does not send this control handshake. A true
`client_detached` ends that attach loop cleanly without implying that the
durable session was terminated.

```json
{"jsonrpc":"2.0","id":4,"method":"terminal/presentation/acknowledge","params":{"idempotency_key":"frame-42","presentation_ids":[7]}}
```

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
negotiate `5 → 4 → 3 → 2 → 1`; observers negotiate `5 → 4 → 3 → 1`, using only
explicit unsupported-event-version initialization results to continue to the
next candidate.
A primary or observer v3–v5 client renders the initial and subsequent pushed
updates without issuing steady-state `terminal/view` requests, reassembling
v4/v5 chunks and applying v5 sparse updates atomically when used.
Primary control responses acknowledge input and resize mutations; observer v3–v5
uses `terminal/resize` to acknowledge only its client-local geometry change.
Legacy event streams retain notification-plus-fetch behavior. This is
rendered-view/input-step control, not raw PTY export; specialized frontends
should design around the supplied view model.

## Events and replay

### Unix event binding handshake

An authenticated Unix `control/initialize` must explicitly include
`event_stream_version: 1` to request event credentials. For a successful
session-client attachment, the result adds:

```json
{"event_binding":{"version":1,"token":"<opaque-binding-token>","expires_at_unix_seconds":1767225660}}
```

This is a result fragment, not a separate response. The token is short-lived
(currently 60 seconds), single-use, and bound to the exact initialized client
and authenticated Unix peer UID. Omission of `event_stream_version` does not
mint a token merely because Unix uses v1 events. Treat the token as a credential;
never log it or substitute a client ID for it.

Connect to the Unix event socket and send this as its first control-framed
JSON request:

```json
{"jsonrpc":"2.0","id":1,"method":"event/initialize","params":{"binding_token":"<opaque-binding-token>","after_event_id":42}}
```

`after_event_id` is optional and defaults to zero; when supplied it must be a
non-negative integer. The event service authenticates the socket peer and
consumes the token through the runtime actor before streaming notifications.
There is no separate success response for this binding frame. Unknown,
expired, reused, or wrong-peer tokens fail closed; detachment invalidates
unconsumed tokens. Every later batch reauthorizes the exact live client, so
observer cutoffs and detach transitions remain effective. Reconnect requires
a fresh binding token rather than replaying the consumed one. This is the
Unix event-socket handshake, not the Iroh server-opened stream preface.

### Notifications and retained replay

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
