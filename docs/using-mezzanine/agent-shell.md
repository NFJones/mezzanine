# Agent shell

## Purpose

Use the pane-local agent prompt, its common controls, and its safe operating
boundaries.

## Prerequisites

Complete [Getting started](../getting-started/README.md) and configure a
provider for model-backed work. Authenticate if that provider requires
credentials; compatible local backends may need none.

## Open and use the prompt

With the default bindings, press `Ctrl+A a` to show or hide the agent shell for
the focused pane. Press and release `Ctrl+A` before pressing `a`. The prompt
appears at the bottom of that pane; it does not replace or merge history with
the pane's process screen. While visible, ordinary pane input goes to the agent
shell. Multiplexer bindings, pane navigation, resizing, and copy mode remain
available. Use `Ctrl+A ?` to inspect effective bindings if yours differ.

The agent works from the pane working directory, its conversation state,
configured instructions, and explicit action results. It does not passively
receive your full terminal screen, scrollback, or other panes. Include relevant
output in your request or ask the agent to inspect it explicitly.

Type a request and press Enter. Use `Ctrl+J` for a newline without submitting.
In native shell mode, `Ctrl+V` pastes host clipboard text into the prompt,
preserving multiline text. Completion supports slash commands, `$` skills,
`#` macros, and `@` MCP server names where enabled; Tab cycles candidates.
Use Up/Down to recall prompts and `Ctrl+R` to search them. Enter in reverse
search accepts the match without submitting it.

Large bracketed pastes appear as compact `[Pasted …]` blocks, but submission
sends the complete underlying text. History recall preserves these blocks.
Exceptionally large complete prompts can be submitted without being retained
for recall. An oversized or timed-out incomplete paste is discarded, along
with subsequent bytes until its closing delimiter arrives. Follow the status
bar notice; `Esc` at an idle prompt resumes ordinary input after a discarded
paste.

Press `Ctrl+A e` to edit the draft externally. Closing a successful editor
returns text to the prompt without submitting it. The editor runs on the
server machine in its own PTY, not through the pane shell, in both `native`
and `pane` modes. It does not alter the pane's shell history or command draft.

## Stop, hide, or start fresh

| Control | Result |
| --- | --- |
| `Esc` at an idle prompt | Clear the draft without hiding the prompt. |
| `Ctrl+D` on an empty prompt | Hide the agent shell. |
| `Ctrl+C` twice within three seconds while idle | Hide the agent shell. |
| `Esc` or `Ctrl+C` while a task runs, or `/stop` | Request interruption of the task. |
| `/new` | Start a separate conversation. |
| `/resume` | Open the saved-conversation picker. |

Hiding the shell asks an in-progress task to stop and blocks ordinary pane
input until the task reaches a terminal state. Non-slash text submitted while
a task runs steers that task rather than starting another turn. Prose is not
an approval decision.

