# Repository Guidelines

## Project Structure & Module Organization
Mezzanine is a five-package Rust 2024 workspace with a virtual root. `SPEC.md`
is normative for user-visible behavior, configuration, protocols, and security.

| Package | Responsibility |
| --- | --- |
| `crates/mezzanine` | Product composition, concrete adapters, product tests, and the `mez` binary. |
| `crates/mez-core` | Stable, low-dependency shared contracts. |
| `crates/mez-terminal` | One-terminal parsing, state, history, width, and protocol engine. |
| `crates/mez-mux` | Multiplexer domain, PTY, layout, input, and presentation. |
| `crates/mez-agent` | Provider-independent agent harness, protocols, and deterministic policy. |

Keep `main.rs` thin and the product `lib.rs` deliberately narrow. Product
subsystems are adapters and composition: import lower-crate contracts directly,
not through product compatibility exports. See
[workspace architecture](docs/contributing/architecture.md) for boundaries.

- Decompose subsystem behavior behind its `mod.rs` facade. When splitting a
  module, create a directory with `mod.rs`, not a parent `<mod_name>.rs`.
- Connect purpose-named test modules with Rust `mod` declarations, never
  `include!` or numbered chunks such as `part_01.rs`. Preserve test function
  names during structural moves. Small cohesive leaves are fine; split files
  over roughly 2,000 lines at a meaningful behavioral boundary.
- Share test fixtures only when at least two owners use them, with the narrowest
  practical visibility. Keep one-consumer setup beside its tests.
- Keep `docs/examples/config.toml` minimal and aligned with generated defaults.
  Never edit or commit generated `target/` output.

## Build, Test, and Development Commands
- Wrap every test invocation in `timeout` with a budget of at least 120 seconds;
  increase it for compilation or larger suites. Use `cargo test --quiet`.
- `just` / `just build-release`: release build, all workspace targets/features.
- `just build`: debug build, all workspace targets/features.
- `just run <args>`: run the release `mez` binary (the recipe inserts `--`).
- `just check`: whole-workspace check, all targets/features; not a narrow check.
- `just fmt`: apply Rust formatting across the workspace.
- `just clippy`: lint all workspace targets/features with warnings denied.
- `just test`: test all workspace targets/features with quiet output.
- `just clean`: remove build artifacts. `just help`: list recipes.

## Coding Style & Maintainability
- Use Rust 2024, standard `rustfmt` defaults, and `snake_case` modules/files.
  Configuration module types are lowercase (for example, `raw`, `mez`).
- Preserve both Linux and macOS compatibility.
- Prefer small, composable functions and explicit dependencies; separate parsing,
  business rules, and I/O rather than hiding global coupling.
- Do not use production `unwrap`/`expect` unless the invariant is documented and
  intentional. Add diagnostic context to propagated errors.
- Document public contracts and non-obvious or safety-sensitive private behavior:
  relevant inputs, outputs, errors, ownership, and invariants. New or substantially
  changed modules should explain purpose and boundaries. Keep long-form
  architectural explanations where they clarify important relationships or
  safety reasoning; prefer specific explanations over boilerplate or length.
- Explain regression intent and non-obvious test setup, invariants, or failure
  cases. Do not require long-form comments for every self-explanatory test/item.
- New behavior needs happy-path and edge/failure coverage. Bug fixes need a
  regression that fails before the fix and passes afterward.
- Update affected docs/examples in the same change: `SPEC.md` for changed behavior
  contracts, configuration examples for changed configuration, relevant manual
  pages for workflows, and `README.md` only for high-level overview changes.
- Do not add legacy APIs, aliases, or compatibility shims solely for prior
  versions unless required. Remove deprecated code, but preserve explicit SPEC
  persistence/protocol contracts and required configuration/data migrations;
  compatibility cleanup must not delete user state or weaken integrity checks.
- Version the config schema and migrate added, renamed/moved, or deleted config
  items using the conventions in `SPEC.md`.
- Save task-specific research or plans in `docs/reference/` when a local handoff
  is useful; small tasks need no new artifact. Never stage or commit that
  directory. Use durable memory only for stable reusable knowledge allowed by
  the active memory contract, never task-specific research state or progress.
- Keep plans and progress notes current during very large multi-phase refactors.

## Validation Requirements
| Task phase/type | Expected validation |
| --- | --- |
| Rust implementation iteration | Smallest relevant package/target check and focused regression tests, e.g. `cargo check -p mez-agent --lib` and bounded quiet tests. Broaden for shared contracts. |
| Final code candidate | All applicable required CI gates runnable on the current machine before merge or issue closure; report unavailable checks. Do not run the full suite for every intermediate edit. |
| Documentation/research/tracker-only work | Relevant artifact checks; no automatic workspace compilation. |

For feature changes, exercise the user-facing path or closest faithful
production-boundary test; helper tests or compilation alone do not establish
connected behavior. Reuse valid results while relevant source/config/toolchain
inputs remain unchanged. An observation timeout does not establish process
termination: inspect the existing run rather than launch a duplicate build.

See [development and validation](docs/contributing/development-and-validation.md)
and [CI](.github/workflows/ci.yml) for full gates, platform/toolchain coverage,
physical temporary-directory requirements, and affected-subsystem acceptance
recipes. Missing platform checks must be reported, not silently treated as passed.

## Commit & Pull Request Requirements
- Commit task-owned, validated, coherent changes at meaningful checkpoints and
  implementation completion. Use a short imperative, sentence-case subject and
  an explanatory body. Never create empty commits or include unrelated user/peer
  changes. Read-only tasks and local research notes require no commit. If work
  stops before a safe checkpoint, report the exact remaining state.
- PRs should summarize changes, actual validation results/gaps, and config changes.
  Keep new behavior aligned with `SPEC.md`.

## Security & Configuration Tips
- Never commit secrets. Use `docs/examples/config.toml` as a baseline and override
  locally. Review network bind addresses and TLS before running in shared environments.
