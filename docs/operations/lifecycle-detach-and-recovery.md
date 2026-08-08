# Lifecycle, detach, and recovery

## Purpose

Operate live sessions safely through detach, reattach, snapshots, and recovery
without treating persisted layout data as a resurrection of old processes.

## Prerequisites

Know the basic session commands in [Sessions and panes](../using-mezzanine/sessions-and-panes.md).

## Manage a live session

Use `mez` to create or attach according to the configured default, `mez new` to
create a session, `mez list` to discover resumable sessions, and `mez attach`
to return to one. Press `Ctrl+A d` or use `mez detach` to leave the primary
client. Detaching normally leaves pane processes and agent tasks running; only
one attached client can be primary at a time.

Use `mez serve` to run a foreground session service without attaching a primary
terminal. Select a specific service with `-S <socket-path>` or `-L <name>`, and
use `--json` for scriptable output. Detached service stderr and panic
diagnostics are retained beside the control socket in its private diagnostics
log.

## Snapshot and resume deliberately

Use `mez snapshot` to create, list, inspect, delete, or resume saved layout
state. A snapshot retains session topology, selections, names, and known pane
working directories. It can contain sensitive titles and paths, but it does not
retain credentials, pending approvals, terminal history, live MCP state, or
pane processes.

Resuming a snapshot creates fresh panes and shell processes. It cannot reconnect
to processes that exited, and it resets previous live approvals. If a saved
directory cannot be used, Mez falls back to the user's home directory and
reports the recovery state. Review interrupted agent work before retrying a
non-idempotent action.

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
