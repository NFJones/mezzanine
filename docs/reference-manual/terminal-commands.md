# Terminal commands

## Purpose

Provide a compact reference for commands entered through Mezzanine's
in-session command prompt. These commands control the multiplexer and are
separate from shell commands, agent slash commands, and the process CLI.

## Prerequisites

Start an interactive primary client and open the command prompt with
`Ctrl+A :`. The default prefix can be changed in configuration.

## Syntax and discovery

The command prompt parses Mezzanine commands; it never sends entered text to
the focused pane shell. Commands accept shell-like quoted and escaped
arguments. An unquoted semicolon separates multiple commands; execution stops
at the first command that fails.

Use `help` in the prompt for the baseline command catalog, brief descriptions,
and the effective key table. It is a catalog, not per-command argument help.
Tab and Shift+Tab move forward and backward through enumerable command and
argument completions. Applying a completion replaces only the active token and
does not submit the command. Run `list-keys` or press `Ctrl+A ?` to inspect the
active bindings and their configuration sources. Runtime- or store-backed
commands can still reject an invocation when required state or authority is
unavailable.

## Common command groups

| Task | Commands |
| --- | --- |
| Manage windows and panes | `new-window`, `split-window`, `select-pane`, `resize-pane`, `rebalance-window`, `synchronize-panes`, `rename-pane`, `list-windows`, and `list-panes` |
| Work with sessions and clients | `list-sessions`, `attach-session`, `detach-client`, `list-clients`, and `kill-session` |
| Copy and retain output | `copy-mode`, `copy-selection`, `paste-clipboard`, `paste-buffer`, `list-buffers`, `search-history`, `export-history`, and `clear-history` |
| Inspect and adjust the interface | `zen`, `pane-settings`, `show-pane-status`, `show-messages`, `show-iroh-status`, `list-keys`, `list-key-presets`, `set-key-preset`, `list-themes`, `set-theme`, `add-options`, `show-options`, `set-option`, `bind-key`, and `unbind-key` |
| Save or load layout state | `save-layout` and `load-layout` |

## Baseline command inventory

The baseline registry contains the following canonical commands. Live `help`
shows this catalog with brief descriptions and effective bindings; prompt
completion, command results, and approval prompts expose invocation-specific
arguments and runtime requirements.

- **Help and configuration:** `help`, `add-options`, `show-options`,
  `set-option`, `source-file`, `refresh-client`, `bind-key`, `unbind-key`,
  `list-keys`, `list-key-presets`, `set-key-preset`, `list-themes`, `set-theme`,
  and `zen`.
- **Groups and windows:** `new-group`, `rename-group`, `kill-group`,
  `select-group`, `next-group`, `previous-group`, `last-group`, `list-groups`,
  `choose-group`, `new-window`, `rename-window`, `kill-window`,
  `select-window`, `next-window`, `previous-window`, `last-window`,
  `list-windows`, `next-layout`, `select-layout`, and `rebalance-window`.
- **Panes and presentation:** `split-window`, `kill-pane`, `select-pane`,
  `resize-pane`, `next-pane`, `previous-pane`, `last-pane`, `rotate-pane`,
  `synchronize-panes`, `zoom-pane`, `swap-pane`, `break-pane`, `join-pane`,
  `display-panes`, `pane-settings`, `list-panes`, `rename-pane`, `capture-pane`,
  `pipe-pane`, and `mark-pane-ready`.
- **Sessions and clients:** `list-clients`, `detach-client`, `attach-session`,
  `list-sessions`, `rename-session`, `kill-session`, `save-layout`,
  `load-layout`, and `exit`.
- **Copy, buffers, and history:** `copy-mode`, `copy-selection`,
  `paste-clipboard`, `paste-buffer`, `create-buffer`, `list-buffers`,
  `choose-buffer`, `delete-buffer`, `save-buffer`, `clear-history`,
  `search-history`, and `export-history`.
- **Agent and diagnostics:** `agent-shell`, `show-messages`, `show-metrics`,
  `show-iroh-status`, and `show-pane-status`.

Some commands require an active runtime, control endpoint, configuration store,
or primary-client authority. Review completion hints, command output, and any
resulting prompt or approval rather than assuming a command affects a detached
or observer client.

## Selected command contracts

The following commands have behavior or safety boundaries that are useful to
know without opening the complete normative contract.

### Pane creation and pipe shell commands

`new-window`/`neww`, `new-group`/`newg`, and `split-window`/`splitw` accept a
command as `--shell-command STRING` or `--command STRING`. A spelling that has
a value takes precedence over words after `--`, and words after `--` take
precedence over positional words; `new-window` and `new-group` treat positional
words as the command only when `-n`/`--name` is present. The single explicit
string is user-authored shell source that the shell runs unchanged after
`exec`, so Mezzanine never re-quotes it. `pipe-pane` joins its positional words
with spaces and runs the result through the resolved shell, so those words are
shell source too. Mezzanine never offers a candidate for a shell-source token
unless the literal path neither begins with `-` nor contains any byte outside
ASCII letters, digits, and `_ - . / @ % + = : ,`, which no supported shell
interprets; static command and flag candidates are suppressed in that token.
Path candidates for other arguments are quoted with the command language and
read back as the exact literal path; words after `--` and `-n` positional words
are re-quoted losslessly before `exec`. Completion classifies the active token
against the whole command line, not only the text before the cursor, so
arguments the pane plan never consumes offer no candidates at all, and a token
whose role cannot be classified (for example a cursor inside a word or an open
quote) leaves the draft unchanged.

