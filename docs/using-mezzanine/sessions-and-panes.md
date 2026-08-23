# Sessions and panes

## Purpose

Operate resumable sessions, window groups, windows, and panes from the CLI and
the multiplexer interface.

## Prerequisites

Complete [Getting started](../getting-started/README.md) and begin in an
interactive primary client.

## Start, list, and attach

```sh
mez             # attach to an available session, or create one
mez new         # always create a new session
mez serve       # run a foreground service without a primary client
mez list         # list resumable sessions
mez attach [session-id] # attach to a resumable session
mez attach [session-id] --observer  # request read-only observer access
```

Use `-S <socket-path>` for an explicit control socket, `-L <name>` for a named
socket, and `--json` for machine-readable command output. `mez serve` starts a
session service and initial pane but does not attach a primary terminal.

Use `mez list` to find a session ID, then pass it to `mez attach <session-id>`
when more than one resumable session is available. Omitting the ID uses the
selected socket or the default attach-selection behavior.

The `mezctl/2` runtime permits up to 16 attached primaries with independent
group, window, and pane navigation, zoom, prompts, overlays, copy mode, mouse
state, and viewport. One elected layout owner controls canonical pane geometry.
Every attach receives a fresh non-resumable client ID, even when display names
match.

`mez attach --observer` requests observer access. An attached primary must
approve the request and becomes its exact view source unless another attached
source is selected atomically. Observers are read-only, receive no history from
before approval, and are revoked rather than silently transferred when their
source primary detaches.

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
| `Ctrl+A d` | Detach the invoking primary client. |

Open the Mezzanine command prompt with `Ctrl+A :` for commands such as
`new-window`, `split-window`, `select-pane`, `resize-pane`, `rename-pane`, and
`list-panes`. The prompt is parsed by Mezzanine, not by the focused pane shell.
Use `Ctrl+A ?` or `list-keys` for the effective bindings; configuration can
change them.

## Persistence and shutdown

Detaching normally keeps pane processes running. Layout, history, and agent
state persist according to their settings, but persistence never bypasses trust
or approval checks. Use `mez snapshot create` to save a layout and
`mez snapshot` to list saved snapshots. Use `mez snapshot inspect
<snapshot-id>` to inspect its metadata. `mez snapshot resume <snapshot-id>`
reconstructs the saved layout model without starting a daemon; add `--serve`
to start it as a live foreground daemon. Use `mez kill --force` or the
command-prompt `exit` to end a session and its panes.

## Related pages

- [Terminal input, copy, and history](terminal-input-copy-and-history.md)
- [Lifecycle, detach, and recovery](../operations/lifecycle-detach-and-recovery.md)
- [Manual reference](../reference-manual/README.md)

## Next step

Read [Terminal input, copy, and history](terminal-input-copy-and-history.md).
