//! Cli Mcp implementation.
//!
//! This module owns the cli mcp boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    BTreeSet, CliEnv, CliOutputFormat, ConfigFormat, ConfigLayer, ConfigMutation,
    ConfigMutationOperation, ConfigMutationPlan, ConfigMutationValue, ConfigPaths, ConfigScope,
    DEFAULT_CONFIG_TOML, EffectiveConfig, McpRegistry, MezError, Parser, ProjectTrustStore, Result,
    Subcommand, TrustDecision, Write, compose_effective_config, default_trust_database_path,
    discover_existing_overlays, discover_project_root, fs, is_cli_help_request, json_escape,
    json_optional, parse_cli_args, persist_config_mutation, write_json_or_plain,
};

// MCP subcommands and config mutation helpers.

/// Runs the run mcp operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn run_mcp<W: Write>(
    args: &[String],
    env: CliEnv,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    if is_cli_help_request(args) {
        writeln!(
            stdout,
            "usage: mez mcp <list|inspect ID|add ID (--command CMD [--arg ARG...]|--url URL)|remove ID|enable ID|disable ID>"
        )?;
        return Ok(());
    }
    let parsed = parse_cli_args::<McpCliArgs>("mez mcp", args)?;
    let paths = env.config_paths()?;
    match parsed.command.unwrap_or_default() {
        McpCliCommand::Help => {
            writeln!(
                stdout,
                "usage: mez mcp <list|inspect ID|add ID (--command CMD [--arg ARG...]|--url URL)|remove ID|enable ID|disable ID>"
            )?;
        }
        McpCliCommand::List => {
            let effective = load_primary_effective_config(&paths)?;
            let registry = McpRegistry::default();
            let output = format!(
                r#"{{"servers":{},"tools":{}}}"#,
                configured_mcp_servers_json(&effective),
                mcp_tools_json(&registry)
            );
            write_json_or_plain(stdout, output_format, &output)?;
        }
        McpCliCommand::Inspect { id } => {
            validate_config_identifier(&id, "MCP server id")?;
            let effective = load_primary_effective_config(&paths)?;
            let Some(server) = configured_mcp_server_json(&effective, &id) else {
                return Err(MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "MCP server not found",
                ));
            };
            let output = format!(r#"{{"server":{server}}}"#);
            write_json_or_plain(stdout, output_format, &output)?;
        }
        McpCliCommand::Add {
            id,
            command,
            url,
            args: arg_values,
        } => {
            validate_config_identifier(&id, "MCP server id")?;
            if command.is_some() == url.is_some() {
                return Err(MezError::invalid_args(
                    "mcp add requires exactly one of --command or --url",
                ));
            }
            let mut plans = Vec::new();
            plans.push(persist_primary_config_mutation(
                &paths,
                ConfigMutation::set_boolean(format!("mcp_servers.{}.enabled", id), true),
            )?);
            if let Some(command) = command {
                plans.push(persist_primary_config_mutation(
                    &paths,
                    ConfigMutation::set_string(format!("mcp_servers.{}.command", id), command),
                )?);
                plans.push(persist_primary_config_mutation(
                    &paths,
                    ConfigMutation::set_string_array(
                        format!("mcp_servers.{}.args", id),
                        &arg_values,
                    ),
                )?);
                plans.push(persist_primary_config_mutation(
                    &paths,
                    ConfigMutation {
                        path: format!("mcp_servers.{}.url", id),
                        operation: ConfigMutationOperation::Unset,
                    },
                )?);
            }
            if let Some(url) = url {
                plans.push(persist_primary_config_mutation(
                    &paths,
                    ConfigMutation::set_string(format!("mcp_servers.{}.url", id), url),
                )?);
                plans.push(persist_primary_config_mutation(
                    &paths,
                    ConfigMutation {
                        path: format!("mcp_servers.{}.command", id),
                        operation: ConfigMutationOperation::Unset,
                    },
                )?);
                plans.push(persist_primary_config_mutation(
                    &paths,
                    ConfigMutation::set_string_array(format!("mcp_servers.{}.args", id), &[]),
                )?);
            }
            let output = format!(
                r#"{{"server_id":"{}","changed":{},"reload_required":{}}}"#,
                json_escape(&id),
                mutation_plans_changed(&plans),
                mutation_plans_reload_required(&plans)
            );
            write_json_or_plain(stdout, output_format, &output)?;
        }
        McpCliCommand::Remove { id } => {
            validate_config_identifier(&id, "MCP server id")?;
            let plans = persist_mcp_server_removal(&paths, &id)?;
            let output = format!(
                r#"{{"server_id":"{}","removed":true,"changed":{},"reload_required":{}}}"#,
                json_escape(&id),
                mutation_plans_changed(&plans),
                mutation_plans_reload_required(&plans)
            );
            write_json_or_plain(stdout, output_format, &output)?;
        }
        McpCliCommand::Enable { id } => {
            validate_config_identifier(&id, "MCP server id")?;
            let enabled = true;
            let plan = persist_primary_config_mutation(
                &paths,
                ConfigMutation::set_boolean(format!("mcp_servers.{id}.enabled"), enabled),
            )?;
            let output = format!(
                r#"{{"server_id":"{}","enabled":{},"changed":{},"reload_required":{}}}"#,
                json_escape(&id),
                enabled,
                plan.changed,
                plan.reload_required
            );
            write_json_or_plain(stdout, output_format, &output)?;
        }
        McpCliCommand::Disable { id } => {
            validate_config_identifier(&id, "MCP server id")?;
            let enabled = false;
            let plan = persist_primary_config_mutation(
                &paths,
                ConfigMutation::set_boolean(format!("mcp_servers.{id}.enabled"), enabled),
            )?;
            let output = format!(
                r#"{{"server_id":"{}","enabled":{},"changed":{},"reload_required":{}}}"#,
                json_escape(&id),
                enabled,
                plan.changed,
                plan.reload_required
            );
            write_json_or_plain(stdout, output_format, &output)?;
        }
    }
    Ok(())
}

