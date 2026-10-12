//! Local HTTP refresh fixtures; no real credentials or provider calls.

/// Holds commit ownership past the bounded wait (and in a separate case aborts
/// the worker after receipt). A subsequent request must persist the cached
/// single-use rotation without contacting the OAuth endpoint a second time.
#[tokio::test]
async fn oauth_refresh_commit_timeout_and_cancel_retain_rotation() {
    for cancel in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (store, root) = fixture(
            &format!("retain-{cancel}"),
            format!("http://{}", listener.local_addr().unwrap()),
        );
        let clone = store.clone();
        let worker = tokio::spawn(async move {
            clone
                .refresh_provider_credential_if_needed_with_leeway_async("openai", 0)
                .await
        });
        let (mut stream, _) = listener.accept().await.unwrap();
        read_headers(&mut stream).await;
        let lock = super::fs::auth_commit_lock(&root).unwrap();
        let body = r#"{"access_token":"retained-access","refresh_token":"retained-refresh","expires_in":999999}"#;
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        if cancel {
            tokio::time::sleep(std::time::Duration::from_millis(60)).await;
            worker.abort();
            assert!(worker.await.unwrap_err().is_cancelled());
        } else {
            assert_eq!(
                worker.await.unwrap().unwrap_err().kind(),
                MezErrorKind::Conflict
            );
        }
        drop(lock);
        // The listener has no second responder: an incorrect HTTP retry hangs
        // until the outer timeout, instead of accidentally passing the test.
        assert!(
            store
                .refresh_provider_credential_if_needed_with_leeway_async("openai", 0)
                .await
                .unwrap()
        );
        use secrecy::ExposeSecret;
        assert_eq!(
            store.provider_secret("openai").unwrap().expose_secret(),
            "retained-access"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// Verifies independent stores (the CLI writer boundary) cannot mutate any
/// auth identity while the shared commit lock is held. Acquisition is bounded
/// and returns Conflict before secret or metadata writes, including logout.
/// After release, explicit replacement succeeds without recreating the store.
#[test]
fn auth_commit_lock_serializes_independent_writers() {
    let (store, root) = fixture("commit-lock", "http://127.0.0.1:1".into());
    let independent = AuthStore::new(AuthPaths::under_config_root(&root));
    let lock = super::fs::auth_commit_lock(&root).unwrap();
    let file = independent.file_credential_store("openai").unwrap();
    assert_eq!(
        independent
            .login_with_api_key("openai", "default", "replacement", &file)
            .unwrap_err()
            .kind(),
        MezErrorKind::Conflict
    );
    assert_eq!(
        independent.logout().unwrap_err().kind(),
        MezErrorKind::Conflict
    );
    assert_eq!(
        independent.logout_mcp_server("fixture").unwrap_err().kind(),
        MezErrorKind::Conflict
    );
    let metadata = store.read_metadata_for_provider("openai").unwrap().unwrap();
    assert_eq!(
        independent.write_metadata(&metadata).unwrap_err().kind(),
        MezErrorKind::Conflict
    );
    let mcp = McpAuthMetadata::new("fixture", "https://example.invalid", "fixture-fingerprint");
    assert_eq!(
        independent.write_mcp_metadata(&mcp).unwrap_err().kind(),
        MezErrorKind::Conflict
    );
    assert_eq!(
        independent
            .login_mcp_static_bearer_credential(mcp, "replacement".into(), &file)
            .unwrap_err()
            .kind(),
        MezErrorKind::Conflict
    );
    use secrecy::ExposeSecret;
    assert_eq!(
        store.provider_secret("openai").unwrap().expose_secret(),
        "fixture-access"
    );
    drop(lock);
    independent
        .login_with_api_key("openai", "default", "replacement", &file)
        .unwrap();
    assert_eq!(
        store.provider_secret("openai").unwrap().expose_secret(),
        "replacement"
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// Forces contention precisely at refresh commit after HTTP success. Refresh
/// must retain its successful response while an unrelated writer holds the
/// lock, then persist rotation without another HTTP refresh or explicit login.
#[tokio::test]
async fn oauth_refresh_commit_contention_preserves_replacement() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (store, root) = fixture(
        "commit-contention",
        format!("http://{}", listener.local_addr().unwrap()),
    );
    let clone = store.clone();
    let worker = tokio::spawn(async move {
        clone
            .refresh_provider_credential_if_needed_with_leeway_async("openai", 0)
            .await
    });
    let (mut stream, _) = listener.accept().await.unwrap();
    read_headers(&mut stream).await;
    let lock = super::fs::auth_commit_lock(&root).unwrap();
    let body =
        r#"{"access_token":"stale-access","refresh_token":"stale-refresh","expires_in":999999}"#;
    stream
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    assert!(!worker.is_finished());
    drop(lock);
    assert!(worker.await.unwrap().unwrap());
    use secrecy::ExposeSecret;
    assert_eq!(
        store.provider_secret("openai").unwrap().expose_secret(),
        "stale-access"
    );
    assert!(
        !store
            .refresh_provider_credential_if_needed_with_leeway_async("openai", 0)
            .await
            .unwrap()
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// Verifies named/shared ChatGPT identity rejection can be repaired through the
/// same selected-store persistence boundary used by CLI browser/device login.
/// Default OpenAI credentials remain byte-for-byte unchanged during recovery.
#[tokio::test]
async fn oauth_refresh_named_profile_login_recovers_selected_record() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (store, root) = fixture(
        "named-profile",
        format!("http://{}", listener.local_addr().unwrap()),
    );
    let credential = OpenAiProviderCredential {
        api_key: "named-access".into(),
        refresh_token: Some("named-refresh".into()),
        account_id: Some("named-account".into()),
        organization_id: None,
        token_expires_at: Some("1".into()),
    };
    store
        .login_chatgpt_profile_credential_with_selected_store(
            "shared-chatgpt",
            "default",
            credential.clone(),
            Some("file"),
        )
        .unwrap();
    let endpoint = reply(&listener, 400, r#"{"error":"invalid_grant"}"#);
    let client = async {
        let error = store
            .refresh_provider_credential_if_needed_with_leeway_async("shared-chatgpt", 0)
            .await
            .unwrap_err();
        assert!(
            error
                .message()
                .contains("mez auth login --provider openai --auth-profile shared-chatgpt")
        );
        assert!(
            !store
                .status_for_provider("shared-chatgpt")
                .unwrap()
                .authenticated
        );
        let mut replacement = credential;
        replacement.refresh_token = Some("named-replacement-refresh".into());
        replacement.token_expires_at = Some("4102444800".into());
        store
            .login_chatgpt_profile_credential_with_selected_store(
                "shared-chatgpt",
                "default",
                replacement,
                Some("file"),
            )
            .unwrap();
        assert!(
            !store
                .refresh_provider_credential_if_needed_with_leeway_async("shared-chatgpt", 0)
                .await
                .unwrap()
        );
        assert!(
            store
                .status_for_provider("shared-chatgpt")
                .unwrap()
                .authenticated
        );
        use secrecy::ExposeSecret;
        assert_eq!(
            store.provider_secret("openai").unwrap().expose_secret(),
            "fixture-access"
        );
    };
    tokio::join!(endpoint, client);
    std::fs::remove_dir_all(root).unwrap();
}

use super::{
    AuthCredentialState, AuthPaths, AuthStore, McpAuthMetadata, McpOAuthCredential,
    OpenAiProviderCredential,
};
use crate::error::MezErrorKind;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Creates an expired synthetic credential in an isolated private-file store.
fn fixture(name: &str, issuer: String) -> (AuthStore, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "mez-refresh-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let store = AuthStore::new(AuthPaths::under_config_root(&root)).with_refresh_issuer(issuer);
    login(&store, "fixture-refresh", "1");
    (store, root)
}

/// Replaces metadata/secrets just as explicit login does, without network login.
fn login(store: &AuthStore, refresh: &str, expiry: &str) {
    store
        .login_openai_provider_credential(
            "default",
            OpenAiProviderCredential {
                api_key: "fixture-access".into(),
                refresh_token: Some(refresh.into()),
                account_id: Some("fixture-account".into()),
                organization_id: None,
                token_expires_at: Some(expiry.into()),
            },
            &store.file_credential_store("openai").unwrap(),
        )
        .unwrap();
}

/// Serves one bounded fixture reply after reading the local request headers.
async fn reply(listener: &tokio::net::TcpListener, status: u16, body: &str) {
    let (mut stream, _) = listener.accept().await.unwrap();
    read_headers(&mut stream).await;
    stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
}

/// Reads a bounded header prefix while honoring arbitrary TCP fragmentation.
async fn read_headers(stream: &mut tokio::net::TcpStream) {
    let mut request = Vec::new();
    while !request.ends_with(b"\r\n\r\n") {
        assert!(request.len() < 8192);
        request.push(stream.read_u8().await.unwrap());
    }
}

/// Protects permanent-rejection classification and redaction using actual HTTP
/// refresh requests. A rejected token is attempted once; malformed and transient
/// replies remain retryable on a later explicit request rather than being cached.
#[tokio::test]
async fn oauth_refresh_rejection_and_transient_controls() {
    for (status, body, permanent) in [
        (
            400,
            r#"{"error":"invalid_grant","error_description":"fixture-refresh"}"#,
            true,
        ),
        (400, r#"{"error":{"code":"refresh_token_reused"}}"#, true),
        (400, r#"{"error":"refresh_token_expired"}"#, true),
        (401, "fixture-refresh", true),
        (429, r#"{"error":"invalid_grant"}"#, false),
        (503, r#"{"error":"invalid_grant"}"#, false),
        (400, "not-json fixture-refresh", false),
        (200, "not-json fixture-refresh", false),
        (
            200,
            r#"{"access_token":"synthetic-access","expires_in":"fixture-refresh"}"#,
            false,
        ),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let (store, root) = fixture(&format!("controls-{status}-{permanent}"), issuer);
        let endpoint = async {
            reply(&listener, status, body).await;
            if !permanent {
                reply(&listener, status, body).await;
            }
        };
        let client = async {
            for _ in 0..2 {
                let error = store
                    .refresh_provider_credential_if_needed_with_leeway_async("openai", 0)
                    .await
                    .unwrap_err();
                assert_eq!(error.kind() == MezErrorKind::Forbidden, permanent);
                assert!(!error.to_string().contains("fixture-refresh"));
            }
            assert_eq!(
                store
                    .status_for_provider("openai")
                    .unwrap()
                    .credential_state
                    == AuthCredentialState::AuthenticationRequired,
                permanent
            );
        };
        tokio::join!(endpoint, client);
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// Verifies serialized concurrent refresh performs only one successful rotation
/// and leaves the rotated credential persisted. This guards against token reuse
/// rejection caused by two workers refreshing the same generation concurrently.
#[tokio::test]
async fn oauth_refresh_rotation_serializes_cloned_stores() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let (store, root) = fixture("rotation", issuer);
    let clone = store.clone();
    let endpoint = reply(
        &listener,
        200,
        r#"{"access_token":"rotated-access","refresh_token":"rotated-refresh","expires_in":999999}"#,
    );
    let client = async {
        let (first, second) = tokio::join!(
            store.refresh_provider_credential_if_needed_with_leeway_async("openai", 0),
            clone.refresh_provider_credential_if_needed_with_leeway_async("openai", 0)
        );
        assert!(first.unwrap());
        assert!(!second.unwrap());
        use secrecy::ExposeSecret;
        assert_eq!(
            store.provider_secret("openai").unwrap().expose_secret(),
            "rotated-access"
        );
        assert!(store.status_for_provider("openai").unwrap().authenticated);
    };
    tokio::join!(endpoint, client);
    std::fs::remove_dir_all(root).unwrap();
}

/// Holds refresh I/O pending while login replaces credentials. The stale reply
/// must not overwrite the new login. Aborting a later worker must release its
/// per-profile lock so subsequent requests can still use fresh credentials.
#[tokio::test]
async fn oauth_refresh_stale_rotation_and_cancellation_preserve_login() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let (store, root) = fixture("stale", issuer);
    let clone = store.clone();
    let worker = tokio::spawn(async move {
        clone
            .refresh_provider_credential_if_needed_with_leeway_async("openai", 0)
            .await
    });
    let (mut stream, _) = listener.accept().await.unwrap();
    read_headers(&mut stream).await;
    login(&store, "new-login-refresh", "4102444800");
    let body =
        r#"{"access_token":"stale-access","refresh_token":"stale-refresh","expires_in":999999}"#;
    stream
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    assert_eq!(
        worker.await.unwrap().unwrap_err().kind(),
        MezErrorKind::Conflict
    );
    assert!(
        !store
            .refresh_provider_credential_if_needed_with_leeway_async("openai", 0)
            .await
            .unwrap()
    );
    login(&store, "pending-refresh", "1");
    let clone = store.clone();
    let worker = tokio::spawn(async move {
        clone
            .refresh_provider_credential_if_needed_with_leeway_async("openai", 0)
            .await
    });
    let (_stream, _) = listener.accept().await.unwrap();
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
    login(&store, "post-cancel-refresh", "4102444800");
    assert!(
        !store
            .refresh_provider_credential_if_needed_with_leeway_async("openai", 0)
            .await
            .unwrap()
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// Verifies MCP refresh rejection is typed, redacted and cached for only its
/// server. A subsequent explicit login clears status without restarting the
/// store, and provider credentials remain available throughout.
#[tokio::test]
async fn mcp_refresh_rejection_is_server_scoped_and_recovers() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let (store, root) = fixture("mcp", issuer.clone());
    let metadata =
        McpAuthMetadata::new("fixture", "https://example.invalid", "fixture-fingerprint");
    let credential = McpOAuthCredential {
        access_token: "mcp-access".into(),
        refresh_token: Some("mcp-refresh".into()),
        token_expires_at: Some("1".into()),
        scopes: vec![],
        client_id: Some("fixture-client".into()),
        resource: None,
        authorization_endpoint: None,
        token_endpoint: Some(format!("{issuer}/token")),
    };
    let file_store = store.file_credential_store("mcp").unwrap();
    store
        .login_mcp_oauth_credential(metadata.clone(), credential.clone(), &file_store)
        .unwrap();
    let endpoint = reply(
        &listener,
        400,
        r#"{"error":"invalid_grant","error_description":"mcp-refresh"}"#,
    );
    let client = async {
        for _ in 0..2 {
            let error = store
                .refresh_mcp_oauth_credential_for_server_async("fixture")
                .await
                .unwrap_err();
            assert_eq!(error.kind(), MezErrorKind::Forbidden);
            assert!(error.message().contains("mez mcp login fixture"));
            assert!(!error.to_string().contains("mcp-refresh"));
        }
        assert!(
            !store
                .mcp_status("fixture", None, None)
                .unwrap()
                .authenticated
        );
        assert!(store.status_for_provider("openai").unwrap().authenticated);
        let mut replacement = credential;
        replacement.refresh_token = Some("replacement-mcp-refresh".into());
        store
            .login_mcp_oauth_credential(metadata, replacement, &file_store)
            .unwrap();
        assert!(
            store
                .mcp_status("fixture", None, None)
                .unwrap()
                .authenticated
        );
    };
    tokio::join!(endpoint, client);
    std::fs::remove_dir_all(root).unwrap();
}
