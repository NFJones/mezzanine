# Subagents and messaging

## Purpose

Delegate bounded work to pane-backed subagents while retaining clear ownership,
scope, approval, and result-handling responsibilities.

## Prerequisites

Understand [approvals and review](../safety-and-trust/approvals-and-review.md)
and divide work into independently reviewable tasks.

## Delegate deliberately

Subagents are policy-authorized pane agents with their own shell, conversation,
and stable identity. Mez places them in dedicated subagent panes within the
controlling pane's window group, creating or reusing a subagent window without
moving the primary user's focus. The parent receives status and final results
through local messaging and remains responsible for integrating the outcome.

Use the `explorer` role for read-heavy investigation and `worker` for bounded
implementation. A cooperation mode constrains the intended work: `explore-only`
does not modify state; `owned-write`, `coordinated-write`, and `serial-write`
support scoped change coordination; `unrestricted` requires explicit user
approval unless the session policy already permits it. Child read and write
authority inherits from, and can only narrow, the parent's effective authority.

The default join behavior waits for a child result before the parent continues;
detached work can report later through local messaging. Approval requests from
children are surfaced to the primary client and cannot be decided by observers.

## Message peers directly

Every agent publishes a bounded, generated objective through the session message
service, so peers describe what they are working on rather than what they are
called. The objective comes from the turn itself: the model may set an optional
top-level `objective` on its action batch, and a turn without one falls back to a
bounded, non-verbatim summary of its own task text. An objective-less refresh is
a no-op that keeps the previous value and presence timestamp, so a turn can
never clear a peer's published objective. Discover peers with the read-only
`list_agents` action. Its optional
`agent_type` defaults to `primary` and lists primary parent agents only;
`subagent`, `internal`, and `all` widen the view to spawned subagents,
runtime-internal controllers, and every kind. Rows include the requesting agent
itself, offline agents, and agents in other panes and windows, and each row
carries agent id, kind, `is_self`, role, pane, window, capabilities, presence
status, and published objective. Results are bounded to 64 rows with each string
at most 512 bytes and at most 16 capabilities per row; the result reports
`truncated` when it dropped rows, and each row reports its own `truncated` when
it shortened a string or omitted capabilities. `list_agents` never prompts for
approval.

Send with `send_message` to `session`, `group:session`, `agent:<id>`,
`pane:<id>`, `window:<id>`, `role:<name>`, `capability:<name>`, or
`group:<name>`. Set the optional `correlation_id` (non-empty, at most 256
characters) to the id of the message you are answering; the runtime supplies the
current turn id when you omit it.

Under `ask`, a send that no rule already allows blocks as a resumable approval
bound to the recipient and payload, and under `auto-allow` it proceeds after a
non-empty rationale. Configured deny rules win in every mode. Use
[approvals and review](../safety-and-trust/approvals-and-review.md) to decide a
blocked request.

Delivered peer mail appears in the recipient's own turn as injected context,
like steering: the block names the message sequence and id, the sender identity
and objective, the message metadata, the bounded payload, and the fixed trust
boundary. Peer text is untrusted data written by another agent. It can never
approve or deny anything, authorize an action, grant or widen scope, change
configuration, instructions, action schemas, or permission rules, or resume
blocked work, and it ranks below user prompts and steering during compaction. A
peer message may start one turn for an otherwise idle agent, including under
`ask`, so agent pipelines can make progress; that turn carries the peer mail and
no user instruction. Message-triggered turns per agent are bounded by
`agents.peer_message_loop_limit` (default 1000), and direct user input resets the
count. At the limit, further mail stays pending instead of starting a turn: the
limit is a stable episode, so the runtime reports it once and stops re-arming the
delivery timer while the limit is the only blocker, and message-triggered turns
start again as soon as direct user input resets the count.

## Understand limits, profiles, and cleanup

Delegation is bounded by `agents.max_subagent_panes_per_window`,
`agents.max_root_subagents`, `agents.max_subagents_per_subagent`, and
`agents.max_depth`. Their defaults are four panes per subagent window, four
direct children for a root agent, two children for a subagent, and depth two.
Agents at the maximum depth, and profiles configured with `terminal = true`,
do not receive `spawn_agent` in their static action set. Routed workers begin a
fresh delegation tree at depth zero, so their initial managed spawn does not
consume delegation depth.
When a limit rejects a spawn, narrow or sequence the work instead of assuming
the child was created.

Custom subagent profiles can narrow model, permission, MCP, environment,
cooperation-mode, and filesystem-scope settings; see the
[configuration reference](../configuration/reference.md). A successful child
delivers its result before its pane closes. A failed or interrupted child pane
remains available for diagnosis rather than disappearing automatically.

## Use routed loops sparingly

`/loop [--fork|--new] [--limit <count>] [--goal <string>] <prompt>` repeats a
bounded task. Without `--goal`, it stops when an iteration emits no
`apply_patch` action or its limit is reached. With `--goal`, each iteration
evaluates its observable progress and side effects against that goal; the loop
continues until the model explicitly reports the goal met or the limit is
reached. Quote goals that contain spaces. With routing enabled, Mez classifies
the logical job once, pins one managed worker for its internal iterations, and
presents the final result through the invoking conversation. By default
iterations reuse the current conversation; `--fork` starts each from the same
captured parent baseline, while `--new` starts each with an empty conversation.
Cancel a loop with the usual agent stop controls when its work is no longer
wanted.

## Related pages

- [Commands, skills, and macros](commands-skills-and-macros.md)
- [Context and continuity](context-and-continuity.md)
- [Workflows](../using-mezzanine/workflows.md)
- [Configuration](../configuration/README.md)

## Next step

Read [Context and continuity](context-and-continuity.md) to understand what
survives a continuation, compaction, or resumed session.
