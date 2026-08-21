# `mezctl/1` control JSON-RPC reference

## Purpose

Specify the version 1 control endpoint used by attach clients, automation, and
alternative frontends. It exposes multiplexer state and mutation separately
from agent-to-agent messaging. This page summarizes the implementer contract;
the [control endpoint in `SPEC.md`](../../../SPEC.md#13-control-endpoint) is
normative.

## Transport, framing, and initialization

The default transport is a user-private Unix-domain socket. TCP is optional,
loopback-only by default, and remote TCP is disabled unless explicitly
configured. Unix clients should use peer credentials and the private socket
path. TCP clients must authenticate with an unguessable bearer token or a
stronger configured mechanism before receiving session data or mutating state.

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

## Roles, authorization, and idempotency

Requested roles are `primary`, `observer`, `agent`, and `automation`. One
interactive primary client owns interactive terminal input. A primary request
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

## Method catalog

The table gives the complete baseline catalog. “RO” means read-only and
naturally idempotent. Every other entry requires `idempotency_key` unless its
note says otherwise. The parameter and result object schemas are specified in
the [baseline method table in `SPEC.md`](../../../SPEC.md#13-control-endpoint).

| Namespace | Methods | Access and purpose |
| --- | --- | --- |
| Control | `control/initialize`, `control/shutdown`, `control/cancel` | Negotiate a connection, close it, or cancel an owned request. Shutdown is naturally idempotent. |
| Session | `session/list`, `session/get`, `session/attach`, `session/rename`, `session/kill` | List/get sessions (RO), attach as primary/observer, rename a session, or terminate it; termination requires `force` while live panes remain. Observer attach creates pending metadata only. |
| Client | `client/list`, `client/detach`, `client/select_primary` | Inspect clients, detach a client, or atomically transfer primary ownership. |
| Observer | `observer/list`, `observer/inspect`, `observer/approve`, `observer/reject`, `observer/revoke` | Inspect and primary-manage observer requests. Pending/approved observers can inspect only their own request-local status. |
| Window | `window/list`, `window/create`, `window/rename`, `window/select`, `window/close` | Inspect, create, name, select, or close windows. List is RO; rename is naturally idempotent when unchanged. |
| Pane | `pane/list`, `pane/create`, `pane/select`, `pane/resize`, `pane/move`, `pane/swap`, `pane/break`, `pane/join`, `pane/close`, `pane/attention`, `pane/capture` | Inspect, mutate layout, control a pane's completion-attention pill, or capture pane content. List is RO; capture is RO when policy permits. |
| Frame | `frame/read` | Read rendered frame fields and text (RO). |
| Terminal | `terminal/view`, `terminal/step`, `terminal/command` | Render a client view, submit bytes/resize, or invoke a terminal command. Primary-only mutation applies to step and command. |
| Agent | `agent/list`, `agent/task/list`, `agent/spawn`, `agent/shell/show`, `agent/shell/hide`, `agent/shell/command` | Inspect agents/tasks (RO), manage an agent shell, start prompt work, or spawn an agent. |
| Approval | `approval/list`, `approval/decide` | Inspect pending approvals (RO) or make a primary decision. |
| Configuration | `config/get`, `config/set`, `config/unset`, `config/reload`, `config/validate` | Inspect or validate config (RO), or mutate/reload it. |
| Project trust | `project/trust/list`, `project/trust/inspect`, `project/trust/decide`, `project/trust/revoke` | Inspect or decide project trust. |
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

The recommended loop is initialize, fetch a view, render it, pass physical
input and size updates via `terminal/step`, then apply the returned view or
request a fresh `terminal/view`. Use events to trigger refreshes rather than a
fixed polling redraw loop. This is rendered-view/input-step control, not raw
PTY export; specialized frontends should design around the supplied view model.

## Events and replay

Server notifications use `event/*` methods. Params contain ordered `event_id`,
`time`, `event_type`, `object`, and `session_id` when the recipient is allowed
to know it; state changes should include `previous` when available. Baseline
events cover client and observer changes, window/pane changes, agent task
changes, approvals, config, snapshots, and MCP availability.

Per-connection event order is preserved. Reconnect with `event/list` and a
known `after_event_id`; replay can be refused once retention has elapsed. The
capabilities limits expose retention. Pending observers receive only their own
request status and no session-bearing event data.

## Related pages

- [Protocol conventions](common-conventions.md)
- [`maap/1` action protocol](maap.md)
- [`mmp/1` local messages](mmp.md)
- [Normative control contract](../../../SPEC.md#13-control-endpoint)
