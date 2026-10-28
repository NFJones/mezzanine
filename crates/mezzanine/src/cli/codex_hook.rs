//! Neutral ordinary Codex SessionStart admission through fixed local telemetry.
//!
//! Bounded vendor stdin is projected into one exact code-owned creator declaration.
//! Standard MEZ routing selects a candidate, never authority; native writer/parent/
//! root evidence remains daemon-owned. No fd3, credential, general RPC, transcript,
//! subprocess is used. Model callbacks permit two bounded immutable delivery
//! attempts; only a typed durable result acknowledges expense. Replies/errors
//! never reach vendor output. Later
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

/// Refuses unknown/shared native parent modes before IPC, including when the
/// selected server predates its own guard. This is negative filtering only;
/// the server must still prove full original native source/root authority.
fn native_route_eligible(pid: u32) -> bool {
    let Some(identity) = mez_mux::process::process_parent_identity_for_pid(pid) else {
        return false;
    };
    let Some(refused) = mez_mux::process::process_argument_refusal_for_pid(
        identity.parent_process_id,
        codex::topology::shared_server,
    ) else {
        return false;
    };
    !refused && mez_mux::process::process_parent_identity_for_pid(pid) == Some(identity)
}

/// Rebuilds only fixed normalized observation fields, never supplied RPC text.
fn codex_observation_body(params: serde_json::Value) -> Result<String> {
    let body = serde_json::json!({"jsonrpc":"2.0","id":"codex-hook","method":"agent/external/codex-observe","params":params}).to_string();
    crate::control::validate_control_method_params_schema(
        &crate::control::parse_json_rpc_request(&body)?,
    )?;
    Ok(body)
}

/// Rebuilds a normalized initial declaration or original-instance turn callback. The fixed
/// process-local instance label permits inert retry for one qualified creator,
/// not another helper-owned namespace. Child/compact/turn/end events cannot
/// implicitly allocate or revive a producer, and vendor content is discarded.
fn request(bytes: &[u8], pane: &str) -> Result<Option<String>> {
    let observation = codex::normalize("best-effort", bytes)?;
    if observation.is_none() {
        if let Some(child) = codex::children::normalize(bytes)? {
            if pane.len() < 2
                || pane.len() > 64
                || !pane.starts_with('%')
                || !pane[1..].bytes().all(|b| b.is_ascii_digit())
            {
                return Err(MezError::invalid_args("Codex child pane unavailable"));
            }
            return Ok(Some(codex_observation_body(serde_json::json!({
                "pane_id":pane,"external_session_id":child.session,"turn_id":child.turn,
                "event":child.event,"agent_id":child.child,
            }))?));
        }
        if let Some(fact) = codex::request::normalize(bytes)? {
            if pane.len() < 2
                || pane.len() > 64
                || !pane.starts_with('%')
                || !pane[1..].bytes().all(|b| b.is_ascii_digit())
            {
                return Err(MezError::invalid_args("Codex request pane unavailable"));
            }
            let mut params = serde_json::json!({"pane_id":pane,"external_session_id":fact.session,"turn_id":fact.turn});
            if let Some(child) = fact.agent_id {
                params["agent_id"] = child.into();
            }
            match fact.phase {
                codex::request::Phase::Before => params["event"] = "PreModelRequest".into(),
                codex::request::Phase::After { counters, .. } => {
                    params["event"] = "PostModelRequest".into();
                    params["counters"] = serde_json::to_value(counters).map_err(|_| {
                        MezError::invalid_state("Codex normalized counters unavailable")
                    })?;
                }
            }
            return Ok(Some(codex_observation_body(params)?));
        }
        return Ok(None);
    }
    let Some(observation) = observation else {
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
        Observation::Activity {
            session,
            turn,
            event,
        } => (
            "agent/external/codex-observe",
            serde_json::json!({"pane_id":pane,"external_session_id":session,"event":event,"turn_id":turn}),
        ),
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

/// Performs one same-user exchange, or two immutable model-hook attempts, after finite
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
        && native_route_eligible(std::process::id())
    {
        let event = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|value| value["params"]["event"].as_str().map(str::to_owned));
        let model_request = matches!(
            event.as_deref(),
            Some("PreModelRequest" | "PostModelRequest")
        );
        for _ in 0..if model_request { 2 } else { 1 } {
            if super::harness_event::exchange_result(
                super::selected_socket_path(socket),
                &body,
                "codex-hook",
            )
            .await
            .is_ok_and(|reply| delivery_complete(event.as_deref(), &reply))
            {
                break;
            }
        }
    }
    writeln!(stdout, "{{}}")?;
    Ok(())
}

