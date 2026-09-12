//! Effective model-capability policy shared by routing and provider requests.
//!
//! Provider API compatibility describes the widest shape an adapter can
//! encode. Model metadata can narrow that shape for one selected model, while
//! models without trustworthy metadata may require a conservative policy.
//! Keeping the resolved record on both profiles and requests prevents command,
//! transport, retry, and accounting paths from independently guessing support.

use crate::{ProviderApiCompatibility, ProviderCapabilities};

/// Provenance policy used to resolve one effective model capability record.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum ModelCapabilityMetadataPolicy {
    /// No model-specific declaration was available; use API-wide behavior.
    #[default]
    ProviderApi,
    /// Configured, discovered, or built-in model metadata was materialized.
    ModelMetadata,
    /// Unknown model behavior is deliberately limited to required compatibility.
    ConservativeUnknown,
}

/// Typed capabilities effective for one selected provider model.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct ModelCapabilities {
    /// Policy describing how model-specific support was established.
    pub metadata_policy: ModelCapabilityMetadataPolicy,
    /// Whether the model supports the provider's native thinking control.
    pub native_thinking: bool,
    /// Ordered provider-facing reasoning efforts accepted by the model.
    pub supported_reasoning_efforts: Vec<String>,
    /// Whether reasoning-effort metadata was explicitly declared.
    ///
    /// This preserves omitted metadata separately from an explicit empty list:
    /// omission leaves API-wide reasoning support unconstrained, while an
    /// explicit empty list rejects every reasoning control.
    pub reasoning_efforts_explicit: bool,
    /// Whether function tools can be included in the request.
    pub function_tools: bool,
    /// Whether a named function tool may be forced by the request.
    pub forced_tool_choice: bool,
    /// Whether this model may receive streaming requests.
    pub streaming: bool,
    /// Whether the model accepts an explicit maximum output-token control.
    pub max_output_tokens: bool,
}

impl ModelCapabilities {
    /// Returns API-wide defaults for callers that do not have model metadata.
    pub fn for_api(api: ProviderApiCompatibility) -> Self {
        let provider = ProviderCapabilities::for_api(api);
        Self {
            metadata_policy: ModelCapabilityMetadataPolicy::ProviderApi,
            native_thinking: provider.supports_thinking_toggle,
            supported_reasoning_efforts: Vec::new(),
            reasoning_efforts_explicit: false,
            function_tools: provider.supports_tool_calls,
            forced_tool_choice: provider.supports_tool_calls,
            streaming: provider.supports_streaming,
            max_output_tokens: provider.supports_max_output_tokens,
        }
    }

    /// Returns the safe compatibility floor for an unknown DeepSeek model.
    ///
    /// MAAP still requires a function tool that can be forced, and bounded
    /// output recovery requires `max_tokens`. Native thinking, model-specific
    /// reasoning efforts, and streaming stay disabled until metadata proves
    /// support.
    pub fn conservative_unknown_deepseek() -> Self {
        Self {
            metadata_policy: ModelCapabilityMetadataPolicy::ConservativeUnknown,
            native_thinking: false,
            supported_reasoning_efforts: Vec::new(),
            reasoning_efforts_explicit: true,
            function_tools: true,
            forced_tool_choice: true,
            streaming: false,
            max_output_tokens: true,
        }
    }

