//! Neutral ordinary Codex SessionStart admission through fixed local telemetry.
//!
//! Bounded vendor stdin is projected into one exact code-owned creator declaration.
//! Standard MEZ routing selects a candidate, never authority; native writer/parent/
//! root evidence remains daemon-owned. No fd3, credential, general RPC, transcript,
//! subprocess or retry is used. Replies/errors never reach vendor output. Later
//! SessionEnd retires only its original session; freshness/usage remain separate.

use super::{MezError, Result, SocketSelection, Write};
use crate::integrations::bootstrap::codex::{self, Observation};
use std::time::Duration;

/// Runs exact fixed argv before HOME/config/CPU discovery using only a bounded
/// current-thread I/O runtime. Missing route/runtime/input is neutral telemetry
/// loss, not an ordinary CLI error. Non-exact argv retains normal CLI startup.
pub(crate) fn run_internal_process(arguments: &[std::ffi::OsString]) -> Option<u8> {
    if arguments.len() != 2 || arguments[1] != "codex-hook" {
        return None;
    }
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
                .block_on(run(&socket, pane.as_deref(), &mut stdout))
                .is_err(),
        ));
    }
    let _ = super::harness_event::read_input_from(&std::io::stdin(), Duration::from_millis(250));
    Some(u8::from(writeln!(stdout, "{{}}").is_err()))
}

/// Rebuilds a normalized initial declaration or original-instance turn callback. The fixed
/// process-local instance label permits inert retry for one qualified creator,
/// not another helper-owned namespace. Child/compact/turn/end events cannot
/// implicitly allocate or revive a producer, and vendor content is discarded.
fn request(bytes: &[u8], pane: &str) -> Result<Option<String>> {
    let Some(observation) = codex::normalize("best-effort", bytes)? else {
        return Ok(None);
    };
    if pane.len() < 2
        || pane.len() > 64
        || !pane.starts_with('%')
        || !pane[1..].bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(MezError::invalid_args("Codex observation unavailable"));
    }
    let (method, params) = match observation {
        Observation::SessionReady { session } => {
            // Read only the source enum after bounded unique main normalization.
            let value: serde_json::Value = serde_json::from_slice(bytes)
                .map_err(|_| MezError::invalid_args("Codex observation unavailable"))?;
            let Some(boundary) = value["source"]
                .as_str()
                .filter(|source| matches!(*source, "startup" | "resume" | "clear" | "fork"))
            else {
                return Ok(None);
            };
            (
                "agent/external/curated-enroll",
                serde_json::json!({
                    "pane_id":pane,"harness":"codex","version":"best-effort","display_name":"Codex",
                    "external_session_id":session,"observer_kind":"command-hook","observer_instance":"codex-local-hooks-1",
                    "source_contract":"codex-local-command/1","session_boundary":boundary,
                }),
            )
        }
        Observation::Turn {
            session,
            turn,
            state,
        } => {
            let event = match state {
                "running" => "UserPromptSubmit",
                "ready" => "Stop",
                "interrupted" => "Interrupt",
                _ => return Ok(None),
            };
            (
                "agent/external/codex-observe",
                serde_json::json!({"pane_id":pane,"external_session_id":session,"event":event,"turn_id":turn}),
            )
        }
        Observation::SessionEnded { session } => (
            "agent/external/codex-observe",
            serde_json::json!({"pane_id":pane,"external_session_id":session,"event":"SessionEnd"}),
        ),
    };
    let body =
        serde_json::json!({"jsonrpc":"2.0","id":"codex-hook","method":method,"params":params})
            .to_string();
    let parsed = crate::control::parse_json_rpc_request(&body)?;
    crate::control::validate_control_method_params_schema(&parsed)?;
    Ok(Some(body))
}

