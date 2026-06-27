//! Regression coverage for the config tests subsystem.
//!
//! These tests describe the behavior protected by the repository
//! specification and workflow guidance. Keeping the scenarios documented
//! makes failures easier to map back to the user-visible contract.

// Config module tests.

use super::{
    CURRENT_CONFIG_SCHEMA_VERSION, ConfigFormat, ConfigLayer, ConfigMutation,
    ConfigMutationOperation, ConfigMutationValue, ConfigPaths, ConfigScope, DEFAULT_CONFIG_TOML,
    PathBuf, compose_effective_config, extract_config_values, fs, migrate_config_text,
    persist_config_mutation, persist_config_mutation_async, plan_config_mutation,
    validate_config_file, validate_config_file_async, validate_config_text,
};
use crate::permissions::{exact_command_sha256, normalize_exact_command_text};
/// Runs the temp root operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn temp_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("mez-config-test-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    root
}

/// Runs the set string operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn set_string(path: &str, value: &str) -> ConfigMutation {
    ConfigMutation {
        path: path.to_string(),
        operation: ConfigMutationOperation::Set(ConfigMutationValue::String(value.to_string())),
    }
}

/// Runs the set integer operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn set_integer(path: &str, value: i64) -> ConfigMutation {
    ConfigMutation {
        path: path.to_string(),
        operation: ConfigMutationOperation::Set(ConfigMutationValue::Integer(value)),
    }
}

/// Runs the set boolean operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn set_boolean(path: &str, value: bool) -> ConfigMutation {
    ConfigMutation {
        path: path.to_string(),
        operation: ConfigMutationOperation::Set(ConfigMutationValue::Boolean(value)),
    }
}

/// Runs the set string array operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn set_string_array(path: &str, values: &[&str]) -> ConfigMutation {
    ConfigMutation {
        path: path.to_string(),
        operation: ConfigMutationOperation::Set(ConfigMutationValue::StringArray(
            values.iter().map(|value| value.to_string()).collect(),
        )),
    }
}

/// Runs the unset operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn unset(path: &str) -> ConfigMutation {
    ConfigMutation {
        path: path.to_string(),
        operation: ConfigMutationOperation::Unset,
    }
}

/// Verifies creates default config file.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn creates_default_config_file() {
    let root = temp_root("creates");
    let paths = ConfigPaths::from_root(root.clone());

    let path = paths.ensure_default_config().unwrap();

    assert_eq!(path, root.join("config.toml"));
    assert_eq!(fs::read_to_string(path).unwrap(), DEFAULT_CONFIG_TOML);

    let _ = fs::remove_dir_all(root);
}

/// Verifies that generated defaults use the same padded pane-title pill
/// template as the renderer's built-in fallback.
///
/// This guards the first-run config path, where an unpadded persisted template
/// would override the renderer default and make pane title spaces uncolored in
/// normal configured runs.
#[test]
fn default_config_pane_frame_template_uses_padded_title_pill() {
    assert!(
        DEFAULT_CONFIG_TOML.contains("template = \" #{pane.index} #{pane.title} \""),
        "{DEFAULT_CONFIG_TOML}"
    );
}

/// Verifies that first-run default config creation can run on Tokio filesystem
/// APIs while preserving the same selected path and default text as the
/// synchronous setup path.
#[tokio::test]
async fn creates_default_config_file_async() {
    let root = temp_root("creates-async");
    let paths = ConfigPaths::from_root(root.clone());

    let path = paths.ensure_default_config_async().await.unwrap();
    let selected = paths.select_primary_file_async().await.unwrap();

    assert_eq!(path, root.join("config.toml"));
    assert_eq!(selected.as_deref(), Some(path.as_path()));
    assert_eq!(
        tokio::fs::read_to_string(path).await.unwrap(),
        DEFAULT_CONFIG_TOML
    );

    let _ = fs::remove_dir_all(root);
}

/// Verifies that first-run default config creation is safe when multiple daemon
/// processes start against a fresh config root at the same time. Only one caller
/// creates `config.toml`; the others must treat the concurrently created file as
/// the selected primary config instead of surfacing `AlreadyExists`.
#[test]
fn concurrent_default_config_creation_is_idempotent() {
    use std::sync::{Arc, Barrier};
    use std::thread;

    let root = temp_root("concurrent-creates");
    let barrier = Arc::new(Barrier::new(8));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let root = root.clone();
        let barrier = barrier.clone();
        handles.push(thread::spawn(move || {
            let paths = ConfigPaths::from_root(root);
            barrier.wait();
            paths.ensure_default_config().unwrap()
        }));
    }

    for handle in handles {
        assert_eq!(handle.join().unwrap(), root.join("config.toml"));
    }
    assert_eq!(
        fs::read_to_string(root.join("config.toml")).unwrap(),
        DEFAULT_CONFIG_TOML
    );

    let _ = fs::remove_dir_all(root);
}

/// Verifies rejects ambiguous primary config files.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_ambiguous_primary_config_files() {
    let root = temp_root("ambiguous");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("config.toml"), "").unwrap();
    fs::write(root.join("config.json"), "{}").unwrap();
    let paths = ConfigPaths::from_root(root.clone());

    let error = paths.select_primary_file().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Config);

    let _ = fs::remove_dir_all(root);
}

/// Verifies default config matches documented example.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn default_config_matches_documented_example() {
    let documented = include_str!("../../docs/examples/config.toml");

    assert_eq!(DEFAULT_CONFIG_TOML.trim(), documented.trim());
}

/// Verifies generated defaults include the built-in Anthropic provider entry
/// and Claude model list used by runtime fallback catalog behavior.
///
/// Keeping the generated config aligned with the runtime built-ins prevents
/// docs and defaults from drifting back to OpenAI/DeepSeek-only provider
/// support while Anthropic remains implemented in code.
#[test]
fn default_config_includes_anthropic_provider_defaults() {
    let parsed: toml::Value = toml::from_str(DEFAULT_CONFIG_TOML).unwrap();
    let anthropic = parsed
        .get("providers")
        .and_then(toml::Value::as_table)
        .and_then(|providers| providers.get("anthropic"))
        .and_then(toml::Value::as_table)
        .unwrap();

    assert_eq!(
        anthropic.get("kind").and_then(toml::Value::as_str),
        Some("anthropic")
    );
    assert_eq!(
        anthropic.get("api").and_then(toml::Value::as_str),
        Some("anthropic-messages")
    );
    assert_eq!(
        anthropic.get("default_model").and_then(toml::Value::as_str),
        Some("claude-fable-5")
    );

    let models = anthropic
        .get("models")
        .and_then(toml::Value::as_array)
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(
        models,
        vec![
            "claude-fable-5",
            "claude-opus-4-8",
            "claude-sonnet-4-6",
            "claude-haiku-4-5-20251001",
        ]
    );

    let claude_code = parsed
        .get("providers")
        .and_then(toml::Value::as_table)
        .and_then(|providers| providers.get("claude-code"))
        .and_then(toml::Value::as_table)
        .unwrap();

    assert_eq!(
        claude_code.get("kind").and_then(toml::Value::as_str),
        Some("claude-code")
    );
    assert_eq!(
        claude_code.get("api").and_then(toml::Value::as_str),
        Some("claude-code")
    );
    assert_eq!(
        claude_code
            .get("default_model")
            .and_then(toml::Value::as_str),
        Some("claude-fable-5")
    );

    let claude_code_models = claude_code
        .get("models")
        .and_then(toml::Value::as_array)
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(claude_code_models, models);
}

/// Verifies generated defaults use provider-aware output token caps for known agent profiles.
///
/// A single universal output cap is not correct for all providers, but the
/// built-in OpenAI and DeepSeek profiles have known agent workload targets.
/// Keeping those caps explicit protects the generated default config from
/// drifting back to provider-default output budgets.
#[test]
fn default_config_uses_provider_aware_output_token_caps() {
    let parsed: toml::Value = toml::from_str(DEFAULT_CONFIG_TOML).unwrap();
    let profiles = parsed
        .get("model_profiles")
        .and_then(toml::Value::as_table)
        .unwrap();

    for (profile, expected) in [
        ("default", 16_384),
        ("auto-size-router", 8_192),
        ("auto-size-small", 16_384),
        ("auto-size-medium", 16_384),
        ("auto-size-large", 32_768),
        ("anthropic-default", 128_000),
        ("anthropic-fast", 64_000),
        ("deepseek-default", 32_768),
        ("deepseek-fast", 32_768),
    ] {
        let tokens = profiles
            .get(profile)
            .and_then(|profile| profile.get("max_output_tokens"))
            .and_then(toml::Value::as_integer);
        assert_eq!(tokens, Some(expected));
    }
}

