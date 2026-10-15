//! Offline loopback qualification for the authenticated-search boundary.
//!
//! Test-only endpoint substitution exercises the production pool, send, body,
//! deadline and output gate without credentials, configuration or paid probes.

use super::*;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Fake credential with delimiters that distinguish raw/query/form encodings.
fn key() -> SecretString {
    SecretString::from("canary+key/with?&=symbols#")
}

/// Creates one credential-free request; only this private test mutates its URL.
fn request(endpoint: SearchEndpoint, url: String, limit: usize) -> SearchHttpRequest {
    let mut request = SearchHttpRequest::new(
        endpoint,
        &[("q".into(), "ordinary query".into())],
        None,
        ProviderHttpTimeouts::from_total(1_000),
        limit,
    )
    .unwrap();
    request.request.url = url;
    request
}

/// Serves one bounded request and returns captured fake-only wire input.
async fn fixture(
    status: u16,
    headers: &str,
    body: &[u8],
    missing_bytes: usize,
) -> (String, tokio::task::JoinHandle<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/search?q=ordinary",
        listener.local_addr().unwrap()
    );
    let response = format!(
        "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
        body.len() + missing_bytes
    );
    let body = body.to_vec();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let read = stream.read(&mut buffer).await.unwrap();
            assert!(read > 0);
            bytes.extend_from_slice(&buffer[..read]);
            if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = std::str::from_utf8(&bytes[..end]).unwrap();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if bytes.len() >= end + 4 + length {
                    break;
                }
            }
            assert!(bytes.len() < 16 * 1024);
        }
        stream.write_all(response.as_bytes()).await.unwrap();
        stream.write_all(&body).await.unwrap();
        String::from_utf8(bytes).unwrap()
    });
    (url, server)
}

