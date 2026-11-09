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
later resumes retain the same model-written head and exact uncovered history.
Older selective formats require explicit normalization before replay. The full source
archive remains inspectable; covered raw rows do not reappear as active context.
Missing or corrupt required compaction state fails visibly rather than creating
a raw-history fork. Fork publication establishes the new target's replay
authority. Historical terminal receipts only validate existing coverage and
cannot publish another selective head. Source capture
may atomically import or repair descriptive summary accounting and legacy row
placement under the
parent's lock, preserving its summary text, coverage, raw archive and pane
bindings. Parent and child keep the same assigned summary occurrences.
An archived terminal handoff may still contain an older baseline. Fork translates
that baseline using the frozen source epoch's original records only when the
legacy import witness verifies it; equal text from a new publication cannot
substitute. The parent's certificate and source rows stay unchanged.
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
Manual compaction uses the current head once, without resending its covered raw
source. Selection preserves complete execution groups, and its
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

The async runtime prepares `/loop` history on a bounded worker before starting
its first turn. Forked iterations keep that original checkpoint or legacy
history projection, so later parent changes do not enter the loop. `/stop` can
cancel preparation before a work turn exists; a late result cannot start the
cancelled loop. A reader that is still running keeps its worker capacity until
it retires.
Reuse-mode loops prepare the next iteration's current history on that worker
lane too. Each result belongs to its exact settled predecessor and iteration;
cancellation or a duplicate result cannot repeat completed work or start an
extra iteration. Source failures stop that controller while other panes remain
usable.
If provider-continuation admission fails after a standalone loop restores its
parent, required terminal cleanup releases the old running-turn slot only with
the original private restoration receipt. A matching conversation name is not
enough. Failed restoration keeps its cleanup obligation; it cannot clear a
replacement turn or resume the old work.

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
| Reduce older context | `/compact` | Applies the configured summary or raw-tail mode while retaining protected instructions and recent exact history. Both modes lose optional old detail from model input. |
| Start an unrelated task | `/new` | Starts a fresh conversation without clearing the terminal view. |
| Start fresh and clear the view | `/clear` | Starts a fresh conversation and clears the visible conversation and terminal view. |
| Try a different approach | `/fork` | Copies the conversation into a new agent pane, leaving the source intact. |
| Return to saved work | `/resume` | Opens the saved-conversation picker. |

Neither `/new` nor `/clear` deletes your working files or reverses earlier
actions. Global or project guidance can still apply to a fresh conversation;
starting fresh is not the same as disabling that guidance or persistent memory.

### Compact a long task

`agents.compaction_mode = "summary"` is the default. Choose `"raw_tail"` to
discard optional older model input without generating a summary. `/compact`
takes no arguments in either mode. Raw mode makes zero compactor model calls,
does not look up compactor credentials, and incurs no auxiliary model usage.
The original archive stays intact, but omitted history does not return to model
input on resume, restore, fork, or a later switch back to summary mode.

Raw retention keeps one newest complete optional suffix under
`agents.compaction_raw_retention_percent` and the remaining ordinary input
capacity after the built-in system prompt, fixed schemas/controls and
stable/protected input. It stops at a
newer group that does not fit rather than backfilling smaller older groups.
Exact active instructions, open native/MCP/peer dependencies and post-freeze
arrivals survive. Protected source may exceed the optional allowance; if it and
fixed overhead exceed input capacity, compaction reports irreducibility.
An unchanged raw tail is no-work. Only an actual provider input rejection can
force further whole optional groups out through a finite descending frontier;
there is no automatic summary fallback or replay of settled actions.

Raw publication uses the same source, claim, worker, receipt and adoption
lifecycle as summary mode. Its explicit intentional-omission epoch has no head,
Memory or elision block. Accepted publication clears ordinary request/cache and
retrieval state and execution-usage display while preserving cumulative expense.
Configuration changes affect future admissions, never rewrite saved history or
retarget an admitted operation; existing freshness and permission fences apply.
Pane-only permission or action-scope edits also invalidate captured work even
without a configuration reload. New claims and live adoption recheck that scope;
refusal retires the waiting work without replaying actions.

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
For canonical history, source selection uses the saved head and exact members,
including qualified later arrivals. Zero pane counts do not make this history
empty. A frozen NoStore conversation can compact its owned body in memory;
this does not save it or establish restart durability. Manual compaction preserves
its exact task and native-group protection, adds no new task, and queues no
ordinary provider continuation after accepting the storage or in-memory receipt.
An explicitly empty NoStore binding with no captured source does not inherit a
saved conversation merely because it has the same ID or a store is installed.
Live context capture and source eligibility remain actor-owned; workers cannot
rediscover or expand authority. A cancelled blocking source read or request worker
can retain one of the finite slots until its actual work finishes.

The compactor receives the selected source once, without duplicating ordinary raw
replay or leaking the retained tail into its input. Replay removal keeps assistant
and owned action/native-tool/MCP evidence together as complete execution groups;
unrelated exact references keep their original identities and order. The summary
candidate restores the retained groups with their original execution ownership.

Prefix summaries and typed heads retain explicit summary identity through saved
epoch replay and provider projection, including a legacy prefix labelled with its
canonical memory UUID. An ordinary UUID memory remains ordinary; only its
owning compaction producer supplies summary identity. Existing labels and
model-visible summary bytes are unchanged, and known legacy summary labels
remain recognized. A restored summary alone does not mean a new compactor ran.
A summary-only segment before exact steering stays anchored while the next
compaction selects later eligible raw work.

