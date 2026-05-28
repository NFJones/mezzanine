//! MCP OAuth browser login and refresh helpers.
//!
//! This module implements the streamable-HTTP MCP OAuth boundary without
//! depending on runtime MCP registry state. It keeps raw tokens inside returned
//! credential values so `AuthStore` remains the only persistence writer.

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::Duration;

use base64::Engine;
use rand::Rng;
use serde_json::Value;
use sha2::Digest;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::error::{MezError, Result};

use super::types::McpOAuthCredential;

const MCP_BROWSER_PORT: u16 = 1457;
const MCP_BROWSER_FALLBACK_PORT: u16 = 1458;
const LOGIN_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_CLIENT_ID: &str = "mezzanine";

/// Runs an MCP OAuth authorization-code + PKCE browser login.
pub async fn run_mcp_oauth_login_async(
    server_url: &str,
    scopes: &[String],
    client_id: Option<&str>,
    resource: Option<&str>,
) -> Result<McpOAuthCredential> {
    let metadata = discover_mcp_oauth_metadata(server_url).await?;
    let client_id = client_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_CLIENT_ID);
    let resource = resource.map(str::trim).filter(|value| !value.is_empty());
    let listener = bind_browser_login_listener()?;
    let redirect_uri = format!(
        "http://127.0.0.1:{}/callback",
        listener.local_addr()?.port()
    );
    let pkce = generate_pkce();
    let state = random_urlsafe_token(32);
    let auth_url = build_authorize_url(
        &metadata.authorization_endpoint,
        client_id,
        &redirect_uri,
        &pkce,
        &state,
        scopes,
        resource,
    );
    let browser_opened = open_browser(&auth_url);
    eprintln!(
        "{}",
        browser_login_launch_message(&auth_url, browser_opened)
    );
    let code = wait_for_browser_authorization_code_async(listener, &state).await?;
    let mut credential = exchange_mcp_code_for_tokens_async(
        &metadata.token_endpoint,
        client_id,
        &redirect_uri,
        &pkce.code_verifier,
        &code,
        scopes,
        resource,
    )
    .await?;
    credential.client_id = Some(client_id.to_string());
    credential.resource = resource.map(ToOwned::to_owned);
    credential.authorization_endpoint = Some(metadata.authorization_endpoint);
    credential.token_endpoint = Some(metadata.token_endpoint);
    Ok(credential)
}