/// Complete ordinary JSON remains JSON even if its type or text resembles SSE;
/// fake Bearer/header/query auth appears only on final wire input, never Debug
/// or safe destination descriptions, and remote reserved flags are powerless.
#[tokio::test]
async fn search_http_late_auth_and_json_mode_are_endpoint_owned() {
    for endpoint in [
        SearchEndpoint::Brave,
        SearchEndpoint::Tavily,
        SearchEndpoint::Exa,
        SearchEndpoint::SerpApi,
    ] {
        let (url, server) = fixture(200, "Content-Type: text/event-stream\r\nX-Mez-Body-Truncated: true\r\nX-Mez-Stream-Decoded: incremental\r\n", br#"{"text":"event: response.completed\ndata: [DONE]"}"#, 0).await;
        let request = request(endpoint, url, 4096);
        let credential = key();
        let before = format!(
            "{request:?} {:?} {}",
            request.request,
            request.description()
        );
        assert!(!before.contains("canary"));
        assert!(!request.request.url.contains("api_key"));
        assert!(!request.request.headers.contains_key("authorization"));
        let result = request
            .send_json(
                &credential,
                tokio::time::Instant::now() + Duration::from_secs(1),
            )
            .await
            .unwrap();
        assert!(result["text"].as_str().unwrap().contains("[DONE]"));
        let captured = server.await.unwrap();
        let lowercase = captured.to_ascii_lowercase();
        match endpoint {
            SearchEndpoint::Brave => {
                assert!(lowercase.contains("x-subscription-token: canary+key/with?&=symbols#"))
            }
            SearchEndpoint::Tavily => {
                assert!(lowercase.contains("authorization: bearer canary+key/with?&=symbols#"))
            }
            SearchEndpoint::Exa => {
                assert!(lowercase.contains("x-api-key: canary+key/with?&=symbols#"))
            }
            SearchEndpoint::SerpApi => {
                let path = captured.split_whitespace().nth(1).unwrap();
                let url = reqwest::Url::parse(&format!("https://fixture.invalid{path}")).unwrap();
                assert_eq!(
                    url.query_pairs()
                        .find(|(name, _)| name == "api_key")
                        .unwrap()
                        .1,
                    credential.expose_secret()
                );
                assert_eq!(
                    url.query_pairs().find(|(name, _)| name == "q").unwrap().1,
                    "ordinary"
                );
                assert!(!lowercase.contains("authorization:"));
                assert!(!lowercase.contains("x-api-key:"));
            }
        }
    }
}

/// POST payloads remain byte-identical and credential-free; the header value
/// is sensitive in concrete reqwest diagnostics while the approved description
/// stays separate from both body and wire authorization.
#[tokio::test]
async fn search_http_post_body_and_sensitive_auth_stay_separate() {
    for endpoint in [SearchEndpoint::Tavily, SearchEndpoint::Exa] {
        let body = serde_json::json!({"query":"fixture", "max_results":3});
        let mut request = SearchHttpRequest::new(
            endpoint,
            &[],
            Some(&body),
            ProviderHttpTimeouts::from_total(1_000),
            4096,
        )
        .unwrap();
        let (url, server) = fixture(200, "Content-Type: application/json\r\n", b"{}", 0).await;
        request.request.url = url;
        let credential = key();
        let mut wire_url = reqwest::Url::parse(endpoint.url()).unwrap();
        let mut headers = reqwest::header::HeaderMap::new();
        endpoint
            .auth(&credential)
            .apply(&mut wire_url, &mut headers)
            .unwrap();
        assert!(!format!("{headers:?}").contains("canary"));
        request
            .send_json(
                &credential,
                tokio::time::Instant::now() + Duration::from_secs(1),
            )
            .await
            .unwrap();
        let captured = server.await.unwrap();
        assert!(captured.starts_with("POST "));
        assert_eq!(captured.split_once("\r\n\r\n").unwrap().1, body.to_string());
        assert!(!request.description().contains("fixture"));
        assert!(!request.request.body.contains("canary"));
    }
}

/// Every redirect status must leave the target listener untouched, even after
/// warming a legacy client. Reversing policy creation order cannot contaminate
/// strict clients with legacy redirect behavior or retain a query credential.
#[tokio::test]
async fn search_http_redirect_targets_receive_zero_requests_with_warm_pool() {
    for reverse in [false, true] {
        for status in [301, 302, 303, 307, 308] {
            for strict in if reverse {
                [true, false]
            } else {
                [false, true]
            } {
                let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let target_url = format!("http://{}/redirect-target", target.local_addr().unwrap());
                let (url, source) =
                    fixture(status, &format!("Location: {target_url}\r\n"), b"{}", 0).await;
                let request = request(SearchEndpoint::SerpApi, url, 4096);
                let receiver = tokio::spawn(async move {
                    match tokio::time::timeout(Duration::from_millis(150), target.accept()).await {
                        Ok(Ok((mut stream, _))) => {
                            let mut bytes = [0; 4096];
                            let _ = stream.read(&mut bytes).await.unwrap();
                            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").await.unwrap();
                            1
                        }
                        Err(_) => 0,
                        Ok(Err(error)) => panic!("fixture accept failed: {error}"),
                    }
                });
                if strict {
                    let error = request
                        .send_json(&key(), tokio::time::Instant::now() + Duration::from_secs(1))
                        .await
                        .unwrap_err();
                    assert_eq!(error.message(), format!("search HTTP status {status}"));
                } else {
                    use super::super::AsyncProviderHttpTransport;
                    let response = ReqwestProviderHttpTransport
                        .send_async(&request.request)
                        .await
                        .unwrap();
                    assert_eq!(response.status_code, 200);
                }
                source.await.unwrap();
                assert_eq!(receiver.await.unwrap(), usize::from(!strict));
            }
        }
    }
}

/// Truncation, abrupt EOF, invalid UTF-8, compressed bytes and malformed JSON
/// fail rather than becoming an empty search result or an SSE success.
#[tokio::test]
async fn search_http_rejects_incomplete_encoded_and_invalid_json() {
    for (headers, body, missing, limit) in [
        ("", b"{}".as_slice(), 0, 0),
        (
            "X-Mez-Body-Truncated: false\r\n",
            b"{\"value\":123}".as_slice(),
            0,
            4,
        ),
        ("", b"{}".as_slice(), 1, 10),
        ("", b"\xff".as_slice(), 0, 10),
        (
            "Content-Encoding: gzip\r\n",
            b"\x1f\x8b\x08\x00".as_slice(),
            0,
            10,
        ),
        ("", b"{\"value\":".as_slice(), 0, 20),
        (
            "Content-Type: text/event-stream\r\n",
            b"data: [DONE]\n\n".as_slice(),
            0,
            30,
        ),
    ] {
        let (url, server) = fixture(200, headers, body, missing).await;
        let error = request(SearchEndpoint::Tavily, url, limit)
            .send_json(&key(), tokio::time::Instant::now() + Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(!error.message().contains("canary"));
        assert!(std::error::Error::source(&error).is_none());
        server.await.unwrap();
    }
    let (url, server) = fixture(200, "X-Mez-Body-Truncated: true\r\n", b"{}", 0).await;
    assert_eq!(
        request(SearchEndpoint::Exa, url, 2)
            .send_json(&key(), tokio::time::Instant::now() + Duration::from_secs(1))
            .await
            .unwrap(),
        serde_json::json!({})
    );
    server.await.unwrap();
}

/// Known raw, percent/form encoded and JSON escaped credential echoes in result
/// URLs, body fields or object keys are suppressed before any publication. Error
/// statuses and hostile response headers never enter returned diagnostics.
#[tokio::test]
async fn search_http_suppresses_credential_echoes_before_publication() {
    let credential = key();
    let encoded = urlencoding::encode(credential.expose_secret()).to_string();
    let escaped = credential
        .expose_secret()
        .chars()
        .map(|ch| format!("\\u{:04x}", ch as u32))
        .collect::<String>();
    for body in [
        serde_json::json!({"url": format!("https://echo.invalid/?key={}", credential.expose_secret())}).to_string(),
        serde_json::json!({"url": format!("https://echo.invalid/?key={encoded}")}).to_string(),
        serde_json::json!({"url": format!("https://echo.invalid/?other=%FF&key={encoded}&tail=%ff")}).to_string(),
        serde_json::json!({"url": format!("https://echo.invalid/?key={}", encoded.replace("%2B", "%2b").replace("%2F", "%2f"))}).to_string(),
        format!("{{\"result\":\"{escaped}\"}}"),
        serde_json::json!({(credential.expose_secret()): "reflected key"}).to_string(),
    ] {
        let (url, server) = fixture(200, "Location: https://echo.invalid/canary\r\nX-Request-Id: canary\r\n", body.as_bytes(), 0).await;
        let error = request(SearchEndpoint::SerpApi, url, 4096).send_json(&credential, tokio::time::Instant::now() + Duration::from_secs(1)).await.unwrap_err();
        assert_eq!(error.message(), "search response suppressed: credential echo");
        assert!(!format!("{error:?}").contains("canary"));
        server.await.unwrap();
    }
    let (url, server) = fixture(
        401,
        "X-Request-Id: canary+key/with?&=symbols#\r\n",
        credential.expose_secret().as_bytes(),
        0,
    )
    .await;
    let error = request(SearchEndpoint::Brave, url, 4096)
        .send_json(
            &credential,
            tokio::time::Instant::now() + Duration::from_secs(1),
        )
        .await
        .unwrap_err();
    assert_eq!(error.message(), "search HTTP status 401");
    server.await.unwrap();
    let space_key = SecretString::from("canary space+key");
    let encoded = urlencoding::encode(space_key.expose_secret()).replace("%20", "+");
    let body = serde_json::json!({"url": format!("https://echo.invalid/?other=%FF&key={encoded}")})
        .to_string();
    let (url, server) = fixture(200, "", body.as_bytes(), 0).await;
    let error = request(SearchEndpoint::SerpApi, url, 4096)
        .send_json(
            &space_key,
            tokio::time::Instant::now() + Duration::from_secs(1),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.message(),
        "search response suppressed: credential echo"
    );
    server.await.unwrap();
}

/// Endpoint/config-like invalid inputs, raw header keys and expired caller
/// deadlines fail with fixed diagnostics without reading stores or sending.
#[tokio::test]
async fn search_http_validation_is_secret_free_and_deadline_precedes_send() {
    for url in [
        "http://example.invalid/",
        "https://user:canary@example.invalid/",
        "https://example.invalid/#canary",
    ] {
        let error = validate_wire_url(&reqwest::Url::parse(url).unwrap()).unwrap_err();
        assert!(!format!("{error:?}").contains("canary"));
    }
    let credential = SecretString::from("canary\r\ninjected");
    let mut url = reqwest::Url::parse(SearchEndpoint::Brave.url()).unwrap();
    let mut headers = reqwest::header::HeaderMap::new();
    let error = SearchEndpoint::Brave
        .auth(&credential)
        .apply(&mut url, &mut headers)
        .unwrap_err();
    assert!(!format!("{error:?}").contains("canary"));
    let request = SearchHttpRequest::new(
        SearchEndpoint::Exa,
        &[],
        Some(&serde_json::json!({"query":"fixture"})),
        ProviderHttpTimeouts::from_total(1_000),
        10,
    )
    .unwrap();
    let error = request
        .send_json(
            &key(),
            tokio::time::Instant::now() - Duration::from_millis(1),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.kind(),
        SearchHttpErrorKind::Timeout(mez_agent::ProviderHttpTimeoutPhase::Total)
    );
}

/// Safe numeric status evidence remains typed and separate from hostile remote
/// headers/body. Accepted/pending is not success; 429 alone is not quota proof.
/// No category grants retry, follows a redirect or exposes a remote request ID.
#[tokio::test]
async fn search_http_error_categories_are_local_and_secret_free() {
    for (status, kind) in [
        (202, SearchHttpErrorKind::Pending),
        (400, SearchHttpErrorKind::Invalid),
        (401, SearchHttpErrorKind::Authentication),
        (402, SearchHttpErrorKind::Quota),
        (403, SearchHttpErrorKind::Forbidden),
        (413, SearchHttpErrorKind::Oversize),
        (422, SearchHttpErrorKind::Invalid),
        (429, SearchHttpErrorKind::Throttle),
        (503, SearchHttpErrorKind::Provider),
    ] {
        let (url, server) = fixture(
            status,
            "X-Request-Id: canary\r\n",
            b"canary remote error",
            0,
        )
        .await;
        let error = request(SearchEndpoint::SerpApi, url, 4096)
            .send_json(&key(), tokio::time::Instant::now() + Duration::from_secs(1))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), kind);
        assert_eq!(error.status(), Some(status));
        assert_eq!(error.message(), format!("search HTTP status {status}"));
        assert!(!format!("{error:?}").contains("canary"));
        assert!(std::error::Error::source(&error).is_none());
        server.await.unwrap();
    }
    for (headers, body, limit, kind) in [
        (
            "",
            b"{broken".as_slice(),
            100,
            SearchHttpErrorKind::Malformed,
        ),
        ("", b"\xff".as_slice(), 100, SearchHttpErrorKind::Malformed),
        (
            "Content-Encoding: gzip\r\n",
            b"{}".as_slice(),
            100,
            SearchHttpErrorKind::Malformed,
        ),
        ("", b"{}".as_slice(), 1, SearchHttpErrorKind::Oversize),
    ] {
        let (url, server) = fixture(200, headers, body, 0).await;
        let error = request(SearchEndpoint::Exa, url, limit)
            .send_json(&key(), tokio::time::Instant::now() + Duration::from_secs(1))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), kind);
        assert!(!format!("{error:?}").contains("canary"));
        server.await.unwrap();
    }
}

