//! User-facing `/plugin` command parsing and display.
//!
//! This module keeps slash command argument handling out of the agent shell
//! dispatcher. Commands operate on the local installed registry and local
//! package store; marketplace and high-risk payload activation are reported as
//! planned-but-unimplemented rather than silently enabled.

use super::install::{install_local_plugin, set_plugin_enabled, uninstall_plugin};
use super::manifest::PluginManifest;
use super::registry::PluginRegistry;
use crate::{MezError, Result};
use std::path::{Path, PathBuf};

/// Parsed `/plugin` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginCommand {
    /// List installed plugins.
    List,
    /// Inspect one installed plugin.
    Inspect { id: String },
    /// Install a local plugin package.
    Install { source: PathBuf, enabled: bool },
    /// Uninstall one plugin.
    Uninstall { id: String },
    /// Enable one installed plugin.
    Enable { id: String },
    /// Disable one installed plugin.
    Disable { id: String },
    /// Report marketplace command status.
    Marketplace,
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
        "list" => Ok(PluginCommand::List),
        "inspect" => {
            let id = required_word(&words, 1, "plugin inspect requires a plugin id")?;
            Ok(PluginCommand::Inspect { id })
        }
        "install" | "add" => {
            let source = required_word(&words, 1, "plugin install requires a local path")?;
            let enabled = words.iter().skip(2).any(|word| *word == "--enable");
            Ok(PluginCommand::Install {
                source: PathBuf::from(source),
                enabled,
            })
        }
        "uninstall" | "remove" => {
            let id = required_word(&words, 1, "plugin uninstall requires a plugin id")?;
            Ok(PluginCommand::Uninstall { id })
        }
        "enable" => {
            let id = required_word(&words, 1, "plugin enable requires a plugin id")?;
            Ok(PluginCommand::Enable { id })
        }
        "disable" => {
            let id = required_word(&words, 1, "plugin disable requires a plugin id")?;
            Ok(PluginCommand::Disable { id })
        }
        "marketplace" => Ok(PluginCommand::Marketplace),
        _ => Err(MezError::invalid_args(format!(
            "unknown plugin subcommand {command:?}"
        ))),
    }
}

/// Executes a parsed `/plugin` command against local plugin state.
///
/// # Parameters
/// - `config_root`: Primary Mezzanine configuration root.
/// - `pane_cwd`: Active pane working directory for resolving relative install paths.
/// - `command`: Parsed plugin command.
pub fn plugin_command_display(
    config_root: &Path,
    pane_cwd: &Path,
    command: PluginCommand,
) -> Result<String> {
    match command {
        PluginCommand::List => plugin_list_display(config_root),
        PluginCommand::Inspect { id } => plugin_inspect_display(config_root, &id),
        PluginCommand::Install { source, enabled } => {
            let source = resolve_source_path(pane_cwd, &source);
            install_local_plugin(config_root, &source, enabled)
        }
        PluginCommand::Uninstall { id } => uninstall_plugin(config_root, &id),
        PluginCommand::Enable { id } => set_plugin_enabled(config_root, &id, true),
        PluginCommand::Disable { id } => set_plugin_enabled(config_root, &id, false),
        PluginCommand::Marketplace => Ok(
            "plugin marketplace support is planned; local install/list/inspect/enable/disable/uninstall are available".to_string(),
        ),
    }
}

fn plugin_list_display(config_root: &Path) -> Result<String> {
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

fn plugin_inspect_display(config_root: &Path, id: &str) -> Result<String> {
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

fn resolve_source_path(pane_cwd: &Path, source: &Path) -> PathBuf {
    if source.is_absolute() {
        source.to_path_buf()
    } else {
        pane_cwd.join(source)
    }
}
