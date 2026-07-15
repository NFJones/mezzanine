//! Semantic-patch shell transaction execution tests.
//!
//! This leaf verifies generated commands against a real POSIX shell while the
//! product pane executor remains outside the lower crate.

use super::*;

#[test]
/// Verifies generated semantic file-mutation commands emit an actual diff on
/// success.
///
/// The runtime uses this cleaned stdout for normal-mode pane logging, so the
/// lowering itself must produce copyable diff content rather than relying on the
/// model to describe the file change after the action completes.
fn semantic_apply_patch_command_emits_success_diff() {
    let temp = test_temp_dir("semantic-patch-diff");
    let patch = add_file_patch("note.txt", "one\ntwo\n");
    let output = run_apply_patch_action(&temp, &patch);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("diff -- apply patch"), "{stdout}");
    assert!(stdout.contains("+one"), "{stdout}");
    assert!(stdout.contains("+two"), "{stdout}");
    std::fs::remove_dir_all(temp).unwrap();
}

#[test]
/// Verifies generated file-content commands do not inject raw multiline model
/// content into the shell source.
///
/// Large patch actions can contain quotes, command substitutions, and
/// hundreds of lines of source text. Embedding that payload directly in the
/// pane shell input risks leaving the shell waiting for more quoted input and
/// prevents Mezzanine from observing the transaction marker. The lowering
/// should encode payload bytes and decode them inside the transaction instead.
fn semantic_apply_patch_command_encodes_shell_sensitive_content() {
    let temp = test_temp_dir("semantic-patch-encoded");
    let target = temp.join("quoted.txt");
    let content = format!(
        "first line\nrepository's quoted text\n$(not-a-command)\n{}\nlast line\n",
        "middle\n".repeat(64)
    );
    let patch = add_file_patch("quoted.txt", &content);
    let action = AgentAction {
        id: "patch-quoted".to_string(),
        rationale: String::new(),
        payload: AgentActionPayload::ApplyPatch {
            patch: patch.clone(),
            strip: None,
        },
    };
    let plan = local_action_plan(&action).unwrap().unwrap();

    assert!(plan.command.contains("base64"), "{}", plan.command);
    assert!(!plan.command.contains("repository's quoted text"));
    assert!(!plan.command.contains("$(not-a-command)"));
    let output = run_apply_patch_action(&temp, &patch);
    assert!(output.status.success(), "command failed: {}", plan.command);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), content);
    std::fs::remove_dir_all(temp).unwrap();
}

#[test]
/// Verifies generated file-content shell source keeps each physical line below
/// PTY canonical-line limits.
///
/// File mutations are delivered as pane shell input. A single oversized base64
/// line can fill the PTY input line discipline before the newline arrives,
/// preventing the transaction wrapper from reaching its end marker.
fn semantic_apply_patch_command_keeps_encoded_lines_short() {
    let temp = test_temp_dir("semantic-patch-short-lines");
    let patch = add_file_patch("large.txt", &"0123456789abcdef\n".repeat(2048));
    let action = AgentAction {
        id: "patch-large".to_string(),
        rationale: String::new(),
        payload: AgentActionPayload::ApplyPatch { patch, strip: None },
    };
    let plan = local_action_plan(&action).unwrap().unwrap();
    let longest_line = plan.command.lines().map(str::len).max().unwrap_or(0);

    assert!(
        longest_line < 1024,
        "generated shell line should stay PTY-safe; longest={longest_line}"
    );
    assert!(plan.command.contains("base64"), "{}", plan.command);
    std::fs::remove_dir_all(temp).unwrap();
}

#[test]
/// Verifies explicit empty `apply_patch` file content creates a
/// zero-byte regular file.
///
/// Empty file content is distinct from an omitted action payload. The semantic
/// planner must still lower it into a complete shell transaction that writes
/// the empty payload and emits bounded success output.
fn semantic_apply_patch_command_writes_zero_byte_content() {
    let temp = test_temp_dir("semantic-patch-empty");
    let target = temp.join("empty-created.txt");
    let patch = add_file_patch("empty-created.txt", "");
    let output = run_apply_patch_action(&temp, &patch);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "stdout={stdout}\nstderr={stderr}");
    assert_eq!(std::fs::metadata(target).unwrap().len(), 0);
    assert!(stdout.contains("diff -- apply patch"), "{stdout}");

    std::fs::remove_dir_all(temp).unwrap();
}