/// Local connection refusal and premature body EOF are transport interruptions,
/// not authentication/provider-body verdicts. Their errors keep no URL, key,
/// raw peer header/body, nested reqwest source or implicit retry decision.
#[tokio::test]
async fn search_http_transport_failure_category_is_secret_free() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/unavailable", listener.local_addr().unwrap());
    drop(listener);
    let error = request(SearchEndpoint::SerpApi, url, 4096)
        .send_json(&key(), tokio::time::Instant::now() + Duration::from_secs(1))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), SearchHttpErrorKind::Transport);
    assert_eq!(error.status(), None);
    assert!(!format!("{error:?}").contains("canary"));
    assert!(std::error::Error::source(&error).is_none());
    let (url, server) = fixture(200, "", b"{", 10).await;
    let error = request(SearchEndpoint::Brave, url, 4096)
        .send_json(&key(), tokio::time::Instant::now() + Duration::from_secs(1))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), SearchHttpErrorKind::Transport);
    assert!(std::error::Error::source(&error).is_none());
    server.await.unwrap();
}

/// Numeric non-success/pending status is authoritative even if the irrelevant
/// error body is compressed, invalid UTF-8, over cap or never reaches EOF.
/// Such a body must not erase authentication/payment/throttle/pending evidence
/// or become a remote diagnostic; legacy decoding rules remain separate.
#[tokio::test]
async fn search_http_status_precedes_untrusted_error_body() {
    for (status, kind) in [
        (401, SearchHttpErrorKind::Authentication),
        (402, SearchHttpErrorKind::Quota),
        (429, SearchHttpErrorKind::Throttle),
        (202, SearchHttpErrorKind::Pending),
    ] {
        for (headers, body, limit, missing) in [
            ("Content-Encoding: gzip\r\n", b"canary".as_slice(), 100, 0),
            ("", b"\xffcanary".as_slice(), 100, 0),
            ("", b"canary".as_slice(), 0, 0),
            ("", b"canary".as_slice(), 100, 10),
        ] {
            let (url, server) = fixture(status, headers, body, missing).await;
            let error = request(SearchEndpoint::SerpApi, url, limit)
                .send_json(&key(), tokio::time::Instant::now() + Duration::from_secs(1))
                .await
                .unwrap_err();
            assert_eq!(error.kind(), kind);
            assert_eq!(error.status(), Some(status));
            assert_eq!(error.message(), format!("search HTTP status {status}"));
            assert!(!format!("{error:?}").contains("canary"));
            assert!(std::error::Error::source(&error).is_none());
            server.await.unwrap();
        }
    }
}

