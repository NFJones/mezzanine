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
manual reference is the canonical place for exhaustive command and key data.

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
