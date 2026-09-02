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
`--iroh-invite-file PATH`, and `--save-as NAME`; they may appear before or after
a command. `--save-as` requires an invitation target and selects the
client-local alias used for later reconnects.
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
| `mez new [--dry-run] [--name NAME]` | Start a new background session and attach when interactive. `--name` assigns a session name. With `--dry-run`, validate session construction instead of starting a daemon. Alias: `new-session`. |
| `mez serve` | Start a foreground session service; it does not attach a primary client unless `--attach-primary` is supplied from an interactive terminal. Alias: `daemon`. |
| `mez list [--all]` | List resumable sessions known to the local client. With the persistent local host, `--all` adds visible remote durable leases to the same scope-tagged aggregate. Alias: `list-sessions`. |
| `mez attach [SESSION_ID] [--observer\|--default] [--x11\|--x11-trusted] [--x11-takeover]` | Attach a primary client, request read-only observer access, select an existing host default without creating, or request X11 forwarding for an authenticated Iroh primary. `--default` conflicts with an explicit target; X11 options are described below. Alias: `attach-session`. |
| `mez detach [--client-id ID]` | Detach the selected client. From an interactive attachment, use `Ctrl+A d` to detach that invoking client; a separate administrative invocation needs the target client ID. Alias: `detach-client`. |
| `mez kill [session-id] --force` | Terminate the selected live session through its control socket; the optional target accepts a registered session id or creation-order index. `--force` confirms the destructive operation. Alias: `kill-session`. |
| `mez snapshot` | Manage persisted snapshots. With no subcommand it lists snapshots; see the snapshot forms below. |
| `mez host` | Serve, inspect, stop, or reconcile the persistent multi-session host. |
| `mez lease` | Inspect, checkpoint, recover, release, revoke, or garbage-collect persistent-host leases through local administration. |

Creating or attaching a primary client needs an interactive terminal. `mez
serve` can run without one. Observer attachment also requires an interactive
terminal and immediately creates a read-only client bound to the current layout
owner. The runtime implements `mezctl/2` and accepts up to 16 independent attached primaries. Each
has caller-local navigation and presentation; one elected layout owner controls
canonical PTY geometry. `mez snapshot resume <snapshot-id> --serve`
restores a snapshot as a foreground daemon; add `--attach-primary` only when
the invoking terminal should attach as a primary client. Use `mez --help` and
`mez <command> --help` for the current argument and target syntax.

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

The built-in Unix attach client discovers the event service at the standard
path derived from the control socket; it has no separate event-socket selector.
A nonstandard explicit `--event-socket` path is therefore useful only to a
custom client configured for that path; the built-in `mez attach` client will
not discover it. Without a reachable event socket, control requests still work,
but an idle attachment does not receive redraw wakeups and may not fetch a
fresh view until input, focus, mouse, or resize activity occurs.

## Snapshot forms

Snapshot payload version 5 preserves recoverable shared topology, canonical
geometry, and client-independent landing navigation, not running processes, live client
identities, layout ownership, transient presentation, terminal history, or
agent conversations. Pending approvals and approval grants do not become
authority in a restored session:

| Command | Behavior |
| --- | --- |
| `mez snapshot` or `mez snapshot list` | List persisted snapshots. |
| `mez snapshot create [-n NAME]` | Create a snapshot of the live session selected by the control socket. |
| `mez snapshot inspect <snapshot-id>` | Inspect one saved snapshot. |
| `mez snapshot delete <snapshot-id>` | Delete one saved snapshot. |
| `mez snapshot resume <snapshot-id>` | Reconstruct the saved layout model without starting a daemon; add `--serve` to launch fresh panes in a foreground daemon. |
| `mez snapshot resume-latest [--session-id ID]` | Reconstruct the newest matching layout model without starting a daemon; it also accepts `--serve`. |

Both restore commands accept `--restart-command <command>`. Use it with
`--serve` when restarted pane processes must remain alive; without `--serve`,
the reconstructed runtime is transient and terminates those processes before
the command exits. A live restore starts fresh processes and cannot reconnect
to the processes that existed when the snapshot was taken.

## Configuration, identity, and integrations

