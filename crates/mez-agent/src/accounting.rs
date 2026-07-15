//! Provider-neutral model token accounting contracts.
//!
//! These records preserve provider-reported request usage and stable
//! provider/model identities without depending on product runtime or storage
//! implementations.

/// Stable provider/model identity for token-cost accounting.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ModelTokenUsageKey {
    /// Provider id that served the request.
    pub provider: String,
    /// Provider model id that served the request.
    pub model: String,
}

impl ModelTokenUsageKey {
    /// Builds a normalized provider/model token-accounting key.
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: non_empty_or_unknown(provider.into()),
            model: non_empty_or_unknown(model.into()),
        }
    }

    /// Builds the fallback key used for legacy aggregate-only metadata.
    pub fn unknown() -> Self {
        Self::new("unknown", "unknown")
    }

    /// Returns a compact display label for provider/model usage tables.
    pub fn display_name(&self) -> String {
        format!("{} via {}", self.model, self.provider)
    }
}

fn non_empty_or_unknown(value: String) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Provider-reported token usage for one or more model requests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModelTokenUsage {
    /// Raw input tokens reported before prompt-cache adjustment.
    pub input_tokens: u64,
    /// Output tokens charged or counted by the provider.
    pub output_tokens: u64,
    /// Output tokens attributed to reasoning by the provider.
    pub reasoning_tokens: u64,
    /// Input tokens served from the provider prompt cache, when reported.
    pub cached_input_tokens: Option<u64>,
    /// Input tokens written into the provider prompt cache, when reported.
    pub cache_write_input_tokens: Option<u64>,
}

impl ModelTokenUsage {
    /// Adds provider usage counters with saturating arithmetic.
    pub fn add_assign(&mut self, other: Self) {
        let had_usage = !self.is_zero();
        let other_has_usage = !other.is_zero();
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.reasoning_tokens = self.reasoning_tokens.saturating_add(other.reasoning_tokens);
        self.cached_input_tokens = match (self.cached_input_tokens, other.cached_input_tokens) {
            (Some(current), Some(next)) => Some(current.saturating_add(next)),
            (None, Some(next)) if !had_usage => Some(next),
            (Some(current), None) if !other_has_usage => Some(current),
            (None, None) => None,
            _ => None,
        };
        self.cache_write_input_tokens = match (
            self.cache_write_input_tokens,
            other.cache_write_input_tokens,
        ) {
            (Some(current), Some(next)) => Some(current.saturating_add(next)),
            (None, Some(next)) if !had_usage => Some(next),
            (Some(current), None) if !other_has_usage => Some(current),
            (None, None) => None,
            _ => None,
        };
    }

    /// Returns true when the provider did not report any token usage.
    pub fn is_zero(self) -> bool {
        self.input_tokens == 0
            && self.output_tokens == 0
            && self.reasoning_tokens == 0
            && self.cached_input_tokens.unwrap_or(0) == 0
            && self.cache_write_input_tokens.unwrap_or(0) == 0
    }

    /// Returns provider-visible total tokens when input and output are known.
    pub fn total_tokens(self) -> u64 {
        self.prompt_cache_input_tokens()
            .saturating_add(self.cache_write_input_tokens.unwrap_or(0))
            .saturating_add(self.output_tokens)
    }

    fn prompt_cache_input_tokens(self) -> u64 {
        let cached = self.cached_input_tokens.unwrap_or(0);
        if cached > self.input_tokens {
            self.input_tokens.saturating_add(cached)
        } else {
            self.input_tokens
        }
    }

    /// Returns input tokens billed outside provider prompt-cache hits.
    pub fn billed_input_tokens(self) -> u64 {
        let input_tokens = if self.cached_input_tokens.unwrap_or(0) > self.input_tokens {
            self.input_tokens
        } else {
            self.input_tokens
                .saturating_sub(self.cached_input_tokens.unwrap_or(0))
        };
        input_tokens.saturating_add(self.cache_write_input_tokens.unwrap_or(0))
    }

