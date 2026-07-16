//! Agent tests for shell execution behavior.
//!
//! This bounded leaf owns the scenarios for this concern while shared
//! fixtures remain in the parent module.

use super::*;

#[test]
/// Verifies omitted shell command timeouts inherit the turn-level budget.
///
/// The shell protocol uses markers for sequencing; ordinary commands without an
/// explicit timeout should not get an additional per-action deadline. Runtime
/// dispatch will cap them with the enclosing turn timeout.
fn semantic_shell_command_plan_leaves_omitted_timeout_unset() {
    let action = AgentAction {
        id: "shell-default-timeout".to_string(),
        rationale: String::new(),
        payload: AgentActionPayload::ShellCommand {
            summary: "List files".to_string(),
            command: "ls".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };

    let plan = local_action_plan(&action).unwrap().unwrap();

    assert_eq!(plan.timeout_ms, None);
}

#[test]
/// Verifies shell command lowering preserves explicit model-provided timeouts.
///
/// Runtime shell transactions use the lowered action plan as the source of
/// execution bounds. Dropping `timeout_ms` here makes slow or stranded commands
/// occupy the pane until the much larger turn-wide timeout expires.
fn semantic_shell_command_plan_preserves_explicit_timeout() {
    let action = AgentAction {
        id: "shell-timeout".to_string(),
        rationale: String::new(),
        payload: AgentActionPayload::ShellCommand {
            summary: "Run bounded grep".to_string(),
            command: "grep -n needle file.txt".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: Some(1500),
        },
    };

    let plan = local_action_plan(&action).unwrap().unwrap();

    assert_eq!(plan.timeout_ms, Some(1500));
}

#[test]
/// Verifies nonzero shell-command action output is decoded before it is
/// returned to model-facing action-result content.
fn shell_action_executor_decodes_encoded_transport_on_nonzero_exit() {
    let turn = turn();
    let action = shell_action("shell-1");
    let mut executor = FakePaneShellExecutor {
        output: Some(ShellExecutionOutput {
            exit_code: Some(7),
            signal: None,
            stdout: "__MEZ_SHELL_OUTPUT_BASE64_BEGIN__\nZmFpbHVyZSBkZXRhaWxzCg==\n__MEZ_SHELL_OUTPUT_BASE64_END__\n".to_string(),
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

    assert_eq!(result.status, ActionStatus::Succeeded);
    assert_eq!(result.content_text(), "failure details\n");
    assert!(
        !result
            .content_text()
            .contains("__MEZ_SHELL_OUTPUT_BASE64_BEGIN__")
    );
}

#[test]
/// Verifies that shell action results infer a signal from exit codes greater
/// than 128 in the POSIX convention (128 + signal number).
fn shell_action_executor_infers_signal_from_high_exit_code() {
    let turn = turn();
    let action = shell_action("shell-signal");
    let mut executor = FakePaneShellExecutor {
        output: Some(ShellExecutionOutput {
            exit_code: Some(137), // 128 + 9 (SIGKILL)
            signal: None,
            stdout: String::new(),
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

    assert_eq!(result.status, ActionStatus::Succeeded);
    let structured = result.structured_content_json.as_deref().unwrap();
    assert!(
        structured.contains(r#""signal":9"#) || structured.contains(r#""signal": 9"#),
        "should infer signal 9 from exit code 137: {structured}"
    );
}

#[test]
/// Verifies shell action executor maps timeout interrupt and nonzero exit.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail. Nonzero exits from plain shell commands are still
/// ordinary command observations and therefore stay model-visible as successful
/// action results with a nonzero `exit_code`.
fn shell_action_executor_maps_timeout_interrupt_and_nonzero_exit() {
    let turn = turn();
    let action = shell_action("shell-1");
    let mut timeout = FakePaneShellExecutor {
        output: Some(ShellExecutionOutput {
            exit_code: None,
            signal: None,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: true,
            interrupted: false,
            transport_diagnostics: Default::default(),
        }),
        ..FakePaneShellExecutor::default()
    };
    let timed_out = execute_shell_action_through_pane(
        &turn,
        &action,
        marker(),
        Path::new("/bin/sh"),
        &mut timeout,
    )
    .unwrap();
    assert_eq!(timed_out.status, ActionStatus::TimedOut);

    let mut interrupted = FakePaneShellExecutor {
        output: Some(ShellExecutionOutput {
            exit_code: None,
            signal: None,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
            interrupted: true,
            transport_diagnostics: Default::default(),
        }),
        ..FakePaneShellExecutor::default()
    };
    let interrupted = execute_shell_action_through_pane(
        &turn,
        &action,
        marker(),
        Path::new("/bin/sh"),
        &mut interrupted,
    )
    .unwrap();
    assert_eq!(interrupted.status, ActionStatus::Interrupted);

    let mut nonzero = FakePaneShellExecutor {
        output: Some(ShellExecutionOutput {
            exit_code: Some(2),
            signal: None,
            stdout: String::new(),
            stderr: "no\n".to_string(),
            timed_out: false,
            interrupted: false,
            transport_diagnostics: Default::default(),
        }),
        ..FakePaneShellExecutor::default()
    };
    let failed = execute_shell_action_through_pane(
        &turn,
        &action,
        marker(),
        Path::new("/bin/sh"),
        &mut nonzero,
    )
    .unwrap();
    assert_eq!(failed.status, ActionStatus::Succeeded);
    assert_eq!(failed.content_texts(), vec!["no\n"]);
    assert!(
        failed
            .structured_content_json
            .as_deref()
            .unwrap_or_default()
            .contains(r#""exit_code":2"#)
    );
}

#[test]
/// Verifies shell action executor receives transaction wrapper and succeeds.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn shell_action_executor_receives_transaction_wrapper_and_succeeds() {
    let turn = turn();
    let action = shell_action("shell-1");
    let mut executor = FakePaneShellExecutor {
        output: Some(ShellExecutionOutput {
            exit_code: Some(0),
            signal: None,
            stdout: framed_shell_output("ok\n"),
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

    assert_eq!(result.status, ActionStatus::Succeeded);
    assert_eq!(result.content_texts(), vec!["ok\n"]);
    let structured = result.structured_content_json.as_deref().unwrap();
    assert!(structured.contains(r#""command":"pwd""#), "{structured}");
    assert!(
        structured.contains(r#""sent_to_pane":true"#),
        "{structured}"
    );
    assert!(
        structured.contains(r#""terminal_observation""#),
        "{structured}"
    );
    assert!(
        structured.contains(r#""stream":"pty_combined""#),
        "{structured}"
    );
    assert!(
        structured.contains(r#""combined_output_bytes":3"#),
        "{structured}"
    );
    assert!(!structured.contains("stdout_bytes"), "{structured}");
    assert!(!structured.contains("stderr_bytes"), "{structured}");
    assert_eq!(executor.requests.len(), 1);
    assert_eq!(executor.requests[0].action_id, "shell-1");
    assert_eq!(executor.requests[0].timeout_ms, Some(1000));
    let wrapper = executor.requests[0].transaction.render_posix();
    assert!(wrapper.contains("MEZ_TURN"));
    assert!(wrapper.contains("MEZ_COMMAND_B64"));
    assert!(wrapper.contains("base64 -d < \"$MEZ_COMMAND_B64\""));
    assert!(!wrapper.contains("\npwd\n"));
    assert!(wrapper.contains("mez_agent"));
    assert!(wrapper.contains("__MEZ_SHELL_OUTPUT_BASE64_BEGIN__"));
}

#[test]
/// Verifies that a normal exit code does not report a signal.
fn shell_action_executor_reports_null_signal_for_normal_exit() {
    let turn = turn();
    let action = shell_action("shell-no-signal");
    let mut executor = FakePaneShellExecutor {
        output: Some(ShellExecutionOutput {
            exit_code: Some(1),
            signal: None,
            stdout: String::new(),
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

    assert_eq!(result.status, ActionStatus::Succeeded);
    let structured = result.structured_content_json.as_deref().unwrap();
    assert!(
        structured.contains(r#""signal":null"#) || structured.contains(r#""signal": null"#),
        "normal exit code should not produce a signal: {structured}"
    );
}

#[test]
/// Verifies that an interrupted shell action reports SIGINT (signal 2)
/// in the terminal_observation.
fn shell_action_executor_reports_sigint_for_interrupted_action() {
    let turn = turn();
    let action = shell_action("shell-interrupt");
    let mut executor = FakePaneShellExecutor {
        output: Some(ShellExecutionOutput {
            exit_code: None,
            signal: None,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
            interrupted: true,
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

    assert_eq!(result.status, ActionStatus::Interrupted);
    let structured = result.structured_content_json.as_deref().unwrap();
    assert!(
        structured.contains(r#""signal":2"#) || structured.contains(r#""signal": 2"#),
        "should report signal 2 (SIGINT) for interrupted action: {structured}"
    );
}

#[test]
/// Verifies that shell action results in the executor path include the marker
/// token in the terminal_observation JSON.
fn shell_action_executor_result_includes_marker_in_terminal_observation() {
    let turn = turn();
    let action = shell_action("shell-marker");
    let mut executor = FakePaneShellExecutor {
        output: Some(ShellExecutionOutput {
            exit_code: Some(0),
            signal: None,
            stdout: String::new(),
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

    assert_eq!(result.status, ActionStatus::Succeeded);
    let structured = result.structured_content_json.as_deref().unwrap();
    assert!(
        structured.contains(r#""marker":"#),
        "terminal_observation in executor path should include marker: {structured}"
    );
}
