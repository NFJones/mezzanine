//! Production native settlement coverage for ordered deferred log release.
//!
//! Action workers settle independently; presentation must drain accepted source
//! in response order without repeating a worker merely to reconstruct output.

use super::*;

/// Two native completions arriving in reverse order must release several later
/// progress components exactly once, before terminal cleanup discards ownership.
#[test]
fn runtime_native_settlement_drains_multiple_deferred_logs() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(120, 32).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Native));
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .execute_agent_shell_command(&primary, "exercise deferred logs")
        .unwrap();
    let shell = |id: &str| mez_agent::AgentAction {
        id: id.to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: format!("run {id}"),
            command: format!(": # {id}"),
            interactive: false,
            stateful: false,
            timeout_ms: Some(5_000),
        },
    };
    let say = |id: &str| mez_agent::AgentAction {
        id: id.to_string(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Progress,
            text: id.to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "Verify ordered deferred publication".to_string(),
                actions: vec![
                    shell("FIRST_BARRIER"),
                    say("FIRST_PROGRESS"),
                    say("SECOND_PROGRESS"),
                    shell("SECOND_BARRIER"),
                    say("LAST_PROGRESS"),
                ],
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service.remove_pending_agent_provider_task("turn-1");
    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    let first = service
        .claim_native_shell_action("turn-1", "FIRST_BARRIER")
        .unwrap()
        .unwrap_or_else(|| panic!("native dispatch absent: {execution:?}"));
    service
        .dispatch_stored_running_shell_actions("turn-1")
        .unwrap();
    let second = service
        .claim_native_shell_action("turn-1", "SECOND_BARRIER")
        .unwrap()
        .unwrap();
    let before = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!before.contains("FIRST_PROGRESS"));
    let second_outcome = crate::runtime::execute_native_shell_dispatch(second);
    assert!(
        service
            .complete_native_shell_action(second_outcome.clone())
            .unwrap()
    );
    let held = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!held.contains("FIRST_PROGRESS"));
    assert!(!held.contains("LAST_PROGRESS"));
    let first_outcome = crate::runtime::execute_native_shell_dispatch(first);
    assert!(
        service
            .complete_native_shell_action(first_outcome.clone())
            .unwrap()
    );
    assert!(!service.complete_native_shell_action(first_outcome).unwrap());
    assert!(
        !service
            .complete_native_shell_action(second_outcome)
            .unwrap()
    );
    let rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let mut previous = 0;
    for marker in [
        "FIRST_BARRIER",
        "FIRST_PROGRESS",
        "SECOND_PROGRESS",
        "SECOND_BARRIER",
        "LAST_PROGRESS",
    ] {
        assert_eq!(rows.matches(marker).count(), 1, "{rows}");
        let position = rows.find(marker).unwrap();
        assert!(position >= previous, "{rows}");
        previous = position;
    }
    service.terminate_all_pane_processes().unwrap();
}
