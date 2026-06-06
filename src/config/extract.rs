//! Config Extract implementation.
//!
//! This module owns the config extract boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AGENT_AUTO_SIZING_KEYS, AGENT_KEYS, AUDIT_KEYS, AUTH_KEYS, BTreeMap, COMMAND_RULE_KEYS,
    CONTROL_KEYS, ConfigDiagnostic, ConfigFormat, ConfigScope, HISTORY_KEYS, HOOK_KEYS,
    INSTRUCTION_KEYS, JsonPathParser, JsonValueParser, KEY_BINDING_KEYS, LAYOUT_KEYS,
    MCP_SERVER_KEYS, MEMORY_KEYS, MESSAGE_PROTOCOL_KEYS, MODEL_PRESET_KEYS, MODEL_PROFILE_KEYS,
    PANE_FRAME_KEYS, PERMISSION_KEYS, PERSONALITY_PROFILE_KEYS, PROVIDER_KEYS, SESSION_KEYS,
    SHELL_KEYS, SNAPSHOT_KEYS, SUBAGENT_PROFILE_KEYS, TERMINAL_KEYS, THEME_KEYS, WINDOW_FRAME_KEYS,
    exact_command_sha256, normalize_exact_command_text, parse_config_json_value_best_effort,
};
use crate::terminal::{UI_COLOR_SLOT_NAMES, valid_color_alias_name};

// Config path/value extraction and command-rule example validation.

/// Runs the line indent operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn line_indent(line: &str) -> String {
    line.chars()
        .take_while(|ch| ch.is_whitespace())
        .collect::<String>()
}

/// Runs the extract toml paths operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn extract_toml_paths(text: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut section = Vec::<String>::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let name = trimmed.trim_matches('[').trim_matches(']');
            section = name
                .split('.')
                .map(clean_key_segment)
                .filter(|segment| !segment.is_empty())
                .collect();
            if let Some(top_level) = section.first() {
                paths.push(top_level.clone());
            }
            continue;
        }
        if let Some((key, _value)) = trimmed.split_once('=') {
            let mut path = section.clone();
            path.push(clean_key_segment(key));
            paths.push(canonical_config_path(&path.join(".")));
        }
    }

    paths
}

/// Runs the extract yaml paths operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn extract_yaml_paths(text: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut stack = Vec::<(usize, String)>::new();

    for line in text.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let indent = line.chars().take_while(|ch| ch.is_whitespace()).count();
        let trimmed = line.trim_start();
        let Some((key, _value)) = trimmed.split_once(':') else {
            continue;
        };
        if key.starts_with('-') {
            continue;
        }

        while stack
            .last()
            .is_some_and(|(existing, _)| *existing >= indent)
        {
            stack.pop();
        }
        stack.push((indent, clean_key_segment(key)));
        paths.push(canonical_config_path(
            &stack
                .iter()
                .map(|(_, key)| key.as_str())
                .collect::<Vec<_>>()
                .join("."),
        ));
    }

    paths
}

/// Runs the extract json paths operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn extract_json_paths(text: &str) -> Vec<String> {
    let mut parser = JsonPathParser::new(text);
    parser
        .parse_paths()
        .into_iter()
        .map(|path| canonical_config_path(&path))
        .collect()
}

/// Runs the extract config values operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn extract_config_values(format: ConfigFormat, text: &str) -> BTreeMap<String, String> {
    canonical_config_values(match format {
        ConfigFormat::Toml => extract_toml_values(text),
        ConfigFormat::Yaml => extract_yaml_values(text),
        ConfigFormat::Json => extract_json_values(text),
    })
}

/// Runs the extract toml values operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn extract_toml_values(text: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    let mut section = Vec::<String>::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let name = trimmed.trim_matches('[').trim_matches(']');
            section = name
                .split('.')
                .map(clean_key_segment)
                .filter(|segment| !segment.is_empty())
                .collect();
            continue;
        }
        if let Some((key, value)) = trimmed.split_once('=') {
            let mut path = section.clone();
            path.push(clean_key_segment(key));
            insert_config_value(&mut values, path.join("."), clean_value(value));
        }
    }

    values
}