/// A transport success alone cannot acknowledge incurred request expense.
/// Lifecycle observations have no financial promise; pre callbacks may also
/// carry an older retained report whose durable acknowledgment is sufficient.
fn delivery_complete(event: Option<&str>, reply: &serde_json::Value) -> bool {
    let durable = reply["accepted"] == true
        && reply["durable"] == true
        && reply["applied"].is_boolean()
        && reply["revision"]
            .as_u64()
            .is_some_and(|revision| revision > 0)
        && reply["coverage"]["source"] == "external-reported"
        && reply["coverage"]["reasoning_known"].is_boolean();
    match event {
        Some("PostModelRequest") => durable,
        Some("PreModelRequest") => durable || reply["observed"] == true,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The installed helper's allowlisted lifecycle envelope cannot promote
    /// OTEL completion counters, context occupancy, selected model or supplied
    /// request/source/project selectors into accounting authority. Standalone
    /// completion telemetry is not a callback and produces no control request.
    #[test]
    fn codex_hook_source_safety_never_forwards_unqualified_usage() {
        for event in ["UserPromptSubmit", "Stop", "Interrupt"] {
            let bytes = serde_json::to_vec(&serde_json::json!({
                "hook_event_name":event,"session_id":"session-a","turn_id":"turn-a",
                "stop_hook_active":false,"model":"selected-model","permission_mode":"default",
                "event.name":"codex.sse_event","event.kind":"response.completed",
                "input_token_count":"121","output_token_count":"13","cached_token_count":32,
                "cache_write_token_count":4,"reasoning_token_count":2,
                "last_token_usage":{"total_tokens":5000},"total_token_usage":{"total_tokens":6000},
                "request_id":"PRIVATE","source_witness":"PRIVATE","project":"PRIVATE",
                "last_assistant_message":"PRIVATE","user.email":"PRIVATE","usage":{"input_tokens":999}
            })).unwrap();
            let body = request(&bytes, "%1").unwrap().unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(parsed["method"], "agent/external/codex-observe");
            assert_eq!(
                parsed["params"],
                serde_json::json!({
                    "pane_id":"%1","external_session_id":"session-a","event":event,"turn_id":"turn-a"
                })
            );
            assert!(!body.contains("PRIVATE"));
        }
        assert!(request(br#"{"event.name":"codex.sse_event","event.kind":"response.completed","conversation.id":"session-a","input_token_count":"121"}"#, "%1").is_err());
    }

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
    /// Activity must retain its exact event rather than aliasing prompt submit,
    /// which would let an old tool callback admit a new turn. Tool contents and
    /// approval decisions remain absent from the fixed observation capsule.
    #[test]
    fn codex_hook_activity_keeps_exact_event_without_content_or_decisions() {
        for event in ["PreToolUse", "PostToolUse", "PermissionRequest"] {
            let bytes = serde_json::to_vec(&serde_json::json!({"hook_event_name":event,"session_id":"session-a","turn_id":"turn-a",
                "tool_input":{"command":"PRIVATE"},"permission_mode":"bypassPermissions"})).unwrap();
            let body = request(&bytes, "%1").unwrap().unwrap();
            let value: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(value["method"], "agent/external/codex-observe");
            assert_eq!(value["params"]["event"], event);
            assert_eq!(value["params"]["turn_id"], "turn-a");
            assert_eq!(value["params"].as_object().unwrap().len(), 4);
            assert!(!body.contains("PRIVATE") && !body.contains("bypassPermissions"));
        }
    }
    /// Genuine model-hook usage survives failed/cancelled requests while paths,
    /// selected model and error text never leave the bounded projector. A child
    /// fact retains its explicit selector; native Start must authorize association.
    #[test]
    fn codex_hook_model_capsules_keep_only_checked_scope_and_usage() {
        for status in ["completed", "failed", "cancelled"] {
            let mut input = serde_json::json!({"hook_event_name":"PostModelRequest",
                "session_id":"session-a","turn_id":"turn-a","status":status,
                "model":"PRIVATE","cwd":"PRIVATE","error":"PRIVATE",
                "token_usage":{"input_tokens":10,"output_tokens":3,"total_tokens":13}});
            let body = request(&serde_json::to_vec(&input).unwrap(), "%1")
                .unwrap()
                .unwrap();
            let value: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(value["params"]["event"], "PostModelRequest");
            assert_eq!(value["params"]["counters"]["input_tokens"], 10);
            assert!(value["params"]["counters"]["reasoning_tokens"].is_null());
            assert!(!body.contains("PRIVATE"));
            input["agent_id"] = "child-a".into();
            let child_body = request(&serde_json::to_vec(&input).unwrap(), "%1")
                .unwrap()
                .unwrap();
            let child: serde_json::Value = serde_json::from_str(&child_body).unwrap();
            assert_eq!(child["params"]["agent_id"], "child-a");
            assert!(!child_body.contains("PRIVATE"));
        }
    }

    /// A successful socket reply may describe observation or telemetry loss.
    /// Only the shared ledger's typed durable result ends financial retry; both
    /// newly applied and conserved replay acknowledgments satisfy that promise.
    #[test]
    fn codex_hook_model_delivery_requires_typed_durable_acknowledgment() {
        for applied in [true, false] {
            let reply = serde_json::json!({"accepted":true,"durable":true,"applied":applied,
                "revision":1,"coverage":{"source":"external-reported","reasoning_known":false}});
            assert!(delivery_complete(Some("PostModelRequest"), &reply));
            let mut zero_revision = reply.clone();
            zero_revision["revision"] = 0.into();
            assert!(!delivery_complete(Some("PostModelRequest"), &zero_revision));
            for field in ["accepted", "durable", "applied", "revision", "coverage"] {
                let mut incomplete = reply.clone();
                incomplete.as_object_mut().unwrap().remove(field);
                assert!(!delivery_complete(Some("PostModelRequest"), &incomplete));
            }
        }
        let observed = serde_json::json!({"observed":true});
        assert!(!delivery_complete(Some("PostModelRequest"), &observed));
        assert!(delivery_complete(Some("PreModelRequest"), &observed));
        assert!(delivery_complete(Some("Stop"), &observed));
    }
}