Internally, ordinary running-turn steering now has distinct occurrence receipts
separate from its immediate canonical insertion. Local ordinary request admission
acknowledges only the exact included user events; preparation, auxiliary requests
and rejected claims do not. This is not proof that a remote model read the input.
Manual-compaction submissions retain separate occurrence IDs through history
preparation and bind together to the existing ordered aggregate prompt event.
History failure retains not-sent evidence without retrying input. Queued or
claimed history cancellation settles actor-owned receipts without waiting for
the worker; late callbacks cannot recreate discarded receipt evidence.
Pre-history compaction queue teardown retains exact not-sent occurrences without
replaying input. Session metadata version 5 retains bounded display-only occurrence
evidence and acceptance order for active pane bindings. Version 4 migration keeps
its stored array order, not a reconstructed original cross-owner order.
Restart converts pending evidence to admission
unknown rather than falsely claiming not-sent or retrying input; admitted and
not-sent evidence remain unchanged. Failed checkpoints retain live receipts but
can leave restart evidence incomplete. Older metadata invents no receipts.
Manual resume uses the same inert recovery policy and restores prior receipt
evidence if its transaction fails. Deferred transitions publish their newest
evidence through the persistence drain; failed direct writes remain eligible for
a later drain and adapter writes use bounded checkpoint retry. This does not
archive every closed conversation's receipts. Pending-tail labels remain
receipt-driven: steering uses `user> [pending]` at the live log tail while later
output arrives. Local admission replaces that occurrence with one normal user
entry; terminal not-sent and restart admission-unknown evidence keep explicit
labels. This is log-tail placement, not forced viewport pinning while browsing.
Visible rows are bounded using composer log geometry. When the gutter or log
rows cannot fit, the preview is omitted while accepted receipt source remains.
Pending text is not saved
as ordinary acknowledged prompt history; settled rows carry occurrence identity
for replay and bounded persistence-only retry. Display installation failures
retain a separate pane/conversation obligation:
ordinary idle drains retry with capped backoff and fair bounded work, even after
repeated failures and with a current checkpoint. No new input or explicit retry
request is needed. A bounded source-free diagnostic reports unavailable display;
successful prefixes are not repeated and replaced owners retire old display work.
Recovered rows, including a successful prefix before a later installation failure,
request repaint in the same drain. Idle enrollment is separately admitted: a full
side-effect queue retains display ownership until real capacity progress can admit
the one tracked wake, without turning an otherwise valid persistence batch into a
display-timer failure.
This display recovery does not retry input, provider work or settled effects, and
does not replace independent durable-write recovery. Source-copy selection retains exact
display text, including authored CRLF and trailing newlines. Selecting an overflow
summary or tail row recovers the complete pending source group once; rendered
copy still selects visible cells. Captured copy views stay frozen during output
and promotion. Automated Linux tests cover promotion, recovery, transient output,
source copying and persistence retry. Physical-terminal and macOS workflows and
the exhaustive observer/theme/selector interaction matrix have not been qualified.
After steering is canonically accepted, a status-display or trace failure does
not reject it or require resubmission.
Deferred transfer diagnostics cannot strand accepted guidance before scheduling.
An unrecoverable partial transfer settles its receipts without replay; terminal
receipt evidence survives later trace or interrupted-transcript write failures.
The internal receipt view contains only current-conversation display evidence in
acceptance order; reading it cannot acknowledge input or change model chronology.
Ordinary receipt ownership also checks the existing pane process incarnation;
replacement or unreadable root identity cannot acknowledge earlier guidance.
Deferred compaction receipts preserve that original binding through history
preparation; a replacement settles the occurrence as not sent, without replay.
Guidance to an already retained blocked task stays on that task; it does not
resume a human pause, grant approval, wake a mail-only wait or queue a second turn.

After interruption, the next non-slash prompt continues from retained user,
assistant, tool, and steering context. It adds guidance without restarting
cancelled actions or processes. Use `/new` when you want an independent task
instead. Detaching the client is different from hiding the agent shell: a
normal detach leaves tasks running.

Interruption is a request, not rollback or proof that every worker has settled.
Native shell workers receive an exact-owned cancellation fence and terminate
their child process group; they do not send Ctrl+C to the pane shell. Provider
requests are aborted when their monitor exits. MCP/network transport retirement
cannot undo a remote effect. Issued actions with unconfirmed outcomes are retained
as interrupted with unknown effects, and are not automatically retried. Inspect
current state before deciding whether a new mutation is needed.

The runtime's targeted pause/resume APIs are distinct from `/stop`: pause retains
the task, gates new dispatch, and drains already-issued work. Pausing is not proof
of quiescence; Paused releases provider capacity but retains exclusive task
ownership. Explicit primary resume supplies one continuation prompt and does not
repeat settled actions. It supersedes unissued candidates rather than carrying
their old approvals into changed work. Peer mail, approval replies and child
completion cannot unpause the agent; already-running children may continue.
Use `/list-agents` in the existing agent prompt for these targeted controls, or
`:list-agents` from the terminal command prompt:
`p` pauses/resumes the selected native agent, `i` interrupts, and `d` arms close
confirmation. Enter focuses only after explicit
activation. Runtime pause is not transparent process suspension across restart.

The shared human browser uses CurrentProject for `/list-agents`, and All for
`/list-agents --all`. Slash accepts no arguments or exactly one literal `--all`;
duplicates, quoted flags, unknown flags and positional arguments show usage in
the pane. Command completion and `/help` expose the command and its option.
`:list-agents` remains all-session and argument-free. CurrentProject compares
exact canonical **current trusted roots**, not display labels, saved/MMP project
membership or accounting origin. It retains the authenticated invoking pane's
incarnation/current owner through refreshes instead of following later focus.
Unavailable or withheld origin evidence produces an empty scoped view with a
nonsensitive reason and an All/`--all` hint, never a silent global fallback.
Search, copy and save see only retained scoped records. Symlink aliases match,
nested trusted roots differ, broad trusted ancestors may group repositories,
and separate worktrees or moved children may exclude MMP peers. This is view
filtering, not a permission or audience grant, and does not change model-facing
discovery or native control rights. It shows current live registrations, not
saved `/resume` conversations, and has no model-facing discovery row cap.

