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
focus, mouse, title, clipboard, and save/restore behaviors. Focus reporting is
host-dependent, and clipboard handling is policy-gated. Standard primary
device-attributes queries (`CSI c` and `CSI 0 c`) receive the conservative
VT100-with-no-options reply `CSI ? 1 ; 0 c`; unsupported query variants are
ignored. General DCS controls and other unimplemented capabilities remain
unsupported even though the two narrowly defined synchronized-output markers
below are recognized.

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

Rendering preserves styled blank cells and terminal autowrap semantics: a
printable glyph in the final column sets a pending wrap rather than scrolling
immediately. Pane-local alternate-screen state is composed into Mez's normal
host presentation; attached clients do not switch the containing terminal to
its alternate screen on behalf of a pane.

Pane alternate screens are separate from normal history. Full-screen programs
can remain visible and explicitly captured, but their rows are not injected
into normal scrollback or default agent context. Host bracketed paste, mouse,
focus, application cursor, and keypad behavior follow the active pane mode
where supported. While a pane application has bracketed paste enabled, a host
paste payload is forwarded opaquely across terminal-read chunks: bytes that
look like a Mez prefix or mouse report are not interpreted as multiplexer
input.

When `terminal.enhanced_keyboard_reporting = true`, Mez-owned readline prompts
on a primary client temporarily push Kitty keyboard flags 1 and 4. Mezzanine
pops exactly its own stack level when the prompt relinquishes input, the option
is disabled, presentation is restored, or the client detaches. This mode is not
enabled for observers or ordinary pane input.

Local Unix-socket and Iroh clients use the same server-owned external-editor
subsystem. Each editor runs on a dedicated PTY independent of the pane PTY.
While an editor owns the presentation, the initiating primary's input,
including prefix-like bytes and bracketed paste, is forwarded without Mez
prompt or keybinding decoding; resize and editor-driven focus and
alternate-screen modes are propagated and restored through both transports.
Observers remain read-only, and editor draft paths and content are not added
to transport metadata.

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
