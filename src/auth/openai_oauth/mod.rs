//! OpenAI ChatGPT OAuth login flows.
//!
//! This module owns the browser and device-code provider sign-in mechanics used
//! by `mez auth login`. It deliberately returns a provider-issued bearer
//! credential to the existing `AuthStore` boundary instead of writing secrets
//! directly. The local metadata file remains non-secret, and credential
//! persistence stays centralized in the configured credential store.

use std::collections::BTreeMap;
use std::io::Write;
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use rand::Rng;
use serde::Deserialize;
use serde_json::Value;
use sha2::Digest;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::error::{MezError, MezErrorKind, Result};
use mez_mux::theme::UiTheme;
use mez_terminal::TerminalColor;

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

/// Compact web token set used by the browser callback page.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LoginPageThemeTokens {
    /// Page background color.
    bg: String,
    /// Card surface color.
    surface: String,
    /// Raised card detail color.
    surface_elevated: String,
    /// Card and badge border color.
    border: String,
    /// Primary readable text color.
    text_primary: String,
    /// Secondary readable text color.
    text_secondary: String,
    /// Primary accent color derived from the active Mezzanine theme.
    accent_primary: String,
    /// Secondary accent color derived from the active Mezzanine theme.
    accent_secondary: String,
    /// Success state color derived from the active Mezzanine theme.
    success: String,
    /// CSS alpha value that controls glow strength.
    glow_strength: &'static str,
    /// Whether the active token set is dark.
    is_dark: bool,
}

/// RGB color used while translating terminal theme colors to CSS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LoginPageRgb {
    /// Red channel.
    red: u8,
    /// Green channel.
    green: u8,
    /// Blue channel.
    blue: u8,
}

impl LoginPageRgb {
    /// Builds an RGB color from explicit channel values.
    fn new(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }
}

/// Browser callback page state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoginPageKind {
    /// Successful provider callback.
    Success,
    /// Failed provider callback.
    Error,
}

impl LoginPageKind {
    /// Selects a page state from the HTTP status code.
    fn from_status(status: u16) -> Self {
        if status == 200 {
            Self::Success
        } else {
            Self::Error
        }
    }

    /// Returns the short badge label for this page state.
    fn badge(self) -> &'static str {
        match self {
            Self::Success => "OK",
            Self::Error => "ERR",
        }
    }

    /// Returns the page headline for this page state.
    fn headline(self) -> &'static str {
        match self {
            Self::Success => "Login successful",
            Self::Error => "Sign-in failed",
        }
    }

    /// Returns the follow-up instruction for this page state.
    fn hint(self) -> &'static str {
        match self {
            Self::Success => "You can close this tab and return to Mezzanine.",
            Self::Error => "Return to Mezzanine and try the sign-in flow again.",
        }
    }
}

/// Runs the default browser-based ChatGPT sign-in flow.
mod browser_flow;
mod callback_server;
mod claims;
mod http;
mod login_page;
mod pkce;
mod platform_browser;

use claims::{deserialize_device_interval, deserialize_optional_u64};

pub use browser_flow::{
    refresh_openai_provider_credential_async, run_openai_browser_login_async,
    run_openai_browser_login_with_theme_async, run_openai_device_code_login_async,
};

#[cfg(test)]
mod tests;