/// Runs the extract yaml values operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn extract_yaml_values(text: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    let mut stack = Vec::<(usize, String)>::new();

    for line in text.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let indent = line.chars().take_while(|ch| ch.is_whitespace()).count();
        let trimmed = line.trim_start();
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        if key.starts_with('-') {
            continue;
        }

        while stack
            .last()
            .is_some_and(|(existing, _)| *existing >= indent)
        {
            stack.pop();
        }
        stack.push((indent, clean_key_segment(key)));
        if !value.trim().is_empty() {
            insert_config_value(
                &mut values,
                stack
                    .iter()
                    .map(|(_, key)| key.as_str())
                    .collect::<Vec<_>>()
                    .join("."),
                clean_value(value),
            );
        }
    }

    values
}

/// Runs the extract json values operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn extract_json_values(text: &str) -> BTreeMap<String, String> {
    let mut parser = JsonValueParser::new(text);
    parser.parse_values()
}

/// Returns the canonical spelling for supported historical configuration paths.
pub(super) fn canonical_config_path(path: &str) -> String {
    match path {
        "terminal.nested_muxxer" => "terminal.nested_multiplexer".to_string(),
        _ => path.to_string(),
    }
}

/// Canonicalizes extracted config values while preserving canonical-key
/// precedence over historical aliases.
fn canonical_config_values(values: BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut canonical = BTreeMap::new();
    for (path, value) in values {
        insert_config_value(&mut canonical, path, value);
    }
    canonical
}

/// Inserts one config value after applying historical path aliases.
fn insert_config_value(values: &mut BTreeMap<String, String>, path: String, value: String) {
    let canonical_path = canonical_config_path(&path);
    if canonical_path == path {
        values.insert(canonical_path, value);
    } else {
        values.entry(canonical_path).or_insert(value);
    }
}

/// Runs the clean key segment operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn clean_key_segment(segment: &str) -> String {
    segment
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_string()
}

/// Runs the clean value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn clean_value(value: &str) -> String {
    value
        .trim()
        .trim_matches(',')
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_string()
}

/// Runs the validate known schema path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_known_schema_path(path: &str) -> Option<String> {
    let segments = path.split('.').collect::<Vec<_>>();
    let top_level = segments.first().copied()?;
    match top_level {
        "version" => validate_top_level_scalar_path(&segments, "version"),
        "session" => validate_static_table_path(&segments, "session", SESSION_KEYS, &[]),
        "terminal" => validate_static_table_path(&segments, "terminal", TERMINAL_KEYS, &[]),
        "shell" => validate_static_table_path(&segments, "shell", SHELL_KEYS, &["env"]),
        "keys" => {
            validate_static_table_path(&segments, "keys", KEY_BINDING_KEYS, &["command_bindings"])
        }
        "layout" => validate_static_table_path(&segments, "layout", LAYOUT_KEYS, &[]),
        "frames" => validate_frames_path(&segments),
        "theme" => validate_theme_path(&segments),
        "themes" => validate_themes_path(&segments),
        "history" => validate_static_table_path(&segments, "history", HISTORY_KEYS, &[]),
        "memory" => validate_static_table_path(&segments, "memory", MEMORY_KEYS, &[]),
        "agents" => validate_agents_path(&segments),
        "model_profiles" => validate_model_profile_path(&segments),
        "model_presets" => validate_model_preset_path(&segments),
        "providers" => validate_provider_path(&segments),
        "subagents" => validate_subagent_profile_path(&segments),
        "personalities" => validate_personality_profile_path(&segments),
        "permissions" => None,
        "message_protocol" => {
            validate_static_table_path(&segments, "message_protocol", MESSAGE_PROTOCOL_KEYS, &[])
        }
        "control" => validate_static_table_path(&segments, "control", CONTROL_KEYS, &[]),
        "mcp_servers" => None,
        "auth" => validate_static_table_path(&segments, "auth", AUTH_KEYS, &[]),
        "instructions" => {
            validate_static_table_path(&segments, "instructions", INSTRUCTION_KEYS, &[])
        }
        "hooks" => validate_hook_path(&segments),
        "snapshots" => validate_static_table_path(&segments, "snapshots", SNAPSHOT_KEYS, &[]),
        "audit" => validate_static_table_path(&segments, "audit", AUDIT_KEYS, &[]),
        "extensions" => None,
        _ => None,
    }
}

/// Runs the validate agents path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_agents_path(segments: &[&str]) -> Option<String> {
    if segments.len() == 1 {
        return None;
    }
    let key = segments[1];
    if !AGENT_KEYS.contains(&key) {
        return Some("unknown agents configuration key".to_string());
    }
    if key == "auto_sizing" {
        if segments.len() == 2 {
            return None;
        }
        if !AGENT_AUTO_SIZING_KEYS.contains(&segments[2]) {
            return Some("unknown agents.auto_sizing configuration key".to_string());
        }
        if segments.len() > 3 {
            return Some(
                "scalar agents.auto_sizing setting must not contain nested keys".to_string(),
            );
        }
        return None;
    }
    if segments.len() > 2 {
        return Some("scalar agents setting must not contain nested keys".to_string());
    }
    None
}

