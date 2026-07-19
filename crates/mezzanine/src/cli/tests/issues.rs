//! CLI issues tests.

use super::*;

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
    assert!(add_output.contains(r#""depends_on":[]"#));
    let id = add_output
        .split(r#""id":""#)
        .nth(1)
        .and_then(|tail| tail.split('"').next())
        .unwrap()
        .to_string();

    let mut dependent_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "issue".to_string(),
            "--project".to_string(),
            "/work/repo".to_string(),
            "add".to_string(),
            "--kind".to_string(),
            "task".to_string(),
            "--title".to_string(),
            "Follow up after renderer fix".to_string(),
            "--depends-on".to_string(),
            id.clone(),
        ],
        env.clone(),
        false,
        &mut dependent_stdout,
        &mut stderr,
    )
    .unwrap();
    let dependent_output = String::from_utf8(dependent_stdout).unwrap();
    assert!(dependent_output.contains(&format!(r#""depends_on":["{}"]"#, id)));
    let dependent_id = dependent_output
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

    let mut resolve_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "issue".to_string(),
            "--project".to_string(),
            "/work/repo".to_string(),
            "update".to_string(),
            dependent_id,
            "--state".to_string(),
            "resolved".to_string(),
        ],
        env.clone(),
        false,
        &mut resolve_stdout,
        &mut stderr,
    )
    .unwrap();
    assert!(
        String::from_utf8(resolve_stdout)
            .unwrap()
            .contains(r#""state":"resolved""#)
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
