//! Provider authentication routing contracts.
//!
//! This module owns the non-secret values that provider adapters use to
//! select wire endpoints and headers. Credential persistence, secret lookup,
//! refresh flows, and product error conversion remain in the Mezzanine
//! composition crate.

/// Stable metadata value for direct provider API-key credentials.
pub const API_KEY_CREDENTIAL_KIND: &str = "api-key";
/// Stable metadata value for ChatGPT browser or device credentials.
pub const CHATGPT_CREDENTIAL_KIND: &str = "chatgpt";

/// Non-secret credential class used to select provider wire behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderCredentialKind {
    /// Direct provider API-key authentication.
    ApiKey,
    /// ChatGPT browser or device authentication.
    ChatGpt,
}

impl ProviderCredentialKind {
    /// Returns the stable metadata identifier for this credential class.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApiKey => API_KEY_CREDENTIAL_KIND,
            Self::ChatGpt => CHATGPT_CREDENTIAL_KIND,
        }
    }

    /// Parses one stable credential metadata identifier.
    pub fn from_id(value: &str) -> Option<Self> {
        match value {
            API_KEY_CREDENTIAL_KIND => Some(Self::ApiKey),
            CHATGPT_CREDENTIAL_KIND => Some(Self::ChatGpt),
            _ => None,
        }
    }
}

/// Secret-safe provider authentication metadata needed by wire adapters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAuthMetadata {
    /// Credential class that selects the provider endpoint and auth scheme.
    pub credential_kind: ProviderCredentialKind,
    /// Optional ChatGPT account identifier used for account routing.
    pub account_id: Option<String>,
    /// Optional direct-API organization identifier used for request routing.
    pub organization_id: Option<String>,
}

impl ProviderAuthMetadata {
    /// Creates provider authentication metadata for one credential class.
    pub fn new(credential_kind: ProviderCredentialKind) -> Self {
        Self {
            credential_kind,
            account_id: None,
            organization_id: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ProviderAuthMetadata, ProviderCredentialKind};

    #[test]
    /// Verifies credential classes preserve their stable persisted identifiers
    /// and reject unknown values at the provider-neutral contract boundary.
    fn provider_credential_kind_round_trips_stable_ids() {
        for kind in [
            ProviderCredentialKind::ApiKey,
            ProviderCredentialKind::ChatGpt,
        ] {
            assert_eq!(ProviderCredentialKind::from_id(kind.as_str()), Some(kind));
        }
        assert_eq!(ProviderCredentialKind::from_id("oauth"), None);
    }

    #[test]
    /// Verifies new secret-safe provider metadata starts without optional
    /// routing identifiers so product adapters must supply them explicitly.
    fn provider_auth_metadata_defaults_optional_routing_ids() {
        let metadata = ProviderAuthMetadata::new(ProviderCredentialKind::ApiKey);

        assert_eq!(metadata.credential_kind, ProviderCredentialKind::ApiKey);
        assert_eq!(metadata.account_id, None);
        assert_eq!(metadata.organization_id, None);
    }
}
