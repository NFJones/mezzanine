# Agent actions

## Purpose

Describe the user-visible `maap/1` action model, its action families, and the
review and result semantics that govern agent execution.

## Prerequisites

Read [Agent overview](../agent/overview.md) and [Approvals and review](../safety-and-trust/approvals-and-review.md).

## Action batch model

An agent response is a validated `maap/1` batch containing a concise rationale
and one or more actions, plus a bounded `objective` stating what the agent is
currently working on (a string, or `null` when the published objective is
unchanged). The objective is a factual statement of current work and never a copy
of the user prompt; it is published for read-only peer discovery, and a null,
missing, malformed, or out-of-bounds value publishes nothing and never fails a
turn. Mezzanine assigns turn and action identities,
validates actions against the current allowed set, independently classifies
their effects, and records a result for every syntactically identifiable
action. A result can be
`rejected`, `blocked`, `denied`, `running`, `succeeded`, `failed`, `cancelled`,
`timed_out`, or `interrupted`. A batch-level parse or schema failure that
prevents Mezzanine from identifying an action is recorded as a malformed
response error instead. Model-provided effect claims and bookkeeping identities
are not authoritative.

`say` presents display-only text as `progress`, `final`, or `blocked`; text
that looks like a command or patch does not execute. Action results are bounded
evidence for a later continuation, while credentials, hidden policy, and raw
terminal state remain outside ordinary model context. A blocked action result
means execution is waiting at a resumable approval boundary. A `say` action
with status `blocked` instead ends the conversation because user input or an
external condition is required.

## Action families

| Action | Use | Important boundary |
| --- | --- | --- |
| `say` | Present progress, completion, or a blocker to the user. | It is display-only and cannot execute text that resembles a command or patch. |
| `shell_command` | Local shell inspection, commands, validation, and filesystem operations. | Uses the effective native or pane shell mode and can require approval. |
| `apply_patch` | Semantic file-content add, update, move, or delete using `*** Begin Patch` format. | It is a MAAP action, never a shell executable; confirmed earlier file changes remain applied if a later file operation fails. |
| `web_search`, `fetch_url` | User-requested current web search or HTTP(S) retrieval. | They are runtime network actions, not local-path readers. |
| `list_agents` | Read-only discovery of session peers and their published objectives. | It never prompts for approval and returns only bounded identity rows. |
| `send_message`, `spawn_agent` | Local coordination and pane-backed delegation. | `send_message` is approved per message and recipient: `ask` blocks an ungated send as a resumable approval bound to the recipient and payload digest, `auto-allow` requires a non-empty rationale, `full-access` and `host-access` use the policy bypass path, and configured deny rules always win. `spawn_agent` may use `session: fork` for a bounded immutable parent-history snapshot or `session: new` for isolation; scope and policy inherit independently and cannot be broadened by that choice. |
| `config_change` | Supported live leaf configuration mutation. | Set values accept strings, signed integers, booleans, or string arrays; execution-boundary settings remain direct-user-only. |
| `mcp_server_search`, `mcp_server_get` | Discover configured MCP servers and retrieve one complete tool contract. | Retrieve the selected server before a later call; discovery does not invoke an external tool. |
| `mcp_call` | Call a durably retrieved, currently available configured MCP tool. | The live registry revalidates server, tool, arguments, external capability, and approval policy. |
| `memory_search`, `memory_store` | Retrieve or retain runtime-owned durable memory when enabled. | Records must be safe, durable, and non-secret. |
| `issue_add`, `issue_update`, `issue_query`, `issue_delete` | Manage runtime-owned local issues for the active project. | Issue records remain subject to the configured action set and project-store rules. |

The active provider schema exposes only the action subset allowed for the
current request. `agents.enabled_actions` supplies the configured upper bound
and defaults to every executable action. Capability negotiation and
model-selected skill actions are not part of the ordinary provider schema. The
model uses exposed actions directly; the runtime still revalidates live
integration availability, permissions, and arguments, returning an explicit
action result on failure.

## Local mutation and recovery

Use `shell_command` for shell-visible inspection and `apply_patch` for ordinary
file-content changes. Patch paths are normally relative to the pane working
directory; traversal is rejected. Under active, non-bypassed Bubblewrap,
absolute paths may target effective write scopes. Other execution modes reject
absolute patch headers and targets outside the pane working directory. A patch
failure is evidence, not success: preserve confirmed per-file changes, inspect
the failed target's current context, and issue a smaller fresh patch rather
than replaying the same stale hunk. Shell commands report pane-shell transport,
bounded output, exit, timeout, and truncation data.

Blocked actions wait for a primary-client decision; observers cannot decide
them. Denied, timed-out, cancelled, and policy-forbidden actions remain in the
result history. Mezzanine can provide bounded correction opportunities for
model-correctable failures, but a rejected approval or user cancellation is not
automatically retried.

## Related pages

- [Commands, skills, and macros](../agent/commands-skills-and-macros.md)
- [MCP integration](../agent/mcp-integration.md)
- [Sandboxing](../safety-and-trust/sandboxing.md)
- [Complete `maap/1` reference](protocols/maap.md)
- [Normative MAAP contract](../../SPEC.md#98-mezzanine-agent-action-protocol)

## Next step

Read [Terminal compatibility](terminal-compatibility.md) for the pane surface
that carries local action input and output.
