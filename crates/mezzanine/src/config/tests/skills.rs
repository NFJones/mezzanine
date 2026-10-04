//! Presence-preserving primary skill-discovery policy contracts.
//!
//! Validation retains scalar types and blocks project policy overrides. Migration
//! advances version only; explicit invocation and action allowlists are unchanged.

use super::*;

/// Comment-only mapping tails are not scalars. Set/unset must preserve authored
/// comments and sibling policy while restoring absence for the removed leaf.
#[test]
fn skill_discovery_yaml_commented_ancestors_preserve_policy_and_comments() {
    let text = "skills: # user policy\n  discovery: true\n  overrides: # overrides\n    other:\n      discovery: false\n";
    let changed = plan_config_mutation(
        ConfigFormat::Yaml,
        text,
        ConfigScope::Primary,
        ConfigMutation {
            path: "skills.overrides.review.discovery".to_string(),
            operation: ConfigMutationOperation::Set(ConfigMutationValue::Boolean(true)),
        },
    )
    .unwrap();
    assert!(changed.text.contains("# user policy"));
    assert!(changed.text.contains("# overrides"));
    let text = changed.text.replace("review:", "review: # review policy");
    let removed = plan_config_mutation(
        ConfigFormat::Yaml,
        &text,
        ConfigScope::Primary,
        unset("skills.overrides.review.discovery"),
    )
    .unwrap();
    assert!(removed.text.contains("# review policy"));
    let root = crate::config::parse_config_json_value(ConfigFormat::Yaml, &removed.text).unwrap();
    assert!(root.pointer("/skills/overrides/review").is_none());
    assert_eq!(
        root.pointer("/skills/overrides/other/discovery"),
        Some(&serde_json::json!(false))
    );
    assert!(
        plan_config_mutation(
            ConfigFormat::Yaml,
            "skills: '# scalar'\n",
            ConfigScope::Primary,
            ConfigMutation {
                path: "skills.overrides.review.discovery".to_string(),
                operation: ConfigMutationOperation::Set(ConfigMutationValue::Boolean(true))
            }
        )
        .is_err()
    );
}

/// Supported formats preserve absence, booleans and per-name overrides; invalid
/// types, names, nested keys and trusted project settings fail closed.
#[test]
fn skill_discovery_configuration_is_typed_and_primary_owned() {
    for (format, text) in [
        (
            ConfigFormat::Toml,
            "[skills]\ndiscovery = true\n[skills.overrides.review]\ndiscovery = false\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"skills":{"discovery":true,"overrides":{"review":{"discovery":false}}}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "skills:\n  discovery: true\n  overrides:\n    review:\n      discovery: false\n",
        ),
    ] {
        let config = compose_effective_config(&[ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format,
            scope: ConfigScope::Primary,
            trusted: true,
            text: text.to_string(),
        }])
        .unwrap();
        let policy = crate::config::skill_discovery_policy(&config);
        assert_eq!(policy.global, Some(true));
        assert_eq!(policy.overrides.get("review"), Some(&false));
        let project =
            format!("version = {CURRENT_CONFIG_SCHEMA_VERSION}\n[skills]\ndiscovery = true\n");
        assert!(
            !validate_config_text(ConfigFormat::Toml, &project, ConfigScope::ProjectOverlay).valid
        );
    }
    for text in [
        r#"{"skills":{"discovery":"true"}}"#,
        r#"{"skills":{"discovery":null}}"#,
        r#"{"skills":{"overrides":{"BadName":{"discovery":true}}}}"#,
        r#"{"skills":{"overrides":{"review":{"discovery":1}}}}"#,
        r#"{"skills":{"overrides":{"review":{"unknown":true}}}}"#,
    ] {
        assert!(
            !validate_config_text(ConfigFormat::Json, text, ConfigScope::Primary).valid,
            "{text}"
        );
    }
    let config = compose_effective_config(&[]).unwrap();
    assert_eq!(crate::config::skill_discovery_policy(&config).global, None);
}

/// Live user mutation accepts only the four-segment per-name boolean leaf,
/// and removal restores absence rather than installing a false override.
#[test]
fn skill_discovery_override_mutation_preserves_presence() {
    for (format, text) in [
        (ConfigFormat::Toml, ""),
        (ConfigFormat::Json, "{}"),
        (ConfigFormat::Yaml, ""),
    ] {
        let changed = plan_config_mutation(
            format,
            text,
            ConfigScope::Primary,
            ConfigMutation {
                path: "skills.overrides.review.discovery".to_string(),
                operation: ConfigMutationOperation::Set(ConfigMutationValue::Boolean(true)),
            },
        )
        .unwrap();
        assert!(validate_config_text(format, &changed.text, ConfigScope::Primary).valid);
        let removed = plan_config_mutation(
            format,
            &changed.text,
            ConfigScope::Primary,
            unset("skills.overrides.review.discovery"),
        )
        .unwrap();
        assert!(
            !extract_config_values(format, &removed.text)
                .contains_key("skills.overrides.review.discovery")
        );
    }
}

/// V98 migration does not manufacture discovery declarations or alter authored
/// enabled actions, including when upgrading JSON/YAML as well as TOML.
#[test]
fn skill_discovery_migration_leaves_policy_absent_and_actions_unchanged() {
    for (format, text) in [
        (
            ConfigFormat::Toml,
            "version = 98\n[agents]\nenabled_actions = [\"say\"]\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":98,"agents":{"enabled_actions":["say"]}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 98\nagents:\n  enabled_actions: [say]\n",
        ),
    ] {
        let migrated = migrate_config_text(format, text).unwrap();
        let root = crate::config::parse_config_json_value(format, &migrated.text).unwrap();
        assert_eq!(root["version"], CURRENT_CONFIG_SCHEMA_VERSION);
        assert!(root.get("skills").is_none());
        assert_eq!(
            root["agents"]["enabled_actions"],
            serde_json::json!(["say"])
        );
    }
}
