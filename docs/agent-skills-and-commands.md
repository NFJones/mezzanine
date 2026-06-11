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
- Terminal commands:

| Command | Use it for |
| --- | --- |
| `new-window` | Create a window with one pane. |
| `rename-window` | Rename a window. |
| `kill-window` | Close a window. |
| `select-window` | Focus a window. |
| `next-window` | Focus the next window. |
| `previous-window` | Focus the previous window. |
| `last-window` | Focus the previously active window. |
| `new-group` | Create a window group with one landing window. |
| `rename-group` | Rename a window group. |
| `kill-group` | Close a window group and its windows. |
| `select-group` | Focus a window group. |
| `next-group` | Focus the next window group. |
| `previous-group` | Focus the previous window group. |
| `last-group` | Focus the previously active window group. |
| `split-window` | Split the active or target pane. |
| `kill-pane` | Close a pane. |
| `select-pane` | Focus a pane. |
| `resize-pane` | Resize a pane. |
| `rebalance-window` | Reapply the active window layout. |
| `swap-pane` | Exchange two panes. |
| `break-pane` | Move a pane into a new window. |
| `join-pane` | Move a pane into another window or split. |
| `display-panes` | Show temporary pane identifiers for selection. |
| `list-windows` | Show window identities, names, active state, and sizes. |
| `list-groups` | Show window group identities, names, and active state. |
| `choose-group` | Open an interactive group picker. |
| `list-panes` | Show pane identities, active state, size, pid, and agent data. |
| `list-clients` | Show attached clients and pending observers. |
| `detach-client` | Detach a client. |
| `attach-session` | Attach to a resumable session. |
| `list-sessions` | Show resumable sessions. |
| `rename-session` | Rename the current or target session. |
| `kill-session` | Terminate a session and all panes. |
| `help` | Show the terminal command guide. |
| `copy-mode` | Enter pane-local copy mode. |
| `copy-selection` | Copy the active selection to a buffer and clipboard when available. |
| `paste-clipboard` | Paste host clipboard text into the active pane. |
| `paste-buffer` | Paste a named or recent paste buffer. |
| `create-buffer` | Create a named internal paste buffer. |
| `list-buffers` | Show paste buffers. |
| `choose-buffer` | Open an interactive paste buffer picker. |
| `delete-buffer` | Delete a paste buffer. |
| `show-messages` | Show diagnostics, pending approvals, and observer requests. |
| `list-keys` | Show effective key bindings. |
| `list-themes` | Show built-in and configured UI themes. |
| `set-theme` | Switch the active UI theme by name. |
| `bind-key` | Add or replace a live key binding. |
| `unbind-key` | Remove a live key binding. |
| `show-options` | Show effective options. |
| `set-option` | Set a live-mutable option. |
| `source-file` | Load a configuration file. |
| `refresh-client` | Redraw the client. |
| `refresh-provider-info` | Refresh cached provider model and quota information. |
| `agent-shell` | Show, hide, or toggle the agent shell for a pane. |
| `snapshot-session` | Create a structured session snapshot, optionally named with `--name`. |
| `resume-session` | Resume a snapshot by id or with `--latest [--session id]`. |
| `save-buffer` | Save a paste buffer. |
| `clear-history` | Clear bounded pane history. |
| `search-history` | Search pane history. |
| `export-history` | Export bounded pane history. |
| `pipe-pane` | Pipe future pane output to a file or command. |
| `mark-pane-ready` | Temporarily mark a pane as ready after risk acknowledgement. |
| `list-observers` | Show observer requests and approved observers. |
| `choose-observer` | Open an interactive observer picker. |
| `approve-observer` | Approve a pending observer. |
| `reject-observer` | Reject a pending observer. |
| `revoke-observer` | Revoke an approved observer. |

Use terminal commands when you want to control the multiplexer itself.

### 2. Agent shell slash commands

Open the pane-local agent shell with `Ctrl+A a`. The prompt is pane-scoped and
non-modal, so other panes and normal multiplexer navigation remain available.

Common slash commands:

| Command | Use it for |
| --- | --- |
| `/help` | Show the live command list. |
| `/status` | Inspect the current pane agent session; pager output supports `/` search and empty `/` repeats. |
| `/model` | Inspect or change the active model selection. |
| `/thinking` | Toggle provider thinking mode when supported. |
| `/approval` | Inspect or change the session approval mode. |
| `/permissions` | Inspect or change permission policy. |
| `/directive` | Show or set a session-scoped developer-instruction addendum. |
| `/list-skills` | Show the effective skill catalog available to the pane. |
| `/plugin` | Show read-only installed plugin status with `list`, `status`, and `inspect`; use `mez plugin` for lifecycle changes. |
| `/list-mcp` | Show configured MCP tools. |
| `/compact` | Compact older conversation context while opportunistically pruning expired persistent records. |
| `/remember` | Generate durable memories from the current context or a supplied statement while opportunistically pruning expired persistent records. |
| `/loop` | Re-run a prompt in the current conversation until an iteration completes without `apply_patch` actions or the loop limit is reached; pass `--fork` to use fresh parent-conversation forks. |
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
$mez-reference show me the commands I need to inspect sessions and panes
$mez-reference switch to the nord theme and show the exact setting path
```

Use `/list-skills` to inspect the effective catalog before invoking a skill.

## Built-in skills

The repository currently ships built-in skills including:

- `mez-reference`: Mezzanine terminal commands, slash commands, skill
  invocation, common workflows, and supported live configuration changes.
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
