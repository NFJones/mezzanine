//! Runtime tests for routed-worker model-selection workflows.
//!
//! These tests own the product integration between automatic model selection
//! and the routed child lifecycle, including worker creation, loop ownership,
//! cancellation, recovery, terminal handoff, and late-result idempotency.

use super::*;
use crate::runtime::RuntimeAgentCompactionTask;
use crate::runtime::agent_state::RuntimeAgentCompactionTarget;
use mez_agent::AutoSizingRoutingSelection;

/// Builds a stable pane identity for routed primary-authority tests.
fn routed_path_resolution_environment(working_directory: &Path) -> mez_agent::EnvironmentSignature {
    mez_agent::EnvironmentSignature::new(
        "linux",
        "x86_64",
        None,
        "test-host",
        "test-user",
        None,
        "/bin/sh",
        mez_agent::ShellClassification::PosixSh,
        None,
        Some("/usr/bin:/bin".to_string()),
        working_directory.to_string_lossy(),
        None,
        true,
        None,
        Vec::new(),
    )
    .unwrap()
}

/// Starts a routed loop with one explicit worker startup mode.
fn selected_routed_loop_with_mode(
    command: &str,
    shell_mode: crate::runtime::config::ShellMode,
) -> (RuntimeSessionService, String, mez_agent::AgentTurnRecord) {
    let mut service = test_runtime_service();
    service.set_agent_default_shell_mode(shell_mode);
    service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(1)
        .unwrap();
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
        .apply_routing_selected_transition(
            &AgentId::opaque("agent-%1").unwrap(),
            &parent_turn_id,
            AutoSizingRoutingSelection {
                selected_profile: worker_profile,
                selected_profile_name: "default".to_string(),
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

/// Starts a routed loop whose worker is immediately ready through native mode.
fn selected_routed_loop(
    command: &str,
) -> (RuntimeSessionService, String, mez_agent::AgentTurnRecord) {
    selected_routed_loop_with_mode(command, crate::runtime::config::ShellMode::Native)
}

/// Verifies concurrent routed workers spawned by independent parent panes can
/// share one subagent bucket without sharing bootstrap or readiness ownership.
///
/// The second worker exercises the existing-window split path that differs
/// from the first worker's new-window path. Both panes must retain independent
/// bounded bootstrap transactions before either provider is allowed to run.
#[test]
fn runtime_concurrent_routed_workers_share_bucket_with_independent_bootstraps() {
    let mut service = test_runtime_service();
    service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(2)
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(120, 40).unwrap(), 120)
        .unwrap();
    let second_parent_pane = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    service.session.select_pane(&primary, "%1").unwrap();
    for pane_id in ["%1", second_parent_pane.as_str()] {
        let mut screen = TerminalScreen::new(Size::new(40, 12).unwrap(), 20).unwrap();
        screen.feed(b"ready\n");
        service.set_pane_screen(pane_id.to_string(), screen);
        service
            .agent_shell_store_mut()
            .enter_or_resume(pane_id)
            .unwrap();
        service
            .execute_agent_shell_loop_command(pane_id, "/loop inspect shared routed startup")
            .unwrap();
    }

    let parent_turns = ["%1", second_parent_pane.as_str()].map(|pane_id| {
        service
            .agent_loop_turns_for_tests()
            .iter()
            .find(|(_, loop_turn)| loop_turn.pane_id == pane_id)
            .map(|(turn_id, _)| turn_id.clone())
            .expect("each parent pane should own a routed loop turn")
    });
    let mut worker_turns = Vec::new();
    for (pane_id, parent_turn_id) in ["%1", second_parent_pane.as_str()]
        .into_iter()
        .zip(parent_turns)
    {
        let worker_profile = service
            .agent_turn_model_profile(&parent_turn_id)
            .cloned()
            .expect("parent profile should exist");
        service
            .apply_routing_selected_transition(
                &AgentId::opaque(format!("agent-{pane_id}")).unwrap(),
                &parent_turn_id,
                AutoSizingRoutingSelection {
                    selected_profile: worker_profile,
                    selected_profile_name: "default".to_string(),
                    routing_token_usage_by_model: std::collections::BTreeMap::new(),
                    decision_summary: None,
                    fallback: None,
                },
            )
            .unwrap();
        let worker_turn_id = service
            .routed_workflow_for_tests(&parent_turn_id)
            .and_then(|workflow| workflow.child_turn_id.clone())
            .expect("routing selection should create a worker turn");
        worker_turns.push(
            service
                .agent_turn_ledger()
                .turns()
                .iter()
                .find(|turn| turn.turn_id == worker_turn_id)
                .cloned()
                .expect("worker turn should remain in the ledger"),
        );
    }

    let worker_windows = worker_turns
        .iter()
        .map(|turn| {
            service
                .find_pane_descriptor(&turn.pane_id)
                .expect("worker pane should remain in the layout")
                .window_id
        })
        .collect::<Vec<_>>();
    assert_eq!(worker_windows[0], worker_windows[1]);
    assert_ne!(worker_turns[0].pane_id, worker_turns[1].pane_id);
    for worker in &worker_turns {
        assert!(service.pane_bootstrap_is_pending_for_tests(&worker.pane_id));
        assert_eq!(
            service.runtime_agent_surface_startup_phase_for_tests(&worker.pane_id),
            Some("managed-admitting")
        );
        assert_eq!(worker.state, AgentTurnState::Queued);
        assert!(!service.agent_provider_task_is_pending(&worker.turn_id));
        assert!(
            service
                .running_shell_transactions_for_tests()
                .values()
                .all(|transaction| transaction.pane_id != worker.pane_id)
        );
    }

    service.terminate_all_pane_processes().unwrap();
}

/// Verifies native routed-worker startup does not inherit pane readiness or
/// foreign-shell bootstrap requirements before provider dispatch.
#[test]
fn runtime_routed_worker_native_startup_skips_pane_readiness() {
    let (mut service, parent_turn_id, worker_turn) =
        selected_routed_loop("/loop --limit 3 use native routed startup");

    assert_eq!(
        service.runtime_agent_surface_startup_phase_for_tests(&worker_turn.pane_id),
        Some("ready")
    );
    assert_eq!(worker_turn.state, AgentTurnState::Running);
    assert!(service.agent_provider_task_is_pending(&worker_turn.turn_id));
    assert!(!service.pane_bootstrap_is_pending_for_tests(&worker_turn.pane_id));
    assert!(!service.pane_has_uncertified_foreign_shell_boundary(&worker_turn.pane_id));
    assert!(
        service
            .running_shell_transactions_for_tests()
            .values()
            .all(|transaction| transaction.pane_id != worker_turn.pane_id)
    );
    assert_eq!(
        service
            .routed_workflow_for_tests(&parent_turn_id)
            .map(|workflow| workflow.phase.clone()),
        Some(mez_agent::routed_workflow::RoutedWorkflowPhase::WaitingForWorkerResult)
    );

    service.terminate_all_pane_processes().unwrap();
}

/// Verifies restart metadata retains only the durable routed parent and never
/// restores its managed worker as an ordinary session on the parent transcript.
#[test]
fn runtime_routed_worker_checkpoint_restores_only_parent() {
    let (mut service, parent_turn_id, worker_turn) =
        selected_routed_loop("/loop --limit 3 inspect routed restart isolation");
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-routed-worker-checkpoint"));
    service.set_agent_transcript_store(transcript_store.clone());
    let mezzanine_session_id = service.session().id.as_str().to_string();
    let parent_pane_id = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == parent_turn_id)
        .map(|turn| turn.pane_id.clone())
        .expect("routed parent turn should remain in the ledger");
    let parent_conversation_id = service
        .agent_shell_store()
        .get(&parent_pane_id)
        .map(|session| session.session_id.clone())
        .expect("routed parent should retain its conversation");

    service.checkpoint_agent_session_metadata().unwrap();

    let metadata = transcript_store
        .load_agent_session_metadata(&mezzanine_session_id)
        .unwrap();
    assert_eq!(metadata.len(), 1);
    assert_eq!(metadata[0].pane_id, parent_pane_id);
    assert_eq!(metadata[0].conversation_id, parent_conversation_id);
    assert_eq!(
        metadata[0].running_turn_id.as_deref(),
        Some(parent_turn_id.as_str())
    );
    assert_eq!(
        metadata[0].running_turn_kind.as_deref(),
        Some("routed-workflow")
    );
    assert_ne!(metadata[0].pane_id, worker_turn.pane_id);

    let mut restored = test_runtime_service();
    restored.session.id = service.session().id.clone();
    restored.set_agent_transcript_store(transcript_store);
    let restored_count = restored
        .restore_agent_sessions_from_transcript_store()
        .unwrap();

    assert_eq!(restored_count, 1);
    assert_eq!(
        restored
            .agent_shell_store()
            .get(&parent_pane_id)
            .map(|session| session.session_id.as_str()),
        Some(parent_conversation_id.as_str())
    );
    assert!(
        restored
            .agent_shell_store()
            .get(&worker_turn.pane_id)
            .is_none()
    );
    assert_eq!(
        restored
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == parent_turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Interrupted)
    );
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

/// Verifies a lineage-owned subagent applies routing to its existing turn.
///
/// The classifier result must preserve the turn and agent identities, replace
/// only the turn-local profile, queue ordinary provider redispatch, and ignore
/// a duplicate classifier result without creating a routed-worker workflow.
#[test]
fn runtime_subagent_routing_applies_selected_profile_in_place_once() {
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
        r#"{"jsonrpc":"2.0","id":"in-place-routing","method":"agent/shell/command","params":{"idempotency_key":"in-place-routing","input":"route this child turn"}}"#,
        &primary,
    );
    assert!(prompt.contains(r#""state":"running""#), "{prompt}");
    service.set_subagent_lineage(
        "agent-%1",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-root".to_string(),
            root_agent_id: "agent-root".to_string(),
            depth: 1,
            display_name: "Child".to_string(),
        },
    );
    service.remove_pending_agent_provider_task("turn-1");
    let selected_profile = ModelProfile {
        reasoning_profile: Some("high".to_string()),
        ..runtime_model_profile("runtime-batch", "routed-child-model")
    };
    let selection = AutoSizingRoutingSelection {
        selected_profile: selected_profile.clone(),
        selected_profile_name: "default".to_string(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        decision_summary: Some("high reasoning on routed-child-model".to_string()),
        fallback: None,
    };
    let child_agent = AgentId::opaque("agent-%1").unwrap();

    service
        .apply_routing_selected_transition(&child_agent, "turn-1", selection.clone())
        .unwrap();

    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .expect("routed child turn should remain in place");
    assert_eq!(turn.agent_id, "agent-%1");
    assert_eq!(turn.pane_id, "%1");
    assert_eq!(turn.state, AgentTurnState::Running);
    assert_eq!(
        service.agent_turn_model_profile("turn-1"),
        Some(&selected_profile)
    );
    assert!(service.agent_turn_routing_applied("turn-1"));
    assert!(service.agent_provider_task_is_pending("turn-1"));
    assert!(service.routed_workflow_for_tests("turn-1").is_none());
    assert!(
        service
            .runtime_auto_sizing_dispatch_for_turn(turn, &selected_profile)
            .unwrap()
            .is_none()
    );

    service.remove_pending_agent_provider_task("turn-1");
    service
        .apply_routing_selected_transition(&child_agent, "turn-1", selection)
        .unwrap();
    assert!(!service.agent_provider_task_is_pending("turn-1"));
    assert!(service.routed_workflow_for_tests("turn-1").is_none());
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
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    service.set_agent_default_routing(true);
    service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(1)
        .unwrap();
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
    let selection = AutoSizingRoutingSelection {
        selected_profile: parent_profile,
        selected_profile_name: "default".to_string(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        decision_summary: Some("large/high".to_string()),
        fallback: None,
    };
    let parent_agent = AgentId::opaque("agent-%1").unwrap();

    service
        .apply_routing_selected_transition(&parent_agent, "turn-1", selection)
        .unwrap();

    assert_eq!(service.agent_scheduler().snapshot().waiting, 1);
    assert_eq!(service.agent_scheduler().snapshot().running, 1);
    assert_eq!(service.agent_scheduler().snapshot().active_capacity_used, 1);
    assert_eq!(service.agent_routing_override("%2"), Some(true));
    assert!(service.agent_auto_sizing_override("%2").is_some());
    let child_turn_id = service
        .routed_workflow_for_tests("turn-1")
        .and_then(|workflow| workflow.child_turn_id.as_deref())
        .expect("routed worker should own a child turn");
    assert!(service.agent_turn_routing_applied(child_turn_id));

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
    assert!(
        transcript_store
            .inspect_presentation(&child_conversation_id)
            .unwrap()
            .is_empty()
    );

    service
        .update_agent_shell_output_preview(
            "%2",
            crate::runtime::render::RuntimeAgentShellPreviewOwner {
                turn_id: child_turn_id.to_string(),
                action_id: "routed-shell".to_string(),
                marker: "routed-marker".to_string(),
            },
            1,
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
    assert!(presentation.is_empty(), "{presentation:#?}");
    assert!(
        transcript_store
            .saved_sessions()
            .unwrap()
            .iter()
            .all(|session| session.summary.conversation_id != child_conversation_id)
    );
    assert_ne!(child_conversation_id, "routed-turn-1-worker");
    transcript_store
        .append_presentation(&crate::storage::transcript::AgentPresentationEntry {
            conversation_id: child_conversation_id,
            sequence: 1,
            created_at_unix_seconds: 1,
            pane_id: "%2".to_string(),
            turn_id: None,
            terminal_width: 20,
            style_names: vec!["assistant".to_string()],
            display_lines: vec!["stale routed worker sentinel".to_string()],
            copy_lines: vec!["stale routed worker sentinel".to_string()],
            ansi_text: None,
            source_text: Some("stale routed worker sentinel".to_string()),
            source_content_type: Some(mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string()),
        })
        .unwrap();
    assert!(
        !service
            .rebuild_agent_presentation_after_resize("%2", Size::new(20, 4).unwrap())
            .unwrap()
    );
    let child_after_resize = service
        .pane_screen("%2")
        .expect("routed child screen should remain in place")
        .visible_lines()
        .join("\n");
    assert!(
        child_after_resize.contains("parent> implement routed logging"),
        "{child_after_resize}"
    );
    assert!(
        !child_after_resize.contains("stale routed worker sentinel"),
        "{child_after_resize}"
    );
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
        .retain_blocks(|block| {
            block.source != ContextSourceKind::UserInstruction || block.label != "user prompt"
        })
        .expect("removing the test prompt should preserve context validity");
    let selection = AutoSizingRoutingSelection {
        selected_profile: parent_profile,
        selected_profile_name: "default".to_string(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        decision_summary: Some("large/high".to_string()),
        fallback: None,
    };
    let parent_agent = AgentId::opaque("agent-%1").unwrap();

    service
        .apply_routing_selected_transition(&parent_agent, "turn-1", selection.clone())
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
        .blocks()
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
        .apply_routing_selected_transition(&parent_agent, "turn-1", selection)
        .unwrap();
    assert_eq!(service.pending_agent_provider_tasks().len(), 1);
    let replay_diagnostic_count = service
        .agent_turn_contexts()
        .get("turn-1")
        .expect("parent context should remain available")
        .blocks()
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
    let selection = AutoSizingRoutingSelection {
        selected_profile: parent_profile,
        selected_profile_name: "default".to_string(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        decision_summary: Some("large/high".to_string()),
        fallback: None,
    };
    let parent_agent = AgentId::opaque("agent-%1").unwrap();

    service
        .apply_routing_selected_transition(&parent_agent, "turn-1", selection)
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

/// Verifies a trace failure after child scheduler publication cannot orphan the
/// routed worker because workflow and reverse ownership are committed before
/// best-effort observability runs.
#[test]
fn runtime_routed_selection_keeps_enqueued_child_owned_after_trace_failure() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let prompt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"routed-enqueue-trace-failure","method":"agent/shell/command","params":{"idempotency_key":"routed-enqueue-trace-failure","input":"implement this"}}"#,
        &primary,
    );
    assert!(prompt.contains(r#""state":"running""#), "{prompt}");
    let parent_profile = service
        .agent_turn_model_profile("turn-1")
        .expect("parent profile should exist")
        .clone();
    service.fail_next_routed_child_enqueue_trace_for_tests();

    service
        .apply_routing_selected_transition(
            &AgentId::opaque("agent-%1").unwrap(),
            "turn-1",
            AutoSizingRoutingSelection {
                selected_profile: parent_profile,
                selected_profile_name: "default".to_string(),
                routing_token_usage_by_model: std::collections::BTreeMap::new(),
                decision_summary: None,
                fallback: None,
            },
        )
        .unwrap();

    let child_turn_id = service
        .routed_workflow_for_tests("turn-1")
        .and_then(|workflow| workflow.child_turn_id.clone())
        .expect("enqueued child should remain owned after trace failure");
    assert_eq!(
        service
            .routed_parent_turn_id_for_child(&child_turn_id)
            .as_deref(),
        Some("turn-1")
    );
    assert!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .any(|turn| turn.turn_id == child_turn_id
                && turn.parent_turn_id.as_deref() == Some("turn-1"))
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
    let selection = AutoSizingRoutingSelection {
        selected_profile: parent_profile.clone(),
        selected_profile_name: "default".to_string(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        decision_summary: None,
        fallback: None,
    };
    let parent_agent = AgentId::opaque("agent-%1").unwrap();

    service
        .apply_routing_selected_transition(&parent_agent, &parent_turn_id, selection)
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

/// Verifies a routed `/loop` continues when a worker applies a patch before a
/// later provider continuation completes without another patch action.
///
/// The active execution is replaced for each provider response. Retaining the
/// patch fact on the loop controller prevents the terminal response from
/// incorrectly classifying the iteration as patch-free.
#[test]
fn runtime_routed_loop_retains_patch_work_across_provider_continuations() {
    let (mut service, parent_turn_id, worker_turn) =
        selected_routed_loop("/loop --limit 3 retain routed patch evidence");
    let worker_profile = service
        .agent_turn_model_profile(&worker_turn.turn_id)
        .expect("routed worker profile should exist")
        .clone();

    let mut patch_execution = routed_patch_execution(&worker_turn);
    patch_execution
        .response
        .action_batch
        .as_mut()
        .unwrap()
        .final_turn = false;
    patch_execution.final_turn = false;
    patch_execution.terminal_state = AgentTurnState::Running;
    service
        .apply_agent_provider_execution(
            &worker_turn,
            &worker_profile,
            "runtime-batch",
            patch_execution,
        )
        .unwrap();

    let completion_batch =
        runtime_complete_batch_for(worker_turn.turn_id.clone(), worker_turn.agent_id.clone());
    let completion_result = mez_agent::ActionResult::succeeded(
        &worker_turn,
        &completion_batch.actions[0],
        vec!["Done.".to_string()],
        None,
    );
    service
        .apply_agent_provider_execution(
            &worker_turn,
            &worker_profile,
            "runtime-batch",
            mez_agent::AgentTurnExecution {
                request: runtime_model_request_fixture_for_agent(
                    &worker_turn.turn_id,
                    &worker_turn.agent_id,
                ),
                response: mez_agent::ModelResponse {
                    provider: "runtime-batch".to_string(),
                    model: "test".to_string(),
                    raw_text: "completed work".to_string(),
                    usage: Default::default(),
                    latest_request_usage: None,
                    quota_usage: Default::default(),
                    action_batch: Some(completion_batch),
                    provider_transcript_events: Vec::new(),
                },
                latest_response_usage: Default::default(),
                routing_token_usage_by_model: std::collections::BTreeMap::new(),
                action_results: vec![completion_result],
                final_turn: true,
                terminal_state: AgentTurnState::Completed,
            },
        )
        .unwrap();

    let workflow = service
        .routed_workflow_for_tests(&parent_turn_id)
        .expect("patch work should retain the routed workflow");
    assert_ne!(
        workflow.child_turn_id.as_deref(),
        Some(worker_turn.turn_id.as_str())
    );
    assert_eq!(
        workflow.phase,
        mez_agent::routed_workflow::RoutedWorkflowPhase::WaitingForWorkerResult
    );
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
        .apply_routing_selected_transition(
            &AgentId::opaque("agent-%1").unwrap(),
            &parent_turn_id,
            AutoSizingRoutingSelection {
                selected_profile: parent_profile,
                selected_profile_name: "default".to_string(),
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
    assert!(service.terminal_result_claimed_by_execution(&first_worker_turn_id));
    service
        .complete_running_agent_turn_and_start_ready(
            &first_worker_turn,
            AgentTurnState::Completed,
            "routed_loop_iteration_settled",
        )
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
    assert!(service.agent_turn_routing_applied(&second_worker_turn_id));
    let second_worker_profile = service
        .agent_turn_model_profile(&second_worker_turn_id)
        .expect("continued worker should retain its selected profile");
    assert!(
        service
            .runtime_auto_sizing_dispatch_for_turn(&second_worker_turn, second_worker_profile)
            .unwrap()
            .is_none()
    );
    let completion_batch = runtime_complete_batch_for(
        second_worker_turn_id.clone(),
        second_worker_turn.agent_id.clone(),
    );
    let completion_result = mez_agent::ActionResult::succeeded(
        &second_worker_turn,
        &completion_batch.actions[0],
        vec!["Done.".to_string()],
        None,
    );
    let patch_free_execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&second_worker_turn_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "completed work".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(completion_batch),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![completion_result],
        final_turn: true,
        terminal_state: AgentTurnState::Completed,
    };
    service
        .emit_subagent_task_result_for_execution(&second_worker_turn, &patch_free_execution)
        .unwrap();
    assert!(service.terminal_result_claimed_by_execution(&second_worker_turn_id));
    service
        .complete_running_agent_turn_and_start_ready(
            &second_worker_turn,
            AgentTurnState::Completed,
            "routed_loop_terminal_iteration_settled",
        )
        .unwrap();

    let workflow = service
        .routed_workflow_for_tests(&parent_turn_id)
        .expect("patch-free iteration should queue a terminal handoff");
    assert_eq!(
        workflow.phase,
        mez_agent::routed_workflow::RoutedWorkflowPhase::WaitingForHandoff,
        "{workflow:?}"
    );
    assert_ne!(
        workflow.child_turn_id.as_deref(),
        Some(second_worker_turn_id.as_str())
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!pane_text.contains("result committed: subagent task completed"));
}

/// Verifies a routed child that terminates without an execution record resumes
/// the blocked parent through bounded failure recovery rather than false success.
#[test]
fn runtime_routed_child_missing_execution_recovers_parent() {
    let (mut service, parent_turn_id, worker_turn) =
        selected_routed_loop("/loop --limit 3 recover missing routed execution");

    service
        .complete_running_agent_turn_and_start_ready(
            &worker_turn,
            AgentTurnState::Completed,
            "routed_worker_missing_execution",
        )
        .unwrap();

    let workflow = service
        .routed_workflow_for_tests(&parent_turn_id)
        .expect("missing execution should retain routed recovery state");
    assert_eq!(
        workflow.phase,
        mez_agent::routed_workflow::RoutedWorkflowPhase::ReadyForErrorExplanation
    );
    assert!(
        workflow
            .diagnostic
            .as_deref()
            .is_some_and(|value| value.contains("completed without an execution record"))
    );
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == parent_turn_id)
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!pane_text.contains("result committed: subagent task completed"));
}

/// Verifies a routed parent in a trusted project reacquires exact primary
/// authority before its continuation reaches provider planning. Missing cache
/// evidence queues the canonical project resolver instead of exposing
/// unresolved empty scopes to the classifier or Bubblewrap compiler.
#[test]
fn runtime_routed_parent_continuation_reacquires_trusted_project_authority() {
    let root = temp_root("runtime-routed-parent-primary-authority");
    let project_root = root.join("project");
    fs::create_dir_all(project_root.join(".git")).unwrap();
    let (mut service, parent_turn_id, worker_turn) =
        selected_routed_loop("/loop --limit 3 recover routed parent authority");
    service.set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Pane));
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut trust_store = ProjectTrustStore::default();
    trust_store
        .decide_at(
            project_root.clone(),
            TrustDecision::Trusted,
            Some(project_root.join(".git")),
            1,
        )
        .unwrap();
    service.set_project_trust_store(trust_store, None);
    service.set_pane_current_working_directory("%1".to_string(), project_root.clone());
    service.set_pane_environment_signature_for_tests(
        "%1",
        routed_path_resolution_environment(&project_root),
    );
    mark_test_pane_ready(&mut service, "%1");

    service
        .complete_running_agent_turn_and_start_ready(
            &worker_turn,
            AgentTurnState::Completed,
            "routed_worker_missing_execution",
        )
        .unwrap();
    let parent_turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == parent_turn_id)
        .cloned()
        .unwrap();
    assert!(service.path_scopes_for_pane("%1").is_none());

    assert!(
        service
            .claim_configured_agent_provider_task(
                &AgentId::opaque(parent_turn.agent_id.clone()).unwrap(),
                &parent_turn.turn_id,
            )
            .unwrap()
            .is_none()
    );
    let transaction = service
        .running_shell_transactions_for_tests()
        .values()
        .find(|transaction| {
            matches!(
                transaction.kind,
                RunningShellTransactionKind::PathResolution { .. }
            )
        })
        .unwrap();
    let RunningShellTransactionKind::PathResolution { cache_key, waiters } = &transaction.kind
    else {
        unreachable!();
    };
    let expected = project_root.to_string_lossy().into_owned();
    assert_eq!(cache_key.request.read_scopes, vec![expected.clone()]);
    assert_eq!(cache_key.request.write_scopes, vec![expected]);
    assert!(cache_key.request.additional_paths.is_empty());
    assert!(waiters.is_empty());

    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// Verifies a pane-mode routed worker remains queued until its authenticated
/// POSIX startup admission and environment bootstrap complete.
#[test]
fn runtime_routed_worker_waits_for_agent_surface_startup() {
    let (mut service, parent_turn_id, worker_turn) = selected_routed_loop_with_mode(
        "/loop --limit 3 wait for routed agent surface",
        crate::runtime::config::ShellMode::Pane,
    );

    assert_eq!(worker_turn.state, AgentTurnState::Queued);
    assert_eq!(
        service.runtime_agent_surface_startup_phase_for_tests(&worker_turn.pane_id),
        Some("managed-admitting")
    );
    assert!(!service.agent_provider_task_is_pending(&worker_turn.turn_id));
    assert!(!service.pane_has_uncertified_foreign_shell_boundary(&worker_turn.pane_id));
    let token = service
        .posix_startup_token_for_tests(&worker_turn.pane_id)
        .unwrap()
        .to_string();
    assert_eq!(
        service
            .observe_managed_shell_protocol_event(
                &worker_turn.pane_id,
                mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
                mez_terminal::ManagedShellAdapter::Posix,
                &token,
                &mez_terminal::ManagedShellProtocolEvent::AdapterAvailable { trigger: None },
            )
            .unwrap(),
        1
    );
    let (marker, bootstrap_turn_id) = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| {
            (transaction.pane_id == worker_turn.pane_id
                && transaction.kind == RunningShellTransactionKind::Bootstrap)
                .then(|| (marker.clone(), transaction.turn_id.clone()))
        })
        .expect("admitted routed startup should own bootstrap");
    service
        .observe_agent_shell_transaction_start(
            &worker_turn.pane_id,
            &marker,
            &bootstrap_turn_id,
            &worker_turn.agent_id,
            &worker_turn.pane_id,
        )
        .unwrap();
    let output = "env\tos\tLinux\nenv\tarch\tx86_64\nenv\thost\ttest-host\nenv\tuser\ttest-user\nenv\tshell_path\t/bin/sh\nenv\tshell_class\tposix-sh\nenv\tpath\t/usr/bin:/bin\nenv\tcwd\t/tmp\nenv\tgit_repo\t0\nbootstrap\tcomplete\t1714500000\n";
    let transaction = service
        .running_shell_transactions_mut_for_tests()
        .get_mut(&marker)
        .unwrap();
    transaction.observed_output_bytes = output.len();
    transaction.observed_output_preview = output.to_string();
    service
        .observe_agent_shell_transaction_end(
            &worker_turn.pane_id,
            &marker,
            &bootstrap_turn_id,
            &worker_turn.agent_id,
            &worker_turn.pane_id,
            0,
        )
        .unwrap();

    assert_eq!(
        service.runtime_agent_surface_startup_phase_for_tests(&worker_turn.pane_id),
        Some("ready")
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == worker_turn.turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Running)
    );
    assert!(service.agent_provider_task_is_pending(&worker_turn.turn_id));
    assert_eq!(
        service
            .routed_workflow_for_tests(&parent_turn_id)
            .map(|workflow| workflow.phase.clone()),
        Some(mez_agent::routed_workflow::RoutedWorkflowPhase::WaitingForWorkerResult)
    );

    service.terminate_all_pane_processes().unwrap();
}

/// Verifies routed pane startup timeout fails the queued worker and releases
/// the parent workflow instead of leaving either side bootstrapping.
#[test]
fn runtime_routed_worker_startup_timeout_releases_parent() {
    let (mut service, parent_turn_id, worker_turn) = selected_routed_loop_with_mode(
        "/loop --limit 3 bound routed startup",
        crate::runtime::config::ShellMode::Pane,
    );

    assert_eq!(
        service
            .recover_expired_runtime_agent_surface_startups(u64::MAX)
            .unwrap(),
        1
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == worker_turn.turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Failed)
    );
    assert!(!service.agent_provider_task_is_pending(&worker_turn.turn_id));
    assert_ne!(
        service
            .routed_workflow_for_tests(&parent_turn_id)
            .map(|workflow| workflow.phase.clone()),
        Some(mez_agent::routed_workflow::RoutedWorkflowPhase::WaitingForWorkerResult)
    );

    service.terminate_all_pane_processes().unwrap();
}

/// Verifies malformed automatic-compaction completion fails a routed worker
/// through normal terminal settlement and releases its blocked parent.
///
/// A provider completion without a usable summary previously discarded the
/// claimed compaction task and its resume turn id, leaving the worker running
/// and the routed parent blocked in `waiting` indefinitely.
#[test]
fn runtime_routed_child_malformed_compaction_completion_recovers_parent() {
    let (mut service, parent_turn_id, worker_turn) =
        selected_routed_loop("/loop --limit 3 recover malformed compaction completion");
    let model_profile = service
        .agent_turn_model_profile(&worker_turn.turn_id)
        .expect("worker profile should exist")
        .clone();
    let conversation_id = service
        .agent_shell_store()
        .get(&worker_turn.pane_id)
        .expect("worker shell should exist")
        .session_id
        .clone();
    service.claim_agent_compaction_task_state(
        worker_turn.pane_id.clone(),
        RuntimeAgentCompactionTask {
            pane_id: worker_turn.pane_id.clone(),
            conversation_id,
            source: "provider-output-limit".to_string(),
            transcript_entries: 1,
            retained_transcript_entries: 1,
            summarized_entries: 1,
            model_profile_name: worker_turn.model_profile.clone(),
            model_profile,
            request: runtime_model_request_fixture_for_agent(
                &worker_turn.turn_id,
                &worker_turn.agent_id,
            ),
            resume_turn_id: Some(worker_turn.turn_id.clone()),
            target: RuntimeAgentCompactionTarget::Conversation,
        },
    );

    assert!(
        service
            .apply_agent_compaction_completed_event(
                &worker_turn.pane_id,
                runtime_test_compaction_response("")
            )
            .unwrap()
    );

    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == worker_turn.turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Failed)
    );
    let workflow = service
        .routed_workflow_for_tests(&parent_turn_id)
        .expect("malformed compaction should retain routed recovery state");
    assert_eq!(
        workflow.phase,
        mez_agent::routed_workflow::RoutedWorkflowPhase::ReadyForErrorExplanation
    );
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == parent_turn_id)
    );
}

