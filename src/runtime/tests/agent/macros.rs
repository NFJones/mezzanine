//! Runtime tests for agent macros behavior.

use super::*;

/// Verifies `/list-macros` displays the effective pane macro catalog with the
/// same `#macro` invocation syntax accepted by explicit macro prompts. This
/// gives users a discoverable way to inspect configured prompt workflows before
/// invoking one.
#[test]
fn runtime_agent_shell_list_macros_displays_effective_catalog() {
    let config_root = temp_root("runtime-list-macros");
    let macro_dir = config_root.join("macros/release-check");
    fs::create_dir_all(&macro_dir).unwrap();
    fs::write(
        macro_dir.join("MACRO.md"),
        "---\nname: release-check\ndescription: Release readiness workflow\n---\n\n# Macro: release-check\n\n## Steps\n\n1. Inspect release notes.\n2. Summarize release blockers.\n",
    )
    .unwrap();
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_config_root(config_root);

    let response = service
        .execute_agent_shell_command(&primary, "/list-macros")
        .unwrap();

    assert!(response.contains("## Macros"), "{response}");
    assert!(response.contains("Start a prompt with `#`"), "{response}");
    assert!(
        response.contains("`#<macro-name> [additional context]`"),
        "{response}"
    );
    assert!(
        response.contains("| `#release-check` | user | 2 | Release readiness workflow |"),
        "{response}"
    );
}

/// Verifies unknown `#macro` prompt submissions fail before starting provider
/// work and point users to `/list-macros` for discovery.
///
/// Macro execution is implemented by later runtime orchestration work, but the
/// UX layer should already reject misspelled or unavailable macro names instead
/// of treating them as ordinary free-form prompts.
#[test]
fn runtime_agent_shell_unknown_macro_prompt_reports_list_macros_guidance() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_config_root(temp_root("runtime-unknown-macro"));

    let response = service
        .execute_agent_shell_command(&primary, "#missing-macro do the work")
        .unwrap();

    assert!(
        response.contains("agent macro error: unknown macro `#missing-macro`"),
        "{response}"
    );
    assert!(response.contains("/list-macros"), "{response}");
    assert!(service.agent_turn_ledger.turns().is_empty());
}

