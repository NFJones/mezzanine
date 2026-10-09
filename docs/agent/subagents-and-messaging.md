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

## Coordinate peers safely

Agents use the read-only `list_agents` action before sending messages. Its
default view lists primary agents in the same trusted project; use
`agent_type: "subagent"` or `"all"` to find children. Explicit session scope
widens discovery across projects but grants no authority. Discovery is bounded,
so a truncated result is not a complete census.

An unavailable persistent-child close can offer bounded same-turn correction;
the rejected result does not mean the child was closed. Rediscover children with
`agent_type: "subagent"` or `"all"`, verify current parent-conversation ownership,
then choose a corrected close or continue useful work. Do not replay an unchanged
unavailable target or bypass ownership. Exhaustion ends the turn truthfully;
policy/user denials and post-close checkpoint failures are not this pre-effect case.

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
task-status and task-result bridge messages do not wake it.

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
