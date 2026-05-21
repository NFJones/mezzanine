//! Config Schema implementation.
//!
//! This module owns the config schema boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

// Known config schema keys and schema validation helpers.

/// Defines the PRIMARY CONFIG FILENAMES const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const PRIMARY_CONFIG_FILENAMES: &[&str] =
    &["config.toml", "config.yaml", "config.yml", "config.json"];

/// Defines the BASELINE TOP LEVEL KEYS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const BASELINE_TOP_LEVEL_KEYS: &[&str] = &[
    "version",
    "session",
    "terminal",
    "shell",
    "keys",
    "layout",
    "frames",
    "theme",
    "themes",
    "history",
    "agents",
    "model_profiles",
    "permissions",
    "providers",
    "subagents",
    "personalities",
    "message_protocol",
    "control",
    "mcp_servers",
    "auth",
    "instructions",
    "hooks",
    "snapshots",
    "audit",
    "extensions",
];

/// Provider-visible operation names for model-authored live config changes.
pub const CONFIG_CHANGE_OPERATION_NAMES: &[&str] = &["set", "unset", "reset"];

/// Provider-visible value guidance for model-authored live config changes.
pub const CONFIG_CHANGE_VALUE_DESCRIPTION: &str = "For operation=set, provide a string containing one JSON scalar or string array accepted by config/set: JSON string, integer, boolean, or string array. Plain text is accepted as a JSON string. For operation=unset or reset, use null. reset removes the explicit override so the lower-precedence or default value becomes effective. Objects and null set-values are rejected.";

/// One provider-visible annotation for a supported live `config_change` path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigChangePathAnnotation {
    /// Dotted path pattern accepted by the live mutation planner.
    pub pattern: &'static str,
    /// Why an agent would mutate this path.
    pub purpose: &'static str,
    /// Supported value type for `set` operations.
    pub value_type: &'static str,
    /// Required value format or dynamic-segment convention.
    pub format: &'static str,
    /// Provider-visible operations supported for this path pattern.
    pub operations: &'static [&'static str],
}

