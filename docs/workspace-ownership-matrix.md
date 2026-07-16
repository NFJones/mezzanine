# Workspace ownership matrix

This matrix records the current production-module audit against the five-package
workspace architecture. A legal Cargo dependency graph is
necessary but not sufficient: deterministic subsystem behavior lives with its
owning lower crate, while the root package contains product policy,
persistence, transport, and composition adapters.

## Classification vocabulary

- **Owner** is the package that should contain reusable deterministic behavior.
- **Root role** explains why code may remain in `mezzanine`.
- **Migration state** is `owned`, `adapter`, or `temporary`.
  - `owned`: implementation and intrinsic tests are in the final package.
  - `adapter`: root code intentionally integrates product I/O, policy, or state.
  - `temporary`: lower-crate behavior or forwarding remains in root and must be
    removed before the decomposition issue is resolved.

## Current status

The decomposition is **open**. The exhaustive machine-readable root inventory
is `docs/workspace-root-ownership.toml`; its temporary surfaces are the
authoritative list of unresolved ownership. This matrix records the behavioral
boundaries behind those temporary classifications. A green architecture check
while the manifest is open validates dependency direction and inventory
consistency, not decomposition completion.

## Lower-crate ownership

| Surface | Final owner | Current evidence | State | Boundary evidence |
|---|---|---|---|---|
| Stable identifiers | `mez-core` | `crates/mez-core/src/ids.rs`; product consumers import `mez_core::ids` directly | owned | No root compatibility facade remains. |
| Terminal geometry, history, protocol, state, style, width, profiles, and screen parser | `mez-terminal` | `crates/mez-terminal/src/{geometry,history,protocol,state,style,width,profile,screen}.rs` and crate-owned screen tests | owned | Root consumers import terminal contracts directly. The root retains only the explicitly named OSC 133 decoder and host-policy adapters. |
| Layout, session state/effects, PTY processes, input contracts, theme, and copy/readline primitives | `mez-mux` | `crates/mez-mux/src/{layout,session,process,input,theme,copy,readline}` | owned | The lower-crate fake pane-output-to-terminal-screen-to-headless-client flow covers input routing, resize/focus/layout effects, copy-mode transitions, and redraw. Styled terminal-derived copy state plus prompt buffer ownership, reverse history search, multiline navigation, baseline terminal-input transitions, and 26 relocated exact intrinsic readline regressions are mux-owned; product transcript/Markdown normalization and selector candidate policy remain adapter concerns. |
| Mux presentation geometry and canvas primitives | `mez-mux` | `crates/mez-mux/src/{attached_client,overlay,record_browser,presentation,render}` | owned | Attached-client output/input, record browsing, generic overlay state and interaction, rich-text and unified-diff parsing, syntax span generation, wrapping, geometry, color math, frame plans, prompt-region composition, and style layering are lower-owned. Parser dependencies are absent from the root package. |
| Agent contracts and provider-independent policy | `mez-agent` | `crates/mez-agent/src/` owns action surfaces, schemas, context policy, provider contracts, scheduler, canonical turn records/ledger, execution ports, production turn orchestration, patch parsing, shell transaction contracts, messaging, instruction planning, permission policy, subagent scope enforcement, memory, issue, transcript, MCP, provider routing, auto-sizing decisions, outcome recovery, progress ledgers, child-result shaping, and shell observation | owned | Root agent code supplies credentials, concrete HTTP/process/pane/MCP/filesystem adapters, embedded assets, persistence calls, runtime state mutation, clocks, audit, and product error projection. Architecture guards reject restoration of the extracted policy declarations. |

## Root agent adapter audit

