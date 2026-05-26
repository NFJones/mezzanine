//! Model profile, message, and request context types.
//!
//! This module owns provider-facing model metadata that is independent of
//! context block storage: model messages, model profiles and overrides,
//! selected-profile metadata, request envelopes, and profile selection helpers.

use super::super::validate_non_empty;
use super::{AllowedActionSet, ContextSourceKind, ModelInteractionKind};
use crate::error::Result;
use crate::mcp::McpPromptTool;

/// Fallback context window when the model profile does not carry one.
const MODEL_CONTEXT_FALLBACK_WINDOW_TOKENS: usize = 128 * 1024;
/// Output-token cap used for the first output-limit retry when no profile cap
/// was configured.
const MODEL_OUTPUT_LIMIT_RETRY_TOKENS: usize = 16_384;
/// Upper bound for automatic output-limit retry cap escalation.
const MODEL_OUTPUT_LIMIT_RETRY_CEILING_TOKENS: usize = 32_768;
/// Conservative numerator for converting token context windows into word budgets.
const MODEL_CONTEXT_BUDGET_WORDS_PER_TOKEN_NUMERATOR: usize = 3;
/// Conservative denominator for converting token context windows into word budgets.
const MODEL_CONTEXT_BUDGET_WORDS_PER_TOKEN_DENOMINATOR: usize = 4;
/// Documented context window for OpenAI frontier 1M-token model families.
const OPENAI_FRONTIER_CONTEXT_WINDOW_TOKENS: usize = 1_050_000;
/// Documented context window for OpenAI GPT-5 family 400K-token model families.
const OPENAI_STANDARD_GPT5_CONTEXT_WINDOW_TOKENS: usize = 400_000;
/// Documented context window for OpenAI GPT-5.3-Codex-Spark.
const OPENAI_CODEX_SPARK_CONTEXT_WINDOW_TOKENS: usize = 128_000;
/// Documented context window for DeepSeek V4 model families.
const DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS: usize = 1_000_000;

/// Carries Model Message Role state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelMessageRole {
    /// Represents the System case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    System,
    /// Represents the Developer case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Developer,
    /// Represents the User case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    User,
    /// Represents the Assistant case for this enumeration.
    ///
    /// Prior assistant messages must keep their role when replayed so the
    /// model can distinguish user instructions from earlier model output.
    Assistant,
    /// Represents the Tool case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Tool,
}

/// Carries Model Message state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelMessage {
    /// Stores the role value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub role: ModelMessageRole,
    /// Stores the source value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub source: ContextSourceKind,
    /// Stores the content value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub content: String,
}

/// Carries Model Profile state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelProfile {
    /// Stores the provider value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub provider: String,
    /// Stores the model value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub model: String,
    /// Stores the reasoning profile value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub reasoning_profile: Option<String>,
    /// Stores the latency preference value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub latency_preference: Option<String>,
    /// Stores the multimodal required value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub multimodal_required: bool,
    /// Stores the provider options value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub provider_options: std::collections::BTreeMap<String, String>,
    /// Safety tier for model failover comparison. When a model is unavailable,
    /// Mezzanine MUST NOT silently switch to a model with a lower safety tier.
    /// Tiers: `"high"`, `"medium"`, `"basic"` (or absent when unset).
    pub safety_tier: Option<String>,
}

impl ModelProfile {
    /// Returns the approximate provider context window in model tokens.
    ///
    /// Profile-specific values may be supplied through `provider_options` as
    /// `context_window_tokens` or `context_limit_tokens`. When omitted,
    /// Mezzanine first uses built-in provider model metadata for known default
    /// models, then falls back to a conservative built-in default so automatic
    /// compaction has a stable budget before provider metadata is available.
    pub fn context_window_tokens(&self) -> usize {
        self.configured_context_window_tokens()
            .or_else(|| known_provider_model_context_window_tokens(&self.provider, &self.model))
            .unwrap_or(MODEL_CONTEXT_FALLBACK_WINDOW_TOKENS)
    }