### Configuration discovery

`add-options` displays the schema-owned reference for supported live
configuration paths, including purpose, type, and constrained value or format
guidance. `show-options` remains the separate view of effective configured
values and their source layers.

### Pane status and providers

`pane-settings [-t pane]` opens a keyboard selector for the active or requested
pane's configured status entries, including values moved into menu overflow.
Read-only entries are labeled as such. Configured actions are limited to
`rename-pane`, `copy-mode`, and `copy-selection` terminal commands with exactly
one `-t {pane}`, and agent `/plan` and `/stop` controls. They use the same typed
pane-scoped action as their mouse pills. The target is held by stable pane,
configuration, and pane-context identity, so focus does not move and stale,
closed, or changed targets are rejected instead of applying to another pane.
`{pane}` is replaced only after revalidation with the stable owner pane. The
exact effective `on_click` source must be present and trusted at execution;
other terminal or agent effects are rejected. Only attached primary clients
may open or apply the selector.

`pane-settings --providers [-t pane]` lists already-retained blocked pane
providers using only provider names and sanitized reason codes. It is safe to
use while zen mode hides pane chrome and does not refresh, admit, or execute a
provider. After changing the underlying approval, permission, trust, context,
or sandbox condition separately, use
`pane-settings --retry-provider NAME [-t pane]` to clear that exact current
block and make the provider due for normal admission. Retry does not approve a
command, change trust or permissions, bypass policy, weaken sandboxing, or run
the command inline. Observer callers and stale, closed, missing, or unblocked
targets are rejected.

`show-pane-status [-t pane]` diagnoses the active or requested live pane without
changing focus. It reports the effective pane-status preset and override
sources, stable rail/occurrence/action ownership, unavailable or
condition-hidden entries, authoritative full/compact/hidden/overflow decisions,
cell budgets, and retained provider pending/blocked/error/stale/refresh-age
state. It uses the same condition and layout resolver as rendering, remains
available in zen mode, and does not reconcile, schedule, admit, refresh, or run
providers. Output omits provider command/output/environment data, source paths,
working directories, and raw admission failures. Missing and stale pane targets
are errors. The command requires the same attached-primary read authority as
other terminal diagnostic commands.

### Zen mode

`zen on`, `zen off`, and `zen toggle` control the session-wide live
`terminal.zen_mode` override. Successful changes are silent because their
effect is immediately visible; control clients still receive a structured
`mutated` or `noop` outcome. The command does not write configuration files or
change frame settings, so `zen off` restores the current configured frames.
Set `terminal.zen_mode = true` in configuration for persistent startup
behavior. Normal command bindings may invoke `zen toggle`. The command requires
an attached primary client, and accepts exactly one lowercase mode.

`terminal.zen_focus_label_duration_ms` controls transient zen focus labels
(1000 ms by default; 0 disables; maximum 60000). It does not change the `zen`
command syntax. Committed focus changes show the highest changed identity:
group at top-left, window at bottom-left, or pane at its top-left/shared top
divider. Labels reserve no rows and run no status providers. Required controls
take precedence; observers inherit their source primary's remaining lifetime.
A label remains pending, without an expiry timer, until a frame actually paints
it and reaches the client. Local terminals start the lifetime only after the
complete ANSI frame commits, including retained partial or deferred frames.
Iroh uses a successful server-stream flush as the delivery approximation and
does not suppress an otherwise identical view carrying a new pending label.
Stale or duplicate delivery receipts do not renew a lifetime.

### Iroh diagnostics

`show-iroh-status` displays a table for the invoking remote client's selected
Iroh path. It includes RTT, jitter, recent transfer rates, loss and congestion
deltas, congestion window, MTU, sample freshness, negotiated codec, and
connection-local session compression effectiveness. Compression reports the
decoded-to-wire ratio, bytes saved or expanded, and compressed versus identity
record counts accumulated for the current connection and codec, including
bounded X11 setup and application records but excluding the raw X11 stream
preface. Render-update diagnostics report snapshot and delta counts,
changed rows, selected wire/decoded bytes, full-snapshot candidate bytes,
coalescing, suppression, snapshot fallback, maximum ready depth, and total and
maximum write-and-flush wait. A new connection or codec context starts with an
`insufficient sample` state until it carries a complete frame, rather than
comparing counters across reconnects.
Path type and quality remain independent from compression effectiveness.
Topology identifiers, addresses, credentials, terminal contents, and
payload-derived samples are intentionally omitted.
Local control-socket clients see an unavailable state because they are not
attached through Iroh.

The bottom window bar independently shows a privacy-safe plain-text Iroh status
pill, such as `good` or `degraded`, for that same live Iroh client.
It is hidden while a command-output pager is active and returns after that
pager closes. It is omitted for local Unix-socket clients and contains no path,
endpoint, address, relay, peer, or diagnostic information; use
`show-iroh-status` for the detailed client-local table.

## Related pages

- [Key bindings](key-bindings.md)
- [Sessions and panes](../using-mezzanine/sessions-and-panes.md)
- [Terminal input, copy, and history](../using-mezzanine/terminal-input-copy-and-history.md)
- [CLI reference](cli.md)

## Next step

Read [Agent actions](agent-actions.md) for the separate action model used by
the pane-local agent.
