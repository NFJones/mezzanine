//! Config migration tests.

use super::*;
use crate::config::parse_config_json_value;

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
    assert!(
        plan.text
            .contains(&format!("version = {CURRENT_CONFIG_SCHEMA_VERSION}"))
    );
    assert!(plan.text.contains("emoji_width = \"wide\""));
    assert!(plan.text.contains("agent_wrap_column_cap = 120"));
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
        !plan
            .text
            .contains("implementation_pressure_after_shell_actions")
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
    assert_eq!(
        values.get("version"),
        Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
    );
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
    assert_eq!(
        values.get("version"),
        Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
    );
    assert_eq!(
        values.get("terminal.emoji_width"),
        Some(&"wide".to_string())
    );
    assert_eq!(
        values.get("auth.provider_refresh_leeway_seconds"),
        Some(&"86400".to_string())
    );
    assert!(!values.contains_key("agents.implementation_pressure_after_shell_actions"));
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
    assert_eq!(
        values.get("version"),
        Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
    );
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
    assert_eq!(
        values.get("version"),
        Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
    );
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
    assert_eq!(
        missing_values.get("version"),
        Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
    );
    assert_eq!(
        missing_values.get("terminal.emoji_width"),
        Some(&"wide".to_string())
    );

    let explicit = migrate_config_text(
        ConfigFormat::Toml,
        "version = 9\n[terminal]\nemoji_width = \"narrow\"\n",
    )
    .unwrap();
    let explicit_values = extract_config_values(ConfigFormat::Toml, &explicit.text);
    assert_eq!(
        explicit_values.get("version"),
        Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
    );
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
/// Verifies the v18 agent wrap-column cap migration backfills the default
/// display-width cap without overriding an explicit user value.
///
/// The cap controls persisted agent log and transcript presentation row widths,
/// so legacy configs must receive the previous 120-column behavior while users
/// who already configured the new setting keep their chosen width.
#[test]
fn migrates_agent_wrap_column_cap_default_to_current_schema() {
    let missing = migrate_config_text(
        ConfigFormat::Toml,
        "version = 17\n[terminal]\nrender_rate_limit_fps = 5\n",
    )
    .unwrap();
    let missing_values = extract_config_values(ConfigFormat::Toml, &missing.text);
    assert_eq!(
        missing_values.get("version"),
        Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
    );
    assert_eq!(
        missing_values.get("terminal.agent_wrap_column_cap"),
        Some(&"120".to_string())
    );

    let explicit = migrate_config_text(
        ConfigFormat::Toml,
        "version = 17\n[terminal]\nagent_wrap_column_cap = 96\n",
    )
    .unwrap();
    let explicit_values = extract_config_values(ConfigFormat::Toml, &explicit.text);
    assert_eq!(
        explicit_values.get("version"),
        Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
    );
    assert_eq!(
        explicit_values.get("terminal.agent_wrap_column_cap"),
        Some(&"96".to_string())
    );
}

/// Verifies schema 19 removes the obsolete implementation-pressure setting.
///
/// The setting only controlled model-facing pressure prose, which no longer
/// belongs in request context. Migration must delete it for every supported
/// primary-config format while preserving unrelated agent settings.
#[test]
fn migrates_schema_19_implementation_pressure_setting_to_schema_20() {
    for (format, input) in [
        (
            ConfigFormat::Toml,
            "version = 19\n[agents]\nimplementation_pressure_after_shell_actions = 7\nloop_limit = 9\n",
        ),
        (
            ConfigFormat::Yaml,
            "version: 19\nagents:\n  implementation_pressure_after_shell_actions: 7\n  loop_limit: 9\n",
        ),
        (
            ConfigFormat::Json,
            "{\"version\":19,\"agents\":{\"implementation_pressure_after_shell_actions\":7,\"loop_limit\":9}}",
        ),
    ] {
        let plan = migrate_config_text(format, input).unwrap();
        let values = extract_config_values(format, &plan.text);

        assert_eq!(plan.from_version, 19);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert!(plan.changed);
        assert_eq!(
            values.get("version"),
            Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
        );
        assert_eq!(values.get("agents.loop_limit"), Some(&"9".to_string()));
        assert!(!values.contains_key("agents.implementation_pressure_after_shell_actions"));
    }
}

/// Verifies schema v20 migration preserves authorization while selecting the
/// policy-only backend and never inventing filesystem authority or effects.
#[test]
fn migrates_schema_20_permissions_without_inferred_authority() {
    let plan = migrate_config_text(
        ConfigFormat::Toml,
        "version = 20\n[permissions]\napproval_policy = \"full-access\"\n[[permissions.command_rules]]\npattern = [\"cargo\", \"test\"]\ndecision = \"allow\"\n",
    )
    .unwrap();
    let values = extract_config_values(ConfigFormat::Toml, &plan.text);

    assert_eq!(plan.from_version, 20);
    assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
    assert_eq!(
        values.get("version"),
        Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
    );
    assert_eq!(
        values.get("permissions.sandbox"),
        Some(&"policy-only".to_string())
    );
    assert_eq!(
        values.get("permissions.approval_policy"),
        Some(&"full-access".to_string())
    );
    assert!(!values.contains_key("permissions.read_scopes"));
    assert!(!values.contains_key("permissions.write_scopes"));
    assert!(!plan.text.contains("effects"));
}

/// Verifies schema v24 advances existing documents without reading or
/// inventing a host Git identity for Bubblewrap.
#[test]
fn migrates_schema_23_without_inventing_git_identity() {
    let plan = migrate_config_text(
        ConfigFormat::Toml,
        "version = 23\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\nnetwork = \"isolated\"\n",
    )
    .unwrap();
    let values = extract_config_values(ConfigFormat::Toml, &plan.text);

    assert_eq!(plan.from_version, 23);
    assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
    assert_eq!(
        values.get("version"),
        Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
    );
    assert!(!values.contains_key("permissions.bubblewrap.git_user_name"));
    assert!(!values.contains_key("permissions.bubblewrap.git_user_email"));
}

/// Verifies schema v25 preserves an omitted toolchain selection and never
/// infers host roots from ambient state or project trust.
#[test]
fn migrates_schema_24_without_enabling_toolchains() {
    let plan = migrate_config_text(
        ConfigFormat::Toml,
        "version = 24\n[permissions]\nsandbox = \"bubblewrap\"\n",
    )
    .unwrap();
    let values = extract_config_values(ConfigFormat::Toml, &plan.text);

    assert_eq!(plan.from_version, 24);
    assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
    assert_eq!(
        values.get("version"),
        Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
    );
    assert!(!values.contains_key("permissions.bubblewrap.toolchains"));
}

/// Verifies schema v26 preserves both omission and an existing Rust selection
/// without discovering or enabling Zig from the migration environment.
#[test]
fn migrates_schema_25_without_enabling_zig() {
    for input in [
        "version = 25\n[permissions]\nsandbox = \"bubblewrap\"\n",
        "version = 25\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = [\"rust\"]\n",
    ] {
        let plan = migrate_config_text(ConfigFormat::Toml, input).unwrap();
        let values = extract_config_values(ConfigFormat::Toml, &plan.text);

        assert_eq!(plan.from_version, 25);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("version"),
            Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
        );
        assert!(!values.contains_key("permissions.bubblewrap.toolchains"));
        assert!(!plan.text.contains("custom_toolchains"));
        assert!(!plan.text.contains("zig"));
    }
}

/// Verifies schema v27 preserves both omission and existing built-in
/// selections without discovering or enabling Go from ambient state.
#[test]
fn migrates_schema_26_without_enabling_go() {
    for input in [
        "version = 26\n[permissions]\nsandbox = \"bubblewrap\"\n",
        "version = 26\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = [\"rust\", \"zig\"]\n",
    ] {
        let plan = migrate_config_text(ConfigFormat::Toml, input).unwrap();
        let values = extract_config_values(ConfigFormat::Toml, &plan.text);

        assert_eq!(plan.from_version, 26);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("version"),
            Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
        );
        assert!(!values.contains_key("permissions.bubblewrap.toolchains"));
        assert!(!plan.text.contains("custom_toolchains"));
    }
}

/// Verifies schema v28 preserves both omission and existing built-in
/// selections without discovering or enabling Deno from ambient state.
#[test]
fn migrates_schema_27_without_enabling_deno() {
    for input in [
        "version = 27\n[permissions]\nsandbox = \"bubblewrap\"\n",
        "version = 27\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = [\"rust\", \"zig\", \"go\"]\n",
    ] {
        let plan = migrate_config_text(ConfigFormat::Toml, input).unwrap();
        let values = extract_config_values(ConfigFormat::Toml, &plan.text);

        assert_eq!(plan.from_version, 27);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("version"),
            Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
        );
        assert!(!values.contains_key("permissions.bubblewrap.toolchains"));
        assert!(!plan.text.contains("custom_toolchains"));
        assert!(!plan.text.contains("deno"));
    }
}

/// Verifies schema v29 preserves omission and existing built-in selections
/// without discovering or enabling Bun from ambient process state.
#[test]
fn migrates_schema_28_without_enabling_bun() {
    for input in [
        "version = 28\n[permissions]\nsandbox = \"bubblewrap\"\n",
        "version = 28\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = [\"rust\", \"zig\", \"go\", \"deno\"]\n",
    ] {
        let plan = migrate_config_text(ConfigFormat::Toml, input).unwrap();
        let values = extract_config_values(ConfigFormat::Toml, &plan.text);

        assert_eq!(plan.from_version, 28);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("version"),
            Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
        );
        assert!(!values.contains_key("permissions.bubblewrap.toolchains"));
        assert!(!plan.text.contains("custom_toolchains"));
        assert!(!plan.text.contains("bun"));
    }
}

