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
in conversation chronology. Call only a server and tool advertised by the
current action surface and retrieved contract; do not infer tools or argument
shapes from a server name. Unknown, disabled, ambiguous, or unavailable
identifiers expose no substitute tools.

Configured always-exposed servers contribute only compact directory records to
the conversation. Directory changes append an authoritative transition; older
records remain causal history. Search results and explicit references remain
referencable after restart or resume. Retrieved tool contracts are cleared by
compaction, so retrieve the selected server again before calling one of its
tools. The live MCP registry remains authoritative for execution: it
revalidates the selected server, tool, availability, and arguments immediately
before a call runs, against the currently selected tool schema and the schema
generation the approval was bound to.

MCP calls are external actions. A tool that reads or changes local files,
reaches the network, accesses credentials, or executes processes requires
approval unless the active policy explicitly permits that external capability,
and the call is audited. An MCP server can operate outside the pane shell, so
treat its declared capabilities as a distinct boundary rather than assuming a
shell sandbox contains it.

## Tool schema validation, limits, and unavailable tools

Mez validates tool arguments itself before transport dispatch. The supported
dialect is JSON Schema 2020-12. A schema that declares another `$schema` is
unsupported and leaves its tool unavailable rather than switching assertion
semantics. `enum`, `const`, `additionalProperties`, `unevaluatedProperties`,
`unevaluatedItems`, nested `required`, array and object bounds, string and
numeric bounds, `pattern`, type unions, and `allOf`/`anyOf`/`oneOf`/`not` are
enforced. `format` and content keywords stay annotations, so an approximate email
address or encoded body never fails an otherwise valid call. `title`,
`description`, `examples`, `default`, `$comment`, and extension keywords such as
`x-*` are harmless metadata: their values are data rather than schema positions,
so a `$ref`- or `$schema`-shaped string inside them neither withdraws the tool
nor counts against the reference budget, and screening descends only through
applicator and assertion subtrees.

References are never fetched. Remote, file, relative, recursive, and oversized
references are rejected during screening without any network or filesystem
access; only bounded same-document JSON Pointer `$ref`/`$dynamicRef` fragments
(`#` or a `/`-prefixed pointer such as `#/$defs/kind`) are resolved, and only from
the document itself. Plain-name anchors such as `#name` are rejected rather than
resolved.

Validation work is bounded on both sides:

| Bound | Default |
| --- | --- |
| schema bytes | 256 KiB |
| schema nesting depth / nodes (compilation work) | 32 / 8192 |
| references / bytes per reference | 128 / 512 |
| argument bytes | 1 MiB |
| argument nesting depth / nodes (validation work) | 32 / 8192 |
| regex backtracking steps / compiled regex bytes | 100000 / 1 MiB |
| cached compiled schemas per registry | 64 |

Admission and validation diagnostics carry only a bounded category, a bounded
keyword, and a bounded JSON pointer. They never include argument values, schema
text, or unbounded server-provided key text, so they are safe to show to the
model and to record in audit entries.

Compiled schemas are cached under a stable generation derived from tool identity
and exact schema bytes as a full SHA-256 digest over a length-prefixed encoding of
the server, tool, and schema bytes, evicted under the table's capacity, and
invalidated whenever MCP metadata refreshes. A tool whose schema is invalid,
unsupported, or over budget becomes unavailable with a bounded reason while its
server and sibling tools stay usable; Mez never substitutes a fabricated
zero-argument tool for a rejected schema.

An approval binds to the schema generation that validated the approved
arguments. The comparison happens before any configured pre-MCP hook runs and
before any transport is leased. If the selected schema changes before dispatch,
only that unexecuted call is settled with a bounded `mcp_schema_changed` failure
and no hook or transport runs; already successful siblings in the same batch are
neither replayed nor re-run. An approved call whose approval recorded no schema
generation at all, because its approved arguments could not be re-planned against
the then-selected schema, is settled unexecuted as a bounded `mcp_schema_unbound`
failure instead of being dispatched unchecked. A metadata refresh never creates,
widens, or renews approval.

Argument mistakes are repairable: they fail as bounded invalid-argument errors
before any dispatch and stay inside the turn's existing bounded repair budget.
An invalid server schema is an operator problem instead, so Mez withdraws the
tool rather than inviting repeated model repair. Ambiguous transport outcomes
are not retried, and denial or cancellation stays final.

## Related pages

- [Approvals and review](../safety-and-trust/approvals-and-review.md)
- [Sandboxing](../safety-and-trust/sandboxing.md)
- [Configuration](../configuration/README.md)
- [Normative MCP contract](../../SPEC.md#14-model-context-protocol-integration)

## Next step

Return to [the agent section](README.md) or use [Operations and
troubleshooting](../operations/README.md) when a provider or integration is
unavailable.
