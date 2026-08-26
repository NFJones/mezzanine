//! Config Defaults implementation.
//!
//! This module owns the config defaults boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

// Generated default configuration.

/// Target platform used to select first-run permission defaults.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GeneratedConfigPlatform {
    /// Linux, where Bubblewrap can provide operating-system confinement.
    Linux {
        /// Whether the code-owned Bubblewrap executable is available.
        bubblewrap_available: bool,
    },
    /// macOS, where first-run actions use model-gated automatic approval.
    MacOs,
    /// Any other supported platform without a native sandbox backend.
    Other,
}

impl GeneratedConfigPlatform {
    /// Returns the platform targeted by the current build.
    fn current() -> Self {
        if cfg!(target_os = "linux") {
            Self::Linux {
                bubblewrap_available: crate::security::sandbox::bubblewrap_executable_available(
                    std::path::Path::new("/usr/bin/bwrap"),
                ),
            }
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Other
        }
    }
}

/// Returns the initial primary configuration without provider-specific entries.
///
/// Provider connection, model-profile, and preset defaults are materialized
/// only after their provider has authenticated successfully. Keeping the
/// catalog here lets the first-run configuration remain compact while one
/// source still defines the provider defaults copied after authentication.
pub(crate) fn initial_config_toml() -> crate::error::Result<String> {
    initial_config_toml_for_platform(GeneratedConfigPlatform::current())
}

/// Returns the initial primary configuration for one target platform.
///
/// Keeping platform selection injectable lets non-macOS builders verify the
/// generated macOS security posture without changing explicit user settings.
pub(crate) fn initial_config_toml_for_platform(
    platform: GeneratedConfigPlatform,
) -> crate::error::Result<String> {
    let mut document = DEFAULT_CONFIG_TOML
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| {
            crate::error::MezError::config(format!("invalid built-in TOML config: {error}"))
        })?;
    let root = document.as_table_mut();
    root.remove("providers");
    root.remove("model_profiles");
    root.remove("model_presets");
    let auto_sizing = root
        .get_mut("agents")
        .and_then(toml_edit::Item::as_table_mut)
        .and_then(|agents| agents.get_mut("auto_sizing"))
        .and_then(toml_edit::Item::as_table_mut)
        .ok_or_else(|| {
            crate::error::MezError::config(
                "built-in default config is missing `agents.auto_sizing` table",
            )
        })?;
    for key in [
        "router_model_profile",
        "small_model_profile",
        "medium_model_profile",
        "large_model_profile",
    ] {
        auto_sizing.insert(key, toml_edit::value("default"));
    }
    let permissions = root
        .get_mut("permissions")
        .and_then(toml_edit::Item::as_table_mut)
        .ok_or_else(|| {
            crate::error::MezError::config("built-in default config is missing `permissions` table")
        })?;
    let approval_policy = permissions
        .get_mut("approval_policy")
        .and_then(toml_edit::Item::as_value_mut)
        .ok_or_else(|| {
            crate::error::MezError::config(
                "built-in default config is missing `permissions.approval_policy`",
            )
        })?;
    *approval_policy = toml_edit::Value::from(match platform {
        GeneratedConfigPlatform::MacOs => "auto-allow",
        GeneratedConfigPlatform::Linux {
            bubblewrap_available: true,
        } => "full-access",
        GeneratedConfigPlatform::Linux {
            bubblewrap_available: false,
        } => "auto-allow",
        GeneratedConfigPlatform::Other => "ask",
    });
    let sandbox = permissions
        .get_mut("sandbox")
        .and_then(toml_edit::Item::as_value_mut)
        .ok_or_else(|| {
            crate::error::MezError::config(
                "built-in default config is missing `permissions.sandbox`",
            )
        })?;
    *sandbox = toml_edit::Value::from(match platform {
        GeneratedConfigPlatform::Linux {
            bubblewrap_available: true,
        } => "bubblewrap",
        GeneratedConfigPlatform::Linux {
            bubblewrap_available: false,
        } => "policy-only",
        GeneratedConfigPlatform::MacOs | GeneratedConfigPlatform::Other => "policy-only",
    });
    Ok(document.to_string())
}

