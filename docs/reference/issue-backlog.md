# Issue Backlog

## Open Issues
None currently recorded.

## Implementation Plan

No implementation plan is currently active.

## Completed
- Command-output pager links once again show a visible movable selector gutter:
  the active row renders with a right-pointing triangle while other
  selectable rows keep an unmarked gutter, leaving the selected link obvious
  without adding background styling while link text stays bold, underlined,
  and theme-colored.
- Pager view links now preserve their rendered bold, underlined,
  theme-colored markdown styling without background treatment, including
  `/list-sessions` command overlay selections.
- `terminal.reduced_motion` now defaults to false and, when enabled, renders
  running agent status indicators as static status text instead of animated
  scan effects.
- `/list-sessions` command overlays now expose one selectable UUID link per
  session, avoiding a second hidden resume-command link for the same session.
- Pane rendering now preserves the display width of emoji-presentation warning
  signs before split borders, so borders remain fixed in their expected
  columns.
- Remaining group and agent-shell defaults now live behind prefix bindings
  instead of direct Alt-style accelerators, and the generated defaults,
  documentation, and help output describe the prefix defaults.
- The async control listener now serves accepted control socket connections in
  independent tasks, so a long-lived primary or observer connection cannot
  block later control connections from registering observer requests or
  servicing pane-spawn/control work.
- `mez attach --observer` registration now has async listener regression
  coverage proving a pending observer can initialize while the primary control
  socket remains open and is then visible through `observer/list`.
- Host clipboard integration now accepts optional
  `terminal.clipboard_copy_command` and `terminal.clipboard_paste_command`
  commands, using stdin for copy and stdout for paste while falling back to the
  default command list for omitted directions.
- The default window status layout now right-aligns status text to the final
  terminal cell instead of reserving an extra trailing blank column.
- Standalone top pane status/header rows now use space fill rather than
  horizontal box-drawing glyphs, while pane status rows merged into horizontal
  split dividers continue to use Unicode box drawing for the divider surface.
- Pane render regions now reserve right-side shared divider cells as well as
  bottom divider rows, so a selected left-pane agent prompt cannot draw text or
  prompt background over the vertical border.
- Shell-output Base64 transport decoding now returns only decoded child command
  output once transport markers are present, preventing `cat` output and model
  observations from retaining Mezzanine transaction wrapper echo.
- `/list-mcp` now renders a human-readable markdown display with server
  headings, status, retryability, blacklist reason, transport, and nested tool
  state instead of JSON-like object text.
- Stdio MCP subprocesses now receive a usable `PATH` for command lookup unless
  the server configuration explicitly sets or passes `PATH`, while undeclared
  environment variables remain isolated.
- Pane-local agent prompt Up/Down navigation now derives its body width from
  the same rendered pane region used by the terminal compositor, including
  split-pane divider reservation.
- `:list-observers` now opens a command display overlay for pending and
  approved observers, includes pending counts, and exposes concrete
  approve/reject/inspect/revoke action commands for selectable rows.
- Pending observer requests now emit an `observer_requested` lifecycle event and
  append a visible active-pane status line containing the observer request id.
- Agent prompt shell tab-completion shadow hints now carry a dim,
  contrast-managed grey style span in attached-client rendering.
- Default prefix key coverage now pins the tmux-compatible command table, while
  the specification and generated defaults distinguish prefix bindings from
  direct convenience accelerators.
- The built-in `mez-config` skill now lists concrete theme color slots, includes
  live annotated configuration schema guidance, and injects a current effective
  configuration summary when explicitly loaded for a prompt.
- Agent `config_change` now accepts `reset`, removing the explicit persisted
  override so lower-precedence config or defaults become effective again.
- Horizontal pane divider rows now use Unicode box-drawing glyphs without
  themed background fill on divider and junction cells, while title/status
  pills and the top terminal header preserve their themed backgrounds.
- Overlong right-aligned pane-frame agent status now preserves the rightmost
  horizontal border-fill cell, including pane frame rows merged into horizontal
  split dividers.
- `/mcp` has been renamed to `/list-mcp`; the agent-shell MCP display now
  exposes MCP server id, per-server state, retryability, transport, reason, and
  nested tool state.
