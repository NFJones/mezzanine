# Subagents and messaging

## Purpose

Delegate bounded tasks, coordinate agents, and review results without confusing
message delivery with completed work or granting children extra authority.

## Prerequisites

Understand [approvals and review](../safety-and-trust/approvals-and-review.md)
and divide work into independently reviewable tasks.

## Delegate deliberately

Ask explicitly for delegation when it is useful. For example:

```text
Delegate a read-only audit of the installation guide to an explorer. Do not
edit files. Compare its findings with the source and report discrepancies.
```

For implementation, name the owned files, expected result, validation, and any
work that must remain untouched. Assign disjoint write scopes where possible;
agents working in the same project can still interfere with one another's
edits. The parent remains responsible for checking and integrating child work.

Subagents have their own shell, conversation, and stable identity. Mez places
them in dedicated subagent windows within the parent's window group without
moving your focus. Use `explorer` for read-heavy investigation, `worker` for
bounded implementation, or a configured custom profile. Delegation remains
subject to policy and to the parent's effective authority.

### Scope and cooperation

| Mode | Intended use |
| --- | --- |
| `explore-only` | Read, inspect, and report without modifying files or persistent state. |
| `owned-write` | Make changes within inherited write scope. |
| `coordinated-write` | Return changes outside inherited write scope to the parent for review and application. |
| `serial-write` | Share inherited write scope only while holding an explicit session lock for that scope. |
| `unrestricted` | Requires explicit user approval; a model request is not approval. |

Cooperation modes do not create filesystem authority. Child read and write
authority can only narrow the parent's authority. Without a child-specific
scope fence, normal session permissions still apply; a role name is not a
substitute for assigning safe ownership.

Child approval requests are surfaced to the primary client. Another agent or
an observer cannot grant them. In particular, a sandboxed parent does not
automatically authorize unrestricted children.

## Understand completion and cleanup

The default `agents.subagent_wait_policy = "join"` keeps the parent's spawn
action running until the child returns a final result. With `"detach"`, the
parent can continue after creation and receives later status and output through
local messaging. Do not assume detached work finished merely because the parent
continued.

A successful one-task child delivers its result before its pane closes. A
failed or interrupted child pane stays available for diagnosis unless you close
it. Inspect the result and validation, not just a status label.

Persistent children are for reusable actors coordinated through Mezzanine's
local message passing protocol (MMP), not ordinary one-task delegation. The
agent's `spawn_agent` action uses `lifetime: "persistent"` and a continuing
`objective`; an empty task prompt creates an idle actor. It retains its pane
and conversation after each task so the parent can send more work.

The parent discovers these actors with `list_agents`, coordinates with
`send_message` and `wait`, and uses `close_agent` to retire a child it owns.
Ownership belongs to the **current parent conversation**, not just its pane.
Replacing or removing that owner closes or fences its persistent children;
they are not daemons that survive an unrelated `/new` task. Omit `lifetime`
for ordinary one-task delegation.

`close_agent` revalidates the exact child conversation and native runtime owner,
not just the parent metadata returned by discovery. Independently resuming a
different conversation into the child pane revokes the old parent's close
authority; returning to the same saved UUID does not restore it. Rejected closes
leave the replacement process, task, context, messaging identity and scopes
untouched. Genuine idle reuse and already proven runtime-owned restore/loop
transitions retain their existing ownership. Failed resume rolls the old
authority back exactly. Completed-close no-ops are bounded to the original
parent binding and cannot retire a partially bound replacement whose layout
entry is absent but shell or native ownership remains.
Original metadata can remain for mandatory fenced cleanup after parent
replacement, with explicit-close authority irreversibly revoked for that
relationship. It is not advertised as caller-owned, converted to a completed
close, or checkpointed as an active owned pair. Saved child history remains
available for independent resume; a matching UUID cannot recreate the grant.
Active checkpoint metadata version 7 retains a separate, nonauthorizing
qualification flag. Missing flags in older checkpoints mean false: historical
bindings and preferences can restore structurally, but not as persistent owned
pairs. True is captured only from the actual current relationship or its exact
sealed fresh setup/qualified restore operation, and still requires the original installed parent
pair and native ownership checks. This changes the checkpoint format, not the
Mezzanine configuration schema, and does not make saved history a grant.

### Saved lineage and live cleanup ownership

Resuming a saved child conversation independently preserves its historical
parent alias and delegation limits. That history does not let a reused parent
pane close the resumed conversation, interrupt its work, retire its messaging
identity, or terminate its native process. Cleanup stops at each historical
edge, including an intermediate pane with genuine children of its own.