/// Returns the catalog defaults that belong to one supported provider.
///
/// Unknown providers deliberately have no generated configuration because Mez
/// cannot safely infer their API or model catalog from authentication metadata.
pub(crate) fn provider_default_config_toml(provider: &str) -> crate::error::Result<Option<String>> {
    let profiles = match provider {
        "openai" => &[
            "default",
            "auto-size-router",
            "auto-size-small",
            "auto-size-medium",
            "auto-size-large",
        ][..],
        "anthropic" => &["anthropic-default", "anthropic-fast"][..],
        "deepseek" => &["deepseek-default", "deepseek-fast"][..],
        _ => return Ok(None),
    };
    let mut document = DEFAULT_CONFIG_TOML
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| {
            crate::error::MezError::config(format!("invalid built-in TOML config: {error}"))
        })?;
    let root = document.as_table_mut();
    let section_names = root
        .iter()
        .map(|(name, _)| name.to_string())
        .collect::<Vec<_>>();
    for name in section_names {
        if !matches!(
            name.as_str(),
            "providers" | "model_profiles" | "model_presets"
        ) {
            root.remove(&name);
        }
    }
    retain_named_tables(root, "providers", &[provider])?;
    retain_named_tables(root, "model_profiles", profiles)?;
    retain_named_tables(root, "model_presets", &[provider])?;

    Ok(Some(document.to_string()))
}

/// Retains only selected named child tables within one catalog section.
fn retain_named_tables(
    root: &mut toml_edit::Table,
    section: &str,
    selected: &[&str],
) -> crate::error::Result<()> {
    let table = root
        .get_mut(section)
        .and_then(toml_edit::Item::as_table_mut)
        .ok_or_else(|| {
            crate::error::MezError::config(format!(
                "built-in default config is missing `{section}` table"
            ))
        })?;
    let names = table
        .iter()
        .map(|(name, _)| name.to_string())
        .collect::<Vec<_>>();
    for name in names {
        if !selected.contains(&name.as_str()) {
            table.remove(&name);
        }
    }
    Ok(())
}

/// Defines the DEFAULT CONFIG TOML const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const DEFAULT_CONFIG_TOML: &str = r##"# Mezzanine default configuration.
# Active settings below materialize Mezzanine's built-in defaults. Optional
# settings and dynamic-map examples remain commented so the first launch is
# behaviorally unchanged while the complete configuration surface is visible.
# Provider connections, model profiles, and provider presets are intentionally
# absent from first-launch output; `mez auth login` adds those after login.
# Schema version used for migrations. Change only through a supported migration.
version = 74

# Persistent multi-session host policy. The host and inbound Iroh listener are
# disabled until explicitly started or enabled by the primary user.
[host]
enabled = false
auto_start_local = true
max_sessions = 64
max_live_sessions = 16
shutdown_timeout_ms = 10000
checkpoint_interval_seconds = 300
recover_on_start = "lazy"
default_session_policy = "most_recent_attachable"

[host.leases]
default_ttl_seconds = 0
failed_retention_seconds = 604800
released_retention_seconds = 604800
max_per_remote_client = 8

# Process-wide runtime sizing. Changes require a restart.
[runtime]
# Tokio worker threads available to the daemon and foreground services.
cpu_count = 2

# Remote client/server transport. Unix sockets remain the default and local
# recovery path. Every network behavior is independently opt-in and changes
# require a daemon restart.
[transport.iroh]
enabled = false
outbound_enabled = true
bind_port = 0
identity = "host"
address_lookup = "disabled"
address_lookup_domain = ""
relay_mode = "disabled"
relay_urls = []
direct_connections = true
port_mapping = false
proxy_from_env = false
system_ca_store = false
invitation_ttl_seconds = 600
max_connections = 16
max_streams_per_connection = 1
setup_timeout_ms = 10000
idle_timeout_ms = 300000
compression_codecs = ["zstd", "lz4", "none"]
compression_min_bytes = 512
compression_zstd_level = 3

# Terminal emulation, pane startup, clipboard, rendering, and cursor behavior.
[terminal]
# Terminal compatibility profile advertised to pane applications.
profile = "xterm-compatible"
# TERM value exported to pane processes.
term = "xterm-256color"
# New panes start in the user home directory; use "same-directory" to inherit.
pane_spawn_directory = "home"
# New panes open a shell; use "agent" to open the agent view.
pane_spawn_view = "shell"
# Advertise and render 24-bit color.
true_color = true
# Forward mouse input using terminal mouse protocols.
mouse = true
# Preserve bracketed-paste boundaries for applications.
bracketed_paste = true
# OSC 52 writes: "external" stores internally and attempts a host copy;
# "internal" stores only in the internal buffer; "disabled" rejects writes.
clipboard = "external"
# Optional explicit host clipboard commands. Copy commands receive content on
# stdin; paste commands return clipboard text on stdout.
# clipboard_copy_command = "xclip -selection clipboard"
# clipboard_paste_command = "xclip -selection clipboard -out"
# Maximum time to wait for a host clipboard read.
clipboard_read_timeout_ms = 250
# Maximum bytes accepted from a host clipboard read.
clipboard_read_max_bytes = 1048576
# Support the terminal alternate-screen buffer.
alternate_screen = true
# Forward terminal focus-in and focus-out events.
focus_events = true
# Detect nested multiplexers automatically; alternatives are "enabled" and "disabled".
nested_multiplexer = "auto"
# Keep terminal escape sequences contained instead of passing them through.
passthrough = false
# Treat emoji/status glyphs as wide; use "narrow" for one-column terminals.
emoji_width = "wide"
# Disable optional motion and animation when true.
reduced_motion = false
# Render provider output incrementally while a validated response is still arriving.
streaming_output = true
# Opt in to enhanced keyboard reporting only while a Mez readline prompt owns input.
enhanced_keyboard_reporting = false
# Whether completion-attention title pills alternate their attention color.
completion_attention_flashing = true
# Coalesce terminal resize events for this many milliseconds.
resize_debounce_ms = 200
# Maximum attached-terminal redraw rate.
render_rate_limit_fps = 30
# Hidden shell-output previews retain this many visual rows.
shell_output_preview_lines = 5
# Maximum display width for Mezzanine-owned agent rows.
agent_wrap_column_cap = 120
# Cursor shape: "block", "underline", or "bar".
cursor_style = "block"
# Keep the terminal cursor steady by default.
cursor_blink = false
# Cursor blink period when blinking is enabled.
cursor_blink_interval_ms = 500

