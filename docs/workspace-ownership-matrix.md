# Workspace ownership matrix

This matrix audits the remaining production modules against the five-package
workspace architecture. A legal Cargo dependency graph is necessary but not
sufficient: code is complete only when deterministic subsystem behavior lives
with its owning lower crate and the root package contains product policy,
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

| Surface | Final owner | Current evidence | State | Required follow-up |
|---|---|---|---|---|
| Stable identifiers | `mez-core` | `crates/mez-core/src/ids.rs`; product consumers import `mez_core::ids` directly | owned | No root compatibility facade remains. |
| Terminal geometry, history, protocol, state, style, width, profiles, and screen parser | `mez-terminal` | `crates/mez-terminal/src/{geometry,history,protocol,state,style,width,profile,screen}.rs` and crate-owned screen tests | owned | The unused root profile facade is removed; remove remaining screen/style/state forwarding exports while keeping explicitly named product adapters for OSC 133 and host policy. |
| Layout, session state/effects, PTY processes, input contracts, theme, and copy/readline primitives | `mez-mux` | `crates/mez-mux/src/{layout,session,process,input,theme,copy,readline}` | owned | The lower-crate fake pane-output-to-terminal-screen-to-headless-client flow covers input routing, resize/focus/layout effects, copy-mode transitions, and redraw. Styled terminal-derived copy state plus prompt buffer ownership, reverse history search, multiline navigation, and baseline terminal-input transitions are mux-owned; product transcript/Markdown normalization and selector candidate policy remain adapter concerns. |
| Mux presentation geometry and canvas primitives | `mez-mux` | `crates/mez-mux/src/{presentation,render}.rs` and `crates/mez-mux/src/render/{overlay,prompt,style}.rs` | owned | Neutral window render planning (including zoom selection, pane geometry, frame reservations, and divider-frame merging), pane-to-canvas composition, divider rendering, exact-width pane/window/group frame-row composition, generic frame/status template expansion, semantic right-status composition and placement, attached-client status-row composition, Unicode-aware window/group frame pillbox layout, and prompt wrapping/viewport/cursor/shadow-region layout are mux-owned. Product pane content, prompt kinds and summary policy, field resolution, merged-frame overlays, palettes, animation, and hit-action policy remain adapters. |
| Agent contracts and extracted pure policy | `mez-agent` | `crates/mez-agent/src/` owns action surfaces, schemas, context validation, provider contracts, scheduler, patch parsing, and ports | owned | Move the reusable turn harness and provider-independent execution/state machines, not just their DTOs. |

## Residual root agent audit

| Root surface | Final owner / root role | State | Required migration |
|---|---|---|---|
| `src/agent/actions/` | `mez-agent` harness; root local-action, permission, transcript, and runtime adapters | temporary | Move deterministic gating, planning, recovery, result-context shaping, and turn execution state into `mez-agent`. Retain concrete shell/MCP/filesystem dispatch behind narrow ports. |
| `src/agent/context/` | `mez-agent` | temporary | Move message/context assembly, compaction, evidence/provenance shaping, model selection preconditions, and intrinsic tests. Inject product guidance, memory, instructions, skills, and MCP summaries. |
| `src/agent/maap.rs` | root error and shell-policy adapter over `mez-agent` | adapter | Canonical action/domain types, parsing, normalization, and validation are lower-owned; keep only product error projection, shell policy, and execution formatting here. |
| `src/agent/provider/` | `mez-agent` provider behavior plus root credential/HTTP/runtime adapters | temporary | Move provider-independent OpenAI/Anthropic/DeepSeek request, response, schema, cache, and model behavior. Retain concrete auth stores, reqwest transport, refresh, and runtime event conversion in root adapters. |
| `src/agent/prompt.rs` | `mez-agent` with root embedded-asset adapter | temporary | Move provider-neutral prompt assembly; inject repository instructions and product-owned embedded assets. |
| `src/agent/semantic/` | `mez-agent` planning plus root filesystem adapter | temporary | Move deterministic snapshot interpretation, matching, and transaction planning. Retain filesystem reads/writes and shell execution behind `LocalActionExecutor`. |
| `src/agent/session.rs`, `turn.rs` | `mez-agent` | temporary | Move deterministic state machines and intrinsic tests; keep presentation and runtime mutation in product adapters. |
| `src/agent/slash.rs` | root agent-shell execution adapter over `mez-agent` | adapter | Canonical slash registry, parser records, effects, and intrinsic parsing tests are lower-owned. Keep product session mutation, display rendering, runtime-effect routing, and product error projection here. |
| `src/agent/network.rs`, `shell.rs` | `mez-agent` contracts plus root transport adapter | temporary | Move provider-independent protocol and action behavior; retain network I/O and pane-shell transport in root. |
| `src/agent/mod.rs` | product composition facade | temporary | Canonical MAAP, action-result, turn-state, transcript, MCP execution, and provider-transcript records are no longer re-exported; continue replacing provider/context/prompt/semantic compatibility exports with explicit product adapters. |
| `src/agent/tests/` | split by behavior owner | temporary | Readiness tests now run directly in `mez-agent`; move remaining intrinsic harness/provider/context/MAAP/patch tests and retain tests that exercise concrete product stores, transports, permissions, runtime, or UI. |

## Residual root mux and terminal audit

