//! Mezzanine product command dispatch and integration.
//!
//! `mez-mux` owns the reusable command grammar, typed session plans, validation,
//! and neutral presentation. This module projects product errors and executes
//! commands against concrete runtime, configuration, auth, audit, agent, and
//! persistence services.

#[cfg(test)]
use std::{fs, path::PathBuf};

#[cfg(test)]
use crate::config::parse_config_json_value;
#[cfg(test)]
use crate::config::{
    ConfigFormat, ConfigMutation, ConfigMutationOperation, ConfigMutationPlan, ConfigMutationValue,
    ConfigPaths, ConfigScope, persist_config_mutation, persist_config_text, plan_config_mutation,
    validate_config_file,
};
use crate::error::{MezError, Result};
use crate::security::audit::{AuditActor, AuditLog, AuditRecord};
use crate::security::auth::{AuthStatus, AuthStore, CredentialStoreKind};
use mez_agent::{PaneReadinessOverrideStore, PaneReadinessState};
use mez_core::ids::ClientId;
use mez_mux::input::{KeyBindings, KeyChord, KeyCode};
use mez_mux::layout::PaneNavigationDirection;
use mez_mux::session::Session;
#[cfg(test)]
use mez_mux::theme::{
    UI_COLOR_SLOT_NAMES, UiThemeDefinition, builtin_ui_theme_definition, resolve_ui_theme,
};

/// Builds stable `key=value` command output lines with a caller-selected separator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeyValueLine {
    /// Separator inserted between successive `key=value` fields.
    separator: &'static str,
    /// Accumulated `key=value` fields in emission order.
    fields: Vec<String>,
}

impl KeyValueLine {
    /// Creates a space-separated key-value output line.
    pub(crate) fn spaced() -> Self {
        Self::new(" ")
    }

    /// Creates a colon-separated key-value output line.
    pub(crate) fn colon_separated() -> Self {
        Self::new(":")
    }

    /// Creates a key-value output line with the provided field separator.
    pub(crate) fn new(separator: &'static str) -> Self {
        Self {
            separator,
            fields: Vec::new(),
        }
    }

    /// Appends one `key=value` field while preserving insertion order.
    pub(crate) fn push(mut self, key: &str, value: impl std::fmt::Display) -> Self {
        self.fields.push(format!("{key}={value}"));
        self
    }

    /// Finishes the accumulated output line.
    pub(crate) fn finish(self) -> String {
        self.fields.join(self.separator)
    }
}

/// Exposes the dispatch module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod dispatch;
/// Exposes the display module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod display;
/// Exposes the permissions module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod permissions;
/// Exposes the stores module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod stores;
/// Exposes the types module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod types;

pub use dispatch::{execute_auth_command, execute_command, execute_mark_pane_ready_command};
#[cfg(test)]
pub use dispatch::{execute_command_sequence, execute_config_store_command};
pub(crate) use display::{
    bind_key_args, binding_config_key, command_help_display_with_key_bindings, key_chord_notation,
};
use mez_mux::command::CommandInvocation;
#[cfg(test)]
use mez_mux::command::parse_command_sequence;
pub use types::{CommandOutcome, LayoutLoadSelector, baseline_commands};

#[cfg(test)]
use display::parse_config_command_value;
use display::{
    capture_pane_display, choose_buffer_display, clear_history_display, command_help_display,
    copy_mode_display, copy_selection_display, create_buffer_display, export_history_display,
    list_baseline_commands, list_buffers_display, list_default_key_bindings, list_default_themes,
    load_layout_selector, mcp_status_plan_display, mutated_pane_command_outcome,
    paste_buffer_display, paste_clipboard_display, pipe_pane_display, save_buffer_display,
    save_layout_name, search_history_display, set_option_args, set_theme_arg, show_default_options,
    show_messages_display, show_metrics_display,
};
use permissions::{
    command_target_pane_id, credential_store_kind_name, mark_pane_ready_audit_record,
    mark_pane_ready_warning_display, pane_readiness_state_name, validate_command_identifier,
};
pub(crate) use stores::auth_status_store_display;
#[cfg(test)]
use stores::{
    config_set_string, config_unset, persist_command_config_mutation, persist_command_theme_config,
};
use stores::{mcp_server_id, mcp_status_store_display};

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