/// Verifies an automatic-compaction failure after summary parsing still fails
/// the routed worker and releases its blocked parent.
///
/// Removing the worker shell session after the task is claimed makes transcript
/// retention fail after the valid summary has already been parsed and stored.
/// Completion application must retain the resume turn id through that failure.
#[test]
fn runtime_routed_child_post_summary_compaction_failure_recovers_parent() {
    let (mut service, parent_turn_id, worker_turn) =
        selected_routed_loop("/loop --limit 3 recover post-summary compaction failure");
    let model_profile = service
        .agent_turn_model_profile(&worker_turn.turn_id)
        .expect("worker profile should exist")
        .clone();
    let conversation_id = service
        .agent_shell_store()
        .get(&worker_turn.pane_id)
        .expect("worker shell should exist")
        .session_id
        .clone();
    service.claim_agent_compaction_task_state(
        worker_turn.pane_id.clone(),
        RuntimeAgentCompactionTask {
            pane_id: worker_turn.pane_id.clone(),
            conversation_id,
            source: "provider-output-limit".to_string(),
            transcript_entries: 1,
            retained_transcript_entries: 1,
            summarized_entries: 1,
            model_profile_name: worker_turn.model_profile.clone(),
            model_profile,
            request: runtime_model_request_fixture_for_agent(
                &worker_turn.turn_id,
                &worker_turn.agent_id,
            ),
            resume_turn_id: Some(worker_turn.turn_id.clone()),
            target: RuntimeAgentCompactionTarget::Conversation,
        },
    );
    service
        .agent_shell_store_mut()
        .remove_session(&worker_turn.pane_id);

    assert!(
        service
            .apply_agent_compaction_completed_event(
                &worker_turn.pane_id,
                runtime_test_compaction_response("valid compacted summary")
            )
            .unwrap()
    );

    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == worker_turn.turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Failed)
    );
    let workflow = service
        .routed_workflow_for_tests(&parent_turn_id)
        .expect("post-summary failure should retain routed recovery state");
    assert_eq!(
        workflow.phase,
        mez_agent::routed_workflow::RoutedWorkflowPhase::ReadyForErrorExplanation
    );
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == parent_turn_id)
    );
}

