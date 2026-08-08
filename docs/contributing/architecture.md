# Workspace architecture

## Purpose

Describe Mezzanine's workspace boundaries so contributors can place changes in
the owning crate and avoid coupling product adapters to reusable domain code.

## Prerequisites

Read the repository [AGENTS.md](../../AGENTS.md) before editing. Consult
[SPEC.md](../../SPEC.md) for normative product behavior rather than treating
this page as a behavioral contract.

## Workspace layers

The Rust 2024 workspace has five packages. Dependency direction flows upward:
the product package composes the four lower crates, while lower crates do not
depend on the product package.

| Package | Owns | Boundary |
| --- | --- | --- |
| `mez-core` | Stable identifiers and low-dependency shared contracts | No product policy, I/O, persistence, runtime orchestration, or general utility layer. |
| `mez-terminal` | One-pane terminal parsing, screen state, history, styles, width, mouse, and compatibility profiles | Does not own layouts, clients, agent presentation, or multiplexer policy. |
| `mez-mux` | Sessions, panes, windows, layouts, PTYs, input, copy/readline, command planning, themes, and presentation | Is agent-independent and consumes core and terminal contracts. |
| `mez-agent` | Provider-independent agent harness, MAAP, context, provider shaping, policies, scheduling, and integration ports | Leaves credentials, persistence, transport, process execution, and UI to product adapters. |
| `mezzanine` | The `mez` binary, CLI, configuration, control, host I/O, integrations, runtime, security, storage, and UI composition | Imports lower-crate contracts directly instead of re-exporting compatibility layers. |

## Product composition

`crates/mezzanine/src/main.rs` is intentionally a thin process boundary: it
creates the Tokio runtime and calls the product CLI. The library root owns
application composition. Product subsystems live under their named directories
(`cli`, `config`, `control`, `host`, `integrations`, `protocol`, `runtime`,
`security`, `storage`, and `ui`) behind focused `mod.rs` facades.

Put reusable terminal, multiplexer, agent-policy, or identifier behavior in
the relevant lower crate. Put provider credentials, local persistence,
concrete transports, process execution, and terminal-facing product adapters
in the product crate. This boundary keeps provider-independent logic testable
without product-only dependencies.

## Ownership and tests

Follow the closest owner rather than forwarding contracts through
`crates/mezzanine/src/lib.rs`. Keep behavior-specific product tests in a
named `tests/` module under the owning subsystem. Shared fixtures must serve at
least two test owners; leave one-consumer setup beside its tests.

Each substantial module and item needs documentation describing its purpose,
inputs, outputs, boundaries, invariants, and errors as applicable. Preserve
the workspace's focused-module organization instead of expanding `main.rs` or
creating catch-all files.

## Related pages

- [Development and validation](development-and-validation.md)
- [AGENTS.md](../../AGENTS.md)
- [Manual home](../README.md)

## Next step

Use [Development and validation](development-and-validation.md) to make and
verify a change in the appropriate owner.
