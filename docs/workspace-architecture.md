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

The lower crates initially contain only documented facades. Production code is
moved only after its responsibilities have been separated and reverse
dependencies have been replaced with explicit contracts or effects.

## Dependency direction

The allowed Mezzanine workspace edges are:

```text
mez-terminal -> mez-core
mez-mux      -> mez-core + mez-terminal
mez-agent    -> mez-core
mezzanine    -> mez-core + mez-terminal + mez-mux + mez-agent
```

No lower-level crate may depend on `mezzanine`. The mux and agent crates may not
depend on each other, and the terminal crate may not depend on mux or agent
behavior. Run `just architecture` to validate these constraints against
`cargo metadata`.

## Extraction rule

Dependency inversion precedes file movement. Mixed modules are first split
behind their intended facade, tests remain with their behavior owner, and then
the focused implementation moves into the destination package. This avoids
using broad public APIs or dependency cycles as temporary migration tools.
