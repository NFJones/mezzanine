//! Runtime tests for agent model_selection routing behavior.

use super::*;
use crate::runtime::RuntimeRoutedWorkerSelection;

/// Starts a routed loop and returns its blocked parent plus selected worker turn.
fn selected_routed_loop(
    command: &str,
) -> (RuntimeSessionService, String, mez_agent::AgentTurnRecord) {
    let mut service = test_runtime_service();
    let _primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .execute_agent_shell_loop_command("%1", command)
        .unwrap();
    let parent_turn_id = service
        .agent_loop_turns_for_tests()
        .iter()
        .find(|(_, loop_turn)| loop_turn.pane_id == "%1")
        .map(|(turn_id, _)| turn_id.clone())
        .expect("loop command should create a parent-owned work turn");
    let worker_profile = service
        .agent_turn_model_profile(&parent_turn_id)
        .expect("parent profile should exist")
        .clone();
    service
        .apply_routed_worker_selected_transition(
            &AgentId::opaque("agent-%1").unwrap(),
            &parent_turn_id,
            RuntimeRoutedWorkerSelection {
                worker_profile,
                routing_token_usage_by_model: std::collections::BTreeMap::new(),
                decision_summary: None,
                fallback: None,
            },
        )
        .unwrap();
    let worker_turn_id = service
        .routed_workflow_for_tests(&parent_turn_id)
        .and_then(|workflow| workflow.child_turn_id.clone())
        .expect("selected worker should own a work turn");
    let worker_turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == worker_turn_id)
        .cloned()
        .expect("selected worker turn should exist");
    (service, parent_turn_id, worker_turn)
}

/// Builds a completed routed work execution containing one successful patch.
fn routed_patch_execution(turn: &mez_agent::AgentTurnRecord) -> mez_agent::AgentTurnExecution {
    let patch_action = mez_agent::AgentAction {
        id: format!("patch-{}", turn.turn_id),
        rationale: "make the requested change".to_string(),
        payload: mez_agent::AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** End Patch".to_string(),
            strip: None,
        },
    };
    mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test routed patch iteration".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![patch_action.clone()],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![mez_agent::ActionResult::succeeded(
            turn,
            &patch_action,
            vec!["patch applied".to_string()],
            None,
        )],
        final_turn: true,
        terminal_state: AgentTurnState::Completed,
    }
}

/// Verifies managed routed workers retain the normal subagent pane transcript.
///
/// Routing creates an idle child before queueing the real instruction. The
/// queued prompt, transient command status, and durable assistant output must
/// therefore be presented in the child pane and counted in its transcript
/// without changing the parent workflow state.
#[test]
fn runtime_routed_worker_presents_child_prompt_status_and_output() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-routed-presentation"));
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let prompt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"routed-presentation","method":"agent/shell/command","params":{"idempotency_key":"routed-presentation","input":"implement routed logging"}}"#,
        &primary,
    );
    assert!(prompt.contains(r#""state":"running""#), "{prompt}");
    let parent_profile = service
        .agent_turn_model_profile("turn-1")
        .expect("parent profile should exist")
        .clone();
    let selection = RuntimeRoutedWorkerSelection {
        worker_profile: parent_profile,
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        decision_summary: Some("large/high".to_string()),
        fallback: None,
    };
    let parent_agent = AgentId::opaque("agent-%1").unwrap();

    service
        .apply_routed_worker_selected_transition(&parent_agent, "turn-1", selection)
        .unwrap();

    let child_prompt = service
        .pane_screen("%2")
        .expect("routed child screen should exist")
        .visible_lines()
        .join("\n");
    assert!(
        child_prompt.contains("parent> implement routed logging"),
        "{child_prompt}"
    );
    let child_conversation_id = service
        .agent_shell_store()
        .get("%2")
        .expect("routed child shell should exist")
        .session_id
        .clone();
    assert_eq!(
        transcript_store
            .inspect_presentation(&child_conversation_id)
            .unwrap()
            .len(),
        1
    );

    service
        .append_agent_shell_output_status_lines_to_terminal_buffer(
            "%2",
            &["running routed command".to_string()],
        )
        .unwrap();
    let child_status = service
        .pane_screen("%2")
        .unwrap()
        .visible_lines()
        .join("\n");
    assert!(
        child_status.contains("running routed command"),
        "{child_status}"
    );

    service
        .append_agent_assistant_text_to_terminal_buffer("%2", "routed worker output")
        .unwrap();
    let child_output = service
        .pane_screen("%2")
        .unwrap()
        .visible_lines()
        .join("\n");
    assert!(
        child_output.contains("mez> routed worker output"),
        "{child_output}"
    );
    let presentation = transcript_store
        .inspect_presentation(&child_conversation_id)
        .unwrap();
    assert_eq!(presentation.len(), 2);
    assert_eq!(presentation[0].style_names, vec!["user-prompt"]);
    assert_eq!(presentation[1].style_names, vec!["assistant"]);
    assert_eq!(
        service
            .routed_workflow_for_tests("turn-1")
            .expect("routed workflow should remain active")
            .phase,
        mez_agent::routed_workflow::RoutedWorkflowPhase::WaitingForWorkerResult
    );
}

/// Verifies routed selection setup failure is contained after classification.
///
/// A missing active prompt deterministically fails before worker creation. The
/// runtime must retain one bounded main-model explanation, avoid child state,
/// and treat replay of the consumed classifier event as an idempotent no-op.
#[test]
fn runtime_routed_selection_setup_failure_recovers_once() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let prompt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"routed-setup-failure","method":"agent/shell/command","params":{"idempotency_key":"routed-setup-failure","input":"implement this"}}"#,
        &primary,
    );
    assert!(prompt.contains(r#""state":"running""#), "{prompt}");
    let parent_profile = service
        .agent_turn_model_profile("turn-1")
        .expect("parent profile should exist")
        .clone();
    service
        .agent_turn_contexts_mut()
        .get_mut("turn-1")
        .expect("parent context should exist")
        .blocks
        .retain(|block| {
            block.source != ContextSourceKind::UserInstruction || block.label != "user prompt"
        });
    let selection = RuntimeRoutedWorkerSelection {
        worker_profile: parent_profile,
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        decision_summary: Some("large/high".to_string()),
        fallback: None,
    };
    let parent_agent = AgentId::opaque("agent-%1").unwrap();

    service
        .apply_routed_worker_selected_transition(&parent_agent, "turn-1", selection.clone())
        .unwrap();

    let workflow = service
        .routed_workflow_for_tests("turn-1")
        .expect("setup failure should retain recovery state");
    assert_eq!(
        workflow.phase,
        mez_agent::routed_workflow::RoutedWorkflowPhase::ReadyForErrorExplanation
    );
    assert!(workflow.error_explanation_attempted);
    assert!(workflow.child_turn_id.is_none());
    assert_eq!(service.pending_agent_provider_tasks().len(), 1);
    assert!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .all(|turn| turn.parent_turn_id.as_deref() != Some("turn-1"))
    );
    let diagnostic_count = service
        .agent_turn_contexts()
        .get("turn-1")
        .expect("parent context should remain available")
        .blocks
        .iter()
        .filter(|block| {
            block.label == "routed workflow failure"
                && block
                    .content
                    .contains("routed parent prompt is unavailable")
        })
        .count();
    assert_eq!(diagnostic_count, 1);

    service
        .apply_routed_worker_selected_transition(&parent_agent, "turn-1", selection)
        .unwrap();
    assert_eq!(service.pending_agent_provider_tasks().len(), 1);
    let replay_diagnostic_count = service
        .agent_turn_contexts()
        .get("turn-1")
        .expect("parent context should remain available")
        .blocks
        .iter()
        .filter(|block| {
            block.label == "routed workflow failure"
                && block
                    .content
                    .contains("routed parent prompt is unavailable")
        })
        .count();
    assert_eq!(replay_diagnostic_count, 1);
}

