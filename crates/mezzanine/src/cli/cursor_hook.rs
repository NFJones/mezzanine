//! Native Cursor interactive command-hook projection, never IDE schema attestation.
//!
//! CLI changelog and vendor support confirm conversation/generation correlation
//! for interactive hooks. Session labels are only selectors; the daemon proves
//! native writer/direct creator/root. Non-interactive generation fallbacks,
//! children, content and unknown counters never become lifecycle/usage authority.
//! Strict64KiB/250ms input and one bounded local exchange remain neutral on loss.

use super::{Args, MezError, Result, SocketSelection, Write};
use std::time::{Duration, Instant};

/// Fixed non-permission topics; no general control selector can enter argv.
const EVENTS: &[&str] = &[
    "sessionStart",
    "beforeSubmitPrompt",
    "postToolUse",
    "preCompact",
    "afterAgentResponse",
    "stop",
    "sessionEnd",
];

/// Code-owned event argv cannot supply source, session or native authority.
#[derive(Debug, Clone, Args)]
pub(super) struct CursorHookArgs {
    /// Supported native topic, not a vendor decision or caller-selected RPC.
    #[arg(value_parser = ["sessionStart", "beforeSubmitPrompt", "postToolUse", "preCompact", "afterAgentResponse", "stop", "sessionEnd"])]
    pub(super) event: String,
}

/// Fixed exact argv runs before HOME/config/CPU startup, without vendor launch.
pub(crate) fn run_internal_process(arguments: &[std::ffi::OsString]) -> Option<u8> {
    if arguments.len() != 3
        || arguments[1] != "cursor-hook"
        || !arguments[2]
            .to_str()
            .is_some_and(|event| EVENTS.contains(&event))
    {
        return None;
    }
    let event = arguments[2].to_str()?;
    let mut stdout = std::io::stdout();
    let discovery = std::env::var_os("MEZ");
    if let Ok(Some(socket)) = super::env::socket_selection_from_mez(discovery.as_ref())
        && let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
    {
        let pane = std::env::var("MEZ_PANE").ok();
        return Some(u8::from(
            runtime
                .block_on(run(event, &socket, pane.as_deref(), &mut stdout))
                .is_err(),
        ));
    }
    let _ = read_input_from(&std::io::stdin());
    Some(u8::from(writeln!(stdout, "{{}}").is_err()))
}

/// Zeroized temporary content under a strict byte/deadline limit; full-cap input
/// is unavailable without an extra EOF read. No payload file or transcript opens.
fn read_input_from(input: &impl std::os::fd::AsFd) -> Result<zeroize::Zeroizing<Vec<u8>>> {
    let deadline = Instant::now() + Duration::from_millis(250);
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    let mut buffer = zeroize::Zeroizing::new([0u8; 4096]);
    while bytes.len() < 65_536 {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| MezError::invalid_state("Cursor input unavailable"))?;
        let timeout = rustix::event::Timespec::try_from(remaining)
            .map_err(|_| MezError::invalid_state("Cursor input unavailable"))?;
        let mut fds = [rustix::event::PollFd::new(
            input,
            rustix::event::PollFlags::IN,
        )];
        match rustix::event::poll(&mut fds, Some(&timeout)) {
            Ok(0) => return Err(MezError::invalid_state("Cursor input unavailable")),
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => return Err(std::io::Error::from(error).into()),
            Ok(_) => {}
        }
        let length = buffer.len().min(65_536 - bytes.len());
        let count = match rustix::io::read(input, &mut buffer[..length]) {
            Err(rustix::io::Errno::INTR) => continue,
            result => result.map_err(std::io::Error::from)?,
        };
        if count == 0 {
            return Ok(bytes);
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    Err(MezError::invalid_args("Cursor input unavailable"))
}

/// Only bounded inert IDs survive projection; errors never include vendor text.
fn identifier(value: &serde_json::Value, key: &str) -> Result<String> {
    value[key]
        .as_str()
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 128
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
        })
        .map(str::to_owned)
        .ok_or_else(|| MezError::invalid_args("Cursor identity unavailable"))
}

