//! CLI mcp tests.

use super::*;

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