- MCP server names now feed tab completion for agent `/list-mcp` arguments and
  terminal `:mcp-remove`/`:mcp-retry` arguments from the live runtime registry.
- Runtime MCP discovery now preserves existing runtime-owned transports across
  prompts and turns, lazily starts configured servers before provider task
  claims and `/list-mcp`, and logs load/failure events with failure reasons.
- Local MCP transports now share the Mez runtime lifetime and are dropped on
  explicit kill, forced shutdown, supervisor failure, and async actor shutdown.
- `mez-config` now includes live annotated config-change schema guidance with
  path purpose, value/type, format requirements, and supported operations,
  derived from the same config schema metadata used by provider action
  descriptions.
- Agent `config_change` control idempotency keys now include a stable payload
  fingerprint, so distinct settings in the same turn cannot conflict merely
  because they reused a local action id.
- Conversation compaction now injects an explicit model-facing notice when a
  compacted memory summary is present, explaining that older durable transcript
  entries were summarized and only the retained raw tail remains exact.
- Copy-mode raw text for wrapped markdown presentation now skips
  presentation-only continuation rows, preserving valid raw pipe-table markdown
  when rendered agent output wraps visually.
- Subagent panes now display the parent-supplied prompt as a `parent>` log entry
  before the child turn starts.
- `/dump-agent-context` has been renamed to `/copy-context`, with
  `pane`, `buffer [name]`, and `clipboard` targets matching the other copy
  commands.
- Agent prompt shadow completions now render only at the end of the active
  token, preventing `$skill` completion text from shifting multi-line prompt
  edits while the cursor is inside a token.
- Pane frame fill now uses horizontal box-drawing glyphs outside title and
  status pills, leaving the rightmost frame cell as border fill.
- Terminal `:` and agent `/` help displays now group commands into categories,
  and `:zoom-pane` has explicit help text.
- `/list-skills` now uses aligned table formatting for skill name, scope, and
  description.
- `/list-sessions` now renders bold conversation UUID links without showing
  internal `mez-agent:` destinations while preserving keyboard/mouse
  selection.
- Display overlay scrolling now clamps to the available content range so
  PageDown cannot scroll indefinitely past the final row.
- The agent system prompt now explicitly identifies the model as Mez.
- Built-in dark theme thinking/status greys now maintain a brighter neutral
  floor while preserving low-emphasis contrast.
- Pane-scoped project `.mezzanine/config.*` overlay discovery now refreshes
  before agent context construction, agent prompt submission, and `/list-skills`
  so long-running daemon sessions honor the active pane repository rather than
  only the daemon startup directory.
- Closing a parent agent shell now force-closes descendant subagent panes and
  clears child lineage/scope/routing state so subagents cannot remain orphaned
  after their parent session exits.
- `config_change` now follows the ordinary approval policy mechanism. It blocks
  for `/approve` under restrictive policies, but full-access, approval bypass,
  and auto-allow can accept or resume it through the same policy reconciliation
  path as other privileged actions.
- Built-in `mez-config` and `mez-manual` skills now ship with Mezzanine. The
  config skill includes the live annotated config-change schema, and the manual
  skill derives terminal and agent command indexes from the implementation
  registries.
- Built-in UI themes now contrast-manage muted and thinking aliases against
  their surface colors so inactive pane text, thinking/status transcript text,
  and syntax comment/operator text remain legible across dark and light themes.
- Command and agent prompt shadow-completion hints now use the lowest-emphasis
  readable grey for the active prompt background, including `$skill` hints.
- Default window and group pillbox titles now render as `<index> <title>` rather
  than `<index>: <title>`, matching the no-colon pane title style.
- The built-in `create-skill` workflow now defaults new skills to user scope
  unless the user explicitly asks for a repo/project-scoped skill.
- Skill discovery for `$skill` prompts, `/list-skills`, and tab completion is
  filesystem-backed at use time, so newly created skills do not require a
  session restart.
- Agent prompt hard wrapping now keeps the first wrapped input row at the top of
  the prompt region and no longer treats the `agent>` separator space as a word
  break for unbroken input.
- Idle Ctrl+C in a nonempty pane-local agent prompt now clears the prompt buffer
  first. Only an already empty idle prompt enters the double-confirm exit path.