/// Verifies post-spawn routed setup failure removes the unregistered worker.
///
/// A successful idle spawn acquires a pane, shell session, and subagent
/// authority before routed turn registration. Failure at that boundary must
/// tear those resources down directly instead of relying on child-turn lookup.
#[test]
fn runtime_routed_selection_post_spawn_failure_removes_worker() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let prompt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"routed-post-spawn-failure","method":"agent/shell/command","params":{"idempotency_key":"routed-post-spawn-failure","input":"implement this"}}"#,
        &primary,
    );
    assert!(prompt.contains(r#""state":"running""#), "{prompt}");
    let parent_profile = service
        .agent_turn_model_profile("turn-1")
        .expect("parent profile should exist")
        .clone();
    service.fail_next_routed_worker_after_spawn_for_tests();
    let selection = RuntimeRoutedWorkerSelection {
        worker_profile: parent_profile,
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        decision_summary: Some("large/high".to_string()),
        fallback: None,
    };
    let parent_agent = AgentId::opaque("agent-%1").unwrap();

    service
        .apply_routed_worker_selected_transition(&parent_agent, "turn-1", selection)
        .unwrap();

    assert!(service.agent_shell_store().get("%2").is_none());
    assert!(service.find_pane_descriptor("%2").is_none());
    assert!(!service.has_subagent_authority_state("agent-%2"));
    assert!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .all(|turn| turn.parent_turn_id.as_deref() != Some("turn-1"))
    );
    let workflow = service
        .routed_workflow_for_tests("turn-1")
        .expect("setup failure should retain recovery state");
    assert_eq!(
        workflow.phase,
        mez_agent::routed_workflow::RoutedWorkflowPhase::ReadyForErrorExplanation
    );
    assert!(
        workflow
            .diagnostic
            .as_deref()
            .is_some_and(|value| value.contains("post-spawn setup failure"))
    );
}

/// Verifies routed `/loop` work transfers from the classifier turn to the
/// selected worker so patch accounting and later settlement remain attached to
/// one logical loop.
#[test]
fn runtime_routed_loop_transfers_work_turn_ownership_to_selected_worker() {
    let mut service = test_runtime_service();
    let _primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .execute_agent_shell_loop_command("%1", "/loop implement routed ownership")
        .unwrap();

    let (parent_turn_id, parent_loop_turn) = service
        .agent_loop_turns_for_tests()
        .iter()
        .find(|(_, loop_turn)| loop_turn.pane_id == "%1")
        .map(|(turn_id, loop_turn)| (turn_id.clone(), loop_turn.clone()))
        .expect("loop command should create a parent-owned work turn");
    let parent_profile = service
        .agent_turn_model_profile(&parent_turn_id)
        .expect("parent profile should exist")
        .clone();
    let selection = RuntimeRoutedWorkerSelection {
        worker_profile: parent_profile.clone(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        decision_summary: None,
        fallback: None,
    };
    let parent_agent = AgentId::opaque("agent-%1").unwrap();

    service
        .apply_routed_worker_selected_transition(&parent_agent, &parent_turn_id, selection)
        .unwrap();

    let (child_turn_id, child_loop_turn) = service
        .agent_loop_turns_for_tests()
        .iter()
        .find(|(_, loop_turn)| loop_turn.pane_id == "%2")
        .map(|(turn_id, loop_turn)| (turn_id.clone(), loop_turn.clone()))
        .expect("selected worker should own the loop work turn");
    assert_ne!(child_turn_id, parent_turn_id);
    assert_eq!(child_loop_turn.loop_id, parent_loop_turn.loop_id);
    assert_eq!(child_loop_turn.iteration, parent_loop_turn.iteration);
    assert!(service.agent_loop_turn(&parent_turn_id).is_none());

    let state = service
        .agent_loop_state("%2")
        .expect("selected worker should index the logical loop");
    assert_eq!(state.execution_pane_id, "%2");
    assert_eq!(
        state.routed_parent_turn_id.as_deref(),
        Some(parent_turn_id.as_str())
    );
    assert_eq!(state.routed_worker_profile.as_ref(), Some(&parent_profile));
}

/// Verifies a routed `/loop` keeps its parent blocked after patch work and
/// reuses the selected worker for the next iteration before queueing one
/// terminal handoff.
#[test]
fn runtime_routed_loop_continues_in_one_worker_before_terminal_handoff() {
    let mut service = test_runtime_service();
    let _primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .execute_agent_shell_loop_command("%1", "/loop implement routed iterations")
        .unwrap();

    let parent_turn_id = service
        .agent_loop_turns_for_tests()
        .iter()
        .find(|(_, loop_turn)| loop_turn.pane_id == "%1")
        .map(|(turn_id, _)| turn_id.clone())
        .expect("loop command should create a parent-owned work turn");
    let parent_profile = service
        .agent_turn_model_profile(&parent_turn_id)
        .expect("parent profile should exist")
        .clone();
    service
        .apply_routed_worker_selected_transition(
            &AgentId::opaque("agent-%1").unwrap(),
            &parent_turn_id,
            RuntimeRoutedWorkerSelection {
                worker_profile: parent_profile,
                routing_token_usage_by_model: std::collections::BTreeMap::new(),
                decision_summary: None,
                fallback: None,
            },
        )
        .unwrap();

    let first_worker_turn_id = service
        .routed_workflow_for_tests(&parent_turn_id)
        .expect("routed workflow should exist")
        .child_turn_id
        .clone()
        .expect("selected worker should have a work turn");
    let first_worker_turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == first_worker_turn_id)
        .cloned()
        .expect("selected worker turn should exist");
    let patch_action = mez_agent::AgentAction {
        id: "patch-1".to_string(),
        rationale: "make the requested change".to_string(),
        payload: mez_agent::AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** End Patch".to_string(),
            strip: None,
        },
    };
    let patched_execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&first_worker_turn_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test routed patch iteration".to_string(),
                thought: None,
                turn_id: first_worker_turn_id.clone(),
                agent_id: first_worker_turn.agent_id.clone(),
                actions: vec![patch_action.clone()],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![mez_agent::ActionResult::succeeded(
            &first_worker_turn,
            &patch_action,
            vec!["patch applied".to_string()],
            None,
        )],
        final_turn: true,
        terminal_state: AgentTurnState::Completed,
    };
    service
        .emit_subagent_task_result_for_execution(&first_worker_turn, &patched_execution)
        .unwrap();

    let second_worker_turn_id = service
        .routed_workflow_for_tests(&parent_turn_id)
        .expect("routed workflow should remain active after patch work")
        .child_turn_id
        .clone()
        .expect("continued loop should queue a second worker turn");
    assert_ne!(second_worker_turn_id, first_worker_turn_id);
    assert_eq!(
        service
            .routed_workflow_for_tests(&parent_turn_id)
            .expect("routed workflow should remain active")
            .phase,
        mez_agent::routed_workflow::RoutedWorkflowPhase::WaitingForWorkerResult
    );

    let second_worker_turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == second_worker_turn_id)
        .cloned()
        .expect("continued worker turn should exist");
    let patch_free_execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&second_worker_turn_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "completed work".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(runtime_complete_batch_for(
                second_worker_turn_id.clone(),
                second_worker_turn.agent_id.clone(),
            )),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: Vec::new(),
        final_turn: true,
        terminal_state: AgentTurnState::Completed,
    };
    service
        .emit_subagent_task_result_for_execution(&second_worker_turn, &patch_free_execution)
        .unwrap();

    let workflow = service
        .routed_workflow_for_tests(&parent_turn_id)
        .expect("patch-free iteration should queue a terminal handoff");
    assert_eq!(
        workflow.phase,
        mez_agent::routed_workflow::RoutedWorkflowPhase::WaitingForHandoff
    );
    assert_ne!(
        workflow.child_turn_id.as_deref(),
        Some(second_worker_turn_id.as_str())
    );
}

