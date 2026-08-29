# Context and continuity

## Purpose

Explain what the agent retains across turns, how compaction changes that
history, and how to inspect context and cache-related diagnostics.

## Prerequisites

Use the [agent shell](../using-mezzanine/agent-shell.md) in an active pane.

## What a turn receives

Each request combines stable runtime and project guidance, ordered conversation
history, applicable explicit action results, and a short-lived suffix of facts
needed for the next request. User, assistant, action-result, project-file, and
terminal sources retain distinct roles. Terminal text becomes context only when
an explicit action result includes it; live controller state, passive terminal
content, credentials, and unrelated pane data are not normal model context.

The pane conversation can survive hiding the agent shell, detaching and
reattaching the client, and ordinary session persistence. A forked or routed
conversation uses its captured source boundary and does not absorb later parent
history.

Saved-conversation discovery uses a private SQLite metadata catalog so a large
session collection does not require reconstructing every summary during normal
startup. Conversation transcripts and presentation history remain in their
per-session files and are not stored as database blobs. The catalog can be
rebuilt from those retained files if it is lost or corrupt. Mez writes session
files first and then updates their catalog metadata. Exact `/resume <uuid>`,
`/resume --latest`, and active-session retention use indexed catalog queries;
exact lookup can repair only the requested UUID from retained files. Active
sessions are governed by the configured age and count limits; named sessions
count toward those limits, while archived sessions are exempt.
Resume completion is capped, and the interactive picker fetches viewport-sized
keyset pages rather than loading the full catalog. Directory and subagent
toggles plus picker search are applied by SQLite, while transcript detail is
loaded only for the row you explicitly open.

Archived payloads are private tar+zstd files with bounded metadata sidecars.
Ordinary catalog listing reads the sidecars rather than decompressing archives;
restore verifies the compressed digest, manifest, entry types, and paths before
installing an active session directory.

In the `/resume` picker, press `r` to switch between active and archived-only
sessions. Press `A` to archive the selected active session or restore the
selected archive. Enter on an archived row restores it asynchronously and then
resumes it in the pane that opened the picker. Default `/resume`, completion,
and `--latest` continue to consider active sessions only.

For catalog diagnostics and explicit recovery, use `mez session-catalog
status` and `mez session-catalog rebuild`. Normal status and discovery remain
bounded; rebuild is the deliberate full scan of retained session files.

## Compact and recover

Use `/compact` when an old conversation no longer fits efficiently. Mez
summarizes only closed older execution groups, retains a recent exact raw tail,
and protects active prompts and steering instructions from summarization. A
summary is intentionally lossy; start `/new` when old context should not affect
a new task, or use `/resume` to choose a saved conversation.

Use `/status` for current-pane context and token information. Cache reuse is a
provider observation, not proof that context is correct: provider/model changes
and compaction can legitimately create a cold request. Consult operations
diagnostics for cache and continuity interpretation.

Use `/show-context` to browse the current pane's conversation entries and, when
appropriate, delete an entry. Use `/copy-context` only to export the current
model request context for diagnostics; its contents can include sensitive task
material even though credentials and hidden runtime policy are excluded.

## Related pages

- [Subagents and messaging](subagents-and-messaging.md)
- [Cache status and diagnostics](../operations/cache-status-and-diagnostics.md)
- [Agent shell](../using-mezzanine/agent-shell.md)
- [Normative context contract](../../SPEC.md#96-context-assembly)

## Next step

Configure or select a model through [Providers and models](providers-and-models.md).