Each accepted summary also owns a separate occurrence UUID and cached local
input estimate. Historical version-3 epoch files store those records with the
prefix and selective summaries. Their codec preserves original identities;
selective replay is refused before accounting import. Prefix-only accounting
imports once under the conversation lock before exposing its assigned records.
Current typed publication, receipt retries and reopened replay reuse their
original records; equal text from a new compaction receives a new occurrence.
Stale estimates are repaired against actual rendering
without replacing identities or rewriting raw history. Invalid typed record
slots fail visibly. Existing summary text limits stay at one MiB; the v3 file
allows sixteen MiB to accommodate accounting metadata during migration.
Accounting remains outside model messages and billed usage.

A loop captured from a historical prefix keeps its original summary and exact
selected raw window as typed memory-only history, including original occurrence
placement and producer fields. Covered task dependencies stay in copy backing,
outside model context. Capture does not migrate or save the parent, and later
parent edits or summaries cannot change a saved child. The child's actual save
receipt establishes its durability.

The source imported for a turn keeps its original store, replay epoch, physical
row images and accepted pending ownership. Compaction uses that capture through
dispatch, correction and interruption recovery. Installing another store with
the same conversation or record IDs does not replace it. Later pending arrivals
remain qualified by their original archive path. Summary publication, MCP reset
and guidance rows, and accepted replay refresh use that same store; a turn that
originally had no storage does not acquire it retroactively. Conflicting pending
ownership fails preparation visibly instead of being treated as empty history.

An interruption reference appended after an accepted head keeps its original
occurrence, producer, cost and exact rendered bytes through replay and saved
copies. It remains a runtime reference and restores no execution or tool rights.

Ordinary terminal bookkeeping and completion receipts retain the same original
store and the turn's admitted save policy, including when a store is installed
or replaced before completion-group admission. A child's save policy is separate
from the history window it inherits. Retiring context moves its original source
with the retained projection metadata. Rebuilding the same completion evidence
for a failed handoff reuses its occurrence and local estimate; a changed proposal
or a new group receives its own record.
Saved completion projection checks the original accepted head and revision.
An unpublished local rewrite cannot substitute for that head, and replacement
live metadata cannot repair a mismatched source after detachment. Late native
effects remain retained while the affected archive projection is refused.
After terminal admission for a typed head, the original bookkeeping candidate
waits for its ordinary append receipt. A durable head alone does not settle
those rows: peer receive and new prompt history remain fenced through failed or
partial writes. Only the exact original path and batch completion clears the
fence; the waiting candidate cannot be dispatched for another read.

Routed workers retain the surviving seed records and chronology anchors instead
of importing bare block text again. Their original history source and imported
boundary travel with the context through retirement, handoff and repair. A later
store installation cannot supply that history, and each child retains its own
save policy independently of its inherited source.

Recovery of an existing legacy prefix receipt retains the original proposal occurrence. If its epoch
reached disk before a later write failed, retry checks the same frozen source
rows and exact proposal before settling the required-state marker. An equal-text
proposal with a different occurrence cannot take over that publication.

Manual candidate sizing and publication share the canonical frozen head/tail
plan. The command and prepared request worker require that original owner;
there is no separate row/count/word selection or flat-transcript request factory.
The history importer qualifies legacy source before selection and retains its
original occurrence identities, costs and native protection. Optional memory
and display counts cannot substitute for missing source ownership.
Accounting and audit remain outside provider input. Invalid accounting envelopes
stop admission, while literal marker text in user or assistant rows remains
authored content. Candidate preview preserves unrelated optional memory, exact
references and native group metadata without rebuilding a legacy range projection.
Read-only candidate previews measure the next request with tools and controls;
they do not decide summary acceptance or replay an action.

If an earlier accepted prompt or command is still preparing its history, a new
`/compact` is refused before changing the conversation epoch. The earlier input
continues normally; this also protects guidance resumed after cancelling compaction.

The pane/footer, `agent/list` and `/status` agree that the current operation is
compacting even when there is no ordinary user turn. Allowed inspections do not
replace that footer with a generic command-running label. The footer and status
diagnostics show preparation, queued/claimed work, packing, inference,
validation or publication, call count and existing pause detail. `/status`
also shows logical epoch, dispatch generation and serial depth. Invocation is
not proof of HTTP delivery. Elapsed time starts once
per logical operation and does not reset for source/request handoffs or compactor
chunks/retries. Completed or cancelled work no longer appears active.

When `/compact` has no model work, feedback names the eligibility reason: no
logical entries, no durable source, no eligible closed prefix, or an irreducible
exact/open retained tail. A fitting budget alone is not why explicit compaction
skips. Exact/open groups remain intact, but independent completed units on
either side can enter the same canonical reference summary. Retained source
keeps its original bytes, identities and relative order. The historical
`no-eligible-closed-prefix` reason code now means there is no eligible whole
closed source. Dedicated intact MCP epoch metadata cannot create model work.
Content-free `manual_compaction_no_work` events and feedback reason codes identify
these outcomes without copying source text or claiming a summary completed.
Preparing may discover a skip, but it then clears active work without queuing a
provider or changing the persisted replay epoch; a logical admission fence is not
a model request or summary commit.

Compaction and memory request panics settle as failures against the original
task or observation; they do not stop healthy sibling provider work. Generated
title request panics reject that exact title attempt. Cancellation is reported
separately. Structured failures contain bounded categories, while the ordinary
process panic diagnostics remain available. Shared actor failures still stop
the compromised owner.

Compactor construction, required timer admission and lost claim replies settle
immediately when the owner can prove no worker received the dispatch. The exact
claim and its zero-expense accounting identity are released together, with
operation diagnostics and accepted steering preserved. They need no later worker
callback. Deadline expiry or Stop with a potentially live worker keeps the
original expense owner until physical retirement and final usage settlement.
Successful reply enqueue still leaves a handoff in progress. If every unread
dispatch copy disappears before producer registration, normal worker progress or
claim expiry settles the original zero-expense owner, including after Stop.
Registered workers keep their original cost custody through physical retirement;
cloning or dropping another dispatch copy cannot replace that owner.

