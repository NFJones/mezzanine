//! User-facing plugin status command parsing and display.
//!
//! This module keeps read-only `/plugin` argument handling and common plugin
//! display rendering out of the agent shell dispatcher. Persistent lifecycle
//! changes are owned by the process CLI rather than slash commands.

use super::manifest::PluginManifest;
use super::registry::PluginRegistry;
use crate::{MezError, Result};
use std::path::Path;

/// Parsed read-only plugin status command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginCommand {
    /// List installed plugins.
    List,
    /// Inspect one installed plugin.
    Inspect { id: String },
}

/// Parses `/plugin` arguments.
///
/// # Parameters
/// - `args`: Slash-command argument string after `/plugin`.
pub fn plugin_command_from_args(args: &str) -> Result<PluginCommand> {
    let words = args.split_whitespace().collect::<Vec<_>>();
    let Some(command) = words.first().copied() else {
        return Ok(PluginCommand::List);
    };
    match command {
        "list" => {
            require_exact_words(&words, 1, "plugin list does not accept extra arguments")?;
            Ok(PluginCommand::List)
        }
        "status" if words.len() == 1 => Ok(PluginCommand::List),
        "status" | "inspect" => {
            let id = required_word(&words, 1, "plugin status requires a plugin id")?;
            require_exact_words(
                &words,
                2,
                "plugin inspect/status accepts exactly one plugin id",
            )?;
            Ok(PluginCommand::Inspect { id })
        }
        "install" | "add" | "uninstall" | "remove" | "enable" | "disable" | "marketplace" => {
            Err(plugin_cli_migration_error())
        }
        _ => Err(MezError::invalid_args(format!(
            "unknown plugin subcommand {command:?}"
        ))),
    }
}

/// Executes a parsed read-only `/plugin` command against local plugin state.
///
/// # Parameters
/// - `config_root`: Primary Mezzanine configuration root.
/// - `pane_cwd`: Active pane working directory for resolving relative install paths.
/// - `command`: Parsed plugin command.
pub fn plugin_command_display(
    config_root: &Path,
    _pane_cwd: &Path,
    command: PluginCommand,
) -> Result<String> {
    plugin_status_display(config_root, command)
}

/// Renders a parsed read-only plugin status command.
///
/// # Parameters
/// - `config_root`: Primary Mezzanine configuration root.
/// - `command`: Parsed read-only plugin status command.
pub fn plugin_status_display(config_root: &Path, command: PluginCommand) -> Result<String> {
    match command {
        PluginCommand::List => plugin_list_display(config_root),
        PluginCommand::Inspect { id } => plugin_inspect_display(config_root, &id),
    }
}

/// Renders installed plugin list status.
///
/// # Parameters
/// - `config_root`: Primary Mezzanine configuration root.
pub fn plugin_list_display(config_root: &Path) -> Result<String> {
    let registry = PluginRegistry::read(config_root)?;
    let mut lines = vec!["## Plugins".to_string(), String::new()];
    if registry.plugins.is_empty() {
        lines.push("No plugins are installed.".to_string());
        return Ok(lines.join("\n"));
    }
    lines.push("| Plugin | Enabled | Version | Description |".to_string());
    lines.push("| --- | --- | --- | --- |".to_string());
    for plugin in registry.plugins.values() {
        lines.push(format!(
            "| `{}` | {} | `{}` | {} |",
            plugin.id, plugin.enabled, plugin.version, plugin.description
        ));
    }
    Ok(lines.join("\n"))
}

/// Renders detailed status for one installed plugin.
///
/// # Parameters
/// - `config_root`: Primary Mezzanine configuration root.
/// - `id`: Installed plugin id.
pub fn plugin_inspect_display(config_root: &Path, id: &str) -> Result<String> {
    let registry = PluginRegistry::read(config_root)?;
    let plugin = registry.plugins.get(id).ok_or_else(|| {
        MezError::new(
            crate::MezErrorKind::NotFound,
            format!("plugin {id:?} is not installed"),
        )
    })?;
    let manifest = PluginManifest::read_from_root(&plugin.path)?;
    let mut lines = vec![
        format!("## Plugin `{}`", plugin.id),
        String::new(),
        format!("name: {}", plugin.name),
        format!("version: {}", plugin.version),
        format!("enabled: {}", plugin.enabled),
        format!("path: {}", plugin.path.display()),
        format!("description: {}", plugin.description),
        String::new(),
        "payloads:".to_string(),
    ];
    if let Some(skills) = manifest.payloads.skills {
        lines.push(format!("- skills: {}", skills.display()));
    }
    if let Some(mcp) = manifest.payloads.mcp_servers {
        lines.push(format!("- mcp_servers: {} (reserved)", mcp.display()));
    }
    if let Some(hooks) = manifest.payloads.hooks {
        lines.push(format!("- hooks: {} (reserved)", hooks.display()));
    }
    if let Some(subagents) = manifest.payloads.subagents {
        lines.push(format!("- subagents: {} (reserved)", subagents.display()));
    }
    if let Some(personalities) = manifest.payloads.personalities {
        lines.push(format!(
            "- personalities: {} (reserved)",
            personalities.display()
        ));
    }
    Ok(lines.join("\n"))
}

fn required_word(words: &[&str], index: usize, message: &str) -> Result<String> {
    words
        .get(index)
        .map(|word| (*word).to_string())
        .ok_or_else(|| MezError::invalid_args(message))
}

fn require_exact_words(words: &[&str], expected: usize, message: &str) -> Result<()> {
    if words.len() == expected {
        Ok(())
    } else {
        Err(MezError::invalid_args(message))
    }
}

fn plugin_cli_migration_error() -> MezError {
    MezError::invalid_args(
        "plugin lifecycle changes moved to the CLI; use `mez plugin install`, `mez plugin uninstall`, `mez plugin enable`, `mez plugin disable`, or `mez plugin marketplace ...`",
    )
}
