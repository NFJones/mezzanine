# Agents, providers, and authentication

## Purpose

Configure agent defaults, provider definitions, model profiles, and
authentication boundaries.

## Prerequisites

Read [Configuration overview](overview.md) and complete provider sign-in with
`mez auth` before expecting model-backed turns to work.

## Configure agent and model behavior

The `agents` table controls the default provider and model profile, routing,
compaction retention, concurrency, loop limits, and subagent limits. Provider
definitions belong under `providers.<name>`; named `model_profiles.<name>`
combine a provider, model, reasoning, latency, capability, and non-secret
provider options. Use `/model`, `/routing`, and `/thinking` for pane-scoped
runtime choices where supported.

Provider configuration describes connections and model catalogs; it does not
store authentication secrets. Use `mez auth login`, `mez auth status`, and
`mez auth logout` for credentials and account state. Do not put API keys,
tokens, or bearer credentials in `config.toml`.

## Configure identities and long-lived assistance

`subagents` defines named profiles and their narrower defaults. `personalities`
defines user-selected response styles. `memory` and `issues` configure the
local persistent stores used by the agent surfaces. Project overlays can add
allowed project-specific settings only after the project is trusted.

## Related pages

- [Providers and models](../agent/providers-and-models.md)
- [Authenticate a provider](../getting-started/authentication.md)
- [MCP integration](../agent/mcp-integration.md)
- [Configuration reference](reference.md)

## Next step

Read [Permissions, sandbox, and trust](permissions-sandbox-and-trust.md) before
granting a configured agent authority.
