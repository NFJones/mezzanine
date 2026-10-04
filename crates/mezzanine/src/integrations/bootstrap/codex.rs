//! Content-free lifecycle projection pinned to Codex rust-v0.160.0.
//!
//! Source: codex-rs/hooks/schema/generated at the released tag. Hook input is
//! observational data, not pane/registration authority. A launcher must privately
//! bind a main session and serialize presentation sequences before forwarding.
//! No transcripts, prompt text, tool payloads, credentials or counters leave this
//! owner. This component is not a certified installed adapter or usage tap.

use crate::error::{MezError, Result};

/// Exact upstream release whose lifecycle fields were inspected.
pub(crate) const RELEASE: &str = "0.160.0";

/// Observed lifecycle fact; no executable action or registration credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Observation {
    /// A main session began/resumed; binding still needs trusted launch authority.
    SessionReady { session: String },
    /// Coarse active/ended/interrupted turn presentation, not process death.
    Turn {
        session: String,
        turn: String,
        state: &'static str,
    },
    /// Exact main session end notification, not an attestation of process exit.
    SessionEnded { session: String },
}

/// Reads one bounded inert identifier without including supplied text in errors.
fn identifier(value: &serde_json::Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|text| {
            !text.is_empty()
                && text.len() <= 128
                && text.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
                })
        })
        .map(str::to_string)
        .ok_or_else(|| MezError::invalid_args("Codex observation unavailable"))
}

/// Projects allowlisted main-session events from the pinned release.
/// Child events and ambiguous permission/tool activity are intentionally inert.
/// Unknown additional payload data is discarded, never copied into output.
pub(crate) fn normalize(release: &str, bytes: &[u8]) -> Result<Option<Observation>> {
    if release != RELEASE || bytes.len() > 64 * 1024 {
        return Err(MezError::invalid_args("Codex observation unavailable"));
    }
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| MezError::invalid_args("Codex observation unavailable"))?;
    if !value.is_object() {
        return Err(MezError::invalid_args("Codex observation unavailable"));
    }
    // Upstream child hook contexts reuse parent session IDs. They must never
    // clear or repaint the parent's registration; separate child binding awaits
    // independent qualification.
    if value.get("agent_id").is_some() || value.get("agent_type").is_some() {
        return Ok(None);
    }
    let event = value
        .get("hook_event_name")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| MezError::invalid_args("Codex observation unavailable"))?;
    match event {
        "SessionStart" => {
            if !matches!(
                value.get("source").and_then(serde_json::Value::as_str),
                Some("startup" | "resume" | "clear" | "compact" | "fork")
            ) {
                return Err(MezError::invalid_args("Codex observation unavailable"));
            }
            Ok(Some(Observation::SessionReady {
                session: identifier(&value, "session_id")?,
            }))
        }
        "UserPromptSubmit" | "Interrupt" | "Stop" => {
            if event == "Stop"
                && !value
                    .get("stop_hook_active")
                    .is_some_and(serde_json::Value::is_boolean)
            {
                return Err(MezError::invalid_args("Codex observation unavailable"));
            }
            Ok(Some(Observation::Turn {
                session: identifier(&value, "session_id")?,
                turn: identifier(&value, "turn_id")?,
                state: match event {
                    "UserPromptSubmit" => "running",
                    "Interrupt" => "interrupted",
                    _ => "complete",
                },
            }))
        }
        "SessionEnd" => Ok(Some(Observation::SessionEnded {
            session: identifier(&value, "session_id")?,
        })),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Released fields project only identifiers and enum facts. Sensitive task
    /// data, fake counters and child context cannot create parent telemetry.
    #[test]
    fn codex_pinned_lifecycle_discards_content_and_child_context() {
        let bytes = br#"{"session_id":"session-1","turn_id":"turn-1","hook_event_name":"UserPromptSubmit","prompt":"PRIVATE PROMPT","transcript_path":"PRIVATE PATH","cwd":"PRIVATE CWD","usage":{"input_tokens":999}}"#;
        assert_eq!(
            normalize(RELEASE, bytes).unwrap(),
            Some(Observation::Turn {
                session: "session-1".into(),
                turn: "turn-1".into(),
                state: "running",
            })
        );
        let mut child: serde_json::Value = serde_json::from_slice(bytes).unwrap();
        child["agent_id"] = "child-1".into();
        assert_eq!(
            normalize(RELEASE, &serde_json::to_vec(&child).unwrap()).unwrap(),
            None
        );
        for (event, state) in [("Stop", "complete"), ("Interrupt", "interrupted")] {
            child.as_object_mut().unwrap().remove("agent_id");
            child["hook_event_name"] = event.into();
            child["stop_hook_active"] = false.into();
            assert_eq!(
                normalize(RELEASE, &serde_json::to_vec(&child).unwrap()).unwrap(),
                Some(Observation::Turn {
                    session: "session-1".into(),
                    turn: "turn-1".into(),
                    state,
                })
            );
        }
    }

    /// Wrong releases, invalid identities and oversized input fail generically;
    /// payload strings never appear in diagnostics and no usage is inferred.
    #[test]
    fn codex_pinned_lifecycle_rejects_ambiguous_or_unbounded_input() {
        assert!(normalize("future", b"{}").is_err());
        assert!(normalize(RELEASE, &vec![b'x'; 65537]).is_err());
        let error = normalize(
            RELEASE,
            br#"{"hook_event_name":"SessionEnd","session_id":"SECRET\ninvalid"}"#,
        )
        .unwrap_err();
        assert!(!error.message().contains("SECRET"));
        assert_eq!(
            normalize(RELEASE, br#"{"hook_event_name":"PermissionRequest"}"#).unwrap(),
            None
        );
    }
}