- `/status` now displays billed provider input tokens as `input=` by subtracting
  provider-reported cached input tokens while preserving `raw_input=`,
  `cached_input=`, and cache-hit diagnostics.
- Recoverable URL fetch failures now render as terse warning-style logs while
  preserving status/content details in model-facing action results for bounded
  self-correction.
- Reverse search now keeps newest fuzzy-substring matching, uses repeated
  Ctrl+R for older matches, Tab for newer matches, and Shift-Tab for older
  matches.
- Readline history navigation now treats multi-line history entries as whole
  entries until the user explicitly moves/edits inside the loaded entry.
- Auto-sizing router guidance now separates model size as task scope from
  reasoning effort as task depth/complexity, reserves small models for chat,
  requires medium-or-higher reasoning for implementation, and requires high or
  xhigh reasoning for planning, investigation, complex implementation,
  debugging, architecture, and security review work.
- Agent shell prompt navigation now treats prompt rows consistently across
  explicit Ctrl+J newlines and rendered soft-wrap rows before falling back to
  history, and SS3 application-cursor arrow sequences are normalized by the
  readline prompt.
- `:list-themes` rows now expose immediate `set-theme` actions for built-in and configured themes.
- `/new` now clears the live pane viewport like Ctrl+L while starting a fresh visible conversation.
- `/resume` with no argument now delegates to `/list-sessions` instead of using a separate picker-only path.
- Active agent-session metadata now persists and restores the last provider-reported context-usage label alongside token totals.
- The agent pane approval pill now uses the same `full-access` label as the approval selector and command surface.

- Idle cleanup now honors actor-owned provider retry progress. A running turn
  waiting on retry backoff no longer fails as having no remaining progress path
  when an idle cleanup timer fires before the provider retry timer.
- `copy-mode` now initializes through the same live overlay viewport sizing as
  attached-terminal copy-mode entry, so entering copy mode no longer opens on a
  one-row-short view that shifts the pane buffer.
- Slash diagnostic export commands now use copy-oriented names:
  `/copy-trace-log` for the retained trace log and `/copy-patches` for
  retained patch records.
- `/copy` now copies the latest retained model `say.text` and accepts the same
  `pane`, `buffer [name]`, and `clipboard` targets as the diagnostic copy
  commands.
- `/copy-patches` now exports retained `apply_patch` payloads with turn/action
  ids and observed statuses to the pane, a named paste buffer, or the clipboard
  using the same target syntax as `/copy-trace-log`.
- `apply_patch` now tolerates omitted blank-only separator context at anchored
  add-only insertion boundaries, preserves those blank lines, and still rejects
  ambiguous matches or nonblank skipped content.
- Command-output pagers can now be dismissed with `q` as well as Escape.
- Terse single-line `:` and `/` command display output now logs to the active
  pane instead of opening a modal pager that only reports acknowledgement-style
  status.
- Standalone Escape now cancels primary command and pane-local agent reverse
  search without closing the surrounding prompt.
- Configured elevated approval defaults such as `full-access` are preserved
  during agent-session restore, and legacy metadata can no longer narrow the
  configured default.
- `apply_patch` now recovers from uniformly indented patch payloads and common
  copied path header shapes such as leading `./` or git-diff `a/`/`b/`
  prefixes, and the prompt/schema now emphasizes small anchored hunks with
  exact context as the reliable default.
- Ctrl+R reverse search now uses fzf-style matching so command and agent prompt
  history can be found by case-insensitive internal substrings and ordered
  query characters rather than strict prefixes.
- Pane-local agent prompt Up/Down handling now follows shared readline history
  navigation instead of intercepting ordinary arrow keys for soft-wrap cursor
  movement, so long single-line drafts recall the newest prompt history entry
  predictably.
- Plain `agent>` assistant output now uses the same width-bounded rendered-line
  wrapper as markdown output, aligning continuation rows under the first
  writable column after the `agent>` indicator.
- `apply_patch` hunk mismatch diagnostics now include structured recovery
  hints such as failure code, affected path, suggested next step, retry policy,
  and bounded read range alongside the current-file context snippet.
- Display-only `say` output that contains a raw Codex patch block now renders
  literally instead of being collapsed into `[mez: no output]`.
