//! CLI config tests.

use super::*;

/// Serves a fixed provider `/models` response and records each raw request.
fn spawn_config_model_catalog_server(
    status: u16,
    body: &str,
    request_count: usize,
) -> (String, std::thread::JoinHandle<Vec<String>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let body = body.to_string();
    let handle = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for _ in 0..request_count {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 1024];
            loop {
                let read = stream.read(&mut buffer).unwrap();
                if read == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..read]);
                if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            requests.push(String::from_utf8(bytes).unwrap());
            let reason = if status == 200 {
                "OK"
            } else {
                "Service Unavailable"
            };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
        requests
    });
    (base_url, handle)
}

/// Stores one file-backed provider API key for CLI model-catalog tests.
fn store_config_model_sync_api_key(env: &CliEnv, provider: &str, secret: &str) {
    let paths = env.config_paths().unwrap();
    let auth = AuthStore::new(AuthPaths::under_config_root(paths.root()));
    let credential_store = auth.file_credential_store(provider).unwrap();
    auth.login_provider_api_key(provider, "default", secret, &credential_store)
        .unwrap();
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

/// Verifies sync previews without writing, then atomically fills absent fields
/// and adds opaque collision-prone ids from the raw authenticated catalog.
#[test]
fn config_model_sync_preview_and_apply_use_raw_authenticated_catalog() {
    let (env, home) = test_env("config-model-sync-apply");
    let paths = env.config_paths().unwrap();
    let config_path = paths.ensure_default_config().unwrap();
    let catalog = r#"{"data":[{"id":"existing/model","display_name":"Observed name","reasoning_levels":["high"],"capabilities":["tool_use"],"context_length":32768},{"id":"vendor/model:latest"},{"id":"vendor.model/latest"}]}"#;
    let (base_url, server) = spawn_config_model_catalog_server(200, catalog, 2);
    fs::write(
        &config_path,
        format!(
            "version = {}\n[providers.local]\nkind = \"custom\"\napi = \"openai-chat-completions\"\nauth_profile = \"default\"\nbase_url = \"{base_url}\"\n[providers.local.models.existing]\nid = \"existing/model\"\ndisplay_name = \"Configured name\"\naliases = [\"stable\"]\nreasoning_levels = []\n[providers.local.models.existing.provider_options]\ntier = \"local\"\n[providers.local.models.vendor-model-latest]\nid = \"configured-only\"\n",
            crate::config::CURRENT_CONFIG_SCHEMA_VERSION
        ),
    )
    .unwrap();
    store_config_model_sync_api_key(&env, "local", "sk-sync-secret");
    let original = fs::read_to_string(&config_path).unwrap();
    let mut stderr = Vec::new();

    let mut preview_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "config".to_string(),
            "model".to_string(),
            "sync".to_string(),
            "local".to_string(),
        ],
        env.clone(),
        false,
        &mut preview_stdout,
        &mut stderr,
    )
    .unwrap();
    let preview: serde_json::Value = serde_json::from_slice(&preview_stdout).unwrap();
    assert_eq!(preview["apply"], false);
    assert_eq!(preview["changed"], true);
    assert_eq!(preview["persisted"], false);
    assert_eq!(preview["retained"], serde_json::json!(["configured-only"]));
    assert_eq!(preview["additions"].as_array().unwrap().len(), 2);
    assert_eq!(fs::read_to_string(&config_path).unwrap(), original);

    let mut apply_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "config".to_string(),
            "model".to_string(),
            "sync".to_string(),
            "local".to_string(),
            "--apply".to_string(),
        ],
        env,
        false,
        &mut apply_stdout,
        &mut stderr,
    )
    .unwrap();
    let applied: serde_json::Value = serde_json::from_slice(&apply_stdout).unwrap();
    assert_eq!(applied["persisted"], true);
    let text = fs::read_to_string(&config_path).unwrap();
    assert!(
        text.contains("display_name = \"Configured name\""),
        "{text}"
    );
    assert!(text.contains("reasoning_levels = []"), "{text}");
    assert!(text.contains("capabilities = [\"tool_use\"]"), "{text}");
    assert!(text.contains("context_window_tokens = 32768"), "{text}");
    assert!(text.contains("aliases = [\"stable\"]"), "{text}");
    assert!(text.contains("tier = \"local\""), "{text}");
    assert!(text.contains("id = \"vendor/model:latest\""), "{text}");
    assert!(text.contains("id = \"vendor.model/latest\""), "{text}");
    assert!(
        text.contains("[providers.local.models.vendor-model-latest-2]"),
        "{text}"
    );
    assert!(
        text.contains("[providers.local.models.vendor-model-latest-3]"),
        "{text}"
    );
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|request| request.starts_with("GET /v1/models "))
    );
    assert!(requests.iter().all(|request| {
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer sk-sync-secret")
    }));
    assert!(
        !String::from_utf8(apply_stdout)
            .unwrap()
            .contains("sk-sync-secret")
    );
    assert!(stderr.is_empty());
    let _ = fs::remove_dir_all(home);
}

