# Commands, skills, and macros

## Purpose

Choose between multiplexer commands, agent slash commands, reusable skills,
and ordered macros without confusing their scope or safety boundaries.

## Prerequisites

Open the [agent shell](../using-mezzanine/agent-shell.md) in a pane.

## Choose the control surface

Use `Ctrl+A :` and its command prompt for session, window, pane, copy, and
presentation control. It is parsed by Mezzanine rather than the pane shell.
Use the pane agent prompt and slash commands for the agent session itself.
Common commands are `/help`, `/status`, `/model`, `/approval`, `/permissions`,
`/list-mcp`, `/compact`, `/new`, `/resume`, and `/stop`.

Use `/help` and the command prompt's `help` output for the effective live
command catalog; bindings and capabilities can vary with configuration. The
manual reference covers CLI, key, action, terminal, and protocol contracts;
the live help surfaces remain authoritative for the available slash and
terminal commands.

## Use operational slash commands

Use the following agent-shell commands to control the current pane without
turning the command into an ordinary model request:

| Goal | Commands |
| --- | --- |
| Inspect or change authority | `/status`, `/permissions`, `/approval`, `/approve`, `/show-approvals`, and `/sandbox` |
| Control the current task | `/plan`, `/directive`, `/stop`, `/new`, `/fork`, `/resume`, and `/name-session` |
| Inspect or preserve context | `/compact`, `/show-context`, `/copy`, `/copy-context`, `/copy-patches`, and `/copy-trace-log` |
| Select model behavior | `/model`, `/routing`, `/latency`, `/thinking`, `/personality`, and `/list-personalities` |
| Work with local stores | `/memory`, `/remember`, `/show-memories`, `/issue`, and `/show-issues` |

`/approve` decides a pending action in the current pane; use
`/show-approvals` when the request may belong to another pane. `/sandbox`
reports or changes pane-local sandbox state, while advanced setup, profiles,
and managed-home cache operations remain under `mez sandbox`. `/plan on`
keeps the conversation in plan-only mode and removes write scopes for later
turns; use `/plan off` before asking the agent to edit files.

`/new` starts a conversation without prior context, `/fork` creates a branch,
and `/resume` returns to a saved conversation. `/compact` summarizes older
closed work and is intentionally lossy; use `/copy-context` or
`/copy-trace-log` only when the resulting diagnostic material can be handled
safely. `/memory` controls persistent-memory availability, while `/issue`
manages runtime-owned project issues rather than an external tracker.

## Invoke a skill or macro explicitly

Start a prompt with `$<skill-name>` to load a reusable skill, followed by any
task-specific context. Use `/list-skills` to inspect the effective catalog.
User skills live under `~/.config/mezzanine/skills/<name>/SKILL.md`; trusted
project skills live under `.mezzanine/skills/<name>/SKILL.md`. Project skills
remain untrusted content for security purposes and do not override approvals
or action rules, even after project trust enables their discovery.

Start a prompt with `#<macro-name>` to run an ordered macro. Macros use a
`MACRO.md` definition in the corresponding user or trusted-project macro root.
One persistent subagent runs the sequence, while normal prompt parsing,
permissions, and approvals still apply to every step. Use `/list-macros` before
invoking an unfamiliar macro.

Use `@<server-id>` only when a task requires a configured MCP server. That
server's callable metadata is available for the current turn, not permanently
added to the conversation.

## Related pages

- [Subagents and messaging](subagents-and-messaging.md)
- [MCP integration](mcp-integration.md)
- [Agent shell](../using-mezzanine/agent-shell.md)
- [Safety, trust, and security](../safety-and-trust/README.md)

## Next step

Read [Subagents and messaging](subagents-and-messaging.md) before delegating
work or using a routed loop.
