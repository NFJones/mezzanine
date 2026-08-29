//! Config defaults tests.

use super::*;
use crate::config::defaults::{GeneratedConfigPlatform, initial_config_toml_for_platform};
use crate::config::initial_config_toml;

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
    let config = fs::read_to_string(path).unwrap();
    assert_eq!(config, initial_config_toml().unwrap());
    assert!(!config.contains("[providers."), "{config}");
    assert!(!config.contains("[model_profiles."), "{config}");
    assert!(root.join("macros").is_dir());

    let _ = fs::remove_dir_all(root);
}

/// Verifies generated defaults leave pane-title pill padding to the renderer.
///
/// This guards the first-run config path from embedding visual chrome in a
/// semantic template and thereby receiving duplicate renderer-owned padding.
#[test]
fn default_config_pane_frame_template_uses_raw_title_content() {
    assert!(
        DEFAULT_CONFIG_TOML.contains("template = \"#{pane.index} #{pane.title}\""),
        "{DEFAULT_CONFIG_TOML}"
    );
}

/// Verifies generated configuration declares the active saved-session age and
/// count policy explicitly so first-run and omitted-value behavior agree.
#[test]
fn default_config_uses_time_and_count_saved_session_retention() {
    let parsed: toml::Value = toml::from_str(DEFAULT_CONFIG_TOML).unwrap();
    let history = parsed
        .get("history")
        .and_then(toml::Value::as_table)
        .unwrap();

    assert_eq!(
        history
            .get("saved_sessions_limit")
            .and_then(toml::Value::as_integer),
        Some(10_000)
    );
    assert_eq!(
        history
            .get("saved_sessions_retention_days")
            .and_then(toml::Value::as_integer),
        Some(90)
    );
}

/// Verifies enhanced keyboard reporting remains an explicit opt-in in newly
/// generated configuration files.
#[test]
fn default_config_disables_enhanced_keyboard_reporting() {
    assert!(
        DEFAULT_CONFIG_TOML.contains("enhanced_keyboard_reporting = false"),
        "{DEFAULT_CONFIG_TOML}"
    );
}

/// Verifies active-turn sleep inhibition remains disabled until the primary
/// user explicitly chooses a host power policy in generated configuration.
#[test]
fn default_config_disables_active_turn_sleep_inhibition() {
    assert!(
        DEFAULT_CONFIG_TOML.contains("active_turn_sleep_inhibition = \"disabled\""),
        "{DEFAULT_CONFIG_TOML}"
    );
}

/// Generated schema-v73 configuration must retain local-only startup posture
/// while declaring the host-scoped identity used by the persistent host.
#[test]
fn default_config_disables_persistent_host_and_inbound_iroh() {
    let parsed: toml::Value = toml::from_str(DEFAULT_CONFIG_TOML).unwrap();
    let host = parsed.get("host").and_then(toml::Value::as_table).unwrap();
    let iroh = parsed
        .get("transport")
        .and_then(toml::Value::as_table)
        .and_then(|transport| transport.get("iroh"))
        .and_then(toml::Value::as_table)
        .unwrap();

    assert_eq!(
        host.get("enabled").and_then(toml::Value::as_bool),
        Some(false)
    );
    assert_eq!(
        iroh.get("enabled").and_then(toml::Value::as_bool),
        Some(false)
    );
    assert_eq!(
        iroh.get("identity").and_then(toml::Value::as_str),
        Some("host")
    );
}

/// Verifies generated configuration selects native shell execution and
/// platform-appropriate sandbox confinement for first-run users.
#[test]
fn initial_config_uses_native_shell_and_platform_sandbox_defaults() {
    let parsed: toml::Value = toml::from_str(&initial_config_toml().unwrap()).unwrap();
    let agents = parsed
        .get("agents")
        .and_then(toml::Value::as_table)
        .unwrap();
    let permissions = parsed
        .get("permissions")
        .and_then(toml::Value::as_table)
        .unwrap();

    assert_eq!(
        agents.get("shell_mode").and_then(toml::Value::as_str),
        Some("native")
    );
    assert_eq!(
        permissions.get("sandbox").and_then(toml::Value::as_str),
        Some(
            crate::security::sandbox::SandboxPlatformAvailability::current().default_sandbox_name()
        )
    );
}

