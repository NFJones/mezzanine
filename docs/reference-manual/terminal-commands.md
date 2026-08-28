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
arguments. Separate multiple commands with an unquoted semicolon.

Use `help` in the prompt for the effective command catalog and argument
syntax. Tab and Shift+Tab offer enumerable completions. Run `list-keys` or
press `Ctrl+A ?` to inspect the active bindings and their configuration
sources. The live prompt is authoritative because configuration and runtime
state can affect what is available.

## Common command groups

| Task | Commands |
| --- | --- |
| Manage windows and panes | `new-window`, `split-window`, `select-pane`, `resize-pane`, `rename-pane`, `list-windows`, and `list-panes` |
| Work with sessions and clients | `list-sessions`, `attach-session`, `detach-client`, `list-clients`, and `kill-session` |
| Copy and retain output | `copy-mode`, `copy-selection`, `paste-clipboard`, `paste-buffer`, `list-buffers`, `search-history`, `export-history`, and `clear-history` |
| Inspect and adjust the interface | `show-messages`, `show-iroh-status`, `list-keys`, `list-key-presets`, `set-key-preset`, `list-themes`, `set-theme`, `add-options`, `show-options`, `set-option`, `bind-key`, and `unbind-key` |
| Save or load layout state | `save-layout` and `load-layout` |

## Baseline command inventory

The baseline registry contains the following canonical commands. Live `help`
remains authoritative for arguments, aliases, configured availability, and
runtime requirements.

- **Help and configuration:** `help`, `add-options`, `show-options`,
  `set-option`, `source-file`, `refresh-client`, `bind-key`, `unbind-key`,
  `list-keys`, `list-key-presets`, `set-key-preset`, `list-themes`, and
  `set-theme`.
- **Groups and windows:** `new-group`, `rename-group`, `kill-group`,
  `select-group`, `next-group`, `previous-group`, `last-group`, `list-groups`,
  `choose-group`, `new-window`, `rename-window`, `kill-window`,
  `select-window`, `next-window`, `previous-window`, `last-window`,
  `list-windows`, `next-layout`, `select-layout`, and `rebalance-window`.
- **Panes and presentation:** `split-window`, `kill-pane`, `select-pane`,
  `resize-pane`, `next-pane`, `previous-pane`, `last-pane`, `rotate-pane`,
  `synchronize-panes`, `zoom-pane`, `swap-pane`, `break-pane`, `join-pane`,
  `display-panes`, `list-panes`, `rename-pane`, `capture-pane`, `pipe-pane`,
  and `mark-pane-ready`.
- **Sessions and clients:** `list-clients`, `detach-client`, `attach-session`,
  `list-sessions`, `rename-session`, `kill-session`, `save-layout`,
  `load-layout`, and `exit`.
- **Copy, buffers, and history:** `copy-mode`, `copy-selection`,
  `paste-clipboard`, `paste-buffer`, `create-buffer`, `list-buffers`,
  `choose-buffer`, `delete-buffer`, `save-buffer`, `clear-history`,
  `search-history`, and `export-history`.
- **Agent and diagnostics:** `agent-shell`, `show-messages`, `show-metrics`,
  and `show-iroh-status`.

Some commands require an active runtime, control endpoint, or primary-client
authority. Use `help <command>` when available and review the resulting prompt
or approval rather than assuming a command affects a detached or observer
client.

`add-options` displays the schema-owned reference for supported live
configuration paths, including purpose, type, and constrained value or format
guidance. `show-options` remains the separate view of effective configured
values and their source layers.

`show-iroh-status` displays a table for the invoking remote client's selected
Iroh path. It includes RTT, jitter, recent transfer rates, loss and congestion
deltas, congestion window, MTU, sample freshness, negotiated codec, and
connection-local session compression effectiveness. Compression reports the
decoded-to-wire ratio, bytes saved or expanded, and compressed versus identity
frame counts accumulated for the current connection and codec. Render-update diagnostics report snapshot and delta counts,
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
