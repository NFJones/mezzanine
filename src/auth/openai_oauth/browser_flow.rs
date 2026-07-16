//! Browser and device-code OAuth flow orchestration.

use super::callback_server::parse_callback_request;
use super::claims::provider_credential_from_tokens;
use super::http::{
    exchange_code_for_tokens_async, poll_device_authorization_async, refresh_tokens_async,
    request_device_code_async, write_http_response_with_tokens_async,
};
use super::login_page::login_page_theme_tokens;
use super::pkce::{build_authorize_url, generate_pkce, random_urlsafe_token};
use super::platform_browser::open_browser;
use super::*;

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
pub(super) fn browser_login_launch_message(auth_url: &str, browser_opened: bool) -> String {
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
pub(super) fn openai_auth_issuer() -> String {
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
pub(super) async fn complete_authorization_code_login_async(
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
pub(super) fn bind_browser_login_listener() -> Result<TcpListener> {
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
pub(super) async fn wait_for_browser_authorization_code_async(
    listener: TcpListener,
    expected_state: &str,
) -> Result<String> {
    let page_tokens = login_page_theme_tokens(&UiTheme::default());
    wait_for_browser_authorization_code_with_page_async(listener, expected_state, &page_tokens)
        .await
}

/// Runs the wait loop and writes a themed browser-visible callback page.
pub(super) async fn wait_for_browser_authorization_code_with_page_async(
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
pub(super) fn browser_login_timeout_error() -> MezError {
    MezError::invalid_state("OpenAI browser login timed out after 15 minutes")
}