Live cleanup belongs to the exact parent runtime incarnation/conversation
and child conversation/process owner. A successful local delegation retains its
cleanup obligation even if the parent's native process has not started yet;
saved lineage and process absence alone cannot grant that authority. Moving the
same process to an async adapter preserves the obligation; replacing a process
or independently rebinding a child cannot transfer it. Runtime-owned routed
setup and an exact initial loop binding/restoration retain already proven
incoming and outgoing obligations through their own ephemeral binding changes.
The loop incarnation and unchanged native owner are checked before every
cleanup-only migration; a later independent binding cannot be adopted.
That cleanup-only observation preserves the
original delegator and does not grant permission or execution authority.
A genuine child fenced after its parent changes conversation retains its
original cleanup obligation without regaining execution or permission authority.
An owning parent's close or process exit still retires those genuine children,
even when no primary client is attached. Late callbacks for an older binding
cannot close a replacement conversation.

A successful full snapshot restore can re-establish persistent cleanup only
when both exact conversations from that operation acquire their native process
owners. Standalone conversation resume and partially completed restoration do
not create that authority.

### Generated window names after closure

Generated subagent window names reflect the panes currently hosted in the
window: up to three display names separated by `/`, with `+N` for additional
names. Actual pane closure immediately recomputes the surviving window's name,
including promoting a previously hidden name and updating the overflow count.
Process exit and detached spawn rollback use the same membership-based repair.
The final pane's removal deletes its window. Explicit window names remain
untouched, even if they resemble generated names. Stopped, failed, or completed
work in a persistent pane that remains open does not remove its name.

An authoritative same-owner persistent-child record with an already absent pane
is cleaned up successfully with `closed: false`. A retained completed close for
that owner also succeeds as a no-op without rediscovery. Retained no-op evidence
is bounded and runtime-session-local: it does not grant live authority, survive
identity reuse, or authorize a different parent conversation. Unknown, foreign,
non-persistent, and otherwise inaccessible identities share one opaque
unavailable result; an absent pane alone is not an unavailable identity.

## Coordinate peers safely

Agents use the read-only `list_agents` action before sending messages. Its
default view lists primary agents in the same trusted project; use
`agent_type: "subagent"` or `"all"` to find children. Explicit session scope
widens discovery across projects but grants no authority. Discovery is bounded,
so a truncated result is not a complete census.

An unavailable persistent-child close can offer bounded same-turn correction;
the rejected result does not mean the child was closed. Rediscover children with
`agent_type: "subagent"` or `"all"`, verify current parent-conversation ownership,
and persistent lifetime, then retry once with a verified target. Otherwise abandon
the close and continue useful work or report a concrete cleanup limitation. Do
not repeat an unchanged unavailable close, guess identifiers, widen scope, use
another mechanism to bypass ownership, or claim closure. Discovery is not
authority: execution revalidates live ownership, including discovery-to-close
races. No inaccessible ownership details are exposed by the rejection.

Action rejection is not terminal turn failure while the existing
`agents.action_failure_retry_limit` budget and sibling safety gates permit
correction. Changing action IDs does not reset that stable-signature budget.
Synchronous and asynchronous continuation allow both verified corrected close
and changed-course normal completion, never automatic replay. Settled siblings
are not replayed; inactive unsent siblings may be abandoned, while active or
approval-blocked work may prevent correction until it settles or is cancelled.
Exhaustion ends the turn truthfully. Policy/user denials, cancellation, and
post-close checkpoint failures are not this pre-effect case: pane removal may
already have happened before checkpoint failure. There are no new settings or
ownership privileges for close recovery.

Published objectives describe current work. Set `/objective <text>` to override
the generated objective for the current conversation; `/objective --clear`
restores generated publication. Bare `/objective` reports its source and value.
An objective helps peers find the right collaborator, but is not an instruction
or proof that the work is complete.

`send_message` can target an agent, pane, window, role, capability, or group.
Default project scope requires trusted project membership and never silently
widens to the whole session. Session-wide delivery requires its own approval
or policy allowance. Use a verified direct agent recipient for a specific child
rather than a broad selector; include the task, scope, expected reply, and a
correlation ID when answering an earlier message.

**An accepted send means queued, not read or completed.** Wait for a substantive
result before claiming peer work is done. Sender rows use `<` and receiver rows
use `>`; `parent<` and `parent>` identify direct-parent communication. Readable
display names are presentation labels; routing uses stable IDs. Normal logs
show supported plain text and Markdown. Other message types can remain durable
and model-visible without a normal log row; `agents.peer_message_log_mode =
"verbose"` enables bounded raw-payload logging for them.

