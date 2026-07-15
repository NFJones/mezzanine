//! Agent tests for agent runtime behavior.
//!
//! This bounded leaf owns the scenarios for this concern while shared
//! fixtures remain in the parent module.

use super::*;

#[test]
/// Verifies that stateful Fish wrappers run through a Fish-native block and
/// evaluate the command in the active shell context, so stateful operations can
/// persist while still reporting OSC 133 transaction boundaries.
fn fish_stateful_wrapper_uses_active_shell_eval_block() {
    let transaction = ShellTransaction::new(
        marker(),
        "t1",
        "a1",
        "p1",
        Path::new("/bin/fish"),
        "cd /tmp",
    )
    .unwrap();

    let wrapper = transaction.render_stateful_for_classification(ShellClassification::Fish);

    assert!(wrapper.contains("begin\n"));
    assert!(wrapper.contains("eval 'cd /tmp'"));
    assert!(wrapper.contains("set -l MEZ_STATUS $status"));
    assert!(!wrapper.contains("command '/bin/fish' -c"));
}

#[test]
/// Verifies semantic action names remain valid as ordinary shell arguments.
///
/// The semantic-action guard should reject command-position mistakes without
/// blocking legitimate repository searches for action names or prompt text.
fn shell_command_allows_semantic_action_names_as_arguments() {
    let mut action = shell_action("semantic-argument");
    if let AgentActionPayload::ShellCommand { command, .. } = &mut action.payload {
        *command = "rg apply_patch src/agent".to_string();
    }

    assert!(local_action_plan(&action).unwrap().is_some());
}

#[test]
/// Verifies shell command heredoc validation is lexical rather than a raw
/// substring ban.
///
/// Search commands and diagnostics may need to mention `<<` as quoted data or
/// comments. Those should remain valid, while unquoted here-string forms are
/// rejected with the same repair guidance as heredocs.
fn shell_command_heredoc_validation_allows_quoted_mentions_and_rejects_here_strings() {
    let mut quoted = shell_action("quoted");
    if let AgentActionPayload::ShellCommand { command, .. } = &mut quoted.payload {
        *command = "printf '%s\\n' '<<EOF' # <<comment".to_string();
    }
    assert!(local_action_plan(&quoted).unwrap().is_some());

    let mut here_string = shell_action("here-string");
    if let AgentActionPayload::ShellCommand { command, .. } = &mut here_string.payload {
        *command = "cat <<< value".to_string();
    }
    let error = local_action_plan(&here_string).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(error.message().contains("heredoc"), "{}", error.message());
    assert!(
        error.message().contains("apply_patch"),
        "{}",
        error.message()
    );
}

#[test]
/// Verifies model-authored shell commands cannot invoke MAAP action names as
/// shell programs.
///
/// Semantic actions are lowered by Mezzanine, not installed into the pane shell.
/// Rejecting command-position invocations before dispatch prevents the model
/// from turning a recoverable action-choice mistake into `command not found`
/// terminal traffic.
fn shell_command_rejects_semantic_action_invocation_as_shell_program() {
    let mut action = shell_action("semantic-shell");
    if let AgentActionPayload::ShellCommand { command, .. } = &mut action.payload {
        *command = "printf '%s\\n' '*** Begin Patch' | apply_patch".to_string();
    }

    let error = local_action_plan(&action).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(
        error.message().contains("MAAP action `apply_patch`"),
        "{}",
        error.message()
    );
    assert!(
        error.message().contains("emit a `apply_patch` action"),
        "{}",
        error.message()
    );
}

#[test]
/// Verifies turn execution can be converted to transcript entries.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn turn_execution_can_be_converted_to_transcript_entries() {
    let turn = turn();
    let action = shell_action("a1");
    let execution = AgentTurnExecution {
        request: assemble_model_request(
            &ModelProfile {
                provider: "openai".to_string(),
                model: "default".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
            &turn,
            &AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "run pwd".to_string(),
            }])
            .unwrap(),
        )
        .unwrap(),
        response: ModelResponse {
            provider: "openai".to_string(),
            model: "default".to_string(),
            raw_text: "I will inspect the directory.".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: None,
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![ActionResult::running(
            &turn,
            &action,
            vec!["shell command accepted for pane execution".to_string()],
            None,
        )],
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };

    let entries = transcript_entries_for_execution("conv1", 1, 200, &turn, &execution).unwrap();

    assert_eq!(entries[0].sequence, 1);
    assert_eq!(entries[0].role, TranscriptRole::User);
    assert_eq!(entries[0].content, "run pwd");
    assert!(
        entries
            .iter()
            .all(|entry| entry.role != TranscriptRole::System)
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry.role == TranscriptRole::Assistant)
    );
    assert!(entries.iter().any(|entry| {
        entry.role == TranscriptRole::Tool
            && entry
                .content
                .contains("[action_result a1 shell_command running]")
    }));
}
