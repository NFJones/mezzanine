//! Config defaults tests.

use super::*;

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
    let documented = include_str!("../../../../../docs/examples/config.toml");

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
