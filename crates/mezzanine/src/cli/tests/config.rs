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
        crate::config::initial_config_toml().unwrap()
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
            "sandbox".to_string(),
            "trust".to_string(),
            "add".to_string(),
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
    assert!(project_text.contains("lines = 12"));
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies typed provider-model commands preserve opaque ids, generate the
/// same deterministic path-safe collision suffixes as schema migration, and
/// apply updates selectively without dropping unrelated model metadata.
#[test]
fn config_model_add_list_and_update_are_typed_and_deterministic() {
    let (env, home) = test_env("config-model-lifecycle");
    let paths = env.config_paths().unwrap();
    let config_path = paths.ensure_default_config().unwrap();
    fs::write(
        &config_path,
        format!(
            "version = {}\n[providers.custom]\nkind = \"custom\"\napi = \"openai-chat-completions\"\nbase_url = \"http://localhost:1234/v1\"\nmodels = {{}}\n",
            crate::config::CURRENT_CONFIG_SCHEMA_VERSION
        ),
    )
    .unwrap();
    let mut stderr = Vec::new();

    for (id, display_name) in [
        ("vendor/model:latest", "Latest"),
        ("vendor.model/latest", "Alternate"),
    ] {
        let mut stdout = Vec::new();
        run_with(
            vec![
                "mez".to_string(),
                "--json".to_string(),
                "config".to_string(),
                "model".to_string(),
                "add".to_string(),
                "custom".to_string(),
                id.to_string(),
                "--display-name".to_string(),
                display_name.to_string(),
                "--aliases".to_string(),
                format!("{display_name}-alias,stable-{display_name}"),
                "--context-window-tokens".to_string(),
                "32768".to_string(),
                "--provider-option".to_string(),
                "service_tier=priority".to_string(),
            ],
            env.clone(),
            false,
            &mut stdout,
            &mut stderr,
        )
        .unwrap();
    }

    let mut update_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "--json".to_string(),
            "config".to_string(),
            "model".to_string(),
            "update".to_string(),
            "custom".to_string(),
            "vendor.model/latest".to_string(),
            "--max-output-tokens".to_string(),
            "4096".to_string(),
        ],
        env.clone(),
        false,
        &mut update_stdout,
        &mut stderr,
    )
    .unwrap();

    let text = fs::read_to_string(&config_path).unwrap();
    assert!(
        text.contains("[providers.custom.models.vendor-model-latest]"),
        "{text}"
    );
    assert!(
        text.contains("[providers.custom.models.vendor-model-latest-2]"),
        "{text}"
    );
    assert!(text.contains("id = \"vendor/model:latest\""), "{text}");
    assert!(text.contains("display_name = \"Alternate\""), "{text}");
    assert!(text.contains("context_window_tokens = 32768"), "{text}");
    assert!(text.contains("max_output_tokens = 4096"), "{text}");
    assert!(text.contains("service_tier = \"priority\""), "{text}");

    let mut list_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "--json".to_string(),
            "config".to_string(),
            "model".to_string(),
            "list".to_string(),
            "custom".to_string(),
        ],
        env.clone(),
        false,
        &mut list_stdout,
        &mut stderr,
    )
    .unwrap();
    let output: serde_json::Value = serde_json::from_slice(&list_stdout).unwrap();
    let models = output["models"].as_array().unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0]["id"], "vendor.model/latest");
    assert_eq!(models[1]["id"], "vendor/model:latest");

    let mut remove_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "--json".to_string(),
            "config".to_string(),
            "model".to_string(),
            "remove".to_string(),
            "custom".to_string(),
            "vendor.model/latest".to_string(),
        ],
        env,
        false,
        &mut remove_stdout,
        &mut stderr,
    )
    .unwrap();
    let remove: serde_json::Value = serde_json::from_slice(&remove_stdout).unwrap();
    assert_eq!(remove["operation"], "remove");
    assert_eq!(remove["id"], "vendor.model/latest");
    let text = fs::read_to_string(&config_path).unwrap();
    assert!(!text.contains("id = \"vendor.model/latest\""), "{text}");
    assert!(text.contains("id = \"vendor/model:latest\""), "{text}");
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies typed provider-model mutations reject duplicate metadata, invalid
/// token limits, secret-looking provider options, and destructive changes to
/// ids still selected by either a provider default or a model profile.
#[test]
fn config_model_validation_and_reference_guards_are_safe() {
    let (env, home) = test_env("config-model-guards");
    let paths = env.config_paths().unwrap();
    let config_path = paths.ensure_default_config().unwrap();
    fs::write(
        &config_path,
        format!(
            "version = {}\n[providers.custom]\nkind = \"custom\"\napi = \"openai-chat-completions\"\nbase_url = \"http://localhost:1234/v1\"\ndefault_model = \"alpha/model\"\n[providers.custom.models.alpha]\nid = \"alpha/model\"\naliases = [\"alpha\"]\n[model_profiles.work]\nprovider = \"custom\"\nmodel = \"alpha/model\"\n",
            crate::config::CURRENT_CONFIG_SCHEMA_VERSION
        ),
    )
    .unwrap();
    let original = fs::read_to_string(&config_path).unwrap();
    let mut stderr = Vec::new();

    for arguments in [
        vec!["add", "custom", "beta", "--aliases", "duplicate,duplicate"],
        vec!["add", "custom", "beta", "--context-window-tokens", "0"],
        vec![
            "add",
            "custom",
            "beta",
            "--provider-option",
            "api_key=secret",
        ],
        vec!["remove", "custom", "alpha/model"],
        vec![
            "update",
            "custom",
            "alpha/model",
            "--new-id",
            "renamed/model",
        ],
    ] {
        let mut argv = vec!["mez".to_string(), "config".to_string(), "model".to_string()];
        argv.extend(arguments.into_iter().map(str::to_string));
        let mut stdout = Vec::new();
        assert!(
            run_with(argv, env.clone(), false, &mut stdout, &mut stderr).is_err(),
            "unexpected success: {}",
            String::from_utf8_lossy(&stdout)
        );
        assert_eq!(fs::read_to_string(&config_path).unwrap(), original);
    }

    let _ = fs::remove_dir_all(home);
}