/// Refreshes an MCP OAuth credential with a persisted refresh token.
pub async fn refresh_mcp_oauth_credential_async(
    token_endpoint: &str,
    refresh_token: &str,
    client_id: Option<&str>,
    resource: Option<&str>,
) -> Result<McpOAuthCredential> {
    if token_endpoint.trim().is_empty() {
        return Err(MezError::invalid_args(
            "MCP OAuth token endpoint must not be empty",
        ));
    }
    if refresh_token.trim().is_empty() {
        return Err(MezError::invalid_args(
            "MCP OAuth refresh token must not be empty",
        ));
    }
    let client_id = client_id.unwrap_or(DEFAULT_CLIENT_ID);
    let mut form = BTreeMap::new();
    form.insert("grant_type", "refresh_token".to_string());
    form.insert("refresh_token", refresh_token.to_string());
    form.insert("client_id", client_id.to_string());
    if let Some(resource) = resource.filter(|value| !value.trim().is_empty()) {
        form.insert("resource", resource.to_string());
    }
    let tokens = post_token_form(token_endpoint, &form).await?;
    let mut credential = credential_from_token_response(&tokens, &[])?;
    credential.client_id = Some(client_id.to_string());
    credential.resource = resource.map(ToOwned::to_owned);
    credential.token_endpoint = Some(token_endpoint.to_string());
    Ok(credential)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct McpOAuthMetadata {
    authorization_endpoint: String,
    token_endpoint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PkceCodes {
    code_verifier: String,
    code_challenge: String,
}

async fn discover_mcp_oauth_metadata(server_url: &str) -> Result<McpOAuthMetadata> {
    let origin = http_url_origin(server_url)?;
    let authorization_server = format!(
        "{}/.well-known/oauth-authorization-server",
        origin.trim_end_matches('/')
    );
    match fetch_oauth_metadata(&authorization_server).await {
        Ok(metadata) => Ok(metadata),
        Err(_) => Ok(McpOAuthMetadata {
            authorization_endpoint: format!("{}/oauth/authorize", origin.trim_end_matches('/')),
            token_endpoint: format!("{}/oauth/token", origin.trim_end_matches('/')),
        }),
    }
}

async fn fetch_oauth_metadata(url: &str) -> Result<McpOAuthMetadata> {
    let response = async_http_client().get(url).send().await.map_err(|error| {
        MezError::invalid_state(format!("MCP OAuth metadata request failed: {error}"))
    })?;
    if !response.status().is_success() {
        return Err(MezError::invalid_state(format!(
            "MCP OAuth metadata returned status {}",
            response.status().as_u16()
        )));
    }
    let body = response.text().await.map_err(|error| {
        MezError::invalid_state(format!("MCP OAuth metadata response read failed: {error}"))
    })?;
    let value: Value = serde_json::from_str(&body).map_err(|error| {
        MezError::invalid_state(format!("MCP OAuth metadata was not JSON: {error}"))
    })?;
    let authorization_endpoint = string_json_field(&value, "authorization_endpoint")?;
    let token_endpoint = string_json_field(&value, "token_endpoint")?;
    Ok(McpOAuthMetadata {
        authorization_endpoint,
        token_endpoint,
    })
}

async fn exchange_mcp_code_for_tokens_async(
    token_endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    code_verifier: &str,
    code: &str,
    scopes: &[String],
    resource: Option<&str>,
) -> Result<McpOAuthCredential> {
    let mut form = BTreeMap::new();
    form.insert("grant_type", "authorization_code".to_string());
    form.insert("client_id", client_id.to_string());
    form.insert("redirect_uri", redirect_uri.to_string());
    form.insert("code_verifier", code_verifier.to_string());
    form.insert("code", code.to_string());
    if let Some(resource) = resource.filter(|value| !value.trim().is_empty()) {
        form.insert("resource", resource.to_string());
    }
    let tokens = match post_token_form(token_endpoint, &form).await {
        Ok(tokens) => tokens,
        Err(error) if !scopes.is_empty() => {
            eprintln!(
                "MCP OAuth token exchange failed with requested scopes; retrying without scopes: {}",
                error.message()
            );
            post_token_form(token_endpoint, &form).await?
        }
        Err(error) => return Err(error),
    };
    credential_from_token_response(&tokens, scopes)
}

async fn post_token_form(token_endpoint: &str, form: &BTreeMap<&str, String>) -> Result<Value> {
    let response = async_http_client()
        .post(token_endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form_body(form))
        .send()
        .await
        .map_err(|error| {
            MezError::invalid_state(format!("MCP OAuth token request failed: {error}"))
        })?;
    let status = response.status();
    let body = response.text().await.map_err(|error| {
        MezError::invalid_state(format!("MCP OAuth token response read failed: {error}"))
    })?;
    if !status.is_success() {
        return Err(MezError::invalid_state(format!(
            "MCP OAuth token endpoint returned status {}",
            status.as_u16()
        )));
    }
    serde_json::from_str(&body).map_err(|error| {
        MezError::invalid_state(format!("MCP OAuth token response was not JSON: {error}"))
    })
}

fn credential_from_token_response(
    value: &Value,
    requested_scopes: &[String],
) -> Result<McpOAuthCredential> {
    let access_token = string_json_field(value, "access_token")?;
    let refresh_token = value
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let token_expires_at = value
        .get("expires_in")
        .and_then(Value::as_u64)
        .map(|seconds| (chrono::Utc::now().timestamp() + seconds as i64).to_string());
    let scopes = value
        .get("scope")
        .and_then(Value::as_str)
        .map(|scope| scope.split_whitespace().map(ToOwned::to_owned).collect())
        .unwrap_or_else(|| requested_scopes.to_vec());
    Ok(McpOAuthCredential {
        access_token,
        refresh_token,
        token_expires_at,
        scopes,
        client_id: None,
        resource: None,
        authorization_endpoint: None,
        token_endpoint: None,
    })
}

fn bind_browser_login_listener() -> Result<TcpListener> {
    for port in [MCP_BROWSER_PORT, MCP_BROWSER_FALLBACK_PORT] {
        match TcpListener::bind(("127.0.0.1", port)) {
            Ok(listener) => return Ok(listener),
            Err(_) => continue,
        }
    }
    Err(MezError::conflict(format!(
        "MCP OAuth login requires localhost callback port {MCP_BROWSER_PORT} or {MCP_BROWSER_FALLBACK_PORT}, but both are unavailable"
    )))
}

async fn wait_for_browser_authorization_code_async(
    listener: TcpListener,
    expected_state: &str,
) -> Result<String> {
    listener.set_nonblocking(true)?;
    let listener = tokio::net::TcpListener::from_std(listener)?;
    let deadline = tokio::time::Instant::now() + LOGIN_TIMEOUT;
    let (mut stream, _) = tokio::time::timeout_at(deadline, listener.accept())
        .await
        .map_err(|_| {
            MezError::invalid_state("MCP OAuth browser login timed out after 15 minutes")
        })??;
    let mut buffer = [0u8; 8192];
    let bytes_read = tokio::time::timeout(HTTP_REQUEST_TIMEOUT, stream.read(&mut buffer))
        .await
        .map_err(|_| {
            MezError::invalid_state("MCP OAuth browser callback timed out while reading")
        })??;
    let request = String::from_utf8_lossy(&buffer[..bytes_read]);
    let code = match parse_callback_request(&request, expected_state) {
        Ok(code) => {
            let _ = write_http_response_async(&mut stream, 200, "MCP login complete.").await;
            code
        }
        Err(error) => {
            let _ = write_http_response_async(&mut stream, 400, "MCP login failed.").await;
            return Err(error);
        }
    };
    Ok(code)
}

async fn write_http_response_async(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    message: &str,
) -> Result<()> {
    let reason = if status == 200 { "OK" } else { "Bad Request" };
    let body = format!("<html><body>{message}</body></html>");
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    tokio::time::timeout(HTTP_REQUEST_TIMEOUT, stream.write_all(response.as_bytes()))
        .await
        .map_err(|_| {
            MezError::invalid_state("MCP OAuth browser callback timed out while writing")
        })??;
    Ok(())
}

fn parse_callback_request(request: &str, expected_state: &str) -> Result<String> {
    let request_line = request
        .lines()
        .next()
        .ok_or_else(|| MezError::invalid_state("MCP OAuth browser callback was empty"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| MezError::invalid_state("MCP OAuth browser callback was malformed"))?;
    let target = parts.next().ok_or_else(|| {
        MezError::invalid_state("MCP OAuth browser callback did not include authorization data")
    })?;
    if method != "GET" {
        return Err(MezError::invalid_state(
            "MCP OAuth browser callback used an unsupported HTTP method",
        ));
    }
    let (path, query) = target.split_once('?').ok_or_else(|| {
        MezError::invalid_state("MCP OAuth browser callback did not include authorization data")
    })?;
    if path != "/callback" {
        return Err(MezError::invalid_state(
            "MCP OAuth browser callback used an unexpected path",
        ));
    }
    let query = parse_query(query)?;
    let state = query.get("state").ok_or_else(|| {
        MezError::invalid_state("MCP OAuth browser callback did not include state")
    })?;
    if state != expected_state {
        return Err(MezError::invalid_state(
            "MCP OAuth browser callback state did not match the login request",
        ));
    }
    if let Some(error) = query.get("error") {
        return Err(MezError::invalid_state(format!(
            "MCP OAuth login failed: {error}"
        )));
    }
    query
        .get("code")
        .cloned()
        .ok_or_else(|| MezError::invalid_state("MCP OAuth browser callback did not include a code"))
}

fn build_authorize_url(
    authorization_endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    pkce: &PkceCodes,
    state: &str,
    scopes: &[String],
    resource: Option<&str>,
) -> String {
    let mut params = vec![
        ("response_type", "code".to_string()),
        ("client_id", client_id.to_string()),
        ("redirect_uri", redirect_uri.to_string()),
        ("code_challenge", pkce.code_challenge.clone()),
        ("code_challenge_method", "S256".to_string()),
        ("state", state.to_string()),
    ];
    if !scopes.is_empty() {
        params.push(("scope", scopes.join(" ")));
    }
    if let Some(resource) = resource.filter(|value| !value.trim().is_empty()) {
        params.push(("resource", resource.to_string()));
    }
    format!("{}?{}", authorization_endpoint, encode_form_query(&params))
}

fn generate_pkce() -> PkceCodes {
    let code_verifier = random_urlsafe_token(64);
    let digest = sha2::Sha256::digest(code_verifier.as_bytes());
    let code_challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    PkceCodes {
        code_verifier,
        code_challenge,
    }
}

fn random_urlsafe_token(length: usize) -> String {
    let mut bytes = vec![0u8; length];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn form_body(fields: &BTreeMap<&str, String>) -> String {
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

fn encode_form_query(params: &[(&str, String)]) -> String {
    params
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

fn parse_query(query: &str) -> Result<BTreeMap<String, String>> {
    let mut params = BTreeMap::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = urlencoding::decode(key).map_err(|_| {
            MezError::invalid_state("MCP OAuth browser callback had malformed query")
        })?;
        let value = urlencoding::decode(value).map_err(|_| {
            MezError::invalid_state("MCP OAuth browser callback had malformed query")
        })?;
        params.insert(key.into_owned(), value.into_owned());
    }
    Ok(params)
}

fn string_json_field(value: &Value, field: &str) -> Result<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| MezError::invalid_state(format!("MCP OAuth response missing `{field}`")))
}

fn http_url_origin(url: &str) -> Result<String> {
    let Some((scheme, rest)) = url.split_once("://") else {
        return Err(MezError::invalid_args(
            "MCP HTTP server URL must include a scheme",
        ));
    };
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .filter(|authority| !authority.is_empty())
        .ok_or_else(|| MezError::invalid_args("MCP HTTP server URL must include a host"))?;
    Ok(format!("{}://{}", scheme.to_ascii_lowercase(), authority))
}

fn async_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

fn browser_login_launch_message(auth_url: &str, browser_opened: bool) -> String {
    if browser_opened {
        format!("Opened a browser for MCP login.\nIf it did not open, use this URL:\n{auth_url}")
    } else {
        format!("Open this URL in your browser to continue MCP login:\n{auth_url}")
    }
}

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

#[cfg(target_os = "macos")]
fn browser_open_commands(url: &str) -> Vec<Command> {
    let mut command = Command::new("open");
    command.arg(url);
    vec![command]
}

#[cfg(target_os = "windows")]
fn browser_open_commands(url: &str) -> Vec<Command> {
    let mut command = Command::new("rundll32");
    command.args(["url.dll,FileProtocolHandler", url]);
    vec![command]
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn browser_open_commands(url: &str) -> Vec<Command> {
    ["xdg-open", "gio", "sensible-browser"]
        .into_iter()
        .map(|program| {
            let mut command = Command::new(program);
            command.arg(url);
            command
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies authorize URLs carry the PKCE, scope, and resource fields MCP
    /// servers need for OAuth authorization-code login.
    #[test]
    fn authorize_url_contains_pkce_scope_and_resource_parameters() {
        let pkce = PkceCodes {
            code_verifier: "verifier".to_string(),
            code_challenge: "challenge".to_string(),
        };
        let url = build_authorize_url(
            "https://auth.example.test/oauth/authorize",
            "client",
            "http://127.0.0.1:1457/callback",
            &pkce,
            "state",
            &["tools.read".to_string(), "tools.write".to_string()],
            Some("https://mcp.example.test"),
        );
        assert!(url.starts_with("https://auth.example.test/oauth/authorize?"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("code_challenge=challenge"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("scope=tools.read%20tools.write"));
        assert!(url.contains("resource=https%3A%2F%2Fmcp.example.test"));
    }

    /// Verifies callback parsing rejects CSRF state mismatches while accepting
    /// the callback path and authorization code used by the browser flow.
    #[test]
    fn callback_parser_requires_matching_state() {
        let request = "GET /callback?code=ok&state=good HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";
        assert_eq!(parse_callback_request(request, "good").unwrap(), "ok");
        let error = parse_callback_request(request, "bad").unwrap_err();
        assert!(error.message().contains("state did not match"));
    }
}