/// Verifies schema v30 preserves omission and existing built-in selections
/// without discovering or enabling Node.js from ambient process state.
#[test]
fn migrates_schema_29_without_enabling_node() {
    for input in [
        "version = 29\n[permissions]\nsandbox = \"bubblewrap\"\n",
        "version = 29\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = [\"rust\", \"zig\", \"go\", \"deno\", \"bun\"]\n",
    ] {
        let plan = migrate_config_text(ConfigFormat::Toml, input).unwrap();
        let values = extract_config_values(ConfigFormat::Toml, &plan.text);

        assert_eq!(plan.from_version, 29);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("version"),
            Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
        );
        assert!(!values.contains_key("permissions.bubblewrap.toolchains"));
        assert!(!plan.text.contains("custom_toolchains"));
        assert!(!plan.text.contains("node"));
    }
}

/// Verifies schema v31 preserves omission and existing built-in selections
/// without discovering or enabling Python from ambient process state.
#[test]
fn migrates_schema_30_without_enabling_python() {
    for input in [
        "version = 30\n[permissions]\nsandbox = \"bubblewrap\"\n",
        "version = 30\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = [\"rust\", \"zig\", \"go\", \"deno\", \"bun\", \"node\"]\n",
    ] {
        let plan = migrate_config_text(ConfigFormat::Toml, input).unwrap();
        let values = extract_config_values(ConfigFormat::Toml, &plan.text);

        assert_eq!(plan.from_version, 30);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("version"),
            Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
        );
        assert!(!values.contains_key("permissions.bubblewrap.toolchains"));
        assert!(!plan.text.contains("custom_toolchains"));
        assert!(!plan.text.contains("python"));
    }
}

/// Verifies schema v32 preserves existing built-in selections and omission
/// without inventing custom definitions or selections from ambient state.
#[test]
fn migrates_schema_31_without_enabling_custom_toolchains() {
    for input in [
        "version = 31\n[permissions]\nsandbox = \"bubblewrap\"\n",
        "version = 31\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = [\"rust\", \"python\"]\n",
    ] {
        let plan = migrate_config_text(ConfigFormat::Toml, input).unwrap();
        let values = extract_config_values(ConfigFormat::Toml, &plan.text);

        assert_eq!(plan.from_version, 31);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("version"),
            Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
        );
        assert!(!values.contains_key("permissions.bubblewrap.toolchains"));
        assert!(!plan.text.contains("custom_toolchains"));
        assert!(!plan.text.contains("custom_toolchains"));
        assert!(!plan.text.contains("custom:"));
    }
}

/// Verifies schema v33 preserves existing built-in and custom selections
/// without discovering or enabling a JDK from ambient host state.
#[test]
fn migrates_schema_32_without_enabling_jdk() {
    for input in [
        "version = 32\n[permissions]\nsandbox = \"bubblewrap\"\n",
        "version = 32\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = [\"rust\", \"custom:acme\"]\n[permissions.bubblewrap.custom_toolchains.acme]\nroots = [\"/opt/acme\"]\npath_entries = [\"0:bin\"]\n",
    ] {
        let plan = migrate_config_text(ConfigFormat::Toml, input).unwrap();
        let values = extract_config_values(ConfigFormat::Toml, &plan.text);

        assert_eq!(plan.from_version, 32);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("version"),
            Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
        );
        assert!(!values.contains_key("permissions.bubblewrap.toolchains"));
        assert!(!plan.text.contains("custom_toolchains"));
        assert!(!plan.text.contains("jdk"));
    }
}

/// Verifies schema v34 preserves existing built-in and custom selections
/// without discovering or enabling a .NET SDK from ambient host state.
#[test]
fn migrates_schema_33_without_enabling_dotnet() {
    for input in [
        "version = 33\n[permissions]\nsandbox = \"bubblewrap\"\n",
        "version = 33\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = [\"jdk\", \"custom:acme\"]\n[permissions.bubblewrap.custom_toolchains.acme]\nroots = [\"/opt/acme\"]\npath_entries = [\"0:bin\"]\n",
    ] {
        let plan = migrate_config_text(ConfigFormat::Toml, input).unwrap();
        let values = extract_config_values(ConfigFormat::Toml, &plan.text);

        assert_eq!(plan.from_version, 33);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("version"),
            Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
        );
        assert!(!values.contains_key("permissions.bubblewrap.toolchains"));
        assert!(!plan.text.contains("custom_toolchains"));
        assert!(!plan.text.contains("dotnet"));
    }
}

/// Verifies schema v35 preserves existing built-in and custom selections
/// without discovering or enabling a Dart SDK from ambient host state.
#[test]
fn migrates_schema_34_without_enabling_dart() {
    for input in [
        "version = 34\n[permissions]\nsandbox = \"bubblewrap\"\n",
        "version = 34\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = [\"dotnet\", \"custom:acme\"]\n[permissions.bubblewrap.custom_toolchains.acme]\nroots = [\"/opt/acme\"]\npath_entries = [\"0:bin\"]\n",
    ] {
        let plan = migrate_config_text(ConfigFormat::Toml, input).unwrap();
        let values = extract_config_values(ConfigFormat::Toml, &plan.text);

        assert_eq!(plan.from_version, 34);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("version"),
            Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
        );
        assert!(!values.contains_key("permissions.bubblewrap.toolchains"));
        assert!(!plan.text.contains("custom_toolchains"));
        assert!(!plan.text.contains("dart"));
    }
}

/// Verifies schema v36 preserves existing built-in and custom selections
/// without discovering or enabling a Kotlin/JVM compiler from ambient state.
#[test]
fn migrates_schema_35_without_enabling_kotlin() {
    for input in [
        "version = 35\n[permissions]\nsandbox = \"bubblewrap\"\n",
        "version = 35\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = [\"jdk\", \"dart\", \"custom:acme\"]\n[permissions.bubblewrap.custom_toolchains.acme]\nroots = [\"/opt/acme\"]\npath_entries = [\"0:bin\"]\n",
    ] {
        let plan = migrate_config_text(ConfigFormat::Toml, input).unwrap();
        let values = extract_config_values(ConfigFormat::Toml, &plan.text);

        assert_eq!(plan.from_version, 35);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("version"),
            Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
        );
        assert!(!values.contains_key("permissions.bubblewrap.toolchains"));
        assert!(!plan.text.contains("custom_toolchains"));
        assert!(!plan.text.contains("kotlin"));
    }
}

/// Verifies schema v37 preserves existing built-in and custom selections
/// without discovering or enabling a Ruby runtime from ambient host state.
#[test]
fn migrates_schema_36_without_enabling_ruby() {
    for input in [
        "version = 36\n[permissions]\nsandbox = \"bubblewrap\"\n",
        "version = 36\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = [\"kotlin\", \"custom:acme\"]\n[permissions.bubblewrap.custom_toolchains.acme]\nroots = [\"/opt/acme\"]\npath_entries = [\"0:bin\"]\n",
    ] {
        let plan = migrate_config_text(ConfigFormat::Toml, input).unwrap();
        let values = extract_config_values(ConfigFormat::Toml, &plan.text);

        assert_eq!(plan.from_version, 36);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("version"),
            Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
        );
        assert!(!values.contains_key("permissions.bubblewrap.toolchains"));
        assert!(!plan.text.contains("custom_toolchains"));
        assert!(!plan.text.contains("ruby"));
    }
}

/// Verifies schema v38 preserves existing built-in and custom selections
/// without discovering or enabling PHP or Composer from ambient host state.
#[test]
fn migrates_schema_37_without_enabling_php_or_composer() {
    for input in [
        "version = 37\n[permissions]\nsandbox = \"bubblewrap\"\n",
        "version = 37\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = [\"ruby\", \"custom:acme\"]\n[permissions.bubblewrap.custom_toolchains.acme]\nroots = [\"/opt/acme\"]\npath_entries = [\"0:bin\"]\n",
    ] {
        let plan = migrate_config_text(ConfigFormat::Toml, input).unwrap();
        let values = extract_config_values(ConfigFormat::Toml, &plan.text);

        assert_eq!(plan.from_version, 37);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("version"),
            Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
        );
        assert!(!values.contains_key("permissions.bubblewrap.toolchains"));
        assert!(!plan.text.contains("custom_toolchains"));
        assert!(!plan.text.contains("php"));
        assert!(!plan.text.contains("composer"));
    }
}

/// Verifies schema v39 preserves existing built-in and custom selections
/// without discovering or enabling Erlang or Elixir from ambient host state.
#[test]
fn migrates_schema_38_without_enabling_erlang_or_elixir() {
    for input in [
        "version = 38\n[permissions]\nsandbox = \"bubblewrap\"\n",
        "version = 38\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = [\"composer\", \"custom:acme\"]\n[permissions.bubblewrap.custom_toolchains.acme]\nroots = [\"/opt/acme\"]\npath_entries = [\"0:bin\"]\n",
    ] {
        let plan = migrate_config_text(ConfigFormat::Toml, input).unwrap();
        let values = extract_config_values(ConfigFormat::Toml, &plan.text);

        assert_eq!(plan.from_version, 38);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("version"),
            Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
        );
        assert!(!values.contains_key("permissions.bubblewrap.toolchains"));
        assert!(!plan.text.contains("custom_toolchains"));
        assert!(!plan.text.contains("erlang"));
        assert!(!plan.text.contains("elixir"));
    }
}

/// Verifies schema v40 preserves existing built-in and custom selections
/// without discovering or enabling GHC, Cabal, or Stack from ambient state.
#[test]
fn migrates_schema_39_without_enabling_haskell_toolchains() {
    for input in [
        "version = 39\n[permissions]\nsandbox = \"bubblewrap\"\n",
        "version = 39\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = [\"elixir\", \"custom:acme\"]\n[permissions.bubblewrap.custom_toolchains.acme]\nroots = [\"/opt/acme\"]\npath_entries = [\"0:bin\"]\n",
    ] {
        let plan = migrate_config_text(ConfigFormat::Toml, input).unwrap();
        let values = extract_config_values(ConfigFormat::Toml, &plan.text);

        assert_eq!(plan.from_version, 39);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("version"),
            Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
        );
        assert!(!values.contains_key("permissions.bubblewrap.toolchains"));
        assert!(!plan.text.contains("custom_toolchains"));
        assert!(!plan.text.contains("ghc"));
        assert!(!plan.text.contains("cabal"));
        assert!(!plan.text.contains("stack"));
    }
}