/// Returns provider-facing annotations for model-authored live config paths.
///
/// The annotation set is intentionally compact: it groups paths by behavioral
/// contract instead of enumerating every concrete key. Validation still uses
/// the implementation schema after a `config_change` action is accepted.
pub fn config_change_setting_path_annotations() -> Vec<ConfigChangePathAnnotation> {
    vec![
        ConfigChangePathAnnotation {
            pattern: "version",
            purpose: "Set the primary Mezzanine config schema version.",
            value_type: "integer",
            format: "Positive TOML/JSON integer; normally leave unchanged unless migrating config.",
            operations: CONFIG_CHANGE_OPERATION_NAMES,
        },
        ConfigChangePathAnnotation {
            pattern: "theme.active",
            purpose: "Switch the active built-in or configured UI theme.",
            value_type: "string",
            format: "Theme name exactly as shown by :list-themes; set uses set-theme behavior.",
            operations: CONFIG_CHANGE_OPERATION_NAMES,
        },
        ConfigChangePathAnnotation {
            pattern: "theme.aliases.<alias>",
            purpose: "Override one named theme alias used by the active UI theme.",
            value_type: "string",
            format: "`<alias>` is an alias name; value must be #rgb or #rrggbb.",
            operations: CONFIG_CHANGE_OPERATION_NAMES,
        },
        ConfigChangePathAnnotation {
            pattern: "theme.colors.<slot>",
            purpose: "Override one concrete UI color slot.",
            value_type: "string",
            format: "`<slot>` is a color slot name; value must be #rgb, #rrggbb, or a valid alias name.",
            operations: CONFIG_CHANGE_OPERATION_NAMES,
        },
        ConfigChangePathAnnotation {
            pattern: "agents.<key>",
            purpose: "Change global agent behavior such as provider, model profile, compaction, or subagent limits.",
            value_type: "string, integer, or boolean",
            format: "Use supported agents keys; profile fields are strings, counts/percents are integers, flags are booleans.",
            operations: CONFIG_CHANGE_OPERATION_NAMES,
        },
        ConfigChangePathAnnotation {
            pattern: "agents.auto_sizing.<key>",
            purpose: "Tune the auto-sizing router model, tier model profiles, reasoning efforts, or fallback behavior.",
            value_type: "string or string array",
            format: "Model profile fields are strings; allowed_reasoning_efforts is a string array.",
            operations: CONFIG_CHANGE_OPERATION_NAMES,
        },
        ConfigChangePathAnnotation {
            pattern: "model_profiles.<name>.<key>",
            purpose: "Create or adjust a named model profile used by agents or routing.",
            value_type: "string, integer, boolean, or string array",
            format: "`<name>` is the model profile identifier; supported keys exclude provider_options.",
            operations: CONFIG_CHANGE_OPERATION_NAMES,
        },
        ConfigChangePathAnnotation {
            pattern: "providers.<name>.<key>",
            purpose: "Create or adjust a named provider connection profile.",
            value_type: "string, boolean, or string array",
            format: "`<name>` is the provider identifier; supported keys exclude provider options and secrets.",
            operations: CONFIG_CHANGE_OPERATION_NAMES,
        },
        ConfigChangePathAnnotation {
            pattern: "mcp_servers.<name>.<key>",
            purpose: "Enable, disable, or retarget a named MCP server without editing config files manually.",
            value_type: "string, integer, boolean, or string array",
            format: "`<name>` is the server identifier; supported keys exclude env, headers, tool approvals, and external capability.",
            operations: CONFIG_CHANGE_OPERATION_NAMES,
        },
        ConfigChangePathAnnotation {
            pattern: "history.<key>",
            purpose: "Adjust scrollback retention and history behavior.",
            value_type: "integer, boolean, or string",
            format: "lines/rotate_lines are integers, persist is boolean, search_mode is a string enum.",
            operations: CONFIG_CHANGE_OPERATION_NAMES,
        },
        ConfigChangePathAnnotation {
            pattern: "permissions.<key>",
            purpose: "Change high-level permission defaults and approval behavior.",
            value_type: "string or string array",
            format: "Supported scalar permission keys; command-rule arrays are not mutable through config_change.",
            operations: CONFIG_CHANGE_OPERATION_NAMES,
        },
        ConfigChangePathAnnotation {
            pattern: "<static-section>.<key>",
            purpose: "Adjust supported scalar settings in session, terminal, shell, keys, layout, frames, message protocol, control, auth, instructions, hooks, snapshots, or audit sections.",
            value_type: "string, integer, boolean, or string array",
            format: "Use only documented keys from the supported-pattern list; inspect current config for dynamic names.",
            operations: CONFIG_CHANGE_OPERATION_NAMES,
        },
    ]
}