/// Performs at most one same-user peer-qualified Unix exchange after finite
/// stdin normalization. No daemon result or credentials are exposed; only
/// writing the required neutral JSON can fail the callback process.
pub(super) async fn run<W: Write>(
    socket: &SocketSelection,
    pane: Option<&str>,
    stdout: &mut W,
) -> Result<()> {
    if let Ok(bytes) =
        super::harness_event::read_input_from(&std::io::stdin(), Duration::from_millis(250))
        && let Some(pane) = pane
        && let Ok(Some(body)) = request(&bytes, pane)
    {
        let _ = super::harness_event::exchange_result(
            super::selected_socket_path(socket),
            &body,
            "codex-hook",
        )
        .await;
    }
    writeln!(stdout, "{{}}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Early helper recognition is exact, not a broad CLI parser bypass. Other
    /// commands and option-bearing forms keep normal startup/validation; none
    /// of these inputs opens a socket or consumes stdin/configuration.
    #[test]
    fn codex_hook_internal_argv_does_not_capture_other_cli_forms() {
        for argv in [
            vec![],
            vec!["mez"],
            vec!["mez", "version"],
            vec!["mez", "codex-hook", "extra"],
            vec!["mez", "--json", "codex-hook"],
            vec!["mez", "codex-hook", "--help"],
        ] {
            let arguments = argv
                .into_iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>();
            assert_eq!(run_internal_process(&arguments), None);
        }
    }

    /// Only genuine normalized main-session boundaries become fixed declarations.
    /// Sensitive content, supplied credentials/PIDs and fake counters never enter
    /// transport; Stop/child/compact callbacks are not initial-admission evidence.
    #[test]
    fn codex_hook_frames_are_neutral_and_content_free() {
        for boundary in ["startup", "resume", "clear", "fork"] {
            let bytes = serde_json::json!({"hook_event_name":"SessionStart","session_id":"session-a","source":boundary,
                "prompt":"PRIVATE","transcript_path":"PRIVATE","launch_token":"PRIVATE","pid":123,"usage":{"input_tokens":99}}).to_string();
            let body = request(bytes.as_bytes(), "%1").unwrap().unwrap();
            assert!(!body.contains("PRIVATE"));
            let value: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(value["method"], "agent/external/curated-enroll");
            assert_eq!(value["params"]["source_contract"], "codex-local-command/1");
            assert_eq!(value["params"]["session_boundary"], boundary);
            assert_eq!(value["params"].as_object().unwrap().len(), 9);
        }
        for bytes in [
            br#"{"hook_event_name":"SessionStart","session_id":"session-a","source":"startup","agent_id":"child"}"#.as_slice(),
            br#"{"hook_event_name":"SessionStart","session_id":"session-a","source":"compact"}"#,
        ] { assert!(request(bytes, "%1").unwrap().is_none()); }
        let end = request(br#"{"hook_event_name":"SessionEnd","session_id":"session-a","transcript_path":"PRIVATE"}"#, "%1").unwrap().unwrap();
        let end: serde_json::Value = serde_json::from_str(&end).unwrap();
        assert_eq!(end["params"]["event"], "SessionEnd");
        assert_eq!(end["params"].as_object().unwrap().len(), 3);
        assert!(end["params"].get("turn_id").is_none());
        for event in ["UserPromptSubmit", "Stop", "Interrupt"] {
            let bytes = serde_json::json!({"hook_event_name":event,"session_id":"a","turn_id":"turn","stop_hook_active":false,"prompt":"PRIVATE"}).to_string();
            let body = request(bytes.as_bytes(), "%1").unwrap().unwrap();
            assert!(!body.contains("PRIVATE"));
            let value: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(value["method"], "agent/external/codex-observe");
            assert_eq!(value["params"]["event"], event);
            assert_eq!(value["params"].as_object().unwrap().len(), 4);
        }
    }

    /// Duplicate identity/source fields, malformed/oversized input and invalid
    /// route selectors cannot request native work. No opaque field supplies
    /// general control authority and unsupported events never become a fallback.
    #[test]
    fn codex_hook_requests_reject_ambiguous_input_and_route() {
        for bytes in [
            br#"{"hook_event_name":"SessionStart","session_id":"a","session_id":"b","source":"startup"}"#.as_slice(),
            br#"{"hook_event_name":"SessionStart","session_id":"a","source":"startup","source":"fork"}"#,
            b"{}", b"[]", b"not json",
        ] { assert!(request(bytes, "%1").is_err()); }
        let bytes = br#"{"hook_event_name":"SessionStart","session_id":"a","source":"startup"}"#;
        for pane in ["", "%", "../pane", "%1;command", "%x"] {
            assert!(request(bytes, pane).is_err());
        }
        assert!(request(&vec![b' '; 65_537], "%1").is_err());
    }
}