# `escape` is the prefix key. Optional direct key settings below cover actions
# without generated default prefix equivalents.
[keys]
escape = "C-a"
# Direct bindings are disabled by default; uncomment any desired chord.
# split_vertical = "A-\\"
# split_horizontal = "A--"
# new_window = "A-="
# new_group = "A-S-="
# agent_shell = "A-]"
# focus_up = "C-A-Up"
# focus_down = "C-A-Down"
# focus_left = "C-A-Left"
# focus_right = "C-A-Right"
# focus_previous_window = "C-A-PageUp"
# focus_next_window = "C-A-PageDown"
# focus_previous_group = "C-A-S-PageUp"
# focus_next_group = "C-A-S-PageDown"

# Map arbitrary key chords to command-prompt command sequences.
[keys.command_bindings]
# "A-t" = "new-window"

# Select a built-in or user-defined key preset.
[key_preset]
active = "default"

# Optional custom presets inherit omitted bindings from the default preset.
# [key_presets.custom]
# escape = "C-a"
# split_vertical = "A-\\"
# split_horizontal = "A--"
# new_window = "A-="
# new_group = "A-S-="
# agent_shell = "A-]"
# focus_up = "C-A-Up"
# focus_down = "C-A-Down"
# focus_left = "C-A-Left"
# focus_right = "C-A-Right"
# focus_previous_window = "C-A-PageUp"
# focus_next_window = "C-A-PageDown"
# focus_previous_group = "C-A-S-PageUp"
# focus_next_group = "C-A-S-PageDown"
# [key_presets.custom.command_bindings]
# "A-t" = "new-window"

# Window status frame shown at the bottom of the terminal.
[frames.window]
enabled = true
position = "bottom"
template = "#{window.list}"
right_status = "#{iroh.status} #{pane.pwd} #{button:-|terminal|split-window -h} #{button:+|terminal|split-window} #{button:□|terminal|new-window} #{button:⊕|terminal|new-group} #{button:λ|terminal|agent-shell} #{system.uptime} #{datetime.local}"
style = "default"
visible_fields = ["window.list", "window.index", "window.name", "window.id", "pane.index", "pane.title", "pane.id", "window.pane_count", "window.buttons", "pane.pwd", "system.uptime", "datetime.local", "iroh.status"]

[frames.window.pills]
# Optional asynchronously refreshed right-status pill.
# [frames.window.pills.example]
# label = "Build"
# command = "git status --short"
# interval_seconds = 30
# initial = "checking"
# timeout_ms = 2000
# empty_behavior = "hide"
# error_behavior = "show_error"
# max_output_chars = 80
# style = "default"

# Pane title frame rendered on each pane border.
[frames.pane]
# Toggle pane frames and choose their placement, content, and style.
enabled = true
position = "border"
template = " #{pane.index} #{pane.title} "
style = "default"
visible_fields = ["pane.index", "pane.title", "pane.id", "pane.status", "history.position", "agent.model", "agent.reasoning", "agent.thinking", "agent.planning", "agent.routing", "agent.latency", "agent.preset", "agent.name", "policy.mode", "agent.context_usage", "agent.status"]

# Select the active built-in or custom theme.
[theme]
active = "acid_lime"

# Reusable color aliases referenced by concrete UI color slots.
[theme.aliases]
primary = "#bfff00"
secondary = "#7fbf3f"
tertiary = "#d7ff5f"
thinking = "#c9d89a"
danger = "#ff5c57"
foreground = "#eef7d0"
muted = "#6f7f3c"
surface = "#1b1f0a"
danger_foreground = "#ff7b74"
danger_text = "#140200"
muted_text = "#0f1206"
primary_foreground = "#d8ff5a"
primary_text = "#111400"
secondary_foreground = "#a8e85a"
secondary_text = "#111400"
tertiary_foreground = "#e6ff8a"
tertiary_text = "#111400"

