//! Provider authentication metadata and local credential storage.
//!
//! Secret-bearing provider credentials are kept behind a credential-store
//! interface. Auth metadata records only non-secret provider state and opaque
//! references to stored credentials.

/// Exposes the command store module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod command_store;
/// Exposes the file store module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod file_store;
/// Exposes the fs module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod fs;
/// Exposes the MCP oauth module boundary.
///
/// The nested module keeps MCP-specific OAuth discovery, browser login, and
/// refresh mechanics isolated from provider auth code.
mod mcp_oauth;
/// Exposes the metadata module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod metadata;
/// Exposes the openai oauth module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod openai_oauth;
/// Exposes the secret service module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod secret_service;
/// Exposes the store module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod store;
/// Exposes the types module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod types;

#[cfg(test)]
pub use command_store::{
    CommandBackedCredentialStore, CommandCredentialBackend, CredentialCommandOutput,
    CredentialCommandRunner, SystemCredentialCommandRunner,
};
#[cfg(test)]
pub use file_store::PrivateFileCredentialStore;

pub use mcp_oauth::run_mcp_oauth_login_async;
pub use openai_oauth::{
    OpenAiProviderCredential, run_openai_browser_login_with_theme_async,
    run_openai_device_code_login_async,
};
pub use store::{AuthStore, DEFAULT_PROVIDER_AUTH_REFRESH_LEEWAY_SECONDS};
pub use types::{
    AuthCredentialKind, AuthCredentialState, AuthMetadata, AuthMethod, AuthPaths, AuthStatus,
    CredentialStoreKind, McpAuthMetadata, McpAuthStatus, McpCredentialKind, McpOAuthCredential,
    selected_auth_method_from_flags,
};
#[cfg(test)]
pub use types::{
    CredentialStore, CredentialStoreAvailability, CredentialStorePlan, FileCredentialFallbackReason,
};

impl mez_agent::ProviderCredentialSource for AuthStore {
    type Error = crate::error::MezError;
    type Credential = secrecy::SecretString;

    /// Adapts persisted product metadata into the provider-neutral routing
    /// contract without exposing credential-store details to the agent crate.
    fn provider_auth_metadata(
        &self,
        provider: &str,
    ) -> Result<Option<mez_agent::ProviderAuthMetadata>, Self::Error> {
        self.read_metadata_for_provider(provider).map(|metadata| {
            metadata.map(|metadata| mez_agent::ProviderAuthMetadata {
                credential_kind: match metadata.credential_kind {
                    AuthCredentialKind::ApiKey => mez_agent::ProviderCredentialKind::ApiKey,
                    AuthCredentialKind::ChatGpt => mez_agent::ProviderCredentialKind::ChatGpt,
                },
                account_id: metadata.account_id,
                organization_id: metadata.organization_id,
            })
        })
    }

    /// Retrieves one provider credential through the product-owned secret
    /// store while presenting only the narrow agent credential-source port.
    fn provider_credential(&self, provider: &str) -> Result<Self::Credential, Self::Error> {
        self.provider_secret(provider)
    }
}

#[cfg(test)]
use types::SECRET_TOOL_PROGRAM;

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
