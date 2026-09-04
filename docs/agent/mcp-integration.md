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
start, authenticate, or connect is marked unavailable and session-blacklisted,
so ordinary agent work continues with a reduced tool catalog. `/list-mcp`
shows the failure reason and whether retry is available; an explicit retry or
re-enable can attempt discovery again.

## Discover and use tools for one task

Use `mcp_server_search` when the relevant configured server is not already
known, or mention `@<server-id>` to create a durable reference to a known
server. Use `mcp_server_get` for a referencable server before `mcp_call`; it
returns the complete safe tool and argument contract and records that retrieval
in conversation chronology. Unknown, disabled, ambiguous, or unavailable
identifiers expose no substitute tools.

Configured always-exposed servers contribute only compact directory records to
the conversation. Directory changes append an authoritative transition; older
records remain causal history. Search results and explicit references remain
referencable after restart or resume. Retrieved tool contracts are cleared by
compaction, so retrieve the selected server again before calling one of its
tools. The live MCP registry remains authoritative for execution: it
revalidates the selected server, tool, availability, and arguments immediately
before a call runs.

MCP calls are external actions. A tool that reads or changes local files,
reaches the network, accesses credentials, or executes processes requires
approval unless the active policy explicitly permits that external capability,
and the call is audited. An MCP server can operate outside the pane shell, so
treat its declared capabilities as a distinct boundary rather than assuming a
shell sandbox contains it.

## Related pages

- [Approvals and review](../safety-and-trust/approvals-and-review.md)
- [Sandboxing](../safety-and-trust/sandboxing.md)
- [Configuration](../configuration/README.md)
- [Normative MCP contract](../../SPEC.md#14-model-context-protocol-integration)

## Next step

Return to [the agent section](README.md) or use [Operations and
troubleshooting](../operations/README.md) when a provider or integration is
unavailable.
