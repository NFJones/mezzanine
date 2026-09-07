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

Set `terminal.zen_mode = true` to hide passive Mezzanine group, window, and pane
bars and pills for the session while keeping pane split dividers visible. Zen
mode is live and session-wide. It does not rewrite `frames.window.enabled`,
`frames.pane.enabled`, or frame templates, so setting it back to `false`
restores the currently configured chrome. Application-drawn status bars remain
pane content, and command prompts, explicit overlays, copy/search controls, and
approval or trust interactions remain available.

`terminal.zen_focus_label_duration_ms` controls transient focus identity labels
(default `1000`, integer `0`–`60000`; `0` disables labels). Observers share their
source primary's original deadline; disabling labels or leaving zen clears all
pending scopes. Positive duration changes affect future labels only.

Labels use pane top-left (an eligible shared top divider when available),
window bottom-left, and group top-left anchors without reserving rows, resizing
PTYs, adding mouse targets, or running status providers. Custom non-zen frame
positions do not move these anchors. Only the highest changed scope receives a
new label; independent live labels can coexist, with group over window over
pane on intersection. Required input, selectors and modal UI take precedence;
editor and resize-drag takeovers suppress labels while deadlines continue.
Labels temporarily cover application cells, then repaint current content on
expiry rather than restoring saved rows. Rename updates the displayed identity
without extending its deadline. Viewport clipping never relocates an anchor.

`terminal.agent_wrap_column_cap` limits structured Mezzanine-owned agent rows,
including transcript text, statuses, errors, diagnostics, action headers,
result previews, and structured persistence fallbacks. The limit applies per
runtime service and never widens beyond the pane. Continuation rows repeat the
agent gutter; ordinary log rows preserve a leading `agent: ` hanging indent and
split unbroken text only at grapheme boundaries. Legacy ANSI-only presentation
records remain byte-preserving and can therefore wrap at the physical pane
width instead.

`keys` configures the prefix and direct bindings; `frames` configures window
and pane status presentation; `theme`, `themes`, and aliases configure colors.
Use `Ctrl+A ?` or the `list-keys` terminal command to inspect effective bindings
before replacing one. Live-mutability is shown by configuration diagnostics.

Pane titles and pane status rails are configured independently. Keep identity
in `frames.pane.template`, place title-adjacent items in
`frames.pane.left_status`, and order right-aligned items with
`frames.pane.right_status`. Bare built-in markers use their standard behavior;
named definitions under `frames.pane.pills.<name>` can change labels, finite
visibility conditions, supported formatting, width metadata, theme roles,
palette-name `foreground` and `background` channels, and actions. Palette names
resolve through the active theme, including `theme.aliases`; define an alias
instead of placing raw hex at a pill path. Omitted channels retain the semantic
style, a foreground-only running pill remains animated, and an explicit
background suppresses that occurrence's scan. Bare fields retain their standard
colors, so wrap one in a named definition for per-pill colors. A named
definition selects exactly one source: a built-in `field` or a
pane-scoped `command` with `cwd = "pane"`. Command values are cached outside the
renderer and normalized to bounded, inert single-line text. Empty rail strings
remain empty. `frames.pane.visible_fields` remains only the fallback used when
the title template is empty; it does not filter either status rail.

Select `frames.pane.status_preset` as `standard`, `minimal`, `agent-focused`,
or `full-controls`. Presets supply defaults before explicit rail and named-pill
overrides; partial named definitions inherit by stable ID. Generated configuration
leaves rails omitted so changing only the preset takes effect. Migrated explicit
rails, including empty rails, remain overrides. Presets never change agent settings
or pane geometry. Use `show-pane-status [-t pane]` to inspect resolved conditions,
layout decisions, provenance, and retained provider state without running providers,
including while zen mode is active.

Narrow panes use `frames.pane.overflow = "menu"` by default. Both rails share
one priority pool, with lower-priority pills compacted or removed first and
equal priorities removed from the end of template order. `compact` omits the
overflow menu, while `hide` skips compact forms. `frames.pane.title_min_width`
reserves eight terminal cells for the title by default, and the renderer always
preserves the trailing structural cell. Use `pane-settings [-t pane]` for
keyboard access to configured controls and overflowed or read-only entries;
opening it does not move focus to the target pane.

Command providers run only while referenced and condition-eligible on a
presented pane with frames visible and zen mode off. Overflow does not suspend
them. Mez requires trusted source provenance, an explicit permission `Allow`,
live pane CWD/authority, and a compiled Bubblewrap or Seatbelt launch; blocked
providers do not prompt on each timer tick. Provider launchers receive no daemon
or arbitrary pane credentials, while the sandbox payload receives the documented
`MEZ_PANE_ID`. `terminal:...{pane}...` and `agent:/...` pill actions retain the
stable owner pane and reject stale or closed targets without changing focus.
Use `pane-settings --providers [-t pane]` to inspect retained blocked state,
including in zen mode, without refreshing or executing providers. The display
contains only provider names and sanitized reason codes. An attached primary may
use `pane-settings --retry-provider NAME [-t pane]` to clear an exact current
block and make it due for normal admission; retry does not approve the command,
change trust or permissions, bypass policy, or weaken sandboxing.

Changing the effective theme or another visual presentation setting queues an
immediate full redraw for every attached client. The redraw restyles Mez-owned
frames, prompts, overlays, and transcript surfaces without waiting for unrelated
terminal activity. It cannot reinterpret ANSI or RGB colors already emitted by
applications into pane history; those application-owned colors remain literal.

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
