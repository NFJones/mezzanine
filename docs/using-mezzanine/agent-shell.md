# Agent shell

## Purpose

Use the pane-local agent prompt, its common controls, and its safe operating
boundaries.

## Prerequisites

Complete [Getting started](../getting-started/README.md) and authenticate a
provider for model-backed work.

## Open and use the prompt

Press `Ctrl+A a` to show or hide the agent shell for the focused pane. The
agent prompt appears at the bottom of that pane; it does not replace the pane's
process screen. Mezzanine retains the process and agent surfaces separately, so
showing, hiding, or rebinding a conversation does not merge their history or
screen state. While the prompt is visible, ordinary input for that pane goes to
the agent shell, while multiplexer bindings, pane navigation, resizing, and
copy-mode controls remain available. Hiding the shell asks an in-progress task
to stop and blocks ordinary pane input until the task reaches a terminal state.
The agent works from the pane working directory, its conversation state,
configured instructions, and explicit action results; it does not passively
receive your full terminal screen, scrollback, or other panes.

Structured agent output wraps at the smaller of the pane width and
`terminal.agent_wrap_column_cap` (120 display cells by default). This includes
status, error, diagnostic, action, and result rows as well as transcript text.
Continuation rows repeat the `▐ ` gutter. Wrapped `agent: ` status/action and
`thinking: ` rationale/summary rows keep their first-row label and use five
spaces after the gutter on later rows. `user>`, `mez>`, `parent>`, and peer-message
continuations also use five spaces after the gutter regardless of label length;
Markdown structure adds its own indentation. Copy mode recovers the original logical row
instead of inserting presentation-only wrap boundaries. Retained raw ANSI
projections from very old saved presentation records are replayed unchanged
and are not rewrapped to this configured cap; they may wrap at the physical
pane width because rewriting terminal-control bytes is unsafe.

The transcript rail uses the status-theme foreground independently of speaker
or action category, without bold or ANSI dim. Assistant labels and rationale
remain normal-weight; user, command and error cues retain their accents and
words. Markdown emphasis remains authored structure, not rail styling. The
two-cell `▐ ` footprint is unchanged, as are copied source and continuation
offsets. This styling change does not rewrite legacy ANSI-only records.

Use `/show-context activity` to inspect recent identity-bearing action outcomes
and retained result previews, plus accepted command intent captured before
settlement and response-wide rationale. Rationale retains its response identity
whether it arrived through streaming or a complete response; it does not claim
an action or executor attempt. An `accepted` command is not evidence of execution or success.
Enter or click a row to open detail; Escape returns
to the list without altering conversation logs. `/show-context activity <sequence>`
opens the activity containing that presentation sequence. Details distinguish
accepted intent from observed outcomes and retain source beyond the bounded live
preview where available. The snapshot reads at most the latest 200 cleartext
presentation records within 8 MiB; legacy rows and unavailable attempt identities
are not guessed. This is a client-local inspection view, not automatic inline
folding, a log-level change, an approval route or a promise of all raw output.

Streaming rationale that exactly matches a validated completion can remain
visible without a second copy being appended. A matching action header may
remain while its accepted action is pending, together with matching progress
text; the action's actual result is still reported separately. A changed header
can be replaced atomically while matching rationale and progress text stay
visible. A visible preview is not yet permanent: field closure and whole-action
receipt do not validate the batch or complete its rich render. Only validated,
fully rendered components become finalized; result rows additionally require
their own settlement. Later visible logs wait for earlier visible components to
finalize; hidden or deduplicated components release their ordering slot after
validation without a pane projection, while replaceable command-output tails
and executor progress never hold that permanent-log barrier. Neither failure
nor a later pane write removes finalized logs. Provider progress is optional:
providers that return only a complete response, including
those that stream transport events without MAAP fragments, use the same
validated log presentation without synthetic streaming previews. Missing
progress does not suppress or duplicate the completed answer. Provisional
action previews do not prove execution; rejected source does not become an action
result. Matching command previews and multiple headers can remain visible
across acceptance. Accepted command previews and summaries stay separate from
their batch rationale: shell readiness, approval waits, execution failures, and
settling a live output tail do not erase or replace those intent rows. In a
multi-action batch, validation retains the already-rendered accepted prefix;
later actions still follow their normal ordering and execution gates. A final
answer following pending actions
stays provisional and is recorded only if those actions complete successfully.
Failed deferred URL fetches and web searches can enter bounded model correction
after their in-flight siblings settle. Their results become context rather than
automatic retries of the same URL or query; policy denials and exhausted
correction budgets remain terminal.