/// Runs the validate top level scalar path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_top_level_scalar_path(segments: &[&str], name: &str) -> Option<String> {
    if segments.len() > 1 {
        Some(format!("{name} must be a scalar value"))
    } else {
        None
    }
}

/// Runs the validate static table path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_static_table_path(
    segments: &[&str],
    table: &str,
    allowed_keys: &[&str],
    dynamic_map_keys: &[&str],
) -> Option<String> {
    if segments.len() == 1 {
        return None;
    }
    let key = segments[1];
    if !allowed_keys.contains(&key) {
        return Some(format!("unknown {table} configuration key"));
    }
    if dynamic_map_keys.contains(&key) {
        return None;
    }
    if segments.len() > 2 {
        return Some(format!(
            "scalar {table} setting must not contain nested keys"
        ));
    }
    None
}

/// Runs the validate frames path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_frames_path(segments: &[&str]) -> Option<String> {
    if segments.len() == 1 {
        return None;
    }
    let target = segments[1];
    if !matches!(target, "window" | "pane") {
        return Some("unknown frames configuration target".to_string());
    }
    if segments.len() == 2 {
        return None;
    }
    let key = segments[2];
    let frame_keys = match target {
        "window" => WINDOW_FRAME_KEYS,
        "pane" => PANE_FRAME_KEYS,
        _ => unreachable!("frame target was validated above"),
    };
    if !frame_keys.contains(&key) {
        return Some("unknown frame configuration key".to_string());
    }
    if segments.len() > 3 {
        return Some("scalar frame setting must not contain nested keys".to_string());
    }
    None
}

/// Runs the validate theme path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_theme_path(segments: &[&str]) -> Option<String> {
    if segments.len() == 1 {
        return None;
    }
    let key = segments[1];
    if !THEME_KEYS.contains(&key) {
        return Some("unknown theme configuration key".to_string());
    }
    match key {
        "active" => {
            if segments.len() > 2 {
                Some("scalar theme.active setting must not contain nested keys".to_string())
            } else {
                None
            }
        }
        "aliases" => validate_theme_alias_path(segments, 2),
        "colors" => validate_theme_color_path(segments, 2),
        _ => None,
    }
}

/// Runs the validate themes path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_themes_path(segments: &[&str]) -> Option<String> {
    if segments.len() == 1 {
        return None;
    }
    let theme_name = segments[1];
    if !valid_color_alias_name(theme_name) {
        return Some("theme name must be an identifier".to_string());
    }
    if segments.len() == 2 {
        return None;
    }
    match segments[2] {
        "aliases" => validate_theme_alias_path(segments, 3),
        "colors" => validate_theme_color_path(segments, 3),
        _ => Some("unknown custom theme configuration key".to_string()),
    }
}

/// Runs the validate theme alias path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_theme_alias_path(segments: &[&str], alias_index: usize) -> Option<String> {
    if segments.len() == alias_index {
        return None;
    }
    let alias = segments[alias_index];
    if !valid_color_alias_name(alias) {
        return Some("theme alias must be an identifier".to_string());
    }
    if segments.len() > alias_index + 1 {
        return Some("scalar theme alias must not contain nested keys".to_string());
    }
    None
}

/// Runs the validate theme color path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_theme_color_path(segments: &[&str], slot_index: usize) -> Option<String> {
    if segments.len() == slot_index {
        return None;
    }
    let slot = segments[slot_index];
    if !UI_COLOR_SLOT_NAMES.contains(&slot) {
        return Some("unknown theme color slot".to_string());
    }
    if segments.len() > slot_index + 1 {
        return Some("scalar theme color setting must not contain nested keys".to_string());
    }
    None
}

/// Runs the validate provider path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_provider_path(segments: &[&str]) -> Option<String> {
    if segments.len() <= 2 {
        return None;
    }
    let key = segments[2];
    if !PROVIDER_KEYS.contains(&key) {
        return Some("unknown provider configuration key".to_string());
    }
    if key == "options" {
        return None;
    }
    if segments.len() > 3 {
        return Some("scalar provider setting must not contain nested keys".to_string());
    }
    None
}

