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
large profile and reasoning effort for one root or subagent turn. It evaluates
the workload's scope and risk rather than prompt length alone, then restores the
ordinary pane selection after the turn. A `/loop` uses its selected worker
profile across its internal iterations instead of rerouting each one.

Use `/routing` to inspect or change automatic sizing, `/latency` to select a
pane-local latency/cost preference, and `/thinking` only when the selected
provider supports a native thinking toggle. Use `/refresh-provider-info` to
refresh cached provider model and quota information before treating a stale
catalog as an entitlement failure. Authentication secrets remain in `mez auth`,
never ordinary configuration.

## Related pages

- [Authenticate a provider](../getting-started/authentication.md)
- [Configuration](../configuration/README.md)
- [Operations and troubleshooting](../operations/README.md)
- [Normative provider selection contract](../../SPEC.md#23-provider-model-selection)

## Next step

Read [MCP integration](mcp-integration.md) when the task needs an external tool
server.
