//! Model profile policy regressions.
//!
//! These tests remain with the provider-independent profile records and
//! selection rules they protect.

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

    assert_eq!(profile.context_window_tokens(), Some(1024));
}

#[test]
/// Verifies a configured maximum input remains distinct from the advertised
/// context window and lowers the compaction target when it is the tighter
/// provider constraint. This prevents stateless Responses retries from
/// treating output and provider overhead capacity as usable prompt context.
fn model_profile_max_input_limit_constrains_context_budget() {
    let mut provider_options = std::collections::BTreeMap::new();
    provider_options.insert("context_window_tokens".to_string(), "400000".to_string());
    provider_options.insert("max_input_tokens".to_string(), "272000".to_string());
    let profile = ModelProfile {
        provider: "openai".to_string(),
        model: "gpt-5.3-codex".to_string(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options,
        safety_tier: None,
    };

    assert_eq!(profile.context_window_tokens(), Some(400_000));
    assert_eq!(profile.max_input_tokens(), Some(272_000));
    assert_eq!(profile.context_window_budget_words(), Some(204_000));
}

#[test]
/// Verifies known DeepSeek model names do not inject runtime token limits.
fn model_profile_context_window_requires_deepseek_configuration() {
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
            None,
            "{model} must not receive a hard-coded context window"
        );
    }
}

#[test]
/// Verifies known OpenAI model names do not inject runtime token limits.
fn model_profile_context_window_requires_openai_configuration() {
    for model in [
        "gpt-5.5",
        "gpt-5.5-2026-05-19",
        "gpt-5.4",
        "gpt-5.4-mini",
        "gpt-5.3-codex",
        "gpt-5.3-codex-spark",
        "gpt-5.3-codex-spark-2026-02-12",
        "gpt-5.2",
        "gpt-5-codex",
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
            None,
            "{model} must not receive a hard-coded context window"
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

/// Verifies a complete model profile and turn identity satisfy request
/// preconditions before product context assembly begins.
#[test]
fn model_profile_request_preconditions_accept_complete_identity() {
    let profile = ModelProfile {
        provider: "openai".to_string(),
        model: "gpt-5.5".to_string(),
        ..ModelProfile::default()
    };

    assert!(validate_model_profile_request(&profile, "turn-1").is_ok());
}

/// Verifies every provider-independent request identity field rejects blank
/// input with the same stable field-specific diagnostic used by root assembly.
#[test]
fn model_profile_request_preconditions_reject_blank_identity_fields() {
    let complete = ModelProfile {
        provider: "openai".to_string(),
        model: "gpt-5.5".to_string(),
        ..ModelProfile::default()
    };
    let cases = [
        (
            ModelProfile {
                provider: " ".to_string(),
                ..complete.clone()
            },
            "turn-1",
            "model provider must not be empty",
        ),
        (
            ModelProfile {
                model: "\t".to_string(),
                ..complete.clone()
            },
            "turn-1",
            "model must not be empty",
        ),
        (complete, "", "turn_id must not be empty"),
    ];

    for (profile, turn_id, expected) in cases {
        let error = validate_model_profile_request(&profile, turn_id).unwrap_err();
        assert_eq!(error.message(), expected);
    }
}
