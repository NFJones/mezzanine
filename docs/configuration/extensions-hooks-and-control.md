# Extensions, hooks, and control

## Purpose

Configure MCP servers, hooks, control access, audit records, and extension data
while keeping external capabilities visible and reviewable.

## Prerequisites

Read [Configuration overview](overview.md) and review the safety implications
of every external command, endpoint, credential reference, and hook.

## Configure integrations explicitly

`mcp_servers` contains configured Model Context Protocol integrations. Per-server
settings cover transport, enablement, tools, timeouts, and non-secret metadata.
Use `mez mcp` and `/list-mcp` to manage and inspect runtime state. Store MCP
secrets through authentication flows or environment references, not ordinary
configuration.

`hooks` configures lifecycle and command hooks. Hooks can execute or contact
external systems, so they remain subject to configuration trust, permission,
and audit requirements. `control` configures the local control endpoint;
`message_protocol` configures local agent messaging; `snapshots` controls
persisted session metadata; and `audit` controls structured security records.

Treat hook runners as distinct execution boundaries. Program hooks can invoke
external programs and receive structured event data on standard input. Shell
hooks use the event's pane shell when one is available; agent hooks are queued
through the regular agent-shell action path and wait for that shell to be
ready. A blocking hook failure or timeout stops the associated operation, while
nonblocking behavior must be selected explicitly through the hook's failure
policy. Inspect hook failures with `show-messages` and audit records rather
than assuming an event completed.

Use `extensions` only for implementation-specific extension data. Unknown
top-level keys are rejected rather than silently interpreted as configuration.

## Related pages

- [MCP integration](../agent/mcp-integration.md)
- [Audit and diagnostics](../safety-and-trust/audit-and-diagnostics.md)
- [Configuration reference](reference.md)

## Next step

Return to [Configuration overview](overview.md) or validate the final file with
`mez config validate`.