/// Verifies newly generated macOS configuration enables Seatbelt with
/// full-access approval when the fixed executable is present.
#[test]
fn initial_macos_config_uses_full_access_with_available_seatbelt() {
    let config = initial_config_toml_for_platform(GeneratedConfigPlatform::MacOs {
        seatbelt_available: true,
    })
    .unwrap();
    let parsed: toml::Value = toml::from_str(&config).unwrap();
    let permissions = parsed
        .get("permissions")
        .and_then(toml::Value::as_table)
        .unwrap();

    assert_eq!(
        permissions
            .get("approval_policy")
            .and_then(toml::Value::as_str),
        Some("full-access")
    );
    assert_eq!(
        permissions.get("sandbox").and_then(toml::Value::as_str),
        Some("seatbelt")
    );
}

/// Verifies newly generated macOS configuration pairs model-gated automatic
/// approval with policy-only execution when the fixed executable is absent.
///
/// Keeping both values in one platform-targeted regression prevents first-run
/// macOS configuration from selecting an unavailable fail-closed backend.
#[test]
fn initial_macos_config_uses_auto_allow_with_policy_only_sandboxing() {
    let config = initial_config_toml_for_platform(GeneratedConfigPlatform::MacOs {
        seatbelt_available: false,
    })
    .unwrap();
    let parsed: toml::Value = toml::from_str(&config).unwrap();
    let permissions = parsed
        .get("permissions")
        .and_then(toml::Value::as_table)
        .unwrap();

    assert_eq!(
        permissions
            .get("approval_policy")
            .and_then(toml::Value::as_str),
        Some("auto-allow")
    );
    assert_eq!(
        permissions.get("sandbox").and_then(toml::Value::as_str),
        Some("policy-only")
    );
}

/// Verifies newly generated Linux configuration uses unrestricted approval
/// inside Bubblewrap when the configured executable is available.
///
/// The availability input is injected so this contract remains testable on
/// builders that do not have Bubblewrap installed at the code-owned path.
#[test]
fn initial_linux_config_uses_full_access_with_available_bubblewrap() {
    let config = initial_config_toml_for_platform(GeneratedConfigPlatform::Linux {
        bubblewrap_available: true,
    })
    .unwrap();
    let parsed: toml::Value = toml::from_str(&config).unwrap();
    let permissions = parsed
        .get("permissions")
        .and_then(toml::Value::as_table)
        .unwrap();

    assert_eq!(
        permissions
            .get("approval_policy")
            .and_then(toml::Value::as_str),
        Some("full-access")
    );
    assert_eq!(
        permissions.get("sandbox").and_then(toml::Value::as_str),
        Some("bubblewrap")
    );
}

/// Verifies newly generated Linux configuration falls back to model-gated
/// approval and policy-only execution when Bubblewrap is unavailable.
///
/// Selecting both values together prevents an unusable fail-closed Bubblewrap
/// backend from being persisted into first-run configuration.
#[test]
fn initial_linux_config_uses_auto_allow_without_bubblewrap() {
    let config = initial_config_toml_for_platform(GeneratedConfigPlatform::Linux {
        bubblewrap_available: false,
    })
    .unwrap();
    let parsed: toml::Value = toml::from_str(&config).unwrap();
    let permissions = parsed
        .get("permissions")
        .and_then(toml::Value::as_table)
        .unwrap();

    assert_eq!(
        permissions
            .get("approval_policy")
            .and_then(toml::Value::as_str),
        Some("auto-allow")
    );
    assert_eq!(
        permissions.get("sandbox").and_then(toml::Value::as_str),
        Some("policy-only")
    );
}

