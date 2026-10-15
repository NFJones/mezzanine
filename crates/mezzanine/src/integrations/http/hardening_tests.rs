//! Local completion, policy-key and observer lifecycle regression coverage.

use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Prepares a legacy ordinary exchange without environment/config mutation.
fn request(url: String, limit: Option<usize>) -> ProviderHttpRequest {
    ProviderHttpRequest {
        method: "GET".into(),
        url,
        headers: BTreeMap::new(),
        body: String::new(),
        timeouts: ProviderHttpTimeouts::from_total(1_000),
        max_response_bytes: limit,
    }
}

/// Serves exact bytes, including arbitrary reserved peer metadata.
async fn fixture(body: &[u8], headers: &str) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/ordinary", listener.local_addr().unwrap());
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
        body.len()
    );
    let body = body.to_vec();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 4096];
        assert!(stream.read(&mut buffer).await.unwrap() > 0);
        stream.write_all(response.as_bytes()).await.unwrap();
        stream.write_all(&body).await.unwrap();
    });
    (url, server)
}

/// Zero/exact/overflow caps and multibyte boundaries use local evidence, not
/// hostile headers. Invalid interior UTF-8 fails even when a cap was exceeded.
#[tokio::test]
async fn http_ordinary_completion_and_utf8_are_local() {
    for (body, cap, expected, completion) in [
        ("", 0, "", HttpCompletion::Complete),
        ("a", 0, "", HttpCompletion::Truncated),
        ("abc", 3, "abc", HttpCompletion::Complete),
        ("abcd", 3, "abc", HttpCompletion::Truncated),
        ("aéz", 2, "a", HttpCompletion::Truncated),
        ("aéz", 3, "aé", HttpCompletion::Truncated),
    ] {
        let (url, server) = fixture(body.as_bytes(), "X-Mez-Body-Truncated: true\r\nX-Mez-Stream-Decoded: incremental\r\nContent-Type: text/event-stream\r\n").await;
        let request = request(url, Some(cap));
        let response = ReqwestProviderHttpTransport
            .send_async_with_observers(
                &request,
                HttpExecution::legacy(HttpResponseMode::Ordinary),
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(response.completion, completion);
        assert_eq!(response.response.body, expected);
        assert!(
            response
                .response
                .headers
                .keys()
                .all(|name| !name.starts_with("x-mez-"))
        );
        server.await.unwrap();
    }
    for cap in [None, Some(2), Some(3)] {
        let (url, server) = fixture(b"a\xffzz", "").await;
        let request = request(url, cap);
        let error = ReqwestProviderHttpTransport
            .send_async_with_observers(
                &request,
                HttpExecution::legacy(HttpResponseMode::Ordinary),
                None,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(error.message(), "provider HTTP response body is not UTF-8");
        server.await.unwrap();
    }
}

/// A capped successful inference stream cannot masquerade as terminal-decoded
/// completion after a complete but nonterminal event. Ordinary byte caps are
/// not permission to return incomplete inference SSE.
#[tokio::test]
async fn http_capped_inference_sse_never_succeeds_before_terminal() {
    let body =
        b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"a\"}\n\ndata: [DONE]\n\n";
    let (url, server) = fixture(body, "Content-Type: text/event-stream\r\n").await;
    let request = request(url, Some(body.len() - 10));
    let error = ReqwestProviderHttpTransport
        .send_async(&request)
        .await
        .unwrap_err();
    assert_eq!(
        error.message(),
        "provider HTTP response exceeds configured limit"
    );
    server.await.unwrap();
}

/// All variable construction dimensions are represented by the key. Fixed
/// pool limits and identity compression apply to every bundle. Credentials,
/// endpoint query, body, response mode and caller deadline are never key input.
#[test]
fn http_client_policy_keys_separate_every_construction_bundle() {
    let mut policies = std::collections::BTreeSet::new();
    for routing in [
        HttpRoutingPolicy::Legacy,
        HttpRoutingPolicy::AuthenticatedSearch,
    ] {
        for plaintext_http in [false, true] {
            for connect_timeout_ms in [100, 200] {
                assert!(policies.insert(ProviderHttpClientPolicy {
                    plaintext_http,
                    connect_timeout_ms,
                    routing
                }));
            }
        }
    }
    assert_eq!(policies.len(), 8);
}

/// Records when a pending observer future is dropped rather than detached.
struct ObserverDrop(Arc<AtomicUsize>);
impl Drop for ObserverDrop {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// A pending rendering future cannot hold the body reader or successful EOF.
/// Its scoped future is dropped before return, with no detached work surviving.
#[tokio::test]
async fn http_pending_progress_does_not_block_body_and_is_dropped() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let request = request(
        format!("http://{}/observer", listener.local_addr().unwrap()),
        Some(20),
    );
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 4096];
        assert!(stream.read(&mut buffer).await.unwrap() > 0);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nc")
            .await
            .unwrap();
        started_rx.await.unwrap();
        stream.write_all(b"omplete").await.unwrap();
    });
    let dropped = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(AtomicUsize::new(0));
    let mut started_tx = Some(started_tx);
    let mut observer = |_: Vec<u8>| {
        started.fetch_add(1, Ordering::SeqCst);
        started_tx.take().unwrap().send(()).unwrap();
        let guard = ObserverDrop(dropped.clone());
        Box::pin(async move {
            let _guard = guard;
            std::future::pending::<()>().await;
        }) as Pin<Box<dyn Future<Output = ()> + Send>>
    };
    let result = tokio::time::timeout(
        Duration::from_millis(500),
        ReqwestProviderHttpTransport.send_async_with_body_chunks(&request, &mut observer),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.response.body, "complete");
    assert_eq!(result.completion, HttpCompletion::Complete);
    assert_eq!(started.load(Ordering::SeqCst), 1);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    server.await.unwrap();
}

/// Caller cancellation drops the in-flight body and the tracked observer;
/// loopback EOF proves no request/body worker was detached after cancellation.
#[tokio::test]
async fn http_cancellation_drops_body_and_progress_future() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let request = request(
        format!("http://{}/cancel", listener.local_addr().unwrap()),
        Some(20),
    );
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = [0; 4096];
        assert!(stream.read(&mut bytes).await.unwrap() > 0);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\na")
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), stream.read(&mut bytes))
            .await
            .unwrap()
            .unwrap()
    });
    let dropped = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(AtomicUsize::new(0));
    let mut observer = |_: Vec<u8>| {
        started.fetch_add(1, Ordering::SeqCst);
        let guard = ObserverDrop(dropped.clone());
        Box::pin(async move {
            let _guard = guard;
            std::future::pending::<()>().await;
        }) as Pin<Box<dyn Future<Output = ()> + Send>>
    };
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            ReqwestProviderHttpTransport.send_async_with_body_chunks(&request, &mut observer)
        )
        .await
        .is_err()
    );
    assert_eq!(started.load(Ordering::SeqCst), 1);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert_eq!(server.await.unwrap(), 0);
}
