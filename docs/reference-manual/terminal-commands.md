# Terminal commands

## Purpose

Provide a compact reference for commands entered through Mezzanine's
in-session command prompt. These commands control the multiplexer and are
separate from shell commands, agent slash commands, and the process CLI.

## Prerequisites

Start an interactive primary client and open the command prompt with
`Ctrl+A :`. The default prefix can be changed in configuration.

## Syntax and discovery

The command prompt parses Mezzanine commands; it never sends entered text to
the focused pane shell. Commands accept shell-like quoted and escaped
arguments. Separate multiple commands with an unquoted semicolon.

Use `help` in the prompt for the effective command catalog and argument
syntax. Tab and Shift+Tab offer enumerable completions. Run `list-keys` or
press `Ctrl+A ?` to inspect the active bindings and their configuration
sources. The live prompt is authoritative because configuration and runtime
state can affect what is available.

## Common command groups

| Task | Commands |
| --- | --- |
| Manage windows and panes | `new-window`, `split-window`, `select-pane`, `resize-pane`, `rename-pane`, `list-windows`, and `list-panes` |
| Work with sessions and clients | `list-sessions`, `attach-session`, `detach-client`, `list-clients`, and `kill-session` |
| Copy and retain output | `copy-mode`, `copy-selection`, `paste-clipboard`, `paste-buffer`, `list-buffers`, `search-history`, `export-history`, and `clear-history` |
| Inspect and adjust the interface | `show-messages`, `list-keys`, `list-themes`, `set-theme`, `show-options`, `set-option`, `bind-key`, and `unbind-key` |
| Save or load layout state | `save-layout` and `load-layout` |

Some commands require an active runtime, control endpoint, or primary-client
authority. Use `help <command>` when available and review the resulting prompt
or approval rather than assuming a command affects a detached or observer
client.

## Related pages

- [Key bindings](key-bindings.md)
- [Sessions and panes](../using-mezzanine/sessions-and-panes.md)
- [Terminal input, copy, and history](../using-mezzanine/terminal-input-copy-and-history.md)
- [CLI reference](cli.md)

## Next step

Read [Agent actions](agent-actions.md) for the separate action model used by
the pane-local agent.
