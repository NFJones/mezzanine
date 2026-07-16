//! Auth data types, credential-store traits, and non-secret flow plans.
//!
//! These definitions describe auth metadata, credential-store selection, prompts,
//! entitlement checks, status reporting, and shared store traits.

use std::path::{Path, PathBuf};

use secrecy::SecretString;

use crate::error::{MezError, Result};

/// Defines the OS CREDENTIAL SERVICE const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const OS_CREDENTIAL_SERVICE: &str = "mezzanine";
/// Defines the SECRET SERVICE BACKEND const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const SECRET_SERVICE_BACKEND: &str = "secret-service";
/// Defines the SECRET TOOL BACKEND const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const SECRET_TOOL_BACKEND: &str = "secret-tool";
/// Defines the SECRET TOOL PROGRAM const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const SECRET_TOOL_PROGRAM: &str = "secret-tool";

/// Carries Auth Metadata state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthMetadata {
    /// Stores the provider value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub provider: String,
    /// Stores the credential kind value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub credential_kind: AuthCredentialKind,
    /// Stores the account id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub account_id: Option<String>,
    /// Stores the organization id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub organization_id: Option<String>,
    /// Stores the selected model profile value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub selected_model_profile: String,
    /// Stores the credential store ref value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub credential_store_ref: Option<String>,
    /// Stores the refresh credential store ref value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub refresh_credential_store_ref: Option<String>,
    /// Stores the token expires at value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub token_expires_at: Option<String>,
}

/// Non-secret MCP OAuth metadata bound to one configured MCP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpAuthMetadata {
    /// Stable configured MCP server identifier.
    pub server_id: String,
    /// Secret-bearing credential class represented by this MCP auth record.
    pub credential_kind: McpCredentialKind,
    /// Origin component of the URL that minted the credential.
    pub url_origin: String,
    /// Stable fingerprint of the full configured MCP URL.
    pub url_fingerprint: String,
    /// Optional non-secret OAuth scopes attached to the credential.
    pub scopes: Vec<String>,
    /// Optional OAuth client identifier used to mint/refresh the credential.
    pub client_id: Option<String>,
    /// Optional OAuth resource indicator sent during login/refresh.
    pub resource: Option<String>,
    /// Authorization endpoint used by the browser login flow.
    pub authorization_endpoint: Option<String>,
    /// Token endpoint used by login and refresh flows.
    pub token_endpoint: Option<String>,
    /// Opaque access-token credential-store reference.
    pub credential_store_ref: Option<String>,
    /// Opaque refresh-token credential-store reference.
    pub refresh_credential_store_ref: Option<String>,
    /// Optional Unix-seconds access-token expiration timestamp.
    pub token_expires_at: Option<String>,
}

/// Secret-bearing MCP credential class represented by MCP auth metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpCredentialKind {
    /// OAuth access token used as a bearer token for streamable HTTP MCP.
    OAuthBearer,
    /// Static bearer token stored without OAuth refresh semantics.
    StaticBearer,
}

impl McpCredentialKind {
    /// Returns the stable metadata string written to the MCP auth file.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OAuthBearer => "oauth-bearer",
            Self::StaticBearer => "static-bearer",
        }
    }

    /// Parses a stable metadata string into an MCP credential kind.
    pub fn from_metadata_value(value: &str) -> Option<Self> {
        match value {
            "oauth-bearer" => Some(Self::OAuthBearer),
            "static-bearer" => Some(Self::StaticBearer),
            _ => None,
        }
    }
}

/// Secret-safe MCP auth status for a configured server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpAuthStatus {
    /// Stable configured MCP server identifier.
    pub server_id: String,
    /// Whether a usable access-token secret is currently available.
    pub authenticated: bool,
    /// Whether metadata exists for this server.
    pub metadata_present: bool,
    /// Secret-safe credential availability state.
    pub credential_state: AuthCredentialState,
    /// Credential metadata when present.
    pub metadata: Option<McpAuthMetadata>,
    /// URL binding mismatch for current config, when detected.
    pub stale_url: bool,
}

/// Secret-bearing MCP OAuth credential returned by a login or refresh flow.
#[derive(Clone, PartialEq, Eq)]
pub struct McpOAuthCredential {
    /// Access token used in the MCP Authorization header.
    pub access_token: String,
    /// Optional refresh token used to renew the access token.
    pub refresh_token: Option<String>,
    /// Optional Unix-seconds access-token expiration timestamp.
    pub token_expires_at: Option<String>,
    /// Optional non-secret scopes granted with the credential.
    pub scopes: Vec<String>,
    /// Optional OAuth client identifier used to mint/refresh the credential.
    pub client_id: Option<String>,
    /// Optional OAuth resource indicator sent during login/refresh.
    pub resource: Option<String>,
    /// Authorization endpoint used by the login flow.
    pub authorization_endpoint: Option<String>,
    /// Token endpoint used by login and refresh flows.
    pub token_endpoint: Option<String>,
}

