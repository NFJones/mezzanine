//! Provider-independent model response contract.
//!
//! This module owns the canonical successful response record exchanged between
//! provider adapters and the agent harness. Provider transports remain
//! responsible for authentication, HTTP execution, quota-header extraction,
//! and product error projection before constructing this record.

use crate::{MaapBatch, ModelTokenUsage, ProviderQuotaUsage, ProviderTranscriptEvent};

/// Canonical successful response returned by a model provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelResponse {
    /// Provider identity that produced the response.
    pub provider: String,
    /// Provider-reported model identity.
    pub model: String,
    /// Visible provider text retained for transcript and diagnostics.
    pub raw_text: String,
    /// Provider-reported usage for the full exchange.
    pub usage: ModelTokenUsage,
    /// Usage for the last concrete request when `usage` is accumulated.
    pub latest_request_usage: Option<ModelTokenUsage>,
    /// Provider quota usage derived from response metadata.
    pub quota_usage: Vec<ProviderQuotaUsage>,
    /// Parsed MAAP batch when the interaction produced actions.
    pub action_batch: Option<MaapBatch>,
    /// Hidden provider-native events required for later transcript replay.
    pub provider_transcript_events: Vec<ProviderTranscriptEvent>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies the canonical response record preserves accumulated and latest
    /// request usage, quota metadata, and provider-native transcript events.
    #[test]
    fn model_response_preserves_complete_provider_result() {
        let latest_request_usage = ModelTokenUsage {
            input_tokens: 5,
            output_tokens: 3,
            ..ModelTokenUsage::default()
        };
        let response = ModelResponse {
            provider: "deepseek".to_string(),
            model: "deepseek-v4-pro".to_string(),
            raw_text: "executing".to_string(),
            usage: ModelTokenUsage {
                input_tokens: 8,
                output_tokens: 5,
                ..ModelTokenUsage::default()
            },
            latest_request_usage: Some(latest_request_usage),
            quota_usage: vec![ProviderQuotaUsage {
                name: "tokens".to_string(),
                used_basis_points: 2500,
                limit: 100,
                remaining: 75,
                reset: Some("10s".to_string()),
            }],
            action_batch: None,
            provider_transcript_events: vec![ProviderTranscriptEvent::DeepSeekToolResult {
                tool_call_id: "call-1".to_string(),
                content: "result".to_string(),
            }],
        };

        assert_eq!(response.latest_request_usage, Some(latest_request_usage));
        assert_eq!(response.quota_usage[0].used_percent_display(), "25.00%");
        assert_eq!(response.provider_transcript_events.len(), 1);
    }
}
