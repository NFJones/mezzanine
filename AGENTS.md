# Repository Guidelines

## Project Structure & Module Organization
Mezzanine is a five-package Rust 2024 workspace whose root package builds the
`mez` binary. Keep user-visible behavior aligned with `SPEC.md`, and keep
implementation logic in subsystem modules rather than expanding `src/main.rs`.

- `SPEC.md`: normative behavior for the multiplexer, agent harness,
  configuration, protocol, and security posture.
- `AGENTS.md`: repository workflow and implementation guidance for agents.
- `Cargo.toml` and `Cargo.lock`: workspace and root-package metadata, binary
  target, and shared dependency lockfile.
- `crates/mez-core/`: stable low-dependency contracts shared by lower crates.
- `crates/mez-terminal/`: one-terminal parsing, state, history, width, and
  protocol engine.
- `crates/mez-mux/`: multiplexer domain, PTY, layout, input, and presentation
  behavior.
- `crates/mez-agent/`: provider-independent agent harness, protocols, and
  deterministic policy.
- `justfile`: local development command entry points.
- `src/main.rs`: thin binary entry point for `mez`.
- `src/lib.rs`: library module root for testable subsystem code.
- `src/*.rs`: crate roots plus single-file subsystem modules.
- `src/<subsystem>/mod.rs`: roots for decomposed subsystem modules, including
  CLI, config, runtime, terminal handling, control protocol, command handling,
  permissions, and the agent harness.
- `src/<subsystem>/*.rs`: focused components that implement decomposed
  subsystem behavior behind the subsystem `mod.rs` facade.
- Root subsystem modules are product adapters. Import lower-crate contracts
  directly rather than forwarding them through root compatibility exports.
- `src/<subsystem>/tests/`: purpose-named test modules grouped by production
  behavior or domain and connected with true Rust `mod` declarations. Do not
  flatten test files with `include!` or use numbered chunk names such as
  `part_01.rs`.
- Shared test fixture modules should contain only helpers used by at least two
  test owners and should use the narrowest practical visibility. Keep
  one-consumer setup beside its owning tests, preserve test function names
  during structural moves, and generally keep leaf test modules around
  300–1,500 lines. Split files over roughly 2,000 lines at a meaningful
  behavioral boundary instead of adding sequence suffixes.
- `docs/examples/config.toml`: minimal example configuration aligned with the
  generated defaults.
- `target/`: generated Cargo build output; do not edit or commit files from it.

## Build, Test, and Development Commands
- Always wrap tests in a 60 second `timeout` or greater to check for hangs.
- `just`: build all targets and features in release mode.
- `just build`: build all targets and features in debug mode.
- `just build-release`: build all targets and features in release mode.
- `just run -- <args>`: run the release `mez` binary with arguments.
- `just check`: run `cargo check --all-targets --all-features`.
- `just fmt`: apply Rust formatting with `cargo fmt --all`.
- `just clippy`: run clippy for all targets/features with warnings denied.
- `just test`: run `cargo test --all-targets --all-features`.
- `just clean`: remove Cargo build artifacts.
- `just help`: list available recipes.

## Coding Style & Naming Convention Requirements
- Rust edition is 2024; follow standard `rustfmt` defaults (4-space indentation, line-wrapping via rustfmt).
- Module and file names are `snake_case` (e.g., `src/modules/raw.rs`); config module types are lowercase (e.g., `raw`, `mez`).

## Maintainability & Documentation Standards
- New or substantially changed modules should include a full module-level comment describing purpose, boundaries, and key invariants.
- Major architectural components should have long form comments explaining their purpose and how they relate to other architectural components.
- Public and private Rust items (`pub` structs/enums/traits/functions/methods) should have rustdoc comments describing behavior, inputs/outputs, and error conditions.
- Prefer small, composable functions; split logic that combines parsing, business rules, and I/O into clearer units.
- Avoid hidden global coupling; pass dependencies explicitly where practical.
- Do not use `unwrap`/`expect` in production paths unless the invariant is documented and intentional.
- Add context to propagated errors so failures are diagnosable in logs and tests.
- New behavior should include tests for the happy path and at least one edge or failure case.
- Bug fixes should include a regression test that fails before the fix and passes after.
- Behavior/config changes must update related documentation and examples (`README.md`, `SPEC.md`, `docs/examples/config.toml`) in the same change.
- All tests should have a long form docstring to explain what is being tested and why.
- Unless specifically instructed, do not maintain backwards compatibility with prior versions of this software. Deprecated code and modules should be removed.
- The config schema must be versioned and you must provide a migration for config items which are added, renamed/moved or deleted from the schema using conventions established in `SPEC.md`.
- Commit changes at major sequence points with long form commit messages to describe what has changed.
- When working on a research task, document your results in `docs/reference/`.
- When working on very large multi-phase refactors, write a refactor progress document out to `docs/reference/` and keep it up to date throughout the refactor.
- Never stage or commit documents contained in `docs/reference/`. They are for local use only.
- When decomposing a module into multiple compilation units, prefer to create a `mod.rs` in the module directory rather than leaving a `<mod_name>.rs` source in the parent directory.

## Testing Requirements
- Use `just check` for fast type-checking while developing.
- All changes must pass `just fmt`, `just clippy`, and `just test` before handoff.
- Prefer end-to-end coverage for feature changes whenever possible.

## Commit & Pull Request Requirements
- Commit messages in history are short, imperative, sentence case, and often end with a period (e.g., “Update pyo3 to latest release and fix compilation issues.”). Keep new commits consistent.
- PRs should include a clear summary, test commands/results, and any config changes. Add docs updates in `README.md` or `SPEC.md` when behavior changes.
- When adding new behaviors, ensure that they do not violate the requirements in `SPEC.md`.
- Always commit your changes at the end of a turn with a long-form informative message. Never skip this.

## Security & Configuration Tips
- Do not commit secrets in config files. Use `docs/examples/config.toml` as the baseline and override locally.
- Review network bind addresses and TLS settings before running in shared environments.
