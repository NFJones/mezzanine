//! Cli Config implementation.
//!
//! This module owns the cli config boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::mcp::load_runtime_config_layers;
use super::{
    Args, CliEnv, CliOutputFormat, ConfigDiagnostic, ConfigFormat, ConfigLayer, ConfigMutation,
    ConfigMutationOperation, ConfigMutationPlan, ConfigMutationValue, ConfigPaths, ConfigScope,
    DEFAULT_CONFIG_TOML, DEFAULT_PROJECT_CONFIG_TOML, EffectiveConfig, MezError, PathBuf,
    ProjectTrustRecord, ProjectTrustStore, Result, Serialize, Subcommand, TrustDecision, Write,
    compose_effective_config, default_trust_database_path, diagnostics_json, discover_project_root,
    fs, json_escape, json_optional, persist_config_mutation, render_cli_help, serialize_json,
    validate_config_file, validate_config_text, write_json_or_plain,
};

// Config and project-trust subcommands.

/// Runs the run config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn run_config<W: Write>(
    parsed: ConfigCliArgs,
    env: CliEnv,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let paths = env.config_paths()?;

    match parsed.command {
        None => write!(stdout, "{}", render_cli_help(&["config"])?)?,
        Some(ConfigCliCommand::Init) => {
            let path = paths.ensure_default_config()?;
            writeln!(stdout, "{}", path.display())?;
        }
        Some(ConfigCliCommand::Path) => {
            let path = paths
                .select_primary_file()?
                .unwrap_or_else(|| paths.default_primary_file());
            writeln!(stdout, "{}", path.display())?;
        }
        Some(ConfigCliCommand::DefaultConfig) => {
            write!(stdout, "{DEFAULT_CONFIG_TOML}")?;
        }
        Some(ConfigCliCommand::Validate { path }) => {
            let validation = if let Some(path) = path {
                validate_config_file(&path, ConfigScope::Primary)?
            } else if let Some(path) = paths.select_primary_file()? {
                validate_config_file(&path, ConfigScope::Primary)?
            } else {
                validate_config_text(
                    ConfigFormat::Toml,
                    DEFAULT_CONFIG_TOML,
                    ConfigScope::Primary,
                )
            };
            let output = format!(
                r#"{{"valid":{},"diagnostics":{}}}"#,
                validation.valid,
                diagnostics_json(&validation.diagnostics)?
            );
            write_json_or_plain(stdout, output_format, &output)?;
        }
        Some(ConfigCliCommand::Get { path }) => {
            run_config_get(path.as_deref(), &paths, output_format, stdout)?
        }
        Some(ConfigCliCommand::Layers) => run_config_layers(&paths, output_format, stdout)?,
        Some(ConfigCliCommand::Set(args)) => run_config_set(args, &paths, output_format, stdout)?,
        Some(ConfigCliCommand::Unset(args)) => {
            run_config_unset(args, &paths, output_format, stdout)?
        }
        Some(ConfigCliCommand::Trust(args)) => {
            run_config_trust(args, &paths, output_format, stdout)?
        }
    }

    Ok(())
}

/// Typed process CLI arguments for `mez config`.
#[derive(Debug, Clone, Args)]
pub(super) struct ConfigCliArgs {
    /// Optional configuration subcommand.
    #[command(subcommand)]
    command: Option<ConfigCliCommand>,
}

/// Typed process CLI subcommands for configuration management.
#[derive(Debug, Clone, Subcommand)]
enum ConfigCliCommand {
    /// Creates the default user config if missing.
    Init,
    /// Prints the selected primary config path.
    Path,
    /// Prints the built-in default config.
    #[command(name = "default")]
    DefaultConfig,
    /// Validates the selected primary config or a given file.
    Validate {
        /// Optional config file to validate.
        path: Option<PathBuf>,
    },
    /// Shows effective config or one effective path.
    Get {
        /// Optional effective config path.
        path: Option<String>,
    },
    /// Shows config layer order and diagnostics.
    Layers,
    /// Persists a scalar config value.
    Set(ConfigSetCliArgs),
    /// Removes a persisted scalar config value.
    Unset(ConfigUnsetCliArgs),
    /// Inspects or changes project trust records.
    Trust(ConfigTrustCliArgs),
}

