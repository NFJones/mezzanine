//! OAuth provider HTTP requests and response error projection.

use super::login_page::write_http_response_with_tokens;
use super::pkce::form_body;
use super::platform_browser::json_escape;
use super::{
    DEFAULT_CLIENT_ID, DeviceAuthorizationResponse, DeviceCodeResponse, HTTP_CLIENT_TIMEOUT,
    HTTP_REQUEST_TIMEOUT, LOGIN_TIMEOUT, LoginPageThemeTokens, MezError, MezErrorKind,
    OPENAI_OAUTH_SCOPE, PkceCodes, Result, TokenResponse,
};
use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

/// Writes a themed browser callback response to an async stream.
pub(super) async fn write_http_response_with_tokens_async(
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
pub(super) async fn exchange_code_for_tokens_async(
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
pub(super) async fn refresh_tokens_async(
    issuer: &str,
    refresh_token: &str,
) -> Result<TokenResponse> {
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
pub(super) async fn request_device_code_async(issuer: &str) -> Result<DeviceCodeResponse> {
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
pub(super) fn device_code_request_body(client_id: &str) -> String {
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
pub(super) async fn poll_device_authorization_async(
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
pub(super) async fn post_form_async<T: for<'de> Deserialize<'de>>(
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
pub(super) async fn post_json_async<T: for<'de> Deserialize<'de>>(
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
pub(super) async fn parse_provider_response_async<T: for<'de> Deserialize<'de>>(
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
pub(super) fn async_http_client() -> reqwest::Client {
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
pub(super) fn provider_transport_error(error: reqwest::Error) -> MezError {
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
pub(super) fn provider_parse_error(error: serde_json::Error) -> MezError {
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
pub(super) fn provider_error_detail(body: &str) -> String {
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