/// Verifies a known `#macro` prompt starts runtime orchestration instead of
/// falling through as an ordinary user prompt. The macro run should create one
/// persistent macro-managed child and give the parent turn the ordered steps,
/// user context, and child recipient needed to drive the sequence.
#[test]
fn runtime_agent_shell_known_macro_prompt_starts_orchestration() {
    let config_root = temp_root("runtime-known-macro");
    let macro_dir = config_root.join("macros/release-check");
    fs::create_dir_all(&macro_dir).unwrap();
    fs::write(
        macro_dir.join("MACRO.md"),
        "---\nname: release-check\ndescription: Release readiness workflow\n---\n\n# Macro: release-check\n\n## Steps\n\n1. /loop inspect release notes for the requested version.\n2. Summarize release blockers.\n",
    )
    .unwrap();
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(120, 40).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_config_root(config_root);

    let response = service
        .execute_agent_shell_command(&primary, "#release-check for v1.2")
        .unwrap();

    assert!(response.contains(r#""kind":"turn_started""#), "{response}");
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("user> /loop inspect release notes for the requested version."),
        "{pane_text}"
    );
    let step_index = pane_text
        .find("macro release-check (1/2): dispatched to agent-%")
        .expect("parent transcript should include the first macro dispatch status");
    let prompt_index = pane_text
        .find("user> /loop inspect release notes for the requested version.")
        .expect("parent transcript should include the first macro prompt line");
    assert!(step_index < prompt_index, "{pane_text}");
    assert!(
        pane_text.contains("macro release-check: started; 2 steps; worker agent-%"),
        "{pane_text}"
    );
    let macro_children = service
        .macro_managed_subagent_agents
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(macro_children.len(), 1, "{macro_children:?}");
    let child_agent_id = &macro_children[0];
    assert!(child_agent_id.starts_with("agent-%"), "{child_agent_id}");
    let child_pane_id = child_agent_id
        .strip_prefix("agent-")
        .expect("macro child agent should identify its pane");
    let loop_state = service
        .agent_loops_by_pane
        .get(child_pane_id)
        .expect("macro /loop step should start a loop controller in the child pane");
    assert_eq!(
        loop_state.original_prompt,
        "inspect release notes for the requested version.\n\nUser additional context for this macro invocation:\nfor v1.2"
    );
    let loop_turn_id = service
        .agent_loop_turns
        .iter()
        .find(|(_, loop_turn)| loop_turn.pane_id == child_pane_id)
        .map(|(turn_id, _)| turn_id)
        .expect("macro /loop step should start its first loop-owned work turn");
    let completion = loop_state
        .completion
        .as_ref()
        .expect("macro step should join the logical loop controller result");
    assert_eq!(completion.child_agent_id, *child_agent_id);
    assert!(
        !service
            .joined_subagent_dependencies
            .contains_key(loop_turn_id)
    );
    let parent_turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.agent_id == "agent-%1")
        .cloned()
        .expect("parent macro orchestration turn should exist");
    let macro_run = service
        .macro_runs_by_parent_turn
        .get(parent_turn.turn_id.as_str())
        .expect("macro run state should be keyed by parent turn");
    assert_eq!(macro_run.run_id, parent_turn.turn_id);
    assert_eq!(macro_run.parent_turn_id, parent_turn.turn_id);
    assert_eq!(macro_run.parent_agent_id, parent_turn.agent_id);
    assert_eq!(macro_run.parent_pane_id, "%1");
    assert_eq!(macro_run.child_agent_id, *child_agent_id);
    assert_eq!(macro_run.macro_name, "release-check");
    assert_eq!(macro_run.macro_description, "Release readiness workflow");
    assert_eq!(macro_run.invocation_prompt, "#release-check for v1.2");
    assert_eq!(macro_run.invocation_context.as_deref(), Some("for v1.2"));
    assert_eq!(macro_run.current_step, 0);
    assert_eq!(macro_run.steps.len(), 2);
    assert_eq!(macro_run.steps[0].index, 0);
    assert_eq!(
        macro_run.steps[0].scripted_prompt,
        "/loop inspect release notes for the requested version."
    );
    assert_eq!(macro_run.steps[1].index, 1);
    assert_eq!(
        macro_run.steps[1].scripted_prompt,
        "Summarize release blockers."
    );
    let orchestration_context = service
        .agent_turn_contexts
        .values()
        .map(|context| {
            context
                .blocks
                .iter()
                .map(|block| block.content.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .find(|content| content.contains("Agent macro invocation: #release-check"))
        .expect("macro orchestration context should exist");
    assert!(
        orchestration_context.contains(&format!(
            "Persistent subagent recipient: agent:{child_agent_id}"
        )),
        "{orchestration_context}"
    );
    assert!(
        orchestration_context.contains("Step 1 has already been sent to the persistent subagent by the runtime; wait for that result before judging whether to continue."),
        "{orchestration_context}"
    );
    assert!(
        orchestration_context.contains(&format!("The runtime submits every later macro step to `agent:{child_agent_id}` after a valid structured judge decision.")),
        "{orchestration_context}"
    );
    assert!(
        orchestration_context.contains("Judge each completed step with one outcome: continue, continue_with_adapted_prompt, stop_failure, or finish_success"),
        "{orchestration_context}"
    );
    assert!(
        orchestration_context.contains("User additional context:\nfor v1.2"),
        "{orchestration_context}"
    );
    assert!(
        orchestration_context.contains("1. /loop inspect release notes for the requested version."),
        "{orchestration_context}"
    );
    assert!(
        orchestration_context.contains("slash commands such as /loop remain valid"),
        "{orchestration_context}"
    );

    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a completed macro step asks the main model for a structured judge
/// decision and lets the runtime dispatch the next scripted step itself. This
/// protects harness-owned continuation from regressing to a parent MAAP
/// `send_message` requirement after the first child step finishes.
#[test]
fn runtime_agent_macro_judge_dispatches_next_step_after_child_result() {
    let config_root = temp_root("runtime-macro-judge-next-step");
    let macro_dir = config_root.join("macros/release-check");
    fs::create_dir_all(&macro_dir).unwrap();
    fs::write(
        macro_dir.join("MACRO.md"),
        "---\nname: release-check\ndescription: Release readiness workflow\n---\n\n# Macro: release-check\n\n## Steps\n\n1. Inspect release notes.\n2. Summarize release blockers.\n",
    )
    .unwrap();
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(120, 40).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_config_root(config_root);

    let response = service
        .execute_agent_shell_command(&primary, "#release-check for v1.2")
        .unwrap();
    assert!(response.contains(r#""kind":"turn_started""#), "{response}");
    let parent_turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.agent_id == "agent-%1")
        .cloned()
        .expect("parent macro orchestration turn should exist");
    let first_child_turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.cooperation_mode.as_deref() == Some("macro-step"))
        .cloned()
        .expect("first runtime-owned macro step should create a child turn");

    service
        .agent_turn_ledger
        .finish_turn(&first_child_turn.turn_id, AgentTurnState::Completed)
        .unwrap();
    service
        .emit_subagent_task_result_for_state(&first_child_turn, AgentTurnState::Completed)
        .unwrap();
    assert_eq!(
        service.macro_judge_step_index_for_turn(&parent_turn.turn_id),
        Some(0)
    );

    let judge_provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: r#"{"outcome":"continue","step_success":true,"rationale":"step one satisfied its intent","adapted_prompt":null,"user_message":null}"#.to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: None,
            provider_transcript_events: Vec::new(),
        },
    };
    service
        .execute_agent_turn_with_provider(
            &parent_turn.turn_id,
            &judge_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    let macro_run = service
        .macro_runs_by_parent_turn
        .get(parent_turn.turn_id.as_str())
        .expect("macro should continue after a valid judge decision");
    assert_eq!(macro_run.current_step, 1);
    assert!(macro_run.steps[0].task_result.as_ref().unwrap().success);
    assert_eq!(
        macro_run.steps[0].judgment.as_ref().unwrap().outcome,
        mez_agent::MacroJudgeOutcome::Continue
    );
    assert_eq!(
        macro_run.steps[1].submitted_prompt.as_deref(),
        Some("Summarize release blockers.")
    );
    let second_child_turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| {
            turn.cooperation_mode.as_deref() == Some("macro-step")
                && turn.turn_id != first_child_turn.turn_id
        })
        .expect("judge continuation should dispatch the next child step");
    assert_eq!(
        second_child_turn.parent_turn_id.as_deref(),
        Some(parent_turn.turn_id.as_str())
    );
    assert!(
        service
            .joined_subagent_dependencies
            .contains_key(&second_child_turn.turn_id)
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("user> Summarize release blockers."),
        "{pane_text}"
    );
    let step_index = pane_text
        .find("macro release-check (2/2): judge continued; dispatched to agent-%")
        .expect("parent transcript should include the continued macro dispatch status");
    let prompt_index = pane_text
        .find("user> Summarize release blockers.")
        .expect("parent transcript should include the continued macro prompt line");
    assert!(step_index < prompt_index, "{pane_text}");
    assert!(
        pane_text.contains("macro release-check (1/2): result received; evaluating"),
        "{pane_text}"
    );

    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a recoverable macro judge retry decision resubmits the same
/// scripted step to the persistent macro subagent without advancing. This
/// protects incomplete-but-fixable subagent output from forcing macro failure
/// or incorrectly continuing to a later scripted step.
#[test]
fn runtime_agent_macro_judge_retries_current_step_after_child_result() {
    let config_root = temp_root("runtime-macro-judge-retry-current-step");
    let macro_dir = config_root.join("macros/release-check");
    fs::create_dir_all(&macro_dir).unwrap();
    fs::write(
        macro_dir.join("MACRO.md"),
        "---\nname: release-check\ndescription: Release readiness workflow\n---\n\n# Macro: release-check\n\n## Steps\n\n1. Inspect release notes.\n2. Summarize release blockers.\n",
    )
    .unwrap();
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(120, 40).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_config_root(config_root);

    let response = service
        .execute_agent_shell_command(&primary, "#release-check for v1.2")
        .unwrap();
    assert!(response.contains(r#""kind":"turn_started""#), "{response}");
    let parent_turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.agent_id == "agent-%1")
        .cloned()
        .expect("parent macro orchestration turn should exist");
    let first_child_turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.cooperation_mode.as_deref() == Some("macro-step"))
        .cloned()
        .expect("first runtime-owned macro step should create a child turn");

    service
        .agent_turn_ledger
        .finish_turn(&first_child_turn.turn_id, AgentTurnState::Completed)
        .unwrap();
    service
        .emit_subagent_task_result_for_state(&first_child_turn, AgentTurnState::Completed)
        .unwrap();
    assert_eq!(
        service.macro_judge_step_index_for_turn(&parent_turn.turn_id),
        Some(0)
    );

    let judge_provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: r#"{"outcome":"retry_current_step","step_success":false,"rationale":"subagent asked for clarification but can retry with a direct prompt","adapted_prompt":"Inspect release notes directly and list blockers.","user_message":null}"#
                .to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: None,
            provider_transcript_events: Vec::new(),
        },
    };
    service
        .execute_agent_turn_with_provider(
            &parent_turn.turn_id,
            &judge_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    let macro_run = service
        .macro_runs_by_parent_turn
        .get(parent_turn.turn_id.as_str())
        .expect("macro should remain active after retrying a recoverable step");
    assert_eq!(macro_run.current_step, 0);
    assert_eq!(
        macro_run.steps[0].submitted_prompt.as_deref(),
        Some("Inspect release notes directly and list blockers.")
    );
    assert!(macro_run.steps[0].task_result.is_none());
    assert!(macro_run.steps[0].judgment.is_none());
    assert!(macro_run.steps[1].submitted_prompt.is_none());
    let retry_child_turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| {
            turn.cooperation_mode.as_deref() == Some("macro-step")
                && turn.turn_id != first_child_turn.turn_id
        })
        .expect("judge retry should dispatch another child turn for the current step");
    assert_eq!(
        retry_child_turn.parent_turn_id.as_deref(),
        Some(parent_turn.turn_id.as_str())
    );
    assert!(
        service
            .joined_subagent_dependencies
            .contains_key(&retry_child_turn.turn_id)
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("user> Inspect release notes directly and list blockers."),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("macro release-check (1/2): judge requested retry attempt 2:"),
        "{pane_text}"
    );

    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a macro judge stop-failure decision after a successful child result
/// fully fails the parent turn and closes the persistent macro subagent.
///
/// Child `task_result.success` only reports transport-level task completion. The
/// macro judge can still reject the returned content semantically; that failure
/// path must tear down the macro-managed child and finish the parent shell turn
/// instead of leaving the parent failed only in the ledger with a live subagent.
#[test]
fn runtime_agent_macro_judge_stop_failure_closes_successful_child_subagent() {
    let config_root = temp_root("runtime-macro-judge-stop-failure-closes-child");
    let macro_dir = config_root.join("macros/release-check");
    fs::create_dir_all(&macro_dir).unwrap();
    fs::write(
        macro_dir.join("MACRO.md"),
        "---\nname: release-check\ndescription: Release readiness workflow\n---\n\n# Macro: release-check\n\n## Steps\n\n1. Inspect release notes.\n2. Summarize release blockers.\n",
    )
    .unwrap();
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(120, 40).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_config_root(config_root);

    let response = service
        .execute_agent_shell_command(&primary, "#release-check for v1.2")
        .unwrap();
    assert!(response.contains(r#""kind":"turn_started""#), "{response}");
    let parent_turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.agent_id == "agent-%1")
        .cloned()
        .expect("parent macro orchestration turn should exist");
    let first_child_turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.cooperation_mode.as_deref() == Some("macro-step"))
        .cloned()
        .expect("first runtime-owned macro step should create a child turn");
    let child_agent_id = first_child_turn.agent_id.clone();
    let child_pane_id = first_child_turn.pane_id.clone();

    service
        .agent_turn_ledger
        .finish_turn(&first_child_turn.turn_id, AgentTurnState::Completed)
        .unwrap();
    service
        .emit_subagent_task_result_for_state(&first_child_turn, AgentTurnState::Completed)
        .unwrap();
    assert_eq!(
        service.macro_judge_step_index_for_turn(&parent_turn.turn_id),
        Some(0)
    );

    let judge_provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: r#"{"outcome":"stop_failure","step_success":false,"rationale":"child asked for clarification instead of inspecting","adapted_prompt":null,"user_message":"the subagent did not perform the requested inspection"}"#
                .to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: None,
            provider_transcript_events: Vec::new(),
        },
    };
    service
        .execute_agent_turn_with_provider(
            &parent_turn.turn_id,
            &judge_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == parent_turn.turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Failed)
    );
    assert!(
        service
            .agent_shell_store()
            .get("%1")
            .is_some_and(|session| {
                session.running_turn_id.as_deref() != Some(parent_turn.turn_id.as_str())
            })
    );
    assert!(
        !service
            .macro_runs_by_parent_turn
            .contains_key(parent_turn.turn_id.as_str())
    );
    assert!(
        !service
            .macro_managed_subagent_agents
            .contains_key(&child_agent_id)
    );
    assert!(!service.subagent_lineage.contains_key(&child_agent_id));
    assert!(
        !service
            .subagent_scope_declarations
            .contains_key(&child_agent_id)
    );
    assert!(service.agent_shell_store().get(&child_pane_id).is_none());
    assert!(
        service
            .session()
            .windows()
            .iter()
            .flat_map(|window| window.panes())
            .all(|pane| pane.id.as_str() != child_pane_id)
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("macro release-check (1/2): stopped: the subagent did not perform the requested inspection"),
        "{pane_text}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a final-step macro judge finish-success decision closes the
/// persistent macro subagent and completes the parent shell turn.
///
/// Successful macro completion must use the same terminal lifecycle cleanup as
/// other finished parent turns so the runtime does not leave the controlling
/// turn running or the persistent child pane alive after the last scripted
/// step succeeds.
#[test]
fn runtime_agent_macro_judge_finish_success_closes_child_subagent_and_completes_parent_turn() {
    let config_root = temp_root("runtime-macro-judge-finish-success-closes-child");
    let macro_dir = config_root.join("macros/release-check");
    fs::create_dir_all(&macro_dir).unwrap();
    fs::write(
        macro_dir.join("MACRO.md"),
        "---\nname: release-check\ndescription: Release readiness workflow\n---\n\n# Macro: release-check\n\n## Steps\n\n1. Inspect release notes.\n",
    )
    .unwrap();
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(120, 40).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_config_root(config_root);

    let response = service
        .execute_agent_shell_command(&primary, "#release-check for v1.2")
        .unwrap();
    assert!(response.contains(r#""kind":"turn_started""#), "{response}");
    let parent_turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.agent_id == "agent-%1")
        .cloned()
        .expect("parent macro orchestration turn should exist");
    let child_turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.cooperation_mode.as_deref() == Some("macro-step"))
        .cloned()
        .expect("final runtime-owned macro step should create a child turn");
    let child_agent_id = child_turn.agent_id.clone();
    let child_pane_id = child_turn.pane_id.clone();

    service
        .agent_turn_ledger
        .finish_turn(&child_turn.turn_id, AgentTurnState::Completed)
        .unwrap();
    service
        .emit_subagent_task_result_for_state(&child_turn, AgentTurnState::Completed)
        .unwrap();
    assert_eq!(
        service.macro_judge_step_index_for_turn(&parent_turn.turn_id),
        Some(0)
    );

    let judge_provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: r#"{"outcome":"finish_success","step_success":true,"rationale":"the only scripted step completed successfully","adapted_prompt":null,"user_message":null}"#
                .to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: None,
            provider_transcript_events: Vec::new(),
        },
    };
    service
        .execute_agent_turn_with_provider(
            &parent_turn.turn_id,
            &judge_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == parent_turn.turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Completed)
    );
    assert!(
        service
            .agent_shell_store()
            .get("%1")
            .is_some_and(|session| {
                session.running_turn_id.as_deref() != Some(parent_turn.turn_id.as_str())
            })
    );
    assert!(
        !service
            .macro_runs_by_parent_turn
            .contains_key(parent_turn.turn_id.as_str())
    );
    assert!(
        !service
            .macro_managed_subagent_agents
            .contains_key(&child_agent_id)
    );
    assert!(!service.subagent_lineage.contains_key(&child_agent_id));
    assert!(
        !service
            .subagent_scope_declarations
            .contains_key(&child_agent_id)
    );
    assert!(service.agent_shell_store().get(&child_pane_id).is_none());
    assert!(
        service
            .session()
            .windows()
            .iter()
            .flat_map(|window| window.panes())
            .all(|pane| pane.id.as_str() != child_pane_id)
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("macro release-check (1/1): completed"),
        "{pane_text}"
    );

    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a macro-step child failure without a shell binding still resolves the joined parent dependency.
