//! Regression coverage for the control tests subsystem.
//!
//! These tests describe the behavior protected by the repository
//! specification and workflow guidance. Keeping the scenarios documented
//! makes failures easier to map back to the user-visible contract.

// Control module tests.

use self::json_rpc_fixture::JsonRpcRequestBuilder;
use self::temp_dir::TestTempDir;
use super::registry::control_method_spec;
use super::types::PRIMARY_CONTROL_METHODS;
use super::{
    AGENT_CONTROL_METHODS, AUTOMATION_CONTROL_METHODS, AgentShellStore, AgentTurnLedger,
    AgentTurnState, AuditLog, AuthenticationMaterial, Capabilities, ClientDescriptor, ConfigFormat,
    ConfigLayer, ConfigScope, ControlConnectionState, ControlIdempotencyCache, GrantedRole,
    InitializeContext, InitializeParams, OBSERVER_CONTROL_METHODS,
    PENDING_OBSERVER_CONTROL_METHODS, PaneCaptureSource, RequestedRole, TerminalDescriptor,
    config_response_advances_generation, decode_control_frame, dispatch_control_request,
    dispatch_control_request_cached, dispatch_control_request_for_client,
    dispatch_control_request_for_client_with_agent_state,
    dispatch_control_request_for_client_with_config,
    dispatch_control_request_for_client_with_config_and_audit,
    dispatch_control_request_for_client_with_events, dispatch_control_request_for_connection,
    dispatch_control_request_with_approvals, dispatch_control_request_with_approvals_and_audit,
    dispatch_control_request_with_captures, dispatch_control_request_with_mcp,
    dispatch_control_request_with_snapshots, dispatch_project_trust_request,
    dispatch_session_attach_request, encode_control_body, handle_control_frame,
    handle_control_frames, handle_control_frames_for_connection, initialize,
    initialize_result_json, json_escape, parse_json_rpc_request,
};
use crate::host::shell::{ResolvedShell, ShellSource};
use crate::protocol::event::{EventKind, EventLog, EventVisibility};
use crate::security::project::ProjectTrustStore;
use crate::storage::snapshot::{
    PaneSnapshotPayload, SessionSnapshotPayload, SnapshotFrameState, SnapshotPaneGeometry,
    SnapshotRepository, SnapshotSessionState, SnapshotShellMetadata, WindowSnapshotPayload,
};
use crate::test_support::runtime::SessionFixture;
use mez_agent::mcp::{McpRegistry, McpServerConfig, McpToolEffects, McpToolState};
use mez_agent::permissions::{
    BlockedApprovalQueue, BlockedApprovalRequest, BlockedApprovalState, builtin_rules,
};
use mez_core::ids::ClientId;
use mez_mux::layout::SplitDirection;
use mez_mux::layout::{LayoutPolicy, Size};
use mez_mux::session::Session;
use mez_terminal::DEFAULT_HISTORY_LIMIT;
use mez_terminal::DEFAULT_PANE_TERM;
use mez_terminal::{
    GraphicRendition, TerminalColor, TerminalModeState, TerminalSavedState, TerminalStyleSpan,
};
use std::fs;
use std::path::PathBuf;

mod json_rpc_fixture;
mod temp_dir;

/// Runs the primary params operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn primary_params() -> InitializeParams {
    InitializeParams {
        client_name: "test".to_string(),
        requested_version: 1,
        requested_role: RequestedRole::Primary,
        client_version: None,
        session_target_json: None,
        detach_primary_on_disconnect: false,
        client: Some(ClientDescriptor {
            name: "test".to_string(),
            version: None,
            pid: None,
            host: None,
            user: None,
            purpose: None,
            requested_role: None,
            interactive: true,
            stdio: None,
            metadata_json: None,
            terminal: Some(TerminalDescriptor {
                columns: 80,
                rows: 24,
                term: "xterm-256color".to_string(),
                features: Vec::new(),
            }),
        }),
        authentication: Some(AuthenticationMaterial::peer_credentials()),
    }
}

/// Runs the test session operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn test_session() -> (Session, ClientId) {
    SessionFixture::new().build_with_primary()
}

/// Runs the temp root operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn temp_root(name: &str) -> TestTempDir {
    TestTempDir::new(&format!("control-test-{name}"))
}

mod agent;
mod approvals;
mod authz;
mod capture;
mod clients;
mod config;
mod connections;
mod events;
mod idempotency;
mod initialization;
mod mcp;
mod schemas;
mod session;
mod state;
mod targets;
mod trust;
