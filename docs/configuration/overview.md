# Configuration overview

## Purpose

Safely locate, validate, layer, and change Mezzanine configuration.

## Prerequisites

Run these commands from an account that owns the Mezzanine configuration
directory. Keep credentials in [authentication](../getting-started/authentication.md),
not in configuration files.

## Create and inspect configuration

Mezzanine resolves its configuration root from `$HOME` as
`$HOME/.config/mezzanine/`; it does not consult `XDG_CONFIG_HOME`. It accepts
one primary file there: `config.toml`, `config.yaml`, `config.yml`, or
`config.json`. If none exists, starting a session creates the default TOML
configuration. If more than one exists, Mez stops with a configuration error
rather than choosing by filename precedence.
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

Use `mez config default` to print the complete code-owned baseline, including
provider catalogs and model presets that Mezzanine can materialize after
authentication. It is not the exact first-launch file. `mez config init` and
automatic first launch create a provider-free configuration, and `mez auth
login` adds only the successfully authenticated provider's connection,
profiles, and preset defaults.

`mez config set` and `mez config unset` persist supported scalar changes to the
user configuration by default; use their `--scope project` option only for a
trusted, eligible project overlay. These offline commands change the selected
file, not an already-running session; their JSON result reports
`reload_required` when the runtime must reload configuration to observe the
change. Validate after an edit; invalid configuration is rejected rather than
partially applied.

See the [CLI reference](../reference-manual/cli.md#configuration-identity-and-integrations)
for complete `mez config` command forms, options, and output behavior.

Run `mez config path` before a mutation when several supported formats might
exist or when you need to confirm the target. A default mutation updates the
selected primary file; if none exists, it creates the default `config.toml`.
For user-scoped mutations, `--file PATH` selects an existing file under the
private configuration root. With `--scope project`, it instead selects the
target overlay within a trusted project. For example:

```sh
mez config set terminal.emoji_width narrow
mez config unset terminal.emoji_width
mez config set --scope project agents.routing true
mez config validate
```

The project-scoped command requires a trusted project and writes the eligible
project overlay. Use `mez config layers` afterward to confirm which layer wins
for the setting.

`mez config validate [PATH]` validates the selected file as a primary user
configuration. It does not apply project-overlay-specific rules to an
arbitrary path. Use `mez config set --scope project` to create or update a
managed overlay, then run `mez config layers` from that project to inspect its
state and diagnostics.

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

Only one overlay format may exist in each directory. Multiple supported
overlay files in that directory are a configuration error; Mez does not select
one by filename order or merge them.
When a relevant overlay is pending and no primary client can decide trust, work
that depends on it—including agent prompts, hooks, MCP, command rules, or
provider settings—waits rather than silently using a lower-precedence
configuration. Use `mez config layers` to see pending and ignored overlays, and
use the project-trust commands in [Project trust and
instructions](../safety-and-trust/project-trust-and-instructions.md) to make
the decision.

## Schema versions and examples

The current schema is version `74`. Older primary user configurations migrate
on launch; a configuration declaring a newer schema is rejected. Existing
project overlays must declare the current schema version and are not migrated
automatically. When `mez config set --scope project` creates or updates an
eligible overlay, it writes the current version for that managed file.

The checked-in [example configuration](../examples/config.toml) is the
provider-free first-launch template for version 74. Actual generation adjusts
`permissions.approval_policy` and `permissions.sandbox` for the current
platform and fixed Bubblewrap or Seatbelt executable presence, so those values
can differ from the portable checked-in template. Presence is not capability,
and migration does not auto-enable an existing configuration. Use `mez config default` when the complete
code-owned provider and model catalog is needed for reference.

## Related pages

- [Appearance and terminal](appearance-and-terminal.md)
- [Agents, providers, and authentication](agents-providers-and-auth.md)
- [Permissions, sandbox, and trust](permissions-sandbox-and-trust.md)
- [Configuration reference](reference.md)

## Next step

Choose a focused chapter for a workflow, or use [Configuration reference](reference.md)
to look up an exact field.