/// Verifies routed loop continuation queue failure terminates the controller
/// and resumes the blocked parent through one bounded explanation.
///
/// A patch-producing iteration normally queues more work. If that queue step
/// fails after loop settlement, the runtime must not strand the routed parent,
/// retain stale loop indexes, or duplicate diagnostics when the old worker
/// result is observed again.
#[test]
fn runtime_routed_loop_continuation_queue_failure_recovers_once() {
    let (mut service, parent_turn_id, worker_turn) =
        selected_routed_loop("/loop --limit 3 implement routed queue recovery");
    service.fail_next_routed_loop_continuation_queue_for_tests();
    let execution = routed_patch_execution(&worker_turn);

    service
        .emit_subagent_task_result_for_execution(&worker_turn, &execution)
        .unwrap();

    let workflow = service
        .routed_workflow_for_tests(&parent_turn_id)
        .expect("queue failure should retain routed recovery state");
    assert_eq!(
        workflow.phase,
        mez_agent::routed_workflow::RoutedWorkflowPhase::ReadyForErrorExplanation
    );
    assert!(workflow.error_explanation_attempted);
    assert!(
        workflow
            .diagnostic
            .as_deref()
            .is_some_and(|value| value.contains("continuation queue"))
    );
    assert!(service.agent_loop_state("%1").is_none());
    assert!(service.agent_loop_state(&worker_turn.pane_id).is_none());
    assert!(service.agent_loop_turns_for_tests().is_empty());
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == parent_turn_id)
    );
    assert_eq!(
        service
            .agent_turn_contexts()
            .get(&parent_turn_id)
            .expect("parent context should remain available")
            .blocks
            .iter()
            .filter(|block| {
                block.label == "routed workflow failure"
                    && block.content.contains("continuation queue")
            })
            .count(),
        1
    );

    service
        .emit_subagent_task_result_for_execution(&worker_turn, &execution)
        .unwrap();
    assert_eq!(
        service
            .agent_turn_contexts()
            .get(&parent_turn_id)
            .expect("parent context should remain available")
            .blocks
            .iter()
            .filter(|block| block.label == "routed workflow failure")
            .count(),
        1
    );
}

/// Verifies a provider failure in the pinned routed worker terminates the
/// logical loop and resumes its parent through one response-only explanation.
///
/// Provider failure is terminal for the current loop rather than a signal to
/// classify another iteration. Controller, pane indexes, and worker routes
/// must be released before the parent recovery request is queued.
#[test]
fn runtime_routed_loop_worker_provider_failure_terminates_controller() {
    let (mut service, parent_turn_id, worker_turn) =
        selected_routed_loop("/loop --limit 3 implement routed provider recovery");
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(
            &worker_turn.turn_id,
            &worker_turn.agent_id,
        ),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "provider request failed".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: None,
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: Vec::new(),
        final_turn: true,
        terminal_state: AgentTurnState::Failed,
    };

    service
        .emit_subagent_task_result_for_execution(&worker_turn, &execution)
        .unwrap();

    let workflow = service
        .routed_workflow_for_tests(&parent_turn_id)
        .expect("provider failure should retain routed recovery state");
    assert_eq!(
        workflow.phase,
        mez_agent::routed_workflow::RoutedWorkflowPhase::ReadyForErrorExplanation
    );
    assert!(
        workflow
            .diagnostic
            .as_deref()
            .is_some_and(|value| value.contains("worker failed before handoff"))
    );
    assert!(service.agent_loop_state("%1").is_none());
    assert!(service.agent_loop_state(&worker_turn.pane_id).is_none());
    assert!(service.agent_loop_turns_for_tests().is_empty());
    assert_eq!(service.subagent_task_parent(&worker_turn.turn_id), None);
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == parent_turn_id)
    );
}

/// Verifies routed `--fork` and `--new` loops restore the invoking parent at
/// their iteration limit while keeping worker attempts ephemeral and isolated.
///
/// Both modes are classified once, execute in the pinned worker, and then
/// hand off exactly once. Fork mode retains a transcript source while new mode
/// starts without one; neither may leave the parent bound to the attempt.
#[test]
fn runtime_routed_loop_fresh_modes_restore_parent_at_limit() {
    for (command, expects_source) in [
        ("/loop --fork --limit 1 inspect fork isolation", true),
        ("/loop --new --limit 1 inspect new isolation", false),
    ] {
        let (mut service, parent_turn_id, worker_turn) = selected_routed_loop(command);
        let parent_conversation_id = service
            .agent_loop_state(&worker_turn.pane_id)
            .expect("selected worker should own loop state")
            .parent_conversation_id
            .clone();
        let worker_session = service
            .agent_shell_store()
            .get(&worker_turn.pane_id)
            .expect("selected worker session should exist");
        assert!(worker_session.ephemeral);
        if expects_source {
            assert!(
                worker_session
                    .ephemeral_transcript_source_conversation_id
                    .is_some()
            );
        } else {
            let worker_context = service
                .agent_turn_contexts()
                .get(&worker_turn.turn_id)
                .expect("new-mode worker context should exist");
            assert!(!worker_context.blocks.iter().any(|block| {
                matches!(
                    block.source,
                    ContextSourceKind::TranscriptUser
                        | ContextSourceKind::TranscriptAssistant
                        | ContextSourceKind::TranscriptTool
                )
            }));
        }

        service
            .emit_subagent_task_result_for_execution(
                &worker_turn,
                &routed_patch_execution(&worker_turn),
            )
            .unwrap();

        assert_eq!(
            service
                .agent_shell_store()
                .get("%1")
                .expect("invoking parent session should remain available")
                .session_id,
            parent_conversation_id
        );
        assert!(service.agent_loop_state("%1").is_none());
        assert!(service.agent_loop_state(&worker_turn.pane_id).is_none());
        assert_eq!(
            service
                .routed_workflow_for_tests(&parent_turn_id)
                .expect("limit settlement should advance to handoff")
                .phase,
            mez_agent::routed_workflow::RoutedWorkflowPhase::WaitingForHandoff
        );
    }
}