/// Verifies retain-by-default and atomic prune behavior, including aggregation
/// of canonical and alias-selected reference blockers before any removal.
#[test]
fn config_model_sync_prune_is_explicit_atomic_and_reference_safe() {
    let (env, home) = test_env("config-model-sync-prune");
    let paths = env.config_paths().unwrap();
    let config_path = paths.ensure_default_config().unwrap();
    let (base_url, server) = spawn_config_model_catalog_server(200, r#"{"data":[]}"#, 2);
    fs::write(
        &config_path,
        format!(
            "version = {}\n[providers.local]\nkind = \"custom\"\napi = \"openai-chat-completions\"\nbase_url = \"{base_url}\"\ndefault_model = \"default-alias\"\n[providers.local.models.referenced]\nid = \"referenced/model\"\naliases = [\"default-alias\", \"profile-alias\"]\n[providers.local.models.unreferenced]\nid = \"unreferenced/model\"\n[model_profiles.work]\nprovider = \"local\"\nmodel = \"profile-alias\"\n",
            crate::config::CURRENT_CONFIG_SCHEMA_VERSION
        ),
    )
    .unwrap();
    let original = fs::read_to_string(&config_path).unwrap();
    let mut stderr = Vec::new();

    let mut retain_stdout = Vec::new();
    run_with(
        vec!["mez", "config", "model", "sync", "local"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        env.clone(),
        false,
        &mut retain_stdout,
        &mut stderr,
    )
    .unwrap();
    let retained: serde_json::Value = serde_json::from_slice(&retain_stdout).unwrap();
    assert_eq!(retained["retained"].as_array().unwrap().len(), 2);
    assert_eq!(retained["removals"], serde_json::json!([]));

    let mut prune_stdout = Vec::new();
    run_with(
        vec![
            "mez", "config", "model", "sync", "local", "--apply", "--prune",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
        env,
        false,
        &mut prune_stdout,
        &mut stderr,
    )
    .unwrap();
    let prune: serde_json::Value = serde_json::from_slice(&prune_stdout).unwrap();
    assert_eq!(prune["persisted"], false);
    assert_eq!(prune["removals"].as_array().unwrap().len(), 2);
    let blockers = prune["blockers"].as_array().unwrap();
    assert_eq!(blockers.len(), 2);
    assert!(
        blockers
            .iter()
            .any(|blocker| blocker["reference"] == "providers.local.default_model")
    );
    assert!(
        blockers
            .iter()
            .any(|blocker| blocker["reference"] == "model_profiles.work.model")
    );
    assert_eq!(fs::read_to_string(&config_path).unwrap(), original);
    assert_eq!(server.join().unwrap().len(), 2);
    assert!(stderr.is_empty());
    let _ = fs::remove_dir_all(home);
}

/// Verifies unreferenced configured-only models are removed only when both
/// prune and apply are explicit, and the resulting no-op preview is stable.
#[test]
fn config_model_sync_applies_unblocked_prune_and_renders_stable_noop() {
    let (env, home) = test_env("config-model-sync-noop");
    let paths = env.config_paths().unwrap();
    let config_path = paths.ensure_default_config().unwrap();
    let catalog = r#"{"data":[{"id":"live/model"}]}"#;
    let (base_url, server) = spawn_config_model_catalog_server(200, catalog, 3);
    fs::write(
        &config_path,
        format!(
            "version = {}\n[providers.local]\nkind = \"custom\"\napi = \"openai-chat-completions\"\nbase_url = \"{base_url}\"\n[providers.local.models.live]\nid = \"live/model\"\n[providers.local.models.old]\nid = \"old/model\"\n",
            crate::config::CURRENT_CONFIG_SCHEMA_VERSION
        ),
    )
    .unwrap();
    let mut stderr = Vec::new();
    let mut prune_stdout = Vec::new();
    run_with(
        vec![
            "mez", "config", "model", "sync", "local", "--prune", "--apply",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
        env.clone(),
        false,
        &mut prune_stdout,
        &mut stderr,
    )
    .unwrap();
    let prune: serde_json::Value = serde_json::from_slice(&prune_stdout).unwrap();
    assert_eq!(prune["persisted"], true);
    assert!(
        !fs::read_to_string(&config_path)
            .unwrap()
            .contains("old/model")
    );

    let mut first = Vec::new();
    let mut second = Vec::new();
    for output in [&mut first, &mut second] {
        run_with(
            vec!["mez", "config", "model", "sync", "local"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            env.clone(),
            false,
            output,
            &mut stderr,
        )
        .unwrap();
    }
    assert_eq!(first, second);
    let no_op: serde_json::Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(no_op["changed"], false);
    assert_eq!(no_op["persisted"], false);
    assert_eq!(server.join().unwrap().len(), 3);
    assert!(stderr.is_empty());
    let _ = fs::remove_dir_all(home);
}

/// Verifies a provider fetch failure leaves bytes unchanged and returns
/// secret-free manual-add guidance rather than planning from fallback data.
#[test]
fn config_model_sync_failed_fetch_preserves_bytes_and_gives_guidance() {
    let (env, home) = test_env("config-model-sync-failure");
    let paths = env.config_paths().unwrap();
    let config_path = paths.ensure_default_config().unwrap();
    let (base_url, server) = spawn_config_model_catalog_server(503, r#"{"error":"offline"}"#, 1);
    fs::write(
        &config_path,
        format!(
            "version = {}\n[providers.local]\nkind = \"custom\"\napi = \"openai-chat-completions\"\nbase_url = \"{base_url}\"\nmodels = {{}}\n",
            crate::config::CURRENT_CONFIG_SCHEMA_VERSION
        ),
    )
    .unwrap();
    store_config_model_sync_api_key(&env, "local", "sk-failure-secret");
    let original = fs::read_to_string(&config_path).unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let error = run_with(
        vec!["mez", "config", "model", "sync", "local", "--apply"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap_err();

    assert_eq!(fs::read_to_string(&config_path).unwrap(), original);
    assert!(
        error
            .message()
            .contains("mez config model add local MODEL_ID")
    );
    assert!(!error.message().contains("sk-failure-secret"));
    assert!(stdout.is_empty());
    let request = server.join().unwrap().pop().unwrap();
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer sk-failure-secret")
    );
    assert!(stderr.is_empty());
    let _ = fs::remove_dir_all(home);
}

/// Verifies project synchronization reads an inherited provider connection
/// while writing only minimal model overrides to the selected project layer.
#[test]
fn config_model_sync_project_target_uses_effective_connection_without_copying_user_config() {
    let (env, home) = test_env("config-model-sync-project");
    let paths = env.config_paths().unwrap();
    let user_config = paths.ensure_default_config().unwrap();
    let catalog =
        r#"{"data":[{"id":"inherited/model","display_name":"Observed"},{"id":"new/model"}]}"#;
    let (base_url, server) = spawn_config_model_catalog_server(200, catalog, 1);
    fs::write(
        &user_config,
        format!(
            "version = {}\n[providers.local]\nkind = \"custom\"\napi = \"openai-chat-completions\"\nbase_url = \"{base_url}\"\n[providers.local.models.inherited]\nid = \"inherited/model\"\n[providers.local.models.user-only]\nid = \"user-only/model\"\n",
            crate::config::CURRENT_CONFIG_SCHEMA_VERSION
        ),
    )
    .unwrap();
    let original_user = fs::read_to_string(&user_config).unwrap();
    let project = home.join("repo");
    fs::create_dir_all(project.join(".git")).unwrap();
    let project_config = project.join(".mezzanine/config.toml");
    let mut stderr = Vec::new();
    let mut trust_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "trust".to_string(),
            "add".to_string(),
            project.to_string_lossy().into_owned(),
        ],
        env.clone(),
        false,
        &mut trust_stdout,
        &mut stderr,
    )
    .unwrap();

    let mut stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "config".to_string(),
            "model".to_string(),
            "sync".to_string(),
            "local".to_string(),
            "--apply".to_string(),
            "--scope".to_string(),
            "project".to_string(),
            "--file".to_string(),
            project_config.to_string_lossy().into_owned(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    let output: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(output["scope"], "project");
    assert_eq!(output["persisted"], true);
    assert_eq!(fs::read_to_string(&user_config).unwrap(), original_user);
    let project_text = fs::read_to_string(&project_config).unwrap();
    assert!(
        project_text.contains("id = \"inherited/model\""),
        "{project_text}"
    );
    assert!(
        project_text.contains("display_name = \"Observed\""),
        "{project_text}"
    );
    assert!(
        project_text.contains("id = \"new/model\""),
        "{project_text}"
    );
    assert!(!project_text.contains("user-only/model"), "{project_text}");
    assert!(!project_text.contains("base_url"), "{project_text}");
    assert!(!project_text.contains("api ="), "{project_text}");
    assert!(!project_text.contains("kind ="), "{project_text}");
    assert_eq!(server.join().unwrap().len(), 1);
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
