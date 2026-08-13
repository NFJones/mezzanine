//! Running shell transaction output retention.

use super::{
    ApplyPatchTransactionPhase, RunningShellTransactionKind, RuntimeSessionService,
    agent_shell_transaction_bytes_before_end_marker, agent_shell_transaction_observation_bytes,
    apply_patch_transaction_phase, find_byte_subsequence,
    latest_agent_shell_transaction_output_lines, runtime_shell_transaction_observation_limit,
};
use crate::host::terminal::parse_mez_shell_transaction_osc;
use mez_terminal::TerminalOscEvent;

/// Maximum bytes retained while waiting for one mandatory OSC start boundary.
const SHELL_TRANSACTION_START_BOUNDARY_LIMIT_BYTES: usize = 4096;
/// Maximum bytes retained while waiting for one matching OSC end boundary.
const SHELL_TRANSACTION_END_BOUNDARY_LIMIT_BYTES: usize = 4096;
/// Maximum bytes retained while waiting for one private control OSC terminator.
const SHELL_TRANSACTION_CONTROL_OSC_LIMIT_BYTES: usize = 4096;

/// Returns bytes after a complete matching OSC start marker.
///
/// Bytes before the boundary are wrapper echo or pane noise and are discarded.
/// An incomplete matching boundary is retained across PTY reads up to the
/// protocol marker limit.
fn shell_transaction_bytes_after_start_marker(
    pending: &mut Vec<u8>,
    bytes: &[u8],
    marker: &str,
) -> Option<Vec<u8>> {
    pending.extend_from_slice(bytes);
    let marker_prefix = format!("\x1b]133;C;mez_marker={marker};");
    if let Some(start) = find_byte_subsequence(pending, marker_prefix.as_bytes()) {
        let terminator_search_start = start + marker_prefix.len();
        let terminator = pending[terminator_search_start..]
            .iter()
            .enumerate()
            .find_map(|(offset, byte)| match *byte {
                0x07 => Some(terminator_search_start + offset + 1),
                0x1b if pending.get(terminator_search_start + offset + 1) == Some(&b'\\') => {
                    Some(terminator_search_start + offset + 2)
                }
                _ => None,
            });
        if let Some(end) = terminator {
            let transaction_bytes = pending[end..].to_vec();
            pending.clear();
            return Some(transaction_bytes);
        }
        if start > 0 {
            pending.drain(..start);
        }
    } else {
        let max_prefix_len = marker_prefix.len().saturating_sub(1).min(pending.len());
        let retained_len = (1..=max_prefix_len)
            .rev()
            .find(|length| pending[pending.len() - length..] == marker_prefix.as_bytes()[..*length])
            .unwrap_or(0);
        if retained_len == 0 {
            pending.clear();
        } else {
            pending.drain(..pending.len() - retained_len);
        }
    }
    if pending.len() > SHELL_TRANSACTION_START_BOUNDARY_LIMIT_BYTES {
        pending.clear();
    }
    None
}

/// Returns bytes before a complete matching OSC end marker while retaining a
/// possible marker fragment across PTY reads.
fn shell_transaction_bytes_before_end_marker(
    pending: &mut Vec<u8>,
    bytes: &[u8],
    marker: &str,
) -> Vec<u8> {
    pending.extend_from_slice(bytes);
    let before_end = agent_shell_transaction_bytes_before_end_marker(pending, marker);
    if before_end.len() < pending.len() {
        let transaction_bytes = before_end.to_vec();
        pending.clear();
        return transaction_bytes;
    }

    const END_PREFIX: &[u8] = b"\x1b]133;D;";
    if let Some(start) = find_byte_subsequence(pending, END_PREFIX) {
        let candidate = &pending[start..];
        let complete_nonmatching =
            candidate.contains(&0x07) || candidate.windows(2).any(|window| window == [0x1b, b'\\']);
        if !complete_nonmatching && candidate.len() <= SHELL_TRANSACTION_END_BOUNDARY_LIMIT_BYTES {
            let transaction_bytes = pending[..start].to_vec();
            pending.drain(..start);
            return transaction_bytes;
        }
        return std::mem::take(pending);
    }

    let partial_len = (1..END_PREFIX.len().min(pending.len() + 1))
        .rev()
        .find(|length| pending.ends_with(&END_PREFIX[..*length]))
        .unwrap_or(0);
    let transaction_end = pending.len().saturating_sub(partial_len);
    let transaction_bytes = pending[..transaction_end].to_vec();
    pending.drain(..transaction_end);
    transaction_bytes
}