    /// Returns the best-effort display value for provider prompt-cache hits.
    pub fn cached_input_tokens_display(self) -> String {
        self.cached_input_tokens
            .map(|tokens| tokens.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }

    /// Returns the best-effort provider prompt-cache hit ratio.
    pub fn cached_input_hit_ratio_basis_points(self) -> Option<u32> {
        let cached = self.cached_input_tokens?;
        let input_tokens = self.billed_input_tokens().saturating_add(cached);
        if input_tokens == 0 {
            return Some(0);
        }
        let basis_points = cached
            .saturating_mul(10_000)
            .saturating_add(input_tokens / 2)
            / input_tokens;
        Some(basis_points.min(10_000) as u32)
    }

    /// Returns a human-readable best-effort provider prompt-cache hit ratio.
    pub fn cached_input_hit_ratio_display(self) -> String {
        self.cached_input_hit_ratio_basis_points()
            .map(|basis_points| format!("{}.{:02}%", basis_points / 100, basis_points % 100))
            .unwrap_or_else(|| "unknown".to_string())
    }
}

/// Last known provider request context usage for one selected model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentContextUsageSnapshot {
    /// Provider-reported input tokens for the most recent request.
    pub input_tokens: u64,
    /// Total model context window used as the status denominator.
    pub context_window_tokens: u64,
    /// Provider-reported cached input tokens for the same request, if known.
    pub cached_input_tokens: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::{ModelTokenUsage, ModelTokenUsageKey};

    #[test]
    /// Verifies stable accounting keys normalize absent identity fields while
    /// preserving provider and model display order.
    fn token_usage_keys_normalize_missing_identity() {
        assert_eq!(
            ModelTokenUsageKey::new("", "model").display_name(),
            "model via unknown"
        );
        assert_eq!(
            ModelTokenUsageKey::unknown().display_name(),
            "unknown via unknown"
        );
    }

    #[test]
    /// Verifies aggregation fails closed when cache counters are missing from
    /// one contributing request.
    fn token_usage_aggregation_preserves_unknown_cache_counters() {
        let mut usage = ModelTokenUsage::default();
        usage.add_assign(ModelTokenUsage {
            input_tokens: 100,
            output_tokens: 10,
            reasoning_tokens: 0,
            cached_input_tokens: Some(60),
            cache_write_input_tokens: Some(20),
        });
        assert_eq!(usage.cache_write_input_tokens, Some(20));
        assert_eq!(usage.billed_input_tokens(), 60);
        assert_eq!(usage.total_tokens(), 130);
        usage.add_assign(ModelTokenUsage {
            input_tokens: 50,
            output_tokens: 5,
            reasoning_tokens: 0,
            cached_input_tokens: None,
            cache_write_input_tokens: None,
        });

        assert_eq!(usage.input_tokens, 150);
        assert_eq!(usage.output_tokens, 15);
        assert_eq!(usage.cached_input_tokens, None);
        assert_eq!(usage.cache_write_input_tokens, None);
        assert_eq!(usage.cached_input_tokens_display(), "unknown");
        assert_eq!(usage.cached_input_hit_ratio_basis_points(), None);
        assert_eq!(usage.cached_input_hit_ratio_display(), "unknown");
    }

    #[test]
    /// Verifies providers may report cache hits outside ordinary input tokens
    /// without corrupting billed and total token calculations.
    fn token_usage_accounts_for_separately_reported_cache_hits() {
        let usage = ModelTokenUsage {
            input_tokens: 2,
            output_tokens: 12,
            reasoning_tokens: 0,
            cached_input_tokens: Some(10_496),
            cache_write_input_tokens: Some(6_112),
        };

        assert_eq!(usage.input_tokens, 2);
        assert_eq!(usage.billed_input_tokens(), 6_114);
        assert_eq!(usage.total_tokens(), 16_622);
        assert_eq!(usage.cached_input_hit_ratio_display(), "63.19%");
    }
}
