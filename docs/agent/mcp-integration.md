# MCP integration

## Purpose

Connect explicit Model Context Protocol (MCP) integrations and use their tools
without assuming that a configured server is available or contained by the
pane's shell sandbox.

## Prerequisites

Choose the external service, its supported transport, and the tools needed for
your task. Review its filesystem, process, network, and credential implications
before enabling it. Server software and accounts are not supplied by Mez.

## Configure a server

Mez supports local **stdio** subprocesses and **streamable HTTP** endpoints.
Use `mez mcp` for persistent server management. Replace the example paths and
URLs with those supplied by your server:

```console
mez mcp add local-tools --command /absolute/path/to/mcp-server --disabled
mez mcp inspect local-tools
mez mcp enable local-tools
mez config validate
```

Pass each stdio argument with a separate `--arg`. A server gets a usable `PATH`,
but other environment variables must be explicitly configured or listed in
`env_vars`. Pass-through values and HTTP `bearer_token_env` values come from the
Mez server process environment, not the interactive pane shell. Supply required
variables securely before starting that server; exporting one later in a pane
does not update the server environment. `permissions.env_whitelist` controls
shell workloads, not MCP servers.

For an HTTP integration:

```console
mez mcp add remote-tools --url https://example.com/mcp --disabled
mez mcp login remote-tools
mez mcp status remote-tools
mez mcp enable remote-tools
mez config validate
```

The interactive login starts browser OAuth when supported by the server.
`mez mcp status <id>` reports authentication separately from tool availability.
Use environment references or the separate MCP authentication store for secrets,
not ordinary configuration. A configured `bearer_token_env` takes precedence
over stored credentials. Login refuses such a server unless you pass
`--replace-env-token`; that flag permits storing credentials but does not remove
the configuration reference. To use the stored credential instead, remove the
reference with `mez config unset mcp_servers.<id>.bearer_token_env` and reload
configuration. Changing an HTTP URL can make stored credentials stale and
require login again.

If the integration supports only a static bearer token, prefer a securely
supplied `bearer_token_env`. The current `mez mcp login --token` option accepts
the secret as a command argument, not through a hidden prompt or token file;
process listings, shell history, and command logs may expose it. A configured
but missing bearer-token variable fails authentication rather than falling back
to stored credentials.

These CLI commands change persistent configuration; reload the running session's
configuration or start a new session before relying on the changes. `mez mcp
list` and `inspect` show configuration, whereas `/list-mcp` in the agent shell
shows live availability, tools, and failure reasons.

### Limit tools and authority

```console
mez mcp tools enable remote-tools read-item list-items
mez mcp tools disable remote-tools delete-item
mez mcp approval set remote-tools prompt
```

Replace the sample tool names with exact names your server advertises. These
commands **replace** their respective allow or deny lists; they do not append
one entry. Disabled tools win over enabled tools. `mez mcp tools reset <id>`
clears both filters. Server approval values are `inherit`, `prompt`, `allow`,
and `deny`; they do not remove other applicable permission checks.

Configuration under `mcp_servers.<id>` also supports enablement, startup and
tool timeouts, per-tool approval, and external-capability declarations. Declare
filesystem mutation, process execution, and credential access outside the pane
shell accurately. Add non-secret purpose and usage guidance to help the agent
choose the integration. See the [configuration reference](../configuration/reference.md)
for those fields.

## Check live availability

Open the agent shell and run `/list-mcp`. Enabled servers initialize at session
startup; explicit listing or retry can also discover pending integrations.
In native shell mode, preparing an ordinary provider turn does not implicitly
start a pending stdio server. Listing is therefore an important check after
configuration changes, not just a catalog display.

A server that cannot start, authenticate, connect, or complete its handshake is
marked unavailable and session-blacklisted. Other agent work can continue with
a reduced tool catalog. Fix the reported cause, then start a new session or use
the `mcp/retry` control method to retry the live server. Re-enabling an already
enabled server and reloading unchanged configuration does not clear its
blacklist. To recover through configuration changes, disable the server and
reload the running session, then enable it and reload again; both live changes
must take effect. See [configuration changes](../configuration/overview.md)
for the distinction between disk edits and live reload. There is no
`mez mcp retry` CLI subcommand;
passive model metadata lookup does not start a transport or clear a blacklist.

## Use an integration for a task

Mention the configured server at the start of your task, for example:

```text
@remote-tools Read the release checklist and summarize incomplete items. Do not change it.
```

Use the canonical configured server ID, not an inferred service name. The
reference lets the agent retrieve that server's safe metadata; it does not
authenticate the server, grant approval, or expose tools by itself.

The agent uses `mcp_server_search` when it needs to find a configured server,
`mcp_server_get` to retrieve the selected server's complete tool contracts,
and `mcp_call` to invoke an advertised tool. Search and retrieval are passive
metadata operations. Unknown, disabled, ambiguous, or unavailable servers do
not acquire substitute tools.

Explicit references and search results survive restart or resume. Compaction
clears retrieved tool contracts, so the agent must retrieve the server again
before calling tools. Even an earlier successful retrieval does not guarantee
that the same tool is currently enabled: the live registry checks availability
and arguments immediately before execution.

MCP calls are permission-gated and audited external actions. Calls that access
files, execute processes, use credentials, or reach the network need approval
unless the active policy explicitly permits that capability. A server may run
outside the pane shell; the shell sandbox is not evidence that the server is
contained. Server instructions and tool output remain untrusted task data.

## Troubleshoot tool failures

| Symptom | What to check |
| --- | --- |
| Server unavailable or blacklisted | `/list-mcp` failure reason, executable path, required environment, endpoint, authentication, and startup timeout. Fix the cause before explicit retry. |
| Authentication succeeds but tools are absent | Server enablement, tool filters, discovery status, and each tool's schema diagnostic. |
| One tool is unavailable but others work | Its advertised input schema may be invalid, unsupported, or over the validation budget. Correct the server metadata; repeated argument guesses will not repair it. |
| Invalid arguments | Required fields, value types, and constraints in the retrieved contract. Validation rejects mistakes before dispatch. |
| `mcp_schema_changed` or `mcp_schema_unbound` | That call was not dispatched. Retrieve current metadata and submit a new, reviewable call; refresh does not renew old approval. |
| Timeout or uncertain transport outcome | Inspect the external service before retrying a mutation. An uncertain result is not proof that no side effect occurred. |

Mez supports common JSON Schema dialects from Draft-04 through 2020-12, with
bounded schema and argument validation. It does not fetch remote or file schema
references. Unsupported metadata withdraws only the affected tool, not every
tool on the server. For exact dialects and limits, consult the
[normative MCP contract](../../SPEC.md#14-model-context-protocol-integration)
rather than treating this guide as a server-implementation reference.

## Related pages

- [Approvals and review](../safety-and-trust/approvals-and-review.md)
- [Sandboxing](../safety-and-trust/sandboxing.md)
- [Configuration](../configuration/README.md)
- [CLI reference](../reference-manual/cli.md)

## Next step

Return to [the agent section](README.md) or use [Operations and
troubleshooting](../operations/README.md) when a provider or integration is
unavailable.
