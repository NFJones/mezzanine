# Agent pane UX: a calmer conversation and a clearer composer

## Status and recommendation

Design proposal with the first composer stage implemented. The runtime now
supplies typed submission/binding context and the shared prompt block adds
display-only context/help in comfortable geometry, retaining compact panes and
existing input semantics. Transcript renditions have been restored to the pinned
pre-refactor baseline: category-colored gutters, bold assistant/user labels and
dim status/thinking text. The quieter rail remains a proposal, not the adopted
appearance. Explicit retained activity list/detail inspection now exists through
`/show-context activity`, with typed component ownership and exact exports.
Automatic inline folding remains unimplemented. The evidence table records the original inspected
baseline, not every current behavior. Synthetic rendering/layout regressions
are not measured usability or real-terminal font evaluation.

**Recommend a lightweight conversation/activity/composer layout, not a boxed
chat application inside every pane.** Improve input ownership and information
hierarchy first; introduce expandable activity groups only after giving them
reliable semantic identity. Preserve Mezzanine's multiplexer density, keyboard
editing, explicit execution boundaries, and stable logs.

## What the current implementation establishes

| Evidence | UX implication |
| --- | --- |
| `runtime/render/presentation/style.rs` defines the two-cell `▐ ` prefix; `presentation/text.rs` applies it to each rendered line. `SPEC.md` requires repeat gutters and source-aware continuation indentation. | The rail reliably distinguishes the agent surface, but its visual weight competes with content. The latter is a design judgment, not a demonstrated usability measurement. |
| `host/terminal/render/prompt.rs::render_agent_prompt_block` composes a full-width colored input surface with `❱ ` and bounds input to approximately half the pane body. | Multiline editing already exists. Modernization should expose it, not replace the editor. |
| `prompt_can_show_agent_live_footer` admits running status only when the prompt is empty and no selector is open. The corresponding `agent_prompt.rs` test deliberately verifies its disappearance when typing. | Status and editing currently compete for the same space. Typing guidance obscures elapsed time and the interrupt hint in that row, although pane-frame status can still be available. |
| `runtime/render/client_view.rs` derives running, routing, thinking, executing, waiting, and approval states. `runtime/render/input.rs` intercepts Escape for active-turn interruption. | Display submission and stop semantics from the actual input/runtime state, rather than guessing from a generic busy flag. |
| `ui/readline` and `mez-mux::readline` already support history, reverse search, completion, Unicode editing, multiline navigation, and structured paste placeholders. | These are valuable capabilities with limited in-place discoverability. Preserve their baseline key behavior. |
| `runtime/commands/mod.rs::inject_agent_steering_for_running_turn` commits ordinary mid-turn input as guidance. | A running composer should say “Guide this task,” not imply that Enter starts an independent task. |
| `host/terminal/render/frame/pane.rs` subtracts reserved composer rows and crops content; `runtime/processes/mod.rs::pane_process_size_for` shares prompt reservation with process sizing. | Borders, padding, and helper rows are geometry changes, not merely paint. A single layout calculation must govern rendering, cursor placement, hit testing, and reserved rows. |
| `storage/transcript/types.rs::AgentPresentationEntry` retains sequence, optional turn ID, styles, source, and media type, but no general action/execution/component identity. Thinking summaries and rationale use the same renderer media type. | Semantic reflow is a useful foundation, but reliable per-action grouping and per-component disclosure require additional metadata. Neither timestamps nor rendered labels are sufficient. |
| Presentation settlement, replay, copy metadata, and attached-terminal parity have focused regressions. | A redesign must not reintroduce command flicker, rationale replacement, duplication, or decoration in copied source. |

The existing theme system already separates prompt, assistant, user, status,
command, and error colors. It also has pane status presets, narrow-rail overflow,
zen mode, and reduced motion. Do not build a second competing configuration or
status system to obtain a new appearance.

## Research: applicable patterns, not a new framework dependency