/// Verifies the built-in DeepSeek preset uses canonical auto-sizing effort
/// names rather than provider-native aliases.
///
/// Auto-sizing decisions are parsed through Mezzanine's shared schema before
/// provider-specific request mapping occurs. Keeping the default preset on
/// `xhigh` lets the router select maximum DeepSeek thinking while preserving
/// the shared schema contract.
#[test]
fn default_deepseek_preset_uses_canonical_auto_sizing_efforts() {
    let parsed: toml::Value = toml::from_str(DEFAULT_CONFIG_TOML).unwrap();
    let efforts = parsed
        .get("model_presets")
        .and_then(|presets| presets.get("deepseek"))
        .and_then(|preset| preset.get("allowed_reasoning_efforts"))
        .and_then(toml::Value::as_array)
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(efforts, vec!["high", "xhigh"]);
    assert!(!efforts.contains(&"max"));
}

/// Verifies validates default toml config.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn validates_default_toml_config() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        DEFAULT_CONFIG_TOML,
        ConfigScope::Primary,
    );

    assert!(validation.valid, "{:?}", validation.diagnostics);
}

/// Verifies that user-facing theme configuration accepts alias-based color
/// assignments while still rejecting malformed hex values and unknown UI slots.
/// Theme values are applied at runtime, but static config validation needs to
/// catch spelling mistakes before a user reloads a broken interactive theme.
#[test]
fn validates_theme_aliases_and_color_slots() {
    let valid = validate_config_text(
        ConfigFormat::Toml,
        r##"
[theme]
active = "gruvbox_dark"

[theme.aliases]
primary = "#123456"

[theme.colors]
window_active_bg = "primary"
prompt_fg = "#abc"
syntax_keyword_fg = "primary"

[themes.deepforest_alt.aliases]
tertiary = "#fed"

[themes.deepforest_alt.colors]
pane_divider_fg = "tertiary"
syntax_string_fg = "tertiary"
"##,
        ConfigScope::Primary,
    );

    assert!(valid.valid, "{:?}", valid.diagnostics);

    let invalid = validate_config_text(
        ConfigFormat::Toml,
        r##"
[theme.aliases]
primary = "green"

[theme.colors]
not_a_slot = "primary"
prompt_fg = "$bad"
"##,
        ConfigScope::Primary,
    );

    assert!(!invalid.valid);
    assert!(invalid.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "theme.aliases.primary" && diagnostic.message.contains("hex colors")
    }));
    assert!(invalid.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "theme.colors.not_a_slot"
            && diagnostic.message.contains("unknown theme color slot")
    }));
    assert!(invalid.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "theme.colors.prompt_fg"
            && diagnostic.message.contains("hex colors or alias names")
    }));
}

/// Verifies validate config text rejects malformed supported formats.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn validate_config_text_rejects_malformed_supported_formats() {
    let json = validate_config_text(ConfigFormat::Json, r#"{ "session": "#, ConfigScope::Primary);
    let toml = validate_config_text(ConfigFormat::Toml, "[session", ConfigScope::Primary);
    let yaml = validate_config_text(
        ConfigFormat::Yaml,
        "session:\n  - [unterminated\n",
        ConfigScope::Primary,
    );

    assert!(!json.valid);
    assert_eq!(json.diagnostics[0].path, "$");
    assert!(json.diagnostics[0].message.contains("invalid JSON"));
    assert!(!toml.valid);
    assert!(toml.diagnostics[0].message.contains("invalid TOML"));
    assert!(!yaml.valid);
    assert!(yaml.diagnostics[0].message.contains("invalid YAML"));
}
/// Verifies YAML config parsing preserves mapping and root-shape behavior.
///
/// This regression scenario covers the maintained YAML parser replacement so
/// empty documents, mapping roots, and scalar roots keep the same user-visible
/// validation contract.
#[test]
fn yaml_config_parser_preserves_mapping_and_root_shape_behavior() {
    let empty = validate_config_text(ConfigFormat::Yaml, "  \n", ConfigScope::Primary);
    assert!(empty.valid, "{:?}", empty.diagnostics);

    let mapping = validate_config_text(
        ConfigFormat::Yaml,
        "history:\n  lines: 200\n  persist: true\n",
        ConfigScope::Primary,
    );
    assert!(mapping.valid, "{:?}", mapping.diagnostics);
    let values = extract_config_values(
        ConfigFormat::Yaml,
        "history:\n  lines: 200\n  persist: true\n",
    );
    assert_eq!(values.get("history.lines").map(String::as_str), Some("200"));
    assert_eq!(
        values.get("history.persist").map(String::as_str),
        Some("true")
    );

    let scalar = validate_config_text(ConfigFormat::Yaml, "42\n", ConfigScope::Primary);
    assert!(!scalar.valid);
    assert_eq!(scalar.diagnostics[0].path, "$".to_string());
    assert!(
        scalar.diagnostics[0]
            .message
            .contains("YAML configuration root must be a mapping")
    );
}

/// Verifies validate config file reports syntax errors with file context.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn validate_config_file_reports_syntax_errors_with_file_context() {
    let root = temp_root("parse-context");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("config.json");
    fs::write(&path, r#"{ "history": "#).unwrap();

    let validation = validate_config_file(&path, ConfigScope::Primary).unwrap();

    assert!(!validation.valid);
    assert_eq!(validation.diagnostics[0].path, path.display().to_string());

    let _ = fs::remove_dir_all(root);
}

/// Verifies config mutation sets toml scalar and plans reload.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn config_mutation_sets_toml_scalar_and_plans_reload() {
    let plan = plan_config_mutation(
        ConfigFormat::Toml,
        "[history]\nlines = 10000\n",
        ConfigScope::Primary,
        set_integer("history.lines", 2000),
    )
    .unwrap();

    assert!(plan.changed);
    assert!(plan.reload_required);
    assert!(plan.validation.valid);
    assert_eq!(
        extract_config_values(ConfigFormat::Toml, &plan.text).get("history.lines"),
        Some(&"2000".to_string())
    );
}

/// Verifies config mutation sets string array values.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn config_mutation_sets_string_array_values() {
    let plan = plan_config_mutation(
        ConfigFormat::Toml,
        "[mcp_servers.fs]\ncommand = \"mcp-fs\"\n",
        ConfigScope::Primary,
        set_string_array("mcp_servers.fs.args", &["--root", "."]),
    )
    .unwrap();

    assert!(plan.changed);
    assert!(plan.validation.valid);
    assert_eq!(
        extract_config_values(ConfigFormat::Toml, &plan.text).get("mcp_servers.fs.args"),
        Some(&r#"["--root", "."]"#.to_string())
    );

    let json = plan_config_mutation(
        ConfigFormat::Json,
        r#"{"mcp_servers":{"fs":{"command":"mcp-fs"}}}"#,
        ConfigScope::Primary,
        set_string_array("mcp_servers.fs.args", &["--root", "."]),
    )
    .unwrap();

    assert!(json.changed);
    assert!(json.validation.valid);
    let raw_json_args = extract_config_values(ConfigFormat::Json, &json.text)
        .get("mcp_servers.fs.args")
        .cloned()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&raw_json_args).unwrap(),
        serde_json::json!(["--root", "."])
    );
}

/// Verifies config mutation sets nested MCP external-capability scalars.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn config_mutation_sets_mcp_external_capability_nested_scalars() {
    let usage_plan = plan_config_mutation(
        ConfigFormat::Toml,
        "[mcp_servers.fs]\ncommand = \"mcp-fs\"\n",
        ConfigScope::Primary,
        set_string(
            "mcp_servers.fs.external_capability.usage_instructions",
            "Use read_file only when the task needs file contents.",
        ),
    )
    .unwrap();

    assert!(usage_plan.changed);
    assert!(usage_plan.validation.valid);
    assert_eq!(
        extract_config_values(ConfigFormat::Toml, &usage_plan.text)
            .get("mcp_servers.fs.external_capability.usage_instructions"),
        Some(&"Use read_file only when the task needs file contents.".to_string())
    );

    let purpose_plan = plan_config_mutation(
        ConfigFormat::Toml,
        "[mcp_servers.fs]\ncommand = \"mcp-fs\"\n",
        ConfigScope::Primary,
        set_string(
            "mcp_servers.fs.external_capability.purpose",
            "Filesystem read operations",
        ),
    )
    .unwrap();

    assert!(purpose_plan.changed);
    assert!(purpose_plan.validation.valid);
    assert_eq!(
        extract_config_values(ConfigFormat::Toml, &purpose_plan.text)
            .get("mcp_servers.fs.external_capability.purpose"),
        Some(&"Filesystem read operations".to_string())
    );

    let safety_plan = plan_config_mutation(
        ConfigFormat::Toml,
        "[mcp_servers.fs]\ncommand = \"mcp-fs\"\n[mcp_servers.fs.external_capability]\npurpose = \"Filesystem write operations\"\n",
        ConfigScope::Primary,
        set_boolean(
            "mcp_servers.fs.external_capability.mutates_filesystem_outside_shell",
            true,
        ),
    )
    .unwrap();

    assert!(safety_plan.changed);
    assert!(safety_plan.validation.valid);
    assert_eq!(
        extract_config_values(ConfigFormat::Toml, &safety_plan.text)
            .get("mcp_servers.fs.external_capability.mutates_filesystem_outside_shell"),
        Some(&"true".to_string())
    );
}

