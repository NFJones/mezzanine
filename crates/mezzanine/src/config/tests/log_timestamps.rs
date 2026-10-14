//! Presence-preserving schema migration for default-off log decoration.

use super::*;
use crate::config::parse_config_json_value;

/// The default remains eight cells and format-only changes never opt into
/// decoration. Custom formats must survive the ordinary mutation planner,
/// rather than merely being accepted by a formatter hidden from configuration.
#[test]
fn log_timestamp_format_default_and_custom_mutation_remain_default_off() {
    let defaults = extract_config_values(ConfigFormat::Toml, DEFAULT_CONFIG_TOML);
    assert_eq!(
        defaults.get("terminal.agent_log_timestamp_format"),
        Some(&"%H:%M:%S".to_string())
    );
    for value in ["%H:%M", "[%S-%M-%H]", "%%", "UTC %H%M%S", "literal"] {
        let plan = plan_config_mutation(
            ConfigFormat::Toml,
            DEFAULT_CONFIG_TOML,
            ConfigScope::Primary,
            set_string("terminal.agent_log_timestamp_format", value),
        )
        .unwrap();
        assert!(plan.validation.valid, "{:?}", plan.validation.diagnostics);
        let values = extract_config_values(ConfigFormat::Toml, &plan.text);
        assert_eq!(
            values.get("terminal.agent_log_timestamp_format"),
            Some(&value.to_string())
        );
        assert_eq!(
            values.get("terminal.agent_log_timestamps"),
            Some(&"false".to_string())
        );
    }
    assert!(
        crate::config::config_change_setting_path_description()
            .contains("agent_log_timestamp_format")
    );
}

/// Unsafe or variable-width formats fail with a path-specific diagnostic and
/// cannot yield a persistence plan. The immutable input document stays byte
/// identical, including authored neighboring policy and timestamp-looking text.
#[test]
fn log_timestamp_format_invalid_values_fail_before_mutation() {
    let source = "[terminal]\nagent_log_timestamps = false\nagent_wrap_column_cap = 80\n";
    for value in [
        "",
        "%",
        "%Y",
        "%f",
        "%T",
        "%c",
        "%_H",
        "%-H",
        "%3H",
        "%:z",
        "\n",
        "\t",
        "\x1b",
        "\x7f",
        "é",
        "🙂",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%",
    ] {
        let text = format!(
            "[terminal]\nagent_log_timestamp_format = {}\n",
            serde_json::to_string(value).unwrap()
        );
        let validation = validate_config_text(ConfigFormat::Toml, &text, ConfigScope::Primary);
        assert!(!validation.valid, "invalid format was accepted");
        assert!(
            validation
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.path == "terminal.agent_log_timestamp_format")
        );
        assert!(
            plan_config_mutation(
                ConfigFormat::Toml,
                source,
                ConfigScope::Primary,
                set_string("terminal.agent_log_timestamp_format", value)
            )
            .is_err()
        );
    }
    for value in [
        "123456789012345678901234567890123",
        "1234567890123456789012345678901234",
        "%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%%",
    ] {
        assert!(
            plan_config_mutation(
                ConfigFormat::Toml,
                source,
                ConfigScope::Primary,
                set_string("terminal.agent_log_timestamp_format", value)
            )
            .unwrap()
            .validation
            .valid
        );
    }
}

/// The next sequential migration is version-only: every serialized format
/// preserves authored custom formats and absence, never materializing an opt-in.
/// Repeating migration must be inert even with an inline terminal table.
#[test]
fn log_timestamp_format_migration_preserves_all_authored_formats() {
    assert_eq!(CURRENT_CONFIG_SCHEMA_VERSION, 103);
    for (format, source, expected) in [
        (
            ConfigFormat::Toml,
            "version = 102\nterminal = { agent_log_timestamp_format = '%H:%M', agent_log_timestamps = false }\n",
            Some("%H:%M"),
        ),
        (
            ConfigFormat::Toml,
            "version = 102\n[terminal]\nagent_log_timestamp_format = '[%S-%M-%H]'\nagent_log_timestamps = false\n",
            Some("[%S-%M-%H]"),
        ),
        (
            ConfigFormat::Toml,
            "version = 102\n[terminal]\nagent_log_timestamps = false\n",
            None,
        ),
        (
            ConfigFormat::Json,
            r#"{"version":102,"terminal":{"agent_log_timestamp_format":"%H:%M","agent_log_timestamps":false}}"#,
            Some("%H:%M"),
        ),
        (
            ConfigFormat::Json,
            r#"{"version":102,"terminal":{"agent_log_timestamps":false}}"#,
            None,
        ),
        (
            ConfigFormat::Yaml,
            "version: 102\nterminal:\n  agent_log_timestamp_format: '%H:%M'\n  agent_log_timestamps: false\n",
            Some("%H:%M"),
        ),
        (
            ConfigFormat::Yaml,
            "version: 102\nterminal:\n  agent_log_timestamps: false\n",
            None,
        ),
    ] {
        let plan = migrate_config_text(format, source).unwrap();
        assert_eq!(plan.from_version, 102);
        assert_eq!(plan.to_version, 103);
        let root = parse_config_json_value(format, &plan.text).unwrap();
        assert_eq!(
            root.pointer("/terminal/agent_log_timestamp_format")
                .and_then(serde_json::Value::as_str),
            expected
        );
        assert_eq!(
            root.pointer("/terminal/agent_log_timestamps")
                .and_then(serde_json::Value::as_bool),
            Some(false)
        );
        let repeated = migrate_config_text(format, &plan.text).unwrap();
        assert!(!repeated.changed);
        assert_eq!(repeated.text, plan.text);
    }
}

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
