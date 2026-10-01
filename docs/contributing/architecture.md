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
depend on the product package. The current manifest graph is:

```text
mezzanine -> mez-agent -> mez-core
          -> mez-mux -> mez-terminal -> mez-core
          -> mez-core
```

`mez-agent` and `mez-terminal` each depend only on `mez-core`; `mez-mux`
depends on `mez-core` and `mez-terminal`; `mezzanine` depends on all four lower
crates. Keep new dependencies consistent with that layering rather than
introducing a reverse edge.

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

Agent context contracts are exported through `mez-agent/src/context/mod.rs`.
Its canonical owner retains private typed stable slots and chronological events;
focused children implement append/rebase, range compaction, legacy history
policy, validation, provider-message storage, request projection, and errors.
There is one context store and one checked candidate-before-commit boundary.
Behavior-grouped tests preserve event identity, causal ownership, trust, and
byte-exact projection contracts. Transcript filesystem I/O remains product-owned.

Runtime process orchestration remains under `runtime/processes/mod.rs`, with
focused children for adapter ownership, typed input delivery, terminal settings,
conversation-bound screens, transaction retirement, termination, and lifecycle
event payloads. All operate on the existing `RuntimeSessionService` state;
process generations, shell interaction epochs, and exact input leases remain
explicit handoffs rather than independently mutable policy stores.

Iroh transport composition lives under `runtime/iroh/mod.rs`. Focused children
own render fragmentation and flush accounting, connection-local control serving,
task settlement, and privacy-safe diagnostic projections. The listener retains
the endpoint and diagnostic registry; successful delivery remains the boundary
for committing render bases and presentation receipts. Transport authorization
and exact-client cleanup stay above the agent-independent presentation crates.

Agent terminal presentation application lives under
`runtime/render/presentation/buffer_apply/`. Its components distinguish cumulative
provider source, immutable worker projection, freshness acceptance, validated
settlement, message acceptance, durable replay/resize, and shell-preview layers.
They all operate on the runtime service's existing conversation screen and
presentation state, not independently mutable stores. Captured response identity,
source revision, geometry, policy, and installed-screen lineage remain explicit
handoffs; a provisional display row is not evidence of action execution.

## Ownership and tests

Follow the closest owner rather than forwarding contracts through
`crates/mezzanine/src/lib.rs`. Keep behavior-specific product tests in a
named `tests/` module under the owning subsystem. Shared fixtures must serve at
least two test owners; leave one-consumer setup beside its tests.

New or substantially changed modules need a module-level comment covering
their purpose, boundaries, and key invariants. Public and private Rust items
need rustdoc that describes their behavior, inputs, outputs, and error
conditions. Preserve the workspace's focused-module organization instead of
expanding `main.rs` or creating catch-all files.

## Related pages

- [Development and validation](development-and-validation.md)
- [AGENTS.md](../../AGENTS.md)
- [Manual home](../README.md)

## Next step

Use [Development and validation](development-and-validation.md) to make and
verify a change in the appropriate owner.
