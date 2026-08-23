# Configuration reference

## Purpose

Provide the exhaustive field reference for the current Mezzanine configuration
schema. Use the focused configuration chapters for workflows and this page for
an exact setting, type, default, or constraint.

## Prerequisites

Read [Configuration overview](overview.md) to understand file discovery,
precedence, trust, and validation. Compare settings against the generated
[example configuration](../examples/config.toml) for the current release.

## Related pages

- [Configuration overview](overview.md)
- [Permissions, sandbox, and trust](permissions-sandbox-and-trust.md)
- [Example configuration](../examples/config.toml)
- [SPEC.md Section 8](../../SPEC.md#8-configuration) for normative behavior

## Configuration Files and Layers

Primary config discovery resolves `$HOME/.config/mezzanine/` directly; it does
not consult `XDG_CONFIG_HOME`. It accepts exactly one of these files there:

- `config.toml`
- `config.yaml`
- `config.yml`
- `config.json`

If no primary config exists, `mez config init` creates
`~/.config/mezzanine/config.toml` with private file permissions. `mez config
set` and `mez config unset` target the selected primary configuration by
default; if none exists, the mutation creates the default TOML file. Their
`--scope project` option targets an eligible trusted project overlay instead.
For user-scoped mutations, `--file PATH` selects an existing user configuration
file under the private configuration root. With `--scope project`, it selects
the target overlay within the trusted project. These commands persist an offline
change and report `reload_required` in JSON when a running session must reload
configuration to observe it. If multiple supported primary files exist, Mez reports a
configuration error; remove or relocate all but the intended file. See
[Configuration overview](overview.md) for mutation examples and the layer
selection workflow.

The current config schema version is `63`. On launch, Mezzanine migrates an
older supported primary user config to the current schema before validation,
backfilling missing defaults, rewriting renamed settings, and removing settings
that no longer exist. Config files declaring a schema version newer than the
running binary supports are rejected instead of interpreted best-effort.

Project overlays can use `.mezzanine/config.toml`, `.mezzanine/config.yaml`,
`.mezzanine/config.yml`, or `.mezzanine/config.json` under a project directory.
Only one supported overlay file may exist in a directory; multiple files are a
configuration error. Existing overlays must declare the current schema version;
they are not migrated on load. When `mez config set --scope project` creates or
updates an eligible overlay, it writes the current version for that managed
file. The project root is the nearest ancestor of the pane working directory
with a `.git` directory or file; otherwise the pane working directory is used.

Configuration is conservative:

- Unknown top-level keys are rejected unless placed under `extensions`.
- The `session` table is no longer supported. Session attachment, detach, and
  snapshot behavior is controlled by the session CLI and in-session commands,
  rather than configuration fields.
- `session.default_command` is removed by the v1-to-v2 primary-config
  migration and rejected if it still appears in a current-schema layer; pass
  pane commands explicitly when creating windows or panes.
- The `shell`, `layout`, `message_protocol`, `control`, and `snapshots` tables
  are removed by the v15-to-v16 primary-config migration and rejected in a
  current-schema layer. Their behavior is runtime-owned; the pane shell is
  resolved from `$SHELL` or `/bin/sh`.
- `history.search_mode`, the former memory storage-path and automatic-injection
  settings, and `issues.storage` are also removed by that migration and
  rejected in current-schema layers. Their storage and retrieval behavior is
  runtime-owned.
- `agents.implementation_pressure_after_shell_actions` is removed by the
  v19-to-v20 primary-config migration and rejected in current-schema layers;
  model-facing action-pressure prompts are no longer part of runtime policy.
- Secret material is rejected from config. Use `mez auth` and credential stores.
- Live mutation accepts scalar strings, integers, booleans, and string arrays
  for supported paths.

If you are new to Mezzanine, you usually do not need the full schema on first
run. Start with `mez config init`, `mez config get`, and `mez config validate`,
then return to the schema reference when customizing behavior in detail.


## Full Config Schema

The tables below list the supported fields, first-launch defaults where
applicable, built-in provider catalog values, and concise descriptions.
`omitted` means the field is valid but not written by the first-launch config.
For a TOML primary configuration, provider connections, model profiles, and
model presets are materialized only after successful authentication for that
built-in provider. YAML and JSON primary configurations are not rewritten by
authentication. Dynamic maps are otherwise empty unless a default entry is
shown.

### Top-level fields

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `version` | integer | `63` | Config schema version. Do not change this. |
| `runtime` | table | see below | Process runtime settings. |
| `terminal` | table | see below | Terminal compatibility and presentation. |
| `keys` | table | see below | Prefix and direct key bindings. |
| `key_preset` | table | see below | Active key-assignment preset. |
| `key_presets` | map | `{}` | User-defined key-assignment presets. |
| `frames` | table | see below | Window and pane frame templates. |
| `theme` | table | see below | Active theme aliases and colors. |
| `themes` | map | `{}` | User-defined named themes. |
| `history` | table | see below | Per-pane history buffering. |
| `memory` | table | see below | Persistent memory storage, retrieval, injection, and retention defaults. |
| `issues` | table | see below | Local project issue tracking storage and availability. |
| `agents` | table | see below | Agent defaults and limits. |
| `model_profiles` | map | omitted on first launch; built-in catalog shown below | Model profile definitions. |
| `model_presets` | map | omitted on first launch; built-in catalog shown below | Named default and automatic-sizing model-profile selections. |
| `permissions` | table | see below | Approval, command, and authority policy. |
| `transport` | table | see below | Primary-user-only, disabled-by-default remote transport policy. |
| `providers` | map | omitted on first launch; built-in catalog shown below | Provider connection profiles. |
| `subagents` | map | `{}` | Named subagent profiles. |
| `personalities` | map | `{}` | User-defined agent personalities. |
| `mcp_servers` | map | `{}` | MCP server definitions. |
| `auth` | table | see below | Auth metadata paths and profile names. |
| `instructions` | table | see below | Project instruction discovery. |
| `hooks` | map | `{}` | Lifecycle and command hooks. |
| `audit` | table | see below | Security audit logging. |
| `extensions` | map | `{}` | Implementation-specific extension data. |

Shell discovery, pane layout, local messaging, and snapshot storage are runtime
behavior rather than configurable schema tables. Use the relevant task and
reference pages to inspect those live facilities.

### `transport.iroh`

This primary-user-only table is conservative and disabled by default. Project
overlays and model-authored configuration changes cannot enable or retarget it.
Unix sockets remain the default control and recovery transport. Network policy
changes require a daemon restart.

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `transport.iroh.enabled` | boolean | `false` | Start the inbound Iroh listener; this does not gate explicit outbound targets. |
| `transport.iroh.outbound_enabled` | boolean | `true` | Permit explicit `--iroh-invite-file` and `--iroh-profile` connections; set false for an administrator-controlled outbound opt-out. |
| `transport.iroh.bind_port` | integer | `0` | Server direct-transport port. `0` is ephemeral; configure 1 through 65535 for restart-stable direct invitations. |
| `transport.iroh.identity` | string | `"per_session"` | Persist a distinct protected endpoint identity for each session. |
| `transport.iroh.address_lookup` | string | `"disabled"` | Endpoint-address lookup policy. |
| `transport.iroh.address_lookup_domain` | string | `""` | Explicit lookup domain when lookup policy requires one. |
| `transport.iroh.relay_mode` | string | `"disabled"` | Relay policy; relay use is independent from direct connectivity. |
| `transport.iroh.relay_urls` | string array | `[]` | Explicit relay URLs. |
| `transport.iroh.direct_connections` | boolean | `true` | Permit direct peer connections. |
| `transport.iroh.port_mapping` | boolean | `false` | Permit automatic port mapping. |
| `transport.iroh.proxy_from_env` | boolean | `false` | Inherit supported proxy settings from the process environment. |
| `transport.iroh.system_ca_store` | boolean | `false` | Use the system CA store for applicable HTTPS infrastructure. |
| `transport.iroh.invitation_ttl_seconds` | integer | `600` | Default invitation lifetime; valid values are 30 through 86400 seconds. |
| `transport.iroh.max_connections` | integer | `16` | Maximum remote connections; valid values are 1 through 1024. |
| `transport.iroh.max_streams_per_connection` | integer | `1` | Fixed v1 limit for the single client-opened bidirectional control stream; the only valid value is 1. |
| `transport.iroh.setup_timeout_ms` | integer | `10000` | Bounded connection setup timeout. |
| `transport.iroh.idle_timeout_ms` | integer | `300000` | Bounded idle timeout. |
| `transport.iroh.compression_codecs` | string array | `["zstd", "lz4", "none"]` | Ordered, unique application-frame codec policy. Valid entries are `zstd`, `lz4`, and `none`; one through three entries are required. |
| `transport.iroh.compression_min_bytes` | integer | `512` | Complete v2 frames below this decoded size use an identity envelope. Valid values are 0 through 1048576. |
| `transport.iroh.compression_zstd_level` | integer | `3` | Zstandard level for eligible v2 frames. Valid values are -5 through 22. |

When enabled, daemon startup binds the protected per-session endpoint and runs
Iroh control alongside Unix control. A configured endpoint failure is a startup
error; Mezzanine does not silently weaken explicit enablement into Unix-only
operation. The endpoint applies the selected lookup, relay, direct-IP, port
mapping, proxy, and CA policies to both listening and explicit clients. It
advertises the configured compression ALPNs in order, accepts one client-opened
control stream per connection, and bounds setup, idle, connection, frame, and
shutdown work.

Schema v71 also defines the ordered application-layer compression policy used
by version 2 Iroh framing. This is compression of complete Mezzanine frames,
not an Iroh or QUIC transport feature. `zstd` maps to
`mezzanine/transport/2/zstd`, `lz4` maps to
`mezzanine/transport/2/lz4`, and `none` retains the unchanged
`mezzanine/transport/1` bytes. Clients try configured codecs in order only
before opening a stream; no fallback occurs after initialization data may have
been written. The selected codec then applies to eligible control and event
frames for that connection. Setting `compression_codecs = ["none"]` is the
restart-required compatibility and rollback policy.

The reproducible release benchmark (`just iroh-compression-bench`) confirms the
512-byte threshold avoids codec work for interactive small frames and that zstd
level 3 provides the strongest measured bandwidth reduction while LZ4 provides
the lower-CPU alternative. Keep these defaults unless a comparable release-mode
run of the checked-in fixtures contradicts the documented budgets in the Iroh
operations guide.

Running `mez remote status` through local Unix control ensures the protected
per-session endpoint identity exists and reports its public endpoint ID and the
current bound endpoint address when available. The private endpoint key, client
endpoint key, protected profiles, device credentials, and trust database are
not configuration fields and must not be edited directly. See [Remote pairing
and recovery](../safety-and-trust/remote-pairing-and-recovery.md).

Valid route shapes are direct-only, custom-relay-required, or controlled
direct-plus-custom-relay. Disabling direct connections while relays are
disabled is rejected. Custom relay URLs require printable HTTPS values; a
custom lookup domain is valid only with `custom_dns`. Public relay and n0 DNS
policies are development options until production service ownership and release
gates are approved. Local `remote/status` and `show-metrics` expose aggregate
listener, setup, connection, shutdown, and path diagnostics without credentials
or unnecessary peer addresses. See [Iroh production operations and
rollout](../operations/iroh-production-operations-and-rollout.md).

### `runtime`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `runtime.cpu_count` | integer | `2` | Tokio worker threads available to daemon and foreground services; must be positive. |

### `terminal`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `terminal.profile` | string | `"xterm-compatible"` | Terminal compatibility profile. `xterm-compatible` is Mezzanine's bounded implemented subset, not a full xterm-emulator claim; valid defaults include `xterm-compatible` and `dumb`. |
| `terminal.term` | string | `"xterm-256color"` | `TERM` value exposed to panes. |
| `terminal.pane_spawn_directory` | string | `"home"` | Directory policy for newly created panes: `home` or `same-directory`. |
| `terminal.pane_spawn_view` | string | `"shell"` | Initial pane surface: `shell` or `agent`. |
| `terminal.true_color` | boolean | `true` | Enable true-color presentation where supported. |
| `terminal.mouse` | boolean | `true` | Enable mouse reporting, selection, scrolling, UI clicks, and explicit visible alternate-screen selection when pane applications have not captured mouse input. |
| `terminal.bracketed_paste` | boolean | `true` | Enable bracketed paste handling. |
| `terminal.clipboard` | string | `"external"` | Pane-originated OSC 52 write policy: `external` stores internally then attempts a best-effort host copy, `internal` stores only in the internal `osc52` buffer, and `disabled` rejects the write. Clipboard queries are not answered. |
| `terminal.clipboard_copy_command` | string or string array | omitted | Host copy command; receives content on stdin. |
| `terminal.clipboard_paste_command` | string or string array | omitted | Host paste command; writes content to stdout. |
| `terminal.clipboard_read_timeout_ms` | integer | `250` | Maximum time to wait for a host clipboard helper to return pasted content; must be positive. |
| `terminal.clipboard_read_max_bytes` | integer | `1048576` | Maximum bytes accepted from a host clipboard read; must be positive. |
| `terminal.alternate_screen` | boolean | `true` | Support alternate-screen applications. |
| `terminal.focus_events` | boolean | `true` | Enable focus event reporting when supported. |
| `terminal.nested_multiplexer` | string | `"auto"` | Nested multiplexer handling mode. |
| `terminal.passthrough` | boolean | `false` | Allow broader terminal passthrough behavior when configured. |
| `terminal.emoji_width` | string | `"wide"` | Emoji status-glyph width policy: `wide` for explicit two-cell emoji-presentation sequences, `narrow` for one-cell text fallback terminals. |
| `terminal.reduced_motion` | boolean | `false` | Disable optional frame/status animations. |
| `terminal.streaming_output` | boolean | `true` | Render provisional provider output incrementally. Reduced-motion mode overrides this setting and suppresses provisional rendering; final validated output is still rendered normally. This does not control provider transport streaming. |
| `terminal.enhanced_keyboard_reporting` | boolean | `false` | Opt in to enhanced keyboard reporting while a Mez-owned readline prompt owns input on the primary client. Ordinary process panes, observers, and modal display overlays do not activate it. |
| `terminal.completion_attention_flashing` | boolean | `true` | Whether completion-attention title pills alternate their attention color. |
| `terminal.resize_debounce_ms` | integer | `200` | Milliseconds to debounce resize redraws. |
| `terminal.render_rate_limit_fps` | integer | `30` | Maximum burst render frames per second; `0` disables render rate limiting. |
| `terminal.shell_output_preview_lines` | integer | `5` | Maximum preview lines shown for shell-command output; must be positive. |
| `terminal.agent_wrap_column_cap` | integer | `120` | Maximum presentation width for persisted agent logs and transcripts; must be positive. |
| `terminal.cursor_style` | string | `"block"` | Cursor style: `block`, `underline`, or `bar`. |
| `terminal.cursor_blink` | boolean | `false` | Whether Mezzanine-rendered cursors blink. |
| `terminal.cursor_blink_interval_ms` | integer | `500` | Full blink cycle length in milliseconds. |

The historical `terminal.nested_muxxer` spelling is accepted as a version 1
migration alias and is rewritten to `terminal.nested_multiplexer` before layer
composition.

### `keys`

The prefix key table remains available even when direct bindings are omitted.

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `keys.escape` | string | `"C-a"` | Prefix key. |
| `keys.split_vertical` | string | omitted | Optional direct vertical split key. Prefix default is `Ctrl+A %`. |
| `keys.split_horizontal` | string | omitted | Optional direct horizontal split key. Prefix default is `Ctrl+A "`. |
| `keys.new_window` | string | omitted | Optional direct new-window key. Prefix default is `Ctrl+A c`. |
| `keys.new_group` | string | omitted | Optional direct new-group key. Prefix default is `Ctrl+A C`. |
| `keys.agent_shell` | string | omitted | Optional direct agent-shell key. Prefix default is `Ctrl+A a`. |
| `keys.focus_up` | string | omitted | Optional direct focus-up key. Prefix default is `Ctrl+A Up`. |
| `keys.focus_down` | string | omitted | Optional direct focus-down key. Prefix default is `Ctrl+A Down`. |
| `keys.focus_left` | string | omitted | Optional direct focus-left key. Prefix default is `Ctrl+A Left`. |
| `keys.focus_right` | string | omitted | Optional direct focus-right key. Prefix default is `Ctrl+A Right`. |
| `keys.focus_previous_window` | string | omitted | Optional direct previous-window key. Prefix default is `Ctrl+A p`. |
| `keys.focus_next_window` | string | omitted | Optional direct next-window key. Prefix default is `Ctrl+A n`. |
| `keys.focus_previous_group` | string | omitted | Optional direct previous-group key. Prefix default is `Ctrl+A (`. |
| `keys.focus_next_group` | string | omitted | Optional direct next-group key. Prefix default is `Ctrl+A )`. |
| `keys.command_bindings` | map | `{}` | User-defined key to Mezzanine command bindings. |

### `key_preset` and `key_presets`

`key_preset.active` selects a built-in or configured key-assignment preset.
The generated default is `default`, which preserves the prefix-only bindings.
The built-in `simple` preset keeps `C-a` as the prefix and adds the direct
bindings documented by `list-key-presets`.

Configured presets live under `key_presets.<name>` and accept the same fields
as `keys`. Omitted fields inherit the `default` preset; `null` explicitly
disables an optional direct binding. `set-key-preset <name>` materializes the
selected preset into `keys`, reconciles `keys.command_bindings`, applies it to
the live session, and persists it to the primary config. Later `bind-key`,
`unbind-key`, or `keys.*` changes remain explicit low-level overrides.

### `frames.window`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `frames.window.enabled` | boolean | `true` | Render the window frame/status bar. |
| `frames.window.position` | string | `"bottom"` | `top`, `bottom`, or `border`. |
| `frames.window.template` | string | `"#{window.list}"` | Left/main window frame template. |
| `frames.window.right_status` | string | `"#{pane.pwd} #{button:-|terminal|split-window -h} #{button:+|terminal|split-window} #{button:□|terminal|new-window} #{button:⊕|terminal|new-group} #{button:λ|terminal|agent-shell} #{system.uptime} #{datetime.local}#{iroh.status}"` | Right-aligned status and command buttons; the built-in `pane.pwd` display is home-relative when possible and collapses deep paths to the last three segments. |
| `frames.window.pills` | table | `{}` | Named command-backed status pills referenced from `frames.window.right_status` as `#{pill.<name>}`. |
| `frames.window.style` | string | `"default"` | Frame text style: `default`, `bold`, `underline`, `inverse`, or `reverse`. |
| `frames.window.visible_fields` | string array | `[...]` | Allowed template fields for window frames. |

Default `frames.window.visible_fields`:

```toml
["window.list", "window.index", "window.name", "window.id", "pane.index", "pane.title", "pane.id", "window.pane_count", "window.buttons", "pane.pwd", "system.uptime", "datetime.local", "iroh.status"]
```

`#{iroh.status}` is a client-local, non-clickable status pill. It renders `🔗`
only for the exact client with a live Iroh connection and is omitted without
padding for Unix-socket, never-Iroh, or disconnected clients. Good, degraded,
poor, and stale/unknown connected samples use the matching Iroh theme color
pair. It is included by the generated default but custom right-status templates
must reference it explicitly; detailed diagnostics remain in `show-iroh-status`.

Command-backed status pills are configured under `frames.window.pills.<name>`
and render only when the active right-status template references
`#{pill.<name>}`. A pill definition requires `command` and `interval_seconds`;
it may also set `label`, `initial`, `timeout_ms`, `empty_behavior`,
`error_behavior`, `max_output_chars`, and `style`. Command output uses stdout,
is trimmed to the first line, is bounded by `max_output_chars`, and is cached
between refresh intervals. Empty output behavior is `hide`, `show_empty`, or
`keep_previous`; error behavior is `hide`, `show_error`, or `keep_previous`.
Configured pills whose names are not present in `frames.window.right_status` are
not executed.

```toml
[frames.window.pills.cpu]
label = "CPU"
command = "printf '42%'"
interval_seconds = 1
timeout_ms = 750
empty_behavior = "hide"
error_behavior = "keep_previous"
max_output_chars = 32
```

### `frames.pane`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `frames.pane.enabled` | boolean | `true` | Render pane frame or border metadata. |
| `frames.pane.position` | string | `"border"` | `top`, `bottom`, or `border`. |
| `frames.pane.template` | string | `" #{pane.index} #{pane.title} "` | Pane frame template. |
| `frames.pane.style` | string | `"default"` | Frame text style. |
| `frames.pane.visible_fields` | string array | `[...]` | Allowed template fields for pane frames. |

Default `frames.pane.visible_fields`:

```toml
["pane.index", "pane.title", "pane.id", "pane.status", "history.position", "agent.model", "agent.reasoning", "agent.thinking", "agent.planning", "agent.routing", "agent.latency", "agent.preset", "agent.name", "policy.mode", "agent.context_usage", "agent.status"]
```

### Frame template fields

Window templates support `session.id`, `window.list`, `window.id`,
`window.index`, `window.title`, `window.active`, `window.pane_count`,
`window.buttons`, `window.actions`, `system.uptime`, `datetime.local`,
`layout.name`, `agent.active_count`, `message.unread_count`, and configured
command-backed status pill fields named `pill.<name>`. They may also use
active-pane fields such as `pane.index`, `pane.id`, and `pane.title`.

Pane templates support `session.id`, `window.id`, `window.index`, `pane.id`,
`pane.index`, `pane.title`, `pane.active`, `pane.size`, `pane.primary_pid`,
`pane.process_name`, `pane.exit_status`, `pane.pwd`, `pane.mode`, `pane.status`, `agent.id`,
`agent.name`, `agent.status`, `agent.model`, `agent.reasoning`,
`agent.thinking`, `agent.planning`, `agent.routing`, `agent.latency`, `agent.preset`,
`agent.context_usage`, `policy.mode`, `observer.pending_count`, and
`history.position`.

### `theme`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `theme.active` | string | `"acid_lime"` | Active built-in or configured theme. |
| `theme.aliases.<alias>` | map value | see below | Alias to `#rgb` or `#rrggbb`. |
| `theme.colors.<slot>` | map value | see below | UI color slot set to a hex color or alias. |

Default aliases:

| Alias | Default declaration | Description |
| --- | --- | --- |
| `primary` | `"#bfff00"` | Primary accent. |
| `secondary` | `"#7fbf3f"` | Secondary accent. |
| `tertiary` | `"#d7ff5f"` | Tertiary accent. |
| `thinking` | `"#c9d89a"` | Muted agent thinking/status accent. |
| `danger` | `"#ff5c57"` | Destructive/error accent. |

Default color slots:

| Slot | Default declaration | Description |
| --- | --- | --- |
| `window_frame_fg` | `"primary_foreground"` | Window frame foreground. |
| `window_frame_bg` | `"surface"` | Window frame background. |
| `window_active_fg` | `"primary_text"` | Active window pill foreground. |
| `window_active_bg` | `"primary"` | Active window pill background. |
| `window_inactive_fg` | `"secondary_text"` | Inactive window pill foreground. |
| `window_inactive_bg` | `"secondary"` | Inactive window pill background. |
| `pane_frame_active_fg` | `"secondary_text"` | Active pane frame foreground. |
| `pane_frame_active_bg` | `"secondary"` | Active pane frame background. |
| `pane_frame_inactive_fg` | `"muted"` | Inactive pane frame foreground. |
| `pane_frame_inactive_bg` | `"surface"` | Inactive pane frame background. |
| `pane_border_active_fg` | `"primary_foreground"` | Active pane border foreground. |
| `pane_border_active_bg` | `"surface"` | Active pane border background. |
| `pane_border_inactive_fg` | `"muted"` | Inactive pane border foreground. |
| `pane_border_inactive_bg` | `"surface"` | Inactive pane border background. |
| `pane_divider_fg` | `"tertiary_foreground"` | Pane divider foreground. |
| `pane_divider_bg` | `"surface"` | Pane divider background. |
| `frame_fill_fg` | `"foreground"` | Frame fill foreground. |
| `frame_fill_bg` | `"surface"` | Frame fill background. |
| `scroll_indicator_fg` | `"tertiary_text"` | Scroll indicator foreground. |
| `scroll_indicator_bg` | `"tertiary"` | Scroll indicator background. |
| `pane_pwd_fg` | `"muted_text"` | Pane working-directory pill foreground. |
| `pane_pwd_bg` | `"muted"` | Pane working-directory pill background. |
| `window_status_uptime_fg` | `"secondary_text"` | Uptime status foreground. |
| `window_status_uptime_bg` | `"secondary"` | Uptime status background. |
| `window_status_datetime_fg` | `"tertiary_text"` | Date/time status foreground. |
| `window_status_datetime_bg` | `"tertiary"` | Date/time status background. |
| `iroh_status_good_fg` | `"primary_text"` | Healthy Iroh-status pill foreground. |
| `iroh_status_good_bg` | `"primary"` | Healthy Iroh-status pill background. |
| `iroh_status_degraded_fg` | `"tertiary_text"` | Degraded Iroh-status pill foreground. |
| `iroh_status_degraded_bg` | `"tertiary"` | Degraded Iroh-status pill background. |
| `iroh_status_poor_fg` | `"danger_text"` | Poor Iroh-status pill foreground. |
| `iroh_status_poor_bg` | `"danger"` | Poor Iroh-status pill background. |
| `iroh_status_unknown_fg` | `"muted_text"` | Unknown or stale Iroh-status pill foreground. |
| `iroh_status_unknown_bg` | `"muted"` | Unknown or stale Iroh-status pill background. |
| `prompt_fg` | `"primary_foreground"` | Command prompt foreground. |
| `prompt_bg` | `"surface"` | Command prompt background. |
| `agent_prompt_fg` | `"#f8ffe0"` | Agent prompt foreground. |
| `agent_prompt_bg` | `"#20250c"` | Agent prompt background. |
| `agent_transcript_user_fg` | `"primary_foreground"` | Agent transcript user foreground. |
| `agent_transcript_user_bg` | `"surface"` | Agent transcript user background. |
| `agent_transcript_assistant_fg` | `"secondary_foreground"` | Agent transcript assistant foreground. |
| `agent_transcript_assistant_bg` | `"surface"` | Agent transcript assistant background. |
| `agent_transcript_status_fg` | `"thinking"` | Agent status/thinking foreground. |
| `agent_transcript_status_bg` | `"surface"` | Agent status/thinking background. |
| `agent_transcript_error_fg` | `"danger_foreground"` | Agent error foreground. |
| `agent_transcript_error_bg` | `"surface"` | Agent error background. |
| `agent_transcript_command_fg` | `"tertiary_foreground"` | Agent command foreground. |
| `agent_transcript_command_bg` | `"surface"` | Agent command background. |
| `agent_model_fg` | `"secondary_text"` | Agent model pill foreground. |
| `agent_model_bg` | `"secondary"` | Agent model pill background. |
| `agent_reasoning_fg` | `"tertiary_text"` | Agent reasoning pill foreground. |
| `agent_reasoning_bg` | `"tertiary"` | Agent reasoning pill background. |
| `agent_status_idle_fg` | `"muted_text"` | Idle agent status foreground. |
| `agent_status_idle_bg` | `"muted"` | Idle agent status background. |
| `agent_status_running_fg` | `"primary_text"` | Running agent status foreground. |
| `agent_status_running_bg` | `"primary"` | Running agent status background. |
| `agent_status_blocked_fg` | `"tertiary_text"` | Blocked agent status foreground. |
| `agent_status_blocked_bg` | `"tertiary"` | Blocked agent status background. |
| `agent_approval_attention_fg` | `"danger_text"` | Approval-attention foreground for pane, window, and group pills. |
| `agent_approval_attention_bg` | `"danger"` | Approval-attention background for pane, window, and group pills. |
| `agent_status_failed_fg` | `"danger_text"` | Failed agent status foreground. |
| `agent_status_failed_bg` | `"danger"` | Failed agent status background. |
| `display_overlay_fg` | `"secondary_foreground"` | Display overlay foreground. |
| `display_overlay_bg` | `"surface"` | Display overlay background. |
| `copy_selection_fg` | `"tertiary_text"` | Copy selection foreground. |
| `copy_selection_bg` | `"tertiary"` | Copy selection background. |
| `syntax_plain_fg` | `"foreground"` | Plain syntax foreground. |
| `syntax_plain_bg` | `"surface"` | Plain syntax background. |
| `syntax_keyword_fg` | `"primary_foreground"` | Keyword syntax foreground. |
| `syntax_keyword_bg` | `"surface"` | Keyword syntax background. |
| `syntax_string_fg` | `"tertiary_foreground"` | String syntax foreground. |
| `syntax_string_bg` | `"surface"` | String syntax background. |
| `syntax_comment_fg` | `"thinking"` | Comment syntax foreground. |
| `syntax_comment_bg` | `"surface"` | Comment syntax background. |
| `syntax_type_fg` | `"secondary_foreground"` | Type syntax foreground. |
| `syntax_type_bg` | `"surface"` | Type syntax background. |
| `syntax_function_fg` | `"primary_foreground"` | Function syntax foreground. |
| `syntax_function_bg` | `"surface"` | Function syntax background. |
| `syntax_number_fg` | `"tertiary_foreground"` | Number syntax foreground. |
| `syntax_number_bg` | `"surface"` | Number syntax background. |
| `syntax_operator_fg` | `"muted"` | Operator syntax foreground. |
| `syntax_operator_bg` | `"surface"` | Operator syntax background. |

### `themes.<name>`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `themes.<name>.aliases.<alias>` | string | omitted | Custom named theme alias. |
| `themes.<name>.colors.<slot>` | string | omitted | Custom named theme color slot. |

Custom named themes may omit aliases and slots. Omitted values inherit from the
documented built-in base for custom themes.

Built-in theme names include `deepforest`, `apprentice`, `gruvbox_dark`,
`gruvbox_light`, `solarized_dark`, `solarized_light`, `monokai`, `dracula`, `nord`,
`tokyo_night`, `catppuccin_latte`, `catppuccin_frappe`,
`catppuccin_macchiato`, `catppuccin_mocha`, `one_half_dark`,
`one_half_light`, `onedark`, `rose_pine`, `rose_pine_moon`, `rose_pine_dawn`,
`kanagawa`, `everforest_dark`, `everforest_light`, `ayu`, `ayu_dark`,
`ayu_light`, `ayu_mirage`, `acid_lemon`, `acid_tangerine`, `acid_lime`,
`acid_grapefruit`, `high_contrast_dark`, and `high_contrast_light`.

Built-ins fall into three fidelity groups. `apprentice`, `nord`, `tokyo_night`,
`catppuccin_latte`, `catppuccin_frappe`, `catppuccin_macchiato`,
`catppuccin_mocha`, `rose_pine`, `rose_pine_moon`, `rose_pine_dawn`,
`kanagawa`, `everforest_dark`, `everforest_light`, `dracula`, `monokai`,
`one_half_dark`, `one_half_light`, `onedark`, `ayu`, `ayu_dark`, `ayu_light`,
and `ayu_mirage` are upstream-family adaptations whose core base, foreground,
accent, muted, and danger anchors are expected to remain recognizable against
the named family. `gruvbox_dark`, `gruvbox_light`, `solarized_dark`, and
`solarized_light` are interpretive family adaptations that intentionally choose
Mezzanine UI anchors from canonical families rather than strict editor-theme
semantic slots. `deepforest`, the four `acid_*` themes, `high_contrast_dark`,
and `high_contrast_light` are Mezzanine-native themes.

### `history`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `history.lines` | integer | `10000` | Maximum retained history lines per pane. |
| `history.rotate_lines` | integer | `1000` | Number of old lines to evict on overflow. |
| `history.saved_sessions_limit` | integer | `100` | Maximum saved agent conversations listed by `/resume`; older saved sessions are deleted when new conversations are created. |
| `history.persist` | boolean | `true` | Persist retained history across supported restarts. |

### `memory`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `memory.enabled` | boolean | `true` | Enable persistent memory commands, durable memory loading, and gated on-demand memory MAAP actions. |
| `memory.max_records` | integer | `5000` | Retention cap for persistent records before archival or pruning. |
| `memory.max_bytes` | integer | `10485760` | Persistent memory content-byte cap enforced by `mez memory prune`. |
| `memory.fts_enabled` | boolean | `true` | Enable SQLite FTS candidate search for memory queries. |
| `memory.archive_before_prune` | boolean | `true` | Archive non-expired over-limit records before destructive pruning. |
| `memory.default_ttl_days` | integer | `180` | Default retention horizon for model-generated memory records when the model does not provide `expires_in_days`. Records store this as an expiration duration so selected-and-used memories refresh their expiry from wall-clock time. |

### `issues`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `issues.enabled` | boolean | `true` | Enable local SQLite project issue tracking through `mez issue`, `/issue`, and gated issue MAAP actions. |
| `issues.database_path` | string | `""` | Optional database path override; empty uses `<config_root>/issues.sqlite`. Relative paths are created privately under `<config_root>`; absolute paths are opened as caller-owned locations without creating or chmodding the parent directory. |

Issue records include a required single-line title, an `open`, `in-progress`,
or `resolved` state, optional body text for the stable issue description, and
optional mutable notes for working progress, handoff context, and next steps.
New records default to `open`, normal issue queries return open records unless a
state filter is supplied, and resolved records remain stored for history. The
intended lifecycle is `open` -> `in-progress` -> `resolved`; reopening sets the
state to `open`. The `mez issue`, `/issue`, and gated MAAP issue surfaces can
set an initial state and update state and notes without rewriting the issue
description.

### `agents`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `agents.default_provider` | string | `"openai"` | Provider profile used by default. |
| `agents.default_model_profile` | string | `"default"` | Model profile used by default. |
| `agents.active_turn_sleep_inhibition` | string | `"disabled"` | Primary-user-only host power policy: `disabled`, `system` (best-effort prevention of automatic idle system sleep), or `system-and-display` (also request display wakefulness where supported; higher battery use). It is held only while at least one canonical agent turn is `Running`, including a detached session, and releases when the final turn settles or the runtime stops or fails. Unsupported platforms and failed requests are nonfatal; current support is macOS system/display assertions, while other platforms report unavailable until a native backend is added. Neither mode overrides explicit sleep, lid-close, thermal, or critical-battery safeguards, and model-authored config changes cannot alter it. |
| `agents.shell_only` | boolean | `true` | Require local system actions to go through the pane shell. |
| `agents.shell_mode` | string | `"native"` | Default agent shell execution transport: `native` runs each action in a freshly spawned shell inferred from the pane root process without sending pane input; `pane` sends shell-backed actions through the pane shell. Use `/shell-mode status` to view the effective pane mode, configured global mode, and override provenance in the pager. Use `/shell-mode pane` or `/shell-mode native` for an active-pane override, or append `--global` to persist the default for panes without an override. |
| `agents.compaction_raw_retention_percent` | integer | `10` | Initial percent of complete raw groups retained outside model-authored summary input; provider context-limit backoff may grow the exact tail one complete group at a time; 1 to 100. |
| `agents.routing` | boolean | `false` | Enable pane-local routing selection by default. |
| `agents.action_failure_retry_limit` | integer | `5` | Self-correction attempts per repeated correctable action failure signature other than `apply_patch`. |
| `agents.loop_limit` | integer | `8` | Maximum iterations for a `/loop`; must be positive. |
| `agents.custom_system_prompt` | string | `""` | User-owned system prompt appended after built-in prompt content. |
| `agents.default_personality` | string | `""` | Default personality profile id; empty means none. |
| `agents.always_exposed_mcp_servers` | string array | `[]` | MCP server ids whose model-safe metadata and callable tools are exposed on every applicable model turn; availability alone does not instruct the model to use them. |
| `agents.auto_sizing` | table | see below | Model auto-sizing settings. |
| `agents.subagent_placement` | string | `"new-window"` | Where root-spawned subagents are placed. |
| `agents.max_concurrent_agents` | integer | `4` | Global active-agent limit; parents waiting for routed, joined, or macro dependencies release capacity and reacquire it fairly before continuing. |
| `agents.max_queued_turns` | integer | `256` | Maximum scheduler-queued agent turns; must be positive. |
| `agents.max_queued_bytes` | integer | `4194304` | Maximum estimated bytes retained across scheduler-queued turns; must be positive. |
| `agents.max_root_subagents` | integer | `4` | Maximum subagents a root agent may spawn. |
| `agents.max_subagents_per_subagent` | integer | `2` | Maximum child subagents for each subagent. |
| `agents.max_subagent_panes_per_window` | integer | `4` | Maximum subagent panes per window. |
| `agents.subagent_wait_policy` | string | `"join"` | Default wait behavior for spawned subagents. |
| `agents.max_depth` | integer | `2` | Maximum subagent tree depth. |

### `agents.auto_sizing`

Before provider authentication, every model-profile selector defaults to
`"default"`, which the runtime can resolve without a materialized provider
catalog. In a TOML primary configuration, the first successful built-in
provider login replaces these selectors with that provider's catalog choices.

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `agents.auto_sizing.root_routing_policy` | string | `"subagent"` | Where automatic sizing runs a root turn: `subagent` or `in-place`. |
| `agents.auto_sizing.router_model_profile` | string | `"default"` before login | Profile used to classify turn size. |
| `agents.auto_sizing.small_model_profile` | string | `"default"` before login | Profile for small turns. |
| `agents.auto_sizing.medium_model_profile` | string | `"default"` before login | Profile for medium turns. |
| `agents.auto_sizing.large_model_profile` | string | `"default"` before login | Profile for large turns. |
| `agents.auto_sizing.allowed_reasoning_efforts` | string array | `["low", "medium", "high", "xhigh"]` | Reasoning efforts the router may select. |
| `agents.auto_sizing.fallback_policy` | string | `"use-default-profile"` | Fallback for invalid router decisions; routing-model provider request failures are surfaced as turn errors. |

### `providers.<name>`

The first-launch configuration contains no provider entries. The declarations
below are built-in catalog values added to a TOML primary configuration after a
provider's authentication succeeds; later provider logins add their catalog
without replacing an existing default selection. Authentication does not
rewrite YAML or JSON primary configurations.

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `providers.<name>.kind` | string | `providers.openai.kind = "openai"` | Provider brand/default profile kind. Built-ins include `openai`, `anthropic`, `deepseek`, and legacy `openai-compatible`. |
| `providers.<name>.api` | string | `providers.openai.api = "openai-responses"` | Wire API compatibility: `openai-responses`, `openai-chat-completions`, `anthropic-messages`, or `deepseek-chat-completions`. |
| `providers.<name>.auth_profile` | string | `providers.openai.auth_profile = "default"` | Auth profile id. |
| `providers.<name>.base_url` | string | `providers.openai.base_url = ""` | Optional API base URL. Empty uses provider default. |
| `providers.<name>.models` | string array | see below | Selectable model ids. Empty may use provider built-ins. |
| `providers.<name>.default_model` | string | `providers.openai.default_model = "gpt-5.6-terra"` | Default model for the provider. |
| `providers.<name>.options` | table | `{}` | Provider-specific non-secret options. |
| `providers.anthropic.options.anthropic_version` | string | omitted | Optional Anthropic Messages API version header; defaults to `2023-06-01`. |
| `providers.anthropic.options.default_max_tokens` | integer | omitted | Fallback Anthropic `max_tokens` budget when the selected model profile omits `max_output_tokens`; `max_tokens` is accepted as an alias. |
| `providers.openai.options.organization_id` | string | omitted | Optional OpenAI organization header for API-key requests. |
| `providers.openai.options.project_id` | string | omitted | Optional OpenAI project header for API-key requests. |

Default `providers.openai.models`:

```toml
["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna", "gpt-5.5", "gpt-5.4", "gpt-5.4-mini"]
```

Default `providers.anthropic.models`:

```toml
["claude-fable-5", "claude-opus-5", "claude-sonnet-5", "claude-haiku-4-5-20251001"]
```

Default `providers.deepseek.models`:

```toml
["deepseek-v4-pro", "deepseek-v4-flash"]
```

Provider `api` selects the reusable wire adapter independently from provider
brand/defaults. Use `openai-responses` for Responses-compatible backends,
`openai-chat-completions` for generic Chat Completions-compatible backends,
`anthropic-messages` for the Anthropic Messages dialect,
`deepseek-chat-completions` for the DeepSeek Chat Completions dialect. Configure
one provider entry per backend, set `base_url` to the backend API base such as
`https://api.example.com/v1`, and provide `models` plus `default_model` unless
the backend's `/models` endpoint is sufficient for live catalog refresh.
The generic `openai-chat-completions` adapter uses the canonical OpenAI-style
function-tool surface and does not send DeepSeek thinking fields,
`reasoning_content`, or DeepSeek MAAP shim function names. Generic compatible
providers can tune MAAP behavior with provider options such as `tool_calls`
(`auto`, `enabled`, or `disabled`), `tool_choice` (`named`, `required`, `auto`,
or `disabled`), `parallel_tool_calls` (`auto`, `enabled`, or `disabled`),
`maap_output` (`auto`, `tools`, or `structured_json`),
`structured_output` (`auto`, `json_object`, `json_schema`, or `disabled`),
`output_token_field` (`max_tokens` or `max_completion_tokens`), and
`maap_surface` (`canonical_batch` or `content_json`). The string option
`streaming` defaults to `disabled`; set it to `enabled` only when the backend
implements standard OpenAI Chat Completions SSE chunks. Enabled streaming does
not auto-detect or fall back to unary JSON: malformed, non-SSE, provider-error,
or unterminated responses fail with a compatibility diagnostic. Supported
enabled aliases are `enable`, `true`, `yes`, and `on`; disabled aliases are
`disable`, `false`, `no`, and `off`. Provider option values are strings, so use
`streaming = "true"`, not a bare TOML boolean. LM Studio-style model
catalog capability tags such as `tool_use` are retained in provider model
metadata and copied into runtime-generated profile options as
`model_capabilities`. By default Mezzanine sends the canonical
`submit_maap_action_batch` tool with string `tool_choice = "required"`; use
`tool_choice = "named"` only for backends that accept object-valued named tool
selection. Set `maap_output = "structured_json"` and
`structured_output = "json_schema"` for LM Studio/local models that obey JSON
Schema response formats more reliably than native OpenAI tool-call emission.
The native `anthropic-messages` adapter uses Anthropic `tool_use` as the MAAP
carrier, maps profile `max_output_tokens` to wire `max_tokens`, and accepts
provider options `anthropic_version`, `reasoning_effort`, plus
`default_max_tokens`. It serializes non-empty reasoning effort as Anthropic
`output_config.effort`. It uses Anthropic Console API-key credentials.
Anthropic providers reject OpenAI-compatible or
DeepSeek-only provider options such as `maap_output`, `structured_output`,
`tool_choice`, `parallel_tool_calls`, `output_token_field`, `maap_surface`,
`prompt_cache_retention`, and `thinking`.

Example LM Studio-compatible provider:

```toml
[providers.lmstudio]
kind = "openai-compatible"
api = "openai-chat-completions"
auth_profile = "default"
base_url = "http://localhost:1234/v1"
models = ["local-model"]
default_model = "local-model"

[providers.lmstudio.options]
maap_output = "structured_json"
structured_output = "json_schema"
tool_choice = "required" # only used when maap_output selects native tools
parallel_tool_calls = "disabled"
streaming = "enabled" # optional; backend must implement standard OpenAI SSE
```

### `model_profiles.<name>`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `model_profiles.<name>.provider` | string | required for custom profiles | Provider profile id. |
| `model_profiles.<name>.model` | string | required for custom profiles | Provider model id. |
| `model_profiles.<name>.reasoning_profile` | string | profile-specific | Human-level reasoning profile. |
| `model_profiles.<name>.reasoning_effort` | string | omitted | Compatibility scalar for reasoning effort. |
| `model_profiles.<name>.latency_preference` | string | profile-specific | Latency/cost routing preference: `slow`, `default`, or `fast`. `slow` and `default` both use the standard tier; `fast` uses the premium priority tier. When omitted the API auto-selects. |
| `model_profiles.<name>.multimodal_required` | boolean | profile-specific | Require multimodal model capability. |
| `model_profiles.<name>.multimodal` | boolean | omitted | Compatibility multimodal capability flag. |
| `model_profiles.<name>.context_window_tokens` | integer | profile-specific | Display and compaction context denominator. |
| `model_profiles.<name>.context_limit_tokens` | integer | omitted | Alternative explicit context limit. |
| `model_profiles.<name>.max_input_tokens` | integer | profile-specific | Optional hard estimated cap for the complete provider request. Mez compacts eligible context before dispatch when the estimate exceeds this positive limit. |
| `model_profiles.<name>.max_output_tokens` | integer | profile/provider-specific | Optional provider output-token cap. Generated OpenAI and DeepSeek agent profiles include provider-aware recommended caps; local OpenAI-compatible examples omit the field so the provider default applies. |
| `model_profiles.<name>.provider_options` | table | see below | Provider-specific non-secret model options. |
| `model_profiles.<name>.safety_tier` | string | `"high"` in generated profiles | Safety posture label. |
| `model_profiles.<name>.privacy_tier` | string | `"standard"` in generated profiles | Privacy posture label. |
| `model_profiles.<name>.residency` | string | `"global"` in generated profiles | Data residency label. |
| `model_profiles.<name>.approval_policy` | string | `"ask"` in generated profiles | Approval policy for this profile: `ask`, `auto-allow`, or `full-access`. |
| `model_profiles.<name>.fallback_profiles` | string array | `[]` in generated profiles | Ordered fallback profile ids. |

Built-in model-profile catalog:

| Profile | Field | Default declaration |
| --- | --- | --- |
| `default` | `provider` | `"openai"` |
| `default` | `model` | `"gpt-5.6-terra"` |
| `default` | `reasoning_profile` | `"high"` |
| `default` | `latency_preference` | `"default"` |
| `default` | `multimodal_required` | `false` |
| `default` | `context_window_tokens` | `1050000` |
| `default` | `max_input_tokens` | `922000` |
| `default` | `max_output_tokens` | `16384` |
| `default` | `safety_tier` | `"high"` |
| `default` | `privacy_tier` | `"standard"` |
| `default` | `residency` | `"global"` |
| `default` | `approval_policy` | `"ask"` |
| `default` | `fallback_profiles` | `[]` |
| `auto-size-router` | `provider` | `"openai"` |
| `auto-size-router` | `model` | `"gpt-5.6-luna"` |
| `auto-size-router` | `reasoning_profile` | `"low"` |
| `auto-size-router` | `latency_preference` | `"fast"` |
| `auto-size-router` | `multimodal_required` | `false` |
| `auto-size-router` | `context_window_tokens` | `400000` |
| `auto-size-router` | `max_output_tokens` | `8192` |
| `auto-size-router` | `safety_tier` | `"high"` |
| `auto-size-router` | `privacy_tier` | `"standard"` |
| `auto-size-router` | `residency` | `"global"` |
| `auto-size-router` | `approval_policy` | `"ask"` |
| `auto-size-router` | `fallback_profiles` | `[]` |
| `auto-size-small` | `provider` | `"openai"` |
| `auto-size-small` | `model` | `"gpt-5.6-luna"` |
| `auto-size-small` | `reasoning_profile` | `"medium"` |
| `auto-size-small` | `latency_preference` | `"fast"` |
| `auto-size-small` | `multimodal_required` | `false` |
| `auto-size-small` | `context_window_tokens` | `400000` |
| `auto-size-small` | `max_output_tokens` | `16384` |
| `auto-size-small` | `safety_tier` | `"high"` |
| `auto-size-small` | `privacy_tier` | `"standard"` |
| `auto-size-small` | `residency` | `"global"` |
| `auto-size-small` | `approval_policy` | `"ask"` |
| `auto-size-small` | `fallback_profiles` | `[]` |
| `auto-size-medium` | `provider` | `"openai"` |
| `auto-size-medium` | `model` | `"gpt-5.6-terra"` |
| `auto-size-medium` | `reasoning_profile` | `"medium"` |
| `auto-size-medium` | `latency_preference` | `"default"` |
| `auto-size-medium` | `multimodal_required` | `false` |
| `auto-size-medium` | `context_window_tokens` | `1050000` |
| `auto-size-medium` | `max_output_tokens` | `16384` |
| `auto-size-medium` | `safety_tier` | `"high"` |
| `auto-size-medium` | `privacy_tier` | `"standard"` |
| `auto-size-medium` | `residency` | `"global"` |
| `auto-size-medium` | `approval_policy` | `"ask"` |
| `auto-size-medium` | `fallback_profiles` | `[]` |
| `auto-size-large` | `provider` | `"openai"` |
| `auto-size-large` | `model` | `"gpt-5.6-sol"` |
| `auto-size-large` | `reasoning_profile` | `"high"` |
| `auto-size-large` | `latency_preference` | `"default"` |
| `auto-size-large` | `multimodal_required` | `false` |
| `auto-size-large` | `context_window_tokens` | `1050000` |
| `auto-size-large` | `max_output_tokens` | `32768` |
| `auto-size-large` | `safety_tier` | `"high"` |
| `auto-size-large` | `privacy_tier` | `"standard"` |
| `auto-size-large` | `residency` | `"global"` |
| `auto-size-large` | `approval_policy` | `"ask"` |
| `auto-size-large` | `fallback_profiles` | `[]` |
| `anthropic-default` | `provider` | `"anthropic"` |
| `anthropic-default` | `model` | `"claude-sonnet-5"` |
| `anthropic-default` | `reasoning_profile` | `"high"` |
| `anthropic-default` | `latency_preference` | `"default"` |
| `anthropic-default` | `multimodal_required` | `false` |
| `anthropic-default` | `context_window_tokens` | `1000000` |
| `anthropic-default` | `max_output_tokens` | `128000` |
| `anthropic-default` | `safety_tier` | `"high"` |
| `anthropic-default` | `privacy_tier` | `"standard"` |
| `anthropic-default` | `residency` | `"global"` |
| `anthropic-default` | `approval_policy` | `"ask"` |
| `anthropic-default` | `fallback_profiles` | `[]` |
| `anthropic-default.provider_options` | `prompt_caching` | `"enabled"` |
| `anthropic-fast` | `provider` | `"anthropic"` |
| `anthropic-fast` | `model` | `"claude-haiku-4-5-20251001"` |
| `anthropic-fast` | `latency_preference` | `"fast"` |
| `anthropic-fast` | `multimodal_required` | `false` |
| `anthropic-fast` | `context_window_tokens` | `200000` |
| `anthropic-fast` | `max_output_tokens` | `64000` |
| `anthropic-fast` | `safety_tier` | `"high"` |
| `anthropic-fast` | `privacy_tier` | `"standard"` |
| `anthropic-fast` | `residency` | `"global"` |
| `anthropic-fast` | `approval_policy` | `"ask"` |
| `anthropic-fast` | `fallback_profiles` | `[]` |
| `anthropic-fast.provider_options` | `prompt_caching` | `"enabled"` |
| `deepseek-default` | `provider` | `"deepseek"` |
| `deepseek-default` | `model` | `"deepseek-v4-pro"` |
| `deepseek-default` | `reasoning_profile` | `"high"` |
| `deepseek-default` | `latency_preference` | `"default"` |
| `deepseek-default` | `multimodal_required` | `false` |
| `deepseek-default` | `context_window_tokens` | `1000000` |
| `deepseek-default` | `max_output_tokens` | `32768` |
| `deepseek-default` | `safety_tier` | `"high"` |
| `deepseek-default` | `privacy_tier` | `"standard"` |
| `deepseek-default` | `residency` | `"global"` |
| `deepseek-default` | `approval_policy` | `"ask"` |
| `deepseek-default` | `fallback_profiles` | `[]` |
| `deepseek-default.provider_options` | `thinking` | `"enabled"` |
| `deepseek-fast` | `provider` | `"deepseek"` |
| `deepseek-fast` | `model` | `"deepseek-v4-flash"` |
| `deepseek-fast` | `reasoning_profile` | `"high"` |
| `deepseek-fast` | `latency_preference` | `"fast"` |
| `deepseek-fast` | `multimodal_required` | `false` |
| `deepseek-fast` | `context_window_tokens` | `1000000` |
| `deepseek-fast` | `max_output_tokens` | `32768` |
| `deepseek-fast` | `safety_tier` | `"high"` |
| `deepseek-fast` | `privacy_tier` | `"standard"` |
| `deepseek-fast` | `residency` | `"global"` |
| `deepseek-fast` | `approval_policy` | `"ask"` |
| `deepseek-fast` | `fallback_profiles` | `[]` |
| `deepseek-fast.provider_options` | `thinking` | `"enabled"` |

Provider options under a model profile:

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `model_profiles.<name>.provider_options.reasoning_effort` | string | profile-specific | Reasoning effort sent to the provider. |
| `model_profiles.<name>.provider_options.thinking` | string | `"enabled"` for generated DeepSeek profiles | DeepSeek thinking mode override: `enabled` or `disabled`. |

### `model_presets.<name>`

Model presets select a default profile and the profiles automatic sizing uses
for a named provider-oriented choice. Each referenced profile must exist in
`model_profiles`; omitted automatic-sizing profile fields fall back to the
preset's `default_model_profile`.

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `model_presets.<name>.default_model_profile` | string | required | Default model-profile id for the preset. |
| `model_presets.<name>.auto_sizing_router_model_profile` | string | `default_model_profile` when omitted | Model-profile id used to classify automatic-sizing requests. |
| `model_presets.<name>.auto_sizing_small_model_profile` | string | `default_model_profile` when omitted | Model-profile id used for small automatically sized turns. |
| `model_presets.<name>.auto_sizing_medium_model_profile` | string | `default_model_profile` when omitted | Model-profile id used for medium automatically sized turns. |
| `model_presets.<name>.auto_sizing_large_model_profile` | string | `default_model_profile` when omitted | Model-profile id used for large automatically sized turns. |
| `model_presets.<name>.allowed_reasoning_efforts` | string array | `[]` when omitted | Allowed automatic-sizing reasoning efforts: `low`, `medium`, `high`, or `xhigh`. |

The built-in catalog defines `openai`, `deepseek`, and `anthropic` presets.
For a TOML primary configuration, each provider's preset and referenced profiles
are materialized after successful authentication for that provider. Select a
preset through the supported model-selection controls; edit the referenced
profiles when changing provider, model, or provider options.

### `permissions`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `permissions.approval_policy` | string | `"ask"` | Default approval policy: `ask`, `auto-allow`, `full-access`, or primary-user-only `host-access`. `full-access` remains sandboxed; `host-access` executes local shell actions on the host outside the configured sandbox. |
| `permissions.preset` | string | omitted | Optional preset, such as `read-only` or `auto`. |
| `permissions.sandbox` | string | `"bubblewrap"` on Linux; `"policy-only"` otherwise | Additive confinement backend. Linux defaults to fail-closed `bubblewrap`; platforms without Bubblewrap support default to `policy-only`, which does not provide OS-level isolation. |
| `permissions.read_scopes` | string array | omitted | Maximum pane-resolved read authority for the primary agent. When both scope arrays are omitted, a trusted current project is granted read-write authority for its root. Paths unavailable on the active pane are omitted with a warning. |
| `permissions.write_scopes` | string array | omitted | Maximum pane-resolved write authority; write also implies read. When both scope arrays are omitted, a trusted current project is granted read-write authority for its root. Paths unavailable on the active pane are omitted with a warning. |
| `permissions.bubblewrap.executable` | string | `"/usr/bin/bwrap"` | Absolute Bubblewrap path resolved and probed in the pane environment. |
| `permissions.bubblewrap.unavailable` | string | `"fail"` | Never runs unsandboxed automatically. A prompt-classified action may offer one exact approval-gated fallback after Bubblewrap failure. |
| `permissions.bubblewrap.network` | string | `"isolated"` | Private network namespace policy. |
| `permissions.bubblewrap.environment` | string | `"minimal"` | Clear inherited variables and rebuild a fixed non-secret environment. |
| `permissions.bubblewrap.group_whitelist` | string array | `[]` | Schema-v49 primary-user-only pane group mappings. The active pane's primary group is automatic and must not be listed. Names must be non-empty, non-numeric, and unique; at most 64 names and 8 KiB are accepted. A name unavailable in the active pane is omitted with a warning. Empty projects provide no supplementary group names but do not filter inherited pane credentials. |
| `permissions.bubblewrap.env_whitelist` | string array | `["PATH"]` when omitted | Schema-v50 primary-user-only portable variable names read from the active pane process for ordinary sandboxed actions. Values are best-effort, bounded, and universally redacted from status/logs. A successfully resolved whitelisted `PATH` controls sandbox command lookup; other fixed sandbox environment invariants remain protected. Set an explicit `[]` to opt out. Internal semantic `apply_patch` phases intentionally use the fixed environment without forwarding these optional values. |
| `permissions.bubblewrap.git_user_name` | string | omitted | Optional non-secret Git author name. Must be configured with `git_user_email`; projected only through Git command-scope configuration. |
| `permissions.bubblewrap.git_user_email` | string | omitted | Optional non-secret Git author email. Must be configured with `git_user_name`; projected only through Git command-scope configuration. |
| `permissions.command_rules` | array | `[]` | User/project command rule entries. |
| `permissions.session_command_rules` | array | `[]` | Session-scoped command rule entries. |
| `permissions.global_command_rules` | array | `[]` | Global command rule entries. |
| `permissions.network_policy` | string | `"prompt"` | Shell-network policy: `deny` isolates every Bubblewrap shell action, `allow` connects every action, and `prompt` connects authorized network actions. |
| `permissions.destructive_action_policy` | string | `"prompt"` | Destructive action policy. |
| `permissions.bypass_mode` | boolean | `false` | Explicit bypass state; cannot be enabled from config. |

Command rule fields for each entry in a command rule array:

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `id` | string | omitted | Stable configured rule identity. |
| `pattern` | string | required per rule | Command prefix, exact command, or rule pattern. |
| `decision` | string | required per rule | Rule decision: allow, prompt, or forbid. |
| `scope` | string | inferred or explicit | Rule scope such as built-in, session, project, user, or managed. |
| `match` | string | omitted | Match mode such as prefix, exact, or exact SHA-256. |
| `exact_sha256` | string | omitted | Digest for exact command matching. |
| `shell_classification` | string | `"unix-like"` when needed | Shell class used for exact command normalization. |
| `argument_policy` | string or table | omitted | Constraints for allowed arguments. |
| `executable_policy` | string or table | omitted | Constraints for executable resolution. |
| `justification` | string | omitted | Human reason for the rule. |
| `examples` | string array | omitted | Example commands covered by the rule. |
| `match_examples` | string array | omitted | Commands expected to match. |
| `not_match_examples` | string array | omitted | Commands expected not to match. |
| `effects.completeness` | string | `"unknown"` | `unknown` retains maximum scopes; `complete` permits per-command narrowing. |
| `effects.read_scopes` | string array | `[]` | Required read paths, bounded by configured maximum authority. |
| `effects.write_scopes` | string array | `[]` | Required write paths, bounded by configured maximum authority. |
| `effects.network` | boolean | required when complete | Whether network access is required. |
| `effects.credentials` | boolean | required when complete | Whether credential access is required; it does not expose host credentials. |
| `effects.process_control` | boolean | required when complete | Whether host process control is required; initially unsupported by Bubblewrap mode. |

Effects are accepted only on `allow` rules and may narrow but never grant authority.
For complete effects, Mezzanine resolves read, write, create, delete, and touch
paths before probing or launching Bubblewrap. Pane shell mode uses one
action-specific pane-shell request. Native shell mode canonicalizes the same
authority directly from the pane root process's working directory and host
filesystem metadata, without sending pane input. Resolver failure, timeout,
truncation, or stale process identity fails closed. Unknown effects retain
bounded maximum authority. Scope configuration alone determines filesystem
exposure,
including credential-bearing paths; Bubblewrap does not inspect, mask, or
reject paths based on credential-directory names. The multi-user `/home` root
remains forbidden. Exact primary, subagent, and
action requests are cached independently for the current pane environment and
configuration generation. With no explicit scopes, a trusted current project
provides the default read-write scope; nested trusted projects select the
deepest matching root, and other working directories remain unresolved.
Bubblewrap subagents inherit omitted read or write scope axes from that bounded
parent authority. Explicit child or profile scopes may only narrow the parent;
an explicit empty array remains empty, and nested children never rediscover a
broader trusted-project root. Bubblewrap activation requires usable scopes.
Schema v20 migration selects `policy-only` and does not infer scopes or effects.

`/permissions` and session permission status distinguish configured scopes from
the active pane's effective scopes. `effective_scope_provenance` is `explicit`,
`trusted-project`, or `none`; trusted-project authority also reports the selected
root. Bubblewrap status reports stable restriction identifiers:
`authority-mounts-only`, `synthetic-home`, `minimal-path`, `network-policy-enforced`.
These describe likely denial causes without including raw Bubblewrap arguments,
environment values, or unrelated host paths.

Use `mez sandbox status [PATH] [--verbose]` for a standalone configured/effective
projection with stable readiness diagnostics. Global `--json` emits the
versioned structured projection. This command is intentionally read-only: it
inspects but does not migrate configuration, change project trust, create
managed homes, or run/cache the pane-specific Bubblewrap probe. Only a direct
user may apply policy changes; diagnostics never broaden authority or select
host execution automatically.

Managed-home usage and lifecycle are available through `mez sandbox cache`.
`cache status [PATH]` is read-only and reports existence, regular-file bytes,
and whether a workload currently holds the home active. `cache clear [PATH]`
and `cache prune` preview inactive deletion candidates unless `--yes` is given;
`--dry-run` always previews. Active homes are skipped. Inspection and deletion
reject symlinks and remain scoped to Mezzanine's private project/profile cache
root. There is no automatic cleanup or persisted quota setting.

Guided setup is available through `mez sandbox plan`, `enable`, `preset apply`,
and `disable`. Project trust records are managed through `mez sandbox trust`.
The code-owned presets are
`project-safe` (Bubblewrap plus `ask`), `project-auto` (Bubblewrap plus
`auto-allow`), `project-read-only` (project read scope with no write scope),
and `off` (policy-only while retaining the other sandbox settings). Planning
and `--dry-run` are read-only. Every guided setup mutation requires confirmation;
noninteractive or JSON mutation requires `--yes`, and setup must explicitly
choose `trusted-project` or `explicit-scope` authority. Trusted-project mode
can activate applicable project overlays, macros, and skills, while
explicit-scope mode does not change project trust.

`mez sandbox profile export [--path PATH]` emits a deterministic version-2 JSON
recipe with exactly `version`, `preset`, and `authority`. Export derives only
safe preset-equivalent state; it omits host paths, trust records,
Bubblewrap executable and identity fields, environment values, command rules,
hooks, provider/MCP state, arbitrary mounts, credentials, and `host-access`.
`mez sandbox profile import FILE [--path PATH] [--dry-run] [--yes]` strictly
rejects version-1 recipes and unknown or unsupported fields, including the
removed toolchain field. It independently discovers the local project and uses
the same atomic guided-setup transaction. Import previews by default and never
auto-applies a repository profile, even when that repository is already trusted.

Bubblewrap disables system and global Git configuration. When both sanitized
identity fields are configured, Mezzanine projects only `user.name` and
`user.email` through Git command-scope configuration, which takes precedence
over repository-local identity. It never imports credential helpers, signing
keys, includes, URL rewrites, hooks, or arbitrary host Git settings. When the
fields are omitted, repository-local Git identity may still apply.

Schema v51 removes `permissions.bubblewrap.toolchains` and
`permissions.bubblewrap.custom_toolchains`. Primary configurations migrating
from v50 lose both fields; current primary and project configuration rejects
them. There is no replacement selector or discovery command.

Use ordinary scopes and environment forwarding for an installed SDK:

```toml
[permissions]
read_scopes = ["/opt/acme-sdk"]

[permissions.bubblewrap]
env_whitelist = ["ACME_HOME"]
```

`/opt/acme-sdk` is mounted read-only at the same path. `ACME_HOME` must already
exist in the active pane, its value remains redacted from status and audit, and
it grants no filesystem authority. An omitted allowlist forwards `PATH` for
sandbox command lookup; an explicit list replaces that default, so this example
uses the fixed `/usr/bin:/bin` search path. Include `PATH` when sandboxed
commands need the pane's command-search path. Scope every required loader,
library, or dependency root explicitly; scoping the SDK does not expose
credentials, host caches, manager state, sockets, or unrelated installations.

For a trusted project, Bubblewrap uses a persistent managed home below
`<config-root>/sandbox/cache-homes/<project-profile-key>/home`. The key hashes
the canonical project root and Bubblewrap runtime-profile version, so panes in
the same project share caches while different projects and future incompatible
profiles remain isolated. The host directory and its `.cache`, `.config`,
`.local/share`, and `.local/state` children are user-private and are mounted at
`/home/<pane-user>`; the corresponding `HOME` and XDG variables point only
inside that mount. The synthetic account and its home path use the active pane
user name, while the mount never copies the real home, credentials, or user
configuration. Trust revocation performs best-effort removal of that project's keyed home.
Mezzanine does not currently enforce a built-in size quota or periodic age-based
pruning; operators may apply filesystem quotas or remove inactive private cache
directories while no sandbox command is using them.

Each managed home stores immutable synthetic passwd/group records below an
identity-hash directory. The passwd entry uses the pane UID and primary GID;
the group file uses the pane primary-group name and only the configured active
supplementary groups. Changing the mapping does not discard the project's
persistent XDG caches.

Mezzanine collects the active UID, primary GID, and named kernel group set from
the active pane bootstrap. It rejects unknown or inactive configured names,
duplicate GID mappings, and the automatic primary group. Probes and workloads
invoke the pane-local configured Bubblewrap executable directly. Mezzanine does
not add, replace, or filter supplementary credentials, so a pane session must
already carry every configured group and may retain unconfigured ambient groups.

Mezzanine then validates the configured Bubblewrap executable inside the target
pane environment. The probe requires usable
user, mount, PID, IPC, UTS, cgroup, and network namespaces plus the fixed
read-only runtime projection. Missing executables and unsupported namespace
facilities never trigger an automatic unsandboxed retry. For a local action
whose original policy decision was `prompt`, `ask` mode first requires the
ordinary action approval; Bubblewrap never substitutes for that approval gate.
`auto-allow` still requires the model-rationale gate, and `full-access` skips
whitelist prompts, but both remain confined by Bubblewrap. A subsequent
probe/setup/pre-exec failure may create one normal approval for an exact
unsandboxed retry only when the retained action decision was `prompt`; sandbox
failure never weakens `full-access` automatically. A failed or
timed-out probe is not cached; after shell readiness recovers, a later
independent action may probe the same identity again. Successful capabilities
remain cached, and concurrent waiters share one in-flight probe.

Bubblewrap lifecycle status is captured separately from command output. A
validated `exit-code` event proves payload execution; clean status closure
without that event is pre-payload failure evidence. For a non-zero payload exit,
Mezzanine may run one bounded structured model assessment using redacted policy,
effect, restriction, status, and output evidence. Only an explicit
`sandbox_failure` assessment may create an approval, and that approval warns
that partial effects may already exist. Command-failure, uncertain, malformed,
timed-out, or failed assessments settle the original command normally. Approval
never causes automatic execution: it grants only the retained turn/action one
unsandboxed retry, and the grant is consumed exactly once.

### `subagents.<name>`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `subagents.<name>.name` | string | omitted | Display name. |
| `subagents.<name>.description` | string | omitted | Role description. |
| `subagents.<name>.developer_instructions` | string | omitted | Role-specific developer instructions. |
| `subagents.<name>.developer_prompt` | string | omitted | Compatibility developer prompt field. |
| `subagents.<name>.model_profile` | string | omitted | Model profile id. |
| `subagents.<name>.model_profile_override` | string | omitted | Runtime override profile id. |
| `subagents.<name>.permission_preset` | string | omitted | Permission preset for the role. |
| `subagents.<name>.permission_override` | string | omitted | Permission override policy. |
| `subagents.<name>.mcp_servers` | string array | omitted | MCP server ids available to this role. |
| `subagents.<name>.shell_env` | map | omitted | Extra shell environment for this role. |
| `subagents.<name>.default_cooperation_mode` | string | omitted | Cooperation mode default. |
| `subagents.<name>.default_mode` | string | omitted | Compatibility mode default. |
| `subagents.<name>.default_read_scopes` | string array | omitted | Default child read-scope narrowing; Bubblewrap intersects it with inherited parent authority. |
| `subagents.<name>.default_write_scopes` | string array | omitted | Default child write-scope narrowing; Bubblewrap intersects it with inherited parent authority. |

### `personalities.<name>`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `personalities.<name>.name` | string | omitted | Display name. |
| `personalities.<name>.system_prompt` | string | omitted | System prompt text for the profile. |
| `personalities.<name>.instructions` | string | omitted | Additional system instructions. |
| `personalities.<name>.response_style` | string | omitted | Response style guidance. |
| `personalities.<name>.style` | string | omitted | Compatibility style field. |
| `personalities.<name>.model_profile` | string | omitted | Preferred model profile. |
| `personalities.<name>.planning_enabled` | boolean | omitted | Enable planning behavior for the profile. |
| `personalities.<name>.planning` | boolean | omitted | Compatibility planning field. |
| `personalities.<name>.routing_enabled` | boolean | omitted | Enable routing for the profile. |
| `personalities.<name>.routing` | boolean | omitted | Compatibility routing field. |

### `mcp_servers.<name>`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `mcp_servers.<name>.name` | string | omitted | Human-readable server name. |
| `mcp_servers.<name>.command` | string | omitted | Stdio server command. |
| `mcp_servers.<name>.args` | string array | omitted | Stdio server arguments. |
| `mcp_servers.<name>.url` | string | omitted | Streamable HTTP server URL. |
| `mcp_servers.<name>.env` | map | omitted | Extra environment values. |
| `mcp_servers.<name>.env_vars` | string array | omitted | Environment variable names to pass through. |
| `mcp_servers.<name>.cwd` | string | omitted | Server working directory. |
| `mcp_servers.<name>.http_headers` | map | omitted | HTTP headers for streamable HTTP servers. |
| `mcp_servers.<name>.bearer_token_env` | string | omitted | Environment variable containing a bearer token. |
| `mcp_servers.<name>.enabled_tools` | string array | omitted | Tool allow-list. |
| `mcp_servers.<name>.disabled_tools` | string array | omitted | Tool deny-list. |
| `mcp_servers.<name>.startup_timeout_sec` | integer | omitted | Startup timeout in seconds. |
| `mcp_servers.<name>.startup_timeout_ms` | integer | omitted | Startup timeout in milliseconds. |
| `mcp_servers.<name>.tool_timeout_sec` | integer | omitted | Tool timeout in seconds. |
| `mcp_servers.<name>.tool_timeout_ms` | integer | omitted | Tool timeout in milliseconds. |
| `mcp_servers.<name>.enabled` | boolean | omitted | Whether the server is enabled. |
| `mcp_servers.<name>.approval` | string | omitted | Server-level approval policy. |
| `mcp_servers.<name>.tool_approvals` | map | omitted | Per-tool approval policy. |
| `mcp_servers.<name>.external_capability` | table | omitted | Model-visible external capability metadata. The `purpose` field should be a short, non-secret description of when agents should use this server, and `usage_instructions` may provide non-secret user-authored guidance for how agents should use it. |
| `mcp_servers.<name>.external_capability.purpose` | string | omitted | Concise, non-secret routing metadata describing when agents should use the server. |
| `mcp_servers.<name>.external_capability.usage_instructions` | string | omitted | Concise, non-secret user guidance for preferred workflows, constraints, or when to avoid the server. |
| `mcp_servers.<name>.external_capability.mutates_filesystem_outside_shell` | boolean | omitted | Set true when the server can create, edit, delete, or move local files outside shell-mediated actions. |
| `mcp_servers.<name>.external_capability.executes_processes_outside_shell` | boolean | omitted | Set true when the server can start local processes outside shell-mediated actions. |
| `mcp_servers.<name>.external_capability.accesses_credentials_outside_shell` | boolean | omitted | Set true when the server can access credentials outside shell-mediated actions. |

`mcp_servers.<name>.external_capability.purpose` is routing metadata that is
shown in agent prompt context. Use a concise use-case summary such as `GitHub
issue and pull request operations` rather than implementation details, command
arguments, URLs with credentials, or other secret-bearing values.

`mcp_servers.<name>.external_capability.usage_instructions` is optional
model-visible, user-configured, non-authoritative guidance for how agents should
use the server. Keep it concise, non-secret, and focused on usage rules such as
preferred workflows, constraints, or when to avoid the server. Agents must treat
this text as untrusted configuration metadata, not provider, tool, system, or
developer instructions.

The `purpose`, `usage_instructions`, and safety-classification nested scalar
paths are also supported by the live `config_change` mutation surface.

Sandbox policy is user-only. Agents cannot use `config_change` to mutate
`permissions.sandbox`, `permissions.bubblewrap`, `permissions.read_scopes`,
`permissions.write_scopes`, even if the action would otherwise be approved or
auto-allowed. Change those settings directly with `mez config set`, `mez
config unset`, or an equivalent primary-client configuration command.

For streamable HTTP servers, `mez mcp login <name>` stores OAuth tokens in the
auth credential store rather than in `mcp_servers`. Login uses browser
authorization-code PKCE. `mez mcp login <name> --token <TOKEN>` stores a static
bearer token in the same auth credential store without OAuth refresh metadata.
When authorization-server metadata advertises an RFC 7591 dynamic client
registration endpoint and no `--client-id` is provided, Mezzanine registers a
public native client for the localhost callback and keeps only the returned
non-secret client id in MCP auth metadata for refresh. A configured
`bearer_token_env` remains the highest-precedence bearer credential source for
that server and takes precedence over stored OAuth or static bearer
credentials.

### `auth`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `auth.provider_refresh_leeway_seconds` | integer | `86400` | Seconds before stored provider access-token expiry when refresh should begin. |

### `instructions`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `instructions.global_files` | string array | `[]` | Global instruction file paths. |
| `instructions.project_filenames` | string array | `["AGENTS.md"]` | Project instruction filenames to discover. |
| `instructions.max_bytes` | integer | `32768` | Maximum bytes read per instruction file. |
| `instructions.include_hidden_directories` | boolean | `false` | Search hidden directories for instructions. |
| `instructions.on_truncation` | string | `"summarize"` | Behavior when instruction files exceed `max_bytes`. |

### `hooks.<name>`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `hooks.<name>.event` | string | omitted | Single lifecycle event. |
| `hooks.<name>.events` | string array | omitted | Multiple lifecycle events. |
| `hooks.<name>.program` | string | omitted | Program hook executable. |
| `hooks.<name>.command` | string | omitted | Command or focused-shell hook text. |
| `hooks.<name>.args` | string array | omitted | Program hook arguments. |
| `hooks.<name>.shell` | string | omitted | Focused-shell hook command. |
| `hooks.<name>.kind` | string | omitted | Hook kind. |
| `hooks.<name>.enabled` | boolean | omitted | Whether the hook is enabled. |
| `hooks.<name>.required` | boolean | omitted | Whether hook failure blocks the triggering action. |
| `hooks.<name>.agent_hook` | boolean | omitted | Whether the hook is agent-facing. |
| `hooks.<name>.timeout_ms` | integer | omitted | Hook timeout in milliseconds. |
| `hooks.<name>.timeout_sec` | integer | omitted | Hook timeout in seconds. |
| `hooks.<name>.on_failure` | string | omitted | Failure behavior. |
| `hooks.<name>.match` | table | omitted | Single matcher definition. |
| `hooks.<name>.matches` | array | omitted | Matcher group definitions. |
| `hooks.<name>.env` | map | omitted | Extra hook environment. |
| `hooks.<name>.working_directory` | string | omitted | Hook working directory. |
| `hooks.<name>.cwd` | string | omitted | Compatibility working-directory field. |
| `hooks.<name>.inject_instructions` | boolean | omitted | Inject hook output into agent instructions. |
| `hooks.<name>.mutates_policy` | boolean | omitted | Declares that the hook can mutate policy. |
| `hooks.<name>.alters_action` | boolean | omitted | Declares that the hook can alter an action. |

Hook events include `session_start`, `session_stop`, `client_attach`,
`client_detach`, `window_create`, `window_close`, `session_detach`,
`pane_create`, `pane_close`, `user_prompt_submit`, `agent_turn_start`,
`agent_turn_stop`, `pre_shell_command`, `post_shell_command`,
`permission_request`, `permission_decision`, `pre_mcp_tool_use`,
`post_mcp_tool_use`, `snapshot_create`, and `snapshot_resume`.

### `audit`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `audit.enabled` | boolean | `true` | Enable audit logging. |
| `audit.path` | string | `"audit.jsonl"` | Audit log path under config root. |
| `audit.format` | string | `"jsonl"` | Audit log format. |
| `audit.retention_days` | integer | `30` | Audit retention period. |
| `audit.hash_chain` | boolean | `false` | Enable hash chaining of audit records. |
| `audit.required` | boolean | `false` | Require audit logging for sensitive operations. |

Agent shell-command records identify the active sandbox backend. Bubblewrap
records also contain only redacted plan facts: runtime-profile version,
maximum/narrowed authority source, read-only and read-write mount counts,
protected-mask count, and launch-plan SHA-256. Mount paths, Bubblewrap argv,
command content, and environment values are never included. Policy-only records
omit plan-specific fields.

### `extensions.<name>`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `extensions.<name>.*` | implementation-defined | omitted | Extension-specific config. Unknown non-extension top-level keys are rejected. |
