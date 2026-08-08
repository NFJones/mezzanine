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
| `mez snapshot` | Manage persisted snapshots. With no subcommand it lists snapshots; see the snapshot forms below. |

Creating or attaching a primary client needs an interactive terminal. `mez
serve` can run without one. An observer request also requires an interactive
terminal and remains pending until the primary client approves it. `mez snapshot
resume <snapshot-id> --serve` restores a snapshot as a foreground daemon; add
`--attach-primary` only when the invoking terminal should attach as its primary
client. Use `mez --help` and `mez <command> --help` for the current argument
and target syntax.

## Snapshot forms

Snapshots preserve recoverable session layout state, not running processes. Use
the planning commands before a restore when the result needs review:

| Command | Behavior |
| --- | --- |
| `mez snapshot` or `mez snapshot list` | List persisted snapshots. |
| `mez snapshot create [-n NAME]` | Create a snapshot of the live session selected by the control socket. |
| `mez snapshot inspect <snapshot-id>` / `delete <snapshot-id>` | Inspect or delete one saved snapshot. |
| `mez snapshot resume-plan <snapshot-id>` | Show the restore plan without loading the snapshot payload. |
| `mez snapshot latest-plan [--session-id ID]` | Show the restore plan for the newest matching snapshot. |
| `mez snapshot rollback-plan <snapshot-id>` | Show whether a snapshot can serve as a rollback point. |
| `mez snapshot resume <snapshot-id>` | Restore the saved layout model; add `--serve` to launch fresh panes in a foreground daemon. |
| `mez snapshot resume-latest [--session-id ID]` | Restore the newest matching snapshot; it also accepts `--serve`. |

Both restore commands accept `--restart-command <command>` for restorable pane
processes. A live restore starts fresh processes and cannot reconnect to the
processes that existed when the snapshot was taken.

## Configuration, identity, and integrations

| Command | Subcommands and scope |
| --- | --- |
| `mez config` | `init`, `path`, `default`, `validate`, `get`, `layers`, `set`, and `unset`. `set` and `unset` write the user configuration by default; their `--scope project` option targets an eligible trusted project overlay. |
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