- `/list-sessions` command-overlay resume links now produce one keyboard
  selection stop per logical session item while keeping the resume link
  selectable.
- Shell-backed agent actions now inherit the remaining turn-wide timeout
  budget instead of honoring model-requested per-action shell timeouts.
- Idle Ctrl+C in the pane-local agent prompt now requires a second Ctrl+C
  within three seconds before exiting agent mode; active turns still interrupt
  immediately.
- Agent prompt and action-schema guidance now tells models to discover command
  invocation details only when needed and reuse them during the same work
  cycle instead of repeating equivalent discovery branches.
- Pane composition now clears the whole footprint of a wide glyph when a
  divider or other mux-owned single-cell element overwrites either half, so
  neighboring pane content cannot shift right on only those rows.
- `/resume --latest` now resolves the newest saved conversation using the same
  ordering as the saved-session picker, while `/resume` with no argument still
  opens the picker.
- Agent slash-command markdown display output now opens the shared command
  display pager instead of appending informational blocks into the pane
  transcript, and rendered `mez-agent:` session links remain selectable.
- Additional acknowledgement-only `:` commands now return immediately without
  opening a modal display overlay when the successful side effect is already
  observable.
- Count-based per-turn shell and network dispatch caps have been removed.
  Duplicate already-successful file mutations still short-circuit as
  idempotent successes, and network actions still apply URL, policy, and
  response-size validation.
- Compaction now overrides stale/latest turn status in pane frame metadata,
  counts as active agent work for the window while in progress, and continues
  to render `compacting` with the running agent-status style.
- Pane-local agent prompt continuation rows now align with the editable column
  after `agent>`, while still preferring word-boundary wraps and falling back
  to hard cell boundaries only for unbroken text.
- Conversation compaction now logs start, success, skip, and model-backed
  failure states to the pane while preserving the dedicated `compacting`
  in-progress state for provider-backed summary work.
- `/model --secondary <model> [reasoning]` now sets the auto-sizing router
  model, with `--secondary show`, `--secondary list`, and `--secondary clear`
  following the same command surface.
- Pane composition now omits internal continuation cells for wide glyphs, so
  double-width symbols such as `✅` do not push pane borders to the right.
- Agent output copy and transcript fork command rows now render as concise
  human-readable sentences instead of raw key/value records.
- Subagent spawns now inherit the parent pane's effective auto-reasoning
  setting before the child turn starts.
- Pane-local agent prompt wrapping now prefers word boundaries and falls back
  to hard cell boundaries for unbroken text. Up/Down move through soft-wrapped
  prompt rows before falling back to history navigation.
- Running shell-command tail rows now skip prompt repaint lines defensively and
  keep displaying the last decoded command-output line.
- Explicit live approval-policy changes now survive unrelated config reloads
  and approved config-change actions, matching the existing live approval-bypass
  preservation behavior.
- Agent markdown and shell-command wrapping now prefer word boundaries and fall
  back to character/cell boundaries when no whitespace break exists.
- Agent diff previews now wrap at the pane width or 120 columns, whichever is
  smaller, using prior whitespace as the only forced wrap point and preserving
  diff-gutter continuation indentation.
- Model-authored markdown `say` output no longer receives a full-width divider
  above the first `agent>` line.
- `apply_patch` initially required single-file patch actions for simpler
  recovery, and later broadened to accept related multi-file Codex patch blocks
  while still recommending separate actions for independent edits.
- `/list-modified-files` now renders compact `edited path (+N -M)` rows with
  colorized added and removed counts instead of a verbose object list.
- Provider token usage is now persisted in active agent session metadata,
  restored during daemon-style active-session recovery, and rehydrated when
  `/resume <conversation>` binds a saved conversation.
- Pane-local auto-reasoning overrides and the saved approval policy are now
  restored from agent session metadata during active-session recovery and
  `/resume <conversation>` without treating those explicit session choices as
  default configuration changes.
- The model-facing local file/directory inspection actions have been removed.
  Models now use `shell_command` for local inspection, discovery, validation,
  and non-content path operations, while `apply_patch` remains the dedicated
  action for file-content mutations.
- Runtime policy and auto-reasoning key/value command rows now render as terse
  human-readable statements in normal logs instead of raw field lists.