/// Returns complete UTF-8 bytes while retaining only an incomplete trailing scalar.
fn complete_transaction_utf8_bytes(pending: &mut Vec<u8>, bytes: &[u8]) -> Vec<u8> {
    pending.extend_from_slice(bytes);
    let complete_len = match std::str::from_utf8(pending) {
        Ok(_) => pending.len(),
        Err(error) if error.error_len().is_none() => error.valid_up_to(),
        Err(_) => pending.len(),
    };
    pending.drain(..complete_len).collect()
}

/// Returns whether one complete OSC record is Mezzanine control framing for
/// the active transaction marker.
fn shell_transaction_control_osc_matches_marker(record: &[u8], marker: &str) -> bool {
    let payload_end = if record.ends_with(b"\x07") {
        record.len().saturating_sub(1)
    } else if record.ends_with(b"\x1b\\") {
        record.len().saturating_sub(2)
    } else {
        return false;
    };
    let Some(payload) = record
        .get(2..payload_end)
        .and_then(|payload| std::str::from_utf8(payload).ok())
    else {
        return false;
    };
    let Some(event) = parse_mez_shell_transaction_osc(payload) else {
        return false;
    };
    match event {
        TerminalOscEvent::ShellReceiverReady {
            marker: event_marker,
            ..
        }
        | TerminalOscEvent::ShellReceiverInstalled {
            marker: event_marker,
            ..
        }
        | TerminalOscEvent::ShellReceiverComplete {
            marker: event_marker,
            ..
        }
        | TerminalOscEvent::ShellTransactionPayloadReceiverReady {
            marker: event_marker,
            ..
        }
        | TerminalOscEvent::ShellTransactionStart {
            marker: event_marker,
            ..
        }
        | TerminalOscEvent::ShellTransactionEnd {
            marker: event_marker,
            ..
        } => event_marker == marker,
        _ => false,
    }
}

/// Removes complete marker-correlated Mezzanine control OSC records while
/// preserving child-owned OSC output and possible private-record fragments.
fn shell_transaction_bytes_without_control_osc(
    pending: &mut Vec<u8>,
    bytes: &[u8],
    marker: &str,
) -> Vec<u8> {
    const CONTROL_OSC_PREFIX: &[u8] = b"\x1b]133;";

    pending.extend_from_slice(bytes);
    let mut filtered = Vec::with_capacity(pending.len());
    let mut cursor = 0usize;
    loop {
        let Some(relative_start) = find_byte_subsequence(&pending[cursor..], CONTROL_OSC_PREFIX)
        else {
            let partial_len = (1..CONTROL_OSC_PREFIX.len().min(pending.len() + 1))
                .rev()
                .find(|length| pending.ends_with(&CONTROL_OSC_PREFIX[..*length]))
                .unwrap_or(0);
            let emit_end = pending.len().saturating_sub(partial_len);
            filtered.extend_from_slice(&pending[cursor..emit_end]);
            pending.drain(..emit_end);
            return filtered;
        };
        let start = cursor + relative_start;
        filtered.extend_from_slice(&pending[cursor..start]);
        let terminator_search_start = start + CONTROL_OSC_PREFIX.len();
        let terminator = pending[terminator_search_start..]
            .iter()
            .enumerate()
            .find_map(|(offset, byte)| match *byte {
                0x07 => Some(terminator_search_start + offset + 1),
                0x1b if pending.get(terminator_search_start + offset + 1) == Some(&b'\\') => {
                    Some(terminator_search_start + offset + 2)
                }
                _ => None,
            });
        let Some(end) = terminator else {
            if pending.len().saturating_sub(start) > SHELL_TRANSACTION_CONTROL_OSC_LIMIT_BYTES {
                filtered.extend_from_slice(&pending[start..]);
                pending.clear();
            } else {
                pending.drain(..start);
            }
            return filtered;
        };
        if !shell_transaction_control_osc_matches_marker(&pending[start..end], marker) {
            filtered.extend_from_slice(&pending[start..end]);
        }
        cursor = end;
    }
}

