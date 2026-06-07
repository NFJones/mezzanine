# Configuration Reference
This document collects the stable configuration guidance, generated default schema, and supported fields for current Mezzanine builds.

Related docs:
- [README](../README.md) for overview, quick start, and daily workflows.
- [Documentation guide](README.md) for doc entry points by audience.
- [Example config](examples/config.toml) for the generated baseline file.
- [SPEC.md Section 8](../SPEC.md#8-configuration) for normative behavior.

## Configuration Files and Layers

Primary config discovery looks for exactly one of these files under
`~/.config/mezzanine/`:

- `config.toml`
- `config.yaml`
- `config.yml`
- `config.json`

If no primary config exists, `mez config init` creates
`~/.config/mezzanine/config.toml` with private file permissions.

The current config schema version is `12`. On launch, Mezzanine migrates an
older supported primary user config to the current schema before validation,
backfilling missing defaults, rewriting renamed settings, and removing settings
that no longer exist. Config files declaring a schema version newer than the
running binary supports are rejected instead of interpreted best-effort.

Project overlays are intended for `.mezzanine/config.toml` under the project
root. The project root is the nearest ancestor of the pane working directory
with a `.git` directory or file; otherwise the pane working directory is used.

Configuration is conservative:

- Unknown top-level keys are rejected unless placed under `extensions`.
- `session.default_command` is removed by the v1-to-v2 primary-config
  migration and rejected if it still appears in a current-schema layer; pass
  pane commands explicitly when creating windows or panes.
- `shell.path`, `shell.executable`, and `shell.command` are removed by the
  v1-to-v2 primary-config migration and rejected if they still appear in a
  current-schema layer; the shell executable is resolved from `$SHELL` or
  `/bin/sh`.
- Secret material is rejected from config. Use `mez auth` and credential stores.
- Live mutation accepts scalar strings, integers, booleans, and string arrays
  for supported paths.

If you are new to Mezzanine, you usually do not need the full schema on first
run. Start with `mez config init`, `mez config get`, and `mez config validate`,
then return to the schema reference when customizing behavior in detail.


## Full Config Schema

The tables below list the supported fields, generated defaults, and concise
descriptions. `omitted` means the field is valid but not written by the
generated default config. Dynamic maps are empty by default unless a default
entry is shown.

### Top-level fields

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `version` | integer | `12` | Config schema version. Do not change this. |
| `session` | table | see below | Session lifecycle behavior. |
| `terminal` | table | see below | Terminal compatibility and presentation. |
| `shell` | table | see below | Shell mode and environment policy. |
| `keys` | table | see below | Prefix and direct key bindings. |
| `layout` | table | see below | Pane layout policy. |
| `frames` | table | see below | Window and pane frame templates. |
| `theme` | table | see below | Active theme aliases and colors. |
| `themes` | map | `{}` | User-defined named themes. |
| `history` | table | see below | Per-pane history buffering. |
| `memory` | table | see below | Persistent memory storage, retrieval, injection, and sidecar defaults. |
| `agents` | table | see below | Agent defaults and limits. |
| `model_profiles` | map | default profiles shown below | Model profile definitions. |
| `permissions` | table | see below | Approval, command, and authority policy. |
| `providers` | map | `providers.openai` | Provider connection profiles. |
| `subagents` | map | `{}` | Named subagent profiles. |
| `personalities` | map | `{}` | User-defined agent personalities. |
| `message_protocol` | table | see below | Local agent message passing. |
| `control` | table | see below | Control endpoint settings. |
| `mcp_servers` | map | `{}` | MCP server definitions. |
| `auth` | table | see below | Auth metadata paths and profile names. |
| `instructions` | table | see below | Project instruction discovery. |
| `hooks` | map | `{}` | Lifecycle and command hooks. |
| `snapshots` | table | see below | Snapshot persistence policy. |
| `audit` | table | see below | Security audit logging. |
| `extensions` | map | `{}` | Implementation-specific extension data. |

### `session`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `session.detach_behavior` | string | `"keep-running"` | What happens to panes when the primary client detaches. |
| `session.reattach_behavior` | string | `"default-session"` | How bare attach/start resolves resumable sessions. |
| `session.empty_session_behavior` | string | `"keep-open"` | What happens when the final window or pane closes. |
| `session.restore_strategy` | string | `"live-first"` | Preference for live state versus restored state. |
| `session.default_command` | string | rejected | Not supported in v1; use explicit pane or window commands. |

### `terminal`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `terminal.profile` | string | `"xterm-compatible"` | Terminal compatibility profile. Valid defaults include `xterm-compatible` and `dumb`. |
| `terminal.term` | string | `"screen-256color"` | `TERM` value exposed to panes; must not claim host identity such as `xterm-256color`. |
| `terminal.true_color` | boolean | `true` | Enable true-color presentation where supported. |
| `terminal.mouse` | boolean | `true` | Enable mouse reporting, selection, scrolling, UI clicks, and explicit visible alternate-screen selection when pane applications have not captured mouse input. |
| `terminal.bracketed_paste` | boolean | `true` | Enable bracketed paste handling. |
| `terminal.clipboard` | string | `"external"` | Clipboard integration mode. |
| `terminal.clipboard_copy_command` | string or string array | omitted | Host copy command; receives content on stdin. |
| `terminal.clipboard_paste_command` | string or string array | omitted | Host paste command; writes content to stdout. |
| `terminal.alternate_screen` | boolean | `true` | Support alternate-screen applications. |
| `terminal.focus_events` | boolean | `true` | Enable focus event reporting when supported. |
| `terminal.nested_multiplexer` | string | `"auto"` | Nested multiplexer handling mode. |
| `terminal.passthrough` | boolean | `false` | Allow broader terminal passthrough behavior when configured. |
| `terminal.emoji_width` | string | `"wide"` | Emoji status-glyph width policy: `wide` for two-cell emoji renderers, `narrow` for one-cell text fallback terminals. |
| `terminal.reduced_motion` | boolean | `false` | Disable optional frame/status animations. |
| `terminal.resize_debounce_ms` | integer | `200` | Milliseconds to debounce resize redraws. |
| `terminal.render_rate_limit_fps` | integer | `5` | Maximum burst render frames per second; `0` disables render rate limiting. |
| `terminal.cursor_style` | string | `"block"` | Cursor style: `block`, `underline`, or `bar`. |
| `terminal.cursor_blink` | boolean | `false` | Whether Mezzanine-rendered cursors blink. |
| `terminal.cursor_blink_interval_ms` | integer | `500` | Full blink cycle length in milliseconds. |

The historical `terminal.nested_muxxer` spelling is accepted as a version 1
migration alias and is rewritten to `terminal.nested_multiplexer` before layer
composition.

### `shell`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `shell.login` | boolean | `false` | Start pane shells as login shells when supported. |
| `shell.interactive` | boolean | `true` | Start pane shells interactively. |
| `shell.integration` | boolean | `true` | Enable passive shell integration when possible. |
| `shell.integration_mode` | string | `"passive"` | Shell integration strategy. |
| `shell.default_working_directory` | string | `"."` | Initial pane working directory. |
| `shell.env` | map | `{}` | Extra environment values for pane shells. |
| `shell.tool_discovery` | boolean | `true` | Discover shell tools for agent context. |
| `shell.tool_cache` | boolean | `true` | Cache discovered tools by environment signature. |
| `shell.fallback_behavior` | string | `"bin-sh"` | Fallback behavior when `$SHELL` is unusable. |
| `shell.path` | string | rejected | Shell executable override is not allowed. |
| `shell.executable` | string | rejected | Shell executable override is not allowed. |
| `shell.command` | string | rejected | Default shell command override is not allowed. |

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

### `layout`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `layout.default` | string | `"tiled"` | Default layout policy. |
| `layout.resize_policy` | string | `"relative"` | How layout ratios respond to terminal resize. |
| `layout.close_policy` | string | `"rebalance"` | How remaining panes fill space after close. |
| `layout.min_pane_columns` | integer | `8` | Minimum pane width. |
| `layout.min_pane_rows` | integer | `3` | Minimum pane height. |

### `frames.window`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `frames.window.enabled` | boolean | `true` | Render the window frame/status bar. |
| `frames.window.position` | string | `"bottom"` | `top`, `bottom`, or `border`. |
| `frames.window.template` | string | `"#{window.list}"` | Left/main window frame template. |
| `frames.window.right_status` | string | `"#{pane.pwd} #{button:-|terminal|split-window -h} #{button:+|terminal|split-window} #{button:□|terminal|new-window} #{button:⊕|terminal|new-group} #{button:λ|terminal|agent-shell} #{system.uptime} #{datetime.local}"` | Right-aligned status and command buttons; the built-in `pane.pwd` display is home-relative when possible and collapses deep paths to the last three segments. |
| `frames.window.style` | string | `"default"` | Frame text style: `default`, `bold`, `underline`, `inverse`, or `reverse`. |
| `frames.window.visible_fields` | string array | `[...]` | Allowed template fields for window frames. |

Default `frames.window.visible_fields`:

```toml
["window.list", "window.index", "window.name", "window.id", "pane.index", "pane.title", "pane.id", "window.pane_count", "window.buttons", "pane.pwd", "system.uptime", "datetime.local"]
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
["pane.index", "pane.title", "pane.id", "history.position", "agent.model", "agent.reasoning", "agent.thinking", "agent.routing", "agent.latency", "agent.preset", "agent.name", "policy.mode", "agent.context_usage", "agent.status"]
```

### Frame template fields

Window templates support `session.id`, `window.list`, `window.id`,
`window.index`, `window.title`, `window.active`, `window.pane_count`,
`window.buttons`, `window.actions`, `system.uptime`, `datetime.local`,
`layout.name`, `agent.active_count`, and `message.unread_count`. They may also
use active-pane fields such as `pane.index`, `pane.id`, and `pane.title`.

Pane templates support `session.id`, `window.id`, `window.index`, `pane.id`,
`pane.index`, `pane.title`, `pane.active`, `pane.size`, `pane.primary_pid`,
`pane.process_name`, `pane.exit_status`, `pane.pwd`, `pane.mode`, `agent.id`,
`agent.name`, `agent.status`, `agent.model`, `agent.reasoning`,
`agent.thinking`, `agent.routing`, `agent.latency`, `agent.preset`,
`agent.context_usage`, `policy.mode`, `observer.pending_count`, and
`history.position`.

### `theme`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `theme.active` | string | `"kanagawa"` | Active built-in or configured theme. |
| `theme.aliases.<alias>` | map value | see below | Alias to `#rgb` or `#rrggbb`. |
| `theme.colors.<slot>` | map value | see below | UI color slot set to a hex color or alias. |

Default aliases:

| Alias | Default declaration | Description |
| --- | --- | --- |
| `primary` | `"#7e9cd8"` | Primary accent. |
| `secondary` | `"#7aa89f"` | Secondary accent. |
| `tertiary` | `"#e6c384"` | Tertiary accent. |
| `thinking` | `"#938aa9"` | Muted agent thinking/status accent. |
| `danger` | `"#e82424"` | Destructive/error accent. |

Default color slots:

| Slot | Default declaration | Description |
| --- | --- | --- |
| `window_frame_fg` | `"primary"` | Window frame foreground. |
| `window_frame_bg` | `"#1f1f28"` | Window frame background. |
| `window_active_fg` | `"#1f1f28"` | Active window pill foreground. |
| `window_active_bg` | `"primary"` | Active window pill background. |
| `window_inactive_fg` | `"#dcd7ba"` | Inactive window pill foreground. |
| `window_inactive_bg` | `"secondary"` | Inactive window pill background. |
| `pane_frame_active_fg` | `"#dcd7ba"` | Active pane frame foreground. |
| `pane_frame_active_bg` | `"secondary"` | Active pane frame background. |
| `pane_frame_inactive_fg` | `"#727169"` | Inactive pane frame foreground. |
| `pane_frame_inactive_bg` | `"#1f1f28"` | Inactive pane frame background. |
| `pane_border_active_fg` | `"primary"` | Active pane border foreground. |
| `pane_border_active_bg` | `"#1f1f28"` | Active pane border background. |
| `pane_border_inactive_fg` | `"#727169"` | Inactive pane border foreground. |
| `pane_border_inactive_bg` | `"#1f1f28"` | Inactive pane border background. |
| `pane_divider_fg` | `"tertiary"` | Pane divider foreground. |
| `pane_divider_bg` | `"#1f1f28"` | Pane divider background. |
| `frame_fill_fg` | `"#dcd7ba"` | Frame fill foreground. |
| `frame_fill_bg` | `"#1f1f28"` | Frame fill background. |
| `scroll_indicator_fg` | `"#1f1f28"` | Scroll indicator foreground. |
| `scroll_indicator_bg` | `"tertiary"` | Scroll indicator background. |
| `pane_pwd_fg` | `"#1f1f28"` | Pane working-directory pill foreground. |
| `pane_pwd_bg` | `"#727169"` | Pane working-directory pill background. |
| `window_status_uptime_fg` | `"#1f1f28"` | Uptime status foreground. |
| `window_status_uptime_bg` | `"secondary"` | Uptime status background. |
| `window_status_datetime_fg` | `"#1f1f28"` | Date/time status foreground. |
| `window_status_datetime_bg` | `"tertiary"` | Date/time status background. |
| `prompt_fg` | `"primary"` | Command prompt foreground. |
| `prompt_bg` | `"#1f1f28"` | Command prompt background. |
| `agent_prompt_fg` | `"#ffffff"` | Agent prompt foreground. |
| `agent_prompt_bg` | `"#2a2a37"` | Agent prompt background. |
| `agent_transcript_user_fg` | `"primary"` | Agent transcript user foreground. |
| `agent_transcript_user_bg` | `"#1f1f28"` | Agent transcript user background. |
| `agent_transcript_assistant_fg` | `"secondary"` | Agent transcript assistant foreground. |
| `agent_transcript_assistant_bg` | `"#1f1f28"` | Agent transcript assistant background. |
| `agent_transcript_status_fg` | `"thinking"` | Agent status/thinking foreground. |
| `agent_transcript_status_bg` | `"#1f1f28"` | Agent status/thinking background. |
| `agent_transcript_error_fg` | `"danger"` | Agent error foreground. |
| `agent_transcript_error_bg` | `"#1f1f28"` | Agent error background. |
| `agent_transcript_command_fg` | `"tertiary"` | Agent command foreground. |
| `agent_transcript_command_bg` | `"#1f1f28"` | Agent command background. |
| `agent_model_fg` | `"#1f1f28"` | Agent model pill foreground. |
| `agent_model_bg` | `"secondary"` | Agent model pill background. |
| `agent_reasoning_fg` | `"#1f1f28"` | Agent reasoning pill foreground. |
| `agent_reasoning_bg` | `"tertiary"` | Agent reasoning pill background. |
| `agent_status_idle_fg` | `"#1f1f28"` | Idle agent status foreground. |
| `agent_status_idle_bg` | `"#727169"` | Idle agent status background. |
| `agent_status_running_fg` | `"#1f1f28"` | Running agent status foreground. |
| `agent_status_running_bg` | `"primary"` | Running agent status background. |
| `agent_status_blocked_fg` | `"#1f1f28"` | Blocked agent status foreground. |
| `agent_status_blocked_bg` | `"tertiary"` | Blocked agent status background. |
| `agent_status_failed_fg` | `"#1f1f28"` | Failed agent status foreground. |
| `agent_status_failed_bg` | `"danger"` | Failed agent status background. |
| `display_overlay_fg` | `"secondary"` | Display overlay foreground. |
| `display_overlay_bg` | `"#1f1f28"` | Display overlay background. |
| `copy_selection_fg` | `"#1f1f28"` | Copy selection foreground. |
| `copy_selection_bg` | `"tertiary"` | Copy selection background. |
| `syntax_plain_fg` | `"#dcd7ba"` | Plain syntax foreground. |
| `syntax_plain_bg` | `"#1f1f28"` | Plain syntax background. |
| `syntax_keyword_fg` | `"primary"` | Keyword syntax foreground. |
| `syntax_keyword_bg` | `"#1f1f28"` | Keyword syntax background. |
| `syntax_string_fg` | `"tertiary"` | String syntax foreground. |
| `syntax_string_bg` | `"#1f1f28"` | String syntax background. |
| `syntax_comment_fg` | `"thinking"` | Comment syntax foreground. |
| `syntax_comment_bg` | `"#1f1f28"` | Comment syntax background. |
| `syntax_type_fg` | `"secondary"` | Type syntax foreground. |
| `syntax_type_bg` | `"#1f1f28"` | Type syntax background. |
| `syntax_function_fg` | `"primary"` | Function syntax foreground. |
| `syntax_function_bg` | `"#1f1f28"` | Function syntax background. |
| `syntax_number_fg` | `"tertiary"` | Number syntax foreground. |
| `syntax_number_bg` | `"#1f1f28"` | Number syntax background. |
| `syntax_operator_fg` | `"#727169"` | Operator syntax foreground. |
| `syntax_operator_bg` | `"#1f1f28"` | Operator syntax background. |

### `themes.<name>`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `themes.<name>.aliases.<alias>` | string | omitted | Custom named theme alias. |
| `themes.<name>.colors.<slot>` | string | omitted | Custom named theme color slot. |

Custom named themes may omit aliases and slots. Omitted values inherit from the
documented built-in base for custom themes.

Built-in theme names include `deepforest`, `gruvbox_dark`, `gruvbox_light`,
`solarized_dark`, `solarized_light`, `monokai`, `dracula`, `nord`,
`tokyo_night`, `catppuccin_latte`, `catppuccin_frappe`,
`catppuccin_macchiato`, `catppuccin_mocha`, `one_half_dark`,
`one_half_light`, `onedark`, `rose_pine`, `rose_pine_moon`, `rose_pine_dawn`,
`kanagawa`, `everforest_dark`, `everforest_light`, `ayu`, `ayu_dark`,
`ayu_light`, `ayu_mirage`, `high_contrast_dark`, and `high_contrast_light`.

### `history`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `history.lines` | integer | `10000` | Maximum retained history lines per pane. |
| `history.rotate_lines` | integer | `1000` | Number of old lines to evict on overflow. |
| `history.saved_sessions_limit` | integer | `100` | Maximum saved agent conversations listed by `/resume`; older saved sessions are deleted when new conversations are created. |
| `history.persist` | boolean | `true` | Persist retained history across supported restarts. |
| `history.search_mode` | string | `"literal"` | Default history search mode. |

### `memory`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `memory.enabled` | boolean | `false` | Enable persistent memory commands, durable memory loading, and gated on-demand memory MAAP actions. |
| `memory.storage` | string | `"sqlite"` | Persistent memory storage backend. Current builds use SQLite with TSV import/export compatibility. |
| `memory.database_path` | string | `""` | Optional database path override; empty uses `<config_root>/memory.sqlite`. |
| `memory.max_records` | integer | `5000` | Retention cap for persistent records before archival or pruning. |
| `memory.max_bytes` | integer | `10485760` | Persistent memory content-byte cap enforced by `mez memory prune`. |
| `memory.max_injected_records` | integer | `12` | Maximum persistent memory records eligible for automatic context injection. |
| `memory.max_injected_bytes` | integer | `24576` | Maximum bytes of persistent memory eligible for automatic context injection. |
| `memory.candidate_limit` | integer | `100` | Maximum local candidates retrieved before sidecar selection. |
| `memory.fts_enabled` | boolean | `true` | Enable SQLite FTS candidate search for memory queries. |
| `memory.sidecar_model_profile` | string | `"memory-sidecar"` | Model profile name for memory sidecar planning and reranking calls. |
| `memory.sidecar_planning_timeout_ms` | integer | `1500` | Maximum sidecar query-planning time. |
| `memory.sidecar_rerank_timeout_ms` | integer | `1500` | Maximum sidecar reranking time. |
| `memory.sidecar_max_queries` | integer | `5` | Maximum sidecar-planned FTS queries per retrieval pass. |
| `memory.archive_before_prune` | boolean | `true` | Archive non-expired over-limit records before destructive pruning. |
| `memory.default_ttl_days` | integer | `180` | Default retention horizon for model-generated memory records when the model does not provide `expires_in_days`. Records store this as an expiration duration so selected-and-used memories refresh their expiry from wall-clock time. |

### `agents`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `agents.default_provider` | string | `"openai"` | Provider profile used by default. |
| `agents.default_model_profile` | string | `"default"` | Model profile used by default. |
| `agents.shell_only` | boolean | `true` | Require local system actions to go through the pane shell. |
| `agents.compaction_raw_retention_percent` | integer | `10` | Percent of raw context retained during manual compaction and provider context-limit recovery; 1 to 100. |
| `agents.routing` | boolean | `false` | Enable pane-local routing selection by default. |
| `agents.action_failure_retry_limit` | integer | `5` | Self-correction attempts per repeated correctable action failure signature other than `apply_patch`. |
| `agents.implementation_pressure_after_shell_actions` | integer | `5` | Successive shell-action count before adding an advisory take-action hint. |
| `agents.custom_system_prompt` | string | `""` | User-owned system prompt appended after built-in prompt content. |
| `agents.default_personality` | string | `""` | Default personality profile id; empty means none. |
| `agents.auto_sizing` | table | see below | Model auto-sizing settings. |
| `agents.subagent_placement` | string | `"new-window"` | Where root-spawned subagents are placed. |
| `agents.max_concurrent_agents` | integer | `4` | Global concurrent agent limit. |
| `agents.max_root_subagents` | integer | `4` | Maximum subagents a root agent may spawn. |
| `agents.max_subagents_per_subagent` | integer | `2` | Maximum child subagents for each subagent. |
| `agents.max_subagent_panes_per_window` | integer | `4` | Maximum subagent panes per window. |
| `agents.subagent_wait_policy` | string | `"join"` | Default wait behavior for spawned subagents. |
| `agents.max_depth` | integer | `2` | Maximum subagent tree depth. |
| `agents.prompt_profile` | string | `"default"` | Agent system prompt profile id. |
| `agents.default_agent_role` | string | `"default"` | Default subagent role/profile id. |

### `agents.auto_sizing`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `agents.auto_sizing.router_model_profile` | string | `"auto-size-router"` | Profile used to classify turn size. |
| `agents.auto_sizing.small_model_profile` | string | `"auto-size-small"` | Profile for small turns. |
| `agents.auto_sizing.medium_model_profile` | string | `"auto-size-medium"` | Profile for medium turns. |
| `agents.auto_sizing.large_model_profile` | string | `"auto-size-large"` | Profile for large turns. |
| `agents.auto_sizing.allowed_reasoning_efforts` | string array | `["low", "medium", "high", "xhigh"]` | Reasoning efforts the router may select. |
| `agents.auto_sizing.fallback_policy` | string | `"use-default-profile"` | Fallback when routing fails. |

### `providers.<name>`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `providers.<name>.kind` | string | `providers.openai.kind = "openai"` | Provider brand/default profile kind. Built-ins include `openai`, `deepseek`, and legacy `openai-compatible`. |
| `providers.<name>.api` | string | `providers.openai.api = "openai-responses"` | Wire API compatibility: `openai-responses`, `openai-chat-completions`, or `deepseek-chat-completions`. |
| `providers.<name>.auth_profile` | string | `providers.openai.auth_profile = "default"` | Auth profile id. |
| `providers.<name>.base_url` | string | `providers.openai.base_url = ""` | Optional API base URL. Empty uses provider default. |
| `providers.<name>.models` | string array | see below | Selectable model ids. Empty may use provider built-ins. |
| `providers.<name>.default_model` | string | `providers.openai.default_model = "gpt-5.5"` | Default model for the provider. |
| `providers.<name>.options` | table | `{}` | Provider-specific non-secret options. |
| `providers.openai.options.organization_id` | string | omitted | Optional OpenAI organization header for API-key requests. |
| `providers.openai.options.project_id` | string | omitted | Optional OpenAI project header for API-key requests. |

Default `providers.openai.models`:

```toml
["gpt-5.5", "gpt-5.4", "gpt-5.4-mini", "gpt-5.3-codex", "gpt-5.3-codex-spark", "gpt-5.2"]
```

Default `providers.deepseek.models`:

```toml
["deepseek-v4-pro", "deepseek-v4-flash"]
```

Provider `api` selects the reusable wire adapter independently from provider
brand/defaults. Use `openai-responses` for Responses-compatible backends,
`openai-chat-completions` for generic Chat Completions-compatible backends, and
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
`maap_surface` (`canonical_batch` or `content_json`). LM Studio-style model
catalog capability tags such as `tool_use` are retained in provider model
metadata and copied into runtime-generated profile options as
`model_capabilities`. By default Mezzanine sends the canonical
`submit_maap_action_batch` tool with string `tool_choice = "required"`; use
`tool_choice = "named"` only for backends that accept object-valued named tool
selection. Set `maap_output = "structured_json"` and
`structured_output = "json_schema"` for LM Studio/local models that obey JSON
Schema response formats more reliably than native OpenAI tool-call emission.

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
| `model_profiles.<name>.max_output_tokens` | integer | omitted | Optional provider output-token cap. |
| `model_profiles.<name>.provider_options` | table | see below | Provider-specific non-secret model options. |
| `model_profiles.<name>.safety_tier` | string | `"high"` in generated profiles | Safety posture label. |
| `model_profiles.<name>.privacy` | string | omitted | Compatibility privacy field. |
| `model_profiles.<name>.privacy_tier` | string | `"standard"` in generated profiles | Privacy posture label. |
| `model_profiles.<name>.residency` | string | `"global"` in generated profiles | Data residency label. |
| `model_profiles.<name>.approval` | string | omitted | Compatibility approval field. |
| `model_profiles.<name>.approval_policy` | string | `"ask"` in generated profiles | Approval policy for this profile: `ask`, `auto-allow`, or `full-access`. |
| `model_profiles.<name>.fallback_profiles` | string array | `[]` in generated profiles | Ordered fallback profile ids. |

Default model profiles:

| Profile | Field | Default declaration |
| --- | --- | --- |
| `default` | `provider` | `"openai"` |
| `default` | `model` | `"gpt-5.5"` |
| `default` | `reasoning_profile` | `"medium"` |
| `default` | `latency_preference` | `"default"` |
| `default` | `multimodal_required` | `false` |
| `default` | `context_window_tokens` | `1050000` |
| `default` | `safety_tier` | `"high"` |
| `default` | `privacy_tier` | `"standard"` |
| `default` | `residency` | `"global"` |
| `default` | `approval_policy` | `"ask"` |
| `default` | `fallback_profiles` | `[]` |
| `default.provider_options` | `reasoning_effort` | `"medium"` |
| `auto-size-router` | `provider` | `"openai"` |
| `auto-size-router` | `model` | `"gpt-5.4-mini"` |
| `auto-size-router` | `reasoning_profile` | `"low"` |
| `auto-size-router` | `latency_preference` | `"fast"` |
| `auto-size-router` | `multimodal_required` | `false` |
| `auto-size-router` | `context_window_tokens` | `400000` |
| `auto-size-router` | `safety_tier` | `"high"` |
| `auto-size-router` | `privacy_tier` | `"standard"` |
| `auto-size-router` | `residency` | `"global"` |
| `auto-size-router` | `approval_policy` | `"ask"` |
| `auto-size-router` | `fallback_profiles` | `[]` |
| `auto-size-router.provider_options` | `reasoning_effort` | `"low"` |
| `auto-size-small` | `provider` | `"openai"` |
| `auto-size-small` | `model` | `"gpt-5.3-codex"` |
| `auto-size-small` | `reasoning_profile` | `"medium"` |
| `auto-size-small` | `latency_preference` | `"fast"` |
| `auto-size-small` | `multimodal_required` | `false` |
| `auto-size-small` | `context_window_tokens` | `400000` |
| `auto-size-small` | `safety_tier` | `"high"` |
| `auto-size-small` | `privacy_tier` | `"standard"` |
| `auto-size-small` | `residency` | `"global"` |
| `auto-size-small` | `approval_policy` | `"ask"` |
| `auto-size-small` | `fallback_profiles` | `[]` |
| `auto-size-small.provider_options` | `reasoning_effort` | `"medium"` |
| `auto-size-medium` | `provider` | `"openai"` |
| `auto-size-medium` | `model` | `"gpt-5.4"` |
| `auto-size-medium` | `reasoning_profile` | `"medium"` |
| `auto-size-medium` | `latency_preference` | `"default"` |
| `auto-size-medium` | `multimodal_required` | `false` |
| `auto-size-medium` | `context_window_tokens` | `1050000` |
| `auto-size-medium` | `safety_tier` | `"high"` |
| `auto-size-medium` | `privacy_tier` | `"standard"` |
| `auto-size-medium` | `residency` | `"global"` |
| `auto-size-medium` | `approval_policy` | `"ask"` |
| `auto-size-medium` | `fallback_profiles` | `[]` |
| `auto-size-medium.provider_options` | `reasoning_effort` | `"medium"` |
| `auto-size-large` | `provider` | `"openai"` |
| `auto-size-large` | `model` | `"gpt-5.5"` |
| `auto-size-large` | `reasoning_profile` | `"high"` |
| `auto-size-large` | `latency_preference` | `"default"` |
| `auto-size-large` | `multimodal_required` | `false` |
| `auto-size-large` | `context_window_tokens` | `1050000` |
| `auto-size-large` | `safety_tier` | `"high"` |
| `auto-size-large` | `privacy_tier` | `"standard"` |
| `auto-size-large` | `residency` | `"global"` |
| `auto-size-large` | `approval_policy` | `"ask"` |
| `auto-size-large` | `fallback_profiles` | `[]` |
| `auto-size-large.provider_options` | `reasoning_effort` | `"high"` |
| `deepseek-default` | `provider` | `"deepseek"` |
| `deepseek-default` | `model` | `"deepseek-v4-pro"` |
| `deepseek-default` | `reasoning_profile` | `"high"` |
| `deepseek-default` | `latency_preference` | `"default"` |
| `deepseek-default` | `multimodal_required` | `false` |
| `deepseek-default` | `context_window_tokens` | `1000000` |
| `deepseek-default` | `safety_tier` | `"high"` |
| `deepseek-default` | `privacy_tier` | `"standard"` |
| `deepseek-default` | `residency` | `"global"` |
| `deepseek-default` | `approval_policy` | `"ask"` |
| `deepseek-default` | `fallback_profiles` | `[]` |
| `deepseek-default.provider_options` | `thinking` | `"enabled"` |
| `deepseek-default.provider_options` | `reasoning_effort` | `"high"` |
| `deepseek-fast` | `provider` | `"deepseek"` |
| `deepseek-fast` | `model` | `"deepseek-v4-flash"` |
| `deepseek-fast` | `reasoning_profile` | `"high"` |
| `deepseek-fast` | `latency_preference` | `"fast"` |
| `deepseek-fast` | `multimodal_required` | `false` |
| `deepseek-fast` | `context_window_tokens` | `1000000` |
| `deepseek-fast` | `safety_tier` | `"high"` |
| `deepseek-fast` | `privacy_tier` | `"standard"` |
| `deepseek-fast` | `residency` | `"global"` |
| `deepseek-fast` | `approval_policy` | `"ask"` |
| `deepseek-fast` | `fallback_profiles` | `[]` |
| `deepseek-fast.provider_options` | `thinking` | `"enabled"` |
| `deepseek-fast.provider_options` | `reasoning_effort` | `"high"` |

Provider options under a model profile:

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `model_profiles.<name>.provider_options.reasoning_effort` | string | profile-specific | Reasoning effort sent to the provider. |
| `model_profiles.<name>.provider_options.thinking` | string | `"enabled"` for generated DeepSeek profiles | DeepSeek thinking mode override: `enabled` or `disabled`. |
| `model_profiles.<name>.provider_options.prompt_cache_retention` | string | omitted | Optional OpenAI cache retention: `in_memory` or `24h` when supported. |

### `permissions`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `permissions.approval_policy` | string | `"ask"` | Default approval policy: `ask`, `auto-allow`, or `full-access`. |
| `permissions.preset` | string | omitted | Optional preset, such as `read-only` or `auto`. |
| `permissions.trusted_directories` | string array | `[]` | Trusted directory roots. |
| `permissions.trusted_projects` | string array | `[]` | Trusted project roots. |
| `permissions.command_rules` | array | `[]` | User/project command rule entries. |
| `permissions.session_command_rules` | array | `[]` | Session-scoped command rule entries. |
| `permissions.global_command_rules` | array | `[]` | Global command rule entries. |
| `permissions.network_policy` | string | `"prompt"` | Network action policy. |
| `permissions.destructive_action_policy` | string | `"prompt"` | Destructive action policy. |
| `permissions.bypass_mode` | boolean | `false` | Explicit bypass state; cannot be enabled from config. |

Command rule fields for each entry in a command rule array:

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
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
| `subagents.<name>.default_read_scopes` | string array | omitted | Default readable scopes. |
| `subagents.<name>.default_write_scopes` | string array | omitted | Default writable scopes. |

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

### `message_protocol`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `message_protocol.enabled` | boolean | `true` | Enable local agent message protocol. |
| `message_protocol.endpoint` | string | `"local"` | Endpoint mode. |
| `message_protocol.retention_messages` | integer | `1000` | Maximum retained local messages. |
| `message_protocol.retention_bytes` | integer | `1048576` | Maximum retained message bytes. |
| `message_protocol.allow_remote_bridges` | boolean | `false` | Permit configured remote bridges. |

### `control`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `control.endpoint` | string | `"unix"` | Control endpoint kind. |
| `control.socket_path` | string | `""` | Explicit Unix socket path; empty uses runtime default. |
| `control.tcp_bind` | string | `"127.0.0.1:0"` | TCP bind address when TCP is enabled. |
| `control.tcp_enabled` | boolean | `false` | Enable TCP control endpoint. |
| `control.auth_token_file` | string | `""` | Auth token file for control access. |
| `control.observer_policy` | string | `"primary-approval"` | Observer request approval policy. |

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
| `mcp_servers.<name>.external_capability` | string or table | omitted | Declared external capability metadata. |

For streamable HTTP servers, `mez mcp login <name>` stores OAuth tokens in the
auth credential store rather than in `mcp_servers`. Login uses browser
authorization-code PKCE. When authorization-server metadata advertises an RFC
7591 dynamic client registration endpoint and no `--client-id` is provided,
Mezzanine registers a public native client for the localhost callback and keeps
only the returned non-secret client id in MCP auth metadata for refresh. A
configured `bearer_token_env` remains the highest-precedence bearer credential
source for that server.

### `auth`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `auth.auth_file` | string | `"auth.toml"` | Non-secret auth metadata file name. |
| `auth.credential_store` | string | `"auto"` | Credential backend selection. |
| `auth.default_profile` | string | `"default"` | Default auth profile id. |

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

### `snapshots`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `snapshots.enabled` | boolean | `true` | Enable snapshot support. |
| `snapshots.path` | string | `"snapshots"` | Snapshot storage path under config root. |
| `snapshots.on_detach` | boolean | `false` | Snapshot when the primary detaches. |
| `snapshots.on_interval_seconds` | integer | `0` | Periodic snapshot interval; 0 disables. |
| `snapshots.on_agent_turn` | boolean | `false` | Snapshot around agent turns. |
| `snapshots.retention_count` | integer | `10` | Number of snapshots to retain. |

### `audit`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `audit.enabled` | boolean | `true` | Enable audit logging. |
| `audit.path` | string | `"audit.jsonl"` | Audit log path under config root. |
| `audit.format` | string | `"jsonl"` | Audit log format. |
| `audit.retention_days` | integer | `30` | Audit retention period. |
| `audit.redact_secrets` | boolean | `true` | Redact detected secrets in audit records. |
| `audit.hash_chain` | boolean | `false` | Enable hash chaining of audit records. |
| `audit.required` | boolean | `false` | Require audit logging for sensitive operations. |

### `extensions.<name>`

| Field | Type | Default declaration | Description |
| --- | --- | --- | --- |
| `extensions.<name>.*` | implementation-defined | omitted | Extension-specific config. Unknown non-extension top-level keys are rejected. |
