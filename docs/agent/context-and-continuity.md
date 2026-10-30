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
to the next turn. Each current snapshot represents all discovered scopes, with
source contents sharing `instructions.max_bytes`; omitted or truncated contents
are identified. Successfully empty discovery explicitly supersedes earlier
guidance, retaining earlier snapshots as history. A native directory snapshot
owns its discovery separately from later pane-root bootstrap updates and keeps
it through its runtime-owned same-pane loop conversations. This is not a
guarantee that files are reread before every request. The agent does not
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
That snapshot includes the saved compaction projection: the first request and
later resumes retain the same model-written summary and exact uncovered history,
including selective summaries at their original positions. The full source
archive remains inspectable; covered raw rows do not reappear as active context.
Missing or corrupt required compaction state fails visibly rather than creating
a raw-history fork. Fork publication and any eligible saved terminal-handoff
reconciliation affect only the new target, never the parent's stored bytes.
If source persistence or terminal bookkeeping has not settled, the fork fails
visibly instead of cloning a shorter archive. Retry after the original storage
owner settles; retained append receipts reserve target identities as well.
Failed cloning or pane setup removes only the newly created target; requesting
an existing target does not overwrite or delete it. Forks retain source occurrence
times while receiving their own conversation/catalog creation time, identity,
name state and later appends. Historical usage and actions are not charged or
executed again, and copied history grants no new execution authority.
Cloning and storage rollback run on the command worker, so a large or locked
archive does not block terminal/control handling. A cancelled or stale fork
removes only its own unadopted target generation. If pane teardown or the adoption
reply is uncertain, Mezzanine retains the complete target and reports the failure
instead of risking live history; retained recovery intent owns any incomplete
safe cleanup. A waiting resume rechecks the target under the same storage lease.
Command-service cancellation and supervisor shutdown retain and observe fork
workers until they retire; a service error does not turn cleanup into detached
background work or erase the original failure.
For authenticated `agent/shell/command` control requests, a stored `/fork`
acknowledges acceptance with a mutation result and null body/turn. Observe the
visible source shell for eventual success or failure; that initial response is
not completion. Reusing its idempotency key returns the original acknowledgment
without creating another fork. Native `/cd` also acknowledges deferred acceptance;
observe the visible shell
for its final navigation result. Acceptance alone does not prove cwd changed.
When manual compaction folds a selective summary into its prefix, it uses that
summary once instead of resending the covered raw text. Later summaries stay in
the retained history. Selection preserves whole turns and whole ranges, and its
retained row count continues to describe physical history rather than model blocks. If
another publisher changes a summary while compaction is running, the old result
cannot overwrite that newer projection merely because the raw rows are unchanged.

Manual compaction freezes its source frontier when preparation is admitted. Later
appends do not silently enter that operation, and a changed source frontier
invalidates preparation even if its retained row count stays the same.

Model replay reads its compaction projection and checked transcript window
together, so an epoch change cannot splice one summary to a separately read tail.
Captured pending rows remain distinct from durable history until storage settles.
Initial projection persistence retains the inherited summary together with its
exact raw rows, including a genuinely empty tail. A partial write remains
recoverable and cannot acknowledge a complete inherited context or overwrite a
different saved summary. Recovery does not ask a model to recreate that context
or execute historical actions again.
A forked subagent uses that captured projection for its first request as well as
later replay. Its initial turn waits for complete publication, even when there
are no raw rows to append. Previously accepted terminal-compaction summaries
remain summaries; their copied audit records do not execute or compact again.

Rows appended after a compactor's source was captured stay in retained history.
Runtime counters follow assigned sequence positions, including intervening writers;
a shorter model representation does not remove those physical rows.

The saved entry total describes the archive. It can exceed the raw history
retained for model replay after compaction. Resume reconstructs that replay
count separately from the archive's logical sequence frontier; further
compaction must keep moving forward. Fork source work captures its summary and checked rows together, even when the
summary has replaced every raw row. A summary that crosses the captured cutoff
is rejected rather than shortened. Forks capture the source frontier so later
parent rows cannot silently enter an earlier snapshot. This count repair
adds no configuration option or persisted metadata field.

Conversation continuity is not proof that a command completed or that the
filesystem is unchanged: verify interrupted work before asking the agent to
continue.