/// Runs the validate model profile path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_model_profile_path(segments: &[&str]) -> Option<String> {
    if segments.len() <= 2 {
        return None;
    }
    let key = segments[2];
    if !MODEL_PROFILE_KEYS.contains(&key) {
        return Some("unknown model profile configuration key".to_string());
    }
    if key == "provider_options" {
        return None;
    }
    if segments.len() > 3 {
        return Some("scalar model profile setting must not contain nested keys".to_string());
    }
    None
}

/// Runs the validate model preset path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_model_preset_path(segments: &[&str]) -> Option<String> {
    if segments.len() <= 2 {
        return None;
    }
    let key = segments[2];
    if !MODEL_PRESET_KEYS.contains(&key) {
        return Some("unknown model preset configuration key".to_string());
    }
    if segments.len() > 3 {
        return Some("scalar model preset setting must not contain nested keys".to_string());
    }
    None
}

/// Runs the validate subagent profile path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_subagent_profile_path(segments: &[&str]) -> Option<String> {
    if segments.len() <= 2 {
        return None;
    }
    let key = segments[2];
    if !SUBAGENT_PROFILE_KEYS.contains(&key) {
        return Some("unknown subagent profile configuration key".to_string());
    }
    if key == "shell_env" {
        return None;
    }
    if segments.len() > 3 {
        return Some("scalar subagent profile setting must not contain nested keys".to_string());
    }
    None
}

/// Runs the validate personality profile path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_personality_profile_path(segments: &[&str]) -> Option<String> {
    if segments.len() <= 2 {
        return None;
    }
    let key = segments[2];
    if !PERSONALITY_PROFILE_KEYS.contains(&key) {
        return Some("unknown personality profile configuration key".to_string());
    }
    if segments.len() > 3 {
        return Some("scalar personality profile setting must not contain nested keys".to_string());
    }
    None
}

/// Runs the validate hook path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_hook_path(segments: &[&str]) -> Option<String> {
    if segments.len() <= 2 {
        return None;
    }
    let key = segments[2];
    if !HOOK_KEYS.contains(&key) {
        return Some("unknown hook configuration key".to_string());
    }
    if matches!(key, "env" | "match" | "matches") {
        return None;
    }
    if segments.len() > 3 {
        return Some("scalar hook setting must not contain nested keys".to_string());
    }
    None
}

/// Runs the contains secret material operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn contains_secret_material(path: &str, scope: ConfigScope) -> bool {
    let lower = path.to_ascii_lowercase();
    let secret_segment = lower.split('.').any(|segment| {
        matches!(
            segment,
            "token" | "api_key" | "secret" | "password" | "access_token" | "refresh_token"
        )
    });

    let mcp_inline_secret = lower.starts_with("mcp_servers.")
        && lower
            .split('.')
            .collect::<Vec<_>>()
            .windows(2)
            .any(|window| {
                matches!(window[0], "env" | "http_headers")
                    && (window[1].contains("token")
                        || window[1].contains("secret")
                        || window[1].contains("password")
                        || window[1].contains("authorization")
                        || window[1].contains("cookie")
                        || window[1].contains("key"))
            });

    ((scope == ConfigScope::ProjectOverlay || lower.starts_with("auth.")) && secret_segment)
        || mcp_inline_secret
}

/// Runs the validate mcp server path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_mcp_server_path(path: &str) -> Option<String> {
    let segments = path.split('.').collect::<Vec<_>>();
    if segments.first().copied() != Some("mcp_servers") {
        return None;
    }
    if segments.len() <= 2 {
        return None;
    }
    let key = segments[2];
    if !MCP_SERVER_KEYS.contains(&key) {
        return Some("unknown MCP server configuration key".to_string());
    }
    if matches!(
        key,
        "env" | "http_headers" | "tool_approvals" | "external_capability"
    ) {
        return None;
    }
    if segments.len() > 3 {
        return Some("scalar MCP server setting must not contain nested keys".to_string());
    }
    None
}

