//! Running shell transaction output retention.

use super::{
    ApplyPatchTransactionPhase, RunningShellTransactionKind, RuntimeSessionService,
    agent_shell_transaction_bytes_before_end_marker, agent_shell_transaction_observation_bytes,
    apply_patch_transaction_phase, find_byte_subsequence,
    latest_agent_shell_transaction_output_lines, runtime_shell_transaction_observation_limit,
};

/// Maximum bytes retained while waiting for one mandatory OSC start boundary.
const SHELL_TRANSACTION_START_BOUNDARY_LIMIT_BYTES: usize = 4096;

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

impl RuntimeSessionService {
    pub(crate) fn record_running_shell_transaction_output(&mut self, pane_id: &str, bytes: &[u8]) {
        let output_preview_lines = self.process.settings.terminal_shell_output_preview_lines;
        let mut apply_patch_transport_updates = Vec::new();
        let mut status_line_updates = Vec::new();
        for (marker, transaction) in self.process.running_shell_transactions.iter_mut() {
            if transaction.pane_id == pane_id {
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
                        bytes,
                        marker,
                    ) else {
                        continue;
                    };
                    boundary_bytes
                } else {
                    bytes.to_vec()
                };
                let transaction_bytes =
                    agent_shell_transaction_bytes_before_end_marker(&boundary_bytes, marker);
                let complete_bytes = complete_transaction_utf8_bytes(
                    self.process
                        .shell_transaction_output_utf8_pending
                        .entry(marker.clone())
                        .or_default(),
                    transaction_bytes,
                );
                let observed_bytes = match transaction.kind {
                    RunningShellTransactionKind::AgentAction { .. } => {
                        agent_shell_transaction_observation_bytes(
                            &complete_bytes,
                            &transaction.command,
                        )
                    }
                    RunningShellTransactionKind::ReadinessProbe
                    | RunningShellTransactionKind::Bootstrap
                    | RunningShellTransactionKind::PathResolution { .. }
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
                if let RunningShellTransactionKind::AgentAction { action_id } = &transaction.kind {
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
