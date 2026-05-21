//! Config Validation implementation.
//!
//! This module owns the config validation boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    BASELINE_TOP_LEVEL_KEYS, BTreeMap, ConfigDiagnostic, ConfigFormat, ConfigLayer, ConfigMutation,
    ConfigMutationPlan, ConfigScope, ConfigValidation, ConfigValue, EffectiveConfig, MezError,
    Path, Result, contains_secret_material, extract_config_values, extract_json_paths,
    extract_toml_paths, extract_yaml_paths, format_diagnostics, fs, mutate_json_text,
    mutate_toml_text, mutate_yaml_text, parse_mutation_path, reject_container_target,
    reject_unsupported_mutation_path, validate_command_rule_examples, validate_known_schema_path,
    validate_mcp_server_path, validate_permission_value, validate_permissions_path,
    write_private_config_file, write_private_config_file_async,
};
use crate::terminal::{parse_hex_color, valid_color_alias_name};

// Config file and text validation entry points.

/// Runs the validate config file operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn validate_config_file(path: &Path, scope: ConfigScope) -> Result<ConfigValidation> {
    let format = ConfigFormat::from_path(path)?;
    let text = fs::read_to_string(path)?;
    let mut validation = validate_config_text(format, &text, scope);
    for diagnostic in &mut validation.diagnostics {
        if diagnostic.path == "$" {
            diagnostic.path = path.display().to_string();
        }
    }
    Ok(validation)
}

/// Validate a config file read through Tokio filesystem APIs.
pub async fn validate_config_file_async(
    path: &Path,
    scope: ConfigScope,
) -> Result<ConfigValidation> {
    let format = ConfigFormat::from_path(path)?;
    let text = tokio::fs::read_to_string(path).await?;
    let mut validation = validate_config_text(format, &text, scope);
    for diagnostic in &mut validation.diagnostics {
        if diagnostic.path == "$" {
            diagnostic.path = path.display().to_string();
        }
    }
    Ok(validation)
}

/// Build a validated text update for a conservative set or unset operation.
///
/// The planner handles TOML, YAML, and JSON text using a deliberately narrow
/// subset: scalar sets and scalar or container unsets of up to three segments.
/// It rejects container sets, command rule arrays, secret-bearing paths caught
/// by validation, and any proposed result that fails schema validation.
pub fn plan_config_mutation(
    format: ConfigFormat,
    text: &str,
    scope: ConfigScope,
    mutation: ConfigMutation,
) -> Result<ConfigMutationPlan> {
    let segments = parse_mutation_path(&mutation.path)?;
    reject_unsupported_mutation_path(&segments)?;
    reject_container_target(format, text, &segments, &mutation.operation)?;

    let mutated = match format {
        ConfigFormat::Toml => mutate_toml_text(text, &segments, &mutation.operation)?,
        ConfigFormat::Yaml => mutate_yaml_text(text, &segments, &mutation.operation)?,
        ConfigFormat::Json => mutate_json_text(text, &segments, &mutation.operation)?,
    };
    let validation = validate_config_text(format, &mutated, scope);
    if !validation.valid {
        return Err(MezError::config(format!(
            "configuration mutation rejected; proposed config is invalid: {}",
            format_diagnostics(&validation.diagnostics)
        )));
    }

    let changed = mutated != text;
    Ok(ConfigMutationPlan {
        format,
        scope,
        path: mutation.path,
        operation: mutation.operation,
        text: mutated,
        validation,
        changed,
        reload_required: changed,
    })
}

/// Apply a validated config mutation to a file while preserving private config
/// file posture.
pub fn persist_config_mutation(
    path: &Path,
    scope: ConfigScope,
    mutation: ConfigMutation,
) -> Result<ConfigMutationPlan> {
    let format = ConfigFormat::from_path(path)?;
    let text = fs::read_to_string(path)?;
    let plan = plan_config_mutation(format, &text, scope, mutation)?;
    if plan.changed {
        write_private_config_file(path, &plan.text)?;
    }
    Ok(plan)
}

/// Persist already-mutated configuration text after validating the complete
/// replacement document against the selected config scope.
///
/// This is used by callers that need to batch several conservative scalar
/// mutations into one atomic private-file write. The function preserves the
/// same private config file posture as individual mutation persistence and
/// rejects invalid replacement text before touching disk.
pub fn persist_config_text(path: &Path, scope: ConfigScope, text: &str) -> Result<()> {
    let format = ConfigFormat::from_path(path)?;
    let validation = validate_config_text(format, text, scope);
    if !validation.valid {
        return Err(MezError::config(format!(
            "configuration write rejected; proposed config is invalid: {}",
            format_diagnostics(&validation.diagnostics)
        )));
    }
    write_private_config_file(path, text)
}