/// Verifies routed selection recovery terminates cleanly without parent context.
///
/// Losing the complete parent context makes a model-authored explanation
/// impossible. Recovery must preserve the original setup diagnostic, fail the
/// parent through normal lifecycle cleanup, and avoid propagating a second
/// invalid-state error or leaving provider work queued.
#[test]
fn runtime_routed_selection_missing_parent_context_fails_cleanly() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let prompt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"routed-missing-context","method":"agent/shell/command","params":{"idempotency_key":"routed-missing-context","input":"implement this"}}"#,
        &primary,
    );
    assert!(prompt.contains(r#""state":"running""#), "{prompt}");
    let parent_profile = service
        .agent_turn_model_profile("turn-1")
        .expect("parent profile should exist")
        .clone();
    service.agent_turn_contexts_mut().remove("turn-1");
    let selection = RuntimeRoutedWorkerSelection {
        worker_profile: parent_profile,
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        decision_summary: Some("large/high".to_string()),
        fallback: None,
    };
    let parent_agent = AgentId::opaque("agent-%1").unwrap();

    service
        .apply_routed_worker_selected_transition(&parent_agent, "turn-1", selection)
        .unwrap();

    let parent_turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .expect("parent turn should remain in the ledger");
    assert_eq!(parent_turn.state, mez_agent::AgentTurnState::Failed);
    assert!(service.pending_agent_provider_tasks().is_empty());
    let workflow = service
        .routed_workflow_for_tests("turn-1")
        .expect("failed workflow should retain its diagnostic");
    assert_eq!(
        workflow.phase,
        mez_agent::routed_workflow::RoutedWorkflowPhase::Failed
    );
    assert!(
        workflow
            .diagnostic
            .as_deref()
            .is_some_and(|value| value.contains("routed parent context is unavailable"))
    );
}

/// Verifies routed child cancellation resumes the parent exactly once and
/// routed parent cancellation terminates its active managed child.
///
/// Worker, handoff, and parent interruption use the normal routed selection
/// and pane stop paths. Late child settlement after parent cancellation must
/// be a handled no-op rather than reviving the interrupted workflow.
#[test]
fn runtime_routed_child_cancellation_resumes_parent_once() {
    let setup = || {
        let mut service = test_runtime_service();
        service
            .replace_config_layers(vec![ConfigLayer {
                name: "primary".to_string(),
                path: None,
                format: ConfigFormat::Toml,
                scope: ConfigScope::Primary,
                trusted: true,
                text: r#"
[agents]
default_provider = "runtime-batch"
default_model_profile = "default"
routing = true

[agents.auto_sizing]
router_model_profile = "router"
small_model_profile = "small"
medium_model_profile = "medium"
large_model_profile = "large"
allowed_reasoning_efforts = ["low", "medium", "high", "xhigh"]
fallback_policy = "use-default-profile"

[providers.runtime-batch]
kind = "openai"
models = ["gpt-router", "gpt-default", "gpt-5.3-codex", "gpt-5.4", "gpt-5.5"]
default_model = "gpt-default"

[model_profiles.default]
provider = "runtime-batch"
model = "gpt-default"
reasoning_profile = "medium"

[model_profiles.router]
provider = "runtime-batch"
model = "gpt-router"
reasoning_profile = "low"

[model_profiles.small]
provider = "runtime-batch"
model = "gpt-5.3-codex"
reasoning_profile = "medium"

[model_profiles.medium]
provider = "runtime-batch"
model = "gpt-5.4"
reasoning_profile = "medium"

[model_profiles.large]
provider = "runtime-batch"
model = "gpt-5.5"
reasoning_profile = "high"
"#
                .to_string(),
            }])
            .unwrap();
        let primary = service
            .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
        screen.feed(b"ready\n");
        service.set_pane_screen("%1".to_string(), screen);
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        let prompt = service.dispatch_runtime_control_body(
            r#"{"jsonrpc":"2.0","id":"routed-cancel","method":"agent/shell/command","params":{"idempotency_key":"routed-cancel","input":"implement this"}}"#,
            &primary,
        );
        assert!(prompt.contains(r#""state":"running""#), "{prompt}");
        let provider = RuntimeAutoSizingProvider {
            requests: RefCell::new(Vec::new()),
        };
        assert!(
            service
                .poll_agent_provider_tasks_with_provider(&provider, 1)
                .unwrap()
                .is_empty()
        );
        let child_turn_id = service
            .routed_workflow_for_tests("turn-1")
            .and_then(|workflow| workflow.child_turn_id.clone())
            .expect("routing should queue a managed worker");
        (service, primary, child_turn_id)
    };
    let completed_execution = |turn: &mez_agent::AgentTurnRecord, text: &str| {
        let action = mez_agent::AgentAction {
            id: format!("say-{}", turn.turn_id),
            rationale: "return the routed result".to_string(),
            payload: mez_agent::AgentActionPayload::Say {
                status: mez_agent::SayStatus::Final,
                text: text.to_string(),
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            },
        };
        mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: text.to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: None,
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![mez_agent::ActionResult::succeeded(
                turn,
                &action,
                vec![text.to_string()],
                None,
            )],
            final_turn: true,
            terminal_state: AgentTurnState::Completed,
        }
    };

    let (mut worker_service, _worker_primary, worker_turn_id) = setup();
    let worker_turn = worker_service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == worker_turn_id)
        .cloned()
        .expect("managed worker turn should exist");
    let worker_pane_id = worker_turn
        .agent_id
        .strip_prefix("agent-")
        .expect("managed child agent should identify its pane")
        .to_string();
    worker_service
        .stop_agent_turn_for_pane(&worker_pane_id)
        .unwrap();
    let worker_workflow = worker_service
        .routed_workflow_for_tests("turn-1")
        .expect("cancelled worker should retain parent recovery state");
    assert_eq!(
        worker_workflow.phase,
        mez_agent::routed_workflow::RoutedWorkflowPhase::ReadyForErrorExplanation
    );
    assert!(worker_workflow.error_explanation_attempted);
    assert_eq!(worker_service.pending_agent_provider_tasks().len(), 1);
    assert_eq!(worker_service.subagent_task_parent(&worker_turn_id), None);
    let worker_failure_count = worker_service
        .agent_turn_contexts()
        .get("turn-1")
        .expect("parent context should remain available")
        .blocks
        .iter()
        .filter(|block| {
            block.label == "routed workflow failure"
                && block.content.contains("routed worker was cancelled")
        })
        .count();
    assert_eq!(worker_failure_count, 1);
    assert!(
        worker_service
            .handle_routed_child_cancellation(&worker_turn)
            .unwrap()
    );
    assert_eq!(worker_service.pending_agent_provider_tasks().len(), 1);

    let (mut handoff_service, _handoff_primary, worker_turn_id) = setup();
    let worker_turn = handoff_service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == worker_turn_id)
        .cloned()
        .expect("managed worker turn should exist");
    let worker_pane_id = worker_turn
        .agent_id
        .strip_prefix("agent-")
        .expect("managed child agent should identify its pane")
        .to_string();
    let exact_result = "worker completed before handoff cancellation";
    let worker_profile = handoff_service
        .agent_turn_model_profile(&worker_turn.turn_id)
        .expect("managed worker profile should remain pinned")
        .clone();
    handoff_service
        .apply_agent_provider_execution(
            &worker_turn,
            &worker_profile,
            "runtime-batch",
            completed_execution(&worker_turn, exact_result),
        )
        .unwrap();
    let handoff_turn_id = handoff_service
        .routed_workflow_for_tests("turn-1")
        .and_then(|workflow| workflow.child_turn_id.clone())
        .expect("worker completion should queue a handoff turn");
    let handoff_turn = handoff_service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == handoff_turn_id)
        .cloned()
        .expect("managed handoff turn should exist");
    handoff_service.start_ready_agent_turns().unwrap();
    assert_eq!(
        handoff_service
            .agent_shell_store()
            .get(&worker_pane_id)
            .and_then(|session| session.running_turn_id.as_deref()),
        Some(handoff_turn_id.as_str())
    );
    handoff_service
        .stop_agent_turn_for_pane(&worker_pane_id)
        .unwrap();
    let handoff_workflow = handoff_service
        .routed_workflow_for_tests("turn-1")
        .expect("cancelled handoff should retain parent recovery state");
    assert_eq!(
        handoff_workflow.phase,
        mez_agent::routed_workflow::RoutedWorkflowPhase::ReadyForErrorExplanation
    );
    assert!(handoff_workflow.error_explanation_attempted);
    assert_eq!(handoff_service.pending_agent_provider_tasks().len(), 1);
    assert_eq!(handoff_service.subagent_task_parent(&handoff_turn_id), None);
    let parent_context = handoff_service
        .agent_turn_contexts()
        .get("turn-1")
        .expect("parent context should remain available");
    assert_eq!(
        parent_context
            .blocks
            .iter()
            .filter(|block| {
                block.label == "routed worker exact final result" && block.content == exact_result
            })
            .count(),
        1
    );
    assert_eq!(
        parent_context
            .blocks
            .iter()
            .filter(|block| {
                block.label == "routed workflow failure"
                    && block.content.contains("routed handoff was cancelled")
            })
            .count(),
        1
    );
    assert!(
        handoff_service
            .handle_routed_child_cancellation(&handoff_turn)
            .unwrap()
    );
    assert_eq!(handoff_service.pending_agent_provider_tasks().len(), 1);

    let (mut parent_service, _parent_primary, child_turn_id) = setup();
    let child_turn = parent_service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == child_turn_id)
        .cloned()
        .expect("managed worker turn should exist before parent cancellation");
    parent_service.stop_agent_turn_for_pane("%1").unwrap();
    assert_eq!(
        parent_service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == "turn-1")
            .map(|turn| turn.state),
        Some(AgentTurnState::Interrupted)
    );
    assert_eq!(
        parent_service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == child_turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Interrupted)
    );
    assert!(parent_service.routed_workflow_for_tests("turn-1").is_none());
    assert_eq!(parent_service.subagent_task_parent(&child_turn_id), None);
    assert!(parent_service.pending_agent_provider_tasks().is_empty());
    let late_execution = completed_execution(&child_turn, "late worker result");
    assert!(
        parent_service
            .handle_routed_child_execution_result(&child_turn, &late_execution)
            .unwrap()
    );
    assert!(
        parent_service
            .handle_routed_child_execution_result(&child_turn, &late_execution)
            .unwrap()
    );
    assert!(parent_service.pending_agent_provider_tasks().is_empty());
}

/// Verifies subagents inherit the live parent pane routing decision.
///
/// Auto-reasoning is a pane-local agent behavior, not just a global default.
/// Child agents should continue with the parent pane's effective setting so a
/// user does not have to re-toggle it after spawning helpers.
#[test]
fn runtime_subagent_routing_inherits_parent_pane_setting() {
    let mut service = test_runtime_service();
    service.set_agent_default_routing(false);
    service.set_agent_routing_override("%1", Some(true));

    assert_eq!(
        service.inherited_routing_for_child_agent("agent-%1"),
        Some(true)
    );

    service.set_agent_routing_override("%1", None);
    service.set_agent_default_routing(true);
    assert_eq!(
        service.inherited_routing_for_child_agent("agent-%1"),
        Some(true)
    );
}