Agent log entries retain presentation-only local occurrence evidence separately
from saved-session creation and activity timestamps. Streamed components keep the
time first observed locally; accepted prompts, steering and message receipts keep
their local acceptance time. This is not remote generation, command-start or
delivery evidence. Newlines, tables and wrapping do not create more entries, and
equal or backward clocks do not change conversation order.

Semantic replay, resize, fork and persistence retries preserve captured metadata
without adding timestamp text to source, copy output or model context. Older
source-backed logs retain only coarse record-time evidence; missing times and
opaque ANSI rows are not guessed from file dates or resume time. Hidden and
ephemeral/no-store panes retain this metadata in memory. Restart retention still
depends on the existing durable presentation log, steering checkpoint and peer
outbox; process-only snapshots do not introduce an ephemeral agent-history store.
This metadata foundation adds no visible timestamp column or configuration toggle.

Idle peer-triggered turns wait while their exact bound conversation's terminal
bookkeeping is unchecked, including worker-claimed history and terminal summary
append receipts. Accepted mail remains unread; an exact-owned idle child failure
keeps its original local receipt. Neither consumes a receive-turn budget or
creates speculative context before the history fence settles. Checked acceptance
and append settlement wake retained delivery through the existing timer. They
do not retry providers, actions or child work. A blocked archive check remains a
visible, fail-closed history error, not permission to invent empty history, and
does not fail healthy session services or independent recipients. Mail retains
its original TTL; expiry maintenance does not restart that deadline. Retained
work keeps the existing bounded one-second delivery timer. A deferred sweep
checks only the exact pending fence; it does not retry a failed archive read or
construct speculative history/context. Delivery fanout budgets are unchanged.

Filtered executor progress keeps its captured metadata only for the existing
component/promotion receipt's bounded lifetime; this does not make suppressed
content appear later or persist it after that owner retires.

Provider projections compose with active shell previews and executor progress
before pending steering. Initial stream and response-generation capture strip
exact-owned shell and executor overlays from the rollback prefix, regardless of
arrival order.
Failed provider installation retains executor source and settlement receipts.
Confirmed executor promotion extends the provider's rollback prefix without
retaining provisional provider text. Later provider
updates or discard keep those confirmed sections and their exact settlement
receipts, so final action presentation does not append the same effect again.

Transcript-only forks retain a bounded original record tail in the existing
presentation log so their new conversation/catalog creation time does not replace
coarse source-time evidence. Replay keeps each record boundary separate and
resume chrome untagged; malformed fallback source is rejected rather than shown
as raw model history. Exact validated streamed components retain their original
observation evidence even when ordering or geometry requires static re-rendering.

Saved peer messages replay with their original receipt identities, including
runtime turn/action IDs containing colons. Internal copy metadata escapes its
identity separators separately from message text, so resume and source-copy
preserve authored colons, percent signs and Unicode without rewriting history.
Source-backed replay regenerates copy carriers from the retained semantic record;
older numeric or delimiter-free grouped carrier identities remain opaque rather
than being percent-decoded or guessed from payload text. Legacy source-free ANSI
and recorded-width projection behavior is unchanged.

Active bindings are checkpointed separately for each Mezzanine session. New
versions import older shared binding metadata once without rewriting a running
older daemon's index. The known trailing-bracket corruption is recovered only
after an exact private backup; other damage is diagnosed rather than silently
discarded. See [lifecycle recovery](../operations/lifecycle-detach-and-recovery.md)
for coexistence, backup privacy and downgrade limitations.

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

The compactor receives the selected source once, without duplicating ordinary raw
replay or leaking the retained tail into its input. Replay removal keeps assistant
and owned action/native-tool/MCP evidence together as complete execution groups;
unrelated exact references keep their original identities and order. The summary
candidate restores the retained groups with their original execution ownership.

Prefix and selective summaries retain explicit summary identity through saved
epoch replay and provider projection, including a prefix labelled with its
canonical memory UUID. An ordinary UUID memory remains ordinary; only its
owning compaction producer supplies summary identity. Existing labels and
model-visible summary bytes are unchanged, and known legacy summary labels
remain recognized. A restored summary alone does not mean a new compactor ran.
A summary-only segment before exact steering stays anchored while the next
compaction selects later eligible raw work.