///
/// Macro steps are ordinary child agent-shell turns, but queued or blocked
/// children can fail through the no-shell-session cleanup path before a
/// provider execution exists. The parent macro orchestration turn must receive
/// that failed step result as a runtime-level failed parent action so the
/// macro stops with the user-visible explanation required by SPEC §10.5 instead
/// of treating the child failure as a successful join.
#[test]
fn runtime_macro_step_failure_without_shell_session_requeues_parent() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    let child_pane = service
        .session
        .split_active_pane(&primary, SplitDirection::Horizontal)
        .unwrap();
    for pane in ["%1", child_pane.as_str()] {
        service
            .agent_shell_store_mut()
            .enter_or_resume(pane)
            .unwrap();
        let mut screen = TerminalScreen::new(Size::new(24, 5).unwrap(), 10).unwrap();
        screen.feed(b"ready\n");
        service.set_pane_screen(pane.to_string(), screen);
    }

    let parent = service
        .start_agent_prompt_turn("%1", "parent macro")
        .unwrap();
    let child = service
        .start_agent_prompt_turn(child_pane.as_str(), "macro step")
        .unwrap();
    let parent_turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == parent.turn_id)
        .cloned()
        .unwrap();
    let action = mez_agent::AgentAction {
        id: "macro-step-1".to_string(),
        rationale: "send macro step".to_string(),
        payload: mez_agent::AgentActionPayload::SendMessage {
            recipient: format!("agent:{}", child.agent_id),
            content_type: "text/plain; charset=utf-8".to_string(),
            payload: "step one".to_string(),
        },
    };
    service.agent_turn_executions.insert(
        parent.turn_id.clone(),
        mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture_for_agent(&parent.turn_id, &parent.agent_id),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "send macro step".to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "test macro action batch".to_string(),
                    thought: None,
                    turn_id: parent.turn_id.clone(),
                    agent_id: parent.agent_id.clone(),
                    actions: vec![action.clone()],
                    final_turn: false,
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![mez_agent::ActionResult::running(
                &parent_turn,
                &action,
                vec!["waiting for macro step".to_string()],
                None,
            )],
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        },
    );
    service.joined_subagent_dependencies.insert(
        child.turn_id.clone(),
        JoinedSubagentDependency {
            parent_turn_id: parent.turn_id.clone(),
            parent_action_id: "macro-step-1".to_string(),
            child_turn_id: child.turn_id.clone(),
            child_agent_id: child.agent_id.clone(),
            child_display_name: Some("macro child".to_string()),
        },
    );
    service.pending_agent_provider_tasks.remove(&parent.turn_id);
    service
        .agent_scheduler_mut()
        .complete(&parent.turn_id)
        .unwrap();
    service
        .agent_turn_ledger
        .finish_turn(&parent.turn_id, AgentTurnState::Blocked)
        .unwrap();

    let child_record = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == child.turn_id)
        .cloned()
        .unwrap();
    service
        .finish_agent_turn_without_shell_session(&child_record, AgentTurnState::Failed)
        .unwrap();

    assert!(
        !service
            .joined_subagent_dependencies
            .contains_key(&child.turn_id)
    );
    assert!(
        !service
            .pending_agent_provider_tasks
            .contains(&parent.turn_id)
    );
    let execution = service.agent_turn_executions.get(&parent.turn_id).unwrap();
    assert_eq!(execution.action_results[0].status, ActionStatus::Failed);
    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    let structured = execution.action_results[0]
        .structured_content_json
        .as_deref()
        .unwrap_or_default();
    assert!(structured.contains(r#""success":false"#), "{structured}");
    assert!(
        structured.contains("failed without provider output"),
        "{structured}"
    );
}