    /// Returns the configured provider output-token cap, if present.
    ///
    /// OpenAI-compatible providers use `max_output_tokens`; compatible
    /// adapters may expose the same concept as `max_completion_tokens`, so both
    /// non-secret profile options are accepted.
    pub fn max_output_tokens(&self) -> Option<usize> {
        self.provider_options
            .get("max_output_tokens")
            .or_else(|| self.provider_options.get("max_completion_tokens"))
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|tokens| *tokens > 0)
    }

    /// Returns an explicit thinking-mode override, if configured.
    ///
    /// Providers that expose a native thinking toggle use this separate control
    /// so reasoning effort can continue to describe the level to use when
    /// thinking is enabled.
    pub fn thinking_enabled(&self) -> Option<bool> {
        self.provider_options
            .get("thinking")
            .or_else(|| self.provider_options.get("thinking_mode"))
            .or_else(|| self.provider_options.get("thinking_enabled"))
            .and_then(|value| match value.trim().to_ascii_lowercase().as_str() {
                "enabled" | "on" | "true" => Some(true),
                "disabled" | "off" | "false" => Some(false),
                _ => None,
            })
    }

    /// Returns the output-token cap to use after a provider output-limit
    /// failure.
    pub fn output_limit_retry_tokens(&self) -> usize {
        let configured = self.max_output_tokens().unwrap_or(0);
        configured.saturating_mul(2).clamp(
            MODEL_OUTPUT_LIMIT_RETRY_TOKENS,
            MODEL_OUTPUT_LIMIT_RETRY_CEILING_TOKENS,
        )
    }

    /// Returns the profile-configured context window, if the profile carries one.
    fn configured_context_window_tokens(&self) -> Option<usize> {
        self.provider_options
            .get("context_window_tokens")
            .or_else(|| self.provider_options.get("context_limit_tokens"))
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|tokens| *tokens > 0)
    }

    /// Returns the word budget used when explicit context compaction needs a
    /// model-window-sized target.
    pub fn context_window_budget_words(&self) -> usize {
        self.context_window_tokens()
            .saturating_mul(MODEL_CONTEXT_BUDGET_WORDS_PER_TOKEN_NUMERATOR)
            .saturating_div(MODEL_CONTEXT_BUDGET_WORDS_PER_TOKEN_DENOMINATOR)
            .max(1)
    }

    /// Ordinal for comparison: higher number = stronger safety.
    fn safety_ordinal(tier: Option<&str>) -> u8 {
        match tier {
            Some("high") => 3,
            Some("medium") => 2,
            Some("basic") => 1,
            _ => 0,
        }
    }

    /// Returns true if `fallback` has equivalent or stronger configured
    /// characteristics than `self`, permitting it to be offered as a safe
    /// failover candidate. Privacy, residency, and approval characteristics are
    /// modeled as exact non-secret provider options because their ordering is
    /// provider- and deployment-specific.
    pub fn failover_safe(&self, fallback: &Self) -> bool {
        if Self::safety_ordinal(fallback.safety_tier.as_deref())
            < Self::safety_ordinal(self.safety_tier.as_deref())
        {
            return false;
        }
        for key in [
            "privacy",
            "privacy_tier",
            "residency",
            "residency_region",
            "approval",
            "approval_policy",
        ] {
            if let Some(required) = self.provider_options.get(key)
                && fallback.provider_options.get(key) != Some(required)
            {
                return false;
            }
        }
        true
    }
}

/// Returns known provider model context-window metadata for built-in providers.
fn known_provider_model_context_window_tokens(provider: &str, model: &str) -> Option<usize> {
    match provider.trim().to_ascii_lowercase().as_str() {
        "openai" => openai_known_model_context_window_tokens(model),
        "deepseek" => deepseek_known_model_context_window_tokens(model),
        _ => None,
    }
}

/// Returns documented context windows for OpenAI model families Mezzanine ships.
fn openai_known_model_context_window_tokens(model: &str) -> Option<usize> {
    let model = model.trim().to_ascii_lowercase();
    if openai_model_matches_snapshot_family(&model, "gpt-5.3-codex-spark") {
        return Some(OPENAI_CODEX_SPARK_CONTEXT_WINDOW_TOKENS);
    }
    if openai_model_matches_snapshot_family(&model, "gpt-5.5")
        || openai_model_matches_snapshot_family(&model, "gpt-5.5-pro")
        || openai_model_matches_snapshot_family(&model, "gpt-5.4")
        || openai_model_matches_snapshot_family(&model, "gpt-5.4-pro")
    {
        return Some(OPENAI_FRONTIER_CONTEXT_WINDOW_TOKENS);
    }
    if openai_model_matches_snapshot_family(&model, "gpt-5.4-mini")
        || openai_model_matches_snapshot_family(&model, "gpt-5.4-nano")
        || openai_model_matches_snapshot_family(&model, "gpt-5.3-codex")
        || openai_model_matches_snapshot_family(&model, "gpt-5.2")
        || openai_model_matches_snapshot_family(&model, "gpt-5-codex")
        || openai_model_matches_snapshot_family(&model, "gpt-5-mini")
        || openai_model_matches_snapshot_family(&model, "gpt-5-nano")
        || openai_model_matches_snapshot_family(&model, "gpt-5")
    {
        return Some(OPENAI_STANDARD_GPT5_CONTEXT_WINDOW_TOKENS);
    }
    None
}

/// Returns documented context windows for DeepSeek model families Mezzanine ships.
fn deepseek_known_model_context_window_tokens(model: &str) -> Option<usize> {
    match model.trim().to_ascii_lowercase().as_str() {
        "deepseek-v4-pro" | "deepseek-v4-flash" => Some(DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS),
        _ => None,
    }
}