/// Verifies an explicitly empty compatible-provider catalog is reported
/// deterministically with a command that can populate it.
#[test]
fn config_model_list_empty_catalog_includes_guidance() {
    let (env, home) = test_env("config-model-empty");
    let paths = env.config_paths().unwrap();
    let config_path = paths.ensure_default_config().unwrap();
    fs::write(
        &config_path,
        format!(
            "version = {}\n[providers.local]\nkind = \"custom\"\napi = \"openai-chat-completions\"\nbase_url = \"http://localhost:1234/v1\"\nmodels = {{}}\n",
            crate::config::CURRENT_CONFIG_SCHEMA_VERSION
        ),
    )
    .unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec![
            "mez".to_string(),
            "--json".to_string(),
            "config".to_string(),
            "model".to_string(),
            "list".to_string(),
            "local".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    let output: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(output["models"], serde_json::json!([]));
    assert!(
        output["guidance"]
            .as_str()
            .unwrap()
            .contains("mez config model add local MODEL_ID")
    );
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies typed model commands honor an explicit JSON user target and can
/// safely rename an unreferenced opaque id while regenerating its local key.
/// A leading hyphen in the provider-facing id must remain data rather than
/// being interpreted as an option by the CLI parser.
#[test]
fn config_model_rename_supports_opaque_ids_and_explicit_json_files() {
    let (env, home) = test_env("config-model-json-rename");
    let paths = env.config_paths().unwrap();
    paths.ensure_default_config().unwrap();
    let config_path = paths.root().join("catalog.json");
    fs::write(
        &config_path,
        format!(
            "{{\"version\":{},\"providers\":{{\"custom\":{{\"kind\":\"custom\",\"api\":\"openai-chat-completions\",\"base_url\":\"http://localhost:1234/v1\",\"models\":{{}}}}}}}}",
            crate::config::CURRENT_CONFIG_SCHEMA_VERSION
        ),
    )
    .unwrap();
    let target = config_path.to_string_lossy().to_string();
    let mut stderr = Vec::new();

    let mut add_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "config".to_string(),
            "model".to_string(),
            "add".to_string(),
            "custom".to_string(),
            "--vendor/model".to_string(),
            "--scope".to_string(),
            "user".to_string(),
            "--file".to_string(),
            target.clone(),
        ],
        env.clone(),
        false,
        &mut add_stdout,
        &mut stderr,
    )
    .unwrap();

    let mut update_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "--json".to_string(),
            "config".to_string(),
            "model".to_string(),
            "update".to_string(),
            "custom".to_string(),
            "--vendor/model".to_string(),
            "--new-id".to_string(),
            "vendor/model:v2".to_string(),
            "--file".to_string(),
            target,
        ],
        env,
        false,
        &mut update_stdout,
        &mut stderr,
    )
    .unwrap();

    let output: serde_json::Value = serde_json::from_slice(&update_stdout).unwrap();
    assert_eq!(output["id"], "vendor/model:v2");
    assert_eq!(output["entry_key"], "vendor-model-v2");
    let document: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(config_path).unwrap()).unwrap();
    assert_eq!(
        document["providers"]["custom"]["models"]["vendor-model-v2"]["id"],
        "vendor/model:v2"
    );
    assert!(
        document["providers"]["custom"]["models"]
            .get("vendor-model")
            .is_none()
    );
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies sandbox trust subcommands persist project decisions.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn sandbox_trust_subcommands_persist_project_decisions() {
    let (env, home) = test_env("sandbox-trust");
    let project = home.join("repo");
    fs::create_dir_all(project.join(".git")).unwrap();
    let mut trust_stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "trust".to_string(),
            "add".to_string(),
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
            "sandbox".to_string(),
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
    assert_eq!(
        effective.get("version"),
        Some(
            crate::config::CURRENT_CONFIG_SCHEMA_VERSION
                .to_string()
                .as_str()
        )
    );
    assert_eq!(
        effective.get("terminal.nested_multiplexer"),
        Some("disabled")
    );
    assert!(
        effective
            .get("agents.implementation_pressure_after_shell_actions")
            .is_none()
    );
    assert!(migrated.contains(&format!(
        "version = {}",
        crate::config::CURRENT_CONFIG_SCHEMA_VERSION
    )));
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
        format!(
            "version = {}\n[history]\nlines = 7\n",
            crate::config::CURRENT_CONFIG_SCHEMA_VERSION
        ),
    )
    .unwrap();
    fs::write(
        nested.join(".mezzanine/config.toml"),
        format!(
            "version = {}\n[history]\nlines = 11\n",
            crate::config::CURRENT_CONFIG_SCHEMA_VERSION
        ),
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