/// Typed process CLI arguments for `mez config set`.
#[derive(Debug, Clone, clap::Args)]
pub(super) struct ConfigSetCliArgs {
    /// Config path to persist.
    path: String,
    /// Config scalar value to persist.
    #[arg(allow_hyphen_values = true)]
    value: String,
    /// Persistence target options.
    #[command(flatten)]
    target: CliConfigPersistOptions,
}

/// Typed process CLI arguments for `mez config unset`.
#[derive(Debug, Clone, clap::Args)]
pub(super) struct ConfigUnsetCliArgs {
    /// Config path to remove.
    path: String,
    /// Persistence target options.
    #[command(flatten)]
    target: CliConfigPersistOptions,
}

/// Typed process CLI arguments for `mez config trust`.
#[derive(Debug, Clone, clap::Args)]
pub(super) struct ConfigTrustCliArgs {
    /// Optional trust subcommand, defaulting to `list`.
    #[command(subcommand)]
    command: Option<ConfigTrustCliCommand>,
}

/// Runs the run config get operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn run_config_get<W: Write>(
    path: Option<&str>,
    paths: &ConfigPaths,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let layers = load_runtime_config_layers(paths)?;
    let effective = compose_effective_config(&layers)?;
    let layer_json = cli_config_layers_json(&layers, &effective)?;
    if let Some(path) = path {
        let output = format!(
            r#"{{"path":"{}","value":{},"source":{},"layers":{}}}"#,
            json_escape(path),
            cli_config_optional_value_json(effective.get(path)),
            json_optional(effective.source_for(path)),
            layer_json
        );
        write_json_or_plain(stdout, output_format, &output)?;
        return Ok(());
    }

    let values = effective
        .values()
        .iter()
        .map(|(path, value)| {
            format!(
                r#""{}":{}"#,
                json_escape(path),
                cli_config_value_json(&value.value)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let output = format!(r#"{{"value":{{{values}}},"layers":{layer_json}}}"#);
    write_json_or_plain(stdout, output_format, &output)?;
    Ok(())
}

/// Runs the run config layers operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn run_config_layers<W: Write>(
    paths: &ConfigPaths,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let layers = load_runtime_config_layers(paths)?;
    let effective = compose_effective_config(&layers)?;
    let output = format!(
        r#"{{"layers":{}}}"#,
        cli_config_layers_json(&layers, &effective)?
    );
    write_json_or_plain(stdout, output_format, &output)?;
    Ok(())
}

/// Runs the run config set operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn run_config_set<W: Write>(
    args: ConfigSetCliArgs,
    paths: &ConfigPaths,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let target = cli_config_mutation_target(paths, args.target)?;
    let plan = persist_config_mutation(
        &target.path,
        target.scope,
        ConfigMutation {
            path: args.path,
            operation: ConfigMutationOperation::Set(cli_config_mutation_value(&args.value)?),
        },
    )?;
    let output = cli_config_mutation_result_json(&target, &plan)?;
    write_json_or_plain(stdout, output_format, &output)?;
    Ok(())
}

/// Runs the run config unset operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn run_config_unset<W: Write>(
    args: ConfigUnsetCliArgs,
    paths: &ConfigPaths,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let target = cli_config_mutation_target(paths, args.target)?;
    let plan = persist_config_mutation(
        &target.path,
        target.scope,
        ConfigMutation {
            path: args.path,
            operation: ConfigMutationOperation::Unset,
        },
    )?;
    let output = cli_config_mutation_result_json(&target, &plan)?;
    write_json_or_plain(stdout, output_format, &output)?;
    Ok(())
}