/// Matches an exact model family or a dated model snapshot for that family.
fn openai_model_matches_snapshot_family(model: &str, family: &str) -> bool {
    model == family
        || model
            .strip_prefix(family)
            .and_then(|suffix| suffix.strip_prefix('-'))
            .and_then(|suffix| suffix.chars().next())
            .is_some_and(|first| first.is_ascii_digit())
}

/// Carries Model Profile Overrides state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelProfileOverrides {
    /// Stores the default profile value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub default_profile: Option<String>,
    /// Stores the session profile value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub session_profile: Option<String>,
    /// Stores the window profile value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub window_profile: Option<String>,
    /// Stores the pane profile value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_profile: Option<String>,
    /// Stores the agent profile value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub agent_profile: Option<String>,
    /// Stores the subagent profile value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub subagent_profile: Option<String>,
}

/// Carries Selected Model Profile state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedModelProfile {
    /// Stores the profile value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub profile: String,
    /// Stores the source value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub source: ModelProfileOverrideSource,
}

/// Carries Model Profile Override Source state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelProfileOverrideSource {
    /// Represents the Default case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Default,
    /// Represents the Session case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Session,
    /// Represents the Window case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Window,
    /// Represents the Pane case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Pane,
    /// Represents the Agent case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Agent,
    /// Represents the Subagent case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Subagent,
}

/// Carries Model Request state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRequest {
    /// Stores the provider value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub provider: String,
    /// Stores the model value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub model: String,
    /// Stores the provider reasoning effort for this request, when configured.
    ///
    /// The field is runtime-owned per request so temporary turn sizing can
    /// adjust reasoning without mutating saved model profiles.
    pub reasoning_effort: Option<String>,
    /// Explicit provider thinking-mode override for providers that support it.
    pub thinking_enabled: Option<bool>,
    /// Latency/cost preference for provider request routing, when configured.
    ///
    /// The value is runtime-owned per request so pane-local profile overrides
    /// can select provider service tiers without mutating saved profiles.
    pub latency_preference: Option<String>,
    /// Provider prompt-cache retention policy, when configured.
    ///
    /// OpenAI-compatible providers use this to request longer-lived prefix
    /// cache retention without baking retention policy into the prompt cache
    /// key itself.
    pub prompt_cache_retention: Option<String>,
    /// Provider output-token cap, when configured or temporarily escalated for
    /// an output-limit retry.
    pub max_output_tokens: Option<usize>,
    /// Live Mezzanine session identifier used to route provider prompt-cache
    /// entries without coupling the local key to provider or model names.
    ///
    /// The value is non-secret and is derived from runtime session context when
    /// present. Requests built outside a live session leave it unset.
    pub prompt_cache_session_id: Option<String>,
    /// Stores the turn id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub turn_id: String,
    /// Stores the agent id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub agent_id: String,
    /// Stores the available mcp tools value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub available_mcp_tools: Vec<McpPromptTool>,
    /// Stores the interaction kind for this provider request.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub interaction_kind: ModelInteractionKind,
    /// Stores the concrete MAAP action surface exposed for this request.
    ///
    /// The provider adapter uses this set to generate a strict per-request
    /// schema rather than exposing every MAAP action on every turn.
    pub allowed_actions: AllowedActionSet,
    /// Stores the messages value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub messages: Vec<ModelMessage>,
}

/// Runs the select model profile operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn select_model_profile(
    overrides: &ModelProfileOverrides,
    configured_default: &str,
) -> Result<SelectedModelProfile> {
    validate_non_empty("configured default model profile", configured_default)?;
    let candidates = [
        (
            overrides.subagent_profile.as_deref(),
            ModelProfileOverrideSource::Subagent,
        ),
        (
            overrides.agent_profile.as_deref(),
            ModelProfileOverrideSource::Agent,
        ),
        (
            overrides.pane_profile.as_deref(),
            ModelProfileOverrideSource::Pane,
        ),
        (
            overrides.window_profile.as_deref(),
            ModelProfileOverrideSource::Window,
        ),
        (
            overrides.session_profile.as_deref(),
            ModelProfileOverrideSource::Session,
        ),
        (
            overrides.default_profile.as_deref(),
            ModelProfileOverrideSource::Default,
        ),
    ];
    for (profile, source) in candidates {
        if let Some(profile) = profile {
            validate_non_empty("model profile override", profile)?;
            return Ok(SelectedModelProfile {
                profile: profile.to_string(),
                source,
            });
        }
    }
    Ok(SelectedModelProfile {
        profile: configured_default.to_string(),
        source: ModelProfileOverrideSource::Default,
    })
}