- Command and agent slash-completion suffixes now use a contrast-managed
  shadow foreground instead of matching the editable prompt text.
- Agent patch diffs now update a pane-local modified-file summary with added
  and removed line counts, and `/list-modified-files` renders that summary as
  a markdown object list.
- Agent thinking logs now wrap at the pane width or 120 columns, whichever is
  smaller, and continuation rows align after the `thinking:` label.
- Live agent footers now advertise `Esc to interrupt`, and both Escape and
  Ctrl+C share the same pane-local agent-shell contract: interrupt active work
  through the `/stop` path, or exit agent mode when idle.
- Agent prompt/footer overlay cleanup now treats terminal columns as grapheme
  cell coordinates, so wide glyphs such as `✅` in neighboring pane content do
  not confuse stale-footer clearing or leave duplicate/blank status rows.
- OpenAI-compatible cached-token accounting now sums all reported cached-token
  fields into the single `/status` `cached_input` value instead of selecting one
  field.
- Pending blocked agent approvals are now reconciled after approval-policy and
  related permission-policy changes. When the live policy now allows a pending
  blocked action, Mezzanine decides and resumes the action instead of leaving
  the turn stuck in `waiting_approval`.
- Agent communication guidance now explicitly asks models to keep progress
  updates to one or two short sentences by default, use concise bullets only
  when they improve scan value, and avoid repeated intent or long
  self-explanation while preserving validation and blocker evidence.
- Patch guidance is tracked as concise prompt/recovery requirements instead of
  a long pasted transcript. The active prompt already requires fresh context
  and a smaller fresh patch after hunk mismatch, and rejects delete/recreate as
  a substitute for editing.
- The model-facing file mutation surface has been reduced to `apply_patch` for
  file-content changes. Zero-byte creation, append-like additions, full
  replacement, content deletion, and moves with content changes are represented
  through Codex-style patches; non-content path work is handled through
  `shell_command` under the active approval policy.
- Model-correctable action failures now use action-type based recovery
  eligibility with explicit exclusions for policy/user/runtime infrastructure
  boundaries, and generated semantic action final diagnostics collapse
  wrapper-only output into concise guidance instead of exposing shell wrapper
  fragments.
- Common model-emitted colored/status glyphs now use grapheme-cluster width
  handling in terminal and agent transcript wrapping, preserving gutter
  wrapping and avoiding extra unstyled blank rows without relying on a fixed
  glyph whitelist.
- `/compact` now builds model-backed compaction requests from the complete
  provider-bound context, enters a pane-local compacting state before provider
  execution, and rejects overlapping compaction requests for the same pane.
- Normal model context now automatically injects only the active conversation's
  compact summary; generic and unrelated compact memory records stay out of
  default request context.
- `/dump-context` now works for idle agent panes by rendering a preview of the
  next provider-bound model request without starting a turn or mutating
  transcript state.
- Terminal `help` and agent `/help` command rows now render alphabetically.
- The standalone file creation action has been removed; `apply_patch` now owns
  exact file-content create/update/delete/move behavior.
- Mezzanine no longer enforces delete-before-write rejection in controller validation. The system prompt and failure-recovery guidance remain responsible for steering models away from delete-and-recreate edit workflows.
- Agent presentation logs now keep a cleartext append tail and compact large tails into concatenated `presentation.tsv.zst` zstd frames that are read back with the active cleartext tail as one ordered stream.
- `/fork` now clones the current conversation into a new same-window agent pane, preserves the source pane's conversation binding, and seeds the forked pane prompt with the last submitted prompt before `/fork`.
- Reverse search now supports Enter/Right-arrow match acceptance without submission and Escape/Ctrl+C/Left/Up/Down cancellation back to the original draft.