/// Apply a validated config mutation using Tokio filesystem APIs.
pub async fn persist_config_mutation_async(
    path: &Path,
    scope: ConfigScope,
    mutation: ConfigMutation,
) -> Result<ConfigMutationPlan> {
    let format = ConfigFormat::from_path(path)?;
    let text = tokio::fs::read_to_string(path).await?;
    let plan = plan_config_mutation(format, &text, scope, mutation)?;
    if plan.changed {
        write_private_config_file_async(path, &plan.text).await?;
    }
    Ok(plan)
}

/// Runs the validate config text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn validate_config_text(
    format: ConfigFormat,
    text: &str,
    scope: ConfigScope,
) -> ConfigValidation {
    let mut diagnostics = validate_config_syntax(format, text);
    if !diagnostics.is_empty() {
        diagnostics.sort_by(|left, right| left.path.cmp(&right.path));
        diagnostics.dedup();
        return ConfigValidation::from_diagnostics(diagnostics);
    }

    let paths = match format {
        ConfigFormat::Toml => extract_toml_paths(text),
        ConfigFormat::Yaml => extract_yaml_paths(text),
        ConfigFormat::Json => extract_json_paths(text),
    };
    let values = extract_config_values(format, text);

    for path in &paths {
        if let Some(top_level) = path.split('.').next()
            && !BASELINE_TOP_LEVEL_KEYS.contains(&top_level)
        {
            diagnostics.push(ConfigDiagnostic {
                path: top_level.to_string(),
                message: "unknown top-level configuration key".to_string(),
            });
        }

        if path == "session.default_command" {
            diagnostics.push(ConfigDiagnostic {
                path: path.clone(),
                message: "session.default_command is not supported; provide explicit pane commands"
                    .to_string(),
            });
        }

        if let Some(message) = validate_known_schema_path(path) {
            diagnostics.push(ConfigDiagnostic {
                path: path.clone(),
                message,
            });
        }

        if matches!(
            path.as_str(),
            "shell.path" | "shell.executable" | "shell.command"
        ) {
            diagnostics.push(ConfigDiagnostic {
                path: path.clone(),
                message: "configuration must not override the resolved shell path".to_string(),
            });
        }

        if contains_secret_material(path, scope) {
            diagnostics.push(ConfigDiagnostic {
                path: path.clone(),
                message: "configuration must not contain authentication secret material"
                    .to_string(),
            });
        }

        if let Some(message) = validate_mcp_server_path(path) {
            diagnostics.push(ConfigDiagnostic {
                path: path.clone(),
                message,
            });
        }

        if let Some(message) = validate_permissions_path(path) {
            diagnostics.push(ConfigDiagnostic {
                path: path.clone(),
                message,
            });
        }
    }

    for (path, value) in values {
        if path == "history.lines"
            || path == "history.rotate_lines"
            || path == "agents.max_concurrent_agents"
            || path == "agents.max_root_subagents"
            || path == "agents.max_subagents_per_subagent"
            || path == "agents.max_subagent_panes_per_window"
            || path == "agents.max_depth"
            || path == "agents.action_failure_retry_limit"
        {
            if let Some(message) = validate_positive_usize_value(&value, &path) {
                diagnostics.push(ConfigDiagnostic { path, message });
            }
        } else if path == "agents.compaction_raw_retention_percent" {
            match value.parse::<usize>() {
                Ok(percent) if (1..=100).contains(&percent) => {}
                _ => diagnostics.push(ConfigDiagnostic {
                    path,
                    message:
                        "agents.compaction_raw_retention_percent must be an integer from 1 to 100"
                            .to_string(),
                }),
            }
        } else if let Some(message) = validate_terminal_value(&path, &value) {
            diagnostics.push(ConfigDiagnostic { path, message });
        } else if let Some(message) = validate_frame_value(&path, &value) {
            diagnostics.push(ConfigDiagnostic { path, message });
        } else if let Some(message) = validate_theme_value(&path, &value) {
            diagnostics.push(ConfigDiagnostic { path, message });
        } else if is_approval_policy_value_path(&path)
            && !matches!(value.as_str(), "ask" | "auto-allow" | "full-access")
        {
            diagnostics.push(ConfigDiagnostic {
                path,
                message: "unsupported approval policy; use ask, auto-allow, or full-access"
                    .to_string(),
            });
        } else if path == "agents.subagent_wait_policy"
            && !matches!(
                value.as_str(),
                "join" | "join-and-wait" | "wait" | "detach" | "fire-and-forget"
            )
        {
            diagnostics.push(ConfigDiagnostic {
                path,
                message: "unsupported subagent wait policy; use join or detach".to_string(),
            });
        } else if path == "agents.auto_compact_threshold" {
            match value.parse::<f64>() {
                Ok(threshold) if threshold > 0.0 && threshold <= 1.0 => {}
                _ => diagnostics.push(ConfigDiagnostic {
                    path,
                    message: "auto compact threshold must be greater than 0 and at most 1"
                        .to_string(),
                }),
            }
        } else if path == "agents.auto_sizing.fallback_policy" && value != "use-default-profile" {
            diagnostics.push(ConfigDiagnostic {
                path,
                message: "unsupported auto sizing fallback policy; use use-default-profile"
                    .to_string(),
            });
        } else if path.ends_with(".context_window_tokens")
            || path.ends_with(".context_limit_tokens")
            || path.ends_with(".max_output_tokens")
        {
            if let Some(message) = validate_positive_usize_value(&value, &path) {
                diagnostics.push(ConfigDiagnostic { path, message });
            }
        } else if path == "permissions.preset" && !matches!(value.as_str(), "read-only" | "auto") {
            diagnostics.push(ConfigDiagnostic {
                path,
                message:
                    "unsupported permission preset; use read-only, auto, or explicit bypass mode"
                        .to_string(),
            });
        } else if path == "permissions.bypass_mode" && value == "true" {
            diagnostics.push(ConfigDiagnostic {
                path,
                message: "permissions.bypass_mode cannot be enabled from configuration; use explicit approval bypass activation".to_string(),
            });
        } else if let Some(message) = validate_permission_value(&path, &value) {
            diagnostics.push(ConfigDiagnostic { path, message });
        }
    }
    diagnostics.extend(validate_command_rule_examples(format, text));

    diagnostics.sort_by(|left, right| left.path.cmp(&right.path));
    diagnostics.dedup();
    ConfigValidation::from_diagnostics(diagnostics)
}

