//! Regression coverage for read-only sandbox workflow commands.
//!
//! These tests protect the status/doctor output contract and, critically, the
//! invariant that inspection never migrates configuration, creates runtime
//! directories, writes trust state, prepares managed homes, or probes Bubblewrap.

use super::*;

/// Verifies sandbox status reports a trusted-project Bubblewrap boundary in
/// JSON without creating any inspection-owned state.
#[test]
fn sandbox_status_is_structured_and_strictly_read_only() {
    let (env, home) = test_env("sandbox-status-read-only");
    let config_root = home.join(".config/mezzanine");
    fs::create_dir_all(&config_root).unwrap();
    let config_path = config_root.join("config.toml");
    let config_text = "version = 23\n[permissions]\napproval_policy = \"full-access\"\nsandbox = \"bubblewrap\"\nread_scopes = [\"/tmp\"]\nwrite_scopes = []\n[permissions.bubblewrap]\nexecutable = \"/bin/sh\"\nunavailable = \"fail\"\nnetwork = \"isolated\"\nenvironment = \"minimal\"\n";
    fs::write(&config_path, config_text).unwrap();
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let before_config = fs::read(&config_path).unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let exit_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "status".to_string(),
            project.to_string_lossy().into_owned(),
        ]),
        env,
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();

    assert_eq!(exit_code, 0);
    let output: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(output["configured"]["sandbox"], "bubblewrap");
    assert_eq!(output["effective"]["sandbox"], "bubblewrap");
    assert_eq!(output["effective"]["scope_provenance"], "explicit");
    assert_eq!(
        output["effective"]["bubblewrap_executable_state"],
        "available"
    );
    assert_eq!(output["effective"]["bubblewrap_probe_state"], "not-probed");
    assert_eq!(output["mutations"], serde_json::json!([]));
    assert_eq!(output["confirmation"]["required"], false);
    assert!(stderr.is_empty());
    assert_eq!(fs::read(&config_path).unwrap(), before_config);
    assert!(!config_root.join("project-trust.tsv").exists());
    assert!(!config_root.join("sandbox").exists());
    assert!(!home.join("runtime/mez-0").exists());

    let _ = fs::remove_dir_all(home);
}

/// Verifies doctor returns the documented warning and error statuses while
/// retaining stable diagnostic identifiers in machine-readable output.
#[test]
fn sandbox_doctor_uses_stable_zero_one_two_exit_semantics() {
    let (warning_env, warning_home) = test_env("sandbox-doctor-warning");
    let warning_project = warning_home.join("project");
    fs::create_dir_all(&warning_project).unwrap();
    let mut warning_stdout = Vec::new();
    let mut warning_stderr = Vec::new();

    let warning_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "doctor".to_string(),
            warning_project.to_string_lossy().into_owned(),
        ]),
        warning_env,
        false,
        &mut warning_stdout,
        &mut warning_stderr,
    ))
    .unwrap();

    assert_eq!(warning_code, 1);
    let warning: serde_json::Value = serde_json::from_slice(&warning_stdout).unwrap();
    assert!(
        warning["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| {
                diagnostic["id"] == "sandbox.project-root-fallback"
                    && diagnostic["severity"] == "warning"
            })
    );
    assert!(warning_stderr.is_empty());

    let (error_env, error_home) = test_env("sandbox-doctor-error");
    let config_root = error_home.join(".config/mezzanine");
    fs::create_dir_all(&config_root).unwrap();
    fs::write(
        config_root.join("config.toml"),
        "version = 23\n[permissions]\nsandbox = \"bubblewrap\"\nread_scopes = [\"/tmp\"]\n[permissions.bubblewrap]\nexecutable = \"/definitely/missing/bwrap\"\n",
    )
    .unwrap();
    let error_project = error_home.join("project");
    fs::create_dir_all(error_project.join(".git")).unwrap();
    let mut error_stdout = Vec::new();
    let mut error_stderr = Vec::new();

    let error_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "doctor".to_string(),
            error_project.to_string_lossy().into_owned(),
        ]),
        error_env,
        false,
        &mut error_stdout,
        &mut error_stderr,
    ))
    .unwrap();

    assert_eq!(error_code, 2);
    let error: serde_json::Value = serde_json::from_slice(&error_stdout).unwrap();
    assert!(
        error["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| {
                diagnostic["id"] == "sandbox.bubblewrap-executable-unavailable"
                    && diagnostic["severity"] == "error"
            })
    );
    assert!(error_stderr.is_empty());

    let _ = fs::remove_dir_all(warning_home);
    let _ = fs::remove_dir_all(error_home);
}