/// Verifies schema v41 preserves existing built-in and custom selections
/// without discovering or enabling an OCaml local switch from ambient state.
#[test]
fn migrates_schema_40_without_enabling_ocaml() {
    for input in [
        "version = 40\n[permissions]\nsandbox = \"bubblewrap\"\n",
        "version = 40\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = [\"stack\", \"custom:acme\"]\n[permissions.bubblewrap.custom_toolchains.acme]\nroots = [\"/opt/acme\"]\npath_entries = [\"0:bin\"]\n",
    ] {
        let plan = migrate_config_text(ConfigFormat::Toml, input).unwrap();
        let values = extract_config_values(ConfigFormat::Toml, &plan.text);

        assert_eq!(plan.from_version, 40);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("version"),
            Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
        );
        assert!(!values.contains_key("permissions.bubblewrap.toolchains"));
        assert!(!plan.text.contains("custom_toolchains"));
        assert!(!plan.text.contains("ocaml"));
    }
}

/// Verifies schema v42 preserves existing built-in and custom selections
/// without discovering or enabling native compiler and build-tool kinds.
#[test]
fn migrates_schema_41_without_enabling_native_toolchains() {
    for input in [
        "version = 41\n[permissions]\nsandbox = \"bubblewrap\"\n",
        "version = 41\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = [\"ocaml\", \"custom:acme\"]\n[permissions.bubblewrap.custom_toolchains.acme]\nroots = [\"/opt/acme\"]\npath_entries = [\"0:bin\"]\n",
    ] {
        let plan = migrate_config_text(ConfigFormat::Toml, input).unwrap();
        let values = extract_config_values(ConfigFormat::Toml, &plan.text);

        assert_eq!(plan.from_version, 41);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("version"),
            Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
        );
        assert!(!values.contains_key("permissions.bubblewrap.toolchains"));
        assert!(!plan.text.contains("custom_toolchains"));
        for kind in ["llvm", "gcc", "cmake", "ninja", "meson"] {
            assert!(!plan.text.contains(kind));
        }
    }
}

/// Verifies schema v43 preserves existing built-in and custom selections
/// without discovering or enabling Swift from ambient host state.
#[test]
fn migrates_schema_42_without_enabling_swift() {
    for input in [
        "version = 42\n[permissions]\nsandbox = \"bubblewrap\"\n",
        "version = 42\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = [\"llvm\", \"custom:acme\"]\n[permissions.bubblewrap.custom_toolchains.acme]\nroots = [\"/opt/acme\"]\npath_entries = [\"0:bin\"]\n",
    ] {
        let plan = migrate_config_text(ConfigFormat::Toml, input).unwrap();
        let values = extract_config_values(ConfigFormat::Toml, &plan.text);

        assert_eq!(plan.from_version, 42);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("version"),
            Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
        );
        assert!(!values.contains_key("permissions.bubblewrap.toolchains"));
        assert!(!plan.text.contains("custom_toolchains"));
        assert!(!plan.text.contains("swift"));
    }
}

/// Verifies schema v44 preserves existing built-in and custom selections
/// without discovering or enabling Maven or Gradle from ambient host state.
#[test]
fn migrates_schema_43_without_enabling_jvm_build_tools() {
    for input in [
        "version = 43\n[permissions]\nsandbox = \"bubblewrap\"\n",
        "version = 43\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = [\"jdk\", \"custom:acme\"]\n[permissions.bubblewrap.custom_toolchains.acme]\nroots = [\"/opt/acme\"]\npath_entries = [\"0:bin\"]\n",
    ] {
        let plan = migrate_config_text(ConfigFormat::Toml, input).unwrap();
        let values = extract_config_values(ConfigFormat::Toml, &plan.text);

        assert_eq!(plan.from_version, 43);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("version"),
            Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
        );
        assert!(!values.contains_key("permissions.bubblewrap.toolchains"));
        assert!(!plan.text.contains("custom_toolchains"));
        assert!(!plan.text.contains("maven"));
        assert!(!plan.text.contains("gradle"));
    }
}

/// Verifies schema v45 removes inert permission trust lists without creating
/// durable project-trust state or changing unrelated permission settings.
#[test]
fn migrates_schema_44_by_removing_inert_permission_trust_lists() {
    for (format, input) in [
        (
            ConfigFormat::Toml,
            "version = 44\n[permissions]\ntrusted_directories = [\"/workspace\"]\ntrusted_projects = [\"/workspace/project\"]\napproval_policy = \"ask\"\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":44,"permissions":{"trusted_directories":["/workspace"],"trusted_projects":["/workspace/project"],"approval_policy":"ask"}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 44\npermissions:\n  trusted_directories:\n    - /workspace\n  trusted_projects:\n    - /workspace/project\n  approval_policy: ask\n",
        ),
    ] {
        let plan = migrate_config_text(format, input).unwrap();
        let values = extract_config_values(format, &plan.text);

        assert_eq!(plan.from_version, 44);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("version"),
            Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
        );
        assert!(!values.contains_key("permissions.trusted_directories"));
        assert!(!values.contains_key("permissions.trusted_projects"));
        assert_eq!(
            values.get("permissions.approval_policy"),
            Some(&"ask".to_string())
        );
    }
}

/// Verifies schema v46 adds the default-enabled completion-attention flashing
/// setting without changing unrelated terminal behavior in any supported format.
#[test]
fn migrates_schema_45_with_completion_attention_flashing_enabled() {
    for (format, input) in [
        (
            ConfigFormat::Toml,
            "version = 45\n[terminal]\nreduced_motion = false\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":45,"terminal":{"reduced_motion":false}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 45\nterminal:\n  reduced_motion: false\n",
        ),
    ] {
        let plan = migrate_config_text(format, input).unwrap();
        let values = extract_config_values(format, &plan.text);

        assert_eq!(plan.from_version, 45);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("version"),
            Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
        );
        assert_eq!(
            values.get("terminal.completion_attention_flashing"),
            Some(&"true".to_string())
        );
        assert_eq!(
            values.get("terminal.reduced_motion"),
            Some(&"false".to_string())
        );
    }
}

/// Verifies schema v47 gives existing configurations the responsive default
/// Tokio worker count in every supported primary configuration format.
///
/// The runtime must be constructed before asynchronous CLI dispatch, so this
/// migration preserves a deterministic worker-pool size for upgraded users.
#[test]
fn migrates_schema_46_with_two_runtime_workers() {
    for (format, input) in [
        (
            ConfigFormat::Toml,
            "version = 46\n[terminal]\nmouse = true\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":46,"terminal":{"mouse":true}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 46\nterminal:\n  mouse: true\n",
        ),
    ] {
        let plan = migrate_config_text(format, input).unwrap();
        let values = extract_config_values(format, &plan.text);

        assert_eq!(plan.from_version, 46);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("version"),
            Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
        );
        assert_eq!(values.get("runtime.cpu_count"), Some(&"2".to_string()));
        assert_eq!(values.get("terminal.mouse"), Some(&"true".to_string()));
    }
}

/// Verifies schema v48 removes ambient supplementary-group inheritance for
/// every supported primary configuration format. Upgraded users receive an
/// explicit empty exact set until they select host group names themselves.
#[test]
fn migrates_schema_47_with_empty_group_whitelist() {
    for (format, input) in [
        (
            ConfigFormat::Toml,
            "version = 47\n[permissions]\nsandbox = \"bubblewrap\"\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":47,"permissions":{"sandbox":"bubblewrap"}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 47\npermissions:\n  sandbox: bubblewrap\n",
        ),
    ] {
        let plan = migrate_config_text(format, input).unwrap();
        let values = extract_config_values(format, &plan.text);

        assert_eq!(plan.from_version, 47);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("version"),
            Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
        );
        assert_eq!(
            values.get("permissions.bubblewrap.group_whitelist"),
            Some(&"[]".to_string())
        );
    }
}

/// Verifies schema v49 renames the pane group mapping allowlist in every
/// supported primary configuration format without changing configured names.
#[test]
fn migrates_schema_48_group_whitelist_name() {
    for (format, input) in [
        (
            ConfigFormat::Toml,
            "version = 48\n[permissions.bubblewrap]\nsupplementary_groups = [\"sudo\", \"docker\"]\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":48,"permissions":{"bubblewrap":{"supplementary_groups":["sudo","docker"]}}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 48\npermissions:\n  bubblewrap:\n    supplementary_groups: [sudo, docker]\n",
        ),
    ] {
        let plan = migrate_config_text(format, input).unwrap();
        let values = extract_config_values(format, &plan.text);

        assert_eq!(plan.from_version, 48);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("version"),
            Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
        );
        assert!(plan.text.contains("group_whitelist"), "{}", plan.text);
        assert!(plan.text.contains("sudo"), "{}", plan.text);
        assert!(plan.text.contains("docker"), "{}", plan.text);
        assert!(!plan.text.contains("supplementary_groups"), "{}", plan.text);
    }
}

/// Verifies schema v49 rejects ambiguous documents that define both the old
/// and canonical pane group mapping keys.
#[test]
fn schema_48_group_whitelist_rename_rejects_conflicts() {
    for (format, input) in [
        (
            ConfigFormat::Toml,
            "version = 48\n[permissions.bubblewrap]\nsupplementary_groups = [\"sudo\"]\ngroup_whitelist = [\"docker\"]\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":48,"permissions":{"bubblewrap":{"supplementary_groups":["sudo"],"group_whitelist":["docker"]}}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 48\npermissions:\n  bubblewrap:\n    supplementary_groups: [sudo]\n    group_whitelist: [docker]\n",
        ),
    ] {
        let error = migrate_config_text(format, input).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("defines both"), "{message}");
        assert!(message.contains("supplementary_groups"), "{message}");
        assert!(message.contains("group_whitelist"), "{message}");
    }
}

