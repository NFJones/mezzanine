//! Composed generated bucket-title tests at actual persistent pane removal.
//!
//! These fixtures provision idle native children through the real spawn action
//! and close through the real action executor, without a provider request or a
//! manual title refresh. Persistent work completion alone is not pane removal.

use super::*;

/// Provisions a parent and idle persistent children sharing a generated bucket.
/// Returns the authoritative parent turn and each child's identity and name;
/// all shell processes are local fixture processes, never paid providers.
fn title_children(
    count: usize,
) -> (
    RuntimeSessionService,
    AgentTurnRecord,
    Vec<(String, String)>,
) {
    let mut service = test_runtime_service();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    service.set_agent_transcript_store(AgentTranscriptStore::new(temp_root("bucket-close-title")));
    service
        .attach_primary("primary", true, Size::new(240, 80).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let parent = service
        .start_agent_prompt_turn("%1", "provision reusable title fixtures")
        .unwrap();
    service.remove_pending_agent_provider_task(&parent.turn_id);
    let turn = service
        .agent_turn_ledger()
        .turn(&parent.turn_id)
        .cloned()
        .unwrap();
    let mut children = Vec::new();
    for index in 0..count {
        let mut action = runtime_spawn_agent_action(&format!("title-child-{index}"), "");
        let mez_agent::AgentActionPayload::SpawnAgent {
            lifetime,
            objective,
            ..
        } = &mut action.payload
        else {
            unreachable!()
        };
        *lifetime = mez_agent::SubagentLifetime::Persistent;
        *objective = Some("Reusable title fixture".to_string());
        let result = service
            .execute_spawn_action_for_turn(&turn, &action)
            .unwrap();
        let value: serde_json::Value =
            serde_json::from_str(result.structured_content_json.as_deref().unwrap()).unwrap();
        children.push((
            value["spawn"]["agent"]["id"].as_str().unwrap().to_string(),
            value["spawn"]["agent"]["display_name"]
                .as_str()
                .unwrap()
                .to_string(),
        ));
    }
    (service, turn, children)
}

/// Executes a planned close through the runtime's actual persistent executor.
/// This intentionally does not call any naming helper, spawn another child, or
/// synthesize removal so the regression proves the user-visible close path.
fn close_title_child(
    service: &mut RuntimeSessionService,
    turn: &AgentTurnRecord,
    agent_id: &str,
) -> mez_agent::ActionResult {
    let action = mez_agent::AgentAction {
        id: "title-close".to_string(),
        payload: mez_agent::AgentActionPayload::CloseAgent {
            agent_id: agent_id.to_string(),
        },
    };
    let planned =
        mez_agent::plan_action_result(turn, &action, mez_agent::ActionPlanningInput::default())
            .unwrap();
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "close title fixture".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "close title fixture".to_string(),
                actions: vec![action],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: vec![planned],
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    assert_eq!(
        service
            .execute_running_close_agent_actions_for_turn(turn, &mut execution)
            .unwrap(),
        1
    );
    execution.action_results.remove(0)
}

/// Verifies actual close_agent removal immediately replaces the two-child title
/// with the surviving persistent child's display name and retires authority.
/// No task-stop event or explicit/manual refresh is used to repair presentation.
#[test]
fn runtime_close_agent_refreshes_surviving_generated_bucket_title() {
    let (mut service, turn, children) = title_children(2);
    let first_pane = children[0].0.strip_prefix("agent-").unwrap();
    let second_pane = children[1].0.strip_prefix("agent-").unwrap();
    let bucket = service.find_pane_descriptor(first_pane).unwrap().window_id;
    assert_eq!(
        service.find_pane_descriptor(second_pane).unwrap().window_id,
        bucket
    );
    let result = close_title_child(&mut service, &turn, &children[0].0);
    assert_eq!(result.status, ActionStatus::Succeeded);
    assert!(service.find_pane_descriptor(first_pane).is_none());
    assert!(service.persistent_subagent(&children[0].0).is_none());
    assert!(service.subagent_lineage(&children[0].0).is_none());
    let title = service
        .session
        .windows()
        .iter()
        .find(|window| window.id == bucket)
        .unwrap()
        .name
        .clone();
    service.terminate_all_pane_processes().unwrap();
    assert_eq!(title, children[1].1);
}

/// Reads the title through the survivor's current membership, never by matching
/// or editing substrings in the previously generated window name.
fn title_for_child(service: &RuntimeSessionService, child: &(String, String)) -> String {
    let pane_id = child.0.strip_prefix("agent-").unwrap();
    let bucket = service.find_pane_descriptor(pane_id).unwrap().window_id;
    service
        .session
        .windows()
        .iter()
        .find(|window| window.id == bucket)
        .unwrap()
        .name
        .clone()
}

/// Verifies each possible removal position updates overflow and promotes hidden
/// names from current pane order. Repeated closure remains a no-op; removal of
/// the final pane deletes and prunes its window rather than renaming a ghost.
#[test]
fn runtime_bucket_title_removal_updates_overflow_first_middle_and_last() {
    for removed in 0..4 {
        let (mut service, turn, children) = title_children(4);
        let bucket = service
            .find_pane_descriptor(children[0].0.strip_prefix("agent-").unwrap())
            .unwrap()
            .window_id;
        assert_eq!(
            title_for_child(&service, &children[0]),
            format!("{}/{}/{}+1", children[0].1, children[1].1, children[2].1)
        );
        assert_eq!(
            close_title_child(&mut service, &turn, &children[removed].0).status,
            ActionStatus::Succeeded
        );
        let survivors = children
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != removed)
            .map(|(_, child)| child)
            .collect::<Vec<_>>();
        assert_eq!(
            title_for_child(&service, survivors[0]),
            survivors
                .iter()
                .map(|child| child.1.as_str())
                .collect::<Vec<_>>()
                .join("/")
        );
        let repeated = close_title_child(&mut service, &turn, &children[removed].0);
        assert_eq!(repeated.status, ActionStatus::Succeeded);
        assert!(
            repeated
                .structured_content_json
                .unwrap()
                .contains("\"closed\":false")
        );
        for (index, child) in survivors.iter().enumerate() {
            assert_eq!(
                close_title_child(&mut service, &turn, &child.0).status,
                ActionStatus::Succeeded
            );
            if let Some(survivor) = survivors.get(index + 1) {
                assert_eq!(
                    title_for_child(&service, survivor),
                    survivors[index + 1..]
                        .iter()
                        .map(|child| child.1.as_str())
                        .collect::<Vec<_>>()
                        .join("/")
                );
            }
        }
        assert!(
            !service
                .session
                .windows()
                .iter()
                .any(|window| window.id == bucket)
        );
        assert!(!service.is_subagent_window(bucket.as_str()));
        service.terminate_all_pane_processes().unwrap();
    }
}

