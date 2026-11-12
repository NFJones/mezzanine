//! Physical exchange counts at the production inference/fetch transport boundary.
//! All listeners are owned loopback fixtures; no provider or configuration is used.

use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Read the whole small request so closing the fixture cannot reset an unread
/// POST body and accidentally make a redirect appear to have been refused.
async fn read_request(stream: &mut tokio::net::TcpStream) {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let read = stream.read(&mut buffer).await.unwrap();
        assert!(read > 0);
        bytes.extend_from_slice(&buffer[..read]);
        assert!(bytes.len() < 16 * 1024);
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&bytes[..end]).unwrap();
            let length = headers
                .lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length {
                return;
            }
        }
    }
}

/// Inference must return the original redirect without a second submission.
/// Both unary and incremental entrypoints share that rule. Ordinary fetch
/// retains its established redirects, even when warming the same policy cache
/// before or after inference; a global redirect-policy change is not the fix.
#[tokio::test]
async fn inference_redirects_never_issue_hidden_exchanges() {
    for reverse in [false, true] {
        for stream in [false, true] {
            for status in [301, 302, 303, 307, 308] {
                for inference in if reverse {
                    [true, false]
                } else {
                    [false, true]
                } {
                    let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    let target_url = format!("http://{}/target", target.local_addr().unwrap());
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    let url = format!("http://{}/source", listener.local_addr().unwrap());
                    let source = tokio::spawn(async move {
                        let (mut socket, _) = listener.accept().await.unwrap();
                        read_request(&mut socket).await;
                        socket.write_all(format!(
                            "HTTP/1.1 {status} Redirect\r\nLocation: {target_url}\r\nContent-Length: 2\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{{}}"
                        ).as_bytes()).await.unwrap();
                    });
                    let target = tokio::spawn(async move {
                        match tokio::time::timeout(Duration::from_millis(150), target.accept())
                            .await
                        {
                            Ok(Ok((mut socket, _))) => {
                                read_request(&mut socket).await;
                                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}").await.unwrap();
                                1
                            }
                            Err(_) => 0,
                            Ok(Err(error)) => panic!("target fixture: {error}"),
                        }
                    });
                    let request = ProviderHttpRequest {
                        method: "POST".into(),
                        url,
                        headers: BTreeMap::new(),
                        body: "{}".into(),
                        timeouts: ProviderHttpTimeouts::from_total(1_000),
                        max_response_bytes: Some(4096),
                    };
                    let response = if !inference {
                        let mut observer =
                            |_| Box::pin(async {}) as Pin<Box<dyn Future<Output = ()> + Send>>;
                        ReqwestProviderHttpTransport
                            .send_async_with_body_chunks(&request, &mut observer)
                            .await
                            .unwrap()
                            .response
                    } else if stream {
                        let mut observer =
                            |_| Box::pin(async {}) as Pin<Box<dyn Future<Output = ()> + Send>>;
                        ReqwestProviderHttpTransport
                            .send_async_with_sse_events(&request, &mut observer)
                            .await
                            .unwrap()
                    } else {
                        ReqwestProviderHttpTransport
                            .send_async(&request)
                            .await
                            .unwrap()
                    };
                    source.await.unwrap();
                    let physical_extra = target.await.unwrap();
                    assert_eq!(
                        physical_extra,
                        usize::from(!inference),
                        "status={status} inference={inference} stream={stream}"
                    );
                    assert_eq!(response.status_code, if inference { status } else { 200 });
                }
            }
        }
    }
}
