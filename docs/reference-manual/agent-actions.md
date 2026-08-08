# Agent actions

## Purpose

Describe the user-visible `maap/1` action model, its action families, and the
review and result semantics that govern agent execution.

## Prerequisites

Read [Agent overview](../agent/overview.md) and [Approvals and review](../safety-and-trust/approvals-and-review.md).

## Action batch model

An agent response is a validated `maap/1` batch with a concise rationale and
one or more actions. Mezzanine assigns turn and action identities, validates
the active action surface, independently classifies effects, and records a
result for every accepted, rejected, blocked, denied, executed, or interrupted
action. Model-provided effect claims and bookkeeping identities are not
authoritative.

`say` presents display-only text as `progress`, `final`, or `blocked`; text
that looks like a command or patch does not execute. Action results are bounded
evidence for a later continuation, while credentials, hidden policy, and raw
terminal state remain outside ordinary model context.

## Action families

| Action | Use | Important boundary |
| --- | --- | --- |
| `say` | Present progress, completion, or a blocker to the user. | It is display-only and cannot execute text that resembles a command or patch. |
| `request_capability` | Ask the controller to expose a coarse action family for this turn. | It is not a user permission prompt. |
| `shell_command` | Pane-shell inspection, commands, validation, and filesystem operations. | Runs through the pane shell and can require approval. |
| `apply_patch` | Semantic file-content add, update, move, or delete using `*** Begin Patch` format. | It is a MAAP action, never a shell executable. |
| `web_search`, `fetch_url` | User-requested current web search or HTTP(S) retrieval. | They are runtime network actions, not local-path readers. |
| `send_message`, `spawn_agent` | Local coordination and pane-backed delegation. | Scope and policy inherit from the parent. |
| `config_change` | Supported scalar live configuration mutation. | Execution-boundary settings remain direct-user-only. |
| `mcp_call` | Call a currently available configured MCP tool. | External capability and approval policy still apply. |
| `memory_search`, `memory_store` | Retrieve or retain runtime-owned durable memory when enabled. | Records must be safe, durable, and non-secret. |
| `issue_add`, `issue_update`, `issue_query`, `issue_delete` | Manage runtime-owned local issues for the active project. | Issue records remain subject to the active action surface and project-store rules. |

The controller exposes only actions available to the current turn. An absent
action family should be requested through `request_capability`, not simulated
with prose or a different local mechanism.

## Local mutation and recovery

Use `shell_command` for shell-visible inspection and `apply_patch` for ordinary
file-content changes. Patch paths are normally relative to the pane working
directory; traversal is rejected. A patch failure is evidence, not success:
inspect current file context and issue a smaller fresh patch rather than replay
the same stale hunk. Shell commands report pane-shell transport, bounded output,
exit, timeout, and truncation data.

Blocked actions wait for a primary-client decision; observers cannot decide
them. Denied, timed-out, cancelled, and policy-forbidden actions remain in the
result history. Mezzanine can provide bounded correction opportunities for
model-correctable failures, but a rejected approval or user cancellation is not
automatically retried.

## Related pages

- [Commands, skills, and macros](../agent/commands-skills-and-macros.md)
- [MCP integration](../agent/mcp-integration.md)
- [Sandboxing](../safety-and-trust/sandboxing.md)
- [Normative MAAP contract](../../SPEC.md#98-mezzanine-agent-action-protocol)

## Next step

Read [Terminal compatibility](terminal-compatibility.md) for the pane surface
that carries local action input and output.