Manual candidate sizing and publication share the same prefix/range projection.
Absorbed ranges disappear; surviving ranges retain their anchors exactly once.
The candidate rebuilds owned history rather than retaining a cached summary
beside the same replayed range, and preserves unrelated optional memory, exact
references and native group metadata. A split range or backwards boundary is
rejected without changing the epoch. Fit checks still measure the complete
provider request with tools, controls and output/headroom allowance; fixing a
projection does not relax the request limit or replay an action.

If an earlier accepted prompt or command is still preparing its history, a new
`/compact` is refused before changing the conversation epoch. The earlier input
continues normally; this also protects guidance resumed after cancelling compaction.

The pane/footer, `agent/list` and `/status` agree that the current operation is
compacting even when there is no ordinary user turn. Allowed inspections do not
replace that footer with a generic command-running label. The footer and status
diagnostics show preparing/queued/claimed and existing pause detail; a claim is
not evidence that a provider has received a request. Elapsed time starts once
per logical operation and does not reset for source/request handoffs or compactor
chunks/retries. Completed or cancelled work no longer appears active.

When `/compact` has no model work, feedback names the eligibility reason: no
logical entries, no durable source, no eligible closed prefix, or an irreducible
exact/open retained tail. A fitting budget alone is not why explicit compaction
skips. An early exact/open group blocks later completed work from entering a
contiguous prefix; Mez does not silently skip that barrier. Dedicated intact MCP
epoch metadata uses the same narrow eligibility in forced and final selection.
Content-free `manual_compaction_no_work` events and feedback reason codes identify
these outcomes without copying source text or claiming a summary completed.
Preparing may discover a skip, but it then clears active work without queuing a
provider or changing the persisted replay epoch; a logical admission fence is not
a model request or summary commit.

If compaction fails, the error does not mean the source history was discarded.
Mez does not publish a partial replacement as a completed durable compaction.
Interrupted compactor responses (including HTTP 200 with an incomplete body)
retry the same frozen auxiliary work with the configured provider retry budget
and jittered backoff. The ordinary turn waits; settled actions are not replayed.
Manual and observed-input compaction use the same policy. Stop and human pause
remain effective during backoff. Exhausted or nonretryable failures are terminal,
and incomplete summaries never replace authoritative history.
Completed summaries are also checked before private chunk/synthesis staging.
Empty or malformed output, oversized UTF-8 text, and text above the frozen word
or estimated-input-token allowance retry only auxiliary summarization of the
same validated source. The rejected text is not published or quoted back. The
model receives the actual word/input-token unit and independent UTF-8 byte bound.
Compaction keeps its raw-text provider contract. If the model instead returns
the requested final-say JSON or `mezzanine-action-json` frame, local validation
extracts that one final payload before measuring or publishing it; malformed
frames consume the same finite correction budget, never execution authority.
Unframed JSON is recognized only by a top-level rationale plus an actions array
with type-tagged objects (or empty actions), not an isolated field name. Unknown
or non-final typed actions still fail validation. Ordinary JSON/data/code fences
and incomplete unframed JSON remain raw summary text under the same local bounds;
an explicit `mezzanine-action-json` fence also identifies incomplete frames.
Shorter correction guidance never becomes a stricter semantic acceptance limit:
a summary fitting the original allowance proceeds to complete-request fit even
above guidance. OpenAI never receives `max_output_tokens` in ordinary, compaction,
auxiliary or retry requests, streaming or not. Existing omission/cache behavior
is intentional; other providers retain genuinely supported generation controls
and configured framing/reasoning headroom. Still-invalid output must shrink each
fixed invalid dimension. Guidance stays nonzero and invalid output at the issued
floor fails with typed, content-free provenance rather than halving to zero.
Full-request fit and nonreducing-candidate checks still decide publication; no
summary is truncated and no ordinary action is replayed. Auxiliary diagnostics
separate phase/generation, frozen units, measured sizes, advisory targets and
actual admitted corrections from ordinary MAAP recovery counts.
Empty/malformed output gets at most two corrections per current
source, even with unlimited transport retries. Advancing legitimate chunks or
ranges has no arbitrary total-response cap. Earlier accepted summaries stay
private until the complete replay fits. Known auxiliary expense counts once,
but does not become an ordinary execution sample. Changed source, stale epochs
and corrupt required history are integrity failures, not retryable output.
Missing or corrupt required history is reported rather than silently omitted.
Provider-rejection recovery and observed-input recovery with uncommitted source
can initially be **turn-local**. An accepted summary is not automatically a
saved Memory record. At terminal settlement, Mez retains its original source
occurrences and can publish a separate durable handoff after complete typed
execution groups have committed through transcript receipts. Following prompts
wait for that handoff's bookkeeping and receipt settlement. Its summaries then
survive subsequent turns and reopen once, without restoring covered raw groups
or re-executing their actions. The archive itself is never truncated.