/// Verifies schema v50 adds an empty environment whitelist in every supported
/// primary configuration format without inheriting controller environment state.
#[test]
fn migrates_schema_49_with_empty_env_whitelist() {
    for (format, input) in [
        (
            ConfigFormat::Toml,
            "version = 49\n[permissions.bubblewrap]\ngroup_whitelist = []\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":49,"permissions":{"bubblewrap":{"group_whitelist":[]}}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 49\npermissions:\n  bubblewrap:\n    group_whitelist: []\n",
        ),
    ] {
        let plan = migrate_config_text(format, input).unwrap();
        let values = extract_config_values(format, &plan.text);
        assert_eq!(plan.from_version, 49);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("version"),
            Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
        );
        assert_eq!(
            values.get("permissions.bubblewrap.env_whitelist"),
            Some(&"[]".to_string())
        );
    }
}

/// Verifies schema v51 removes every persisted toolchain selector and custom
/// definition without disturbing the surviving generic Bubblewrap settings.
#[test]
fn migrates_schema_50_without_toolchain_configuration() {
    for (format, input) in [
        (
            ConfigFormat::Toml,
            "version = 50\n[permissions.bubblewrap]\nenv_whitelist = [\"ACME_HOME\"]\ntoolchains = [\"rust\", \"custom:acme\"]\n[permissions.bubblewrap.custom_toolchains.acme]\nroots = [\"/opt/acme\"]\npath_entries = [\"0:bin\"]\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":50,"permissions":{"bubblewrap":{"env_whitelist":["ACME_HOME"],"toolchains":["rust","custom:acme"],"custom_toolchains":{"acme":{"roots":["/opt/acme"],"path_entries":["0:bin"]}}}}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 50\npermissions:\n  bubblewrap:\n    env_whitelist: [ACME_HOME]\n    toolchains: [rust, 'custom:acme']\n    custom_toolchains:\n      acme:\n        roots: [/opt/acme]\n        path_entries: ['0:bin']\n",
        ),
    ] {
        let plan = migrate_config_text(format, input).unwrap();
        let values = extract_config_values(format, &plan.text);
        let migrated = parse_config_json_value(format, &plan.text).unwrap();
        assert_eq!(plan.from_version, 50);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("version"),
            Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
        );
        assert_eq!(
            migrated
                .pointer("/permissions/bubblewrap/env_whitelist")
                .and_then(serde_json::Value::as_array),
            Some(&vec![serde_json::json!("ACME_HOME")]),
            "surviving env whitelist missing after {format:?} migration: {}",
            plan.text
        );
        assert!(!values.contains_key("permissions.bubblewrap.toolchains"));
        assert!(
            !values
                .keys()
                .any(|path| path.starts_with("permissions.bubblewrap.custom_toolchains"))
        );
    }
}

/// Verifies schema v52 backfills the home pane-spawn policy in every supported
/// format while preserving a policy already declared by the user.
#[test]
fn migrates_schema_51_with_pane_spawn_directory_policy() {
    for (format, missing, explicit) in [
        (
            ConfigFormat::Toml,
            "version = 51\n[terminal]\nterm = \"screen-256color\"\n",
            "version = 51\n[terminal]\npane_spawn_directory = \"same-directory\"\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":51,"terminal":{"term":"screen-256color"}}"#,
            r#"{"version":51,"terminal":{"pane_spawn_directory":"same-directory"}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 51\nterminal:\n  term: screen-256color\n",
            "version: 51\nterminal:\n  pane_spawn_directory: same-directory\n",
        ),
    ] {
        let missing_plan = migrate_config_text(format, missing).unwrap();
        let missing_values = extract_config_values(format, &missing_plan.text);
        assert_eq!(missing_plan.from_version, 51);
        assert_eq!(missing_plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            missing_values.get("version"),
            Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
        );
        assert_eq!(
            missing_values.get("terminal.pane_spawn_directory"),
            Some(&"home".to_string())
        );

        let explicit_plan = migrate_config_text(format, explicit).unwrap();
        let explicit_values = extract_config_values(format, &explicit_plan.text);
        assert_eq!(explicit_plan.from_version, 51);
        assert_eq!(explicit_plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            explicit_values.get("version"),
            Some(&CURRENT_CONFIG_SCHEMA_VERSION.to_string())
        );
        assert_eq!(
            explicit_values.get("terminal.pane_spawn_directory"),
            Some(&"same-directory".to_string())
        );
    }
}

/// Verifies schema v53 backfills shell view in every supported format while
/// preserving an explicit request to show the agent surface after pane spawn.
#[test]
fn migrates_schema_52_with_pane_spawn_view_policy() {
    for (format, missing, explicit) in [
        (
            ConfigFormat::Toml,
            "version = 52\n[terminal]\nterm = \"screen-256color\"\n",
            "version = 52\n[terminal]\npane_spawn_view = \"agent\"\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":52,"terminal":{"term":"screen-256color"}}"#,
            r#"{"version":52,"terminal":{"pane_spawn_view":"agent"}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 52\nterminal:\n  term: screen-256color\n",
            "version: 52\nterminal:\n  pane_spawn_view: agent\n",
        ),
    ] {
        let missing_plan = migrate_config_text(format, missing).unwrap();
        let missing_values = extract_config_values(format, &missing_plan.text);
        assert_eq!(missing_plan.from_version, 52);
        assert_eq!(missing_plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            missing_values.get("terminal.pane_spawn_view"),
            Some(&"shell".to_string())
        );

        let explicit_plan = migrate_config_text(format, explicit).unwrap();
        let explicit_values = extract_config_values(format, &explicit_plan.text);
        assert_eq!(explicit_plan.from_version, 52);
        assert_eq!(explicit_plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            explicit_values.get("terminal.pane_spawn_view"),
            Some(&"agent".to_string())
        );
    }
}

/// Verifies schema v54 defaults to explicit-only MCP exposure in every format
/// while preserving an already configured always-exposed server list.
#[test]
fn migrates_schema_53_with_always_exposed_mcp_servers() {
    for (format, missing, explicit) in [
        (
            ConfigFormat::Toml,
            "version = 53\n[agents]\nrouting = false\n",
            "version = 53\n[agents]\nalways_exposed_mcp_servers = [\"GitHub\", \"state\"]\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":53,"agents":{"routing":false}}"#,
            r#"{"version":53,"agents":{"always_exposed_mcp_servers":["GitHub","state"]}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 53\nagents:\n  routing: false\n",
            "version: 53\nagents:\n  always_exposed_mcp_servers: [GitHub, state]\n",
        ),
    ] {
        let missing_plan = migrate_config_text(format, missing).unwrap();
        let missing_values = extract_config_values(format, &missing_plan.text);
        assert_eq!(missing_plan.from_version, 53);
        assert_eq!(missing_plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            missing_values.get("agents.always_exposed_mcp_servers"),
            Some(&"[]".to_string())
        );

        let explicit_plan = migrate_config_text(format, explicit).unwrap();
        let explicit_document = parse_config_json_value(format, &explicit_plan.text).unwrap();
        assert_eq!(explicit_plan.from_version, 53);
        assert_eq!(explicit_plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            explicit_document
                .pointer("/agents/always_exposed_mcp_servers")
                .and_then(serde_json::Value::as_array),
            Some(&vec![
                serde_json::json!("GitHub"),
                serde_json::json!("state")
            ])
        );
    }
}

/// Verifies schema v55 adds disabled enhanced keyboard reporting in every
/// supported format while preserving an explicit user opt-in.
#[test]
fn migrates_schema_54_with_enhanced_keyboard_reporting() {
    for (format, missing, explicit) in [
        (
            ConfigFormat::Toml,
            "version = 54\n[terminal]\nmouse = true\n",
            "version = 54\n[terminal]\nenhanced_keyboard_reporting = true\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":54,"terminal":{"mouse":true}}"#,
            r#"{"version":54,"terminal":{"enhanced_keyboard_reporting":true}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 54\nterminal:\n  mouse: true\n",
            "version: 54\nterminal:\n  enhanced_keyboard_reporting: true\n",
        ),
    ] {
        let missing_plan = migrate_config_text(format, missing).unwrap();
        let missing_values = extract_config_values(format, &missing_plan.text);
        assert_eq!(missing_plan.from_version, 54);
        assert_eq!(missing_plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            missing_values.get("terminal.enhanced_keyboard_reporting"),
            Some(&"false".to_string())
        );

        let explicit_plan = migrate_config_text(format, explicit).unwrap();
        let explicit_values = extract_config_values(format, &explicit_plan.text);
        assert_eq!(explicit_plan.from_version, 54);
        assert_eq!(explicit_plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            explicit_values.get("terminal.enhanced_keyboard_reporting"),
            Some(&"true".to_string())
        );
    }
}

/// Verifies schema v56 adds the disabled active-turn sleep-inhibition policy
/// in every supported format while preserving explicit user selections.
#[test]
fn migrates_schema_55_with_active_turn_sleep_inhibition() {
    for (format, missing, explicit) in [
        (
            ConfigFormat::Toml,
            "version = 55\n[agents]\nrouting = false\n",
            "version = 55\n[agents]\nactive_turn_sleep_inhibition = \"system\"\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":55,"agents":{"routing":false}}"#,
            r#"{"version":55,"agents":{"active_turn_sleep_inhibition":"system"}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 55\nagents:\n  routing: false\n",
            "version: 55\nagents:\n  active_turn_sleep_inhibition: system\n",
        ),
    ] {
        let missing_plan = migrate_config_text(format, missing).unwrap();
        let missing_values = extract_config_values(format, &missing_plan.text);
        assert_eq!(missing_plan.from_version, 55);
        assert_eq!(missing_plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            missing_values.get("agents.active_turn_sleep_inhibition"),
            Some(&"disabled".to_string())
        );

        let explicit_plan = migrate_config_text(format, explicit).unwrap();
        let explicit_values = extract_config_values(format, &explicit_plan.text);
        assert_eq!(explicit_plan.from_version, 55);
        assert_eq!(explicit_plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            explicit_values.get("agents.active_turn_sleep_inhibition"),
            Some(&"system".to_string())
        );
    }
}

