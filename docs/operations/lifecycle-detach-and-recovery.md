# Lifecycle, detach, and recovery

## Purpose

Operate live sessions safely through detach, reattach, snapshots, and recovery
without treating persisted layout data as a resurrection of old processes.

## Prerequisites

Know the basic session commands in [Sessions and panes](../using-mezzanine/sessions-and-panes.md).

## Manage a live session

Use `mez new` to create a session. Bare `mez` attaches to the first session
that accepts a primary client and creates a session only when none is
available. Use `mez list` to discover resumable sessions and `mez attach` to
select one. Press `Ctrl+A d` or use `mez detach` to leave the invoking client.
Detaching normally leaves pane processes and agent tasks running.

The runtime exposes `mezctl/2` and allows up to 16 equal-authority attached
primaries with independent navigation and transient presentation. One layout
owner controls canonical PTY geometry; non-owner resizes affect only that
client's viewport. Owner detach elects the oldest remaining primary, while
final-primary detach retains canonical size and lets background work continue.

Use `mez serve` to run a foreground session service without attaching a primary
terminal. Select a specific service with `-S <socket-path>` or `-L <name>`, and
use `--json` for scriptable output. A background daemon started by `mez new`
retains stderr and panic diagnostics beside its control socket in a private
`<control-socket>.diagnostics.log`; a foreground `mez serve` reports directly
to its invoking terminal.

For an explicitly paired remote session, use `mez --iroh-profile PROFILE
attach`, pair without attaching with `mez remote pair --invite-file PATH --name
NAME`, or first pair and attach with `mez --iroh-invite-file PATH --save-as NAME
attach`. Add `--observer` to attach immediately with read-only access. These selectors do
not inspect the local session registry and never fall back to a Unix socket. A
role ceiling of `observer` cannot be elevated to primary attachment. Remote
attach also
negotiates an authorized event stream for redraw wakeups. Pending observers do
not receive it until approval; revocation, detach, or stream failure terminates
the remote attach and requires an explicit reconnect. The configured Iroh setup
timeout bounds both waiting for that stream and receiving its preface; timeout
closes the connection instead of leaving attach waiting indefinitely.

Remote terminal input is not retried after an ambiguous connection failure. If
Mez reports that an input outcome is unknown, treat the command as possibly
applied, inspect the session through a new explicit attach, and do not assume
that the lost input is safe to repeat. The local Unix socket remains available
for administration, revocation, and recovery independently of the failed remote
channel.

## Snapshot and resume deliberately

Use `mez snapshot create` to save layout state, and `mez snapshot` to list
saved snapshots. The `inspect`, `delete`, `resume`, and `resume-latest`
subcommands operate on those saved layouts. Snapshot payload version 5 retains
shared session topology, canonical geometry, names, known pane working directories,
and a client-independent landing view. It never restores attached client IDs,
layout ownership, client-local focus/history/zoom, transient presentation,
observer authority, event credentials, credentials, terminal history, agent
conversations, local message state, live MCP state, pending approvals, approval
grants, or pane processes. Restored sessions begin with zero attached primaries.

Snapshots are stored under Mezzanine's user-private configuration area. The
snapshot CLI uses its `snapshots` directory, while live session layout commands
use the separate `layouts` directory. Neither location is configurable. Treat
snapshot files as sensitive metadata: inspect their paths and titles before
sharing, copying, or backing them up outside your normal private storage
boundary.

Use `mez snapshot inspect <snapshot-id>` to inspect saved snapshot metadata.
`mez snapshot resume <snapshot-id>` reconstructs a saved session model without
starting a daemon; add `--serve` to start it as a live foreground daemon.
`resume-latest` offers the same behavior for the newest matching snapshot.
Both restore commands accept `--restart-command` for restorable pane processes.
A live restore creates fresh panes and shell
processes. It cannot reconnect to processes that exited, and it resets previous
live approvals. If a saved directory cannot be used, Mez falls back to the
user's home directory and reports the recovery state. Review interrupted agent
work before retrying a non-idempotent action.

## Recover an agent conversation

Agent transcripts, presentation logs, and pane session metadata are persisted
separately from snapshots. Use `/resume` to select a saved conversation,
`/new` to begin without prior conversation context, and `/fork` to open a fresh
pane for a copied conversation branch. After a restart, an active turn that
cannot be reconnected is marked interrupted rather than silently resumed.

## Related pages

- [Sessions and panes](../using-mezzanine/sessions-and-panes.md)
- [Context and continuity](../agent/context-and-continuity.md)
- [Troubleshooting](troubleshooting.md)
- [Normative persistence contract](../../SPEC.md#19-detach-reattach-snapshots-and-persistence)

## Next step

Use [Cache status and diagnostics](cache-status-and-diagnostics.md) when a
running agent's context or provider behavior needs inspection.
