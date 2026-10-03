# Terminal compatibility

## Purpose

State Mezzanine's terminal profile, supported behavior, limitations, and the
diagnostics to use when host rendering differs from expected behavior.

## Prerequisites

Know the active terminal profile and pane `TERM` setting from configuration or
diagnostics.

## Supported profile

The default `xterm-compatible` profile is a bounded implemented subset, not a
claim of complete xterm emulation. It supports cursor movement, screen clearing,
colors and styles, alternate screens, application cursor/keypad modes,
bracketed paste, focus events, mouse reporting, titles, clipboard requests, and
cursor save/restore within that subset. Focus reporting depends on the outer
terminal; clipboard requests remain subject to policy. Device-attributes
queries receive a conservative VT100-with-no-options response, not a claim of
all xterm features. General DCS controls remain unsupported except for the
synchronized-output markers below. Applications that require an unimplemented
extension may need a different mode or an ordinary terminal outside Mez.

### Synchronized output

Mezzanine implements DEC synchronized-output mode 2026 and the bounded legacy
DCS markers `=1s` and `=2s`. Applications can use these to avoid showing a
partially drawn screen: output continues to be processed, but the pane keeps
its previous display until the update ends. Resize, copy-mode entry, process
exit, and a safety timeout prevent a missing end marker from leaving the pane
frozen indefinitely. Repeated begin markers do not create nested updates.

### Application progress

Mezzanine also supports pane-local OSC 9;4 progress reports. Determinate
normal progress appears as a percentage pill immediately to the right of the
pane title and disappears on clear; warning, error, and indeterminate records
remove any stale percentage. Child panes receive an additive `P` in
`TERM_FEATURES` so tools such as Cargo can discover this support. The progress
state belongs to its pane and is not passed through to the outer terminal.
Custom pane-frame templates can display the active scalar with
`#{pane.progress}`.

### TERM and terminfo selection

Panes receive `TERM=xterm-256color` by default. Mezzanine-specific
`mez-256color` and `mezzanine-256color` entries describe the bounded
`xterm-compatible` profile and may be used when installed. When a requested
Mezzanine entry is unavailable, the safe installed fallback order is
`screen-256color`, `screen`, `vt100`, then `dumb`. If none is installed, Mez
uses its built-in `dumb` profile and sets `TERM=dumb`. Diagnostics expose the
selected profile, terminfo name, and degraded capabilities. The pane identity
describes Mezzanine's compatibility surface; it does not claim unrestricted
passthrough of the host terminal.

## Rendering and input boundaries

Mezzanine composes terminal cells, preserves wide-glyph footprints, and uses a
single emoji-width policy across rendering, prompts, and copy mode. Use
`terminal.emoji_width = "wide"` for two-cell emoji presentation or `"narrow"`
for one-cell text fallback terminals. The setting does not make all complex
emoji narrow.

Each displayed grapheme (one character with its combining marks) retains at
most 256 UTF-8 bytes. Extra combining characters beyond that limit are dropped;
subsequent text still renders normally. Restored display and history text use
the same limit. Ordinary accents, variation selectors, and emoji sequences
remain supported. This is a display limit, not a guarantee that a source-copy
record has been truncated in the same way.

Background colors on blank cells and normal terminal line wrapping are
preserved. A full-screen pane application does not itself switch the outer
terminal into its alternate screen; Mez manages the attached presentation.

Pane alternate screens are separate from normal history. Full-screen programs
can remain visible and explicitly captured, but their rows are not injected
into normal scrollback or default agent context. Host bracketed paste, mouse,
focus, application cursor, and keypad behavior follow the active pane mode
where supported. While a pane application has bracketed paste enabled, a host
paste is delivered as application input: pasted bytes that look like a Mez
prefix or mouse report do not trigger multiplexer actions.

When `terminal.enhanced_keyboard_reporting = true`, Mezzanine prompts on a
primary client temporarily request enhanced Kitty keyboard reporting from a
compatible outer terminal. The previous keyboard mode is restored when the
prompt closes or the client detaches. This option does not enable enhanced
reporting for ordinary pane input or observers.

External prompt editors run on the session host for both local and Iroh
attachments, not on the remote attaching machine. While editing, the editor
owns the whole attached terminal: prefix keys and pasted text go to the editor
instead of invoking Mez actions. Resizing continues to work, and closing the
editor restores Mez without changing the pane shell's screen or input.
Observers cannot edit. See [Key bindings](key-bindings.md#prompt-and-browser-controls)
for draft review and recovery commands.

The agent shell is a separate pane presentation surface whose prompt appears at
the bottom of its pane. While it is visible, ordinary process input is captured
by the agent prompt, but the retained process screen remains distinct and is
restored unchanged after the prompt is hidden.

## Diagnose a mismatch

Inspect the effective profile, selected terminfo name, degraded capability set,
and terminal configuration. For shifted status glyphs, change
`terminal.emoji_width` to match the host font. For a full-screen program,
verify alternate-screen and mouse behavior before assuming a passthrough
problem. In nested multiplexers, do not assume exclusive control of the outer
terminal; configure an outer binding when the default prefix does not arrive.

## Related pages

- [Appearance and terminal](../configuration/appearance-and-terminal.md)
- [Terminal input, copy, and history](../using-mezzanine/terminal-input-copy-and-history.md)
- [Troubleshooting](../operations/troubleshooting.md)
- [Normative terminal contract](../../SPEC.md#67-terminal-compatibility)

## Next step

Return to [the manual home](../README.md) to choose a task-oriented guide.
