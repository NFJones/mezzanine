# Workspace ownership matrix

This matrix records the completed behavior-level audit of Mezzanine's five
Cargo packages. Cargo edges alone do not establish ownership: deterministic
single-domain behavior belongs to a lower crate, while the `mezzanine` package
contains the executable product, concrete effects, product policy, and
cross-domain composition.

## Completion status

The decomposition is **complete**. The repository root is a virtual workspace
with no `src/`, `tests/`, package, binary, or product dependency ownership. The
application package and `mez` binary live at `crates/mezzanine/`. The exhaustive
machine-readable application inventory is
`docs/workspace-product-ownership.toml`.

The migration-state vocabulary is:

- `owned`: deterministic implementation and intrinsic tests live in their
  final lower package.
- `product`: the application surface defines the executable or a product
  contract, policy, security boundary, or cross-domain transition.
- `adapter`: the application surface binds lower contracts to concrete I/O,
  persistence, host facilities, or product presentation.

## Package graph and manifests

| Package | Manifest | Permitted workspace dependencies | Package role |
|---|---|---|---|
| `mez-core` | `crates/mez-core/Cargo.toml` | none | Stable, low-dependency identities shared by lower crates. |
| `mez-terminal` | `crates/mez-terminal/Cargo.toml` | none | One-terminal parsing, screen state, history, modes, geometry, style, and width. |
| `mez-mux` | `crates/mez-mux/Cargo.toml` | `mez-core`, `mez-terminal` | Multiplexer state, PTY domain, layout, input, readline, selection, copy, overlay, and presentation planning. |
| `mez-agent` | `crates/mez-agent/Cargo.toml` | `mez-core` | Provider-independent agent contracts, protocols, deterministic policy, planning, execution state, and intrinsic presentation decisions. |
| `mezzanine` | `crates/mezzanine/Cargo.toml` | all four lower crates | `mez` executable, product protocols and policy, concrete adapters, persistence, and serialized cross-domain composition. |

The architecture guard checks these exact package names, manifest locations,
workspace edges, and lower-package dependency allowlists. No lower package can
depend on `mezzanine`, and the mux and agent packages remain independent.

## Lower-crate ownership

| Behavior family | Final owner | Boundary evidence | State |
|---|---|---|---|
| Stable session, window, pane, client, and agent identifiers | `mez-core` | Product and lower consumers import `mez_core::ids` directly; no application identifier facade exists. | owned |
| Terminal protocol parsing, cells, screen lifecycle/editing, history, style, modes, profiles, geometry, Unicode width, and input encoding | `mez-terminal` | The screen engine is decomposed under `crates/mez-terminal/src/screen/`; product code retains only host terminal effects and explicit product protocol adaptation. | owned |
| Layout, session transitions, process/PTY domain, command grammar, host-input decoding, readline, copy mode, selectors, overlays, record browsers, render plans, and attached-client presentation | `mez-mux` | Product presentation imports the lower contracts directly and adds configured labels, product actions, filesystem candidates, transcript normalization, or host I/O. | owned |
| Agent action and provider contracts, MAAP, context policy, transcripts, messaging, MCP state/protocol, permissions, memory and issue records, skills, macros, subagent scope, retries, turn activity, execution planning, and outcome policy | `mez-agent` | Concrete credentials, HTTP/process transports, SQLite/filesystem stores, pane execution, clocks, audit, and application-event projection remain in adapters. | owned |

Intrinsic tests moved with these behaviors. Application tests exercise concrete
adapters or transitions that cross a product boundary; they do not preserve a
second copy of lower-domain policy.

## Application surface ownership