| Root surface | Final owner / root role | State | Boundary evidence |
|---|---|---|---|
| `src/agent/actions/` | `mez-agent` harness plus root local-action, permission, provider, transcript-store, and runtime adapters | adapter | Initial and resumed action-result planning, batch continuation/recovery, memory guardrails, request gating, canonical execution projection, lifecycle derivation, transcript projection, shell-output decoding, execution ports, and production turn orchestration are lower-owned. Root supplies concrete permission/scope/MCP facts, request assembly, provider and transcript I/O, pane/MCP/filesystem dispatch, and product error projection through `AgentTurnEnvironment` and execution-port implementations. |
| `src/agent/context/` | `mez-agent` with root product-input adapters | adapter | Canonical records, compaction, request projection, evidence/cache/provider shaping, skill constraints, and all context appenders are lower-owned. Root context code supplies only turn identity and embedded prompt assets through one assembly function; the ignored retained-tail compatibility API is removed. All appender and request-shaping scenarios run lower; root retains one turn-record and embedded-asset adapter regression. |
| `src/agent/maap.rs` | root shell-policy adapter over `mez-agent` | adapter | Canonical action/domain types, parsing, normalization, validation, and 25 intrinsic protocol regressions are lower-owned and called directly by provider adapters. Root retains shell policy, execution formatting, and one shell-policy callback integration. |
| `src/agent/provider/` | `mez-agent` provider behavior plus root credential/HTTP/runtime adapters | adapter | Canonical request/response contracts and all OpenAI Responses, compatible Chat Completions, Anthropic, DeepSeek, and Claude Code request, response, schema, capability, session, prompt, accounting, diagnostic-redaction, and corrective-retry policy are lower-owned. Root provider code is audited concrete composition: auth-store lookup, credential and header attachment, reqwest/process transport, temporary settings files, bounded process retries, refresh, quota attachment, runtime event conversion, and `MezError` projection. Intrinsic schema, capability, compatibility, and accounting tests run lower; root tests exercise concrete adapters. |
| `src/agent/prompt.rs` | `mez-agent` with root embedded-asset adapter | adapter | Prompt profiles, validation, section ordering, repository-guidance embedding, provider selection, and subagent scope formatting are lower-owned through `AgentPromptAssetSource`. Root retains only `include_dir` asset lookup and the product-facing assembly facade. |
| `src/agent/semantic/` | `mez-agent` planning plus root shell-policy/error adapter | adapter | Canonical local-action plans plus semantic-patch parsing, snapshot interpretation, hunk matching/diagnostics, and read/write transaction generation are lower-owned. Root validates product-authored shell commands, projects lower planning errors into `MezError`, and leaves filesystem reads/writes and pane-shell execution behind `LocalActionExecutor`. |
| Agent-shell session/store policy | `mez-agent` | owned | Visibility/log policy, pane session records, conversation and ephemeral-lineage binding, transcript counters, running-turn transitions, UUID generation, and help/status/permission/MCP display shaping are lower-owned. The root session module and type facade are removed; two exact store regressions run lower while root agent-shell coverage is limited to slash, permission, and live MCP integration. |
| `src/agent/slash.rs` | root agent-shell execution adapter over `mez-agent` | adapter | Canonical slash registry, parser records, effects, and intrinsic parsing tests are lower-owned. Keep product session mutation, display rendering, runtime-effect routing, and product error projection here. |
| `src/agent/network.rs` | `mez-agent` contracts plus root HTTP transport adapter | adapter | Network action plans, summaries, permission-facing pseudo commands, and structured result shaping are lower-owned with no root plan forwarding. Root retains HTTP transport, response caps/parsing, and product error projection. |
| Shell transaction boundary | `mez-agent` | owned | `crates/mez-agent/src/shell/` owns classification, transaction rendering, authored-command policy, bootstrap scripts/parsing, environment signatures, tool inventory/cache state, and intrinsic tests. `src/agent/shell.rs` is retired; root retains pane execution, timeout, output observation, and product error mapping only. |
| `src/agent/mod.rs` | product adapter namespace | adapter | Canonical lower contracts and helpers are imported directly from `mez-agent`. Product consumers import explicit `actions`, `context`, `maap`, `network`, `prompt`, `provider`, `semantic`, or `slash` adapters; the module root retains only private sibling wiring and no compatibility exports. |
| `src/agent/tests/` | product adapter integrations | adapter | Intrinsic shell transaction/bootstrap/tool-cache tests run in `mez-agent`; root retains pane-executor discovery, output-decoding, concrete provider/recovery, permission, MCP, transcript, and runtime integrations. All 56 root turn-runner regressions exercise the same lower canonical state machine used by production rather than a root-owned orchestration loop. |

## Root mux and terminal adapter audit

