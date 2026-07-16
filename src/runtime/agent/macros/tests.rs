//! Focused macro orchestration tests.

use super::helpers::*;
use super::*;

/// Verifies that a macro judge can only continue with an adapted prompt
/// when another scripted step remains and the adapted prompt is non-empty.
/// This protects the harness-owned continuation path from dispatching an
/// empty or out-of-order next macro step after structured provider output.
#[test]
fn macro_judge_decision_validates_adapted_continuation() {
    let decision = macro_judge_decision_from_text(
        r#"{"outcome":"continue_with_adapted_prompt","step_success":true,"rationale":"step passed","adapted_prompt":"Run the next step with the observed id.","user_message":null}"#,
        2,
        0,
    )
    .unwrap();
    assert_eq!(
        decision.outcome,
        MacroJudgeOutcome::ContinueWithAdaptedPrompt
    );
    assert_eq!(
        decision.adapted_prompt.as_deref(),
        Some("Run the next step with the observed id.")
    );

    let missing_prompt = macro_judge_decision_from_text(
        r#"{"outcome":"continue_with_adapted_prompt","step_success":true,"rationale":"step passed","adapted_prompt":null,"user_message":null}"#,
        2,
        0,
    )
    .unwrap_err();
    assert!(
        missing_prompt
            .message()
            .contains("adapted continuation requires adapted_prompt"),
        "{missing_prompt}"
    );

    let final_step = macro_judge_decision_from_text(
        r#"{"outcome":"continue_with_adapted_prompt","step_success":true,"rationale":"step passed","adapted_prompt":"extra work","user_message":null}"#,
        1,
        0,
    )
    .unwrap_err();
    assert!(
        final_step
            .message()
            .contains("cannot adapt a next prompt after the final step"),
        "{final_step}"
    );
}

/// Verifies that recoverable macro judge decisions can retry the current
/// step without advancing the macro, including on the final scripted step.
/// This keeps incomplete-but-fixable subagent output on a runtime-owned
/// retry path instead of forcing failure or out-of-order continuation.
#[test]
fn macro_judge_decision_allows_retry_current_step() {
    let retry = macro_judge_decision_from_text(
        r#"{"outcome":"retry_current_step","step_success":false,"rationale":"the subagent asked for clarification but can retry with a narrower prompt","adapted_prompt":"Inspect the release notes directly and list blockers.","user_message":null}"#,
        2,
        0,
    )
    .unwrap();
    assert_eq!(retry.outcome, MacroJudgeOutcome::RetryCurrentStep);
    assert_eq!(
        retry.adapted_prompt.as_deref(),
        Some("Inspect the release notes directly and list blockers.")
    );

    let final_step = macro_judge_decision_from_text(
        r#"{"outcome":"retry_current_step","step_success":false,"rationale":"the final step was incomplete but recoverable","adapted_prompt":null,"user_message":null}"#,
        1,
        0,
    )
    .unwrap();
    assert_eq!(final_step.outcome, MacroJudgeOutcome::RetryCurrentStep);
}

/// Verifies that terminal macro judge decisions are position-sensitive:
/// `finish_success` is accepted only after the last scripted step and
/// `stop_failure` must include a user-visible explanation. These checks
/// keep invalid structured judge output from becoming a stranded parent
/// turn or a generic missing-MAAP failure.
#[test]
fn macro_judge_decision_validates_terminal_outcomes() {
    let finish = macro_judge_decision_from_text(
        r#"{"outcome":"finish_success","step_success":true,"rationale":"all steps completed","adapted_prompt":null,"user_message":null}"#,
        2,
        1,
    )
    .unwrap();
    assert_eq!(finish.outcome, MacroJudgeOutcome::FinishSuccess);

    let early_finish = macro_judge_decision_from_text(
        r#"{"outcome":"finish_success","step_success":true,"rationale":"done early","adapted_prompt":null,"user_message":null}"#,
        2,
        0,
    )
    .unwrap_err();
    assert!(
        early_finish
            .message()
            .contains("cannot finish before the final step"),
        "{early_finish}"
    );

    let missing_message = macro_judge_decision_from_text(
        r#"{"outcome":"stop_failure","step_success":false,"rationale":"step failed","adapted_prompt":null,"user_message":null}"#,
        2,
        0,
    )
    .unwrap_err();
    assert!(
        missing_message
            .message()
            .contains("stop_failure requires user_message"),
        "{missing_message}"
    );
}

/// Verifies that `macro_message_recipient_agent_id` trims whitespace
/// from the extracted agent id after the `agent:` prefix, so that
/// recipients like `"agent: agent-%3"` or `"agent:agent-%3 "` are
/// correctly routed through the macro bridge instead of silently
/// falling back to plain MMP delivery.
#[test]
fn macro_recipient_trims_whitespace_after_agent_prefix() {
    // Leading whitespace after `agent:`
    assert_eq!(
        macro_message_recipient_agent_id("agent: agent-%5"),
        Some("agent-%5".to_string())
    );
    // Trailing whitespace
    assert_eq!(
        macro_message_recipient_agent_id("agent:agent-%7 "),
        Some("agent-%7".to_string())
    );
    // Both leading and trailing whitespace
    assert_eq!(
        macro_message_recipient_agent_id("agent:  agent-%9  "),
        Some("agent-%9".to_string())
    );
    // Only whitespace after agent: should still be filtered (empty after trim)
    assert_eq!(macro_message_recipient_agent_id("agent:   "), None);
    // Normal untrimmed case still works
    assert_eq!(
        macro_message_recipient_agent_id("agent:agent-%3"),
        Some("agent-%3".to_string())
    );
    // Bare agent-% pattern (no agent: prefix) still works
    assert_eq!(
        macro_message_recipient_agent_id("agent-%12"),
        Some("agent-%12".to_string())
    );
}

/// Verifies that `deregister_macro_managed_subagent` removes an agent
/// from the macro-managed set, preventing stale entries from accumulating
/// and preventing recycled pane ids from hijacking macro bridge routing.
#[test]
fn deregister_macro_managed_removes_agent_from_set() {
    let fixture = crate::test_support::runtime::RuntimeServiceFixture::new();
    let mut service = fixture.build();
    let agent_id = "agent-%99";

    // Initially empty
    assert!(!service.macro_managed_subagent_agents.contains_key(agent_id));

    // Register
    service.register_macro_managed_subagent(agent_id, "turn-99", "agent-%1", "test-macro");
    assert!(service.macro_managed_subagent_agents.contains_key(agent_id));

    // Deregister
    service.deregister_macro_managed_subagent(agent_id);
    assert!(!service.macro_managed_subagent_agents.contains_key(agent_id));

    // Deregistering an already-absent id is a no-op
    service.deregister_macro_managed_subagent(agent_id);
    assert!(!service.macro_managed_subagent_agents.contains_key(agent_id));
}
