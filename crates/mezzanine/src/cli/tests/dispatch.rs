//! CLI dispatch tests.

use super::*;

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