/// Verifies schema v57 moves the former pane TERM default to xterm-256color
/// in every supported format while preserving explicit alternative values.
#[test]
fn migrates_schema_56_with_xterm_pane_term_default() {
    for (format, old_default, explicit) in [
        (
            ConfigFormat::Toml,
            "version = 56\n[terminal]\nterm = \"screen-256color\"\n",
            "version = 56\n[terminal]\nterm = \"dumb\"\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":56,"terminal":{"term":"screen-256color"}}"#,
            r#"{"version":56,"terminal":{"term":"dumb"}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 56\nterminal:\n  term: screen-256color\n",
            "version: 56\nterminal:\n  term: dumb\n",
        ),
    ] {
        let default_plan = migrate_config_text(format, old_default).unwrap();
        let default_values = extract_config_values(format, &default_plan.text);
        assert_eq!(default_plan.from_version, 56);
        assert_eq!(default_plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            default_values.get("terminal.term"),
            Some(&"xterm-256color".to_string())
        );

        let explicit_plan = migrate_config_text(format, explicit).unwrap();
        let explicit_values = extract_config_values(format, &explicit_plan.text);
        assert_eq!(explicit_plan.from_version, 56);
        assert_eq!(explicit_plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            explicit_values.get("terminal.term"),
            Some(&"dumb".to_string())
        );
    }
}

/// Verifies schema v58 classifies typed legacy key maps as the built-in
/// default or simple preset in every supported primary-config format.
#[test]
fn migrates_schema_56_to_builtin_key_presets() {
    for (format, default_text, simple_text) in [
        (
            ConfigFormat::Toml,
            "version = 56\n[keys]\nescape = \"C-a\"\n",
            "version = 56\n[keys]\nescape = \"C-a\"\nsplit_vertical = \"A-\\\\\"\nsplit_horizontal = \"A--\"\nnew_window = \"A-=\"\nnew_group = \"A-S-=\"\nagent_shell = \"A-]\"\nfocus_up = \"C-A-Up\"\nfocus_down = \"C-A-Down\"\nfocus_left = \"C-A-Left\"\nfocus_right = \"C-A-Right\"\nfocus_previous_window = \"C-A-PageUp\"\nfocus_next_window = \"C-A-PageDown\"\nfocus_previous_group = \"C-A-S-PageUp\"\nfocus_next_group = \"C-A-S-PageDown\"\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":56,"keys":{"escape":"C-a"}}"#,
            r#"{"version":56,"keys":{"escape":"C-a","split_vertical":"A-\\","split_horizontal":"A--","new_window":"A-=","new_group":"A-S-=","agent_shell":"A-]","focus_up":"C-A-Up","focus_down":"C-A-Down","focus_left":"C-A-Left","focus_right":"C-A-Right","focus_previous_window":"C-A-PageUp","focus_next_window":"C-A-PageDown","focus_previous_group":"C-A-S-PageUp","focus_next_group":"C-A-S-PageDown"}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 56\nkeys:\n  escape: C-a\n",
            "version: 56\nkeys:\n  escape: C-a\n  split_vertical: 'A-\\'\n  split_horizontal: A--\n  new_window: A-=\n  new_group: A-S-=\n  agent_shell: A-]\n  focus_up: C-A-Up\n  focus_down: C-A-Down\n  focus_left: C-A-Left\n  focus_right: C-A-Right\n  focus_previous_window: C-A-PageUp\n  focus_next_window: C-A-PageDown\n  focus_previous_group: C-A-S-PageUp\n  focus_next_group: C-A-S-PageDown\n",
        ),
    ] {
        for (text, expected) in [(default_text, "default"), (simple_text, "simple")] {
            let plan = migrate_config_text(format, text).unwrap();
            let document = parse_config_json_value(format, &plan.text).unwrap();
            assert_eq!(plan.from_version, 56);
            assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
            assert_eq!(
                document
                    .pointer("/key_preset/active")
                    .and_then(serde_json::Value::as_str),
                Some(expected)
            );
        }
    }
}

/// Verifies non-built-in legacy maps become a selected custom preset and keep
/// command bindings and explicit disabled direct bindings intact.
#[test]
fn migrates_schema_56_custom_key_map_to_migrated_preset() {
    for (format, text) in [
        (
            ConfigFormat::Toml,
            "version = 56\n[keys]\nescape = \"C-b\"\nnew_window = \"A-n\"\n[keys.command_bindings]\nx = \"new-window\"\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":56,"keys":{"escape":"C-b","new_window":"A-n","new_group":null,"command_bindings":{"x":"new-window"}}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 56\nkeys:\n  escape: C-b\n  new_window: A-n\n  new_group: null\n  command_bindings:\n    x: new-window\n",
        ),
    ] {
        let plan = migrate_config_text(format, text).unwrap();
        let document = parse_config_json_value(format, &plan.text).unwrap();
        assert_eq!(
            document
                .pointer("/key_preset/active")
                .and_then(serde_json::Value::as_str),
            Some("migrated")
        );
        assert_eq!(
            document
                .pointer("/key_presets/migrated/escape")
                .and_then(serde_json::Value::as_str),
            Some("C-b")
        );
        assert_eq!(
            document
                .pointer("/key_presets/migrated/command_bindings/x")
                .and_then(serde_json::Value::as_str),
            Some("new-window")
        );
    }
}

/// Verifies schema v58 reconciles configs emitted by either branch that
/// independently claimed schema v57 without discarding an existing preset.
#[test]
fn migrates_both_colliding_schema_57_shapes() {
    for (format, term_only, preset_only) in [
        (
            ConfigFormat::Toml,
            "version = 57\n[terminal]\nterm = \"xterm-256color\"\n[keys]\nescape = \"C-b\"\n",
            "version = 57\n[terminal]\nterm = \"screen-256color\"\n[key_preset]\nactive = \"simple\"\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":57,"terminal":{"term":"xterm-256color"},"keys":{"escape":"C-b"}}"#,
            r#"{"version":57,"terminal":{"term":"screen-256color"},"key_preset":{"active":"simple"}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 57\nterminal:\n  term: xterm-256color\nkeys:\n  escape: C-b\n",
            "version: 57\nterminal:\n  term: screen-256color\nkey_preset:\n  active: simple\n",
        ),
    ] {
        let term_plan = migrate_config_text(format, term_only).unwrap();
        let term_document = parse_config_json_value(format, &term_plan.text).unwrap();
        assert_eq!(term_plan.from_version, 57);
        assert_eq!(term_plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            term_document
                .pointer("/terminal/term")
                .and_then(serde_json::Value::as_str),
            Some("xterm-256color")
        );
        assert_eq!(
            term_document
                .pointer("/key_preset/active")
                .and_then(serde_json::Value::as_str),
            Some("migrated")
        );

        let preset_plan = migrate_config_text(format, preset_only).unwrap();
        let preset_document = parse_config_json_value(format, &preset_plan.text).unwrap();
        assert_eq!(preset_plan.from_version, 57);
        assert_eq!(preset_plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            preset_document
                .pointer("/terminal/term")
                .and_then(serde_json::Value::as_str),
            Some("xterm-256color")
        );
        assert_eq!(
            preset_document
                .pointer("/key_preset/active")
                .and_then(serde_json::Value::as_str),
            Some("simple")
        );
    }
}

/// Verifies schema v59 accepts the optional model maximum-input ceiling while
/// advancing documents that predate the field without materializing a value.
#[test]
fn migrates_schema_58_maximum_input_limit_without_backfill() {
    for (format, text) in [
        (
            ConfigFormat::Toml,
            "version = 58\n[model_profiles.default]\nprovider = \"openai\"\nmodel = \"gpt-5.6-terra\"\nmax_input_tokens = 922000\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":58,"model_profiles":{"default":{"provider":"openai","model":"gpt-5.6-terra","max_input_tokens":922000}}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 58\nmodel_profiles:\n  default:\n    provider: openai\n    model: gpt-5.6-terra\n    max_input_tokens: 922000\n",
        ),
    ] {
        let plan = migrate_config_text(format, text).unwrap();
        let values = extract_config_values(format, &plan.text);
        assert_eq!(plan.from_version, 58);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("model_profiles.default.max_input_tokens"),
            Some(&"922000".to_string())
        );
    }
}

/// Verifies schema v60 adds the approval-attention theme pair in every
/// supported primary configuration format without changing unrelated colors.
#[test]
fn migrates_schema_59_with_approval_attention_colors() {
    for (format, text) in [
        (
            ConfigFormat::Toml,
            "version = 59\n[theme.colors]\nagent_status_blocked_bg = \"tertiary\"\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":59,"theme":{"colors":{"agent_status_blocked_bg":"tertiary"}}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 59\ntheme:\n  colors:\n    agent_status_blocked_bg: tertiary\n",
        ),
    ] {
        let plan = migrate_config_text(format, text).unwrap();
        let values = extract_config_values(format, &plan.text);

        assert_eq!(plan.from_version, 59);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("theme.colors.agent_approval_attention_fg"),
            Some(&"danger_text".to_string())
        );
        assert_eq!(
            values.get("theme.colors.agent_approval_attention_bg"),
            Some(&"danger".to_string())
        );
        assert_eq!(
            values.get("theme.colors.agent_status_blocked_bg"),
            Some(&"tertiary".to_string())
        );
    }
}

/// Verifies schema v61 materializes finite host clipboard read bounds in all
/// supported primary configuration formats.
#[test]
fn migrates_schema_60_with_host_clipboard_read_bounds() {
    for (format, text) in [
        (
            ConfigFormat::Toml,
            "version = 60\n[terminal]\nclipboard = \"external\"\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":60,"terminal":{"clipboard":"external"}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 60\nterminal:\n  clipboard: external\n",
        ),
    ] {
        let plan = migrate_config_text(format, text).unwrap();
        let values = extract_config_values(format, &plan.text);

        assert_eq!(plan.from_version, 60);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("terminal.clipboard_read_timeout_ms"),
            Some(&"250".to_string())
        );
        assert_eq!(
            values.get("terminal.clipboard_read_max_bytes"),
            Some(&"1048576".to_string())
        );
        assert_eq!(
            values.get("terminal.clipboard"),
            Some(&"external".to_string())
        );
    }
}

