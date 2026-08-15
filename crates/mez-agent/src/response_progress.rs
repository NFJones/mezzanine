//! Provider-response accounting across one agent turn.
//!
//! This module owns the provider-neutral aggregation rules used while a turn
//! retries, repairs, or continues after capability decisions. Provider
//! adapters retain response parsing while product turn runners retain failure
//! presentation and durable transcript ownership.

use crate::{ModelTokenUsage, ProviderQuotaUsage};

/// Aggregates the response accounting state observed during one agent turn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderResponseProgress {
    cumulative_usage: ModelTokenUsage,
    latest_response_usage: Option<ModelTokenUsage>,
    latest_quota_usage: Vec<ProviderQuotaUsage>,
}

impl ProviderResponseProgress {
    /// Records usage and quota observations from one completed provider request.
    ///
    /// The cumulative counter includes every request in the turn, while the
    /// latest request usage and quota records describe the most recent concrete
    /// provider response that supplied those values.
    pub fn observe(
        &mut self,
        usage: ModelTokenUsage,
        latest_request_usage: Option<ModelTokenUsage>,
        quota_usage: &[ProviderQuotaUsage],
    ) {
        let latest_response_usage = latest_request_usage.unwrap_or(usage);
        if latest_response_usage.input_tokens > 0 {
            self.latest_response_usage = Some(latest_response_usage);
        }
        self.cumulative_usage.add_assign(usage);
        if !quota_usage.is_empty() {
            self.latest_quota_usage = quota_usage.to_vec();
        }
    }

    /// Returns the accumulated token usage for every observed provider request.
    pub fn cumulative_usage(&self) -> ModelTokenUsage {
        self.cumulative_usage
    }

    /// Returns usage from the latest response with provider input accounting.
    ///
    /// Responses that omit input usage do not erase an earlier concrete sample.
    /// A default value is returned before any input-accounted response has been
    /// observed so callers can keep error paths total without inventing a
    /// product error.
    pub fn latest_response_usage(&self) -> ModelTokenUsage {
        self.latest_response_usage.unwrap_or_default()
    }

    /// Returns the most recent non-empty provider quota observation.
    pub fn latest_quota_usage(&self) -> &[ProviderQuotaUsage] {
        &self.latest_quota_usage
    }
}

#[cfg(test)]
mod tests {
    use super::ProviderResponseProgress;
    use crate::{ModelTokenUsage, ProviderQuotaUsage};

    /// Aggregation retains cumulative token usage while replacing quota data
    /// only when a newer concrete provider response supplies it.
    #[test]
    fn provider_response_progress_accumulates_usage_and_retains_latest_quota() {
        let mut progress = ProviderResponseProgress::default();
        let quota = ProviderQuotaUsage {
            name: "requests".to_string(),
            used_basis_points: 2_500,
            limit: 100,
            remaining: 75,
            reset: None,
        };

        progress.observe(
            ModelTokenUsage {
                input_tokens: 3,
                output_tokens: 5,
                ..ModelTokenUsage::default()
            },
            None,
            std::slice::from_ref(&quota),
        );
        progress.observe(
            ModelTokenUsage {
                input_tokens: 7,
                output_tokens: 11,
                ..ModelTokenUsage::default()
            },
            Some(ModelTokenUsage {
                input_tokens: 2,
                output_tokens: 3,
                ..ModelTokenUsage::default()
            }),
            &[],
        );

        assert_eq!(progress.cumulative_usage().input_tokens, 10);
        assert_eq!(progress.cumulative_usage().output_tokens, 16);
        assert_eq!(progress.latest_response_usage().input_tokens, 2);
        assert_eq!(progress.latest_response_usage().output_tokens, 3);
        assert_eq!(progress.latest_quota_usage(), &[quota]);
    }

    /// Verifies responses that omit input accounting cannot erase the latest
    /// concrete request sample while their reported totals still accumulate.
    ///
    /// Providers may omit usage on capability continuations or repairs. Both
    /// omission representations must preserve the sample used for context
    /// display until a newer response reports a positive input-token count.
    #[test]
    fn provider_response_progress_retains_latest_input_usage_across_omissions() {
        let mut progress = ProviderResponseProgress::default();
        let concrete = ModelTokenUsage {
            input_tokens: 251,
            output_tokens: 30,
            cached_input_tokens: Some(80),
            ..ModelTokenUsage::default()
        };

        progress.observe(concrete, None, &[]);
        progress.observe(
            ModelTokenUsage {
                output_tokens: 5,
                ..ModelTokenUsage::default()
            },
            None,
            &[],
        );
        progress.observe(
            ModelTokenUsage {
                output_tokens: 7,
                ..ModelTokenUsage::default()
            },
            Some(ModelTokenUsage::default()),
            &[],
        );

        assert_eq!(progress.cumulative_usage().input_tokens, 251);
        assert_eq!(progress.cumulative_usage().output_tokens, 42);
        assert_eq!(progress.latest_response_usage(), concrete);

        let newer = ModelTokenUsage {
            input_tokens: 300,
            output_tokens: 9,
            ..ModelTokenUsage::default()
        };
        progress.observe(newer, Some(newer), &[]);
        assert_eq!(progress.latest_response_usage(), newer);
    }
}
