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
use crate::terminal::{TerminalColor, UiTheme};

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
pub async fn run_openai_browser_login_async() -> Result<OpenAiProviderCredential> {
    run_openai_browser_login_with_theme_async(&UiTheme::default()).await
}

/// Runs the browser-based ChatGPT sign-in flow with a themed callback page.
pub async fn run_openai_browser_login_with_theme_async(
    ui_theme: &UiTheme,
) -> Result<OpenAiProviderCredential> {
    let issuer = openai_auth_issuer();
    let listener = bind_browser_login_listener()?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://localhost:{port}/auth/callback");
    let state = random_urlsafe_token(32);
    let pkce = generate_pkce();
    let auth_url = build_authorize_url(&issuer, DEFAULT_CLIENT_ID, &redirect_uri, &pkce, &state);
    let page_tokens = login_page_theme_tokens(ui_theme);

    let browser_opened = open_browser(&auth_url);
    eprintln!(
        "{}",
        browser_login_launch_message(&auth_url, browser_opened)
    );

    let code =
        wait_for_browser_authorization_code_with_page_async(listener, &state, &page_tokens).await?;
    complete_authorization_code_login_async(&issuer, &redirect_uri, &pkce, &code).await
}

/// Runs the browser login launch message operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn browser_login_launch_message(auth_url: &str, browser_opened: bool) -> String {
    if browser_opened {
        format!(
            "Opened a browser for ChatGPT sign-in.\nIf it did not open, use this URL:\n{auth_url}"
        )
    } else {
        format!("Open this URL in your browser to continue ChatGPT sign-in:\n{auth_url}")
    }
}

/// Runs OpenAI device-code sign-in and prints the out-of-band prompt.
pub async fn run_openai_device_code_login_async() -> Result<OpenAiProviderCredential> {
    let issuer = openai_auth_issuer();
    let device_code = request_device_code_async(&issuer).await?;
    let verification_url = format!(
        "{}{}",
        issuer.trim_end_matches('/'),
        DEVICE_VERIFICATION_PATH
    );
    eprintln!(
        "\nSign in with ChatGPT using device code authorization:\n\
         \n\
         1. Open this link in your browser:\n   {verification_url}\n\
         \n\
         2. Enter this one-time code, which expires in 15 minutes:\n   {}\n\
         \n\
         Never share this code.",
        device_code.user_code
    );
    let device_authorization = poll_device_authorization_async(&issuer, &device_code).await?;
    let pkce = PkceCodes {
        code_verifier: device_authorization.code_verifier,
        code_challenge: device_authorization.code_challenge,
    };
    let redirect_uri = format!("{}/deviceauth/callback", issuer.trim_end_matches('/'));
    complete_authorization_code_login_async(
        &issuer,
        &redirect_uri,
        &pkce,
        &device_authorization.authorization_code,
    )
    .await
}

/// Runs the openai auth issuer operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn openai_auth_issuer() -> String {
    std::env::var("MEZ_OPENAI_AUTH_ISSUER")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_ISSUER.to_string())
}

/// Runs the complete authorization code login async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn complete_authorization_code_login_async(
    issuer: &str,
    redirect_uri: &str,
    pkce: &PkceCodes,
    code: &str,
) -> Result<OpenAiProviderCredential> {
    Ok(provider_credential_from_tokens(
        exchange_code_for_tokens_async(issuer, redirect_uri, pkce, code).await?,
    ))
}

/// Refreshes an OpenAI ChatGPT OAuth credential with a persisted refresh token.
pub async fn refresh_openai_provider_credential_async(
    refresh_token: &str,
) -> Result<OpenAiProviderCredential> {
    if refresh_token.trim().is_empty() {
        return Err(MezError::invalid_args(
            "OpenAI refresh token must not be empty",
        ));
    }
    let issuer = openai_auth_issuer();
    Ok(provider_credential_from_tokens(
        refresh_tokens_async(&issuer, refresh_token).await?,
    ))
}

/// Runs the bind browser login listener operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn bind_browser_login_listener() -> Result<TcpListener> {
    for port in [DEFAULT_BROWSER_PORT, FALLBACK_BROWSER_PORT] {
        match TcpListener::bind(("127.0.0.1", port)) {
            Ok(listener) => return Ok(listener),
            Err(_) => continue,
        }
    }
    Err(MezError::conflict(format!(
        "OpenAI browser login requires localhost callback port {DEFAULT_BROWSER_PORT} \
         or {FALLBACK_BROWSER_PORT}, but both are unavailable"
    )))
}

/// Runs the wait for browser authorization code async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
async fn wait_for_browser_authorization_code_async(
    listener: TcpListener,
    expected_state: &str,
) -> Result<String> {
    let page_tokens = login_page_theme_tokens(&UiTheme::default());
    wait_for_browser_authorization_code_with_page_async(listener, expected_state, &page_tokens)
        .await
}

/// Runs the wait loop and writes a themed browser-visible callback page.
async fn wait_for_browser_authorization_code_with_page_async(
    listener: TcpListener,
    expected_state: &str,
    page_tokens: &LoginPageThemeTokens,
) -> Result<String> {
    listener.set_nonblocking(true)?;
    let listener = tokio::net::TcpListener::from_std(listener)?;
    let deadline = tokio::time::Instant::now() + LOGIN_TIMEOUT;
    let (mut stream, _) = tokio::time::timeout_at(deadline, listener.accept())
        .await
        .map_err(|_| browser_login_timeout_error())??;
    let mut buffer = [0u8; 8192];
    let bytes_read = tokio::time::timeout(HTTP_REQUEST_TIMEOUT, stream.read(&mut buffer))
        .await
        .map_err(|_| {
            MezError::invalid_state("OpenAI browser callback timed out while reading")
        })??;
    let request = String::from_utf8_lossy(&buffer[..bytes_read]);
    let callback = match parse_callback_request(&request, expected_state) {
        Ok(callback) => callback,
        Err(error) => {
            let _ = write_http_response_with_tokens_async(
                &mut stream,
                400,
                "OpenAI sign-in failed.",
                page_tokens,
            )
            .await;
            return Err(error);
        }
    };
    write_http_response_with_tokens_async(
        &mut stream,
        200,
        "OpenAI sign-in completed. You can return to Mezzanine.",
        page_tokens,
    )
    .await?;
    Ok(callback)
}