///
/// Verifies subagents inherit the live parent pane auto-sizing configuration.
///
/// Auto-sizing uses pane-local model profile names for router and bucket
/// selection. Child agents must inherit that configuration with the parent
/// model profile so a DeepSeek parent pane does not spawn children that use the
/// global OpenAI sizing defaults.
#[test]
fn runtime_subagent_auto_sizing_inherits_parent_pane_setting() {
    let mut service = test_runtime_service();
    let mut parent_auto_sizing = service.agent_auto_sizing().clone();
    parent_auto_sizing.router_model_profile = "deepseek-fast".to_string();
    parent_auto_sizing.small_model_profile = "deepseek-fast".to_string();
    parent_auto_sizing.medium_model_profile = "deepseek-default".to_string();
    parent_auto_sizing.large_model_profile = "deepseek-default".to_string();
    parent_auto_sizing.allowed_reasoning_efforts = vec!["high".to_string(), "xhigh".to_string()];
    service.set_agent_auto_sizing_override("%1", Some(parent_auto_sizing.clone()));

    assert_eq!(
        service.inherited_auto_sizing_for_child_agent("agent-%1"),
        Some(parent_auto_sizing)
    );
}

/// Verifies that configured named model profiles populate the full
/// specification-facing profile fields and that configured fallback profiles
/// are filtered through safety, privacy, residency, and approval
/// characteristics before they can be offered after provider failure.
#[test]
fn runtime_applies_named_model_profile_fields_and_safe_fallbacks() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"work\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-work\", \"gpt-safe\", \"gpt-weak\", \"gpt-external\"]\ndefault_model = \"gpt-work\"\n[model_profiles.work]\nprovider = \"openai\"\nmodel = \"gpt-work\"\nreasoning_profile = \"high\"\nlatency_preference = \"default\"\nmultimodal_required = true\nsafety_tier = \"high\"\nprivacy_tier = \"strict\"\nresidency = \"us\"\napproval_policy = \"ask\"\nfallback_profiles = [\"safe\", \"weak\", \"external\"]\n[model_profiles.work.provider_options]\nreasoning_effort = \"high\"\n[model_profiles.safe]\nprovider = \"openai\"\nmodel = \"gpt-safe\"\nsafety_tier = \"high\"\nprivacy_tier = \"strict\"\nresidency = \"us\"\napproval_policy = \"ask\"\n[model_profiles.weak]\nprovider = \"openai\"\nmodel = \"gpt-weak\"\nsafety_tier = \"medium\"\nprivacy_tier = \"strict\"\nresidency = \"us\"\napproval_policy = \"ask\"\n[model_profiles.external]\nprovider = \"openai\"\nmodel = \"gpt-external\"\nsafety_tier = \"high\"\nprivacy_tier = \"external\"\nresidency = \"eu\"\napproval_policy = \"full-access\"\n"
                .to_string(),
        }])
        .unwrap();

    let registry = service.provider_registry();
    let profile = registry.resolve_profile("work").unwrap();
    assert_eq!(profile.provider, "openai");
    assert_eq!(profile.model, "gpt-work");
    assert_eq!(profile.reasoning_profile.as_deref(), Some("high"));
    assert_eq!(profile.latency_preference.as_deref(), Some("default"));
    assert!(profile.multimodal_required);
    assert_eq!(profile.safety_tier.as_deref(), Some("high"));
    assert_eq!(
        profile
            .provider_options
            .get("reasoning_effort")
            .map(String::as_str),
        Some("high")
    );
    assert_eq!(
        registry.safe_fallback_profiles("work").unwrap(),
        vec!["safe".to_string()]
    );
}

/// Verifies that provider failure reporting only offers configured fallback
/// profiles whose safety, privacy, residency, and approval characteristics are
/// non-weaker than the active model profile.
#[test]
fn runtime_provider_failure_reports_only_safe_model_fallbacks() {
    let mut service = test_runtime_service();
    let transcript_root = temp_root("runtime-provider-safe-fallback-transcript");
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"runtime-fail\"\ndefault_model_profile = \"work\"\n[providers.runtime-fail]\nkind = \"runtime-fail\"\napi = \"openai-responses\"\nmodels = [\"primary\", \"safe\", \"weak\"]\ndefault_model = \"primary\"\n[model_profiles.work]\nprovider = \"runtime-fail\"\nmodel = \"primary\"\nsafety_tier = \"high\"\nprivacy_tier = \"strict\"\nresidency = \"us\"\napproval_policy = \"ask\"\nfallback_profiles = [\"safe\", \"weak\"]\n[model_profiles.safe]\nprovider = \"runtime-fail\"\nmodel = \"safe\"\nsafety_tier = \"high\"\nprivacy_tier = \"strict\"\nresidency = \"us\"\napproval_policy = \"ask\"\n[model_profiles.weak]\nprovider = \"runtime-fail\"\nmodel = \"weak\"\nsafety_tier = \"medium\"\nprivacy_tier = \"external\"\nresidency = \"eu\"\napproval_policy = \"full-access\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-provider-safe-fallback","input":"summarize the pane"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == "turn-1")
            .map(|turn| turn.model_profile.as_str()),
        Some("work")
    );

    let error = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &RuntimeFailingProvider,
            service.provider_registry().resolve_profile("work").unwrap(),
        )
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    let entries = transcript_store.inspect(&conversation_id).unwrap();
    let failure = entries
        .iter()
        .find(|entry| {
            entry.role == mez_agent::transcript::TranscriptRole::Assistant
                && entry.content.contains("provider_error")
        })
        .unwrap();
    assert!(failure.content.contains("safe_fallback_profiles: safe"));
    assert!(!failure.content.contains("weak"));
    let _ = fs::remove_dir_all(transcript_root);
}

/// Verifies that changing reasoning from the pane-frame selector preserves the
/// active latency preference and keeps the latency pill visible.
///
/// Reasoning changes generate a new pane-scoped model profile. That generated
/// profile must carry forward the provider-visible latency selection so the
/// status bar does not lose its latency dropdown after the user changes only
/// the reasoning level.
#[test]
fn runtime_pane_agent_status_reasoning_preserves_latency_preference() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\"]\ndefault_model = \"gpt-5.5\"\n\n[model_profiles.default]\nprovider = \"openai\"\nmodel = \"gpt-5.5\"\nreasoning_profile = \"low\"\nlatency_preference = \"fast\"\n\n[model_profiles.default.provider_options]\nreasoning_effort = \"low\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.cache_provider_model_catalog_for_tests(
        "openai",
        vec![mez_agent::ProviderModelInfo {
            id: "gpt-5.5".to_string(),
            display_name: None,
            reasoning_levels: vec!["low".to_string(), "high".to_string()],
            context_window_tokens: Some(1_050_000),
            capabilities: Vec::new(),
        }],
        vec!["low".to_string(), "high".to_string()],
    );

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Reasoning,
                    },
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    let reasoning_items = service
        .pane_agent_status_selector()
        .map(|selector| selector.items.clone())
        .unwrap_or_default();
    let high_index = reasoning_items
        .iter()
        .position(|item| item == "high")
        .expect("reasoning selector should include high");
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::SelectPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Reasoning,
                        item_index: high_index,
                    },
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    let (_name, profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(profile.reasoning_profile.as_deref(), Some("high"));
    assert_eq!(profile.latency_preference.as_deref(), Some("fast"));
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get("%1").unwrap();
    assert_eq!(pane_context.agent_latency.as_deref(), Some("fast"));

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Latency,
                    },
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert!(
        service.pane_agent_status_selector().is_some(),
        "latency selector should remain available after reasoning changes"
    );
}

