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
select one. Press `Ctrl+A d` or use `mez detach` to leave the primary client.
Detaching normally leaves pane processes and agent tasks running; only one
attached client can be primary at a time.

Use `mez serve` to run a foreground session service without attaching a primary
terminal. Select a specific service with `-S <socket-path>` or `-L <name>`, and
use `--json` for scriptable output. A background daemon started by `mez new`
retains stderr and panic diagnostics beside its control socket in a private
`<control-socket>.diagnostics.log`; a foreground `mez serve` reports directly
to its invoking terminal.

## Snapshot and resume deliberately

Use `mez snapshot create` to save layout state, and `mez snapshot` to list
saved snapshots. The `inspect`, `delete`, `resume`, `resume-latest`, and
planning subcommands operate on those saved layouts. A snapshot retains session topology,
selections, names, and known pane working directories. It can contain sensitive
titles and paths, but it does not retain credentials, pending approvals,
terminal history, live MCP state, or pane processes.

Use `mez snapshot resume-plan <snapshot-id>` before restoring when you need to
inspect the proposed layout recovery. Use `mez snapshot latest-plan` to inspect
the newest matching snapshot without selecting an ID. `mez snapshot resume
<snapshot-id>` reconstructs a saved session model; add `--serve` to start it as
a live foreground daemon. `resume-latest` offers the same behavior for the
newest matching snapshot. Both restore commands accept `--restart-command` for
restorable pane processes. A live restore creates fresh panes and shell
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
