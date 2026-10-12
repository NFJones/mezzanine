//! Neutral classic Copilot hooks with bounded metadata-only local projection.
//!
//! Official hooks-reference documents sessionId/timestamp in classic camelCase
//! stdin, separately from SDK invocation context. Only fixed event argv and inert
//! IDs/enums enter same-user local ingress; the daemon proves native creator/root.
//! No prompt/tool/error/transcript path is forwarded or read. Loss is neutral,
//! and output never modifies approval, context, continuation or vendor behavior.

use super::{Args, MezError, Result, SocketSelection, Write};
use std::time::{Duration, Instant};

/// Literal supported classic event names, not SDK or permissionRequest aliases.
const EVENTS: &[&str] = &[
    "sessionStart",
    "userPromptSubmitted",
    "preToolUse",
    "postToolUse",
    "notification",
    "agentStop",
    "errorOccurred",
    "sessionEnd",
];

/// Fixed compiled argv supplies event semantics; stdin cannot select a method.
#[derive(Debug, Clone, Args)]
pub(super) struct CopilotHookArgs {
    /// Known topic; neither it nor inherited routing supplies native authority.
    #[arg(value_parser = ["sessionStart", "userPromptSubmitted", "preToolUse", "postToolUse", "notification", "agentStop", "errorOccurred", "sessionEnd"])]
    pub(super) event: String,
}

/// Recognizes exact argv before HOME/config/CPU startup; no discovery fallback.
pub(crate) fn run_internal_process(arguments: &[std::ffi::OsString]) -> Option<u8> {
    if arguments.len() != 3
        || arguments[1] != "copilot-hook"
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

/// Strict 64KiB/250ms input; a full buffer is unavailable, without EOF read-ahead.
/// Zeroized storage bounds temporary vendor content even on a stalled writer.
fn read_input_from(input: &impl std::os::fd::AsFd) -> Result<zeroize::Zeroizing<Vec<u8>>> {
    let deadline = Instant::now() + Duration::from_millis(250);
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    let mut buffer = zeroize::Zeroizing::new([0u8; 4096]);
    while bytes.len() < 65_536 {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| MezError::invalid_state("Copilot input unavailable"))?;
        let timeout = rustix::event::Timespec::try_from(remaining)
            .map_err(|_| MezError::invalid_state("Copilot input unavailable"))?;
        let mut fds = [rustix::event::PollFd::new(
            input,
            rustix::event::PollFlags::IN,
        )];
        match rustix::event::poll(&mut fds, Some(&timeout)) {
            Ok(0) => return Err(MezError::invalid_state("Copilot input unavailable")),
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
    Err(MezError::invalid_args("Copilot input unavailable"))
}

/// Projects one strict classic payload into fixed local control metadata only.
fn request(event: &str, bytes: &[u8], pane: &str) -> Result<Option<String>> {
    if bytes.len() >= 65_536
        || !EVENTS.contains(&event)
        || pane.len() < 2
        || pane.len() > 64
        || !pane.starts_with('%')
        || !pane[1..].bytes().all(|b| b.is_ascii_digit())
    {
        return Err(MezError::invalid_args("Copilot observation unavailable"));
    }
    let value = crate::protocol::strict_json::decode(bytes)
        .map_err(|_| MezError::invalid_args("Copilot observation unavailable"))?;
    let session = value["sessionId"]
        .as_str()
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 128
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
        })
        .ok_or_else(|| MezError::invalid_args("Copilot session unavailable"))?;
    let timestamp = value["timestamp"]
        .as_u64()
        .filter(|n| *n > 0 && *n <= 9_007_199_254_740_991)
        .ok_or_else(|| MezError::invalid_args("Copilot event order unavailable"))?;
    if value.get("agentId").is_some() || value.get("agentType").is_some() {
        return Ok(None);
    }
    let (method, params) = if event == "sessionStart" {
        let boundary = value["source"]
            .as_str()
            .filter(|s| matches!(*s, "startup" | "resume" | "new"))
            .ok_or_else(|| MezError::invalid_args("Copilot boundary unavailable"))?;
        (
            "agent/external/curated-enroll",
            serde_json::json!({"pane_id":pane,"harness":"copilot","version":"best-effort","display_name":"Copilot","external_session_id":session,"observer_kind":"command-hook","observer_instance":"copilot-local-hooks-1","source_contract":"copilot-local-command/1","session_boundary":boundary}),
        )
    } else {
        let event = match event {
            "notification" => match value["notification_type"].as_str() {
                Some("permission_prompt") => "permission_prompt",
                Some("elicitation_dialog") => "elicitation_dialog",
                _ => return Ok(None),
            },
            "agentStop" => {
                if value["stopReason"] != "end_turn" || !value["stop_hook_active"].is_boolean() {
                    return Ok(None);
                }
                // Another hook has already requested continuation: not ready.
                if value["stop_hook_active"] == true {
                    return Ok(None);
                }
                "agentStop"
            }
            "errorOccurred" => {
                if value["recoverable"] != false {
                    return Ok(None);
                }
                "errorOccurred"
            }
            "sessionEnd" => {
                if !value["reason"].as_str().is_some_and(|r| {
                    matches!(r, "complete" | "error" | "abort" | "timeout" | "user_exit")
                }) {
                    return Ok(None);
                }
                "sessionEnd"
            }
            other => other,
        };
        (
            "agent/external/copilot-observe",
            serde_json::json!({"pane_id":pane,"external_session_id":session,"event":event,"timestamp":timestamp}),
        )
    };
    let body =
        serde_json::json!({"jsonrpc":"2.0","id":"copilot-hook","method":method,"params":params})
            .to_string();
    crate::control::validate_control_method_params_schema(
        &crate::control::parse_json_rpc_request(&body)?,
    )?;
    Ok(Some(body))
}

