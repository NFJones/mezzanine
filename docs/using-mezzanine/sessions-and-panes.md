# Sessions and panes

## Purpose

Operate resumable sessions, window groups, windows, and panes from the CLI and
the multiplexer interface.

## Prerequisites

Complete [Getting started](../getting-started/README.md) and begin in an
interactive primary client.

## Start, list, and attach

```sh
mez             # create or attach according to configuration
mez new         # create a new session
mez serve       # run a foreground service without a primary client
mez list         # list resumable sessions
mez attach       # attach to a resumable session
```

Use `-S <socket-path>` for an explicit control socket, `-L <name>` for a named
socket, and `--json` for machine-readable command output. `mez serve` starts a
session service and initial pane but does not attach a primary terminal.

Only one attached client can be primary at a time. An observer must request
access and the primary client must approve it. Observers are read-only and do
not receive history from before approval.

## Work with windows and panes

The default prefix is `Ctrl+A`.

| Key | Result |
| --- | --- |
| `Ctrl+A c` | Create a window. |
| `Ctrl+A %` | Split the active pane vertically. |
| `Ctrl+A "` | Split the active pane horizontally. |
| `Ctrl+A` then an arrow key | Focus an adjacent pane. |
| `Ctrl+A n` / `Ctrl+A p` | Select the next or previous window. |
| `Ctrl+A C` | Create a window group. |
| `Ctrl+A (` / `Ctrl+A )` | Select the previous or next group. |
| `Ctrl+A d` | Detach the primary client. |

Open the Mezzanine command prompt with `Ctrl+A :` for commands such as
`new-window`, `split-window`, `select-pane`, `resize-pane`, `rename-pane`, and
`list-panes`. The prompt is parsed by Mezzanine, not by the focused pane shell.
Use `Ctrl+A ?` or `list-keys` for the effective bindings; configuration can
change them.

## Persistence and shutdown

Detaching normally keeps pane processes running. Layout, history, and agent
state persist according to their settings, but persistence never bypasses trust
or approval checks. Use `mez snapshot create` to save a layout and
`mez snapshot` to list saved snapshots; use `mez kill-session --force` or the
command-prompt `exit` to end a session and its panes.

## Related pages

- [Terminal input, copy, and history](terminal-input-copy-and-history.md)
- [Lifecycle and recovery](../operations/README.md)
- [Manual reference](../reference-manual/README.md)

## Next step

Read [Terminal input, copy, and history](terminal-input-copy-and-history.md).