When the provider service retires a claimed request, it observes request
termination and settles the original claim. A completed result still follows
normal actor validation; cleanup does not substitute a cancellation for it.
Reported auxiliary usage survives cancellation or a request panic, including
when response content was never published. Normal returned usage is not charged
again from the retained counters. Inference keeps its wire checkpoints.
Compaction checkpoints each reported request before continuing private work;
final or cancellation settlement adds only the remaining reported usage.
Duplicate receipts are ignored, and these accounting checkpoints do not publish
summary content or authorize another request. Cleanup
preserves the first failure if another retirement step also fails; shared monitor
failures remain fatal to their owning service. Existing title retry bounds still
apply, and no settled action or provider request is replayed by retirement.

If compaction fails, the error does not mean the source history was discarded.
Mez does not publish a partial replacement as a completed durable compaction.
Interrupted compactor responses (including HTTP 200 with an incomplete body)
retry the same frozen auxiliary work with the configured provider retry budget
and jittered backoff. The ordinary turn waits; settled actions are not replayed.
Manual and observed-input compaction use the same policy. Stop and human pause
remain effective during backoff. Stop also retires an in-flight compaction
request while preserving its reported usage and keeping the actor available.
A claim stopped before worker admission cannot start another request.
Exhausted or nonretryable failures are terminal,
and incomplete summaries never replace authoritative history.
In summary mode, all three triggers use one frozen-source model operation. It
packs actual
provider requests outside the actor, retaining the selected prefix, raw tail,
model and source owner across waves. Rolling carry or parallel reduction can
satisfy the operation; four concurrent requests is a ceiling, not a promised
fanout. A fitting source and usable final require one auxiliary call. Only a
complete typed result reaches the original publisher.

Empty, malformed or explicitly incomplete output gets at most two repairs per
logical work item, including with unlimited transport retries. Genuine summary
memory overflow fails without a shortening loop. New summaries have a 128-KiB
decoded resource bound. Advisory word/token targets, equal-size output and a
large next ordinary-request estimate cannot reject a usable final or generate
another summary. Actual input rejection repacks private work without changing
the selected source or raw tail. Each newly packed rejection halves its measured
input target, reaching a usable capacity or a truthful terminal refusal instead
of retrying nearly identical prefixes. Older in-flight failures retain their
own measured input sizes when refining an already lower admission ceiling.

Raw text remains supported. A valid final-say JSON or
`mezzanine-action-json` frame contributes its single final payload without
executing any action. Malformed recognized frames consume the repair allowance.
Unframed JSON requires a top-level rationale plus an actions array with typed
objects (or empty actions); ordinary JSON/data/code fences remain raw text.
An explicit action fence also identifies incomplete framed output. OpenAI
requests always omit `max_output_tokens`, streaming or not; other providers
retain their supported generation controls.

Auxiliary diagnostics record only phase, operation, dispatch generation, actual
model-call count, serial dependency depth and elapsed time. Retries and repairs
count as calls without increasing logical dependency depth. Known expense counts
once without becoming an ordinary execution sample. Changed source, stale
epochs and corrupt required history remain integrity failures; missing history
is reported rather than silently omitted.
For a canonical first task, its active user instruction remains exact while
completed execution groups after it can be compacted. Exact steering, pending
peer handoffs and unfinished native groups remain retained islands, never
summary input or implicitly completed work. The rolling head is a reference
snapshot with explicit selected occurrence coverage, not an assertion that a
contiguous prefix disappeared. Sparse summaries use version-thirteen receipts;
existing prefix receipts retain their original validation and digests. Nested
checkpoint version five retains exact membership and records retained handled
occurrence revisions. This distinguishes new eligible work from eligibility
lost when an unfinished group's span grows. Original version-four checkpoints
remain readable without inventing per-occurrence handling evidence.
An empty eligible selection is checked before summary sizing; no-work does not
fail already accepted execution. An ordinary continuation keeps exact source;
a provider rejection with no eligible source settles the affected request
without another identical compactor request or action replay. Having no ordinary
archive yet grants no permission to drop instructions or acknowledge pending
transcript writes. Unclaimed auxiliary results cannot install a summary.
Provider-rejection recovery, observed-input recovery and manual compaction use
the same canonical source and one-head publisher. Valid uncommitted source is
backed before publication is acknowledged; its ordinary transcript writes keep
their own receipt and settlement requirements. A summary does not become a saved
Memory record or wait for a second terminal-summary writer. Subsequent turns and
reopen select the accepted head and exact retained raw members without restoring
covered groups or re-executing their actions. The audit archive is not truncated.

Imported source keeps its exact frozen archive encoding, whether legacy or
accounted, bound to the original occurrence. Live source uses the same accounted
encoding as terminal bookkeeping. Recompacting a summary preserves required
original backing; changed source fails qualification rather than replacing its
proof. Historical terminal certificates and inherited audit remain inspectable
recovery evidence, excluded from compactor input along with nested accounting
data. Their receipt settlement does not select another replay history. Current
bookkeeping projects the actual canonical chronology and emits no new terminal
compaction certificate or private replacement witness.

An unchanged published head and closed source cannot start another automatic
compactor merely because its continuation is rejected again or a new response
reports high input. Repeated rejection reports context unavailable. Fresh
eligible history (including completed groups after an active instruction),
genuine task completion or changed relevant model/input
configuration can rearm recovery; new protected steering alone cannot. An
explicit new `/compact` still uses any eligible old tail through the same
planner. It cannot summarize protected or unfinished work.

