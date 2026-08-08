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

## Related pages

- [Subagents and messaging](subagents-and-messaging.md)
- [Cache status and diagnostics](../operations/cache-status-and-diagnostics.md)
- [Agent shell](../using-mezzanine/agent-shell.md)
- [Normative context contract](../../SPEC.md#96-context-assembly)

## Next step

Configure or select a model through [Providers and models](providers-and-models.md).
