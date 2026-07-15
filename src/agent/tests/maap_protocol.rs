//! Product MAAP validation adapter tests.
//!
//! Canonical parsing and provider-neutral validation scenarios run in
//! `mez-agent`. This leaf retains coverage for the concrete shell-command
//! policy injected by the product adapter.

use super::*;
use crate::agent::MaapBatchProductValidation;

#[test]
/// Verifies model-authored heredoc shell payloads are rejected at the MAAP
/// validation boundary.
///
/// Mezzanine uses its own shell wrapper internally, but provider-authored
/// heredocs can strand the interactive shell waiting for an unterminated body.
/// The validator should reject those commands before dispatch and point the
/// model toward semantic file actions or patches.
fn maap_batch_rejects_shell_command_heredoc_payloads() {
    let mut action = shell_action("a1");
    if let AgentActionPayload::ShellCommand { command, .. } = &mut action.payload {
        *command = "cat > src/main.rs <<'EOF'\nfn main() {}\nEOF".to_string();
    }
    let batch = MaapBatch {
        protocol: "maap/1".to_string(),
        rationale: "test action batch rationale".to_string(),
        thought: None,
        turn_id: "turn-1".to_string(),
        agent_id: "agent-1".to_string(),
        actions: vec![action],
        final_turn: false,
    };

    let error = batch.validate(&turn(), &[], &[]).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(error.message().contains("heredoc"), "{}", error.message());
    assert!(
        error.message().contains("apply_patch"),
        "{}",
        error.message()
    );
}