/// Carries Cli Config Mutation Target state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
struct CliConfigMutationTarget {
    /// Stores the scope value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    scope: ConfigScope,
    /// Stores the scope name value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    scope_name: &'static str,
    /// Stores the path value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    path: PathBuf,
}

/// Runs the cli config mutation target operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn cli_config_mutation_target(
    paths: &ConfigPaths,
    options: CliConfigPersistOptions,
) -> Result<CliConfigMutationTarget> {
    let scope = options.scope.as_deref().unwrap_or("user");
    let file = options.file;
    match scope {
        "user" => {
            let path = cli_user_config_mutation_path(paths, file)?;
            Ok(CliConfigMutationTarget {
                scope: ConfigScope::Primary,
                scope_name: "user",
                path,
            })
        }
        "project" => {
            let path = cli_project_config_mutation_path(paths, file)?;
            Ok(CliConfigMutationTarget {
                scope: ConfigScope::ProjectOverlay,
                scope_name: "project",
                path,
            })
        }
        "live" => Err(MezError::invalid_args(
            "offline mez config mutations cannot target live scope; use a live control connection",
        )),
        _ => Err(MezError::invalid_args(
            "config mutation scope must be user or project",
        )),
    }
}

/// Carries Cli Config Persist Options state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, clap::Args)]
struct CliConfigPersistOptions {
    /// Stores the scope value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    #[arg(long)]
    scope: Option<String>,
    /// Stores the file value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    #[arg(long)]
    file: Option<PathBuf>,
}

/// Runs the cli user config mutation path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn cli_user_config_mutation_path(
    paths: &ConfigPaths,
    explicit_file: Option<PathBuf>,
) -> Result<PathBuf> {
    let path = explicit_file.unwrap_or_else(|| paths.default_primary_file());
    let path = if path == paths.default_primary_file() {
        paths.ensure_default_config()?
    } else {
        path
    };
    if !path.is_file() {
        return Err(MezError::new(
            crate::error::MezErrorKind::NotFound,
            format!("user config target {} does not exist", path.display()),
        ));
    }
    let canonical_target = path.canonicalize()?;
    let selected_primary = paths
        .select_primary_file()?
        .map(|path| path.canonicalize())
        .transpose()?;
    let canonical_root = paths.root().canonicalize()?;
    if selected_primary.as_ref() == Some(&canonical_target)
        || canonical_target.starts_with(&canonical_root)
    {
        return Ok(path);
    }
    Err(MezError::new(
        crate::error::MezErrorKind::Forbidden,
        format!(
            "user config target {} must be under the user-private config root {}",
            canonical_target.display(),
            canonical_root.display()
        ),
    ))
}

/// Runs the cli project config mutation path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn cli_project_config_mutation_path(
    paths: &ConfigPaths,
    explicit_file: Option<PathBuf>,
) -> Result<PathBuf> {
    let trust_store =
        ProjectTrustStore::load_from_file(&default_trust_database_path(paths.root()))?;
    let path = match explicit_file {
        Some(path) => path,
        None => {
            let current_dir = std::env::current_dir()?;
            discover_project_root(&current_dir).join(".mezzanine/config.toml")
        }
    };
    let project_root = cli_project_root_for_target(&path)?;
    let Some(record) = trust_store.get(&project_root) else {
        return Err(MezError::new(
            crate::error::MezErrorKind::Conflict,
            format!(
                "project config target {} is not covered by a trusted project record",
                path.display()
            ),
        ));
    };
    match record.state {
        TrustDecision::Trusted => {}
        TrustDecision::Pending => {
            return Err(MezError::new(
                crate::error::MezErrorKind::Conflict,
                format!(
                    "project config target {} is pending project trust",
                    path.display()
                ),
            ));
        }
        TrustDecision::Rejected | TrustDecision::Revoked => {
            return Err(MezError::new(
                crate::error::MezErrorKind::Forbidden,
                format!("project config target {} is not trusted", path.display()),
            ));
        }
    }
    let canonical_root = record.project_root.canonicalize()?;
    let parent = path.parent().ok_or_else(|| {
        MezError::invalid_args(format!(
            "project config target {} has no parent directory",
            path.display()
        ))
    })?;
    if !parent.exists() {
        fs::create_dir_all(parent)?;
    }
    let canonical_parent = parent.canonicalize()?;
    if !canonical_parent.starts_with(&canonical_root) {
        return Err(MezError::new(
            crate::error::MezErrorKind::Forbidden,
            format!(
                "project config target {} must be under trusted project root {}",
                path.display(),
                canonical_root.display()
            ),
        ));
    }
    if !path.exists() {
        fs::write(&path, DEFAULT_PROJECT_CONFIG_TOML)?;
    }
    Ok(path)
}

