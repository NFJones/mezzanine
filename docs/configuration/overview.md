# Configuration overview

## Purpose

Safely locate, validate, layer, and change Mezzanine configuration.

## Prerequisites

Run these commands from an account that owns the Mezzanine configuration
directory. Keep credentials in [authentication](../getting-started/authentication.md),
not in configuration files.

## Create and inspect configuration

Mezzanine accepts one primary file in `~/.config/mezzanine/`: `config.toml`,
`config.yaml`, `config.yml`, or `config.json`. If none exists, starting a
session creates the default TOML configuration. If more than one exists, Mez
stops with a configuration error rather than choosing by filename precedence.
Run `mez config init` first only when you want to create and inspect that
baseline before starting a session. It creates the default only when no primary
file exists, so it does not replace an existing configuration:

```sh
mez config init
mez config path
mez config validate
mez config get
mez config layers
```

Use `mez config default` to print the built-in baseline. `mez config set` and
`mez config unset` persist supported scalar changes to the user configuration
by default; use their `--scope project` option only for a trusted, eligible
project overlay. Validate after an edit; invalid configuration is rejected
rather than partially applied.

## Understand precedence and trust

The effective configuration layers, from lowest to highest precedence, are
built-in defaults, the primary user file, a trusted project-root overlay,
trusted overlays between that root and the pane directory, and live session
overrides. Later scalar values override earlier ones; maps merge recursively
unless their schema says otherwise, while lists normally replace earlier lists.

Project overlays live under `.mezzanine/config.{toml,yaml,yml,json}`. They stay
pending until the primary user decides whether to trust the project. Even a
trusted overlay cannot change primary-user-only execution boundaries such as
sandbox backend, scopes, network policy, approval policy, or bypass state.

Only one overlay format may be selected in each directory. Multiple supported
overlay files in that directory are a configuration error unless an explicit
format precedence is configured; Mez does not merge them by filename order.
When a relevant overlay is pending and no primary client can decide trust, work
that depends on it—including agent prompts, hooks, MCP, command rules, or
provider settings—waits rather than silently using a lower-precedence
configuration. Use `mez config layers` to see pending and ignored overlays, and
use the project-trust commands in [Project trust and
instructions](../safety-and-trust/project-trust-and-instructions.md) to make
the decision.

## Schema versions and examples

The current schema is version `54`. Older primary user configurations migrate
on launch; a configuration declaring a newer schema is rejected. Project
overlays must already declare the current schema version and are not migrated
automatically. The checked-in [example configuration](../examples/config.toml)
is generated for version 54 and is the baseline for valid default settings.

## Related pages

- [Appearance and terminal](appearance-and-terminal.md)
- [Agents, providers, and authentication](agents-providers-and-auth.md)
- [Permissions, sandbox, and trust](permissions-sandbox-and-trust.md)
- [Configuration reference](reference.md)

## Next step

Choose a focused chapter for a workflow, or use [Configuration reference](reference.md)
to look up an exact field.