/// Verifies that `/routing` stores a pane-local override used by
/// subsequent turns without mutating the global configured default. This covers
/// the command surface for enabling, toggling, and inspecting automatic model
/// sizing.
#[test]
fn runtime_agent_shell_routing_command_sets_pane_override() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let enabled = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"routing-on","method":"agent/shell/command","params":{"idempotency_key":"routing-on","input":"/routing on"}}"#,
        &primary,
    );

    assert!(enabled.contains(r#""kind":"mutated""#), "{enabled}");
    assert!(enabled.contains(r#""command":"routing""#), "{enabled}");
    assert!(enabled.contains("enabled=true"), "{enabled}");
    assert!(enabled.contains("default=false"), "{enabled}");
    assert!(enabled.contains("changed=true"), "{enabled}");
    assert_eq!(service.agent_routing_override("%1"), Some(true));

    let status = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"routing-status","method":"agent/shell/command","params":{"idempotency_key":"routing-status","input":"/routing status"}}"#,
        &primary,
    );
    assert!(status.contains(r#""kind":"display""#), "{status}");
    assert!(status.contains("enabled=true"), "{status}");
    assert!(status.contains("override_present=true"), "{status}");

    let toggled = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"routing-toggle","method":"agent/shell/command","params":{"idempotency_key":"routing-toggle","input":"/routing toggle"}}"#,
        &primary,
    );
    assert!(toggled.contains(r#""kind":"mutated""#), "{toggled}");
    assert!(toggled.contains("enabled=false"), "{toggled}");
    assert!(toggled.contains("changed=true"), "{toggled}");
    assert_eq!(service.agent_routing_override("%1"), Some(false));
}

/// Verifies that routing runs an internal router request before
/// the turn provider request, applies the selected model and reasoning effort,
/// and keeps router prompt/response correspondence out of persisted model
/// context. Only the effective profile and bounded logs survive.
#[test]
fn runtime_agent_turn_routing_selects_profile_without_context_leak() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"
[agents]
default_provider = "runtime-batch"
default_model_profile = "default"
routing = true

[agents.auto_sizing]
router_model_profile = "router"
small_model_profile = "small"
medium_model_profile = "medium"
large_model_profile = "large"
allowed_reasoning_efforts = ["low", "medium", "high", "xhigh"]
fallback_policy = "use-default-profile"

[providers.runtime-batch]
kind = "openai"
models = ["gpt-router", "gpt-default", "gpt-5.3-codex", "gpt-5.4", "gpt-5.5"]
default_model = "gpt-default"

[model_profiles.default]
provider = "runtime-batch"
model = "gpt-default"
reasoning_profile = "medium"

[model_profiles.router]
provider = "runtime-batch"
model = "gpt-router"
reasoning_profile = "low"

[model_profiles.small]
provider = "runtime-batch"
model = "gpt-5.3-codex"
reasoning_profile = "medium"

[model_profiles.medium]
provider = "runtime-batch"
model = "gpt-5.4"
reasoning_profile = "medium"

[model_profiles.large]
provider = "runtime-batch"
model = "gpt-5.5"
reasoning_profile = "high"
"#
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let prompt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"auto-sized-prompt","method":"agent/shell/command","params":{"idempotency_key":"auto-sized-prompt","input":"implement this"}}"#,
        &primary,
    );
    assert!(prompt.contains(r#""state":"running""#), "{prompt}");
    assert_eq!(service.pending_agent_provider_tasks().len(), 1);
    let frame_context = service.terminal_frame_context();
    let pane_context = frame_context
        .panes
        .get("%1")
        .expect("routing pane context should exist");
    assert_eq!(pane_context.agent_status.as_deref(), Some("routing"));
    assert!(
        pane_context
            .agent_display_lines
            .iter()
            .any(|line| line.starts_with("routing (") && line.contains(" • esc to interrupt")),
        "{pane_context:?}"
    );
    let context = service.agent_turn_contexts_mut().get_mut("turn-1").unwrap();
    for block in [
            mez_agent::ContextBlock {
                source: ContextSourceKind::TranscriptAssistant,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "old minified assistant context for pane %1".to_string(),
                content: format!("minified-context:{}", "x".repeat(200 * 1024)),
            },
            mez_agent::ContextBlock {
                source: ContextSourceKind::TranscriptAssistant,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "transcript assistant entry 2 for pane %1".to_string(),
                content: "Recommended next tasks:\n1. Document the model picker.\n2. Clean up stale quota UI.\n3. Implement multi-file runtime auto-sizing.".to_string(),
            },
            mez_agent::ContextBlock {
                source: ContextSourceKind::TranscriptTool,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "previous tool output for pane %1".to_string(),
                content: "tool-only output should not reach the router".to_string(),
            },
            mez_agent::ContextBlock {
                source: ContextSourceKind::Policy,
                placement: mez_agent::ContextPlacement::StablePrefix,
                label: "policy context".to_string(),
                content: "policy-only context should not reach the router".to_string(),
            },
            mez_agent::ContextBlock {
                source: ContextSourceKind::RuntimeHint,
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "active routed runtime hint".to_string(),
                content: "runtime-hint sentinel must reach the routed worker".to_string(),
            },
            mez_agent::ContextBlock {
                source: ContextSourceKind::ActionResult,
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "active routed action result".to_string(),
                content: "action-result sentinel must reach the routed worker".to_string(),
            },
        ] {
        mez_agent::insert_context_block_by_placement(&mut context.blocks, block);
    }

    let provider = RuntimeAutoSizingProvider {
        requests: RefCell::new(Vec::new()),
    };
    let executions = service
        .poll_agent_provider_tasks_with_provider(&provider, 1)
        .unwrap();
    assert!(executions.is_empty());
    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].interaction_kind,
        mez_agent::ModelInteractionKind::AutoSizing
    );
    assert_eq!(requests[0].model, "gpt-router");
    assert!(requests[0].turn_id.ends_with(":auto-sizing"));
    let router_context = requests[0]
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(router_context.contains("implement this"));
    assert!(router_context.contains("Implement multi-file runtime auto-sizing"));
    assert!(router_context.contains("Latest submitted task"));
    assert!(router_context.contains("Referential prompt detected"));
    assert!(router_context.contains("Do not choose small/low merely because"));
    assert!(router_context.contains("Model size reflects task scope"));
    assert!(router_context.contains("reasoning effort reflects the depth and complexity"));
    assert!(router_context.contains("Small models are only for chat"));
    assert!(router_context.contains("Planning, investigation, complex implementation"));
    assert!(router_context.contains("Never choose low reasoning for coding"));
    assert!(router_context.contains("do not return only a discovery plan"));
    assert!(router_context.contains("[truncated for auto-sizing router]"));
    assert!(
        router_context.len() < 180 * 1024,
        "router context should stay bounded independently of model-window fallback estimates"
    );
    assert!(requests[0].messages.iter().any(|message| {
        message.role == mez_agent::ModelMessageRole::User
            && message.source == ContextSourceKind::UserInstruction
            && message.content.contains("implement this")
    }));
    assert!(requests[0].messages.iter().any(|message| {
        message.role == mez_agent::ModelMessageRole::Assistant
            && message.source == ContextSourceKind::TranscriptAssistant
            && message
                .content
                .contains("Implement multi-file runtime auto-sizing")
    }));
    assert!(
        !router_context.contains("tool-only output should not reach the router"),
        "{router_context}"
    );
    assert!(
        !router_context.contains("policy-only context should not reach the router"),
        "{router_context}"
    );
    let workflow = service
        .routed_workflow_for_tests("turn-1")
        .expect("routing should create a managed worker workflow");
    assert_eq!(
        workflow.phase,
        mez_agent::routed_workflow::RoutedWorkflowPhase::WaitingForWorkerResult,
        "{workflow:#?}"
    );
    assert_eq!(workflow.main_model_profile, "default");
    assert_eq!(workflow.worker_model_profile.as_deref(), Some("gpt-5.5"));
    assert_eq!(workflow.original_user_prompt, "implement this");
    let child_turn_id = workflow
        .child_turn_id
        .clone()
        .expect("managed worker turn should be queued");
    let child_profile = service
        .agent_turn_model_profile(&child_turn_id)
        .expect("managed worker profile should be pinned");
    assert_eq!(child_profile.model, "gpt-5.5");
    assert_eq!(child_profile.reasoning_profile.as_deref(), Some("high"));
    let child_context = service
        .agent_turn_contexts()
        .get(&child_turn_id)
        .expect("managed worker context should exist");
    assert_eq!(
        child_context
            .blocks
            .iter()
            .filter(|block| block.content == "implement this")
            .count(),
        1
    );
    assert!(child_context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::TranscriptAssistant
            && block
                .content
                .contains("Implement multi-file runtime auto-sizing")
    }));
    assert!(child_context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::TranscriptTool
            && block.content == "tool-only output should not reach the router"
    }));
    assert!(child_context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::RuntimeHint
            && block.content == "runtime-hint sentinel must reach the routed worker"
    }));
    assert!(child_context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block.content == "action-result sentinel must reach the routed worker"
    }));
    assert!(!child_context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::Policy
            && block.content == "policy-only context should not reach the router"
    }));
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == "turn-1")
            .map(|turn| turn.state),
        Some(mez_agent::AgentTurnState::Blocked)
    );
    drop(requests);
    let waiting_tasks = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"routed-worker-wait","method":"agent/task/list","params":{"target":{"agent_id":"agent-%1"}}}"#,
        &primary,
    );
    assert!(
        waiting_tasks.contains(r#""state":"waiting""#),
        "{waiting_tasks}"
    );
    assert!(
        waiting_tasks.contains(r#""approval_ids":[]"#),
        "{waiting_tasks}"
    );
    let status = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"auto-sizing-token-status","method":"agent/shell/command","params":{"idempotency_key":"auto-sizing-token-status","input":"/status"}}"#,
        &primary,
    );
    assert!(
        status.contains("| Pane agent tokens | gpt-router via runtime-batch:"),
        "{status}"
    );
    assert!(status.contains("### Mez Session Token Usage"), "{status}");
    assert!(
        status.contains("| runtime-batch | gpt-router | 60 | 30 | 10 | 3 | 33.33% |"),
        "{status}"
    );
    assert!(!status.contains("| runtime-batch | gpt-5.5 |"), "{status}");

    let completed_say_execution = |turn: &mez_agent::AgentTurnRecord, text: &str| {
        let action = mez_agent::AgentAction {
            id: format!("say-{}", turn.turn_id),
            rationale: "return the routed result".to_string(),
            payload: mez_agent::AgentActionPayload::Say {
                status: mez_agent::SayStatus::Final,
                text: text.to_string(),
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            },
        };
        mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "completed routed response".to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: None,
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![mez_agent::ActionResult::succeeded(
                turn,
                &action,
                vec![text.to_string()],
                None,
            )],
            final_turn: true,
            terminal_state: AgentTurnState::Completed,
        }
    };
    let completed_progress_and_final_execution =
        |turn: &mez_agent::AgentTurnRecord, progress: &str, final_text: &str| {
            let progress_action = mez_agent::AgentAction {
                id: format!("progress-{}", turn.turn_id),
                rationale: "report routed progress".to_string(),
                payload: mez_agent::AgentActionPayload::Say {
                    status: mez_agent::SayStatus::Progress,
                    text: progress.to_string(),
                    content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
                },
            };
            let final_action = mez_agent::AgentAction {
                id: format!("final-{}", turn.turn_id),
                rationale: "return the routed result".to_string(),
                payload: mez_agent::AgentActionPayload::Say {
                    status: mez_agent::SayStatus::Final,
                    text: final_text.to_string(),
                    content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
                },
            };
            mez_agent::AgentTurnExecution {
                request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
                response: mez_agent::ModelResponse {
                    provider: "runtime-batch".to_string(),
                    model: "test".to_string(),
                    raw_text: "completed routed response".to_string(),
                    usage: Default::default(),
                    latest_request_usage: None,
                    quota_usage: Default::default(),
                    action_batch: Some(mez_agent::MaapBatch {
                        protocol: "maap/1".to_string(),
                        rationale: "report progress and the final routed result".to_string(),
                        thought: None,
                        turn_id: turn.turn_id.clone(),
                        agent_id: turn.agent_id.clone(),
                        actions: vec![progress_action.clone(), final_action.clone()],
                        final_turn: true,
                    }),
                    provider_transcript_events: Vec::new(),
                },
                latest_response_usage: Default::default(),
                routing_token_usage_by_model: std::collections::BTreeMap::new(),
                action_results: vec![
                    mez_agent::ActionResult::succeeded(
                        turn,
                        &progress_action,
                        vec![progress.to_string()],
                        None,
                    ),
                    mez_agent::ActionResult::succeeded(
                        turn,
                        &final_action,
                        vec![final_text.to_string()],
                        None,
                    ),
                ],
                final_turn: true,
                terminal_state: AgentTurnState::Completed,
            }
        };
    let worker_turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == child_turn_id)
        .cloned()
        .expect("managed worker turn should remain recorded");
    let exact_worker_result = "Implemented the routed fix and verified its regression test.";
    let worker_progress = "Still running the routed regression test.";
    let worker_context = service
        .agent_turn_contexts_mut()
        .get_mut(&worker_turn.turn_id)
        .expect("managed worker context should be available before completion");
    for block in [
        mez_agent::ContextBlock {
            source: ContextSourceKind::TranscriptAssistant,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "live routed worker assistant context".to_string(),
            content: "live assistant sentinel must reach the routed handoff".to_string(),
        },
        mez_agent::ContextBlock {
            source: ContextSourceKind::ActionResult,
            placement: mez_agent::ContextPlacement::EphemeralTail,
            label: "live routed worker action result".to_string(),
            content: "live action-result sentinel must reach the routed handoff".to_string(),
        },
    ] {
        mez_agent::insert_context_block_by_placement(&mut worker_context.blocks, block);
    }
    assert!(
        service
            .handle_routed_child_execution_result(
                &worker_turn,
                &completed_progress_and_final_execution(
                    &worker_turn,
                    worker_progress,
                    exact_worker_result,
                ),
            )
            .unwrap()
    );

    let workflow = service
        .routed_workflow_for_tests("turn-1")
        .expect("worker completion should advance the routed workflow");
    assert_eq!(
        workflow.phase,
        mez_agent::routed_workflow::RoutedWorkflowPhase::WaitingForHandoff
    );
    assert_eq!(
        workflow.worker_final_result.as_deref(),
        Some(exact_worker_result)
    );
    assert_ne!(
        workflow.worker_final_result.as_deref(),
        Some(worker_progress)
    );
    let handoff_turn_id = workflow
        .child_turn_id
        .as_deref()
        .expect("worker completion should queue a handoff turn");
    let handoff_context = service
        .agent_turn_contexts()
        .get(handoff_turn_id)
        .expect("handoff context should be recorded");
    assert!(handoff_context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::TranscriptAssistant
            && block.content == "live assistant sentinel must reach the routed handoff"
    }));
    assert!(handoff_context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block.content == "live action-result sentinel must reach the routed handoff"
    }));
    assert!(handoff_context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::RoutedHandoff
            && block.label == "routed worker exact final result"
            && block.content == exact_worker_result
    }));

    let handoff_turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == handoff_turn_id)
        .cloned()
        .expect("handoff turn should remain recorded");
    assert!(
        service
            .handle_routed_child_execution_result(
                &handoff_turn,
                &completed_say_execution(&handoff_turn, "invalid handoff"),
            )
            .unwrap()
    );

    let workflow = service
        .routed_workflow_for_tests("turn-1")
        .expect("invalid handoff should retain the routed workflow");
    assert_eq!(workflow.handoff_repair_attempts, 1);
    let repair_turn_id = workflow
        .child_turn_id
        .as_deref()
        .expect("invalid handoff should queue one repair turn");
    let repair_context = service
        .agent_turn_contexts()
        .get(repair_turn_id)
        .expect("repair context should be recorded");
    assert_eq!(
        repair_context
            .blocks
            .iter()
            .filter(|block| {
                block.source == ContextSourceKind::RoutedHandoff
                    && block.label == "routed worker exact final result"
                    && block.content == exact_worker_result
            })
            .count(),
        1
    );
    assert!(repair_context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::RoutedHandoff
            && block.label == "invalid routed handoff output"
            && block.content == "invalid handoff"
    }));
    assert!(repair_context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::RuntimeHint
            && block.label == "routed handoff validation feedback"
            && block
                .content
                .contains("invalid routed handoff JSON: expected value")
    }));

    let repair_turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == repair_turn_id)
        .cloned()
        .expect("repair turn should remain recorded");
    let valid_handoff = r#"{"version":1,"result_summary":"Routing fix complete","decisions":["preserve exact output"],"evidence":["regression passed"],"changes":["added routed result context"],"validation":["focused test"],"assumptions":[],"unresolved_risks":[],"follow_up_context":[]}"#;
    assert!(
        service
            .handle_routed_child_execution_result(
                &repair_turn,
                &completed_say_execution(&repair_turn, valid_handoff),
            )
            .unwrap()
    );

    let parent_context = service
        .agent_turn_contexts()
        .get("turn-1")
        .expect("parent context should remain recorded");
    assert!(parent_context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::RoutedHandoff
            && block.label == "routed worker exact final result"
            && block.content == exact_worker_result
    }));
    assert!(parent_context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::RoutedHandoff
            && block.label == "routed worker handoff context"
            && block
                .content
                .contains("\"result_summary\":\"Routing fix complete\"")
    }));
    assert_eq!(
        service
            .routed_workflow_for_tests("turn-1")
            .map(|workflow| workflow.phase.clone()),
        Some(mez_agent::routed_workflow::RoutedWorkflowPhase::ReadyForPresentation)
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == "turn-1")
            .and_then(|turn| turn.initial_capability),
        Some(mez_agent::AgentCapability::RespondOnly),
        "routed presentation must expose a hard response-only action surface"
    );
    let parent_turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .cloned()
        .expect("parent turn should remain recorded");
    service
        .complete_running_agent_turn_and_start_ready(
            &parent_turn,
            AgentTurnState::Failed,
            "routed presentation failed",
        )
        .unwrap();
    let failed_workflow = service
        .routed_workflow_for_tests("turn-1")
        .expect("failed parent presentation should remain observable");
    assert_eq!(
        failed_workflow.phase,
        mez_agent::routed_workflow::RoutedWorkflowPhase::ExplainingError
    );
    assert_eq!(
        failed_workflow.diagnostic.as_deref(),
        Some("routed parent presentation failed")
    );
    assert!(failed_workflow.error_explanation_attempted);
    service
        .complete_running_agent_turn_and_start_ready(
            &parent_turn,
            AgentTurnState::Failed,
            "routed error explanation failed",
        )
        .unwrap();
    assert_eq!(
        service
            .routed_workflow_for_tests("turn-1")
            .map(|workflow| workflow.phase.clone()),
        Some(mez_agent::routed_workflow::RoutedWorkflowPhase::Failed),
        "the one allowed error explanation failure must remain observable"
    );
}

