# Providers and models

## Purpose

Choose a provider and model profile, maintain a model catalog, and control
per-turn routing without assuming that configuration establishes entitlement.

## Prerequisites

Complete [authentication](../getting-started/authentication.md) and open the
active pane's agent shell. Keep credentials in `mez auth`, not configuration
or prompts.

## Choose a provider

Mez has adapters for OpenAI Responses, Anthropic Messages, DeepSeek Chat
Completions, and custom OpenAI-compatible services. Authentication and the
provider connection are separate from model selection:

| Provider | Setup |
| --- | --- |
| OpenAI | `mez auth login` for interactive sign-in; explicit device-code and API-key methods are also available. |
| Anthropic | `mez auth login --provider anthropic --api-key` for an Anthropic Console API key. |
| DeepSeek | `mez auth login --provider deepseek --api-key` for an API key. |
| Custom OpenAI-compatible service | Configure its API dialect, base URL, models, and named profiles; store any required key with `mez auth login --provider <name> --api-key`. Custom login does not create those connection records. |

Successful built-in authentication adds that provider's defaults to the primary
TOML configuration without replacing an existing default selection. YAML and
JSON configurations need manual provider/profile setup. Check `mez auth status`
and `mez config validate` separately. For a custom service, see the
[configuration guide](../configuration/agents-providers-and-auth.md) or the
[Bedrock walkthrough](aws-bedrock-openai-compatible.md).

## Select a profile or model

A **model profile** is a named combination of provider, model, reasoning,
latency preferences, capability requirements, and non-secret options. Use a
configured profile to switch providers, then inspect that provider's catalog:

```text
/model
/model anthropic-default
/model list
```

The example requires the `anthropic-default` profile to be configured. Bare
`/model` reports the current selection and configured profiles; `/model list`
lists models for the **active provider**, not every provider. A bare model ID
or alias selects within that active provider. To choose a supported reasoning
level, use `/model <model-id> <reasoning-level>` or
`/model <model-id> --reasoning <reasoning-level>`.

The default selection scope is the current pane. `/model --clear` removes that
override rather than changing configuration. Other scopes are `session`,
`window`, `agent`, and `subagent`; use `--target` when selecting for another
identity. For a child:

```text
/model --scope subagent --target <agent-id> <profile-name>
/model --scope subagent --target <agent-id> --clear
```

Replace the placeholders with verified identities and configured profile names.
Overrides resolve in subagent, agent, pane, window, session, then default order.
Clearing a child's explicit override restores its spawn-selected profile.

Model selection does not establish entitlement or silently lower configured
safety, privacy, residency, or approval characteristics. If a model is
unavailable, inspect the error and configured fallback profiles instead of
assuming an equivalent replacement was selected.

## Maintain the catalog

Configured model records under `providers.<name>.models` hold reusable IDs,
aliases, limits, reasoning levels, capabilities, and options. Profiles can
override them. Metadata precedence is profile override, configured model,
provider discovery, built-in metadata, then fallback. Omitted lists inherit;
explicit empty lists clear lower-precedence values. Option maps merge per key.
`reasoning_levels` lists supported choices; `reasoning_profile` selects one.

Choose the operation based on whether you need a temporary refresh or a durable
configuration change:

| Operation | Effect |
| --- | --- |
| `/refresh-provider-info` | Refreshes the running session's best-effort provider catalog and quota information; does not edit configuration. |
| `mez config model list PROVIDER` | Lists configured base model records. |
| `mez config model sync PROVIDER` | Previews a comparison with the raw live catalog. |
| `mez config model sync PROVIDER --apply` | Persists the validated sync plan. |
| `mez config model add PROVIDER MODEL_ID` | Adds an exact provider-facing ID when discovery is unavailable or incomplete. |
| `mez config model update PROVIDER MODEL_ID ...` | Selectively updates known metadata. |
| `mez config model remove PROVIDER MODEL_ID` | Removes an unreferenced record. |

Sync fills omitted metadata only. Explicit values, including empty lists,
remain authoritative and disagreements are reported as conflicts. Without
`--prune`, records absent from one provider response remain configured.
`--prune` proposes removals but still requires `--apply` to write; references
to affected IDs or aliases block the whole prune write. Review the preview
before applying it.

Model IDs are opaque values, even when they contain dots, slashes, or colons.
The typed commands generate safe local keys; do not splice an ID into a dotted
configuration path. Renaming or removing an ID requires updating matching
provider-default and profile references first. Use `--help` for metadata and
clear flags. These commands accept `--scope user|project` and `--file` targets.

The LM Studio example starts with an empty model table. Preview with
`mez config model sync lmstudio` and apply only verified discoveries, or add
the backend's exact ID manually. Mez does not import OpenAI built-ins into an
empty custom-provider response or infer token limits from a model name.

Configured models remain selectable when discovery omits them; unavailable
live metadata can fall back to configured records, with its source labeled.
Reload configuration after durable catalog changes, or start a new session.
An in-flight turn keeps its selected profile; a refresh affects future lookups.

## Control routing and thinking

Automatic sizing chooses an ephemeral small, medium, or large profile and
reasoning effort from workload scope and risk, not prompt length alone:

- `subagent` routing runs root work in one managed worker, then the parent's
  normal profile presents the result.
- `in-place` routing applies the selected profile to the current root turn.
- Already-spawned subagents route in place. A routed `/loop` pins its selected
  worker profile across iterations.

After the turn, the pane's ordinary selection remains unchanged. Routing may
therefore use a different execution model than the one shown by `/model`.

```text
/routing status
/routing on
/routing policy in-place
```

**Bare `/routing` toggles routing; it is not a read-only status command.** Use
`/routing off` to disable it. `/routing policy subagent` selects managed-worker
routing; `/routing policy --global in-place` persists a fallback policy for
panes without an override. `/model --routing` inspects the router model profile,
not the ordinary work model.

Use `/latency` to inspect or select a pane-local latency/cost preference and
`/thinking` to inspect native thinking on models that support it. Use
`/thinking on`, `/thinking off`, or `/thinking toggle` to change it.
Reasoning levels and thinking support are provider-specific: use current
metadata rather than copying another provider's values. Unknown DeepSeek
models disable native thinking, reasoning controls, and streaming until
metadata establishes support. A catalog refresh is not a remedy for missing
credentials, account entitlement, or quota.

## Related pages

- [Authenticate a provider](../getting-started/authentication.md)
- [Configure AWS Bedrock through its OpenAI-compatible API](aws-bedrock-openai-compatible.md)
- [Configuration](../configuration/README.md)
- [Operations and troubleshooting](../operations/README.md)
- [Normative provider selection contract](../../SPEC.md#23-provider-model-selection)

## Next step

Read [MCP integration](mcp-integration.md) when the task needs an external tool
server.
