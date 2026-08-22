//! CLI dispatch tests.

use super::*;

/// Verifies CLI startup creates the selected private runtime directory.
///
/// Daemon discovery and stale-socket cleanup both run before command dispatch,
/// so a fresh installation must have its owner-only runtime directory ready
/// even when no socket has been bound yet.
#[test]
fn cli_startup_creates_private_runtime_directory() {
    let (env, home) = test_env("startup-runtime-directory");
    let directory = default_socket_directory(&env.runtime).unwrap();
    assert!(!directory.path.exists());
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with_plain(
        vec!["mez".to_string(), "version".to_string()],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    let metadata = fs::symlink_metadata(&directory.path).unwrap();
    assert!(metadata.is_dir());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
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
    assert!(output.contains("kill"));
    assert!(output.contains("kill-session"));
    assert!(output.contains("sandbox"));
    assert!(output.contains("version"));
    assert!(output.contains("--version"));
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies the short session-termination command is canonical while the
/// established long spelling remains available as a visible alias.
///
/// Both spellings must parse into the same command variant so existing scripts
/// retain their session termination behavior as the primary CLI becomes `kill`.
#[test]
fn kill_command_accepts_kill_session_as_an_alias() {
    let (env, home) = test_env("kill-alias");

    for command in ["kill", "kill-session"] {
        let invocation = CliInvocation::parse(
            &[
                "mez".to_string(),
                command.to_string(),
                "$2".to_string(),
                "--force".to_string(),
            ],
            &env.runtime,
            None,
        )
        .unwrap();

        assert!(matches!(
            invocation.command,
            Some(CliCommand::Kill(KillSessionCliArgs {
                session_id: Some(session_id),
                force: true,
            })) if session_id == "$2"
        ));
    }

    let _ = fs::remove_dir_all(home);
}

/// Verifies `mez kill` resolves a creation-order session alias through the
/// registry before sending its termination request.
///
/// A targeted termination must reach the socket registered for `$1`, rather
/// than the default control socket, so users can kill a listed session without
/// copying its stable identifier.
#[test]
fn kill_routes_session_index_alias_to_registered_control_socket() {
    let (env, home) = test_env("kill-session-index-alias");
    let directory = default_socket_directory(&env.runtime).unwrap();
    let socket_path = directory.path.join("selected.sock");
    crate::runtime::ensure_private_socket_directory(&directory.path, env.runtime.uid).unwrap();
    let listener = bind_control_socket(&socket_path, env.runtime.uid).unwrap();
    let registry = SessionRegistry::new(directory.path.clone(), env.runtime.uid);
    registry
        .upsert(SessionRecord {
            session_id: "$selected".to_string(),
            name: "selected".to_string(),
            state: RegistrySessionState::Running,
            socket_path,
            created_at_unix_seconds: 100,
            last_attach_at_unix_seconds: None,
            window_count: 1,
            client_count: 0,
            primary_available: true,
            authoritative_columns: 80,
            authoritative_rows: 24,
        })
        .unwrap();
    let server = thread::spawn(move || {
        let (_probe, _) = listener.accept().unwrap();
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_control_response_frames(&mut stream, 4096, 1).unwrap();
        let (initialize, _) = decode_control_frame(&request, 4096).unwrap();
        assert!(
            initialize.contains(r#""method":"control/initialize""#),
            "{initialize}"
        );
        stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-init","result":{"granted_role":"primary"}}"#,
            ))
            .unwrap();
        let request = read_control_response_frames(&mut stream, 4096, 1).unwrap();
        let (kill, _) = decode_control_frame(&request, 4096).unwrap();
        assert!(kill.contains(r#""method":"session/kill""#), "{kill}");
        assert!(kill.contains(r#""force":true"#), "{kill}");
        stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli","result":{"killed":true}}"#,
            ))
            .unwrap();
    });
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec![
            "mez".to_string(),
            "kill".to_string(),
            "$1".to_string(),
            "--force".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();
    server.join().unwrap();

    assert!(
        String::from_utf8(stdout)
            .unwrap()
            .contains(r#""killed":true"#)
    );
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies completion generation exposes the complete typed clap command tree.
///
/// Shell definitions are generated from the same `CliArgv` parser used for
/// process dispatch, so new commands and options are included without a
/// separately maintained completion catalog.
#[test]
fn completion_command_generates_zsh_definition_from_cli_tree() {
    let (env, home) = test_env("completion-zsh");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with_plain(
        vec![
            "mez".to_string(),
            "completion".to_string(),
            "zsh".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains("#compdef mez"), "{output}");
    assert!(output.contains("completion"), "{output}");
    assert!(output.contains("sandbox"), "{output}");
    assert!(output.contains("--json"), "{output}");
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
        env.clone(),
        false,
        &mut empty_stdout,
        &mut empty_stderr,
    )
    .unwrap();

    let empty_output = String::from_utf8(empty_stdout).unwrap();
    assert!(empty_output.contains("Usage: mez config"), "{empty_output}");
    assert!(!empty_output.contains("trust"), "{empty_output}");
    assert!(empty_stderr.is_empty());

    let mut sandbox_stdout = Vec::new();
    let mut sandbox_stderr = Vec::new();
    run_with_plain(
        vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "--help".to_string(),
        ],
        env.clone(),
        false,
        &mut sandbox_stdout,
        &mut sandbox_stderr,
    )
    .unwrap();
    let sandbox_output = String::from_utf8(sandbox_stdout).unwrap();
    assert!(
        sandbox_output.contains("Usage: mez sandbox"),
        "{sandbox_output}"
    );
    assert!(sandbox_output.contains("trust"), "{sandbox_output}");
    assert!(
        !sandbox_output.contains("trust-current-project"),
        "{sandbox_output}"
    );
    assert!(sandbox_stderr.is_empty());

    let mut trust_stdout = Vec::new();
    let mut trust_stderr = Vec::new();
    run_with_plain(
        vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "trust".to_string(),
            "--help".to_string(),
        ],
        env,
        false,
        &mut trust_stdout,
        &mut trust_stderr,
    )
    .unwrap();
    let trust_output = String::from_utf8(trust_stdout).unwrap();
    assert!(
        trust_output.contains("Usage: mez sandbox trust"),
        "{trust_output}"
    );
    assert!(trust_output.contains("add"), "{trust_output}");
    assert!(!trust_output.contains("  trust"), "{trust_output}");
    assert!(trust_stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies the removed standalone toolchain hierarchy is rejected by the
/// typed command parser instead of retaining a deprecated alias.
#[test]
fn clap_rejects_removed_toolchain_commands() {
    let (env, home) = test_env("toolchain-help");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let error = run_with_plain(
        vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "toolchains".to_string(),
            "custom".to_string(),
            "define".to_string(),
            "--help".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(
        error.message().contains("unrecognized subcommand"),
        "{error}"
    );
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

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
        tmpdir: None,
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
        tmpdir: None,
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

/// Verifies explicit Iroh targets are mutually exclusive with Unix selectors.
#[test]
fn invocation_parses_explicit_iroh_target_without_unix_fallback() {
    let runtime = RuntimeEnv {
        mez_tmpdir: Some(OsString::from("/tmp")),
        xdg_runtime_dir: None,
        tmpdir: None,
        uid: 1000,
    };
    let invocation = CliInvocation::parse(
        &[
            "mez".to_string(),
            "--iroh-profile".to_string(),
            "workstation".to_string(),
            "kill".to_string(),
            "--force".to_string(),
        ],
        &runtime,
        None,
    )
    .unwrap();
    assert!(matches!(
        invocation.control_target,
        ControlTargetSelection::IrohProfile(ref profile) if profile == "workstation"
    ));
    assert!(!matches!(invocation.command, Some(CliCommand::Attach(_))));

    let invitation = CliInvocation::parse(
        &[
            "mez".to_string(),
            "--iroh-invite-file".to_string(),
            "/tmp/pairing.json".to_string(),
            "attach".to_string(),
            "--observer".to_string(),
        ],
        &runtime,
        None,
    )
    .unwrap();
    assert!(matches!(
        invitation.control_target,
        ControlTargetSelection::IrohInvitation(ref path)
            if path == &PathBuf::from("/tmp/pairing.json")
    ));
    assert!(matches!(invitation.command, Some(CliCommand::Attach(_))));

    let profile_attach = CliInvocation::parse(
        &[
            "mez".to_string(),
            "--iroh-profile".to_string(),
            "workstation".to_string(),
            "attach".to_string(),
        ],
        &runtime,
        None,
    )
    .unwrap();
    assert!(matches!(
        profile_attach.control_target,
        ControlTargetSelection::IrohProfile(ref profile) if profile == "workstation"
    ));
    assert!(matches!(
        profile_attach.command,
        Some(CliCommand::Attach(_))
    ));

    let error = CliInvocation::parse(
        &[
            "mez".to_string(),
            "-S".to_string(),
            "/tmp/local.sock".to_string(),
            "--iroh-profile".to_string(),
            "workstation".to_string(),
            "kill".to_string(),
            "--force".to_string(),
        ],
        &runtime,
        None,
    )
    .unwrap_err();
    assert!(error.message().contains("cannot be used with"), "{error}");
}

/// Verifies rejected commands name the complete command surface supported by
/// explicit Iroh targets.
///
/// The guidance must include `attach` alongside the lifecycle commands so a
/// rejected invocation does not incorrectly hide a working remote workflow.
#[test]
fn explicit_iroh_rejection_lists_every_supported_command() {
    let (env, home) = test_env("explicit-iroh-supported-command-guidance");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let error = run_with(
        vec![
            "mez".to_string(),
            "--iroh-profile".to_string(),
            "workstation".to_string(),
            "list".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap_err();

    assert_eq!(
        error.message(),
        "explicit Iroh targets currently support only attach, kill, and detach"
    );
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}