/// Formats config-change annotations as compact provider-facing prose.
pub fn config_change_setting_path_annotations_text() -> String {
    config_change_setting_path_annotations()
        .into_iter()
        .map(|annotation| {
            format!(
                "- {}: purpose={}; value_type={}; format={}; operations={}",
                annotation.pattern,
                annotation.purpose,
                annotation.value_type,
                annotation.format,
                annotation.operations.join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Formats config-change annotations as a Markdown table for built-in skills.
pub fn config_change_setting_path_annotations_markdown() -> String {
    let mut rows = vec![
        "| Path pattern | Purpose | Value/type | Format requirements | Operations |".to_string(),
        "| --- | --- | --- | --- | --- |".to_string(),
    ];
    rows.extend(
        config_change_setting_path_annotations()
            .into_iter()
            .map(|annotation| {
                format!(
                    "| `{}` | {} | {} | {} | {} |",
                    annotation.pattern,
                    annotation.purpose,
                    annotation.value_type,
                    annotation.format,
                    annotation.operations.join(", ")
                )
            }),
    );
    rows.join("\n")
}

/// Builds provider-facing path guidance for model-authored live config changes.
///
/// The returned text intentionally mirrors the conservative live mutation
/// planner: dotted ASCII paths, scalar targets only, and at most three path
/// segments. Dynamic `<name>` segments are caller-selected identifiers such as a
/// model profile name, provider name, MCP server name, hook name, or theme alias.
pub fn config_change_setting_path_description() -> String {
    format!(
        "Dotted live Mezzanine config path. Use only ASCII path segments [A-Za-z0-9_-]. The live mutation planner supports scalar paths up to three segments; inspect current config with shell_command before changing dynamic names. Supported patterns: version (integer); session.<key> where key is one of [{}] except default_command; terminal.<key> where key is one of [{}]; shell.<key> where key is one of [{}] except path/executable/command, plus shell.env.<name>; keys.<key> where key is one of [{}]; layout.<key> where key is one of [{}]; frames.window.<key> where key is one of [{}]; frames.pane.<key> where key is one of [{}]; theme.active, theme.aliases.<alias>, theme.colors.<slot>; history.<key> where key is one of [{}]; agents.<key> where key is one of [{}], plus agents.auto_sizing.<key> where key is one of [{}]; model_profiles.<name>.<key> where key is one of [{}] except provider_options; providers.<name>.<key> where key is one of [{}] except options; subagents.<name>.<key> where key is one of [{}] except shell_env; personalities.<name>.<key> where key is one of [{}]; permissions.<key> where key is one of [{}] except command rule arrays; message_protocol.<key> where key is one of [{}]; control.<key> where key is one of [{}]; mcp_servers.<name>.<key> where key is one of [{}] except env/http_headers/tool_approvals/external_capability; auth.<key> where key is one of [{}]; instructions.<key> where key is one of [{}]; hooks.<name>.<key> where key is one of [{}] except env/match/matches; snapshots.<key> where key is one of [{}]; audit.<key> where key is one of [{}]. Runtime validation still rejects secrets, unsafe shell override paths, unsupported enum values, invalid colors, container targets, and array-entry mutation paths. Schema annotations: {}",
        config_keys_except(SESSION_KEYS, &["default_command"]),
        TERMINAL_KEYS.join(", "),
        config_keys_except(SHELL_KEYS, &["path", "executable", "command"]),
        KEY_BINDING_KEYS.join(", "),
        LAYOUT_KEYS.join(", "),
        WINDOW_FRAME_KEYS.join(", "),
        PANE_FRAME_KEYS.join(", "),
        HISTORY_KEYS.join(", "),
        AGENT_KEYS.join(", "),
        AGENT_AUTO_SIZING_KEYS.join(", "),
        config_keys_except(MODEL_PROFILE_KEYS, &["provider_options"]),
        config_keys_except(PROVIDER_KEYS, &["options"]),
        config_keys_except(SUBAGENT_PROFILE_KEYS, &["shell_env"]),
        PERSONALITY_PROFILE_KEYS.join(", "),
        config_keys_except(
            PERMISSION_KEYS,
            &[
                "command_rules",
                "session_command_rules",
                "global_command_rules",
            ],
        ),
        MESSAGE_PROTOCOL_KEYS.join(", "),
        CONTROL_KEYS.join(", "),
        config_keys_except(
            MCP_SERVER_KEYS,
            &[
                "env",
                "http_headers",
                "tool_approvals",
                "external_capability",
            ],
        ),
        AUTH_KEYS.join(", "),
        INSTRUCTION_KEYS.join(", "),
        config_keys_except(HOOK_KEYS, &["env", "match", "matches"]),
        SNAPSHOT_KEYS.join(", "),
        AUDIT_KEYS.join(", "),
        config_change_setting_path_annotations_text(),
    )
}

/// Formats a key list while omitting unsupported nested/container entries.
fn config_keys_except(keys: &[&str], excluded: &[&str]) -> String {
    keys.iter()
        .copied()
        .filter(|key| !excluded.contains(key))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Defines the MCP SERVER KEYS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const MCP_SERVER_KEYS: &[&str] = &[
    "name",
    "command",
    "args",
    "url",
    "env",
    "env_vars",
    "cwd",
    "http_headers",
    "bearer_token_env",
    "enabled_tools",
    "disabled_tools",
    "startup_timeout_sec",
    "startup_timeout_ms",
    "tool_timeout_sec",
    "tool_timeout_ms",
    "enabled",
    "approval",
    "tool_approvals",
    "external_capability",
];

/// Defines the PERMISSION KEYS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const PERMISSION_KEYS: &[&str] = &[
    "approval_policy",
    "preset",
    "trusted_directories",
    "trusted_projects",
    "command_rules",
    "session_command_rules",
    "global_command_rules",
    "network_policy",
    "destructive_action_policy",
    "bypass_mode",
];

/// Defines the COMMAND RULE KEYS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const COMMAND_RULE_KEYS: &[&str] = &[
    "pattern",
    "decision",
    "scope",
    "match",
    "exact_sha256",
    "shell_classification",
    "argument_policy",
    "executable_policy",
    "justification",
    "examples",
    "match_examples",
    "not_match_examples",
];

/// Defines the SESSION KEYS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const SESSION_KEYS: &[&str] = &[
    "detach_behavior",
    "reattach_behavior",
    "empty_session_behavior",
    "restore_strategy",
    "default_command",
];

/// Defines the TERMINAL KEYS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const TERMINAL_KEYS: &[&str] = &[
    "profile",
    "term",
    "true_color",
    "mouse",
    "bracketed_paste",
    "clipboard",
    "clipboard_copy_command",
    "clipboard_paste_command",
    "alternate_screen",
    "focus_events",
    "nested_multiplexer",
    "passthrough",
    "resize_debounce_ms",
    "cursor_style",
    "cursor_blink",
    "cursor_blink_interval_ms",
];

/// Defines the SHELL KEYS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const SHELL_KEYS: &[&str] = &[
    "login",
    "interactive",
    "integration",
    "integration_mode",
    "default_working_directory",
    "env",
    "tool_discovery",
    "tool_cache",
    "fallback_behavior",
    "path",
    "executable",
    "command",
];

