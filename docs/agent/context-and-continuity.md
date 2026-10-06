# Context and continuity

## Purpose

Keep a task's useful history, resume saved work, and choose when to start fresh
without confusing conversation history with persistent memory.

## Prerequisites

Use the [agent shell](../using-mezzanine/agent-shell.md) in an active pane.

## What the agent knows

Each turn receives the applicable project instructions, selected guidance, and
the conversation's ordered prompts, replies, and explicit action results. Mez
assembles discovered project guidance before provider requests, but freezes it
once a provider request chain has been accepted; changed guidance is deferred
to the next turn. This is not a guarantee that files are reread before every
request. The agent does not
automatically see terminal scrollback, a full-screen application's contents,
other panes, or files it has not inspected. Ask it to read a file or run a
bounded command when current evidence is needed.

Action results are retained as the bounded output originally shown to the model,
not recaptured from the terminal on later turns. A truncated result does not
become complete when you resume the conversation. Shell, patch, MCP, and web
results can contain sensitive task data; saved conversations and context exports
need the same care as the files being worked on.

Hiding the agent shell or detaching the client does not start a new
conversation. Saved history can also be resumed after restart. A fork receives
a snapshot of its source conversation and does not absorb later parent work.
Conversation continuity is not proof that a command completed or that the
filesystem is unchanged: verify interrupted work before asking the agent to
continue.

## Choose a conversation operation

| Goal | Command | Effect |
| --- | --- | --- |
| Continue the same task | Enter another prompt | Keeps the current conversation as context. |
| Reduce older context | `/compact` | Summarizes eligible completed work while retaining protected instructions and recent exact history. The summary is lossy. |
| Start an unrelated task | `/new` | Starts a fresh conversation without clearing the terminal view. |
| Start fresh and clear the view | `/clear` | Starts a fresh conversation and clears the visible conversation and terminal view. |
| Try a different approach | `/fork` | Copies the conversation into a new agent pane, leaving the source intact. |
| Return to saved work | `/resume` | Opens the saved-conversation picker. |

Neither `/new` nor `/clear` deletes your working files or reverses earlier
actions. Global or project guidance can still apply to a fresh conversation;
starting fresh is not the same as disabling that guidance or persistent memory.

### Compact a long task

Use `/compact` when older completed work crowds out the current task. Mez can
also attempt context-length recovery after a provider rejects an oversized
request. It does not summarize unfinished work indiscriminately or treat token
estimates as provider guarantees.

Manual `/compact` with retained history first reports **preparing** and shows
compacting while the daemon reads the captured source outside its actor. This
phase is not a sent model request. Other panes and Stop remain responsive;
cancelled or obsolete preparation cannot later queue provider work. The daemon
checks the original operation, conversation, configuration, store and pane before
adoption, captures current model/policy/context and eligible source, then renders
the immutable context and request on a second worker under the same logical owner.
Final adoption rechecks freshness before using the existing model compactor.
Live context capture and source eligibility remain actor-owned; workers cannot
rediscover or expand authority. A cancelled blocking source read or request worker
can retain one of the finite slots until its actual work finishes.

If compaction fails, the error does not mean the source history was discarded.
Mez does not publish a partial replacement as a completed durable compaction.
Missing or corrupt required history is reported rather than silently omitted.
Check the error and model limits, or start `/new` with a concise, verified
handoff. After a successful compaction, ask the agent to recheck important file
contents and results instead of treating the summary as exact evidence.

Retrieved MCP tool contracts are cleared by compaction. Explicit server
references survive, but the agent must retrieve a server's current metadata
again before calling its tools; see [MCP integration](mcp-integration.md).

## Resume and preserve saved work

Give an important conversation a recognizable name while it is idle:

```text
/name-session Release checklist
/resume
```

The picker initially shows active saved conversations for the current project.
Use its footer for navigation and filtering; these keys affect the selected row:

| Key | Action |
| --- | --- |
| `a` | Toggle current-project and all-project views. |
| `i` | Inspect the selected transcript before resuming. |
| Enter | Resume the selected conversation. On an archived row, restore it first. |
| `c` | Clear the selected name without deleting the conversation. |
| `d` | Delete the selected saved conversation; deletion is refused while it is bound to a live agent pane. |
| `r` | Toggle active and archived-only views. |
| `A` | Archive the selected active conversation or restore the selected archive. |

`/resume <uuid>` resumes an exact active conversation. `/resume --latest`
selects the most recently active saved **root** conversation, not necessarily
the first named row or a conversation from the current project. Direct UUID
resume cannot open an archive until it is restored.

Names do not protect conversations from the configured age and count retention
limits. Archive work you need to preserve long term; archives are exempt from
automatic active-session retention. `/name-session --ephemeral <name>` keeps a
display name but ranks it with unnamed rows by recency. `/name-session --clear`
removes only the current conversation's name.

If saved-conversation discovery is damaged, `mez session-catalog status`
reports catalog health and `mez session-catalog rebuild` explicitly rebuilds
the index from retained session files. Rebuild is not a way to recover files
already deleted by retention or by the user.

## Reuse guidance across conversations

Use a **context document** for text you want future turns to receive. It is a
user-owned source document, not an editable copy of a live provider request.
For project guidance:

```text
/context-doc create --scope project --title "Release constraints"
/context-doc edit <id>
/context-doc enable <id>
```

Replace `<id>` with the ID returned by `create`. Documents start empty and
disabled; edit and review their content before enabling them. Use `--scope
global` only for guidance intended for every project. `/context-doc list` and
`/context-doc show <id>` inspect documents; `disable <id>` stops future inclusion
without deleting the source, and `delete <id>` removes it.

Changes affect later turns, not already queued, running, or completed turns.
Concurrent edits or deletion can leave a private recovery draft instead of
overwriting newer content; use `/editor-recovery` when Mez reports that conflict.

Persistent **memory** is separate from conversation history and context
documents. `/memory` controls availability, `/show-memories` browses records,
and `/remember` asks the model to propose durable knowledge. Use memory for
stable reusable facts, not current task evidence, credentials, or progress logs.
Disabling memory does not erase the current conversation or its saved summary.

## Inspect and export context safely

Use `/status` for context and token information. Provider input usage includes
cache reads; local size estimates are not exact provider token counts. A cache
hit does not prove the context is correct, and a cold request after compaction
or a model change is not by itself a continuity failure.

`/show-context` browses conversation entries. It lets you edit selected content
with `e` or delete it with `d`; this changes later model replay, not files or
already executed actions. It does not edit persisted context documents.

`/copy-context` exports the running turn's assembled provider request. When idle,
it exports a synthetic next-request preview with a user-prompt placeholder;
that preview has not been sent to a provider. `/copy-patches` exports retained
patch payloads and outcomes, and `/copy-trace-log` exports retained diagnostics.
Review all exports before sharing them: exclusion of credentials and hidden
runtime policy does not remove sensitive data from prompts or action output.

## Related pages

- [Subagents and messaging](subagents-and-messaging.md)
- [Cache status and diagnostics](../operations/cache-status-and-diagnostics.md)
- [Agent shell](../using-mezzanine/agent-shell.md)
- [Normative context contract](../../SPEC.md#96-context-assembly)

## Next step

Configure or select a model through [Providers and models](providers-and-models.md).