Opening reuses the full interactive browser, including columns and metadata,
stable-ID keyboard/mouse focus, native controls, search, copy, save, resize and
refresh. Search/save typing wins over admin keys. Refresh retains the original
query and valid selection; a removed selected row retires its old action and
close confirmation, never transfers them to a neighbor. External rows are
focus-only. Opening is provider-free and creates no turn or conversation, and
does not interrupt, resume or focus work: it is available in visible idle,
running, blocked, waiting, paused and compacting prompts. Control/textual calls
return display output without leaving a typed UI capture for a later request.
Display output includes nonsensitive diagnostic chrome, including the unavailable
project reason and literal `--all` hint. Raw copy/save exports intentionally omit
transient error/notice chrome; the retained records and scope are the same.

In the `/resume` picker, Enter resumes the selected conversation, `i` opens its
transcript for inspection, and `a` toggles between the current project and all
saved conversations. Press `d` to delete the selected saved conversation;
deletion is refused while any live durable agent pane still has it open. Switch
those panes to `/new` or another conversation first. Hiding the prompt alone
does not make the conversation eligible for deletion.

## Inspect and control a conversation

Use `/help` for available commands, `/status` for the pane's active model,
policy, context, and token state, and `/approval` for approval controls.
Primary agents receive a conversation-owned generated identity under
`agents.name_mode`, shared with child naming. Hidden conversations retain their
names; resume and restart preserve them even when the current policy changes.
New and forked conversations allocate a new identity. This is separate from
the generated conversation title and canonical agent ID. The pane frame projects
the identity only while agent mode is visible; explicit pane/window titles remain
authoritative, and hiding reveals the underlying shell/program title again.
At comfortable composer geometry, unfocused/read-only panes show their resolved
conversation title instead of `agent draft`; focused panes retain editing labels.
Manual names and the existing session-title policy determine this inert label.
Long titles are ellipsized to available cells without hiding live status. Title
rendering uses retained metadata, not catalog reads or provider requests.
Active composer status is separated from its trailing horizontal rule by one
space when room remains, without changing draft text or the allocated row width.
`/status [--extended] [--project | --all-projects]` accepts either flag order.
Without scope flags it retains overall totals.
The live status view also includes a pane-scoped **External Telemetry** section:
harness, last externally reported state, server-observed telemetry health, and
usage coverage. `enrolled`, `awaiting-first-proof`, `connection-lost`, and legacy `lease-active` describe
different evidence, not vendor completion or process death. Unavailable usage
is not zero or invoice proof. A curated source awaiting its first native proof
has not yet demonstrated freshness, rather than having lost a proven observer.
With no current telemetry, the view says so without
inferring installation or vendor activation. This read-only section works with
or without a native conversation, does not renew leases or query durable history,
and excludes other panes' telemetry; accounting tables keep their existing scope.
Every token table includes Harness, Provider and Model; native calls use `mez`.
Same-model external rows remain separate, with omitted reasoning/cache counters
shown as unknown. External-only panes can report and reset the pane view without
creating a native conversation; they have no native latest-request sample.
`--project` takes no path and selects the invoking pane's eligible accounting project; missing evidence is
reported as unavailable rather than silently showing overall usage.
`--all-projects` includes zero-use registered projects, trust/version labels,
historical project IDs and unattributed expense. Scoped reports are STATIC
snapshots; diagnostics still describe the pane, not the selected accounting scope.
Only `--extended` reads durable history, on a worker using one frozen UTC instant
and read snapshot. Rolling sections use age-limited 1/7/30/60/90-day windows.
Changing project, replacing the conversation or detaching the caller while work
is pending prevents stale interactive delivery. `/reset-status` clears only the
pane view, not session partitions or durable history.
Generated session-title requests contribute reported token usage even when their
reply is rejected as a title. Their usage is auxiliary: it does not replace the
latest ordinary request's context/cache sample. Late issued responses settle
once against the original conversation; they cannot rename a replacement or
charge its pane. Storage failures do not rerun paid title requests.
`/model` shows the active and configured profiles; `/model <profile-name>`
selects one. `/model --list` lists the active provider's model catalog.

Enter `/plan on` before a read-only investigation. Plan-only mode applies to
subsequent turns until `/plan off` or `/plan toggle` disables it. While enabled,
the pane has no write sandbox scopes; `/plan status` reports the mode. Enabling
it during active work requests that work stop. Plan mode does not replace
approval policy or operating-system confinement. With `policy-only` or sandbox
bypass, removing write scopes does not make shell processes OS-confined or
guarantee read-only execution.