| Command | Subcommands and scope |
| --- | --- |
| `mez config` | `init`, `path`, `default`, `validate`, `get`, `layers`, `set`, and `unset`. `set` and `unset` write the user configuration by default; their `--scope project` option targets an eligible trusted project overlay. |
| `mez auth` | `status`, `login`, and `logout` for provider credentials and metadata. |
| `mez mcp` | `list`, `inspect`, `login`, `logout`, `status`, `add`, `remove`, `enable`, `disable`, `set`, `unset`, `tools`, and `approval` manage configured MCP servers, stored MCP credentials, tool filters, and server approval settings. |
| `mez sandbox` | Inspect version-2 backend status, plan or enable the platform backend, disable confinement, manage presets and sanitized profiles, inspect managed-home caches, and manage project trust. Plans report the selected backend and fixed-executable presence; unavailable mutating enablement fails without changing state. `mez sandbox trust` supports `list`, `inspect PATH`, `add PATH`, `reject PATH`, and `revoke PATH`. |
| `mez issue` | Add, show, update, query, and delete local project issues. |
| `mez memory` | List, inspect, add, edit, delete, archive, mark stale, restore, record use or confirmation, supersede, prune, export, and search persistent memory records. |
| `mez remote` | Use authenticated local Unix control for `status`, `invite`, `clients`, `rename CLIENT_ID LABEL`, and `revoke CLIENT_ID [--reason TEXT]`. Client-local commands are `pair --invite-file PATH [--name NAME]`, `invitation inspect PATH`, and `profile list|show|rename|remove|check`. Paired Iroh clients cannot use server trust-administration methods. |
| `mez completion <shell>` | Generate a completion definition for `bash`, `elvish`, `fish`, `powershell`, or `zsh`. |

### Iroh targeting and pairing

Pairing starts on a configured, running host and finishes on the client after
the invitation file is transferred through a confidential channel:

```console
# Host, through local Unix control
mez remote invite --role primary --allow-create --output mez-invite.json

# Client, after confidential transfer
mez remote invitation inspect mez-invite.json
mez remote pair --invite-file mez-invite.json --name home-mez
mez --iroh-profile home-mez attach
```

Use an `observer` invitation when the device needs read-only attachment, and
omit `--allow-create` when it must attach only to an explicitly named existing
session. See [Remote pairing and
recovery](../safety-and-trust/remote-pairing-and-recovery.md) for listener
prerequisites, route policy, role ceilings, revocation, and identity recovery.

`mez remote invite` accepts `--role observer|primary`, `--expires SECONDS`, and
`--output PATH`. A role ceiling does not grant session creation. Add
`--allow-create` when the device may use remote `new` or omitted-target
`attach`; optional `--max-leases`, `--max-live-sessions`, and
`--lease-lifetime-ceiling` narrow that authority. `--allow-kill` separately
permits a primary device with creation authority to force-kill sessions it
created.

An explicit Iroh primary `attach` also accepts these X11 options:

| Option | Behavior |
| --- | --- |
| `--x11` | Request X SECURITY untrusted forwarding. Forwarding remains off unless requested, and this mode fails closed if a local untrusted credential cannot be prepared. |
| `--x11-trusted` | Request full trusted X11 forwarding. This conflicts with `--x11` and requires `transport.iroh.x11.allow_trusted = true` on the host. |
| `--x11-takeover` | Explicitly replace another attachment's X11 route. It requires either `--x11` or `--x11-trusted`. |

These flags require an authenticated Iroh primary and are rejected for
observers and Unix targets. An unsupported peer or denied host policy is a
visible initialization failure; the client does not reconnect without the
requested forwarding. The attaching machine accepts conventional Unix
displays, constrained XQuartz launchd sockets, and TCP displays. A TCP hostname
or address—including a non-loopback target—is resolved once and frozen with
its real cookie before dialing. Neither value is sent to the server.

Direct control commands keep Unix as their default target. `--iroh-invite-file
PATH` explicitly performs first-use pairing from an owner-only, bounded JSON
invitation file, while `--iroh-profile NAME` explicitly uses a protected paired
profile. Add `--save-as NAME` to save invitation-issued authority under a
human-readable client-local alias. The alias is not a trust input: the pinned
server identity, client endpoint identity, role ceiling, and protected device
credential remain authoritative. These selectors conflict with `-S` and `-L`,
never fall back to Unix, and apply to the supported remote session commands:
`new`, `list`, `attach`, `kill`, and `detach`. Host-profile attach and kill
targets accept a lease ID, stable session ID, or exact name. Remote kill
requires an explicit target, `--force`, a primary role ceiling, and separately
granted force-kill authority.

An enabled host-scoped Iroh configuration does not change bare `mez` or `mez
serve` into remote-listener commands. Those direct session commands use Unix
control; only `mez host serve` binds the host Iroh endpoint. Use an explicit
`--iroh-profile` or `--iroh-invite-file` selector with a supported remote
command to initiate Iroh client transport.

Explicit Iroh targets work when listener-oriented `transport.iroh.enabled` is
false. They require `transport.iroh.outbound_enabled = true` (the default) and
derive a client-only direct or relay policy from the target's pinned address;
they do not start a listener or enable port mapping. Invitation targets perform
no address lookup. Paired profiles may use an address-lookup service only when
the user explicitly configured one; a successful endpoint-ID-pinned reconnect
refreshes authenticated route hints in the protected profile.

