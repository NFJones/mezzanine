//! Fixed normalized observational hook bridge, not a vendor payload interpreter.
//!
//! Credentials arrive only on bounded stdin, never argv or emitted diagnostics.
//! The helper makes one restricted same-user Unix exchange without initialize,
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

/// Runs only the exact installed-helper argv before ordinary HOME/config/CPU
/// runtime discovery. Other CLI invocations keep their existing startup path.
/// Missing/invalid MEZ discovery or runtime failure drains bounded stdin and
/// emits neutral output; this mode cannot create a client role or default route.
pub(crate) fn run_internal_process(arguments: &[std::ffi::OsString]) -> Option<u8> {
    if arguments.len() != 2 || arguments[1] != "harness-event" {
        return None;
    }
    let mut stdout = std::io::stdout();
    let discovery = std::env::var_os("MEZ");
    if let Ok(Some(socket)) = super::env::socket_selection_from_mez(discovery.as_ref())
        && let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
    {
        return Some(u8::from(
            runtime.block_on(run(&socket, &mut stdout)).is_err(),
        ));
    }
    let _ = read_input();
    Some(u8::from(writeln!(stdout, "{{}}").is_err()))
}

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

/// Token-free selectors for one independently enrolled ordinary producer.
/// Private credentials are absent; original generation and observer witness
/// fence delayed callbacks/restarts and are never daemon authority.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HelperEvent {
    operation: String,
    harness: String,
    generation: u64,
    observer_witness: String,
    external_session_id: String,
    data: serde_json::Value,
}

