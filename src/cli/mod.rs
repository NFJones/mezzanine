//! Command-line interface for the `mez` binary.
//!
//! The CLI remains a thin layer over library modules. It validates user-facing
//! command behavior, initializes default configuration, and dispatches local or
//! control-socket-backed commands.

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

use crate::async_runtime::{
    AsyncAttachedTerminalClientServiceConfig, AsyncAttachedTerminalIo,
    AsyncAttachedTerminalLoopRequest, AsyncAttachedTerminalPresentationGuard,
    AsyncRuntimeActorConfig, AsyncRuntimeControlConnectionConfig, AsyncRuntimeDaemonConfig,
    AsyncRuntimeDaemonListeners, AsyncRuntimeService, AsyncRuntimeServiceExit,
    AsyncRuntimeSessionActor, ClientEvent, DEFAULT_ASYNC_ATTACHED_TERMINAL_POLL_TIMEOUT,
    RuntimeEvent, RuntimeEventBatch, build_async_runtime_daemon_services,
    run_async_attached_terminal_client_service, supervise_async_runtime_services,
};
use crate::auth::{
    AuthMethod, AuthPaths, AuthStore, OpenAiProviderCredential,
    run_openai_browser_login_with_theme_async, run_openai_device_code_login_async,
};
use crate::config::{
    ConfigDiagnostic, ConfigFormat, ConfigLayer, ConfigMutation, ConfigMutationOperation,
    ConfigMutationPlan, ConfigMutationValue, ConfigPaths, ConfigScope, DEFAULT_CONFIG_TOML,
    DEFAULT_PROJECT_CONFIG_TOML, EffectiveConfig, compose_effective_config, migrate_config_file,
    persist_config_mutation, validate_config_file, validate_config_text,
};
use crate::control::{decode_control_frame, encode_control_body};
use crate::error::{MezError, Result};
use crate::ids::ClientId;
use crate::mcp::McpRegistry;
use crate::memory::{
    MemoryKind, MemoryRecord, MemoryRetentionPolicy, MemoryScope, MemorySearchRequest,
    MemorySource, MemoryState, PersistentMemoryStore,
};
use crate::project::{
    ProjectTrustRecord, ProjectTrustStore, TrustDecision, default_trust_database_path,
    discover_existing_overlays, discover_project_root,
};
use crate::registry::{
    SessionRecord, SessionRegistry, records_to_json, resolve_session_record_target,
};
use crate::runtime::{
    AuxiliarySocketKind, DEFAULT_SOCKET_NAME, MEZ_ENV_FIELD_SEPARATOR, RuntimeEnv,
    RuntimeLifecycleState, RuntimeSessionService, auxiliary_socket_path_for_control_socket,
    bind_control_socket, default_socket_directory, prune_stale_socket_files_in_directory,
    runtime_effective_config_value, runtime_ui_theme_from_config, socket_path_for_name,
};
use crate::shell::resolve_shell;
use crate::snapshot::{
    LayoutLoadPlan, SessionSnapshotPayload, SnapshotKind, SnapshotRepository,
    SnapshotRestoreResult, SnapshotRollbackPlan, SnapshotState,
};
use crate::terminal::{
    AttachedTerminalClientLoopConfig, AttachedTerminalOutputModes, ClientViewRole,
    TerminalClientLoopConfig, TerminalCursorStyle, UiTheme, attached_terminal_output_disconnected,
};
use crate::transcript::AgentTranscriptStore;
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

pub use dispatch::{CliEnv, run, run_with};

#[cfg(test)]
use args::parse_cli_arg_group;
use attach::{run_attach, run_list};
use auth::run_auth;
use config::{json_string_array, run_config};
use control_client::{
    incomplete_control_response_error, read_control_response_frames, run_control_request,
};
use env::{
    CliCommand, CliInvocation, CliInvocationParse, SocketSelection, cli_idempotency_key,
    registry_root, render_cli_help, render_cli_version, selected_socket_path,
    terminal_size_from_fd_or_environment,
};
use issue::run_issue;
use json::{
    CliOutputFormat, current_unix_seconds, diagnostics_json, json_escape, json_optional,
    serialize_json, write_control_response, write_json_or_plain,
};
use mcp::{load_runtime_config_layers, run_mcp};
use memory::run_memory;
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