Unowned or ambiguous source fails before model work is queued. Interleaved
display rows do not grant ownership or require a separate selective compactor;
publication uses the original canonical members and qualified source backing.
Reopening a selected execution group prevents publication, and runtime ownership
checks still reject late results lacking an owning assistant. A changed source,
epoch or failed publication reports an explicit persistence/history error;
receipt recovery retries persistence, never provider actions. Diagnostics omit
private source text and opaque provider/group labels. These guarantees do not
assert that every old session is compactable, or that a larger follow-up prompt
cannot legitimately need recovery.

The typed epoch publisher returns a private acknowledgment for its exact
prepared summary and original archive. Its live adoption boundary installs that
same summary occurrence while retaining current instructions, request metadata,
pending native ownership and source received during the write. A duplicate reply
preserves subsequent work; a wrong archive, replacement owner, changed protected
selection or newer head refuses the stale adoption. Arrivals keep their original
birth revisions and required original backing. Subsequent publication qualifies
that source again under the conversation lease. A cold checkpoint describes the
selected history; it cannot grant execution authority or acknowledge an ordinary
append whose actual persistence owner has not settled it.

An independently authored later task stays beyond the captured archive cut,
including when it arrives after proposal preparation. Reopening a selected
execution group instead prevents publication. Earlier qualified pending/frontier
controls remain inert under canonical causal ordering, including on cold replay;
a delayed older control cannot erase a newer renewal. The writer's receipt carries its
checked physical row position; live adoption combines it with original pending
rows from the same archive without settling them. The logical row frontier and
physical post-cut count remain separate from retained model membership.
For a first head over authored raw history, captured rows must match the full
original image at any committed slot or occurrence. Matching semantic content
cannot hide a moved row, changed timestamp or changed accounting envelope.
The existing leased preparation and publication check their captured windows.
Genuine uncommitted authored rows can still use immutable source escrow; their
ordinary append remains owned and unacknowledged until its actual commit.

Before a first head exists, a checked legacy raw window can retain canonical
ownership from its original committed row images, occurrence IDs and cached
costs. Literal user rows keep their historical role without admitting a task.
Independent raw copies encode that historical source explicitly, so sharing an
archive or producer with real task admissions cannot promote it to instructions.
Imported native groups stay protected even when their call and result are both
present. Reads and independent copies can retain descriptive pending legacy
source without acknowledging it. Durable normalization requires those exact
rows to settle through their original append owner; receipt admission alone
does not establish physical commitment.
The first model summary uses the same publisher as later epochs, which checks
the captured source again under its conversation lease. Editing that source or
replacing its authority prevents publication. Storage retries retain the original
proposal and recover its exact head after an uncertain acknowledgment.
Legacy root TSVs use the existing metadata migration before head publication,
including archives whose IDs, costs, names and action catalog already exist.
The publisher keeps the conversation lock and the original raw bytes while
that owner moves the payload. Native custody must settle before the move;
promotion failures retain the existing rollback and restart-recovery behavior.
Later head-write failures leave complete migrated raw history, and storage
retries keep the original accepted summary and proposal.

Saved forks copy a typed head and raw membership through the existing locked
fork transaction. The child has independent payload backing and its own storage
revision chain while preserving the canonical epoch, original occurrence IDs and
costs. Its copied archive boundary retains the source's logical frontier, so
later authored rows stay available as arrivals. Later parent activity and parent
deletion cannot change the child's captured source. Copies retire retrieved MCP
grants and carry no execution authority or current permissions; they do not run
a compactor or acknowledge pending ordinary appends.

NoStore copies also preserve the accounting identity of unchanged trusted
guidance when its original archive carrier is covered. Freezing and publishing
the copy keep that descriptive baseline; the child's current trusted settings
still supply all instruction bytes.

Subagent forks carry the captured typed head through their existing initial
append owner. Retained original raw occurrences receive independent contiguous
child rows and keep their IDs, costs and canonical ordering. The initial receipt
binds the planned root and all required payloads; interrupted publication recovers
that exact root before readiness. Preparing a child history does not acknowledge
the write. Subsequent compaction checks the actual parent after initial receipt
recovery. An old initial retry cannot replace a later child head.
The initial commit also indexes its own copied rows, allowing later compactions
to skip already archived source during ordinary history reads. This does not
acknowledge pending parent writes or infer missing indexes in legacy history.

A provider-context rejection resumes its original bounded retry only after that
acknowledgment is accepted. Inference completion and physical publication alone
do not release the waiting provider. Recovery keeps its original diagnostic
reason and completes the existing retry state; duplicate completion does not
start another attempt. Observed-input compaction keeps its separate reason,
and idle manual compaction creates no provider continuation.
Active-turn admission requires the same canonical source as manual compaction.
A legacy planner proposal supplies optional output guidance, never replacement
source or permission to use a separate turn-local compactor. Missing source
ownership fails before model queue admission, and a completed response cannot
publish through a historical writer after losing its accepted owner.
The same check precedes transport retry, smaller-input recovery and malformed
output correction. Losing either the canonical source or its original storage
owner settles that compaction rather than reconstructing ownership from the
current store or issuing another auxiliary request.
Canonical recovery keeps the configured raw-tail reservation. A small completed
history can still make progress by summarizing its oldest eligible complete
unit, even when the optional reservation could hold all closed source. Active
instructions and unfinished groups remain protected. The recovery request does
not ask for an empty summary because an older projection found no work.

