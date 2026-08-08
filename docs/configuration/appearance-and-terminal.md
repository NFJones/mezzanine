# Appearance and terminal

## Purpose

Configure terminal compatibility, interaction, frames, themes, and history
without overstating what the terminal profile can emulate.

## Prerequisites

Read [Configuration overview](overview.md) and keep a working session available
to verify presentation changes.

## Configure the terminal surface

The `terminal` table controls the bounded terminal profile, pane `TERM` value,
color, mouse, bracketed paste, alternate-screen support, clipboard policy,
emoji width, and agent transcript wrapping. `xterm-compatible` is a supported
subset, not a claim to emulate every xterm feature. Use `terminal.emoji_width`
when status glyphs occupy the wrong number of cells in the host terminal.

`keys` configures the prefix and direct bindings; `frames` configures window
and pane status presentation; `theme`, `themes`, and aliases configure colors.
Use `Ctrl+A ?` or the `list-keys` terminal command to inspect effective bindings
before replacing one. Live-mutability is shown by configuration diagnostics.

## Preserve history and clipboard expectations

The `history` table controls bounded pane history and persistence. Clipboard
settings determine whether OSC 52 content is kept internally, copied to a host
clipboard integration, or rejected. Host clipboard commands receive copied data
on standard input and return pasted data on standard output; review them as
local integrations.

## Related pages

- [Terminal input, copy, and history](../using-mezzanine/terminal-input-copy-and-history.md)
- [Terminal compatibility](../reference-manual/terminal-compatibility.md)
- [Configuration reference](reference.md)

## Next step

Use [Agents, providers, and authentication](agents-providers-and-auth.md) for
agent behavior and model selection settings.
