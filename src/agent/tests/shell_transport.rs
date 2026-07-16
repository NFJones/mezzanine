//! Agent tests for shell transport behavior.
//!
//! This bounded leaf owns the scenarios for this concern while shared
//! fixtures remain in the parent module.

use super::*;
#[test]
/// Verifies isolated shell transactions can encode child output before it
/// crosses the pane PTY and that postprocessing restores the decoded output
/// for model-facing action results.
fn posix_wrapper_can_encode_child_output_for_model_transport() {
    let command = "printf '%s\n' VISIBLE_STDOUT; printf '%s\n' VISIBLE_STDERR >&2";
    let transaction =
        ShellTransaction::new(marker(), "t1", "a1", "p1", Path::new("/bin/sh"), command)
            .unwrap()
            .with_output_transport(ShellTransactionOutputTransport::Base64);
    let input = transaction.render_for_classification_input(ShellClassification::PosixSh);
    let output = run_sh_transaction(&input, "");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let action = AgentAction {
        id: "shell-transport".to_string(),
        rationale: String::new(),
        payload: AgentActionPayload::ShellCommand {
            summary: "capture child output".to_string(),
            command: command.to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };

    let decoded = postprocess_shell_action_success_output(&action, stdout.to_string());

    assert!(
        output.status.success(),
        "status={:?} stdout={stdout:?} stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !stdout.contains("VISIBLE_STDOUT"),
        "raw PTY output should carry encoded child output: {stdout:?}"
    );
    assert!(
        !decoded.contains("\u{1b}]133;D;"),
        "decoded model-facing output should not expose transaction marker bytes: {decoded:?}"
    );
    assert!(decoded.contains("VISIBLE_STDOUT"), "{decoded:?}");
    assert!(decoded.contains("VISIBLE_STDERR"), "{decoded:?}");
}
