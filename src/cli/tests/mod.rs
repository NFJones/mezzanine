//! Regression coverage for the cli tests subsystem.
//!
//! These tests describe the behavior protected by the repository
//! specification and workflow guidance. Keeping the scenarios documented
//! makes failures easier to map back to the user-visible contract.

// CLI module tests.

use super::attach::{
    AttachRenderAction, AttachedRuntimeEventStream,
    run_control_socket_attached_observer_client_loop_async,
    run_control_socket_attached_primary_client_loop_async,
    run_control_socket_attached_primary_client_loop_async_with_runtime_events,
    terminal_step_control_request, terminal_step_response_line_style_spans,
    terminal_step_response_output_modes, terminal_step_response_refresh_requirement,
};
use super::mcp::load_runtime_config_layers_for_directory;
use super::serve::assign_unique_live_session_id;
use super::{
    AuthPaths, AuthStore, CliEnv, CliInvocation, OpenAiProviderCredential, OsString, PathBuf,
    SocketSelection, UnixStream, read_control_response_frames, selected_socket_path,
};
use crate::config::{DEFAULT_CONFIG_TOML, compose_effective_config};
use crate::control::{decode_control_frame, encode_control_body};
use crate::host::async_runtime::AsyncFakeAttachedTerminalIo;
use crate::host::shell::resolve_shell;
use crate::host::terminal::{
    AttachedTerminalFdReadiness, AttachedTerminalFdRole, TerminalFdInterest,
};
use crate::runtime::{MEZ_ENV_FIELD_SEPARATOR, RuntimeEnv, default_socket_directory};
use crate::runtime::{bind_control_socket, effective_uid_for_tests};
use crate::security::project::{ProjectTrustStore, TrustDecision};
use crate::storage::registry::SessionRegistry;
use crate::storage::registry::{RegistrySessionState, SessionRecord};
use crate::storage::snapshot::{SnapshotKind, SnapshotRepository};
use crate::storage::snapshot::{SnapshotManifest, SnapshotPaneCapture, SnapshotState};
use mez_mux::layout::Size;
use mez_mux::session::Session;
use std::fs;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};

/// Runs the test env operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn test_env(name: &str) -> (CliEnv, PathBuf) {
    let home = std::env::temp_dir().join(format!("mez-cli-test-{name}-{}", std::process::id()));
    let runtime_tmp = home.join("runtime");
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(&runtime_tmp).unwrap();
    fs::set_permissions(&runtime_tmp, fs::Permissions::from_mode(0o700)).unwrap();
    (
        CliEnv {
            home: Some(home.clone()),
            shell: Some(OsString::from("/bin/sh")),
            mez: None,
            runtime: RuntimeEnv {
                mez_tmpdir: Some(runtime_tmp.into_os_string()),
                xdg_runtime_dir: None,
                uid: effective_uid_for_tests(),
            },
        },
        home,
    )
}

/// Builds one framed JSON-RPC event notification for attach-loop tests.
///
/// # Parameters
/// - `event_type`: The event type to encode in the notification method and
///   params object.
fn event_notification_frame(event_type: &str) -> Vec<u8> {
    encode_control_body(&format!(
        r#"{{"jsonrpc":"2.0","method":"event/{event_type}","params":{{"event_id":1,"time":"2026-05-21T00:00:00Z","event_type":"{event_type}","session_id":null,"object":{{}}}}}}"#
    ))
}

/// Builds one readable input readiness record for async attached-terminal tests.
fn readable_input_readiness() -> AttachedTerminalFdReadiness {
    AttachedTerminalFdReadiness {
        role: AttachedTerminalFdRole::Input,
        fd: std::io::stdin().as_raw_fd(),
        interest: TerminalFdInterest::read(),
        readable: true,
        writable: false,
        hangup: false,
        error: false,
    }
}

/// Runs the run with operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn run_with(
    args: Vec<String>,
    env: CliEnv,
    interactive: bool,
    stdout: &mut Vec<u8>,
    stderr: &mut Vec<u8>,
) -> crate::error::Result<()> {
    block_on_cli(super::run_with(
        with_json_output(args),
        env,
        interactive,
        stdout,
        stderr,
    ))
}

/// Runs the run with plain operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn run_with_plain(
    args: Vec<String>,
    env: CliEnv,
    interactive: bool,
    stdout: &mut Vec<u8>,
    stderr: &mut Vec<u8>,
) -> crate::error::Result<()> {
    block_on_cli(super::run_with(args, env, interactive, stdout, stderr))
}

/// Runs the block on cli operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn block_on_cli<F>(future: F) -> crate::error::Result<()>
where
    F: std::future::Future<Output = crate::error::Result<()>>,
{
    tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .unwrap()
        .block_on(future)
}

/// Spawns one deterministic noninteractive attach control stub.
///
/// The helper reads exactly the `control/initialize` and follow-up request
/// frames emitted by `mez attach` when stdout is not a terminal, then returns
/// prebuilt responses without depending on runtime session service state.
fn spawn_noninteractive_attach_stub_server(
    listener: std::os::unix::net::UnixListener,
    expected_follow_up_method: Option<&'static str>,
    initialize_result: &'static str,
    follow_up_result: Option<&'static str>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let (mut stream, _addr) = listener.accept().unwrap();
        let request = read_control_response_frames(&mut stream, 4096, 1).unwrap();
        let (initialize, consumed) = decode_control_frame(&request, 4096).unwrap();
        assert!(
            initialize.contains(r#""method":"control/initialize""#),
            "{initialize}"
        );
        stream
            .write_all(&encode_control_body(initialize_result))
            .unwrap();
        if let Some(expected_follow_up_method) = expected_follow_up_method {
            let request = if consumed < request.len() {
                request[consumed..].to_vec()
            } else {
                read_control_response_frames(&mut stream, 4096, 1).unwrap()
            };
            let (follow_up, _) = decode_control_frame(&request, 4096).unwrap();
            assert!(follow_up.contains(expected_follow_up_method), "{follow_up}");
            stream
                .write_all(&encode_control_body(
                    follow_up_result.expect("follow-up response is required"),
                ))
                .unwrap();
        }
    })
}

/// Runs the with json output operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn with_json_output(mut args: Vec<String>) -> Vec<String> {
    if !args.iter().any(|arg| arg == "--json") {
        args.insert(1, "--json".to_string());
    }
    args
}

/// Runs the wait for path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn wait_for_path(path: &std::path::Path) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        thread::yield_now();
    }
    false
}

/// Runs the connect when ready operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn connect_when_ready(path: &std::path::Path) -> Option<UnixStream> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match UnixStream::connect(path) {
            Ok(stream) => return Some(stream),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                thread::yield_now();
            }
            Err(_) => return None,
        }
    }
    None
}

mod attach;
mod auth;
mod config;
mod dispatch;
mod issues;
mod mcp;
mod memory;
mod new_serve;
mod snapshot;
mod terminal_protocol;
