//! Plugin package management and runtime loading.
//!
//! Plugins are declarative extension packages installed below the Mezzanine
//! configuration root. This subsystem owns manifest validation, installed
//! registry persistence, local package installation/removal, read-only display,
//! and side-effect-free runtime payload discovery. Loading never executes
//! plugin content; it only converts enabled manifest payloads into existing
//! Mezzanine capability surfaces such as skill roots.

mod display;
mod install;
mod load;
mod manifest;
mod registry;

pub use display::{
    PluginCommand, plugin_command_display, plugin_command_from_args, plugin_inspect_display,
    plugin_list_display, plugin_status_display,
};
pub use install::{install_local_plugin, set_plugin_enabled, uninstall_plugin};
pub use load::{PluginLoadOutcome, PluginSkillRoot, load_enabled_plugins};
pub use manifest::{PLUGIN_MANIFEST_FILE_NAME, PluginManifest, PluginPayloads};
pub use registry::{InstalledPlugin, PluginRegistry, plugin_registry_path};

#[cfg(test)]
mod tests;
