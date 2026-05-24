//! Provider quota accounting helpers.
//!
//! This module owns the provider rate-limit header parsing boundary. Provider
//! adapters pass raw response headers in, and callers receive normalized quota
//! usage buckets suitable for runtime status display and transcript metadata.

use std::collections::BTreeMap;

/// Provider-reported quota usage for one rate-limit bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderQuotaUsage {
    /// Provider quota bucket name, such as `requests` or `tokens`.
    pub name: String,
    /// Usage percentage in basis points, where `10000` is exactly 100%.
    pub used_basis_points: u32,
    /// Provider-reported quota limit for this bucket.
    pub limit: u64,
    /// Provider-reported quota remaining for this bucket.
    pub remaining: u64,
    /// Provider-reported reset value for this bucket, if supplied.
    pub reset: Option<String>,
}

impl ProviderQuotaUsage {
    /// Returns a human-readable percentage with two decimal places.
    pub fn used_percent_display(&self) -> String {
        format!(
            "{}.{:02}%",
            self.used_basis_points / 100,
            self.used_basis_points % 100
        )
    }
}

/// Extracts quota usage percentages from provider rate-limit headers.
pub fn provider_quota_usage_from_headers(
    headers: &BTreeMap<String, String>,
) -> Vec<ProviderQuotaUsage> {
    let normalized = headers
        .iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_string()))
        .collect::<BTreeMap<_, _>>();
    let mut quotas = Vec::new();
    for (header, value) in &normalized {
        let Some(name) = header.strip_prefix("x-ratelimit-limit-") else {
            continue;
        };
        let Some(limit) = provider_header_u64(value) else {
            continue;
        };
        let remaining_header = format!("x-ratelimit-remaining-{name}");
        let Some(remaining) = normalized
            .get(&remaining_header)
            .and_then(|remaining| provider_header_u64(remaining))
        else {
            continue;
        };
        let used = limit.saturating_sub(remaining.min(limit));
        let used_basis_points = if limit == 0 {
            0
        } else {
            ((u128::from(used) * 10_000 + u128::from(limit / 2)) / u128::from(limit))
                .min(u128::from(u32::MAX)) as u32
        };
        quotas.push(ProviderQuotaUsage {
            name: name.to_string(),
            used_basis_points,
            limit,
            remaining,
            reset: normalized
                .get(&format!("x-ratelimit-reset-{name}"))
                .cloned(),
        });
    }
    quotas.sort_by(|left, right| left.name.cmp(&right.name));
    quotas.dedup_by(|left, right| left.name == right.name);
    quotas
}

/// Parses the leading unsigned integer from a provider quota header.
fn provider_header_u64(value: &str) -> Option<u64> {
    let value = value.trim();
    if let Ok(parsed) = value.parse::<u64>() {
        return Some(parsed);
    }
    let normalized = value
        .chars()
        .filter(|character| *character != ',' && *character != '_')
        .collect::<String>();
    if normalized
        .chars()
        .all(|character| character.is_ascii_digit())
    {
        normalized.parse::<u64>().ok()
    } else {
        None
    }
}