    /// Resolves optional model metadata over one API's compatibility defaults.
    ///
    /// Capability lists are complete replacement declarations. Consequently,
    /// `Some(&[])` intentionally clears inherited API support while `None`
    /// retains it. Reasoning efforts use the same omission-versus-empty rule.
    pub fn from_metadata(
        api: ProviderApiCompatibility,
        capabilities: Option<&[String]>,
        reasoning_efforts: Option<&[String]>,
        conservative_unknown: bool,
    ) -> Self {
        let mut effective =
            if conservative_unknown && capabilities.is_none() && reasoning_efforts.is_none() {
                Self::conservative_unknown_deepseek()
            } else {
                Self::for_api(api)
            };
        if capabilities.is_some() || reasoning_efforts.is_some() {
            effective.metadata_policy = ModelCapabilityMetadataPolicy::ModelMetadata;
        }
        if let Some(capabilities) = capabilities {
            effective.native_thinking = has_capability(capabilities, "native_thinking");
            effective.function_tools = capabilities.iter().any(|capability| {
                matches!(
                    capability.trim(),
                    "function_tools" | "function_calling" | "tool_use" | "tools"
                )
            });
            effective.forced_tool_choice = has_capability(capabilities, "forced_tool_choice");
            effective.streaming = has_capability(capabilities, "streaming");
            effective.max_output_tokens = capabilities.iter().any(|capability| {
                matches!(
                    capability.trim(),
                    "max_output_tokens" | "max_output_token_control"
                )
            });
        }
        if let Some(reasoning_efforts) = reasoning_efforts {
            effective.supported_reasoning_efforts = reasoning_efforts.to_vec();
            effective.reasoning_efforts_explicit = true;
        }
        effective
    }

    /// Resolves an API-default placeholder against the actual request API.
    pub fn resolved_for_api(&self, api: ProviderApiCompatibility) -> Self {
        if self.metadata_policy == ModelCapabilityMetadataPolicy::ProviderApi {
            Self::for_api(api)
        } else {
            self.clone()
        }
    }

    /// Returns whether a provider-facing reasoning effort is supported.
    ///
    /// An empty list under API-default policy means support is not
    /// model-constrained. Empty declared or conservative lists mean no
    /// reasoning control is supported.
    pub fn supports_reasoning_effort(&self, effort: &str) -> bool {
        if self.supported_reasoning_efforts.is_empty() {
            return !self.reasoning_efforts_explicit;
        }
        self.supported_reasoning_efforts.iter().any(|supported| {
            supported == effort
                || matches!(
                    (supported.as_str(), effort),
                    ("max", "xhigh") | ("xhigh", "max")
                )
        })
    }
}

/// Reports whether a normalized declaration contains one exact capability.
fn has_capability(capabilities: &[String], expected: &str) -> bool {
    capabilities
        .iter()
        .any(|capability| capability.trim() == expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies omitted metadata retains API behavior while explicit empty
    /// declarations clear every model-specific capability and reasoning level.
    #[test]
    fn model_capability_metadata_distinguishes_omitted_from_explicit_empty_lists() {
        let omitted = ModelCapabilities::from_metadata(
            ProviderApiCompatibility::DeepSeekChatCompletions,
            None,
            None,
            false,
        );
        assert!(omitted.native_thinking);
        assert!(omitted.function_tools);
        assert!(omitted.streaming);

        let empty = ModelCapabilities::from_metadata(
            ProviderApiCompatibility::DeepSeekChatCompletions,
            Some(&[]),
            Some(&[]),
            false,
        );
        assert_eq!(
            empty.metadata_policy,
            ModelCapabilityMetadataPolicy::ModelMetadata
        );
        assert!(!empty.native_thinking);
        assert!(!empty.function_tools);
        assert!(!empty.forced_tool_choice);
        assert!(!empty.streaming);
        assert!(!empty.max_output_tokens);
        assert!(!empty.supports_reasoning_effort("high"));
    }

    /// Verifies unknown DeepSeek models retain only the compatibility needed
    /// for mandatory MAAP dispatch and bounded output-token recovery.
    #[test]
    fn conservative_unknown_deepseek_suppresses_model_specific_features() {
        let capabilities = ModelCapabilities::conservative_unknown_deepseek();
        assert_eq!(
            capabilities.metadata_policy,
            ModelCapabilityMetadataPolicy::ConservativeUnknown
        );
        assert!(!capabilities.native_thinking);
        assert!(!capabilities.supports_reasoning_effort("high"));
        assert!(capabilities.function_tools);
        assert!(capabilities.forced_tool_choice);
        assert!(!capabilities.streaming);
        assert!(capabilities.max_output_tokens);
    }
}