/// Verifies schema v62 materializes finite scheduler queue admission bounds in
/// all supported primary configuration formats without replacing neighboring
/// agent settings.
#[test]
fn migrates_schema_61_with_agent_scheduler_queue_bounds() {
    for (format, text) in [
        (
            ConfigFormat::Toml,
            "version = 61\n[agents]\nmax_concurrent_agents = 2\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":61,"agents":{"max_concurrent_agents":2}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 61\nagents:\n  max_concurrent_agents: 2\n",
        ),
    ] {
        let plan = migrate_config_text(format, text).unwrap();
        let values = extract_config_values(format, &plan.text);

        assert_eq!(plan.from_version, 61);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("agents.max_queued_turns"),
            Some(&"256".to_string())
        );
        assert_eq!(
            values.get("agents.max_queued_bytes"),
            Some(&"4194304".to_string())
        );
        assert_eq!(
            values.get("agents.max_concurrent_agents"),
            Some(&"2".to_string())
        );
    }
}

/// Verifies schema v63 exposes the plan-only status pill exactly once in every
/// supported format, including configurations that omitted or customized the
/// pane visible-field list.
#[test]
fn migrates_schema_62_with_agent_planning_visible_field() {
    for (format, absent, explicit, already_present) in [
        (
            ConfigFormat::Toml,
            "version = 62\n",
            "version = 62\n[frames.pane]\nvisible_fields = [\"agent.reasoning\", \"agent.thinking\", \"agent.routing\"]\n",
            "version = 62\n[frames.pane]\nvisible_fields = [\"agent.thinking\", \"agent.planning\", \"agent.routing\"]\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":62}"#,
            r#"{"version":62,"frames":{"pane":{"visible_fields":["agent.reasoning","agent.thinking","agent.routing"]}}}"#,
            r#"{"version":62,"frames":{"pane":{"visible_fields":["agent.thinking","agent.planning","agent.routing"]}}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 62\n",
            "version: 62\nframes:\n  pane:\n    visible_fields: [agent.reasoning, agent.thinking, agent.routing]\n",
            "version: 62\nframes:\n  pane:\n    visible_fields: [agent.thinking, agent.planning, agent.routing]\n",
        ),
    ] {
        for text in [absent, explicit, already_present] {
            let plan = migrate_config_text(format, text).unwrap();
            let migrated = parse_config_json_value(format, &plan.text).unwrap();
            let fields = migrated
                .pointer("/frames/pane/visible_fields")
                .and_then(serde_json::Value::as_array)
                .unwrap();
            let planning_indices = fields
                .iter()
                .enumerate()
                .filter_map(|(index, value)| {
                    (value.as_str() == Some("agent.planning")).then_some(index)
                })
                .collect::<Vec<_>>();
            let thinking_index = fields
                .iter()
                .position(|value| value.as_str() == Some("agent.thinking"))
                .unwrap();

            assert_eq!(plan.from_version, 62);
            assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
            assert_eq!(planning_indices, vec![thinking_index + 1]);
            let validation = validate_config_text(format, &plan.text, ConfigScope::Primary);
            assert!(validation.valid, "{:?}", validation.diagnostics);
        }
    }
}

/// Verifies schema v64 preserves established live provider-output rendering
/// while retaining neighboring terminal policy in every supported format.
#[test]
fn migrates_schema_63_with_streaming_output_enabled() {
    for (format, text) in [
        (
            ConfigFormat::Toml,
            "version = 63\n[terminal]\nreduced_motion = false\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":63,"terminal":{"reduced_motion":false}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 63\nterminal:\n  reduced_motion: false\n",
        ),
    ] {
        let plan = migrate_config_text(format, text).unwrap();
        let values = extract_config_values(format, &plan.text);

        assert_eq!(plan.from_version, 63);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("terminal.streaming_output"),
            Some(&"true".to_string())
        );
        assert_eq!(
            values.get("terminal.reduced_motion"),
            Some(&"false".to_string())
        );
    }
}

/// Verifies schema v65 materializes the historical 30-minute agent-turn
/// deadline in every supported format while retaining neighboring settings.
#[test]
fn migrates_schema_64_with_agent_turn_timeout() {
    for (format, text) in [
        (
            ConfigFormat::Toml,
            "version = 64\n[agents]\naction_failure_retry_limit = 2\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":64,"agents":{"action_failure_retry_limit":2}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 64\nagents:\n  action_failure_retry_limit: 2\n",
        ),
    ] {
        let plan = migrate_config_text(format, text).unwrap();
        let values = extract_config_values(format, &plan.text);

        assert_eq!(plan.from_version, 64);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("agents.turn_timeout_ms"),
            Some(&"1800000".to_string())
        );
        assert_eq!(
            values.get("agents.action_failure_retry_limit"),
            Some(&"2".to_string())
        );
    }
}

/// Verifies schema v65 retains a timeout explicitly declared before the key
/// became part of the schema instead of replacing user intent with the default.
#[test]
fn migrates_schema_64_without_overwriting_explicit_agent_turn_timeout() {
    for (format, text) in [
        (
            ConfigFormat::Toml,
            "version = 64\n[agents]\nturn_timeout_ms = 900000\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":64,"agents":{"turn_timeout_ms":900000}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 64\nagents:\n  turn_timeout_ms: 900000\n",
        ),
    ] {
        let plan = migrate_config_text(format, text).unwrap();
        let values = extract_config_values(format, &plan.text);

        assert_eq!(plan.from_version, 64);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("agents.turn_timeout_ms"),
            Some(&"900000".to_string())
        );
    }
}

/// Verifies schema v66 materializes the default native shell execution mode in
/// every supported format while retaining neighboring settings.
#[test]
fn migrates_schema_65_with_agent_shell_mode() {
    for (format, text) in [
        (
            ConfigFormat::Toml,
            "version = 65\n[agents]\naction_failure_retry_limit = 2\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":65,"agents":{"action_failure_retry_limit":2}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 65\nagents:\n  action_failure_retry_limit: 2\n",
        ),
    ] {
        let plan = migrate_config_text(format, text).unwrap();
        let values = extract_config_values(format, &plan.text);

        assert_eq!(plan.from_version, 65);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(values.get("agents.shell_mode"), Some(&"native".to_string()));
        assert_eq!(
            values.get("agents.action_failure_retry_limit"),
            Some(&"2".to_string())
        );
    }
}

/// Verifies schema v66 retains a mode declared before the key became part of
/// the schema instead of replacing user intent with the default.
#[test]
fn migrates_schema_65_without_overwriting_explicit_agent_shell_mode() {
    for (format, text) in [
        (
            ConfigFormat::Toml,
            "version = 65\n[agents]\nshell_mode = \"native\"\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":65,"agents":{"shell_mode":"native"}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 65\nagents:\n  shell_mode: native\n",
        ),
    ] {
        let plan = migrate_config_text(format, text).unwrap();
        let values = extract_config_values(format, &plan.text);

        assert_eq!(plan.from_version, 65);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(values.get("agents.shell_mode"), Some(&"native".to_string()));
    }
}

/// Verifies schema v67 materializes a disabled, conservative Iroh policy.
#[test]
fn migrates_schema_66_with_disabled_iroh_transport() {
    for (format, text) in [
        (
            ConfigFormat::Toml,
            "version = 66\n[runtime]\ncpu_count = 3\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":66,"runtime":{"cpu_count":3}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 66\nruntime:\n  cpu_count: 3\n",
        ),
    ] {
        let plan = migrate_config_text(format, text).unwrap();
        let values = extract_config_values(format, &plan.text);

        assert_eq!(plan.from_version, 66);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("transport.iroh.enabled"),
            Some(&"false".to_string())
        );
        assert_eq!(
            values.get("transport.iroh.address_lookup"),
            Some(&"disabled".to_string())
        );
        assert_eq!(
            values.get("transport.iroh.relay_mode"),
            Some(&"disabled".to_string())
        );
        assert_eq!(
            values.get("transport.iroh.port_mapping"),
            Some(&"false".to_string())
        );
        assert_eq!(values.get("runtime.cpu_count"), Some(&"3".to_string()));
    }
}

/// Verifies schema v68 normalizes the legacy configurable stream count to the
/// single bidirectional control stream enforced by the v1 Iroh protocol.
#[test]
fn migrates_schema_67_to_fixed_iroh_stream_limit() {
    assert_eq!(CURRENT_CONFIG_SCHEMA_VERSION, 83);
    for (format, text) in [
        (
            ConfigFormat::Toml,
            "version = 67\n[transport.iroh]\nmax_streams_per_connection = 2\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":67,"transport":{"iroh":{"max_streams_per_connection":16}}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 67\ntransport:\n  iroh:\n    max_streams_per_connection: 4\n",
        ),
    ] {
        let plan = migrate_config_text(format, text).unwrap();
        let values = extract_config_values(format, &plan.text);

        assert_eq!(plan.from_version, 67);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("transport.iroh.max_streams_per_connection"),
            Some(&"1".to_string())
        );
    }
}

/// Verifies schema v69 preserves explicit outbound Iroh target use while
/// keeping listener enablement disabled for clean client configurations.
#[test]
fn migrates_schema_68_with_separate_outbound_iroh_permission() {
    for (format, text) in [
        (
            ConfigFormat::Toml,
            "version = 68\n[transport.iroh]\nenabled = false\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":68,"transport":{"iroh":{"enabled":false}}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 68\ntransport:\n  iroh:\n    enabled: false\n",
        ),
    ] {
        let plan = migrate_config_text(format, text).unwrap();
        let values = extract_config_values(format, &plan.text);

        assert_eq!(plan.from_version, 68);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("transport.iroh.enabled"),
            Some(&"false".to_string())
        );
        assert_eq!(
            values.get("transport.iroh.outbound_enabled"),
            Some(&"true".to_string())
        );
    }
}

/// Verifies schema v70 adds an explicit stable Iroh bind-port setting without
/// changing the existing ephemeral behavior unless the owner configures it.
#[test]
fn migrates_schema_69_with_iroh_bind_port() {
    assert_eq!(CURRENT_CONFIG_SCHEMA_VERSION, 83);
    for (format, text) in [
        (
            ConfigFormat::Toml,
            "version = 69\n[transport.iroh]\nenabled = false\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":69,"transport":{"iroh":{"enabled":false}}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 69\ntransport:\n  iroh:\n    enabled: false\n",
        ),
    ] {
        let plan = migrate_config_text(format, text).unwrap();
        let values = extract_config_values(format, &plan.text);

        assert_eq!(plan.from_version, 69);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            values.get("transport.iroh.bind_port"),
            Some(&"0".to_string())
        );
    }
}