/// Typed process CLI arguments for `mez mcp`.
#[derive(Debug, Parser)]
#[command(
    name = "mez mcp",
    disable_help_flag = true,
    disable_help_subcommand = true
)]
struct McpCliArgs {
    /// Optional MCP subcommand, defaulting to `list`.
    #[command(subcommand)]
    command: Option<McpCliCommand>,
}

/// Typed process CLI subcommands for MCP server configuration.
#[derive(Debug, Clone, Subcommand, Default)]
enum McpCliCommand {
    /// Shows MCP CLI usage.
    #[command(name = "help")]
    Help,
    /// Lists configured MCP servers and known tools.
    #[default]
    List,
    /// Inspects one configured MCP server.
    Inspect {
        /// MCP server id.
        id: String,
    },
    /// Adds or replaces one configured MCP server.
    Add {
        /// MCP server id.
        id: String,
        /// Stdio command path/name.
        #[arg(long)]
        command: Option<String>,
        /// Streamable HTTP endpoint URL.
        #[arg(long)]
        url: Option<String>,
        /// Stdio command argument. May be repeated.
        #[arg(long = "arg", allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Removes one configured MCP server.
    Remove {
        /// MCP server id.
        id: String,
    },
    /// Enables one configured MCP server.
    Enable {
        /// MCP server id.
        id: String,
    },
    /// Disables one configured MCP server.
    Disable {
        /// MCP server id.
        id: String,
    },
}

impl ConfigMutation {
    /// Runs the set string operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn set_string(path: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            operation: ConfigMutationOperation::Set(ConfigMutationValue::String(value.into())),
        }
    }

    /// Runs the set boolean operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn set_boolean(path: impl Into<String>, value: bool) -> Self {
        Self {
            path: path.into(),
            operation: ConfigMutationOperation::Set(ConfigMutationValue::Boolean(value)),
        }
    }

    /// Runs the set string array operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn set_string_array(path: impl Into<String>, values: &[String]) -> Self {
        Self {
            path: path.into(),
            operation: ConfigMutationOperation::Set(ConfigMutationValue::StringArray(
                values.to_vec(),
            )),
        }
    }
}

/// Runs the load primary effective config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn load_primary_effective_config(paths: &ConfigPaths) -> Result<EffectiveConfig> {
    compose_effective_config(&load_primary_config_layers(paths)?)
}

/// Runs the load runtime config layers operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn load_runtime_config_layers(paths: &ConfigPaths) -> Result<Vec<ConfigLayer>> {
    let trust_store =
        ProjectTrustStore::load_from_file(&default_trust_database_path(paths.root()))?;
    let current_dir = std::env::current_dir()?;
    load_runtime_config_layers_for_directory(paths, &trust_store, &current_dir)
}

/// Runs the load runtime config layers for directory operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn load_runtime_config_layers_for_directory(
    paths: &ConfigPaths,
    trust_store: &ProjectTrustStore,
    current_dir: &std::path::Path,
) -> Result<Vec<ConfigLayer>> {
    let mut layers = load_primary_config_layers(paths)?;
    let project_root = discover_project_root(current_dir);
    let overlay_files = discover_existing_overlays(&project_root, current_dir)?;
    let trusted = trust_store
        .get(&project_root)
        .is_some_and(|record| record.state == TrustDecision::Trusted);
    let overlay_count = overlay_files.len();

    for (index, overlay_path) in overlay_files.into_iter().enumerate() {
        let name = if overlay_count == 1 {
            "project".to_string()
        } else {
            format!("project:{}", index + 1)
        };
        layers.push(ConfigLayer {
            name,
            format: ConfigFormat::from_path(&overlay_path)?,
            text: fs::read_to_string(&overlay_path)?,
            path: Some(overlay_path),
            scope: ConfigScope::ProjectOverlay,
            trusted,
        });
    }

    Ok(layers)
}

