//! Command-line interface for the `mez` binary.
//!
//! The CLI remains a thin layer over library modules. It validates user-facing
//! command behavior, initializes default configuration, and dispatches local or
//! control-socket-backed commands.

use mez_mux::presentation::{AttachedTerminalOutputModes, ClientViewRole, TerminalCursorStyle};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Args, Parser, Subcommand};
use serde::Serialize;

use crate::config::{
    ConfigDiagnostic, ConfigFormat, ConfigLayer, ConfigMutation, ConfigMutationOperation,
    ConfigMutationPlan, ConfigMutationValue, ConfigPaths, ConfigScope, DEFAULT_CONFIG_TOML,
    DEFAULT_PROJECT_CONFIG_TOML, EffectiveConfig, compose_effective_config,
    persist_config_mutation, validate_config_file, validate_config_text,
};
use crate::control::{decode_control_frame, encode_control_body};
use crate::error::{MezError, Result};
use crate::host::async_runtime::{
    AsyncAttachedTerminalClientServiceConfig, AsyncAttachedTerminalIo,
    AsyncAttachedTerminalLoopRequest, AsyncAttachedTerminalPresentationGuard, AsyncRuntimeService,
    AsyncRuntimeServiceExit, ClientEvent, DEFAULT_ASYNC_ATTACHED_TERMINAL_POLL_TIMEOUT,
    RuntimeEvent, RuntimeEventBatch, run_async_attached_terminal_client_service,
};
use crate::host::shell::resolve_shell;
use crate::host::terminal::{
    AttachedTerminalClientLoopConfig, TerminalClientLoopConfig,
    attached_terminal_output_disconnected,
};
use crate::runtime::{
    AuxiliarySocketKind, DEFAULT_SOCKET_NAME, MEZ_ENV_FIELD_SEPARATOR, RuntimeEnv,
    RuntimeLifecycleState, RuntimeSessionService, auxiliary_socket_path_for_control_socket,
    default_socket_directory, ensure_private_socket_directory,
    prune_stale_socket_files_in_directory, runtime_effective_config_value,
    runtime_ui_theme_from_config, socket_path_for_name,
};
use crate::security::auth::{
    AuthMethod, AuthPaths, AuthStore, OpenAiProviderCredential,
    run_openai_browser_login_with_theme_async, run_openai_device_code_login_async,
};
use crate::security::project::{
    ProjectTrustStore, TrustDecision, default_trust_database_path, discover_project_root,
};
use crate::storage::memory::PersistentMemoryStore;
use crate::storage::registry::{
    SessionRecord, SessionRegistry, records_to_json, resolve_session_record_target,
};
use crate::storage::snapshot::{
    LayoutLoadPlan, SessionSnapshotPayload, SnapshotKind, SnapshotRepository,
    SnapshotRestoreResult, SnapshotState,
};
use mez_agent::mcp::McpRegistry;
use mez_agent::memory::{MemoryKind, MemoryRecord, MemoryScope, MemorySource, MemoryState};
use mez_core::ids::ClientId;
use mez_mux::layout::Size;
use mez_mux::session::Session;
use mez_terminal::{GraphicRendition, TerminalColor, TerminalStyleSpan};

use self::mcp::load_primary_config_layers;

/// Exposes the args module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod args;
/// Exposes the attach module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod attach;
/// Exposes the auth module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod auth;
/// Exposes the config module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod config;
/// Exposes the control client module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod control_client;
/// Exposes the dispatch module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod dispatch;
/// Exposes the env module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod env;
/// Exposes persistent local host lifecycle and routing commands.
mod host;
/// Exposes the issue module boundary.
///
/// The nested module keeps local issue tracking CLI behavior isolated while this
/// declaration makes the boundary available to the dispatcher.
mod issue;
/// Exposes the json module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod json;
/// Exposes local durable lease administration through the host socket.
mod lease;
/// Exposes the mcp module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod mcp;
/// Exposes the memory module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod memory;
/// Exposes the project trust command boundary.
///
/// The nested module owns direct-user inspection and persistence of project
/// trust records beneath the sandbox CLI hierarchy.
mod project_trust;
/// Exposes local remote-transport administration through Unix control.
mod remote;
/// Exposes the sandbox module boundary.
///
/// The nested module owns direct-user sandbox status and diagnostic workflows.
mod sandbox;
/// Exposes the serve module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod serve;
/// Exposes the snapshot module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod snapshot;

#[cfg(test)]
pub use dispatch::run_with;
pub use dispatch::{CliEnv, run};

#[cfg(test)]
use args::parse_cli_arg_group;
use attach::{run_attach, run_list};
use auth::run_auth;
use config::{json_string_array, run_config};
#[cfg(test)]
pub(crate) use control_client::{IrohControlTarget, exchange_iroh_control_request};
use control_client::{
    check_iroh_profile, force_kill_iroh_host_session, incomplete_control_response_error,
    inspect_iroh_invitation_file, list_iroh_host_sessions, open_persistent_iroh_control_channel,
    pair_iroh_invitation, read_control_response_frames, request_control_body, run_control_request,
    run_control_request_for_target,
};
use env::{
    CliCommand, CliInvocation, CliInvocationParse, ControlTargetSelection, SocketSelection,
    cli_idempotency_key, registry_root, render_cli_help, render_cli_version, selected_socket_path,
    terminal_size_from_fd_or_environment,
};
use host::{
    ensure_host_available, host_create_session, host_list_sessions_with_all,
    host_resolve_or_create_session, host_resolve_session, request_host, run_host,
};
use issue::run_issue;
use json::{
    CliOutputFormat, current_unix_seconds, diagnostics_json, json_escape, json_optional,
    serialize_json, write_control_response, write_json_or_plain,
};
use lease::run_lease;
use mcp::{load_runtime_config_layers, run_mcp};
use memory::run_memory;
use project_trust::{ProjectTrustCliArgs, run_project_trust};
use remote::run_remote;
use sandbox::run_sandbox;
use serve::{
    LoadedRuntimeConfig, ParsedServeOptions, RestoredSnapshotDaemonRequest, RuntimeDaemonStartup,
    apply_default_serve_auxiliary_sockets, run_foreground_control_daemon, run_new, run_serve,
    validate_serve_options,
};
use snapshot::run_snapshot;

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
