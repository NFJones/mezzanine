//! High-level auth store orchestration.
//!
//! AuthStore coordinates metadata persistence, auth-flow planning, provider login,
//! credential loading, credential-state reporting, and logout cleanup.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use secrecy::{ExposeSecret, SecretString};

use crate::error::{MezError, Result};

use super::command_store::{CommandBackedCredentialStore, CredentialCommandRunner};
use super::file_store::PrivateFileCredentialStore;
use super::fs::{
    ensure_private_dir, path_is_under_directory, set_private_file, validate_safe_name,
};
use super::mcp_oauth::refresh_mcp_oauth_credential_async;
use super::metadata::{
    decode_all_mcp_metadata, decode_all_metadata, encode_all_mcp_metadata, encode_all_metadata,
};
use super::openai_oauth::{OpenAiProviderCredential, refresh_openai_provider_credential_async};
use super::secret_service::NativeSecretServiceCredentialStore;
use super::types::{
    AuthCredentialKind, AuthCredentialState, AuthFlowPlan, AuthInteractivePromptPlan, AuthMetadata,
    AuthMethod, AuthPaths, AuthPromptAction, AuthStatus, CredentialStore,
    CredentialStoreAvailability, CredentialStoreKind, CredentialStorePlan,
    FileCredentialFallbackReason, McpAuthMetadata, McpAuthStatus, McpOAuthCredential,
    ProviderBrowserFlowPlan, ProviderEntitlementPersistence, ProviderEntitlementPlan,
    ProviderEntitlementValidation,
};

/// Defines the OPENAI PROVIDER const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const OPENAI_PROVIDER: &str = "openai";
/// Defines the OPENAI REFRESH CREDENTIAL NAME const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const OPENAI_REFRESH_CREDENTIAL_NAME: &str = "openai-refresh";
/// Defines the DEFAULT PROVIDER AUTH REFRESH LEEWAY SECONDS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const DEFAULT_PROVIDER_AUTH_REFRESH_LEEWAY_SECONDS: u64 = 24 * 60 * 60;

/// Carries Auth Store state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthStore {
    /// Stores the paths value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    paths: AuthPaths,
}

impl AuthStore {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(paths: AuthPaths) -> Self {
        Self { paths }
    }