[theme.colors]
window_frame_fg = "primary_foreground"
window_frame_bg = "surface"
window_active_fg = "primary_text"
window_active_bg = "primary"
window_inactive_fg = "secondary_text"
window_inactive_bg = "secondary"
pane_frame_active_fg = "secondary_text"
pane_frame_active_bg = "secondary"
pane_frame_inactive_fg = "muted"
pane_frame_inactive_bg = "surface"
pane_border_active_fg = "primary_foreground"
pane_border_active_bg = "surface"
pane_border_inactive_fg = "muted"
pane_border_inactive_bg = "surface"
pane_divider_fg = "tertiary_foreground"
pane_divider_bg = "surface"
frame_fill_fg = "foreground"
frame_fill_bg = "surface"
scroll_indicator_fg = "tertiary_text"
scroll_indicator_bg = "tertiary"
pane_progress_fg = "tertiary_text"
pane_progress_bg = "tertiary"
pane_pwd_fg = "muted_text"
pane_pwd_bg = "muted"
window_status_uptime_fg = "secondary_text"
window_status_uptime_bg = "secondary"
window_status_datetime_fg = "tertiary_text"
window_status_datetime_bg = "tertiary"
iroh_status_good_fg = "primary_text"
iroh_status_good_bg = "primary"
iroh_status_degraded_fg = "tertiary_text"
iroh_status_degraded_bg = "tertiary"
iroh_status_poor_fg = "danger_text"
iroh_status_poor_bg = "danger"
iroh_status_unknown_fg = "muted_text"
iroh_status_unknown_bg = "muted"
prompt_fg = "primary_foreground"
prompt_bg = "surface"
agent_prompt_fg = "#f8ffe0"
agent_prompt_bg = "#20250c"
agent_transcript_user_fg = "primary_foreground"
agent_transcript_user_bg = "surface"
agent_transcript_assistant_fg = "secondary_foreground"
agent_transcript_assistant_bg = "surface"
agent_transcript_status_fg = "thinking"
agent_transcript_status_bg = "surface"
agent_transcript_error_fg = "danger_foreground"
agent_transcript_error_bg = "surface"
agent_transcript_command_fg = "tertiary_foreground"
agent_transcript_command_bg = "surface"
agent_model_fg = "secondary_text"
agent_model_bg = "secondary"
agent_reasoning_fg = "tertiary_text"
agent_reasoning_bg = "tertiary"
agent_status_idle_fg = "muted_text"
agent_status_idle_bg = "muted"
agent_status_running_fg = "primary_text"
agent_status_running_bg = "primary"
agent_status_blocked_fg = "tertiary_text"
agent_status_blocked_bg = "tertiary"
agent_approval_attention_fg = "danger_text"
agent_approval_attention_bg = "danger"
agent_status_failed_fg = "danger_text"
agent_status_failed_bg = "danger"
display_overlay_fg = "secondary_foreground"
display_overlay_bg = "surface"
copy_selection_fg = "tertiary_text"
copy_selection_bg = "tertiary"
syntax_plain_fg = "foreground"
syntax_plain_bg = "surface"
syntax_keyword_fg = "primary_foreground"
syntax_keyword_bg = "surface"
syntax_string_fg = "tertiary_foreground"
syntax_string_bg = "surface"
syntax_comment_fg = "thinking"
syntax_comment_bg = "surface"
syntax_type_fg = "secondary_foreground"
syntax_type_bg = "surface"
syntax_function_fg = "primary_foreground"
syntax_function_bg = "surface"
syntax_number_fg = "tertiary_foreground"
syntax_number_bg = "surface"
syntax_operator_fg = "muted"
syntax_operator_bg = "surface"

# Optional custom themes inherit omitted aliases and slots from deepforest.
# [themes.custom.aliases]
# primary = "#88ccff"
# secondary = "#6699cc"
# [themes.custom.colors]
# window_active_bg = "primary"
# pane_border_active_fg = "secondary"

# Terminal scrollback, rotation, saved-agent-session retention, and persistence.
[history]
# Maximum retained scrollback lines per terminal.
lines = 10000
# Number of oldest lines removed when history exceeds its limit.
rotate_lines = 1000
# Maximum saved agent conversations retained on disk.
saved_sessions_limit = 100
# Persist history and saved conversations across launches.
persist = true

