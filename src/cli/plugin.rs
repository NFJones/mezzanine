//! Cli Plugin implementation.
//!
//! This module owns the process CLI plugin boundary for Mezzanine. It keeps
//! plugin lifecycle changes in the CLI surface while agent slash commands stay
//! read-only status views.

use super::{Args, CliEnv, CliOutputFormat, Result, Serialize, Subcommand, Write, serialize_json};
use crate::plugins::{
    PluginRegistry, install_local_plugin, plugin_inspect_display, plugin_list_display,
    set_plugin_enabled, uninstall_plugin,
};
use std::path::PathBuf;

/// Runs one `mez plugin` command.
///
/// # Parameters
/// - `parsed`: Parsed plugin CLI arguments.
/// - `env`: Process CLI environment.
/// - `output_format`: Requested output rendering mode.
/// - `stdout`: Destination for command output.
pub(super) fn run_plugin<W: Write>(
    parsed: PluginCliArgs,
    env: CliEnv,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let paths = env.config_paths()?;
    match parsed.command.unwrap_or(PluginCliCommand::List) {
        PluginCliCommand::List => {
            write_plugin_output(
                stdout,
                output_format,
                || plugin_list_json(paths.root()),
                || plugin_list_display(paths.root()),
            )?;
        }
        PluginCliCommand::Inspect { id } => {
            write_plugin_output(
                stdout,
                output_format,
                || plugin_inspect_json(paths.root(), &id),
                || plugin_inspect_display(paths.root(), &id),
            )?;
        }
        PluginCliCommand::Install { path, enable } => {
            let source = resolve_cli_plugin_source(path)?;
            let message = install_local_plugin(paths.root(), &source, enable)?;
            write_plugin_mutation(stdout, output_format, "install", Some(&message))?;
        }
        PluginCliCommand::Uninstall { id } => {
            let message = uninstall_plugin(paths.root(), &id)?;
            write_plugin_mutation(stdout, output_format, "uninstall", Some(&message))?;
        }
        PluginCliCommand::Enable { id } => {
            let message = set_plugin_enabled(paths.root(), &id, true)?;
            write_plugin_mutation(stdout, output_format, "enable", Some(&message))?;
        }
        PluginCliCommand::Disable { id } => {
            let message = set_plugin_enabled(paths.root(), &id, false)?;
            write_plugin_mutation(stdout, output_format, "disable", Some(&message))?;
        }
        PluginCliCommand::Marketplace { .. } => {
            write_plugin_mutation(
                stdout,
                output_format,
                "marketplace",
                Some(
                    "plugin marketplace support is planned; local install/list/inspect/enable/disable/uninstall are available",
                ),
            )?;
        }
    }
    Ok(())
}

/// Typed process CLI arguments for `mez plugin`.
#[derive(Debug, Clone, Args)]
pub(super) struct PluginCliArgs {
    /// Optional plugin subcommand, defaulting to `list`.
    #[command(subcommand)]
    command: Option<PluginCliCommand>,
}

/// Typed process CLI subcommands for local plugin management.
#[derive(Debug, Clone, Subcommand)]
enum PluginCliCommand {
    /// Lists installed plugins.
    List,
    /// Inspects one installed plugin.
    Inspect {
        /// Installed plugin id.
        id: String,
    },
    /// Installs one local plugin package.
    #[command(visible_alias = "add")]
    Install {
        /// Local plugin package path.
        path: PathBuf,
        /// Enable the plugin immediately after installation.
        #[arg(long)]
        enable: bool,
    },
    /// Uninstalls one local plugin package.
    #[command(visible_alias = "remove")]
    Uninstall {
        /// Installed plugin id.
        id: String,
    },
    /// Enables one installed plugin.
    Enable {
        /// Installed plugin id.
        id: String,
    },
    /// Disables one installed plugin.
    Disable {
        /// Installed plugin id.
        id: String,
    },
    /// Reserves the marketplace management namespace.
    Marketplace {
        /// Marketplace command words.
        args: Vec<String>,
    },
}

/// Structured JSON emitted for plugin mutation commands.
#[derive(Serialize)]
struct PluginMutationJson<'a> {
    /// Operation that was requested.
    operation: &'a str,
    /// Human-facing result message.
    message: Option<&'a str>,
}

/// Structured JSON emitted for plugin list commands.
#[derive(Serialize)]
struct PluginListJson {
    /// Installed plugin records.
    plugins: Vec<PluginJson>,
}

/// Structured JSON emitted for plugin inspect commands.
#[derive(Serialize)]
struct PluginInspectJson {
    /// Installed plugin record.
    plugin: PluginJson,
}

/// Secret-free installed plugin JSON record.
#[derive(Serialize)]
struct PluginJson {
    /// Stable plugin id.
    id: String,
    /// Plugin display name.
    name: String,
    /// Plugin description.
    description: String,
    /// Plugin version.
    version: String,
    /// Installed package path.
    path: String,
    /// Whether runtime payloads are active.
    enabled: bool,
}

/// Writes either JSON or plain plugin output.
fn write_plugin_output<W, J, P>(
    stdout: &mut W,
    output_format: CliOutputFormat,
    json: J,
    plain: P,
) -> Result<()>
where
    W: Write,
    J: FnOnce() -> Result<String>,
    P: FnOnce() -> Result<String>,
{
    if output_format.is_json() {
        writeln!(stdout, "{}", json()?)?;
    } else {
        write!(stdout, "{}", plain()?)?;
    }
    Ok(())
}

/// Writes either JSON or plain plugin mutation output.
fn write_plugin_mutation<W: Write>(
    stdout: &mut W,
    output_format: CliOutputFormat,
    operation: &str,
    message: Option<&str>,
) -> Result<()> {
    if output_format.is_json() {
        let output = serialize_json(&PluginMutationJson { operation, message })?;
        writeln!(stdout, "{output}")?;
    } else if let Some(message) = message {
        write!(stdout, "{message}")?;
    }
    Ok(())
}

/// Resolves a plugin source path against the process working directory.
fn resolve_cli_plugin_source(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path);
    }
    Ok(std::env::current_dir()
        .map_err(|error| {
            crate::MezError::invalid_state(format!("failed to read current directory: {error}"))
        })?
        .join(path))
}

/// Serializes all installed plugins as JSON.
fn plugin_list_json(config_root: &std::path::Path) -> Result<String> {
    let registry = PluginRegistry::read(config_root)?;
    let plugins = registry.plugins.values().map(plugin_json).collect();
    serialize_json(&PluginListJson { plugins })
}

/// Serializes one installed plugin as JSON.
fn plugin_inspect_json(config_root: &std::path::Path, id: &str) -> Result<String> {
    let registry = PluginRegistry::read(config_root)?;
    let plugin = registry.plugins.get(id).ok_or_else(|| {
        crate::MezError::new(
            crate::MezErrorKind::NotFound,
            format!("plugin {id:?} is not installed"),
        )
    })?;
    serialize_json(&PluginInspectJson {
        plugin: plugin_json(plugin),
    })
}

/// Converts one installed plugin record into secret-free JSON.
fn plugin_json(plugin: &crate::plugins::InstalledPlugin) -> PluginJson {
    PluginJson {
        id: plugin.id.clone(),
        name: plugin.name.clone(),
        description: plugin.description.clone(),
        version: plugin.version.clone(),
        path: plugin.path.display().to_string(),
        enabled: plugin.enabled,
    }
}
