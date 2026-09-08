# mez-mux

The agent-independent multiplexer domain and presentation engine for Mezzanine.
`mez-mux` manages the concepts and behavior needed to compose terminal surfaces
into panes, windows, groups, and sessions.

## Why mez-mux?

- **Multiplexer domain:** sessions, layouts, pane and window behavior, and
  command planning.
- **Process and input handling:** PTY processes, keyboard input, paste, and
  attached-client input policy.
- **Interactive presentation:** copy mode, readline, selectors, overlays,
  themes, and multi-surface rendering.
- **Agent-independent design:** terminal multiplexing does not depend on the
  agent harness or product composition package.

This library is not the `mez` executable. For installation and everyday use,
see the [product crate](../mezzanine/README.md).

## Prerequisites

- Rust 1.91 or newer, with Rust 2024 edition support.
- Linux or macOS for supported Mezzanine operation.
- Pseudoterminals and a POSIX-style shell for process-backed use and tests.
- A checkout of the [Mezzanine repository](../../README.md).

## Quick start

From the repository root:

```sh
cargo build -p mez-mux --locked
cargo doc -p mez-mux --no-deps --open
```

Rust imports use `mez_mux`. The [library root](src/lib.rs) exposes subsystem
modules and the shared `MuxError`, `MuxErrorKind`, and `Result` types. Start
with `session` and `layout` for domain state, `process` for PTY ownership, or
`presentation` and `render` for display composition.

## Main components

| Modules | Responsibility |
| --- | --- |
| `session`, `layout`, `command` | Multiplexer state, layout, and command planning |
| `process` | PTY process behavior |
| `attached_client`, `host_input`, `input`, `key_preset` | Client input handling and key policy |
| `clipboard`, `copy`, `paste`, `readline` | Selection, clipboard, paste, and line editing |
| `presentation`, `render`, `overlay`, `theme` | Multi-surface presentation and visual policy |
| `selector`, `record_browser` | Selection and record-browsing interfaces |

## Workspace boundaries

`mez-mux` consumes shared identifiers from [`mez-core`](../mez-core/README.md)
and one-terminal surfaces from [`mez-terminal`](../mez-terminal/README.md).
It does not depend on [`mez-agent`](../mez-agent/README.md). Product runtime
composition and concrete agent integration belong to
[`mezzanine`](../mezzanine/README.md), not this library.

## Development and validation

Run commands from the repository root:

```sh
cargo check -p mez-mux --all-targets --all-features
timeout 120s cargo test -p mez-mux --all-targets --all-features --quiet
```

For workspace changes, also run:

```sh
just fmt
just clippy
timeout 300s just test
```

Test commands require a `timeout` executable; increase the limit when needed.
Validate changes to PTY and input behavior on both supported operating systems;
a successful run on one platform does not qualify the other.

## Documentation

- [Workspace overview](../../README.md)
- [Sessions and panes](../../docs/using-mezzanine/sessions-and-panes.md)
- [Terminal input, copy, and history](../../docs/using-mezzanine/terminal-input-copy-and-history.md)
- [Architecture](../../docs/contributing/architecture.md)
- [Development and validation](../../docs/contributing/development-and-validation.md)
- [Behavior and compatibility specification](../../SPEC.md)
- [Repository workflow requirements](../../AGENTS.md)

## License

Licensed under the [Apache License 2.0](../../COPYING).