/// Runs the cli project root for target operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn cli_project_root_for_target(path: &std::path::Path) -> Result<PathBuf> {
    let anchor = path.parent().ok_or_else(|| {
        MezError::invalid_args(format!(
            "project config target {} has no parent directory",
            path.display()
        ))
    })?;
    let existing_anchor = if anchor.exists() {
        anchor.to_path_buf()
    } else {
        anchor
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| anchor.to_path_buf())
    };
    Ok(discover_project_root(&existing_anchor))
}

/// Runs the cli config mutation value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn cli_config_mutation_value(value: &str) -> Result<ConfigMutationValue> {
    match serde_json::from_str::<serde_json::Value>(value) {
        Ok(serde_json::Value::String(value)) => Ok(ConfigMutationValue::String(value)),
        Ok(serde_json::Value::Bool(value)) => Ok(ConfigMutationValue::Boolean(value)),
        Ok(serde_json::Value::Number(value)) => value
            .as_i64()
            .map(ConfigMutationValue::Integer)
            .ok_or_else(|| MezError::invalid_args("config set integer value is invalid")),
        Ok(serde_json::Value::Array(values)) => {
            let mut strings = Vec::with_capacity(values.len());
            for value in values {
                let serde_json::Value::String(value) = value else {
                    return Err(MezError::invalid_args(
                        "config set string arrays must contain only strings",
                    ));
                };
                strings.push(value);
            }
            Ok(ConfigMutationValue::StringArray(strings))
        }
        Ok(serde_json::Value::Object(_) | serde_json::Value::Null) => Err(MezError::invalid_args(
            "config set supports only string, integer, boolean, or string-array values",
        )),
        Err(_) => match value {
            "true" => Ok(ConfigMutationValue::Boolean(true)),
            "false" => Ok(ConfigMutationValue::Boolean(false)),
            _ => match value.parse::<i64>() {
                Ok(value) => Ok(ConfigMutationValue::Integer(value)),
                Err(_) => Ok(ConfigMutationValue::String(value.to_string())),
            },
        },
    }
}

/// Runs the cli config mutation result json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn cli_config_mutation_result_json(
    target: &CliConfigMutationTarget,
    plan: &ConfigMutationPlan,
) -> Result<String> {
    serialize_json(&CliConfigMutationResultJson {
        applied: plan.changed,
        persisted: true,
        reload_required: plan.reload_required,
        diagnostics: cli_config_diagnostic_json_values(&plan.validation.diagnostics),
        plan: CliConfigMutationPlanJson {
            operation: cli_config_mutation_operation_name(&plan.operation),
            path: &plan.path,
            target: CliConfigMutationTargetJson {
                scope: target.scope_name,
                path: target.path.to_string_lossy().into_owned(),
            },
            format: cli_config_format_name(plan.format),
            scope: cli_config_scope_name(plan.scope),
            changed: plan.changed,
            validated: plan.validation.valid,
            reload_required: plan.reload_required,
        },
    })
}

