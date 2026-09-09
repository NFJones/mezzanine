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
tools. The default native mode starts fresh shell actions from the validated
pane root process without injecting pane input. Pane mode uses the interactive
pane shell; in that mode, a remote shell, container, full-screen program,
password prompt, or uncertain shell boundary can make commands unavailable.
Return a pane-mode shell to a usable prompt or follow the reported readiness
guidance rather than assuming a command was sent safely.

## Review visible actions

The agent uses visible actions for local reads, shell commands, patches, and
other local interaction. The runtime provides the enabled action surface for a
request. That configured executable catalog remains stable across ordinary
turns until configuration changes; runtime validation still decides integration
availability, permissions, and arguments when an action executes. Results
become bounded conversation evidence, allowing the agent to repair recoverable
failures without repeating already successful work.

Permission decisions remain runtime-owned. A model cannot grant itself host
access, filesystem authority, credentials, or a hidden local executor. Review
the requested action and its scope when approval is required.

### Live command output

When raw shell output is hidden, running commands show a bounded live tail below
the command preview. `terminal.shell_output_preview_lines` controls its maximum
wrapped display rows (five by default). On short panes, the combined preview
window is also limited to the pane height, showing its newest rows without
discarding the retained source. New output replaces that window in
place. After completion, the next persistent log consumes the window from its
first row. Shorter output or removal leaves unused rows blank: it does not pull
earlier pane logs back down. Logs scroll upward again only when new output
reaches the bottom of the pane. The live tail is not saved as transcript history.

## Related pages

- [Commands, skills, and macros](commands-skills-and-macros.md)
- [Context and continuity](context-and-continuity.md)
- [Approvals and review](../safety-and-trust/approvals-and-review.md)
- [Manual reference](../reference-manual/README.md)

## Next step

Use [Commands, skills, and macros](commands-skills-and-macros.md) to choose
the right interactive control surface.
