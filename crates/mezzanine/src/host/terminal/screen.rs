//! Product-owned decoding for Mezzanine shell integration events.
//!
//! The terminal parser and screen state live in `mez-terminal`. This module
//! only translates generic shell-integration payloads into product transaction
//! semantics.

use std::collections::BTreeMap;

use mez_terminal::{
    ManagedShellAdapter, ManagedShellParentOutcome, ManagedShellProtocolEvent, TerminalOscEvent,
};

/// Decodes one OSC 133 payload into product shell-transaction semantics.
pub(crate) fn parse_mez_shell_transaction_osc(payload: &str) -> Option<TerminalOscEvent> {
    let mut fields = payload.split(';');
    if fields.next()? != "133" {
        return None;
    }
    let kind = fields.next()?;
    match kind {
        "A" => Some(TerminalOscEvent::ShellPromptStart),
        "B" => Some(TerminalOscEvent::ShellPromptEnd),
        "C" => {
            let values = parse_semicolon_key_values(fields);
            if values.contains_key("mez_marker") {
                Some(TerminalOscEvent::ShellTransactionStart {
                    marker: required_marker_field(&values, "mez_marker")?,
                    turn_id: required_marker_field(&values, "mez_turn")?,
                    agent_id: required_marker_field(&values, "mez_agent")?,
                    pane_id: required_marker_field(&values, "mez_pane")?,
                })
            } else {
                Some(TerminalOscEvent::ShellCommandOutputStart)
            }
        }
        "D" => {
            let parts = fields.collect::<Vec<_>>();
            let exit_code = parts.first().and_then(|field| field.parse::<i32>().ok());
            let key_value_start = usize::from(exit_code.is_some());
            let values = parse_semicolon_key_values(parts.iter().skip(key_value_start).copied());
            if values.contains_key("mez_marker") {
                Some(TerminalOscEvent::ShellTransactionEnd {
                    marker: required_marker_field(&values, "mez_marker")?,
                    turn_id: required_marker_field(&values, "mez_turn")?,
                    agent_id: required_marker_field(&values, "mez_agent")?,
                    pane_id: required_marker_field(&values, "mez_pane")?,
                    exit_code: exit_code?,
                })
            } else {
                Some(TerminalOscEvent::ShellCommandFinished { exit_code })
            }
        }
        "R" => {
            let values = parse_semicolon_key_values(fields);
            if values.get("mez_foreign_loader").copied() == Some("ready") {
                return Some(TerminalOscEvent::ForeignShellLoaderReady {
                    marker: required_marker_field(&values, "mez_marker")?,
                });
            }
            if values.get("mez_foreign_loader").copied() == Some("exited") {
                return Some(TerminalOscEvent::ForeignShellLoaderExited {
                    marker: required_marker_field(&values, "mez_marker")?,
                    exit_code: required_marker_field(&values, "mez_status")?.parse().ok()?,
                });
            }
            if values.get("mez_payload_receiver").copied() == Some("ready") {
                return Some(TerminalOscEvent::ShellTransactionPayloadReceiverReady {
                    marker: required_marker_field(&values, "mez_marker")?,
                    turn_id: required_marker_field(&values, "mez_turn")?,
                    agent_id: required_marker_field(&values, "mez_agent")?,
                    pane_id: required_marker_field(&values, "mez_pane")?,
                });
            }
            if values.contains_key("mez_protocol") || values.contains_key("mez_event") {
                return parse_managed_shell_protocol_event(&values);
            }
            let token = required_marker_field(&values, "mez_token")?;
            let marker = required_marker_field(&values, "mez_marker")?;
            if values.get("mez_parent").copied() == Some("restored") {
                return Some(TerminalOscEvent::ShellParentRestored {
                    token,
                    marker,
                    exit_code: required_marker_field(&values, "mez_status")?.parse().ok()?,
                });
            }
            match values.get("mez_receiver").copied()? {
                "ready" => Some(TerminalOscEvent::ShellReceiverReady { token, marker }),
                "installed" => Some(TerminalOscEvent::ShellReceiverInstalled { token, marker }),
                "complete" => Some(TerminalOscEvent::ShellReceiverComplete {
                    token,
                    marker,
                    exit_code: required_marker_field(&values, "mez_status")?.parse().ok()?,
                }),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Decodes one versioned shell-neutral managed-adapter event.
fn parse_managed_shell_protocol_event(values: &BTreeMap<&str, &str>) -> Option<TerminalOscEvent> {
    let version = required_marker_field(values, "mez_protocol")
        .and_then(|version| version.parse::<u16>().ok())?;
    let shell = match values.get("mez_shell").copied()? {
        "bash" => ManagedShellAdapter::Bash,
        "fish" => ManagedShellAdapter::Fish,
        "zsh" => ManagedShellAdapter::Zsh,
        _ => return None,
    };
    let token = required_marker_field(values, "mez_token")?;
    let event = match values.get("mez_event").copied()? {
        "adapter-available" => ManagedShellProtocolEvent::AdapterAvailable {
            trigger: match values.get("mez_trigger") {
                Some(_) => Some(required_bounded_field(values, "mez_trigger", 32)?),
                None => None,
            },
        },
        "adapter-unavailable" => ManagedShellProtocolEvent::AdapterUnavailable {
            reason: required_bounded_field(values, "mez_reason", 64)?,
        },
        "receiver-awaiting" => ManagedShellProtocolEvent::ReceiverAwaiting,
        "editor-clear-requested" => ManagedShellProtocolEvent::EditorClearRequested {
            marker: values
                .contains_key("mez_marker")
                .then(|| required_marker_field(values, "mez_marker"))
                .flatten(),
        },
        "editor-cleared" => ManagedShellProtocolEvent::EditorCleared {
            marker: values
                .contains_key("mez_marker")
                .then(|| required_marker_field(values, "mez_marker"))
                .flatten(),
        },
        "editor-held" => ManagedShellProtocolEvent::EditorHeld {
            marker: required_marker_field(values, "mez_marker")?,
        },
        "frame-admitted" => ManagedShellProtocolEvent::FrameAdmitted {
            marker: required_marker_field(values, "mez_marker")?,
        },
        "child-installed" => ManagedShellProtocolEvent::ChildInstalled {
            marker: required_marker_field(values, "mez_marker")?,
        },
        "receiver-rejected" => ManagedShellProtocolEvent::ReceiverRejected {
            marker: values
                .contains_key("mez_marker")
                .then(|| required_marker_field(values, "mez_marker"))
                .flatten(),
            reason: required_bounded_field(values, "mez_reason", 64)?,
        },
        "child-exited" => ManagedShellProtocolEvent::ChildExited {
            marker: required_marker_field(values, "mez_marker")?,
            exit_code: required_marker_field(values, "mez_status")
                .and_then(|status| status.parse::<i32>().ok())?,
        },
        "parent-ready" => {
            let outcome = match values.get("mez_outcome").copied()? {
                "completed" => ManagedShellParentOutcome::Completed,
                "cancelled" => ManagedShellParentOutcome::Cancelled,
                "frame-rejected" => ManagedShellParentOutcome::FrameRejected,
                "source-failed" => ManagedShellParentOutcome::SourceFailed,
                "child-launch-failed" => ManagedShellParentOutcome::ChildLaunchFailed,
                _ => return None,
            };
            ManagedShellProtocolEvent::ParentReady {
                marker: required_marker_field(values, "mez_marker")?,
                outcome,
                exit_code: required_marker_field(values, "mez_status")
                    .and_then(|status| status.parse::<i32>().ok())?,
                proof: match values.get("mez_proof") {
                    Some(_) => Some(required_bounded_field(values, "mez_proof", 256)?),
                    None => None,
                },
            }
        }
        _ => return None,
    };
    Some(TerminalOscEvent::ManagedShell {
        version,
        shell,
        token,
        event,
    })
}

/// Parses semicolon-delimited key-value fields from a shell marker.
fn parse_semicolon_key_values<'a>(
    fields: impl Iterator<Item = &'a str>,
) -> BTreeMap<&'a str, &'a str> {
    fields
        .filter_map(|field| field.split_once('='))
        .collect::<BTreeMap<_, _>>()
}

/// Returns one required non-empty marker field.
fn required_marker_field(values: &BTreeMap<&str, &str>, key: &str) -> Option<String> {
    values
        .get(key)
        .copied()
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// Returns one required non-empty field within its protocol byte bound.
fn required_bounded_field(
    values: &BTreeMap<&str, &str>,
    key: &str,
    max_bytes: usize,
) -> Option<String> {
    required_marker_field(values, key).filter(|value| value.len() <= max_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies managed zsh startup records retain only bounded, authenticated
    /// trigger and failure metadata in the typed protocol.
    #[test]
    fn zsh_receiver_availability_events_parse_bounded_metadata() {
        assert_eq!(
            parse_mez_shell_transaction_osc(
                "133;R;mez_protocol=2;mez_shell=zsh;mez_token=pane-token;mez_event=adapter-available;mez_trigger=escape-n"
            ),
            Some(TerminalOscEvent::ManagedShell {
                version: 2,
                shell: ManagedShellAdapter::Zsh,
                token: "pane-token".to_string(),
                event: ManagedShellProtocolEvent::AdapterAvailable {
                    trigger: Some("escape-n".to_string()),
                },
            })
        );
        assert_eq!(
            parse_mez_shell_transaction_osc(
                "133;R;mez_protocol=2;mez_shell=zsh;mez_token=pane-token;mez_event=adapter-unavailable;mez_reason=no-free-trigger"
            ),
            Some(TerminalOscEvent::ManagedShell {
                version: 2,
                shell: ManagedShellAdapter::Zsh,
                token: "pane-token".to_string(),
                event: ManagedShellProtocolEvent::AdapterUnavailable {
                    reason: "no-free-trigger".to_string(),
                },
            })
        );
        assert_eq!(
            parse_mez_shell_transaction_osc(
                "133;R;mez_protocol=2;mez_shell=zsh;mez_token=pane-token;mez_event=receiver-awaiting"
            ),
            Some(TerminalOscEvent::ManagedShell {
                version: 2,
                shell: ManagedShellAdapter::Zsh,
                token: "pane-token".to_string(),
                event: ManagedShellProtocolEvent::ReceiverAwaiting,
            })
        );

        for payload in [
            "133;R;mez_protocol=2;mez_shell=zsh;mez_token=;mez_event=adapter-available;mez_trigger=escape-m",
            "133;R;mez_protocol=2;mez_shell=zsh;mez_token=pane-token;mez_event=adapter-available;mez_trigger=",
            "133;R;mez_protocol=2;mez_shell=zsh;mez_token=pane-token;mez_event=adapter-unavailable;mez_reason=",
            "133;R;mez_protocol=2;mez_shell=zshhhhhhhhhhhhhhhhh;mez_token=pane-token;mez_event=adapter-available;mez_trigger=escape-m",
            "133;R;mez_protocol=2;mez_shell=zsh;mez_token=pane-token;mez_event=adapter-unavailable;mez_reason=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            assert_eq!(parse_mez_shell_transaction_osc(payload), None, "{payload}");
        }
    }

    /// Verifies private Bash receiver admission and completion records retain
    /// their authenticated token, transaction marker, and final eval status.
    #[test]
    fn bash_receiver_events_parse_authenticated_protocol_fields() {
        assert_eq!(
            parse_mez_shell_transaction_osc(
                "133;R;mez_receiver=ready;mez_token=pane-token;mez_marker=transaction-marker"
            ),
            Some(TerminalOscEvent::ShellReceiverReady {
                token: "pane-token".to_string(),
                marker: "transaction-marker".to_string(),
            })
        );
        assert_eq!(
            parse_mez_shell_transaction_osc(
                "133;R;mez_receiver=installed;mez_token=pane-token;mez_marker=transaction-marker"
            ),
            Some(TerminalOscEvent::ShellReceiverInstalled {
                token: "pane-token".to_string(),
                marker: "transaction-marker".to_string(),
            })
        );
        assert_eq!(
            parse_mez_shell_transaction_osc(
                "133;R;mez_receiver=complete;mez_token=pane-token;mez_marker=transaction-marker;mez_status=7"
            ),
            Some(TerminalOscEvent::ShellReceiverComplete {
                token: "pane-token".to_string(),
                marker: "transaction-marker".to_string(),
                exit_code: 7,
            })
        );
    }

    /// Verifies incomplete, unknown, and non-numeric private receiver records
    /// are discarded instead of becoming transaction state-machine events.
    #[test]
    fn bash_receiver_events_reject_malformed_protocol_fields() {
        for payload in [
            "133;R;mez_receiver=awaiting;mez_token=",
            "133;R;mez_receiver=ready;mez_marker=transaction-marker",
            "133;R;mez_receiver=installed;mez_token=pane-token;mez_marker=",
            "133;R;mez_receiver=unknown;mez_token=pane-token;mez_marker=transaction-marker",
            "133;R;mez_receiver=complete;mez_token=pane-token;mez_marker=transaction-marker;mez_status=invalid",
        ] {
            assert_eq!(parse_mez_shell_transaction_osc(payload), None, "{payload}");
        }
    }

    /// Verifies versioned managed-shell events retain semantic Bash lifecycle
    /// fields, typed outcomes, and optional parent-only readiness proof.
    #[test]
    fn managed_shell_protocol_events_parse_typed_bash_lifecycle() {
        assert_eq!(
            parse_mez_shell_transaction_osc(
                "133;R;mez_protocol=2;mez_shell=bash;mez_token=pane-token;mez_event=editor-held;mez_marker=handoff-marker"
            ),
            Some(TerminalOscEvent::ManagedShell {
                version: 2,
                shell: ManagedShellAdapter::Bash,
                token: "pane-token".to_string(),
                event: ManagedShellProtocolEvent::EditorHeld {
                    marker: "handoff-marker".to_string(),
                },
            })
        );
        assert_eq!(
            parse_mez_shell_transaction_osc(
                "133;R;mez_protocol=2;mez_shell=fish;mez_token=pane-token;mez_event=editor-clear-requested;mez_marker=handoff-marker"
            ),
            Some(TerminalOscEvent::ManagedShell {
                version: 2,
                shell: ManagedShellAdapter::Fish,
                token: "pane-token".to_string(),
                event: ManagedShellProtocolEvent::EditorClearRequested {
                    marker: Some("handoff-marker".to_string()),
                },
            })
        );
        assert_eq!(
            parse_mez_shell_transaction_osc(
                "133;R;mez_protocol=2;mez_shell=zsh;mez_token=pane-token;mez_event=editor-clear-requested"
            ),
            Some(TerminalOscEvent::ManagedShell {
                version: 2,
                shell: ManagedShellAdapter::Zsh,
                token: "pane-token".to_string(),
                event: ManagedShellProtocolEvent::EditorClearRequested { marker: None },
            })
        );
        assert_eq!(
            parse_mez_shell_transaction_osc(
                "133;R;mez_protocol=2;mez_shell=fish;mez_token=pane-token;mez_event=editor-cleared;mez_marker=handoff-marker"
            ),
            Some(TerminalOscEvent::ManagedShell {
                version: 2,
                shell: ManagedShellAdapter::Fish,
                token: "pane-token".to_string(),
                event: ManagedShellProtocolEvent::EditorCleared {
                    marker: Some("handoff-marker".to_string()),
                },
            })
        );
        assert_eq!(
            parse_mez_shell_transaction_osc(
                "133;R;mez_protocol=2;mez_shell=zsh;mez_token=pane-token;mez_event=editor-cleared"
            ),
            Some(TerminalOscEvent::ManagedShell {
                version: 2,
                shell: ManagedShellAdapter::Zsh,
                token: "pane-token".to_string(),
                event: ManagedShellProtocolEvent::EditorCleared { marker: None },
            })
        );
        assert_eq!(
            parse_mez_shell_transaction_osc(
                "133;R;mez_protocol=2;mez_shell=bash;mez_token=pane-token;mez_event=parent-ready;mez_marker=handoff-marker;mez_outcome=source-failed;mez_status=7;mez_proof=fedcba9876543210fedcba9876543210"
            ),
            Some(TerminalOscEvent::ManagedShell {
                version: 2,
                shell: ManagedShellAdapter::Bash,
                token: "pane-token".to_string(),
                event: ManagedShellProtocolEvent::ParentReady {
                    marker: "handoff-marker".to_string(),
                    outcome: ManagedShellParentOutcome::SourceFailed,
                    exit_code: 7,
                    proof: Some("fedcba9876543210fedcba9876543210".to_string()),
                },
            })
        );
    }

    /// Verifies malformed or unbounded semantic records are discarded before
    /// they can acquire editor or transaction ownership.
    #[test]
    fn managed_shell_protocol_events_reject_invalid_fields() {
        let oversized_reason = "a".repeat(65);
        let oversized_proof = "b".repeat(257);
        for payload in [
            "133;R;mez_protocol=x;mez_shell=bash;mez_token=pane-token;mez_event=adapter-available".to_string(),
            "133;R;mez_protocol=2;mez_shell=unknown;mez_token=pane-token;mez_event=adapter-available".to_string(),
            "133;R;mez_protocol=2;mez_shell=bash;mez_token=;mez_event=adapter-available".to_string(),
            "133;R;mez_protocol=2;mez_shell=bash;mez_token=pane-token;mez_event=editor-held;mez_marker=".to_string(),
            format!("133;R;mez_protocol=2;mez_shell=bash;mez_token=pane-token;mez_event=receiver-rejected;mez_reason={oversized_reason}"),
            format!("133;R;mez_protocol=2;mez_shell=bash;mez_token=pane-token;mez_event=parent-ready;mez_marker=handoff-marker;mez_outcome=completed;mez_status=0;mez_proof={oversized_proof}"),
        ] {
            assert_eq!(parse_mez_shell_transaction_osc(&payload), None, "{payload}");
        }
    }

    /// Verifies Fish payload admission records retain the same transaction
    /// correlation fields as start and end boundaries. The runtime must never
    /// release deferred bytes for an uncorrelated terminal OSC record.
    #[test]
    fn fish_payload_receiver_ready_event_parses_transaction_metadata() {
        assert_eq!(
            parse_mez_shell_transaction_osc(
                "133;R;mez_payload_receiver=ready;mez_marker=transaction-marker;mez_turn=turn-1;mez_agent=agent-%1;mez_pane=%1"
            ),
            Some(TerminalOscEvent::ShellTransactionPayloadReceiverReady {
                marker: "transaction-marker".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
            })
        );
        assert_eq!(
            parse_mez_shell_transaction_osc(
                "133;R;mez_payload_receiver=ready;mez_marker=transaction-marker;mez_turn=turn-1;mez_agent=agent-%1"
            ),
            None
        );
    }
}