/// Defines the KEY BINDING KEYS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const KEY_BINDING_KEYS: &[&str] = &[
    "escape",
    "split_vertical",
    "split_horizontal",
    "new_window",
    "new_group",
    "agent_shell",
    "focus_up",
    "focus_down",
    "focus_left",
    "focus_right",
    "focus_previous_window",
    "focus_next_window",
    "focus_previous_group",
    "focus_next_group",
    "command_bindings",
];

/// Defines the LAYOUT KEYS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const LAYOUT_KEYS: &[&str] = &[
    "default",
    "resize_policy",
    "close_policy",
    "min_pane_columns",
    "min_pane_rows",
];

/// Defines the WINDOW FRAME KEYS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const WINDOW_FRAME_KEYS: &[&str] = &[
    "enabled",
    "position",
    "template",
    "right_status",
    "style",
    "visible_fields",
];

/// Defines the PANE FRAME KEYS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const PANE_FRAME_KEYS: &[&str] =
    &["enabled", "position", "template", "style", "visible_fields"];

/// Defines the THEME KEYS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const THEME_KEYS: &[&str] = &["active", "aliases", "colors"];

/// Defines the HISTORY KEYS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const HISTORY_KEYS: &[&str] = &["lines", "rotate_lines", "persist", "search_mode"];

/// Defines the AGENT KEYS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const AGENT_KEYS: &[&str] = &[
    "default_provider",
    "default_model_profile",
    "shell_only",
    "auto_compact",
    "auto_compact_threshold",
    "compaction_raw_retention_percent",
    "auto_reasoning",
    "action_failure_retry_limit",
    "custom_system_prompt",
    "default_personality",
    "auto_sizing",
    "subagent_placement",
    "max_concurrent_agents",
    "max_root_subagents",
    "max_subagents_per_subagent",
    "max_subagent_panes_per_window",
    "subagent_wait_policy",
    "max_depth",
    "prompt_profile",
    "default_agent_role",
];