| Root surface | Final owner / root role | State | Required migration |
|---|---|---|---|
| `src/terminal/render/` | `mez-mux` composition plus root product presentation adapter | temporary | Generic render cells, normal/zoomed window render planning, pane placement and clipping, plain/styled pane canvas composition, divider composition, exact-width pane/window/group frame-row layout, generic frame/status template parsing, semantic right-status segmentation and row placement, attached-client status-row composition, window/group frame pillbox layout, prompt wrapping/viewport/cursor/shadow-region layout, terminal color/contrast math, animation gradients, and style-span coalescing are mux-owned. Retain product pane content, prompt kinds and summary policy, field resolution, merged-frame overlays, injected agent/prompt/permission/overlay view models, palettes, animation timing, and hit actions while moving the remaining neutral rendering behavior. |
| `src/terminal/client_loop.rs` | `mez-mux` headless client policy plus root host I/O adapter | adapter | Neutral readiness, lifecycle, output precedence, input, layout/focus/resize, copy-state, and redraw planning are mux-owned and covered headlessly. The root loop now consumes those contracts directly and retains OS polling, raw-mode lifecycle, terminal encoding, and terminal FD operations. |
| `src/terminal/copy.rs`, `client_loop.rs`, `mouse.rs` | `mez-mux` domain plus root product policy adapters | adapter | Styled copy state, viewport/navigation/search/selection, key transitions, client lifecycle, and redraw planning are mux-owned. Root retains transcript/Markdown clipboard normalization, agent selectors, templated actions, overlays, host clipboard execution, and attached-host policy. |
| `src/terminal/host_clipboard.rs` | root host clipboard process adapter | adapter | Keep platform command discovery and process execution product-owned; generic paste-buffer state is owned by `mez-mux`. |
| `src/terminal/fd.rs` | root host terminal adapter | adapter | Keep raw terminal mode, FD polling, and host restoration product-owned; depend on mux/terminal contracts directly. |
| `src/terminal/screen.rs` | root OSC 133 product adapter over `mez-terminal` | adapter | Keep the explicitly named shell-transaction decoder product-owned; do not restore the removed profile facade or add terminal-screen forwarding here. |
| `src/terminal/mod.rs` | product presentation/host adapter facade | adapter | Lower mux status, viewport, theme, attached-client view/output, and cursor contracts are imported directly. The remaining exports name product host, copy-normalization, mouse-action, and presentation adapters. |
| `src/terminal/tests/` | split by behavior owner | temporary | Move neutral rendering/input/copy tests to `mez-mux`; retain real host-loop, product overlay, agent annotation, and raw-mode integration tests. |
| `src/readline/` | `mez-mux` generic prompt behavior plus root command/selector adapter | adapter | Prompt buffer ownership, reverse history search, multiline navigation, baseline terminal-input transitions, and decoding are mux-owned. Root retains product command completion, selector discovery/cycling, prefixes, runtime effects, and agent-specific presentation policy. |

## Other root subsystem audit

| Root surface | Final owner / root role | State | Required migration |
|---|---|---|---|
| `src/macros/` | `mez-agent` state machine plus root asset/discovery adapter | temporary | Move parsing, validation, catalog semantics, and judge/retry state. Retain filesystem discovery, embedded product assets, and runtime dispatch in root. |
| `src/subagent/` | `mez-agent` spawn/profile/scope state plus root effect-classification adapter | adapter | Canonical records, validation, profiles, and scope conflicts are imported directly from `mez-agent`. Root retains only friendly presentation names and shell/patch permission-path classification implementing the lower enforcement port. |
| `src/command/` | `mez-mux` pure mux grammar/plans plus product dispatch | temporary | Extract only commands that mutate mux state without auth/config/audit/agent/persistence concerns. Keep cross-cutting dispatch product-owned. |
| `src/runtime/`, `src/async_runtime/` | product composition | adapter | Keep serialized ownership, supervision, persistence, scheduling, transport, and effect execution. Ensure deterministic lower-crate transitions are invoked rather than duplicated. |
| auth, config, control, audit, hooks, MCP, memory, issues, snapshot, transcript stores | product policy/persistence/transport | adapter | Keep concrete stores and transports in root; implement narrow lower-crate ports and convert errors once at boundaries. |

## Transitional compatibility surfaces

The following current exports are migration markers, not completion evidence:

- `src/agent/mod.rs` exports root product implementations through one facade;
  its former broad canonical-contract block is private, and product consumers
  import those records directly from `mez-agent`. Provider, context, prompt,
  and semantic submodules still expose compatibility exports.
- `src/terminal/mod.rs` exposes product copy/render and host-I/O adapters; lower
  mux status, viewport, theme, and attached-client contracts are imported
  directly from `mez-mux`.
- `src/readline/` specializes mux-owned prompt state with product command and agent selector policy; it no longer owns neutral reverse-search or multiline transition state.
- Product provider and runtime modules still forward selected `mez-agent`
  contracts. Permission enums, MCP prompt records, instruction discovery
  records, and provider-facing config constants are imported directly from
  `mez-agent` and are no longer exposed through product subsystem facades.

These surfaces must be removed or narrowed to an adapter that adds documented
product behavior. Consumers should otherwise import the owning crate directly.

## Acceptance evidence still required

1. A complete `mez-agent` fake-provider/fake-port turn covering context,
   request/response, MAAP validation, action execution/result, recovery,
   transcript persistence, and completion.
2. A headless `mez-mux` fake-process/fake-client flow covering terminal output,
   viewport composition, input routing, resize/focus/layout effects, copy mode,
   and redraw. This is covered by the `headless_client_*` presentation tests;
   retain product integration coverage for host I/O and runtime effect adapters.
3. Independent `mez-terminal` tests covering the complete one-surface engine.
4. Root end-to-end tests for real PTY/host restoration and product agent-to-mux
   adapters.
5. Removal of all temporary facades listed above, followed by public API,
   direct dependency, duplicate dependency, feature, and package-content
   audits.

Update this matrix whenever ownership or a migration state changes. The
decomposition issue may be resolved only when no `temporary` row remains and
the acceptance evidence is recorded.
