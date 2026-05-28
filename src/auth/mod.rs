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

pub use command_store::{
    CommandBackedCredentialStore, CommandCredentialBackend, CredentialCommandOutput,
    CredentialCommandRunner, SystemCredentialCommandRunner,
};
pub use file_store::PrivateFileCredentialStore;
pub use mcp_oauth::{refresh_mcp_oauth_credential_async, run_mcp_oauth_login_async};
pub use openai_oauth::{
    OpenAiProviderCredential, run_openai_browser_login_async,
    run_openai_browser_login_with_theme_async, run_openai_device_code_login_async,
};
pub use secret_service::NativeSecretServiceCredentialStore;
pub use store::AuthStore;
pub use types::{
    AuthCredentialKind, AuthCredentialState, AuthFlowPlan, AuthInteractivePromptPlan, AuthMetadata,
    AuthMethod, AuthPaths, AuthPromptAction, AuthStatus, CredentialStore,
    CredentialStoreAvailability, CredentialStoreKind, CredentialStorePlan,
    FileCredentialFallbackReason, McpAuthMetadata, McpAuthStatus, McpCredentialKind,
    McpOAuthCredential, ProviderBrowserFlowPlan, ProviderEntitlementPersistence,
    ProviderEntitlementPlan, ProviderEntitlementValidation,
};

#[cfg(test)]
use types::SECRET_TOOL_PROGRAM;

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
