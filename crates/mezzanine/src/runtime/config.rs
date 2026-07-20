//! Runtime Config implementation.
//!
//! This module owns the runtime config boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    BTreeMap, DEFAULT_AGENT_ACTION_FAILURE_RETRY_LIMIT,
    DEFAULT_AGENT_COMPACTION_RAW_RETENTION_PERCENT, DEFAULT_AGENT_LOOP_LIMIT,
    DEFAULT_AGENT_ROUTING, DEFAULT_AUTO_SIZING_FALLBACK_POLICY, DEFAULT_MAX_CONCURRENT_AGENTS,
    DEFAULT_MAX_ROOT_SUBAGENTS, DEFAULT_MAX_SUBAGENT_DEPTH, DEFAULT_MAX_SUBAGENT_PANES_PER_WINDOW,
    DEFAULT_MAX_SUBAGENTS_PER_SUBAGENT, DEFAULT_SUBAGENT_WAIT_POLICY, MezError, Result,
    RuntimeAgentPersonalityProfile, RuntimeAutoSizingConfig, RuntimeAutoSizingFallbackPolicy,
    RuntimeConfigApplyReport, RuntimeSessionService, SubagentProfile, SubagentWaitPolicy, Value,
    builtin_subagent_profiles, ensure_absolute, optional_path_json, optional_string_json,
    runtime_cooperation_mode, runtime_cooperation_mode_name, runtime_json_string_field,
    runtime_json_value,
};

mod agents;
mod audit;
mod effective;
mod frames;
mod hooks;
mod mcp;
mod model;
mod permissions;
mod providers;
mod terminal_options;
mod theme;
mod trust;
pub(super) use agents::{
    runtime_agent_action_failure_retry_limit_from_config, runtime_agent_auto_sizing_from_config,
    runtime_agent_compaction_raw_retention_percent_from_config,
    runtime_agent_custom_system_prompt_from_config, runtime_agent_loop_limit_from_config,
    runtime_agent_personality_profiles_from_config, runtime_agent_routing_from_config,
    runtime_default_agent_personality_from_config, runtime_max_concurrent_agents_from_config,
    runtime_max_root_subagents_from_config, runtime_max_subagent_depth_from_config,
    runtime_max_subagent_panes_per_window_from_config,
    runtime_max_subagents_per_subagent_from_config, runtime_subagent_profiles_from_config,
    runtime_subagent_wait_policy_from_config,
};
pub(super) use audit::{runtime_audit_config_present, runtime_audit_log_from_config};
pub use effective::runtime_effective_config_value;
pub(super) use frames::{
    runtime_command_bindings_from_effective, runtime_key_bindings_from_config,
    runtime_pane_frame_position_from_config, runtime_pane_frame_style_from_config,
    runtime_pane_frame_template_from_config, runtime_pane_frame_visible_fields_from_config,
    runtime_pane_frames_enabled_from_config, runtime_window_frame_position_from_config,
    runtime_window_frame_right_status_template_from_config, runtime_window_frame_style_from_config,
    runtime_window_frame_template_from_config, runtime_window_frame_visible_fields_from_config,
    runtime_window_frames_enabled_from_config,
};
pub(super) use hooks::{
    runtime_agent_turn_start_hook_payload, runtime_hook_definitions_from_config,
    runtime_hook_target_pane_id, runtime_marker_for_action, runtime_mcp_error_code,
    runtime_permission_decision_hook_payload, runtime_permission_request_hook_payload,
    runtime_post_mcp_hook_payload, runtime_post_shell_hook_payload, runtime_pre_mcp_hook_payload,
    runtime_pre_shell_hook_payload, runtime_random_marker_token, runtime_user_prompt_hook_payload,
};
pub(super) use mcp::runtime_mcp_registry_from_config;
pub(super) use model::{
    RUNTIME_LATENCY_PREFERENCES, runtime_model_command_args, runtime_model_override_scope_for_args,
    runtime_model_override_scope_name, runtime_model_profile_display,
    runtime_validate_latency_preference,
};
pub(super) use permissions::{
    ConfiguredPermissions, runtime_approval_decision_name_to_kind,
    runtime_blocked_approval_request, runtime_config_permission_preset,
    runtime_configured_permissions_from_config, runtime_message_recipient,
};
#[cfg(test)]
pub(super) use permissions::{NetworkPolicy, SandboxConfig};
pub(crate) use providers::runtime_default_models_for_provider;
pub(super) use providers::{
    runtime_preset_registry_from_config, runtime_provider_registry_from_config,
    runtime_recommended_model_for_provider,
};
pub(super) use terminal_options::{
    runtime_history_limit_from_config, runtime_history_rotate_lines_from_config,
    runtime_host_clipboard_from_config, runtime_saved_agent_session_limit_from_config,
    runtime_terminal_agent_wrap_column_cap_from_config, runtime_terminal_clipboard_from_config,
    runtime_terminal_cursor_blink_from_config,
    runtime_terminal_cursor_blink_interval_ms_from_config,
    runtime_terminal_cursor_style_from_config, runtime_terminal_emoji_width_from_config,
    runtime_terminal_reduced_motion_from_config,
    runtime_terminal_render_rate_limit_fps_from_config,
    runtime_terminal_resize_debounce_ms_from_config,
    runtime_terminal_shell_output_preview_lines_from_config, runtime_terminal_term_from_config,
};
pub use theme::runtime_ui_theme_from_config;
pub(super) use trust::{
    runtime_path_under_project_root, runtime_project_root_param, runtime_project_trust_record_json,
    runtime_trust_decision_name, runtime_trust_decision_param,
};