/// A peer withholds every body byte until the local header-status result is
/// observed. Returning HeadersOnly must therefore precede body EOF, and dropping
/// the unread response closes its owned connection instead of draining forever.
#[tokio::test]
async fn search_http_header_status_finishes_before_withheld_body() {
    for status in [202, 401, 402, 429] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/withheld", listener.local_addr().unwrap());
        let (settled, observed) = tokio::sync::oneshot::channel::<()>();
        let server = async {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 4096];
            assert!(stream.read(&mut bytes).await.unwrap() > 0);
            stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Length: 100\r\nContent-Encoding: gzip\r\nX-Request-Id: canary\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
            // The peer never sends the promised body, even after this signal.
            observed.await.unwrap();
            assert_eq!(stream.read(&mut bytes).await.unwrap(), 0);
        };
        let client = async {
            let request = request(SearchEndpoint::SerpApi, url, 0);
            let credential = key();
            let exchange = ReqwestProviderHttpTransport
                .send_async_with_observers(
                    &request.request,
                    HttpExecution {
                        mode: HttpResponseMode::SearchJson,
                        routing: HttpRoutingPolicy::AuthenticatedSearch,
                        deadline: Some(tokio::time::Instant::now() + Duration::from_secs(1)),
                        auth: request.endpoint.auth(&credential),
                    },
                    None,
                    None,
                )
                .await
                .unwrap();
            assert_eq!(exchange.completion, HttpCompletion::HeadersOnly);
            assert_eq!(exchange.response.status_code, status);
            assert!(exchange.response.body.is_empty());
            assert!(exchange.response.headers.is_empty());
            let error = checked_json(exchange, &credential).unwrap_err();
            assert_eq!(error.status(), Some(status));
            settled.send(()).unwrap();
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(client, server)
        })
        .await
        .unwrap();
    }
    let error = checked_json(
        super::super::HttpExchangeResponse {
            completion: HttpCompletion::HeadersOnly,
            response: mez_agent::ProviderHttpResponse {
                status_code: 200,
                headers: Default::default(),
                body: "{}".into(),
            },
        },
        &key(),
    )
    .unwrap_err();
    assert_eq!(error.kind(), SearchHttpErrorKind::Malformed);
    assert_eq!(error.status(), Some(200));
}