/// Validates a fixed operation and its allowlisted fields before transport.
fn request(bytes: &[u8]) -> Result<Zeroizing<String>> {
    if bytes.len() > MAX_EVENT_BYTES {
        return Err(MezError::invalid_args("event unavailable"));
    }
    // Outgoing control validation cannot recover keys already overwritten by
    // Value decoding inside data/counters. Reject ambiguity at the raw boundary.
    let unique = crate::protocol::strict_json::decode(bytes)
        .map_err(|_| MezError::invalid_args("event unavailable"))?;
    if !unique.is_object() {
        return Err(MezError::invalid_args("event unavailable"));
    }
    let (operation, identity, data) = if unique.get("operation").and_then(serde_json::Value::as_str)
        == Some("helper-observe")
    {
        let event: HelperEvent = serde_json::from_value(unique)
            .map_err(|_| MezError::invalid_args("event unavailable"))?;
        (
            event.operation,
            serde_json::json!({"harness":event.harness,"generation":event.generation,"observer_witness":event.observer_witness,"external_session_id":event.external_session_id}),
            event.data,
        )
    } else {
        let event: Event = serde_json::from_value(unique)
            .map_err(|_| MezError::invalid_args("event unavailable"))?;
        (
            event.operation,
            serde_json::json!({"launch_token":event.launch_token,"generation":event.generation,"external_session_id":event.external_session_id}),
            event.data,
        )
    };
    let method = match operation.as_str() {
        "register" => "agent/external/register",
        "renew" => "agent/external/renew",
        "end" => "agent/external/deregister",
        "presentation" => "agent/external/presentation",
        "helper-presentation" => "agent/external/helper-presentation",
        "helper-observe" => "agent/external/helper-observe",
        "usage" => "agent/external/usage",
        _ => return Err(MezError::invalid_args("event unavailable")),
    };
    let mut params = data
        .as_object()
        .cloned()
        .ok_or_else(|| MezError::invalid_args("event unavailable"))?;
    if [
        "launch_token",
        "generation",
        "external_session_id",
        "harness",
        "observer_witness",
    ]
    .iter()
    .any(|key| params.contains_key(*key))
    {
        return Err(MezError::invalid_args("event unavailable"));
    }
    // Top-level field allowlisting alone cannot stop vendor content hidden in
    // a nominal counter or title field. Only counters may be an object.
    for (key, value) in &params {
        if key == "counters" && operation == "usage" {
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
    params.extend(
        identity
            .as_object()
            .ok_or_else(|| MezError::invalid_args("event unavailable"))?
            .clone(),
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
    exchange_result(socket, body, "harness-event")
        .await
        .map(|_| ())
}

/// Shared bounded same-user exchange; callers must separately project/validate
/// fixed typed results before exposure. Raw daemon errors/unknown fields are not
/// diagnostics. This performs no initialize, retry, process spawn or config I/O.
pub(super) async fn exchange_result(
    socket: &std::path::Path,
    body: &str,
    expected_id: &str,
) -> Result<serde_json::Value> {
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
            if let Ok((body, consumed)) =
                crate::control::decode_control_frame(&response, MAX_EVENT_BYTES)
            {
                if consumed != response.len() {
                    return Err(MezError::invalid_state("event reply unavailable"));
                }
                let value = crate::protocol::strict_json::decode(body.as_bytes())
                    .map_err(|_| MezError::invalid_state("event reply unavailable"))?;
                if value.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0")
                    || value.get("id").and_then(serde_json::Value::as_str) != Some(expected_id)
                    || !value
                        .get("result")
                        .is_some_and(serde_json::Value::is_object)
                    || value.get("error").is_some()
                {
                    return Err(MezError::invalid_state("event rejected"));
                }
                return Ok(value["result"].clone());
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

    /// The helper's internal exchange result must not accept overwritten reply
    /// IDs or result fields, simultaneous error/result, non-object results or
    /// buffered trailing frames/data. A real same-user Unix server receives one
    /// original request per case; failures must neither resend it nor echo the
    /// rejected body. A valid object acknowledgment remains accepted, without
    /// claiming that generic JSON-RPC success is a durable usage ledger receipt.
    #[tokio::test(flavor = "current_thread")]
    async fn harness_event_helper_rejects_ambiguous_acknowledgments_without_replay() {
        let root = std::env::temp_dir().join(format!(
            "mez-hook-reply-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let socket = root.join("control.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let event = serde_json::json!({"operation":"renew","launch_token":"x".repeat(43),"generation":1,"external_session_id":"run","data":{}});
        let request_body = request(event.to_string().as_bytes()).unwrap();
        for (body, trailing, accepted) in [
            (
                r#"{"jsonrpc":"2.0","id":"harness-event","result":{"renewed":true}}"#,
                false,
                true,
            ),
            (
                r#"{"jsonrpc":"2.0","id":"PRIVATE","\u0069d":"harness-event","result":{}}"#,
                false,
                false,
            ),
            (
                r#"{"jsonrpc":"2.0","id":"harness-event","result":{"sequence":1,"sequence":2}}"#,
                false,
                false,
            ),
            (
                r#"{"jsonrpc":"2.0","id":"harness-event","result":{},"error":{"message":"PRIVATE"}}"#,
                false,
                false,
            ),
            (r#"{"id":"harness-event","result":{}}"#, false, false),
            (
                r#"{"jsonrpc":"1.0","id":"harness-event","result":{}}"#,
                false,
                false,
            ),
            (
                r#"{"jsonrpc":"2.0","id":"harness-event","result":null}"#,
                false,
                false,
            ),
            (
                r#"{"jsonrpc":"2.0","id":"harness-event","result":{}}"#,
                true,
                false,
            ),
        ] {
            let server = async {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = [0; 4096];
                let mut input = Vec::new();
                let (received, consumed) = loop {
                    let count = stream.read(&mut bytes).await.unwrap();
                    assert_ne!(count, 0, "request frame unexpectedly closed");
                    input.extend_from_slice(&bytes[..count]);
                    assert!(input.len() <= MAX_EVENT_BYTES);
                    if let Ok(frame) = crate::control::decode_control_frame(&input, MAX_EVENT_BYTES)
                    {
                        break frame;
                    }
                };
                assert_eq!(consumed, input.len());
                assert_eq!(received, *request_body);
                let mut reply = crate::control::encode_control_body(body);
                if trailing {
                    reply.extend_from_slice(b"PRIVATE trailing data");
                }
                stream.write_all(&reply).await.unwrap();
                let count = stream.read(&mut bytes).await.unwrap();
                assert_eq!(
                    count, 0,
                    "rejected acknowledgment must not replay the request"
                );
            };
            let (result, ()) = tokio::join!(exchange(&socket, &request_body), server);
            assert_eq!(
                result.is_ok(),
                accepted,
                "helper acknowledgment classification differed"
            );
            if let Err(error) = result {
                assert!(!error.message().contains("PRIVATE"));
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Nested stdin objects must retain unique decoded membership before any
    /// canonicalization. Generic Value decoding would erase conflicting or even
    /// identical state/sequence/title/counter keys, including escaped aliases,
    /// before outgoing control validation could reject them. Diagnostics must
    /// not echo the private envelope or a hidden earlier field value.
    #[test]
    fn harness_event_helper_rejects_recursive_duplicate_aliases() {
        let token = "x".repeat(43);
        let binding =
            format!(r#""launch_token":"{token}","generation":1,"external_session_id":"run""#);
        let mut inputs = Vec::new();
        for data in [
            r#"{"sequence":1,"state":"running","state":"failed"}"#,
            r#"{"sequence":1,"state":"running","\u0073tate":"failed"}"#,
            r#"{"sequence":1,"sequence":1,"state":"running"}"#,
            r#"{"sequence":1,"state":"running","title":"PRIVATE","\u0074itle":"Task"}"#,
        ] {
            inputs.push(format!(
                r#"{{"operation":"presentation",{binding},"data":{data}}}"#
            ));
        }
        for counters in [
            r#"{"input_tokens":1,"input_tokens":2,"output_tokens":1}"#,
            r#"{"input_tokens":2,"\u0069nput_tokens":2,"output_tokens":1}"#,
            r#"{"input_tokens":2,"output_tokens":1,"cached_input_tokens":0,"cached_input_tokens":1}"#,
        ] {
            inputs.push(format!(r#"{{"operation":"usage",{binding},"data":{{"epoch":"epoch","event_id":"event","sequence":1,"mode":"delta","observed_at":"2026-10-01T00:00:00Z","provider":"fixture","model":"fixture","counters":{counters}}}}}"#));
        }
        inputs.push(format!(
            r#"{{"operation":"end","\u006fperation":"renew",{binding},"data":{{}}}}"#
        ));
        inputs.push(format!(
            r#"{{"operation":"renew",{binding},"\u0067eneration":1,"data":{{}}}}"#
        ));
        for input in inputs {
            let decoded = request(input.as_bytes());
            assert!(
                decoded.is_err(),
                "ambiguous helper input reached normalization"
            );
            let error = decoded.err().unwrap();
            assert!(!error.message().contains(&token));
            assert!(!error.message().contains("PRIVATE"));
            assert!(!error.message().contains("input_tokens"));
        }
    }

    /// Unique input retains ordinary Unicode and null/zero/absent counter
    /// semantics at the exact byte boundary. Trailing JSON, malformed input and
    /// excessive depth fail without leaking the envelope; the unique decoder
    /// does not interpret JSON-looking strings as nested objects.
    #[test]
    fn harness_event_helper_unique_input_preserves_bounds_and_values() {
        let token = "x".repeat(43);
        let event = serde_json::json!({"operation":"presentation","launch_token":token,"generation":1,
            "external_session_id":"run","data":{"sequence":1,"state":"running","title":"Task ✓"}});
        let mut bytes = event.to_string().into_bytes();
        bytes.resize(MAX_EVENT_BYTES, b' ');
        let body = request(&bytes).unwrap();
        let projected: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(projected["params"]["title"], "Task ✓");
        bytes.push(b' ');
        assert!(request(&bytes).is_err());

        let text = r#"{"state":"running","state":"failed"}"#;
        let mut opaque = event.clone();
        opaque["data"]["title"] = serde_json::json!(text);
        let body = request(opaque.to_string().as_bytes()).unwrap();
        let projected: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(projected["params"]["title"], text);

        let usage = serde_json::json!({"operation":"usage","launch_token":token,"generation":1,
            "external_session_id":"run","data":{"epoch":"epoch","event_id":"event","sequence":1,
                "mode":"delta","observed_at":"2026-10-01T00:00:00Z","provider":"fixture","model":"fixture",
                "counters":{"input_tokens":2,"output_tokens":1,"reasoning_tokens":null,"cached_input_tokens":0}}});
        let body = request(usage.to_string().as_bytes()).unwrap();
        let projected: serde_json::Value = serde_json::from_str(&body).unwrap();
        let counters = &projected["params"]["counters"];
        assert_eq!(counters["input_tokens"], 2);
        assert_eq!(counters["cached_input_tokens"], 0);
        assert!(counters["reasoning_tokens"].is_null());
        assert!(counters.get("cache_write_input_tokens").is_none());
        for input in [
            format!("{event} {{}}"),
            "{invalid PRIVATE}".to_string(),
            format!("{}0{}", "[".repeat(129), "]".repeat(129)),
        ] {
            let error = request(input.as_bytes()).err().unwrap();
            assert!(!error.message().contains(&token));
            assert!(!error.message().contains("PRIVATE"));
        }
    }

    /// The documented envelope is a named object, not a positional array that
    /// serde's derived struct sequence visitor could otherwise reinterpret as
    /// operation/credential/session fields. Other non-object roots reject too.
    #[test]
    fn harness_event_helper_requires_object_envelope() {
        let positional = serde_json::json!(["renew", "x".repeat(43), 1, "run", {}]);
        assert!(
            request(positional.to_string().as_bytes()).is_err(),
            "positional fields are not a fixed object envelope"
        );
        for input in [b"null".as_slice(), b"true", b"1", b"\"PRIVATE\""] {
            let error = request(input).err().unwrap();
            assert!(!error.message().contains("PRIVATE"));
        }
    }

    /// Fixed operation mapping refuses passthrough methods, credentials in data,
    /// unknown vendor content and oversized envelopes without echoing input.
    #[test]
    fn harness_event_helper_filters_normalized_fields() {
        let base = serde_json::json!({"operation":"presentation","launch_token":"x".repeat(43),"generation":1,"external_session_id":"run","data":{"sequence":1,"state":"running","title":"Task"}});
        let body = request(base.to_string().as_bytes()).unwrap();
        assert!(body.contains("agent/external/presentation"));
        assert!(!body.contains("control/initialize"));
        let mut helper = base.clone();
        helper["operation"] = serde_json::json!("helper-presentation");
        let body = request(helper.to_string().as_bytes()).unwrap();
        assert!(body.contains("agent/external/helper-presentation"));
        for field in ["pid", "parent_pid", "pane_id", "event", "objective"] {
            let mut bad = helper.clone();
            bad["data"][field] = serde_json::json!("forbidden selector");
            assert!(request(bad.to_string().as_bytes()).is_err());
        }
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

    /// Token-free callback selectors are not a permissive alternate format for
    /// existing credential operations. Mixed/null identity fields, overrides,
    /// duplicate keys and arbitrary process/control selectors never forward.
    #[test]
    fn harness_event_helper_observe_has_distinct_strict_identity() {
        let base = serde_json::json!({"operation":"helper-observe","harness":"pi","generation":1,"observer_witness":"b".repeat(64),"external_session_id":"run","data":{"sequence":1,"state":"running"}});
        let body = request(base.to_string().as_bytes()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["method"], "agent/external/helper-observe");
        assert!(value["params"].get("launch_token").is_none());
        assert_eq!(value["params"]["generation"], 1);
        for field in ["launch_token", "generation", "pid", "pane_id"] {
            let mut bad = base.clone();
            bad[field] = serde_json::Value::Null;
            assert!(request(bad.to_string().as_bytes()).is_err());
            let mut bad = base.clone();
            bad["data"][field] = serde_json::json!(1);
            assert!(request(bad.to_string().as_bytes()).is_err());
        }
        for field in ["harness", "external_session_id", "observer_witness"] {
            let mut bad = base.clone();
            bad["data"][field] = serde_json::json!("override");
            assert!(request(bad.to_string().as_bytes()).is_err());
        }
        let mut missing = base.clone();
        missing.as_object_mut().unwrap().remove("generation");
        assert!(request(missing.to_string().as_bytes()).is_err());
        let mut wrong = base;
        wrong["operation"] = serde_json::json!("renew");
        assert!(request(wrong.to_string().as_bytes()).is_err());
        assert!(request(br#"{"operation":"helper-observe","harness":"pi","harness":"opencode","external_session_id":"run","data":{"sequence":1,"state":"running"}}"#).is_err());
    }
}