// Runtime config parsing and project trust helpers.

/// Runs the json escape operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn json_escape(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch.is_control() => escaped.push(' '),
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// Runs the runtime fit status line operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_fit_status_line(value: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut output = String::new();
    let mut used = 0usize;
    for grapheme in mez_terminal::terminal_graphemes(value) {
        let grapheme_width = mez_terminal::active_terminal_grapheme_width(grapheme);
        if used.saturating_add(grapheme_width) > width {
            break;
        }
        output.push_str(grapheme);
        used = used.saturating_add(grapheme_width);
    }
    if used < width {
        output.push_str(&" ".repeat(width.saturating_sub(used)));
    }
    output
}

/// Runs the runtime config method applies to live service operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_config_method_applies_to_live_service(method: &str) -> bool {
    matches!(method, "config/set" | "config/unset" | "config/reload")
}

/// Runs the runtime config apply event payload operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_config_apply_event_payload(
    method: &str,
    report: &RuntimeConfigApplyReport,
) -> String {
    format!(
        r#"{{"method":"{}","applied_layers":{},"skipped_layers":{},"terminal_history_limit":{},"terminal_history_rotate_lines":{},"terminal_term":"{}","ui_theme":"{}","window_frames_enabled":{},"pane_frames_enabled":{},"max_concurrent_agents":{},"permission_policy_applied":{},"mcp_servers_configured":{},"mcp_servers_blacklisted":{},"project_trust_prompts_announced":{}}}"#,
        json_escape(method),
        runtime_string_array_json(&report.applied_layers),
        runtime_string_array_json(&report.skipped_layers),
        report.terminal_history_limit,
        report.terminal_history_rotate_lines,
        json_escape(&report.terminal_term),
        json_escape(&report.ui_theme),
        report.window_frames_enabled,
        report.pane_frames_enabled,
        report.max_concurrent_agents,
        report.permission_policy_applied,
        report.mcp_servers_configured,
        runtime_string_array_json(&report.mcp_servers_blacklisted),
        report.project_trust_prompts_announced
    )
}

/// Runs the runtime string array json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_string_array_json(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| format!(r#""{}""#, json_escape(value)))
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// Runs the validate runtime terminal term operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_runtime_terminal_term(term: &str) -> Result<()> {
    if term.trim().is_empty() || term.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(MezError::config(
            "terminal.term must be a non-empty printable string",
        ));
    }
    if matches!(term, "xterm" | "xterm-256color") {
        return Err(MezError::config(
            "terminal.term must not use the host terminal identity in the default profile",
        ));
    }
    Ok(())
}

/// Runs the runtime json object operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_json_object<'a>(
    value: &'a Value,
    field: &str,
) -> Option<&'a serde_json::Map<String, Value>> {
    value.get(field).and_then(Value::as_object)
}

/// Runs the runtime json string operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_json_string(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

/// Runs the runtime json scalar string operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_json_scalar_string(path: &str, value: &Value) -> Result<String> {
    match value {
        Value::String(value) if !value.is_empty() => Ok(value.clone()),
        Value::Bool(value) => Ok(value.to_string()),
        Value::Number(value) => Ok(value.to_string()),
        _ => Err(MezError::config(format!(
            "{path} must be a non-empty scalar value"
        ))),
    }
}

/// Runs the runtime json bool operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_json_bool(value: Option<&Value>) -> Option<bool> {
    value.and_then(Value::as_bool)
}

/// Runs the runtime json u64 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_json_u64(value: Option<&Value>) -> Option<u64> {
    value.and_then(Value::as_u64)
}

/// Returns the configured provider auth refresh leeway in seconds.
pub(super) fn runtime_provider_auth_refresh_leeway_seconds_from_config(root: &Value) -> u64 {
    runtime_json_object(root, "auth")
        .and_then(|auth| runtime_json_u64(auth.get("provider_refresh_leeway_seconds")))
        .unwrap_or(crate::security::auth::DEFAULT_PROVIDER_AUTH_REFRESH_LEEWAY_SECONDS)
}

/// Runs the runtime json string array operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_json_string_array(value: Option<&Value>) -> Result<Option<Vec<String>>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let array = value
        .as_array()
        .ok_or_else(|| MezError::config("configuration value must be an array of strings"))?;
    let mut output = Vec::with_capacity(array.len());
    for item in array {
        let Some(item) = item.as_str() else {
            return Err(MezError::config(
                "configuration array values must all be strings",
            ));
        };
        output.push(item.to_string());
    }
    Ok(Some(output))
}

