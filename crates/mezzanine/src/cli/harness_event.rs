//! Fixed normalized observational hook bridge, not a vendor payload interpreter.
//!
//! Credentials arrive only on bounded stdin, never argv or emitted diagnostics.
//! The helper makes one capability-only same-user Unix exchange without initialize,
//! retries, subprocesses or general RPC passthrough. Errors lose telemetry and
//! return neutral output. Vendor adapters own normalization and neutral-response
//! compatibility; this helper does not certify a vendor or read its transcripts.

use super::{MezError, Result, SocketSelection, Write};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zeroize::Zeroizing;

const MAX_EVENT_BYTES: usize = 64 * 1024;
const INPUT_DEADLINE: Duration = Duration::from_millis(250);
const EXCHANGE_DEADLINE: Duration = Duration::from_millis(500);

/// Bounded normalized envelope. Arbitrary upstream payloads are never forwarded.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Event {
    operation: String,
    launch_token: String,
    generation: u64,
    external_session_id: String,
    data: serde_json::Value,
}

/// Validates a fixed operation and its allowlisted fields before transport.
fn request(bytes: &[u8]) -> Result<Zeroizing<String>> {
    if bytes.len() > MAX_EVENT_BYTES {
        return Err(MezError::invalid_args("event unavailable"));
    }
    let event: Event =
        serde_json::from_slice(bytes).map_err(|_| MezError::invalid_args("event unavailable"))?;
    let method = match event.operation.as_str() {
        "register" => "agent/external/register",
        "renew" => "agent/external/renew",
        "end" => "agent/external/deregister",
        "presentation" => "agent/external/presentation",
        "usage" => "agent/external/usage",
        _ => return Err(MezError::invalid_args("event unavailable")),
    };
    let mut params = event
        .data
        .as_object()
        .cloned()
        .ok_or_else(|| MezError::invalid_args("event unavailable"))?;
    if ["launch_token", "generation", "external_session_id"]
        .iter()
        .any(|key| params.contains_key(*key))
    {
        return Err(MezError::invalid_args("event unavailable"));
    }
    // Top-level field allowlisting alone cannot stop vendor content hidden in
    // a nominal counter or title field. Only counters may be an object.
    for (key, value) in &params {
        if key == "counters" && event.operation == "usage" {
            let counters: crate::storage::token_usage::ExternalCounters =
                serde_json::from_value(value.clone())
                    .map_err(|_| MezError::invalid_args("event unavailable"))?;
            counters.validate()?;
        } else if !matches!(
            value,
            serde_json::Value::Null
                | serde_json::Value::Bool(_)
                | serde_json::Value::Number(_)
                | serde_json::Value::String(_)
        ) {
            return Err(MezError::invalid_args("event unavailable"));
        }
    }
    params.insert("launch_token".into(), event.launch_token.into());
    params.insert("generation".into(), event.generation.into());
    params.insert(
        "external_session_id".into(),
        event.external_session_id.into(),
    );
    let body = Zeroizing::new(
        serde_json::json!({"jsonrpc":"2.0", "id":"harness-event", "method":method,"params":params})
            .to_string(),
    );
    let parsed = crate::control::parse_json_rpc_request(&body)?;
    crate::control::validate_control_method_params_schema(&parsed)?;
    Ok(body)
}

/// Reads stdin to EOF with byte and wall-clock limits, without buffered read-ahead.
fn read_input() -> Result<Zeroizing<Vec<u8>>> {
    let stdin = std::io::stdin();
    read_input_from(&stdin, INPUT_DEADLINE)
}

/// Reads an owned input descriptor under the same bounded hook input contract.
pub(super) fn read_input_from(
    input: &impl std::os::fd::AsFd,
    limit: Duration,
) -> Result<Zeroizing<Vec<u8>>> {
    let deadline = Instant::now() + limit;
    let mut bytes = Zeroizing::new(Vec::new());
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| MezError::invalid_state("event input deadline"))?;
        let timeout = rustix::event::Timespec::try_from(remaining)
            .map_err(|_| MezError::invalid_state("event input deadline"))?;
        let mut fds = [rustix::event::PollFd::new(
            input,
            rustix::event::PollFlags::IN,
        )];
        match rustix::event::poll(&mut fds, Some(&timeout)) {
            Ok(0) => return Err(MezError::invalid_state("event input deadline")),
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => return Err(std::io::Error::from(error).into()),
            Ok(_) => {}
        }
        let mut buffer = [0; 4096];
        let count = rustix::io::read(input, &mut buffer).map_err(std::io::Error::from)?;
        if count == 0 {
            return Ok(bytes);
        }
        if bytes.len().saturating_add(count) > MAX_EVENT_BYTES {
            return Err(MezError::invalid_args("event unavailable"));
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
}

