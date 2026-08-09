# CLI reference

## Purpose

Provide the command-line entry points for starting, selecting, inspecting, and
administering Mezzanine sessions and local services.

## Prerequisites

Install `mez` and use an interactive terminal when creating or attaching a
primary client.

## Invocation and global options

```text
mez [GLOBAL OPTIONS] [COMMAND [ARGUMENTS...]]
```

Global options are `--json`, `-S PATH`, and `-L NAME`; they may appear before
or after a command. `--json` selects machine-readable output. `-S` selects an
explicit control socket and `-L` selects a named socket in the Mez runtime
directory. Without a subcommand, `mez` attaches to the first session that
accepts a primary client; when none is available, it starts a new session. Use
`mez new` to always start a new session, or `mez attach` to select an existing
one.

## Session commands

| Command | Behavior |
| --- | --- |
| `mez new [--dry-run]` | Start a new background session and attach when interactive. With `--dry-run`, validate session construction instead of starting a daemon. Alias: `new-session`. |
| `mez serve` | Start a foreground session service; it does not attach a primary client unless `--attach-primary` is supplied from an interactive terminal. Alias: `daemon`. |
| `mez list` | List resumable sessions. Alias: `list-sessions`. |
| `mez attach [session-id] [--observer]` | Attach a primary client, or request read-only observer access. Alias: `attach-session`. |
| `mez detach` | Detach the current or selected client. Alias: `detach-client`. |
| `mez kill --force` | Terminate a live session through its control socket; `--force` confirms the destructive operation. Alias: `kill-session`. |
| `mez snapshot` | Manage persisted snapshots. With no subcommand it lists snapshots; see the snapshot forms below. |

Creating or attaching a primary client needs an interactive terminal. `mez
serve` can run without one. An observer request also requires an interactive
terminal and remains pending until the primary client approves it. `mez snapshot
resume <snapshot-id> --serve` restores a snapshot as a foreground daemon; add
`--attach-primary` only when the invoking terminal should attach as its primary
client. Use `mez --help` and `mez <command> --help` for the current argument
and target syntax.

## Snapshot forms

Snapshots preserve recoverable session layout state, not running processes,
terminal history, or agent conversations. Pending approvals and approval grants
do not become authority in a restored session:

| Command | Behavior |
| --- | --- |
| `mez snapshot` or `mez snapshot list` | List persisted snapshots. |
| `mez snapshot create [-n NAME]` | Create a snapshot of the live session selected by the control socket. |
| `mez snapshot inspect <snapshot-id>` | Inspect one saved snapshot. |
| `mez snapshot delete <snapshot-id>` | Delete one saved snapshot. |
| `mez snapshot resume <snapshot-id>` | Reconstruct the saved layout model without starting a daemon; add `--serve` to launch fresh panes in a foreground daemon. |
| `mez snapshot resume-latest [--session-id ID]` | Reconstruct the newest matching layout model without starting a daemon; it also accepts `--serve`. |

Both restore commands accept `--restart-command <command>` for restorable pane
processes. A live restore starts fresh processes and cannot reconnect to the
processes that existed when the snapshot was taken.

## Configuration, identity, and integrations

| Command | Subcommands and scope |
| --- | --- |
| `mez config` | `init`, `path`, `default`, `validate`, `get`, `layers`, `set`, and `unset`. `set` and `unset` write the user configuration by default; their `--scope project` option targets an eligible trusted project overlay. |
| `mez auth` | `status`, `login`, and `logout` for provider credentials and metadata. |
| `mez mcp` | `list`, `inspect`, `login`, `logout`, `status`, `add`, `remove`, `enable`, `disable`, `set`, `unset`, `tools`, and `approval` manage configured MCP servers, stored MCP credentials, tool filters, and server approval settings. |
| `mez sandbox` | Inspect, plan, enable, disable, manage presets, profiles, project trust, and Bubblewrap-home caches. `mez sandbox trust` supports `list`, `inspect PATH`, `add PATH`, `reject PATH`, and `revoke PATH`. |
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