/// Verifies config mutation unsets yaml scalar.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn config_mutation_unsets_yaml_scalar() {
    let plan = plan_config_mutation(
        ConfigFormat::Yaml,
        "history:\n  lines: 10000\n  persist: true\n",
        ConfigScope::Primary,
        unset("history.persist"),
    )
    .unwrap();

    assert!(plan.changed);
    assert!(plan.validation.valid);
    assert_eq!(
        extract_config_values(ConfigFormat::Yaml, &plan.text).get("history.lines"),
        Some(&"10000".to_string())
    );
    assert!(!extract_config_values(ConfigFormat::Yaml, &plan.text).contains_key("history.persist"));
}

/// Verifies config mutation rejects validation failure.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn config_mutation_rejects_validation_failure() {
    let error = plan_config_mutation(
        ConfigFormat::Toml,
        "[permissions]\napproval_policy = \"ask\"\n",
        ConfigScope::Primary,
        set_string("permissions.approval_policy", "on-failure"),
    )
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Config);
    assert!(error.message().contains("permissions.approval_policy"));
}

/// Verifies config mutation rejects unsupported nested paths and container sets.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn config_mutation_rejects_unsupported_nested_paths_and_container_sets() {
    let nested_error = plan_config_mutation(
        ConfigFormat::Toml,
        "[mcp_servers.fs.env]\nLOG_LEVEL = \"debug\"\n",
        ConfigScope::Primary,
        set_string("mcp_servers.fs.env.LOG_LEVEL", "trace"),
    )
    .unwrap_err();

    assert_eq!(nested_error.kind(), crate::error::MezErrorKind::Config);
    assert!(nested_error.message().contains("three segments"));

    let container_error = plan_config_mutation(
        ConfigFormat::Json,
        r#"{"history":{"lines":10000}}"#,
        ConfigScope::Primary,
        set_string("history", "oops"),
    )
    .unwrap_err();

    assert_eq!(container_error.kind(), crate::error::MezErrorKind::Config);
    assert!(container_error.message().contains("nested container"));
}

/// Verifies config mutation unsets nested containers.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn config_mutation_unsets_nested_containers() {
    let plan = plan_config_mutation(
        ConfigFormat::Json,
        r#"{"mcp_servers":{"fs":{"command":"mcp-fs","env":{"LOG_LEVEL":"debug"}}},"history":{"lines":10000}}"#,
        ConfigScope::Primary,
        unset("mcp_servers.fs"),
    )
    .unwrap();

    assert!(plan.changed);
    assert!(plan.validation.valid);
    let values = extract_config_values(ConfigFormat::Json, &plan.text);
    assert!(!values.keys().any(|path| path.starts_with("mcp_servers.fs")));
    assert_eq!(values.get("history.lines"), Some(&"10000".to_string()));

    let yaml = plan_config_mutation(
        ConfigFormat::Yaml,
        "mcp_servers:\n  fs:\n    command: mcp-fs\n    env:\n      LOG_LEVEL: debug\nhistory:\n  lines: 10000\n",
        ConfigScope::Primary,
        unset("mcp_servers.fs"),
    )
    .unwrap();

    assert!(yaml.changed);
    assert!(yaml.validation.valid);
    let values = extract_config_values(ConfigFormat::Yaml, &yaml.text);
    assert!(!values.keys().any(|path| path.starts_with("mcp_servers.fs")));
    assert_eq!(values.get("history.lines"), Some(&"10000".to_string()));
}

/// Verifies config mutation persists json scalar with private posture.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn config_mutation_persists_json_scalar_with_private_posture() {
    let root = temp_root("persist");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("config.json");
    fs::write(&path, r#"{"history":{"lines":10000}}"#).unwrap();

    let plan = persist_config_mutation(
        &path,
        ConfigScope::Primary,
        set_boolean("history.persist", true),
    )
    .unwrap();

    assert!(plan.changed);
    assert!(plan.validation.valid);
    let text = fs::read_to_string(&path).unwrap();
    assert_eq!(
        extract_config_values(ConfigFormat::Json, &text).get("history.persist"),
        Some(&"true".to_string())
    );

    let _ = fs::remove_dir_all(root);
}

/// Verifies that async config mutation persistence uses the same planning,
/// validation, and private-file behavior as the synchronous mutation path.
#[tokio::test]
async fn config_mutation_persists_json_scalar_with_private_posture_async() {
    let root = temp_root("persist-async");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("config.json");
    fs::write(&path, r#"{"history":{"lines":10000}}"#).unwrap();

    let plan = persist_config_mutation_async(
        &path,
        ConfigScope::Primary,
        set_boolean("history.persist", true),
    )
    .await
    .unwrap();
    let validation = validate_config_file_async(&path, ConfigScope::Primary)
        .await
        .unwrap();

    assert!(plan.changed);
    assert!(plan.validation.valid);
    assert!(validation.valid);
    let text = fs::read_to_string(&path).unwrap();
    assert_eq!(
        extract_config_values(ConfigFormat::Json, &text).get("history.persist"),
        Some(&"true".to_string())
    );

    let _ = fs::remove_dir_all(root);
}

/// Verifies rejects unknown top level keys.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_unknown_top_level_keys() {
    let validation =
        validate_config_text(ConfigFormat::Toml, "unknown = true\n", ConfigScope::Primary);

    assert!(!validation.valid);
    assert_eq!(validation.diagnostics[0].path, "unknown");
}

/// Verifies rejects unknown nested schema keys.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_unknown_nested_schema_keys() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[terminal]\nextra = true\n[frames.status]\nenabled = true\n[frames.pane]\nright_status = \"pane\"\n[providers.openai]\nunknown = true\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "terminal.extra"
            && diagnostic.message == "unknown terminal configuration key"
    }));
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "frames.status.enabled"
            && diagnostic.message == "unknown frames configuration target"
    }));
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "frames.pane.right_status"
            && diagnostic.message == "unknown frame configuration key"
    }));
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "providers.openai.unknown"
            && diagnostic.message == "unknown provider configuration key"
    }));
}

/// Verifies that the historical nested-muxer key spelling is accepted only as
/// a migration alias for the canonical terminal nested-multiplexer setting.
/// This protects existing primary configuration files written before the
/// spelling cleanup from blocking daemon launch while keeping the effective
/// configuration surface canonical.
#[test]
fn accepts_legacy_nested_muxxer_alias_as_terminal_migration_key() {
    let text = "[terminal]\nnested_muxxer = \"auto\"\n";
    let validation = validate_config_text(ConfigFormat::Toml, text, ConfigScope::Primary);

    assert!(validation.valid, "{:?}", validation.diagnostics);
    let values = extract_config_values(ConfigFormat::Toml, text);
    assert_eq!(
        values.get("terminal.nested_multiplexer"),
        Some(&"auto".to_string())
    );
    assert!(!values.contains_key("terminal.nested_muxxer"));
}

/// Verifies that the canonical terminal nested-multiplexer key wins if both it
/// and the historical migration alias are present. Keeping this precedence
/// deterministic avoids file-order sensitivity during startup config merge.
#[test]
fn canonical_nested_multiplexer_key_overrides_legacy_alias() {
    let values = extract_config_values(
        ConfigFormat::Toml,
        "[terminal]\nnested_multiplexer = \"disabled\"\nnested_muxxer = \"auto\"\n",
    );

    assert_eq!(
        values.get("terminal.nested_multiplexer"),
        Some(&"disabled".to_string())
    );
}