- The default window right-status template now places `pane.pwd` at the far left and includes literal template spaces between adjacent status and command-button pills.
- Copy-mode mouse selection now clamps impossible selection coordinates before applying ranges, preventing out-of-range mouse events from wedging pane input.
- Path-like arguments in `:` command prompts and `/` agent command prompts are now tab-completable from the filesystem.
- `:` command prompts and `/` agent command prompts now use interactive `(reverse-i-search'<substring>'): <item>` history search, with repeated `Ctrl+R` moving backward and Tab/Shift-Tab moving forward while search is active.
- Directional pane navigation from a full-height pane no longer vertically wraps into a neighboring over/under split pair that has no horizontal overlap with the active pane.
- Agent pane output now persists a separate presentation log with exact rendered terminal bytes, display lines, style names, copy text, pane id, turn id, and original terminal width; `/resume <session>` replays that log exactly when present and falls back to synthesized transcript summaries for older sessions.
- Agent session metadata now persists the best-known working directory and project root, and session transcript starts include a directory system entry for `/list-sessions` and `/resume` flows.
- `/resume <session>` now restores the saved working directory when it still exists, emits a warning when it does not, and advances the current pane view before replaying transcript context.
- Config now supports `agents.custom_system_prompt` as provider system context and user-defined `[personalities.<id>]` profiles with `/personality` selection and tab completion. Mezzanine still defines no built-in personalities.
- The default new-window button now uses `□`.
- The agent context usage pill now sits between the approval policy pill and the activity status pill.
- Agent prompt guidance and MAAP validation now steer models away from delete-then-recreate edit workflows.
- `apply_patch` shell/transport failures are covered by bounded action-failure
  feedback so the model can self-correct instead of failing through
  immediately.
- Host bracketed-paste routing now buffers incomplete paste frames until their closing delimiter arrives, preventing large heredoc pastes from forwarding partial payloads ahead of subsequent typed input.
- Right-click paste now targets the active primary command prompt or pane-local agent prompt in the clicked pane before falling back to pane PTY input.
- `/resume` transcript replay now renders human-readable user/agent log lines, decodes clearly base64 text payloads, and extracts `say` tool text instead of exposing raw transcript metadata.
- Live generated model/reasoning profiles referenced by pane overrides now survive approval-policy config reloads, keeping agent status model and reasoning pills visible after approval selection.
- Action-failure recovery budgets are now tracked per failed action signature instead of per batch, so one failed action cannot consume correction attempts for unrelated failed actions.
- Agent status dropdown scrolling now moves only the dropdown contents, clamps at list edges, and keeps the active row visible without closing the selector.
- Approval-policy selector coverage now verifies the model, reasoning, auto-reasoning, and approval pills remain visible after selecting `full-access`.
- Agent-mode enter/exit behavior now has regression coverage for preserving retained pane logs while advancing the live view to a clean visual slate on reentry.
- `/compact` behavior is now specified as explicit transcript compaction when compactable durable transcript entries exist, with no-op diagnostics only for empty or unavailable compactable context.
- `/list-sessions` now renders conversation UUIDs as markdown command links whose destination is `/resume <uuid>`, while also showing the literal resume command for copyability.
- `/help` now renders human-readable aligned command rows with descriptions and omits internal effect-type names.
- `/resume <session>` now replays saved transcript context with conversation id, entry count, sequence, and turn metadata in the pane buffer, while still reloading the saved prompt history and rebinding model context.
- Shell command failures were verified to remain model-visible command evidence without consuming the semantic action failure-recovery budget.
- Semantic file operation failures now receive broader recovery guidance so the model can distinguish patch, path mutation, and file-inspection correction paths.
- `agents.action_failure_retry_limit` now configures the model-correctable action failure retry budget and defaults to `5`.
- `apply_patch` has a clarified contract as the only file-content mutation
  action, while `shell_command` handles non-content mkdir/move/delete
  operations.