/// Verifies that an inaccessible routing model fails the turn instead of
/// silently falling back to the default profile.
///
/// Router provider request failures usually mean the configured router model is
/// unavailable to the account or provider. The user needs that provider error
/// surfaced so they can choose a routing model they can access.
#[test]
fn runtime_agent_turn_routing_provider_error_fails_turn() {
    struct InaccessibleRouterProvider {
        requests: RefCell<Vec<mez_agent::ModelRequest>>,
    }

    impl ModelProvider for InaccessibleRouterProvider {
        fn provider_id(&self) -> &str {
            "runtime-batch"
        }

        fn send_request(
            &self,
            request: &mez_agent::ModelRequest,
        ) -> Result<mez_agent::ModelResponse> {
            self.requests.borrow_mut().push(request.clone());
            if request.interaction_kind == mez_agent::ModelInteractionKind::AutoSizing {
                return Err(MezError::invalid_state(
                    "OpenAI Responses API returned status 404: model `gpt-5.3-codex-spark` is not available",
                )
                .with_provider_failure_json(
                    r#"{"status_code":404,"error":{"message":"model `gpt-5.3-codex-spark` is not available","type":"invalid_request_error","code":"model_not_found"}}"#,
                ));
            }
            Ok(runtime_say_response(
                &request.turn_id,
                "unexpected normal response",
                true,
            ))
        }
    }

    let mut service = test_runtime_service();
    let transcript_root = temp_root("runtime-routing-provider-fail-transcript");
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"
[agents]
default_provider = "runtime-batch"
default_model_profile = "default"
routing = true

[agents.auto_sizing]
router_model_profile = "router"
small_model_profile = "default"
medium_model_profile = "default"
large_model_profile = "default"
fallback_policy = "use-default-profile"

[providers.runtime-batch]
kind = "openai"
models = ["gpt-default", "gpt-5.3-codex-spark"]
default_model = "gpt-default"

[model_profiles.default]
provider = "runtime-batch"
model = "gpt-default"
reasoning_profile = "medium"

[model_profiles.router]
provider = "runtime-batch"
model = "gpt-5.3-codex-spark"
reasoning_profile = "low"
"#
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let prompt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"router-fail-prompt","method":"agent/shell/command","params":{"idempotency_key":"router-fail-prompt","input":"use routing"}}"#,
        &primary,
    );
    assert!(prompt.contains(r#""state":"running""#), "{prompt}");
    let provider = InaccessibleRouterProvider {
        requests: RefCell::new(Vec::new()),
    };
    let error = service
        .poll_agent_provider_tasks_with_provider(&provider, 1)
        .unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert!(
        error
            .message()
            .contains("auto-sizing router request failed for profile `router`"),
        "{error}"
    );
    assert_eq!(provider.requests.borrow().len(), 1);
    assert_eq!(
        provider.requests.borrow()[0].interaction_kind,
        mez_agent::ModelInteractionKind::AutoSizing
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == "turn-1")
            .map(|turn| turn.state),
        Some(AgentTurnState::Failed)
    );
    let entries = transcript_store.inspect(&conversation_id).unwrap();
    let failure = entries
        .iter()
        .find(|entry| {
            entry.role == mez_agent::transcript::TranscriptRole::Assistant
                && entry.content.contains("provider_error")
        })
        .unwrap();
    assert!(failure.content.contains("gpt-5.3-codex-spark"));
    assert!(service.pending_agent_provider_tasks().is_empty());
    let _ = fs::remove_dir_all(transcript_root);
}

