# Agent shell

## Purpose

Use the pane-local agent prompt, its common controls, and its safe operating
boundaries.

## Prerequisites

Complete [Getting started](../getting-started/README.md) and authenticate a
provider for model-backed work.

## Open and use the prompt

Press `Ctrl+A a` to show or hide the agent shell for the focused pane. The
agent prompt appears at the bottom of that pane; it does not replace the pane's
process screen. Mezzanine retains the process and agent surfaces separately, so
showing, hiding, or rebinding a conversation does not merge their history or
screen state. While the prompt is visible, ordinary input for that pane goes to
the agent shell, while multiplexer bindings, pane navigation, resizing, and
copy-mode controls remain available. Hiding the shell asks an in-progress task
to stop and blocks ordinary pane input until the task reaches a terminal state.
The agent works from the pane working directory, its conversation state,
configured instructions, and explicit action results; it does not passively
receive your full terminal screen, scrollback, or other panes.

Type a request and press Enter. `Ctrl+V` pastes host clipboard text into the
editable prompt while preserving multiline text. Prompt completion supports
slash commands, `$` skills, `#` macros, and `@` MCP server names where enabled.
The prompt remains in this in-pane entry area by default. Press `Ctrl+A e` (or
the active key preset's `edit_prompt` binding) to request external editing;
closing a successful editor returns the text to the same prompt and never
submits it automatically.
Large bracketed pastes are shown as compact `[Pasted …]` blocks, but typed text
and smaller pastes remain visible literally. History recall and `Ctrl+R` restore
the same pasted blocks shown when the prompt was entered, and submission still
sends the agent the complete original text. The bounded history capacity can
retain a maximum-size bracketed paste with surrounding typed text; exceptionally
larger complete prompts are submitted normally but are not retained for recall.
Press `Esc` to clear a draft without hiding the prompt. `Ctrl+D` on an empty
prompt hides it. When no task is running, press `Ctrl+C` twice within three
seconds to hide the prompt; when a task is running, `Ctrl+C` requests an
immediate interruption. Non-slash text submitted while a task runs is steering
for that task rather than a separate turn.

Common controls are `/help`, `/status`, `/model`, `/approval`, `/new`,
`/resume`, and `/stop`. Use `/plan on` to enable pane-local plan-only mode;
it applies to subsequent turns until `/plan off` (or `/plan toggle`) disables
it. While enabled, the pane has no write sandbox scopes. Use `/plan status` to
inspect the current mode.

## Choose a shell mode

The default `native` mode validates the pane root process and runs each local
agent action in a fresh compatible shell without writing bootstrap input into
the pane. `pane` mode instead sends shell-backed work through the interactive
pane shell. Use `/shell-mode status` to inspect the effective mode,
`/shell-mode native` or `/shell-mode pane` for a pane override, and append
`--global` to persist the default for panes without an override.

Pane mode requires a supported Bash, Fish, Zsh, or POSIX `sh` prompt to be
ready for input. A full-screen program, password prompt, or uncertain shell
boundary makes injection unsafe; return it to an empty prompt. Runtime-created
agent panes use bounded startup and fail with a copyable diagnostic instead of
remaining indefinitely in bootstrap.

## Work inside SSH and container shells

When the pane enters SSH, a container shell, a chroot, or another nested
interactive environment, Mezzanine treats that environment as a separate shell
authority. Explicit agent entry asserts that the foreign shell is at an empty,
interactive prompt. Mezzanine immediately issues a bounded syntax-neutral
identity probe and, after resolving the shell, launches an ephemeral managed
child through a one-command `/bin/sh` loader. No Mezzanine executable,
startup-file modification, or preinstalled compatibility shim is required
inside the nested environment, and host-side Bash, Fish, or Zsh tokens and
startup files are never reused across this boundary.

This explicit empty-prompt assertion applies only to an existing user-owned
foreign environment. Runtime-created agent panes use their mode-specific
startup contract and never enter this foreign-shell discovery path.

Agent work waits for that dependency-free bootstrap to validate the foreign
shell before generated input is released. Mezzanine never silently edits remote
startup files and never installs software in the foreign environment.

An unmanaged nested shell that is not at an empty, interactive prompt cannot be
probed safely from the local `ssh` or container-client process alone; Mezzanine
will not inject input into a password prompt, full-screen program, or unknown
command line. Exit the nested environment to restore normal discovery of the
local pane shell.

For all supported shells, bootstrap remains bounded and fail-closed. Exiting a
nested environment clears its shell authority and re-arms discovery for the
original pane shell when agent mode is visible.

## Review actions and context

The agent may request file reads, bounded commands, patches, configured MCP
calls, or scoped subagent work. Shell, network, destructive, configuration,
and some MCP actions can require approval. Approval policy does not itself
confine a permitted process; sandboxing is a separate boundary.

Put repository-specific instructions in `AGENTS.md`. Project configuration
overlays under `.mezzanine/config.toml`, `.mezzanine/config.yaml`,
`.mezzanine/config.yml`, or `.mezzanine/config.json` remain pending until
explicitly trusted. Inspect trust with `mez sandbox trust list` before trusting
an unfamiliar root.

## Related pages

- [Agent and integrations](../agent/README.md)
- [Safety, trust, and security](../safety-and-trust/README.md)
- [Configuration](../configuration/README.md)
- [Manual reference](../reference-manual/README.md)

## Next step

Use [Workflows](workflows.md) for bounded investigation, implementation, and
recovery patterns.