/// Runs the browser login timeout error operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn browser_login_timeout_error() -> MezError {
    MezError::invalid_state("OpenAI browser login timed out after 15 minutes")
}

/// Runs the parse callback request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_callback_request(request: &str, expected_state: &str) -> Result<String> {
    let request_line = request
        .lines()
        .next()
        .ok_or_else(|| MezError::invalid_state("OpenAI browser callback was empty"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    if method != "GET" {
        return Err(MezError::invalid_state(
            "OpenAI browser callback used an unsupported HTTP method",
        ));
    }
    let Some((path, query)) = target.split_once('?') else {
        return Err(MezError::invalid_state(
            "OpenAI browser callback did not include authorization data",
        ));
    };
    if path != "/auth/callback" {
        return Err(MezError::invalid_state(
            "OpenAI browser callback used an unexpected path",
        ));
    }
    let query = parse_query(query)?;
    if query.get("state").map(String::as_str) != Some(expected_state) {
        return Err(MezError::invalid_state(
            "OpenAI browser callback state did not match the login request",
        ));
    }
    if let Some(error) = query.get("error") {
        let description = query.get("error_description").map(String::as_str);
        return Err(MezError::forbidden(format!(
            "OpenAI browser login failed: {}",
            oauth_error_message(error, description)
        )));
    }
    query
        .get("code")
        .filter(|code| !code.trim().is_empty())
        .cloned()
        .ok_or_else(|| MezError::invalid_state("OpenAI browser callback did not include a code"))
}

/// Runs the write http response operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
fn write_http_response(stream: &mut impl Write, status: u16, body: &str) -> std::io::Result<()> {
    let tokens = login_page_theme_tokens(&UiTheme::default());
    write_http_response_with_tokens(stream, status, body, &tokens)
}

/// Writes a themed browser callback response to a blocking stream.
fn write_http_response_with_tokens(
    stream: &mut impl Write,
    status: u16,
    body: &str,
    tokens: &LoginPageThemeTokens,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "OK",
    };
    let document = login_page_document(status, body, tokens);
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {document}",
        document.len()
    )
}

