# Mezzanine control JSON-RPC reference

## Purpose

Describe the implemented `mezctl/1` endpoint and the specified `mezctl/2`
independent-primary cutover. This page summarizes the implementer contract;
the [control endpoint in `SPEC.md`](../../../SPEC.md#13-control-endpoint) is
normative. The current runtime remains v1 and MUST NOT enable a second primary
until the complete v2 cutover is implemented.

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
with `unsupported_version`; a v1 server continues rejecting version 2 and must
not advertise v2 feature flags.

V2 event delivery binds every stream to an exact initialized client. Shared
events target all primaries, private presentation events target one client,
and observers receive only their source primary's live session view after the
approval marker. Snapshot payload v5 persists shared topology, canonical size,
and landing navigation, but never live clients, owner, event credentials, or
transient presentation.

## Transport, framing, and initialization

The default transport is a user-private Unix-domain socket. TCP is optional,
loopback-only by default, and remote TCP is disabled unless explicitly
configured. Unix clients should use peer credentials and the private socket
path. TCP clients must authenticate with an unguessable bearer token or a
stronger configured mechanism before receiving session data or mutating state.

The opt-in Iroh adapter uses ALPN `mezzanine/transport/1` and carries the same
bounded `mezctl/1` frames on exactly one long-lived, client-opened bidirectional
control stream. The server accepts no client-opened unidirectional streams and
lowers each connection to one concurrent bidirectional control stream. An
interactive client may request `event_stream_version: 1` during
`control/initialize`; only after that initialize response is flushed may the
server open one unidirectional stream beginning with the exact preface
`mezzanine/events/1\n`. Setup, idle operation, writes, and teardown are bounded;
wrong ALPNs, excess streams, malformed frames, stalled setup, and one failed
connection are isolated from later clients and from the Unix listener.

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
{"jsonrpc":"2.0","id":1,"method":"control/initialize","params":{"client_name":"example-ui","client_version":"1.0.0","requested_version":1,"requested_role":"primary","client":{"name":"example-ui","terminal":{"columns":120,"rows":40,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}
```

The result contains `selected_version`, a secret-free `server` identity, the
granted role, negotiated `capabilities`, `approval_pending`, and
`observer_request`; it includes `session` except for a pending observer.
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
replacement, and role escalation fail closed. Re-pairing a revoked endpoint
creates a new active record without allowing the old record to shadow it. Never
log or copy invitation or device credentials into diagnostics.

## Roles, authorization, and idempotency

Requested roles are `primary`, `observer`, `agent`, and `automation`. Under
v1, one interactive primary owns terminal input. Under v2, every attached
primary may submit actor-ordered input against its own navigation, while only
the layout owner's resize changes canonical geometry. A primary request always
requires a verifiable interactive terminal; a client descriptor alone is not
sufficient when the transport does not trust that assertion.

Observer attachment begins as `pending_observer`. Before approval it may only
initialize, attach or inspect its own observer request, cancel its own request,
or shut down; it receives no session view. Approved observers receive only
post-approval rendered views and permitted events, and cannot mutate the
session. Primary-only operations fail with `not_primary` for other callers.
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

## Version 1 method catalog

The table gives the complete implemented v1 catalog. V2 removes
`session/attach` and `client/select_primary` and adds
`client/set_layout_owner`; the table below does not define the v2 surface.
“RO” means read-only and
naturally idempotent. Every other entry requires `idempotency_key` unless its
note says otherwise. The parameter and result object schemas are specified in
the [baseline method table in `SPEC.md`](../../../SPEC.md#13-control-endpoint).

| Namespace | Methods | Access and purpose |
| --- | --- | --- |
| Control | `control/initialize`, `control/shutdown`, `control/cancel` | Negotiate a connection, close it, or cancel an owned request. Shutdown is naturally idempotent. |
| Session | `session/list`, `session/get`, `session/attach`, `session/rename`, `session/kill` | List/get sessions (RO), attach as primary/observer, rename a session, or terminate it; termination requires `force` while live panes remain. Observer attach creates pending metadata only. |
| Client | `client/list`, `client/detach`, `client/select_primary` | Inspect clients, detach a client, or atomically transfer primary ownership. |
| Observer | `observer/list`, `observer/inspect`, `observer/approve`, `observer/reject`, `observer/revoke` | Inspect and primary-manage observer requests. Pending/approved observers can inspect only their own request-local status. |
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

The result is `{ "view": RenderedClientView | null }`. A view includes its
role; authoritative and client size; viewport and scroll bounds; cursor state;
input/output modes; an optional agent-prompt region; textual `lines`; and
`line_style_spans`. A frontend renders this projection, respecting cursor,
styles, scroll responsibility, bracketed paste, mouse reporting, and any
animation refresh interval.

Send user input through `terminal/step`, with bytes as integers in `0..255`.
Include `client_size` whenever geometry changes and set `render` false only
when the caller deliberately wants no immediate view. Its result reports input
count, forwarded bytes, multiplexer/agent/mouse actions, redraw requirements,
unsupported actions, optional `view`, UI theme, and session termination.

```json
{"jsonrpc":"2.0","id":3,"method":"terminal/step","params":{"idempotency_key":"ui-step-0001","client_size":{"columns":120,"rows":40},"render":true,"input_bytes":[108,115,13]}}
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

The recommended loop is initialize, fetch a view, render it, pass physical
input and size updates via `terminal/step`, then apply the returned view or
request a fresh `terminal/view`. Local clients use the Unix event socket; an
Iroh attach negotiates the version 1 server-opened event stream. Both transports
use events only as redraw wakeups and refetch authoritative rendered state over
control. This is rendered-view/input-step control, not raw PTY export;
specialized frontends should design around the supplied view model.

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
The capabilities limits expose retention.

Every Iroh event batch re-resolves the live session client before projection.
Pending observers receive no event stream until approval. Approved observers
see only `SessionView` events at or after their approval marker; primary-only,
other-observer, agent, automation, and pre-approval payloads are omitted.
Revocation, detach, control completion, reset, or connection shutdown closes the
event stream. Transport endpoint authentication alone never authorizes events.

## Related pages

- [Protocol conventions](common-conventions.md)
- [`maap/1` action protocol](maap.md)
- [`mmp/1` local messages](mmp.md)
- [Normative control contract](../../../SPEC.md#13-control-endpoint)
