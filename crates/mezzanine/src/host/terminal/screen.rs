//! Product-owned decoding for Mezzanine shell integration events.
//!
//! The terminal parser and screen state live in `mez-terminal`. This module
//! only translates generic shell-integration payloads into product transaction
//! semantics.

use std::collections::BTreeMap;

use mez_terminal::TerminalOscEvent;

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
            if values.get("mez_payload_receiver").copied() == Some("ready") {
                return Some(TerminalOscEvent::ShellTransactionPayloadReceiverReady {
                    marker: required_marker_field(&values, "mez_marker")?,
                    turn_id: required_marker_field(&values, "mez_turn")?,
                    agent_id: required_marker_field(&values, "mez_agent")?,
                    pane_id: required_marker_field(&values, "mez_pane")?,
                });
            }
            let token = required_marker_field(&values, "mez_token")?;
            if values.get("mez_receiver").copied() == Some("available") {
                return Some(TerminalOscEvent::ShellReceiverAvailable {
                    token,
                    shell: required_bounded_field(&values, "mez_shell", 16)?,
                    trigger: required_bounded_field(&values, "mez_trigger", 32)?,
                });
            }
            if values.get("mez_receiver").copied() == Some("unavailable") {
                return Some(TerminalOscEvent::ShellReceiverUnavailable {
                    token,
                    shell: required_bounded_field(&values, "mez_shell", 16)?,
                    reason: required_bounded_field(&values, "mez_reason", 64)?,
                });
            }
            if values.get("mez_receiver").copied() == Some("awaiting") {
                return Some(TerminalOscEvent::ShellReceiverAwaiting { token });
            }
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

    /// Verifies managed zsh startup availability records retain only bounded,
    /// authenticated trigger and failure metadata.
    #[test]
    fn zsh_receiver_availability_events_parse_bounded_metadata() {
        assert_eq!(
            parse_mez_shell_transaction_osc(
                "133;R;mez_receiver=available;mez_shell=zsh;mez_token=pane-token;mez_trigger=escape-n"
            ),
            Some(TerminalOscEvent::ShellReceiverAvailable {
                token: "pane-token".to_string(),
                shell: "zsh".to_string(),
                trigger: "escape-n".to_string(),
            })
        );
        assert_eq!(
            parse_mez_shell_transaction_osc(
                "133;R;mez_receiver=unavailable;mez_shell=zsh;mez_token=pane-token;mez_reason=no-free-trigger"
            ),
            Some(TerminalOscEvent::ShellReceiverUnavailable {
                token: "pane-token".to_string(),
                shell: "zsh".to_string(),
                reason: "no-free-trigger".to_string(),
            })
        );

        for payload in [
            "133;R;mez_receiver=available;mez_shell=zsh;mez_token=;mez_trigger=escape-m",
            "133;R;mez_receiver=available;mez_shell=zsh;mez_token=pane-token;mez_trigger=",
            "133;R;mez_receiver=unavailable;mez_shell=zsh;mez_token=pane-token;mez_reason=",
            "133;R;mez_receiver=available;mez_shell=zshhhhhhhhhhhhhhhhh;mez_token=pane-token;mez_trigger=escape-m",
            "133;R;mez_receiver=unavailable;mez_shell=zsh;mez_token=pane-token;mez_reason=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            assert_eq!(parse_mez_shell_transaction_osc(payload), None, "{payload}");
        }
    }

    /// Verifies private Bash receiver admission and completion records retain
    /// their authenticated token, transaction marker, and final eval status.
    #[test]
    fn bash_receiver_events_parse_authenticated_protocol_fields() {
        assert_eq!(
            parse_mez_shell_transaction_osc("133;R;mez_receiver=awaiting;mez_token=pane-token"),
            Some(TerminalOscEvent::ShellReceiverAwaiting {
                token: "pane-token".to_string(),
            })
        );
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
