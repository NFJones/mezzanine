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

Where supported, `-t TARGET` selects the target, `-s SOURCE` selects a source
for a move or copy, `-c DIRECTORY` sets a new pane's starting directory, and
`-F FORMAT` requests formatted output. Use `list-panes`, `list-windows`,
`list-groups`, or `list-clients` to find identities and indexes before targeting
an object. Flags are command-specific, not interchangeable across commands.

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

The implementation also accepts `choose-window` for an interactive window
picker and `move-window -t INDEX` to reindex the current window; these are not
listed in the baseline catalog above.

## Selected command contracts

The following commands have behavior or safety boundaries that are useful to
know without opening the complete normative contract.

### Pane creation and pipe shell commands

`new-window`/`neww`, `new-group`/`newg`, and `split-window`/`splitw` accept a
command as `--shell-command STRING` or `--command STRING`. A spelling that has
a value takes precedence over words after `--`, and words after `--` take
precedence over positional words; `new-window` and `new-group` treat positional
words as the command only when `-n`/`--name` is present. With no command, a new
pane starts an interactive shell. `-c DIRECTORY` overrides
`terminal.pane_spawn_directory`.

An explicit command string is shell source: quote it for the Mezzanine prompt
so that shell operators reach the new pane's shell. Words after `--` are
preserved as literal command arguments instead. For example:

```text
new-window -n build -c /tmp -- make test
split-window -h -d --shell-command 'printf "ready\\n"; exec bash'
```

`split-window` selects the new pane by default; `-d`/`--no-select` keeps focus
on the original pane. `-h` splits horizontally; the default is vertical.

`pipe-pane` sends subsequent pane output to a file or shell command:

```text
pipe-pane -o /tmp/pane.log
pipe-pane --list
pipe-pane --stop
```

Its positional command words are joined with spaces and interpreted by the
shell, so use one quoted string when shell quoting matters. Do not paste
untrusted shell source into either command form. Completion is deliberately
limited for shell source: unsafe path suggestions and ambiguous tokens receive
no candidates. Lack of a completion does not mean a command is unsupported.

### Shared input, shutdown, and saved layouts

`synchronize-panes on|off|toggle|status` controls ordinary process input for the
current window. When enabled, typing reaches every pane in that window; check
`status` before entering a destructive command and use `off` when finished.

Closing live panes, windows, groups, or sessions can require confirmation or
an explicit force flag. `exit` terminates the current session and all its panes;
it is not a detach command. Use `detach-client` to leave processes running.
`kill-group` cannot close the final group; terminate the session instead.

`save-layout` and `load-layout` save and restore layout snapshots, not running
processes or agent conversations. See [CLI snapshots](cli.md#snapshot-forms)
for what is retained and for offline inspection and restore commands.

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
one `-t {pane}`, and agent `/plan` and `/stop` controls. Only trusted configured
actions may run. Selecting an entry does not move focus; closed or changed
targets are rejected rather than applying the action to another pane. Only
attached primary clients may open or use the selector.

`pane-settings --providers [-t pane]` lists already-retained blocked pane
status providers by name and reason code, without running them. It remains
available while zen mode hides pane chrome. After resolving the reported
approval, permission, trust, context, or sandbox condition separately, use
`pane-settings --retry-provider NAME [-t pane]` to clear that exact current
block so the provider can be retried normally. Retry does not approve a
command, change permissions or trust, bypass sandboxing, or run the command
immediately. It requires a primary client and a currently blocked provider on
a live, unchanged pane.

`show-pane-status [-t pane]` diagnoses the active or requested live pane without
changing focus or running status providers. Use it to explain which preset and
overrides apply, why entries are hidden or moved to overflow, how much display
space they have, and whether a provider is pending, blocked, failed, or stale.
It remains available in zen mode. Sensitive provider commands, output,
environment values, paths, and raw failures are omitted. An attached primary
client and a live pane target are required.

### Zen mode

`zen on`, `zen off`, and `zen toggle` control the session-wide live
`terminal.zen_mode` override. Successful changes are silent because their
effect is immediately visible. The command does not write configuration files or
change frame settings, so `zen off` restores the current configured frames.
Set `terminal.zen_mode = true` in configuration for persistent startup
behavior. Normal command bindings may invoke `zen toggle`. The command requires
an attached primary client, and accepts exactly one lowercase mode.

`terminal.zen_focus_label_duration_ms` controls transient zen focus labels
(1000 ms by default; 0 disables; maximum 60000). It does not change the `zen`
command syntax. Focus changes briefly identify the highest changed level:
group at top-left, window at bottom-left, or pane at its top-left/shared top
divider. Labels take no extra rows and do not run status providers. Required
controls take precedence. The lifetime starts when the label is presented,
not while a redraw is still waiting; observers share the source primary's
remaining label lifetime.

### Iroh diagnostics

`show-iroh-status` displays a table for the invoking remote client's selected
Iroh path. Use latency (RTT), jitter, transfer rates, packet loss, congestion,
and sample freshness to diagnose a slow or unstable connection. The table also
shows the negotiated compression codec, bytes saved or expanded, and rendering
traffic and wait measurements. Compression totals include X11 traffic and
restart for each connection; a new connection may report `insufficient sample`.
Good compression does not imply a good network path. Addresses, endpoint
identities, credentials, and terminal contents are omitted. Local Unix-socket
clients see an unavailable state because they are not attached through Iroh.

The bottom window bar independently shows a privacy-safe Iroh connection pill
for that same client: `up` while connected, or `dn` when its retained view is
shown as disconnected. Color indicates sampled path quality; the label itself
does not distinguish `good`, `degraded`, `poor`, or `unknown`. Use
`show-iroh-status` to read quality and measurements without relying on color.
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