Use `/objective <text>` to set a durable, peer-visible objective for the
conversation. This overrides automatic objectives until `/objective --clear`.
Bare `/objective` reports the value and whether its source is `user`,
`automatic`, or `none`. Objectives are unavailable for ephemeral loop-worker
conversations; do not put secrets in peer-visible text.

## Review actions and context

Created and Updated dates in record browsers use RFC3339 UTC (a trailing `Z`),
including detail and saved Markdown views. Stored timestamps and chronological
ordering remain numeric and unchanged. Missing approval dates show `unknown`;
dates outside the supported four-digit year range show `unavailable`.

The agent may request file reads, bounded commands, patches, configured MCP
calls, or scoped subagent work. Shell, network, destructive, configuration,
and some MCP actions can require approval. Approval policy does not itself
confine a permitted process; sandboxing is a separate boundary.

Streaming text and action previews are provisional. An accepted command or
action header shows intent, not execution or success. Read the settled result
before relying on an action. A confirmed file-mutation section proves that
section's effect, not whole-action success; a later failure does not imply
rollback.

Use `/show-context activity` to inspect recent retained action intent, outcomes,
and bounded result previews. Enter or click a row to open detail; Escape
returns to the list. `/show-context activity <sequence>` opens the activity
containing that presentation sequence. Press `y` to export versioned JSON
with the retained activity sources. Treat exports as potentially sensitive:
they can include commands, paths, and output. A queued clipboard copy does
not prove desktop delivery; adapter failures remain visible.

This inspection view does not alter conversation logs, approvals, or logging
levels. It reads at most the latest 200 cleartext presentation records within
8 MiB, so it is not a complete raw-output archive. Legacy records and missing
identities are not reconstructed.

Put repository-specific instructions in `AGENTS.md`. Project configuration
overlays under `.mezzanine/config.toml`, `.mezzanine/config.yaml`,
`.mezzanine/config.yml`, or `.mezzanine/config.json` remain pending until
explicitly trusted. Inspect trust with `mez sandbox trust list` before trusting
an unfamiliar root.

## Read and copy agent output

Structured agent output wraps at the smaller of the pane width and
`terminal.agent_wrap_column_cap` (120 display cells by default). Copy mode
recovers logical text rather than inserting presentation-only wrap boundaries.
Older ANSI-only saved records are replayed unchanged and may wrap differently.

### Optional log occurrence timestamps

`terminal.agent_log_timestamps` defaults to `false`. Enabling it shows default
**UTC HH:MM:SS** (eight cells and one separator) left of the two-cell log gutter,
once at the original first row of each structured user/parent/peer message,
thinking/rationale/say component, action header/preview/result, status/error/macro
or structured shell-output entry. Authored newlines, tables and wrapped rows
reserve the same blank nine-cell margin; scrolling to a continuation never
repeats the timestamp. Untagged interactive PTY chunks and opaque legacy ANSI
records do not acquire guessed entry times. Unknown or unrepresentable times
are blank. Original legacy record times, when safely available, are coarse
seconds-precision persistence evidence rather than observed occurrence time.

These captured server wall-clock instants are not elapsed time, executor start
or remote generation/delivery proof. Clock-only text omits dates, so midnight,
equal instants and backward clock adjustments can repeat. UTC does not jump for
local daylight-saving changes; sequence order, not clock text, orders entries.
`terminal.agent_log_timestamp_format` defaults to `%H:%M:%S`. Custom formats
accept printable ASCII literals and only `%H`, `%M`, `%S` (two zero-padded digits)
and `%%` (literal percent), 1–32 decoded cells and at most 64 source bytes.
`%H:%M` gives five cells; `[%S-%M-%H]` gives ten. This is not arbitrary
chrono/strftime formatting: empty/malformed, non-ASCII/control, date, fraction,
locale, flag, width and unknown directives reject before mutation. Selecting
a format never enables timestamps; literal-only formats still blank completely
for unavailable/cropped instants. Clock-only formats do not restrict the year.
There is no timezone setting. Rendering never reads the current
clock or timezone files, and there is no periodic timestamp repaint.