Type a request and press Enter. Use `Ctrl+J` to insert a literal newline
without submitting it. In native shell mode, `Ctrl+V` pastes host clipboard
text into the editable prompt while preserving multiline text. Prompt completion
supports slash commands, `$` skills, `#` macros, and `@` MCP server names where
enabled.
The editable prompt begins with `❱ ` (and `▐ ❱ ` when the agent gutter is
shown). This display-only input marker does not change assistant transcript
rows, which continue to use `mez> `.
In roomy panes (at least 64 columns and 14 body rows), a lightweight context row
and help row surround the input. `Ask Mez` indicates ordinary submission;
`Guide this task` indicates prose will steer the running turn. Status and elapsed
time remain visible while drafting. Search, slash commands, pending approvals
and discarded paste show their own context; prose does not approve a request.
Tab cycles completion and Enter still submits, while Enter in reverse search
accepts the match without submitting. Active-work Escape retains interruption
precedence. The editor hint uses effective bindings, and intercepted baseline
hints are omitted. Observers and unfocused panes show a read-only cue.
Small panes keep the compact editor. Decoration never becomes submitted or
copied conversation text; reservation remains stable as status/help changes.
The prompt remains in this in-pane entry area by default. Press `Ctrl+A e` (or
the active key preset's `edit_prompt` binding) to request external editing;
closing a successful editor returns the text to the same prompt and never
submits it automatically. Mez launches the editor on the server machine as a
direct subprocess with its own PTY. It does not run a command through the pane
shell, so opening an editor does not add to shell history or disturb a command
draft already present there. This behavior is the same in `pane` and `native`
agent shell modes.

Large bracketed pastes are shown as compact `[Pasted …]` blocks, but typed text
and smaller pastes remain visible literally. History recall and `Ctrl+R` restore
the same pasted blocks shown when the prompt was entered, and submission still
sends the agent the complete original text. The bounded history capacity can
retain a maximum-size bracketed paste with surrounding typed text; exceptionally
larger complete prompts are submitted normally but are not retained for recall.
A paste payload that exceeds the retained-byte limit, or whose closing delimiter
never arrives in time, is discarded instead of becoming prompt input. The bytes
after it are discarded too until the real closing delimiter arrives, so a
truncated paste cannot submit anything; the status bar reports the discarded
paste and `Esc` at an idle prompt resumes ordinary input.
Press `Esc` to clear a draft without hiding the prompt. `Ctrl+D` on an empty
prompt hides it. When no task is running, press `Ctrl+C` twice within three
seconds to hide the prompt; when a task is running, `Ctrl+C` requests an
immediate interruption. Non-slash text submitted while a task runs is steering
for that task rather than a separate turn. After an interruption, the next
non-slash prompt continues from the retained user, assistant, tool, and steering
context: Mezzanine appends the new text as guidance without restarting the
cancelled action or process. Use `/new` instead when the next prompt should
start a separate conversation.

Common controls are `/help`, `/status`, `/model`, `/approval`, `/new`,
`/resume`, and `/stop`. Use `/plan on` to enable pane-local plan-only mode;
it applies to subsequent turns until `/plan off` (or `/plan toggle`) disables
it. While enabled, the pane has no write sandbox scopes. Use `/plan status` to
inspect the current mode.

While a user-owned root agent shell remains visible, its pane frame displays
`mez` unless the pane has an explicit title. This remains true while a hide is
pending completion; after the agent surface actually hides, the ordinary
automatic or program-derived title resumes. The override is presentation-only
and does not change stored title state. Explicit names (including an explicit
`mez`), spawned subagent names, and ephemeral worker titles are preserved.

Use `/objective <text>` to set a durable, peer-visible objective for the
current conversation. It takes precedence over prompt- and model-derived
objectives until `/objective --clear` restores automatic publication. Bare
`/objective` reports the value and whether its source is `user`, `automatic`,
or `none`. Objectives are unavailable for ephemeral loop-worker conversations.

## Choose a shell mode

The default `native` mode validates the pane root process and runs each local
agent action in a fresh compatible shell without writing bootstrap input into
the pane. `pane` mode instead sends shell-backed work through the interactive
pane shell. Use `/shell-mode status` to inspect the effective mode,
`/shell-mode native` or `/shell-mode pane` for a pane override, and append
`--global` to persist the default for panes without an override.

Native patch execution is currently shell-backed. The process-free filesystem
replacement is staged behind typed contracts and launch-accounting work; do not
interpret native mode today as a zero-child guarantee. Its target transport is
`native_runtime`, while actual shell commands retain `spawned_shell`. There is
no second native mode or automatic fallback. See the
[migration contract](../../SPEC.md#process-free-semantic-adapter-contract-and-migration).

Pane mode requires a supported Bash, Fish, Zsh, or POSIX `sh` prompt to be
ready for input. A full-screen program, password prompt, or uncertain shell
boundary makes injection unsafe; return it to an empty prompt. Runtime-created
agent panes use bounded startup and fail with a copyable diagnostic instead of
remaining indefinitely in bootstrap.

Native launches compose their own environment instead of inheriting the
Mezzanine daemon process environment wholesale. For ordinary actions, only
names in `permissions.env_whitelist` are selected from an immutable snapshot
captured when Mez starts; selected values such as `PATH` reach native,
Bubblewrap, and Seatbelt actions unchanged. Pane-root metadata still selects
the native shell and working directory, but exports or startup-file changes
made later in a pane do not alter forwarded values. The runtime adds only its
documented requirements and drops all unselected server values, including
harness transport credentials.

## Work inside SSH and container shells in pane mode

This workflow applies to `pane` mode. `native` mode runs actions in fresh
shells derived from the pane's local root process; it does not inject them into
an interactive SSH, container, chroot, or other nested shell. Select
`/shell-mode pane` before using the foreign-shell workflow below.

When a pane-mode shell enters SSH, a container shell, a chroot, or another
nested interactive environment, Mezzanine treats that environment as a
separate shell authority. Explicit agent entry asserts that the foreign shell
is at an empty, interactive prompt. Mezzanine immediately issues a bounded
syntax-neutral identity probe and, after resolving the shell, launches an
ephemeral managed child through a one-command `/bin/sh` loader. No Mezzanine
executable, startup-file modification, or preinstalled compatibility shim is
required inside the nested environment, and host-side Bash, Fish, or Zsh
tokens and startup files are never reused across this boundary.

This explicit empty-prompt assertion applies only to an existing user-owned
foreign environment. Runtime-created agent panes use their mode-specific
startup contract and never enter this foreign-shell discovery path.

Agent work waits for that dependency-free bootstrap to validate the foreign
shell before generated input is released. Mezzanine never silently edits remote
startup files and never installs software in the foreign environment.

The dependency-free handoff uses correlation rather than cryptographic
attestation. On a host-observable local foreign shell, the runtime records the
foreground process group when it writes the loader command and releases the
bootstrap payload only after the pane worker observes a different foreground
group. A verified non-shell foreground leader, such as an interactive SSH
client, is an opaque transport: its remote descendants do not expose a
host-visible process group, so a matching fresh loader record supplies launch
correlation without waiting for an impossible transition. Unreadable or stale
leader identity retains the stronger transition rule. The loader payload and
fresh child token are typed into the same PTY, so a process that controls that
PTY can observe and replay them. The process-group, interaction-generation,
marker, and managed-child admission checks protect against stale, mismatched,
and accidental records; they do not establish an unforgeable boundary against
the active pane environment itself.

Selecting `pane` shell mode and explicitly entering the agent shell opts into
using a successfully correlated pane bootstrap as environment and path
authority. After the loader and child admission checks succeed, bootstrap
completes with a parsed environment signature, and the required foreground
observations agree, Mezzanine publishes that signature and its derived path
authority and marks the pane ready for typed agent shell commands. This policy
applies to the original local pane shell and to dependency-free SSH, container,
chroot, and other nested interactive environments. The user remains responsible
for deciding whether the active pane environment is appropriate for agent work.

Mezzanine does not promote a pane shell from version text alone. Dependency-free
identity discovery selects an absolute launch target and shell dialect for the
current interaction generation, and successful bootstrap binds that identity to
the correlated child lifecycle. Failed, truncated, stale, mismatched, or
incomplete bootstrap evidence remains degraded and typed agent shell commands
are refused before input is generated.

Managed Fish bootstrap completion waits for the matching post-source child
prompt before foreground certification. The wrapper's end record can precede
receiver cleanup jobs; their temporary process groups are not the persistent
shell. The prompt is only a scheduling fence: existing process, interaction,
foreground, and environment checks still decide authority. A missing prompt
retains the bootstrap deadline and fails closed rather than waiting forever.

An unmanaged nested shell that is not at an empty, interactive prompt cannot be
probed safely from the local `ssh` or container-client process alone; Mezzanine
will not inject input into a password prompt, full-screen program, or unknown
command line. Exit the nested environment to restore normal discovery of the
local pane shell.

For all supported shells, bootstrap remains bounded and fail-closed. Exiting a
nested environment clears its shell authority and re-arms discovery for the
original pane shell when agent mode is visible.

## Review actions and context

The agent may request file reads, bounded commands, patches, configured MCP
calls, or scoped subagent work. Shell, network, destructive, configuration,
and some MCP actions can require approval. Approval policy does not itself
confine a permitted process; sandboxing is a separate boundary.

Put repository-specific instructions in `AGENTS.md`. Project configuration
overlays under `.mezzanine/config.toml`, `.mezzanine/config.yaml`,
`.mezzanine/config.yml`, or `.mezzanine/config.json` remain pending until
explicitly trusted. Inspect trust with `mez sandbox trust list` before trusting
an unfamiliar root.

## Related pages

- [Agent and integrations](../agent/README.md)
- [Safety, trust, and security](../safety-and-trust/README.md)
- [Configuration](../configuration/README.md)
- [Manual reference](../reference-manual/README.md)

## Next step

Use [Workflows](workflows.md) for bounded investigation, implementation, and
recovery patterns.
