# MCP integration

## Purpose

Configure and use Model Context Protocol servers as explicit, permission-gated
external integrations.

## Prerequisites

Know the external service to connect and review its credentials, filesystem,
process, and network implications.

## Configure and inspect a server

MCP servers are configured under `mcp_servers`. Mez supports stdio servers and
streamable HTTP servers, with per-server enablement, tool filtering, timeouts,
authentication, and approval settings. Use `mez mcp` for server management and
`/list-mcp` to inspect enabled, unavailable, or session-blacklisted servers and
their tools.

Keep tokens and static bearer credentials in the MCP authentication flow or
environment references, not in ordinary configuration. A server that cannot
start, authenticate, or connect is unavailable for the session; it degrades the
tool catalog rather than preventing ordinary agent work.

## Expose tools for one task

Start a prompt with `@<server-id>` to resolve and expose that server's callable
tools and argument contracts for the current turn. Unknown, disabled,
ambiguous, or unavailable identifiers expose no substitute tools. Configured
always-exposed servers use the same validation rules.

MCP calls are external actions. A tool that reaches the network, accesses
credentials, executes processes, or changes files can require approval and is
audited. An MCP server can operate outside the pane shell, so treat its declared
capabilities as a distinct boundary rather than assuming Bubblewrap contains it.

## Related pages

- [Approvals and review](../safety-and-trust/approvals-and-review.md)
- [Sandboxing](../safety-and-trust/sandboxing.md)
- [Configuration](../configuration/README.md)
- [Normative MCP contract](../../SPEC.md#14-model-context-protocol-integration)

## Next step

Return to [the agent section](README.md) or use [Operations and troubleshooting](../operations/README.md)
when a provider or integration is unavailable.