/// Verifies schema v71 materializes the ordered compression policy in every
/// supported primary configuration format without enabling the Iroh listener.
#[test]
fn migrates_schema_70_with_iroh_compression_defaults() {
    assert_eq!(CURRENT_CONFIG_SCHEMA_VERSION, 83);
    for (format, text) in [
        (
            ConfigFormat::Toml,
            "version = 70\n[transport.iroh]\nenabled = false\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":70,"transport":{"iroh":{"enabled":false}}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 70\ntransport:\n  iroh:\n    enabled: false\n",
        ),
    ] {
        let plan = migrate_config_text(format, text).unwrap();
        let values = extract_config_values(format, &plan.text);
        let root = parse_config_json_value(format, &plan.text).unwrap();

        assert_eq!(plan.from_version, 70);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            root.pointer("/transport/iroh/compression_codecs"),
            Some(&serde_json::json!(["zstd", "lz4", "none"]))
        );
        assert_eq!(
            values.get("transport.iroh.compression_min_bytes"),
            Some(&"512".to_string())
        );
        assert_eq!(
            values.get("transport.iroh.compression_zstd_level"),
            Some(&"3".to_string())
        );
        assert_eq!(
            values.get("transport.iroh.enabled"),
            Some(&"false".to_string())
        );
    }
}

/// Verifies schema v72 adds all Iroh status theme colors in TOML, JSON, and
/// YAML while retaining the caller's status template and Iroh listener policy.
#[test]
fn migrates_schema_71_with_iroh_status_theme_defaults() {
    for (format, text) in [
        (
            ConfigFormat::Toml,
            "version = 71\n[transport.iroh]\nenabled = false\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":71,"transport":{"iroh":{"enabled":false}}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 71\ntransport:\n  iroh:\n    enabled: false\n",
        ),
    ] {
        let plan = migrate_config_text(format, text).unwrap();
        let root = parse_config_json_value(format, &plan.text).unwrap();

        assert_eq!(plan.from_version, 71);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            root.pointer("/transport/iroh/enabled"),
            Some(&serde_json::json!(false))
        );
        assert_eq!(
            root.pointer("/theme/colors/iroh_status_good_fg"),
            Some(&serde_json::json!("primary_text"))
        );
        assert_eq!(
            root.pointer("/theme/colors/iroh_status_degraded_bg"),
            Some(&serde_json::json!("tertiary"))
        );
        assert_eq!(
            root.pointer("/theme/colors/iroh_status_poor_bg"),
            Some(&serde_json::json!("danger"))
        );
        assert_eq!(
            root.pointer("/theme/colors/iroh_status_unknown_fg"),
            Some(&serde_json::json!("muted_text"))
        );
    }
}

/// Schema v73 must add disabled host and lease defaults in every supported
/// format without enabling inbound Iroh or moving protected identity material.
#[test]
fn migrates_schema_72_with_disabled_persistent_host_defaults() {
    for (format, text) in [
        (
            ConfigFormat::Toml,
            "version = 72\n[transport.iroh]\nenabled = false\nidentity = \"per_session\"\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":72,"transport":{"iroh":{"enabled":false,"identity":"per_session"}}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 72\ntransport:\n  iroh:\n    enabled: false\n    identity: per_session\n",
        ),
    ] {
        let plan = migrate_config_text(format, text).unwrap();
        let root = parse_config_json_value(format, &plan.text).unwrap();

        assert_eq!(plan.from_version, 72);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(
            root.pointer("/host/enabled"),
            Some(&serde_json::json!(false))
        );
        assert_eq!(
            root.pointer("/host/auto_start_local"),
            Some(&serde_json::json!(true))
        );
        assert_eq!(
            root.pointer("/host/max_sessions"),
            Some(&serde_json::json!(64))
        );
        assert_eq!(
            root.pointer("/host/leases/max_per_remote_client"),
            Some(&serde_json::json!(8))
        );
        assert_eq!(
            root.pointer("/transport/iroh/enabled"),
            Some(&serde_json::json!(false))
        );
        assert_eq!(
            root.pointer("/transport/iroh/identity"),
            Some(&serde_json::json!("host"))
        );
    }
}

/// Schema v74 changes only the declared version and preserves every existing
/// sandbox and approval decision or omission in all supported formats.
#[test]
fn migrates_schema_73_without_enabling_seatbelt_or_changing_policy() {
    for (format, text, expected_sandbox, expected_approval) in [
        (
            ConfigFormat::Toml,
            "version = 73\n[permissions]\nsandbox = \"policy-only\"\napproval_policy = \"auto-allow\"\n",
            Some("policy-only"),
            Some("auto-allow"),
        ),
        (
            ConfigFormat::Json,
            r#"{"version":73,"permissions":{"sandbox":"bubblewrap","approval_policy":"full-access"}}"#,
            Some("bubblewrap"),
            Some("full-access"),
        ),
        (
            ConfigFormat::Yaml,
            "version: 73\npermissions: {}\n",
            None,
            None,
        ),
    ] {
        let plan = migrate_config_text(format, text).unwrap();
        let values = extract_config_values(format, &plan.text);

        assert_eq!(plan.from_version, 73);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(values.get("version"), Some(&"83".to_string()));
        assert_eq!(
            values.get("permissions.sandbox").map(String::as_str),
            expected_sandbox
        );
        assert_eq!(
            values
                .get("permissions.approval_policy")
                .map(String::as_str),
            expected_approval
        );
        assert!(
            !values
                .keys()
                .any(|path| path.starts_with("permissions.seatbelt"))
        );
    }
}

/// Schema v75 expands only the accepted Iroh codec vocabulary and preserves
/// every existing transport preference in all supported configuration formats.
///
/// Streaming codecs remain opt-in during migration: old codec lists are not
/// reordered, and an explicit v74 streaming list is retained byte-for-value.
#[test]
fn migrates_schema_74_without_enabling_streaming_compression() {
    for (format, text, expected_codecs) in [
        (
            ConfigFormat::Toml,
            "version = 74\n[transport.iroh]\ncompression_codecs = [\"zstd\", \"lz4\", \"none\"]\n",
            serde_json::json!(["zstd", "lz4", "none"]),
        ),
        (
            ConfigFormat::Json,
            r#"{"version":74,"transport":{"iroh":{"compression_codecs":["lz4-stream","zstd-stream","none"]}}}"#,
            serde_json::json!(["lz4-stream", "zstd-stream", "none"]),
        ),
        (
            ConfigFormat::Yaml,
            "version: 74\ntransport:\n  iroh:\n    compression_codecs:\n      - none\n",
            serde_json::json!(["none"]),
        ),
    ] {
        let plan = migrate_config_text(format, text).unwrap();
        let root = parse_config_json_value(format, &plan.text).unwrap();

        assert_eq!(plan.from_version, 74);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(CURRENT_CONFIG_SCHEMA_VERSION, 83);
        assert_eq!(
            root.pointer("/transport/iroh/compression_codecs"),
            Some(&expected_codecs)
        );
    }
}

/// Schema v76 removes padding only from the exact historical pane-title
/// default while preserving custom and missing templates in every format.
#[test]
fn migrates_schema_75_pane_title_template_to_renderer_owned_padding() {
    for (format, exact_default, custom, missing) in [
        (
            ConfigFormat::Toml,
            "version = 75\n[frames.pane]\ntemplate = \" #{pane.index} #{pane.title} \"\n",
            "version = 75\n[frames.pane]\ntemplate = \" [#{pane.title}] \"\n",
            "version = 75\n[frames.pane]\nstyle = \"default\"\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":75,"frames":{"pane":{"template":" #{pane.index} #{pane.title} "}}}"#,
            r#"{"version":75,"frames":{"pane":{"template":" [#{pane.title}] "}}}"#,
            r#"{"version":75,"frames":{"pane":{"style":"default"}}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 75\nframes:\n  pane:\n    template: ' #{pane.index} #{pane.title} '\n",
            "version: 75\nframes:\n  pane:\n    template: ' [#{pane.title}] '\n",
            "version: 75\nframes:\n  pane:\n    style: default\n",
        ),
    ] {
        let exact = parse_config_json_value(
            format,
            &migrate_config_text(format, exact_default).unwrap().text,
        )
        .unwrap();
        let custom =
            parse_config_json_value(format, &migrate_config_text(format, custom).unwrap().text)
                .unwrap();
        let missing =
            parse_config_json_value(format, &migrate_config_text(format, missing).unwrap().text)
                .unwrap();

        assert_eq!(exact.pointer("/version"), Some(&serde_json::json!(83)));
        assert_eq!(
            exact.pointer("/frames/pane/template"),
            Some(&serde_json::json!("#{pane.index} #{pane.title}"))
        );
        assert_eq!(
            custom.pointer("/frames/pane/template"),
            Some(&serde_json::json!(" [#{pane.title}] "))
        );
        assert_eq!(missing.pointer("/frames/pane/template"), None);
    }
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

