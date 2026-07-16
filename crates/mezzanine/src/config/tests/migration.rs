//! Config migration tests.

use super::*;

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
    assert!(plan.text.contains("version = 19"));
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
    assert_eq!(values.get("version"), Some(&"19".to_string()));
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
    assert_eq!(values.get("version"), Some(&"19".to_string()));
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
    assert_eq!(values.get("version"), Some(&"19".to_string()));
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
    assert_eq!(values.get("version"), Some(&"19".to_string()));
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
    assert_eq!(missing_values.get("version"), Some(&"19".to_string()));
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
    assert_eq!(explicit_values.get("version"), Some(&"19".to_string()));
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
    assert_eq!(missing_values.get("version"), Some(&"19".to_string()));
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
    assert_eq!(explicit_values.get("version"), Some(&"19".to_string()));
    assert_eq!(
        explicit_values.get("terminal.agent_wrap_column_cap"),
        Some(&"96".to_string())
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
