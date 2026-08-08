# CLI reference

## Purpose

Provide the command-line entry points for starting, selecting, inspecting, and
administering Mezzanine sessions and local services.

## Prerequisites

Install `mez` and use an interactive terminal when creating or attaching a
primary client.

## Invocation and global options

```text
mez [--json] [-S PATH] [-L NAME] [<command> [arguments]]
```

`--json` selects machine-readable output. `-S` selects an explicit control
socket and `-L` selects a named socket in the Mez runtime directory. Without a
subcommand, `mez` creates or attaches according to the configured default
session behavior.

## Session commands

| Command | Behavior |
| --- | --- |
| `mez new` | Start a new background session and attach when interactive. Alias: `new-session`. |
| `mez serve` | Start a foreground session service without attaching a primary client. Alias: `daemon`. |
| `mez list` | List resumable sessions. Alias: `list-sessions`. |
| `mez attach [session-id] [--observer]` | Attach a primary client, or request read-only observer access with `--observer`. Alias: `attach-session`. |
| `mez detach` | Detach the current or selected client. Alias: `detach-client`. |
| `mez kill-session --force` | Terminate a live session through its control socket; `--force` confirms the destructive operation. |
| `mez snapshot` | List, create, inspect, delete, plan a resume with `resume-plan`, or resume persisted snapshots. |

Creating or attaching a primary client needs an interactive terminal. `mez
serve` can run without one. An observer request also requires an interactive
terminal and remains pending until the primary client approves it. Use `mez
--help` and `mez <command> --help` for the current argument and target syntax.

## Configuration, identity, and integrations

| Command | Subcommands and scope |
| --- | --- |
| `mez config` | `init`, `path`, `default`, `validate`, `get`, `layers`, `set`, and `unset` for user configuration. |
| `mez auth` | `status`, `login`, and `logout` for provider credentials and metadata. |
| `mez mcp` | List, inspect, authenticate, add, remove, enable, disable, and configure MCP servers and tools. |
| `mez sandbox` | Inspect, plan, enable, disable, manage presets/profiles, project trust, and Bubblewrap-home caches. |
| `mez issue` | Add, show, update, query, and delete local project issues. |
| `mez memory` | List, inspect, add, edit, delete, archive, mark stale, restore, record use or confirmation, supersede, prune, export, and search persistent memory records. |

`mez version` prints version information. `mez help` and `mez <command> --help`
show the generated command contract. Human-readable output is the default;
scripts should request `--json` and handle errors explicitly.

## Related pages

- [Sessions and panes](../using-mezzanine/sessions-and-panes.md)
- [Lifecycle, detach, and recovery](../operations/lifecycle-detach-and-recovery.md)
- [Configuration overview](../configuration/overview.md)

## Next step

Use [Key bindings](key-bindings.md) for in-session interactive controls.