/// Schema v77 converts provider model-id arrays into keyed metadata records in
/// every supported format while preserving profile-local policy unchanged.
///
/// Generated keys are path-safe and receive deterministic numeric suffixes
/// when different model ids normalize to the same key.
#[test]
fn migrates_schema_76_provider_models_to_structured_records() {
    for (format, text) in [
        (
            ConfigFormat::Toml,
            "version = 76\n[providers.custom]\nmodels = [\"alpha/model\", \"alpha.model\", \"plain\"]\ndefault_model = \"alpha/model\"\n[model_profiles.work]\nprovider = \"custom\"\nmodel = \"alpha/model\"\ncontext_window_tokens = 123456\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":76,"providers":{"custom":{"models":["alpha/model","alpha.model","plain"],"default_model":"alpha/model"}},"model_profiles":{"work":{"provider":"custom","model":"alpha/model","context_window_tokens":123456}}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 76\nproviders:\n  custom:\n    models:\n      - alpha/model\n      - alpha.model\n      - plain\n    default_model: alpha/model\nmodel_profiles:\n  work:\n    provider: custom\n    model: alpha/model\n    context_window_tokens: 123456\n",
        ),
    ] {
        let plan = migrate_config_text(format, text).unwrap();
        let root = parse_config_json_value(format, &plan.text).unwrap();

        assert_eq!(plan.from_version, 76);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(CURRENT_CONFIG_SCHEMA_VERSION, 83);
        assert_eq!(
            root.pointer("/providers/custom/models/alpha-model/id"),
            Some(&serde_json::json!("alpha/model"))
        );
        assert_eq!(
            root.pointer("/providers/custom/models/alpha-model-2/id"),
            Some(&serde_json::json!("alpha.model"))
        );
        assert_eq!(
            root.pointer("/providers/custom/models/plain/id"),
            Some(&serde_json::json!("plain"))
        );
        assert_eq!(
            root.pointer("/providers/custom/default_model"),
            Some(&serde_json::json!("alpha/model"))
        );
        assert_eq!(
            root.pointer("/model_profiles/work/context_window_tokens"),
            Some(&serde_json::json!(123456))
        );
    }
}

/// Schema v78 establishes active saved-session age and count defaults while
/// preserving an explicitly customized count in every primary config format.
#[test]
fn migrates_schema_77_saved_session_retention_defaults() {
    for (format, missing, legacy_default, custom_count) in [
        (
            ConfigFormat::Toml,
            "version = 77\n",
            "version = 77\n[history]\nsaved_sessions_limit = 100\n",
            "version = 77\n[history]\nsaved_sessions_limit = 321\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":77}"#,
            r#"{"version":77,"history":{"saved_sessions_limit":100}}"#,
            r#"{"version":77,"history":{"saved_sessions_limit":321}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 77\n",
            "version: 77\nhistory:\n  saved_sessions_limit: 100\n",
            "version: 77\nhistory:\n  saved_sessions_limit: 321\n",
        ),
    ] {
        let migrated_missing =
            parse_config_json_value(format, &migrate_config_text(format, missing).unwrap().text)
                .unwrap();
        let migrated_default = parse_config_json_value(
            format,
            &migrate_config_text(format, legacy_default).unwrap().text,
        )
        .unwrap();
        let migrated_custom = parse_config_json_value(
            format,
            &migrate_config_text(format, custom_count).unwrap().text,
        )
        .unwrap();

        assert_eq!(
            migrated_missing.pointer("/version"),
            Some(&serde_json::json!(83))
        );
        assert_eq!(
            migrated_missing.pointer("/history/saved_sessions_limit"),
            Some(&serde_json::json!(10_000))
        );
        assert_eq!(
            migrated_missing.pointer("/history/saved_sessions_retention_days"),
            Some(&serde_json::json!(90))
        );
        assert_eq!(
            migrated_default.pointer("/version"),
            Some(&serde_json::json!(83))
        );
        assert_eq!(
            migrated_default.pointer("/history/saved_sessions_limit"),
            Some(&serde_json::json!(10_000))
        );
        assert_eq!(
            migrated_default.pointer("/history/saved_sessions_retention_days"),
            Some(&serde_json::json!(90))
        );
        assert_eq!(
            migrated_custom.pointer("/history/saved_sessions_limit"),
            Some(&serde_json::json!(321))
        );
        assert_eq!(
            migrated_custom.pointer("/history/saved_sessions_retention_days"),
            Some(&serde_json::json!(90))
        );
    }
}

/// Schema v79 adds the portable structured external-editor candidates without
/// changing existing key-preset selection or prompt behavior.
#[test]
fn migrates_schema_78_external_editor_defaults() {
    for (format, text) in [
        (ConfigFormat::Toml, "version = 78\n"),
        (ConfigFormat::Json, r#"{"version":78}"#),
        (ConfigFormat::Yaml, "version: 78\n"),
    ] {
        let plan = migrate_config_text(format, text).unwrap();
        let root = parse_config_json_value(format, &plan.text).unwrap();

        assert_eq!(plan.from_version, 78);
        assert_eq!(plan.to_version, 83);
        assert_eq!(CURRENT_CONFIG_SCHEMA_VERSION, 83);
        assert_eq!(
            root.pointer("/external_editor/command"),
            Some(&serde_json::json!(["editor", "{file}"]))
        );
        assert_eq!(
            root.pointer("/external_editor/fallback"),
            Some(&serde_json::json!([
                ["vim", "{file}"],
                ["nano", "{file}"],
                ["vi", "{file}"]
            ]))
        );
    }
}

/// Schema v80 adds a disabled X11 forwarding policy in every supported config
/// format without changing the caller's existing Iroh listener policy.
#[test]
fn migrates_schema_79_disabled_x11_policy() {
    for (format, text) in [
        (
            ConfigFormat::Toml,
            "version = 79\n[transport.iroh]\nenabled = true\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":79,"transport":{"iroh":{"enabled":true}}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 79\ntransport:\n  iroh:\n    enabled: true\n",
        ),
    ] {
        let plan = migrate_config_text(format, text).unwrap();
        let root = parse_config_json_value(format, &plan.text).unwrap();

        assert_eq!(plan.from_version, 79);
        assert_eq!(plan.to_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(root.pointer("/version"), Some(&serde_json::json!(83)));
        assert_eq!(
            root.pointer("/transport/iroh/enabled"),
            Some(&serde_json::json!(true))
        );
        assert_eq!(
            root.pointer("/transport/iroh/x11/enabled"),
            Some(&serde_json::json!(false))
        );
        assert_eq!(
            root.pointer("/transport/iroh/x11/allow_trusted"),
            Some(&serde_json::json!(false))
        );
        assert_eq!(
            root.pointer("/transport/iroh/x11/max_connections_per_route"),
            Some(&serde_json::json!(16))
        );
        assert_eq!(
            root.pointer("/transport/iroh/x11/setup_timeout_ms"),
            Some(&serde_json::json!(5000))
        );
    }
}

/// Schema v81 adds finite provider retry defaults and a separate disabled
/// unlimited mode without overwriting an explicit retry count.
#[test]
fn migrates_schema_80_provider_retry_policy_defaults() {
    for (format, missing, configured) in [
        (
            ConfigFormat::Toml,
            "version = 80\n",
            "version = 80\n[agents]\nprovider_error_retry_limit = 9\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":80}"#,
            r#"{"version":80,"agents":{"provider_error_retry_limit":9}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 80\n",
            "version: 80\nagents:\n  provider_error_retry_limit: 9\n",
        ),
    ] {
        let missing =
            parse_config_json_value(format, &migrate_config_text(format, missing).unwrap().text)
                .unwrap();
        let configured = parse_config_json_value(
            format,
            &migrate_config_text(format, configured).unwrap().text,
        )
        .unwrap();

        assert_eq!(missing.pointer("/version"), Some(&serde_json::json!(83)));
        assert_eq!(
            missing.pointer("/agents/provider_error_retry_limit"),
            Some(&serde_json::json!(5))
        );
        assert_eq!(
            missing.pointer("/agents/provider_error_retry_unlimited"),
            Some(&serde_json::json!(false))
        );
        assert_eq!(
            configured.pointer("/agents/provider_error_retry_limit"),
            Some(&serde_json::json!(9))
        );
        assert_eq!(
            configured.pointer("/agents/provider_error_retry_unlimited"),
            Some(&serde_json::json!(false))
        );
    }
}

/// Schema v82 materializes the complete static action allowlist without
/// overwriting an allowlist already declared by the user.
#[test]
fn migrates_schema_81_static_enabled_actions() {
    for (format, missing, configured) in [
        (
            ConfigFormat::Toml,
            "version = 81\n",
            "version = 81\n[agents]\nenabled_actions = [\"say\"]\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":81}"#,
            r#"{"version":81,"agents":{"enabled_actions":["say"]}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 81\n",
            "version: 81\nagents:\n  enabled_actions:\n    - say\n",
        ),
    ] {
        let missing =
            parse_config_json_value(format, &migrate_config_text(format, missing).unwrap().text)
                .unwrap();
        let configured = parse_config_json_value(
            format,
            &migrate_config_text(format, configured).unwrap().text,
        )
        .unwrap();

        assert_eq!(missing.pointer("/version"), Some(&serde_json::json!(83)));
        assert_eq!(
            missing.pointer("/agents/enabled_actions"),
            Some(&serde_json::json!([
                "say",
                "shell_command",
                "apply_patch",
                "web_search",
                "fetch_url",
                "send_message",
                "spawn_agent",
                "config_change",
                "mcp_call",
                "memory_search",
                "memory_store",
                "issue_add",
                "issue_update",
                "issue_query",
                "issue_delete"
            ]))
        );
        assert_eq!(
            configured.pointer("/agents/enabled_actions"),
            Some(&serde_json::json!(["say"]))
        );
    }
}

/// Schema v83 adds optional terminal subagent profiles without changing
/// existing profiles' recursive delegation behavior.
#[test]
fn migrates_schema_82_terminal_subagent_profiles() {
    for (format, text) in [
        (
            ConfigFormat::Toml,
            "version = 82\n[subagents.worker]\nterminal = true\n",
        ),
        (
            ConfigFormat::Json,
            r#"{"version":82,"subagents":{"worker":{"terminal":true}}}"#,
        ),
        (
            ConfigFormat::Yaml,
            "version: 82\nsubagents:\n  worker:\n    terminal: true\n",
        ),
    ] {
        let plan = migrate_config_text(format, text).unwrap();
        let root = parse_config_json_value(format, &plan.text).unwrap();

        assert_eq!(plan.from_version, 82);
        assert_eq!(plan.to_version, 83);
        assert_eq!(root.pointer("/version"), Some(&serde_json::json!(83)));
        assert_eq!(
            root.pointer("/subagents/worker/terminal"),
            Some(&serde_json::json!(true))
        );
    }
}
