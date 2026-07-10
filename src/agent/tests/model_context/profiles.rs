//! Model Context tests for profiles behavior.
//!
//! This bounded leaf owns the named behavioral scenarios.

use super::*;

#[test]
/// Verifies explicit profile context-window values remain authoritative even
/// when the model family also has built-in provider metadata. This protects test
/// fixtures and user configurations that intentionally use a smaller budget to
/// force earlier compaction.
fn model_profile_context_window_preserves_explicit_override() {
    let mut provider_options = std::collections::BTreeMap::new();
    provider_options.insert("context_window_tokens".to_string(), "1024".to_string());
    let profile = ModelProfile {
        provider: "openai".to_string(),
        model: "gpt-5.5".to_string(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options,
        safety_tier: None,
    };

    assert_eq!(profile.context_window_tokens(), 1024);
}

#[test]
/// Verifies that known DeepSeek V4 models use their documented 1M-token
/// context windows when a profile omits an explicit context override. This
/// protects custom DeepSeek profiles from falling back to the conservative
/// generic 128Ki-token display denominator.
fn model_profile_context_window_uses_known_deepseek_metadata_when_unconfigured() {
    for model in ["deepseek-v4-pro", "deepseek-v4-flash"] {
        let profile = ModelProfile {
            provider: "deepseek".to_string(),
            model: model.to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        };

        assert_eq!(
            profile.context_window_tokens(),
            1_000_000,
            "{model} should use documented DeepSeek metadata"
        );
    }
}

#[test]
/// Verifies known OpenAI model metadata supplies context-window budgets when a
/// profile omits explicit token counts. This keeps generated profiles, ad-hoc
/// model selection, and frame usage percentages from falling back to the much
/// smaller local safety budget for documented high-context model families.
fn model_profile_context_window_uses_known_openai_metadata_when_unconfigured() {
    for (model, expected_tokens) in [
        ("gpt-5.5", 1_050_000),
        ("gpt-5.5-2026-05-19", 1_050_000),
        ("gpt-5.4", 1_050_000),
        ("gpt-5.4-mini", 400_000),
        ("gpt-5.3-codex", 400_000),
        ("gpt-5.3-codex-spark", 128_000),
        ("gpt-5.3-codex-spark-2026-02-12", 128_000),
        ("gpt-5.2", 400_000),
        ("gpt-5-codex", 400_000),
    ] {
        let profile = ModelProfile {
            provider: "openai".to_string(),
            model: model.to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        };

        assert_eq!(
            profile.context_window_tokens(),
            expected_tokens,
            "{model} should use documented OpenAI metadata"
        );
    }
}

#[test]
/// Verifies model profile failover requires non weaker configured characteristics.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn model_profile_failover_requires_non_weaker_configured_characteristics() {
    let mut preferred_options = std::collections::BTreeMap::new();
    preferred_options.insert("privacy_tier".to_string(), "strict".to_string());
    preferred_options.insert("residency".to_string(), "us".to_string());
    preferred_options.insert("approval_policy".to_string(), "ask".to_string());
    let preferred = ModelProfile {
        provider: "openai".to_string(),
        model: "primary".to_string(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options: preferred_options.clone(),
        safety_tier: Some("high".to_string()),
    };
    let safe = ModelProfile {
        provider: "openai".to_string(),
        model: "fallback".to_string(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options: preferred_options,
        safety_tier: Some("high".to_string()),
    };
    let weaker_safety = ModelProfile {
        safety_tier: Some("medium".to_string()),
        ..safe.clone()
    };
    let mut weaker_options = safe.provider_options.clone();
    weaker_options.insert("privacy_tier".to_string(), "external".to_string());
    let weaker_privacy = ModelProfile {
        provider_options: weaker_options,
        ..safe.clone()
    };

    assert!(preferred.failover_safe(&safe));
    assert!(!preferred.failover_safe(&weaker_safety));
    assert!(!preferred.failover_safe(&weaker_privacy));
}

#[test]
/// Verifies model profile selection uses most specific override.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn model_profile_selection_uses_most_specific_override() {
    let selection = select_model_profile(
        &ModelProfileOverrides {
            default_profile: Some("default".to_string()),
            session_profile: Some("session".to_string()),
            window_profile: Some("window".to_string()),
            pane_profile: Some("pane".to_string()),
            agent_profile: Some("agent".to_string()),
            subagent_profile: Some("subagent".to_string()),
        },
        "configured-default",
    )
    .unwrap();

    assert_eq!(selection.profile, "subagent");
    assert_eq!(selection.source, ModelProfileOverrideSource::Subagent);

    let selection = select_model_profile(
        &ModelProfileOverrides {
            session_profile: Some("session".to_string()),
            window_profile: Some("window".to_string()),
            ..ModelProfileOverrides::default()
        },
        "configured-default",
    )
    .unwrap();

    assert_eq!(selection.profile, "window");
    assert_eq!(selection.source, ModelProfileOverrideSource::Window);
}