/// Verifies that an older primary config document is upgraded to the current
/// schema by removing deleted keys, normalizing renamed keys, and backfilling
/// current defaults. This protects daemon startup from rejecting legacy user
/// files before the migration path has a chance to repair them.
#[test]
fn migrates_legacy_primary_config_to_current_schema() {
    let legacy = r#"
version = 1

[terminal]
nested_muxxer = "disabled"

[session]
default_command = "vim"
detach_behavior = "exit"
reattach_behavior = "new-session"
empty_session_behavior = "close"
restore_strategy = "snapshot-first"

[shell]
path = "/bin/bash"
login = true
interactive = false
integration = false
integration_mode = "active"
default_working_directory = "/tmp"
tool_discovery = false
tool_cache = false
fallback_behavior = "error"

[layout]
default = "even-horizontal"
resize_policy = "absolute"
close_policy = "preserve"
min_pane_columns = 1
min_pane_rows = 1

[history]
search_mode = "regex"

[memory]
storage = "sqlite"
database_path = "memory.sqlite"
max_records = 1
max_bytes = 2
max_injected_records = 3
max_injected_bytes = 4
candidate_limit = 5
archive_before_prune = false

[issues]
storage = "sqlite"

[message_protocol]
enabled = false
endpoint = "remote"
retention_messages = 1
retention_bytes = 2
allow_remote_bridges = true

[control]
endpoint = "tcp"
socket_path = "control.sock"
tcp_bind = "127.0.0.1:1234"
tcp_enabled = true
auth_token_file = "token"
observer_policy = "open"

[snapshots]
enabled = false
path = "snapshots"
on_detach = true
on_interval_seconds = 60
on_agent_turn = true
retention_count = 1

[audit]
redact_secrets = false

[frames.pane]
visible_fields = ["pane.index", "agent.auto_reasoning", "agent.model"]
[agents]
prompt_profile = "legacy"
default_agent_role = "worker"
auto_reasoning = true
auto_compact = true
auto_compact_threshold = 0.5
implementation_pressure_after_shell_actions = 8
[personalities.careful]
auto_reasoning_enabled = true
"#;

    let plan = migrate_config_text(ConfigFormat::Toml, legacy).unwrap();

    assert_eq!(plan.from_version, 1);
    assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
    assert!(plan.changed);
    assert!(plan.text.contains("version = 17"));
    assert!(plan.text.contains("emoji_width = \"wide\""));
    assert!(!plan.text.contains("detach_behavior"));
    assert!(!plan.text.contains("integration_mode"));
    assert!(!plan.text.contains("search_mode"));
    assert!(!plan.text.contains("max_injected_records"));
    assert!(!plan.text.contains("prompt_profile"));
    assert!(!plan.text.contains("message_protocol"));
    assert!(!plan.text.contains("tcp_bind"));
    assert!(!plan.text.contains("on_agent_turn"));
    assert!(!plan.text.contains("redact_secrets"));
    assert!(
        plan.text
            .contains("provider_refresh_leeway_seconds = 86400")
    );
    assert!(
        plan.text
            .contains("implementation_pressure_after_shell_actions = 3")
    );
    assert!(plan.text.contains("loop_limit = 8"));
    assert!(plan.text.contains("context_window_tokens = 1000000"));
    assert!(plan.text.contains("nested_multiplexer = \"disabled\""));
    assert!(!plan.text.contains("nested_muxxer"));
    assert!(plan.text.contains("routing = true"));
    assert!(plan.text.contains("routing_enabled = true"));
    assert!(plan.text.contains("\"agent.routing\""));
    assert!(plan.text.contains("\"agent.thinking\""));
    assert!(!plan.text.contains("auto_reasoning"));
    assert!(!plan.text.contains("agent.auto_reasoning"));
    assert!(!plan.text.contains("auto_compact"));
    assert!(!plan.text.contains("auto_compact_threshold"));
    assert!(!plan.text.contains("default_command"));
    assert!(!plan.text.contains("path = \"/bin/bash\""));
    assert!(plan.text.contains("\"agent.preset\""));
    assert!(plan.text.contains("[model_presets.deepseek]"));
    assert!(plan.text.contains("[model_presets.openai]"));

    let validation = validate_config_text(ConfigFormat::Toml, &plan.text, ConfigScope::Primary);
    assert!(validation.valid, "{:?}", validation.diagnostics);
}

/// Verifies that the schema v14 migration removes config fields that were
/// accepted by earlier schemas but had no meaningful runtime behavior. This
/// protects startup for legacy primary configs while keeping the current schema
/// free of auth-store selector fields and model-profile compatibility aliases.
#[test]
fn migrates_v13_dead_config_fields_to_current_schema() {
    let legacy = r#"
version = 13

[auth]
auth_file = "custom-auth.toml"
credential_store = "file"
default_profile = "legacy"
provider_refresh_leeway_seconds = 3600

[model_profiles.default]
provider = "openai"
model = "gpt-5.2"
privacy = "legacy-private"
privacy_tier = "standard"
residency = "global"
approval = "legacy-approval"
approval_policy = "ask"

[model_profiles.fast]
provider = "openai"
model = "gpt-5-mini"
privacy = "legacy-fast"
approval = "legacy-fast-approval"
"#;

    let plan = migrate_config_text(ConfigFormat::Toml, legacy).unwrap();
    let values = extract_config_values(ConfigFormat::Toml, &plan.text);

    assert_eq!(plan.from_version, 13);
    assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
    assert!(plan.changed);
    assert_eq!(values.get("version"), Some(&"17".to_string()));
    assert_eq!(
        values.get("auth.provider_refresh_leeway_seconds"),
        Some(&"3600".to_string())
    );
    assert!(!values.contains_key("auth.auth_file"));
    assert!(!values.contains_key("auth.credential_store"));
    assert!(!values.contains_key("auth.default_profile"));
    assert!(!values.contains_key("model_profiles.default.privacy"));
    assert!(!values.contains_key("model_profiles.default.approval"));
    assert!(!values.contains_key("model_profiles.fast.privacy"));
    assert!(!values.contains_key("model_profiles.fast.approval"));
    assert_eq!(
        values.get("model_profiles.default.privacy_tier"),
        Some(&"standard".to_string())
    );
    assert_eq!(
        values.get("model_profiles.default.approval_policy"),
        Some(&"ask".to_string())
    );

    let validation = validate_config_text(ConfigFormat::Toml, &plan.text, ConfigScope::Primary);
    assert!(validation.valid, "{:?}", validation.diagnostics);
}

/// Verifies that current-schema configs reject fields removed in schema v14
/// instead of continuing to accept inert compatibility settings. This keeps
/// primary configs and project overlays aligned with the reduced live surface.
#[test]
fn rejects_v14_dead_config_fields() {
    let invalid_auth_file = validate_config_text(
        ConfigFormat::Toml,
        "[auth]\nauth_file = \"custom-auth.toml\"\n",
        ConfigScope::Primary,
    );
    assert!(!invalid_auth_file.valid);
    assert!(invalid_auth_file.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "auth.auth_file"
            && diagnostic.message == "unknown auth configuration key"
    }));

    let invalid_credential_store = validate_config_text(
        ConfigFormat::Toml,
        "[auth]\ncredential_store = \"file\"\n",
        ConfigScope::Primary,
    );
    assert!(!invalid_credential_store.valid);
    assert!(
        invalid_credential_store
            .diagnostics
            .iter()
            .any(|diagnostic| {
                diagnostic.path == "auth.credential_store"
                    && diagnostic.message == "unknown auth configuration key"
            })
    );

    let invalid_default_profile = validate_config_text(
        ConfigFormat::Toml,
        "[auth]\ndefault_profile = \"legacy\"\n",
        ConfigScope::Primary,
    );
    assert!(!invalid_default_profile.valid);
    assert!(
        invalid_default_profile
            .diagnostics
            .iter()
            .any(|diagnostic| {
                diagnostic.path == "auth.default_profile"
                    && diagnostic.message == "unknown auth configuration key"
            })
    );

    let invalid_privacy_alias = validate_config_text(
        ConfigFormat::Toml,
        "[model_profiles.default]\nprivacy = \"legacy-private\"\n",
        ConfigScope::Primary,
    );
    assert!(!invalid_privacy_alias.valid);
    assert!(invalid_privacy_alias.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "model_profiles.default.privacy"
            && diagnostic.message == "unknown model profile configuration key"
    }));

    let invalid_approval_alias = validate_config_text(
        ConfigFormat::Toml,
        "[model_profiles.default]\napproval = \"legacy-approval\"\n",
        ConfigScope::Primary,
    );
    assert!(!invalid_approval_alias.valid);
    assert!(invalid_approval_alias.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "model_profiles.default.approval"
            && diagnostic.message == "unknown model profile configuration key"
    }));
}