/// Defines the AGENT AUTO SIZING KEYS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const AGENT_AUTO_SIZING_KEYS: &[&str] = &[
    "router_model_profile",
    "small_model_profile",
    "medium_model_profile",
    "large_model_profile",
    "allowed_reasoning_efforts",
    "fallback_policy",
];

/// Defines the PROVIDER KEYS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const PROVIDER_KEYS: &[&str] = &[
    "kind",
    "auth_profile",
    "base_url",
    "models",
    "default_model",
    "options",
];

/// Defines the MODEL PROFILE KEYS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const MODEL_PROFILE_KEYS: &[&str] = &[
    "provider",
    "model",
    "reasoning_profile",
    "reasoning_effort",
    "latency_preference",
    "multimodal_required",
    "multimodal",
    "context_window_tokens",
    "context_limit_tokens",
    "max_output_tokens",
    "provider_options",
    "safety_tier",
    "privacy",
    "privacy_tier",
    "residency",
    "approval",
    "approval_policy",
    "fallback_profiles",
];

/// Defines the SUBAGENT PROFILE KEYS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const SUBAGENT_PROFILE_KEYS: &[&str] = &[
    "name",
    "description",
    "developer_instructions",
    "developer_prompt",
    "model_profile",
    "model_profile_override",
    "permission_preset",
    "permission_override",
    "mcp_servers",
    "shell_env",
    "default_cooperation_mode",
    "default_mode",
    "default_read_scopes",
    "default_write_scopes",
];

/// Defines the PERSONALITY PROFILE KEYS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const PERSONALITY_PROFILE_KEYS: &[&str] = &[
    "name",
    "system_prompt",
    "instructions",
    "response_style",
    "style",
    "model_profile",
    "planning_enabled",
    "planning",
    "auto_reasoning_enabled",
    "auto_reasoning",
];

/// Defines the MESSAGE PROTOCOL KEYS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const MESSAGE_PROTOCOL_KEYS: &[&str] = &[
    "enabled",
    "endpoint",
    "retention_messages",
    "retention_bytes",
    "allow_remote_bridges",
];

/// Defines the CONTROL KEYS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const CONTROL_KEYS: &[&str] = &[
    "endpoint",
    "socket_path",
    "tcp_bind",
    "tcp_enabled",
    "auth_token_file",
    "observer_policy",
];

/// Defines the AUTH KEYS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const AUTH_KEYS: &[&str] = &["auth_file", "credential_store", "default_profile"];

/// Defines the INSTRUCTION KEYS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const INSTRUCTION_KEYS: &[&str] = &[
    "global_files",
    "project_filenames",
    "max_bytes",
    "include_hidden_directories",
    "on_truncation",
];

/// Defines the SNAPSHOT KEYS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const SNAPSHOT_KEYS: &[&str] = &[
    "enabled",
    "path",
    "on_detach",
    "on_interval_seconds",
    "on_agent_turn",
    "retention_count",
];

/// Defines the AUDIT KEYS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const AUDIT_KEYS: &[&str] = &[
    "enabled",
    "path",
    "format",
    "retention_days",
    "redact_secrets",
    "hash_chain",
    "required",
];

/// Defines the HOOK KEYS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const HOOK_KEYS: &[&str] = &[
    "event",
    "events",
    "program",
    "command",
    "args",
    "shell",
    "kind",
    "enabled",
    "required",
    "agent_hook",
    "timeout_ms",
    "timeout_sec",
    "on_failure",
    "match",
    "matches",
    "env",
    "working_directory",
    "cwd",
    "inject_instructions",
    "mutates_policy",
    "alters_action",
];
