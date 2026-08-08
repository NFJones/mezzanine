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
tools through the pane shell. A remote shell, container, full-screen program,
password prompt, or uncertain shell boundary can make non-interactive commands
unavailable. Return the pane to a usable prompt or follow the reported
readiness guidance rather than assuming a command was sent safely.

## Review visible actions

The agent uses visible actions for local reads, shell commands, patches, and
other local interaction. It can request capabilities and then receive an
action surface appropriate to the task. Results become bounded conversation
evidence, allowing the agent to repair recoverable failures without repeating
already successful work.

Permission decisions remain runtime-owned. A model cannot grant itself host
access, filesystem authority, credentials, or a hidden local executor. Review
the requested action and its scope when approval is required.

## Related pages

- [Commands, skills, and macros](commands-skills-and-macros.md)
- [Context and continuity](context-and-continuity.md)
- [Approvals and review](../safety-and-trust/approvals-and-review.md)
- [Manual reference](../reference-manual/README.md)

## Next step

Use [Commands, skills, and macros](commands-skills-and-macros.md) to choose
the right interactive control surface.
