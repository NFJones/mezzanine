# Agent shell

## Purpose

Use the pane-local agent prompt, its common controls, and its safe operating
boundaries.

## Prerequisites

Complete [Getting started](../getting-started/README.md) and authenticate a
provider for model-backed work.

## Open and use the prompt

With the default bindings, press `Ctrl+A a` to show or hide the agent shell for
the focused pane. Press and release `Ctrl+A` before pressing `a`. The prompt
appears at the bottom of that pane; it does not replace or merge history with
the pane's process screen. While visible, ordinary pane input goes to the agent
shell. Multiplexer bindings, pane navigation, resizing, and copy mode remain
available. Use `Ctrl+A ?` to inspect effective bindings if yours differ.

The agent works from the pane working directory, its conversation state,
configured instructions, and explicit action results. It does not passively
receive your full terminal screen, scrollback, or other panes. Include relevant
output in your request or ask the agent to inspect it explicitly.

Type a request and press Enter. Use `Ctrl+J` for a newline without submitting.
In native shell mode, `Ctrl+V` pastes host clipboard text into the prompt,
preserving multiline text. Completion supports slash commands, `$` skills,
`#` macros, and `@` MCP server names where enabled; Tab cycles candidates.
Use Up/Down to recall prompts and `Ctrl+R` to search them. Enter in reverse
search accepts the match without submitting it.

Large bracketed pastes appear as compact `[Pasted …]` blocks, but submission
sends the complete underlying text. History recall preserves these blocks.
Exceptionally large complete prompts can be submitted without being retained
for recall. An oversized or timed-out incomplete paste is discarded, along
with subsequent bytes until its closing delimiter arrives. Follow the status
bar notice; `Esc` at an idle prompt resumes ordinary input after a discarded
paste.

Press `Ctrl+A e` to edit the draft externally. Closing a successful editor
returns text to the prompt without submitting it. The editor runs on the
server machine in its own PTY, not through the pane shell, in both `native`
and `pane` modes. It does not alter the pane's shell history or command draft.

## Stop, hide, or start fresh

| Control | Result |
| --- | --- |
| `Esc` at an idle prompt | Clear the draft without hiding the prompt. |
| `Ctrl+D` on an empty prompt | Hide the agent shell. |
| `Ctrl+C` twice within three seconds while idle | Hide the agent shell. |
| `Ctrl+C` while a task runs, or `/stop` | Request interruption of the task. |
| `/new` | Start a separate conversation. |
| `/resume` | Open the saved-conversation picker. |

Hiding the shell asks an in-progress task to stop and blocks ordinary pane
input until the task reaches a terminal state. Non-slash text submitted while
a task runs steers that task rather than starting another turn. Prose is not
an approval decision.

After interruption, the next non-slash prompt continues from retained user,
assistant, tool, and steering context. It adds guidance without restarting
cancelled actions or processes. Use `/new` when you want an independent task
instead. Detaching the client is different from hiding the agent shell: a
normal detach leaves tasks running.

## Inspect and control a conversation

Use `/help` for available commands, `/status` for the pane's active model,
policy, context, and token state, and `/approval` for approval controls.
`/model` shows the active and configured profiles; `/model <profile-name>`
selects one. `/model --list` lists the active provider's model catalog.

Enter `/plan on` before a read-only investigation. Plan-only mode applies to
subsequent turns until `/plan off` or `/plan toggle` disables it. While enabled,
the pane has no write sandbox scopes; `/plan status` reports the mode. Enabling
it during active work requests that work stop. Plan mode does not replace
approval policy or operating-system confinement.

Use `/objective <text>` to set a durable, peer-visible objective for the
conversation. This overrides automatic objectives until `/objective --clear`.
Bare `/objective` reports the value and whether its source is `user`,
`automatic`, or `none`. Objectives are unavailable for ephemeral loop-worker
conversations; do not put secrets in peer-visible text.

## Review actions and context

The agent may request file reads, bounded commands, patches, configured MCP
calls, or scoped subagent work. Shell, network, destructive, configuration,
and some MCP actions can require approval. Approval policy does not itself
confine a permitted process; sandboxing is a separate boundary.

Streaming text and action previews are provisional. An accepted command or
action header shows intent, not execution or success. Read the settled result
before relying on an action. A confirmed file-mutation section proves that
section's effect, not whole-action success; a later failure does not imply
rollback.