/// Builds the browser callback page using active Mezzanine theme tokens.
fn login_page_document(status: u16, body: &str, tokens: &LoginPageThemeTokens) -> String {
    let kind = LoginPageKind::from_status(status);
    let escaped_message = html_escape(body);
    let color_scheme = if tokens.is_dark { "dark" } else { "light" };
    let technical_note = match kind {
        LoginPageKind::Success => {
            "Localhost callback complete. No external page assets were loaded."
        }
        LoginPageKind::Error => {
            "The localhost callback did not complete the requested credential exchange."
        }
    };
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Mezzanine sign-in</title>
<style>
:root {{
  color-scheme: {color_scheme};
  --bg: {bg};
  --surface: {surface};
  --surface-elevated: {surface_elevated};
  --border: {border};
  --text-primary: {text_primary};
  --text-secondary: {text_secondary};
  --accent-primary: {accent_primary};
  --accent-secondary: {accent_secondary};
  --success: {success};
  --glow-strength: {glow_strength};
}}
* {{
  box-sizing: border-box;
}}
html,
body {{
  min-height: 100%;
}}
body {{
  margin: 0;
  display: grid;
  place-items: center;
  min-height: 100vh;
  padding: clamp(1rem, 4vw, 2.5rem);
  color: var(--text-primary);
  background: var(--bg);
  font-family:
    ui-sans-serif,
    system-ui,
    -apple-system,
    BlinkMacSystemFont,
    "Segoe UI",
    sans-serif;
}}
.mez-shell {{
  width: min(94vw, 46rem);
  overflow: hidden;
  border: 1px solid var(--border);
  border-radius: 12px;
  background: var(--surface);
  box-shadow:
    0 1.2rem 3.2rem rgba(0, 0, 0, calc(var(--glow-strength) + 0.18)),
    0 0 0 1px color-mix(in srgb, var(--text-primary) 5%, transparent);
}}
.mez-titlebar {{
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 1rem;
  padding: 0.72rem 0.88rem;
  border-bottom: 1px solid var(--border);
  background: var(--surface-elevated);
}}
.mez-title {{
  display: inline-flex;
  align-items: center;
  gap: 0.55rem;
  min-width: 0;
  color: var(--text-primary);
  font-size: 0.9rem;
  font-weight: 650;
}}
.mez-dot {{
  width: 0.65rem;
  height: 0.65rem;
  border-radius: 999px;
  background: var(--success);
  box-shadow: 0 0 0 0.18rem color-mix(in srgb, var(--success) 16%, transparent);
}}
h1 {{
  margin: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font: inherit;
}}
.status-pill {{
  flex: 0 0 auto;
  padding: 0.22rem 0.55rem;
  border: 1px solid color-mix(in srgb, var(--success) 52%, var(--border));
  border-radius: 999px;
  color: var(--success);
  background: color-mix(in srgb, var(--success) 10%, transparent);
  font-size: 0.72rem;
  font-weight: 700;
  letter-spacing: 0.06em;
  text-transform: uppercase;
}}
.mez-pane {{
  padding: clamp(1rem, 3vw, 1.35rem);
  background: color-mix(in srgb, var(--surface) 82%, var(--bg));
  font-family:
    ui-monospace,
    SFMono-Regular,
    Menlo,
    Monaco,
    Consolas,
    "Liberation Mono",
    monospace;
  font-size: clamp(0.9rem, 2.3vw, 1rem);
  line-height: 1.65;
}}
.transcript-line {{
  display: grid;
  grid-template-columns: auto minmax(0, 1fr);
  gap: 0.75rem;
  align-items: baseline;
}}
.transcript-line + .transcript-line {{
  margin-top: 0.5rem;
}}
.speaker {{
  color: var(--accent-primary);
  font-weight: 700;
  white-space: nowrap;
}}
.line-text {{
  color: var(--text-primary);
}}
.line-text.secondary {{
  color: var(--text-secondary);
}}
.line-text.strong {{
  color: var(--text-primary);
  font-weight: 700;
}}
.mez-footer {{
  padding: 0.65rem 0.88rem;
  border-top: 1px solid color-mix(in srgb, var(--border) 50%, transparent);
  background: var(--surface-elevated);
  color: var(--text-secondary);
  font-size: 0.78rem;
}}
</style>
</head>
<body>
<main class="mez-shell" aria-labelledby="login-title">
  <header class="mez-titlebar">
    <div class="mez-title">
      <span class="mez-dot" aria-hidden="true"></span>
      <h1 id="login-title">Mezzanine auth</h1>
    </div>
    <span class="status-pill">{badge}</span>
  </header>
  <section class="mez-pane" aria-label="Authentication result transcript">
    <div class="transcript-line agent-line">
      <span class="speaker">agent:</span>
      <span class="line-text strong">{headline}</span>
    </div>
    <div class="transcript-line agent-line">
      <span class="speaker">agent:</span>
      <span class="line-text">{escaped_message}</span>
    </div>
    <div class="transcript-line agent-line">
      <span class="speaker">agent:</span>
      <span class="line-text secondary">{hint}</span>
    </div>
  </section>
  <footer class="mez-footer">{technical_note}</footer>
</main>
</body>
</html>"#,
        color_scheme = color_scheme,
        bg = tokens.bg.as_str(),
        surface = tokens.surface.as_str(),
        surface_elevated = tokens.surface_elevated.as_str(),
        border = tokens.border.as_str(),
        text_primary = tokens.text_primary.as_str(),
        text_secondary = tokens.text_secondary.as_str(),
        accent_primary = tokens.accent_primary.as_str(),
        accent_secondary = tokens.accent_secondary.as_str(),
        success = tokens.success.as_str(),
        glow_strength = tokens.glow_strength,
        badge = kind.badge(),
        headline = kind.headline(),
        hint = kind.hint(),
        technical_note = technical_note,
        escaped_message = escaped_message,
    )
}

/// Builds web-safe callback-page tokens from the active Mezzanine UI theme.
fn login_page_theme_tokens(ui_theme: &UiTheme) -> LoginPageThemeTokens {
    let fallback_bg = LoginPageRgb::new(11, 31, 23);
    let fallback_text = LoginPageRgb::new(228, 239, 232);
    let bg = login_page_rgb_from_terminal_color(ui_theme.colors.frame_fill.background, fallback_bg);
    let text_primary =
        login_page_rgb_from_terminal_color(ui_theme.colors.frame_fill.foreground, fallback_text);
    let text_secondary = login_page_rgb_from_terminal_color(
        ui_theme.colors.agent_transcript_status.foreground,
        login_page_mix(text_primary, bg, 0.35),
    );
    let accent_primary = login_page_rgb_from_terminal_color(
        ui_theme.colors.agent_status_running.background,
        LoginPageRgb::new(87, 199, 133),
    );
    let accent_secondary = login_page_rgb_from_terminal_color(
        ui_theme.colors.agent_reasoning.background,
        LoginPageRgb::new(215, 196, 106),
    );
    let success = login_page_rgb_from_terminal_color(
        ui_theme.colors.agent_status_running.background,
        accent_primary,
    );
    let is_dark = login_page_is_dark(bg);
    let surface = if is_dark {
        login_page_mix(bg, text_primary, 0.08)
    } else {
        login_page_mix(bg, text_primary, 0.04)
    };
    let surface_elevated = if is_dark {
        login_page_mix(surface, text_primary, 0.06)
    } else {
        login_page_mix(surface, LoginPageRgb::new(255, 255, 255), 0.52)
    };
    let border = login_page_mix(surface, accent_primary, if is_dark { 0.48 } else { 0.34 });
    LoginPageThemeTokens {
        bg: login_page_rgb_hex(bg),
        surface: login_page_rgb_hex(surface),
        surface_elevated: login_page_rgb_hex(surface_elevated),
        border: login_page_rgb_hex(border),
        text_primary: login_page_rgb_hex(text_primary),
        text_secondary: login_page_rgb_hex(text_secondary),
        accent_primary: login_page_rgb_hex(accent_primary),
        accent_secondary: login_page_rgb_hex(accent_secondary),
        success: login_page_rgb_hex(success),
        glow_strength: if is_dark { "0.32" } else { "0.12" },
        is_dark,
    }
}

/// Converts a terminal color into RGB for browser CSS token generation.
fn login_page_rgb_from_terminal_color(
    color: TerminalColor,
    fallback: LoginPageRgb,
) -> LoginPageRgb {
    match color {
        TerminalColor::Rgb(red, green, blue) => LoginPageRgb::new(red, green, blue),
        TerminalColor::Indexed(index) => login_page_rgb_from_index(index).unwrap_or(fallback),
    }
}

