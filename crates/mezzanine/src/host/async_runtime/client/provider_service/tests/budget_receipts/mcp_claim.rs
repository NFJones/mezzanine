//! Physical HTTP MCP endpoint for native budget claim acceptance. Discovery and
//! tool calls use the actual configured transport. Request capability is obtained
//! from the real runtime metadata-get result, never a fabricated tool manifest.

use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(super) struct Fixture {
    pub(super) url: String,
    pub(super) calls: Arc<AtomicUsize>,
    discovered: Arc<AtomicUsize>,
    server: Option<tokio::task::JoinHandle<()>>,
}

impl Fixture {
    pub(super) async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let discovered = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let lists = discovered.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let request = loop {
                    let mut chunk = [0; 4096];
                    let n = socket.read(&mut chunk).await.unwrap();
                    assert_ne!(n, 0, "MCP request closed before its complete body");
                    bytes.extend_from_slice(&chunk[..n]);
                    assert!(bytes.len() < 1024 * 1024);
                    if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                        let length: usize = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:").map(str::trim))
                            .unwrap()
                            .parse()
                            .unwrap();
                        if bytes.len() >= end + 4 + length {
                            break serde_json::from_slice::<serde_json::Value>(
                                &bytes[end + 4..end + 4 + length],
                            )
                            .unwrap();
                        }
                    }
                };
                let result = match request["method"].as_str().unwrap() {
                    "initialize" => {
                        serde_json::json!({"protocolVersion":"2025-11-25", "capabilities":{"tools":{}}, "serverInfo":{"name":"budget-fixture","version":"1"}})
                    }
                    "notifications/initialized" => {
                        socket.write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                        continue;
                    }
                    "tools/list" => {
                        lists.fetch_add(1, Ordering::SeqCst);
                        serde_json::json!({"tools":[{"name":"echo","description":"Return original fixture text", "inputSchema":{"type":"object","properties":{"message":{"type":"string"}},"required":["message"]}}]})
                    }
                    "tools/call" => {
                        assert_eq!(request["params"]["name"], "echo");
                        assert_eq!(
                            request["params"]["arguments"],
                            serde_json::json!({"message":"original"})
                        );
                        count.fetch_add(1, Ordering::SeqCst);
                        serde_json::json!({"content":[{"type":"text","text":"original MCP result"}],"isError":false})
                    }
                    method => panic!("unexpected MCP fixture method: {method}"),
                };
                let body =
                    serde_json::json!({"jsonrpc":"2.0", "id":request["id"], "result":result})
                        .to_string();
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
        });
        Self {
            url,
            calls,
            discovered,
            server: Some(server),
        }
    }

    pub(super) async fn stop(mut self) -> crate::Result<()> {
        let server = self.server.take().unwrap();
        server.abort();
        match server.await {
            Err(error) if error.is_cancelled() => {
                if self.discovered.load(Ordering::SeqCst) == 0 {
                    return Err(MezError::invalid_state(
                        "MCP fixture was never physically discovered",
                    ));
                }
                Ok(())
            }
            other => Err(MezError::invalid_state(format!(
                "MCP fixture did not retire cleanly: {other:?}"
            ))),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Setup/unwind safety only; successful acceptance explicitly joins above.
        if let Some(server) = &self.server {
            server.abort();
        }
    }
}
