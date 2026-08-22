//! Config Validation implementation.
//!
//! This module owns the config validation boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

#[cfg(test)]
use super::write_private_config_file_async;
use super::{
    BASELINE_TOP_LEVEL_KEYS, BTreeMap, CURRENT_CONFIG_SCHEMA_VERSION, ConfigBatchMutationPlan,
    ConfigDiagnostic, ConfigFormat, ConfigLayer, ConfigMutation, ConfigMutationOperation,
    ConfigMutationPlan, ConfigMutationValue, ConfigScope, ConfigValidation, ConfigValue,
    EffectiveConfig, MezError, Path, Result, contains_secret_material, extract_config_values,
    extract_json_paths, extract_toml_paths, extract_yaml_paths, format_diagnostics, fs,
    mutate_json_text, mutate_toml_text, mutate_yaml_text, parse_config_json_value,
    parse_config_schema_version, parse_mutation_path, reject_container_target,
    reject_unsupported_mutation_path, validate_command_rule_effects,
    validate_command_rule_examples, validate_known_schema_path, validate_mcp_server_path,
    validate_permission_value, validate_permissions_path, write_private_config_file,
};
use mez_mux::theme::{parse_hex_color, valid_color_alias_name};

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
#[cfg(test)]
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

    let mut mutated = match format {
        ConfigFormat::Toml => mutate_toml_text(text, &segments, &mutation.operation)?,
        ConfigFormat::Yaml => mutate_yaml_text(text, &segments, &mutation.operation)?,
        ConfigFormat::Json => mutate_json_text(text, &segments, &mutation.operation)?,
    };
    if scope == ConfigScope::ProjectOverlay {
        mutated = materialize_project_overlay_schema_version(format, &mutated)?;
    }
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

