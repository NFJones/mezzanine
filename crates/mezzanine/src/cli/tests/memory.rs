//! CLI memory tests.

use super::*;

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
