# Agent Skills and Commands

This guide explains the user-facing command surfaces around the pane-local
agent.

Related docs:
- [README](../README.md) for quick start and example workflows.
- [Configuration reference](configuration-reference.md) for config paths and
  defaults that affect agent behavior.
- [SPEC.md Section 10.4](../SPEC.md#104-skills) for normative skill behavior.
- [SPEC.md Section 10.5](../SPEC.md#105-agent-macros) for normative macro
  behavior.
- [SPEC.md Section 11](../SPEC.md#11-agent-shell-commands) for the baseline
  slash-command contract.

## The four command surfaces

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
| `exit` | Terminate the current session and exit Mezzanine. |
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
| `list-themes` | Show built-in and configured UI themes with short palette previews. |
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
| `/status` | Inspect the current pane agent session, pane-lifetime token usage across conversation switches, and mez-session retained-conversation token totals; pager output supports `/` search and empty `/` repeats. |
| `/model` | Inspect or change the active model selection. |
| `/thinking` | Toggle provider thinking mode when supported. |
| `/approval` | Inspect or change the session approval mode. |
| `/permissions` | Inspect or change permission policy. |
| `/directive` | Show or set a session-scoped developer-instruction addendum. |
| `/list-skills` | Show the effective skill catalog available to the pane. |
| `/sync-builtin-skills` | Resync Mez-managed built-in skill copies in the user config root and report created, replaced, preserved, and current entries. |
| `/list-macros` | Show the effective agent macro catalog available to the pane. |
| `/list-mcp` | Show configured MCP tools. |
| `/compact` | Compact older conversation context while opportunistically pruning expired persistent records. |
| `/issue` | Add, show, update, query, or delete local project issues for the active pane repository, including mutable progress notes. |
| `/show-issues` | Browse open project issues, apply filters, open record details, and save the Markdown view to a file. |
| `/show-memories` | Browse project-scoped persistent memories, apply filters, open record details, and save the Markdown view to a file. |

`/show-issues` and `/show-memories` use the shared command-output pager. `/` keeps the normal in-page text search, while record-browser keys provide database-backed actions: `k` opens the kind filter, `p` opens the project/scope filter, `x` opens the full-text filter, `s` opens the save prompt, `Enter` opens the focused record, and `Esc` closes prompts, returns from detail to list, or exits the list view.
| `/remember` | Generate durable memories from the current context or a supplied statement while opportunistically pruning expired persistent records. |
| `/loop` | Re-run a prompt until an iteration completes without `apply_patch` actions or the loop limit is reached; pass `--fork` to use fresh parent-conversation forks, `--new` to use fresh empty conversations, or `--limit <int>` to override the loop limit for that command. |
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

Use `/list-skills` to inspect the effective catalog before invoking a skill. Mez also silently materializes and refreshes managed built-in skill copies under the user config root on startup; use `/sync-builtin-skills` to run that same user-scope sync on demand. Managed copies include a `mez_managed_version` front-matter field, while user overrides without that field are preserved.

### 4. Explicit macro invocation

You can invoke an agent macro by starting the agent prompt with:

```text
#<macro-name> [additional context]
```

Macros are ordered prompt sequences stored as `MACRO.md` files. A macro run uses
one persistent subagent session for every step. Each step is submitted to that
subagent as a normal agent-shell prompt, so slash commands such as `/loop`,
explicit skills, and explicit MCP server syntax are interpreted with the same
runtime semantics and permission checks they would have if typed directly into
that subagent. The runtime submits each later step after the parent model returns a
bounded structured judgment: continue, continue with an adapted prompt,
stop as failure, or finish after the final step. The model judges results;
the harness owns step sequencing.

Use `/list-macros` to inspect the effective macro catalog before invoking a
macro.

## Built-in skills

The repository currently ships built-in skills including:

- `mez-reference`: Mezzanine terminal commands, slash commands, skill
  invocation, common workflows, and supported live configuration changes.
- `create-skill`: guidance for creating or updating OpenAI-structured skills.
- `create-macro`: guidance for creating or updating agent macros with
  `MACRO.md` front matter and ordered prompt steps.
- `add-doc`: guidance for saving durable documentation or reference content to
  memory as readable Markdown using the `documentation` memory kind.
- `add-issues`: guidance for turning recent concrete findings into local Mez
  issue tracker entries, including dependency relationships when findings have
  hard prerequisites. New prerequisite issues should be created first; dependent
  issues should be added in a later action batch with the real returned
  prerequisite issue id in `depends_on`.
- `add-research`: guidance for saving durable research findings to memory as
  readable Markdown using the `research` memory kind.
- `fix-issues`: guidance for working the local Mez issue tracker in dependency
  order until verified fixes are removed.

## Where skills and macros live

- User skills: `~/.config/mezzanine/skills/<skill-name>/SKILL.md`
- Project skills: `<project-root>/.mezzanine/skills/<skill-name>/SKILL.md`
- User macros: `~/.config/mezzanine/macros/<macro-name>/MACRO.md`
- Project macros: `<project-root>/.mezzanine/macros/<macro-name>/MACRO.md`

Project skills and macros are subject to project trust. Until the project root
is trusted, project-local skills and macros should not be treated as active
user-facing capability for that pane.

## Which surface should I use?

- Use the **terminal command prompt** for panes, windows, themes, and session
  management.
- Use **slash commands** for model, policy, approvals, logs, conversation
  state, and pane-agent lifecycle.
- Use **explicit skills** when you want a reusable workflow prompt scaffold at
  the start of an agent turn.
- Use **explicit macros** when you want an ordered workflow of normal
  agent-shell prompts to run through one persistent subagent session.
- Use `@<mcp-server-name>` in an agent prompt, or inside an explicit skill, to
  inject that MCP server callable metadata for the current turn only. In the
  agent prompt, tab completion and shaded shadow text work for `@` MCP server
  names in the same prompt-local manner as `$` skill names. MCP server catalogs
  are not globally included in ordinary prompts, and injected MCP details are
  ephemeral rather than durable transcript or later-turn context.