Legacy/unowned source and archive layouts that cannot be represented safely by
the selective epoch contract remain turn-local. Certificates and terminal rows
share occurrence numbering over the complete restored chronology, so separate
witnesses cannot restart a group's ordinals. Duplicate or contradictory witness
sequences fail before certificate capture; the map cannot replace one witness's
original payload with another witness's source. Compaction closes a group at its
consumed frontier; it does not seal the canonical group forever. A later valid
same-group occurrence can leave that group split across exact barriers or
summary ranges. Such a layout cannot satisfy the complete-group selective
contract and keeps raw replay authoritative before certificate append. Existing
runtime ownership checks still reject late results lacking an owning assistant;
this does not authorize replaying a provider response or an action.

Interleaved display rows can likewise make adjacent model-visible groups unmappable. Mez reports
that limitation before recording a publication certificate and keeps raw replay
authoritative. A changed source/epoch or failed publication instead fails closed
with an explicit persistence/history error; receipt recovery retries persistence,
not provider actions. Missing or ambiguous source errors report capture, admission or
committed-publication phase, witness/source indexes, source count and match count
without including private source text or opaque provider/group labels. The typed
occurrence encoding and handoff representation remain at their existing version.
These guarantees do not assert that every old session is
compactable, or that a larger follow-up prompt cannot legitimately need recovery.

The context percentage is the latest positive ordinary execution sample divided
by the model's context-window budget, not the size of a newly rebuilt request or
its `max_input_tokens` limit. Below 100% therefore does not establish that the
next request is under its input cap. Proactive compaction needs a fresh ordinary
response at or above that cap; prior-turn compaction and auxiliary usage are not
triggers. Installing a summary clears the displayed usage but keeps the old
pressure sample consumed until a new ordinary execution response arrives.
This applies even when the summary is turn-local and no durable epoch is
publishable. Latest-request cache reuse also becomes unknown; cumulative expense
is conserved. Cleared display metadata is checkpointed before recovery queues
the ordinary continuation. An immediate checkpoint error fails the waiting turn
without admitting a continuation, rather than restoring the pre-summary sample.
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

Expense totals retain provider counters already observed before malformed output
or bounded correction. A best-effort failure summary, including its own repair,
must not erase that earlier expense merely because it replaces displayed text.
An unaccepted summary remains a failure; missing counters are not proof of free
work, and reported counters are not verified vendor invoices. Recovery never
authorizes replaying completed actions or bypassing approvals.

An unchanged saved compaction summary does not imply another compaction. Local
continuity diagnostics compare summary content identities across snapshots;
only new canonical summary content accompanying a non-append rewrite explains
a local `compaction` transition. Rebuilt context retaining an old summary is
classified as `new_turn` at a turn boundary or `unexpected_rewrite` within the
same turn. These labels describe local context changes, not compactor dispatch
or provider cache decisions.

`/show-context` browses conversation entries. It lets you edit selected content
with `e` or delete it with `d`; this changes later model replay, not files or
already executed actions. It does not edit persisted context documents.
Rows already represented by a committed compaction summary are protected from
content edits, including both the compacted prefix and every inclusive selective
range. SAVE rechecks this protection even if compaction finished after the editor
opened. A refused save reports that the history is already represented by a
compaction epoch and retains your private recovery draft; it does not change the
current prompt or remove the summary. Uncovered rows remain editable through the
normal revision/pane checks, and reopening shows an accepted edit once. A missing
or corrupt required epoch fails closed rather than authorizing an edit.

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