impl std::fmt::Debug for McpOAuthCredential {
    /// Formats MCP OAuth credentials without exposing bearer token material.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpOAuthCredential")
            .field("access_token", &"[REDACTED]")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("token_expires_at", &self.token_expires_at)
            .field("scopes", &self.scopes)
            .field("client_id", &self.client_id)
            .field("resource", &self.resource)
            .field("authorization_endpoint", &self.authorization_endpoint)
            .field("token_endpoint", &self.token_endpoint)
            .finish()
    }
}

/// Secret-bearing credential class represented by an auth metadata record.
///
/// The credential kind is non-secret. Runtime provider setup uses it to choose
/// the correct network endpoint and request headers for the persisted secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthCredentialKind {
    /// A direct provider API key that is accepted by the provider API endpoint.
    ApiKey,
    /// A ChatGPT account OAuth access token returned by browser or device login.
    ChatGpt,
}

impl AuthCredentialKind {
    /// Returns the stable metadata string written to the auth file.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApiKey => "api-key",
            Self::ChatGpt => "chatgpt",
        }
    }

    /// Parses a stable metadata string into a credential kind.
    pub fn from_metadata_value(value: &str) -> Option<Self> {
        match value {
            "api-key" => Some(Self::ApiKey),
            "chatgpt" => Some(Self::ChatGpt),
            _ => None,
        }
    }
}
/// Carries Auth Paths state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthPaths {
    /// Stores the root value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    root: PathBuf,
    /// Stores the auth file value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    auth_file: PathBuf,
    /// Stores the mcp auth file value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    mcp_auth_file: PathBuf,
    /// Stores the secret directory value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    secret_directory: PathBuf,
}

impl AuthPaths {
    /// Runs the under config root operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn under_config_root(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            auth_file: root.join("auth.toml"),
            mcp_auth_file: root.join("mcp-auth.toml"),
            secret_directory: root.join("auth-secrets"),
        }
    }

    /// Runs the root operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Runs the auth file operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn auth_file(&self) -> &Path {
        &self.auth_file
    }

    /// Runs the mcp auth file operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn mcp_auth_file(&self) -> &Path {
        &self.mcp_auth_file
    }

    /// Runs the secret directory operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn secret_directory(&self) -> &Path {
        &self.secret_directory
    }
}

/// Carries Auth Method state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthMethod {
    /// Represents the Api Key case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ApiKey,
    /// Represents the Browser case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Browser,
    /// Represents the Device Code case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    DeviceCode,
}

impl AuthMethod {
    /// Returns the stable command-line display name for this auth method.
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "test-only adapter retained for focused boundary coverage"
    )]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ApiKey => "api-key",
            Self::Browser => "browser",
            Self::DeviceCode => "device-code",
        }
    }
}

/// Selects one auth method from parsed command flags.
///
/// Browser login remains the default when no explicit method is selected. The
/// caller supplies the conflict message so CLI and command-language diagnostics
/// keep their existing wording.
pub fn selected_auth_method_from_flags(
    api_key: bool,
    browser: bool,
    device_code: bool,
    conflict_message: &str,
) -> Result<AuthMethod> {
    let selected_methods = [api_key, browser, device_code]
        .into_iter()
        .filter(|selected| *selected)
        .count();
    if selected_methods > 1 {
        return Err(MezError::invalid_args(conflict_message));
    }
    if api_key {
        Ok(AuthMethod::ApiKey)
    } else if device_code {
        Ok(AuthMethod::DeviceCode)
    } else {
        Ok(AuthMethod::Browser)
    }
}

/// Carries Credential Store Kind state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialStoreKind {
    /// Represents the Operating System case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    OperatingSystem,
    /// Represents the Private File Fallback case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    PrivateFileFallback,
}

impl CredentialStoreKind {
    /// Runs the reference prefix operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn reference_prefix(self) -> &'static str {
        match self {
            Self::OperatingSystem => "os-keyring:",
            Self::PrivateFileFallback => "file:",
        }
    }
}

/// Carries Credential Store Plan state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialStorePlan {
    /// Represents the Operating System case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    OperatingSystem {
        /// Stores the service value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        service: String,
        /// Stores the account value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        account: String,
    },
    /// Represents the Private File Fallback case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    PrivateFileFallback {
        /// Stores the directory value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        directory: PathBuf,
        /// Stores the reason value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reason: FileCredentialFallbackReason,
    },
}

impl CredentialStorePlan {
    /// Runs the selected store operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub fn selected_store(&self) -> CredentialStoreKind {
        match self {
            Self::OperatingSystem { .. } => CredentialStoreKind::OperatingSystem,
            Self::PrivateFileFallback { .. } => CredentialStoreKind::PrivateFileFallback,
        }
    }
}

/// Carries File Credential Fallback Reason state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileCredentialFallbackReason {
    /// Represents the Operating System Store Unavailable case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    OperatingSystemStoreUnavailable,
}

