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
Provider-scoped base records under `providers.<name>.models.<entry>` define
reusable model identity, aliases, context and input/output limits, supported
reasoning levels, capabilities, and non-secret model options. Profiles may
override those values for one usage policy without duplicating the base facts.
The precedence is profile override, configured model, provider discovery,
built-in metadata, then fallback. Configured lists replace lower lists; option
maps merge per key. A model record's `reasoning_levels` lists supported choices,
while a profile's `reasoning_profile` selects one choice.
The built-in `deepseek-flash` record for DeepSeek-V4.1-Flash, the retained
`deepseek-v4-pro` record, and the retired `deepseek-v4-flash` name all advertise
provider-facing reasoning levels `low`, `high`, and `max`, plus
`native_thinking`, `function_tools`, `forced_tool_choice`, `streaming`, and
`max_output_tokens`. The same declarations are retained when the runtime falls
back to its code-defined DeepSeek catalog. Omitting either list inherits
lower-precedence metadata; configuring an empty list deliberately clears it.
Use `mez config model list PROVIDER` to inspect these configured base records.
`mez config model add`, `update`, and `remove` address records by the opaque
provider-facing id and generate path-safe local keys automatically. Updates are
selective; explicit clear flags remove optional scalar metadata or provider
options, while an empty list value clears list metadata. Renaming or removing
an id is refused until matching provider-default and model-profile references
are updated. All model commands accept the normal offline `--scope` and
`--file` target selectors.

Use `mez config model sync PROVIDER` to compare explicit records with the raw
live catalog. Sync is a preview unless `--apply` is present. Add `--prune` to
plan configured-only removals; `--prune` still does not write without
`--apply`, and referenced canonical ids or aliases block the whole prune write.
Without `--prune`, models absent from one provider response remain configured.
Observed display names, reasoning levels, token limits, and capabilities fill
only omitted fields. Explicit values—including empty lists—remain authoritative
and are reported as conflicts when they differ from the observation. Project
sync can use an inherited user-level provider connection while persisting only
minimal model overrides in the selected project file.
Use `/model list` to see the active provider's available catalog and `/model`
to select a model or supported reasoning level for the pane. When live provider
metadata is unavailable, the list can fall back to configured models and labels
that source accordingly.

For a compatible custom provider with no configured or discoverable models,
the typed list output includes an `add` command and live-catalog guidance rather
than presenting an unexplained empty result.

The generated LM Studio example intentionally starts with an empty structured
model table. `/refresh-provider-info` discovers models only for the running
session. Use `mez config model sync lmstudio` to preview raw discoveries and
rerun with `--apply` to persist them, or use
`mez config model add lmstudio MODEL_ID` when the backend does not support a
compatible catalog endpoint. Sync never imports OpenAI built-ins into an empty
custom-provider response and never invents token limits from a model name.

Configured models remain available when discovery omits them, and discovered
metadata fills only configured gaps. Aliases select the canonical model id;
unlisted custom profile models remain valid. `/refresh-provider-info`
rematerializes future profile lookups, but an in-flight turn keeps its cloned
profile. Its cache and fallback effects are ephemeral and never edit config.
Configuration reload after a durable sync similarly rebases retained generated
selections against the new configured model base.

Model selection does not establish an entitlement or silently lower configured
safety, privacy, residency, or approval characteristics. If a preferred model
is unavailable, inspect the error and configured fallback profiles rather than
assuming Mez selected an equivalent replacement.

## Understand routing and thinking

When automatic sizing is enabled, Mez selects an ephemeral small, medium, or
large profile and reasoning effort from the workload's scope and risk rather
than prompt length alone. For a root turn, the `subagent` routing policy runs
the selected profile in one managed worker. The worker's exact output and a
bounded context summary return to the parent, whose normal profile produces
the final presentation. The `in-place` policy applies the selected profile
directly to the current root turn. An already-spawned subagent always routes in
place. After the turn, the pane's ordinary model selection remains unchanged.
A `/loop` classifies its logical job once and pins one worker profile across
its internal iterations.

Use `/routing` to inspect automatic sizing. `/routing policy subagent` or
`/routing policy in-place` changes the current pane policy; put `--global`
before the policy value to persist the fallback for panes without an override.
Use `/latency` for a pane-local latency/cost preference and `/thinking` only
when the selected model supports a native thinking toggle. Unknown DeepSeek
models conservatively keep MAAP function tools, forced tool choice, and output
token bounds while disabling native thinking, reasoning controls, and
streaming until metadata establishes support. Use
`/refresh-provider-info` before treating a stale model or quota catalog as an
entitlement failure. Authentication secrets remain in `mez auth`, never
ordinary configuration.

## Related pages

- [Authenticate a provider](../getting-started/authentication.md)
- [Configure AWS Bedrock through its OpenAI-compatible API](aws-bedrock-openai-compatible.md)
- [Configuration](../configuration/README.md)
- [Operations and troubleshooting](../operations/README.md)
- [Normative provider selection contract](../../SPEC.md#23-provider-model-selection)

## Next step

Read [MCP integration](mcp-integration.md) when the task needs an external tool
server.
