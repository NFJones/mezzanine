//! OpenAI ChatGPT OAuth login flows.
//!
//! This module owns the browser and device-code provider sign-in mechanics used
//! by `mez auth login`. It deliberately returns a provider-issued bearer
//! credential to the existing `AuthStore` boundary instead of writing secrets
//! directly. The local metadata file remains non-secret, and credential
//! persistence stays centralized in the configured credential store.

use std::time::Duration;

use serde::Deserialize;

use crate::error::{MezError, MezErrorKind, Result};

/// Defines the DEFAULT ISSUER const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const DEFAULT_ISSUER: &str = "https://auth.openai.com";
/// Defines the DEFAULT CLIENT ID const used by this subsystem.
///
/// This is an intentionally public native-app OAuth client identifier for the
/// ChatGPT browser/device-code login flows. It is sent as request metadata and
/// is not a client secret; no paired client secret is stored in this repository.
const DEFAULT_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Defines the DEFAULT BROWSER PORT const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const DEFAULT_BROWSER_PORT: u16 = 1455;
/// Defines the FALLBACK BROWSER PORT const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const FALLBACK_BROWSER_PORT: u16 = 1457;
/// Defines the LOGIN TIMEOUT const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Defines the HTTP REQUEST TIMEOUT const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Defines the HTTP CLIENT TIMEOUT const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const HTTP_CLIENT_TIMEOUT: Duration = Duration::from_secs(30);
/// Defines the DEVICE VERIFICATION PATH const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const DEVICE_VERIFICATION_PATH: &str = "/codex/device";
/// OAuth scopes requested by browser/device ChatGPT sign-in.
///
/// These are OAuth client scopes, not restricted API-key endpoint permission
/// labels. In particular, `api.model.read` is an API-key permission surface and
/// is not currently accepted by this ChatGPT OAuth client.
const OPENAI_OAUTH_SCOPE: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";

/// Provider-issued bearer credential returned by OpenAI browser/device login.
#[derive(Clone, PartialEq, Eq)]
pub struct OpenAiProviderCredential {
    /// Provider bearer credential returned by ChatGPT OAuth.
    pub api_key: String,
    /// Optional provider refresh token returned by ChatGPT OAuth.
    pub refresh_token: Option<String>,
    /// Optional ChatGPT/OpenAI account identifier parsed from the ID token.
    pub account_id: Option<String>,
    /// Optional OpenAI organization identifier parsed from provider JWT claims.
    pub organization_id: Option<String>,
    /// Optional token expiry as a Unix timestamp string parsed from the ID token.
    pub token_expires_at: Option<String>,
}

impl std::fmt::Debug for OpenAiProviderCredential {
    /// Formats provider credentials without exposing bearer or refresh tokens.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenAiProviderCredential")
            .field("api_key", &"[REDACTED]")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("account_id", &self.account_id)
            .field("organization_id", &self.organization_id)
            .field("token_expires_at", &self.token_expires_at)
            .finish()
    }
}

/// Carries Pkce Codes state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone)]
struct PkceCodes {
    /// Stores the code verifier value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    code_verifier: String,
    /// Stores the code challenge value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    code_challenge: String,
}

/// Carries Token Response state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Deserialize)]
struct TokenResponse {
    /// Stores the access token value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    access_token: String,
    /// Stores the id token value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    #[serde(default)]
    id_token: Option<String>,
    /// Stores the refresh token value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    #[serde(default)]
    refresh_token: Option<String>,
    /// Stores the expires in value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    #[serde(default, deserialize_with = "deserialize_optional_u64")]
    expires_in: Option<u64>,
}

impl std::fmt::Debug for TokenResponse {
    /// Formats OAuth token responses without exposing raw token material.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TokenResponse")
            .field("access_token", &"[REDACTED]")
            .field("id_token", &self.id_token.as_ref().map(|_| "[REDACTED]"))
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

/// Carries Device Code Response state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    /// Stores the device auth id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    device_auth_id: String,
    /// Stores the user code value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    #[serde(alias = "usercode")]
    user_code: String,
    /// Stores the interval value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    #[serde(default, deserialize_with = "deserialize_device_interval")]
    interval: u64,
}

/// Carries Device Authorization Response state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Deserialize)]
struct DeviceAuthorizationResponse {
    /// Stores the authorization code value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    authorization_code: String,
    /// Stores the code challenge value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    code_challenge: String,
    /// Stores the code verifier value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    code_verifier: String,
}

/// Runs the default browser-based ChatGPT sign-in flow.
mod browser_flow;
mod callback_server;
mod claims;
mod http;
mod pkce;
mod platform_browser;

use super::callback_page::LoginPageThemeTokens;
use claims::{deserialize_device_interval, deserialize_optional_u64};

pub use browser_flow::{
    refresh_openai_provider_credential_async, run_openai_browser_login_with_theme_async,
    run_openai_device_code_login_async,
};

#[cfg(test)]
mod tests;