The configured cap includes timestamp margin and gutter; the optional margin
is suppressed below `format width + 13` capped pane cells (21 by default).
Continuations reserve that format width plus one blank separator; the editable composer and PTY size
do not change. Timestamps use dim foreground-only passive framing style;
category gutters and authored body emphasis remain independent. Visual client
frames/screenshots retain the margin. Content copy, source copy, search, capture,
history export and model observations omit it while preserving their existing
content format, including literal timestamp-looking authored text. Selecting
only the margin does not copy an unrelated whole source entry.
An active copy view retains its frozen rows, styles, selection and matching
column map through a display-setting change, including equal-width formats.
Live formats reproject original instants without source rewriting or effect replay.
Exit/reenter copy mode for the new
appearance; content copying remains decoration-free in either snapshot.

Later action logs and assistant text wait behind earlier presentation barriers.
When a barrier releases, eligible components drain in response order. Accepted
sources remain owned until installation succeeds; a bounded presentation-only
retry does not repeat the underlying action. Already installed components remain
visible once, while failed or interrupted work does not promote unverified
success text.

The composer remains visible while the log scrolls or is in copy mode. In
explicit log-copy mode, the selection and keyboard input belong to the log;
Enter does not submit the visible draft. Drag across entered draft text or
double-click a word to copy it independently. For keyboard draft selection,
use `copy-mode --draft` from the Mezzanine command prompt. Use
`copy-selection --draft --format source` to include complete source from
intersected collapsed paste blocks. See [Terminal input, copy, and
history](terminal-input-copy-and-history.md) for selection controls and buffers.

Received peer messages keep authored source separate from display wrapping.
Source-format copying of a wrapped plaintext continuation recovers its original
source line, including literal marker-looking text; a Markdown peer selection
recovers the complete original entry. CRLF and trailing blank bytes are retained,
including when timestamps have never been enabled. The newest source anchor is
retained at each identity-bearing received group's tail so head pruning does not
turn a remaining continuation into display text. Reflow and presentation retries
do not resend the message or change its receipt identity or occurrence time.

## Choose a shell mode

The default `native` mode validates the pane's local root process and runs
shell-backed agent actions in fresh compatible shells without typing into the
pane. It can work while a full-screen application occupies the pane, but
stateful or interactive actions are rejected rather than redirected to it.
State such as a command's `cd` or `export` does not carry into the next fresh
shell.

`pane` mode sends shell-backed work through the interactive pane shell. It
requires a supported Bash, Fish, Zsh, or POSIX `sh` shell ready at an empty
prompt. A full-screen program, password prompt, or uncertain shell boundary
makes injection unsafe. Runtime-created agent panes use bounded startup and
report a diagnostic on failure rather than remaining in bootstrap indefinitely.

Use `/shell-mode status` to inspect the effective mode. Select
`/shell-mode native` or `/shell-mode pane` for a pane-local override. Append
`--global` to persist the default for panes without an override. A global change
does not clear an existing pane-local override, including one in the issuing
pane; check `/shell-mode status` afterward. Pane-local overrides are not durable
across runtime restarts.

Native actions use a cleared base environment, not the daemon's full
environment. Optional values come only from `permissions.env_whitelist` in
the immutable snapshot captured when Mez starts, plus documented runtime
requirements. Pane-root metadata selects the shell and working directory;
later pane exports and startup-file changes do not change forwarded values.
If a command needs an extra value, review the whitelist and restart the owning
daemon to refresh its snapshot. Never put credentials in an agent prompt.

## Work inside SSH and container shells

Select `/shell-mode pane` before working in an interactive SSH, container,
chroot, or other nested shell. Native mode uses the pane's local root process;
it does not run inside that nested environment.

Before explicitly opening the agent shell in an existing foreign environment,
ensure it is at an empty interactive prompt. Agent entry asserts that this is
safe. Mez runs a bounded identity probe and launches an ephemeral managed
child through a `/bin/sh` loader. It needs no Mezzanine executable or
preinstalled shim there and does not modify startup files or install software.
Generated agent commands wait for bootstrap validation. Failed or incomplete
bootstrap refuses typed agent commands.

Selecting pane mode and entering the agent shell opts into the correlated
bootstrap as environment and path authority. Correlation detects stale or
mismatched records; it is not cryptographic attestation against the active
pane environment, which can observe and replay PTY traffic. Decide whether
that environment is appropriate for agent work before entering.

Do not enter from a password prompt, full-screen program, or unknown command
line. Return to an empty prompt or exit the nested environment. Exiting clears
its shell authority and re-arms local discovery while agent mode is visible.

## Related pages

- [Agent and integrations](../agent/README.md)
- [Safety, trust, and security](../safety-and-trust/README.md)
- [Configuration](../configuration/README.md)
- [Manual reference](../reference-manual/README.md)

## Next step

Use [Workflows](workflows.md) for bounded investigation, implementation, and
recovery patterns.