The primary `transport.iroh.compression_codecs` array defines codec preference
within streaming and non-streaming classes for explicit clients as well as
listeners. When both peers support a streaming codec and its non-streaming
alternative, `zstd-stream` or `lz4-stream` takes precedence. `zstd-stream` and
`lz4-stream` are opt-in stateful v3 codecs, `zstd` and `lz4` are independent
v2 application-frame codecs, and `none` is the unchanged v1 compatibility route. A
client may try the next configured codec only before opening a stream. There is
no hidden downgrade when `none` is absent, and
`compression_codecs = ["none"]` is the restart-required rollback setting.

X11 forwarding automatically uses that same negotiated connection codec; it
has no separate compression flag or fallback. The authenticated X11 stream
preface stays raw, while setup and application traffic follow the selected
record format. Use `compression_codecs = ["none"]` when raw X11 transport is
required for rollback or diagnosis.

### Persistent-host command contract

Configuration schema 73 defines the persistent-host mode above the existing
per-session runtime. It includes the local host and session-routing commands,
host-scoped Iroh identity and trust store, and protocol-v3 host-only pairing
and profile checks. The direct `mez serve` compatibility endpoint remains
session-bound and does not interpret an omitted target as creation.

The persistent-host command surface is:

```text
mez host serve [--max-sessions N] [--max-live-sessions N]
mez host status
mez host stop [--timeout SECONDS]
mez host reconcile

mez lease list [--state STATE] [--owner CLIENT_ID] [--all]
mez lease show <lease-id|session-id|name>
mez lease checkpoint <lease-id|session-id|name>
mez lease recover <lease-id|session-id|name>
mez lease release <lease-id|session-id|name> [--terminate]
mez lease revoke <lease-id|session-id|name> [--reason TEXT] [--terminate]
mez lease gc [--older-than DURATION] [--dry-run|--apply]
```

The `mez host serve` limits override the configured maximum durable session
records and concurrently live session runtimes for that invocation. Both must
be positive.

Lease administration uses only the protected local host socket. Active release
or revocation requires `--terminate`; neither operation revokes device trust.
Garbage collection previews by default, removes only terminal lease tombstones,
and requires `--apply` to mutate durable state. Durations accept plain seconds
or `s`, `m`, `h`, and `d` suffixes. Checkpoint capture and recovery are
generation-fenced, and recovery always starts fresh processes from the validated
checkpoint rather than preserving the previous PTY or process tree.

In that mode, bare local `mez` uses the protected local host, attaches to an
eligible session, or immediately creates and attaches when none is eligible.
`mez new [--name NAME]` always creates. Local commands do not require Iroh or
pairing. `mez serve` remains the foreground single-session compatibility path;
`mez host serve` is the sshd-like foreground service for a service manager.
It writes its initial machine-readable readiness record to standard output and
writes local and remote client connection, rejection, timeout, and failure
diagnostics to standard error. Local records identify the authenticated Unix
peer UID and request method. Remote Iroh connection, disconnection, and
post-authentication failure records identify the client by authenticated
endpoint ID and a privacy-safe route category (`direct`, `relay`, `custom`, or
`unknown`). Capacity and recurring maintenance degradation are logged only on
state transitions so routine operation remains quiet.

An Iroh profile in persistent-host mode identifies one stable host rather than
one session. The remote forms are:

```text
mez --iroh-profile HOST attach
mez --iroh-profile HOST attach <lease-id|session-id|name>
mez --iroh-profile HOST attach --default
mez --iroh-profile HOST new [--name NAME]
mez --iroh-profile HOST list
mez --iroh-profile HOST kill <lease-id|session-id|name> --force
```

Omitted-target `attach` atomically selects the existing host default or creates
one when none exists. `attach --default` selects an existing default and never
creates. `new` explicitly requests fresh idempotent creation, while an explicit
attach target selects only an authorized existing lease. Pairing and profile
checks are implemented as host-only operations and cannot create or attach a
session.
Host-only initialization advertises only the methods granted to that trust
record. Force-kill is distinct from detach and lease administration: it must be
granted when issuing a primary invitation and durably revokes the selected
lease before terminating its runtime.
Protected profiles report scope `host` or `legacy_session`; old profiles
without scope metadata remain legacy and are not granted host authority. Lease
release, lease revocation, runtime kill, and client-trust revocation remain
distinct.

