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

## Work inside SSH and container shells

When the pane enters SSH, a container shell, a chroot, or another nested
interactive environment, Mezzanine treats that environment as a separate shell
authority. Agent work waits for compatible Mezzanine shell integration running
inside the nested environment. The integration must announce itself at a
completed prompt and pass a shell-native editor challenge before Mezzanine can
discover that environment's shell or send generated input. Host-side Bash,
Fish, or Zsh tokens and startup files are never reused across this boundary.

Install or explicitly activate the matching integration inside each remote or
container environment where agent commands should run. Mezzanine does not
silently edit remote startup files. An already-running unmanaged nested shell
cannot be identified safely from the local `ssh` or container-client process
alone; bootstrap therefore times out with an installation/activation message
instead of injecting a probe into a password prompt, full-screen program, or
unknown command line. After integration is admitted, shell-specific identity
and child-launch support determines whether that adapter can complete the
bootstrap.

For Bash, install `mez` inside the nested environment and explicitly activate
the current shell process:

```bash
eval "$(mez shell-integration bash)"
```

Run this only at an ordinary empty Bash prompt. Activation preserves the visible
prompt and installs the private receiver only in that Bash process; it does not
edit `.bashrc`. Mezzanine then challenges the adapter, discovers the nested
shell through its authenticated receiver, creates a temporary owner-only child
rcfile with a fresh token, and removes that temporary directory when the child
returns. Exit the nested environment to restore normal discovery of the local
pane shell.

For Fish, explicitly activate the current process at an empty prompt:

```fish
mez shell-integration fish | source
```

The Fish adapter uses a native command-line callback for its source-free
challenge and authenticated receiver stages. After identity discovery,
Mezzanine starts a startup-suppressed Fish child with a fresh receiver token in
its `--init-command`. The parent adapter does not reuse local Fish launch state.

For Zsh, explicitly activate the current process at an empty prompt:

```zsh
eval "$(mez shell-integration zsh)"
```

The Zsh adapter selects an unused managed ZLE trigger, uses that widget for the
challenge and authenticated receiver, and stages an owner-only temporary
`ZDOTDIR` for the fresh-token child. The temporary startup directory is created
inside the nested environment and removed when the child returns. Local
`ZDOTDIR`, startup files, tokens, and trigger admission are not consulted while
the foreign boundary is active.

For all three shells, bootstrap remains bounded and fail-closed. Mezzanine
releases the environment bootstrap only after the fresh child publishes an
authenticated installation event, then certifies the child process group with
fresh foreground observations. Exiting the nested environment clears that
authority and re-arms discovery for the original pane shell when agent mode is
visible.

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