/// Verifies that non-TOML primary config formats follow the same schema
/// migration contract as TOML: renamed keys are canonicalized, deleted keys are
/// removed, and current defaults are backfilled before validation. This keeps
/// alternate supported config formats from becoming launch-only edge cases.
#[test]
fn migrates_json_primary_config_to_current_schema() {
    let legacy = r#"{
  "version": 1,
  "terminal": {
    "nested_muxxer": "disabled"
  },
  "shell": {
    "command": "zsh"
  },
  "agents": {
    "auto_compact": true,
    "auto_compact_threshold": 0.5,
    "implementation_pressure_after_shell_actions": 8
  }
}"#;

    let plan = migrate_config_text(ConfigFormat::Json, legacy).unwrap();
    let values = extract_config_values(ConfigFormat::Json, &plan.text);
    assert_eq!(values.get("version"), Some(&"17".to_string()));
    assert_eq!(
        values.get("terminal.emoji_width"),
        Some(&"wide".to_string())
    );
    assert_eq!(
        values.get("auth.provider_refresh_leeway_seconds"),
        Some(&"86400".to_string())
    );
    assert_eq!(
        values.get("agents.implementation_pressure_after_shell_actions"),
        Some(&"3".to_string())
    );
    assert_eq!(values.get("agents.loop_limit"), Some(&"8".to_string()));
    assert!(!values.contains_key("agents.auto_compact"));
    assert!(!values.contains_key("agents.auto_compact_threshold"));
    assert_eq!(
        values.get("terminal.nested_multiplexer"),
        Some(&"disabled".to_string())
    );
    assert!(!values.contains_key("terminal.nested_muxxer"));
    assert!(!values.contains_key("shell.command"));
    assert_eq!(
        values.get("model_presets.deepseek.default_model_profile"),
        Some(&"deepseek-fast".to_string())
    );
    assert_eq!(
        values.get("model_profiles.deepseek-fast.context_window_tokens"),
        Some(&"1000000".to_string())
    );
    let migrated_json: serde_json::Value = serde_json::from_str(&plan.text).unwrap();
    let pane_fields = migrated_json["frames"]["pane"]["visible_fields"]
        .as_array()
        .unwrap();
    let reasoning_index = pane_fields
        .iter()
        .position(|value| value.as_str() == Some("agent.reasoning"))
        .unwrap();
    let thinking_index = pane_fields
        .iter()
        .position(|value| value.as_str() == Some("agent.thinking"))
        .unwrap();
    assert_eq!(thinking_index, reasoning_index + 1);

    let validation = validate_config_text(ConfigFormat::Json, &plan.text, ConfigScope::Primary);
    assert!(validation.valid, "{:?}", validation.diagnostics);
}

/// Verifies that schema v7 repairs only the stale built-in DeepSeek V4 context
/// defaults. Generated v6 configs carried an older half-megatoken estimate, but
/// user-defined profiles and explicitly customized built-in profiles must keep
/// their own context budgets.
#[test]
fn migrates_deepseek_v4_context_defaults_to_current_schema() {
    let legacy = r#"
version = 6

[model_profiles.deepseek-default]
provider = "deepseek"
model = "deepseek-v4-pro"
context_window_tokens = 524288

[model_profiles.deepseek-fast]
provider = "deepseek"
model = "deepseek-v4-flash"
context_window_tokens = 640000

[model_profiles.custom-deepseek]
provider = "deepseek"
model = "deepseek-v4-pro"
context_window_tokens = 524288
"#;

    let plan = migrate_config_text(ConfigFormat::Toml, legacy).unwrap();
    let values = extract_config_values(ConfigFormat::Toml, &plan.text);

    assert_eq!(plan.from_version, 6);
    assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
    assert_eq!(values.get("version"), Some(&"17".to_string()));
    assert_eq!(
        values.get("terminal.emoji_width"),
        Some(&"wide".to_string())
    );
    assert_eq!(
        values.get("auth.provider_refresh_leeway_seconds"),
        Some(&"86400".to_string())
    );
    assert_eq!(
        values.get("model_profiles.deepseek-default.context_window_tokens"),
        Some(&"1000000".to_string())
    );
    assert_eq!(
        values.get("model_profiles.deepseek-fast.context_window_tokens"),
        Some(&"640000".to_string())
    );
    assert_eq!(
        values.get("model_profiles.custom-deepseek.context_window_tokens"),
        Some(&"524288".to_string())
    );
}

/// Verifies the DeepSeek context-window migration also applies to
/// JSON-compatible primary config formats. This keeps TOML and non-TOML
/// generated v6 configs from diverging when they are upgraded.
#[test]
fn migrates_json_deepseek_v4_context_defaults_to_current_schema() {
    let legacy = r#"{
  "version": 6,
  "model_profiles": {
    "deepseek-default": {
      "provider": "deepseek",
      "model": "deepseek-v4-pro",
      "context_window_tokens": 524288
    },
    "deepseek-fast": {
      "provider": "deepseek",
      "model": "deepseek-v4-flash"
    }
  }
}"#;

    let plan = migrate_config_text(ConfigFormat::Json, legacy).unwrap();
    let values = extract_config_values(ConfigFormat::Json, &plan.text);

    assert_eq!(plan.from_version, 6);
    assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
    assert_eq!(values.get("version"), Some(&"17".to_string()));
    assert_eq!(
        values.get("terminal.emoji_width"),
        Some(&"wide".to_string())
    );
    assert_eq!(
        values.get("auth.provider_refresh_leeway_seconds"),
        Some(&"86400".to_string())
    );
    assert_eq!(
        values.get("model_profiles.deepseek-default.context_window_tokens"),
        Some(&"1000000".to_string())
    );
    assert_eq!(
        values.get("model_profiles.deepseek-fast.context_window_tokens"),
        Some(&"1000000".to_string())
    );
}

/// Verifies that the v10 terminal emoji-width migration backfills the new
/// default without overriding an explicit user-selected narrow fallback. This
/// keeps existing users on the default wide policy while preserving deliberate
/// terminal/font compatibility choices.
#[test]
fn migrates_terminal_emoji_width_default_to_current_schema() {
    let missing = migrate_config_text(
        ConfigFormat::Toml,
        "version = 9\n[terminal]\nterm = \"screen-256color\"\n",
    )
    .unwrap();
    let missing_values = extract_config_values(ConfigFormat::Toml, &missing.text);
    assert_eq!(missing_values.get("version"), Some(&"17".to_string()));
    assert_eq!(
        missing_values.get("terminal.emoji_width"),
        Some(&"wide".to_string())
    );
    assert_eq!(
        missing_values.get("agents.local_action_executor"),
        Some(&"pane_shell".to_string())
    );

    let explicit = migrate_config_text(
        ConfigFormat::Toml,
        "version = 9\n[terminal]\nemoji_width = \"narrow\"\n",
    )
    .unwrap();
    let explicit_values = extract_config_values(ConfigFormat::Toml, &explicit.text);
    assert_eq!(explicit_values.get("version"), Some(&"17".to_string()));
    assert_eq!(
        explicit_values.get("terminal.emoji_width"),
        Some(&"narrow".to_string())
    );
}

/// Verifies the v17 local-action executor migration backfills the conservative
/// pane-shell default without overriding an explicit native setting.
///
/// The executor setting changes how accepted local MAAP actions reach the host
/// filesystem or process table, so legacy primary configs must migrate to the
/// existing pane-shell behavior unless the user has already made an explicit
/// selection.
#[test]
fn migrates_local_action_executor_default_to_current_schema() {
    let missing = migrate_config_text(
        ConfigFormat::Toml,
        "version = 16\n[agents]\nrouting = true\n",
    )
    .unwrap();
    let missing_values = extract_config_values(ConfigFormat::Toml, &missing.text);
    assert_eq!(missing_values.get("version"), Some(&"17".to_string()));
    assert_eq!(
        missing_values.get("agents.local_action_executor"),
        Some(&"pane_shell".to_string())
    );

    let explicit = migrate_config_text(
        ConfigFormat::Toml,
        "version = 16\n[agents]\nlocal_action_executor = \"native\"\n",
    )
    .unwrap();
    let explicit_values = extract_config_values(ConfigFormat::Toml, &explicit.text);
    assert_eq!(explicit_values.get("version"), Some(&"17".to_string()));
    assert_eq!(
        explicit_values.get("agents.local_action_executor"),
        Some(&"native".to_string())
    );
}

/// Verifies that config validation refuses documents written for a newer
/// schema version than the running binary understands. This prevents older
/// binaries from silently interpreting keys whose migration or meaning belongs
/// to a future release.
#[test]
fn rejects_newer_config_schema_version() {
    let validation =
        validate_config_text(ConfigFormat::Toml, "version = 999\n", ConfigScope::Primary);

    assert!(!validation.valid);
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "version"
            && diagnostic
                .message
                .contains("newer than this mez binary supports")
    }));
}