/// Exchanges one frame with a same-user Unix daemon under a single total deadline.
async fn exchange(socket: &std::path::Path, body: &str) -> Result<()> {
    tokio::time::timeout(EXCHANGE_DEADLINE, async {
        let mut stream = tokio::net::UnixStream::connect(socket).await?;
        crate::runtime::authenticated_unix_peer_uid(
            stream.as_raw_fd(),
            crate::runtime::current_effective_uid(),
        )?;
        let frame = Zeroizing::new(crate::control::encode_control_body(body));
        stream.write_all(&frame).await?;
        let mut response = Zeroizing::new(Vec::new());
        let mut buffer = [0; 4096];
        loop {
            let count = stream.read(&mut buffer).await?;
            if count == 0 {
                return Err(MezError::invalid_state("event reply unavailable"));
            }
            response.extend_from_slice(&buffer[..count]);
            if response.len() > MAX_EVENT_BYTES {
                return Err(MezError::invalid_state("event reply unavailable"));
            }
            if let Ok((body, _)) = crate::control::decode_control_frame(&response, MAX_EVENT_BYTES)
            {
                let value: serde_json::Value = serde_json::from_str(&body)
                    .map_err(|_| MezError::invalid_state("event reply unavailable"))?;
                if value.get("id").and_then(serde_json::Value::as_str) != Some("harness-event")
                    || value.get("result").is_none()
                {
                    return Err(MezError::invalid_state("event rejected"));
                }
                return Ok(());
            }
        }
    })
    .await
    .map_err(|_| MezError::invalid_state("event exchange deadline"))?
}

/// Forwards at most one event and always emits neutral JSON, suppressing secrets/errors.
pub(super) async fn run<W: Write>(socket: &SocketSelection, stdout: &mut W) -> Result<()> {
    if let Ok(bytes) = read_input()
        && let Ok(body) = request(&bytes)
    {
        let _ = exchange(super::selected_socket_path(socket), &body).await;
    }
    writeln!(stdout, "{{}}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// EOF completes bounded input, but a silent or slow producer cannot extend
    /// the total deadline; oversized input fails before normalized forwarding.
    #[test]
    fn harness_event_helper_input_has_total_deadline_and_byte_limit() {
        use std::io::Write;
        let (input, mut producer) = std::os::unix::net::UnixStream::pair().unwrap();
        producer.write_all(b"{}").unwrap();
        drop(producer);
        assert_eq!(
            &*read_input_from(&input, Duration::from_secs(1)).unwrap(),
            b"{}"
        );
        let (input, _silent) = std::os::unix::net::UnixStream::pair().unwrap();
        assert!(read_input_from(&input, Duration::from_millis(30)).is_err());
        let (input, mut producer) = std::os::unix::net::UnixStream::pair().unwrap();
        let writer = std::thread::spawn(move || {
            for _ in 0..10 {
                if producer.write_all(b"x").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        assert!(read_input_from(&input, Duration::from_millis(30)).is_err());
        drop(input);
        writer.join().unwrap();
        let (input, mut producer) = std::os::unix::net::UnixStream::pair().unwrap();
        let writer = std::thread::spawn(move || {
            let _ = producer.write_all(&vec![b'x'; MAX_EVENT_BYTES + 1]);
        });
        assert!(read_input_from(&input, Duration::from_secs(1)).is_err());
        drop(input);
        writer.join().unwrap();
    }

    /// A real same-user Unix exchange sends exactly one restricted frame, never
    /// initializes a client, and terminates stalled replies within its deadline.
    #[tokio::test(flavor = "current_thread")]
    async fn harness_event_helper_uses_one_bounded_capability_frame() {
        let root = std::env::temp_dir().join(format!(
            "mez-hook-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let socket = root.join("control.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let event = serde_json::json!({"operation":"renew","launch_token":"x".repeat(43),"generation":1,"external_session_id":"run","data":{}});
        let body = request(event.to_string().as_bytes()).unwrap();
        let server = async {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = vec![0; MAX_EVENT_BYTES];
            let count = stream.read(&mut bytes).await.unwrap();
            let (body, consumed) =
                crate::control::decode_control_frame(&bytes[..count], MAX_EVENT_BYTES).unwrap();
            assert_eq!(consumed, count);
            assert!(!body.contains("control/initialize"));
            assert!(body.contains("agent/external/renew"));
            stream
                .write_all(&crate::control::encode_control_body(
                    r#"{"jsonrpc":"2.0","id":"harness-event","result":{}}"#,
                ))
                .await
                .unwrap();
        };
        let (result, ()) = tokio::join!(exchange(&socket, &body), server);
        result.unwrap();
        let stalled = async {
            let (_stream, _) = listener.accept().await.unwrap();
            tokio::time::sleep(EXCHANGE_DEADLINE + Duration::from_millis(100)).await;
        };
        let (result, ()) = tokio::join!(exchange(&socket, &body), stalled);
        assert!(result.is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Fixed operation mapping refuses passthrough methods, credentials in data,
    /// unknown vendor content and oversized envelopes without echoing input.
    #[test]
    fn harness_event_helper_filters_normalized_fields() {
        let base = serde_json::json!({"operation":"presentation","launch_token":"x".repeat(43),"generation":1,"external_session_id":"run","data":{"sequence":1,"state":"running","title":"Task"}});
        let body = request(base.to_string().as_bytes()).unwrap();
        assert!(body.contains("agent/external/presentation"));
        assert!(!body.contains("control/initialize"));
        for (field, value) in [
            ("prompt", serde_json::json!("SECRET")),
            ("pane_id", serde_json::json!("%2")),
            ("launch_token", serde_json::json!("other")),
            ("title", serde_json::json!({"prompt":"SECRET"})),
        ] {
            let mut bad = base.clone();
            bad["data"][field] = value;
            assert!(request(bad.to_string().as_bytes()).is_err());
        }
        let mut bad = base;
        bad["operation"] = serde_json::json!("terminal/command");
        assert!(request(bad.to_string().as_bytes()).is_err());
        assert!(request(&vec![b'x'; MAX_EVENT_BYTES + 1]).is_err());
    }
}
