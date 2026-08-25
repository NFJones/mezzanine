# Providers and models

## Purpose

Authenticate supported providers, select an appropriate model profile, and
understand per-turn routing without assuming unavailable models or entitlements.

## Prerequisites

Complete [authentication](../getting-started/authentication.md) and inspect the
active pane agent shell.

## Select models

Mezzanine uses named model profiles that combine provider, model, reasoning and
latency preferences, capability requirements, and non-secret provider options.
Use `/model list` to see the active provider's available catalog and `/model`
to select a model or supported reasoning level for the pane. When live provider
metadata is unavailable, the list can fall back to configured models and labels
that source accordingly.

Model selection does not establish an entitlement or silently lower configured
safety, privacy, residency, or approval characteristics. If a preferred model
is unavailable, inspect the error and configured fallback profiles rather than
assuming Mez selected an equivalent replacement.

## Understand routing and thinking

When automatic sizing is enabled, Mez selects an ephemeral small, medium, or
large profile and reasoning effort from the workload's scope and risk rather
than prompt length alone. For a root turn, the `subagent` routing policy runs
the work in one managed worker and returns its final presentation to the
invoking conversation; `in-place` runs the selected profile in the current
conversation. An already-spawned subagent always routes in place. After the
turn, the pane's ordinary model selection remains unchanged. A `/loop`
classifies its logical job once and pins one worker profile across its internal
iterations.

Use `/routing` to inspect automatic sizing. `/routing policy subagent` or
`/routing policy in-place` changes the current pane policy; put `--global`
before the policy value to persist the fallback for panes without an override.
Use `/latency` for a pane-local latency/cost preference and `/thinking` only
when the selected provider supports a native thinking toggle. Use
`/refresh-provider-info` before treating a stale model or quota catalog as an
entitlement failure. Authentication secrets remain in `mez auth`, never
ordinary configuration.

## Related pages

- [Authenticate a provider](../getting-started/authentication.md)
- [Configuration](../configuration/README.md)
- [Operations and troubleshooting](../operations/README.md)
- [Normative provider selection contract](../../SPEC.md#23-provider-model-selection)

## Next step

Read [MCP integration](mcp-integration.md) when the task needs an external tool
server.
