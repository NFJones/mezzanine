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
| Stable identifiers | `mez-core` | `crates/mez-core/src/ids.rs` | owned | Remove the root `mez_core::ids` forwarding export and import IDs directly. |
| Terminal geometry, history, protocol, state, style, width, profiles, and screen parser | `mez-terminal` | `crates/mez-terminal/src/{geometry,history,protocol,state,style,width,profile,screen}.rs` and crate-owned screen tests | owned | The unused root profile facade is removed; remove remaining screen/style/state forwarding exports while keeping explicitly named product adapters for OSC 133 and host policy. |
| Layout, session state/effects, PTY processes, input contracts, theme, and copy/readline primitives | `mez-mux` | `crates/mez-mux/src/{layout,session,process,input,theme,copy,readline}` | owned | The lower-crate fake pane-output-to-terminal-screen-to-headless-client flow covers input routing, resize/focus/layout effects, copy-mode transitions, and redraw. Styled terminal-derived copy state is mux-owned; product transcript/Markdown normalization remains an adapter concern. |
| Mux presentation geometry and canvas primitives | `mez-mux` | `crates/mez-mux/src/{presentation,render}.rs` | owned | Neutral canvas, divider, exact-width pane-frame row, semantic right-status composition and placement, and Unicode-aware window/group frame pillbox layout are mux-owned. Continue moving generic frame-row composition while retaining product field, palette, animation, and hit-action policy in adapters. |
| Agent contracts and extracted pure policy | `mez-agent` | `crates/mez-agent/src/` owns action surfaces, schemas, context validation, provider contracts, scheduler, patch parsing, and ports | owned | Move the reusable turn harness and provider-independent execution/state machines, not just their DTOs. |

## Residual root agent audit

| Root surface | Final owner / root role | State | Required migration |
|---|---|---|---|
| `src/agent/actions/` | `mez-agent` harness; root local-action, permission, transcript, and runtime adapters | temporary | Move deterministic gating, planning, recovery, result-context shaping, and turn execution state into `mez-agent`. Retain concrete shell/MCP/filesystem dispatch behind narrow ports. |
| `src/agent/context/` | `mez-agent` | temporary | Move message/context assembly, compaction, evidence/provenance shaping, model selection preconditions, and intrinsic tests. Inject product guidance, memory, instructions, skills, and MCP summaries. |
| `src/agent/maap.rs` | `mez-agent` | temporary | Move MAAP action/domain parsing and validation. Keep product execution and error adaptation outside the crate. |
| `src/agent/provider/` | `mez-agent` provider behavior plus root credential/HTTP/runtime adapters | temporary | Move provider-independent OpenAI/Anthropic/DeepSeek request, response, schema, cache, and model behavior. Retain concrete auth stores, reqwest transport, refresh, and runtime event conversion in root adapters. |
| `src/agent/prompt.rs` | `mez-agent` with root embedded-asset adapter | temporary | Move provider-neutral prompt assembly; inject repository instructions and product-owned embedded assets. |
| `src/agent/semantic/` | `mez-agent` planning plus root filesystem adapter | temporary | Move deterministic snapshot interpretation, matching, and transaction planning. Retain filesystem reads/writes and shell execution behind `LocalActionExecutor`. |
| `src/agent/session.rs`, `turn.rs`, `slash.rs`, `readiness.rs` | `mez-agent` | temporary | Move deterministic state machines and intrinsic tests; keep presentation and runtime mutation in product adapters. |
| `src/agent/network.rs`, `shell.rs` | `mez-agent` contracts plus root transport adapter | temporary | Move provider-independent protocol and action behavior; retain network I/O and pane-shell transport in root. |
| `src/agent/mod.rs` | product composition facade | temporary | Replace broad re-exports with explicit `src/adapters/agent_*` modules after consumers import `mez_agent` directly. |
| `src/agent/tests/` | split by behavior owner | temporary | Move intrinsic harness/provider/context/MAAP/patch tests to `mez-agent`; retain tests that exercise concrete product stores, transports, permissions, runtime, or UI. |

## Residual root mux and terminal audit

