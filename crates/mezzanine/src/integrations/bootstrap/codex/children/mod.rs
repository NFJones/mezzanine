//! Bounded SubagentStart/Stop facts, separate from parent lifecycle and billing.
//!
//! The shared session and explicit child ID narrow only a native original parent.
//! No payload path, role, content, counters or supplied process identity survives.
//! Child SessionStart/End remain inert and cannot become root declarations.

use super::*;

/// Observational child selectors; actual binding requires native worker proof.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Fact {
    /// Shared original parent-session selector, not source authority.
    pub(crate) session: String,
    /// Explicit descendant thread identity, never an implicit main session.
    pub(crate) child: String,
    /// Actual child turn boundary, separate from the parent's current turn.
    pub(crate) turn: String,
    /// One fixed supported child lifecycle name.
    pub(crate) event: &'static str,
}

/// Projects only known child boundary inputs. Oversized/ambiguous objects and
/// malformed selectors reject without echoing private data; other events abstain.
pub(crate) fn normalize(bytes: &[u8]) -> Result<Option<Fact>> {
    if bytes.len() > 64 * 1024 {
        return Err(MezError::invalid_args(
            "Codex child observation unavailable",
        ));
    }
    let object: UniqueObject = serde_json::from_slice(bytes)
        .map_err(|_| MezError::invalid_args("Codex child observation unavailable"))?;
    let value = serde_json::Value::Object(object.0);
    let event = match value["hook_event_name"].as_str() {
        Some("SubagentStart") => "SubagentStart",
        Some("SubagentStop") => "SubagentStop",
        _ => return Ok(None),
    };
    if event == "SubagentStop"
        && !value
            .get("stop_hook_active")
            .is_some_and(serde_json::Value::is_boolean)
    {
        return Err(MezError::invalid_args(
            "Codex child stop boundary unavailable",
        ));
    }
    Ok(Some(Fact {
        session: identifier(&value, "session_id")?,
        child: identifier(&value, "agent_id")?,
        turn: identifier(&value, "turn_id")?,
        event,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only explicit child start/stop metadata survives projection. Private
    /// fields never escape, duplicate keys and missing IDs reject, and child
    /// SessionEnd remains unable to become a parent retirement declaration.
    #[test]
    fn codex_child_projection_keeps_only_explicit_original_scope() {
        for event in ["SubagentStart", "SubagentStop"] {
            let bytes = serde_json::json!({"hook_event_name":event,"session_id":"root","agent_id":"child",
                "turn_id":"turn","stop_hook_active":false,"cwd":"PRIVATE","agent_type":"PRIVATE","last_assistant_message":"PRIVATE"}).to_string();
            let fact = normalize(bytes.as_bytes()).unwrap().unwrap();
            assert_eq!(
                (
                    fact.session.as_str(),
                    fact.child.as_str(),
                    fact.turn.as_str(),
                    fact.event
                ),
                ("root", "child", "turn", event)
            );
            assert!(!format!("{fact:?}").contains("PRIVATE"));
        }
        assert!(
            normalize(
                br#"{"hook_event_name":"SubagentStart","session_id":"root","turn_id":"turn"}"#
            )
            .is_err()
        );
        assert!(
            normalize(br#"{"hook_event_name":"SubagentStart","agent_id":"a","agent_id":"b"}"#)
                .is_err()
        );
        assert!(
            normalize(
                br#"{"hook_event_name":"SessionEnd","session_id":"root","agent_id":"child"}"#
            )
            .unwrap()
            .is_none()
        );
    }
}