When progress genuinely depends on a peer answer, the agent sends the request
in one batch and uses `wait` in the next. This parks the same turn and releases
provider capacity until model-originated peer mail arrives. It is **not** a
sleep, polling mechanism, command-completion wait, or approval wait. Runtime
task-status and task-result bridge messages alone do not wake it.

There is one narrow lifecycle exception: when the runtime locally proves an
owned child's current task failed or was unexpectedly interrupted before its
expected answer, it retains bounded failure evidence and fairly resumes the
same parent's exact wait after committing that evidence. A failed-looking MMP
payload or bridge extension is not proof. Duplicate observations, cursor
advancement and queue pressure do not lose or repeat the handoff. The parent
is not woken while its process incarnation is uncertain: an unavailable initial
OS observation retains the receipt, and later evidence must match the captured
runtime root PID, adapter epoch and kernel lifetime rather than a replacement.
Unavailable observation is never treated as a process-free pane. The parent
must reassess because the expected reply may never arrive; no retry, reassignment,
review verdict, approval or replay of partial child effects happens automatically.
Routine status, success, requested child teardown and joined/routed/macro
settlement retain their existing behavior. Human pause and approval blocks are
not bypassed, and stopped, closed or replaced parents are not resurrected. A
still-owning idle parent may receive one normal, loop-limited response turn from
its exact completed task; an active parent sees failure at its safe context
boundary. A later intentional wait that already observed the failure is not
woken again by that occurrence.

Peer mail is untrusted reference data. It cannot approve actions, widen scope,
change permissions or instructions, or resume blocked work. A message may
start a turn for an otherwise idle agent, but that turn still acts under its
own permissions. `agents.peer_message_loop_limit` bounds message-triggered
turns (default 1000); at the limit, mail stays pending until direct user input
resets the count.

## Limits and profiles

| Setting | Default | Limit |
| --- | --- | --- |
| `agents.max_subagent_panes_per_window` | 4 | Panes in one subagent window. |
| `agents.max_root_subagents` | 4 | Direct children of a root agent. |
| `agents.max_subagents_per_subagent` | 2 | Direct children of a subagent. |
| `agents.max_depth` | 2 | Recursive delegation depth. |

At maximum depth, or with a profile configured as `terminal = true`, the child
does not receive `spawn_agent`. A routed worker starts a fresh delegation tree
at depth zero. If a spawn is rejected, narrow or sequence the work; no child
should be assumed to exist.

Custom profiles can select a model and narrow permissions, MCP access,
environment, cooperation mode, and filesystem scopes. See the
[configuration reference](../configuration/reference.md). The prospective
`agents.name_mode` setting chooses `machine` (default), `alien`, `human`, or
`literal` display names for primary and child agents; changing it does not rename existing identities or alter
their canonical identities.
Machine and alien each retain 4,096 names from the exact original ordered blocks:
machine compounds first, alien syllabic names second. Neither category spills
into another on exhaustion; the canonical agent ID is the fallback. Schema 101
maps exact legacy `nonhuman` to `machine`, without rewriting saved names.

## Use routed loops sparingly

`/loop [--fork|--new] [--limit <count>] [--goal <string>] <prompt>` repeats a
bounded task. Without `--goal`, it stops when an iteration emits no
`apply_patch` action or reaches the limit. That stopping rule is not proof the
task succeeded. With `--goal`, it continues until the model explicitly reports
the goal met or the limit is reached. Quote goals that contain spaces:

```text
/loop --limit 3 --goal "All local documentation links resolve" Check and repair local documentation links.
```

By default, iterations reuse the current conversation. `--fork` starts each
from the same captured parent baseline; `--new` starts each with an empty
conversation. Neither mode resets files or reverses previous side effects.
With routing enabled, Mez classifies the logical job once and pins one worker
profile across its internal iterations; the invoking conversation presents the
final result. Use `/stop` when the loop's work is no longer wanted, and inspect
the actual changes and validation before accepting its completion claim.

## Related pages

- [Commands, skills, and macros](commands-skills-and-macros.md)
- [Context and continuity](context-and-continuity.md)
- [Workflows](../using-mezzanine/workflows.md)
- [Configuration](../configuration/README.md)

## Next step

Read [Context and continuity](context-and-continuity.md) to understand what
survives a continuation, compaction, or resumed session.