/// Verifies project overlays must declare the current schema version.
///
/// Primary configs are migrated before validation, but project overlays are not
/// migrated. Requiring the exact current version keeps stale overlay semantics
/// from loading as if they already matched the running binary.
#[test]
fn rejects_missing_or_old_project_overlay_schema_version() {
    let missing = validate_config_text(
        ConfigFormat::Toml,
        "[providers]\n",
        ConfigScope::ProjectOverlay,
    );
    let old = validate_config_text(
        ConfigFormat::Toml,
        "version = 1\n[providers]\n",
        ConfigScope::ProjectOverlay,
    );
    let current = validate_config_text(
        ConfigFormat::Toml,
        &format!("version = {CURRENT_CONFIG_SCHEMA_VERSION}\n[providers]\n"),
        ConfigScope::ProjectOverlay,
    );

    assert!(!missing.valid);
    assert!(!old.valid);
    assert!(missing.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "version" && diagnostic.message.contains("project overlay")
    }));
    assert!(old.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "version" && diagnostic.message.contains("project overlay")
    }));
    assert!(current.valid, "{:?}", current.diagnostics);
}

/// Verifies that custom subagent profiles are part of the baseline config
/// schema, including nested shell environment overrides, while unknown profile
/// keys remain rejected.
#[test]
fn validates_custom_subagent_profile_schema() {
    let valid = validate_config_text(
        ConfigFormat::Toml,
        "[subagents.reviewer]\nname = \"Reviewer\"\ndescription = \"Reviews changes\"\ndeveloper_instructions = \"Focus on correctness.\"\nmodel_profile = \"default\"\npermission_preset = \"read-only\"\nmcp_servers = [\"filesystem\"]\ndefault_cooperation_mode = \"explore-only\"\ndefault_read_scopes = [\"src\"]\ndefault_write_scopes = []\n[subagents.reviewer.shell_env]\nREVIEW_MODE = \"strict\"\n",
        ConfigScope::Primary,
    );

    assert!(valid.valid, "{:?}", valid.diagnostics);

    let invalid = validate_config_text(
        ConfigFormat::Toml,
        "[subagents.reviewer]\nunknown = true\n",
        ConfigScope::Primary,
    );

    assert!(!invalid.valid);
    assert!(invalid.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "subagents.reviewer.unknown"
            && diagnostic.message == "unknown subagent profile configuration key"
    }));
}

/// Verifies that user-defined personality profiles are part of the baseline
/// config schema while unknown profile keys remain rejected.
///
/// Personality profiles affect provider prompt construction and pane-local
/// agent preferences, so their table shape must be validated before runtime
/// config application stores those values in live agent state.
#[test]
fn validates_custom_personality_profile_schema() {
    let valid = validate_config_text(
        ConfigFormat::Toml,
        "[agents]\ncustom_system_prompt = \"Follow local conventions.\"\ndefault_personality = \"careful\"\n[personalities.careful]\nname = \"Careful\"\nsystem_prompt = \"Be precise.\"\nresponse_style = \"terse\"\nmodel_profile = \"default\"\nplanning_enabled = true\nrouting_enabled = true\n",
        ConfigScope::Primary,
    );

    assert!(valid.valid, "{:?}", valid.diagnostics);

    let invalid = validate_config_text(
        ConfigFormat::Toml,
        "[personalities.careful]\nunknown = true\n",
        ConfigScope::Primary,
    );

    assert!(!invalid.valid);
    assert!(invalid.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "personalities.careful.unknown"
            && diagnostic.message == "unknown personality profile configuration key"
    }));
}

/// Verifies that named model profiles are accepted as a first-class
/// configuration table, including nested non-secret provider options, while
/// unknown model-profile keys are rejected.
#[test]
fn validates_named_model_profile_schema() {
    let valid = validate_config_text(
        ConfigFormat::Toml,
        "[model_profiles.default]\nprovider = \"openai\"\nmodel = \"gpt-5.2\"\nreasoning_profile = \"medium\"\nlatency_preference = \"default\"\nmultimodal_required = false\ncontext_window_tokens = 128000\nmax_output_tokens = 12000\nsafety_tier = \"high\"\nprivacy_tier = \"standard\"\nresidency = \"global\"\napproval_policy = \"ask\"\nfallback_profiles = [\"fast\"]\n[model_profiles.default.provider_options]\nreasoning_effort = \"medium\"\n",
        ConfigScope::Primary,
    );

    assert!(valid.valid, "{:?}", valid.diagnostics);

    let invalid = validate_config_text(
        ConfigFormat::Toml,
        "[model_profiles.default]\nunknown = true\n",
        ConfigScope::Primary,
    );

    assert!(!invalid.valid);
    assert!(invalid.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "model_profiles.default.unknown"
            && diagnostic.message == "unknown model profile configuration key"
    }));

    let invalid_approval_policy = validate_config_text(
        ConfigFormat::Toml,
        "[model_profiles.default]\napproval_policy = \"on-request\"\n",
        ConfigScope::Primary,
    );

    assert!(!invalid_approval_policy.valid);
    assert!(
        invalid_approval_policy
            .diagnostics
            .iter()
            .any(|diagnostic| {
                diagnostic.path == "model_profiles.default.approval_policy"
                    && diagnostic.message
                        == "unsupported approval policy; use ask, auto-allow, or full-access"
            })
    );

    let invalid_max_output_tokens = validate_config_text(
        ConfigFormat::Toml,
        "[model_profiles.default]\nmax_output_tokens = 0\n",
        ConfigScope::Primary,
    );

    assert!(!invalid_max_output_tokens.valid);
    assert!(
        invalid_max_output_tokens
            .diagnostics
            .iter()
            .any(|diagnostic| {
                diagnostic.path == "model_profiles.default.max_output_tokens"
                    && diagnostic.message
                        == "model_profiles.default.max_output_tokens must be a positive integer"
            })
    );
}

/// Verifies that implementation-exposed audit config keys remain listed in the
/// normative Section 8.2 configuration table.
#[test]
fn specification_lists_all_audit_schema_keys() {
    let specification = include_str!("../../SPEC.md");

    for key in super::schema::AUDIT_KEYS {
        assert!(
            specification.contains(&format!("`{key}`")),
            "SPEC.md must list audit.{key}"
        );
    }
}

/// Verifies rejects invalid frame display values.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_invalid_frame_display_values() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[frames.window]\nenabled = \"yes\"\nposition = \"middle\"\nstyle = \"blink\"\n[frames.pane]\nposition = \"side\"\nstyle = \"loud\"\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "frames.window.enabled"
            && diagnostic.message == "frames.window.enabled must be true or false"
    }));
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "frames.window.position"
            && diagnostic.message == "frames.window.position must be top, bottom, or border"
    }));
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "frames.window.style"
            && diagnostic.message
                == "frames.window.style must be default, bold, underline, inverse, or reverse"
    }));
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "frames.pane.position"
            && diagnostic.message == "frames.pane.position must be top, bottom, or border"
    }));
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "frames.pane.style"
            && diagnostic.message
                == "frames.pane.style must be default, bold, underline, inverse, or reverse"
    }));
}

/// Verifies allows declared dynamic config maps.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn allows_declared_dynamic_config_maps() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[keys.command_bindings]\nrefresh = \"refresh-client\"\n[providers.openai.options]\nreasoning_effort = \"medium\"\n[hooks.notify.env]\nLOG_LEVEL = \"debug\"\n[extensions.example]\nenabled = true\n",
        ConfigScope::Primary,
    );

    assert!(validation.valid, "{:?}", validation.diagnostics);
}

/// Verifies rejects forbidden session default command.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_forbidden_session_default_command() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[session]\ndefault_command = \"vim\"\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == "session.default_command")
    );
}

/// Verifies rejects shell path override.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_shell_path_override() {
    let validation = validate_config_text(
        ConfigFormat::Yaml,
        "shell:\n  path: /bin/bash\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == "shell.path")
    );
}

/// Verifies rejects auth secrets in json config.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_auth_secrets_in_json_config() {
    let validation = validate_config_text(
        ConfigFormat::Json,
        r#"{ "auth": { "access_token": "secret" } }"#,
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert_eq!(validation.diagnostics[0].path, "auth.access_token");
}

/// Verifies rejects project overlay secret material.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_project_overlay_secret_material() {
    let validation = validate_config_text(
        ConfigFormat::Yaml,
        "providers:\n  local:\n    token: secret\n",
        ConfigScope::ProjectOverlay,
    );

    assert!(!validation.valid);
    assert_eq!(validation.diagnostics[0].path, "providers.local.token");
}