# Durable model-facing memory storage and pruning limits.
[memory]
# Enable the memory store and memory actions.
enabled = true
# Maximum records retained before pruning.
max_records = 5000
# Maximum aggregate stored bytes before pruning.
max_bytes = 10485760
# Maintain the full-text search index.
fts_enabled = true
# Archive records before pruning them from the active store.
archive_before_prune = true
# Default lifetime for records without an explicit retention period.
default_ttl_days = 180

# Local project issue/task tracking.
[issues]
enabled = true
# Empty uses the default database beneath the Mezzanine config directory.
database_path = ""

# Global agent defaults, limits, routing, and subagent scheduling.
[agents]
# Provider/profile names resolve to runtime fallbacks until auth login adds a catalog.
default_provider = "openai"
default_model_profile = "default"
# Disabled by default. `system` inhibits automatic idle system sleep only while
# a canonical agent turn is Running; `system-and-display` also requests display
# wakefulness where supported. Detached sessions retain the request while work
# runs. Backends are best-effort and never override explicit sleep, lid-close,
# thermal, or critical-battery safeguards.
active_turn_sleep_inhibition = "disabled"
# Restrict agent terminal work to the shell-mediated action surface.
shell_only = true
# Percentage of the raw conversation tail retained after compaction.
compaction_raw_retention_percent = 10
# Disable automatic model sizing until explicitly enabled.
routing = false
# Maximum recovery attempts after action execution failures.
action_failure_retry_limit = 5
# Total wall-clock budget snapshotted for each new agent turn.
turn_timeout_ms = 1800000
# Agent shell command execution mode. `native` spawns a fresh shell process
# inferred from the pane root process without touching the pane PTY; `pane`
# executes commands through the pane shell.
shell_mode = "native"
# Default bounded iteration count used by /loop.
loop_limit = 8
# User-owned system prompt text appended to the built-in prompt.
custom_system_prompt = ""
# Empty selects no personality profile.
default_personality = ""
# MCP server ids exposed on every applicable model turn.
always_exposed_mcp_servers = []
# Place spawned subagents in a new window by default.
subagent_placement = "new-window"
# Global running-agent and scheduler queue limits.
max_concurrent_agents = 4
max_queued_turns = 256
max_queued_bytes = 4194304
# Per-tree subagent fan-out, pane, wait, and depth limits.
max_root_subagents = 4
max_subagents_per_subagent = 2
max_subagent_panes_per_window = 4
subagent_wait_policy = "join"
max_depth = 2

# Model profiles are materialized by authentication/catalog setup. When a
# profile sets max_input_tokens, Mez treats it as a hard estimated cap on the
# complete wire request and proactively compacts before provider I/O.

# Automatic model-size routing. First launch uses the synthesized default profile;
# auth login replaces these references with provider-specific profiles.
[agents.auto_sizing]
root_routing_policy = "subagent"
router_model_profile = "auto-size-router"
small_model_profile = "auto-size-small"
medium_model_profile = "auto-size-medium"
large_model_profile = "auto-size-large"
allowed_reasoning_efforts = ["low", "medium", "high", "xhigh"]
fallback_policy = "use-default-profile"

# Named subagent profiles. Built-in roles remain available when this table is empty.
[subagents]
# [subagents.reviewer]
# name = "Reviewer"
# description = "Reviews changes without modifying files."
# developer_instructions = "Focus on correctness, regressions, and missing tests."
# model_profile = "default"
# permission_preset = "read-only"
# mcp_servers = []
# default_cooperation_mode = "explore-only"
# default_read_scopes = ["."]
# default_write_scopes = []
# [subagents.reviewer.shell_env]
# REVIEW_MODE = "strict"

# Optional named prompt/style profiles are selectable by agents.default_personality.
# [personalities.concise]
# name = "Concise"
# system_prompt = "Prefer direct answers and compact status reports."
# response_style = "concise"
# model_profile = "default"
# planning_enabled = false
# routing_enabled = false

[personalities]

[providers.openai]
kind = "openai"
api = "openai-responses"
auth_profile = "default"
# Optional API base URL, such as "https://api.openai.com/v1".
# Mezzanine derives /responses and /models endpoints from this base.
# Compatible local APIs may omit stored auth; Mezzanine then sends no
# Authorization header instead of requiring a placeholder key.
base_url = ""
# OpenAI model IDs supported by the default coding-agent harness profile.
models = [
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
    "gpt-5.5",
    "gpt-5.4",
    "gpt-5.4-mini",
]
default_model = "gpt-5.6-terra"

[providers.openai.options]
# Optional documented OpenAI routing headers for multi-organization/project API keys.
# organization_id = "org_..."
# project_id = "proj_..."

[providers.anthropic]
kind = "anthropic"
api = "anthropic-messages"
auth_profile = "default"
# Optional API base URL, such as "https://api.anthropic.com/v1".
# Mezzanine derives the Anthropic Messages endpoint from this base.
base_url = ""
# Anthropic model IDs supported by the built-in Claude provider defaults.
models = [
    "claude-fable-5",
    "claude-opus-5",
    "claude-sonnet-5",
    "claude-haiku-4-5-20251001",
]
default_model = "claude-sonnet-5"