| Root surface | Final owner / root role | State | Boundary evidence |
|---|---|---|---|
| `src/terminal/render/` | product presentation adapters over `mez-mux` | temporary | Neutral frame plans, prompt-region placement, overlay composition, geometry, color math, and path fitting are mux-owned. Root retains product field resolution, readline/agent view-model construction, configured themes, animation, and hit actions. Phase 10 removed lower forwarding aliases and narrowed module-root imports; temporary status now tracks only the retained production and test splits in Phases 11-12. |
| `src/terminal/client_loop.rs` | root product routing and host adapter over `mez-mux::attached_client` | adapter | Retained-frame diffing, style/SGR encoding, mode and cursor transitions, mouse packet encoding, prefix boundaries, and host-paste framing are lower-owned with 41 focused tests. Root retains FD I/O contracts, product command/mouse action mapping, prompt routing, and endpoint error projection. |
| `src/terminal/copy.rs`, `client_loop.rs`, `mouse.rs` | `mez-mux` domain plus root product policy adapters | adapter | Copy state and neutral attached-client policy are lower-owned. Root retains transcript/Markdown clipboard normalization, agent selectors, templated frame actions, product mouse actions, and host clipboard execution. |
| `src/terminal/host_clipboard.rs` | root host clipboard process adapter | adapter | Keep platform command discovery and process execution product-owned; generic paste-buffer state is owned by `mez-mux`. |
| `src/terminal/fd.rs` | root host terminal adapter | adapter | Keep raw terminal mode, FD polling, and host restoration product-owned; depend on mux/terminal contracts directly. |
| `src/terminal/screen.rs` | root OSC 133 product adapter over `mez-terminal` | adapter | Keep the explicitly named shell-transaction decoder product-owned; do not restore the removed profile facade or add terminal-screen forwarding here. |
| `src/terminal/mod.rs` | product presentation/host adapter facade | adapter | Lower mux status, viewport, theme, attached-client view/output, and cursor contracts are imported directly. The remaining exports name product host, copy-normalization, mouse-action, and presentation adapters. |
| `src/terminal/tests/` | product presentation and host integrations | temporary | Intrinsic attached-client, frame-plan, prompt-layout/placement, overlay, and canvas tests run in `mez-mux`; root retains full configured product-view and host-routing integrations. Phase 12 still must split oversized retained test owners and remove redundant wrapper coverage. |
| `src/readline/` | `mez-mux` generic prompt behavior plus root command/selector adapter | adapter | Prompt buffer ownership, reverse history search, multiline navigation, baseline terminal-input transitions, decoding, selector candidate/plan contracts, and active-selection cycling are mux-owned. The unused `cfg(test)` prompt-loop implementation, DTOs, I/O trait, and escape-flush compatibility hook are removed and architecture-guarded as retired. The remaining root tests cross product prompt-kind/prefix, command/agent selector policy, body-width injection, or decoded-input-to-product-prompt boundaries. |

## Other root subsystem audit