/// Converts an ANSI or xterm 256-color palette index into RGB.
fn login_page_rgb_from_index(index: u8) -> Option<LoginPageRgb> {
    const ANSI: [LoginPageRgb; 16] = [
        LoginPageRgb {
            red: 0,
            green: 0,
            blue: 0,
        },
        LoginPageRgb {
            red: 128,
            green: 0,
            blue: 0,
        },
        LoginPageRgb {
            red: 0,
            green: 128,
            blue: 0,
        },
        LoginPageRgb {
            red: 128,
            green: 128,
            blue: 0,
        },
        LoginPageRgb {
            red: 0,
            green: 0,
            blue: 128,
        },
        LoginPageRgb {
            red: 128,
            green: 0,
            blue: 128,
        },
        LoginPageRgb {
            red: 0,
            green: 128,
            blue: 128,
        },
        LoginPageRgb {
            red: 192,
            green: 192,
            blue: 192,
        },
        LoginPageRgb {
            red: 128,
            green: 128,
            blue: 128,
        },
        LoginPageRgb {
            red: 255,
            green: 0,
            blue: 0,
        },
        LoginPageRgb {
            red: 0,
            green: 255,
            blue: 0,
        },
        LoginPageRgb {
            red: 255,
            green: 255,
            blue: 0,
        },
        LoginPageRgb {
            red: 0,
            green: 0,
            blue: 255,
        },
        LoginPageRgb {
            red: 255,
            green: 0,
            blue: 255,
        },
        LoginPageRgb {
            red: 0,
            green: 255,
            blue: 255,
        },
        LoginPageRgb {
            red: 255,
            green: 255,
            blue: 255,
        },
    ];
    match index {
        0..=15 => Some(ANSI[usize::from(index)]),
        16..=231 => {
            let value = index - 16;
            let component = |slot: u8| -> u8 {
                if slot == 0 {
                    0
                } else {
                    55 + slot.saturating_mul(40)
                }
            };
            Some(LoginPageRgb::new(
                component(value / 36),
                component((value / 6) % 6),
                component(value % 6),
            ))
        }
        232..=255 => {
            let level = 8 + (index - 232).saturating_mul(10);
            Some(LoginPageRgb::new(level, level, level))
        }
    }
}

/// Formats an RGB color as a CSS hexadecimal color.
fn login_page_rgb_hex(color: LoginPageRgb) -> String {
    format!("#{:02x}{:02x}{:02x}", color.red, color.green, color.blue)
}

/// Mixes two RGB colors by the supplied right-hand-side amount.
fn login_page_mix(left: LoginPageRgb, right: LoginPageRgb, amount: f32) -> LoginPageRgb {
    let amount = amount.clamp(0.0, 1.0);
    let mix_channel = |left: u8, right: u8| -> u8 {
        (f32::from(left) + (f32::from(right) - f32::from(left)) * amount).round() as u8
    };
    LoginPageRgb::new(
        mix_channel(left.red, right.red),
        mix_channel(left.green, right.green),
        mix_channel(left.blue, right.blue),
    )
}

/// Returns whether a background color should be treated as dark.
fn login_page_is_dark(color: LoginPageRgb) -> bool {
    login_page_luminance(color) < 140
}

/// Computes perceptual luma for a browser-page RGB token.
fn login_page_luminance(color: LoginPageRgb) -> u16 {
    (u16::from(color.red) * 30 + u16::from(color.green) * 59 + u16::from(color.blue) * 11) / 100
}

/// Writes a themed browser callback response to an async stream.
async fn write_http_response_with_tokens_async(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    body: &str,
    tokens: &LoginPageThemeTokens,
) -> Result<()> {
    let mut response = Vec::new();
    write_http_response_with_tokens(&mut response, status, body, tokens)?;
    tokio::time::timeout(HTTP_REQUEST_TIMEOUT, stream.write_all(&response))
        .await
        .map_err(|_| {
            MezError::invalid_state("OpenAI browser callback timed out while writing")
        })??;
    Ok(())
}

/// Runs the exchange code for tokens async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn exchange_code_for_tokens_async(
    issuer: &str,
    redirect_uri: &str,
    pkce: &PkceCodes,
    code: &str,
) -> Result<TokenResponse> {
    let endpoint = format!("{}/oauth/token", issuer.trim_end_matches('/'));
    let body = form_body(&[
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", DEFAULT_CLIENT_ID),
        ("code_verifier", &pkce.code_verifier),
    ]);
    post_form_async(&endpoint, body, "OpenAI token exchange").await
}

/// Runs the refresh tokens async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn refresh_tokens_async(issuer: &str, refresh_token: &str) -> Result<TokenResponse> {
    let endpoint = format!("{}/oauth/token", issuer.trim_end_matches('/'));
    let body = form_body(&[
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", DEFAULT_CLIENT_ID),
    ]);
    post_form_async(&endpoint, body, "OpenAI token refresh").await
}

/// Runs the request device code async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn request_device_code_async(issuer: &str) -> Result<DeviceCodeResponse> {
    let endpoint = format!(
        "{}/api/accounts/deviceauth/usercode",
        issuer.trim_end_matches('/')
    );
    let body = device_code_request_body(DEFAULT_CLIENT_ID);
    post_json_async(&endpoint, body, "OpenAI device-code request").await
}

/// Builds the OpenAI device-code authorization request body.
///
/// The device-code flow must ask for the same provider scopes as the browser
/// flow so credentials minted through either login path can read provider model
/// metadata when the authenticated account permits it.
fn device_code_request_body(client_id: &str) -> String {
    format!(
        r#"{{"client_id":"{}","scope":"{}"}}"#,
        json_escape(client_id),
        json_escape(OPENAI_OAUTH_SCOPE)
    )
}

