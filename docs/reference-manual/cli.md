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

Global options are `--json`, `-S PATH`, `-L NAME`, `--iroh-profile NAME`,
and `--iroh-invite-file PATH`; they may appear before or after a command.
`--json` selects machine-readable output. `-S` selects an explicit control
socket and `-L` selects a named socket in the Mez runtime directory. The Iroh
selectors are explicit remote targets, conflict with Unix socket selectors,
and never fall back to Unix after a remote failure. Without a subcommand, `mez`
attaches to the first local session that accepts a primary client; when none is
available, it starts a new session. Use `mez new` to always start a new session,
or `mez attach` to select an existing one.

## Session commands

| Command | Behavior |
| --- | --- |
| `mez new [--dry-run]` | Start a new background session and attach when interactive. With `--dry-run`, validate session construction instead of starting a daemon. Alias: `new-session`. |
| `mez serve` | Start a foreground session service; it does not attach a primary client unless `--attach-primary` is supplied from an interactive terminal. Alias: `daemon`. |
| `mez list` | List resumable sessions. Alias: `list-sessions`. |
| `mez attach [session-id] [--observer]` | Attach a primary client, or request read-only observer access. Alias: `attach-session`. |
| `mez detach [--client-id ID]` | Detach the current client, or the selected client when `--client-id` is supplied. Alias: `detach-client`. |
| `mez kill [session-id] --force` | Terminate the selected live session through its control socket; the optional target accepts a registered session id or creation-order index. `--force` confirms the destructive operation. Alias: `kill-session`. |
| `mez snapshot` | Manage persisted snapshots. With no subcommand it lists snapshots; see the snapshot forms below. |

Creating or attaching a primary client needs an interactive terminal. `mez
serve` can run without one. An observer request also requires an interactive
terminal and remains pending until the primary client approves it. `mez snapshot
resume <snapshot-id> --serve` restores a snapshot as a foreground daemon; add
`--attach-primary` only when the invoking terminal should attach as its primary
client. Use `mez --help` and `mez <command> --help` for the current argument
and target syntax.

## Foreground service options

`mez serve`, `mez snapshot resume --serve`, and `mez snapshot resume-latest
--serve` accept the same service options:

| Option | Behavior |
| --- | --- |
| `--message-socket PATH` | Bind the local message service at an explicit absolute path. |
| `--event-socket PATH` | Bind the local event service at an explicit absolute path. |
| `--no-aux-sockets` | Do not bind the default message and event sockets. |
| `--attach-primary` | Attach the invoking interactive terminal as the primary client. |
| `--max-control-connections N` | Limit concurrent control connections. |
| `--max-message-connections N` | Limit concurrent message connections. |
| `--max-event-connections N` | Limit concurrent event connections. |
| `--max-event-batches-per-connection N` | Limit event batches served on one event connection. |

By default, a foreground service derives separate message and event socket
paths from the selected control socket. Explicit socket paths must be absolute,
and every connection or batch limit must be greater than zero. A message or
event connection limit requires the corresponding auxiliary socket to be
enabled. Use `--no-aux-sockets` for an intentional control-only service.

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
| `mez remote` | Use authenticated local Unix control for `status`, `invite --role observer|primary [--expires SECONDS]`, `clients`, `rename CLIENT_ID LABEL`, and `revoke CLIENT_ID [--reason TEXT]`. Paired Iroh clients cannot use these administration methods. |
| `mez completion <shell>` | Generate a completion definition for `bash`, `elvish`, `fish`, `powershell`, or `zsh`. |

Direct control commands keep Unix as their default target. `--iroh-invite-file
PATH` explicitly performs first-use pairing from an owner-only, bounded JSON
invitation file, while `--iroh-profile NAME` explicitly uses a protected paired
profile. These selectors conflict with `-S` and `-L`, never fall back to Unix,
and apply to `mez attach`, `mez kill --force`, and `mez detach`. Supplying a
session argument is invalid with an explicit Iroh target because the profile or
invitation already selects the remote session.

Interactive remote attach requires a terminal and keeps one initialized Iroh
control stream open for its lifetime. A `primary` profile may attach as primary
or request observer access; an `observer` profile cannot attach as primary.
The client also negotiates one server-opened version 1 event stream. Authorized
events wake a fresh `terminal/view`; a pending observer receives no event stream
until approval, and revocation or event-stream failure ends the attach visibly.
Acceptance and preface receipt share the configured Iroh setup timeout; expiry
closes the connection and requires an explicit reattach.
Terminal resize, input, and view requests remain ordered one at a time behind
their responses. If the connection fails after terminal input may have been
sent, Mez reports that the outcome is unknown, does not reconnect or replay the
input, and requires an explicit reattach.

Create invitation files without exposing the token through shell arguments or
world-readable output. Omitting `--expires` uses
`transport.iroh.invitation_ttl_seconds`; an explicit override must be from 30
through 86,400 seconds. For example:

```console
umask 077
mez --json remote invite --role primary > mez-invite.json
mez --iroh-invite-file mez-invite.json attach
# Later, after successful pairing persisted the profile named by the invitation:
mez --iroh-profile SESSION_PROFILE attach
mez --iroh-profile SESSION_PROFILE kill --force
```

`mez version` prints version information. `mez help` and `mez <command> --help`
show the generated command contract. Human-readable output is the default;
scripts should request `--json` and handle errors explicitly.

## Related pages

- [Sessions and panes](../using-mezzanine/sessions-and-panes.md)
- [Lifecycle, detach, and recovery](../operations/lifecycle-detach-and-recovery.md)
- [Configuration overview](../configuration/overview.md)

## Next step

Use [Key bindings](key-bindings.md) for in-session interactive controls.