/// Verifies that live agent pane rendering writes a separate durable
/// presentation log and does not leak presentation-only text into future model
/// context.
#[test]
fn runtime_agent_presentation_persistence_stays_out_of_model_context() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-presentation"));
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();

    service
        .append_agent_assistant_text_to_terminal_buffer("%1", "visual-only pane replay")
        .unwrap();

    let presentation = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    assert_eq!(presentation.len(), 1);
    assert_eq!(presentation[0].style_names, vec!["assistant"]);
    assert_eq!(
        presentation[0].display_lines,
        vec![String::from("mez> visual-only pane replay")]
    );
    assert!(
        presentation[0]
            .ansi_text
            .as_deref()
            .is_some_and(|text| text.contains("visual-only pane replay"))
    );
    assert!(transcript_store.inspect(&conversation_id).is_err());
    let context = service
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    assert!(
        context
            .blocks
            .iter()
            .all(|block| !block.content.contains("visual-only pane replay"))
    );
}

/// Verifies a shell command rejected before dispatch by pane readiness is fed
/// back to the model for correction.
///
/// `pane_not_ready` means the shell command never reached the pane shell. The
/// model should receive that readiness diagnostic and choose a different next
/// step instead of the turn failing immediately.
#[test]
fn runtime_shell_pane_not_ready_queues_model_self_correction() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "inspect the pager styling")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.remove_pending_agent_provider_task(&turn.turn_id);
    service.set_pane_readiness("%1", PaneReadinessState::InteractiveBlocked);

    let action = mez_agent::AgentAction {
        id: "shell-not-ready".to_string(),
        rationale: "inspect the render owner".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Inspect the render owner.".to_string(),
            command: "rg -n \"status pager\" src".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    let mut failed = mez_agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::Failed,
        "pane_not_ready",
        "pane %1 is not ready for agent shell input: interactive-blocked",
    )
    .unwrap();
    failed.structured_content_json = Some(
        serde_json::json!({
            "state": "not_ready",
            "readiness_state": "interactive-blocked",
            "command": "rg -n \"status pager\" src"
        })
        .to_string(),
    );
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "pane not ready".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![action],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![failed],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };

    let queued = service
        .queue_agent_failure_feedback_for_correction(
            &turn,
            &mut execution,
            "pane_not_ready_recovery",
        )
        .unwrap();

    assert!(queued);
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result shell-not-ready shell_command failed]")
            && block.content.contains("interactive-blocked")
    }));
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::RuntimeHint
            && block.content.contains("Shell-readiness recovery")
    }));
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: action failed; asking model to recover"),
        "{pane_text}"
    );
    service.terminate_all_pane_processes().unwrap();
}
