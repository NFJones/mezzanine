# Terminal input, copy, and history

## Purpose

Use the command prompt, copy mode, paste buffers, and terminal interaction
without confusing Mezzanine controls with input sent to a pane process.

## Prerequisites

Have an interactive session open and know the prefix key from [Sessions and
panes](sessions-and-panes.md).

## Command prompt and completion

Press `Ctrl+A :` to enter the Mezzanine command prompt. It accepts multiplexer
commands, supports quoting and semicolon-separated commands, and does not run
text through the pane shell. Tab and Shift+Tab select enumerable command and
argument completions; shadow hints show a best match without changing input.

Use `help` in the prompt for its command guide. Use a Mezzanine command rather
than shell text when changing panes, windows, layouts, key bindings, or Mez
settings.

## Copy and paste

Press `Ctrl+A [` to enter pane-local copy mode. You can scroll normal terminal
content, move a selection cursor, and copy without sending keys to the pane
program. `Ctrl+A ]` pastes the most recent buffer into the active pane.

Default copy-mode controls are:

| Key | Result |
| --- | --- |
| Arrow keys, `Home`, and `End` | Move the copy cursor. |
| `Ctrl+Up` / `Ctrl+Down` | Move by larger vertical steps. |
| `PageUp` / `PageDown` | Move by one viewport page. |
| `Space` | Start a selection; press again to copy it. |
| `Escape` | Leave copy mode. |

`Ctrl+A PageUp` enters copy mode and immediately moves one page upward. Use
`Ctrl+A ?` to inspect effective bindings when configuration changes the
defaults.

The command prompt also provides `copy-selection`, `paste-clipboard`,
`paste-buffer`, `create-buffer`, `list-buffers`, `choose-buffer`, and
`delete-buffer`. Bracketed paste is used when the pane application supports it.
Host clipboard behavior depends on the terminal clipboard configuration.

Alternate-screen application content is not added to normal pane scrollback;
copying such a pane copies its currently visible text rather than hidden history.

## History and notifications

Use `search-history` and `export-history` for normal pane history.
`clear-history` clears bounded history after the applicable confirmation policy
without changing the current screen unless requested. Use `show-messages` for
diagnostics, pending approvals, and visible hook failures.

Command-output views support `/` text search. An empty `/` repeats the previous
search. Exact behavior and all default bindings belong to the manual reference.

## Related pages

- [Sessions and panes](sessions-and-panes.md)
- [Terminal commands](../reference-manual/terminal-commands.md)
- [Agent shell](agent-shell.md)
- [Manual reference](../reference-manual/README.md)

## Next step

Open the [Agent shell](agent-shell.md) to run a pane-local agent task.