/// At most one bounded local exchange; all errors remain telemetry loss only.
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
            "copilot-hook",
        )
        .await;
    }
    writeln!(stdout, "{{}}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Exact argv cannot capture unknown permission selectors or general modes.
    #[test]
    fn copilot_hook_exact_argv_rejects_other_modes() {
        for argv in [
            vec!["mez", "copilot-hook"],
            vec!["mez", "copilot-hook", "permissionRequest"],
            vec!["mez", "copilot-hook", "sessionStart", "extra"],
            vec!["mez", "--json", "copilot-hook", "sessionStart"],
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
    /// Oversized input consumes exactly64KiB, preserving the previous byte cap.
    #[test]
    fn copilot_hook_drain_never_consumes_beyond_exact_limit() {
        use std::io::{Read, Write};
        let (mut input, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let task = std::thread::spawn(move || writer.write_all(&vec![b'X'; 73_728]).unwrap());
        assert!(read_input_from(&input).is_err());
        let mut remaining = Vec::new();
        input.read_to_end(&mut remaining).unwrap();
        task.join().unwrap();
        assert_eq!(remaining.len(), 8192);
    }
    /// Classic documented sessionId, never SDK context, determines inert metadata;
    /// all content/PID/credential/counter fields and unsupported semantics drop.
    #[test]
    fn copilot_hook_projection_is_content_free_and_classic_only() {
        for source in ["startup", "resume", "new"] {
            let body = request("sessionStart",serde_json::json!({"sessionId":"s-a","timestamp":1,"source":source,"initialPrompt":"PRIVATE","cwd":"PRIVATE","invocation":{"sessionId":"wrong"},"pid":123,"usage":{"input_tokens":99}}).to_string().as_bytes(),"%1").unwrap().unwrap();
            assert!(!body.contains("PRIVATE"));
            assert!(!body.contains("wrong"));
            let value: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(value["params"]["harness"], "copilot");
            assert_eq!(value["params"]["session_boundary"], source);
        }
        for event in EVENTS.iter().filter(|e| **e != "sessionStart") {
            let input = serde_json::json!({"sessionId":"s-a","timestamp":2,"notification_type":"permission_prompt","stopReason":"end_turn","stop_hook_active":false,"recoverable":false,"reason":"user_exit","prompt":"PRIVATE","toolArgs":"PRIVATE","transcriptPath":"PRIVATE","error":{"message":"PRIVATE"}}).to_string();
            let body = request(event, input.as_bytes(), "%1").unwrap().unwrap();
            assert!(!body.contains("PRIVATE"));
            let value: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(value["params"].as_object().unwrap().len(), 4);
        }
        for bytes in [
            br#"{"sessionId":"a","sessionId":"b","timestamp":1,"source":"startup"}"#.as_slice(),
            br#"{"invocation":{"sessionId":"a"},"timestamp":1,"source":"startup"}"#,
            br#"{"session_id":"a","timestamp":1,"source":"startup"}"#,
            b"[]",
        ] {
            assert!(request("sessionStart", bytes, "%1").is_err());
        }
        assert!(
            request(
                "notification",
                br#"{"sessionId":"a","timestamp":1,"notification_type":"agent_idle"}"#,
                "%1"
            )
            .unwrap()
            .is_none()
        );
        assert!(request("agentStop",br#"{"sessionId":"a","timestamp":1,"stopReason":"end_turn","stop_hook_active":true}"#,"%1").unwrap().is_none());
    }
}
