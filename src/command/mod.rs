//! Mezzanine command-language parsing.
//!
//! Commands entered through the prefix command prompt are parsed by Mezzanine
//! rather than the pane shell. This module implements deterministic tokenization,
//! command-sequence parsing, and the in-memory execution semantics that can be
//! satisfied before the live control endpoint exists.

use std::fs;
use std::path::PathBuf;

use crate::agent::{PaneReadinessOverrideStore, PaneReadinessState};
use crate::audit::{AuditActor, AuditLog, AuditRecord};
use crate::auth::{AuthMethod, AuthStatus, AuthStore, CredentialStoreKind};
use crate::config::{
    ConfigFormat, ConfigMutation, ConfigMutationOperation, ConfigMutationPlan, ConfigMutationValue,
    ConfigPaths, ConfigScope, persist_config_mutation, persist_config_text, plan_config_mutation,
    validate_config_file,
};
use crate::error::{MezError, Result};
use crate::ids::ClientId;
use crate::layout::{
    PaneNavigationDirection, PaneSizeSpec, ResizeAxis, ResizeDirection, SplitDirection,
};
use crate::session::Session;
use crate::terminal::{KeyBindings, KeyChord, KeyCode};

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
/// Exposes the parser module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod parser;
/// Exposes the permissions module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod permissions;
/// Exposes the plans module boundary.
///
/// The nested module converts parsed invocations into typed command plans before
/// execution mutates session state.
mod plans;
/// Exposes the shell module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod shell;
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

pub use dispatch::{
    execute_auth_command, execute_command, execute_command_sequence, execute_config_store_command,
    execute_mark_pane_ready_command, execute_mcp_config_command,
};
pub(crate) use display::{
    bind_key_args, binding_config_key, key_chord_notation, new_window_name,
    new_window_shell_command, split_window_shell_command,
};
pub use parser::parse_command_sequence;
pub(crate) use plans::{resize_spec_from_invocation, split_window_selects_new_pane};
pub use types::{
    BaselineCommand, BaselineCommandStatus, CommandInvocation, CommandOutcome, baseline_commands,
};

use display::{
    attach_session_display, auth_login_plan_display, auth_status_display, capture_pane_display,
    choose_buffer_display, choose_client_display, choose_group_display, choose_observer_display,
    choose_window_display, clear_history_display, command_help_display, copy_mode_display,
    copy_selection_display, create_buffer_display, export_history_display, list_baseline_commands,
    list_buffers_display, list_clients, list_current_session, list_default_key_bindings,
    list_default_themes, list_groups, list_observers, list_panes, list_windows,
    mcp_add_plan_display, mcp_login_plan_display, mcp_logout_plan_display, mcp_remove_plan_display,
    mcp_retry_plan_display, mcp_status_plan_display, mutated_pane_command_outcome,
    parse_config_command_value, paste_buffer_display, paste_clipboard_display, pipe_pane_display,
    resume_session_display, save_buffer_display, search_history_display, set_option_args,
    set_theme_arg, show_default_options, show_messages_display, show_metrics_display,
    snapshot_session_display,
};
use permissions::{
    command_target_pane_id, credential_store_kind_name, mark_pane_ready_audit_record,
    mark_pane_ready_warning_display, pane_readiness_state_name, repeated_flag_values,
    validate_command_identifier,
};
use shell::{
    QuoteState, explicit_shell_command_flag, flag_value, positional_args,
    positional_args_before_double_dash, shell_command_after_double_dash, shell_command_from_words,
};
use stores::{
    auth_status_store_display, config_set_string, config_unset, execute_auth_login, mcp_server_id,
    mcp_status_store_display, mcp_transport_target, mutation_plans_changed,
    mutation_plans_reload_required, persist_command_config_mutation, persist_command_theme_config,
    persist_mcp_add, persist_mcp_remove,
};

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