/// Runs the validate permissions path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_permissions_path(path: &str) -> Option<String> {
    let segments = path.split('.').collect::<Vec<_>>();
    if segments.first().copied() != Some("permissions") {
        return None;
    }
    if segments.len() == 1 {
        return None;
    }
    let key = segments[1];
    if !PERMISSION_KEYS.contains(&key) {
        return Some("unknown permissions configuration key".to_string());
    }
    if matches!(
        key,
        "command_rules" | "session_command_rules" | "global_command_rules"
    ) {
        if segments.len() <= 2 {
            return None;
        }
        let rule_key = segments[2];
        if !COMMAND_RULE_KEYS.contains(&rule_key) {
            return Some("unknown command rule configuration key".to_string());
        }
        if matches!(
            rule_key,
            "argument_policy" | "executable_policy" | "examples"
        ) {
            return None;
        }
        if segments.len() > 3 {
            return Some("scalar command rule setting must not contain nested keys".to_string());
        }
    } else if segments.len() > 2 {
        return Some("scalar permissions setting must not contain nested keys".to_string());
    }
    None
}

/// Runs the validate permission value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_permission_value(path: &str, value: &str) -> Option<String> {
    if command_rule_field(path, "decision")
        && !matches!(value, "allow" | "prompt" | "forbid" | "deny")
    {
        return Some("unsupported command rule decision".to_string());
    }
    if command_rule_field(path, "scope")
        && !matches!(value, "session" | "project" | "user" | "managed")
    {
        return Some(
            "unsupported persisted command rule scope; use session, project, user, or managed"
                .to_string(),
        );
    }
    if command_rule_field(path, "match") && !matches!(value, "prefix" | "exact" | "exact_sha256") {
        return Some("unsupported command rule match kind".to_string());
    }
    if command_rule_field(path, "exact_sha256")
        && (value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Some(
            "exact_sha256 command rule digest must be 64 hexadecimal characters".to_string(),
        );
    }
    if command_rule_field(path, "shell_classification")
        && (value.is_empty()
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')))
    {
        return Some(
            "command rule shell_classification must be non-empty ASCII [A-Za-z0-9._-]".to_string(),
        );
    }
    None
}

/// Carries Command Rule Match Kind state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CommandRuleMatchKind {
    /// Represents the Prefix case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Prefix,
    /// Represents the Exact case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Exact,
    /// Represents the Exact Sha256 case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ExactSha256,
}

/// Carries Command Rule Example Spec state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone)]
pub(super) struct CommandRuleExampleSpec {
    /// Stores the path value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) path: String,
    /// Stores the pattern value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pattern: Vec<String>,
    /// Stores the match kind value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) match_kind: CommandRuleMatchKind,
    /// Stores the digest hex value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) digest_hex: Option<String>,
    /// Stores the shell classification value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) shell_classification: Option<String>,
    /// Stores the match examples value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) match_examples: Vec<String>,
    /// Stores the not match examples value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) not_match_examples: Vec<String>,
}

/// Runs the validate command rule examples operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_command_rule_examples(
    format: ConfigFormat,
    text: &str,
) -> Vec<ConfigDiagnostic> {
    command_rule_example_specs(format, text)
        .into_iter()
        .flat_map(|rule| {
            let mut diagnostics = Vec::new();
            for example in &rule.match_examples {
                if !command_rule_example_matches(&rule, example) {
                    diagnostics.push(ConfigDiagnostic {
                        path: format!("{}.match_examples", rule.path),
                        message: "command rule match_examples must match the configured rule"
                            .to_string(),
                    });
                }
            }
            for example in &rule.not_match_examples {
                if command_rule_example_matches(&rule, example) {
                    diagnostics.push(ConfigDiagnostic {
                        path: format!("{}.not_match_examples", rule.path),
                        message:
                            "command rule not_match_examples must not match the configured rule"
                                .to_string(),
                    });
                }
            }
            diagnostics
        })
        .collect()
}

/// Runs the command rule example specs operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn command_rule_example_specs(
    format: ConfigFormat,
    text: &str,
) -> Vec<CommandRuleExampleSpec> {
    let Some(root) = config_text_to_json_value(format, text) else {
        return Vec::new();
    };
    let Some(permissions) = root
        .get("permissions")
        .and_then(serde_json::Value::as_object)
    else {
        return Vec::new();
    };
    let mut specs = Vec::new();
    for bucket in [
        "command_rules",
        "session_command_rules",
        "global_command_rules",
    ] {
        let Some(rules) = permissions
            .get(bucket)
            .and_then(serde_json::Value::as_array)
        else {
            continue;
        };
        for rule in rules {
            let Some(object) = rule.as_object() else {
                continue;
            };
            let Some(pattern) = object.get("pattern").and_then(command_rule_pattern_value) else {
                continue;
            };
            let match_kind = object
                .get("match")
                .and_then(serde_json::Value::as_str)
                .and_then(parse_command_rule_match_kind)
                .unwrap_or(CommandRuleMatchKind::Prefix);
            let match_examples = object
                .get("match_examples")
                .or_else(|| object.get("examples"))
                .map(command_rule_examples_value)
                .unwrap_or_default();
            let not_match_examples = object
                .get("not_match_examples")
                .map(command_rule_examples_value)
                .unwrap_or_default();
            if match_examples.is_empty() && not_match_examples.is_empty() {
                continue;
            }
            specs.push(CommandRuleExampleSpec {
                path: format!("permissions.{bucket}"),
                pattern,
                match_kind,
                digest_hex: object
                    .get("exact_sha256")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
                shell_classification: object
                    .get("shell_classification")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
                match_examples,
                not_match_examples,
            });
        }
    }
    specs
}

