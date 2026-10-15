//! Strict future filter semantics, deliberately not applied to legacy DDG site
//! terms. Hostnames form a union (including subdomains); UTC dates are checked
//! calendar days, not guessed dates or unchecked recency arithmetic.

use super::SearchError;
use serde::{Deserialize, Deserializer, Serialize};

/// Canonical sorted hostname union. ASCII DNS hostnames only: no scheme, path,
/// port, credentials, wildcard, IP literal or query syntax.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct HostnameUnion(Vec<String>);

impl HostnameUnion {
    /// Validates at most fifty hostname filters and canonicalizes their union.
    pub fn new(values: &[String]) -> Result<Self, SearchError> {
        if values.len() > 50 {
            return Err(SearchError::InvalidBounds);
        }
        let mut hosts = Vec::new();
        for value in values {
            let host = value.to_ascii_lowercase();
            if host.is_empty()
                || host.len() > 253
                || host.parse::<std::net::IpAddr>().is_ok()
                || host.split('.').any(|label| {
                    label.is_empty()
                        || label.len() > 63
                        || label.starts_with('-')
                        || label.ends_with('-')
                        || !label
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                })
            {
                return Err(SearchError::InvalidRequest);
            }
            hosts.push(host);
        }
        hosts.sort();
        hosts.dedup();
        Ok(Self(hosts))
    }
    /// Matches any union member or its dot-delimited subdomain, never a suffix
    /// collision such as `notexample.com` for `example.com`.
    pub fn matches(&self, hostname: &str) -> bool {
        let hostname = hostname.to_ascii_lowercase();
        self.0.iter().any(|host| {
            hostname == *host
                || hostname
                    .strip_suffix(host)
                    .is_some_and(|prefix| prefix.ends_with('.'))
        })
    }
}

/// Checked ISO `YYYY-MM-DD` UTC day, with no unchecked timezone/relative math.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct UtcDate(String);

impl UtcDate {
    /// Rejects invalid calendar days, year zero and malformed ISO input.
    pub fn parse(value: &str) -> Result<Self, SearchError> {
        let b = value.as_bytes();
        if b.len() != 10
            || b[4] != b'-'
            || b[7] != b'-'
            || !b
                .iter()
                .enumerate()
                .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
        {
            return Err(SearchError::InvalidRequest);
        }
        let year: u16 = value[..4]
            .parse()
            .map_err(|_| SearchError::InvalidRequest)?;
        let month: usize = value[5..7]
            .parse()
            .map_err(|_| SearchError::InvalidRequest)?;
        let day: u8 = value[8..]
            .parse()
            .map_err(|_| SearchError::InvalidRequest)?;
        let leap =
            year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
        let days = [
            31,
            if leap { 29 } else { 28 },
            31,
            30,
            31,
            30,
            31,
            31,
            30,
            31,
            30,
            31,
        ];
        if year == 0 || !(1..=12).contains(&month) || day == 0 || day > days[month - 1] {
            return Err(SearchError::InvalidRequest);
        }
        Ok(Self(value.to_string()))
    }
}

impl<'de> Deserialize<'de> for UtcDate {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}