| Application surface | State | Admitted responsibility |
|---|---|---|
| `crates/mezzanine/src/cli/` | product | Process CLI, command workflows, application construction, and exit-code projection. |
| `crates/mezzanine/src/config/` | product | Versioned schema, defaults, migrations, layered loading, mutation, and product validation. |
| `crates/mezzanine/src/control/` | product | Supported JSON-RPC wire schema and client codec, authentication, authorization, idempotency, projection, and dispatch. |
| `crates/mezzanine/src/error.rs` | product | Product error taxonomy and one-time projection of concrete lower or adapter failures. |
| `crates/mezzanine/src/host/` | adapter | Host shell discovery, raw terminal and FD behavior, clipboard, Tokio actors, Unix sockets, workers, supervision, and execution of planned effects. |
| `crates/mezzanine/src/integrations/` | adapter | Provider credentials/transports, pane and MCP execution, hooks, embedded assets, skill/macro discovery, and product event/error projection. |
| `crates/mezzanine/src/lib.rs` | product | Narrow supported API: CLI bootstrap, product errors, and the control-client framing codec. |
| `crates/mezzanine/src/main.rs` | product | Thin Tokio process entry point for `mez`. |
| `crates/mezzanine/src/protocol/` | product | Product event records, local content-length framing, concrete message sinks, and product-local protocol identifiers. |
| `crates/mezzanine/src/runtime/` | product | Seven private state owners coordinated through serialized product transitions and typed side-effect plans. |
| `crates/mezzanine/src/security/` | product | Credential lifecycle, OAuth, audit persistence/redaction, project trust, and the live adapter around lower-owned permission policy. |
| `crates/mezzanine/src/storage/` | adapter | SQLite and filesystem persistence, compatibility migrations, private file posture, registries, snapshots, and lower-record repositories. |
| `crates/mezzanine/src/test_support/` | product | Test-only runtime fixtures shared by multiple independent application test owners. |
| `crates/mezzanine/src/ui/` | adapter | Product command dispatch, prompt adaptation, configured presentation policy, and filesystem/runtime selector candidates. |

Every top-level Rust source surface in the application crate appears exactly
once in this table and in the machine-readable manifest. A new surface must be
classified when it is added; unclassified and stale entries fail
`just architecture`.

## Runtime ownership

`RuntimeSessionService` is a coordinator, not a crate-visible state bag. It
contains exactly seven private component fields:

| Component | State ownership |
|---|---|
| `RuntimePresentationComponent` | Attached-client presentation, prompt/overlay state, selection, copy interaction, and render policy. |
| `RuntimeProcessComponent` | Pane processes, terminal screens, output/history, shell transactions, and process lifecycle. |
| `RuntimeAgentComponent` | Agent turns, schedulers, provider work, permissions, messages, memory, macros, and subagents. |
| `RuntimePersistenceComponent` | Registries, snapshots, transcripts, deferred writes, event persistence, and durable effect state. |
| `RuntimeControlComponent` | Control replay/idempotency, message connections, and event fanout. |
| `RuntimeIntegrationComponent` | Active config, credentials, providers, trust, hooks, MCP transports, and concrete integration bindings. |
| `RuntimeSessionComponent` | Canonical mux session, lifecycle metadata, socket identity, and product session facts. |

Runtime descendants invoke component methods or coordinator transactions. The
architecture guard rejects restoration of `pub(in crate::runtime)` fields,
broad production `use super::*` imports, or a changed coordinator field set
without an intentional architecture update.

## Public API and test ownership

`crates/mezzanine/src/lib.rs` keeps all implementation modules private. Its only
public module is `control_client`; its only public function is `run_cli`; and it
re-exports only the product error types and result alias. The architecture
guard verifies this shape so internal modules cannot silently become a
compatibility API.

The integration target `crates/mezzanine/tests/foreground_cli.rs` owns
process-level CLI behavior. Subsystem test trees use purpose-named Rust modules
and no `include!` or numbered chunks. Shared `test_support` contains only the
runtime fixture family because it has multiple independent owners; agent,
control, and async-runtime fixtures live beside their sole owners.

## Closed ownership invariants

The decomposition remains complete only while all of these hold:

1. The workspace has exactly the five manifests and dependency graph above.
2. The root remains virtual and has no `src/` or `tests/` directory.
3. Lower-owned declarations and retired application compatibility facades are
   not restored.
4. Application modules import lower contracts directly and do not publicly
   forward them.
5. Product source units remain below 2,000 lines and test ownership remains
   expressed through real Rust modules.
6. Runtime component fields remain private and the application API remains
   deliberately narrow.
7. `just check`, `just architecture`, `just clippy`, and `just test` pass.