/// Verifies validates known mcp server keys.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn validates_known_mcp_server_keys() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[mcp_servers.fs]\ncommand = \"mcp-fs\"\nargs = [\"--root\", \".\"]\nenv_vars = [\"MCP_TOKEN\"]\ncwd = \".\"\nenabled_tools = [\"read_file\"]\ndisabled_tools = [\"delete_file\"]\nstartup_timeout_sec = 10\ntool_timeout_sec = 60\nenabled = true\napproval = \"prompt\"\n[mcp_servers.fs.env]\nLOG_LEVEL = \"debug\"\n[mcp_servers.fs.http_headers]\nX_Client = \"mez\"\n[mcp_servers.fs.tool_approvals]\nread_file = \"prompt\"\n[mcp_servers.fs.external_capability]\npurpose = \"File reads and project tree inspection\"\nusage_instructions = \"Use read_file only when the task needs file contents.\"\n",
        ConfigScope::Primary,
    );

    assert!(validation.valid, "{:?}", validation.diagnostics);
}

/// Verifies rejects unknown mcp server keys.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_unknown_mcp_server_keys() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[mcp_servers.fs]\nmagic = true\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert_eq!(validation.diagnostics[0].path, "mcp_servers.fs.magic");
}

/// Verifies rejects inline mcp secret material.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_inline_mcp_secret_material() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[mcp_servers.fs.env]\nAPI_TOKEN = \"secret\"\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert_eq!(
        validation.diagnostics[0].path,
        "mcp_servers.fs.env.API_TOKEN"
    );
}

/// Verifies rejects unsupported permission modes.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_unsupported_permission_modes() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[permissions]\napproval_policy = \"on-failure\"\npreset = \"unsupported\"\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == "permissions.approval_policy")
    );
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == "permissions.preset")
    );
}

/// Verifies that configuration cannot directly enter the explicit approval
/// bypass state. The specification requires bypass activation to go through an
/// obvious user-selected flow with primary authority and audit visibility, so
/// config validation must still allow the documented default `false` value
/// while rejecting an enabling value before it reaches the runtime policy.
#[test]
fn rejects_config_enabled_approval_bypass_mode() {
    let enabled = validate_config_text(
        ConfigFormat::Toml,
        "[permissions]\nbypass_mode = true\n",
        ConfigScope::Primary,
    );
    let disabled = validate_config_text(
        ConfigFormat::Toml,
        "[permissions]\nbypass_mode = false\n",
        ConfigScope::Primary,
    );

    assert!(!enabled.valid);
    assert_eq!(enabled.diagnostics[0].path, "permissions.bypass_mode");
    assert!(
        enabled.diagnostics[0]
            .message
            .contains("cannot be enabled from configuration")
    );
    assert!(disabled.valid, "{:?}", disabled.diagnostics);
}

/// Verifies rejects invalid history limit values.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_invalid_history_limit_values() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[history]\nlines = 0\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert_eq!(validation.diagnostics[0].path, "history.lines");
    assert!(
        validation.diagnostics[0]
            .message
            .contains("positive integer")
    );

    let rotation_validation = validate_config_text(
        ConfigFormat::Toml,
        "[history]\nrotate_lines = 0\n",
        ConfigScope::Primary,
    );

    assert!(!rotation_validation.valid);
    assert_eq!(
        rotation_validation.diagnostics[0].path,
        "history.rotate_lines"
    );
    assert!(
        rotation_validation.diagnostics[0]
            .message
            .contains("positive integer")
    );
}

/// Verifies rejects invalid agent concurrency values.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_invalid_agent_concurrency_values() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[agents]\nmax_concurrent_agents = 0\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert_eq!(
        validation.diagnostics[0].path,
        "agents.max_concurrent_agents"
    );
    assert!(
        validation.diagnostics[0]
            .message
            .contains("positive integer")
    );
}

/// Verifies rejects invalid action-failure retry limits.
///
/// Retry limits must be positive so model-correctable action failures have a
/// clear bounded repair policy instead of an ambiguous zero-attempt state.
#[test]
fn rejects_invalid_action_failure_retry_limit_values() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[agents]\naction_failure_retry_limit = 0\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert_eq!(
        validation.diagnostics[0].path,
        "agents.action_failure_retry_limit"
    );
    assert!(
        validation.diagnostics[0]
            .message
            .contains("positive integer")
    );
}

/// Verifies rejects invalid implementation-pressure shell-action thresholds.
///
/// A zero threshold would make every turn carry pressure before any shell
/// evidence exists, so validation requires the advisory trigger to be a
/// positive integer like other agent loop-control settings.
#[test]
fn rejects_invalid_implementation_pressure_after_shell_actions_values() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[agents]\nimplementation_pressure_after_shell_actions = 0\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert_eq!(
        validation.diagnostics[0].path,
        "agents.implementation_pressure_after_shell_actions"
    );
    assert!(
        validation.diagnostics[0]
            .message
            .contains("positive integer")
    );
}

/// Verifies rejects invalid agent loop iteration limits.
///
/// A zero loop limit would make `/loop` unable to perform even the initial work
/// iteration while still accepting a command whose purpose is bounded automatic
/// continuation, so validation requires a positive integer.
#[test]
fn rejects_invalid_agent_loop_limit_values() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[agents]\nloop_limit = 0\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert_eq!(validation.diagnostics[0].path, "agents.loop_limit");
    assert!(
        validation.diagnostics[0]
            .message
            .contains("positive integer")
    );
}

/// Verifies rejects invalid compaction raw-retention percentages.
///
/// The retained raw tail is configured as a percentage of the active model
/// context budget. Zero or over-100 values would either remove the exact recent
/// tail or exceed the budget contract, so validation rejects them before
/// runtime compaction can apply the setting.
#[test]
fn rejects_invalid_compaction_raw_retention_percent_values() {
    let zero = validate_config_text(
        ConfigFormat::Toml,
        "[agents]\ncompaction_raw_retention_percent = 0\n",
        ConfigScope::Primary,
    );
    let too_large = validate_config_text(
        ConfigFormat::Toml,
        "[agents]\ncompaction_raw_retention_percent = 101\n",
        ConfigScope::Primary,
    );
    let valid = validate_config_text(
        ConfigFormat::Toml,
        "[agents]\ncompaction_raw_retention_percent = 25\n",
        ConfigScope::Primary,
    );

    assert!(!zero.valid);
    assert_eq!(
        zero.diagnostics[0].path,
        "agents.compaction_raw_retention_percent"
    );
    assert!(
        zero.diagnostics[0]
            .message
            .contains("integer from 1 to 100")
    );
    assert!(!too_large.valid);
    assert_eq!(
        too_large.diagnostics[0].path,
        "agents.compaction_raw_retention_percent"
    );
    assert!(valid.valid, "{:?}", valid.diagnostics);
}

/// Verifies rejects invalid root subagent width values.
///
/// The root delegation limit bounds how many direct helpers a pane agent can
/// keep active. A zero value would make every configured pane agent unable to
/// delegate while still advertising subagent capability, so validation must
/// reject it before runtime policy is applied.
#[test]
fn rejects_invalid_root_subagent_width_values() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[agents]\nmax_root_subagents = 0\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert_eq!(validation.diagnostics[0].path, "agents.max_root_subagents");
    assert!(
        validation.diagnostics[0]
            .message
            .contains("positive integer")
    );
}

/// Verifies rejects invalid nested subagent width values.
///
/// Child subagents can delegate further only within a configured branching
/// factor. Zero would make the delegation contract depend on parent depth in a
/// surprising way, so the static validator keeps the runtime policy strictly
/// positive and diagnosable.
#[test]
fn rejects_invalid_child_subagent_width_values() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[agents]\nmax_subagents_per_subagent = 0\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert_eq!(
        validation.diagnostics[0].path,
        "agents.max_subagents_per_subagent"
    );
    assert!(
        validation.diagnostics[0]
            .message
            .contains("positive integer")
    );
}

/// Verifies rejects invalid subagent depth values.
///
/// Depth controls whether a spawned child can create another generation of
/// helpers. A positive value keeps the root-agent and child-agent cases
/// distinct while preventing accidental recursive delegation loops.
#[test]
fn rejects_invalid_subagent_depth_values() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[agents]\nmax_depth = 0\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert_eq!(validation.diagnostics[0].path, "agents.max_depth");
    assert!(
        validation.diagnostics[0]
            .message
            .contains("positive integer")
    );
}

/// Verifies rejects invalid subagent pane bucket values.
///
/// Subagent windows use a positive pane-capacity limit before a new background
/// window is created. Zero would strand placement policy without a usable
/// bucket, so the static validator must reject it at config load time.
#[test]
fn rejects_invalid_subagent_window_capacity_values() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[agents]\nmax_subagent_panes_per_window = 0\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert_eq!(
        validation.diagnostics[0].path,
        "agents.max_subagent_panes_per_window"
    );
    assert!(
        validation.diagnostics[0]
            .message
            .contains("positive integer")
    );
}