/// Carries Auth Flow Plan state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(test)]
pub struct AuthFlowPlan {
    /// Stores the provider value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub provider: String,
    /// Stores the method value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub method: AuthMethod,
    /// Stores the credential target value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub credential_target: String,
    /// Stores the credential store value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub credential_store: CredentialStorePlan,
    /// Stores the prompt value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub prompt: AuthInteractivePromptPlan,
    /// Stores the browser value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub browser: Option<ProviderBrowserFlowPlan>,
    /// Stores the entitlement value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub entitlement: ProviderEntitlementPlan,
    /// Stores the user instruction value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub user_instruction: String,
}

/// Interactive prompt work needed before auth can continue.
///
/// This plan is intentionally non-secret: it describes what the terminal or
/// configuration shell should ask for, but it never carries a collected value.
/// Collected secret values must be passed to `login_with_api_key` or a later
/// provider exchange path so the existing credential-store boundary owns
/// persistence.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(test)]
pub struct AuthInteractivePromptPlan {
    /// Stores the prompt id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub prompt_id: String,
    /// Stores the action value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub action: AuthPromptAction,
    /// Stores the label value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub label: String,
    /// Stores the secret input value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub secret_input: bool,
    /// Stores the required value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub required: bool,
    /// Stores the persist via credential store value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub persist_via_credential_store: bool,
}

/// Carries Auth Prompt Action state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
pub enum AuthPromptAction {
    /// Represents the Open Browser case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    OpenBrowser,
    /// Represents the Collect Secret case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    CollectSecret,
}

/// Browser auth work that can be displayed by a UI adapter.
///
/// The plan is non-secret and descriptive. The CLI may execute a concrete
/// provider exchange separately, but this structure must not carry callback
/// codes, OAuth tokens, provider bearer credentials, or refresh tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(test)]
pub struct ProviderBrowserFlowPlan {
    /// Stores the requires network value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub requires_network: bool,
    /// Stores the launch url value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub launch_url: Option<String>,
    /// Stores the callback binding value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub callback_binding: Option<String>,
    /// Stores the stores secret after exchange value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub stores_secret_after_exchange: bool,
}

/// Provider entitlement validation that must happen before using a credential.
///
/// Entitlements are provider assertions such as model/API access. They are not
/// known locally without a provider exchange, so the plan records the deferred
/// check instead of claiming access.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(test)]
pub struct ProviderEntitlementPlan {
    /// Stores the validation value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub validation: ProviderEntitlementValidation,
    /// Stores the requested entitlements value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub requested_entitlements: Vec<String>,
    /// Stores the persistence value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub persistence: ProviderEntitlementPersistence,
}

/// Carries Provider Entitlement Validation state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
pub enum ProviderEntitlementValidation {
    /// Represents the Deferred Until Browser Exchange case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    DeferredUntilBrowserExchange,
    /// Represents the Deferred Until Credential Validation case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    DeferredUntilCredentialValidation,
}

/// Carries Provider Entitlement Persistence state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
pub enum ProviderEntitlementPersistence {
    /// Represents the Non Secret Metadata Only case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    #[allow(
        dead_code,
        reason = "test-only adapter retained for focused boundary coverage"
    )]
    NonSecretMetadataOnly,
    /// Represents the Credential Store Reference Only case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    CredentialStoreReferenceOnly,
}

/// Carries Auth Status state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthStatus {
    /// Stores the authenticated value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub authenticated: bool,
    /// Stores the metadata value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub metadata: Option<AuthMetadata>,
    /// Stores the credential state value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub credential_state: AuthCredentialState,
}

/// Carries Auth Credential State state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthCredentialState {
    /// Represents the Logged Out case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    LoggedOut,
    /// Represents the Missing Secret case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    MissingSecret {
        /// Stores the reference value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reference: Option<String>,
    },
    /// Represents the Available case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Available {
        /// Stores the store value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        store: CredentialStoreKind,
        /// Stores the reference value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reference: String,
    },
}

impl AuthCredentialState {
    /// Runs the authenticated operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn authenticated(&self) -> bool {
        matches!(self, Self::Available { .. })
    }
}

/// Defines the Credential Store behavior contract for this subsystem.
///
/// Implementors provide the concrete I/O or state transition boundary
/// consumed by higher-level orchestration code.
pub trait CredentialStore {
    /// Runs the kind operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[allow(
        dead_code,
        reason = "credential backends expose their selected storage kind"
    )]
    fn kind(&self) -> CredentialStoreKind;
    /// Runs the store secret operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn store_secret(&self, provider: &str, secret: &SecretString) -> Result<String>;
    /// Runs the load secret operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn load_secret(&self, reference: &str) -> Result<Option<SecretString>>;
    /// Runs the delete secret operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn delete_secret(&self, reference: &str) -> Result<bool>;

    /// Runs the contains secret operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn contains_secret(&self, reference: &str) -> Result<bool> {
        Ok(self.load_secret(reference)?.is_some())
    }
}

/// Carries Credential Store Availability state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialStoreAvailability {
    /// Represents the Available case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Available,
    /// Represents the Unavailable case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Unavailable {
        /// Stores the reason value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reason: String,
    },
}
