# Configuration overview

## Purpose

Safely locate, validate, layer, and change Mezzanine configuration.

## Prerequisites

Run these commands from an account that owns the Mezzanine configuration
directory. Keep credentials in [authentication](../getting-started/authentication.md),
not in configuration files.

## Create and inspect configuration

Mezzanine selects one primary file from `~/.config/mezzanine/`: `config.toml`,
`config.yaml`, `config.yml`, or `config.json`. Starting a session creates the
default TOML configuration when none exists. Run `mez config init` first only
when you want to create and inspect that baseline before starting a session:

```sh
mez config init
mez config path
mez config validate
mez config get
mez config layers
```

Use `mez config default` to print the built-in baseline. `mez config set` and
`mez config unset` persist supported scalar changes. Validate after an edit;
invalid configuration is rejected rather than partially applied.

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

## Schema versions and examples

The current schema is version `54`. Older primary configurations migrate on
launch; a configuration declaring a newer schema is rejected. The checked-in
[example configuration](../examples/config.toml) is generated for version 54
and is the baseline for valid default settings.

## Related pages

- [Appearance and terminal](appearance-and-terminal.md)
- [Agents, providers, and authentication](agents-providers-and-auth.md)
- [Permissions, sandbox, and trust](permissions-sandbox-and-trust.md)
- [Configuration reference](reference.md)

## Next step

Choose a focused chapter for a workflow, or use [Configuration reference](reference.md)
to look up an exact field.
