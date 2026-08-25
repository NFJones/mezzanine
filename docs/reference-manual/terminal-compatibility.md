# Terminal compatibility

## Purpose

State Mezzanine's terminal profile, supported behavior, limitations, and the
diagnostics to use when host rendering differs from expected behavior.

## Prerequisites

Know the active terminal profile and pane `TERM` setting from configuration or
diagnostics.

## Supported profile

The default `xterm-compatible` profile is a bounded implemented subset, not a
claim of complete xterm emulation. It handles the documented C0, ESC, CSI, OSC,
SGR, cursor, alternate-screen, application cursor/keypad, bracketed-paste,
focus, mouse, title, clipboard, and save/restore behaviors. Unimplemented
capabilities, including DCS controls unless documented otherwise, are marked
unsupported rather than assumed to work.

### Synchronized output

Mezzanine implements DEC synchronized-output mode 2026 and the bounded legacy
DCS markers `=1s` and `=2s`. While synchronization is active, terminal state
continues to update but presentation is frozen until the matching end marker,
a lifecycle boundary, or the safety timeout releases it. Synchronization does
not nest; a new begin marker rearms the bounded interval. Alternate-screen and
terminal-lifecycle transitions cannot leave presentation frozen indefinitely.

Mezzanine also supports pane-local OSC 9;4 progress reports. Determinate
normal progress appears as a percentage pill immediately to the right of the
pane title and disappears on clear; warning, error, and indeterminate records
remove any stale percentage. Child panes receive an additive `P` in
`TERM_FEATURES` so tools such as Cargo can discover this support. The progress
state belongs to its pane and is not passed through to the outer terminal.
Custom pane-frame templates can display the active scalar with
`#{pane.progress}`.

Panes receive `TERM=xterm-256color` by default. Mez-specific terminfo entries
can be selected when installed. If a selected Mezzanine-specific entry is not
available, the safe fallback order is `screen-256color`, `screen`, `vt100`,
then `dumb`. The configured pane identity describes Mezzanine's compatibility
surface rather than claiming unrestricted passthrough of the host terminal.

## Rendering and input boundaries

Mezzanine composes terminal cells, preserves wide-glyph footprints, and uses a
single emoji-width policy across rendering, prompts, and copy mode. Use
`terminal.emoji_width = "wide"` for two-cell emoji presentation or `"narrow"`
for one-cell text fallback terminals. The setting does not make all complex
emoji narrow.

Pane alternate screens are separate from normal history. Full-screen programs
can remain visible and explicitly captured, but their rows are not injected
into normal scrollback or default agent context. Host bracketed paste, mouse,
focus, application cursor, and keypad behavior follow the active pane mode
where supported.

The agent shell is a separate pane presentation surface whose prompt appears at
the bottom of its pane. While it is visible, ordinary process input is captured
by the agent prompt, but the retained process screen remains distinct and is
restored unchanged after the prompt is hidden.

## Diagnose a mismatch

Inspect the effective profile, terminfo fallback, and terminal configuration.
For shifted status glyphs, change `terminal.emoji_width` to match the host
font. For a full-screen program, verify alternate-screen and mouse behavior
before enabling passthrough. In nested multiplexers, do not assume exclusive
control of the outer terminal; configure an outer binding when the default
prefix does not arrive.

The compatibility suite covers UTF-8 and width, control sequences, cursor and
screen operations, SGR, alternate screens, resize propagation, paste, focus,
mouse, OSC, application modes, nesting, and copy/history behavior.

## Related pages

- [Appearance and terminal](../configuration/appearance-and-terminal.md)
- [Terminal input, copy, and history](../using-mezzanine/terminal-input-copy-and-history.md)
- [Troubleshooting](../operations/troubleshooting.md)
- [Normative terminal contract](../../SPEC.md#67-terminal-compatibility)

## Next step

Return to [the manual home](../README.md) to choose a task-oriented guide.