If an auxiliary provider rejects its input, Mez keeps the original selected
prefix, configured raw tail, parent and arrival frontier. A newest closed group
that exceeds the reservation stays selected; retries cannot move it into the
raw tail. Smaller temporary chunks and final synthesis still cover the complete
original selection. New arrivals and unfinished ownership cannot enter it. Changed
source or profile fails against the original claim before retry admission.
Temporary compactor requests render selected history as source data. They keep
source labels and ordering with the existing secret redaction, without loading
fragments as another conversation. Completed user tasks remain historical and
current instructions stay protected.
Safe completion settlement observations enter the original canonical context
before archive projection. Repeated staging reuses their occurrence and cost;
publication and cold replay preserve that same evidence. These observations
cannot complete an action group or acknowledge an ordinary archive append.
A valid final goes directly to checked publication. There is no complete
ordinary-request fit check or shortening loop between that result and the
publisher. Large exact arrivals remain raw, even if they make the next request
too large. Ordinary bounded recovery never authorizes truncating them.

Manual preview uses that same typed owner and normal next-prompt composition.
It keeps native call/result ownership, ordering, costs and exact references;
previewing does not start a task or acknowledge a write. Request-worker results
and previews stay bound to the original admitted source and model; equal
conversation labels, generation and display counts cannot substitute another
operation's source. An installed replacement store still cannot retarget or
cancel that already accepted operation. A delayed ordinary copy
of source already backed by the epoch is deduplicated both while pending and
after its real append receipt. Manual summary guidance is advisory too: complete
fit and bounded output decide acceptance. Oversized candidates must make actual
size progress to retry, and only the final publication receipt changes replay.

Neutral references stay exact when a new task starts, including documents whose
source family also supports task instructions. Explicit task-owned local messages
retain their separate completion lifecycle.

At this typed acceptance boundary, retained MCP retrieval evidence keeps its
original bytes, identity and local cost, but stops granting callable tools.
Retrievals arriving during publication are also revoked. Preparing a candidate
or failing publication leaves live availability alone; a duplicate acknowledgment
does not revoke a fresh retrieval made after acceptance. The provider's tool
selector checks these restrictions on both the live body and cold history.
Nested checkpoint versions two and later persist restrictive retrieval IDs, including an
explicit source-copy restriction before the first epoch. Original version-one
checkpoints retain their receipt framing and derive older-epoch restrictions
from their checked member births. These restrictions never grant action or
permission rights; fresh retrieval still needs current registry validation.

Nested checkpoints retain the version-three original execution interval
UUID and generation for retained groups. Cold restore allocates fresh live
handles; matching descriptive identity cannot revive an old completion handle.
An old closure cannot release a renewed interval, even when its source frontier
is unchanged. Older checkpoints retain explicit unknown ancestry until original
source qualification; they cannot guess a renewal or export a producer closure.
Imported source extending a closed group keeps provisional protection until the
original producer's renewed interval is qualified, including across restart.
Version four also carries inert handled-source metadata bound to the actual
head, input/tail policy and provider projection. It fingerprints the originally
closed unselected tail, keeping post-freeze arrivals and later genuine closure
fresh. NoStore, restore and fork preserve it without granting work or durability
rights. Versions one through three remain readable without inventing a fence;
their next actual publication records it. Current model projection is selected
before automatic admission, including when a restored view has no cached model.
Ordinary saved rows carry original lifecycle controls separately from model
blocks. Replay checks their complete original group membership, producer and
interval before applying closure; a displayed response or matching group name
cannot substitute. The controls create no model occurrence or execution rights.

New retrieval evidence also keeps its original turn/action identity and replay
scope in the version-two manifest. An identical fresh retrieval is therefore
distinct from older revoked evidence, while duplicate results remain identical.
Later archive settlement cannot change the scope: a pre-acceptance retrieval
persisted after the head write still cannot grant tools on cold replay. Missing
or mismatched scope after a rewrite requires fresh retrieval. Original unscoped
manifests remain evidence and can grant before the first rewrite. This framing
changes neither configuration nor the enclosing transcript-record version.

Tasks admitted with empty captured history establish canonical ownership before
provider execution. They start at revision zero without a summary head; this
does not save a new archive or acknowledge an append. First compaction backs its
actual source and retains the exact user instruction even if no ordinary archive
exists. Before the first summary, fully accounted saved raw history can restore
the same original occurrences and recorded task boundaries in a new runtime.
This read creates no summary or append acknowledgment. Imported execution groups
stay protected; terminal-looking text cannot supply their completion receipts.
Live responses containing only successful local `say` actions retire their
execution fence after actual presentation. They capture that fence at the new
result append, so a duplicate response cannot close a later source or renewed
protection interval. Mixed batches and asynchronous completion owners keep their
own retirement gates. NoStore continuation retains this actual live closure;
saved raw replay restores it only from qualified original lifecycle records.
Original task input can be backed by immutable publication before its ordinary
append settles. Its active instructions and unfinished execution remain protected.
First publication keeps the original producer of each imported member. Legacy
or mixed raw archives and older summaries still need their explicit source
transition.
Routed workers keep the complete owned chronology while rebuilding current child
policy. A child copy gets its own conversation scope and fresh controller
ownership; continued work in the same conversation retains its existing owner.
Imported parent and earlier worker occurrences keep their original IDs and costs
and are not reattributed to the new worker turn.

Active turns with canonical ownership keep their compactor claim until
the storage worker returns and the actor accepts its exact receipt. Storage
recovery retains the same prepared summary for at most three attempts; it does
not ask the model to summarize again. A lost callback can recover the actual
worker result. Cancellation fences an outstanding writer, and exhausted recovery
fails the waiting turn while unrelated runtime work remains available. Only an
accepted rewrite resets provider request chains and the session's cache lineage.

