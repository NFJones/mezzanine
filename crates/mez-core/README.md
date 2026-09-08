# mez-core

Stable, low-dependency contracts shared by the Mezzanine workspace. `mez-core`
provides canonical identifiers and their validation invariants without pulling
in terminal, multiplexer, agent, or product implementation details.

## Why mez-core?

- **Shared identity:** common types for agents, clients, panes, sessions,
  window groups, and windows.
- **Small dependency surface:** no external dependencies in the crate manifest.
- **Clear ownership:** identifier contracts live here; product policy, I/O,
  persistence, and runtime orchestration do not.

This is a library, not an executable. To install the terminal multiplexer and
pane-local agent, see the [product crate](../mezzanine/README.md).

## Prerequisites

- Rust 1.91 or newer, with Rust 2024 edition support.
- A checkout of the [Mezzanine repository](../../README.md).

Mezzanine targets Linux and macOS. This crate is the workspace dependency root
and does not implement platform-specific process or terminal I/O.

## Quick start

From the repository root, build the library and browse its API documentation:

```sh
cargo build -p mez-core --locked
cargo doc -p mez-core --no-deps --open
```

Rust imports use the name `mez_core`. Start with the
[`ids` module](src/ids.rs), which exports `AgentId`, `ClientId`, `PaneId`,
`SessionId`, `WindowGroupId`, `WindowId`, `StableId`, and `IdFactory` through
the [library root](src/lib.rs).

## Workspace role

`mez-agent`, `mez-mux`, and the `mezzanine` product package depend on these
contracts. Keep identifiers here when multiple subsystems need the same
identity semantics; do not turn this crate into a general-purpose utility or
product-policy layer.

## Development and validation

Run commands from the repository root:

```sh
cargo check -p mez-core --all-targets --all-features
timeout 120s cargo test -p mez-core --all-targets --all-features --quiet
```

Before handing off workspace changes, run the repository checks:

```sh
just fmt
just clippy
timeout 300s just test
```

Test commands require a `timeout` executable; allow a longer limit when needed.
See the [development guide](../../docs/contributing/development-and-validation.md)
for the full validation workflow.

## Documentation

- [Workspace overview](../../README.md)
- [Architecture](../../docs/contributing/architecture.md)
- [Contributor guide](../../docs/contributing/README.md)
- [Behavior and compatibility specification](../../SPEC.md)
- [Repository workflow requirements](../../AGENTS.md)

## License

Licensed under the [Apache License 2.0](../../COPYING).