[providers.anthropic.options]
# anthropic_version = "2023-06-01"
# default_max_tokens = 4096

# Example local OpenAI-compatible Chat Completions backend, such as LM Studio.
# Uncomment and select this provider from a model profile when a local server is
# listening on the configured base URL. Missing stored auth metadata is allowed;
# Mezzanine sends no Authorization header in that case.
# [providers.lmstudio]
# kind = "openai-compatible"
# api = "openai-chat-completions"
# auth_profile = "default"
# base_url = "http://localhost:1234/v1"
# models = ["local-model"]
# default_model = "local-model"
#
# [providers.lmstudio.options]
# maap_output = "structured_json"
# structured_output = "json_schema"
# tool_calls = "auto"
# tool_choice = "required" # only used when maap_output selects native tools
# parallel_tool_calls = "disabled"
# output_token_field = "max_tokens"
# maap_surface = "canonical_batch"
#
# [model_profiles.local-lmstudio]
# provider = "lmstudio"
# model = "local-model"
# reasoning_profile = "medium"
# latency_preference = "default"
# multimodal_required = false
# context_window_tokens = 32768
# safety_tier = "basic"
# privacy_tier = "local"
# residency = "local"
# approval_policy = "ask"
# fallback_profiles = []

[providers.deepseek]
kind = "deepseek"
api = "deepseek-chat-completions"
auth_profile = "default"
# Optional API base URL, such as "https://api.deepseek.com".
# Mezzanine derives /chat/completions and /models endpoints from this base.
base_url = ""
# DeepSeek model IDs supported by the default coding-agent harness profile.
models = [
    "deepseek-v4-pro",
    "deepseek-v4-flash",
]
default_model = "deepseek-v4-pro"

[model_profiles.anthropic-default]
provider = "anthropic"
model = "claude-sonnet-5"
reasoning_profile = "high"
latency_preference = "default"
multimodal_required = false
context_window_tokens = 1000000
max_output_tokens = 128000
safety_tier = "high"
privacy_tier = "standard"
residency = "global"
approval_policy = "ask"
fallback_profiles = []

[model_profiles.anthropic-default.provider_options]
prompt_caching = "enabled"

[model_profiles.anthropic-fast]
provider = "anthropic"
model = "claude-haiku-4-5-20251001"
latency_preference = "fast"
multimodal_required = false
context_window_tokens = 200000
max_output_tokens = 64000
safety_tier = "high"
privacy_tier = "standard"
residency = "global"
approval_policy = "ask"
fallback_profiles = []

[model_profiles.anthropic-fast.provider_options]
prompt_caching = "enabled"

[model_profiles.default]
provider = "openai"
model = "gpt-5.6-terra"
reasoning_profile = "high"
latency_preference = "default"
multimodal_required = false
context_window_tokens = 1050000
# Responses input must leave provider-reserved output, reasoning, and framing capacity.
max_input_tokens = 922000
# Provider-aware recommended output-token cap for the default OpenAI agent profile.
# Mezzanine may temporarily raise this for one output-limit recovery retry.
max_output_tokens = 16384
safety_tier = "high"
privacy_tier = "standard"
residency = "global"
approval_policy = "ask"
fallback_profiles = []

[model_profiles.default.provider_options]
# For OpenAI-compatible Chat Completions backends that support the modern
# developer role, set developer_role = "developer". It defaults to
# "system" for older compatible servers.
# developer_role = "developer"

[model_profiles.auto-size-router]
provider = "openai"
model = "gpt-5.6-luna"
reasoning_profile = "low"
latency_preference = "fast"
multimodal_required = false
context_window_tokens = 400000
max_output_tokens = 8192
safety_tier = "high"
privacy_tier = "standard"
residency = "global"
approval_policy = "ask"
fallback_profiles = []

[model_profiles.auto-size-small]
provider = "openai"
model = "gpt-5.6-luna"
reasoning_profile = "medium"
latency_preference = "fast"
multimodal_required = false
context_window_tokens = 400000
max_output_tokens = 16384
safety_tier = "high"
privacy_tier = "standard"
residency = "global"
approval_policy = "ask"
fallback_profiles = []

[model_profiles.auto-size-medium]
provider = "openai"
model = "gpt-5.6-terra"
reasoning_profile = "medium"
latency_preference = "default"
multimodal_required = false
context_window_tokens = 1050000
max_output_tokens = 16384
safety_tier = "high"
privacy_tier = "standard"
residency = "global"
approval_policy = "ask"
fallback_profiles = []