/// Verifies rejects unsupported subagent wait policy values.
///
/// Parent/subagent coordination changes scheduler semantics, so the static
/// validator must reject typos before runtime config application can fall back
/// to an unintended default.
#[test]
fn rejects_invalid_subagent_wait_policy_values() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[agents]\nsubagent_wait_policy = \"background\"\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert_eq!(
        validation.diagnostics[0].path,
        "agents.subagent_wait_policy"
    );
    assert!(
        validation.diagnostics[0]
            .message
            .contains("unsupported subagent wait policy")
    );
}

/// Verifies rejects unsupported local action executor values.
///
/// The executor setting controls whether accepted local MAAP actions are sent
/// through the pane shell or through a strict native transport. Validation must
/// reject typos so local file and process effects cannot silently use the wrong
/// execution boundary.
#[test]
fn rejects_invalid_local_action_executor_values() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[agents]\nlocal_action_executor = \"host\"\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert_eq!(
        validation.diagnostics[0].path,
        "agents.local_action_executor"
    );
    assert!(
        validation.diagnostics[0]
            .message
            .contains("unsupported local action executor")
    );
}

/// Verifies rejects invalid terminal term and profile values.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_invalid_terminal_term_and_profile_values() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[terminal]\nterm = \"\"\nprofile = \"ansi\"\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == "terminal.term")
    );
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == "terminal.profile")
    );
}

/// Verifies rejects invalid terminal presentation values.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_invalid_terminal_presentation_values() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[terminal]\ncursor_style = \"beam\"\ncursor_blink = \"sometimes\"\nemoji_width = \"auto\"\nreduced_motion = \"sometimes\"\ncursor_blink_interval_ms = 0\nresize_debounce_ms = 0\nrender_rate_limit_fps = -1\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "terminal.cursor_style"
            && diagnostic.message == "terminal.cursor_style must be block, underline, or bar"
    }));
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "terminal.cursor_blink"
            && diagnostic.message == "terminal.cursor_blink must be true or false"
    }));
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "terminal.reduced_motion"
            && diagnostic.message == "terminal.reduced_motion must be true or false"
    }));
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "terminal.emoji_width"
            && diagnostic.message == "terminal.emoji_width must be wide or narrow"
    }));
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "terminal.cursor_blink_interval_ms"
            && diagnostic.message == "terminal.cursor_blink_interval_ms must be a positive integer"
    }));
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "terminal.resize_debounce_ms"
            && diagnostic.message == "terminal.resize_debounce_ms must be a positive integer"
    }));
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "terminal.render_rate_limit_fps"
            && diagnostic.message == "terminal.render_rate_limit_fps must be a non-negative integer"
    }));
}

/// Verifies rejects host terminal identity in default profile.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_host_terminal_identity_in_default_profile() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[terminal]\nterm = \"xterm-256color\"\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "terminal.term" && diagnostic.message.contains("host terminal")
    }));
}

/// Verifies validates command rule schema in toml array tables.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn validates_command_rule_schema_in_toml_array_tables() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[[permissions.command_rules]]\npattern = [\"cargo\", \"test\"]\ndecision = \"allow\"\nscope = \"user\"\nmatch = \"prefix\"\njustification = \"test runner\"\n",
        ConfigScope::Primary,
    );

    assert!(validation.valid, "{:?}", validation.diagnostics);
}

/// Verifies command rule match examples must match rule.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn command_rule_match_examples_must_match_rule() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[[permissions.command_rules]]\npattern = [\"cargo\", \"test\"]\ndecision = \"allow\"\nscope = \"user\"\nmatch = \"prefix\"\nmatch_examples = [\"cargo build\"]\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == "permissions.command_rules.match_examples")
    );
}

/// Verifies command rule not match examples must not match rule.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn command_rule_not_match_examples_must_not_match_rule() {
    let validation = validate_config_text(
        ConfigFormat::Json,
        r#"{"permissions":{"command_rules":[{"pattern":["cargo","test"],"decision":"allow","scope":"user","match":"prefix","not_match_examples":["cargo test --all"]}]}}"#,
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == "permissions.command_rules.not_match_examples")
    );
}

/// Verifies exact sha256 command rule examples are validated.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn exact_sha256_command_rule_examples_are_validated() {
    let example = "printf 'ok\\n'";
    let example_toml = example.replace('\\', "\\\\");
    let digest = exact_command_sha256("unix-like", &normalize_exact_command_text(example, false));
    let valid = validate_config_text(
        ConfigFormat::Toml,
        &format!(
            "[[permissions.command_rules]]\npattern = [\"digest\"]\ndecision = \"allow\"\nscope = \"session\"\nmatch = \"exact_sha256\"\nexact_sha256 = \"{digest}\"\nshell_classification = \"unix-like\"\nmatch_examples = [\"{example_toml}\"]\nnot_match_examples = [\"printf other\"]\n"
        ),
        ConfigScope::Primary,
    );

    assert!(valid.valid, "{:?}", valid.diagnostics);

    let invalid = validate_config_text(
        ConfigFormat::Toml,
        &format!(
            "[[permissions.command_rules]]\npattern = [\"digest\"]\ndecision = \"allow\"\nscope = \"session\"\nmatch = \"exact_sha256\"\nexact_sha256 = \"{digest}\"\nshell_classification = \"unix-like\"\nmatch_examples = [\"printf other\"]\n"
        ),
        ConfigScope::Primary,
    );

    assert!(!invalid.valid);
}

/// Verifies rejects unknown command rule keys and values.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_unknown_command_rule_keys_and_values() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[[permissions.command_rules]]\npattern = [\"cargo\"]\ndecision = \"auto\"\nscope = \"built-in\"\nunknown = true\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == "permissions.command_rules.decision")
    );
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == "permissions.command_rules.scope")
    );
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == "permissions.command_rules.unknown")
    );
}

/// Verifies rejects invalid exact sha256 command rule metadata.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_invalid_exact_sha256_command_rule_metadata() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[[permissions.command_rules]]\npattern = [\"digest\"]\ndecision = \"allow\"\nscope = \"session\"\nmatch = \"exact_sha256\"\nexact_sha256 = \"not-a-digest\"\nshell_classification = \"bad class\"\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == "permissions.command_rules.exact_sha256")
    );
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == "permissions.command_rules.shell_classification")
    );
}

/// Verifies effective config applies layers in order with source tracking.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn effective_config_applies_layers_in_order_with_source_tracking() {
    let effective = compose_effective_config(&[
        ConfigLayer {
            name: "defaults".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[history]\nlines = 10000\n[frames.pane]\nenabled = false\n".to_string(),
        },
        ConfigLayer {
            name: "primary".to_string(),
            path: Some(PathBuf::from("/home/user/.config/mezzanine/config.toml")),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[history]\nlines = 2000\n".to_string(),
        },
        ConfigLayer {
            name: "live".to_string(),
            path: None,
            format: ConfigFormat::Json,
            scope: ConfigScope::LiveOverride,
            trusted: true,
            text: r#"{"frames":{"pane":{"enabled":true}}}"#.to_string(),
        },
    ])
    .unwrap();

    assert_eq!(effective.get("history.lines"), Some("2000"));
    assert_eq!(effective.source_for("history.lines"), Some("primary"));
    assert_eq!(effective.get("frames.pane.enabled"), Some("true"));
    assert_eq!(effective.source_for("frames.pane.enabled"), Some("live"));
    assert_eq!(
        effective.applied_layers(),
        &[
            "defaults".to_string(),
            "primary".to_string(),
            "live".to_string()
        ]
    );
}

/// Verifies untrusted project overlay is skipped with diagnostic.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn untrusted_project_overlay_is_skipped_with_diagnostic() {
    let effective = compose_effective_config(&[
        ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[history]\nlines = 10000\n".to_string(),
        },
        ConfigLayer {
            name: "project".to_string(),
            path: Some(PathBuf::from("/repo/.mezzanine/config.toml")),
            format: ConfigFormat::Toml,
            scope: ConfigScope::ProjectOverlay,
            trusted: false,
            text: "[history]\nlines = 50\n".to_string(),
        },
    ])
    .unwrap();

    assert_eq!(effective.get("history.lines"), Some("10000"));
    assert_eq!(effective.skipped_layers(), &["project".to_string()]);
    assert!(effective.diagnostics()[0].message.contains("pending trust"));
}

/// Verifies invalid layer prevents effective config.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn invalid_layer_prevents_effective_config() {
    let error = compose_effective_config(&[ConfigLayer {
        name: "bad".to_string(),
        path: None,
        format: ConfigFormat::Toml,
        scope: ConfigScope::Primary,
        trusted: true,
        text: "[session]\ndefault_command = \"vim\"\n".to_string(),
    }])
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Config);
}
