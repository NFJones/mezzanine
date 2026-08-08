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

Use `extensions` only for implementation-specific extension data. Unknown
top-level keys are rejected rather than silently interpreted as configuration.

## Related pages

- [MCP integration](../agent/mcp-integration.md)
- [Audit and diagnostics](../safety-and-trust/audit-and-diagnostics.md)
- [Configuration reference](reference.md)

## Next step

Return to [Configuration overview](overview.md) or validate the final file with
`mez config validate`.