[model_profiles.auto-size-large]
provider = "openai"
model = "gpt-5.6-sol"
reasoning_profile = "high"
latency_preference = "default"
multimodal_required = false
context_window_tokens = 1050000
max_output_tokens = 32768
safety_tier = "high"
privacy_tier = "standard"
residency = "global"
approval_policy = "ask"
fallback_profiles = []

[model_profiles.deepseek-default]
provider = "deepseek"
model = "deepseek-v4-pro"
reasoning_profile = "high"
latency_preference = "default"
multimodal_required = false
context_window_tokens = 1000000
max_output_tokens = 32768
safety_tier = "high"
privacy_tier = "standard"
residency = "global"
approval_policy = "ask"
fallback_profiles = []

[model_profiles.deepseek-default.provider_options]
thinking = "enabled"

[model_profiles.deepseek-fast]
provider = "deepseek"
model = "deepseek-v4-flash"
reasoning_profile = "high"
latency_preference = "fast"
multimodal_required = false
context_window_tokens = 1000000
max_output_tokens = 32768
safety_tier = "high"
privacy_tier = "standard"
residency = "global"
approval_policy = "ask"
fallback_profiles = []

[model_profiles.deepseek-fast.provider_options]
thinking = "enabled"

[model_presets.openai]
default_model_profile = "default"
auto_sizing_router_model_profile = "auto-size-router"
auto_sizing_small_model_profile = "auto-size-small"
auto_sizing_medium_model_profile = "auto-size-medium"
auto_sizing_large_model_profile = "auto-size-large"
allowed_reasoning_efforts = ["low", "medium", "high", "xhigh"]

[model_presets.deepseek]
default_model_profile = "deepseek-fast"
auto_sizing_router_model_profile = "deepseek-fast"
auto_sizing_small_model_profile = "deepseek-fast"
auto_sizing_medium_model_profile = "deepseek-default"
auto_sizing_large_model_profile = "deepseek-default"
allowed_reasoning_efforts = ["high", "xhigh"]

[model_presets.anthropic]
default_model_profile = "anthropic-fast"
auto_sizing_router_model_profile = "anthropic-fast"
auto_sizing_small_model_profile = "anthropic-fast"
auto_sizing_medium_model_profile = "anthropic-default"
auto_sizing_large_model_profile = "anthropic-default"
allowed_reasoning_efforts = ["high"]

[permissions]
# Generated Linux configuration selects full-access when Bubblewrap is
# available and auto-allow otherwise. macOS also selects auto-allow.
# auto-allow uses the model gate; full-access skips prompts but stays sandboxed.
# host-access is primary-user-only and executes
# local shell actions on the host outside the configured sandbox.
# Sandbox, scope, network, and approval settings are also primary-user-only;
# trusted project overlays may not change this execution boundary.
approval_policy = "ask"
# Optional named permission preset applied before explicit settings.
# preset = "default"
# Generated Linux configuration uses Bubblewrap for OS-level confinement when
# its code-owned executable is available, and policy-only otherwise. macOS and
# other platforms use policy-only until they provide an equivalent backend.
sandbox = "bubblewrap"
# Scope paths may name files or directories. A Unix-domain socket may also be
# placed in read_scopes for an explicitly trusted service endpoint; a read-only
# mount does not make requests sent through that socket read-only.
# read_scopes = ["/var/run/docker.sock"]
# Installed SDKs use the same generic authority. Every required loader or
# library root must be listed explicitly; read scopes never grant write access.
# read_scopes = ["/opt/acme-sdk"]
# Writable host paths projected into Bubblewrap; omitted by default.
# write_scopes = ["."]
# Optional sanitized Git identity for Bubblewrap commits. Configure both fields;
# Mezzanine never imports the host global Git configuration.
# [permissions.bubblewrap]
# Absolute Bubblewrap executable and fail-closed isolation defaults.
# executable = "/usr/bin/bwrap"
# unavailable = "fail"
# network = "isolated"
# environment = "minimal"
# Exact host supplementary groups to project. The primary group is automatic;
# an empty list strips all ambient supplementary groups.
# group_whitelist = []
# Optional variable names are read from the active pane and always redacted.
# When this field is omitted, PATH is forwarded by default. An explicit list
# replaces that default, so include PATH when sandboxed command lookup should
# use the pane's safely resolved path. Forwarding never grants filesystem authority.
# env_whitelist = ["ACME_HOME"]
# git_user_name = "Your Name"
# git_user_email = "you@example.invalid"
# Explicit macOS Seatbelt configuration uses the same fail-closed policy
# surface without exposing raw SBPL or launcher arguments. Generated macOS
# defaults remain policy-only until the dedicated enablement rollout lands.
# [permissions.seatbelt]
# executable = "/usr/bin/sandbox-exec"
# unavailable = "fail"
# network = "isolated"
# environment = "minimal"
# env_whitelist = ["PATH"]
# git_user_name = "Your Name"
# git_user_email = "you@example.invalid"
# Command-rule arrays are empty by default. A rule may classify matching shell
# commands and declare their complete effects for deterministic authorization.
command_rules = []
session_command_rules = []
global_command_rules = []
# [[permissions.command_rules]]
# id = "cargo-check"
# pattern = "cargo check"
# decision = "allow"
# scope = "managed"
# shell_classification = "simple"
# justification = "Permit the bounded Rust type-check command."
# examples = ["cargo check --workspace"]
# match_examples = ["cargo check"]
# not_match_examples = ["cargo clean"]
# [permissions.command_rules.effects]
# completeness = "complete"
# read_scopes = ["."]
# write_scopes = ["target"]
# network = false
# credentials = false
# process_control = true
# Under policy-only this affects approval classification only. Under Bubblewrap,
# deny keeps shell actions network-isolated; allow connects every shell action;
# prompt connects only authorized network actions. Brokered web and MCP actions
# are controller-gated.
network_policy = "prompt"
# Prompt before destructive actions not already covered by a stronger rule.
destructive_action_policy = "prompt"
# Approval bypass cannot be enabled from config; false documents the safe default.
bypass_mode = false

