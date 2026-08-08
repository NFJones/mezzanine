# Key bindings

## Purpose

List the default Mezzanine prefix bindings and distinguish multiplexer, agent,
copy, and prompt input contexts.

## Prerequisites

Start an interactive primary client. The default prefix is `Ctrl+A`; user
configuration can replace bindings.

## Core prefix bindings

| Binding | Action |
| --- | --- |
| `Ctrl+A Ctrl+A` | Send the prefix key to the active pane. |
| `Ctrl+A :` | Open the Mezzanine command prompt. |
| `Ctrl+A ?` | Show effective bindings. |
| `Ctrl+A d` / `Ctrl+A D` | Detach the primary client / choose a client or observer to detach. |
| `Ctrl+A c` / `Ctrl+A C` | Create a window / window group. |
| `Ctrl+A ,` | Rename the current window. |
| `Ctrl+A w` / `Ctrl+A G` | Choose a window / window group interactively. |
| `Ctrl+A n`, `Ctrl+A p`, `Ctrl+A l` | Next, previous, or last window. |
| `Ctrl+A (` / `Ctrl+A )` | Previous or next window group. |
| `Ctrl+A 0`–`Ctrl+A 9`, `Ctrl+A '` | Select a window by index or prompt for one. |
| `Ctrl+A .` | Prompt for a destination and move the current window. |
| `Ctrl+A %` / `Ctrl+A "` | Split vertically / horizontally. |
| `Ctrl+A` then arrow keys, `o`, or `;` | Select an adjacent, next, or last pane. |
| `Ctrl+A q` | Display pane indexes and selection actions. |
| `Ctrl+A z`, `Ctrl+A Space` | Toggle pane zoom / cycle layouts. |
| `Ctrl+A x` / `Ctrl+A &` | Kill the active pane / current window. |
| `Ctrl+A !`, `Ctrl+A {`, `Ctrl+A }` | Break the active pane into a window / swap it with the previous or next pane. |
| `Ctrl+A [` / `Ctrl+A PageUp` | Enter copy mode / enter copy mode and scroll up. |
| `Ctrl+A ]`, `Ctrl+A #`, `Ctrl+A =`, `Ctrl+A -` | Paste, list, choose, or delete paste buffers. |
| `Ctrl+A O` / `Ctrl+A ~` | Manage observers / show Mez messages. |
| `Ctrl+A a` | Toggle the focused pane's agent shell. |

## Prompt and browser controls

In the Mezzanine command prompt and agent prompt, Tab and Shift+Tab select
enumerable completions; shadow hints do not alter the editable input. The agent
prompt recognizes `/` slash commands, `$` skills, `#` macros, and `@` MCP
servers. `Ctrl+V` pastes host clipboard text into the visible agent prompt
without submitting it.

Command-output pagers use `/` to search, and an empty search repeats the last
query. Record browsers use arrow keys to select stable identifiers, Enter to
open them, and Esc to close a prompt, return to a list, or exit the browser.

## Inspect effective bindings

Run `list-keys` in the command prompt or press `Ctrl+A ?`. This shows active
configuration sources and command expansions. Do not assume a key arrives when
a terminal emulator or nested multiplexer intercepts it; configure the binding
or outer environment deliberately.

## Related pages

- [Terminal input, copy, and history](../using-mezzanine/terminal-input-copy-and-history.md)
- [Terminal commands](terminal-commands.md)
- [Agent shell](../using-mezzanine/agent-shell.md)
- [Configuration reference](../configuration/reference.md)

## Next step

Read [Agent actions](agent-actions.md) to understand how agent work is
requested and reviewed.