/// Structured JSON payload emitted after a config mutation is persisted.
#[derive(Serialize)]
struct CliConfigMutationResultJson<'a> {
    /// Whether the persisted value changed the target config file.
    applied: bool,
    /// Whether the mutation was persisted to disk.
    persisted: bool,
    /// Whether the runtime must reload configuration to observe the change.
    reload_required: bool,
    /// Validation diagnostics produced for the mutated config file.
    diagnostics: Vec<CliConfigDiagnosticJson>,
    /// Detailed mutation plan metadata.
    plan: CliConfigMutationPlanJson<'a>,
}

/// Structured JSON payload describing one persisted config mutation plan.
#[derive(Serialize)]
struct CliConfigMutationPlanJson<'a> {
    /// Mutation operation label.
    operation: &'static str,
    /// Config path affected by the mutation.
    path: &'a str,
    /// Persistence target metadata.
    target: CliConfigMutationTargetJson,
    /// Target config file format.
    format: &'static str,
    /// Config scope affected by the mutation.
    scope: &'static str,
    /// Whether the mutation changed the target config file.
    changed: bool,
    /// Whether the resulting config text validated successfully.
    validated: bool,
    /// Whether the runtime must reload configuration to observe the change.
    reload_required: bool,
}

/// Structured JSON payload describing one config mutation persistence target.
#[derive(Serialize)]
struct CliConfigMutationTargetJson {
    /// Target scope label.
    scope: &'static str,
    /// Target config file path.
    path: String,
}

/// Runs the cli config layers json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn cli_config_layers_json(layers: &[ConfigLayer], effective: &EffectiveConfig) -> Result<String> {
    let layers = layers
        .iter()
        .enumerate()
        .map(|(index, layer)| cli_config_layer_json(index, layer, effective))
        .collect::<Result<Vec<_>>>()?;
    serialize_json(&layers)
}

/// Runs the cli config layer json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn cli_config_layer_json(
    index: usize,
    layer: &ConfigLayer,
    effective: &EffectiveConfig,
) -> Result<CliConfigLayerJson> {
    let state = if effective.applied_layers().contains(&layer.name) {
        "applied"
    } else if effective.skipped_layers().contains(&layer.name) {
        "skipped"
    } else {
        "pending"
    };
    let diagnostics = cli_config_layer_diagnostics(layer);
    Ok(CliConfigLayerJson {
        id: layer.name.clone(),
        version: 1,
        name: layer.name.clone(),
        layer_type: cli_config_layer_type_name(layer.scope),
        precedence: index,
        path: layer
            .path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned()),
        format: cli_config_format_name(layer.format),
        scope: cli_config_scope_name(layer.scope),
        trusted: layer.trusted,
        applied: state == "applied",
        state,
        schema_version: 1,
        diagnostics: cli_config_diagnostic_json_values(&diagnostics),
    })
}

/// Structured JSON payload emitted for one config layer.
#[derive(Serialize)]
struct CliConfigLayerJson {
    /// Stable layer identifier.
    id: String,
    /// Layer JSON schema version.
    version: u32,
    /// User-facing layer name.
    name: String,
    /// Layer type label.
    layer_type: &'static str,
    /// Layer precedence in effective config composition.
    precedence: usize,
    /// Config file path when the layer is file-backed.
    path: Option<String>,
    /// Config file format label.
    format: &'static str,
    /// Config scope label.
    scope: &'static str,
    /// Whether the layer is trusted.
    trusted: bool,
    /// Whether the layer was applied to effective config.
    applied: bool,
    /// User-facing layer state label.
    state: &'static str,
    /// Config schema version represented by this layer view.
    schema_version: u32,
    /// Validation diagnostics associated with the layer.
    diagnostics: Vec<CliConfigDiagnosticJson>,
}