/// Verifies platforms without a supported native backend retain the
/// conservative ask plus policy-only default pair.
#[test]
fn initial_other_platform_config_uses_ask_with_policy_only_sandboxing() {
    let config = initial_config_toml_for_platform(GeneratedConfigPlatform::Other).unwrap();
    let parsed: toml::Value = toml::from_str(&config).unwrap();
    let permissions = parsed
        .get("permissions")
        .and_then(toml::Value::as_table)
        .unwrap();

    assert_eq!(
        permissions
            .get("approval_policy")
            .and_then(toml::Value::as_str),
        Some("ask")
    );
    assert_eq!(
        permissions.get("sandbox").and_then(toml::Value::as_str),
        Some("policy-only")
    );
}

/// Verifies the first-run configuration does not retain references to model
/// profiles that are deliberately withheld until provider authentication.
///
/// The runtime synthesizes the `default` OpenAI profile before a provider
/// catalog exists. Routing must therefore use that resolvable profile rather
/// than a provider-specific auto-sizing profile that is absent from the
/// generated first-run document.
#[test]
fn initial_config_uses_resolvable_auto_sizing_profiles() {
    let parsed: toml::Value = toml::from_str(&initial_config_toml().unwrap()).unwrap();
    let auto_sizing = parsed
        .get("agents")
        .and_then(toml::Value::as_table)
        .and_then(|agents| agents.get("auto_sizing"))
        .and_then(toml::Value::as_table)
        .unwrap();

    for key in [
        "router_model_profile",
        "small_model_profile",
        "medium_model_profile",
        "large_model_profile",
    ] {
        assert_eq!(
            auto_sizing.get(key).and_then(toml::Value::as_str),
            Some("default"),
            "{key} must resolve before provider defaults are materialized"
        );
    }
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
        initial_config_toml().unwrap()
    );
    assert!(root.join("macros").is_dir());

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
        initial_config_toml().unwrap()
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
    let documented = include_str!("../../../../../docs/examples/config.toml");
    let platform = crate::security::sandbox::SandboxPlatformAvailability::current();

    let documented = documented.replace(
        "sandbox = \"bubblewrap\"",
        &format!("sandbox = \"{}\"", platform.default_sandbox_name()),
    );
    let documented = documented.replace(
        "approval_policy = \"ask\"",
        &format!(
            "approval_policy = \"{}\"",
            platform.default_approval_policy_name()
        ),
    );
    assert_eq!(initial_config_toml().unwrap().trim(), documented.trim());
}