| Root surface | Final owner / root role | State | Required migration |
|---|---|---|---|
| `src/terminal/render/` | `mez-mux` composition plus root product presentation adapter | temporary | Generic render cells, Unicode-width fitting/slicing, divider composition, pane-frame row layout, semantic right-status segmentation and row placement, and window/group frame pillbox layout are mux-owned. Move remaining generic frame-row composition; retain injected agent/prompt/permission/overlay view models, palettes, animation, and hit actions. |
| `src/terminal/client_loop.rs` | `mez-mux` headless client policy plus root host I/O adapter | adapter | Neutral readiness, lifecycle, output precedence, input, layout/focus/resize, copy-state, and redraw planning are mux-owned and covered headlessly. The root loop now consumes those contracts directly and retains OS polling, raw-mode lifecycle, terminal encoding, and terminal FD operations. |
| `src/terminal/copy.rs`, `client_loop.rs`, `mouse.rs` | `mez-mux` domain plus root product policy adapters | adapter | Styled copy state, viewport/navigation/search/selection, key transitions, client lifecycle, and redraw planning are mux-owned. Root retains transcript/Markdown clipboard normalization, agent selectors, templated actions, overlays, host clipboard execution, and attached-host policy. |
| `src/terminal/host_clipboard.rs` | root host clipboard process adapter | adapter | Keep platform command discovery and process execution product-owned; generic paste-buffer state is owned by `mez-mux`. |
| `src/terminal/fd.rs` | root host terminal adapter | adapter | Keep raw terminal mode, FD polling, and host restoration product-owned; depend on mux/terminal contracts directly. |
| `src/terminal/screen.rs` | root OSC 133 product adapter over `mez-terminal` | adapter | Keep the explicitly named shell-transaction decoder product-owned; do not restore the removed profile facade or add terminal-screen forwarding here. |
| `src/terminal/mod.rs` | product presentation/host facade | temporary | Mux theme and attached-client view/output/cursor forwarding are removed; continue removing broad copy/render exports and split host I/O from product presentation adapters. |
| `src/terminal/tests/` | split by behavior owner | temporary | Move neutral rendering/input/copy tests to `mez-mux`; retain real host-loop, product overlay, agent annotation, and raw-mode integration tests. |
| `src/readline/` | `mez-mux` generic prompt behavior plus root command/selector adapter | temporary | Move remaining neutral prompt and decoder integration. Retain product command completion, selectors, runtime effects, and agent-specific semantics. |

## Other root subsystem audit

| Root surface | Final owner / root role | State | Required migration |
|---|---|---|---|
| `src/macros/` | `mez-agent` state machine plus root asset/discovery adapter | temporary | Move parsing, validation, catalog semantics, and judge/retry state. Retain filesystem discovery, embedded product assets, and runtime dispatch in root. |
| `src/subagent/` | `mez-agent` state/effects plus root scope/runtime adapter | temporary | Move provider-independent lifecycle and messaging state. Retain pane creation, writer coordination, permission/path enforcement, and process effects in root. |
| `src/command/` | `mez-mux` pure mux grammar/plans plus product dispatch | temporary | Extract only commands that mutate mux state without auth/config/audit/agent/persistence concerns. Keep cross-cutting dispatch product-owned. |
| `src/runtime/`, `src/async_runtime/` | product composition | adapter | Keep serialized ownership, supervision, persistence, scheduling, transport, and effect execution. Ensure deterministic lower-crate transitions are invoked rather than duplicated. |
| auth, config, control, audit, hooks, MCP, memory, issues, snapshot, transcript stores | product policy/persistence/transport | adapter | Keep concrete stores and transports in root; implement narrow lower-crate ports and convert errors once at boundaries. |

## Transitional compatibility surfaces

The following current exports are migration markers, not completion evidence:

- `src/lib.rs` re-exports `mez_core::ids`.
- `src/agent/mod.rs` broadly re-exports `mez-agent` contracts and product
  implementations through one facade.
- `src/terminal/mod.rs` still forwards product copy/render surfaces; mux theme
  and attached-client presentation contracts are imported directly from
  `mez-mux`.
- `src/readline/mod.rs` and `src/readline/types.rs` forward mux readline types.
- Product permission, MCP, instruction, subagent, provider, config, and runtime
  modules still forward selected `mez-agent` contracts.

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