- MAAP validation repair now gets a bounded additional self-correction opportunity before failing through, and existing repair tests verify that repair prompts stay ephemeral rather than entering durable transcript context.
- The approval pill in the agent status area now displays only `ask`, `auto-allow`, or `full-access`, without the deprecated preset prefix.
- The default window status template no longer includes the auto-reasoning button; pane-local agent status controls own auto-reasoning toggling.
- Large paste regressions now cover split bracketed agent-prompt paste beyond the visible pane area and deferred foreground paste across bounded attached-client reads.
- Recent non-web action results from durable transcripts now replay into model context, preserving fresh facts such as failed file reads while still omitting stale web fetch/search payloads.
- Mouse-wheel scrollback now exits on the next key routed through copy mode, while explicit keyboard copy mode remains modal until cancelled.
- `/list-sessions` now renders saved sessions as a nested UUID-keyed list with indented `Last Active`, `Directory`, and width-fitted `Prompt` fields.
- The terminal width model now treats `✅` as a double-width emoji presentation glyph, with regression coverage for wrapping so it does not create phantom copy-mode rows or gutter offsets.
- `apply_patch` write phases now emit unified diff output for added, updated, and deleted paths so normal-mode action logs can render the same readable diff preview as other content mutations.
- The system prompt now asks models to avoid decorative or skeuomorphic Unicode glyphs that render as colored/stylized symbols unless requested or already present.
- The system prompt now explicitly tells models not to delete and recreate files as a substitute for editing them.
- Project-guidance prompt text now emphasizes using current project-guidance blocks after compaction or continuation and inspecting `AGENTS.md` before repo edits when guidance is missing.
- The hard system-prompt byte-size assertion was removed; the spec now says required prompt guidance must not be destructively trimmed to satisfy a built-in fixed cap while still requiring terse communication.
- Nonzero `shell_command` exits now remain model-visible action results and queue a provider continuation without consuming semantic-action failure-recovery budget.
- Cursor blink is now disabled by default while remaining configurable through `terminal.cursor_blink`.
- Window status field pills now render their own styled left/right buffer cells, and the default right-status template no longer relies on literal template spaces for pill padding.
- Large foreground pane pastes now use higher default attached-client loop input limits so expected echoing paste flows can reach the pane without the small harness read ceiling truncating them.
- Agent shell command tail log displays now decode Mezzanine's base64 output transport before choosing the last non-prompt output line.
- Compaction now has its own active running substate, `Compacting`.
- Clicking the `auto:on` or `auto:off` pill in the agent pane status toggles pane-local auto-reasoning.
- The model and reasoning selection dropdown can now show up to 30 rows or three quarters of the pane height, with active-selection scrolling.
- The approval mode status pill now sits left of the activity status pill and opens a dropdown for `ask`, `auto-allow`, and `full-access`.
- The CWD status pill now lives in the window status area and reports the active pane working directory.
- `/resume <session>` now replays saved transcript context into the pane so resumed sessions have visible conversation context.
- `/list-sessions` now shows saved sessions as nested UUID records with `Last Active`, `Directory`, and `Prompt`, with directory derived from saved project-root/CWD context and prompt derived from the first user turn.
- Path-like terminal command targets, including `source-file`, split/new-window start directories, and text-output paths, now expand a leading `~`.
- The bottom right window status area now has configurable default padding around command buttons and adjacent status pills.
- Window status command buttons are now generalized as `#{button:<icon>|terminal|<command>}` and `#{button:<icon>|agent|<command>}` fields, with default controls converted to this convention.
- The default new pane, new window, and new group window-status buttons now use `+`, `⊞`, and `⊕`.
- The default window status now includes a `λ` terminal-command button for `agent-shell`; auto-reasoning is handled by the pane-local agent status pill.
- Pane agent right status now includes approval policy and auto-reasoning pills with themed status styling.
- Agent-shell exit paths now stop active pane-local agent work before hiding and consume pane input with a warning while stop completion is pending.
- Agent-shell show paths now share the clear-and-advance behavior used by terminal `agent-shell` entry.
- Subagent spawn actions now use the same normal-mode action-line renderer as standard actions, with role/placement/mode/task summary as action arguments and action rationale still surfaced as `thinking:` output.
- The agent status indicator/timer in the agent prompt bar now keeps its existing foreground behavior while inheriting the prompt bar background.
- Long foreground paste payloads now stay as ordered logical pane-input side effects for the async pane worker's bounded PTY transport, and foreground pane input clears stale copy-mode scroll state so the pane returns to the live bottom.
- Write-file and edit-file action results now use the same normal-mode diff preview gate as apply-patch results.
- The live agent status now renders in the reserved agent prompt input row while the prompt is empty. It is hidden while the user is composing prompt text.
- Running agent turns now expose more granular display substates:
  - `thinking` while waiting for provider/API work;
  - `executing` while an agent shell action is running;
  - `waiting` while a pending shell action is waiting for shell readiness.
- Stopped/interrupted agent statuses now use muted idle styling instead of failed/error styling.