/// Runs the load primary config layers operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn load_primary_config_layers(paths: &ConfigPaths) -> Result<Vec<ConfigLayer>> {
    let (layer_path, format, text) = if let Some(selected) = paths.select_primary_file()? {
        (
            Some(selected.clone()),
            ConfigFormat::from_path(&selected)?,
            fs::read_to_string(selected)?,
        )
    } else {
        (None, ConfigFormat::Toml, DEFAULT_CONFIG_TOML.to_string())
    };
    Ok(vec![ConfigLayer {
        name: "primary".to_string(),
        path: layer_path,
        format,
        scope: ConfigScope::Primary,
        trusted: true,
        text,
    }])
}

/// Runs the persist primary config mutation operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn persist_primary_config_mutation(
    paths: &ConfigPaths,
    mutation: ConfigMutation,
) -> Result<ConfigMutationPlan> {
    let path = paths.ensure_default_config()?;
    persist_config_mutation(&path, ConfigScope::Primary, mutation)
}

/// Runs the persist mcp server removal operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn persist_mcp_server_removal(
    paths: &ConfigPaths,
    id: &str,
) -> Result<Vec<ConfigMutationPlan>> {
    persist_primary_config_mutation(
        paths,
        ConfigMutation {
            path: format!("mcp_servers.{id}"),
            operation: ConfigMutationOperation::Unset,
        },
    )
    .map(|plan| vec![plan])
}

/// Runs the configured mcp servers json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn configured_mcp_servers_json(effective: &EffectiveConfig) -> String {
    let servers = configured_mcp_server_ids(effective)
        .into_iter()
        .filter_map(|id| configured_mcp_server_json(effective, &id))
        .collect::<Vec<_>>();
    format!("[{}]", servers.join(","))
}

/// Runs the configured mcp server ids operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn configured_mcp_server_ids(effective: &EffectiveConfig) -> Vec<String> {
    let mut ids = BTreeSet::new();
    for path in effective.values().keys() {
        let mut segments = path.split('.');
        if segments.next() == Some("mcp_servers")
            && let Some(id) = segments.next()
            && segments.next().is_some()
        {
            ids.insert(id.to_string());
        }
    }
    ids.into_iter().collect()
}

/// Runs the configured mcp server json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn configured_mcp_server_json(effective: &EffectiveConfig, id: &str) -> Option<String> {
    let prefix = format!("mcp_servers.{id}.");
    if !effective
        .values()
        .keys()
        .any(|path| path.starts_with(&prefix))
    {
        return None;
    }
    let name = effective
        .get(&format!("{prefix}name"))
        .unwrap_or(id)
        .to_string();
    let command = effective.get(&format!("{prefix}command"));
    let url = effective.get(&format!("{prefix}url"));
    let transport = if url.is_some() {
        "streamable_http"
    } else {
        "stdio"
    };
    let enabled = effective
        .get(&format!("{prefix}enabled"))
        .map(|value| value == "true")
        .unwrap_or(true);
    let args = effective
        .get(&format!("{prefix}args"))
        .map(config_value_array_json)
        .unwrap_or_else(|| "[]".to_string());
    Some(format!(
        r#"{{"id":"{}","name":"{}","enabled":{},"transport":"{}","command":{},"url":{},"args":{},"source":"primary"}}"#,
        json_escape(id),
        json_escape(&name),
        enabled,
        transport,
        json_optional(command),
        json_optional(url),
        args
    ))
}

/// Runs the config value array json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn config_value_array_json(value: &str) -> String {
    serde_json::from_str::<serde_json::Value>(value)
        .ok()
        .filter(serde_json::Value::is_array)
        .and_then(|value| serde_json::to_string(&value).ok())
        .unwrap_or_else(|| "[]".to_string())
}

/// Runs the mutation plans changed operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mutation_plans_changed(plans: &[ConfigMutationPlan]) -> bool {
    plans.iter().any(|plan| plan.changed)
}

/// Runs the mutation plans reload required operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mutation_plans_reload_required(plans: &[ConfigMutationPlan]) -> bool {
    plans.iter().any(|plan| plan.reload_required)
}

/// Runs the validate config identifier operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_config_identifier(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(MezError::invalid_args(format!(
            "{label} must contain only ASCII letters, digits, '_' or '-'"
        )));
    }
    Ok(())
}
/// Runs the mcp tools json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mcp_tools_json(registry: &McpRegistry) -> String {
    let tools = registry
        .list_servers()
        .iter()
        .flat_map(|server| {
            server.tools.iter().map(|tool| {
                format!(
                    r#"{{"server_id":"{}","name":"{}","available":{},"blacklisted":{},"permission_required":{},"input_schema":{}}}"#,
                    json_escape(&tool.server_id),
                    json_escape(&tool.name),
                    tool.available,
                    tool.blacklisted,
                    tool.permission_required,
                    tool.input_schema_json
                )
            })
        })
        .collect::<Vec<_>>();
    format!("[{}]", tools.join(","))
}