/// Runs the is approval policy value path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn is_approval_policy_value_path(path: &str) -> bool {
    path == "permissions.approval_policy"
        || (path.starts_with("model_profiles.")
            && path.ends_with(".approval_policy")
            && path.split('.').count() == 3)
}

/// Runs the validate positive usize value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_positive_usize_value(value: &str, path: &str) -> Option<String> {
    match value.parse::<usize>() {
        Ok(parsed) if parsed > 0 => None,
        _ => Some(format!("{path} must be a positive integer")),
    }
}

/// Runs the validate terminal value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_terminal_value(path: &str, value: &str) -> Option<String> {
    match path {
        "terminal.term" => {
            if value.trim().is_empty() || value.bytes().any(|byte| byte.is_ascii_control()) {
                Some("terminal.term must be a non-empty printable string".to_string())
            } else if is_host_terminal_identity(value) {
                Some(
                    "terminal.term must identify Mezzanine or a safe fallback terminfo entry, not the host terminal identity"
                        .to_string(),
                )
            } else {
                None
            }
        }
        "terminal.profile" => {
            if matches!(value, "xterm-compatible" | "dumb") {
                None
            } else {
                Some("unsupported terminal profile".to_string())
            }
        }
        "terminal.cursor_style" => {
            if matches!(value, "block" | "underline" | "bar") {
                None
            } else {
                Some("terminal.cursor_style must be block, underline, or bar".to_string())
            }
        }
        "terminal.cursor_blink" => {
            if matches!(value, "true" | "false") {
                None
            } else {
                Some("terminal.cursor_blink must be true or false".to_string())
            }
        }
        "terminal.cursor_blink_interval_ms" | "terminal.resize_debounce_ms" => {
            match value.parse::<u64>() {
                Ok(interval) if interval > 0 => None,
                _ => Some(format!("{path} must be a positive integer")),
            }
        }
        _ => None,
    }
}

/// Runs the validate frame value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_frame_value(path: &str, value: &str) -> Option<String> {
    match path {
        "frames.window.enabled" | "frames.pane.enabled" => {
            if matches!(value, "true" | "false") {
                None
            } else {
                Some(format!("{path} must be true or false"))
            }
        }
        "frames.window.position" | "frames.pane.position" => {
            if matches!(value, "top" | "bottom" | "border") {
                None
            } else {
                Some(format!("{path} must be top, bottom, or border"))
            }
        }
        "frames.window.style" | "frames.pane.style" => {
            if matches!(
                value,
                "default" | "bold" | "underline" | "inverse" | "reverse"
            ) {
                None
            } else {
                Some(format!(
                    "{path} must be default, bold, underline, inverse, or reverse"
                ))
            }
        }
        _ => None,
    }
}

