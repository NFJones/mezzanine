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
select one. Press `Ctrl+A d` inside an interactive attachment to leave that
client. For administration from another process, use `mez detach --client-id
ID`; a bare one-shot `mez detach` cannot identify a separate attached client.
Detaching normally leaves pane processes and agent tasks running.

Explicit pane termination requests graceful shutdown and then escalates.
Preserve termination errors and inspect remaining processes rather than
assuming a failed stop completed. Detach is not termination.

A session allows up to 16 equal-authority attached
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
attach`. Bare remote attach reconnects to the existing host default, creating
one only when none exists; use `new` when a fresh remote session is intentional.
Add `--observer` to attach immediately with read-only access. These selectors
do not inspect the local session registry and never fall back to a Unix socket.
Observer attachment requires an attached layout-owner primary; if none exists,
initialization fails before allocating an observer or changing session state.
A role ceiling of `observer` cannot be elevated to primary attachment. Remote
attach also negotiates an authorized event stream for redraw wakeups. An
observer follows that exact source primary and receives session-view events
only from its atomic attachment cutoff onward. Detaching the source primary,
revocation, self-detach, or stream failure terminates the remote observer and
requires an explicit reconnect. The configured Iroh setup timeout bounds both
event-stream setup and transport shutdown. Use `:exit` only when session
termination is intended; it is not a detach command. A lost termination response
can leave the outcome uncertain, so inspect session state through local control
before repeating it.

Remote terminal input is not retried after an ambiguous connection failure. If
Mez reports that an input outcome is unknown, treat the command as possibly
applied, inspect the session through a new explicit attach, and do not assume
that the lost input is safe to repeat. Ordinary remote connection failure leaves
local Unix administration available. A supervised Iroh listener failure can stop
the host and its Unix listener together; restore the host before local recovery.
See [Persistent multi-session host](persistent-host.md#stop-upgrade-and-recover).

## Snapshot and resume deliberately

Use `mez snapshot create` to save layout state, and `mez snapshot` to list
saved snapshots. The `inspect`, `delete`, `resume`, and `resume-latest`
subcommands operate on those saved layouts. Current snapshots retain shared
session topology, geometry, names, known pane working directories, a landing
view, and local inter-agent messaging state. Older messaging state without
resolved audience metadata is discarded rather than replayed to a broader
audience. A snapshot does not restore
attached client IDs, layout ownership, client-local focus/history/zoom,
transient presentation, observer authority, event credentials or cursors,
provider credentials, terminal history, agent conversations, live MCP state,
pending approvals, approval grants, or pane processes. Restored sessions begin
with zero attached primaries by default; `--serve
--attach-primary` creates the documented interactive primary during live
restore.

Snapshots are stored under Mezzanine's user-private configuration area. The
snapshot CLI uses its `snapshots` directory, while live session layout commands
use the separate `layouts` directory. Neither location is configurable. Treat
snapshot files as sensitive metadata: inspect their paths and titles before
sharing, copying, or backing them up outside your normal private storage
boundary.

The snapshot directory also holds `snapshots.sqlite`, a derived index the
daemon and the snapshot CLI keep for listing, latest selection, and deletion.
The manifests remain the source of truth: a missing index is rebuilt on the
next read, and an older index schema is replaced from those manifests. A newer
schema is rejected rather than silently discarded; use the matching newer
binary or an appropriate backup instead of deleting it to force a downgrade.
For manual index recovery, stop all daemons and CLI writers using that store
and preserve the manifests, payloads, database, and any `-wal`/`-shm` sidecars
before moving the index aside. Do not delete a live SQLite database or its
sidecars to clear a lock. `mez storage export snapshots` prints the latest
winners in the retired `latest.index` shape without creating or migrating the
database.

Use `mez snapshot inspect <snapshot-id>` to inspect saved snapshot metadata.
`mez snapshot resume <snapshot-id>` reconstructs a saved session model without
starting a daemon; add `--serve` to start it as a live foreground daemon.
`resume-latest` offers the same behavior for the newest matching snapshot.
Both restore commands accept `--restart-command`, but use it with `--serve`
when restarted pane processes must remain alive. Without `--serve`, restore is
transient: any restarted process is terminated when the reconstructed model is
serialized and the command exits. A live restore creates fresh panes and shell
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

Saved-conversation discovery metadata is indexed in
`agent-sessions/catalog.sqlite3` under Mezzanine's user-private configuration
area. Transcript and presentation payloads remain in their existing session
directories; the catalog does not replace or contain conversation content.

The first startup after this catalog is introduced performs a one-time import
of existing session metadata and leaves all existing payloads and sidecars in
place. Later healthy startups validate the catalog without scanning every saved
session. If the database is missing or SQLite identifies it as corrupt,
Mezzanine rebuilds it from retained session files and keeps the previous
database as `.catalog.sqlite3.backup`. A catalog created by a newer Mezzanine
schema is not overwritten or downgraded; startup reports the incompatibility so
the newer data remains intact.

Conversation payloads are written before their catalog updates. If a catalog
write fails, preserve those files and use the recovery commands below instead
of deleting the conversation. An interrupted transcript write can leave only
a durable prefix; queued history may be lost if the process exits before
persistence accepts it. Startup reconciles retained recovery receipts and
rejects conflicting history rather than blindly appending it. A failed
checkpoint or write is not proof that no data was saved, nor that all displayed
history is durable.

The interactive resume picker pages through bounded metadata. Open `i` for a
bounded recent transcript preview of the selected conversation; listing alone
does not load its full transcript.

Treat `catalog.sqlite3`, its `-wal` and `-shm` files, migration markers, and
backups as sensitive metadata. Do not remove transcript directories or legacy
sidecars as part of catalog recovery: they are the reconstruction source.

Use `mez session-catalog status` for a bounded, read-only health report. It
shows schema and integrity state, indexed row count, migration-marker and lock
state, retained backup or rebuild-temporary files, process-local operation
counters, and an actionable diagnostic without prompt content. Status does not
create a missing catalog or enumerate session directories.

Use `mez session-catalog rebuild` only when status recommends recovery or an
operator intentionally wants full reconciliation. Rebuild is the explicit
full-session scan: it waits a bounded time for catalog ownership, rebuilds a
verified temporary database, retains the previous database as
`.catalog.sqlite3.backup`, and cleans failed temporary SQLite files. It refuses
to replace a readable future schema. Keep `named-sessions.json`, `summary.json`,
and `metadata.json`; they remain rollback and rebuild inputs for this catalog
version, and compatibility name writes remain enabled.

Before manual recovery or backup, stop the owning daemon so storage is not
changing, and preserve the payload directories together with metadata, recovery
receipts, and SQLite sidecars. Do not delete a live database's `-wal` or `-shm`
files to clear a lock. A readable newer schema requires the matching newer
Mezzanine version, not forced deletion or downgrade. Recheck catalog status
after recovery and inspect the interrupted conversation before continuing work.

Named active sessions participate in the configured age and count retention
policy. Archive a session to preserve it outside active retention. Archives are
stored beneath `agent-sessions/archived` as private tar+zstd payloads with
bounded metadata sidecars; healthy startup reads only interrupted-operation
recovery journals, while explicit catalog rebuild may enumerate sidecars
without decompressing every archive.

The `/resume` pager is active-only and scoped to the current Git project by
default: directories within the same nearest `.git` root share a project key.
Each separate worktree or nested repository has its own root and project key;
sharing a Git common directory does not merge their saved-session lists.
Outside Git, each canonical directory is its own project. Use `a`
to toggle all projects, `r` for the archived-only view, `A` to archive or
restore the selected row, and Enter to restore and then resume an archived
row. The Directory column and direct resume retain the saved working directory,
not the project root. Archive and restore run on the persistence worker, so
the pager remains responsive and reports completion or failure in place.

## Related pages

- [Sessions and panes](../using-mezzanine/sessions-and-panes.md)
- [Context and continuity](../agent/context-and-continuity.md)
- [Troubleshooting](troubleshooting.md)
- [Normative persistence contract](../../SPEC.md#19-detach-reattach-snapshots-and-persistence)

## Next step

Use [Cache status and diagnostics](cache-status-and-diagnostics.md) when a
running agent's context or provider behavior needs inspection.