/// Verifies first-launch configuration documents every supported non-provider
/// surface while keeping provider catalogs reserved for successful auth login.
///
/// Active defaults and optional examples must both remain discoverable in the
/// generated file, and every section must explain its purpose rather than
/// presenting an unexplained collection of settings.
#[test]
fn initial_config_is_complete_annotated_and_provider_free() {
    let config = initial_config_toml().unwrap();

    for excluded in ["[providers.", "[model_profiles.", "[model_presets."] {
        assert!(
            !config.contains(excluded),
            "unexpected {excluded} in:\n{config}"
        );
    }

    for section in [
        "[runtime]",
        "[terminal]",
        "[keys]",
        "[keys.command_bindings]",
        "[key_preset]",
        "# [key_presets.custom]",
        "[frames.window]",
        "[frames.window.pills]",
        "# [frames.window.pills.example]",
        "[frames.pane]",
        "[theme]",
        "[theme.aliases]",
        "[theme.colors]",
        "# [themes.custom.aliases]",
        "# [themes.custom.colors]",
        "[history]",
        "[memory]",
        "[issues]",
        "[agents]",
        "[agents.auto_sizing]",
        "[subagents]",
        "# [subagents.reviewer]",
        "[personalities]",
        "# [personalities.concise]",
        "[permissions]",
        "# [permissions.bubblewrap]",
        "# [[permissions.command_rules]]",
        "[mcp_servers]",
        "# [mcp_servers.example]",
        "[auth]",
        "[instructions]",
        "[hooks]",
        "# [hooks.example]",
        "[audit]",
        "[extensions]",
    ] {
        assert!(config.contains(section), "missing {section} in:\n{config}");
    }

    for setting in [
        "# split_vertical =",
        "# focus_next_group =",
        "# clipboard_copy_command =",
        "# read_scopes =",
        "# write_scopes =",
        "# preset =",
        "# executable =",
        "# unavailable =",
        "# group_whitelist =",
        "# git_user_email =",
        "# default_cooperation_mode =",
        "# planning_enabled =",
        "# startup_timeout_sec =",
        "# startup_timeout_ms =",
        "# tool_timeout_sec =",
        "# tool_timeout_ms =",
        "# read_file =",
        "# timeout_sec =",
        "# inject_instructions =",
        "# mutates_policy =",
        "# alters_action =",
    ] {
        assert!(config.contains(setting), "missing {setting} in:\n{config}");
    }

    let annotation_count = config
        .lines()
        .filter(|line| line.trim_start().starts_with('#'))
        .count();
    assert!(
        annotation_count >= 100,
        "expected comprehensive annotations, found {annotation_count}"
    );
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
    let openai = parsed
        .get("providers")
        .and_then(toml::Value::as_table)
        .and_then(|providers| providers.get("openai"))
        .and_then(toml::Value::as_table)
        .unwrap();
    let anthropic = parsed
        .get("providers")
        .and_then(toml::Value::as_table)
        .and_then(|providers| providers.get("anthropic"))
        .and_then(toml::Value::as_table)
        .unwrap();

    assert_eq!(
        openai.get("default_model").and_then(toml::Value::as_str),
        Some("gpt-5.6-terra")
    );
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
        Some("claude-sonnet-5")
    );

    let models = anthropic
        .get("models")
        .and_then(toml::Value::as_table)
        .unwrap()
        .values()
        .map(|value| value.get("id").and_then(toml::Value::as_str).unwrap())
        .collect::<Vec<_>>();

    assert_eq!(
        models,
        vec![
            "claude-fable-5",
            "claude-haiku-4-5-20251001",
            "claude-opus-5",
            "claude-sonnet-5",
        ]
    );

    let profiles = parsed
        .get("model_profiles")
        .and_then(toml::Value::as_table)
        .unwrap();
    let default = profiles
        .get("default")
        .and_then(toml::Value::as_table)
        .unwrap();
    let anthropic_default = profiles
        .get("anthropic-default")
        .and_then(toml::Value::as_table)
        .unwrap();

    assert_eq!(
        default.get("model").and_then(toml::Value::as_str),
        Some("gpt-5.6-terra")
    );
    assert_eq!(
        default
            .get("reasoning_profile")
            .and_then(toml::Value::as_str),
        Some("high")
    );
    assert_eq!(
        anthropic_default.get("model").and_then(toml::Value::as_str),
        Some("claude-sonnet-5")
    );
}

/// Verifies authenticated OpenAI model defaults carry the exact editable token limits.
#[test]
fn default_config_uses_configured_openai_model_token_limits() {
    let parsed: toml::Value = toml::from_str(DEFAULT_CONFIG_TOML).unwrap();
    let models = parsed
        .get("providers")
        .and_then(|providers| providers.get("openai"))
        .and_then(|provider| provider.get("models"))
        .and_then(toml::Value::as_table)
        .unwrap();

    for (entry, expected) in [
        ("gpt-5-6-sol", (1_000_000, 800_000, 60_000)),
        ("gpt-5-6-terra", (500_000, 400_000, 30_000)),
        ("gpt-5-6-luna", (250_000, 200_000, 15_000)),
    ] {
        let model = models.get(entry).and_then(toml::Value::as_table).unwrap();
        let actual = (
            model
                .get("context_window_tokens")
                .and_then(toml::Value::as_integer),
            model
                .get("max_input_tokens")
                .and_then(toml::Value::as_integer),
            model
                .get("max_output_tokens")
                .and_then(toml::Value::as_integer),
        );
        assert_eq!(
            actual,
            (Some(expected.0), Some(expected.1), Some(expected.2)),
            "{entry}"
        );
    }
}