/// Builds exact native declaration/observation metadata, not counters or context.
/// Common fields are corroborated for CLI by vendor support, not assumed from IDE.
fn request(event: &str, bytes: &[u8], pane: &str) -> Result<Option<String>> {
    if bytes.len() >= 65_536
        || !EVENTS.contains(&event)
        || pane.len() < 2
        || pane.len() > 64
        || !pane.starts_with('%')
        || !pane[1..].bytes().all(|b| b.is_ascii_digit())
    {
        return Err(MezError::invalid_args("Cursor observation unavailable"));
    }
    let value = crate::protocol::strict_json::decode(bytes)
        .map_err(|_| MezError::invalid_args("Cursor observation unavailable"))?;
    if value["hook_event_name"] != event {
        return Err(MezError::invalid_args("Cursor event unavailable"));
    }
    if [
        "subagent_id",
        "parent_conversation_id",
        "agent_id",
        "subagent_type",
    ]
    .iter()
    .any(|key| value.get(key).is_some())
    {
        return Ok(None);
    }
    let session = identifier(&value, "conversation_id")?;
    let (method, params) = if event == "sessionStart" {
        if identifier(&value, "session_id")? != session {
            return Err(MezError::invalid_args(
                "Cursor session association unavailable",
            ));
        }
        (
            "agent/external/curated-enroll",
            serde_json::json!({"pane_id":pane,"harness":"cursor","version":"best-effort","display_name":"Cursor","external_session_id":session,"observer_kind":"command-hook","observer_instance":"cursor-local-hooks-1","source_contract":"cursor-local-command/1","session_boundary":"session-start"}),
        )
    } else {
        let mut params =
            serde_json::json!({"pane_id":pane,"external_session_id":session,"event":event});
        if event == "sessionEnd" {
            if identifier(&value, "session_id")? != session
                || !value["reason"].as_str().is_some_and(|r| {
                    matches!(
                        r,
                        "completed" | "aborted" | "error" | "window_close" | "user_close"
                    )
                })
            {
                return Ok(None);
            }
        } else {
            let generation = identifier(&value, "generation_id")?;
            // Vendor support describes this non-interactive fallback as not a
            // real turn identity. Do not invent a suffix/timestamp to repair it.
            if generation == session {
                return Ok(None);
            }
            params["generation_id"] = generation.into();
            if event == "stop" {
                let status = value["status"]
                    .as_str()
                    .filter(|s| matches!(*s, "completed" | "aborted" | "error"))
                    .ok_or_else(|| MezError::invalid_args("Cursor stop unavailable"))?;
                if !value["loop_count"]
                    .as_u64()
                    .is_some_and(|n| n <= 9_007_199_254_740_991)
                {
                    return Ok(None);
                }
                params["status"] = status.into();
            }
        }
        ("agent/external/cursor-observe", params)
    };
    let body =
        serde_json::json!({"jsonrpc":"2.0","id":"cursor-hook","method":method,"params":params})
            .to_string();
    crate::control::validate_control_method_params_schema(
        &crate::control::parse_json_rpc_request(&body)?,
    )?;
    Ok(Some(body))
}