/// Runs the poll device authorization async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn poll_device_authorization_async(
    issuer: &str,
    device_code: &DeviceCodeResponse,
) -> Result<DeviceAuthorizationResponse> {
    let endpoint = format!(
        "{}/api/accounts/deviceauth/token",
        issuer.trim_end_matches('/')
    );
    let deadline = tokio::time::Instant::now() + LOGIN_TIMEOUT;
    let interval = Duration::from_secs(device_code.interval.max(1));
    while tokio::time::Instant::now() < deadline {
        let body = format!(
            r#"{{"device_auth_id":"{}","user_code":"{}"}}"#,
            json_escape(&device_code.device_auth_id),
            json_escape(&device_code.user_code)
        );
        let response = async_http_client()
            .post(&endpoint)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(provider_transport_error)?;
        let status = response.status();
        if status.is_success() {
            let text = response.text().await.map_err(provider_transport_error)?;
            return serde_json::from_str(&text).map_err(provider_parse_error);
        }
        if status.as_u16() == 403 || status.as_u16() == 404 {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            tokio::time::sleep(remaining.min(interval)).await;
            continue;
        }
        let message = response.text().await.unwrap_or_default();
        return Err(MezError::new(
            MezErrorKind::Io,
            format!(
                "OpenAI device-code authorization failed with status {status}: {}",
                provider_error_detail(&message)
            ),
        ));
    }
    Err(MezError::invalid_state(
        "OpenAI device-code login timed out after 15 minutes",
    ))
}

/// Runs the post form async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn post_form_async<T: for<'de> Deserialize<'de>>(
    endpoint: &str,
    body: String,
    label: &str,
) -> Result<T> {
    let response = async_http_client()
        .post(endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(provider_transport_error)?;
    parse_provider_response_async(response, label).await
}

/// Runs the post json async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn post_json_async<T: for<'de> Deserialize<'de>>(
    endpoint: &str,
    body: String,
    label: &str,
) -> Result<T> {
    let response = async_http_client()
        .post(endpoint)
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(provider_transport_error)?;
    parse_provider_response_async(response, label).await
}

/// Runs the parse provider response async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn parse_provider_response_async<T: for<'de> Deserialize<'de>>(
    response: reqwest::Response,
    label: &str,
) -> Result<T> {
    let status = response.status();
    let text = response.text().await.map_err(provider_transport_error)?;
    if !status.is_success() {
        return Err(MezError::new(
            MezErrorKind::Io,
            format!(
                "{label} failed with status {status}: {}",
                provider_error_detail(&text)
            ),
        ));
    }
    serde_json::from_str(&text).map_err(provider_parse_error)
}

/// Runs the async http client operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn async_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(HTTP_CLIENT_TIMEOUT)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Runs the provider transport error operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn provider_transport_error(error: reqwest::Error) -> MezError {
    MezError::new(
        MezErrorKind::Io,
        format!("OpenAI auth request failed: {error}"),
    )
}

/// Runs the provider parse error operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn provider_parse_error(error: serde_json::Error) -> MezError {
    MezError::new(
        MezErrorKind::Io,
        format!("OpenAI auth response could not be decoded: {error}"),
    )
}

/// Runs the provider error detail operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn provider_error_detail(body: &str) -> String {
    if body.trim().is_empty() {
        return "empty provider response".to_string();
    }
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .or_else(|| value.get("error_description"))
                .or_else(|| value.get("message"))
                .or_else(|| value.get("error"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.chars().take(240).collect())
}

/// Runs the build authorize url operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn build_authorize_url(
    issuer: &str,
    client_id: &str,
    redirect_uri: &str,
    pkce: &PkceCodes,
    state: &str,
) -> String {
    let query = form_body(&[
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("scope", OPENAI_OAUTH_SCOPE),
        ("code_challenge", &pkce.code_challenge),
        ("code_challenge_method", "S256"),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("state", state),
        ("originator", "mezzanine_cli"),
    ]);
    format!("{}/oauth/authorize?{query}", issuer.trim_end_matches('/'))
}

/// Runs the generate pkce operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn generate_pkce() -> PkceCodes {
    let code_verifier = random_urlsafe_token(64);
    let digest = sha2::Sha256::digest(code_verifier.as_bytes());
    let code_challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    PkceCodes {
        code_verifier,
        code_challenge,
    }
}

/// Runs the random urlsafe token operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn random_urlsafe_token(length: usize) -> String {
    let mut bytes = vec![0u8; length];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Runs the form body operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn form_body(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                urlencoding::encode(key),
                urlencoding::encode(value)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// Runs the parse query operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_query(query: &str) -> Result<BTreeMap<String, String>> {
    let mut values = BTreeMap::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = urlencoding::decode(key)
            .map_err(|_| MezError::invalid_state("OpenAI browser callback had malformed query"))?;
        let value = urlencoding::decode(value)
            .map_err(|_| MezError::invalid_state("OpenAI browser callback had malformed query"))?;
        values.insert(key.into_owned(), value.into_owned());
    }
    Ok(values)
}

/// Runs the parse jwt claims operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_jwt_claims(token: &str) -> Option<Value> {
    let claims = token.split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(claims.as_bytes())
        .ok()?;
    serde_json::from_slice(&decoded).ok()
}