/// Verifies authentication fills missing model limits without replacing explicit values.
#[test]
fn authenticated_provider_defaults_preserve_openai_model_overrides() {
    let root = temp_root("authenticated-openai-limit-merge");
    let paths = ConfigPaths::from_root(root.clone());
    let path = paths.ensure_default_config().unwrap();
    fs::write(
        &path,
        "version = 78\n[providers.openai]\nkind = \"openai\"\ndefault_model = \"gpt-5.6-terra\"\n[providers.openai.models.gpt-5-6-terra]\nid = \"gpt-5.6-terra\"\nmax_input_tokens = 123456\n",
    )
    .unwrap();

    paths
        .materialize_authenticated_provider_defaults("openai")
        .unwrap();

    let parsed: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let models = parsed
        .get("providers")
        .and_then(|providers| providers.get("openai"))
        .and_then(|provider| provider.get("models"))
        .and_then(toml::Value::as_table)
        .unwrap();
    let terra = models
        .get("gpt-5-6-terra")
        .and_then(toml::Value::as_table)
        .unwrap();
    assert_eq!(
        terra
            .get("max_input_tokens")
            .and_then(toml::Value::as_integer),
        Some(123_456)
    );
    assert_eq!(
        terra
            .get("context_window_tokens")
            .and_then(toml::Value::as_integer),
        Some(500_000)
    );
    assert_eq!(
        terra
            .get("max_output_tokens")
            .and_then(toml::Value::as_integer),
        Some(30_000)
    );
    assert!(models.contains_key("gpt-5-6-sol"));
    assert!(models.contains_key("gpt-5-6-luna"));

    let _ = fs::remove_dir_all(root);
}

/// Verifies authenticated DeepSeek defaults carry the exact editable token limits.
#[test]
fn default_config_uses_configured_deepseek_model_token_limits() {
    let parsed: toml::Value = toml::from_str(DEFAULT_CONFIG_TOML).unwrap();
    let models = parsed
        .get("providers")
        .and_then(|providers| providers.get("deepseek"))
        .and_then(|provider| provider.get("models"))
        .and_then(toml::Value::as_table)
        .unwrap();

    for (entry, expected) in [
        ("deepseek-v4-pro", (1_000_000, 800_000, 60_000)),
        ("deepseek-v4-flash", (500_000, 400_000, 30_000)),
    ] {
        let model = models.get(entry).and_then(toml::Value::as_table).unwrap();
        let actual = (
            model
                .get("context_window_tokens")
                .and_then(toml::Value::as_integer),
            model
                .get("max_input_tokens")
                .and_then(toml::Value::as_integer),
            model
                .get("max_output_tokens")
                .and_then(toml::Value::as_integer),
        );
        assert_eq!(
            actual,
            (Some(expected.0), Some(expected.1), Some(expected.2)),
            "{entry}"
        );
    }
}

/// Verifies DeepSeek authentication fills missing limits without replacing overrides.
#[test]
fn authenticated_provider_defaults_preserve_deepseek_model_overrides() {
    let root = temp_root("authenticated-deepseek-limit-merge");
    let paths = ConfigPaths::from_root(root.clone());
    let path = paths.ensure_default_config().unwrap();
    fs::write(
        &path,
        "version = 78\n[providers.deepseek]\nkind = \"deepseek\"\ndefault_model = \"deepseek-v4-pro\"\n[providers.deepseek.models.deepseek-v4-pro]\nid = \"deepseek-v4-pro\"\nmax_input_tokens = 123456\n",
    )
    .unwrap();

    paths
        .materialize_authenticated_provider_defaults("deepseek")
        .unwrap();

    let parsed: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let models = parsed
        .get("providers")
        .and_then(|providers| providers.get("deepseek"))
        .and_then(|provider| provider.get("models"))
        .and_then(toml::Value::as_table)
        .unwrap();
    let pro = models
        .get("deepseek-v4-pro")
        .and_then(toml::Value::as_table)
        .unwrap();
    assert_eq!(
        pro.get("max_input_tokens")
            .and_then(toml::Value::as_integer),
        Some(123_456)
    );
    assert_eq!(
        pro.get("context_window_tokens")
            .and_then(toml::Value::as_integer),
        Some(1_000_000)
    );
    assert_eq!(
        pro.get("max_output_tokens")
            .and_then(toml::Value::as_integer),
        Some(60_000)
    );
    assert!(models.contains_key("deepseek-v4-flash"));

    let _ = fs::remove_dir_all(root);
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