| Root surface | Final owner / root role | State | Boundary evidence |
|---|---|---|---|
| `src/macros/` | `mez-agent` macro contracts plus root asset/discovery adapter | adapter | Macro identities, ordered-step/document/invocation parsing, validation, catalog precedence, and model/JSON projection are lower-owned. Root retains bounded filesystem discovery, embedded product assets, source/path attachment, product error projection, and runtime dispatch. Six intrinsic tests run lower; five asset/filesystem integrations remain root-owned. |
| `src/subagent/` | `mez-agent` subagent domain plus root presentation names | adapter | Canonical spawn/profile/scope state, active write coordination, permission-backed shell effect enforcement, semantic-patch scope enforcement, path normalization, and intrinsic tests are lower-owned. Root retains only product-friendly pane and status display names. |
| `src/command/` | `mez-mux` command grammar/plans/presentation plus product dispatch | adapter | Typed session-mutation plans, command defaults, shell-argument reconstruction, mux validation, and session list/chooser presentation are lower-owned. Root retains product error projection, concrete session mutation/process lifecycle, registry/help/status and product outcomes, cross-cutting dispatch, and auth/config/audit/agent/store adapters. |
| `src/selector/` | `mez-mux` selector engine plus root product candidate providers | adapter | Candidate categories/records, shell-like token context and normalization, deduplication/ranking, replacement plans, shadow-hint records, candidate application, and generic active-selection cycling are lower-owned and imported directly. Root retains product command/slash catalogs, dynamic runtime candidates, parameter hints, filesystem discovery, and working-directory/home lookup behind a focused module facade. Source guardrails reject restoration of the generic engine in root. |
| `src/runtime/`, `src/async_runtime/` | product composition | temporary | Async supervision and effect execution remain product-owned. Agent policy, record-browser/overlay state, rich-text and diff engines, syntax generation, and neutral terminal composition are lower-owned. Phase 10 removed canonical forwarding wrappers and module-root wildcard imports; Phases 11-12 must finish production/test file decomposition. |
| auth, config, control, audit, hooks, snapshot, and concrete stores/transports | product policy/persistence/transport | adapter | Keep concrete stores and transports in root; implement narrow lower-crate ports and convert errors once at boundaries. |
| Instruction discovery | `mez-agent` | adapter | Discovery configuration, path and filename validation, pane-shell command planning, escaped-record parsing, and intrinsic tests are lower-owned. Root executes resulting shell plans through product runtime surfaces and has no instruction facade. |
| Permission policy and approval state | `mez-agent` plus root planning/config/control adapters | adapter | Command classification, path scopes, rules/codecs, authority ordering, approval records and queues, deterministic policy evaluation, and intrinsic tests are lower-owned. Root supplies timestamps and retains config persistence, control authorization, trusted-directory facts, audit, and a borrowed planning adapter. |
| Memory domain | `mez-agent` domain plus root persistence adapter | adapter | Canonical scopes, kinds, lifecycle states, records, validation, line/scope codecs, context projection, and process-local session state are lower-owned with intrinsic tests. Root retains SQLite/FTS storage, legacy migration, configured paths, private filesystem permissions, retrieval I/O, and product error projection. |
| Issue domain | `mez-agent` domain plus root persistence adapter | adapter | Canonical kinds, states, records, updates, queries, result contracts, field validation, and MAAP validation are lower-owned with intrinsic tests. Root retains configured SQLite persistence, dependency existence/cycle checks, project discovery, ID generation, filesystem permissions, and product error projection. |
| Transcript domain | `mez-agent` domain plus root persistence adapter | adapter | Canonical roles/entries, session checkpoint DTOs and validation, conversation summaries, and summary derivation are lower-owned with intrinsic tests. Root retains filesystem layout, TSV compatibility/migration, retention, compression, prompt history, private permissions, sequence allocation I/O, and terminal presentation replay. |
| MCP domain | `mez-agent` domain plus root transport adapters | adapter | `mez-agent::mcp` owns secret-safe server policy, registry transitions with injected timestamps, prompt projections, bounded pagination, JSON-RPC construction/parsing, discovery records, and tool-call planning with 24 intrinsic tests. Root retains stdio/process and reqwest/SSE transports, credential/environment resolution, persisted configuration commands, transport handles, retry execution, audit, and 20 transport integrations. |

## Root product ownership

The remaining root modules are product code, not lower-crate compatibility
surfaces. This table makes every top-level root domain explicit so line count
alone is not mistaken for an unexamined package boundary.

| Root surface | Root responsibility | State | Boundary evidence |
|---|---|---|---|
| `src/lib.rs`, `src/main.rs`, `src/error.rs`, `src/identifiers.rs`, `src/shell.rs` | product composition, error projection, product-local validation, and host shell resolution | adapter | The binary remains thin; lower errors convert once at the root. Identifier grammar has only root product consumers and therefore does not meet the two-lower-consumer rule for `mez-core`. Shell resolution inspects the host process environment and executable filesystem. |
| `src/cli/`, `src/config/`, `src/control/`, `src/framing/` | CLI, schema/migrations, control wire protocol, bounded codecs, and product request dispatch | adapter | These modules define the `mez` executable and its product protocols. |
| `src/message/` | root MMP transport adapter | adapter | Canonical identities, envelopes, presence, bounded queues, subscriptions, snapshots, validation, JSON body dispatch, and delivery policy are owned by `mez-agent::messaging`. Root retains only shared content-length framing and concrete sink writes; runtime owns sockets, wakeups, and audit integration. |
| `src/runtime/`, `src/async_runtime/`, `src/event/` | serialized product state owner, Tokio/Unix orchestration, effect execution, supervision, and observer fanout | temporary | Product effects, state mutation, and projection remain root-owned. Lower rendering engines and canonical forwarding wrappers are gone; temporary status now tracks the oversized production/test owner splits required by Phases 11-12. |
| `src/auth/`, `src/audit/`, `src/project/`, `src/registry/` | credentials, security audit persistence, project trust, and live-session registry I/O | adapter | These are concrete OS keyring, OAuth/HTTP, filesystem, JSONL, trust, and registry adapters with product retention and security policy. |
| `src/hooks/`, `src/skills/`, `src/macros/` | process execution and filesystem/asset discovery | adapter | Root binds lower contracts to configured hooks, embedded assets, and trusted project paths. |
| `src/permissions/`, `src/subagent/` | thin agent policy/presentation adapters | adapter | Permission policy and subagent enforcement are canonical in `mez-agent`; root retains only live planning-state binding and product display names. |
| `src/mcp/` | product MCP transport and configuration adapters | adapter | Canonical protocol, registry state, validation, pagination, prompt projection, and tool planning are imported directly from `mez-agent::mcp`. Root retains process and HTTP transports, credential/environment resolution, persisted configuration commands, product clock injection, transport handles, retry execution, and audit. |
| `src/snapshot/` | cross-crate snapshot repository and restore | adapter | Snapshot compatibility and persistence remain product-owned. |
| `src/memory/` | concrete memory repository adapter | adapter | Canonical records, validation, codecs, and session state are imported directly from `mez-agent`; root keeps SQLite/FTS storage, legacy migration, retrieval I/O, configured paths, and private filesystem permissions. |
| `src/issues/` | concrete issue repository adapter | adapter | Canonical records, updates, queries, and shared MAAP validation are imported directly from `mez-agent`; root keeps configured SQLite persistence, dependency graph checks, project discovery, ID generation, and filesystem permissions. |
| `src/transcript/` | concrete transcript repository and presentation adapter | adapter | Canonical entries, checkpoints, summaries, and summary policy are imported directly from `mez-agent`; root keeps filesystem layout, compatibility encoding/migration, retention/compression, prompt history, private permissions, and terminal replay records. |
| `src/test_support/` and subsystem test trees | crate-internal product integration fixtures and end-to-end coverage | adapter | Shared fixtures are test-only. Intrinsic engine tests live in their lower crates; root tests cover concrete adapters and cross-crate workflows. |