/// Runs the runtime json string map operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_json_string_map(
    value: Option<&Value>,
) -> Result<Option<BTreeMap<String, String>>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = value
        .as_object()
        .ok_or_else(|| MezError::config("configuration value must be a string map"))?;
    let mut output = BTreeMap::new();
    for (key, value) in object {
        let Some(value) = value.as_str() else {
            return Err(MezError::config(
                "configuration map values must all be strings",
            ));
        };
        output.insert(key.clone(), value.to_string());
    }
    Ok(Some(output))
}

/// Runs the runtime mcp approval setting operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn optional_i32_json(value: Option<i32>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string())
}

#[cfg(test)]
mod tests {
    use crate::host::terminal::DEFAULT_AGENT_WRAP_COLUMN_CAP;
    use mez_terminal::{TerminalEmojiWidth, active_terminal_text_width};

    use super::{
        runtime_fit_status_line, runtime_terminal_agent_wrap_column_cap_from_config,
        runtime_terminal_emoji_width_from_config,
    };

    /// Verifies that fitting ASCII text truncates to the requested display width.
    #[test]
    fn fits_ascii_text_to_width() {
        assert_eq!(runtime_fit_status_line("hello world", 5), "hello");
        assert_eq!(runtime_fit_status_line("ab", 5), "ab   ");
    }

    /// Verifies that fitting fullwidth text counts display columns, not Unicode
    /// scalar values, so wide characters do not overflow the target width.
    #[test]
    fn fits_fullwidth_text_by_display_width() {
        let result = runtime_fit_status_line("ＡＢＣＤＥＦ", 6);
        assert_eq!(active_terminal_text_width(&result), 6);
        assert_eq!(result.chars().count(), 3);
    }

    /// Verifies that fitting text with mixed fullwidth and narrow characters
    /// truncates at the display-width boundary.
    #[test]
    fn fits_mixed_width_text_by_display_width() {
        let result = runtime_fit_status_line("ＡbcＤ", 4);
        // Ａ = 2, b = 1, c = 1 → fits in 4 cols
        // Ｄ would be another 2 → 6 > 4, dropped
        assert_eq!(active_terminal_text_width(&result), 4);
        assert!(result.starts_with('Ａ'));
    }

    /// Verifies that fitting with zero width returns an empty string.
    #[test]
    fn fits_zero_width_returns_empty() {
        assert_eq!(runtime_fit_status_line("hello", 0), "");
        assert_eq!(runtime_fit_status_line("ＡＢ", 0), "");
    }

    /// Verifies that runtime configuration parses the terminal emoji-width
    /// compatibility policy and rejects unsupported values. This protects the
    /// pane renderer from silently falling back to the wrong width model when a
    /// user opts into one-cell text fallback status glyphs.
    #[test]
    fn parses_terminal_emoji_width_policy_from_config() {
        assert_eq!(
            runtime_terminal_emoji_width_from_config(&serde_json::json!({
                "terminal": {
                    "emoji_width": "narrow"
                }
            }))
            .unwrap(),
            TerminalEmojiWidth::Narrow
        );
        assert_eq!(
            runtime_terminal_emoji_width_from_config(&serde_json::json!({})).unwrap(),
            TerminalEmojiWidth::Wide
        );
        assert!(
            runtime_terminal_emoji_width_from_config(&serde_json::json!({
                "terminal": {
                    "emoji_width": "auto"
                }
            }))
            .is_err()
        );
    }

    /// Verifies runtime configuration parses the agent transcript row width cap.
    ///
    /// The cap is applied process-wide before agent log and markdown transcript
    /// rows are rendered, so invalid or missing values must not silently alter the
    /// default bounded-row behavior.
    #[test]
    fn parses_terminal_agent_wrap_column_cap_from_config() {
        assert_eq!(
            runtime_terminal_agent_wrap_column_cap_from_config(&serde_json::json!({
                "terminal": {
                    "agent_wrap_column_cap": 96
                }
            }))
            .unwrap(),
            96
        );
        assert_eq!(
            runtime_terminal_agent_wrap_column_cap_from_config(&serde_json::json!({})).unwrap(),
            DEFAULT_AGENT_WRAP_COLUMN_CAP
        );
        assert!(
            runtime_terminal_agent_wrap_column_cap_from_config(&serde_json::json!({
                "terminal": {
                    "agent_wrap_column_cap": 0
                }
            }))
            .is_err()
        );
    }

    /// Verifies that narrow characters pad to the exact display width while
    /// fullwidth characters are not truncated mid-grapheme.
    #[test]
    fn fits_narrow_pads_and_wide_truncates_cleanly() {
        let narrow = runtime_fit_status_line("x", 4);
        assert_eq!(active_terminal_text_width(&narrow), 4);

        let wide = runtime_fit_status_line("Ａ", 1);
        // 'Ａ' is 2 cols wide, 2 > 1, so it is dropped entirely
        assert_eq!(active_terminal_text_width(&wide), 1);
        assert_eq!(wide, " ");
    }
}
