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

## Lower-crate ownership

| Surface | Final owner | Current evidence | State | Boundary evidence |
|---|---|---|---|---|
| Stable identifiers | `mez-core` | `crates/mez-core/src/ids.rs`; product consumers import `mez_core::ids` directly | owned | No root compatibility facade remains. |
| Terminal geometry, history, protocol, state, style, width, profiles, and screen parser | `mez-terminal` | `crates/mez-terminal/src/{geometry,history,protocol,state,style,width,profile,screen}.rs` and crate-owned screen tests | owned | Root consumers import terminal contracts directly. The root retains only the explicitly named OSC 133 decoder and host-policy adapters. |
| Layout, session state/effects, PTY processes, input contracts, theme, and copy/readline primitives | `mez-mux` | `crates/mez-mux/src/{layout,session,process,input,theme,copy,readline}` | owned | The lower-crate fake pane-output-to-terminal-screen-to-headless-client flow covers input routing, resize/focus/layout effects, copy-mode transitions, and redraw. Styled terminal-derived copy state plus prompt buffer ownership, reverse history search, multiline navigation, and baseline terminal-input transitions are mux-owned; product transcript/Markdown normalization and selector candidate policy remain adapter concerns. |
| Mux presentation geometry and canvas primitives | `mez-mux` | `crates/mez-mux/src/{presentation,render}.rs` and `crates/mez-mux/src/render/{overlay,prompt,style}.rs` | owned | Neutral window render planning (including zoom selection, pane geometry, frame reservations, and divider-frame merging), pane-to-canvas composition, divider rendering, exact-width pane/window/group frame-row composition, generic frame/status template expansion, semantic right-status composition and placement, attached-client status-row composition, Unicode-aware window/group frame pillbox layout, and prompt wrapping/viewport/cursor/shadow-region layout are mux-owned. Product pane content, prompt kinds and summary policy, field resolution, merged-frame overlays, palettes, animation, and hit-action policy remain adapters. |
| Agent contracts and provider-independent policy | `mez-agent` | `crates/mez-agent/src/` owns action surfaces, schemas, context policy, provider contracts, scheduler, canonical turn records/ledger, execution ports, production turn orchestration, patch parsing, and shell transaction contracts | owned | The canonical async turn runner operates on production request, response, MAAP, action-result, transcript, and ledger contracts. Root sync tests and the production async adapter both invoke that state machine through an injected product environment; the retired parallel acceptance DTO model cannot be restored without failing the architecture check. |

## Root agent adapter audit

