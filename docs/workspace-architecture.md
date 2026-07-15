# Workspace architecture

Mezzanine is organized as five Cargo packages with one product composition
root. This structure enforces the specification requirement that the terminal
multiplexer and agent harness remain separable logical subsystems.

## Package responsibilities

- `mez-core` owns stable, low-dependency contracts shared by multiple lower
  crates. It does not own product policy, persistence, I/O, or generic helpers.
- `mez-terminal` models one terminal surface: parsing, screen state, history,
  capability profiles, and mode-aware input encoding.
- `mez-mux` arranges and presents terminal surfaces. It owns multiplexer domain
  state, PTY behavior, layout, input routing, and multiplexer UI.
- `mez-agent` owns the provider-independent agent harness and agent protocol
  state machines. Product integrations are supplied through narrow ports.
- `mezzanine` is the product composition root. It owns the `mez` binary,
  configuration, runtime orchestration, persistence, transports, policy, and
  adapters between lower-level crates.

The lower crates contain production-owned domain behavior and tests. Additional
behavior moves only after its responsibilities have been separated and reverse
dependencies have been replaced with explicit contracts or effects; root
adapters retain product policy, persistence, transports, and host I/O.

## Dependency direction

The current Mezzanine workspace edges are:

```text
mez-core      -> (none)
mez-terminal  -> (no workspace dependencies)
mez-mux       -> mez-core + mez-terminal
mez-agent     -> (no workspace dependencies)
mezzanine     -> mez-core + mez-terminal + mez-mux + mez-agent
```

The architecture policy permits `mez-terminal` and `mez-agent` to depend on
`mez-core` when a genuinely shared stable contract requires it; neither crate
currently needs that dependency. No lower-level crate may depend on
`mezzanine`. The mux and agent crates may not depend on each other, and the
terminal crate may not depend on mux or agent behavior. Run `just architecture`
to validate these constraints against `cargo metadata`.

## Ownership rule

Deterministic subsystem behavior and its intrinsic tests live in the owning
lower crate. Root modules may adapt product policy, persistence, transports,
host I/O, and cross-subsystem orchestration, but must import lower contracts
directly instead of forwarding them through compatibility facades. New shared
contracts belong in `mez-core` only when at least two lower crates need them.

The completed module-level audit and acceptance evidence are recorded in the
[workspace ownership matrix](workspace-ownership-matrix.md). A valid Cargo
dependency graph does not by itself prove that package ownership remains
correct, so architecture, public API, dependency, feature, and package-content
audits are part of refactor validation.
