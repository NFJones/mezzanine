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
use crate::async_runtime::AsyncFakeAttachedTerminalIo;
use crate::config::{DEFAULT_CONFIG_TOML, compose_effective_config};
use crate::control::{decode_control_frame, encode_control_body};
use crate::layout::Size;
use crate::project::{ProjectTrustStore, TrustDecision};
use crate::registry::SessionRegistry;
use crate::registry::{RegistrySessionState, SessionRecord};
use crate::runtime::{MEZ_ENV_FIELD_SEPARATOR, RuntimeEnv, default_socket_directory};
use crate::runtime::{bind_control_socket, effective_uid_for_tests};
use crate::session::Session;
use crate::shell::resolve_shell;
use crate::snapshot::{SnapshotKind, SnapshotRepository};
use crate::snapshot::{SnapshotManifest, SnapshotPaneCapture, SnapshotState};
use std::fs;
use std::io::{Read, Write};
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
        let request = read_control_response_frames(
            &mut stream,
            4096,
            if expected_follow_up_method.is_some() {
                2
            } else {
                1
            },
        )
        .unwrap();
        let (initialize, consumed) = decode_control_frame(&request, 4096).unwrap();
        assert!(
            initialize.contains(r#""method":"control/initialize""#),
            "{initialize}"
        );
        stream
            .write_all(&encode_control_body(initialize_result))
            .unwrap();
        if let Some(expected_follow_up_method) = expected_follow_up_method {
            let (follow_up, _) = decode_control_frame(&request[consumed..], 4096).unwrap();
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

/// Verifies help mentions mez commands.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
/// Verifies the top-level help text covers the visible command surface,
/// including the long-form aliases accepted by command dispatch.
fn help_mentions_mez_commands() {
    let (env, home) = test_env("help");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec!["mez".to_string(), "help".to_string()],
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains("Usage: mez"));
    assert!(output.contains("snapshot"));
    assert!(output.contains("new-session"));
    assert!(output.contains("daemon"));
    assert!(output.contains("list-sessions"));
    assert!(output.contains("attach-session"));
    assert!(output.contains("detach-client"));
    assert!(output.contains("version"));
    assert!(output.contains("--version"));
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies clap renders command-local help while preserving the legacy
/// no-subcommand config usage path.
///
/// This regression scenario protects the process-argv presentation contract
/// now that top-level and command-local help are generated from the clap command
/// tree instead of handwritten strings.
#[test]
fn clap_renders_config_help_for_help_flag_and_empty_command() {
    let (env, home) = test_env("config-help");
    let mut flag_stdout = Vec::new();
    let mut flag_stderr = Vec::new();

    run_with_plain(
        vec![
            "mez".to_string(),
            "config".to_string(),
            "--help".to_string(),
        ],
        env.clone(),
        false,
        &mut flag_stdout,
        &mut flag_stderr,
    )
    .unwrap();

    let flag_output = String::from_utf8(flag_stdout).unwrap();
    assert!(flag_output.contains("Usage: mez config"), "{flag_output}");
    assert!(flag_output.contains("validate"), "{flag_output}");
    assert!(flag_stderr.is_empty());

    let mut empty_stdout = Vec::new();
    let mut empty_stderr = Vec::new();
    run_with_plain(
        vec!["mez".to_string(), "config".to_string()],
        env,
        false,
        &mut empty_stdout,
        &mut empty_stderr,
    )
    .unwrap();

    let empty_output = String::from_utf8(empty_stdout).unwrap();
    assert!(empty_output.contains("Usage: mez config"), "{empty_output}");
    assert!(empty_output.contains("trust"), "{empty_output}");
    assert!(empty_stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies legacy top-level command aliases still dispatch through the typed
/// clap command tree.
///
/// This regression scenario ensures the argv parser refactor does not remove
/// long-form aliases accepted by previous releases.
#[test]
fn process_cli_aliases_still_dispatch() {
    let (env, home) = test_env("cli-aliases");
    let mut new_stdout = Vec::new();
    let mut new_stderr = Vec::new();

    run_with_plain(
        vec![
            "mez".to_string(),
            "new-session".to_string(),
            "--dry-run".to_string(),
        ],
        env.clone(),
        false,
        &mut new_stdout,
        &mut new_stderr,
    )
    .unwrap();

    let new_output = String::from_utf8(new_stdout).unwrap();
    assert!(new_output.contains("dry_run: true"), "{new_output}");
    assert!(new_stderr.is_empty());

    let mut list_stdout = Vec::new();
    let mut list_stderr = Vec::new();
    run_with(
        vec!["mez".to_string(), "list-sessions".to_string()],
        env,
        false,
        &mut list_stdout,
        &mut list_stderr,
    )
    .unwrap();

    assert_eq!(String::from_utf8(list_stdout).unwrap(), "[]\n");
    assert!(list_stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies invocation prefers in pane mez socket without explicit selector.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn invocation_prefers_in_pane_mez_socket_without_explicit_selector() {
    let runtime = RuntimeEnv {
        mez_tmpdir: Some(OsString::from("/tmp")),
        xdg_runtime_dir: None,
        uid: 1000,
    };
    let mez = OsString::from(format!(
        "/tmp/mez-1000/in-pane.sock{}session=$1",
        MEZ_ENV_FIELD_SEPARATOR
    ));

    let invocation = CliInvocation::parse(
        &["mez".to_string(), "list".to_string()],
        &runtime,
        Some(&mez),
    )
    .unwrap();

    assert_eq!(
        selected_socket_path(&invocation.socket_selection),
        &PathBuf::from("/tmp/mez-1000/in-pane.sock")
    );
    assert!(matches!(
        invocation.socket_selection,
        SocketSelection::InPane(_)
    ));
}

/// Verifies explicit socket selector overrides in pane mez socket.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn explicit_socket_selector_overrides_in_pane_mez_socket() {
    let runtime = RuntimeEnv {
        mez_tmpdir: Some(OsString::from("/tmp")),
        xdg_runtime_dir: None,
        uid: 1000,
    };
    let mez = OsString::from(format!(
        "/tmp/mez-1000/in-pane.sock{}session=$1",
        MEZ_ENV_FIELD_SEPARATOR
    ));

    let invocation = CliInvocation::parse(
        &[
            "mez".to_string(),
            "-S".to_string(),
            "/tmp/explicit.sock".to_string(),
            "list".to_string(),
        ],
        &runtime,
        Some(&mez),
    )
    .unwrap();

    assert_eq!(
        selected_socket_path(&invocation.socket_selection),
        &PathBuf::from("/tmp/explicit.sock")
    );
    assert!(matches!(
        invocation.socket_selection,
        SocketSelection::Explicit(_)
    ));
}

/// Verifies config init creates default config.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn config_init_creates_default_config() {
    let (env, home) = test_env("config-init");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec!["mez".to_string(), "config".to_string(), "init".to_string()],
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    let config_path = home.join(".config").join("mezzanine").join("config.toml");
    assert!(config_path.is_file());
    assert_eq!(
        fs::read_to_string(config_path).unwrap(),
        DEFAULT_CONFIG_TOML
    );

    let _ = fs::remove_dir_all(home);
}

/// Verifies config validate and get work without existing file.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn config_validate_and_get_work_without_existing_file() {
    let (env, home) = test_env("config-validate-get");
    let mut validate_stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec![
            "mez".to_string(),
            "config".to_string(),
            "validate".to_string(),
        ],
        env.clone(),
        false,
        &mut validate_stdout,
        &mut stderr,
    )
    .unwrap();
    assert!(
        String::from_utf8(validate_stdout)
            .unwrap()
            .contains(r#""valid":true"#)
    );

    let mut get_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "config".to_string(),
            "get".to_string(),
            "history.lines".to_string(),
        ],
        env.clone(),
        false,
        &mut get_stdout,
        &mut stderr,
    )
    .unwrap();

    let output = String::from_utf8(get_stdout).unwrap();
    assert!(output.contains(r#""path":"history.lines""#));
    assert!(output.contains(r#""value":10000"#));
    assert!(output.contains(r#""layers":["#));

    let mut layers_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "config".to_string(),
            "layers".to_string(),
        ],
        env,
        false,
        &mut layers_stdout,
        &mut stderr,
    )
    .unwrap();
    let layers = String::from_utf8(layers_stdout).unwrap();
    assert!(layers.contains(r#""layer_type":"user""#), "{layers}");
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies that offline `mez config set` and `mez config unset` use the same
/// validated mutation planner as runtime config changes while targeting only
/// the selected user-private config file by default. This guards against the
/// CLI silently editing arbitrary files outside the Mezzanine config root.
#[test]
fn config_set_and_unset_persist_user_private_config() {
    let (env, home) = test_env("config-set-unset-user");
    let mut set_stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec![
            "mez".to_string(),
            "config".to_string(),
            "set".to_string(),
            "history.lines".to_string(),
            "2048".to_string(),
        ],
        env.clone(),
        false,
        &mut set_stdout,
        &mut stderr,
    )
    .unwrap();

    let set_output = String::from_utf8(set_stdout).unwrap();
    assert!(set_output.contains(r#""persisted":true"#), "{set_output}");
    assert!(set_output.contains(r#""scope":"user""#), "{set_output}");
    let config_path = home.join(".config").join("mezzanine").join("config.toml");
    let text = fs::read_to_string(&config_path).unwrap();
    assert!(text.contains("lines = 2048"), "{text}");

    let mut unset_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "config".to_string(),
            "unset".to_string(),
            "history.lines".to_string(),
        ],
        env,
        false,
        &mut unset_stdout,
        &mut stderr,
    )
    .unwrap();

    let unset_output = String::from_utf8(unset_stdout).unwrap();
    assert!(
        unset_output.contains(r#""operation":"unset""#),
        "{unset_output}"
    );
    let text = fs::read_to_string(config_path).unwrap();
    assert!(!text.contains("lines = 2048"), "{text}");
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies that project-scoped offline config persistence is gated by a
/// trusted project-root record before the CLI creates or edits a project
/// overlay. This covers the same safety boundary as runtime `PersistTarget`
/// validation for non-live project mutations.
#[test]
fn config_set_project_scope_requires_trusted_project_root() {
    let (env, home) = test_env("config-set-project");
    let project = home.join("repo");
    fs::create_dir_all(project.join(".git")).unwrap();
    let project_config = project.join(".mezzanine").join("config.toml");
    let mut stderr = Vec::new();
    let mut rejected_stdout = Vec::new();

    let error = run_with(
        vec![
            "mez".to_string(),
            "config".to_string(),
            "set".to_string(),
            "history.lines".to_string(),
            "12".to_string(),
            "--scope".to_string(),
            "project".to_string(),
            "--file".to_string(),
            project_config.to_string_lossy().to_string(),
        ],
        env.clone(),
        false,
        &mut rejected_stdout,
        &mut stderr,
    )
    .unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::Conflict);
    assert!(!project_config.exists());

    let mut trust_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "config".to_string(),
            "trust".to_string(),
            "trust".to_string(),
            project.to_string_lossy().to_string(),
        ],
        env.clone(),
        false,
        &mut trust_stdout,
        &mut stderr,
    )
    .unwrap();

    let mut set_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "config".to_string(),
            "set".to_string(),
            "history.lines".to_string(),
            "12".to_string(),
            "--scope".to_string(),
            "project".to_string(),
            "--file".to_string(),
            project_config.to_string_lossy().to_string(),
        ],
        env,
        false,
        &mut set_stdout,
        &mut stderr,
    )
    .unwrap();

    let output = String::from_utf8(set_stdout).unwrap();
    assert!(output.contains(r#""scope":"project""#), "{output}");
    let project_text = fs::read_to_string(&project_config).unwrap();
    assert!(project_text.contains("approval_policy = \"ask\""));
    assert!(project_text.contains("lines = 12"));
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies config trust subcommands persist project decisions.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn config_trust_subcommands_persist_project_decisions() {
    let (env, home) = test_env("config-trust");
    let project = home.join("repo");
    fs::create_dir_all(project.join(".git")).unwrap();
    let mut trust_stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec![
            "mez".to_string(),
            "config".to_string(),
            "trust".to_string(),
            "trust".to_string(),
            project.to_string_lossy().to_string(),
        ],
        env.clone(),
        false,
        &mut trust_stdout,
        &mut stderr,
    )
    .unwrap();
    assert!(
        String::from_utf8(trust_stdout)
            .unwrap()
            .contains(r#""state":"trusted""#)
    );

    let mut list_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "config".to_string(),
            "trust".to_string(),
            "list".to_string(),
        ],
        env,
        false,
        &mut list_stdout,
        &mut stderr,
    )
    .unwrap();
    let output = String::from_utf8(list_stdout).unwrap();
    assert!(output.contains(r#""state":"trusted""#));
    assert!(output.contains("repo"));
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies that runtime startup migrates an existing primary user config
/// before normal layer composition. This protects launch from failing on
/// historical keys that are valid migration inputs but invalid current-schema
/// configuration after migration has completed.
#[test]
fn startup_config_layers_migrate_existing_primary_config() {
    let (env, home) = test_env("startup-primary-migration");
    let paths = env.config_paths().unwrap();
    fs::create_dir_all(paths.root()).unwrap();
    fs::write(
        paths.root().join("config.toml"),
        "version = 1\n[terminal]\nnested_muxxer = \"disabled\"\n[session]\ndefault_command = \"vim\"\n",
    )
    .unwrap();
    let project = home.join("repo");
    fs::create_dir_all(&project).unwrap();

    let layers =
        load_runtime_config_layers_for_directory(&paths, &ProjectTrustStore::default(), &project)
            .unwrap();
    let effective = compose_effective_config(&layers).unwrap();
    let migrated = fs::read_to_string(paths.root().join("config.toml")).unwrap();

    assert_eq!(layers.len(), 1);
    assert_eq!(effective.get("version"), Some("17"));
    assert_eq!(
        effective.get("terminal.nested_multiplexer"),
        Some("disabled")
    );
    assert_eq!(
        effective.get("agents.implementation_pressure_after_shell_actions"),
        Some("3")
    );
    assert!(migrated.contains("version = 17"));
    assert!(migrated.contains("emoji_width = \"wide\""));
    assert!(migrated.contains("provider_refresh_leeway_seconds = 86400"));
    assert!(migrated.contains("implementation_pressure_after_shell_actions = 3"));
    assert!(migrated.contains("[model_presets.deepseek]"));
    assert!(!migrated.contains("nested_muxxer"));
    assert!(!migrated.contains("default_command"));

    let _ = fs::remove_dir_all(home);
}

/// Verifies that runtime startup config assembly discovers project overlays
/// from the invocation directory up to the project root, leaves them skipped
/// while trust is pending, and applies them in root-to-leaf precedence once the
/// canonical project root is trusted.
#[test]
fn startup_config_layers_discover_project_overlays_and_apply_trust() {
    let (env, home) = test_env("startup-project-overlays");
    let paths = env.config_paths().unwrap();
    fs::create_dir_all(paths.root()).unwrap();
    fs::write(paths.root().join("config.toml"), "[history]\nlines = 3\n").unwrap();
    let project = home.join("repo");
    let nested = project.join("src").join("crate");
    fs::create_dir_all(project.join(".git")).unwrap();
    fs::create_dir_all(nested.join(".mezzanine")).unwrap();
    fs::create_dir_all(project.join(".mezzanine")).unwrap();
    fs::write(
        project.join(".mezzanine/config.toml"),
        "version = 17\n[history]\nlines = 7\n",
    )
    .unwrap();
    fs::write(
        nested.join(".mezzanine/config.toml"),
        "version = 17\n[history]\nlines = 11\n",
    )
    .unwrap();

    let pending_layers =
        load_runtime_config_layers_for_directory(&paths, &ProjectTrustStore::default(), &nested)
            .unwrap();
    let pending_effective = compose_effective_config(&pending_layers).unwrap();

    assert_eq!(pending_layers.len(), 3);
    assert!(
        pending_layers
            .iter()
            .filter(|layer| layer.scope == crate::config::ConfigScope::ProjectOverlay)
            .all(|layer| !layer.trusted)
    );
    assert_eq!(pending_effective.get("history.lines"), Some("3"));
    assert_eq!(
        pending_effective.source_for("history.lines"),
        Some("primary")
    );

    let mut trust_store = ProjectTrustStore::default();
    trust_store
        .decide(project.clone(), TrustDecision::Trusted, None)
        .unwrap();
    let trusted_layers =
        load_runtime_config_layers_for_directory(&paths, &trust_store, &nested).unwrap();
    let trusted_effective = compose_effective_config(&trusted_layers).unwrap();

    assert!(
        trusted_layers
            .iter()
            .filter(|layer| layer.scope == crate::config::ConfigScope::ProjectOverlay)
            .all(|layer| layer.trusted)
    );
    assert_eq!(trusted_effective.get("history.lines"), Some("11"));
    assert_eq!(
        trusted_effective.source_for("history.lines"),
        Some("project:2")
    );

    let _ = fs::remove_dir_all(home);
}

/// Verifies noninteractive new requires dry run.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn noninteractive_new_requires_dry_run() {
    let (env, home) = test_env("new-fails");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let error = run_with(
        vec!["mez".to_string(), "new".to_string()],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);

    let _ = fs::remove_dir_all(home);
}

/// Verifies bare mez enters new session path.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn bare_mez_enters_new_session_path() {
    let (env, home) = test_env("bare-new");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let error = run_with(
        vec!["mez".to_string()],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
    assert!(error.message().contains("interactive terminal"));

    let _ = fs::remove_dir_all(home);
}

/// Verifies that `mez new` does not reuse the default control socket when an
/// existing session is already serving it. The interactive command must launch
/// a daemon on a fresh socket so the subsequent attach step cannot accidentally
/// reconnect to the older session.
#[test]
fn new_session_default_socket_allocates_fresh_socket_when_default_is_active() {
    let (env, home) = test_env("new-fresh-socket");
    let directory = default_socket_directory(&env.runtime).unwrap();
    let default_socket =
        crate::runtime::socket_path_for_name(&directory.path, crate::runtime::DEFAULT_SOCKET_NAME)
            .unwrap();
    let _listener = bind_control_socket(&default_socket, env.runtime.uid).unwrap();

    let selection = super::serve::socket_selection_for_new_session(&SocketSelection::Default(
        default_socket.clone(),
    ))
    .unwrap();

    assert_ne!(selected_socket_path(&selection), &default_socket);
    assert!(
        selected_socket_path(&selection)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains(".new."))
    );

    let _ = fs::remove_dir_all(home);
}

/// Verifies that early background-daemon startup failures include the child
/// process stderr. This keeps `mez new` launch failures diagnosable when the
/// foreground client has not yet connected to the daemon socket.
#[test]
fn new_session_daemon_startup_error_includes_child_stderr() {
    let (_env, home) = test_env("new-daemon-stderr");
    let socket_path = home.join("runtime").join("daemon.sock");
    let mut command = std::process::Command::new("/bin/sh");
    command
        .arg("-c")
        .arg("printf 'daemon config failed\\n' >&2; exit 1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let error = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .unwrap()
        .block_on(async {
            let mut daemon = super::serve::BackgroundControlDaemon::spawn(command).unwrap();
            super::serve::wait_for_background_control_daemon(&socket_path, &mut daemon).await
        })
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert!(
        error.message().contains("daemon config failed"),
        "{}",
        error.message()
    );
    let _ = fs::remove_dir_all(home);
}

/// Verifies background daemon startup waits for a complete control response.
///
/// The daemon binds its socket before pane startup and actor supervision are
/// ready. A connect-only readiness check can therefore report success while the
/// child is still starting or about to exit, making the immediate attach fail
/// with a reset socket. This regression server accepts the probe immediately
/// but delays its framed response so the wait helper must not return early.
#[test]
fn new_session_daemon_startup_waits_for_control_probe_response() {
    let (_env, home) = test_env("new-daemon-probe-response");
    let socket_path = home.join("runtime").join("daemon-probe.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _addr) = listener.accept().unwrap();
        let mut request = [0; 1024];
        let read = stream.read(&mut request).unwrap();
        assert!(
            String::from_utf8_lossy(&request[..read]).contains("cli-startup-probe"),
            "{}",
            String::from_utf8_lossy(&request[..read])
        );
        thread::sleep(Duration::from_millis(100));
        stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-startup-probe","error":{"code":-32003,"message":"first control request must be control/initialize"}}"#,
            ))
            .unwrap();
    });
    let mut command = std::process::Command::new("/bin/sh");
    command
        .arg("-c")
        .arg("sleep 5")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let started = Instant::now();
    tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .unwrap()
        .block_on(async {
            let mut daemon = super::serve::BackgroundControlDaemon::spawn(command).unwrap();
            super::serve::wait_for_background_control_daemon(&socket_path, &mut daemon)
                .await
                .unwrap();
            daemon.terminate_for_test().await;
        });

    assert!(
        started.elapsed() >= Duration::from_millis(80),
        "startup probe returned before the control response was written"
    );
    server.join().unwrap();
    let _ = fs::remove_dir_all(home);
}

/// Verifies dry run new builds default session model.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn dry_run_new_builds_default_session_model() {
    let (env, home) = test_env("new-dry-run");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with_plain(
        vec![
            "mez".to_string(),
            "new".to_string(),
            "--dry-run".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains("session_id: $1"));
    assert!(output.contains("window_count: 1"));
    assert!(output.contains("pane_count: 1"));
    assert!(output.contains("dry_run: true"));

    let _ = fs::remove_dir_all(home);
}

/// Verifies that live daemon sessions do not reuse the deterministic `$1`
/// in-memory construction id. The durable registry keys records by session id;
/// if two independently launched daemons both publish `$1`, the later upsert
/// replaces the earlier record and `mez list` hides active sessions.
#[test]
fn live_daemon_session_ids_are_unique_for_registry_listing() {
    let (env, home) = test_env("live-session-id-registry");
    let directory = default_socket_directory(&env.runtime).unwrap();
    let registry = SessionRegistry::new(directory.path.clone(), env.runtime.uid);
    crate::runtime::ensure_private_socket_directory(&directory.path, env.runtime.uid).unwrap();
    let shell = resolve_shell(Some(OsString::from("/bin/sh"))).unwrap();
    let mut first = Session::new_default(shell.clone(), Size::new(80, 24).unwrap());
    let mut second = Session::new_default(shell, Size::new(80, 24).unwrap());
    assign_unique_live_session_id(&mut first).unwrap();
    assign_unique_live_session_id(&mut second).unwrap();
    assert_ne!(first.id, second.id);
    let first_socket = directory.path.join("first.sock");
    let second_socket = directory.path.join("second.sock");
    fs::write(&first_socket, "").unwrap();
    fs::write(&second_socket, "").unwrap();

    registry
        .upsert(SessionRecord::from_session(&first, first_socket, 100, None))
        .unwrap();
    registry
        .upsert(SessionRecord::from_session(
            &second,
            second_socket,
            101,
            None,
        ))
        .unwrap();

    let records = registry.list().unwrap();
    assert_eq!(records.len(), 2);

    let _ = fs::remove_dir_all(directory.path);
    let _ = fs::remove_dir_all(home);
}

/// Verifies serve starts foreground control daemon.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn serve_starts_foreground_control_daemon() {
    let (env, home) = test_env("serve-control");
    let socket = home.join("runtime").join("serve.sock");
    let socket_for_server = socket.clone();
    let server = thread::spawn(move || {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = run_with(
            vec![
                "mez".to_string(),
                "-S".to_string(),
                socket_for_server.to_string_lossy().to_string(),
                "serve".to_string(),
                "--no-aux-sockets".to_string(),
                "--max-control-connections".to_string(),
                "1".to_string(),
            ],
            env,
            false,
            &mut stdout,
            &mut stderr,
        );
        (
            result,
            String::from_utf8(stdout).unwrap(),
            String::from_utf8(stderr).unwrap(),
        )
    });

    let mut stream =
        connect_when_ready(&socket).expect("serve command did not accept socket connections");
    let initialize = r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"mez-test","requested_version":1,"requested_role":"primary","client":{"name":"mez-test","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}}}}"#;
    let get = r#"{"jsonrpc":"2.0","id":"get","method":"session/get","params":{}}"#;
    stream.write_all(&encode_control_body(initialize)).unwrap();
    stream.write_all(&encode_control_body(get)).unwrap();
    stream.flush().unwrap();

    let response = read_control_response_frames(&mut stream, 1024 * 1024, 2).unwrap();
    let (initialize_response, consumed) = decode_control_frame(&response, 1024 * 1024).unwrap();
    let (session_response, _) = decode_control_frame(&response[consumed..], 1024 * 1024).unwrap();
    assert!(initialize_response.contains(r#""granted_role":"primary""#));
    assert!(session_response.contains(r#""session_id":"$"#));
    drop(stream);

    let (result, stdout, stderr) = server.join().unwrap();
    result.unwrap();
    assert!(stdout.contains(r#""serving":true"#));
    assert!(stdout.contains("serve.sock"));
    assert!(stderr.is_empty());
    assert!(!socket.exists());

    let _ = fs::remove_dir_all(home);
}

/// Verifies synchronous control-response reads fail at the socket boundary when
/// EOF arrives before a full protocol frame. Callers should not pass partial
/// buffers to the strict frame decoder and leak its low-level header diagnostic
/// as a user-facing CLI crash.
#[test]
fn read_control_response_frames_rejects_eof_before_complete_frame() {
    let (mut writer, mut reader) = UnixStream::pair().unwrap();
    writer.write_all(b"Content-Length: 16\r\n").unwrap();
    writer.flush().unwrap();
    drop(writer);

    let error = read_control_response_frames(&mut reader, 1024 * 1024, 1).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert!(
        error
            .message()
            .contains("control socket closed before complete response frame"),
        "{error}"
    );
    assert!(
        !error.message().contains("missing header terminator"),
        "{error}"
    );
}

/// Verifies that daemon startup does not spawn an auth refresh worker when the
/// persisted OpenAI access token is still well outside the refresh leeway
/// window. This keeps the launch-time background refresh trigger conditional
/// rather than starting network work on every session.
#[test]
fn serve_skips_background_auth_refresh_when_openai_token_is_still_fresh() {
    let (env, home) = test_env("serve-auth-refresh-fresh");
    let paths = env.config_paths().unwrap();
    let auth_store = AuthStore::new(AuthPaths::under_config_root(paths.root()));
    let credential_store = auth_store.file_credential_store("openai").unwrap();
    auth_store
        .login_openai_provider_credential(
            "default",
            OpenAiProviderCredential {
                api_key: "access-secret".to_string(),
                refresh_token: Some("refresh-secret".to_string()),
                account_id: Some("acct_123".to_string()),
                organization_id: Some("org_123".to_string()),
                token_expires_at: Some("9999999999".to_string()),
            },
            &credential_store,
        )
        .unwrap();

    assert!(!super::serve::spawn_openai_auth_refresh_if_needed(
        auth_store,
        crate::auth::DEFAULT_PROVIDER_AUTH_REFRESH_LEEWAY_SECONDS,
    ));

    let _ = fs::remove_dir_all(home);
}

/// Verifies serve attach primary requires interactive terminal.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn serve_attach_primary_requires_interactive_terminal() {
    let (env, home) = test_env("serve-attach-noninteractive");
    let socket = home.join("runtime").join("serve.sock");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let error = run_with(
        vec![
            "mez".to_string(),
            "-S".to_string(),
            socket.to_string_lossy().to_string(),
            "serve".to_string(),
            "--attach-primary".to_string(),
            "--no-aux-sockets".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
    assert!(error.message().contains("interactive terminal"));
    assert!(!socket.exists());

    let _ = fs::remove_dir_all(home);
}

/// Verifies serve can start message protocol socket.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn serve_can_start_message_protocol_socket() {
    let (env, home) = test_env("serve-message");
    let control_socket = home.join("runtime").join("serve.sock");
    let message_socket = home.join("runtime").join("serve.message.sock");
    let control_for_server = control_socket.clone();
    let message_for_server = message_socket.clone();
    let server = thread::spawn(move || {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = run_with(
            vec![
                "mez".to_string(),
                "-S".to_string(),
                control_for_server.to_string_lossy().to_string(),
                "serve".to_string(),
                "--no-aux-sockets".to_string(),
                "--message-socket".to_string(),
                message_for_server.to_string_lossy().to_string(),
                "--max-control-connections".to_string(),
                "1".to_string(),
                "--max-message-connections".to_string(),
                "1".to_string(),
            ],
            env,
            false,
            &mut stdout,
            &mut stderr,
        );
        (
            result,
            String::from_utf8(stdout).unwrap(),
            String::from_utf8(stderr).unwrap(),
        )
    });

    assert!(wait_for_path(&control_socket));
    assert!(wait_for_path(&message_socket));

    let mut control_stream =
        connect_when_ready(&control_socket).expect("control socket did not accept connections");
    let initialize = r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"mez-test","requested_version":1,"requested_role":"primary","client":{"name":"mez-test","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}}}}"#;
    control_stream
        .write_all(&encode_control_body(initialize))
        .unwrap();
    control_stream.flush().unwrap();
    let control_response =
        read_control_response_frames(&mut control_stream, 1024 * 1024, 1).unwrap();
    let (control_body, _) = decode_control_frame(&control_response, 1024 * 1024).unwrap();
    assert!(control_body.contains(r#""granted_role":"primary""#));
    drop(control_stream);

    let mut message_stream = UnixStream::connect(&message_socket).unwrap();
    message_stream
        .write_all(&crate::message::encode_mmp_body(
            r#"{"protocol":"mmp/1","type":"hello","role":"default"}"#,
        ))
        .unwrap();
    message_stream.flush().unwrap();
    let mut message_response = vec![0; 4096];
    let read = message_stream.read(&mut message_response).unwrap();
    message_response.truncate(read);
    let (message_body, _) = crate::message::decode_mmp_frame(&message_response, 4096).unwrap();
    assert!(message_body.contains(r#""type":"welcome""#));
    drop(message_stream);

    let (result, stdout, stderr) = server.join().unwrap();
    result.unwrap();
    assert!(stdout.contains(r#""message":true"#));
    assert!(stdout.contains("serve.message.sock"));
    assert!(stderr.is_empty());
    assert!(!control_socket.exists());
    assert!(!message_socket.exists());

    let _ = fs::remove_dir_all(home);
}

/// Verifies serve can start event stream socket.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn serve_can_start_event_stream_socket() {
    let (env, home) = test_env("serve-event");
    let control_socket = home.join("runtime").join("serve.sock");
    let event_socket = home.join("runtime").join("serve.event.sock");
    let control_for_server = control_socket.clone();
    let event_for_server = event_socket.clone();
    let server = thread::spawn(move || {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = run_with(
            vec![
                "mez".to_string(),
                "-S".to_string(),
                control_for_server.to_string_lossy().to_string(),
                "serve".to_string(),
                "--no-aux-sockets".to_string(),
                "--event-socket".to_string(),
                event_for_server.to_string_lossy().to_string(),
                "--max-control-connections".to_string(),
                "1".to_string(),
                "--max-event-connections".to_string(),
                "1".to_string(),
            ],
            env,
            false,
            &mut stdout,
            &mut stderr,
        );
        (
            result,
            String::from_utf8(stdout).unwrap(),
            String::from_utf8(stderr).unwrap(),
        )
    });

    assert!(wait_for_path(&control_socket));
    assert!(wait_for_path(&event_socket));
    let mut event_stream =
        connect_when_ready(&event_socket).expect("event socket did not accept connections");
    event_stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    let mut control_stream =
        connect_when_ready(&control_socket).expect("control socket did not accept connections");
    let initialize = r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"mez-test","requested_version":1,"requested_role":"primary","client":{"name":"mez-test","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}}}}"#;
    let kill = r#"{"jsonrpc":"2.0","id":"kill","method":"session/kill","params":{"force":true,"idempotency_key":"kill"}}"#;
    control_stream
        .write_all(&encode_control_body(initialize))
        .unwrap();
    control_stream
        .write_all(&encode_control_body(kill))
        .unwrap();
    control_stream.flush().unwrap();
    let control_response =
        read_control_response_frames(&mut control_stream, 1024 * 1024, 2).unwrap();
    let (initialize_body, consumed) = decode_control_frame(&control_response, 1024 * 1024).unwrap();
    let (kill_body, _) = decode_control_frame(&control_response[consumed..], 1024 * 1024).unwrap();
    assert!(initialize_body.contains(r#""granted_role":"primary""#));
    assert!(kill_body.contains(r#""killed":true"#));
    drop(control_stream);

    let event_response = read_control_response_frames(&mut event_stream, 1024 * 1024, 1).unwrap();
    let (event_body, _) = decode_control_frame(&event_response, 1024 * 1024).unwrap();
    assert!(event_body.contains(r#""method":"event/"#));
    drop(event_stream);

    let (result, stdout, stderr) = server.join().unwrap();
    result.unwrap();
    assert!(stdout.contains(r#""event":true"#));
    assert!(stdout.contains("serve.event.sock"));
    assert!(stderr.is_empty());
    assert!(!control_socket.exists());
    assert!(!event_socket.exists());

    let _ = fs::remove_dir_all(home);
}

/// Verifies serve derives default auxiliary sockets.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn serve_derives_default_auxiliary_sockets() {
    let (env, home) = test_env("serve-default-aux");
    let control_socket = home.join("runtime").join("serve.sock");
    let message_socket = home.join("runtime").join("serve.message.sock");
    let event_socket = home.join("runtime").join("serve.event.sock");
    let control_for_server = control_socket.clone();
    let server = thread::spawn(move || {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = run_with(
            vec![
                "mez".to_string(),
                "-S".to_string(),
                control_for_server.to_string_lossy().to_string(),
                "serve".to_string(),
                "--max-control-connections".to_string(),
                "1".to_string(),
                "--max-message-connections".to_string(),
                "1".to_string(),
                "--max-event-connections".to_string(),
                "1".to_string(),
                "--max-event-batches-per-connection".to_string(),
                "1".to_string(),
            ],
            env,
            false,
            &mut stdout,
            &mut stderr,
        );
        (
            result,
            String::from_utf8(stdout).unwrap(),
            String::from_utf8(stderr).unwrap(),
        )
    });

    assert!(wait_for_path(&control_socket));
    assert!(wait_for_path(&message_socket));
    assert!(wait_for_path(&event_socket));

    let mut message_stream =
        connect_when_ready(&message_socket).expect("message socket did not accept connections");
    message_stream
        .write_all(&crate::message::encode_mmp_body(
            r#"{"protocol":"mmp/1","type":"hello","role":"default"}"#,
        ))
        .unwrap();
    message_stream.flush().unwrap();
    let mut message_response = vec![0; 4096];
    let read = message_stream.read(&mut message_response).unwrap();
    message_response.truncate(read);
    let (message_body, _) = crate::message::decode_mmp_frame(&message_response, 4096).unwrap();
    assert!(message_body.contains(r#""type":"welcome""#));
    drop(message_stream);

    let mut event_stream =
        connect_when_ready(&event_socket).expect("event socket did not accept connections");
    event_stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    let mut control_stream =
        connect_when_ready(&control_socket).expect("control socket did not accept connections");
    let initialize = r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"mez-test","requested_version":1,"requested_role":"primary","client":{"name":"mez-test","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}}}}"#;
    let kill = r#"{"jsonrpc":"2.0","id":"kill","method":"session/kill","params":{"force":true,"idempotency_key":"kill"}}"#;
    control_stream
        .write_all(&encode_control_body(initialize))
        .unwrap();
    control_stream
        .write_all(&encode_control_body(kill))
        .unwrap();
    control_stream.flush().unwrap();
    let control_response =
        read_control_response_frames(&mut control_stream, 1024 * 1024, 2).unwrap();
    let (initialize_body, consumed) = decode_control_frame(&control_response, 1024 * 1024).unwrap();
    let (kill_body, _) = decode_control_frame(&control_response[consumed..], 1024 * 1024).unwrap();
    assert!(initialize_body.contains(r#""granted_role":"primary""#));
    assert!(kill_body.contains(r#""killed":true"#));
    drop(control_stream);

    let event_response = read_control_response_frames(&mut event_stream, 1024 * 1024, 1).unwrap();
    let (event_body, _) = decode_control_frame(&event_response, 1024 * 1024).unwrap();
    assert!(event_body.contains(r#""method":"event/"#));
    drop(event_stream);

    let (result, stdout, stderr) = server.join().unwrap();
    result.unwrap();
    assert!(stdout.contains(r#""message":true"#));
    assert!(stdout.contains(r#""event":true"#));
    assert!(stdout.contains("serve.message.sock"));
    assert!(stdout.contains("serve.event.sock"));
    assert!(stderr.is_empty());
    assert!(!control_socket.exists());
    assert!(!message_socket.exists());
    assert!(!event_socket.exists());

    let _ = fs::remove_dir_all(home);
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

/// Verifies parses socket selection before command.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn parses_socket_selection_before_command() {
    let (env, home) = test_env("socket-selection");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec![
            "mez".to_string(),
            "-L".to_string(),
            "work.sock".to_string(),
            "list-sessions".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    assert_eq!(String::from_utf8(stdout).unwrap(), "[]\n");
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies list reads durable session registry.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn list_reads_durable_session_registry() {
    let (env, home) = test_env("list-registry");
    let directory = default_socket_directory(&env.runtime).unwrap();
    let registry = SessionRegistry::new(directory.path.clone(), env.runtime.uid);
    let socket_path = directory.path.join("default.sock");
    crate::runtime::ensure_private_socket_directory(&directory.path, env.runtime.uid).unwrap();
    fs::write(&socket_path, "").unwrap();
    registry
        .upsert(SessionRecord {
            session_id: "$1".to_string(),
            name: "work".to_string(),
            state: RegistrySessionState::Detached,
            socket_path,
            created_at_unix_seconds: 100,
            last_attach_at_unix_seconds: Some(120),
            window_count: 2,
            client_count: 0,
            primary_available: true,
            authoritative_columns: 100,
            authoritative_rows: 30,
        })
        .unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec!["mez".to_string(), "list".to_string()],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains("\"session_id\":\"$1\""));
    assert!(output.contains("\"index_alias\":\"$1\""));
    assert!(output.contains("\"state\":\"detached\""));
    assert!(output.contains("\"primary_available\":true"));
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(directory.path);
    let _ = fs::remove_dir_all(home);
}

/// Verifies attach uses selected control socket.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attach_uses_selected_control_socket() {
    let (env, home) = test_env("attach-control");
    fs::create_dir_all(&home).unwrap();
    let root = home.join("runtime");
    let socket = root.join("default.sock");
    let listener = match bind_control_socket(&socket, env.runtime.uid) {
        Ok(listener) => listener,
        Err(error)
            if error.kind() == crate::error::MezErrorKind::Io
                && error.message().contains("Operation not permitted") =>
        {
            let _ = fs::remove_dir_all(home);
            return;
        }
        Err(error) => panic!("{error}"),
    };
    let server = spawn_noninteractive_attach_stub_server(
        listener,
        Some(r#""method":"session/get""#),
        r#"{"jsonrpc":"2.0","id":"cli-init","result":{"granted_role":"primary","client_id":"c1"}}"#,
        Some(r#"{"jsonrpc":"2.0","id":"cli","result":{"session":{"session_id":"$1"}}}"#),
    );
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec![
            "mez".to_string(),
            "-S".to_string(),
            socket.to_string_lossy().to_string(),
            "attach".to_string(),
        ],
        env,
        true,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();
    server.join().unwrap();

    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains(r#""session_id":"$1""#));
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies attach observer requests pending observer without session data.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attach_observer_requests_pending_observer_without_session_data() {
    let (env, home) = test_env("attach-observer-control");
    fs::create_dir_all(&home).unwrap();
    let root = home.join("runtime");
    let socket = root.join("default.sock");
    let listener = match bind_control_socket(&socket, env.runtime.uid) {
        Ok(listener) => listener,
        Err(error)
            if error.kind() == crate::error::MezErrorKind::Io
                && error.message().contains("Operation not permitted") =>
        {
            let _ = fs::remove_dir_all(home);
            return;
        }
        Err(error) => panic!("{error}"),
    };
    let server = spawn_noninteractive_attach_stub_server(
        listener,
        None,
        r#"{"jsonrpc":"2.0","id":"cli-init","result":{"granted_role":"pending_observer","approval_pending":true,"session":null}}"#,
        None,
    );
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec![
            "mez".to_string(),
            "-S".to_string(),
            socket.to_string_lossy().to_string(),
            "attach".to_string(),
            "--observer".to_string(),
        ],
        env,
        true,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();
    server.join().unwrap();

    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains(r#""granted_role":"pending_observer""#));
    assert!(output.contains(r#""approval_pending":true"#));
    assert!(output.contains(r#""session":null"#));
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies attach requires interactive terminal.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attach_requires_interactive_terminal() {
    let (env, home) = test_env("attach-noninteractive");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let error = run_with(
        vec!["mez".to_string(), "attach".to_string()],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies snapshot create uses selected control socket.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_create_uses_selected_control_socket() {
    let (env, home) = test_env("snapshot-create-control");
    fs::create_dir_all(&home).unwrap();
    let root = home.join("runtime");
    let socket = root.join("default.sock");
    let listener = match bind_control_socket(&socket, env.runtime.uid) {
        Ok(listener) => listener,
        Err(error)
            if error.kind() == crate::error::MezErrorKind::Io
                && error.message().contains("Operation not permitted") =>
        {
            let _ = fs::remove_dir_all(home);
            return;
        }
        Err(error) => panic!("{error}"),
    };
    let server = thread::spawn(move || {
        let (mut stream, _addr) = listener.accept().unwrap();
        let mut request = vec![0; 4096];
        let read = stream.read(&mut request).unwrap();
        request.truncate(read);
        let (initialize, consumed) = decode_control_frame(&request, 4096).unwrap();
        let (body, _) = decode_control_frame(&request[consumed..], 4096).unwrap();
        assert!(initialize.contains(r#""method":"control/initialize""#));
        assert!(body.contains(r#""method":"snapshot/create""#));
        assert!(body.contains(r#""target":{"default":true}"#));
        assert!(body.contains(r#""name":"checkpoint""#));
        stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-init","result":{"granted_role":"primary"}}"#,
            ))
            .unwrap();
        stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli","result":{"snapshot":{"snapshot_id":"snap1"}}}"#,
            ))
            .unwrap();
    });
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec![
            "mez".to_string(),
            "-S".to_string(),
            socket.to_string_lossy().to_string(),
            "snapshot".to_string(),
            "create".to_string(),
            "--name".to_string(),
            "checkpoint".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();
    server.join().unwrap();

    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains(r#""snapshot_id":"snap1""#));
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies that default `mez attach` discovers a live session through the
/// registry instead of blindly connecting to `default.sock`. Fresh `mez new`
/// sessions use per-session sockets, so default attach must choose an available
/// registry record for primary reattachment.
#[test]
fn default_attach_uses_registry_socket_with_available_primary() {
    let (env, home) = test_env("attach-default-registry");
    let directory = default_socket_directory(&env.runtime).unwrap();
    let registry = SessionRegistry::new(directory.path.clone(), env.runtime.uid);
    crate::runtime::ensure_private_socket_directory(&directory.path, env.runtime.uid).unwrap();
    let first_socket = directory.path.join("first.sock");
    let second_socket = directory.path.join("second.sock");
    fs::write(&first_socket, "").unwrap();
    fs::write(&second_socket, "").unwrap();
    registry
        .upsert(SessionRecord {
            session_id: "$1".to_string(),
            name: "busy".to_string(),
            state: RegistrySessionState::Running,
            socket_path: first_socket,
            created_at_unix_seconds: 100,
            last_attach_at_unix_seconds: None,
            window_count: 1,
            client_count: 1,
            primary_available: false,
            authoritative_columns: 80,
            authoritative_rows: 24,
        })
        .unwrap();
    registry
        .upsert(SessionRecord {
            session_id: "$2".to_string(),
            name: "free".to_string(),
            state: RegistrySessionState::Detached,
            socket_path: second_socket.clone(),
            created_at_unix_seconds: 101,
            last_attach_at_unix_seconds: None,
            window_count: 1,
            client_count: 0,
            primary_available: true,
            authoritative_columns: 80,
            authoritative_rows: 24,
        })
        .unwrap();
    let default_socket = directory.path.join(crate::runtime::DEFAULT_SOCKET_NAME);

    let selection = super::attach::default_attach_socket_selection(
        &SocketSelection::Default(default_socket),
        env.runtime.uid,
        "primary",
    )
    .unwrap()
    .expect("default attach should select a registry socket");

    assert_eq!(selected_socket_path(&selection), &second_socket);

    let _ = fs::remove_dir_all(directory.path);
    let _ = fs::remove_dir_all(home);
}

/// Verifies that default primary attachment does not silently choose a busy
/// registry record. A stale or occupied record should produce a clear conflict
/// instead of connecting to an arbitrary socket and surfacing a confusing
/// initialize failure.
#[test]
fn default_attach_reports_conflict_when_no_primary_is_available() {
    let (env, home) = test_env("attach-default-no-primary");
    let directory = default_socket_directory(&env.runtime).unwrap();
    let registry = SessionRegistry::new(directory.path.clone(), env.runtime.uid);
    crate::runtime::ensure_private_socket_directory(&directory.path, env.runtime.uid).unwrap();
    let busy_socket = directory.path.join("busy.sock");
    fs::write(&busy_socket, "").unwrap();
    registry
        .upsert(SessionRecord {
            session_id: "$1".to_string(),
            name: "busy".to_string(),
            state: RegistrySessionState::Running,
            socket_path: busy_socket,
            created_at_unix_seconds: 100,
            last_attach_at_unix_seconds: Some(110),
            window_count: 1,
            client_count: 1,
            primary_available: false,
            authoritative_columns: 80,
            authoritative_rows: 24,
        })
        .unwrap();
    let default_socket = directory.path.join(crate::runtime::DEFAULT_SOCKET_NAME);

    let error = super::attach::default_attach_socket_selection(
        &SocketSelection::Default(default_socket),
        env.runtime.uid,
        "primary",
    )
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Conflict);
    assert!(
        error
            .message()
            .contains("no registered session currently accepts primary attachment")
    );

    let _ = fs::remove_dir_all(directory.path);
    let _ = fs::remove_dir_all(home);
}

/// Verifies that `mez attach SESSION_ID` resolves the requested session through
/// the durable registry. This keeps explicit attachment attempts from being
/// routed to the default socket when multiple live sessions exist.
#[test]
fn attach_session_id_uses_matching_registry_socket() {
    let (env, home) = test_env("attach-session-id");
    let directory = default_socket_directory(&env.runtime).unwrap();
    let registry = SessionRegistry::new(directory.path.clone(), env.runtime.uid);
    crate::runtime::ensure_private_socket_directory(&directory.path, env.runtime.uid).unwrap();
    let default_socket = directory.path.join(crate::runtime::DEFAULT_SOCKET_NAME);
    let target_socket = directory.path.join("target.sock");
    fs::write(&target_socket, "").unwrap();
    registry
        .upsert(SessionRecord {
            session_id: "$target".to_string(),
            name: "target".to_string(),
            state: RegistrySessionState::Detached,
            socket_path: target_socket.clone(),
            created_at_unix_seconds: 100,
            last_attach_at_unix_seconds: None,
            window_count: 1,
            client_count: 0,
            primary_available: true,
            authoritative_columns: 80,
            authoritative_rows: 24,
        })
        .unwrap();

    let request = super::attach::attach_request_from_args(
        &SocketSelection::Default(default_socket),
        &["$target".to_string()],
        env.runtime.uid,
    )
    .unwrap();

    assert_eq!(request.requested_role, "primary");
    assert_eq!(
        selected_socket_path(&request.socket_selection),
        &target_socket
    );

    let _ = fs::remove_dir_all(directory.path);
    let _ = fs::remove_dir_all(home);
}

/// Verifies that `mez attach` accepts creation-order session aliases in both
/// displayed `$N` form and bare numeric form. This keeps the CLI target syntax
/// short while still deriving the target from the same registry order shown by
/// `mez list`.
#[test]
fn attach_session_alias_uses_creation_order_registry_socket() {
    let (env, home) = test_env("attach-session-alias");
    let directory = default_socket_directory(&env.runtime).unwrap();
    let registry = SessionRegistry::new(directory.path.clone(), env.runtime.uid);
    crate::runtime::ensure_private_socket_directory(&directory.path, env.runtime.uid).unwrap();
    let default_socket = directory.path.join(crate::runtime::DEFAULT_SOCKET_NAME);
    let oldest_socket = directory.path.join("oldest.sock");
    let newest_socket = directory.path.join("newest.sock");
    fs::write(&oldest_socket, "").unwrap();
    fs::write(&newest_socket, "").unwrap();
    registry
        .upsert(SessionRecord {
            session_id: "$newest".to_string(),
            name: "newest".to_string(),
            state: RegistrySessionState::Detached,
            socket_path: newest_socket.clone(),
            created_at_unix_seconds: 200,
            last_attach_at_unix_seconds: None,
            window_count: 1,
            client_count: 0,
            primary_available: true,
            authoritative_columns: 80,
            authoritative_rows: 24,
        })
        .unwrap();
    registry
        .upsert(SessionRecord {
            session_id: "$oldest".to_string(),
            name: "oldest".to_string(),
            state: RegistrySessionState::Detached,
            socket_path: oldest_socket.clone(),
            created_at_unix_seconds: 100,
            last_attach_at_unix_seconds: None,
            window_count: 1,
            client_count: 0,
            primary_available: true,
            authoritative_columns: 80,
            authoritative_rows: 24,
        })
        .unwrap();

    let first = super::attach::attach_request_from_args(
        &SocketSelection::Default(default_socket.clone()),
        &["$1".to_string()],
        env.runtime.uid,
    )
    .unwrap();
    let second = super::attach::attach_request_from_args(
        &SocketSelection::Default(default_socket),
        &["2".to_string()],
        env.runtime.uid,
    )
    .unwrap();

    assert_eq!(
        selected_socket_path(&first.socket_selection),
        &oldest_socket
    );
    assert_eq!(
        selected_socket_path(&second.socket_selection),
        &newest_socket
    );

    let _ = fs::remove_dir_all(directory.path);
    let _ = fs::remove_dir_all(home);
}

/// Verifies observer attachment uses the same index alias resolution as primary
/// attachment while preserving the requested observer role.
#[test]
fn attach_observer_accepts_session_index_alias() {
    let (env, home) = test_env("attach-observer-alias");
    let directory = default_socket_directory(&env.runtime).unwrap();
    let registry = SessionRegistry::new(directory.path.clone(), env.runtime.uid);
    crate::runtime::ensure_private_socket_directory(&directory.path, env.runtime.uid).unwrap();
    let default_socket = directory.path.join(crate::runtime::DEFAULT_SOCKET_NAME);
    let target_socket = directory.path.join("target.sock");
    fs::write(&target_socket, "").unwrap();
    registry
        .upsert(SessionRecord {
            session_id: "$target".to_string(),
            name: "target".to_string(),
            state: RegistrySessionState::Running,
            socket_path: target_socket.clone(),
            created_at_unix_seconds: 100,
            last_attach_at_unix_seconds: Some(120),
            window_count: 1,
            client_count: 1,
            primary_available: false,
            authoritative_columns: 80,
            authoritative_rows: 24,
        })
        .unwrap();

    let request = super::attach::attach_request_from_args(
        &SocketSelection::Default(default_socket),
        &["--observe".to_string(), "1".to_string()],
        env.runtime.uid,
    )
    .unwrap();

    assert_eq!(request.requested_role, "observer");
    assert_eq!(
        selected_socket_path(&request.socket_selection),
        &target_socket
    );

    let _ = fs::remove_dir_all(directory.path);
    let _ = fs::remove_dir_all(home);
}

/// Verifies CLI startup removes unserved sockets from the default runtime
/// directory.
///
/// This regression scenario confirms stale endpoint cleanup runs before normal
/// command dispatch without touching explicit socket paths or requiring a
/// registry record.
#[test]
fn startup_removes_unserved_default_runtime_socket_files() {
    let (env, home) = test_env("startup-stale-socket-cleanup");
    let directory = default_socket_directory(&env.runtime).unwrap();
    crate::runtime::ensure_private_socket_directory(&directory.path, env.runtime.uid).unwrap();
    let stale_socket = directory.path.join("orphan.sock");
    let stale_listener = std::os::unix::net::UnixListener::bind(&stale_socket).unwrap();
    drop(stale_listener);

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_with(
        vec!["mez".to_string(), "list".to_string()],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    assert!(!stale_socket.exists());
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(directory.path);
    let _ = fs::remove_dir_all(home);
}

/// Verifies auth status and logout use dedicated auth store.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn auth_status_and_logout_use_dedicated_auth_store() {
    let (env, home) = test_env("auth-status");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec!["mez".to_string(), "auth".to_string(), "status".to_string()],
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(stdout).unwrap(),
        r#"{"authenticated":false,"metadata":null}"#.to_string() + "\n"
    );

    let mut logout_stdout = Vec::new();
    run_with(
        vec!["mez".to_string(), "auth".to_string(), "logout".to_string()],
        env,
        false,
        &mut logout_stdout,
        &mut stderr,
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(logout_stdout).unwrap(),
        "{\"logged_out\":false}\n"
    );

    let _ = fs::remove_dir_all(home);
}

/// Verifies auth status JSON omits privacy-sensitive provider metadata.
///
/// The default status contract is safe to share for debugging: it reports the
/// coarse credential state without exposing account identifiers or raw
/// credential-store locators from the local auth metadata file.
#[test]
fn auth_status_json_omits_account_and_store_metadata() {
    let (env, home) = test_env("auth-status-private-metadata");
    let paths = env.config_paths().unwrap();
    let auth_store = AuthStore::new(AuthPaths::under_config_root(paths.root()));
    let credential_store = auth_store.file_credential_store("openai").unwrap();
    auth_store
        .login_openai_provider_credential(
            "default",
            OpenAiProviderCredential {
                api_key: "access-secret".to_string(),
                refresh_token: Some("refresh-secret".to_string()),
                account_id: Some("acct_123".to_string()),
                organization_id: Some("org_123".to_string()),
                token_expires_at: Some("9999999999".to_string()),
            },
            &credential_store,
        )
        .unwrap();

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_with(
        vec!["mez".to_string(), "auth".to_string(), "status".to_string()],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains(r#""authenticated":true"#), "{output}");
    assert!(output.contains(r#""provider":"openai""#), "{output}");
    assert!(
        output.contains(r#""credential_kind":"chatgpt""#),
        "{output}"
    );
    assert!(!output.contains("account_id"), "{output}");
    assert!(!output.contains("acct_123"), "{output}");
    assert!(!output.contains("organization_id"), "{output}");
    assert!(!output.contains("org_123"), "{output}");
    assert!(!output.contains("credential_store_ref"), "{output}");
    assert!(!output.contains("access-secret"), "{output}");
    assert!(stderr.is_empty(), "{}", String::from_utf8_lossy(&stderr));

    let _ = fs::remove_dir_all(home);
}

/// Verifies auth login noninteractive default requires browser interaction.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
/// Verifies that default auth login is browser-first and fails actionably when
/// a browser flow cannot be run from a noninteractive terminal.
fn auth_login_noninteractive_default_requires_browser_interaction() {
    let (env, home) = test_env("auth-login-default-noninteractive");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let error = run_with_plain(
        vec!["mez".to_string(), "auth".to_string(), "login".to_string()],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(error.message().contains("defaults to browser-based"));
    assert!(error.message().contains("--device-code"));
    assert!(error.message().contains("--api-key --api-key-file PATH"));
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies auth login method selection is browser first.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
/// Verifies that auth method selection is browser-first while retaining explicit
/// device-code and API-key options.
fn auth_login_method_selection_is_browser_first() {
    assert_eq!(
        super::auth::auth_login_method(&["login".to_string()]).unwrap(),
        crate::auth::AuthMethod::Browser
    );
    assert_eq!(
        super::auth::auth_login_method(&["login".to_string(), "--browser".to_string()]).unwrap(),
        crate::auth::AuthMethod::Browser
    );
    assert_eq!(
        super::auth::auth_login_method(&["login".to_string(), "--device-code".to_string()])
            .unwrap(),
        crate::auth::AuthMethod::DeviceCode
    );
    assert_eq!(
        super::auth::auth_login_method(&["login".to_string(), "--device-auth".to_string()])
            .unwrap(),
        crate::auth::AuthMethod::DeviceCode
    );
    assert_eq!(
        super::auth::auth_login_method(&["login".to_string(), "--api-key".to_string()]).unwrap(),
        crate::auth::AuthMethod::ApiKey
    );
}

/// Verifies non-OpenAI browser and device-code auth guidance points to API-key setup.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
/// Verifies that non-OpenAI browser and device-code auth requests fail with
/// provider-specific API-key guidance for Anthropic.
fn auth_login_rejects_non_openai_browser_and_device_code_with_api_key_guidance() {
    let (env, home) = test_env("auth-login-non-openai-guidance");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let browser_error = run_with_plain(
        vec![
            "mez".to_string(),
            "auth".to_string(),
            "login".to_string(),
            "--provider".to_string(),
            "anthropic".to_string(),
            "--browser".to_string(),
        ],
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap_err();

    assert_eq!(
        browser_error.kind(),
        crate::error::MezErrorKind::InvalidArgs
    );
    assert!(
        browser_error
            .message()
            .contains("browser-based login is only supported for OpenAI")
    );
    assert!(
        browser_error
            .message()
            .contains("--provider anthropic --api-key")
    );
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let device_error = run_with_plain(
        vec![
            "mez".to_string(),
            "auth".to_string(),
            "login".to_string(),
            "--provider".to_string(),
            "anthropic".to_string(),
            "--device-code".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap_err();

    assert_eq!(device_error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(
        device_error
            .message()
            .contains("device-code login is only supported for OpenAI")
    );
    assert!(
        device_error
            .message()
            .contains("--provider anthropic --api-key")
    );
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies noninteractive Anthropic API-key login requires an out-of-band secret source.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn auth_login_noninteractive_anthropic_api_key_requires_api_key_file() {
    let (env, home) = test_env("auth-login-anthropic-api-key-noninteractive");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let error = run_with_plain(
        vec![
            "mez".to_string(),
            "auth".to_string(),
            "login".to_string(),
            "--provider".to_string(),
            "anthropic".to_string(),
            "--api-key".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(
        error
            .message()
            .contains("requires noninteractive API-key input")
    );
    assert!(error.message().contains("--api-key-file PATH"));
    assert!(error.message().contains("--provider anthropic --api-key"));
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies auth login rejects conflicting method flags.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
/// Verifies that mutually exclusive auth method flags are rejected before
/// prompting or writing credential metadata.
fn auth_login_rejects_conflicting_method_flags() {
    let (env, home) = test_env("auth-login-conflicting-methods");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let error = run_with_plain(
        vec![
            "mez".to_string(),
            "auth".to_string(),
            "login".to_string(),
            "--api-key".to_string(),
            "--browser".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(error.message().contains("only one authentication method"));
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies auth login api key file persists metadata without printing secret.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn auth_login_api_key_file_persists_metadata_without_printing_secret() {
    let (env, home) = test_env("auth-login-api-key");
    let secret_path = home.join("openai-key.txt");
    fs::create_dir_all(&home).unwrap();
    fs::write(&secret_path, "sk-test-secret\n").unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec![
            "mez".to_string(),
            "auth".to_string(),
            "login".to_string(),
            "--api-key".to_string(),
            "--api-key-file".to_string(),
            secret_path.to_string_lossy().to_string(),
            "--credential-store".to_string(),
            "file".to_string(),
            "--profile".to_string(),
            "default".to_string(),
        ],
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains(r#""authenticated":true"#));
    assert!(output.contains(r#""credential_store":"file""#));
    assert!(!output.contains("sk-test-secret"));

    let mut status_stdout = Vec::new();
    run_with(
        vec!["mez".to_string(), "auth".to_string(), "status".to_string()],
        env,
        false,
        &mut status_stdout,
        &mut stderr,
    )
    .unwrap();
    let status = String::from_utf8(status_stdout).unwrap();
    assert!(status.contains(r#""authenticated":true"#));
    assert!(!status.contains("sk-test-secret"));

    let _ = fs::remove_dir_all(home);
}

/// Verifies snapshot list reads local snapshot repository.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_list_reads_local_snapshot_repository() {
    let (env, home) = test_env("snapshot-list");
    let repository =
        SnapshotRepository::new(home.join(".config").join("mezzanine").join("snapshots"));
    repository
        .write(&SnapshotManifest {
            state: SnapshotState {
                id: "snap1".to_string(),
                version: 1,
                session_id: "$1".to_string(),
                name: Some("manual".to_string()),
                created_at: "2026-04-30T00:00:00Z".to_string(),
                kind: SnapshotKind::Manual,
                restorable: true,
                window_count: 1,
                pane_count: 1,
                limitations: vec!["pane primary processes must be restarted".to_string()],
                storage_ref: "snap1.payload".to_string(),
            },
            contains_terminal_history: false,
            contains_agent_transcripts: false,
            contains_raw_credentials: false,
            active_approvals_restored: false,
            restart_required_panes: Vec::new(),
        })
        .unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec![
            "mez".to_string(),
            "snapshot".to_string(),
            "list".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains(r#""snapshot_id":"snap1""#));
    assert!(output.contains(r#""kind":"manual""#));
    assert!(output.contains(r#""limitations":["pane primary processes must be restarted"]"#));
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies snapshot resume restores local session shape.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_resume_restores_local_session_shape() {
    let (env, home) = test_env("snapshot-resume");
    let repository =
        SnapshotRepository::new(home.join(".config").join("mezzanine").join("snapshots"));
    let mut session = Session::new_default(
        resolve_shell(Some(OsString::from("/bin/sh"))).unwrap(),
        Size::new(80, 24).unwrap(),
    );
    let primary = session.attach_primary("primary", true).unwrap();
    session
        .split_active_pane(&primary, crate::layout::SplitDirection::Vertical)
        .unwrap();
    repository
        .create_from_session("snap-resume", Some("manual".to_string()), &session)
        .unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec![
            "mez".to_string(),
            "snapshot".to_string(),
            "resume".to_string(),
            "snap-resume".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains(r#""restored":true"#));
    assert!(output.contains(r#""live":false"#));
    assert!(output.contains(r#""pane_count":2"#));
    assert!(output.contains(r#""restart_required_panes":[]"#));
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies snapshot resume can restart restored panes with explicit command.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_resume_can_restart_restored_panes_with_explicit_command() {
    let (env, home) = test_env("snapshot-resume-restart");
    let repository =
        SnapshotRepository::new(home.join(".config").join("mezzanine").join("snapshots"));
    let mut session = Session::new_default(
        resolve_shell(Some(OsString::from("/bin/sh"))).unwrap(),
        Size::new(80, 24).unwrap(),
    );
    let primary = session.attach_primary("primary", true).unwrap();
    session
        .split_active_pane(&primary, crate::layout::SplitDirection::Vertical)
        .unwrap();
    repository
        .create_from_session("snap-restart", Some("manual".to_string()), &session)
        .unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec![
            "mez".to_string(),
            "snapshot".to_string(),
            "resume".to_string(),
            "snap-restart".to_string(),
            "--restart-command".to_string(),
            "true".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains(r#""restored":true"#));
    assert!(output.contains(r#""live":false"#));
    assert!(output.contains(r#""restarted":true"#));
    assert!(output.contains(r#""restarted_panes":["#));
    assert!(output.contains(r#""primary_pid":"#));
    assert!(output.contains(r#""pane_count":2"#));
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies that `snapshot resume --serve` starts a live control daemon from a
/// restored snapshot and assigns that live daemon a fresh discovery identity.
/// Snapshot payloads retain the original session id, but concurrent restored
/// daemons must not overwrite each other in the live session registry.
#[test]
fn snapshot_resume_can_serve_restored_session_over_control_socket() {
    let (env, home) = test_env("snapshot-resume-serve");
    let repository =
        SnapshotRepository::new(home.join(".config").join("mezzanine").join("snapshots"));
    let session = Session::new_default(
        resolve_shell(Some(OsString::from("/bin/sh"))).unwrap(),
        Size::new(80, 24).unwrap(),
    );
    let pane_id = session.windows()[0].panes()[0].id.to_string();
    repository
        .create_from_session_with_captures(
            "snap-serve",
            Some("manual".to_string()),
            &session,
            &[SnapshotPaneCapture {
                pane_id,
                primary_pid: None,
                process_state: Some("exited".to_string()),
                current_working_directory: None,
                readiness_state: Some("unknown".to_string()),
                terminal_history: vec!["snapshot-history".to_string()],
                terminal_history_line_style_spans: vec![Vec::new()],
                visible_lines: vec!["snapshot-visible".to_string()],
                visible_line_style_spans: vec![Vec::new()],
                terminal_modes: crate::terminal::TerminalModeState::default(),
                terminal_saved_state: crate::terminal::TerminalSavedState::default(),
                exit_status: None,
                alternate_screen_active: false,
                transcript_refs: Vec::new(),
            }],
        )
        .unwrap();

    let socket = home.join("runtime").join("snapshot-serve.sock");
    let socket_for_server = socket.clone();
    let server = thread::spawn(move || {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = run_with(
            vec![
                "mez".to_string(),
                "-S".to_string(),
                socket_for_server.to_string_lossy().to_string(),
                "snapshot".to_string(),
                "resume".to_string(),
                "snap-serve".to_string(),
                "--serve".to_string(),
                "--no-aux-sockets".to_string(),
                "--max-control-connections".to_string(),
                "1".to_string(),
            ],
            env,
            false,
            &mut stdout,
            &mut stderr,
        );
        (
            result,
            String::from_utf8(stdout).unwrap(),
            String::from_utf8(stderr).unwrap(),
        )
    });

    assert!(
        wait_for_path(&socket),
        "snapshot resume --serve did not bind socket"
    );
    let mut stream =
        connect_when_ready(&socket).expect("snapshot resume socket did not accept connections");
    let initialize = r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"mez-test","requested_version":1,"requested_role":"primary","client":{"name":"mez-test","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}}}}"#;
    let view = r#"{"jsonrpc":"2.0","id":"view","method":"terminal/view","params":{"client_size":{"columns":80,"rows":24}}}"#;
    stream.write_all(&encode_control_body(initialize)).unwrap();
    stream.write_all(&encode_control_body(view)).unwrap();
    stream.flush().unwrap();

    let response = read_control_response_frames(&mut stream, 1024 * 1024, 2).unwrap();
    let (initialize_response, consumed) = decode_control_frame(&response, 1024 * 1024).unwrap();
    let (view_response, _) = decode_control_frame(&response[consumed..], 1024 * 1024).unwrap();
    assert!(initialize_response.contains(r#""granted_role":"primary""#));
    assert!(
        view_response.contains("pane restarted with a fresh primary PID"),
        "{view_response}"
    );
    drop(stream);

    let (result, stdout, stderr) = server.join().unwrap();
    result.unwrap();
    assert!(stdout.contains(r#""serving":true"#));
    assert!(stdout.contains(r#""restored":true"#));
    assert!(stdout.contains("snapshot-serve.sock"));
    let live_session_id = stdout
        .split(r#""session_id":""#)
        .nth(1)
        .and_then(|tail| tail.split('"').next())
        .unwrap();
    assert_ne!(live_session_id, "$1");
    assert!(stderr.is_empty());
    assert!(!socket.exists());

    let _ = fs::remove_dir_all(home);
}

/// Verifies that control-socket attach clients consume cursor metadata emitted
/// by terminal step/view responses. These clients render line batches locally,
/// so the response parser must carry cursor placement into attached-terminal
/// output modes rather than hiding the cursor by default.
#[test]
fn terminal_step_response_output_modes_parse_cursor_metadata() {
    let modes = terminal_step_response_output_modes(
        r#"{"jsonrpc":"2.0","id":1,"result":{"view":{"cursor":{"row":2,"column":7,"visible":true,"style":"bar","blink":true,"blink_interval_ms":250},"output_modes":{"application_keypad":true,"bracketed_paste":true,"host_mouse_reporting":false,"animation_refresh_interval_ms":180},"lines":["pane"]}}}"#,
    )
    .unwrap()
    .unwrap();

    assert_eq!(modes.cursor_row, 2);
    assert_eq!(modes.cursor_column, 7);
    assert!(modes.cursor_visible);
    assert_eq!(
        modes.cursor_style,
        crate::terminal::TerminalCursorStyle::Bar
    );
    assert!(modes.cursor_blink);
    assert_eq!(modes.cursor_blink_interval_ms, 250);
    assert!(modes.application_keypad);
    assert!(modes.bracketed_paste);
    assert!(!modes.host_mouse_reporting);
    assert_eq!(modes.animation_refresh_interval_ms, 180);
}

/// Verifies that control-socket attach clients parse SGR style spans from the
/// runtime presentation payload. Without this, detachable attach renders the
/// same terminal text but silently drops color and text attributes.
#[test]
fn terminal_step_response_line_style_spans_parse_color_and_attributes() {
    let spans = terminal_step_response_line_style_spans(
        r#"{"jsonrpc":"2.0","id":1,"result":{"view":{"lines":["styled"],"line_style_spans":[[{"start":1,"length":3,"rendition":{"bold":true,"dim":false,"italic":true,"underline":true,"double_underline":false,"strikethrough":true,"inverse":false,"hidden":false,"foreground":{"kind":"rgb","red":1,"green":2,"blue":3},"background":{"kind":"indexed","index":4}}}]]}}}"#,
    )
    .unwrap();

    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0][0].start, 1);
    assert_eq!(spans[0][0].length, 3);
    assert!(spans[0][0].rendition.bold);
    assert!(spans[0][0].rendition.italic);
    assert!(spans[0][0].rendition.underline);
    assert!(spans[0][0].rendition.strikethrough);
    assert_eq!(
        spans[0][0].rendition.foreground,
        Some(crate::terminal::TerminalColor::Rgb(1, 2, 3))
    );
    assert_eq!(
        spans[0][0].rendition.background,
        Some(crate::terminal::TerminalColor::Indexed(4))
    );
}

/// Verifies that the detachable control-socket attach request preserves SGR
/// mouse packets as raw byte arrays for runtime-side hit testing, application
/// forwarding, legacy mouse translation, and pane resize handling.
#[test]
fn terminal_step_control_request_preserves_sgr_mouse_bytes() {
    let client_id = crate::ids::ClientId::parse('c', "c1".to_string()).unwrap();
    let mouse = b"\x1b[<0;12;5M";
    let request =
        terminal_step_control_request(3, &client_id, Size::new(80, 24).unwrap(), mouse, true);
    let parsed: serde_json::Value = serde_json::from_str(&request).unwrap();
    let bytes = parsed
        .get("params")
        .and_then(|params| params.get("input_bytes"))
        .and_then(serde_json::Value::as_array)
        .unwrap()
        .iter()
        .map(|value| value.as_u64().unwrap() as u8)
        .collect::<Vec<_>>();

    assert_eq!(bytes, mouse);
}

/// Verifies that detachable control-socket primary attachment can run its
/// foreground terminal loop through the Tokio terminal IO boundary. This keeps
/// the legacy control protocol surface available while ensuring terminal
/// readiness, presentation entry, frame output, and clean hangup handling no
/// longer depend on the synchronous fd polling trait.
#[tokio::test(flavor = "current_thread")]
async fn control_socket_primary_attach_loop_uses_async_terminal_io() {
    let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
    client_stream.set_nonblocking(true).unwrap();
    let mut client_stream = tokio::net::UnixStream::from_std(client_stream).unwrap();
    let server = thread::spawn(move || {
        let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("method").and_then(serde_json::Value::as_str),
            Some("terminal/step")
        );
        assert_eq!(
            parsed
                .get("params")
                .and_then(|params| params.get("render"))
                .and_then(serde_json::Value::as_bool),
            Some(false)
        );
        assert_eq!(
            parsed
                .get("params")
                .and_then(|params| params.get("input_bytes"))
                .and_then(serde_json::Value::as_array)
                .and_then(|bytes| bytes.first())
                .and_then(serde_json::Value::as_u64),
            Some(u64::from(b'x'))
        );
        server_stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-terminal-step-0","result":{"input_bytes":1,"application":{"forwarded_bytes":0,"mux_actions_applied":1,"mouse_actions_reported":0,"agent_prompt_inputs_applied":0,"view_refresh_required":true,"full_redraw_required":true,"unsupported_actions":[]},"view":null,"ui_theme":null}}"#,
            ))
            .unwrap();
        server_stream.flush().unwrap();
        let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("method").and_then(serde_json::Value::as_str),
            Some("terminal/view")
        );
        server_stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-terminal-view-0","result":{"view":{"lines":["detached async"],"line_style_spans":[[]],"cursor":{"row":0,"column":14,"visible":true,"style":"bar","blink":false},"output_modes":{"application_keypad":false}}}}"#,
            ))
            .unwrap();
        server_stream.flush().unwrap();
    });

    let mut io = AsyncFakeAttachedTerminalIo::default();
    io.push_input(b"x".to_vec());

    let primary_client_id = crate::ids::ClientId::parse('c', "c1".to_string()).unwrap();
    run_control_socket_attached_primary_client_loop_async(
        &mut client_stream,
        &mut io,
        primary_client_id,
        Size::new(80, 24).unwrap(),
    )
    .await
    .unwrap();
    server.join().unwrap();

    assert_eq!(io.presentation_entries, 1);
    assert_eq!(io.invalidated_output_frames, 1);
    assert_eq!(io.written_frames.len(), 1);
    assert_eq!(io.written_frames[0].lines, vec!["detached async"]);
    assert_eq!(io.written_frames[0].modes.cursor_column, 14);
    assert!(io.written_frames[0].modes.cursor_visible);
}

/// Verifies terminal-step response parsing keeps the full-redraw signal
/// separate from the basic view-refresh signal.
///
/// Full redraws must invalidate the attached client's retained output frame
/// before rendering. This regression protects the control-socket attach path
/// from collapsing the two runtime signals into a single boolean and then
/// redrawing against stale frame state.
#[test]
fn terminal_step_response_refresh_requirement_preserves_full_redraw() {
    let refresh = terminal_step_response_refresh_requirement(
        r#"{"jsonrpc":"2.0","id":"cli-terminal-step-0","result":{"application":{"view_refresh_required":false,"full_redraw_required":true}}}"#,
    )
    .unwrap();

    assert!(refresh.view_refresh_required);
    assert!(refresh.full_redraw_required);
}

/// Verifies a light terminal-step refresh requests a new view without
/// invalidating the retained output frame.
///
/// Focus changes need a fresh attached view for cursor and active-frame state,
/// but they should still use the differential renderer. This protects remote
/// terminal sessions from unnecessary full-screen clears during pane navigation.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn control_socket_primary_attach_loop_refreshes_without_invalidating_for_light_step() {
    let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
    client_stream.set_nonblocking(true).unwrap();
    let mut client_stream = tokio::net::UnixStream::from_std(client_stream).unwrap();
    let server = thread::spawn(move || {
        let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("method").and_then(serde_json::Value::as_str),
            Some("terminal/view")
        );
        assert_eq!(
            parsed.get("id").and_then(serde_json::Value::as_str),
            Some("cli-terminal-view-0")
        );
        server_stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-terminal-view-0","result":{"view":{"lines":["initial"],"line_style_spans":[[]],"cursor":{"row":0,"column":7,"visible":true,"style":"bar","blink":false},"output_modes":{"application_keypad":false}}}}"#,
            ))
            .unwrap();
        server_stream.flush().unwrap();

        let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("method").and_then(serde_json::Value::as_str),
            Some("terminal/step")
        );
        assert_eq!(
            parsed.get("id").and_then(serde_json::Value::as_str),
            Some("cli-terminal-step-1")
        );
        server_stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-terminal-step-1","result":{"input_bytes":1,"application":{"forwarded_bytes":0,"mux_actions_applied":1,"mouse_actions_reported":0,"agent_prompt_inputs_applied":0,"view_refresh_required":true,"full_redraw_required":false,"unsupported_actions":[]},"view":null,"ui_theme":null}}"#,
            ))
            .unwrap();
        server_stream.flush().unwrap();

        let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("method").and_then(serde_json::Value::as_str),
            Some("terminal/view")
        );
        assert_eq!(
            parsed.get("id").and_then(serde_json::Value::as_str),
            Some("cli-terminal-view-1")
        );
        server_stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-terminal-view-1","result":{"view":{"lines":["focused"],"line_style_spans":[[]],"cursor":{"row":0,"column":7,"visible":true,"style":"bar","blink":false},"output_modes":{"application_keypad":false}}}}"#,
            ))
            .unwrap();
        server_stream.flush().unwrap();
    });

    let mut io = AsyncFakeAttachedTerminalIo::default();
    io.push_pending_input_read();
    io.push_input(b"x".to_vec());
    let primary_client_id = crate::ids::ClientId::parse('c', "c1".to_string()).unwrap();
    {
        let run = run_control_socket_attached_primary_client_loop_async(
            &mut client_stream,
            &mut io,
            primary_client_id,
            Size::new(80, 24).unwrap(),
        );
        tokio::pin!(run);
        tokio::time::advance(Duration::from_millis(100)).await;
        run.await.unwrap();
    }
    server.join().unwrap();
    assert_eq!(io.presentation_entries, 1);
    assert_eq!(io.invalidated_output_frames, 0);
    assert_eq!(io.written_frames.len(), 2);
    assert_eq!(io.written_frames[0].lines, vec!["initial"]);
    assert_eq!(io.written_frames[1].lines, vec!["focused"]);
}

/// Verifies that once the initial attach redraw has already been satisfied,
/// a later primary-input step can stay input-only when the runtime reports no
/// explicit refresh requirement for that input.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn control_socket_primary_attach_loop_skips_view_after_input_without_refresh_request() {
    let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
    client_stream.set_nonblocking(true).unwrap();
    server_stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    let mut client_stream = tokio::net::UnixStream::from_std(client_stream).unwrap();
    let server = thread::spawn(move || {
        let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("method").and_then(serde_json::Value::as_str),
            Some("terminal/view")
        );
        server_stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-terminal-view-0","result":{"view":{"lines":["initial"],"line_style_spans":[[]],"cursor":{"row":0,"column":7,"visible":true,"style":"bar","blink":false},"output_modes":{"application_keypad":false}}}}"#,
            ))
            .unwrap();
        server_stream.flush().unwrap();
        let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("method").and_then(serde_json::Value::as_str),
            Some("terminal/step")
        );
        assert_eq!(
            parsed
                .get("params")
                .and_then(|params| params.get("render"))
                .and_then(serde_json::Value::as_bool),
            Some(false)
        );
        server_stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-terminal-step-0","result":{"input_bytes":1,"application":{"forwarded_bytes":1,"mux_actions_applied":0,"mouse_actions_reported":0,"agent_prompt_inputs_applied":0,"view_refresh_required":false,"full_redraw_required":false,"unsupported_actions":[]},"view":null,"ui_theme":null}}"#,
            ))
            .unwrap();
        server_stream.flush().unwrap();
        let mut unexpected = [0u8; 256];
        match server_stream.read(&mut unexpected) {
            Ok(0) => {}
            Ok(read) => panic!(
                "unexpected follow-up view request: {}",
                String::from_utf8_lossy(&unexpected[..read])
            ),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => panic!("unexpected server read error: {error}"),
        }
    });
    let mut io = AsyncFakeAttachedTerminalIo::default();
    io.push_pending_input_read();
    io.push_input(b"x".to_vec());
    let primary_client_id = crate::ids::ClientId::parse('c', "c1".to_string()).unwrap();
    {
        let run = run_control_socket_attached_primary_client_loop_async(
            &mut client_stream,
            &mut io,
            primary_client_id,
            Size::new(80, 24).unwrap(),
        );
        tokio::pin!(run);
        tokio::time::advance(Duration::from_millis(100)).await;
        run.await.unwrap();
    }
    server.join().unwrap();
    assert_eq!(io.presentation_entries, 1);
    assert_eq!(io.written_frames.len(), 1);
    assert_eq!(io.written_frames[0].lines, vec!["initial"]);
}

/// Verifies that an idle control-socket primary attach renders once for initial
/// presentation but does not keep sending render requests on repeated terminal
/// input timeouts. This protects the agent-inactive idle path from recreating
/// the previous fixed-cadence `terminal/step render=true` loop.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn control_socket_primary_attach_loop_does_not_repeat_idle_renders() {
    let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
    client_stream.set_nonblocking(true).unwrap();
    server_stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    let mut client_stream = tokio::net::UnixStream::from_std(client_stream).unwrap();
    let server = thread::spawn(move || {
        let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("method").and_then(serde_json::Value::as_str),
            Some("terminal/view")
        );
        server_stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-terminal-view-0","result":{"view":{"lines":["initial"],"line_style_spans":[[]],"cursor":{"row":0,"column":7,"visible":true,"style":"bar","blink":false},"output_modes":{"application_keypad":false}}}}"#,
            ))
            .unwrap();
        server_stream.flush().unwrap();
        let mut unexpected = [0u8; 256];
        match server_stream.read(&mut unexpected) {
            Ok(0) => {}
            Ok(read) => panic!(
                "unexpected repeated idle render request: {}",
                String::from_utf8_lossy(&unexpected[..read])
            ),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => panic!("unexpected server read error: {error}"),
        }
    });
    let mut io = AsyncFakeAttachedTerminalIo::default();
    io.push_pending_input_read();
    io.push_pending_input_read();
    let primary_client_id = crate::ids::ClientId::parse('c', "c1".to_string()).unwrap();
    {
        let run = run_control_socket_attached_primary_client_loop_async(
            &mut client_stream,
            &mut io,
            primary_client_id,
            Size::new(80, 24).unwrap(),
        );
        tokio::pin!(run);
        tokio::time::advance(Duration::from_millis(100)).await;
        tokio::time::advance(Duration::from_millis(100)).await;
        run.await.unwrap();
    }
    server.join().unwrap();
    assert_eq!(io.presentation_entries, 1);
    assert_eq!(io.written_frames.len(), 1);
    assert_eq!(io.written_frames[0].lines, vec!["initial"]);
}

/// Verifies that runtime events wake an otherwise idle primary attach loop for
/// a fresh terminal view request without restoring fixed-cadence idle renders.
///
/// Pane output and lifecycle changes arrive through the daemon event socket, so
/// this regression protects prompt redraws after the idle-loop optimization
/// suppresses repeated timeout-driven renders.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn control_socket_primary_attach_loop_runtime_event_requests_view() {
    let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
    client_stream.set_nonblocking(true).unwrap();
    let mut client_stream = tokio::net::UnixStream::from_std(client_stream).unwrap();
    let (event_client_stream, mut event_server_stream) = UnixStream::pair().unwrap();
    event_client_stream.set_nonblocking(true).unwrap();
    let event_client_stream = tokio::net::UnixStream::from_std(event_client_stream).unwrap();
    let server = thread::spawn(move || {
        for (expected_id, response_lines) in [
            ("cli-terminal-view-0", "initial"),
            ("cli-terminal-view-1", "event redraw"),
        ] {
            let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
            let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(
                parsed.get("method").and_then(serde_json::Value::as_str),
                Some("terminal/view")
            );
            assert_eq!(
                parsed.get("id").and_then(serde_json::Value::as_str),
                Some(expected_id)
            );
            if expected_id == "cli-terminal-view-0" {
                event_server_stream
                    .write_all(&event_notification_frame("pane_changed"))
                    .unwrap();
                event_server_stream.flush().unwrap();
            }
            server_stream
                .write_all(&encode_control_body(&format!(
                    r#"{{"jsonrpc":"2.0","id":"{expected_id}","result":{{"view":{{"lines":["{response_lines}"],"line_style_spans":[[]],"cursor":{{"row":0,"column":12,"visible":true,"style":"bar","blink":false}},"output_modes":{{"application_keypad":false}}}}}}}}"#
                )))
                .unwrap();
            server_stream.flush().unwrap();
        }
    });
    let mut io = AsyncFakeAttachedTerminalIo::default();
    io.push_pending_input_read();
    io.push_pending_input_read();
    let primary_client_id = crate::ids::ClientId::parse('c', "c1".to_string()).unwrap();
    {
        let run = run_control_socket_attached_primary_client_loop_async_with_runtime_events(
            &mut client_stream,
            &mut io,
            primary_client_id,
            Size::new(80, 24).unwrap(),
            Some(event_client_stream),
        );
        tokio::pin!(run);
        tokio::time::advance(Duration::from_millis(100)).await;
        run.await.unwrap();
    }
    server.join().unwrap();
    assert_eq!(io.presentation_entries, 1);
    assert_eq!(io.invalidated_output_frames, 0);
    assert_eq!(io.written_frames.len(), 2);
    assert_eq!(io.written_frames[0].lines, vec!["initial"]);
    assert_eq!(io.written_frames[1].lines, vec!["event redraw"]);
}

/// Verifies active animation metadata refreshes a socket-attached primary view
/// even when no runtime event arrives.
///
/// Agent status animation changes only presentation styling. It should not
/// require durable event-log traffic, but the attach loop still has to request
/// fresh views while the last rendered frame advertises an animation cadence.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn control_socket_primary_attach_loop_refreshes_active_animation_without_runtime_event() {
    let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
    client_stream.set_nonblocking(true).unwrap();
    server_stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut client_stream = tokio::net::UnixStream::from_std(client_stream).unwrap();
    let (event_client_stream, event_server_stream) = UnixStream::pair().unwrap();
    event_client_stream.set_nonblocking(true).unwrap();
    let event_client_stream = tokio::net::UnixStream::from_std(event_client_stream).unwrap();
    let server = thread::spawn(move || {
        let _event_server_stream = event_server_stream;
        for (expected_id, response_lines, animation_refresh_interval_ms) in [
            ("cli-terminal-view-0", "thinking phase one", 180),
            ("cli-terminal-view-1", "thinking phase two", 0),
        ] {
            let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
            let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(
                parsed.get("method").and_then(serde_json::Value::as_str),
                Some("terminal/view")
            );
            assert_eq!(
                parsed.get("id").and_then(serde_json::Value::as_str),
                Some(expected_id)
            );
            let response = serde_json::json!({
                "jsonrpc": "2.0",
                "id": expected_id,
                "result": {
                    "view": {
                        "lines": [response_lines],
                        "line_style_spans": [[]],
                        "cursor": {
                            "row": 0,
                            "column": 18,
                            "visible": true,
                            "style": "bar",
                            "blink": false,
                        },
                        "output_modes": {
                            "application_keypad": false,
                            "animation_refresh_interval_ms": animation_refresh_interval_ms,
                        },
                    },
                },
            })
            .to_string();
            server_stream
                .write_all(&encode_control_body(&response))
                .unwrap();
            server_stream.flush().unwrap();
        }
    });
    let mut io = AsyncFakeAttachedTerminalIo::default();
    io.push_pending_input_read();
    io.push_pending_input_read();
    io.push_pending_input_read();
    let primary_client_id = crate::ids::ClientId::parse('c', "c1".to_string()).unwrap();
    {
        let run = run_control_socket_attached_primary_client_loop_async_with_runtime_events(
            &mut client_stream,
            &mut io,
            primary_client_id,
            Size::new(80, 24).unwrap(),
            Some(event_client_stream),
        );
        tokio::pin!(run);
        tokio::time::advance(Duration::from_millis(100)).await;
        tokio::time::advance(Duration::from_millis(100)).await;
        run.await.unwrap();
    }
    server.join().unwrap();
    assert_eq!(io.presentation_entries, 1);
    assert_eq!(io.invalidated_output_frames, 0);
    assert_eq!(io.written_frames.len(), 2);
    assert_eq!(io.written_frames[0].lines, vec!["thinking phase one"]);
    assert_eq!(io.written_frames[1].lines, vec!["thinking phase two"]);
}
/// Verifies an idle primary control attach notices local terminal resizes and
/// requests a fresh view without waiting for user input or daemon events.
///
/// Terminal resizes are a local presentation concern, so the foreground attach
/// client should poll terminal size on its own and only invalidate/redraw when
/// the measured size actually changes.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn control_socket_primary_attach_loop_refreshes_idle_resize_without_input() {
    let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
    client_stream.set_nonblocking(true).unwrap();
    server_stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut client_stream = tokio::net::UnixStream::from_std(client_stream).unwrap();
    let (event_client_stream, event_server_stream) = UnixStream::pair().unwrap();
    event_client_stream.set_nonblocking(true).unwrap();
    let event_client_stream = tokio::net::UnixStream::from_std(event_client_stream).unwrap();
    let server = thread::spawn(move || {
        let _event_server_stream = event_server_stream;
        let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("method").and_then(serde_json::Value::as_str),
            Some("terminal/view")
        );
        assert_eq!(
            parsed.get("id").and_then(serde_json::Value::as_str),
            Some("cli-terminal-view-0")
        );
        assert_eq!(
            parsed
                .get("params")
                .and_then(|params| params.get("client_size"))
                .and_then(|size| size.get("columns"))
                .and_then(serde_json::Value::as_u64),
            Some(80)
        );
        assert_eq!(
            parsed
                .get("params")
                .and_then(|params| params.get("client_size"))
                .and_then(|size| size.get("rows"))
                .and_then(serde_json::Value::as_u64),
            Some(24)
        );
        server_stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-terminal-view-0","result":{"view":{"lines":["initial"],"line_style_spans":[[]],"cursor":{"row":0,"column":7,"visible":true,"style":"bar","blink":false},"output_modes":{"application_keypad":false}}}}"#,
            ))
            .unwrap();
        server_stream.flush().unwrap();

        let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("method").and_then(serde_json::Value::as_str),
            Some("terminal/step")
        );
        assert_eq!(
            parsed.get("id").and_then(serde_json::Value::as_str),
            Some("cli-terminal-resize-1")
        );
        assert_eq!(
            parsed
                .get("params")
                .and_then(|params| params.get("idempotency_key"))
                .and_then(serde_json::Value::as_str),
            Some("cli-c1-terminal-resize-1")
        );
        assert_eq!(
            parsed
                .get("params")
                .and_then(|params| params.get("render"))
                .and_then(serde_json::Value::as_bool),
            Some(false)
        );
        assert_eq!(
            parsed
                .get("params")
                .and_then(|params| params.get("input_bytes"))
                .and_then(serde_json::Value::as_array)
                .map(Vec::is_empty),
            Some(true)
        );
        assert_eq!(
            parsed
                .get("params")
                .and_then(|params| params.get("client_size"))
                .and_then(|size| size.get("columns"))
                .and_then(serde_json::Value::as_u64),
            Some(100)
        );
        assert_eq!(
            parsed
                .get("params")
                .and_then(|params| params.get("client_size"))
                .and_then(|size| size.get("rows"))
                .and_then(serde_json::Value::as_u64),
            Some(30)
        );
        server_stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-terminal-resize-1","result":{"input_bytes":0,"application":{"forwarded_bytes":0,"mux_actions_applied":0,"mouse_actions_reported":0,"agent_prompt_inputs_applied":0,"view_refresh_required":false,"full_redraw_required":false,"unsupported_actions":[]},"view":null,"ui_theme":null}}"#,
            ))
            .unwrap();
        server_stream.flush().unwrap();

        {
            let (expected_id, expected_columns, expected_rows, response_lines) =
                ("cli-terminal-view-1", 100, 30, "resized");
            let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
            let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(
                parsed.get("method").and_then(serde_json::Value::as_str),
                Some("terminal/view")
            );
            assert_eq!(
                parsed.get("id").and_then(serde_json::Value::as_str),
                Some(expected_id)
            );
            assert_eq!(
                parsed
                    .get("params")
                    .and_then(|params| params.get("client_size"))
                    .and_then(|size| size.get("columns"))
                    .and_then(serde_json::Value::as_u64),
                Some(expected_columns)
            );
            assert_eq!(
                parsed
                    .get("params")
                    .and_then(|params| params.get("client_size"))
                    .and_then(|size| size.get("rows"))
                    .and_then(serde_json::Value::as_u64),
                Some(expected_rows)
            );
            let response = serde_json::json!({
                "jsonrpc": "2.0",
                "id": expected_id,
                "result": {
                    "view": {
                        "lines": [response_lines],
                        "line_style_spans": [[]],
                        "cursor": {
                            "row": 0,
                            "column": 7,
                            "visible": true,
                            "style": "bar",
                            "blink": false,
                        },
                        "output_modes": {
                            "application_keypad": false,
                        },
                    },
                },
            })
            .to_string();
            server_stream
                .write_all(&encode_control_body(&response))
                .unwrap();
            server_stream.flush().unwrap();
        }
    });
    let mut io = AsyncFakeAttachedTerminalIo::default();
    io.push_terminal_size(Some(Size::new(80, 24).unwrap()));
    io.push_terminal_size(Some(Size::new(100, 30).unwrap()));
    io.push_pending_input_read();
    io.push_pending_input_read();
    let primary_client_id = crate::ids::ClientId::parse('c', "c1".to_string()).unwrap();
    {
        let run = run_control_socket_attached_primary_client_loop_async_with_runtime_events(
            &mut client_stream,
            &mut io,
            primary_client_id,
            Size::new(80, 24).unwrap(),
            Some(event_client_stream),
        );
        tokio::pin!(run);
        tokio::time::advance(Duration::from_millis(100)).await;
        tokio::time::advance(Duration::from_millis(100)).await;
        run.await.unwrap();
    }
    server.join().unwrap();
    assert_eq!(io.presentation_entries, 1);
    assert_eq!(io.invalidated_output_frames, 1);
    assert_eq!(io.written_frames.len(), 2);
    assert_eq!(io.written_frames[0].lines, vec!["initial"]);
    assert_eq!(io.written_frames[1].lines, vec!["resized"]);
}

/// Verifies that generic runtime events do not redraw the attached terminal.
///
/// Diagnostic notifications can be emitted as runtime bookkeeping, but they do
/// not alter the visible attached terminal frame. This protects the idle
/// efficiency refactor from turning event traffic into flicker.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn control_socket_primary_attach_loop_ignores_nonvisible_runtime_events() {
    let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
    client_stream.set_nonblocking(true).unwrap();
    let mut client_stream = tokio::net::UnixStream::from_std(client_stream).unwrap();
    let (event_client_stream, mut event_server_stream) = UnixStream::pair().unwrap();
    event_client_stream.set_nonblocking(true).unwrap();
    let event_client_stream = tokio::net::UnixStream::from_std(event_client_stream).unwrap();
    let server = thread::spawn(move || {
        let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed.get("method").and_then(serde_json::Value::as_str),
            Some("terminal/view")
        );
        assert_eq!(
            parsed.get("id").and_then(serde_json::Value::as_str),
            Some("cli-terminal-view-0")
        );
        event_server_stream
            .write_all(&event_notification_frame("diagnostic"))
            .unwrap();
        event_server_stream.flush().unwrap();
        server_stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-terminal-view-0","result":{"view":{"lines":["initial"],"line_style_spans":[[]],"cursor":{"row":0,"column":7,"visible":true,"style":"bar","blink":false},"output_modes":{"application_keypad":false}}}}"#,
            ))
            .unwrap();
        server_stream.flush().unwrap();
        drop(event_server_stream);
    });
    let mut io = AsyncFakeAttachedTerminalIo::default();
    io.push_pending_input_read();
    io.push_pending_input_read();
    let primary_client_id = crate::ids::ClientId::parse('c', "c1".to_string()).unwrap();
    {
        let run = run_control_socket_attached_primary_client_loop_async_with_runtime_events(
            &mut client_stream,
            &mut io,
            primary_client_id,
            Size::new(80, 24).unwrap(),
            Some(event_client_stream),
        );
        tokio::pin!(run);
        tokio::time::advance(Duration::from_millis(100)).await;
        run.await.unwrap();
    }
    server.join().unwrap();
    assert_eq!(io.presentation_entries, 1);
    assert_eq!(io.invalidated_output_frames, 0);
    assert_eq!(io.written_frames.len(), 1);
    assert_eq!(io.written_frames[0].lines, vec!["initial"]);
}

/// Verifies that structural runtime events redraw after invalidating the diff
/// base exactly once.
///
/// Layout-changing event notifications can make the previous output frame an
/// unsafe basis for incremental rendering, so the attach loop should invalidate
/// only for that stronger event class.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn control_socket_primary_attach_loop_structural_runtime_event_invalidates_once() {
    let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
    client_stream.set_nonblocking(true).unwrap();
    let mut client_stream = tokio::net::UnixStream::from_std(client_stream).unwrap();
    let (event_client_stream, mut event_server_stream) = UnixStream::pair().unwrap();
    event_client_stream.set_nonblocking(true).unwrap();
    let event_client_stream = tokio::net::UnixStream::from_std(event_client_stream).unwrap();
    let server = thread::spawn(move || {
        for (expected_id, response_lines) in [
            ("cli-terminal-view-0", "initial"),
            ("cli-terminal-view-1", "window changed"),
        ] {
            let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
            let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(
                parsed.get("method").and_then(serde_json::Value::as_str),
                Some("terminal/view")
            );
            assert_eq!(
                parsed.get("id").and_then(serde_json::Value::as_str),
                Some(expected_id)
            );
            server_stream
                .write_all(&encode_control_body(&format!(
                    r#"{{"jsonrpc":"2.0","id":"{expected_id}","result":{{"view":{{"lines":["{response_lines}"],"line_style_spans":[[]],"cursor":{{"row":0,"column":14,"visible":true,"style":"bar","blink":false}},"output_modes":{{"application_keypad":false}}}}}}}}"#
                )))
                .unwrap();
            server_stream.flush().unwrap();
            if expected_id == "cli-terminal-view-0" {
                event_server_stream
                    .write_all(&event_notification_frame("window_changed"))
                    .unwrap();
                event_server_stream.flush().unwrap();
            }
        }
    });
    let mut io = AsyncFakeAttachedTerminalIo::default();
    io.push_pending_input_read();
    io.push_pending_input_read();
    let primary_client_id = crate::ids::ClientId::parse('c', "c1".to_string()).unwrap();
    {
        let run = run_control_socket_attached_primary_client_loop_async_with_runtime_events(
            &mut client_stream,
            &mut io,
            primary_client_id,
            Size::new(80, 24).unwrap(),
            Some(event_client_stream),
        );
        tokio::pin!(run);
        tokio::time::advance(Duration::from_millis(100)).await;
        run.await.unwrap();
    }
    server.join().unwrap();
    assert_eq!(io.presentation_entries, 1);
    assert_eq!(io.invalidated_output_frames, 1);
    assert_eq!(io.written_frames.len(), 2);
    assert_eq!(io.written_frames[0].lines, vec!["initial"]);
    assert_eq!(io.written_frames[1].lines, vec!["window changed"]);
}

/// Verifies that event stream decoding buffers split frames across socket reads.
///
/// Runtime event notifications use the same framed protocol as control
/// responses, so the attach client must not assume that one socket read contains
/// one complete event.
#[tokio::test(flavor = "current_thread")]
async fn attached_runtime_event_stream_buffers_split_frames() {
    let (event_client_stream, mut event_server_stream) = UnixStream::pair().unwrap();
    event_client_stream.set_nonblocking(true).unwrap();
    let event_client_stream = tokio::net::UnixStream::from_std(event_client_stream).unwrap();
    let mut event_stream = AttachedRuntimeEventStream::new(event_client_stream);
    let frame = event_notification_frame("pane_changed");
    let split_at = frame.len() / 2;
    event_server_stream.write_all(&frame[..split_at]).unwrap();
    event_server_stream.flush().unwrap();
    assert_eq!(
        event_stream.read_render_action().await.unwrap(),
        AttachRenderAction::None
    );
    event_server_stream.write_all(&frame[split_at..]).unwrap();
    event_server_stream.flush().unwrap();
    assert_eq!(
        event_stream.read_render_action().await.unwrap(),
        AttachRenderAction::View
    );
}

/// Verifies that a burst of runtime events is coalesced into the strongest
/// single render action.
///
/// A pane update followed by a structural event should not produce multiple
/// immediate redraw requests; the attach loop only needs the strongest action
/// from the burst.
#[tokio::test(flavor = "current_thread")]
async fn attached_runtime_event_stream_coalesces_event_burst() {
    let (event_client_stream, mut event_server_stream) = UnixStream::pair().unwrap();
    event_client_stream.set_nonblocking(true).unwrap();
    let event_client_stream = tokio::net::UnixStream::from_std(event_client_stream).unwrap();
    let mut event_stream = AttachedRuntimeEventStream::new(event_client_stream);
    let mut burst = event_notification_frame("pane_changed");
    burst.extend_from_slice(&event_notification_frame("window_changed"));
    event_server_stream.write_all(&burst).unwrap();
    event_server_stream.flush().unwrap();
    assert_eq!(
        event_stream.read_render_action().await.unwrap(),
        AttachRenderAction::InvalidateAndView
    );
}

/// Verifies interactive control-socket attachment exits cleanly when the daemon
/// closes the socket before sending a response frame. The foreground terminal
/// loop should treat that as detach/disconnect rather than surfacing the strict
/// frame decoder's partial-header error.
#[tokio::test(flavor = "current_thread")]
async fn control_socket_primary_attach_loop_exits_on_incomplete_response_eof() {
    let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
    client_stream.set_nonblocking(true).unwrap();
    let mut client_stream = tokio::net::UnixStream::from_std(client_stream).unwrap();
    let server = thread::spawn(move || {
        let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
        let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
        assert!(body.contains(r#""terminal/step""#), "{body}");
        drop(server_stream);
    });

    let mut io = AsyncFakeAttachedTerminalIo::default();
    io.push_input(b"x".to_vec());

    let primary_client_id = crate::ids::ClientId::parse('c', "c1".to_string()).unwrap();
    run_control_socket_attached_primary_client_loop_async(
        &mut client_stream,
        &mut io,
        primary_client_id,
        Size::new(80, 24).unwrap(),
    )
    .await
    .unwrap();
    server.join().unwrap();

    assert_eq!(io.presentation_entries, 1);
    assert!(io.written_frames.is_empty());
}

/// Verifies the interactive observer attach loop polls observer-local status
/// before reading the terminal view.
///
/// Pending observers are not authorized for `terminal/view`; this regression
/// ensures the attach client waits on `observer/inspect` until the request is
/// approved, then switches to rendered live view requests.
#[tokio::test(flavor = "current_thread")]
async fn control_socket_observer_attach_loop_waits_for_approval_before_view() {
    let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
    client_stream.set_nonblocking(true).unwrap();
    let mut client_stream = tokio::net::UnixStream::from_std(client_stream).unwrap();
    let server = thread::spawn(move || {
        for expected in ["observer/inspect", "observer/inspect", "terminal/view"] {
            let request = read_control_response_frames(&mut server_stream, 1024 * 1024, 1).unwrap();
            let (body, _) = decode_control_frame(&request, 1024 * 1024).unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(
                parsed.get("method").and_then(serde_json::Value::as_str),
                Some(expected)
            );
            let response = match expected {
                "observer/inspect" if body.contains("cli-observer-inspect-0") => {
                    r#"{"jsonrpc":"2.0","id":"cli-observer-inspect-0","result":{"observer":{"id":"o1","observer_request_id":"o1","client_id":"c2","state":"pending"}}}"#
                }
                "observer/inspect" => {
                    r#"{"jsonrpc":"2.0","id":"cli-observer-inspect-1","result":{"observer":{"id":"o1","observer_request_id":"o1","client_id":"c2","state":"approved"}}}"#
                }
                _ => {
                    r#"{"jsonrpc":"2.0","id":"cli-terminal-view-2","result":{"view":{"lines":["observer live view"],"line_style_spans":[[]],"cursor":{"row":0,"column":18,"visible":true,"style":"block","blink":false},"output_modes":{"application_keypad":false}}}}"#
                }
            };
            server_stream
                .write_all(&encode_control_body(response))
                .unwrap();
            server_stream.flush().unwrap();
        }
    });

    let mut io = AsyncFakeAttachedTerminalIo::default();
    io.push_input(b"x".to_vec());
    io.push_input(b"y".to_vec());
    io.push_input(b"z".to_vec());

    run_control_socket_attached_observer_client_loop_async(
        &mut client_stream,
        &mut io,
        "o1".to_string(),
        Size::new(80, 24).unwrap(),
    )
    .await
    .unwrap();
    server.join().unwrap();

    assert_eq!(io.presentation_entries, 1);
    assert_eq!(io.written_frames.len(), 2);
    assert_eq!(
        io.written_frames[0].lines,
        vec!["observer pending approval"]
    );
    assert_eq!(io.written_frames[1].lines, vec!["observer live view"]);
}

/// Verifies snapshot resume latest selects newest matching snapshot.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_resume_latest_selects_newest_matching_snapshot() {
    let (env, home) = test_env("snapshot-resume-latest");
    let repository =
        SnapshotRepository::new(home.join(".config").join("mezzanine").join("snapshots"));
    let mut session = Session::new_default(
        resolve_shell(Some(OsString::from("/bin/sh"))).unwrap(),
        Size::new(80, 24).unwrap(),
    );
    repository
        .create_from_session("snap-a", Some("old".to_string()), &session)
        .unwrap();
    let primary = session.attach_primary("primary", true).unwrap();
    session
        .split_active_pane(&primary, crate::layout::SplitDirection::Vertical)
        .unwrap();
    repository
        .create_from_session("snap-z", Some("new".to_string()), &session)
        .unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec![
            "mez".to_string(),
            "snapshot".to_string(),
            "resume-latest".to_string(),
            "--session-id".to_string(),
            session.id.to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains(r#""restored":true"#));
    assert!(output.contains(r#""session_id":"$1""#));
    assert!(output.contains(r#""pane_count":2"#));
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies mcp list reports empty configured servers.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn mcp_list_reports_empty_configured_servers() {
    let (env, home) = test_env("mcp-list");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec!["mez".to_string(), "mcp".to_string(), "list".to_string()],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    assert_eq!(
        String::from_utf8(stdout).unwrap(),
        r#"{"servers":[],"tools":[]}"#.to_string() + "\n"
    );
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies cli structured status defaults to plaintext and json opt in remains available.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn cli_structured_status_defaults_to_plaintext_and_json_opt_in_remains_available() {
    let (env, home) = test_env("plain-output");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with_plain(
        vec!["mez".to_string(), "mcp".to_string(), "list".to_string()],
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    let plain = String::from_utf8(stdout).unwrap();
    assert!(plain.contains("servers:"), "{plain}");
    assert!(plain.contains("(none)"), "{plain}");
    assert!(!plain.trim_start().starts_with('{'), "{plain}");

    let mut json_stdout = Vec::new();
    run_with_plain(
        vec![
            "mez".to_string(),
            "mcp".to_string(),
            "list".to_string(),
            "--json".to_string(),
        ],
        env,
        false,
        &mut json_stdout,
        &mut stderr,
    )
    .unwrap();

    assert_eq!(
        String::from_utf8(json_stdout).unwrap(),
        r#"{"servers":[],"tools":[]}"#.to_string() + "\n"
    );
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies mcp cli adds inspects toggles and removes configured server.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn mcp_cli_adds_inspects_toggles_and_removes_configured_server() {
    let (env, home) = test_env("mcp-config");
    let mut stderr = Vec::new();

    let mut add_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "mcp".to_string(),
            "add".to_string(),
            "fs".to_string(),
            "--command".to_string(),
            "mcp-fs".to_string(),
            "--arg".to_string(),
            "--root".to_string(),
            "--arg".to_string(),
            ".".to_string(),
        ],
        env.clone(),
        false,
        &mut add_stdout,
        &mut stderr,
    )
    .unwrap();
    let add_output = String::from_utf8(add_stdout).unwrap();
    assert!(add_output.contains(r#""server_id":"fs""#));
    assert!(add_output.contains(r#""changed":true"#));

    let mut inspect_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "mcp".to_string(),
            "inspect".to_string(),
            "fs".to_string(),
        ],
        env.clone(),
        false,
        &mut inspect_stdout,
        &mut stderr,
    )
    .unwrap();
    let inspect_output = String::from_utf8(inspect_stdout).unwrap();
    assert!(inspect_output.contains(r#""id":"fs""#));
    assert!(inspect_output.contains(r#""name":"fs""#));
    assert!(inspect_output.contains(r#""command":"mcp-fs""#));
    assert!(inspect_output.contains(r#""args":["--root","."]"#));

    let mut disable_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "mcp".to_string(),
            "disable".to_string(),
            "fs".to_string(),
        ],
        env.clone(),
        false,
        &mut disable_stdout,
        &mut stderr,
    )
    .unwrap();
    let disable_output = String::from_utf8(disable_stdout).unwrap();
    assert!(disable_output.contains(r#""enabled":false"#));

    let mut list_stdout = Vec::new();
    run_with(
        vec!["mez".to_string(), "mcp".to_string(), "list".to_string()],
        env.clone(),
        false,
        &mut list_stdout,
        &mut stderr,
    )
    .unwrap();
    let list_output = String::from_utf8(list_stdout).unwrap();
    assert!(list_output.contains(r#""id":"fs""#));
    assert!(list_output.contains(r#""enabled":false"#));

    let config_path = home.join(".config").join("mezzanine").join("config.toml");
    let mut setting_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "mcp".to_string(),
            "set".to_string(),
            "fs".to_string(),
            "startup-timeout-ms".to_string(),
            "1500".to_string(),
        ],
        env.clone(),
        false,
        &mut setting_stdout,
        &mut stderr,
    )
    .unwrap();
    let setting_output = String::from_utf8(setting_stdout).unwrap();
    assert!(setting_output.contains(r#""server_id":"fs""#));
    assert!(setting_output.contains(r#""changed":true"#));

    let mut tools_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "mcp".to_string(),
            "tools".to_string(),
            "enable".to_string(),
            "fs".to_string(),
            "read_file".to_string(),
            "write_file".to_string(),
        ],
        env.clone(),
        false,
        &mut tools_stdout,
        &mut stderr,
    )
    .unwrap();
    assert!(
        String::from_utf8(tools_stdout)
            .unwrap()
            .contains(r#""server_id":"fs""#)
    );

    let mut approval_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "mcp".to_string(),
            "approval".to_string(),
            "set".to_string(),
            "fs".to_string(),
            "prompt".to_string(),
        ],
        env.clone(),
        false,
        &mut approval_stdout,
        &mut stderr,
    )
    .unwrap();
    assert!(
        String::from_utf8(approval_stdout)
            .unwrap()
            .contains(r#""server_id":"fs""#)
    );

    let config_text = fs::read_to_string(&config_path).unwrap();
    assert!(config_text.contains("startup_timeout_ms = 1500"));
    assert!(config_text.contains("enabled_tools"));
    assert!(config_text.contains("read_file"));
    assert!(config_text.contains("approval = \"prompt\""));

    let mut config_text = fs::read_to_string(&config_path).unwrap();
    config_text.push_str("\n[mcp_servers.fs.env]\nLOG_LEVEL = \"debug\"\n");
    fs::write(&config_path, config_text).unwrap();

    let mut remove_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "mcp".to_string(),
            "remove".to_string(),
            "fs".to_string(),
        ],
        env,
        false,
        &mut remove_stdout,
        &mut stderr,
    )
    .unwrap();
    let remove_output = String::from_utf8(remove_stdout).unwrap();
    assert!(remove_output.contains(r#""removed":true"#));
    let config_text = fs::read_to_string(config_path).unwrap();
    assert!(!config_text.contains("[mcp_servers.fs]"));
    assert!(!config_text.contains("[mcp_servers.fs.env]"));
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies memory cli adds inspects edits exports and deletes records.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn memory_cli_adds_inspects_edits_exports_and_deletes_records() {
    let (env, home) = test_env("memory-cli");
    let mut stderr = Vec::new();

    let mut add_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "memory".to_string(),
            "add".to_string(),
            "m1".to_string(),
            "--scope".to_string(),
            "project:/work/repo".to_string(),
            "--content".to_string(),
            "prefer cargo test".to_string(),
        ],
        env.clone(),
        false,
        &mut add_stdout,
        &mut stderr,
    )
    .unwrap();
    let add_output = String::from_utf8(add_stdout).unwrap();
    assert!(add_output.contains(r#""id":"m1""#));
    assert!(add_output.contains(r#""scope":"project:/work/repo""#));

    let mut edit_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "memory".to_string(),
            "edit".to_string(),
            "m1".to_string(),
            "--content".to_string(),
            "prefer cargo test --all-targets".to_string(),
        ],
        env.clone(),
        false,
        &mut edit_stdout,
        &mut stderr,
    )
    .unwrap();
    assert!(
        String::from_utf8(edit_stdout)
            .unwrap()
            .contains("cargo test --all-targets")
    );

    let mut list_stdout = Vec::new();
    run_with(
        vec!["mez".to_string(), "memory".to_string(), "list".to_string()],
        env.clone(),
        false,
        &mut list_stdout,
        &mut stderr,
    )
    .unwrap();
    assert!(
        String::from_utf8(list_stdout)
            .unwrap()
            .contains(r#""id":"m1""#)
    );

    let mut search_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "memory".to_string(),
            "search".to_string(),
            "all-targets".to_string(),
            "--kind".to_string(),
            "fact".to_string(),
        ],
        env.clone(),
        false,
        &mut search_stdout,
        &mut stderr,
    )
    .unwrap();
    assert!(
        String::from_utf8(search_stdout)
            .unwrap()
            .contains(r#""state":"active""#)
    );

    let mut export_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "memory".to_string(),
            "export".to_string(),
        ],
        env.clone(),
        false,
        &mut export_stdout,
        &mut stderr,
    )
    .unwrap();
    assert!(
        String::from_utf8(export_stdout)
            .unwrap()
            .contains("cargo test --all-targets")
    );

    let mut delete_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "memory".to_string(),
            "delete".to_string(),
            "m1".to_string(),
        ],
        env,
        false,
        &mut delete_stdout,
        &mut stderr,
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(delete_stdout).unwrap(),
        "{\"deleted\":true}\n"
    );
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies memory cli manages lifecycle metadata and retention.
///
/// This regression scenario covers the operator-facing commands that mark
/// memory use, confirmation, supersession, and retention effects.
#[test]
fn memory_cli_manages_lifecycle_metadata_and_retention() {
    let (env, home) = test_env("memory-lifecycle-cli");
    let mut stderr = Vec::new();

    for id in ["old", "new", "extra"] {
        let mut stdout = Vec::new();
        run_with(
            vec![
                "mez".to_string(),
                "memory".to_string(),
                "add".to_string(),
                id.to_string(),
                "--scope".to_string(),
                "global".to_string(),
                "--content".to_string(),
                format!("{id} workflow"),
            ],
            env.clone(),
            false,
            &mut stdout,
            &mut stderr,
        )
        .unwrap();
    }

    let mut use_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "memory".to_string(),
            "use".to_string(),
            "old".to_string(),
        ],
        env.clone(),
        false,
        &mut use_stdout,
        &mut stderr,
    )
    .unwrap();
    assert!(
        String::from_utf8(use_stdout)
            .unwrap()
            .contains(r#""use_count":1"#)
    );

    let mut confirm_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "memory".to_string(),
            "confirm".to_string(),
            "new".to_string(),
        ],
        env.clone(),
        false,
        &mut confirm_stdout,
        &mut stderr,
    )
    .unwrap();
    assert!(
        String::from_utf8(confirm_stdout)
            .unwrap()
            .contains(r#""confirmed_count":1"#)
    );

    let mut supersede_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "memory".to_string(),
            "supersede".to_string(),
            "old".to_string(),
            "new".to_string(),
        ],
        env.clone(),
        false,
        &mut supersede_stdout,
        &mut stderr,
    )
    .unwrap();
    let supersede_output = String::from_utf8(supersede_stdout).unwrap();
    assert!(supersede_output.contains(r#""state":"superseded""#));
    assert!(supersede_output.contains(r#""supersedes_id":null"#));

    let paths = env.config_paths().unwrap();
    fs::create_dir_all(paths.root()).unwrap();
    fs::write(
        paths.root().join("config.toml"),
        "version = 17\n[memory]\nmax_records = 2\narchive_before_prune = true\n",
    )
    .unwrap();

    let mut prune_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "memory".to_string(),
            "prune".to_string(),
            "--dry-run".to_string(),
        ],
        env.clone(),
        false,
        &mut prune_stdout,
        &mut stderr,
    )
    .unwrap();
    assert!(
        String::from_utf8(prune_stdout)
            .unwrap()
            .contains(r#""id":"old""#)
    );
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies memory cli accepts user-managed sensitive persistent content.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn memory_cli_accepts_sensitive_persistent_content_without_consent_flag() {
    let (env, home) = test_env("memory-sensitive");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec![
            "mez".to_string(),
            "memory".to_string(),
            "add".to_string(),
            "secret".to_string(),
            "--scope".to_string(),
            "global".to_string(),
            "--content".to_string(),
            "api_key = sk-secret".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    assert!(
        String::from_utf8(stdout)
            .unwrap()
            .contains("api_key = sk-secret")
    );
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies issue cli adds queries and deletes project-scoped records.
///
/// This regression scenario documents the local issue tracker behavior exposed
/// to scripts so project filters, kind filters, and delete results stay stable.
#[test]
fn issue_cli_adds_queries_and_deletes_project_records() {
    let (env, home) = test_env("issue-cli");
    let mut stderr = Vec::new();

    let mut add_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "issue".to_string(),
            "--project".to_string(),
            "/work/repo".to_string(),
            "add".to_string(),
            "--kind".to_string(),
            "defect".to_string(),
            "--title".to_string(),
            "Fix renderer panic".to_string(),
            "--body".to_string(),
            "panic while drawing borders".to_string(),
            "--notes".to_string(),
            "initial investigation started".to_string(),
        ],
        env.clone(),
        false,
        &mut add_stdout,
        &mut stderr,
    )
    .unwrap();
    let add_output = String::from_utf8(add_stdout).unwrap();
    assert!(add_output.contains(r#""project":"/work/repo""#));
    assert!(add_output.contains(r#""kind":"defect""#));
    assert!(add_output.contains(r#""title":"Fix renderer panic""#));
    assert!(add_output.contains(r#""notes":"initial investigation started""#));
    let id = add_output
        .split(r#""id":""#)
        .nth(1)
        .and_then(|tail| tail.split('"').next())
        .unwrap()
        .to_string();

    let mut query_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "issue".to_string(),
            "--project".to_string(),
            "/work/repo".to_string(),
            "query".to_string(),
            "--kind".to_string(),
            "defect".to_string(),
            "--text".to_string(),
            "borders".to_string(),
        ],
        env.clone(),
        false,
        &mut query_stdout,
        &mut stderr,
    )
    .unwrap();
    assert!(String::from_utf8(query_stdout).unwrap().contains(&id));

    let mut update_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "issue".to_string(),
            "--project".to_string(),
            "/work/repo".to_string(),
            "update".to_string(),
            id.clone(),
            "--notes".to_string(),
            "reproduced in narrow pane".to_string(),
        ],
        env.clone(),
        false,
        &mut update_stdout,
        &mut stderr,
    )
    .unwrap();
    let update_output = String::from_utf8(update_stdout).unwrap();
    assert!(update_output.contains(r#""updated":true"#));
    assert!(update_output.contains(r#""notes":"reproduced in narrow pane""#));

    let mut show_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "issue".to_string(),
            "--project".to_string(),
            "/work/repo".to_string(),
            "show".to_string(),
            id.clone(),
        ],
        env.clone(),
        false,
        &mut show_stdout,
        &mut stderr,
    )
    .unwrap();
    assert!(
        String::from_utf8(show_stdout)
            .unwrap()
            .contains(r#""notes":"reproduced in narrow pane""#)
    );

    let mut delete_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "issue".to_string(),
            "--project".to_string(),
            "/work/repo".to_string(),
            "delete".to_string(),
            id,
        ],
        env,
        false,
        &mut delete_stdout,
        &mut stderr,
    )
    .unwrap();
    assert!(
        String::from_utf8(delete_stdout)
            .unwrap()
            .contains(r#""deleted":true"#)
    );
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies issue cli honors the effective `issues.enabled` config gate before
/// opening or mutating the local issue store.
#[test]
fn issue_cli_rejects_commands_when_issue_tracking_is_disabled() {
    let (env, home) = test_env("issue-cli-disabled");
    let config_root = home.join(".config").join("mezzanine");
    fs::create_dir_all(&config_root).unwrap();
    fs::write(
        config_root.join("config.toml"),
        "[issues]\nenabled = false\n",
    )
    .unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let error = run_with(
        vec![
            "mez".to_string(),
            "issue".to_string(),
            "--project".to_string(),
            "/work/repo".to_string(),
            "query".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap_err();

    assert!(
        error
            .message()
            .contains("issue commands require issues.enabled to be true"),
        "{}",
        error.message()
    );
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert!(!config_root.join("issues.sqlite").exists());

    let _ = fs::remove_dir_all(home);
}

/// Verifies issue cli uses `issues.database_path` so scripts share the same
/// configured store path as runtime slash commands and MAAP issue actions.
#[test]
fn issue_cli_uses_configured_database_path() {
    let (env, home) = test_env("issue-cli-database-path");
    let config_root = home.join(".config").join("mezzanine");
    fs::create_dir_all(&config_root).unwrap();
    fs::write(
        config_root.join("config.toml"),
        "[issues]\ndatabase_path = \"custom/issues.sqlite\"\n",
    )
    .unwrap();
    let mut stderr = Vec::new();

    let mut add_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "issue".to_string(),
            "--project".to_string(),
            "/work/repo".to_string(),
            "add".to_string(),
            "--title".to_string(),
            "Use configured issue DB".to_string(),
        ],
        env.clone(),
        false,
        &mut add_stdout,
        &mut stderr,
    )
    .unwrap();
    assert!(config_root.join("custom").join("issues.sqlite").exists());
    assert!(!config_root.join("issues.sqlite").exists());

    let mut query_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "issue".to_string(),
            "--project".to_string(),
            "/work/repo".to_string(),
            "query".to_string(),
            "--text".to_string(),
            "configured".to_string(),
        ],
        env,
        false,
        &mut query_stdout,
        &mut stderr,
    )
    .unwrap();
    assert!(
        String::from_utf8(query_stdout)
            .unwrap()
            .contains("Use configured issue DB")
    );
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}
