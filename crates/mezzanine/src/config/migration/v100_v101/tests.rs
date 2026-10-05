//! Exact legacy-policy conversion across supported document containers.
//! Missing and invalid authored values survive for normal validation.

use super::*;

/// All supported formats and TOML container spellings convert exact legacy
/// policy only, preserving unrelated fields and omission. Repeated migration
/// through the driver is byte-stable after reaching the current schema.
#[test]
fn migration_100_naming_preserves_authored_policy_and_containers() {
    for (format, text, expected) in [
        (
            ConfigFormat::Toml,
            "version = 100\n[agents]\nname_mode = \"nonhuman\"\nmax_depth = 7\n",
            Some(serde_json::json!("machine")),
        ),
        (
            ConfigFormat::Toml,
            "version = 100\nagents = { name_mode = \"nonhuman\", max_depth = 7 }\n",
            Some(serde_json::json!("machine")),
        ),
        (
            ConfigFormat::Toml,
            "version = 100\nagents.name_mode = \"nonhuman\"\nagents.max_depth = 7\n",
            Some(serde_json::json!("machine")),
        ),
        (
            ConfigFormat::Json,
            r#"{"version":100,"agents":{"name_mode":"nonhuman","max_depth":7}}"#,
            Some(serde_json::json!("machine")),
        ),
        (
            ConfigFormat::Yaml,
            "version: 100\nagents:\n  name_mode: nonhuman\n  max_depth: 7\n",
            Some(serde_json::json!("machine")),
        ),
        (
            ConfigFormat::Toml,
            "version = 100\nagents = { name_mode = 7, max_depth = 7 }\n",
            Some(serde_json::json!(7)),
        ),
        (
            ConfigFormat::Json,
            r#"{"version":100,"agents":{"name_mode":"Nonhuman","max_depth":7}}"#,
            Some(serde_json::json!("Nonhuman")),
        ),
        (
            ConfigFormat::Yaml,
            "version: 100\nagents:\n  name_mode: human\n  max_depth: 7\n",
            Some(serde_json::json!("human")),
        ),
        (
            ConfigFormat::Toml,
            "version = 100\n[agents]\nname_mode = \"literal\"\nmax_depth = 7\n",
            Some(serde_json::json!("literal")),
        ),
        (
            ConfigFormat::Toml,
            "version = 100\n[agents]\nmax_depth = 7\n",
            None,
        ),
    ] {
        let migrated = super::super::driver::migrate_config_text(format, text).unwrap();
        let root = super::super::super::parse_config_json_value(format, &migrated.text).unwrap();
        assert_eq!(
            root.pointer("/agents/name_mode"),
            expected.as_ref(),
            "{text}"
        );
        assert_eq!(
            root.pointer("/agents/max_depth"),
            Some(&serde_json::json!(7))
        );
        assert_eq!(root.pointer("/version"), Some(&serde_json::json!(101)));
        let repeat = super::super::driver::migrate_config_text(format, &migrated.text).unwrap();
        assert!(!repeat.changed);
        assert_eq!(repeat.text, migrated.text);
    }
}
