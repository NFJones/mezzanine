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