/// Structured JSON payload emitted for one config diagnostic.
#[derive(Serialize)]
struct CliConfigDiagnosticJson {
    /// Diagnostic config path.
    path: String,
    /// Diagnostic message.
    message: String,
}

/// Converts config diagnostics into typed CLI JSON payload values.
fn cli_config_diagnostic_json_values(
    diagnostics: &[ConfigDiagnostic],
) -> Vec<CliConfigDiagnosticJson> {
    diagnostics
        .iter()
        .map(|diagnostic| CliConfigDiagnosticJson {
            path: diagnostic.path.clone(),
            message: diagnostic.message.clone(),
        })
        .collect()
}

/// Runs the cli config layer diagnostics operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn cli_config_layer_diagnostics(layer: &ConfigLayer) -> Vec<ConfigDiagnostic> {
    let validation = validate_config_text(layer.format, &layer.text, layer.scope);
    let mut diagnostics = validation.diagnostics;
    if layer.scope == ConfigScope::ProjectOverlay && !layer.trusted {
        diagnostics.push(ConfigDiagnostic {
            path: layer
                .path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| layer.name.clone()),
            message: "project overlay is pending trust and was not applied".to_string(),
        });
    }
    diagnostics
}

/// Runs the cli config optional value json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn cli_config_optional_value_json(value: Option<&str>) -> String {
    value
        .map(cli_config_value_json)
        .unwrap_or_else(|| "null".to_string())
}

/// Runs the cli config value json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn cli_config_value_json(value: &str) -> String {
    let trimmed = value.trim();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return value.to_string();
    }
    format!(r#""{}""#, json_escape(value))
}

/// Runs the cli config mutation operation name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn cli_config_mutation_operation_name(operation: &ConfigMutationOperation) -> &'static str {
    match operation {
        ConfigMutationOperation::Set(_) => "set",
        ConfigMutationOperation::Unset => "unset",
    }
}

/// Runs the cli config format name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn cli_config_format_name(format: ConfigFormat) -> &'static str {
    match format {
        ConfigFormat::Toml => "toml",
        ConfigFormat::Yaml => "yaml",
        ConfigFormat::Json => "json",
    }
}

/// Runs the cli config scope name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn cli_config_scope_name(scope: ConfigScope) -> &'static str {
    match scope {
        ConfigScope::Primary => "user",
        ConfigScope::ProjectOverlay => "project",
        ConfigScope::LiveOverride => "live",
    }
}

/// Runs the cli config layer type name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn cli_config_layer_type_name(scope: ConfigScope) -> &'static str {
    match scope {
        ConfigScope::Primary => "user",
        ConfigScope::ProjectOverlay => "project_root",
        ConfigScope::LiveOverride => "live",
    }
}

/// Runs the run config trust operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn run_config_trust<W: Write>(
    args: ConfigTrustCliArgs,
    paths: &ConfigPaths,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let trust_path = default_trust_database_path(paths.root());
    let mut store = ProjectTrustStore::load_from_file(&trust_path)?;
    match args.command.unwrap_or(ConfigTrustCliCommand::List) {
        ConfigTrustCliCommand::List => {
            let output = project_records_json(store.records())?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        ConfigTrustCliCommand::Inspect { root } => {
            let Some(record) = store.get(&root) else {
                return Err(MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "project trust record not found",
                ));
            };
            let output = project_record_json(record)?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        ConfigTrustCliCommand::Trust { root } => {
            persist_config_trust_decision(
                &mut store,
                &trust_path,
                root,
                TrustDecision::Trusted,
                output_format,
                stdout,
            )?;
        }
        ConfigTrustCliCommand::Reject { root } => {
            persist_config_trust_decision(
                &mut store,
                &trust_path,
                root,
                TrustDecision::Rejected,
                output_format,
                stdout,
            )?;
        }
        ConfigTrustCliCommand::Revoke { root } => {
            persist_config_trust_decision(
                &mut store,
                &trust_path,
                root,
                TrustDecision::Revoked,
                output_format,
                stdout,
            )?;
        }
    }
    Ok(())
}

