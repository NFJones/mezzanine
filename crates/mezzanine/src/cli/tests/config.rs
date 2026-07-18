//! CLI config tests.

use super::*;

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
    assert_eq!(effective.get("version"), Some("20"));
    assert_eq!(
        effective.get("terminal.nested_multiplexer"),
        Some("disabled")
    );
    assert!(
        effective
            .get("agents.implementation_pressure_after_shell_actions")
            .is_none()
    );
    assert!(migrated.contains("version = 20"));
    assert!(migrated.contains("emoji_width = \"wide\""));
    assert!(migrated.contains("provider_refresh_leeway_seconds = 86400"));
    assert!(!migrated.contains("implementation_pressure_after_shell_actions"));
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
        "version = 20\n[history]\nlines = 7\n",
    )
    .unwrap();
    fs::write(
        nested.join(".mezzanine/config.toml"),
        "version = 20\n[history]\nlines = 11\n",
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