/// Verifies a failed joined descendant resumes its routed worker with context.
///
/// The outer routed workflow keeps waiting for the worker while the worker
/// model decides how to handle the descendant failure.
#[test]
fn runtime_routed_worker_joined_child_failure_recovers_parent() {
    let (mut service, parent_turn_id, worker_turn) =
        selected_routed_loop("/loop --limit 3 recover failed joined descendant");
    let primary = service
        .session
        .layout_owner_client_id()
        .cloned()
        .expect("primary should remain attached");
    let child_pane = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume(child_pane.as_str())
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(24, 5).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen(child_pane.to_string(), screen);
    let child_turn = service
        .start_agent_prompt_turn(child_pane.as_str(), "joined descendant")
        .unwrap();
    let child_turn_record = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == child_turn.turn_id)
        .cloned()
        .expect("joined descendant turn should be recorded");
    let spawn = runtime_spawn_agent_action("spawn-descendant", "joined descendant");
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(
            &worker_turn.turn_id,
            &worker_turn.agent_id,
        ),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "spawn joined descendant".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "delegate joined work".to_string(),
                thought: None,
                turn_id: worker_turn.turn_id.clone(),
                agent_id: worker_turn.agent_id.clone(),
                actions: vec![spawn.clone()],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![mez_agent::ActionResult::running(
            &worker_turn,
            &spawn,
            vec!["waiting for joined descendant".to_string()],
            None,
        )],
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    service
        .agent_turn_executions_mut()
        .insert(worker_turn.turn_id.clone(), execution.clone());
    service
        .append_agent_execution_chronology(&worker_turn, &execution)
        .unwrap();
    service.insert_joined_subagent_dependency(
        child_turn.turn_id.clone(),
        JoinedSubagentDependency {
            parent_turn_id: worker_turn.turn_id.clone(),
            parent_action_id: spawn.id.clone(),
            child_turn_id: child_turn.turn_id.clone(),
            child_agent_id: child_turn.agent_id.clone(),
            child_display_name: Some("joined descendant".to_string()),
        },
    );
    service.remove_pending_agent_provider_task(&worker_turn.turn_id);
    service
        .agent_scheduler_mut()
        .wait_running(&worker_turn.turn_id)
        .unwrap();
    service
        .agent_turn_ledger_mut()
        .finish_turn(&worker_turn.turn_id, AgentTurnState::Blocked)
        .unwrap();
    service.start_ready_agent_turns().unwrap();

    service
        .complete_running_agent_turn_and_start_ready(
            &child_turn_record,
            AgentTurnState::Failed,
            "joined_descendant_failed",
        )
        .unwrap();

    assert!(!service.has_joined_subagent_dependency(&child_turn.turn_id));
    assert!(
        !service
            .agent_scheduler()
            .waiting_turns()
            .any(|work| work.turn_id == worker_turn.turn_id),
        "routed worker must leave dependency-waiting scheduler state"
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == worker_turn.turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Running)
    );
    let workflow = service
        .routed_workflow_for_tests(&parent_turn_id)
        .expect("routed workflow should remain active while the worker continues");
    assert_eq!(
        workflow.phase,
        mez_agent::routed_workflow::RoutedWorkflowPhase::WaitingForWorkerResult
    );
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == worker_turn.turn_id)
    );
    assert!(
        service
            .agent_turn_contexts()
            .get(&worker_turn.turn_id)
            .is_some_and(|context| context
                .blocks()
                .iter()
                .any(|block| { block.content.contains(r#""success":false"#) }))
    );
}

/// Verifies a routed worker retains sibling native-shell ownership while joined.
///
/// Provider batches may spawn several joined children and dispatch an independent
/// shell action together. The worker releases provider capacity and becomes
/// scheduler `Waiting` while those children run, but that dependency wait must
/// not invalidate the already-authorized native-shell action. After the shell
/// and every child settle, the routed worker must resume for model evaluation
/// instead of remaining visibly waiting forever.
#[test]
fn runtime_routed_worker_native_shell_survives_joined_child_wait() {
    let (mut service, parent_turn_id, worker_turn) =
        selected_routed_loop("/loop --limit 3 inspect with parallel descendants");
    service.configure_subagent_policy(4, 4, 3, 2, SubagentWaitPolicy::Join);
    service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(4)
        .unwrap();
    let shell = mez_agent::AgentAction {
        id: "shell-with-descendants".to_string(),
        rationale: "inspect while descendants run".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Inspect alongside descendants.".to_string(),
            command: "printf routed-shell-result".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "spawn three descendants and inspect locally".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "collect parallel evidence".to_string(),
                thought: None,
                turn_id: worker_turn.turn_id.clone(),
                agent_id: worker_turn.agent_id.clone(),
                actions: vec![
                    runtime_spawn_agent_action("spawn-one", "descendant one"),
                    runtime_spawn_agent_action("spawn-two", "descendant two"),
                    runtime_spawn_agent_action("spawn-three", "descendant three"),
                    shell,
                ],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    service
        .execute_agent_turn_with_provider(
            &worker_turn.turn_id,
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert!(
        service
            .agent_scheduler()
            .waiting_turns()
            .any(|work| work.turn_id == worker_turn.turn_id)
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == worker_turn.turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Blocked)
    );
    let descendants = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .filter(|turn| {
            service.subagent_task_parent(&turn.turn_id).as_deref()
                == Some(worker_turn.agent_id.as_str())
        })
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(descendants.len(), 3, "{descendants:#?}");
    for descendant in descendants {
        service
            .complete_running_agent_turn_and_start_ready(
                &descendant,
                AgentTurnState::Completed,
                "routed_descendant_completed",
            )
            .unwrap();
    }

    assert_eq!(service.joined_subagent_dependency_count(), 0);
    assert!(
        service
            .agent_scheduler()
            .waiting_turns()
            .any(|work| work.turn_id == worker_turn.turn_id)
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == worker_turn.turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Blocked)
    );

    let dispatch = service
        .claim_native_shell_action(&worker_turn.turn_id, "shell-with-descendants")
        .unwrap()
        .expect("dependency-waiting worker must retain its native shell action");
    let outcome = crate::runtime::execute_native_shell_dispatch(dispatch);
    assert!(service.complete_native_shell_action(outcome).unwrap());

    assert!(
        !service
            .agent_scheduler()
            .waiting_turns()
            .any(|work| work.turn_id == worker_turn.turn_id)
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == worker_turn.turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Running)
    );
    assert!(service.agent_provider_task_is_pending(&worker_turn.turn_id));
    assert_eq!(
        service
            .routed_workflow_for_tests(&parent_turn_id)
            .map(|workflow| workflow.phase.clone()),
        Some(mez_agent::routed_workflow::RoutedWorkflowPhase::WaitingForWorkerResult)
    );
    service.terminate_all_pane_processes().unwrap();
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
    service.fail_next_routed_parent_continuation_trace_for_tests();
    let execution = routed_patch_execution(&worker_turn);

    service
        .emit_subagent_task_result_for_execution(&worker_turn, &execution)
        .unwrap();
    service
        .agent_scheduler_mut()
        .complete(&worker_turn.turn_id)
        .unwrap();
    service.start_ready_agent_turns().unwrap();

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
            .blocks()
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
            .blocks()
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
    service
        .agent_scheduler_mut()
        .complete(&worker_turn.turn_id)
        .unwrap();
    service.start_ready_agent_turns().unwrap();

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

/// Verifies a routed worker uses its configured profile identity for safe
/// fallback reporting instead of its synthetic ledger display label.
///
/// Routed workers display `routed:<model>` in their turn ledger, which is not
/// a configured provider profile. A provider failure must retain the original
/// error, settle through the routed workflow, and report only the configured
/// safe fallback rather than attempting to resolve that display label.
#[test]
fn runtime_routed_worker_provider_failure_uses_configured_fallback_profile() {
    let (mut service, parent_turn_id, worker_turn) =
        selected_routed_loop("/loop --limit 3 recover configured routed fallback");
    let primary_profile = service
        .provider_registry()
        .resolve_profile("default")
        .unwrap();
    let safe_profile = primary_profile.clone();
    service
        .integration
        .provider_registry_mut()
        .profiles
        .insert("safe".to_string(), safe_profile);
    service
        .integration
        .provider_registry_mut()
        .fallback_profiles
        .insert("default".to_string(), vec!["safe".to_string()]);

    let error = MezError::invalid_state("routed provider outage");
    service
        .fail_agent_turn_for_provider_error(
            &worker_turn,
            &primary_profile.provider,
            &primary_profile,
            &error,
        )
        .unwrap();

    let context = service.agent_turn_contexts().get(&parent_turn_id).unwrap();
    assert!(
        context
            .blocks()
            .iter()
            .any(|block| { block.content.contains("routed provider outage") })
    );
    assert!(
        context
            .blocks()
            .iter()
            .any(|block| { block.content.contains("safe_fallback_profiles: safe") })
    );
    assert!(
        service
            .agent_turn_executions()
            .get(&worker_turn.turn_id)
            .is_none()
    );
}

/// Verifies resumed shell dispatch presents a routed worker failure before
/// terminal settlement removes the worker pane.
///
/// Bubblewrap capability completion and other asynchronous preparation paths
/// resume through the stored-dispatch entry point. If dispatch fails, routed
/// settlement immediately closes the ephemeral worker pane. The result must
/// therefore reach the worker's agent presentation buffer before that close;
/// attempting presentation afterward used to escape as a fatal missing-pane
/// service error instead of releasing the parent for bounded explanation.
#[test]
fn runtime_routed_worker_stored_dispatch_failure_presents_before_pane_close() {
    let (mut service, parent_turn_id, worker_turn) =
        selected_routed_loop("/loop --limit 3 recover resumed shell dispatch");
    service.remove_pending_agent_provider_task(&worker_turn.turn_id);
    mark_test_pane_ready(&mut service, &worker_turn.pane_id);

    let action = mez_agent::AgentAction {
        id: "resumed-shell-dispatch".to_string(),
        rationale: "inspect the worker directory".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Inspect the worker directory.".to_string(),
            command: "pwd".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    let retained_evaluation = service
        .permission_policy_for_turn(&worker_turn)
        .evaluate_shell_command_structured("pwd");
    service.permission_policy_mut().add_rule(
        mez_agent::permissions::CommandRule::new(["pwd"], RuleDecision::Forbid, RuleMatch::Exact)
            .unwrap(),
    );
    let mut result = mez_agent::ActionResult::running(
        &worker_turn,
        &action,
        vec!["shell action retained for asynchronous dispatch".to_string()],
        None,
    );
    result.permission_evaluation = Some(Box::new(retained_evaluation));
    service.agent_turn_executions_mut().insert(
        worker_turn.turn_id.clone(),
        mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture_for_agent(
                &worker_turn.turn_id,
                &worker_turn.agent_id,
            ),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "run pwd".to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "exercise stored shell dispatch settlement".to_string(),
                    thought: None,
                    turn_id: worker_turn.turn_id.clone(),
                    agent_id: worker_turn.agent_id.clone(),
                    actions: vec![action],
                    final_turn: false,
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![result],
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        },
    );

    let execution = service
        .dispatch_stored_running_shell_actions(&worker_turn.turn_id)
        .expect("stored dispatch failure should settle without targeting a removed pane")
        .expect("stored dispatch should return its terminal execution");

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert_eq!(execution.action_results[0].status, ActionStatus::Failed);
    assert_eq!(
        execution.action_results[0]
            .error
            .as_ref()
            .map(|error| error.code.as_str()),
        Some("forbidden")
    );
    assert!(service.find_pane_descriptor(&worker_turn.pane_id).is_none());
    assert!(
        service
            .agent_shell_store()
            .get(&worker_turn.pane_id)
            .is_none()
    );
    assert_eq!(
        service
            .routed_workflow_for_tests(&parent_turn_id)
            .map(|workflow| workflow.phase.clone()),
        Some(mez_agent::routed_workflow::RoutedWorkflowPhase::ReadyForErrorExplanation)
    );
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == parent_turn_id)
    );
}

/// Verifies a routed worker whose shell action is denied after a persistent
/// foreground-process block releases its parent for bounded error explanation.
#[test]
fn runtime_routed_worker_foreground_dispatch_block_recovers_parent() {
    let (mut service, parent_turn_id, worker_turn) =
        selected_routed_loop("/loop --limit 3 inspect foreground dispatch recovery");
    let action = mez_agent::AgentAction {
        id: "shell-blocked".to_string(),
        rationale: "inspect without disturbing the foreground program".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Inspect the working directory.".to_string(),
            command: "pwd".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    let denied = mez_agent::ActionResult::failed(
        &worker_turn,
        &action,
        ActionStatus::Denied,
        "foreground_process_blocked_dispatch",
        "the foreground process remained active",
    )
    .unwrap();
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(
            &worker_turn.turn_id,
            &worker_turn.agent_id,
        ),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "shell dispatch blocked".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "inspect with shell".to_string(),
                thought: None,
                turn_id: worker_turn.turn_id.clone(),
                agent_id: worker_turn.agent_id.clone(),
                actions: vec![action],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![denied],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };

    service
        .emit_subagent_task_result_for_execution(&worker_turn, &execution)
        .unwrap();
    service
        .agent_scheduler_mut()
        .complete(&worker_turn.turn_id)
        .unwrap();
    service.start_ready_agent_turns().unwrap();

    let workflow = service
        .routed_workflow_for_tests(&parent_turn_id)
        .expect("foreground dispatch failure should retain recovery state");
    assert_eq!(
        workflow.phase,
        mez_agent::routed_workflow::RoutedWorkflowPhase::ReadyForErrorExplanation
    );
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
            assert!(!worker_context.blocks().iter().any(|block| {
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
    let selection = AutoSizingRoutingSelection {
        selected_profile: parent_profile,
        selected_profile_name: "default".to_string(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        decision_summary: Some("large/high".to_string()),
        fallback: None,
    };
    let parent_agent = AgentId::opaque("agent-%1").unwrap();

    service
        .apply_routing_selected_transition(&parent_agent, "turn-1", selection)
        .unwrap();

    let parent_turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .expect("parent turn should remain in the ledger");
    assert_eq!(parent_turn.state, mez_agent::AgentTurnState::Failed);
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert!(service.routed_workflow_for_tests("turn-1").is_none());
    assert!(!service.has_active_routed_workflow("turn-1"));
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
        .blocks()
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
            .blocks()
            .iter()
            .filter(|block| {
                block.label == "routed worker exact final result" && block.content == exact_result
            })
            .count(),
        1
    );
    assert_eq!(
        parent_context
            .blocks()
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
    parent_service
        .running_shell_transactions_mut_for_tests()
        .insert(
            "routed-interrupt".to_string(),
            RunningShellTransactionRef {
                turn_id: child_turn_id.clone(),
                kind: RunningShellTransactionKind::AgentAction {
                    action_id: "routed-interrupt".to_string(),
                },
                pane_id: child_turn.pane_id.clone(),
                command: "sleep 60".to_string(),
                started_at_unix_ms: 0,
                timeout_ms: None,
                pending_input_payload: None,
                observed_output_bytes: 0,
                observed_output_preview: String::new(),
                observed_output_truncated: false,
            },
        );
    parent_service.fail_next_pane_interrupt_write_for_tests();
    let cancellation_error = parent_service.stop_agent_turn_for_pane("%1").unwrap_err();
    assert!(
        cancellation_error
            .message()
            .contains("injected pane interrupt write failure")
    );
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
    assert!(
        parent_service
            .running_shell_transactions_for_tests()
            .is_empty()
    );
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
