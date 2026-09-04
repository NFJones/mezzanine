# `maap/1` agent action protocol reference

## Purpose

Specify Mezzanine Agent Action Protocol version 1 (`maap/1`), the contract by
which a model proposes visible, local, and runtime-mediated work. It is an
agent-to-runtime action protocol, not a frontend API and not the local
agent-message wire protocol. The [MAAP contract in `SPEC.md`](../../../SPEC.md#98-mezzanine-agent-action-protocol)
is normative.

## Batch envelope and ownership

The internal and audit batch is a JSON object with `protocol: "maap/1"`, a
non-empty `rationale`, optional durable `thought`, `turn_id`, `agent_id`,
`actions`, and `final`. A canonical explicitly final internal batch may contain
zero actions; other internal batches contain one or more. Provider-native compact schemas require only
the model-authored `rationale` and `actions`; Mezzanine stamps protocol,
turn/agent identities, final-state bookkeeping, and stable action identities.

`rationale` is a concise immediate-action summary, not private chain of
thought. `thought`, when present, is a durable continuation note and is not
normally rendered. Every action has a `type` and may have an additive
action-local rationale. Models must not supply authoritative effect claims or
rely on self-chosen IDs.

```json
{"protocol":"maap/1","rationale":"Inspect the owner before making a focused change.","thought":null,"turn_id":"runtime-owned","agent_id":"runtime-owned","actions":[{"type":"shell_command","summary":"Locating the owner module","command":"rg -n 'target_symbol' crates"}],"final":false}
```

Structured providers carry one complete batch through their native tool or
schema mechanism. Fallback text uses exactly one `mezzanine-action-json` fenced
JSON block. In both cases the same validation, policy, audit, and result rules
apply. The live provider schema exposes only the current allowed action
surface; an absent action family must be requested, not emulated.

## Action catalog

| Action | Required fields | Contract boundary |
| --- | --- | --- |
| `say` | `status`, `content_type`, `text` | Display-only `progress`, `final`, or `blocked` text. Supported plain-text, Markdown, and diff source is rendered while streaming, then validated and promoted in place without truncation or final replay. Commands and patches in text do not execute. |
| `request_capability` | `capability`, `reason` | Requests a coarse runtime action family; it is not a user permission request. |
| `shell_command` | `summary`, `command` | Sends exact local shell input through the effective native or pane shell transport. Optional `interactive`, `stateful`, and `timeout_ms` refine execution. |
| `apply_patch` | `patch` | The only semantic file-content mutation action; payload uses Mezzanine `*** Begin Patch` format. |
| `web_search` | `query` | Runtime-owned web search, only for user-requested current web information. |
| `fetch_url` | `url` | Runtime-owned HTTP(S) retrieval, never a local-path reader. |
| `send_message` | `recipient`, `content_type`, `payload` | Requests local MMP delivery. |
| `spawn_agent` | `role`, `task_prompt` | Requests pane-backed delegation. Optional `session: fork | new` selects a bounded immutable parent-history snapshot or an isolated child session; scope and policy remain runtime-controlled. |
| `config_change` | `setting_path`, `operation`, `value` | Proposes a supported live leaf configuration mutation. Set values accept strings, signed integers, booleans, or string arrays; objects, null set-values, floats, and mixed arrays are rejected. Provider schemas carry the value as a string containing a JSON scalar or string array, while plain non-JSON text is a string value. |
| `mcp_call` | `server`, `tool`, `arguments` | Invokes one currently exposed MCP tool with JSON-object arguments. |
| `memory_search` | `query` | Searches enabled runtime-owned durable memory; a memory UUID retrieves that record exactly. |
| `memory_store` | `kind`, `keywords`, `content` | Stores safe, durable, non-secret memory; optional priority, scope, and retention apply. |
| `issue_add` | `kind`, `title`, `depends_on` | Creates a local project issue; state, body, and notes are optional. |
| `issue_update` | `id` | Updates an issue with explicit replacement or clear fields. |
| `issue_query` | none | Queries local issues with optional kind, state, text, limit, and refresh filters. |
| `issue_delete` | `id` | Deletes a local project issue. |
| `complete` | none | Marks the turn complete when exposed by a compatibility surface. |

`request_skills` and `call_skill` are reserved actions and must not appear
while model-selected skills are disabled. `abort` is controller-owned and must
not appear in provider action schemas. Availability of memory, issue, MCP,
network, and other action types depends on the live turn capability surface.

### Canonical optional fields

The canonical internal contract accepts optional fields that a compact
provider schema may omit from a particular turn:

- `shell_command`: `interactive` and `stateful` default to `false`;
  `timeout_ms` is optional.
- `apply_patch`: `strip` is accepted for compatibility but is unsupported by
  Mezzanine patch payloads and should be omitted.
- `web_search`: optional `domains`, `recency_days`, and `max_results` filters.
- `fetch_url`: optional `format` and `max_bytes` response bounds.
- `memory_search`: optional `limit`; `memory_store`: optional `priority`,
  `scope`, and `expires_in_days`.
- `spawn_agent`: optional `placement`, `cooperation_mode`, `read_scopes`,
  `write_scopes`, and `session`. `session: fork` copies the bounded parent
  transcript into a distinct child conversation; `session: new`, or omission,
  creates an isolated child conversation. Include task-critical facts in
  `task_prompt` in either mode. Session selection never broadens authority;
  omitted scopes inherit the parent and explicit empty arrays deny it.

Treat the live schema as authoritative for what the current turn may submit.
Provider compatibility carriers can be stricter than the canonical internal
contract and do not make hidden fields available.

## Execution and approval

Mezzanine validates the complete batch and active capabilities before evaluating
permission. It independently classifies effects; model-supplied claims do not
grant authority. Actions requiring approval become `blocked` and are presented
to the primary client. Denial, cancellation, timeout, policy prohibition, and
pre-execution failure remain durable results rather than being silently retried.

`shell_command` is shell-backed. Native mode starts a fresh compatible shell
inferred from the validated pane root process; pane mode uses the interactive
pane shell. Its `summary` is visible progress text; the raw command is not part
of the summary. Model-authored commands cannot use heredoc/here-string
redirection or invoke MAAP semantic action names as shell programs. Use
`apply_patch` for ordinary file-content edits and shell commands for inspection,
validation, and non-content filesystem operations.

An `apply_patch` payload begins with `*** Begin Patch` and ends with
`*** End Patch`. It contains add, update (with anchored hunks), delete, or
move-with-update operations. Paths are normally relative to the pane working
directory and cannot traverse with `..`. Patch failures are recoverable
evidence: inspect fresh owner context and submit a smaller anchored patch;
do not claim a mutation succeeded until its result confirms it.

`web_search` and `fetch_url` execute through the runtime HTTP executor and are
policy/audit controlled. `send_message` lowers to MMP; plain `text/plain` is
normalized to `text/plain; charset=utf-8`. `config_change`, `spawn_agent`, and
MCP actions remain subject to their respective runtime validation and approval
rules.

## Results, continuation, and retries

Mezzanine emits one result for every identifiable accepted, rejected, blocked,
denied, running, succeeded, failed, cancelled, `timed_out`, or interrupted
action. A result includes:

| Field | Meaning |
| --- | --- |
| `protocol`, `turn_id`, `agent_id` | Runtime-owned MAAP and turn identity. |
| `action_id`, `action_type` | Stable synthesized local action identity and type. |
| `status` | `rejected`, `blocked`, `denied`, `running`, `succeeded`, `failed`, `cancelled`, `timed_out`, or `interrupted`. |
| `content` | Bounded model-readable blocks, including `{ "type": "text", "text": string }`. |
| `structured_content` | Type-specific result data or `null`. |
| `is_error`, `error` | Error flag plus `{ "code", "message", "data" }` when applicable. |

Succeeded and running results are non-errors with no error object. Blocked
results are non-errors with pending approval data. All other terminal failure
statuses are errors. Baseline error codes include `invalid_action`,
`unavailable_capability`, `approval_required`, `approval_denied`,
`policy_forbidden`, `permission_unknown`, `command_parse_unknown`,
`command_failed`, `command_timeout`, `user_interrupted`, `transport_error`,
`mcp_protocol_error`, `mcp_tool_error`, `spawn_failed`, and `internal_error`.

Local-action results include summary, pane-shell transport, dispatched command
where relevant, delivery/approval metadata, and bounded cleaned terminal
observation. The continuation receives a compact projection of that result.
Mezzanine may request a bounded correction for model-correctable failures, but
does not automatically repeat completed shell work, approval denials, user
cancellations, or timeouts.

## Provider integration requirements

Provider adapters must preserve the active action surface and lower any
provider-specific carrier or shim into one canonical MAAP batch before
validation. They must not require model-authored runtime identity fields.
Native structured tool output is preferred; text parsing is a compatibility
path, not an alternative authority boundary. Provider transcript projection
must retain enough action/result evidence for continuation while omitting
secrets, hidden policy, wrapper traffic, and audit-only data.

## Related pages

- [Agent actions](../agent-actions.md) for the concise user-facing overview
- [`mmp/1` local messages](mmp.md) for `send_message` delivery
- [Protocol conventions](common-conventions.md)
- [Normative MAAP contract](../../../SPEC.md#98-mezzanine-agent-action-protocol)