/// Persists one project trust decision and writes the resulting record.
///
/// # Parameters
/// - `store`: The trust store to mutate.
/// - `trust_path`: The trust database path to save.
/// - `root`: The project root whose trust state changes.
/// - `decision`: The requested trust decision.
/// - `output_format`: The CLI output format.
/// - `stdout`: The output sink for the response.
fn persist_config_trust_decision<W: Write>(
    store: &mut ProjectTrustStore,
    trust_path: &std::path::Path,
    root: PathBuf,
    decision: TrustDecision,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let git_marker = root.join(".git");
    let git_marker = git_marker.exists().then_some(git_marker);
    store.decide(root.clone(), decision, git_marker)?;
    store.save_to_file(trust_path)?;
    let record = store.get(&root).ok_or_else(|| {
        MezError::new(
            crate::error::MezErrorKind::NotFound,
            "project trust record not found after decision",
        )
    })?;
    let output = project_record_json(record)?;
    write_json_or_plain(stdout, output_format, &output)?;
    Ok(())
}

/// Typed process CLI subcommands for project trust records.
#[derive(Debug, Clone, Subcommand)]
pub(super) enum ConfigTrustCliCommand {
    /// Lists project trust records.
    List,
    /// Inspects one project trust record.
    Inspect {
        /// Project root path.
        root: PathBuf,
    },
    /// Marks one project root as trusted.
    Trust {
        /// Project root path.
        root: PathBuf,
    },
    /// Marks one project root as rejected.
    Reject {
        /// Project root path.
        root: PathBuf,
    },
    /// Revokes one project trust record.
    Revoke {
        /// Project root path.
        root: PathBuf,
    },
}
/// Runs the json string array operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn json_string_array(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| format!(r#""{}""#, json_escape(value)))
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// Runs the project records json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn project_records_json<'a>(
    records: impl Iterator<Item = &'a ProjectTrustRecord>,
) -> Result<String> {
    let records = records
        .map(ProjectTrustRecordJson::from)
        .collect::<Vec<_>>();
    serialize_json(&records)
}

/// Runs the project record json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn project_record_json(record: &ProjectTrustRecord) -> Result<String> {
    serialize_json(&ProjectTrustRecordJson::from(record))
}

/// Structured JSON payload emitted for one project trust record.
#[derive(Serialize)]
struct ProjectTrustRecordJson {
    /// Canonical project root path associated with the trust record.
    project_root: String,
    /// Current trust decision label.
    state: &'static str,
    /// Canonical Git marker path used to bind trust when available.
    git_marker_path: Option<String>,
    /// Unix timestamp recording when the decision was made.
    trusted_at_unix_seconds: u64,
    /// Trust policy version recorded with the decision.
    trust_policy_version: u32,
    /// Configuration schema version recorded with the decision.
    configuration_schema_version: u32,
    /// VCS remote recorded with the decision when available.
    vcs_remote: Option<String>,
}

impl From<&ProjectTrustRecord> for ProjectTrustRecordJson {
    fn from(record: &ProjectTrustRecord) -> Self {
        Self {
            project_root: record.project_root.to_string_lossy().into_owned(),
            state: trust_decision_name(record.state),
            git_marker_path: record
                .git_marker_path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            trusted_at_unix_seconds: record.trusted_at_unix_seconds,
            trust_policy_version: record.trust_policy_version,
            configuration_schema_version: record.configuration_schema_version,
            vcs_remote: record.vcs_remote.clone(),
        }
    }
}

/// Runs the trust decision name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn trust_decision_name(decision: TrustDecision) -> &'static str {
    match decision {
        TrustDecision::Pending => "pending",
        TrustDecision::Trusted => "trusted",
        TrustDecision::Rejected => "rejected",
        TrustDecision::Revoked => "revoked",
    }
}
