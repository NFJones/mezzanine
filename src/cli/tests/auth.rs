//! CLI auth tests.

use super::*;

/// Verifies auth status and logout use dedicated auth store.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn auth_status_and_logout_use_dedicated_auth_store() {
    let (env, home) = test_env("auth-status");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec!["mez".to_string(), "auth".to_string(), "status".to_string()],
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(stdout).unwrap(),
        r#"{"authenticated":false,"metadata":null}"#.to_string() + "\n"
    );

    let mut logout_stdout = Vec::new();
    run_with(
        vec!["mez".to_string(), "auth".to_string(), "logout".to_string()],
        env,
        false,
        &mut logout_stdout,
        &mut stderr,
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(logout_stdout).unwrap(),
        "{\"logged_out\":false}\n"
    );

    let _ = fs::remove_dir_all(home);
}

/// Verifies auth status JSON omits privacy-sensitive provider metadata.
///
/// The default status contract is safe to share for debugging: it reports the
/// coarse credential state without exposing account identifiers or raw
/// credential-store locators from the local auth metadata file.
#[test]
fn auth_status_json_omits_account_and_store_metadata() {
    let (env, home) = test_env("auth-status-private-metadata");
    let paths = env.config_paths().unwrap();
    let auth_store = AuthStore::new(AuthPaths::under_config_root(paths.root()));
    let credential_store = auth_store.file_credential_store("openai").unwrap();
    auth_store
        .login_openai_provider_credential(
            "default",
            OpenAiProviderCredential {
                api_key: "access-secret".to_string(),
                refresh_token: Some("refresh-secret".to_string()),
                account_id: Some("acct_123".to_string()),
                organization_id: Some("org_123".to_string()),
                token_expires_at: Some("9999999999".to_string()),
            },
            &credential_store,
        )
        .unwrap();

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_with(
        vec!["mez".to_string(), "auth".to_string(), "status".to_string()],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains(r#""authenticated":true"#), "{output}");
    assert!(output.contains(r#""provider":"openai""#), "{output}");
    assert!(
        output.contains(r#""credential_kind":"chatgpt""#),
        "{output}"
    );
    assert!(!output.contains("account_id"), "{output}");
    assert!(!output.contains("acct_123"), "{output}");
    assert!(!output.contains("organization_id"), "{output}");
    assert!(!output.contains("org_123"), "{output}");
    assert!(!output.contains("credential_store_ref"), "{output}");
    assert!(!output.contains("access-secret"), "{output}");
    assert!(stderr.is_empty(), "{}", String::from_utf8_lossy(&stderr));

    let _ = fs::remove_dir_all(home);
}

/// Verifies auth login noninteractive default requires browser interaction.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
/// Verifies that default auth login is browser-first and fails actionably when
/// a browser flow cannot be run from a noninteractive terminal.
fn auth_login_noninteractive_default_requires_browser_interaction() {
    let (env, home) = test_env("auth-login-default-noninteractive");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let error = run_with_plain(
        vec!["mez".to_string(), "auth".to_string(), "login".to_string()],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(error.message().contains("defaults to browser-based"));
    assert!(error.message().contains("--device-code"));
    assert!(error.message().contains("--api-key --api-key-file PATH"));
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies auth login method selection is browser first.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
/// Verifies that auth method selection is browser-first while retaining explicit
/// device-code and API-key options.
fn auth_login_method_selection_is_browser_first() {
    assert_eq!(
        super::super::auth::auth_login_method(&["login".to_string()]).unwrap(),
        crate::auth::AuthMethod::Browser
    );
    assert_eq!(
        super::super::auth::auth_login_method(&["login".to_string(), "--browser".to_string()])
            .unwrap(),
        crate::auth::AuthMethod::Browser
    );
    assert_eq!(
        super::super::auth::auth_login_method(&["login".to_string(), "--device-code".to_string(),])
            .unwrap(),
        crate::auth::AuthMethod::DeviceCode
    );
    assert_eq!(
        super::super::auth::auth_login_method(&["login".to_string(), "--device-auth".to_string(),])
            .unwrap(),
        crate::auth::AuthMethod::DeviceCode
    );
    assert_eq!(
        super::super::auth::auth_login_method(&["login".to_string(), "--api-key".to_string(),])
            .unwrap(),
        crate::auth::AuthMethod::ApiKey
    );
}

/// Verifies non-OpenAI browser and device-code auth guidance points to API-key setup.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
/// Verifies that non-OpenAI browser and device-code auth requests fail with
/// provider-specific API-key guidance for Anthropic.
fn auth_login_rejects_non_openai_browser_and_device_code_with_api_key_guidance() {
    let (env, home) = test_env("auth-login-non-openai-guidance");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let browser_error = run_with_plain(
        vec![
            "mez".to_string(),
            "auth".to_string(),
            "login".to_string(),
            "--provider".to_string(),
            "anthropic".to_string(),
            "--browser".to_string(),
        ],
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap_err();

    assert_eq!(
        browser_error.kind(),
        crate::error::MezErrorKind::InvalidArgs
    );
    assert!(
        browser_error
            .message()
            .contains("browser-based login is only supported for OpenAI")
    );
    assert!(
        browser_error
            .message()
            .contains("--provider anthropic --api-key")
    );
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let device_error = run_with_plain(
        vec![
            "mez".to_string(),
            "auth".to_string(),
            "login".to_string(),
            "--provider".to_string(),
            "anthropic".to_string(),
            "--device-code".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap_err();

    assert_eq!(device_error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(
        device_error
            .message()
            .contains("device-code login is only supported for OpenAI")
    );
    assert!(
        device_error
            .message()
            .contains("--provider anthropic --api-key")
    );
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies noninteractive Anthropic API-key login requires an out-of-band secret source.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn auth_login_noninteractive_anthropic_api_key_requires_api_key_file() {
    let (env, home) = test_env("auth-login-anthropic-api-key-noninteractive");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let error = run_with_plain(
        vec![
            "mez".to_string(),
            "auth".to_string(),
            "login".to_string(),
            "--provider".to_string(),
            "anthropic".to_string(),
            "--api-key".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(
        error
            .message()
            .contains("requires noninteractive API-key input")
    );
    assert!(error.message().contains("--api-key-file PATH"));
    assert!(error.message().contains("--provider anthropic --api-key"));
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies auth login rejects conflicting method flags.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
/// Verifies that mutually exclusive auth method flags are rejected before
/// prompting or writing credential metadata.
fn auth_login_rejects_conflicting_method_flags() {
    let (env, home) = test_env("auth-login-conflicting-methods");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let error = run_with_plain(
        vec![
            "mez".to_string(),
            "auth".to_string(),
            "login".to_string(),
            "--api-key".to_string(),
            "--browser".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(error.message().contains("only one authentication method"));
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Verifies auth login api key file persists metadata without printing secret.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn auth_login_api_key_file_persists_metadata_without_printing_secret() {
    let (env, home) = test_env("auth-login-api-key");
    let secret_path = home.join("openai-key.txt");
    fs::create_dir_all(&home).unwrap();
    fs::write(&secret_path, "sk-test-secret\n").unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec![
            "mez".to_string(),
            "auth".to_string(),
            "login".to_string(),
            "--api-key".to_string(),
            "--api-key-file".to_string(),
            secret_path.to_string_lossy().to_string(),
            "--credential-store".to_string(),
            "file".to_string(),
            "--profile".to_string(),
            "default".to_string(),
        ],
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains(r#""authenticated":true"#));
    assert!(output.contains(r#""credential_store":"file""#));
    assert!(!output.contains("sk-test-secret"));

    let mut status_stdout = Vec::new();
    run_with(
        vec!["mez".to_string(), "auth".to_string(), "status".to_string()],
        env,
        false,
        &mut status_stdout,
        &mut stderr,
    )
    .unwrap();
    let status = String::from_utf8(status_stdout).unwrap();
    assert!(status.contains(r#""authenticated":true"#));
    assert!(!status.contains("sk-test-secret"));

    let _ = fs::remove_dir_all(home);
}
