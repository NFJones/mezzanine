//! Local-only search authentication, separate from model and MCP credentials.
//!
//! Public references contain only a bounded logical name or an explicitly named
//! host environment variable. Provider origins are code-owned. Metadata never
//! contains secret values or caller-supplied backend paths. Capturing a binding
//! is not authorization: the future runtime caller must authorize its captured
//! request before resolving it. This module performs no HTTP, provider probing,
//! profile selection, environment discovery, or approval-policy changes.

use serde::{Deserialize, Serialize};

use crate::error::{MezError, Result};

/// Fixed OS service shared only by the two existing search backend adapters.
pub(super) const OS_SERVICE: &str = "mez-search-auth";

/// Reuses existing stores with exact code-owned search-domain references.
mod backend;
/// Versioned non-secret metadata and forward-only credential transactions.
mod storage;
#[allow(
    unused_imports,
    reason = "future trusted search dispatch must name its retained opaque SearchBinding without exposing persistence internals"
)]
pub use storage::SearchBinding;
pub use storage::{SearchAuthStatus, SearchAuthStore};

/// Fixed search providers whose credentials cannot be used for arbitrary URLs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum SearchProvider {
    /// Brave Search API.
    Brave,
    /// Tavily Search API.
    Tavily,
    /// Exa Search API.
    Exa,
    /// SerpApi search API.
    Serpapi,
}

impl SearchProvider {
    /// Returns the only HTTPS origin permitted for this provider's secret.
    pub fn origin(self) -> &'static str {
        match self {
            Self::Brave => "https://api.search.brave.com",
            Self::Tavily => "https://api.tavily.com",
            Self::Exa => "https://api.exa.ai",
            Self::Serpapi => "https://serpapi.com",
        }
    }
}

/// Non-secret selector, never a literal token, path, command, or fallback list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchCredentialRef {
    /// A logical identity in the dedicated search auth domain.
    Stored(String),
    /// Exactly one explicitly selected primary host/service environment name.
    Environment(String),
}

impl SearchCredentialRef {
    /// Parses a bounded explicit reference without reading any environment or
    /// storage. Errors deliberately never quote the supplied text.
    pub fn parse(value: &str) -> Result<Self> {
        if let Some(name) = value.strip_prefix("search-auth:") {
            validate_name(name)?;
            return Ok(Self::Stored(name.to_owned()));
        }
        if let Some(name) = value.strip_prefix("env:") {
            let mut bytes = name.bytes();
            if (1..=128).contains(&name.len())
                && bytes
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
                && bytes.all(|c| c.is_ascii_alphanumeric() || c == b'_')
            {
                return Ok(Self::Environment(name.to_owned()));
            }
        }
        Err(MezError::invalid_args(
            "invalid search credential reference",
        ))
    }
}

/// Rejects ambiguous, unbounded, path-like, or shell-like search identities.
pub fn validate_name(name: &str) -> Result<()> {
    let mut bytes = name.bytes();
    if !(1..=64).contains(&name.len())
        || !bytes.next().is_some_and(|c| c.is_ascii_lowercase())
        || !bytes.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-'))
    {
        return Err(MezError::invalid_args("invalid search credential name"));
    }
    Ok(())
}

/// Fake-only source contracts; no process environment or credential service.
#[cfg(test)]
mod tests {
    use super::*;

    /// References accept only explicit bounded names. Raw secrets, traversal,
    /// inference/MCP references and shell expansion must fail without I/O.
    #[test]
    fn reference_grammar_is_domain_specific_and_non_interpolating() {
        assert_eq!(
            SearchCredentialRef::parse("search-auth:work-1").unwrap(),
            SearchCredentialRef::Stored("work-1".into())
        );
        assert_eq!(
            SearchCredentialRef::parse("env:SEARCH_KEY").unwrap(),
            SearchCredentialRef::Environment("SEARCH_KEY".into())
        );
        for invalid in [
            "file:/tmp/key",
            "os-keyring:secret-service/mezzanine/openai",
            "mcp:work",
            "search-auth:../openai",
            "search-auth:a/b",
            "search-auth:UPPER",
            "env:1KEY",
            "env:${KEY}",
            "env:KEY|OTHER",
            "synthetic-canary-secret",
        ] {
            let error = SearchCredentialRef::parse(invalid).unwrap_err();
            assert!(!format!("{error:?}").contains(invalid));
        }
        assert!(SearchCredentialRef::parse(&format!("search-auth:{}", "a".repeat(65))).is_err());
        assert!(SearchCredentialRef::parse(&format!("env:{}", "A".repeat(129))).is_err());
    }

    /// Provider identity maps to one exact code-owned HTTPS origin, never an
    /// endpoint override or inference profile selected by metadata.
    #[test]
    fn providers_have_fixed_https_origins() {
        assert_eq!(
            SearchProvider::Brave.origin(),
            "https://api.search.brave.com"
        );
        assert_eq!(SearchProvider::Tavily.origin(), "https://api.tavily.com");
        assert_eq!(SearchProvider::Exa.origin(), "https://api.exa.ai");
        assert_eq!(SearchProvider::Serpapi.origin(), "https://serpapi.com");
    }
}
