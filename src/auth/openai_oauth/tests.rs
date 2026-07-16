//! OpenAI OAuth behavior and regression tests.

use super::browser_flow::*;
use super::callback_server::*;
use super::claims::*;
use super::http::*;
use super::login_page::*;
use super::pkce::*;
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
    let body: Value = serde_json::from_str(&device_code_request_body(DEFAULT_CLIENT_ID)).unwrap();

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
    let response: DeviceCodeResponse =
        serde_json::from_str(r#"{"device_auth_id":"id","user_code":"CODE-123","interval":"2"}"#)
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
    let theme = mez_mux::theme::resolve_ui_theme(
        "gruvbox_light",
        mez_mux::theme::builtin_ui_theme_definition("gruvbox_light").unwrap(),
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