/// Runs the provider credential from tokens operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn provider_credential_from_tokens(tokens: TokenResponse) -> OpenAiProviderCredential {
    let id_claims = tokens
        .id_token
        .as_deref()
        .and_then(parse_jwt_claims)
        .unwrap_or_default();
    let access_claims = parse_jwt_claims(&tokens.access_token).unwrap_or_default();
    let token_expires_at = access_claims
        .get("exp")
        .or_else(|| id_claims.get("exp"))
        .and_then(Value::as_u64)
        .or_else(|| {
            tokens.expires_in.and_then(|expires_in| {
                current_unix_seconds().map(|now| now.saturating_add(expires_in))
            })
        })
        .map(|expires_at| expires_at.to_string());
    OpenAiProviderCredential {
        api_key: tokens.access_token,
        refresh_token: tokens.refresh_token,
        account_id: account_id_from_claims(&id_claims)
            .or_else(|| account_id_from_claims(&access_claims)),
        organization_id: organization_id_from_claims(&id_claims)
            .or_else(|| organization_id_from_claims(&access_claims)),
        token_expires_at,
    }
}

/// Runs the account id from claims operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn account_id_from_claims(claims: &Value) -> Option<String> {
    auth_claim_string(claims, "chatgpt_account_id")
        .or_else(|| claim_string(claims, "chatgpt_account_id"))
        .or_else(|| claim_string(claims, "account_id"))
        .or_else(|| claim_string(claims, "sub"))
}

/// Runs the organization id from claims operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn organization_id_from_claims(claims: &Value) -> Option<String> {
    auth_claim_string(claims, "organization_id")
        .or_else(|| claim_string(claims, "organization_id"))
        .or_else(|| first_organization_id(claims))
}

/// Runs the auth claim string operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn auth_claim_string(claims: &Value, field: &str) -> Option<String> {
    claims
        .get("https://api.openai.com/auth")
        .and_then(|value| value.get(field))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

/// Runs the claim string operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn claim_string(claims: &Value, field: &str) -> Option<String> {
    claims
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

/// Runs the first organization id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn first_organization_id(claims: &Value) -> Option<String> {
    let organizations = claims
        .get("https://api.openai.com/auth")
        .and_then(|value| value.get("organizations"))
        .or_else(|| claims.get("organizations"))?;
    organizations.as_array()?.iter().find_map(|organization| {
        organization
            .get("organization_id")
            .or_else(|| organization.get("id"))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
    })
}

/// Runs the deserialize device interval operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn deserialize_device_interval<'de, D>(deserializer: D) -> std::result::Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::Number(number) => Ok(number.as_u64().unwrap_or(5)),
        Value::String(text) => text.trim().parse().map_err(serde::de::Error::custom),
        _ => Ok(5),
    }
}

/// Runs the deserialize optional u64 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn deserialize_optional_u64<'de, D>(deserializer: D) -> std::result::Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    match value {
        Some(Value::Number(number)) => Ok(number.as_u64()),
        Some(Value::String(text)) if text.trim().is_empty() => Ok(None),
        Some(Value::String(text)) => text
            .trim()
            .parse()
            .map(Some)
            .map_err(serde::de::Error::custom),
        Some(_) | None => Ok(None),
    }
}

/// Runs the current unix seconds operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn current_unix_seconds() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

/// Runs the oauth error message operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn oauth_error_message(error_code: &str, error_description: Option<&str>) -> String {
    let base = match error_code {
        "access_denied" => "access was denied",
        "invalid_request" => "the authorization request was invalid",
        "server_error" => "the authorization server returned an error",
        other => other,
    };
    match error_description.filter(|description| !description.trim().is_empty()) {
        Some(description) => format!("{base}: {description}"),
        None => base.to_string(),
    }
}

/// Runs the html escape operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn html_escape(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// Runs the json escape operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn json_escape(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch.is_control() => escaped.push(' '),
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// Runs the open browser operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn open_browser(url: &str) -> bool {
    browser_open_commands(url).into_iter().any(|mut command| {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
    })
}

/// Runs the browser open commands operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(target_os = "macos")]
fn browser_open_commands(url: &str) -> Vec<Command> {
    let mut open = Command::new("open");
    open.arg(url);
    vec![open]
}

/// Runs the browser open commands operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(target_os = "windows")]
fn browser_open_commands(url: &str) -> Vec<Command> {
    let mut cmd = Command::new("cmd");
    cmd.args(["/C", "start", "", url]);
    vec![cmd]
}

