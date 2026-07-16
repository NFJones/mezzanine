//! Config mutation tests.

use super::*;

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
