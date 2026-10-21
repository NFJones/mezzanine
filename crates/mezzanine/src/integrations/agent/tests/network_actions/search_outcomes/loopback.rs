//! Real ordinary HTTP completion/UTF-8 fixtures, exclusively on loopback.
//!
//! The test-only transport rewrites the fixed production URL to the listener
//! only after asserting the original DDG endpoint and resource contracts. It
//! cannot contact a public provider or select a different production backend.

use super::*;
use crate::integrations::http::ReqwestProviderHttpTransport;
use std::{future::Future, pin::Pin};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Reuses the production ordinary HTTP executor against one owned local peer.
struct LoopbackSearchTransport {
    url: String,
}

impl AsyncProviderHttpTransport for LoopbackSearchTransport {
    fn send_async<'a>(
        &'a self,
        request: &'a ProviderHttpRequest,
    ) -> Pin<
        Box<dyn Future<Output = mez_agent::ProviderHttpResult<ProviderHttpResponse>> + Send + 'a>,
    > {
        Box::pin(async move {
            let mut request = request.clone();
            request.url.clone_from(&self.url);
            ReqwestProviderHttpTransport.send_async(&request).await
        })
    }

    fn send_async_with_body_chunks<'a>(
        &'a self,
        request: &'a ProviderHttpRequest,
        observer: &'a mut (dyn FnMut(Vec<u8>) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send),
    ) -> Pin<
        Box<dyn Future<Output = mez_agent::ProviderHttpResult<HttpExchangeResponse>> + Send + 'a>,
    > {
        Box::pin(async move {
            assert!(request.url.starts_with("https://duckduckgo.com/html/?q="));
            assert_eq!(request.max_response_bytes, Some(1024 * 1024));
            assert_eq!(
                request.timeouts,
                mez_agent::ProviderHttpTimeouts::from_total(30_000)
            );
            let mut request = request.clone();
            request.url.clone_from(&self.url);
            ReqwestProviderHttpTransport
                .send_async_with_body_chunks(&request, observer)
                .await
        })
    }
}

/// A real capped body can end after usable progress, including in the middle
/// of a Unicode scalar. Invalid interior UTF-8 is a transport failure, not an
/// empty result. Both paths suppress HTML and preserve the original wire caps.
#[tokio::test]
async fn network_search_loopback_cap_and_invalid_utf8_never_become_empty_success() {
    let anchor = "<a class=\"result__a\" href=\"https://example.test/a\">First</a>";
    let capped = format!("{anchor}{}é", "x".repeat(1024 * 1024 - anchor.len() - 1)).into_bytes();
    for (body, expected_code) in [
        (capped, "network_search_unusable_response"),
        (
            b"<html>HTML_CANARY\xff</html>".to_vec(),
            "network_request_failed",
        ),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let transport = LoopbackSearchTransport {
            url: format!("http://{}/fixture", listener.local_addr().unwrap()),
        };
        let server = async {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert!(stream.read(&mut request).await.unwrap() > 0);
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(headers.as_bytes()).await.unwrap();
            // The bounded client may close as soon as its overflow byte arrives.
            let _ = stream.write_all(&body).await;
        };
        let action = search();
        let turn = turn();
        let mut progress = vec![];
        let mut observer = |source| progress.push(source);
        let execute = execute_network_action_with_transport_async_with_progress(
            &turn,
            &action,
            &transport,
            &mut observer,
        );
        let (result, ()) = tokio::join!(execute, server);
        let result = result.unwrap();
        assert_eq!(result.status, ActionStatus::Failed);
        assert_eq!(result.error.as_ref().unwrap().code, expected_code);
        assert!(!result.content_text().contains("No web results were found."));
        assert!(!action_result_context_content(&result).contains("HTML_CANARY"));
        assert!(
            progress
                .iter()
                .all(|source: &String| !source.contains('�') && !source.contains('<'))
        );
        if expected_code == "network_search_unusable_response" {
            assert!(progress.iter().any(|source| source.contains("1. First")));
            let data: serde_json::Value =
                serde_json::from_str(result.structured_content_json.as_deref().unwrap()).unwrap();
            assert_eq!(data["response"]["status_code"], 200);
            assert_eq!(data["response"]["html_truncated"], true);
            assert!(data["response"]["body_bytes"].as_u64().unwrap() <= 1024 * 1024);
        }
    }
}