/// At most one same-user bounded exchange; only required output write may fail.
pub(super) async fn run<W: Write>(
    event: &str,
    socket: &SocketSelection,
    pane: Option<&str>,
    stdout: &mut W,
) -> Result<()> {
    if let Ok(bytes) = read_input_from(&std::io::stdin())
        && let Some(pane) = pane
        && let Ok(Some(body)) = request(event, &bytes, pane)
    {
        let _ = super::harness_event::exchange_result(
            super::selected_socket_path(socket),
            &body,
            "cursor-hook",
        )
        .await;
    }
    writeln!(stdout, "{{}}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Exact argv is not an arbitrary permission/control or general CLI bypass.
    #[test]
    fn cursor_hook_exact_argv_rejects_unknown_control_topics() {
        for argv in [
            vec!["mez", "cursor-hook"],
            vec!["mez", "cursor-hook", "preToolUse"],
            vec!["mez", "cursor-hook", "sessionStart", "extra"],
            vec!["mez", "--json", "cursor-hook", "stop"],
        ] {
            assert_eq!(
                run_internal_process(
                    &argv
                        .into_iter()
                        .map(std::ffi::OsString::from)
                        .collect::<Vec<_>>()
                ),
                None
            );
        }
    }
    /// Projected CLI fixtures carry only inert IDs/enums. IDE-only session data,
    /// ambiguous identities and non-interactive fallback generations are not
    /// inferred as supported interactive turns; no guessed usage field forwards.
    #[test]
    fn cursor_hook_projection_is_neutral_private_and_cli_specific() {
        for event in EVENTS {
            let bytes = serde_json::json!({"hook_event_name":event,"session_id":"session-a","conversation_id":"session-a","generation_id":"turn-a","status":"completed","loop_count":0,"reason":"user_close","prompt":"PRIVATE","text":"PRIVATE","transcript_path":"PRIVATE","user_email":"PRIVATE","workspace_roots":["PRIVATE"],"usage":{"input_tokens":99},"context_tokens":5000}).to_string();
            let body = request(event, bytes.as_bytes(), "%1").unwrap().unwrap();
            assert!(!body.contains("PRIVATE"));
            assert!(!body.contains("tokens"));
            let value: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(value["params"]["external_session_id"], "session-a");
        }
        for bytes in [br#"{"hook_event_name":"sessionStart","session_id":"s","composer_mode":"agent"}"#.as_slice(),br#"{"hook_event_name":"sessionStart","session_id":"s","conversation_id":"s","conversation_id":"other"}"#,br#"{"hook_event_name":"sessionStart","session_id":"s","conversation_id":"other"}"#] { assert!(request("sessionStart",bytes,"%1").is_err()); }
        let input = br#"{"hook_event_name":"beforeSubmitPrompt","conversation_id":"s","generation_id":"s"}"#;
        assert!(
            request("beforeSubmitPrompt", input, "%1")
                .unwrap()
                .is_none()
        );
        assert!(request("beforeSubmitPrompt", input, "../pane").is_err());
        assert!(request("stop", &vec![b' '; 65_536], "%1").is_err());
    }
    /// IDE, SDK and guessed expense shapes must not become an ordinary CLI
    /// billing source merely because their field names look familiar. Missing,
    /// explicit zero and nonzero counters all leave the same inert lifecycle
    /// projection. Selected model/provider/request labels and content canaries
    /// cannot grant native identity or leak through this metadata-only helper.
    #[test]
    fn cursor_hook_counter_shapes_cannot_claim_ordinary_cli_usage() {
        let shapes = [
            serde_json::json!({}),
            serde_json::json!({"usage":{"input_tokens":0,"output_tokens":0,"cache_read_tokens":0,"cache_write_tokens":0}}),
            serde_json::json!({"input_tokens":11,"output_tokens":7,"cache_read_tokens":5,"cache_write_tokens":3}),
            serde_json::json!({"usage":{"inputTokens":11,"outputTokens":7,"cacheReadTokens":5,"cacheWriteTokens":3,"totalTokens":26,"reasoningTokens":2}}),
            serde_json::json!({"usage":{"input_tokens":11,"output_tokens":7,"cached_input_tokens":5,"reasoning_output_tokens":2},"context_tokens":999999}),
        ];
        for event in ["afterAgentResponse", "stop"] {
            let original = serde_json::json!({"hook_event_name":event,"conversation_id":"session-a","generation_id":"turn-a","status":"completed","loop_count":0});
            let expected = request(event, original.to_string().as_bytes(), "%1")
                .unwrap()
                .unwrap();
            for shape in &shapes {
                let mut payload = original.clone();
                for (key, value) in shape.as_object().unwrap() {
                    payload[key] = value.clone();
                }
                payload["model"] = "PRIVATE-SELECTED-MODEL".into();
                payload["model_id"] = "PRIVATE-SELECTED-ID".into();
                payload["provider"] = "PRIVATE-PROVIDER".into();
                payload["request_id"] = "PRIVATE-REQUEST".into();
                payload["transcript_path"] = "PRIVATE-PATH".into();
                payload["text"] = "PRIVATE-RESPONSE".into();
                assert_eq!(
                    request(event, payload.to_string().as_bytes(), "%1")
                        .unwrap()
                        .unwrap(),
                    expected
                );
            }
        }
    }

    /// A stop count describes previous hook-driven followups, not a new prompt
    /// or permission to continue the vendor. It cannot alter the observational
    /// projection. Child selectors likewise cannot reuse parent labels to repaint
    /// parent status; all responses remain under the existing neutral helper path.
    #[test]
    fn cursor_hook_followup_history_and_children_cannot_rebind_parent() {
        let original = serde_json::json!({"hook_event_name":"stop","conversation_id":"session-a","generation_id":"turn-a","status":"completed","loop_count":0});
        let expected = request("stop", original.to_string().as_bytes(), "%1")
            .unwrap()
            .unwrap();
        for count in [1, 5, 9_007_199_254_740_991u64] {
            let mut payload = original.clone();
            payload["loop_count"] = count.into();
            assert_eq!(
                request("stop", payload.to_string().as_bytes(), "%1")
                    .unwrap()
                    .unwrap(),
                expected
            );
        }
        for key in [
            "subagent_id",
            "parent_conversation_id",
            "agent_id",
            "subagent_type",
        ] {
            let mut payload = original.clone();
            payload[key] = "PRIVATE-CHILD".into();
            assert!(
                request("stop", payload.to_string().as_bytes(), "%1")
                    .unwrap()
                    .is_none()
            );
        }
        for invalid in [
            serde_json::Value::Null,
            (-1).into(),
            serde_json::json!(1.5),
            serde_json::json!(9_007_199_254_740_992u64),
        ] {
            let mut payload = original.clone();
            payload["loop_count"] = invalid;
            assert!(
                request("stop", payload.to_string().as_bytes(), "%1")
                    .unwrap()
                    .is_none()
            );
        }
    }

    /// The lifecycle transition from nonretaining drain preserves the exact byte
    /// cap, with no extra block consumed while discovering oversized input.
    #[test]
    fn cursor_hook_input_consumes_no_more_than_exact_limit() {
        use std::io::{Read, Write};
        let (mut input, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let task = std::thread::spawn(move || writer.write_all(&vec![b'X'; 73_728]).unwrap());
        assert!(read_input_from(&input).is_err());
        let mut remaining = Vec::new();
        input.read_to_end(&mut remaining).unwrap();
        task.join().unwrap();
        assert_eq!(remaining.len(), 8192);
    }
}