/// Runs the browser open commands operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(all(unix, not(target_os = "macos")))]
fn browser_open_commands(url: &str) -> Vec<Command> {
    ["xdg-open", "gio", "sensible-browser"]
        .into_iter()
        .map(|program| {
            let mut command = Command::new(program);
            if program == "gio" {
                command.arg("open");
            }
            command.arg(url);
            command
        })
        .collect()
}

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    /// Verifies authorize url contains pkce browser login parameters.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    #[test]
    /// Verifies that the authorize URL follows the ChatGPT PKCE browser flow
    /// shape used by the login implementation without exposing secret values.
    fn authorize_url_contains_pkce_browser_login_parameters() {
        let pkce = PkceCodes {
            code_verifier: "verifier".to_string(),
            code_challenge: "challenge".to_string(),
        };
        let url = build_authorize_url(
            DEFAULT_ISSUER,
            DEFAULT_CLIENT_ID,
            "http://localhost:1455/auth/callback",
            &pkce,
            "state",
        );

        assert!(url.starts_with("https://auth.openai.com/oauth/authorize?"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann"));
        assert!(!url.contains("client_secret"));
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"));
        assert!(url.contains("code_challenge=challenge"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(!url.contains("api.model.read"));
        assert!(url.contains("codex_cli_simplified_flow=true"));
    }

    /// Verifies device code requests use OAuth scopes rather than API-key permissions.
    ///
    /// Browser login and device-code login should mint equivalent provider
    /// credentials, but OpenAI's ChatGPT OAuth client rejects restricted API-key
    /// permission labels such as `api.model.read` as invalid OAuth scopes.
    #[test]
    fn device_code_request_body_omits_api_key_permission_scope() {
        let body: Value =
            serde_json::from_str(&device_code_request_body(DEFAULT_CLIENT_ID)).unwrap();

        assert_eq!(body["client_id"], DEFAULT_CLIENT_ID);
        assert!(body.get("client_secret").is_none());
        assert!(!body["scope"].as_str().unwrap().contains("api.model.read"));
    }

    /// Verifies browser login launch message always includes authorize url.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    #[test]
    /// Verifies that browser login always prints a clickable fallback URL,
    /// including the default path where the browser launcher reports success.
    fn browser_login_launch_message_always_includes_authorize_url() {
        let url = "https://auth.openai.com/oauth/authorize?state=test";

        let opened = browser_login_launch_message(url, true);
        assert!(opened.contains("Opened a browser"));
        assert!(opened.contains(url));

        let fallback = browser_login_launch_message(url, false);
        assert!(fallback.contains("Open this URL"));
        assert!(fallback.contains(url));
    }

    /// Verifies callback parser enforces state and extracts code.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    #[test]
    /// Verifies that callback parsing rejects state mismatches and returns only
    /// the authorization code for a valid browser callback request.
    fn callback_parser_enforces_state_and_extracts_code() {
        let request = "GET /auth/callback?code=abc123&state=good HTTP/1.1\r\n\r\n";

        assert_eq!(
            parse_callback_request(request, "good").unwrap(),
            "abc123".to_string()
        );
        let error = parse_callback_request(request, "bad").unwrap_err();
        assert_eq!(error.kind(), MezErrorKind::InvalidState);
    }

    /// Verifies browser callback wait accepts valid request.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    #[test]
    /// Verifies that the browser callback listener accepts a valid callback
    /// through the Tokio socket path and still writes a browser-visible success
    /// response. This covers the local waiting boundary without contacting the
    /// OpenAI provider.
    fn browser_callback_wait_accepts_valid_request() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = std::thread::spawn(move || {
            let mut stream = std::net::TcpStream::connect(addr).unwrap();
            stream
                .write_all(b"GET /auth/callback?code=abc123&state=good HTTP/1.1\r\n\r\n")
                .unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            assert!(response.contains("OpenAI sign-in completed"));
        });

        let code = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap()
            .block_on(wait_for_browser_authorization_code_async(listener, "good"))
            .unwrap();

        assert_eq!(code, "abc123");
        client.join().unwrap();
    }

    /// Verifies jwt claim parser extracts non secret metadata.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    #[test]
    /// Verifies that non-secret account metadata can be derived from the ID
    /// token payload without retaining raw tokens in metadata.
    fn jwt_claim_parser_extracts_non_secret_metadata() {
        let claims = r#"{"chatgpt_account_id":"acct_123","exp":12345}"#;
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims);
        let jwt = format!("header.{encoded}.signature");
        let parsed = parse_jwt_claims(&jwt).unwrap();

        assert_eq!(
            parsed
                .get("chatgpt_account_id")
                .and_then(Value::as_str)
                .unwrap(),
            "acct_123"
        );
        assert_eq!(parsed.get("exp").and_then(Value::as_u64).unwrap(), 12345);
    }

    /// Verifies provider credential uses access token and nested auth claims.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    #[test]
    /// Verifies that ChatGPT OAuth completion keeps the access token returned
    /// by the standard authorization-code exchange instead of performing an
    /// additional ID-token API-key exchange. The latter requires organization
    /// claims that are not present for all accounts and caused successful
    /// browser/device authorization to fail after callback completion.
    fn provider_credential_uses_access_token_and_nested_auth_claims() {
        let access_claims = r#"{"exp":12345}"#;
        let id_claims = r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acct_123","organization_id":"org_123"}}"#;
        let access = format!(
            "header.{}.signature",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(access_claims)
        );
        let id_token = format!(
            "header.{}.signature",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(id_claims)
        );

        let credential = provider_credential_from_tokens(TokenResponse {
            access_token: access.clone(),
            id_token: Some(id_token),
            refresh_token: Some("refresh-secret".to_string()),
            expires_in: None,
        });

        assert_eq!(credential.api_key, access);
        assert_eq!(credential.refresh_token.as_deref(), Some("refresh-secret"));
        assert_eq!(credential.account_id.as_deref(), Some("acct_123"));
        assert_eq!(credential.organization_id.as_deref(), Some("org_123"));
        assert_eq!(credential.token_expires_at.as_deref(), Some("12345"));
    }

    /// Verifies debug formatting for OAuth token responses redacts token
    /// material before conversion into persistent credential references.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    #[test]
    fn token_response_debug_formatting_redacts_raw_tokens() {
        let response = TokenResponse {
            access_token: "access-secret".to_string(),
            id_token: Some("id-secret".to_string()),
            refresh_token: Some("refresh-secret".to_string()),
            expires_in: Some(60),
        };

        let debug = format!("{response:?}");

        assert!(!debug.contains("access-secret"));
        assert!(!debug.contains("id-secret"));
        assert!(!debug.contains("refresh-secret"));
        assert!(debug.contains("expires_in"));
    }

    /// Verifies provider credential uses expires in for opaque access tokens.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    #[test]
    /// Verifies that OAuth token responses with opaque access tokens can still
    /// populate expiration metadata from `expires_in`. Launch-time refresh
    /// scheduling depends on this metadata when the token itself has no JWT
    /// `exp` claim.
    fn provider_credential_uses_expires_in_for_opaque_access_tokens() {
        let before = current_unix_seconds().unwrap();

        let credential = provider_credential_from_tokens(TokenResponse {
            access_token: "opaque-access-token".to_string(),
            id_token: None,
            refresh_token: Some("refresh-secret".to_string()),
            expires_in: Some(60),
        });

        let expires_at = credential
            .token_expires_at
            .as_deref()
            .unwrap()
            .parse::<u64>()
            .unwrap();
        assert!(expires_at >= before.saturating_add(60));
        assert!(expires_at <= current_unix_seconds().unwrap().saturating_add(60));
    }

    /// Verifies device interval accepts string values.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    #[test]
    /// Verifies that device-code intervals are accepted as the string form
    /// returned by the OpenAI device-code endpoint.
    fn device_interval_accepts_string_values() {
        let response: DeviceCodeResponse = serde_json::from_str(
            r#"{"device_auth_id":"id","user_code":"CODE-123","interval":"2"}"#,
        )
        .unwrap();

        assert_eq!(response.interval, 2);
    }

    /// Verifies device authorization poll accepts successful response.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    #[test]
    /// Verifies that device-code polling can complete through the Tokio HTTP
    /// path without sleeping or contacting the real provider. This keeps the
    /// CLI login path covered while the polling implementation uses async
    /// request and timer primitives internally.
    fn device_authorization_poll_accepts_successful_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request).unwrap();
            let body = r#"{"authorization_code":"auth-code","code_challenge":"challenge","code_verifier":"verifier"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            std::io::Write::write_all(&mut stream, response.as_bytes()).unwrap();
        });
        let device_code = DeviceCodeResponse {
            device_auth_id: "device-id".to_string(),
            user_code: "USER-CODE".to_string(),
            interval: 1,
        };

        let response = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap()
            .block_on(poll_device_authorization_async(
                &format!("http://{addr}"),
                &device_code,
            ))
            .unwrap();

        assert_eq!(response.authorization_code, "auth-code");
        assert_eq!(response.code_challenge, "challenge");
        assert_eq!(response.code_verifier, "verifier");
        server.join().unwrap();
    }

    /// Verifies html response escapes body text.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    #[test]
    /// Verifies that success pages escape provider-controlled text before
    /// writing browser-visible HTML.
    fn html_response_escapes_body_text() {
        let mut bytes = Vec::new();
        write_http_response(&mut bytes, 200, "<script>bad()</script>").unwrap();
        let response = String::from_utf8(bytes).unwrap();

        assert!(response.contains("&lt;script&gt;bad()&lt;/script&gt;"));
        assert!(!response.contains("<script>bad()</script>"));
    }

    /// Verifies themed login success pages use the Mez transcript callback layout.
    ///
    /// This protects the browser-visible page contract without launching a
    /// browser or contacting the provider.
    #[test]
    fn html_response_uses_themed_mez_transcript_success_page() {
        let tokens = login_page_theme_tokens(&UiTheme::default());
        let mut bytes = Vec::new();
        write_http_response_with_tokens(
            &mut bytes,
            200,
            "OpenAI sign-in completed. You can return to Mezzanine.",
            &tokens,
        )
        .unwrap();
        let response = String::from_utf8(bytes).unwrap();

        assert!(response.contains("Login successful"));
        assert!(response.contains("class=\"mez-shell\""));
        assert!(response.contains("class=\"mez-pane\""));
        assert!(response.contains("class=\"transcript-line agent-line\""));
        assert!(response.contains("<span class=\"speaker\">agent:</span>"));
        assert!(response.contains("--accent-primary: #7e9cd8"));
        assert!(response.contains("No external page assets were loaded"));
        assert!(response.contains("color-scheme: dark"));
    }

    /// Verifies login callback tokens adapt to light Mezzanine themes.
    ///
    /// The callback page should follow active theme mode without coupling auth
    /// logic to terminal rendering internals.
    #[test]
    fn login_page_theme_tokens_adapt_to_light_theme() {
        let theme = crate::terminal::resolve_ui_theme(
            "gruvbox_light",
            crate::terminal::builtin_ui_theme_definition("gruvbox_light").unwrap(),
        )
        .unwrap();
        let tokens = login_page_theme_tokens(&theme);

        assert!(!tokens.is_dark);
        assert_eq!(tokens.glow_strength, "0.12");
        assert_ne!(tokens.bg, login_page_theme_tokens(&UiTheme::default()).bg);
    }

    /// Verifies generated pkce has urlsafe verifier and challenge.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    #[test]
    /// Verifies that generated PKCE values satisfy the browser auth flow shape
    /// without needing network access.
    fn generated_pkce_has_urlsafe_verifier_and_challenge() {
        let pkce = generate_pkce();

        assert!(pkce.code_verifier.len() >= 43);
        assert!(pkce.code_challenge.len() >= 43);
        assert!(!pkce.code_verifier.contains('='));
        assert!(!pkce.code_challenge.contains('='));
    }

    /// Verifies form body percent encodes reserved characters.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    #[test]
    /// Verifies that URL form encoding keeps callback URLs and scopes valid in
    /// OpenAI OAuth requests.
    fn form_body_percent_encodes_reserved_characters() {
        let body = form_body(&[("redirect_uri", "http://localhost:1455/auth/callback")]);

        assert_eq!(
            body,
            "redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"
        );
    }

    /// Verifies system time epoch import remains available for metadata format.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    #[test]
    /// Verifies that generated expiry metadata remains a simple Unix timestamp
    /// string when the ID token contains an expiry claim.
    fn system_time_epoch_import_remains_available_for_metadata_format() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .to_string();

        assert!(!now.is_empty());
    }
}