# Named MCP server definitions. No server is configured by default.
[mcp_servers]
# Example stdio server; use url instead of command/args for streamable HTTP.
# [mcp_servers.example]
# name = "Example tools"
# command = "example-mcp-server"
# args = ["--stdio"]
# url = "https://example.invalid/mcp"
# env_vars = ["EXAMPLE_TOKEN"]
# cwd = "."
# bearer_token_env = "EXAMPLE_TOKEN"
# enabled_tools = ["read_file"]
# disabled_tools = ["delete_file"]
# Timeout aliases accept seconds or milliseconds; configure only one form.
# startup_timeout_sec = 10
# startup_timeout_ms = 10000
# tool_timeout_sec = 60
# tool_timeout_ms = 60000
# enabled = true
# approval = "prompt"
# [mcp_servers.example.env]
# LOG_LEVEL = "info"
# [mcp_servers.example.http_headers]
# X_Client = "mez"
# [mcp_servers.example.tool_approvals]
# read_file = "allow"
# Model-visible purpose and safety metadata for effects outside shell mediation.
# [mcp_servers.example.external_capability]
# purpose = "Issue and pull request operations"
# usage_instructions = "Use for issue triage and pull request review tasks."
# mutates_filesystem_outside_shell = false
# executes_processes_outside_shell = false
# accesses_credentials_outside_shell = false

# Authentication refresh policy. Credentials themselves are never stored here.
[auth]
provider_refresh_leeway_seconds = 86400

# Repository instruction-file discovery and context-size policy.
[instructions]
# Additional global instruction files loaded for every project.
global_files = []
# Instruction filenames searched from the project hierarchy.
project_filenames = ["AGENTS.md"]
# Maximum instruction bytes loaded before applying on_truncation.
max_bytes = 32768
# Skip hidden directories during instruction discovery by default.
include_hidden_directories = false
# Truncation policy: summarize oversized instruction context.
on_truncation = "summarize"

# Named lifecycle hooks. No external command executes by default.
[hooks]
# [hooks.example]
# event = "post_shell_command"
# events = ["post_shell_command"]
# program = "/usr/bin/logger"
# command = "printf hook"
# args = ["mez hook completed"]
# shell = "focused"
# kind = "program"
# enabled = true
# required = false
# agent_hook = false
# timeout_sec = 5
# timeout_ms = 5000
# on_failure = "warn"
# cwd = "."
# working_directory = "."
# inject_instructions = ""
# mutates_policy = false
# alters_action = false
# [hooks.example.env]
# HOOK_MODE = "audit"
# [hooks.example.match]
# path = "action_type"
# equals = "shell_command"
# `matches` accepts an array of matcher groups when one group is insufficient.
# matches = [{ path = "status", equals = "failed" }]

# Append-only security audit log and retention policy.
[audit]
# Enable audit persistence.
enabled = true
# Relative paths resolve beneath the Mezzanine config directory.
path = "audit.jsonl"
# Audit serialization format; only jsonl is currently supported.
format = "jsonl"
# Delete audit records older than this many days.
retention_days = 30
# Chain audit records cryptographically when enabled.
hash_chain = false
# Fail protected operations when audit persistence is unavailable.
required = false

# Reserved namespaced extension configuration for integrations.
[extensions]
"##;

/// Defines the DEFAULT PROJECT CONFIG TOML const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const DEFAULT_PROJECT_CONFIG_TOML: &str = r##"# Mezzanine project configuration.
version = 1
"##;
