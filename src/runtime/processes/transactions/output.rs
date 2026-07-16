//! Running shell transaction output retention.

use super::{
    ApplyPatchTransactionPhase, RunningShellTransactionKind, RuntimeSessionService,
    agent_shell_transaction_bytes_before_end_marker, agent_shell_transaction_observation_bytes,
    apply_patch_transaction_phase, latest_agent_shell_transaction_output_lines,
    runtime_shell_transaction_observation_limit,
};

impl RuntimeSessionService {
    pub(crate) fn record_running_shell_transaction_output(&mut self, pane_id: &str, bytes: &[u8]) {
        let output_preview_lines = self.process.settings.terminal_shell_output_preview_lines;
        let mut apply_patch_transport_updates = Vec::new();
        let mut status_line_updates = Vec::new();
        for (marker, transaction) in self.process.running_shell_transactions.iter_mut() {
            if transaction.pane_id == pane_id {
                let observed_bytes = match transaction.kind {
                    RunningShellTransactionKind::AgentAction { .. } => {
                        let transaction_bytes =
                            agent_shell_transaction_bytes_before_end_marker(bytes, marker);
                        agent_shell_transaction_observation_bytes(
                            transaction_bytes,
                            &transaction.command,
                        )
                    }
                    RunningShellTransactionKind::ReadinessProbe
                    | RunningShellTransactionKind::Bootstrap => bytes.to_vec(),
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
                let observation_limit = runtime_shell_transaction_observation_limit(transaction);
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