Interactive remote attach requires a terminal and keeps one initialized Iroh
control stream open for its lifetime. A `primary` profile may attach as primary
or observer; an `observer` profile cannot attach as primary. The client also
negotiates one server-opened event stream. Primaries attempt versions
`3 → 2 → 1`; observers attempt `3 → 1`. Only a structured unsupported-version
initialization result advances to the next candidate; authentication,
authorization, malformed data, transport, and later stream failures remain
visible. Client-local clipboard writes are enabled only when a primary on v2
or v3 receives explicit `client_clipboard_write` capability confirmation;
observer v3 does not receive that authority.
Legacy authorized events wake a fresh `terminal/view`; observers receive only
session-view events at or after their atomic attachment cutoff, and detach or
event-stream failure ends the attach visibly.
For a negotiated primary or observer v3 stream, the event stream instead sends
an initial authoritative exact-client snapshot and then uses revisioned
whole-row deltas when they are safe and smaller than a replacement snapshot.
Stale, wrong-role, or malformed deltas fail without partially changing the
retained frame; reattachment starts from a fresh snapshot. V3 control responses
remain mutation acknowledgements, so steady-state rendering does not issue
`terminal/view`.
Observer push ownership additionally requires client opt-in and server
`pushed_render_updates` capability confirmation; older observer-v3 peers retain
notification-plus-fetch behavior.
When an event-stream write is backpressured, the server keeps bounded redraw
triggers rather than stale rendered frames, then sends one latest-state update
from the last successfully flushed base. It does not add a debounce or batching
timer. Each observer v3 stream retains its own terminal dimensions. A local
observer resize updates only that observer and prompts an exact-client pushed
snapshot; it does not resize the primary, another observer, or canonical pane
layout.
Acceptance and preface receipt share the configured Iroh setup timeout; expiry
closes the connection and requires an explicit reattach.
Terminal resize, input, and view requests remain ordered one at a time behind
their responses. If the connection fails after terminal input may have been
sent, Mez reports that the outcome is unknown, does not reconnect or replay the
input, and requires an explicit reattach.

For a negotiated primary, completed copy-mode and mouse text selections update
the server session's internal paste buffer and route the copied text to the
attaching machine. While a client clipboard route is active, server-host
clipboard commands are suppressed so the copy is delivered through the
client's configured adapter rather than duplicated on the server host; without
a negotiated route, copies retain the best-effort server-host clipboard write.
The client selects its own
`terminal.clipboard_copy_command`; the server cannot provide or override that
command. Writes are best-effort, limited to 8 MiB, and supported through the
same Linux (`wl-copy`, `xclip`, or `xsel`) and macOS (`pbcopy`) adapters used by
local Mez. WSL clients first bridge UTF-8 text to the Windows host clipboard
with Windows PowerShell `Set-Clipboard`, then retain the Linux helper fallbacks.
Headless or unsupported clients continue normally when no clipboard provider
succeeds. Clipboard reads and remote paste are not included.

Create invitation files without exposing the token through shell arguments or
world-readable output. `--output PATH` securely creates a new mode-`0600` file,
refuses to replace an existing path or symlink, and prints only the created
path. Invitations carry format version 1, and incompatible clients reject them
before dialing. On the direct-session compatibility path, omitting `--expires`
uses `transport.iroh.invitation_ttl_seconds`. The persistent host currently
uses 600 seconds when the option is omitted, even when that configuration value
differs. An explicit override must be from 30 through 86,400 seconds. For
example:

```console
mez remote invite --role primary --allow-create --allow-kill --output mez-invite.json
mez remote invitation inspect mez-invite.json
mez remote pair --invite-file mez-invite.json --name home-mez
mez --iroh-profile home-mez attach
mez --iroh-profile home-mez list
mez --iroh-profile home-mez kill SESSION_TARGET --force
```

`remote pair` redeems and saves the profile without entering a terminal session.
It uses host-only initialization, which cannot create, select, or attach a
session, and prints the exact reconnect command. Replace `SESSION_TARGET` with
a lease ID, session ID, or exact name returned by `list`. `remote profile list`
and `show` expose only aliases, role ceilings, abbreviated server fingerprints,
and route counts. `rename` changes only the local alias. `remove` deletes only
the local reconnect profile and explicitly does not revoke server trust.
`check` uses the same host-only initialization and reports a secret-free result.
Use local Unix `remote revoke` on the server to revoke a device.

Connection failures identify the setup stage and configured deadline. A setup
timeout also reports pinned direct and relay route counts and states that
Mezzanine authentication was not attempted, so users can distinguish network
reachability from trust rejection without exposing addresses or credentials.

`mez version` prints version information. `mez help` and `mez <command> --help`
show the generated command contract. Human-readable output is the default;
scripts should request `--json` and handle errors explicitly.

## Related pages

- [Sessions and panes](../using-mezzanine/sessions-and-panes.md)
- [Lifecycle, detach, and recovery](../operations/lifecycle-detach-and-recovery.md)
- [Configuration overview](../configuration/overview.md)
- [Remote pairing and recovery](../safety-and-trust/remote-pairing-and-recovery.md)
- [X11 forwarding workflow](../using-mezzanine/workflows.md#forward-x11-applications-from-a-remote-session)

## Next step

Use [Key bindings](key-bindings.md) for in-session interactive controls.