Typed NoStore copies capture the original head and exact raw membership before
the conversation binding changes. Later parent changes and replacement stores
do not alter that capture. Each target uses its own conversation scope, current
trusted instructions and fresh controller ownership; copied MCP retrievals must
be retrieved again. Fork-each-iteration loops reuse the original captured parent.
Both isolated loop modes return to the same unsaved parent view and cache
lineage. Their session checkpoint keeps only the original saved ancestor, if
one exists; it does not turn the temporary parent into saved history.
Accepted in-memory compaction and terminal cleanup retain the latest target
body for its next turn, including arrivals after the compaction acknowledgment.
This continuity lasts in the current runtime and does not establish restart
durability for the unsaved parent. A subagent fork from that compacted NoStore
body captures its exact current head, raw members and original producers,
including unsaved arrivals from a running task. The child gets current
permissions and starts without its parent's incurred usage. Its own initial
persistence gates establish independent saved history; deleting the parent's
older saved ancestor does not invalidate the child. Later parent progress
invalidates a pending capture's adoption rather than leaking into its contents.
A saved `/fork <new-conversation>` uses that same NoStore capture and the existing
fork transaction to make only its child durable. It does not need the older
saved ancestor, and does not save the parent. A changed source rejects adoption;
uncertain adoption retains the target for recovery.
New peer inputs keep their exact identities and contents in both raw and
compacted copies. Saving or replaying them does not acknowledge their transport
again.
A NoStore source before its first compaction copies accounted raw records and
their task and execution lifecycle metadata through the initial copy owner. The
child remains without a summary or compaction manifest until a real rewrite.
Its initial publication commits row fingerprints with the original accounting,
so the first rewrite can index those rows and later history reads can skip them.

An empty audit archive can still have a typed head and retained model members.
Presentation writes and saved-session discovery recognize that state while
keeping the archive entry count at zero. Discovery checks the saved header and
its original receipt; actual replay independently checks the required source and
payloads before admitting a request.
Saved typed sources can be forked repeatedly even with no audit rows. Each
target owns a genuine empty archive and its independent epoch. If a retained task
root still references an earlier covered prelude, that original backing travels
with the copy without returning the prelude to the model. Removing required
boundary backing is corruption, even when those bytes are outside model history.
Saved copies validate the original indexed cut and bind those row fingerprints
to the child's actual copied bytes before publishing its independent root.

New publications, source-preserving roots and raw-content revisions use sidecar
version twelve. It records an explicit summary or intentional-omission policy;
only the latter permits an absent head. A raw epoch remains typed even with no
retained members or physical child rows. Reopen and both fork paths preserve
that epoch and its mode without consulting current configuration or restoring
omitted source from the archive. A missing legacy summary remains corruption.
Older headed formats keep their original encodings and immutable receipt hashes.
The current format retains version-seven
original-source escrow and version-eight independently backed saved-copy roots,
and adds a distinct memory-origin root for frozen NoStore copies. A memory
origin records a capture identity rather than claiming a saved parent publication.
Version-eleven raw revisions and their version-twelve extension verify edits
against one immutable original epoch receipt. They keep the same optional head,
head policy, canonical epoch, occurrence
membership, source framing and lifecycle metadata; only edited raw bytes and
their estimates change. Repeated edits reference that same original receipt,
and a saved fork independently backs the edited source. This validation is
separate from the edit transaction's raw archive and row-accounting recovery.
Saving an editable raw row checks its pane and full-row revision under the same
conversation lock as compaction. A bounded operation intent binds the old and
new archive images, exact parent/replacement receipts and selected accounting.
After that intent is synced, a later lock owner finishes the same replacement
before other mutations. Changed or missing recovery source fails visibly and
retains the intent. Covered rows remain protected; the physical archive cut
alone does not make a retained raw member immutable. Native lifecycle and
custody controls cannot be created or changed through ordinary content editing.
An edited epoch still needs its immutable original-epoch proof and the raw source
that proof describes. An interrupted save additionally needs its exact operation
receipts until recovery finishes. Missing required source blocks recovery and
cleanup while preserving the intent. Conversation cleanup removes its own
namespace; saved forks keep independent backing. Archiving and restoring an
edited conversation preserves its optional head, edited bytes and required proof.
If ordinary rows arrive after several summaries, replay
checks their original turn, occurrence, bytes and native metadata before
deduplicating them. Earlier covered source and display copies do not reenter
the model, and a delayed stable snapshot does not override the frozen baseline.
New task admissions record the first original member of their task suffix, so
replay retires the prior task before importing the new prelude and prompt. The
original bytes, IDs and costs remain intact. Steering and peer messages keep the
current task protected. An older user record without an admission witness that
would create two active prompts is refused explicitly. Version-five and
version-six sidecars can advance through checked publication without inference;
they do not gain missing lifecycle witnesses from labels or text.
Supported new authored rows now join the frozen typed base once through the same
history worker. Pending rows must retain their original immutable append batch;
replay preserves original occurrences, costs, native ownership and explicit
user/prompt/catalog sequence reservations. The original history capture carries
their producer into a later publication, including when the ordinary batch has
not yet settled. Ordinary imported rows do not complete old tasks; only the
explicit recorded admission above can supersede them. Replay does not acknowledge
the append or execute native work. Duplicates, broken native order/providers, unowned pending
rows and rewound sequence reservations refuse the read. Unaccounted legacy
semantic arrivals and unsupported controls still require their own custody
integration. An immutable indexed archive cut now lets typed history reads skip
the covered physical prefix while preserving every current model member,
including exact raw tail members inside that cut. Preparation and publication
verify original append row images and the complete frozen prefix; reads check a
bounded boundary anchor and required index references before decoding the tail.
These reads do not audit arbitrary damage in older covered archive bytes.
Missing required current backing, a broken boundary or required index, and a
retired occurrence reintroduced after the cut refuse explicitly. That guard
also checks pending arrivals and surviving original placements whose required
fingerprint is missing. Old pending
overlaps still need their original batch IDs and complete row-image equality.