Use `/show-context activity` to inspect recent retained action intent, outcomes,
and bounded result previews. Enter or click a row to open detail; Escape
returns to the list. `/show-context activity <sequence>` opens the activity
containing that presentation sequence. Press `y` to export versioned JSON
with the retained activity sources. Treat exports as potentially sensitive:
they can include commands, paths, and output. A queued clipboard copy does
not prove desktop delivery; adapter failures remain visible.

This inspection view does not alter conversation logs, approvals, or logging
levels. It reads at most the latest 200 cleartext presentation records within
8 MiB, so it is not a complete raw-output archive. Legacy records and missing
identities are not reconstructed.

Put repository-specific instructions in `AGENTS.md`. Project configuration
overlays under `.mezzanine/config.toml`, `.mezzanine/config.yaml`,
`.mezzanine/config.yml`, or `.mezzanine/config.json` remain pending until
explicitly trusted. Inspect trust with `mez sandbox trust list` before trusting
an unfamiliar root.

## Read and copy agent output

Structured agent output wraps at the smaller of the pane width and
`terminal.agent_wrap_column_cap` (120 display cells by default). Copy mode
recovers logical text rather than inserting presentation-only wrap boundaries.
Older ANSI-only saved records are replayed unchanged and may wrap differently.

The composer remains visible while the log scrolls or is in copy mode. In
explicit log-copy mode, the selection and keyboard input belong to the log;
Enter does not submit the visible draft. Drag across entered draft text or
double-click a word to copy it independently. For keyboard draft selection,
use `copy-mode --draft` from the Mezzanine command prompt. Use
`copy-selection --draft --format source` to include complete source from
intersected collapsed paste blocks. See [Terminal input, copy, and
history](terminal-input-copy-and-history.md) for selection controls and buffers.

## Choose a shell mode

The default `native` mode validates the pane's local root process and runs
shell-backed agent actions in fresh compatible shells without typing into the
pane. It can work while a full-screen application occupies the pane, but
stateful or interactive actions are rejected rather than redirected to it.
State such as a command's `cd` or `export` does not carry into the next fresh
shell. Native patch execution is currently shell-backed too: `native` does
not mean every action runs without child processes.

`pane` mode sends shell-backed work through the interactive pane shell. It
requires a supported Bash, Fish, Zsh, or POSIX `sh` shell ready at an empty
prompt. A full-screen program, password prompt, or uncertain shell boundary
makes injection unsafe. Runtime-created agent panes use bounded startup and
report a diagnostic on failure rather than remaining in bootstrap indefinitely.

Use `/shell-mode status` to inspect the effective mode. Select
`/shell-mode native` or `/shell-mode pane` for a pane-local override. Append
`--global` to persist the default for panes without an override. Pane-local
overrides are not durable across runtime restarts.

Native actions use a cleared base environment, not the daemon's full
environment. Optional values come only from `permissions.env_whitelist` in
the immutable snapshot captured when Mez starts, plus documented runtime
requirements. Pane-root metadata selects the shell and working directory;
later pane exports and startup-file changes do not change forwarded values.
If a command needs an extra value, review the whitelist and restart the owning
daemon to refresh its snapshot. Never put credentials in an agent prompt.

## Work inside SSH and container shells

Select `/shell-mode pane` before working in an interactive SSH, container,
chroot, or other nested shell. Native mode uses the pane's local root process;
it does not run inside that nested environment.

Before explicitly opening the agent shell in an existing foreign environment,
ensure it is at an empty interactive prompt. Agent entry asserts that this is
safe. Mez runs a bounded identity probe and launches an ephemeral managed
child through a `/bin/sh` loader. It needs no Mezzanine executable or
preinstalled shim there and does not modify startup files or install software.
Generated agent commands wait for bootstrap validation. Failed or incomplete
bootstrap refuses typed agent commands.

Selecting pane mode and entering the agent shell opts into the correlated
bootstrap as environment and path authority. Correlation detects stale or
mismatched records; it is not cryptographic attestation against the active
pane environment, which can observe and replay PTY traffic. Decide whether
that environment is appropriate for agent work before entering.

Do not enter from a password prompt, full-screen program, or unknown command
line. Return to an empty prompt or exit the nested environment. Exiting clears
its shell authority and re-arms local discovery while agent mode is visible.

## Related pages

- [Agent and integrations](../agent/README.md)
- [Safety, trust, and security](../safety-and-trust/README.md)
- [Configuration](../configuration/README.md)
- [Manual reference](../reference-manual/README.md)

## Next step

Use [Workflows](workflows.md) for bounded investigation, implementation, and
recovery patterns.