## Final adapter surfaces

The root package now exposes explicit product adapter namespaces rather than
lower-contract compatibility facades:

- `src/agent/mod.rs` exposes product action, context, MAAP, network, prompt,
  provider, semantic, and slash adapters. Canonical contracts are
  imported directly from `mez-agent`.
- `src/terminal/mod.rs` exposes product copy/render and host-I/O adapters; lower
  terminal and mux contracts are imported directly from their owning crates.
- `src/readline/` specializes mux-owned prompt state with product command and agent selector policy; it no longer owns neutral reverse-search or multiline transition state.
- Product provider and runtime modules consume `mez-agent` contracts directly.
  Permission enums, MCP prompt records, instruction discovery records,
  provider-facing config constants, and provider DTOs are no longer exposed
  through product subsystem facades.

An audit of root `pub use` declarations found no forwarding export from any
lower workspace crate. Remaining root exports name adapters that add product
behavior.

## Current acceptance evidence

1. `mez-agent::turn_runner::tests::fake_provider_and_ports_complete_one_agent_turn`
   covers canonical request assembly, provider recovery, MAAP validation and
   execution projection, result replay, ledger transitions, and completion.
2. The `mez-mux` `headless_client_*` tests cover fake pane-process output,
   terminal emulation, viewport presentation, input routing,
   resize/focus/layout effects, copy mode, redraw, readiness, and output work.
3. The independent `mez-terminal` suite contains 120 one-surface engine tests.
4. Root terminal, runtime, and async-runtime integration suites retain real PTY,
   host restoration, product agent-to-mux, persistence, and transport coverage.
5. The package audit still proves five packages, direct dependency direction,
   no lower-contract forwarding exports, no canonical root parser wrappers,
   and no wildcard imports in production agent/runtime/terminal module facades. It does not prove decomposition
   completion. The root ownership manifest records the remaining temporary
   surfaces, and architecture checks must preserve them until each named
   behavior has moved to its final owner.

## Final mux behavior audit

The reopened client/render/effect audit traced production behavior rather than
classifying files by size. Pane composition calls lower neutral render plans,
geometry, canvases, dividers, and frame-row layout. Root frame code resolves
product `Terminal*FrameContext` fields, configured `UiTheme` renditions, agent
status animation, and mouse hit actions. Root prompt/overlay code binds product
readline selectors, agent live state, and display overlays to lower layout and
style primitives. Session layout mutations execute in `mez-mux::session` and
return typed pane-resize transitions; runtime code translates those effects to
PTY resize and render invalidation I/O. The duplicate sync client loop and
generic host-paste decoder were the remaining ownership violations found by
that audit, and both are now removed from root.

The decomposition acceptance criteria are not yet satisfied. The implementation
sequence and closure requirements are documented in the local refactor ledger;
this tracked matrix and the root ownership manifest must be updated after every
completed extraction phase.