/// Runs the config text to json value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn config_text_to_json_value(
    format: ConfigFormat,
    text: &str,
) -> Option<serde_json::Value> {
    parse_config_json_value_best_effort(format, text)
}

/// Runs the command rule pattern value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn command_rule_pattern_value(value: &serde_json::Value) -> Option<Vec<String>> {
    match value {
        serde_json::Value::Array(values) => values
            .iter()
            .map(|value| value.as_str().map(ToOwned::to_owned))
            .collect(),
        serde_json::Value::String(value) => split_example_words(value),
        _ => None,
    }
}

/// Runs the command rule examples value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn command_rule_examples_value(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::Array(values) => values
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
            .collect(),
        serde_json::Value::String(value) => vec![value.clone()],
        _ => Vec::new(),
    }
}

/// Runs the parse command rule match kind operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_command_rule_match_kind(value: &str) -> Option<CommandRuleMatchKind> {
    match value {
        "prefix" => Some(CommandRuleMatchKind::Prefix),
        "exact" => Some(CommandRuleMatchKind::Exact),
        "exact_sha256" => Some(CommandRuleMatchKind::ExactSha256),
        _ => None,
    }
}

/// Runs the command rule example matches operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn command_rule_example_matches(rule: &CommandRuleExampleSpec, example: &str) -> bool {
    match rule.match_kind {
        CommandRuleMatchKind::Prefix => {
            split_example_words(example).is_some_and(|tokens| tokens.starts_with(&rule.pattern))
        }
        CommandRuleMatchKind::Exact => {
            split_example_words(example).is_some_and(|tokens| tokens == rule.pattern)
        }
        CommandRuleMatchKind::ExactSha256 => {
            let Some(digest_hex) = &rule.digest_hex else {
                return false;
            };
            let Some(shell_classification) = &rule.shell_classification else {
                return false;
            };
            let normalized = normalize_exact_command_text(example, false);
            exact_command_sha256(shell_classification, &normalized) == *digest_hex
        }
    }
}

/// Runs the split example words operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn split_example_words(input: &str) -> Option<Vec<String>> {
    /// Carries Quote state for this subsystem.
    ///
    /// The type keeps related data explicit so callers can inspect and move
    /// structured runtime state without parsing display text.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Quote {
        /// Represents the Ground case for this enumeration.
        ///
        /// Callers use this variant to describe one explicit state or command path
        /// without relying on stringly typed status values.
        Ground,
        /// Represents the Single case for this enumeration.
        ///
        /// Callers use this variant to describe one explicit state or command path
        /// without relying on stringly typed status values.
        Single,
        /// Represents the Double case for this enumeration.
        ///
        /// Callers use this variant to describe one explicit state or command path
        /// without relying on stringly typed status values.
        Double,
    }

    let mut quote = Quote::Ground;
    let mut escaped = false;
    let mut current = String::new();
    let mut tokens = Vec::new();

    for ch in input.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match (quote, ch) {
            (Quote::Ground, '\\') | (Quote::Double, '\\') => escaped = true,
            (Quote::Ground, '\'') => quote = Quote::Single,
            (Quote::Single, '\'') => quote = Quote::Ground,
            (Quote::Ground, '"') => quote = Quote::Double,
            (Quote::Double, '"') => quote = Quote::Ground,
            (Quote::Ground, ch) if ch.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }

    if escaped || quote != Quote::Ground {
        return None;
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    (!tokens.is_empty()).then_some(tokens)
}

/// Runs the command rule field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn command_rule_field(path: &str, field: &str) -> bool {
    let segments = path.split('.').collect::<Vec<_>>();
    segments.len() == 3
        && segments[0] == "permissions"
        && matches!(
            segments[1],
            "command_rules" | "session_command_rules" | "global_command_rules"
        )
        && segments[2] == field
}
