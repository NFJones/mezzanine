//! Regression coverage for the command tests subsystem.
//!
//! These tests describe the behavior protected by the repository
//! specification and workflow guidance. Keeping the scenarios documented
//! makes failures easier to map back to the user-visible contract.

// Command module tests.

use super::{
    AuditLog, AuthStore, CommandOutcome, LayoutLoadSelector, PaneReadinessOverrideStore,
    PaneReadinessState, baseline_commands, execute_auth_command, execute_command,
    execute_command_sequence, execute_config_store_command, execute_mark_pane_ready_command,
};
use crate::auth::AuthPaths;
use crate::config::ConfigPaths;
use crate::shell::{ResolvedShell, ShellSource};
use mez_core::ids::ClientId;
use mez_mux::command::parse_command_sequence;
use mez_mux::layout::Size;
use mez_mux::session::{ClientState, Session};
use std::fs;
use std::path::PathBuf;

/// Runs the test session operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn test_session() -> (Session, ClientId) {
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    );
    let primary = session.attach_primary("primary", true).unwrap();
    (session, primary)
}

/// Extracts the body from a display command outcome.
fn display_body(outcome: CommandOutcome) -> String {
    match outcome {
        CommandOutcome::Display { body, .. } => body,
        _ => panic!("expected display outcome"),
    }
}

/// Asserts that a command outcome is the expected no-op command.
fn assert_noop(outcome: CommandOutcome, expected_command: &str) {
    match outcome {
        CommandOutcome::Noop { command } => assert_eq!(command, expected_command),
        _ => panic!("expected noop outcome"),
    }
}

mod auth_mcp;
mod catalog;
mod config;
mod panes;
mod readiness;
mod session;