/// Runs the validate theme value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_theme_value(path: &str, value: &str) -> Option<String> {
    if path == "theme.active" {
        if value.trim().is_empty() || !valid_color_alias_name(value) {
            return Some("theme.active must be a non-empty theme identifier".to_string());
        }
        return None;
    }
    if theme_alias_value_path(path) {
        if parse_hex_color(value).is_none() {
            return Some("theme aliases must be #rgb or #rrggbb hex colors".to_string());
        }
        return None;
    }
    if theme_color_value_path(path) {
        if parse_hex_color(value).is_some() || valid_color_alias_name(value) {
            return None;
        }
        return Some("theme colors must be hex colors or alias names".to_string());
    }
    None
}

/// Runs the theme alias value path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn theme_alias_value_path(path: &str) -> bool {
    let segments = path.split('.').collect::<Vec<_>>();
    matches!(segments.as_slice(), ["theme", "aliases", _])
        || matches!(segments.as_slice(), ["themes", _, "aliases", _])
}

/// Runs the theme color value path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn theme_color_value_path(path: &str) -> bool {
    let segments = path.split('.').collect::<Vec<_>>();
    matches!(segments.as_slice(), ["theme", "colors", _])
        || matches!(segments.as_slice(), ["themes", _, "colors", _])
}

/// Runs the is host terminal identity operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn is_host_terminal_identity(value: &str) -> bool {
    matches!(value, "xterm" | "xterm-256color")
}

/// Runs the compose effective config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn compose_effective_config(layers: &[ConfigLayer]) -> Result<EffectiveConfig> {
    let mut values = BTreeMap::new();
    let mut diagnostics = Vec::new();
    let mut applied_layers = Vec::new();
    let mut skipped_layers = Vec::new();

    for layer in layers {
        let validation = validate_config_text(layer.format, &layer.text, layer.scope);
        if !validation.valid {
            return Err(MezError::config(format!(
                "configuration layer `{}` is invalid: {}",
                layer.name,
                validation
                    .diagnostics
                    .iter()
                    .map(|diagnostic| format!("{}: {}", diagnostic.path, diagnostic.message))
                    .collect::<Vec<_>>()
                    .join("; ")
            )));
        }

        if layer.scope == ConfigScope::ProjectOverlay && !layer.trusted {
            diagnostics.push(ConfigDiagnostic {
                path: layer
                    .path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| layer.name.clone()),
                message: "project overlay is pending trust and was not applied".to_string(),
            });
            skipped_layers.push(layer.name.clone());
            continue;
        }

        for (path, value) in extract_config_values(layer.format, &layer.text) {
            values.insert(
                path,
                ConfigValue {
                    value,
                    source_layer: layer.name.clone(),
                },
            );
        }
        applied_layers.push(layer.name.clone());
    }

    Ok(EffectiveConfig {
        values,
        diagnostics,
        applied_layers,
        skipped_layers,
    })
}
/// Runs the validate config syntax operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_config_syntax(format: ConfigFormat, text: &str) -> Vec<ConfigDiagnostic> {
    match format {
        ConfigFormat::Toml => match text.parse::<toml::Value>() {
            Ok(toml::Value::Table(_)) => Vec::new(),
            Ok(_) => vec![ConfigDiagnostic {
                path: "$".to_string(),
                message: "TOML configuration root must be a table".to_string(),
            }],
            Err(error) => vec![ConfigDiagnostic {
                path: "$".to_string(),
                message: format!("invalid TOML configuration syntax: {error}"),
            }],
        },
        ConfigFormat::Yaml => match serde_yml::from_str::<serde_yml::Value>(text) {
            Ok(serde_yml::Value::Mapping(_)) => Vec::new(),
            Ok(serde_yml::Value::Null) if text.trim().is_empty() => Vec::new(),
            Ok(_) => vec![ConfigDiagnostic {
                path: "$".to_string(),
                message: "YAML configuration root must be a mapping".to_string(),
            }],
            Err(error) => vec![ConfigDiagnostic {
                path: "$".to_string(),
                message: format!("invalid YAML configuration syntax: {error}"),
            }],
        },
        ConfigFormat::Json => match serde_json::from_str::<serde_json::Value>(text) {
            Ok(serde_json::Value::Object(_)) => Vec::new(),
            Ok(_) => vec![ConfigDiagnostic {
                path: "$".to_string(),
                message: "JSON configuration root must be an object".to_string(),
            }],
            Err(error) => vec![ConfigDiagnostic {
                path: "$".to_string(),
                message: format!("invalid JSON configuration syntax: {error}"),
            }],
        },
    }
}