/// Verifies user-authored bucket titles survive actual closure unchanged, even
/// when they contain both child names and generated-looking slash syntax.
#[test]
fn runtime_bucket_title_removal_preserves_explicit_window_name() {
    let (mut service, turn, children) = title_children(2);
    let primary = service.session.layout_owner_client_id().cloned().unwrap();
    let bucket = service
        .find_pane_descriptor(children[0].0.strip_prefix("agent-").unwrap())
        .unwrap()
        .window_id;
    let explicit = format!("my {}/{}+7", children[0].1, children[1].1);
    service
        .session
        .rename_window(&primary, Some(bucket.as_str()), explicit.clone())
        .unwrap();
    assert_eq!(
        close_title_child(&mut service, &turn, &children[0].0).status,
        ActionStatus::Succeeded
    );
    assert_eq!(title_for_child(&service, &children[1]), explicit);
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies detached spawn rollback refreshes the survivor without requiring an
/// attached layout controller. Reconciliation may repeat after actual removal,
/// but does not reclose the pane or change an already correct generated title.
#[test]
fn runtime_bucket_title_detached_failed_spawn_cleanup_is_idempotent() {
    let (mut service, _, children) = title_children(2);
    let primary = service.session.layout_owner_client_id().cloned().unwrap();
    service
        .detach_primary(&primary, Size::new(240, 80).unwrap())
        .unwrap();
    let pane = children[0].0.strip_prefix("agent-").unwrap();
    service.cleanup_failed_subagent_spawn(None, pane, &children[0].0, None);
    assert!(service.find_pane_descriptor(pane).is_none());
    assert!(service.session.layout_owner_client_id().is_none());
    assert_eq!(title_for_child(&service, &children[1]), children[1].1);
    let snapshot = service.session.windows().to_vec();
    let revision = service.session.mutation_revision();
    service.cleanup_removed_pane_runtime_state(pane).unwrap();
    service.cleanup_removed_pane_runtime_state(pane).unwrap();
    assert_eq!(service.session.windows(), snapshot);
    assert_eq!(service.session.mutation_revision(), revision);
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a stale watcher and a process failure leave open persistent children
/// named, while a matching asynchronous process exit removes the pane and
/// refreshes its bucket. A repeated late exit is ignored and cannot resurrect it.
#[test]
fn runtime_bucket_title_async_process_exit_and_failure_follow_membership() {
    let (mut service, _, children) = title_children(2);
    let pane = children[0].0.strip_prefix("agent-").unwrap();
    let original = title_for_child(&service, &children[1]);
    let pid = service.pane_processes().primary_pid(pane).unwrap();
    let status = mez_mux::process::PaneExitStatus {
        code: Some(0),
        signal: None,
        success: true,
    };
    assert!(
        service
            .apply_pane_process_exit_event(pane, pid.saturating_add(1), status)
            .unwrap()
            .is_none()
    );
    service
        .apply_pane_process_failure_event(pane, "fixture watcher failure")
        .unwrap();
    assert_eq!(title_for_child(&service, &children[1]), original);
    assert!(service.persistent_subagent(&children[0].0).is_some());
    let mut process = service.take_running_pane_process_for_adapter(pane).unwrap();
    assert_eq!(process.primary_pid(), pid);
    let token = mez_mux::process::process_start_token_for_pid(pid).unwrap();
    let status = process.terminate(Duration::from_millis(10)).unwrap();
    let update = service
        .apply_pane_process_exit_event(pane, pid, status)
        .unwrap()
        .unwrap();
    assert_ne!(
        mez_mux::process::process_start_token_for_pid(pid),
        Some(token)
    );
    assert!(!update.closed_window);
    assert!(service.find_pane_descriptor(pane).is_none());
    assert!(service.persistent_subagent(&children[0].0).is_none());
    assert_eq!(title_for_child(&service, &children[1]), children[1].1);
    let snapshot = service.session.windows().to_vec();
    let revision = service.session.mutation_revision();
    assert!(
        service
            .apply_pane_process_exit_event(pane, pid, status)
            .unwrap()
            .is_none()
    );
    assert_eq!(service.session.windows(), snapshot);
    assert_eq!(service.session.mutation_revision(), revision);
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies the existing automatic-close path and rendered attached frame both
/// see the repaired name immediately, without another spawn or refresh. Healthy
/// sibling windows and the controller's focus remain unchanged by the scan.
#[test]
fn runtime_bucket_title_auto_close_reaches_frame_and_preserves_siblings() {
    let (mut service, _, children) = title_children(2);
    let primary = service.session.layout_owner_client_id().cloned().unwrap();
    let pane = children[0].0.strip_prefix("agent-").unwrap();
    let bucket = service.find_pane_descriptor(pane).unwrap().window_id;
    let sibling = service
        .session
        .windows()
        .iter()
        .find(|window| window.id != bucket)
        .unwrap()
        .clone();
    let focus = service.session.active_window().unwrap().id.clone();
    let child_turn = service
        .start_agent_prompt_turn(pane, "offline terminal-close fixture")
        .unwrap();
    service.remove_pending_agent_provider_task(&child_turn.turn_id);
    let record = service
        .agent_turn_ledger()
        .turn(&child_turn.turn_id)
        .cloned()
        .unwrap();
    service.mark_terminal_subagent_pane_close_for_tests(pane);
    service
        .close_terminal_subagent_pane_if_pending(&record)
        .unwrap();
    assert_eq!(service.session.active_window().unwrap().id, focus);
    assert_eq!(
        service
            .session
            .windows()
            .iter()
            .find(|window| window.id == sibling.id)
            .unwrap(),
        &sibling
    );
    assert_eq!(title_for_child(&service, &children[1]), children[1].1);
    service
        .session
        .select_window(&primary, bucket.as_str())
        .unwrap();
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(240, 80).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    let rendered = view.lines.join("\n");
    service.terminate_all_pane_processes().unwrap();
    assert!(rendered.contains(&children[1].1));
    assert!(!rendered.contains(&children[0].1));
}

/// Verifies the fallback composer still uses current pane titles after lineage
/// disappears and uses `subagents` when no surviving pane has a usable name.
/// Deferred removal is repaired only after membership has actually changed.
#[test]
fn runtime_bucket_title_deferred_removal_uses_fallback_without_lineage() {
    let (mut service, _, children) = title_children(2);
    let primary = service.session.layout_owner_client_id().cloned().unwrap();
    let removed = children[0].0.strip_prefix("agent-").unwrap();
    let survivor = children[1].0.strip_prefix("agent-").unwrap();
    let original = title_for_child(&service, &children[1]);
    let started = service
        .start_agent_prompt_turn(removed, "retained persistent turn fixture")
        .unwrap();
    service.remove_pending_agent_provider_task(&started.turn_id);
    assert_eq!(
        service
            .fail_agent_turns_for_pane_shutdown(&[removed.to_string()], "fixture stopped work")
            .unwrap(),
        1
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&started.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Failed
    );
    assert_eq!(title_for_child(&service, &children[1]), original);
    assert!(service.find_pane_descriptor(removed).is_some());
    service.remove_subagent_authority_state(&children[1].0);
    service
        .session
        .set_pane_title_explicit(survivor, "shell")
        .unwrap();
    service
        .session
        .kill_pane_with_effects(&primary, Some(removed), true)
        .unwrap();
    service
        .terminate_runtime_pane_process(removed, true)
        .unwrap();
    service.cleanup_removed_pane_runtime_state(removed).unwrap();
    assert_eq!(title_for_child(&service, &children[1]), "subagents");
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a later metadata failure reports failure without undoing actual
/// pane removal or the repaired title. The survivor remains authoritative;
/// retrying close cannot replay termination for the already removed child.
#[test]
fn runtime_bucket_title_committed_close_survives_later_checkpoint_failure() {
    let (mut service, turn, children) = title_children(2);
    let primary = service.session.layout_owner_client_id().cloned().unwrap();
    let pane = children[0].0.strip_prefix("agent-").unwrap();
    service
        .persistence
        .cloned_transcript_store()
        .unwrap()
        .fail_next_agent_session_metadata_write();
    let error = service
        .dispatch_runtime_pane_close(&primary, &format!(r#"{{"pane_id":"{pane}","force":true}}"#))
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("agent session metadata write failure")
    );
    assert!(service.find_pane_descriptor(pane).is_none());
    assert!(!service.has_subagent_authority_state(&children[0].0));
    assert!(service.persistent_subagent(&children[1].0).is_some());
    assert_eq!(title_for_child(&service, &children[1]), children[1].1);
    let revision = service.session.mutation_revision();
    assert_eq!(
        close_title_child(&mut service, &turn, &children[0].0).status,
        ActionStatus::Rejected
    );
    assert_eq!(service.session.mutation_revision(), revision);
    service.terminate_all_pane_processes().unwrap();
}