Older covered escrow backing can retire only after its actual indexed archive
placement is inside the cut. Current pre-rewrite source, unsettled originals and
whole still-required execution groups remain backed. This does not delete files
or acknowledge pending appends. Active escrow remains bounded; a full bound
produces an explicit refusal rather than lost source. Original append accounting
retains compact execution framing in the existing row-accounting index.
A delayed lifecycle control can therefore be checked after later heads retire
that group's escrow, without scanning or restoring the covered model history.
Missing required descriptors, changed producer framing or missing original row
fingerprints refuse explicitly. Older indexes can still use current qualified
escrow; reads do not invent or backfill older execution descriptors.
Ordinary receipt recovery
and settlement still use their existing archive validation. When all retained
ordinary receipts already have complete original accounting inside the cut,
history verifies their original row images, IDs, costs and required state, then
leaves those receipts with their settlement owner. This skips redundant replay
repair while preserving actual physical-source verification before ordinary
acknowledgment. Missing required accounting refuses and retains the receipt.
Uncovered/incomplete recovery and general bounded physical settlement still need
separate integration. Valid version-five escrow can upgrade through this
publication path without a model call.
The earlier version-four prototype still supports frozen empty-archive reads,
but writing a new epoch requires an explicit original-source escrow transition.
Resume never asks a model to invent that missing backing. Legacy prefixes use
the explicit normalization path described above; selective formats use the
visible refusal described below. Actor publication and copies use the same
qualified source and existing persistence owners.

Native suffix composition can now join a saved pending call to its later result
using the original execution group and provider. It preserves the saved source
and the new result's occurrence and cost; it does not repeat the call or invent a
second assistant. Missing metadata, broken ordinal order, mismatched providers
and orphan or duplicate results refuse composition. Unanswered calls remain
protected. Authored archive arrivals use this same combined-source validation;
unqualified ordinary source and bounded ephemeral arrivals remain unsupported.

The context percentage is the latest positive ordinary execution sample divided
by the model's context-window budget, not the size of a newly rebuilt request or
its `max_input_tokens` limit. Below 100% therefore does not establish that the
next request is under its input cap. Proactive compaction needs a fresh ordinary
response at or above that cap; prior-turn compaction and auxiliary usage are not
triggers. Installing a summary clears the displayed usage but keeps the old
pressure sample consumed until a new ordinary execution response arrives.
This also applies to accepted in-memory rewrites of NoStore conversations.
Latest-request cache reuse becomes unknown; cumulative expense
is conserved. Cleared display metadata is checkpointed before recovery queues
the ordinary continuation. An immediate checkpoint error fails the waiting turn
without admitting a continuation, rather than restoring the pre-summary sample.
Check the error and model limits, or start `/new` with a concise, verified
handoff. After a successful compaction, ask the agent to recheck important file
contents and results instead of treating the summary as exact evidence.

Retrieved MCP tool contracts are cleared by compaction. Explicit server
references survive, but the agent must retrieve a server's current metadata
again before calling its tools; see [MCP integration](mcp-integration.md).

### Older selective compaction formats

Saved version-2 selective state and version-3 state containing selective ranges
require explicit single-head normalization. Resume, prompt-history loading and
fork refuse that source visibly and keep its files intact. Other conversations
remain usable. Retrying does not merge distinct summaries, reintroduce covered
raw text or call a provider to create a replacement. New selective publications
are refused, including publications proposed by historical terminal certificates.
Historical certificate and selective-record descriptions above define retained
audit data and integrity checks; they do not enable another replay mode.
New initial copies cannot journal a selective projection. Recovery keeps an
incomplete historical receipt intact and reports the incompatible state. If its
original projection is already fully durable, recovery can retire that exact
receipt without republishing it; the conversation still requires normalization
before model replay.
Terminal append recovery likewise distinguishes an already covered certificate
from a pending selective rewrite. It can settle the former without publishing
another head; the latter keeps its receipt and reports normalization required.

## Resume and preserve saved work

Loading a complete saved prefix history converts its existing single summary
and active raw tail to the typed format without asking a model for another
summary. The original archive and summary identity remain intact. The
version-ten migration provenance remains supported in the current format and
keeps the original prefix sidecar as integrity-checked evidence;
missing or damaged required provenance fails visibly. Pending writes remain
owned by their existing persistence receipts. Historical execution groups with
missing completion evidence stay protected, without claiming a new producer
interval. Retrying an interrupted migration verifies the same frozen source and
reuses the committed summary; a changed source proposal is refused.
Recorded task boundaries keep their original event ordering. If an earlier
prelude is already summarized, its source remains available to validate the
boundary without adding that prelude back to the model's raw history.
This includes catalog snapshots retired by an earlier compaction.
An earlier MCP invalidation marker also keeps its retrieved execution group out
of model replay. This deliberate exclusion is not native-history corruption:
migration preserves the original group as audit backing without reviving its
tool grant. Malformed group ordering still requires repair, even when that
group's retrieval has been invalidated.

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

## Canonical epoch boundary

