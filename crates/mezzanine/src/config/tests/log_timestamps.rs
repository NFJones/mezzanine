//! Presence-preserving schema migration for default-off log decoration.

use super::*;
use crate::config::parse_config_json_value;

/// The setting is a supported boolean mutation path with a generated false
/// default. Invalid types fail validation before persistence; annotations expose
/// the same path so model-selected configuration actions cannot guess a knob.
#[test]
fn log_timestamps_boolean_schema_and_mutation_path_are_supported() {
    assert_eq!(
        extract_config_values(ConfigFormat::Toml, DEFAULT_CONFIG_TOML)
            .get("terminal.agent_log_timestamps"),
        Some(&"false".to_string())
    );
    for value in [false, true] {
        let plan = plan_config_mutation(
            ConfigFormat::Toml,
            DEFAULT_CONFIG_TOML,
            ConfigScope::Primary,
            set_boolean("terminal.agent_log_timestamps", value),
        )
        .unwrap();
        assert!(plan.validation.valid, "{:?}", plan.validation.diagnostics);
        assert_eq!(
            extract_config_values(ConfigFormat::Toml, &plan.text)
                .get("terminal.agent_log_timestamps"),
            Some(&value.to_string())
        );
    }
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[terminal]\nagent_log_timestamps = \"sometimes\"\n",
        ConfigScope::Primary,
    );
    assert!(!validation.valid);
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == "terminal.agent_log_timestamps")
    );
    assert!(
        crate::config::config_change_setting_path_description().contains("agent_log_timestamps")
    );
}

/// Schema 101 upgrades without inventing an opt-in or overwriting an authored
/// false/true value. TOML inline tables, ordinary tables, YAML and JSON preserve
/// adjacent terminal policy; repeated current-schema migration is inert.
#[test]
fn log_timestamps_migration_preserves_authored_choices_in_all_formats() {
    for (format, source, expected) in [
        (
            ConfigFormat::Toml,
            "version = 101\nterminal = { agent_log_timestamps = false, agent_wrap_column_cap = 80 }\n",
            Some(false),
        ),
        (
            ConfigFormat::Toml,
            "version = 101\n[terminal]\nagent_log_timestamps = true\nagent_wrap_column_cap = 80\n",
            Some(true),
        ),
        (
            ConfigFormat::Toml,
            "version = 101\n[terminal]\nagent_wrap_column_cap = 80\n",
            None,
        ),
        (
            ConfigFormat::Json,
            r#"{"version":101,"terminal":{"agent_log_timestamps":false,"agent_wrap_column_cap":80}}"#,
            Some(false),
        ),
        (
            ConfigFormat::Json,
            r#"{"version":101,"terminal":{"agent_log_timestamps":true,"agent_wrap_column_cap":80}}"#,
            Some(true),
        ),
        (
            ConfigFormat::Json,
            r#"{"version":101,"terminal":{"agent_wrap_column_cap":80}}"#,
            None,
        ),
        (
            ConfigFormat::Yaml,
            "version: 101\nterminal:\n  agent_log_timestamps: false\n  agent_wrap_column_cap: 80\n",
            Some(false),
        ),
        (
            ConfigFormat::Yaml,
            "version: 101\nterminal:\n  agent_log_timestamps: true\n  agent_wrap_column_cap: 80\n",
            Some(true),
        ),
        (
            ConfigFormat::Yaml,
            "version: 101\nterminal:\n  agent_wrap_column_cap: 80\n",
            None,
        ),
    ] {
        let plan = migrate_config_text(format, source).unwrap();
        assert_eq!(plan.from_version, 101);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        let root = parse_config_json_value(format, &plan.text).unwrap();
        assert_eq!(
            root.pointer("/terminal/agent_log_timestamps")
                .and_then(serde_json::Value::as_bool),
            expected
        );
        assert_eq!(
            root.pointer("/terminal/agent_wrap_column_cap")
                .and_then(serde_json::Value::as_u64),
            Some(80)
        );
        let repeated = migrate_config_text(format, &plan.text).unwrap();
        assert!(!repeated.changed);
        assert_eq!(repeated.text, plan.text);
    }
}
