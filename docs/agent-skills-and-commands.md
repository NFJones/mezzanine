# Agent Skills and Commands

This guide explains the user-facing command surfaces around the pane-local
agent.

Related docs:
- [README](../README.md) for quick start and example workflows.
- [Configuration reference](configuration-reference.md) for config paths and
  defaults that affect agent behavior.
- [SPEC.md Section 10.4](../SPEC.md#104-skills) for normative skill behavior.
- [SPEC.md Section 11](../SPEC.md#11-agent-shell-commands) for the baseline
  slash-command contract.

## The three command surfaces

### 1. Mezzanine terminal commands

Open the Mezzanine command prompt with `Ctrl+A :`.

- Commands entered there are parsed by Mezzanine, not by the pane shell.
- Use this surface for session, window, pane, and presentation control.
- Common commands include `new-window`, `split-window`, `select-pane`,
  `set-theme`, `list-keys`, and `show-options`.

Use terminal commands when you want to control the multiplexer itself.

### 2. Agent shell slash commands

Open the pane-local agent shell with `Alt+]`. The prompt is pane-scoped and
non-modal, so other panes and normal multiplexer navigation remain available.

Common slash commands:

| Command | Use it for |
| --- | --- |
| `/help` | Show the live command list. |
| `/status` | Inspect the current pane agent session. |
| `/model` | Inspect or change the active model selection. |
| `/approval` | Inspect or change the session approval mode. |
| `/permissions` | Inspect or change permission policy. |
| `/list-skills` | Show the effective skill catalog available to the pane. |
| `/list-mcp` | Show configured MCP tools. |
| `/compact` | Compact older conversation context. |
| `/new` | Start a fresh pane conversation. |
| `/resume` | Resume a saved pane conversation. |
| `/stop` | Interrupt the active turn. |
| `/exit` | Hide the agent shell. |

Use slash commands when you want to control the agent session rather than the
terminal layout.

### 3. Explicit skill invocation

You can invoke a skill by starting the agent prompt with:

```text
$<skill-name> [additional context]
```

Examples:

```text
$mez-manual show me the commands I need to inspect sessions and panes
$mez-config switch to the nord theme and show the exact setting path
```

Use `/list-skills` to inspect the effective catalog before invoking a skill.

## Built-in skills

The repository currently ships built-in skills including:

- `mez-manual`: Mezzanine terminal commands, slash commands, skill invocation,
  and common workflows.
- `mez-config`: supported live configuration changes and setting-path usage.
- `create-skill`: guidance for creating or updating OpenAI-structured skills.

## Where skills live

- User skills: `~/.config/mezzanine/skills/<skill-name>/SKILL.md`
- Project skills: `<project-root>/.mezzanine/skills/<skill-name>/SKILL.md`

Project skills are subject to project trust. Until the project root is trusted,
project-local skills should not be treated as active user-facing capability for
that pane.

## Which surface should I use?

- Use the **terminal command prompt** for panes, windows, themes, and session
  management.
- Use **slash commands** for model, policy, approvals, logs, conversation
  state, and pane-agent lifecycle.
- Use **explicit skills** when you want a reusable workflow prompt scaffold at
  the start of an agent turn.
