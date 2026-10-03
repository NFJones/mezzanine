# Agent overview

## Purpose

Explain the pane-local agent model, visible action lifecycle, and the limits of
the context it receives.

## Prerequisites

Complete [Getting started](../getting-started/README.md) and read the
[agent-shell guide](../using-mezzanine/agent-shell.md).

## Work from a pane

Each agent belongs to one pane and works from its shell-observed working
directory, conversation, configured guidance, and explicit action results. It
does not automatically receive the visible terminal buffer, scrollback,
alternate-screen content, or other panes. Ask it to inspect a file, run a
bounded command, or capture relevant output when that evidence is needed.

Before a turn, Mez bootstraps the pane environment and discovers applicable
tools. In the default native shell mode, commands run in fresh local shells
without typing into the interactive pane. They do not automatically run inside
an SSH session or container merely because one is visible there. Pane mode uses
the interactive pane shell, where a remote shell, full-screen program, password
prompt, or uncertain shell boundary can make commands unavailable. Use
`/shell-mode status` to check the transport, and follow readiness errors rather
than assuming a command reached the intended environment.

## Give a bounded task

State the goal, allowed changes, constraints, and how success should be checked:

```text
Review docs/getting-started/ for incorrect commands. Compare them with CLI
help. Report findings with file references; do not edit files.
```

For implementation, explicitly authorize the intended edits and name work that
must remain untouched. For explanation or review, the agent should inspect and
report rather than implement. Use `/plan on` when you want plan-only work and
`/plan off` before asking for edits. Use `/stop` to stop unwanted active work;
stopping does not undo completed actions.

## Review visible actions

The agent uses visible actions for reads, shell commands, patches, and external
integrations. Configuration determines which actions are offered; the runtime
still checks availability, permissions, and arguments at execution time.
Preview text while a response streams is an intention, not proof of execution.
Check settled action results and the final validation report before accepting
a completion claim.

Results become bounded conversation evidence. Recoverable mistakes can be
repaired, but already successful work should not be repeated merely because
another action failed. Inspect the actual changes and tests when results are
ambiguous, especially after interruption or a failed mutation.

Permission decisions remain runtime-owned. A model cannot grant itself host
access, filesystem authority, credentials, or a hidden local executor. Review
the requested action and its scope when approval is required.

### Live command output

When raw shell output is hidden, running commands show a bounded live tail below
the command preview. `terminal.shell_output_preview_lines` controls its maximum
wrapped display rows (five by default); short panes can show fewer rows. It
updates in place and is not saved as transcript history. The completed action
result, not the temporary tail, is the evidence retained for later turns.

## Related pages

- [Commands, skills, and macros](commands-skills-and-macros.md)
- [Context and continuity](context-and-continuity.md)
- [Approvals and review](../safety-and-trust/approvals-and-review.md)
- [Manual reference](../reference-manual/README.md)

## Next step

Use [Commands, skills, and macros](commands-skills-and-macros.md) to choose
the right interactive control surface.