| Root surface | Final owner / root role | State | Boundary evidence |
|---|---|---|---|
| `src/agent/actions/` | `mez-agent` harness plus root local-action, permission, provider, transcript-store, and runtime adapters | adapter | Initial and resumed action-result planning, batch continuation/recovery, memory guardrails, request gating, canonical execution projection, lifecycle derivation, transcript projection, shell-output decoding, execution ports, and production turn orchestration are lower-owned. Root supplies concrete permission/scope/MCP facts, request assembly, provider and transcript I/O, pane/MCP/filesystem dispatch, and product error projection through `AgentTurnEnvironment` and execution-port implementations. |
| `src/agent/context/` | `mez-agent` with root product-input adapters | adapter | Canonical records, compaction, request projection, evidence/cache/provider shaping, skill constraints, and all context appenders are lower-owned. Root context code supplies only turn identity and embedded prompt assets through one assembly function; the ignored retained-tail compatibility API is removed. All appender and request-shaping scenarios run lower; root retains one turn-record and embedded-asset adapter regression. |
| `src/agent/maap.rs` | root error and shell-policy adapter over `mez-agent` | adapter | Canonical action/domain types, parsing, normalization, validation, and 25 intrinsic protocol regressions are lower-owned. Root retains product error projection, shell policy, execution formatting, and one shell-policy callback integration. |
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
| `src/terminal/render/` | product presentation adapters over `mez-mux` | adapter | Geometry, canvases, pane placement/clipping, dividers, frame/status layout, overlay pagination, prompt layout, color/contrast math, gradients, and style-span coalescing are mux-owned. Root injects product pane/frame fields, merged-frame policy, agent/readline/permission/overlay view models, labels, configured themes, animation timing, and hit actions. |
| `src/terminal/client_loop.rs` | `mez-mux` headless client policy plus root host I/O adapter | adapter | Neutral readiness, lifecycle, output precedence, input, layout/focus/resize, copy-state, and redraw planning are mux-owned and covered headlessly. The root loop now consumes those contracts directly and retains OS polling, raw-mode lifecycle, terminal encoding, and terminal FD operations. |
| `src/terminal/copy.rs`, `client_loop.rs`, `mouse.rs` | `mez-mux` domain plus root product policy adapters | adapter | Styled copy state, viewport/navigation/search/selection, key transitions, client lifecycle, and redraw planning are mux-owned. Root retains transcript/Markdown clipboard normalization, agent selectors, templated actions, overlays, host clipboard execution, and attached-host policy. |
| `src/terminal/host_clipboard.rs` | root host clipboard process adapter | adapter | Keep platform command discovery and process execution product-owned; generic paste-buffer state is owned by `mez-mux`. |
| `src/terminal/fd.rs` | root host terminal adapter | adapter | Keep raw terminal mode, FD polling, and host restoration product-owned; depend on mux/terminal contracts directly. |
| `src/terminal/screen.rs` | root OSC 133 product adapter over `mez-terminal` | adapter | Keep the explicitly named shell-transaction decoder product-owned; do not restore the removed profile facade or add terminal-screen forwarding here. |
| `src/terminal/mod.rs` | product presentation/host adapter facade | adapter | Lower mux status, viewport, theme, attached-client view/output, and cursor contracts are imported directly. The remaining exports name product host, copy-normalization, mouse-action, and presentation adapters. |
| `src/terminal/tests/` | product presentation and host integrations | adapter | Pure key/default-binding and SGR-mouse parsing, built-in theme contracts, and status/divider presentation boundaries run directly in `mez-mux`. Root retains product mouse routing, copy normalization, pane/frame/agent/readline/overlay composition, configured prompt styling, real fd/raw-mode behavior, and attached-client host-loop integrations. |
| `src/readline/` | `mez-mux` generic prompt behavior plus root command/selector adapter | adapter | Prompt buffer ownership, reverse history search, multiline navigation, baseline terminal-input transitions, and decoding are mux-owned. Root retains product command completion, selector discovery/cycling, prefixes, runtime effects, and agent-specific presentation policy. |

## Other root subsystem audit

| Root surface | Final owner / root role | State | Boundary evidence |
|---|---|---|---|
| `src/macros/` | `mez-agent` macro contracts plus root asset/discovery adapter | adapter | Macro identities, ordered-step/document/invocation parsing, validation, catalog precedence, and model/JSON projection are lower-owned. Root retains bounded filesystem discovery, embedded product assets, source/path attachment, product error projection, and runtime dispatch. Six intrinsic tests run lower; five asset/filesystem integrations remain root-owned. |
| `src/subagent/` | `mez-agent` spawn/profile/scope state plus root effect-classification adapter | adapter | Canonical records, validation, profiles, and scope conflicts are imported directly from `mez-agent`. Root retains only friendly presentation names and shell/patch permission-path classification implementing the lower enforcement port. |
| `src/command/` | `mez-mux` command grammar/plans/presentation plus product dispatch | adapter | Typed session-mutation plans, command defaults, shell-argument reconstruction, mux validation, and session list/chooser presentation are lower-owned. Root retains product error projection, concrete session mutation/process lifecycle, registry/help/status and product outcomes, cross-cutting dispatch, and auth/config/audit/agent/store adapters. |
| `src/runtime/`, `src/async_runtime/` | product composition | adapter | Keep serialized ownership, supervision, persistence, scheduling, transport, and effect execution. Ensure deterministic lower-crate transitions are invoked rather than duplicated. |
| auth, config, control, audit, hooks, MCP, memory, issues, snapshot, transcript stores | product policy/persistence/transport | adapter | Keep concrete stores and transports in root; implement narrow lower-crate ports and convert errors once at boundaries. |

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
5. The previous package audit found no root lower-contract forwarding exports.
   `cargo metadata`, `cargo tree`, and `cargo package --list` confirmed five
   packages, the intended workspace edges, no crate features, and owned package
   contents. The reopened audit still requires readline test cleanup, the mux
   effect/client/render ownership review, stronger source guardrails, and a
   fresh final package and public-API audit before completion can be claimed.

Update this matrix whenever ownership or an adapter boundary changes. The
decomposition acceptance criteria remain open while the reopened audit items
above are unfinished.
