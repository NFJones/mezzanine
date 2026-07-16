# Workspace architecture

Mezzanine is organized as five Cargo packages with one product application
package. That package is temporarily located at the workspace root and will
move to `crates/mezzanine/`, leaving a virtual repository workspace. This
structure enforces the specification requirement that the terminal multiplexer
and agent harness remain separable logical subsystems.

## Package responsibilities

- `mez-core` owns stable, low-dependency contracts shared by multiple lower
  crates. It does not own product policy, persistence, I/O, or generic helpers.
- `mez-terminal` models one terminal surface: parsing, screen state, history,
  capability profiles, and mode-aware input encoding.
- `mez-mux` arranges and presents terminal surfaces. It owns multiplexer domain
  state, PTY behavior, layout, input routing, and multiplexer UI.
- `mez-agent` owns the provider-independent agent harness and agent protocol
  state machines. Product integrations are supplied through narrow ports.
- `mezzanine` is the product application package. It owns the `mez` binary,
  configuration, runtime orchestration, persistence, transports, policy, and
  adapters between lower-level crates.

The lower crates contain production-owned domain behavior and tests. Additional
behavior moves only after its responsibilities have been separated and reverse
dependencies have been replaced with explicit contracts or effects;
application adapters retain product policy, persistence, transports, and host
I/O.

## Dependency direction

The current Mezzanine workspace edges are:

```text
mez-core      -> (none)
mez-terminal  -> (no workspace dependencies)
mez-mux       -> mez-core + mez-terminal
mez-agent     -> mez-core
mezzanine     -> mez-core + mez-terminal + mez-mux + mez-agent
```

`mez-agent` uses `mez-core` stable identities for agent messaging, and
`mez-mux` uses the same identity contracts for multiplexer state. No lower-level
crate may depend on `mezzanine`. The mux and agent crates may not depend on each
other, and the terminal crate may not depend on mux or agent behavior. Run
`just architecture` to validate these constraints against `cargo metadata`.
Product I/O dependencies for Tokio orchestration, HTTP, SQLite, and keyring
access remain in `mezzanine`; PTY and Unix process dependencies are explicitly
owned by `mez-mux`.

## Ownership rule

Deterministic subsystem behavior and its intrinsic tests live in the owning
lower crate. Application modules may adapt product policy, persistence,
transports, host I/O, and cross-subsystem orchestration, but must import lower
contracts directly instead of forwarding them through compatibility facades.
New shared contracts belong in `mez-core` only when at least two lower crates
need them.

The completed module-level audit and acceptance evidence are recorded in the
[workspace ownership matrix](workspace-ownership-matrix.md). A valid Cargo
dependency graph does not by itself prove that package ownership remains
correct, so architecture, public API, dependency, feature, and package-content
audits are part of refactor validation.

The architecture check also verifies the exhaustive product ownership manifest,
rejects restoration of retired compatibility surfaces and lower-contract
forwarding exports, and rejects Rust compilation units over 2,000 lines or
module implementations flattened with `include!`. When the ownership state is
complete, it additionally requires `crates/mezzanine/Cargo.toml`, a virtual
workspace root with no `src/` or `tests/`, explicit production imports, and
private runtime component state.