Canonical blocks retain occurrence identities and cached local input estimates
separately from their source text. Equal text at separate occurrences stays
distinct. Accounted typed records preserve this metadata through replay, validate
it against the actual source, and recompute stale estimates. Malformed reserved
records refuse context reconstruction instead of silently dropping history.
Saved guidance metadata does not restore old instructions over current guidance.
Stable guidance and current prompt/personality/directive/style preferences reuse
their validated records during construction, including after cold replay. A
changed preference keeps its occurrence while refreshing its estimate against
current text; removing it does not resurrect saved instructions. Unchanged slots
do not create temporary estimates before applying their saved records.
Current producers identify these slots independently of prefix order, so
removing an earlier preference does not replace another slot's identity. The
checked archive read retains the latest visible snapshot even when a raw window
or compaction covers its carrier. Ephemeral and historical bounds exclude later
rows from frozen source images and metadata adoption; malformed future metadata
does not deny an older bounded view. Covered carriers still pass their original
placement checks. Empty snapshots retire old claims. This adds no archive scan
or persistent cache.
When a fork omits the raw carrier, its version-2 initial accounting payload
retains the snapshot preceding the copied tail. The same receipt publishes it
with the original row records before readiness. It survives cold replay without
copying old instructions; later visible carriers replace it. Historical child
views do not borrow later retained snapshots. Older version-1 initial payloads
explicitly have no stable baseline and keep their original recovery encoding.
Saving or copying an inherited conversation preserves this baseline through
nested forks, using the target's own initial proposal and future-row namespace.
An invalid required source marker refuses the copy before creating its target.
These estimates are not provider usage or billing, and their metadata is not
part of the semantic source supplied to the model.

New ordinary appends retain their original occurrence proposal while queued,
in flight and committed. Cancelling a queued append and creating another at the
same row slot gives unaccounted rows new IDs even when all raw fields match.
Retries retain the original proposal. The version-6 append receipt binds that
proposal to exact rows; worker commit and cold recovery publish raw rows and
accounting under one source lease before completing the receipt. A stale capture
cannot adopt another producer's committed occurrence. These IDs do not replace
worker claim or durability checks, and do not change raw text or roles.

Legacy raw rows also retain their original occurrence through saved forks,
content edits and deletion of preceding rows. Their private conversation-owned
`row-origins.sqlite3` file keeps placement separate from the raw transcript;
importing it does not change source text or roles. Keep this file with saved
conversation data. Once metadata marks it required, missing or corrupt state
fails the affected operation visibly instead of inventing replacement IDs.
Interrupted resequencing recovers only against the exact old or new transcript.

The version-2 projection also saves each original row's local cost estimate.
Cold replay, saved forks and survivor resequencing reuse valid records. Editing
one row or detecting a stale estimate repairs that row's cache against its actual
rendering; the original occurrence stays the same. Cost publication uses the
same source lease as the successful replay, and pending rows receive durable
caches only after their original rows commit. A failed projection publishes no
costs. These records remain descriptive metadata outside model-visible source.

This includes stale estimates inside authored context records: their repaired
costs are saved beside the original occurrence and reused on cold replay. The
original envelope stays intact, including its old descriptive claim. Current
format, identity and rendered content are still validated before costs are used;
an already valid authored record needs no duplicate cache entry.

Forked children carry these original row records through their initial append
receipt even when the parent has no summary. The child gets independent row
positions and a future-row namespace while inherited occurrences keep their IDs.
Pane-dependent labels can change rendering, so the batch prepares those revised
estimates once for the actual child pane. Initial context and cold replay reuse
them. Readiness waits for raw rows, identity/cost mapping and any inherited
summary authority to settle; interrupted publication retains its exact receipt
for recovery. A retry cannot adopt another equal-text proposal or roll back a
newer valid cost cache. Pending work from another store path is excluded.

The pure epoch API tracks stable, summary, raw and eligible raw estimates as
membership changes. Its frozen selection keeps the previous head, selected raw
source and retained tail separate; later arrivals do not change those frozen
estimates. Group reopening and explicit retirement change eligibility without
rewriting original source. These ordinary-framing totals exclude fixed request
overhead and require a separate provider-native visibility projection.

The epoch API can select a provider/API/model view that follows request replay:
incomplete, foreign or interleaved native groups retain neutral history, while
complete native groups avoid charging their neutral duplicates. New source
updates its affected groups; explicit retirement changes eligibility. Provider
changes reuse validated block estimates, and a changed provider/model descriptor
invalidates an earlier frozen publication plan without changing source or usage.

The shared canonical API defines an explicit summary or intentional-omission
head policy and exact retained raw membership. Summaries are reference material:
they cannot enable tools or grant
access. Current controlling instructions and unresolved call/result groups stay
exact, while explicitly completed task instructions can become historical
source without changing their authorship. Covered raw source remains archive
history rather than becoming active replay again.

For summary policy, the shared epoch model-work API packs that frozen selection
using the actual
auxiliary request shape. A fitting source takes one call. Oversized source uses
bounded temporary chunks and necessary merges, preserving redacted bytes and
whole-group publication ownership. A completed usable final is accepted without
word-target, compression-ratio or next-request-size retries. Empty, malformed or
incomplete output has finite private repair; transport retry resends the same
work. Intermediate summaries never become visible history. These are model-work
contracts; trigger integration and runtime settlement remain separate adapters.

The product runtime connects both modes to all three triggers, durable restore,
forks and the same ordered publication/receipt/adoption owners. Raw-tail policy
bypasses the model-work API entirely. See the
[architecture contract](../contributing/architecture.md#product-composition) for
those boundaries; an in-memory completion marker is not a durable receipt.

## Related pages

- [Subagents and messaging](subagents-and-messaging.md)
- [Cache status and diagnostics](../operations/cache-status-and-diagnostics.md)
- [Agent shell](../using-mezzanine/agent-shell.md)
- [Normative context contract](../../SPEC.md#96-context-assembly)

## Next step

Configure or select a model through [Providers and models](providers-and-models.md).
