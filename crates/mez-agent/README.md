# mez-agent

The provider-independent agent harness, protocol contracts, and deterministic
policy layer for Mezzanine. `mez-agent` owns how agent requests, context,
actions, and turns are represented and coordinated, while product adapters
supply credentials, persistence, transports, execution, and UI.

## Why mez-agent?

- **Shared turn machinery:** request normalization, context assembly and
  budgeting, transcript continuity, turn orchestration, and recovery.
- **Reviewable action contracts:** Mezzanine Agent Action Protocol (MAAP)
  batches, schemas, validation, result shaping, and semantic patch planning.
- **Provider protocol support:** request and response handling for OpenAI,
  OpenAI-compatible Chat Completions, Anthropic, and DeepSeek protocols.
- **Deterministic coordination:** scheduling, routing, model sizing, messaging,
  and subagent policy behind narrow integration ports.

This is a library, not a standalone agent CLI. For model-backed work in a pane,
install the [Mezzanine product](../mezzanine/README.md).

## Prerequisites

- Rust 1.91 or newer, with Rust 2024 edition support.
- A checkout of the [Mezzanine repository](../../README.md).

Mezzanine supports Linux and macOS. Building this library does not require
provider credentials. Live model-backed work additionally needs a supported
provider account and sign-in method, or a configured compatible local backend,
through the product's integrations.

## Quick start

From the repository root:

```sh
cargo build -p mez-agent --locked
cargo doc -p mez-agent --no-deps --open
```

Rust imports use `mez_agent`. Use the [library root](src/lib.rs) to find public
contracts and the generated API documentation for individual inputs, outputs,
and errors. For integration work, begin with `harness`, `turn_runner`,
`execution`, and `http` rather than treating the library as a configured,
ready-to-run product agent.

## Main components

| Area | Representative modules |
| --- | --- |
| Turn orchestration and recovery | `harness`, `turn_runner`, `turn_ledger`, `outcome`, `action_recovery` |
| Context and accounting | `context_assembly`, `context_compaction`, `request_accounting`, `accounting` |
| Action contracts and planning | `maap`, `schema`, `surface`, `action_planning`, `action_result` |
| Local execution planning | `execution`, `local_action`, `semantic_patch`, `semantic_patch_planning` |
| Provider protocols | `provider`, `http`, `openai_request`, `openai_response`, `anthropic`, `deepseek` |
| Coordination and routing | `scheduler`, `routing`, `auto_sizing`, `messaging`, `subagent` |
| Integration records and policy | `instructions`, `mcp`, `memory`, `issues`, `permissions` |

## Integration and safety boundaries

The crate depends on [`mez-core`](../mez-core/README.md), not on the terminal,
multiplexer, or product crates. Provider-independent protocol handling can
still contain provider-specific request and response logic; it does not own
the product's concrete network clients or credential storage.

Planning or validating an action is not the same as executing it or granting
it permission. Product adapters implement process execution, persistence,
transport, approval enforcement, and OS confinement. The agent's context is
assembled from explicit inputs and action results, not passive access to a
terminal screen or other panes. See the
[safety guide](../../docs/safety-and-trust/README.md) for the complete product
trust boundary.

## Development and validation

Run commands from the repository root:

```sh
cargo check -p mez-agent --all-targets --all-features
timeout 120s cargo test -p mez-agent --all-targets --all-features --quiet
```

For workspace changes, also run:

```sh
just fmt
just clippy
timeout 300s just test
```

Test commands require a `timeout` executable; increase the limit when needed.
Changes to action or provider contracts should include focused regression
coverage and stay aligned with the [specification](../../SPEC.md).

## Documentation

- [Workspace overview](../../README.md)
- [Agent and integrations](../../docs/agent/README.md)
- [Protocol reference](../../docs/reference-manual/protocols/README.md)
- [Safety, trust, and security](../../docs/safety-and-trust/README.md)
- [Architecture](../../docs/contributing/architecture.md)
- [Development and validation](../../docs/contributing/development-and-validation.md)
- [Repository workflow requirements](../../AGENTS.md)

## License

Licensed under the [Apache License 2.0](../../COPYING).
