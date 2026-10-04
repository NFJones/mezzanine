//! Content-free lifecycle observations pinned to Pi coding-agent 1.0.2.
//!
//! The released extension types and agent-session producer distinguish final
//! agent_settled from agent_end and mutable before-settle boundaries. Session
//! identity comes separately from the extension context's session manager, not
//! event paths or inherited pane environment. These facts grant no registration,
//! renewal, continuation, approval or filesystem authority. The future launcher
//! must own binding, sequencing and resources across extension reloads.

use crate::error::{MezError, Result};

/// Exact installed release whose callback declarations were inspected.
pub(crate) const RELEASE: &str = "1.0.2";

/// An observational fact, not an instruction or proof of process death.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Observation {
    /// Context-bound session activation; replacement needs new launch authority.
    SessionStarted { reason: &'static str },
    /// Agent activity began; no completion or expense is inferred.
    Running,
    /// A blocking extension UI prompt began, not necessarily permission approval.
    InputWait,
    /// A UI prompt ended; prior running/idle state remains launcher-owned.
    InputEnded,
    /// A provisional boundary outcome that may still be followed by continuation.
    CandidateOutcome { outcome: &'static str },
    /// Final notification after automatic recovery/continuation has settled.
    /// Outcome remains the separately observed candidate, or unavailable.
    Settled,
    /// Extension teardown reason, not an attestation of process exit.
    SessionShutdown { reason: &'static str },
}

/// Returns generic diagnostics without echoing callback content or identifiers.
fn unavailable() -> MezError {
    MezError::invalid_args("Pi lifecycle observation unavailable")
}

/// Validates an inert bounded session identifier, never a path or credential.
fn valid_session(session: &str) -> bool {
    !session.is_empty()
        && session.len() <= 128
        && session
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

/// Projects only released lifecycle facts for an explicitly authorized session.
/// `observed_session` must be obtained from callback context, not the event JSON;
/// equality is a filter, not authority to mint/rebind a launch capability.
/// Partial message/tool/context fields and user answers are discarded. Unknown
/// callbacks are inert. Invalid known-event enums or bounds fail generically.
pub(crate) fn normalize(
    release: &str,
    bound_session: &str,
    observed_session: &str,
    bytes: &[u8],
) -> Result<Option<Observation>> {
    if release != RELEASE
        || !valid_session(bound_session)
        || !valid_session(observed_session)
        || bytes.len() > 64 * 1024
    {
        return Err(unavailable());
    }
    if bound_session != observed_session {
        return Ok(None);
    }
    let event: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| unavailable())?;
    if !event.is_object() {
        return Err(unavailable());
    }
    let kind = event
        .get("type")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(unavailable)?;
    let observation = match kind {
        "session_start" | "session_shutdown" => {
            let reason = match event.get("reason").and_then(serde_json::Value::as_str) {
                Some("startup") if kind == "session_start" => "startup",
                Some("quit") if kind == "session_shutdown" => "quit",
                Some("reload") => "reload",
                Some("new") => "new",
                Some("resume") => "resume",
                Some("fork") => "fork",
                _ => return Err(unavailable()),
            };
            if kind == "session_start" {
                Observation::SessionStarted { reason }
            } else {
                Observation::SessionShutdown { reason }
            }
        }
        "agent_start" => Observation::Running,
        "ui_prompt_start" | "ui_prompt_end" => {
            if event.get("reason").and_then(serde_json::Value::as_str) != Some("ui_prompt")
                || !matches!(
                    event.get("kind").and_then(serde_json::Value::as_str),
                    Some("select" | "confirm" | "input" | "editor" | "custom")
                )
            {
                return Err(unavailable());
            }
            if kind == "ui_prompt_start" {
                Observation::InputWait
            } else {
                Observation::InputEnded
            }
        }
        "agent_before_settle" => {
            let outcome = match event.get("outcome").and_then(serde_json::Value::as_str) {
                Some("completed") => "completed",
                Some("aborted") => "aborted",
                Some("error") => "error",
                _ => return Err(unavailable()),
            };
            Observation::CandidateOutcome { outcome }
        }
        "agent_settled" => Observation::Settled,
        // agent_end and turn_end are not final settlement; message callbacks
        // do not establish registration, outcome or independently counted usage.
        _ => return Ok(None),
    };
    Ok(Some(observation))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Explicit offline qualification uses the installed pinned loader/runner,
    /// not a provider or user extension discovery. Package-independent observer
    /// assertions run first; actual runner facts must then match the Rust owner.
    /// Ordinary workspace tests require neither Node nor vendor installation.
    #[test]
    #[ignore = "explicit trusted Pi 1.0.2 package and Node >=22.19.0 required"]
    fn pi_lifecycle_released_loader_observations_match_rust_projection() {
        let node = std::path::PathBuf::from(
            std::env::var_os("MEZ_PI_NODE").expect("explicit MEZ_PI_NODE required"),
        );
        let package = std::path::PathBuf::from(
            std::env::var_os("MEZ_PI_PACKAGE").expect("explicit MEZ_PI_PACKAGE required"),
        );
        assert!(node.is_absolute() && package.is_absolute());
        let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let home = std::env::temp_dir().join(format!(
            "mez-pi-offline-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        std::fs::create_dir(&home).unwrap();
        let run = |script: &str, test: bool| {
            let mut command = std::process::Command::new(&node);
            command
                .env_clear()
                .env("HOME", &home)
                .env("PATH", node.parent().unwrap())
                .current_dir(repository);
            if test {
                command.arg("--test");
            }
            command.arg(repository.join(script));
            if !test {
                command.arg(&package);
            }
            command.output().unwrap()
        };
        let unit = run("scripts/test-pi-observer.mjs", true);
        assert!(
            unit.status.success(),
            "offline observer test failed: {}",
            unit.status
        );
        let extension_unit = run("scripts/test-pi-extension.mjs", true);
        assert!(
            extension_unit.status.success(),
            "offline extension wiring test failed: {}",
            extension_unit.status
        );
        let output = run("scripts/qualify-pi-observer.mjs", false);
        std::fs::remove_dir_all(home).unwrap();
        assert!(
            output.status.success(),
            "released loader fixture failed: {}",
            output.status
        );
        assert!(output.stderr.is_empty());
        assert!(output.stdout.len() < 64 * 1024);
        let facts: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
        let actual = facts
            .iter()
            .map(|fact| {
                let session = fact["session"].as_str().unwrap();
                normalize(
                    RELEASE,
                    "bound",
                    session,
                    &serde_json::to_vec(&fact["event"]).unwrap(),
                )
                .unwrap()
                .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            actual,
            vec![
                Observation::SessionStarted { reason: "startup" },
                Observation::Running,
                Observation::InputWait,
                Observation::InputEnded,
                Observation::CandidateOutcome {
                    outcome: "completed"
                },
                Observation::Settled,
                Observation::SessionShutdown { reason: "reload" },
            ]
        );
    }

    /// Released callback facts preserve provisional/final boundaries while all
    /// prompt, answer, tool, transcript-path and context fields remain discarded.
    #[test]
    fn pi_lifecycle_keeps_boundaries_content_free_and_session_bound() {
        let project = |bytes: &[u8]| normalize(RELEASE, "bound", "bound", bytes).unwrap();
        assert_eq!(
            project(br#"{"type":"agent_start","prompt":"PRIVATE"}"#),
            Some(Observation::Running)
        );
        assert_eq!(
            project(br#"{"type":"agent_end","messages":["PRIVATE"]}"#),
            None
        );
        assert_eq!(
            project(br#"{"type":"turn_end","outcome":"completed","message":"PRIVATE"}"#),
            None
        );
        let candidate = project(br#"{"type":"agent_before_settle","outcome":"completed","continue":true,"context":"PRIVATE"}"#);
        assert_eq!(
            candidate,
            Some(Observation::CandidateOutcome {
                outcome: "completed"
            })
        );
        assert!(!format!("{candidate:?}").contains("PRIVATE"));
        assert_eq!(
            project(br#"{"type":"agent_settled"}"#),
            Some(Observation::Settled)
        );
        assert_eq!(project(br#"{"type":"ui_prompt_start","reason":"ui_prompt","kind":"confirm","title":"PRIVATE"}"#), Some(Observation::InputWait));
        assert_eq!(project(br#"{"type":"ui_prompt_end","reason":"ui_prompt","kind":"confirm","answer":"PRIVATE"}"#), Some(Observation::InputEnded));
        assert_eq!(
            normalize(RELEASE, "bound", "other", br#"{"type":"agent_start"}"#).unwrap(),
            None
        );
        // A forged event session cannot change context-supplied binding.
        assert_eq!(
            normalize(
                RELEASE,
                "bound",
                "other",
                br#"{"type":"agent_start","session_id":"bound"}"#
            )
            .unwrap(),
            None
        );
    }

    /// Reload/new/resume/fork teardown is distinct from quitting; invalid known
    /// event shapes and versions cannot create guessed lifecycle observations.
    #[test]
    fn pi_lifecycle_rejects_unknown_versions_enums_and_unbounded_input() {
        for reason in ["reload", "new", "resume", "fork"] {
            let bytes = serde_json::json!({"type":"session_shutdown","reason":reason,"targetSessionFile":"PRIVATE"}).to_string();
            assert!(matches!(
                normalize(RELEASE, "bound", "bound", bytes.as_bytes()).unwrap(),
                Some(Observation::SessionShutdown { .. })
            ));
        }
        for bytes in [
            br#"{"type":"session_start","reason":"quit"}"#.as_slice(),
            br#"{"type":"agent_before_settle","outcome":"unknown"}"#,
            br#"{"type":"ui_prompt_start","kind":"input"}"#,
            b"[]",
        ] {
            assert!(normalize(RELEASE, "bound", "bound", bytes).is_err());
        }
        assert!(normalize("future", "bound", "bound", b"{}").is_err());
        assert!(normalize(RELEASE, "bound", "bound", &vec![b'x'; 65537]).is_err());
        let error = normalize(RELEASE, "PRIVATE\n", "bound", b"{}").unwrap_err();
        assert!(!error.message().contains("PRIVATE"));
    }
}