/// Builds one validated final document from an ordered mutation batch.
///
/// Every mutation is structurally checked and applied in memory, but schema
/// validation runs only after the complete batch has been composed. Callers
/// can therefore change related fields atomically without exposing invalid
/// intermediate documents to persistence.
pub fn plan_config_mutations(
    format: ConfigFormat,
    text: &str,
    scope: ConfigScope,
    mutations: Vec<ConfigMutation>,
) -> Result<ConfigBatchMutationPlan> {
    let mut mutated = text.to_string();
    let mut mutation_changed = Vec::with_capacity(mutations.len());
    for mutation in &mutations {
        let segments = parse_mutation_path(&mutation.path)?;
        reject_unsupported_mutation_path(&segments)?;
        reject_container_target(format, &mutated, &segments, &mutation.operation)?;
        let next = match format {
            ConfigFormat::Toml => mutate_toml_text(&mutated, &segments, &mutation.operation)?,
            ConfigFormat::Yaml => mutate_yaml_text(&mutated, &segments, &mutation.operation)?,
            ConfigFormat::Json => mutate_json_text(&mutated, &segments, &mutation.operation)?,
        };
        mutation_changed.push(next != mutated);
        mutated = next;
    }
    if scope == ConfigScope::ProjectOverlay {
        mutated = materialize_project_overlay_schema_version(format, &mutated)?;
    }
    let validation = validate_config_text(format, &mutated, scope);
    if !validation.valid {
        return Err(MezError::config(format!(
            "configuration mutation batch rejected; proposed config is invalid: {}",
            format_diagnostics(&validation.diagnostics)
        )));
    }
    let changed = mutated != text;
    Ok(ConfigBatchMutationPlan {
        format,
        scope,
        mutations,
        mutation_changed,
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
    let text = if scope == ConfigScope::ProjectOverlay {
        materialize_project_overlay_schema_version(format, text)?
    } else {
        text.to_string()
    };
    let validation = validate_config_text(format, &text, scope);
    if !validation.valid {
        return Err(MezError::config(format!(
            "configuration write rejected; proposed config is invalid: {}",
            format_diagnostics(&validation.diagnostics)
        )));
    }
    write_private_config_file(path, &text)
}

/// Apply a validated config mutation using Tokio filesystem APIs.
#[cfg(test)]
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

/// Ensures project-overlay writes declare the current schema version.
///
/// Direct validation still rejects missing or stale overlay versions, but
/// runtime-owned persistence paths can safely materialize the current version
/// when creating or extending a project overlay document.
fn materialize_project_overlay_schema_version(format: ConfigFormat, text: &str) -> Result<String> {
    let values = extract_config_values(format, text);
    let raw_schema_version = values.get("version").map(String::as_str);
    let parsed_schema_version =
        raw_schema_version.and_then(|value| parse_config_schema_version(Some(value)).ok());
    if parsed_schema_version == Some(CURRENT_CONFIG_SCHEMA_VERSION) {
        return Ok(text.to_string());
    }
    if raw_schema_version.is_some()
        && !matches!(parsed_schema_version, Some(version) if version < CURRENT_CONFIG_SCHEMA_VERSION)
    {
        return Ok(text.to_string());
    }
    let current_version = i64::try_from(CURRENT_CONFIG_SCHEMA_VERSION)
        .map_err(|_| MezError::config("current config schema version is too large"))?;
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            if raw_schema_version.is_some() {
                document
                    .as_table_mut()
                    .insert("version", toml_edit::value(current_version));
                Ok(document.to_string())
            } else if text.trim().is_empty() {
                Ok(format!("version = {CURRENT_CONFIG_SCHEMA_VERSION}\n"))
            } else if text.ends_with('\n') {
                Ok(format!("version = {CURRENT_CONFIG_SCHEMA_VERSION}\n{text}"))
            } else {
                Ok(format!(
                    "version = {CURRENT_CONFIG_SCHEMA_VERSION}\n{text}\n"
                ))
            }
        }
        ConfigFormat::Yaml => {
            if raw_schema_version.is_some() {
                mutate_yaml_text(
                    text,
                    &["version".to_string()],
                    &ConfigMutationOperation::Set(ConfigMutationValue::Integer(current_version)),
                )
            } else if text.trim().is_empty() {
                Ok(format!("version: {CURRENT_CONFIG_SCHEMA_VERSION}\n"))
            } else if text.ends_with('\n') {
                Ok(format!("version: {CURRENT_CONFIG_SCHEMA_VERSION}\n{text}"))
            } else {
                Ok(format!(
                    "version: {CURRENT_CONFIG_SCHEMA_VERSION}\n{text}\n"
                ))
            }
        }
        ConfigFormat::Json => {
            let mut root: serde_json::Value = serde_json::from_str(text).map_err(|error| {
                MezError::config(format!("JSON configuration parse failed: {error}"))
            })?;
            let Some(object) = root.as_object_mut() else {
                return Err(MezError::config(
                    "JSON project overlay configuration requires an object root",
                ));
            };
            object.insert(
                "version".to_string(),
                serde_json::Value::Number(CURRENT_CONFIG_SCHEMA_VERSION.into()),
            );
            serde_json::to_string_pretty(&root)
                .map(|rendered| format!("{rendered}\n"))
                .map_err(|error| {
                    MezError::config(format!("JSON configuration render failed: {error}"))
                })
        }
    }
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
    diagnostics.extend(validate_agent_turn_timeout_config(format, text));
    diagnostics.extend(validate_iroh_transport_config(format, text));
    diagnostics.extend(validate_group_whitelist_config(format, text));
    diagnostics.extend(validate_env_whitelist_config(format, text));

    let git_user_name = values.get("permissions.bubblewrap.git_user_name");
    let git_user_email = values.get("permissions.bubblewrap.git_user_email");
    if git_user_name.is_some() != git_user_email.is_some() {
        diagnostics.push(ConfigDiagnostic {
            path: "permissions.bubblewrap".to_string(),
            message: "Bubblewrap git_user_name and git_user_email must be configured together"
                .to_string(),
        });
    }

    let raw_schema_version = values.get("version").map(String::as_str);
    match parse_config_schema_version(raw_schema_version) {
        Ok(version)
            if scope == ConfigScope::ProjectOverlay
                && (raw_schema_version.is_none() || version != CURRENT_CONFIG_SCHEMA_VERSION) =>
        {
            diagnostics.push(ConfigDiagnostic {
                path: "version".to_string(),
                message: format!(
                    "project overlay configuration must declare current schema version {CURRENT_CONFIG_SCHEMA_VERSION}"
                ),
            });
        }
        Ok(version) if version <= CURRENT_CONFIG_SCHEMA_VERSION => {}
        Ok(version) => diagnostics.push(ConfigDiagnostic {
            path: "version".to_string(),
            message: format!(
                "configuration schema version {version} is newer than this mez binary supports ({CURRENT_CONFIG_SCHEMA_VERSION})"
            ),
        }),
        Err(_) => diagnostics.push(ConfigDiagnostic {
            path: "version".to_string(),
            message: "configuration schema version must be a positive integer".to_string(),
        }),
    }

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

        if scope == ConfigScope::ProjectOverlay
            && project_overlay_path_changes_execution_authority(path)
        {
            diagnostics.push(ConfigDiagnostic {
                path: path.clone(),
                message: "primary_user_only_execution_authority: project overlays must not change sandbox or execution-authority settings".to_string(),
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
        if path == "runtime.cpu_count"
            || path == "history.lines"
            || path == "history.rotate_lines"
            || path == "history.saved_sessions_limit"
            || path == "agents.max_concurrent_agents"
            || path == "agents.max_queued_turns"
            || path == "agents.max_queued_bytes"
            || path == "agents.max_root_subagents"
            || path == "agents.max_subagents_per_subagent"
            || path == "agents.max_subagent_panes_per_window"
            || path == "agents.max_depth"
            || path == "agents.action_failure_retry_limit"
            || path == "agents.turn_timeout_ms"
            || path == "agents.loop_limit"
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
        } else if path == "issues.enabled" && !matches!(value.as_str(), "true" | "false") {
            diagnostics.push(ConfigDiagnostic {
                path,
                message: "issues.enabled must be true or false".to_string(),
            });
        } else if let Some(message) = validate_terminal_value(&path, &value) {
            diagnostics.push(ConfigDiagnostic { path, message });
        } else if let Some(message) = validate_frame_value(&path, &value) {
            diagnostics.push(ConfigDiagnostic { path, message });
        } else if let Some(message) = validate_theme_value(&path, &value) {
            diagnostics.push(ConfigDiagnostic { path, message });
        } else if is_approval_policy_value_path(&path) {
            if value == "host-access"
                && (scope == ConfigScope::ProjectOverlay
                    || is_model_profile_approval_policy_path(&path))
            {
                diagnostics.push(ConfigDiagnostic {
                    path,
                    message: "user_only_host_access: host-access is allowed only as the primary user approval policy"
                        .to_string(),
                });
            } else if !matches!(
                value.as_str(),
                "ask" | "auto-allow" | "full-access" | "host-access"
            ) {
                diagnostics.push(ConfigDiagnostic {
                    path,
                    message: "unsupported approval policy; use ask, auto-allow, full-access, or host-access"
                        .to_string(),
                });
            }
        } else if path == "agents.active_turn_sleep_inhibition"
            && !matches!(value.as_str(), "disabled" | "system" | "system-and-display")
        {
            diagnostics.push(ConfigDiagnostic {
                path,
                message: "agents.active_turn_sleep_inhibition must be disabled, system, or system-and-display".to_string(),
            });
        } else if path == "agents.shell_mode" && !matches!(value.as_str(), "pane" | "native") {
            diagnostics.push(ConfigDiagnostic {
                path,
                message: "agents.shell_mode must be pane or native".to_string(),
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
        } else if path == "agents.auto_sizing.fallback_policy" && value != "use-default-profile" {
            diagnostics.push(ConfigDiagnostic {
                path,
                message: "unsupported auto sizing fallback policy; use use-default-profile"
                    .to_string(),
            });
        } else if path == "agents.auto_sizing.root_routing_policy"
            && !matches!(value.as_str(), "subagent" | "in-place")
        {
            diagnostics.push(ConfigDiagnostic {
                path,
                message: "unsupported root routing policy; use subagent or in-place".to_string(),
            });
        } else if path.ends_with(".context_window_tokens")
            || path.ends_with(".context_limit_tokens")
            || path.ends_with(".max_input_tokens")
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
    diagnostics.extend(validate_command_rule_effects(format, text));

    diagnostics.sort_by(|left, right| left.path.cmp(&right.path));
    diagnostics.dedup();
    ConfigValidation::from_diagnostics(diagnostics)
}

/// Validates schema-v68 Iroh transport policy with structured value types.
fn validate_iroh_transport_config(format: ConfigFormat, text: &str) -> Vec<ConfigDiagnostic> {
    let Ok(root) = parse_config_json_value(format, text) else {
        return Vec::new();
    };
    let Some(iroh) = root
        .get("transport")
        .and_then(serde_json::Value::as_object)
        .and_then(|transport| transport.get("iroh"))
        .and_then(serde_json::Value::as_object)
    else {
        return Vec::new();
    };
    let mut diagnostics = Vec::new();
    let mut reject = |path: &str, message: &str| {
        diagnostics.push(ConfigDiagnostic {
            path: path.to_string(),
            message: message.to_string(),
        });
    };

    for key in [
        "enabled",
        "direct_connections",
        "port_mapping",
        "proxy_from_env",
        "system_ca_store",
    ] {
        if iroh.get(key).is_some_and(|value| !value.is_boolean()) {
            reject(
                &format!("transport.iroh.{key}"),
                "Iroh transport flag must be true or false",
            );
        }
    }
    if iroh
        .get("identity")
        .is_some_and(|value| value.as_str() != Some("per_session"))
    {
        reject(
            "transport.iroh.identity",
            "transport.iroh.identity must be per_session",
        );
    }
    let address_lookup = iroh
        .get("address_lookup")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("disabled");
    if !matches!(
        address_lookup,
        "disabled" | "n0_dns" | "custom_dns" | "local"
    ) {
        reject(
            "transport.iroh.address_lookup",
            "unsupported Iroh address lookup policy",
        );
    }
    let lookup_domain = iroh
        .get("address_lookup_domain")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if iroh
        .get("address_lookup_domain")
        .is_some_and(|value| !value.is_string())
    {
        reject(
            "transport.iroh.address_lookup_domain",
            "Iroh lookup domain must be a string",
        );
    } else if address_lookup == "custom_dns"
        && (lookup_domain.is_empty()
            || !lookup_domain
                .chars()
                .all(|character| !character.is_control() && !character.is_whitespace()))
    {
        reject(
            "transport.iroh.address_lookup_domain",
            "custom DNS lookup requires a printable domain",
        );
    } else if address_lookup != "custom_dns" && !lookup_domain.is_empty() {
        reject(
            "transport.iroh.address_lookup_domain",
            "lookup domain is valid only with custom_dns",
        );
    }

    let relay_mode = iroh
        .get("relay_mode")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("disabled");
    if !matches!(relay_mode, "disabled" | "public" | "custom") {
        reject("transport.iroh.relay_mode", "unsupported Iroh relay mode");
    }
    let mut relay_urls = Vec::new();
    if let Some(value) = iroh.get("relay_urls") {
        if let Some(urls) = value.as_array() {
            for url in urls {
                match url.as_str() {
                    Some(url) => relay_urls.push(url),
                    None => reject(
                        "transport.iroh.relay_urls",
                        "relay_urls must contain only strings",
                    ),
                }
            }
        } else {
            reject(
                "transport.iroh.relay_urls",
                "relay_urls must be a string array",
            );
        }
    }
    if relay_urls.len() > 8 {
        reject(
            "transport.iroh.relay_urls",
            "relay_urls must contain at most eight URLs",
        );
    }
    if relay_urls.iter().any(|url| {
        !url.starts_with("https://")
            || url.len() <= "https://".len()
            || url.chars().any(char::is_control)
    }) {
        reject(
            "transport.iroh.relay_urls",
            "custom relay URLs must be printable HTTPS URLs",
        );
    }
    if relay_mode == "custom" && relay_urls.is_empty() {
        reject(
            "transport.iroh.relay_urls",
            "custom relay mode requires at least one relay URL",
        );
    } else if relay_mode != "custom" && !relay_urls.is_empty() {
        reject(
            "transport.iroh.relay_urls",
            "relay URLs are valid only with custom relay mode",
        );
    }
    if iroh
        .get("direct_connections")
        .and_then(serde_json::Value::as_bool)
        == Some(false)
        && relay_mode == "disabled"
    {
        reject(
            "transport.iroh.direct_connections",
            "disabling direct connections requires a relay mode",
        );
    }

    for (key, minimum, maximum) in [
        ("invitation_ttl_seconds", 30, 86_400),
        ("max_connections", 1, 1_024),
        ("max_streams_per_connection", 1, 1),
        ("setup_timeout_ms", 100, 120_000),
        ("idle_timeout_ms", 1_000, 86_400_000),
    ] {
        if let Some(value) = iroh.get(key) {
            match value.as_u64() {
                Some(value) if (minimum..=maximum).contains(&value) => {}
                _ => reject(
                    &format!("transport.iroh.{key}"),
                    &format!("transport.iroh.{key} must be an integer from {minimum} to {maximum}"),
                ),
            }
        }
    }
    diagnostics
}

/// Validates the agent-turn timeout with its structured scalar type intact.
fn validate_agent_turn_timeout_config(format: ConfigFormat, text: &str) -> Vec<ConfigDiagnostic> {
    let Ok(root) = parse_config_json_value(format, text) else {
        return Vec::new();
    };
    let Some(value) = root
        .get("agents")
        .and_then(serde_json::Value::as_object)
        .and_then(|agents| agents.get("turn_timeout_ms"))
    else {
        return Vec::new();
    };
    if value.as_u64().is_some_and(|timeout_ms| timeout_ms > 0) {
        Vec::new()
    } else {
        vec![ConfigDiagnostic {
            path: "agents.turn_timeout_ms".to_string(),
            message: "agents.turn_timeout_ms must be a positive integer".to_string(),
        }]
    }
}

/// Validates schema-v49 group whitelist names without consulting NSS.
fn validate_group_whitelist_config(format: ConfigFormat, text: &str) -> Vec<ConfigDiagnostic> {
    let Ok(root) = parse_config_json_value(format, text) else {
        return Vec::new();
    };
    let Some(value) = root
        .get("permissions")
        .and_then(serde_json::Value::as_object)
        .and_then(|permissions| permissions.get("bubblewrap"))
        .and_then(serde_json::Value::as_object)
        .and_then(|bubblewrap| bubblewrap.get("group_whitelist"))
    else {
        return Vec::new();
    };
    let path = "permissions.bubblewrap.group_whitelist";
    let Some(groups) = value.as_array() else {
        return vec![ConfigDiagnostic {
            path: path.to_string(),
            message: "group_whitelist must be a string array".to_string(),
        }];
    };
    if groups.len() > 64 {
        return vec![ConfigDiagnostic {
            path: path.to_string(),
            message: "group_whitelist must contain at most 64 names".to_string(),
        }];
    }
    let mut diagnostics = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut encoded_bytes = 0usize;
    for group in groups {
        let Some(name) = group.as_str() else {
            diagnostics.push(ConfigDiagnostic {
                path: path.to_string(),
                message: "group_whitelist must contain only strings".to_string(),
            });
            continue;
        };
        encoded_bytes = encoded_bytes.saturating_add(name.len());
        if name.is_empty() || name.chars().any(char::is_control) {
            diagnostics.push(ConfigDiagnostic {
                path: path.to_string(),
                message: "supplementary group names must be non-empty printable text".to_string(),
            });
        } else if name.bytes().all(|byte| byte.is_ascii_digit()) {
            diagnostics.push(ConfigDiagnostic {
                path: path.to_string(),
                message: "supplementary group names must not be numeric GIDs".to_string(),
            });
        } else if !seen.insert(name) {
            diagnostics.push(ConfigDiagnostic {
                path: path.to_string(),
                message: "supplementary group names must not contain duplicates".to_string(),
            });
        }
    }
    if encoded_bytes > 8 * 1024 {
        diagnostics.push(ConfigDiagnostic {
            path: path.to_string(),
            message: "group_whitelist exceeds the 8 KiB input limit".to_string(),
        });
    }
    diagnostics
}

/// Validates schema-v50 environment whitelist names without reading any environment.
fn validate_env_whitelist_config(format: ConfigFormat, text: &str) -> Vec<ConfigDiagnostic> {
    let Ok(root) = parse_config_json_value(format, text) else {
        return Vec::new();
    };
    let Some(value) = root
        .get("permissions")
        .and_then(serde_json::Value::as_object)
        .and_then(|permissions| permissions.get("bubblewrap"))
        .and_then(serde_json::Value::as_object)
        .and_then(|bubblewrap| bubblewrap.get("env_whitelist"))
    else {
        return Vec::new();
    };
    let path = "permissions.bubblewrap.env_whitelist";
    let Some(names) = value.as_array() else {
        return vec![ConfigDiagnostic {
            path: path.to_string(),
            message: "env_whitelist must be a string array".to_string(),
        }];
    };
    if names.len() > 128 {
        return vec![ConfigDiagnostic {
            path: path.to_string(),
            message: "env_whitelist must contain at most 128 names".to_string(),
        }];
    }
    let mut diagnostics = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut encoded_bytes = 0usize;
    for value in names {
        let Some(name) = value.as_str() else {
            diagnostics.push(ConfigDiagnostic {
                path: path.to_string(),
                message: "env_whitelist must contain only strings".to_string(),
            });
            continue;
        };
        encoded_bytes = encoded_bytes.saturating_add(name.len());
        let mut bytes = name.bytes();
        let valid = bytes
            .next()
            .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
            && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric());
        if !valid {
            diagnostics.push(ConfigDiagnostic {
                path: path.to_string(),
                message: "environment names must match [A-Za-z_][A-Za-z0-9_]*".to_string(),
            });
        } else if !seen.insert(name) {
            diagnostics.push(ConfigDiagnostic {
                path: path.to_string(),
                message: "environment names must not contain duplicates".to_string(),
            });
        }
    }
    if encoded_bytes > 16 * 1024 {
        diagnostics.push(ConfigDiagnostic {
            path: path.to_string(),
            message: "env_whitelist exceeds the 16 KiB name limit".to_string(),
        });
    }
    diagnostics
}

/// Runs the is approval policy value path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn is_approval_policy_value_path(path: &str) -> bool {
    path == "permissions.approval_policy" || is_model_profile_approval_policy_path(path)
}

/// Reports whether a project-overlay path could change the execution boundary.
///
/// Project configuration is writable under the trusted-project authority that
/// Bubblewrap may grant to an agent. It may therefore carry project command
/// rules, but it must not select or alter the sandbox, filesystem scopes,
/// network policy, approval behavior, or another execution boundary.
fn project_overlay_path_changes_execution_authority(path: &str) -> bool {
    matches!(
        path,
        "permissions.approval_policy"
            | "permissions.preset"
            | "permissions.sandbox"
            | "permissions.read_scopes"
            | "permissions.write_scopes"
            | "permissions.network_policy"
            | "permissions.destructive_action_policy"
            | "permissions.bypass_mode"
            | "permissions.bubblewrap"
    ) || path.starts_with("permissions.bubblewrap.")
        || path == "transport"
        || path.starts_with("transport.")
        || is_model_profile_approval_policy_path(path)
}

/// Reports whether a path is one model profile's approval policy.
fn is_model_profile_approval_policy_path(path: &str) -> bool {
    path.starts_with("model_profiles.")
        && path.ends_with(".approval_policy")
        && path.split('.').count() == 3
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
        "terminal.pane_spawn_directory" => {
            if matches!(value, "home" | "same-directory") {
                None
            } else {
                Some("terminal.pane_spawn_directory must be home or same-directory".to_string())
            }
        }
        "terminal.pane_spawn_view" => {
            if matches!(value, "shell" | "agent") {
                None
            } else {
                Some("terminal.pane_spawn_view must be shell or agent".to_string())
            }
        }
        "terminal.cursor_style" => {
            if matches!(value, "block" | "underline" | "bar") {
                None
            } else {
                Some("terminal.cursor_style must be block, underline, or bar".to_string())
            }
        }
        "terminal.emoji_width" => {
            if matches!(value, "wide" | "narrow") {
                None
            } else {
                Some("terminal.emoji_width must be wide or narrow".to_string())
            }
        }
        "terminal.cursor_blink"
        | "terminal.reduced_motion"
        | "terminal.streaming_output"
        | "terminal.enhanced_keyboard_reporting"
        | "terminal.completion_attention_flashing" => {
            if matches!(value, "true" | "false") {
                None
            } else {
                Some(format!("{path} must be true or false"))
            }
        }
        "terminal.cursor_blink_interval_ms"
        | "terminal.resize_debounce_ms"
        | "terminal.clipboard_read_timeout_ms"
        | "terminal.clipboard_read_max_bytes"
        | "terminal.shell_output_preview_lines"
        | "terminal.agent_wrap_column_cap" => match value.parse::<u64>() {
            Ok(interval) if interval > 0 => None,
            _ => Some(format!("{path} must be a positive integer")),
        },
        "terminal.render_rate_limit_fps" => match value.parse::<u64>() {
            Ok(_) => None,
            _ => Some(format!("{path} must be a non-negative integer")),
        },
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
        ConfigFormat::Toml => match text.parse::<toml::Table>() {
            Ok(_) => Vec::new(),
            Err(error) => vec![ConfigDiagnostic {
                path: "$".to_string(),
                message: format!("invalid TOML configuration syntax: {error}"),
            }],
        },
        ConfigFormat::Yaml => match serde_norway::from_str::<serde_norway::Value>(text) {
            Ok(serde_norway::Value::Mapping(_)) => Vec::new(),
            Ok(serde_norway::Value::Null) if text.trim().is_empty() => Vec::new(),
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
