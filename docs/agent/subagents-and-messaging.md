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
support scoped change coordination; `unrestricted` requires explicit authority.
Child read and write authority inherits from, and can only narrow, the parent.

The default join behavior waits for a child result before the parent continues;
detached work can report later through local messaging. Approval requests from
children are surfaced to the primary client and cannot be decided by observers.

## Understand limits, profiles, and cleanup

Delegation is bounded by `agents.max_subagent_panes_per_window`,
`agents.max_root_subagents`, `agents.max_subagents_per_subagent`, and
`agents.max_depth`. Their defaults are four panes per subagent window, four
direct children for a root agent, two children for a subagent, and depth two.
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