    /// Runs the paths operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn paths(&self) -> &AuthPaths {
        &self.paths
    }

    /// Runs the plan openai flow operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn plan_openai_flow(&self, method: AuthMethod) -> AuthFlowPlan {
        let os_store = CommandBackedCredentialStore::secret_tool();
        let mut plan = self.plan_openai_flow_with_os_store(method, &os_store);
        plan.credential_store = self.credential_store_plan("openai");
        plan
    }

    /// Runs the plan provider flow operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn plan_provider_flow(&self, provider: &str, method: AuthMethod) -> AuthFlowPlan {
        let credential_store = self.credential_store_plan(provider);
        AuthFlowPlan {
            provider: provider.to_string(),
            method,
            credential_target: String::new(),
            credential_store,
            prompt: AuthInteractivePromptPlan {
                prompt_id: String::new(),
                action: AuthPromptAction::CollectSecret,
                label: String::new(),
                secret_input: true,
                required: false,
                persist_via_credential_store: false,
            },
            browser: None,
            entitlement: ProviderEntitlementPlan {
                validation: ProviderEntitlementValidation::DeferredUntilCredentialValidation,
                requested_entitlements: Vec::new(),
                persistence: ProviderEntitlementPersistence::CredentialStoreReferenceOnly,
            },
            user_instruction: String::new(),
        }
    }

    /// Runs the plan openai flow with os store operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn plan_openai_flow_with_os_store<R: CredentialCommandRunner>(
        &self,
        method: AuthMethod,
        os_store: &CommandBackedCredentialStore<R>,
    ) -> AuthFlowPlan {
        let (prompt, browser, entitlement, user_instruction) = match method {
            AuthMethod::Browser => (
                AuthInteractivePromptPlan {
                    prompt_id: "openai-browser-sign-in".to_string(),
                    action: AuthPromptAction::OpenBrowser,
                    label: "OpenAI browser sign-in".to_string(),
                    secret_input: false,
                    required: true,
                    persist_via_credential_store: true,
                },
                Some(ProviderBrowserFlowPlan {
                    requires_network: true,
                    launch_url: None,
                    callback_binding: None,
                    stores_secret_after_exchange: true,
                }),
                ProviderEntitlementPlan {
                    validation: ProviderEntitlementValidation::DeferredUntilBrowserExchange,
                    requested_entitlements: vec!["responses_api".to_string()],
                    persistence: ProviderEntitlementPersistence::CredentialStoreReferenceOnly,
                },
                "open a browser-based provider sign-in flow from the configuration shell",
            ),
            AuthMethod::DeviceCode => (
                AuthInteractivePromptPlan {
                    prompt_id: "openai-device-code-sign-in".to_string(),
                    action: AuthPromptAction::OpenBrowser,
                    label: "OpenAI device-code sign-in".to_string(),
                    secret_input: false,
                    required: true,
                    persist_via_credential_store: true,
                },
                Some(ProviderBrowserFlowPlan {
                    requires_network: true,
                    launch_url: None,
                    callback_binding: None,
                    stores_secret_after_exchange: true,
                }),
                ProviderEntitlementPlan {
                    validation: ProviderEntitlementValidation::DeferredUntilBrowserExchange,
                    requested_entitlements: vec!["responses_api".to_string()],
                    persistence: ProviderEntitlementPersistence::CredentialStoreReferenceOnly,
                },
                "complete an OpenAI device-code provider sign-in flow from the configuration shell",
            ),
            AuthMethod::ApiKey => (
                AuthInteractivePromptPlan {
                    prompt_id: "openai-api-key".to_string(),
                    action: AuthPromptAction::CollectSecret,
                    label: "OpenAI API key".to_string(),
                    secret_input: true,
                    required: true,
                    persist_via_credential_store: true,
                },
                None,
                ProviderEntitlementPlan {
                    validation: ProviderEntitlementValidation::DeferredUntilCredentialValidation,
                    requested_entitlements: vec!["responses_api".to_string()],
                    persistence: ProviderEntitlementPersistence::CredentialStoreReferenceOnly,
                },
                "enter an OpenAI API key through the configuration shell",
            ),
        };
        AuthFlowPlan {
            provider: "openai".to_string(),
            method,
            credential_target: self.paths.auth_file().display().to_string(),
            credential_store: self.credential_store_plan_with_os_store("openai", os_store),
            prompt,
            browser,
            entitlement,
            user_instruction: user_instruction.to_string(),
        }
    }

    /// Runs the credential store plan operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn credential_store_plan(&self, provider: &str) -> CredentialStorePlan {
        let native_store = NativeSecretServiceCredentialStore::new();
        if matches!(
            native_store.availability(),
            CredentialStoreAvailability::Available
        ) {
            return CredentialStorePlan::OperatingSystem {
                service: native_store.plan_service(),
                account: provider.to_string(),
            };
        }

        let os_store = CommandBackedCredentialStore::secret_tool();
        self.credential_store_plan_with_os_store(provider, &os_store)
    }

    /// Runs the credential store plan with os store operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn credential_store_plan_with_os_store<R: CredentialCommandRunner>(
        &self,
        provider: &str,
        os_store: &CommandBackedCredentialStore<R>,
    ) -> CredentialStorePlan {
        if matches!(
            os_store.availability(),
            Ok(CredentialStoreAvailability::Available)
        ) {
            return CredentialStorePlan::OperatingSystem {
                service: os_store.plan_service(),
                account: provider.to_string(),
            };
        }

        CredentialStorePlan::PrivateFileFallback {
            directory: self.paths.secret_directory().join(provider),
            reason: FileCredentialFallbackReason::OperatingSystemStoreUnavailable,
        }
    }

    /// Parses an optional command-line credential-store selector.
    ///
    /// `None` means callers should use the store's preferred backend. Explicit
    /// selectors keep the user-facing CLI vocabulary centralized at the auth
    /// boundary instead of repeating it in provider and MCP callers.
    fn selected_credential_store_kind(
        credential_store: Option<&str>,
    ) -> Result<Option<CredentialStoreKind>> {
        match credential_store {
            Some("file") => Ok(Some(CredentialStoreKind::PrivateFileFallback)),
            Some("os") => Ok(Some(CredentialStoreKind::OperatingSystem)),
            Some(other) => Err(MezError::invalid_args(format!(
                "unknown credential store `{other}`"
            ))),
            None => Ok(None),
        }
    }

    /// Runs the file credential store operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn file_credential_store(&self, provider: &str) -> Result<PrivateFileCredentialStore> {
        validate_safe_name(provider, "provider name is not file-safe")?;
        Ok(PrivateFileCredentialStore::new(
            self.paths.secret_directory().join(provider),
        ))
    }

    /// Runs the write metadata operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn write_metadata(&self, metadata: &AuthMetadata) -> Result<PathBuf> {
        metadata.validate_non_secret()?;
        let mut existing = self.read_all_metadata()?;
        existing.insert(metadata.provider.clone(), metadata.clone());
        ensure_private_dir(self.paths.root())?;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(self.paths.auth_file())?;
        file.write_all(encode_all_metadata(&existing).as_bytes())?;
        file.sync_all()?;
        set_private_file(self.paths.auth_file())?;
        Ok(self.paths.auth_file().to_path_buf())
    }

    /// Writes non-secret MCP auth metadata without storing token material.
    pub fn write_mcp_metadata(&self, metadata: &McpAuthMetadata) -> Result<PathBuf> {
        metadata.validate_non_secret()?;
        let mut existing = self.read_all_mcp_metadata()?;
        existing.insert(metadata.server_id.clone(), metadata.clone());
        ensure_private_dir(self.paths.root())?;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(self.paths.mcp_auth_file())?;
        file.write_all(encode_all_mcp_metadata(&existing).as_bytes())?;
        file.sync_all()?;
        set_private_file(self.paths.mcp_auth_file())?;
        Ok(self.paths.mcp_auth_file().to_path_buf())
    }

    /// Reads all provider metadata entries from the auth file.
    pub fn read_all_metadata(&self) -> Result<BTreeMap<String, AuthMetadata>> {
        if !self.paths.auth_file().exists() {
            return Ok(BTreeMap::new());
        }
        let mut data = String::new();
        fs::File::open(self.paths.auth_file())?.read_to_string(&mut data)?;
        decode_all_metadata(&data)
    }

    /// Reads all MCP auth metadata entries from the MCP auth file.
    pub fn read_all_mcp_metadata(&self) -> Result<BTreeMap<String, McpAuthMetadata>> {
        if !self.paths.mcp_auth_file().exists() {
            return Ok(BTreeMap::new());
        }
        let mut data = String::new();
        fs::File::open(self.paths.mcp_auth_file())?.read_to_string(&mut data)?;
        decode_all_mcp_metadata(&data)
    }

    /// Reads metadata for a specific provider.
    pub fn read_metadata_for_provider(&self, provider: &str) -> Result<Option<AuthMetadata>> {
        Ok(self.read_all_metadata()?.remove(provider))
    }

    /// Reads MCP auth metadata for a configured MCP server id.
    pub fn read_mcp_metadata_for_server(&self, server_id: &str) -> Result<Option<McpAuthMetadata>> {
        Ok(self.read_all_mcp_metadata()?.remove(server_id))
    }

    /// Runs the read metadata operation for this subsystem.
    ///
    /// Returns metadata for the first available provider.
    pub fn read_metadata(&self) -> Result<Option<AuthMetadata>> {
        Ok(self.read_all_metadata()?.into_values().next())
    }

    /// Runs the status operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn status(&self) -> Result<AuthStatus> {
        let metadata_entries = self.read_all_metadata()?;
        let mut first_status = None;
        for metadata in metadata_entries.values() {
            let credential_state = self.credential_state(Some(metadata))?;
            let status = AuthStatus {
                authenticated: credential_state.authenticated(),
                metadata: Some(metadata.clone()),
                credential_state,
            };
            if status.authenticated {
                return Ok(status);
            }
            first_status.get_or_insert(status);
        }
        Ok(first_status.unwrap_or(AuthStatus {
            authenticated: false,
            metadata: None,
            credential_state: AuthCredentialState::LoggedOut,
        }))
    }

    /// Reports secret-safe MCP auth status for one configured server.
    pub fn mcp_status(
        &self,
        server_id: &str,
        url_origin: Option<&str>,
        url_fingerprint: Option<&str>,
    ) -> Result<McpAuthStatus> {
        validate_safe_name(server_id, "MCP server id is not credential-store safe")?;
        let metadata = self.read_mcp_metadata_for_server(server_id)?;
        let credential_state = self.mcp_credential_state(metadata.as_ref())?;
        let stale_url = metadata.as_ref().is_some_and(|metadata| {
            url_origin.is_some_and(|origin| origin != metadata.url_origin)
                || url_fingerprint
                    .is_some_and(|fingerprint| fingerprint != metadata.url_fingerprint)
        });
        Ok(McpAuthStatus {
            server_id: server_id.to_string(),
            authenticated: credential_state.authenticated() && !stale_url,
            metadata_present: metadata.is_some(),
            credential_state,
            metadata,
            stale_url,
        })
    }

    /// Runs the write file secret operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn write_file_secret(&self, provider: &str, secret: &str) -> Result<String> {
        ensure_private_dir(self.paths.root())?;
        ensure_private_dir(self.paths.secret_directory())?;
        let secret = SecretString::from(secret);
        self.file_credential_store(provider)?
            .store_secret(provider, &secret)
    }

    /// Stores an MCP OAuth credential through the chosen credential store.
    pub fn login_mcp_oauth_credential(
        &self,
        mut metadata: McpAuthMetadata,
        credential: McpOAuthCredential,
        credential_store: &dyn CredentialStore,
    ) -> Result<McpAuthMetadata> {
        validate_safe_name(
            &metadata.server_id,
            "MCP server id is not credential-store safe",
        )?;
        if credential.access_token.trim().is_empty() {
            return Err(MezError::invalid_args(
                "MCP OAuth access token must not be empty",
            ));
        }
        let access_secret = SecretString::from(credential.access_token);
        let access_name = mcp_access_credential_name(&metadata.server_id);
        metadata.credential_store_ref =
            Some(credential_store.store_secret(&access_name, &access_secret)?);
        if let Some(refresh_token) = credential.refresh_token {
            let refresh_secret = SecretString::from(refresh_token);
            let refresh_name = mcp_refresh_credential_name(&metadata.server_id);
            metadata.refresh_credential_store_ref =
                Some(credential_store.store_secret(&refresh_name, &refresh_secret)?);
        }
        metadata.token_expires_at = credential.token_expires_at;
        metadata.scopes = credential.scopes;
        metadata.client_id = credential.client_id;
        metadata.resource = credential.resource;
        metadata.authorization_endpoint = credential.authorization_endpoint;
        metadata.token_endpoint = credential.token_endpoint;
        self.write_mcp_metadata(&metadata)?;
        Ok(metadata)
    }

    /// Stores an MCP OAuth credential through the default OS credential store.
    pub fn login_mcp_oauth_credential_with_default_os_store(
        &self,
        metadata: McpAuthMetadata,
        credential: McpOAuthCredential,
    ) -> Result<McpAuthMetadata> {
        let native_store = NativeSecretServiceCredentialStore::new();
        if matches!(
            native_store.availability(),
            CredentialStoreAvailability::Available
        ) {
            return self.login_mcp_oauth_credential(metadata, credential, &native_store);
        }

        let command_store = CommandBackedCredentialStore::secret_tool();
        self.login_mcp_oauth_credential(metadata, credential, &command_store)
    }

    /// Stores an MCP OAuth credential through a selected or preferred store.
    pub fn login_mcp_oauth_credential_with_selected_store(
        &self,
        metadata: McpAuthMetadata,
        credential: McpOAuthCredential,
        credential_store: Option<&str>,
    ) -> Result<McpAuthMetadata> {
        let server_id = metadata.server_id.clone();
        match Self::selected_credential_store_kind(credential_store)? {
            Some(CredentialStoreKind::PrivateFileFallback) => {
                let file_store = self.file_credential_store(&server_id)?;
                self.login_mcp_oauth_credential(metadata, credential, &file_store)
            }
            Some(CredentialStoreKind::OperatingSystem) => {
                self.login_mcp_oauth_credential_with_default_os_store(metadata, credential)
            }
            None => match self.credential_store_plan(&server_id) {
                CredentialStorePlan::OperatingSystem { .. } => {
                    self.login_mcp_oauth_credential_with_default_os_store(metadata, credential)
                }
                CredentialStorePlan::PrivateFileFallback { .. } => {
                    let file_store = self.file_credential_store(&server_id)?;
                    self.login_mcp_oauth_credential(metadata, credential, &file_store)
                }
            },
        }
    }

    /// Loads the MCP OAuth access token for a configured server.
    pub fn mcp_access_token(&self, server_id: &str) -> Result<SecretString> {
        validate_safe_name(server_id, "MCP server id is not credential-store safe")?;
        let metadata = self
            .read_mcp_metadata_for_server(server_id)?
            .ok_or_else(|| {
                MezError::invalid_state(format!("MCP server `{server_id}` is not authenticated"))
            })?;
        let reference = metadata.credential_store_ref.as_deref().ok_or_else(|| {
            MezError::invalid_state("MCP auth metadata has no credential reference")
        })?;
        self.load_secret(reference)?
            .filter(|secret| !secret.expose_secret().trim().is_empty())
            .ok_or_else(|| MezError::invalid_state("MCP OAuth credential is unavailable"))
    }

    /// Loads the MCP OAuth refresh token for a configured server when available.
    pub fn mcp_refresh_token(&self, server_id: &str) -> Result<Option<SecretString>> {
        validate_safe_name(server_id, "MCP server id is not credential-store safe")?;
        let Some(metadata) = self.read_mcp_metadata_for_server(server_id)? else {
            return Ok(None);
        };
        let Some(reference) = metadata.refresh_credential_store_ref.as_deref() else {
            return Ok(None);
        };
        self.load_secret(reference)
    }

    /// Refreshes and writes back one stored MCP OAuth credential.
    pub async fn refresh_mcp_oauth_credential_for_server_async(
        &self,
        server_id: &str,
    ) -> Result<bool> {
        validate_safe_name(server_id, "MCP server id is not credential-store safe")?;
        let Some(mut metadata) = self.read_mcp_metadata_for_server(server_id)? else {
            return Ok(false);
        };
        let Some(refresh_reference) = metadata.refresh_credential_store_ref.as_deref() else {
            return Ok(false);
        };
        let Some(refresh_token) = self.load_secret(refresh_reference)? else {
            return Ok(false);
        };
        let token_endpoint = metadata
            .token_endpoint
            .clone()
            .ok_or_else(|| MezError::invalid_state("MCP auth metadata has no token endpoint"))?;
        let credential = refresh_mcp_oauth_credential_async(
            &token_endpoint,
            refresh_token.expose_secret(),
            metadata.client_id.as_deref(),
            metadata.resource.as_deref(),
        )
        .await?;
        self.update_mcp_oauth_credential(&mut metadata, credential)?;
        Ok(true)
    }

    /// Writes refreshed MCP OAuth access and refresh tokens through existing references.
    fn update_mcp_oauth_credential(
        &self,
        metadata: &mut McpAuthMetadata,
        credential: McpOAuthCredential,
    ) -> Result<()> {
        let access_reference = metadata.credential_store_ref.clone().ok_or_else(|| {
            MezError::invalid_state("MCP auth metadata has no credential reference")
        })?;
        metadata.credential_store_ref = Some(self.store_secret_like_reference(
            &access_reference,
            &mcp_access_credential_name(&metadata.server_id),
            &credential.access_token,
        )?);
        if let Some(refresh_token) = credential.refresh_token {
            let refresh_anchor = metadata
                .refresh_credential_store_ref
                .as_deref()
                .unwrap_or(access_reference.as_str());
            metadata.refresh_credential_store_ref = Some(self.store_secret_like_reference(
                refresh_anchor,
                &mcp_refresh_credential_name(&metadata.server_id),
                &refresh_token,
            )?);
        }
        metadata.token_expires_at = credential
            .token_expires_at
            .or(metadata.token_expires_at.take());
        if !credential.scopes.is_empty() {
            metadata.scopes = credential.scopes;
        }
        if credential.client_id.is_some() {
            metadata.client_id = credential.client_id;
        }
        if credential.resource.is_some() {
            metadata.resource = credential.resource;
        }
        if credential.authorization_endpoint.is_some() {
            metadata.authorization_endpoint = credential.authorization_endpoint;
        }
        if credential.token_endpoint.is_some() {
            metadata.token_endpoint = credential.token_endpoint;
        }
        self.write_mcp_metadata(metadata)?;
        Ok(())
    }

    /// Runs the login with api key operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn login_with_api_key(
        &self,
        provider: &str,
        selected_model_profile: &str,
        api_key: &str,
        credential_store: &dyn CredentialStore,
    ) -> Result<AuthMetadata> {
        validate_safe_name(provider, "provider name is not credential-store safe")?;
        if api_key.trim().is_empty() {
            return Err(MezError::invalid_args("provider API key must not be empty"));
        }
        let api_key = SecretString::from(api_key);
        let reference = credential_store.store_secret(provider, &api_key)?;
        let mut metadata = AuthMetadata::new(provider, selected_model_profile);
        metadata.credential_kind = AuthCredentialKind::ApiKey;
        metadata.credential_store_ref = Some(reference);
        self.write_metadata(&metadata)?;
        Ok(metadata)
    }

    /// Runs the login provider api key operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn login_provider_api_key(
        &self,
        provider: &str,
        selected_model_profile: &str,
        api_key: &str,
        credential_store: &dyn CredentialStore,
    ) -> Result<AuthMetadata> {
        self.login_with_api_key(provider, selected_model_profile, api_key, credential_store)
    }

    /// Stores a provider API key through a selected or preferred credential store.
    pub fn login_provider_api_key_with_selected_store(
        &self,
        provider: &str,
        selected_model_profile: &str,
        api_key: &str,
        credential_store: Option<&str>,
    ) -> Result<AuthMetadata> {
        match Self::selected_credential_store_kind(credential_store)? {
            Some(CredentialStoreKind::PrivateFileFallback) => {
                let credential_store = self.file_credential_store(provider)?;
                self.login_provider_api_key(
                    provider,
                    selected_model_profile,
                    api_key,
                    &credential_store,
                )
            }
            Some(CredentialStoreKind::OperatingSystem) => self
                .login_provider_api_key_with_default_os_store(
                    provider,
                    selected_model_profile,
                    api_key,
                ),
            None => match self.credential_store_plan(provider) {
                CredentialStorePlan::OperatingSystem { .. } => self
                    .login_provider_api_key_with_default_os_store(
                        provider,
                        selected_model_profile,
                        api_key,
                    ),
                CredentialStorePlan::PrivateFileFallback { .. } => {
                    let credential_store = self.file_credential_store(provider)?;
                    self.login_provider_api_key(
                        provider,
                        selected_model_profile,
                        api_key,
                        &credential_store,
                    )
                }
            },
        }
    }

    /// Runs the login provider api key with default os store operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn login_provider_api_key_with_default_os_store(
        &self,
        provider: &str,
        selected_model_profile: &str,
        api_key: &str,
    ) -> Result<AuthMetadata> {
        let native_store = NativeSecretServiceCredentialStore::new();
        if matches!(
            native_store.availability(),
            CredentialStoreAvailability::Available
        ) {
            return self.login_provider_api_key(
                provider,
                selected_model_profile,
                api_key,
                &native_store,
            );
        }

        let command_store = CommandBackedCredentialStore::secret_tool();
        self.login_provider_api_key(provider, selected_model_profile, api_key, &command_store)
    }

    /// Runs the login openai api key operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn login_openai_api_key(
        &self,
        selected_model_profile: &str,
        api_key: &str,
        credential_store: &dyn CredentialStore,
    ) -> Result<AuthMetadata> {
        self.login_with_api_key("openai", selected_model_profile, api_key, credential_store)
    }

    /// Runs the login openai api key with default os store operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn login_openai_api_key_with_default_os_store(
        &self,
        selected_model_profile: &str,
        api_key: &str,
    ) -> Result<AuthMetadata> {
        let native_store = NativeSecretServiceCredentialStore::new();
        if matches!(
            native_store.availability(),
            CredentialStoreAvailability::Available
        ) {
            return self.login_openai_api_key(selected_model_profile, api_key, &native_store);
        }

        let command_store = CommandBackedCredentialStore::secret_tool();
        self.login_openai_api_key(selected_model_profile, api_key, &command_store)
    }

    /// Runs the login openai api key with preferred store operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn login_openai_api_key_with_preferred_store(
        &self,
        selected_model_profile: &str,
        api_key: &str,
    ) -> Result<AuthMetadata> {
        match self.credential_store_plan("openai") {
            CredentialStorePlan::OperatingSystem { .. } => {
                self.login_openai_api_key_with_default_os_store(selected_model_profile, api_key)
            }
            CredentialStorePlan::PrivateFileFallback { .. } => {
                let credential_store = self.file_credential_store("openai")?;
                self.login_openai_api_key(selected_model_profile, api_key, &credential_store)
            }
        }
    }

    /// Runs the login openai provider credential operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn login_openai_provider_credential(
        &self,
        selected_model_profile: &str,
        credential: OpenAiProviderCredential,
        credential_store: &dyn CredentialStore,
    ) -> Result<AuthMetadata> {
        let access_secret = SecretString::from(credential.api_key);
        let mut metadata = AuthMetadata::new(OPENAI_PROVIDER, selected_model_profile);
        metadata.credential_kind = AuthCredentialKind::ChatGpt;
        metadata.credential_store_ref =
            Some(credential_store.store_secret(OPENAI_PROVIDER, &access_secret)?);
        if let Some(refresh_token) = credential.refresh_token {
            let refresh_secret = SecretString::from(refresh_token);
            metadata.refresh_credential_store_ref = Some(
                credential_store.store_secret(OPENAI_REFRESH_CREDENTIAL_NAME, &refresh_secret)?,
            );
        }
        metadata.account_id = credential.account_id;
        metadata.organization_id = credential.organization_id;
        metadata.token_expires_at = credential.token_expires_at;
        self.write_metadata(&metadata)?;
        Ok(metadata)
    }

    /// Stores an OpenAI provider credential through a selected or preferred store.
    pub fn login_openai_provider_credential_with_selected_store(
        &self,
        selected_model_profile: &str,
        credential: OpenAiProviderCredential,
        credential_store: Option<&str>,
    ) -> Result<AuthMetadata> {
        match Self::selected_credential_store_kind(credential_store)? {
            Some(CredentialStoreKind::PrivateFileFallback) => {
                let credential_store = self.file_credential_store(OPENAI_PROVIDER)?;
                self.login_openai_provider_credential(
                    selected_model_profile,
                    credential,
                    &credential_store,
                )
            }
            Some(CredentialStoreKind::OperatingSystem) => self
                .login_openai_provider_credential_with_default_os_store(
                    selected_model_profile,
                    credential,
                ),
            None => self.login_openai_provider_credential_with_preferred_store(
                selected_model_profile,
                credential,
            ),
        }
    }

    /// Runs the login openai provider credential with default os store operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn login_openai_provider_credential_with_default_os_store(
        &self,
        selected_model_profile: &str,
        credential: OpenAiProviderCredential,
    ) -> Result<AuthMetadata> {
        let native_store = NativeSecretServiceCredentialStore::new();
        if matches!(
            native_store.availability(),
            CredentialStoreAvailability::Available
        ) {
            return self.login_openai_provider_credential(
                selected_model_profile,
                credential,
                &native_store,
            );
        }

        let command_store = CommandBackedCredentialStore::secret_tool();
        self.login_openai_provider_credential(selected_model_profile, credential, &command_store)
    }

    /// Runs the login openai provider credential with preferred store operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn login_openai_provider_credential_with_preferred_store(
        &self,
        selected_model_profile: &str,
        credential: OpenAiProviderCredential,
    ) -> Result<AuthMetadata> {
        match self.credential_store_plan(OPENAI_PROVIDER) {
            CredentialStorePlan::OperatingSystem { .. } => self
                .login_openai_provider_credential_with_default_os_store(
                    selected_model_profile,
                    credential,
                ),
            CredentialStorePlan::PrivateFileFallback { .. } => {
                let credential_store = self.file_credential_store(OPENAI_PROVIDER)?;
                self.login_openai_provider_credential(
                    selected_model_profile,
                    credential,
                    &credential_store,
                )
            }
        }
    }

    /// Runs the openai refresh needed soon operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn openai_refresh_needed_soon(&self) -> Result<bool> {
        self.openai_refresh_needed_with_leeway(DEFAULT_PROVIDER_AUTH_REFRESH_LEEWAY_SECONDS)
    }

    /// Checks whether OpenAI provider auth should be refreshed within a custom leeway.
    ///
    /// # Parameters
    /// - `leeway_seconds`: Number of seconds before token expiry that should
    ///   trigger proactive refresh.
    pub fn openai_refresh_needed_with_leeway(&self, leeway_seconds: u64) -> Result<bool> {
        let Some(metadata) = self.read_metadata_for_provider(OPENAI_PROVIDER)? else {
            return Ok(false);
        };
        Ok(openai_refresh_needed_at(
            &metadata,
            current_unix_seconds()?,
            leeway_seconds,
        ))
    }

    /// Runs the refresh openai provider credential if needed async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn refresh_openai_provider_credential_if_needed_async(&self) -> Result<bool> {
        self.refresh_openai_provider_credential_if_needed_with_leeway_async(
            DEFAULT_PROVIDER_AUTH_REFRESH_LEEWAY_SECONDS,
        )
        .await
    }

    /// Refreshes OpenAI provider auth when expiry is within a custom leeway.
    ///
    /// # Parameters
    /// - `leeway_seconds`: Number of seconds before token expiry that should
    ///   trigger proactive refresh.
    pub async fn refresh_openai_provider_credential_if_needed_with_leeway_async(
        &self,
        leeway_seconds: u64,
    ) -> Result<bool> {
        let Some(mut metadata) = self.read_metadata_for_provider(OPENAI_PROVIDER)? else {
            return Ok(false);
        };
        if !openai_refresh_needed_at(&metadata, current_unix_seconds()?, leeway_seconds) {
            return Ok(false);
        }
        let Some(refresh_reference) = metadata.refresh_credential_store_ref.as_deref() else {
            return Ok(false);
        };
        let Some(refresh_token) = self.load_secret(refresh_reference)? else {
            return Ok(false);
        };
        let credential =
            refresh_openai_provider_credential_async(refresh_token.expose_secret()).await?;
        self.update_openai_provider_credential(&mut metadata, credential)?;
        Ok(true)
    }

    /// Runs the update openai provider credential operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn update_openai_provider_credential(
        &self,
        metadata: &mut AuthMetadata,
        credential: OpenAiProviderCredential,
    ) -> Result<()> {
        if metadata.provider != OPENAI_PROVIDER {
            return Err(MezError::invalid_state(format!(
                "auth metadata is for provider `{}`",
                metadata.provider
            )));
        }
        let access_reference = metadata
            .credential_store_ref
            .clone()
            .ok_or_else(|| MezError::invalid_state("auth metadata has no credential reference"))?;
        metadata.credential_store_ref = Some(self.store_secret_like_reference(
            &access_reference,
            OPENAI_PROVIDER,
            &credential.api_key,
        )?);
        metadata.credential_kind = AuthCredentialKind::ChatGpt;
        if let Some(refresh_token) = credential.refresh_token {
            let refresh_anchor = metadata
                .refresh_credential_store_ref
                .as_deref()
                .unwrap_or(access_reference.as_str());
            metadata.refresh_credential_store_ref = Some(self.store_secret_like_reference(
                refresh_anchor,
                OPENAI_REFRESH_CREDENTIAL_NAME,
                &refresh_token,
            )?);
        }
        if credential.account_id.is_some() {
            metadata.account_id = credential.account_id;
        }
        if credential.organization_id.is_some() {
            metadata.organization_id = credential.organization_id;
        }
        metadata.token_expires_at = credential
            .token_expires_at
            .or(metadata.token_expires_at.take());
        self.write_metadata(metadata)?;
        Ok(())
    }

    /// Runs the store secret like reference operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn store_secret_like_reference(
        &self,
        reference: &str,
        credential_name: &str,
        secret: &str,
    ) -> Result<String> {
        let secret = SecretString::from(secret.to_string());
        if reference.starts_with(CredentialStoreKind::OperatingSystem.reference_prefix()) {
            let native_store = NativeSecretServiceCredentialStore::new();
            if native_store.provider_from_reference(reference)?.is_some() {
                return native_store.store_secret(credential_name, &secret);
            }
            return CommandBackedCredentialStore::secret_tool()
                .store_secret(credential_name, &secret);
        }
        if reference.starts_with(CredentialStoreKind::PrivateFileFallback.reference_prefix()) {
            return self
                .file_store_for_reference(reference)?
                .store_secret(credential_name, &secret);
        }
        Err(MezError::config(
            "auth metadata has an unsupported credential reference",
        ))
    }

    /// Runs the read file secret operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn read_file_secret(&self, reference: &str) -> Result<Option<SecretString>> {
        self.file_store_for_reference(reference)?
            .load_secret(reference)
    }

    /// Runs the load secret operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn load_secret(&self, reference: &str) -> Result<Option<SecretString>> {
        if reference.starts_with(CredentialStoreKind::OperatingSystem.reference_prefix()) {
            let native_store = NativeSecretServiceCredentialStore::new();
            if native_store.provider_from_reference(reference)?.is_some() {
                return native_store.load_secret(reference);
            }
            return CommandBackedCredentialStore::secret_tool().load_secret(reference);
        }
        if reference.starts_with(CredentialStoreKind::PrivateFileFallback.reference_prefix()) {
            return self
                .file_store_for_reference(reference)?
                .load_secret(reference);
        }
        Ok(None)
    }

    /// Runs the provider secret operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn provider_secret(&self, provider: &str) -> Result<SecretString> {
        validate_safe_name(provider, "provider name is not credential-store safe")?;
        let metadata = self.read_metadata_for_provider(provider)?.ok_or_else(|| {
            MezError::invalid_state(format!("provider `{provider}` is not authenticated"))
        })?;
        let reference = metadata
            .credential_store_ref
            .as_deref()
            .ok_or_else(|| MezError::invalid_state("auth metadata has no credential reference"))?;
        self.load_secret(reference)?
            .filter(|secret| !secret.expose_secret().trim().is_empty())
            .ok_or_else(|| MezError::invalid_state("provider credential is unavailable"))
    }

    /// Runs the logout operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn logout(&self) -> Result<bool> {
        let metadata_entries = self.read_all_metadata()?;
        let mut changed = false;
        for metadata in metadata_entries.values() {
            if let Some(reference) = metadata.credential_store_ref.as_deref() {
                changed |= self.remove_secret(reference)?;
            }
            if let Some(reference) = metadata.refresh_credential_store_ref.as_deref() {
                changed |= self.remove_secret(reference)?;
            }
        }
        if self.paths.auth_file().exists() {
            fs::remove_file(self.paths.auth_file())?;
            changed = true;
        }
        Ok(changed)
    }

    /// Removes stored MCP OAuth secrets and metadata for one configured server.
    pub fn logout_mcp_server(&self, server_id: &str) -> Result<bool> {
        validate_safe_name(server_id, "MCP server id is not credential-store safe")?;
        let mut metadata_entries = self.read_all_mcp_metadata()?;
        let Some(metadata) = metadata_entries.remove(server_id) else {
            return Ok(false);
        };
        let mut changed = false;
        if let Some(reference) = metadata.credential_store_ref.as_deref() {
            changed |= self.remove_secret(reference)?;
        }
        if let Some(reference) = metadata.refresh_credential_store_ref.as_deref() {
            changed |= self.remove_secret(reference)?;
        }
        if metadata_entries.is_empty() {
            if self.paths.mcp_auth_file().exists() {
                fs::remove_file(self.paths.mcp_auth_file())?;
                changed = true;
            }
        } else {
            ensure_private_dir(self.paths.root())?;
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(self.paths.mcp_auth_file())?;
            file.write_all(encode_all_mcp_metadata(&metadata_entries).as_bytes())?;
            file.sync_all()?;
            set_private_file(self.paths.mcp_auth_file())?;
            changed = true;
        }
        Ok(changed)
    }

    /// Runs the credential state operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn credential_state(&self, metadata: Option<&AuthMetadata>) -> Result<AuthCredentialState> {
        let os_store = CommandBackedCredentialStore::secret_tool();
        self.credential_state_with_os_store(metadata, &os_store)
    }

    /// Computes MCP credential availability without exposing token material.
    fn mcp_credential_state(
        &self,
        metadata: Option<&McpAuthMetadata>,
    ) -> Result<AuthCredentialState> {
        let Some(metadata) = metadata else {
            return Ok(AuthCredentialState::LoggedOut);
        };
        let Some(reference) = metadata.credential_store_ref.as_deref() else {
            return Ok(AuthCredentialState::MissingSecret { reference: None });
        };
        self.credential_state_for_reference(reference)
    }

    /// Computes credential availability for one opaque credential-store reference.
    fn credential_state_for_reference(&self, reference: &str) -> Result<AuthCredentialState> {
        let os_store = CommandBackedCredentialStore::secret_tool();
        self.credential_state_for_reference_with_os_store(reference, &os_store)
    }

    /// Runs the credential state with os store operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn credential_state_with_os_store<R: CredentialCommandRunner>(
        &self,
        metadata: Option<&AuthMetadata>,
        os_store: &CommandBackedCredentialStore<R>,
    ) -> Result<AuthCredentialState> {
        let Some(metadata) = metadata else {
            return Ok(AuthCredentialState::LoggedOut);
        };
        let Some(reference) = metadata.credential_store_ref.as_deref() else {
            return Ok(AuthCredentialState::MissingSecret { reference: None });
        };

        self.credential_state_for_reference_with_os_store(reference, os_store)
    }

    /// Computes credential availability for one reference with an injected OS store.
    pub(super) fn credential_state_for_reference_with_os_store<R: CredentialCommandRunner>(
        &self,
        reference: &str,
        os_store: &CommandBackedCredentialStore<R>,
    ) -> Result<AuthCredentialState> {
        if reference.starts_with(CredentialStoreKind::OperatingSystem.reference_prefix()) {
            let native_store = NativeSecretServiceCredentialStore::new();
            let contains = if native_store.provider_from_reference(reference)?.is_some() {
                native_store.contains_secret(reference)
            } else {
                os_store.contains_secret(reference)
            };
            return match contains {
                Ok(true) => Ok(AuthCredentialState::Available {
                    store: CredentialStoreKind::OperatingSystem,
                    reference: reference.to_string(),
                }),
                Ok(false) => Ok(AuthCredentialState::MissingSecret {
                    reference: Some(reference.to_string()),
                }),
                Err(error) if error.kind() == crate::error::MezErrorKind::InvalidState => {
                    Ok(AuthCredentialState::MissingSecret {
                        reference: Some(reference.to_string()),
                    })
                }
                Err(error) => Err(error),
            };
        }

        if reference.starts_with(CredentialStoreKind::PrivateFileFallback.reference_prefix()) {
            return if self
                .file_store_for_reference(reference)?
                .contains_secret(reference)?
            {
                Ok(AuthCredentialState::Available {
                    store: CredentialStoreKind::PrivateFileFallback,
                    reference: reference.to_string(),
                })
            } else {
                Ok(AuthCredentialState::MissingSecret {
                    reference: Some(reference.to_string()),
                })
            };
        }

        Ok(AuthCredentialState::MissingSecret {
            reference: Some(reference.to_string()),
        })
    }

    /// Runs the remove secret operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn remove_secret(&self, reference: &str) -> Result<bool> {
        let os_store = CommandBackedCredentialStore::secret_tool();
        self.remove_secret_with_os_store(reference, &os_store)
    }

    /// Runs the remove secret with os store operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn remove_secret_with_os_store<R: CredentialCommandRunner>(
        &self,
        reference: &str,
        os_store: &CommandBackedCredentialStore<R>,
    ) -> Result<bool> {
        if reference.starts_with(CredentialStoreKind::OperatingSystem.reference_prefix()) {
            let native_store = NativeSecretServiceCredentialStore::new();
            if native_store.provider_from_reference(reference)?.is_some() {
                return native_store.delete_secret(reference);
            }
            return os_store.delete_secret(reference);
        }
        if reference.starts_with(CredentialStoreKind::PrivateFileFallback.reference_prefix()) {
            self.file_store_for_reference(reference)?
                .delete_secret(reference)
        } else {
            Ok(false)
        }
    }

    /// Runs the file store for reference operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn file_store_for_reference(&self, reference: &str) -> Result<PrivateFileCredentialStore> {
        let Some(path) =
            reference.strip_prefix(CredentialStoreKind::PrivateFileFallback.reference_prefix())
        else {
            return Ok(PrivateFileCredentialStore::new(
                self.paths.secret_directory().to_path_buf(),
            ));
        };
        let path = PathBuf::from(path);
        if !path_is_under_directory(&path, self.paths.secret_directory()) {
            return Err(MezError::forbidden(
                "auth secret reference points outside the auth secret directory",
            ));
        }
        let Some(directory) = path.parent() else {
            return Err(MezError::config(
                "auth secret reference has no parent directory",
            ));
        };
        Ok(PrivateFileCredentialStore::new(directory.to_path_buf()))
    }
}

/// Runs the openai refresh needed at operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn openai_refresh_needed_at(
    metadata: &AuthMetadata,
    now_unix_seconds: u64,
    leeway_seconds: u64,
) -> bool {
    if metadata.provider != OPENAI_PROVIDER || metadata.refresh_credential_store_ref.is_none() {
        return false;
    }
    let Some(expires_at) = metadata
        .token_expires_at
        .as_deref()
        .and_then(|value| value.parse::<u64>().ok())
    else {
        return false;
    };
    expires_at <= now_unix_seconds.saturating_add(leeway_seconds)
}

/// Runs the current unix seconds operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn current_unix_seconds() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| MezError::invalid_state("system clock is before the Unix epoch"))?
        .as_secs())
}

/// Returns the credential-store entry name for an MCP access token.
fn mcp_access_credential_name(server_id: &str) -> String {
    format!("mcp-{server_id}-access")
}

/// Returns the credential-store entry name for an MCP refresh token.
fn mcp_refresh_credential_name(server_id: &str) -> String {
    format!("mcp-{server_id}-refresh")
}
