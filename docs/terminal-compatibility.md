# Terminal Compatibility Coverage

This document maps the current terminal compatibility coverage to the
xterm-compatible profile required by `SPEC.md`.

## xterm-compatible Profile

Automated deterministic coverage currently lives in Rust unit tests under
`src/terminal/*`, `src/layout/*`, `src/runtime/*`, `src/session/*`, and
`src/async_runtime/*`. The suite covers:

- UTF-8 lossy decoding behavior, invalid UTF-8 replacement, selected
  multi-byte character rendering, CJK double-width boundary behavior, and
  combining-mark boundary behavior. Combining marks are currently treated as
  zero-width input that does not advance the cursor and is not composed into
  the previous cell.
- C0 controls for backspace, carriage return, line feed, tab, BEL, and ESC.
- CSI cursor movement, erase display, erase line, insertion/deletion editing,
  scrolling regions, SGR attributes, save/restore, bracketed paste mode,
  alternate-screen entry and exit, and OSC title setting.
- DEC private mode save/restore for the tracked application cursor, mouse,
  focus-event, SGR mouse, bracketed paste, and alternate-screen modes.
- Application cursor, application keypad, and focus-event mode tracking,
  application-cursor translation for unmodified arrow keys, client terminal
  application-keypad mode control, and raw forwarding for keypad-originated SS3
  sequences.
- Bounded scrollback, alternate-screen exclusion from history and copy-mode
  content, copy-mode keyboard navigation, scrolling, search, selection, and
  copy-to-buffer behavior.
- Pane split rendering, visible-content PTY resize propagation after frame and
  divider reservation, attached-client input routing, resize-debounce
  full-surface redraw after invalidation, and primary/observer render sizing
  contracts.
- SGR mouse parsing, resize/copy/scroll classification, application SGR mouse
  mode tracking, anchored pane-local selection across borders, selection edge
  autoscroll, right-click clipboard paste with internal-buffer fallback outside
  application mouse mode, pane-scoped raw mouse forwarding precedence, and
  first-click focus for unfocused mouse-aware panes before later forwarding to
  pane applications.
- Legacy xterm mouse tracking for pane applications that enable DECSET 1000,
  1002, or 1003 without SGR encoding. Host-terminal SGR packets are translated
  to pane-local `ESC [ M` packets for those applications.
- Normal-screen deferred autowrap and soft-wrap reflow/restoration across pane
  width changes.
- Runtime PTY output parsing, activity/BEL event reporting, and live snapshot
  terminal capture.
- DCS string controls are parsed as bounded, terminated strings. The parser
  consumes their payloads without rendering content, treating BEL bytes inside
  DCS payloads as data rather than terminal bells.

## Control Sequence Policy

- DCS, SOS, PM, and APC string controls are consumed as ignored strings. DCS
  reply semantics such as DECRQSS and XTGETTCAP are not implemented. Nested
  muxxer passthrough payloads are bounded and consumed without rendering their
  contents.
- Clipboard control sequences are policy-gated. The xterm-compatible profile
  advertises clipboard capability, OSC 52 payloads are parsed, and runtime
  clipboard policy controls whether accepted payloads update the configured
  host clipboard path.
- Interactive recorded tests against full-screen applications remain useful for
  regression confidence, but deterministic fixtures now cover focus events,
  application cursor/keypad behavior, CJK double-width text, combining boundary
  behavior, invalid UTF-8, OSC clipboard policy, and nested-passthrough parsing.
