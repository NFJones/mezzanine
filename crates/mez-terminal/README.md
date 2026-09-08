# mez-terminal

The terminal emulation and compatibility engine for one Mezzanine terminal
surface. `mez-terminal` turns pane-facing terminal control input into screen,
history, style, and protocol state for higher layers to consume.

## Why mez-terminal?

- **One-terminal state:** parsing, screen buffers, cursor and mode state,
  alternate screens, and bounded scrollback history.
- **Compatibility contracts:** terminal profiles, capability descriptions,
  terminfo selection policy, mouse packets, and terminal-mode input encoding.
- **Text presentation primitives:** styled cells, Unicode grapheme segmentation,
  and display-width handling.
- **Focused boundary:** no multiplexer layout, attached-client policy, or agent
  presentation.

This is a library, not a terminal application. To run Mezzanine, see the
[product crate](../mezzanine/README.md).

## Prerequisites

- Rust 1.91 or newer, with Rust 2024 edition support.
- A checkout of the [Mezzanine repository](../../README.md).

Mezzanine supports Linux and macOS. This crate models a terminal surface;
PTY process ownership belongs to [`mez-mux`](../mez-mux/README.md).

## Quick start

From the repository root:

```sh
cargo build -p mez-terminal --locked
cargo doc -p mez-terminal --no-deps --open
```

Rust imports use `mez_terminal`. Begin with `TerminalScreen`, `TerminalSize`,
and `HistoryBuffer` in the [public API](src/lib.rs). Consult the
[terminal compatibility reference](../../docs/reference-manual/terminal-compatibility.md)
for the implemented terminal surface rather than assuming full compatibility
with every terminal extension.

## Main components

| Module | Responsibility |
| --- | --- |
| `screen`, `state`, `geometry` | Parser, emulated screen, restorable modes, and positive dimensions |
| `history` | Bounded scrollback and history configuration |
| `profile` | Compatibility profiles, capabilities, and terminfo selection |
| `protocol`, `mouse` | Terminal events, managed-shell protocol records, and mouse parsing |
| `style`, `width` | Styled lines, colors, graphemes, and display widths |
| `screen_error` | Screen configuration error contracts |

The crate depends on Unicode segmentation and width libraries, but not on other
Mezzanine crates. Both `mez-mux` and the product package consume its contracts.
Layout, frames, overlays, and application-level interpretation of events remain
outside this crate.

## Development and validation

Run commands from the repository root:

```sh
cargo check -p mez-terminal --all-targets --all-features
timeout 120s cargo test -p mez-terminal --all-targets --all-features --quiet
```

For workspace changes, also run:

```sh
just fmt
just clippy
timeout 300s just test
```

Test commands require a `timeout` executable; increase the limit when needed.
Terminal behavior changes must stay aligned with the compatibility reference
and [specification](../../SPEC.md) on both Linux and macOS.

## Documentation

- [Workspace overview](../../README.md)
- [Terminal compatibility](../../docs/reference-manual/terminal-compatibility.md)
- [Terminal input, copy, and history](../../docs/using-mezzanine/terminal-input-copy-and-history.md)
- [Architecture](../../docs/contributing/architecture.md)
- [Development and validation](../../docs/contributing/development-and-validation.md)
- [Repository workflow requirements](../../AGENTS.md)

## License

Licensed under the [Apache License 2.0](../../COPYING).
