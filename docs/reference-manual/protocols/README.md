# Protocol reference

## Purpose

Provide implementer reference for Mezzanine's versioned local protocols. These
pages describe wire contracts, authorization boundaries, and lifecycle rules.
[SPEC.md](../../../SPEC.md) remains the normative compatibility contract when
this manual and the specification differ.

## Prerequisites

Use the task-oriented manual chapters before implementing an integration. Know
which local service the client needs to reach, and treat the capabilities
advertised by that live service as authoritative over this summary.

## Protocol map

| Protocol | Audience | Responsibility |
| --- | --- | --- |
| [`mezctl/2`](control-json-rpc.md) | Attach clients, automation, and alternative frontends | JSON-RPC control, terminal rendering and input, session state, and events. |
| [`maap/1`](maap.md) | Agent providers and harness integrations | Agent action proposals, approval, execution, and results. |
| [`mmp/1`](mmp.md) | Local agents and coordination services | Agent discovery, local messaging, presence, and task status. |

[Common conventions](common-conventions.md) defines terminology shared by these
pages. The protocols are separate services: use `mezctl/2` for multiplexer
control, `maap/1` for model-to-runtime actions, and `mmp/1` for agent messages.

## Compatibility

Each protocol carries a versioned identity. Clients must negotiate or validate
the version before relying on optional capabilities, methods, actions, events,
or extensions. IDs are opaque; applications must not infer meaning from their
spelling. Unknown extension fields must be ignored where the protocol permits.

The advertised capability surface is authoritative for a live connection or
agent turn. Do not assume a documented method or action is available to every
role, transport, configuration, or policy state.

## Related pages

- [Agent actions](../agent-actions.md) for a user-facing MAAP overview
- [MCP integration](../../agent/mcp-integration.md) for configured external
  integrations
- [Terminal compatibility](../terminal-compatibility.md) for terminal behavior

## Next step

Choose [`mezctl/2`](control-json-rpc.md) to build a client, [`maap/1`](maap.md)
to integrate an agent provider, or [`mmp/1`](mmp.md) to coordinate local agents.