1. **Charm Bubbles:** multiline text areas, independently scrolling viewports,
   lists, and compact/expanded help generated from keybindings are explicit
   component patterns. Adopt their separation of responsibilities and contextual
   help, not a Go framework in this Rust application.
   [Bubbles documentation](https://github.com/charmbracelet/bubbles#readme).
2. **Textual:** the footer exposes bindings for the focused widget; its command
   palette provides fuzzy search and short descriptions; themes distinguish
   focused/blurred borders, readable foregrounds, muted text, and input surfaces.
   These support discoverable controls and a restrained semantic palette.
   [Footer](https://textual.textualize.io/widgets/footer/),
   [command palette](https://textual.textualize.io/guide/command_palette/),
   [theme documentation](https://textual.textualize.io/guide/design/).
3. **Terminal.Gui:** navigation guidance explicitly calls for one input focus,
   visible focus cues, and a keyboard route to every interaction. Apply those
   principles within Mezzanine's existing pane focus and prefix-key system;
   do not transplant Tab navigation over prompt completion.
   [Navigation](https://tui-cs.github.io/Terminal.Gui/docs/navigation.html).
4. **OpenCode:** its documented TUI separates tool-detail visibility, command
   discovery, external editing, message navigation, and editing shortcuts. It
   retains `Ctrl+J` alongside enhanced Enter variants and explains terminal
   limitations. This is useful evidence for progressive disclosure and portable
   composer controls, not a reason to adopt its bindings or `@` file semantics.
   [TUI](https://opencode.ai/docs/tui/),
   [keybinds](https://opencode.ai/docs/keybinds/).
5. **Crush:** its documented command-palette theme preview has explicit confirm
   and cancel/revert behavior. A reversible presentation preview would help
   evaluate Mezzanine's proposed density and palette before changing defaults.
   [Crush README, Themes](https://github.com/charmbracelet/crush#themes).
6. **CLI Guidelines:** human-first design, saying just enough, and ease of
   discovery are transferable principles. The guide explicitly excludes
   full-screen TUIs; it is not evidence for a particular full-screen layout.
   [CLI Guidelines](https://clig.dev/#philosophy).
7. **W3C accessibility guidance:** meaning must not rely only on color; ordinary
   text and placeholder contrast matter. Use 4.5:1 as an engineering target for
   known foreground/background pairs, not as a blanket claim of terminal WCAG
   conformance. Font rendering, ANSI dim, palette remapping, and unknown default
   terminal backgrounds require real-terminal evaluation.
   [Use of color](https://www.w3.org/WAI/WCAG22/Understanding/use-of-color.html),
   [contrast](https://www.w3.org/WAI/WCAG22/Understanding/contrast-minimum.html).
8. **Kitty keyboard protocol:** legacy terminal inputs can be ambiguous; richer
   modified keys require negotiated enhancements. Keep a portable key path for
   every action. Never make Shift+Enter the only newline mechanism.
   [Keyboard protocol](https://sw.kovidgoyal.net/kitty/keyboard-protocol/).

These are documented capabilities and design principles, not comparative user
studies. Sources were read as text; no live competitor comparison, screenshot
evaluation, or user testing was performed. Main-branch documentation can change.

## Proposed experience

### 1. A state-aware composer: highest priority

Keep `❱ ` and existing editing/submission semantics. Give the input a subtle
surface and a lightweight upper separator instead of a complete four-sided box.
Use the existing pane frame for model, project, and policy identity; do not repeat
all of those fields in the composer.

Illustrative comfortable layout; sample commands and outcomes are fictional:

```text
│ user> Make the command logs easier to follow.
│ thinking: Check how streamed commands become retained logs.
│ $ cargo test -p mezzanine --lib --quiet command_promotion
│ agent: command succeeded
│ mez> The accepted command rows now remain separate from their rationale.

── Guide this task ─────────────────── executing · 12s · Esc stop ──
❱ Also check short panes.
  Enter guide · Ctrl+J newline · / commands · [effective editor binding]
```

The rail is muted; main assistant prose uses a readable neutral foreground;
user/command labels carry the accent; errors retain both a word and a color.
The helper row is UI only and is omitted from copied/exported conversation text.

Composer state matrix:

| State | Input label / helper | Important distinction |
| --- | --- | --- |
| Idle | Ask Mez; Enter send; Ctrl+J newline | A request starts or continues the current conversation, not a raw shell command. |
| Active turn | Guide this task; Enter guide; current state/time; Esc stop | Guidance does not itself cancel or restart the current command. |
| Waiting approval | Approval required; show the actual review route | Typed prose does not silently approve a request. Reuse existing approval controls. |
| Selector open | Up/Down select; Tab complete; Enter behavior from selector state | Help must describe current dispatch precedence. Active-turn Escape currently interrupts before prompt decoding; changing that precedence is a separate behavior decision. |
| Reverse search | Search history; active accept/cancel bindings | Do not advertise the ordinary send action while search owns input. |
| Long multiline draft | Visible row range, e.g. lines 4–8 of 12 | All text remains editable and exact; the counter describes the draft, not model tokens. |
| Paste discard | Existing explicit rejection/reset notice | No suggestion, border, or key hint may accidentally reinterpret discarded bytes. |
| Read-only observer / inactive pane | Non-editable or unfocused visual treatment | Show no competing edit cursor or suggestion that this view accepts input. |

Hints must come from effective bindings and actual state. Do not invent a new
global `Ctrl+P` shortcut: readline history and existing prefix behavior must
remain intact. Reuse slash completion for a richer discoverable command list,
with descriptions and optional effective shortcuts. `/help` remains available.

**Responsive geometry:** start prototypes with a comfortable three-row shell
(separator/status, input, helper) when the pane body is at least 14 rows and
64 cells wide. Smaller panes remove helper text first, then merge status into
the existing pane rail, and retain a one-row editor. These thresholds are
prototype parameters, not proposed permanent configuration fields. Never lose
the final content row to decoration. In zen mode the essential status must have
a composer fallback because the passive pane rail may be absent.

Keep reservation stable across idle/running state changes at a given geometry;
allow draft height to grow within the existing half-body bound. Do not repeatedly
resize the PTY because a duration gained a digit or a help string changed.

### 2. A quieter log rail and stronger hierarchy

First prototype `│ ` versus `▐ `, preserving the two-cell footprint. Recommend
the lighter `│ ` for the redesigned agent surface, but validate fonts and split
dividers before choosing the default. Retain textual speaker/type labels in the
first release: changing the rail and removing labels simultaneously makes both
accessibility and regression diagnosis harder.

- Use a muted rail independent of body styling. Currently gutter/label styling
  is derived from the row category; do not dim important command or answer text
  merely to quiet the rail.
- Use normal-weight body prose; reserve bold for labels, headings, focus, and
  urgent state. Avoid syntax-colored reasoning prose.
- Keep five-space continuation indentation and the Markdown hierarchy. Do not
  insert a border into source or treat a rail as an authored Markdown quote.
- Add restrained separation at user-turn boundaries only in comfortable mode;
  avoid a blank row around every tool action. Compact panes stay dense.
- Keep rationale visible by default. Present it as a short explanatory block,
  not an animation or simulated hidden chain of thought. A later display-only
  label such as “Reason” could be tested separately from the established
  `thinking:` label, without changing canonical assistant context.

Both gutter constants, prompt cursor offsets, soft-wrap continuation prefixes,
source-copy metadata, semantic replay, and legacy ANSI behavior need review.
This is not a repository-wide string replacement. Display rails are a visual
ownership cue, not an authentication boundary: untrusted text can mimic labels.

### 3. Readable activity groups: valuable, but a separate phase

Use semantic groups to make long work understandable without hiding what the
agent is doing. Keep the current rationale and command intent stable. Default
to a readable compact activity view, with deliberate expansion for large
settled results and diagnostics. Preserve failures and approval requests visibly.

```text
│ thinking: Check the focused regressions before the workspace suite.
│ $ cargo test ... command_promotion
│   [succeeded] 2.8s · 24 scenarios checked
│   [details] retained result available
```

Only show duration/count/status when runtime evidence actually supplies it.
Do not fabricate progress percentages, collapse approvals into green tool
badges, or relabel provisional intentions as executed commands.

The prerequisite is a presentation component model carrying conversation,
turn/response, action, attempt/transaction, component kind, and stable identity.
Add it at existing producer/persistence boundaries and project into the current
conversation screen. Do not add a second mutable timeline authority. Current
rendered labels, timestamps, and optional turn IDs cannot safely associate a
rationale, summary, command, and result—particularly across retries.

Folding must be an explicit view operation over retained semantic records,
not deletion, rollback, or a settlement-time auto-collapse. Keep expansion state
client-local unless explicitly specified otherwise. Reflow must anchor the
selected semantic component and preserve source-copy selection. Give keyboard
and mouse users equivalent controls. View-detail preferences must not change
model context, execution, audit, or the meaning of `/log-level`.

### 4. Better navigation and contextual discovery

- Reuse copy mode for reading older logs without commandeering the editor's
  Up/Down history navigation. Add explicit previous/next user-turn and
  previous/next failed-action navigation after semantic identity exists.
- Show “Reading history · new activity” with an effective return-to-live route.
  Do not force the viewport back to the bottom while the user reads or selects.
  Do not imply unread counts are already tracked: they need a defined baseline.
- Improve the existing slash selector with human descriptions and state-aware
  availability. Preserve `@` as the current MCP mention mechanism; importing
  OpenCode's file-reference meaning would change behavior and context authority.
- Use one concise idle suggestion, not rotating tips or a large welcome banner
  in every pane. Hide suggestions immediately when the user types; they never
  become draft text, transcript entries, or model input.

## Architecture and implementation sequence

| Phase | Concrete work and owners | Risk / exit condition |
| --- | --- | --- |
| A: executable visual prototypes | Synthetic state fixtures in `host/terminal/tests/presentation/agent_prompt.rs`; screenshot/ANSI harness using existing attached-client frame encoding. Compare current, light-rail, and lightweight composer designs. | No production change. Evaluate 120x40, 80x24, 40x12, and short split panes, light/dark themes, reduced motion, and zen. |
| B: composer clarity and low-risk styling | Typed composer layout/context in `host/terminal/render/prompt.rs` and `mez-mux::presentation`; focus/status from `runtime/render/client_view.rs`; shared reservations in `runtime/processes/mod.rs`; hints from input/readline/selector owners. | Medium geometry risk. Cursor, PTY sizing, mouse targets, and rendering use the same plan. No hidden status while editing where space permits; compact fallback stays usable. |
| C: rail and visual hierarchy | `presentation/style.rs`, `text.rs`, `actions.rs`, `buffer_apply` projection/replay, terminal continuation and copy adapters; theme roles in `mez-mux::theme`. | Medium replay/copy risk. Accepted rows stay in place through streaming and settlement; copied source is unchanged; semantic resize and resume use the new presentation correctly. |
| D: semantic activity navigation | `AgentPresentationEntry`, encoding/store owners, provider/action presentation producers, component indexing and existing surface-specific copy/navigation state. | High identity/persistence risk. Establish retry-safe component ownership and a storage-version decision before implementing folding or result inspection. |

Use existing abstractions; no framework migration, no new provider integration,
no changes to the thin product entry point. If new theme/config keys are needed,
add documented defaults, schema validation, live-mutation rules, a primary-config
migration, examples, and tests. Theme color additions and durable presentation
metadata are separate versioning decisions; a config bump alone does not migrate
saved presentation records. Update `SPEC.md` only when adopting a behavior,
not to turn this proposal into a normative requirement prematurely.

## Validation and adoption criteria

1. **Visual/state matrix:** idle, active empty input, steering draft, multiline,
   selector, search, approval, failure, interrupted, unfocused, observer, copy
   mode, paste-discard, and external-editor takeover. Include hidden pane frames
   and zen mode. Essential text survives narrow widths before optional hints.
2. **Input:** unchanged exact submitted bytes and paste provenance, portable
   Ctrl+J, history/search/editing, prefix bindings, interruption precedence,
   slash commands, and no draft leakage between panes or conversations.
3. **Logs:** preserve rationale/summary/command as separate single-copy records;
   retain accepted rows through validation, output-tail replacement, settlement,
   cancellation, and retries; no results imply success before settlement.
4. **Rendering:** compare incremental attached-terminal styled cells against
   independent full draws; no full-screen clears at stable geometry; Unicode,
   wide glyphs, combining marks, split borders, cursor placement, and reflow.
5. **Accessibility:** readable known color pairs, textual state distinctions,
   visible focus, no mandatory Nerd Font/emoji/animation, monochrome evaluation,
   keyboard routes for all controls. Keep a plain text inspection/export path;
   do not claim universal terminal screen-reader compatibility.
6. **Performance/platforms:** bounded component retention, cached reflow, no
   renderer filesystem/provider work, existing render-rate budgets, and Linux
   plus native macOS runs with both enhanced and legacy key reporting.
7. **Required checks:** `just fmt`, `just check`, `just clippy`, and
   `timeout 900s just test`, plus focused prompt, copy, resize, replay, and
   attached-terminal parity tests for the affected phases.

Before changing defaults, ask a small mixture of new and experienced users to
send a multiline request, guide running work, review an approval, locate a
failed command, copy a command without decoration, and return from history.
Record wrong submissions, help lookups, misidentified focus/state, completion
time, and user preference. Set quantitative targets after measuring the current
baseline; do not invent an improvement percentage from source inspection.

**Suggested first implementation slice:** state-aware composer with portable
contextual help, independent visible status, and a muted light rail. Defer full
boxes, automatic log collapse, new shortcut families, and sidebars. This yields
a noticeably more approachable pane without sacrificing the stable execution
log that Mezzanine has just repaired.
