//! Content-free best-effort lifecycle projection from documented Codex fields.
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

/// Rejects ambiguous top-level keys before any identity/event classification.
/// Unknown vendor fields are accepted as inert data but never forwarded.
struct UniqueObject(serde_json::Map<String, serde_json::Value>);
impl<'de> serde::Deserialize<'de> for UniqueObject {
    fn deserialize<D: serde::Deserializer<'de>>(decoder: D) -> std::result::Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = UniqueObject;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("bounded unique hook object")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> std::result::Result<Self::Value, M::Error> {
                let mut object = serde_json::Map::new();
                while let Some((key, value)) = map.next_entry::<String, serde_json::Value>()? {
                    if object.len() == 256 || object.contains_key(&key) {
                        return Err(serde::de::Error::custom("ambiguous hook object"));
                    }
                    object.insert(key, value);
                }
                Ok(UniqueObject(object))
            }
        }
        decoder.deserialize_map(Visitor)
    }
}

/// Projects allowlisted main-session events independent of observed version.
/// Child events and ambiguous permission/tool activity are intentionally inert.
/// Unknown additional payload data is discarded, never copied into output.
pub(crate) fn normalize(release: &str, bytes: &[u8]) -> Result<Option<Observation>> {
    if release.is_empty()
        || release.len() > 128
        || release.chars().any(char::is_control)
        || bytes.len() > 64 * 1024
    {
        return Err(MezError::invalid_args("Codex observation unavailable"));
    }
    let object: UniqueObject = serde_json::from_slice(bytes)
        .map_err(|_| MezError::invalid_args("Codex observation unavailable"))?;
    let value = serde_json::Value::Object(object.0);
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
                    // Stop callbacks may continue after another handler's
                    // decision; this boundary alone never proves success.
                    _ => "ready",
                },
            }))
        }
        "SessionEnd" => Ok(Some(Observation::SessionEnded {
            session: identifier(&value, "session_id")?,
        })),
        _ => Ok(None),
    }
}

/// Filters an independently authorized exact main session. Matching vendor IDs
/// are observational evidence only; this function cannot mint/rebind authority.
pub(crate) fn normalize_bound(
    release: &str,
    bound: &str,
    bytes: &[u8],
) -> Result<Option<Observation>> {
    identifier(&serde_json::json!({"session_id":bound}), "session_id")?;
    let observation = normalize(release, bytes)?;
    Ok(observation.filter(|item| match item {
        Observation::SessionReady { session }
        | Observation::SessionEnded { session }
        | Observation::Turn { session, .. } => session == bound,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exact member limits and metadata/binding boundaries remain enforced even
    /// when unknown vendor fields are otherwise inert and content-free.
    #[test]
    fn codex_callback_member_and_binding_limits_are_exact() {
        let mut value = serde_json::json!({"hook_event_name":"SessionEnd","session_id":"bound"});
        for index in 0..254 {
            value[format!("field{index}")] = serde_json::Value::Null;
        }
        assert!(
            normalize_bound("any", "bound", &serde_json::to_vec(&value).unwrap())
                .unwrap()
                .is_some()
        );
        value["extra"] = serde_json::Value::Null;
        assert!(normalize_bound("any", "bound", &serde_json::to_vec(&value).unwrap()).is_err());
        for version in ["x".repeat(129), "bad\nversion".into()] {
            assert!(normalize(&version, b"{}").is_err());
        }
        assert!(normalize_bound("any", "bad\nbound", b"{}").is_err());
        assert!(
            normalize_bound(
                "any",
                "bound",
                br#"{"hook_event_name":"SessionEnd","session_id":"bound","agent_id":"child"}"#
            )
            .unwrap()
            .is_none()
        );
    }

    /// Duplicate callback identity/event keys cannot select a different main
    /// session. Unknown task content is still discarded, and other/child sessions
    /// cannot cross an explicitly authorized bound-session filter.
    #[test]
    fn codex_bound_projection_rejects_ambiguous_identity_without_content() {
        for bytes in [
            br#"{"hook_event_name":"SessionEnd","session_id":"bound","session_id":"other"}"#
                .as_slice(),
            br#"{"hook_event_name":"SessionEnd","hook_event_name":"Stop","session_id":"bound"}"#,
        ] {
            assert!(normalize_bound("any", "bound", bytes).is_err());
        }
        assert!(
            normalize_bound(
                "any",
                "bound",
                br#"{"hook_event_name":"SessionEnd","session_id":"other"}"#
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(normalize_bound("any","bound",br#"{"hook_event_name":"SessionEnd","session_id":"bound","transcript_path":"PRIVATE"}"#).unwrap(),Some(Observation::SessionEnded{session:"bound".into()}));
    }

    /// Version observations never gate the documented subset. Stop is a hook
    /// boundary that may still continue; only ready, not success, is inferred.
    /// Source content and ambiguous counters must remain outside telemetry.
    #[test]
    fn codex_best_effort_versions_and_stop_do_not_claim_success() {
        for version in ["any-local-version", RELEASE] {
            let result=normalize(version,br#"{"session_id":"session","turn_id":"turn","hook_event_name":"Stop","stop_hook_active":false,"usage":{"input_tokens":99}}"#).unwrap();
            assert_eq!(
                result,
                Some(Observation::Turn {
                    session: "session".into(),
                    turn: "turn".into(),
                    state: "ready"
                })
            );
        }
    }

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
        for (event, state) in [("Stop", "ready"), ("Interrupt", "interrupted")] {
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

    /// Invalid version text, identities and oversized input fail generically;
    /// payload strings never appear in diagnostics and no usage is inferred.
    #[test]
    fn codex_pinned_lifecycle_rejects_ambiguous_or_unbounded_input() {
        assert!(normalize("", b"{}").is_err());
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
