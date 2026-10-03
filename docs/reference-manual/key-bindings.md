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
| `Ctrl+A .` | Prompt for a new index for the current window. |
| `Ctrl+A %` / `Ctrl+A "` | Split vertically / horizontally. |
| `Ctrl+A` then arrow keys, `o`, or `;` | Select an adjacent, next, or last pane. |
| `Ctrl+A q` | Display pane indexes and selection actions. |
| `Ctrl+A z`, `Ctrl+A Space` | Toggle pane zoom / cycle layouts. |
| `Ctrl+A x` / `Ctrl+A &` | Ask for confirmation, then kill the active pane / current window. |
| `Ctrl+A !`, `Ctrl+A {`, `Ctrl+A }` | Break the active pane into a window / swap it with the previous or next pane. |
| `Ctrl+A [` / `Ctrl+A PageUp` | Enter copy mode / enter copy mode and scroll up. |
| `Ctrl+A ]`, `Ctrl+A #`, `Ctrl+A =`, `Ctrl+A -` | Paste the latest buffer, list buffers, choose the active buffer, or delete the latest buffer in buffer context. |
| `Ctrl+A ~` | Show Mez messages. |
| `Ctrl+A a` | Toggle the focused pane's agent shell. |
| `Ctrl+A e` | Open the visible agent-prompt draft in the configured external editor. |

## Copy-mode controls

Press `Ctrl+A [` to enter copy mode. These keys operate on retained output,
not the pane process:

| Key | Action |
| --- | --- |
| Arrow keys | Move the selection cursor. |
| `Ctrl+Up` / `Ctrl+Down` | Move five rows at a time. |
| `Ctrl+Left` / `Ctrl+Right`, or `Alt+Left` / `Alt+Right` | Move by words. |
| PageUp / PageDown | Scroll by a page. |
| Home / End | Move to the start / end of the line. |
| `Ctrl+Home` / `Ctrl+End` | Move to the top / bottom of retained output. |
| Space | Start a selection; press again after moving to copy it. |
| Esc | Leave copy mode. |

Copying updates the internal paste buffer and attempts a clipboard write when
available. Copy mode remains open after copying; press Esc to resume process
input. `Ctrl+C` is consumed without interrupting the process. See
[Terminal input, copy, and history](../using-mezzanine/terminal-input-copy-and-history.md)
for mouse selection and source-copy options.

## Prompt and browser controls

In the Mezzanine command prompt and agent prompt, Tab and Shift+Tab move forward
and backward through enumerable completions. Applying a completion replaces
only the active token and does not submit the prompt; shadow hints never alter
editable input. The agent prompt recognizes `/` slash commands, `$` skills,
`#` macros, and `@` MCP servers. `Ctrl+V` pastes host clipboard text into the
visible agent prompt without submitting it.

In the agent prompt, `Ctrl+J` inserts a literal newline; ordinary Enter submits
the draft. During a running turn, ordinary submitted text guides that task
rather than starting an independent one. In reverse history search, Enter
accepts the match without submitting it; submit with a later Enter.

External editing is also non-submitting. After a successful editor close, the
edited text returns to the in-pane prompt for review and normal submission.
While the editor is open, it exclusively owns the complete attached terminal:
Mez frames, prompts, overlays, and status rows are hidden, the editor receives
raw terminal input, and closing it restores the Mez display. The editor runs on
the session host, even when attaching remotely. It does not change the pane
shell's history, current input, or terminal screen.

Changed drafts that cannot be safely applied after an editor failure,
interruption, restart, or conflict remain in private host-owned recovery
storage. Run `/editor-recovery list` from the attached primary client to view
only bounded metadata, then use `/editor-recovery reopen <id>`,
`/editor-recovery apply <id>`, or `/editor-recovery discard <id>`. Reopening
lets you review the draft without applying it. Apply can fail if the target
draft has changed; successful apply or discard removes the saved recovery.
Observers cannot list or mutate recoveries.

Command-output pagers use `/` to search, and an empty search repeats the last
query. Record browsers use arrow keys to select stable identifiers, Enter to
open them, and Esc to close a prompt, return to a list, or exit the browser.

## Inspect effective bindings

Run `list-keys` in the command prompt or press `Ctrl+A ?`. This shows active
configuration sources and command expansions. Do not assume a key arrives when
a terminal emulator or nested multiplexer intercepts it; configure the binding
or outer environment deliberately.

For each configurable direct action, an omitted field retains the prefix
binding listed above. Setting the field to a chord installs that direct chord
and removes the corresponding built-in prefix action; setting it to `null`
disables both paths. `keys.edit_prompt` is the exception: it configures only the
suffix used after the prefix, defaults to `e`, and may also be disabled with
`null`. `list-keys` reports only the resulting effective bindings. A configured
prefix command still shadows any remaining built-in action on the same suffix.

Run `list-key-presets` to choose from the interactive preset table. The
`default` preset preserves the prefix-only defaults above. The `simple` preset
keeps `Ctrl+A` as the prefix and replaces 13 corresponding prefix actions:

| Direct binding | Action |
| --- | --- |
| `Alt+\\` / `Alt+-` | Split vertically / horizontally. |
| `Alt+=` / `Alt+Shift+=` | Create a window / window group. |
| `Alt+]` | Toggle the agent shell. |
| `Ctrl+Alt+Arrow` | Focus a pane in that direction. |
| `Ctrl+Alt+PageUp` / `Ctrl+Alt+PageDown` | Focus the previous / next window. |
| `Ctrl+Alt+Shift+PageUp` / `Ctrl+Alt+Shift+PageDown` | Focus the previous / next window group. |

Use `set-key-preset <name>` to apply and persist a built-in or configured
preset.

## Related pages

- [Terminal input, copy, and history](../using-mezzanine/terminal-input-copy-and-history.md)
- [Terminal commands](terminal-commands.md)
- [Agent shell](../using-mezzanine/agent-shell.md)
- [Configuration reference](../configuration/reference.md)

## Next step

Read [Agent actions](agent-actions.md) to understand how agent work is
requested and reviewed.