impl RuntimeSessionService {
    pub(crate) fn record_running_shell_transaction_output(&mut self, pane_id: &str, bytes: &[u8]) {
        let output_preview_lines = self.process.settings.terminal_shell_output_preview_lines;
        let mut apply_patch_transport_updates = Vec::new();
        let mut status_line_updates = Vec::new();
        for (marker, transaction) in self.process.running_shell_transactions.iter_mut() {
            if transaction.pane_id == pane_id {
                // A managed-Bash inner end marker closes command output even
                // though callback completion defers transaction settlement.
                if self
                    .process
                    .shell_receiver_pending_ends
                    .contains_key(marker)
                {
                    continue;
                }
                let acknowledged_bytes = if let Some(remaining) = self
                    .process
                    .shell_transaction_receiver_acknowledgements
                    .get_mut(marker)
                {
                    let mut filtered = Vec::with_capacity(bytes.len());
                    for byte in bytes {
                        if *byte == mez_mux::process::SHELL_INPUT_RECORD_ACK_BYTE && *remaining > 0
                        {
                            *remaining -= 1;
                        } else {
                            filtered.push(*byte);
                        }
                    }
                    filtered
                } else {
                    bytes.to_vec()
                };
                let requires_unobserved_start = self
                    .process
                    .shell_transaction_require_start_markers
                    .contains(marker)
                    && !self
                        .process
                        .shell_transaction_started_markers
                        .contains(marker);
                let boundary_bytes = if requires_unobserved_start {
                    let Some(boundary_bytes) = shell_transaction_bytes_after_start_marker(
                        self.process
                            .shell_transaction_start_boundary_pending
                            .entry(marker.clone())
                            .or_default(),
                        &acknowledged_bytes,
                        marker,
                    ) else {
                        continue;
                    };
                    boundary_bytes
                } else {
                    acknowledged_bytes
                };
                let transaction_bytes = shell_transaction_bytes_before_end_marker(
                    self.process
                        .shell_transaction_end_boundary_pending
                        .entry(marker.clone())
                        .or_default(),
                    &boundary_bytes,
                    marker,
                );
                let output_bytes = shell_transaction_bytes_without_control_osc(
                    self.process
                        .shell_transaction_control_osc_pending
                        .entry(marker.clone())
                        .or_default(),
                    &transaction_bytes,
                    marker,
                );
                let complete_bytes = complete_transaction_utf8_bytes(
                    self.process
                        .shell_transaction_output_utf8_pending
                        .entry(marker.clone())
                        .or_default(),
                    &output_bytes,
                );
                let observed_bytes = match transaction.kind {
                    RunningShellTransactionKind::AgentAction { .. } => {
                        agent_shell_transaction_observation_bytes(
                            &complete_bytes,
                            &transaction.command,
                        )
                    }
                    RunningShellTransactionKind::FocusedShellHook
                    | RunningShellTransactionKind::ReadinessProbe
                    | RunningShellTransactionKind::Bootstrap
                    | RunningShellTransactionKind::ShellIdentityProbe { .. }
                    | RunningShellTransactionKind::PathResolution { .. }
                    | RunningShellTransactionKind::EnvironmentEvidence { .. }
                    | RunningShellTransactionKind::BubblewrapCapabilityProbe { .. } => {
                        complete_bytes
                    }
                };
                if let RunningShellTransactionKind::AgentAction { action_id } = &transaction.kind
                    && apply_patch_transaction_phase(&transaction.command)
                        == Some(ApplyPatchTransactionPhase::Read)
                    && !observed_bytes.is_empty()
                {
                    apply_patch_transport_updates.push((
                        Self::apply_patch_batch_state_key(&transaction.turn_id, action_id),
                        observed_bytes.clone(),
                    ));
                }
                transaction.observed_output_bytes = transaction
                    .observed_output_bytes
                    .saturating_add(observed_bytes.len());
                let observation_limit = runtime_shell_transaction_observation_limit(
                    transaction,
                    self.process
                        .sandboxed_shell_transaction_markers
                        .contains(marker),
                );
                if transaction.observed_output_preview.len() >= observation_limit {
                    if !observed_bytes.is_empty() {
                        transaction.observed_output_truncated = true;
                    }
                    continue;
                }
                let remaining =
                    observation_limit.saturating_sub(transaction.observed_output_preview.len());
                let text = String::from_utf8_lossy(&observed_bytes);
                let mut appended = 0usize;
                for ch in text.chars() {
                    let char_len = ch.len_utf8();
                    if appended + char_len > remaining {
                        transaction.observed_output_truncated = true;
                        break;
                    }
                    transaction.observed_output_preview.push(ch);
                    appended += char_len;
                }
                if appended < text.len() {
                    transaction.observed_output_truncated = true;
                }
                if let RunningShellTransactionKind::AgentAction { action_id } = &transaction.kind
                    && apply_patch_transaction_phase(&transaction.command)
                        != Some(ApplyPatchTransactionPhase::Read)
                {
                    let lines = latest_agent_shell_transaction_output_lines(
                        &transaction.observed_output_preview,
                        output_preview_lines,
                    );
                    if !lines.is_empty() {
                        status_line_updates.push((
                            transaction.turn_id.clone(),
                            action_id.clone(),
                            transaction.pane_id.clone(),
                            lines,
                        ));
                    }
                }
            }
        }
        for (state_key, transport_chunk) in apply_patch_transport_updates {
            self.append_apply_patch_batch_transport(&state_key, &transport_chunk);
        }
        for (turn_id, action_id, pane_id, lines) in status_line_updates {
            if self.agent_shell_transaction_action_shows_live_output(&turn_id, &action_id) {
                let _ = self
                    .append_agent_shell_output_status_lines_to_terminal_buffer(&pane_id, &lines);
            }
        }
    }
}
