//! Semantic-patch product integration tests.
//!
//! This leaf retains coverage that combines lower semantic planning with the
//! root pane executor and durable action-result projection.

use super::*;

#[test]
/// Verifies mutating semantic action results do not retain generated shell
/// commands or inline patch content in durable structured metadata.
///
/// Patch actions can carry large requested file content. Keeping generated
/// commands in action results caused transcript and continuation context to
/// grow with every generated file.
fn semantic_apply_patch_result_elides_generated_command_content() {
    let turn = turn();
    let secret_content = "do-not-retain-this-inline-content\n".repeat(32);
    let patch = add_file_patch("note.txt", &secret_content);
    let action = AgentAction {
        id: "patch-1".to_string(),
        rationale: String::new(),
        payload: AgentActionPayload::ApplyPatch { patch, strip: None },
    };
    let mut executor = FakePaneShellExecutor {
        output: Some(ShellExecutionOutput {
            exit_code: Some(0),
            signal: None,
            stdout: framed_shell_output("diff -- apply patch\n"),
            stderr: String::new(),
            timed_out: false,
            interrupted: false,
            transport_diagnostics: Default::default(),
        }),
        ..FakePaneShellExecutor::default()
    };

    let result = execute_shell_action_through_pane(
        &turn,
        &action,
        marker(),
        Path::new("/bin/sh"),
        &mut executor,
    )
    .unwrap();

    let executed_command = &executor.requests[0].transaction.command;
    assert!(executed_command.contains("base64"));
    assert!(!executed_command.contains("do-not-retain-this-inline-content"));

    let structured = result.structured_content_json.as_deref().unwrap();
    assert!(
        structured.contains(r#""kind":"apply_patch""#),
        "{structured}"
    );
    assert!(
        structured.contains(r#""generated_command_elided":true"#),
        "{structured}"
    );
    assert!(
        structured.contains(r#""command":"apply_patch""#),
        "{structured}"
    );
    assert!(!structured.contains("cat >"), "{structured}");
    assert!(!structured.contains("python3 - <<"), "{structured}");
    assert!(
        !structured.contains("do-not-retain-this-inline-content"),
        "{structured}"
    );

    let context = action_result_context_content(&result);
    assert!(context.contains("command: apply_patch"), "{context}");
    assert!(!context.contains("cat >"), "{context}");
    assert!(!context.contains("python3 - <<"), "{context}");
    assert!(
        !context.contains("do-not-retain-this-inline-content"),
        "{context}"
    );

    let transcript = action_result_transcript_content(&result);
    assert!(!transcript.contains("python3 - <<"), "{transcript}");
    assert!(
        !transcript.contains("do-not-retain-this-inline-content"),
        "{transcript}"
    );
}
