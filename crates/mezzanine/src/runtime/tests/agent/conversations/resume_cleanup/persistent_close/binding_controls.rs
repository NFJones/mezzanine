//! Native, restored, absent and retired persistent-close binding controls.
//!
//! These scenarios extend the causal saved-conversation fixture without live
//! tools, external agents, providers or arbitrary PID signaling. Every real
//! process is owned by CleanupFixture or an exact handed-off CleanupAdapter.

use super::*;

mod parent_replacement;
mod saved_fork;

/// Leaves a real recorded parent turn idle so user-directed resume is legal.
/// Its original identity remains the stale-action witness after replacement.
fn idle_parent_turn(fixture: &mut CleanupFixture) -> AgentTurnRecord {
    let turn = parent_turn(fixture);
    fixture
        .service
        .agent_shell_store_mut()
        .finish_turn("%1", &turn.turn_id)
        .unwrap();
    fixture
        .service
        .agent_turn_ledger_mut()
        .finish_turn(&turn.turn_id, AgentTurnState::Completed)
        .unwrap();
    turn
}

/// Both public fresh-conversation commands must revoke incoming persistent
/// metadata, not merely make the immediate live check fail. Later ordinary
/// removal cannot publish the old parent's completed-close evidence.
#[test]
fn runtime_persistent_close_new_and_clear_revoke_old_child_record() {
    for command in ["/new", "/clear"] {
        let mut fixture = CleanupFixture::new();
        let child = fixture.spawn("agent-%1");
        let pane = child.strip_prefix("agent-").unwrap();
        let original = fixture.service.persistent_subagent(&child).unwrap().clone();
        let native = fixture.native_identity(&child);
        let parent = parent_turn(&mut fixture);
        fixture
            .service
            .session
            .select_pane_global(&fixture.primary, pane)
            .unwrap();
        let response = fixture.service.dispatch_runtime_control_body(
            &format!(r#"{{"jsonrpc":"2.0","id":"replace-child","method":"agent/shell/command","params":{{"idempotency_key":"replace-child","input":"{command}"}}}}"#),
            &fixture.primary,
        );
        assert!(!response.contains("\"error\""), "{response}");
        assert_ne!(
            fixture
                .service
                .agent_shell_store()
                .get(pane)
                .unwrap()
                .session_id,
            original.conversation_id
        );
        assert!(fixture.service.persistent_subagent(&child).is_none());
        assert_eq!(
            close_action(&mut fixture.service, &parent, &child).status,
            ActionStatus::Rejected
        );
        fixture.assert_native_identity(&child, native);
        assert_input(&mut fixture, &child, native.0, "new-clear-survived-close");
        fixture.close(&child);
        assert!(
            fixture
                .service
                .retired_persistent_subagent(&child)
                .is_none()
        );
        assert_eq!(
            close_action(&mut fixture.service, &parent, &child).status,
            ActionStatus::Rejected
        );
    }
}

/// Reinstalling an actual child OR parent native process preserves public
/// labels and conversation IDs but not the old delegation incarnation. Neither
/// immediate close nor subsequent retirement may reuse the old record.
#[test]
fn runtime_persistent_close_native_replacement_rejects_same_conversation_ids() {
    for replace_parent in [false, true] {
        let mut fixture = CleanupFixture::new();
        let child = fixture.spawn("agent-%1");
        let pane = child.strip_prefix("agent-").unwrap();
        let parent = parent_turn(&mut fixture);
        let original = fixture
            .service
            .agent_shell_store()
            .get(pane)
            .unwrap()
            .session_id
            .clone();
        let replaced = if replace_parent { "%1" } else { pane };
        let descriptor = fixture.service.find_pane_descriptor(replaced).unwrap();
        fixture
            .service
            .terminate_runtime_pane_process(replaced, true)
            .unwrap();
        fixture
            .service
            .start_pane_process_with_start_directory(descriptor, Some("/bin/sh"), None)
            .unwrap();
        let survivor = fixture.native_identity(&child);
        assert_eq!(
            fixture
                .service
                .agent_shell_store()
                .get(pane)
                .unwrap()
                .session_id,
            original
        );
        assert!(fixture.service.persistent_subagent(&child).is_none());
        assert_eq!(
            close_action(&mut fixture.service, &parent, &child).status,
            ActionStatus::Rejected
        );
        fixture.assert_native_identity(&child, survivor);
        assert_input(
            &mut fixture,
            &child,
            survivor.0,
            "native-replacement-survived",
        );
        fixture.close(&child);
        assert!(
            fixture
                .service
                .retired_persistent_subagent(&child)
                .is_none()
        );
    }
}

/// An actual manager-to-adapter-to-manager handoff migrates the same owned
/// process proof, not a replacement. It must retain legitimate full native
/// close and repeat-close behavior without relying on pending adapter reap APIs.
#[test]
fn runtime_persistent_close_exact_adapter_handoff_preserves_legitimate_close() {
    let mut fixture = CleanupFixture::new();
    let child = fixture.spawn("agent-%1");
    let pane = child.strip_prefix("agent-").unwrap();
    let native = fixture.native_identity(&child);
    let parent = parent_turn(&mut fixture);
    let process = fixture
        .service
        .take_running_pane_process_for_adapter(pane)
        .unwrap();
    assert!(
        fixture
            .service
            .subagent_cleanup_child_is_owned_by_turn(&child, &parent)
    );
    fixture
        .service
        .restore_running_pane_process_from_adapter(pane, process)
        .unwrap();
    fixture.assert_native_identity(&child, native);
    assert_eq!(
        close_action(&mut fixture.service, &parent, &child).status,
        ActionStatus::Succeeded
    );
    assert!(!fixture.present(&child));
    assert_ne!(
        mez_mux::process::process_start_token_for_pid(native.0),
        Some(native.1)
    );
    assert_eq!(
        close_action(&mut fixture.service, &parent, &child).status,
        ActionStatus::Succeeded
    );
}

/// Live replacement while the exact child process is owned by an adapter must
/// reject without queueing a termination for that generation. The real Child
/// remains held and is reaped by the fixture adapter's Drop, never by its PID.
#[test]
fn runtime_persistent_close_rejects_independent_adapter_child_without_effects() {
    let mut fixture = CleanupFixture::new();
    let child = fixture.spawn("agent-%1");
    let parent = parent_turn(&mut fixture);
    let native = fixture.native_identity(&child);
    let adapter = fixture.handoff(&child);
    fixture.save("adapter-independent-close", Some("agent-%1"));
    fixture.resume(&child, "adapter-independent-close");
    fixture.service.drain_pane_io_transition();
    assert_eq!(
        close_action(&mut fixture.service, &parent, &child).status,
        ActionStatus::Rejected
    );
    fixture.assert_native_identity(&child, native);
    assert_eq!(
        fixture
            .service
            .adapter_owned_pane_process_instance(child.strip_prefix("agent-").unwrap()),
        Some(adapter.instance.clone())
    );
    assert!(
        fixture
            .service
            .drain_pane_io_transition()
            .side_effects
            .is_empty()
    );
}

/// Missing layout is not missing ownership. Test both active and stale retired
/// metadata against a still-bound real child, preserving identity and all maps.
#[test]
fn runtime_persistent_close_absent_layout_with_binding_is_unavailable() {
    for retired in [false, true] {
        let mut fixture = CleanupFixture::new();
        let child = fixture.spawn("agent-%1");
        let pane = child.strip_prefix("agent-").unwrap();
        let parent = parent_turn(&mut fixture);
        let original = fixture.service.persistent_subagent(&child).unwrap().clone();
        let shell = fixture
            .service
            .agent_shell_store()
            .get(pane)
            .unwrap()
            .clone();
        let native = fixture.native_identity(&child);
        let layout = fixture.service.session.clone();
        if retired {
            fixture.service.retire_persistent_subagent_binding(&child);
            fixture
                .service
                .remember_retired_persistent_subagent(&child, original.clone());
        }
        fixture
            .service
            .session
            .kill_pane_with_effects(&fixture.primary, Some(pane), true)
            .unwrap();
        assert!(!fixture.present(&child));
        let result = close_action(&mut fixture.service, &parent, &child);
        assert_eq!(result.status, ActionStatus::Rejected);
        assert_eq!(result.error.unwrap().code, "unavailable");
        assert_eq!(fixture.service.agent_shell_store().get(pane), Some(&shell));
        assert_eq!(
            fixture.service.primary_pid_for_live_pane_process(pane),
            Some(native.0)
        );
        assert_eq!(
            mez_mux::process::process_start_token_for_pid(native.0),
            Some(native.1)
        );
        if retired {
            assert_eq!(
                fixture.service.retired_persistent_subagent(&child),
                Some(&original)
            );
        } else {
            assert_eq!(fixture.service.persistent_subagent(&child), Some(&original));
        }
        fixture.service.session = layout;
        assert_input(
            &mut fixture,
            &child,
            native.0,
            "absent-layout-binding-survived",
        );
    }
}

/// A genuine absent-child record remains an idempotent cleanup control, but
/// retiring its logical parent lifetime revokes the outgoing record even when
/// the same parent conversation label is rebound before native startup.
#[test]
fn runtime_persistent_close_absent_child_cannot_outlive_logical_parent() {
    for retire_parent in [false, true] {
        let mut fixture = CleanupFixture::without_parent_process();
        let parent = parent_turn(&mut fixture);
        let record = crate::runtime::RuntimePersistentSubagent {
            conversation_id: "absent-child-fixture".to_string(),
            parent_agent_id: parent.agent_id.clone(),
            parent_conversation_id: parent.conversation_id.clone(),
            objective: "authoritative absent fixture".to_string(),
            close_authority_revoked: false,
        };
        fixture
            .service
            .set_persistent_subagent("agent-%997", record);
        if retire_parent {
            fixture.service.retire_subagent_cleanup_pane_binding("%1");
        }
        let result = close_action(&mut fixture.service, &parent, "agent-%997");
        assert_eq!(
            result.status,
            if retire_parent {
                ActionStatus::Rejected
            } else {
                ActionStatus::Succeeded
            }
        );
        if !retire_parent {
            assert!(
                result
                    .structured_content_json
                    .unwrap()
                    .contains("\"closed\":false")
            );
            assert_eq!(
                close_action(&mut fixture.service, &parent, "agent-%997").status,
                ActionStatus::Succeeded
            );
        }
    }
}

/// Resume rollback restores the exact bounded retired records, including
/// outgoing evidence from this parent; successful parent replacement revokes
/// them so independent return to the same CID cannot recover close authority.
#[test]
fn runtime_persistent_close_retired_evidence_rollback_and_parent_replacement() {
    let mut fixture = CleanupFixture::new();
    let child = fixture.spawn("agent-%1");
    let parent = idle_parent_turn(&mut fixture);
    assert_eq!(
        close_action(&mut fixture.service, &parent, &child).status,
        ActionStatus::Succeeded
    );
    let retired = fixture
        .service
        .retired_persistent_subagent(&child)
        .unwrap()
        .clone();
    fixture.save("parent-close-replacement", None);
    fixture
        .service
        .fail_next_agent_resume_after_authority_restore_for_tests();
    assert!(
        fixture
            .service
            .execute_agent_shell_resume_command("%1", "/resume parent-close-replacement")
            .is_err()
    );
    assert_eq!(
        fixture.service.retired_persistent_subagent(&child),
        Some(&retired)
    );
    assert_eq!(
        close_action(&mut fixture.service, &parent, &child).status,
        ActionStatus::Succeeded
    );
    fixture.resume("agent-%1", "parent-close-replacement");
    assert!(
        fixture
            .service
            .retired_persistent_subagent(&child)
            .is_none()
    );
    fixture.save(&parent.conversation_id, None);
    fixture.resume("agent-%1", &parent.conversation_id);
    assert_eq!(
        close_action(&mut fixture.service, &parent, &child).status,
        ActionStatus::Rejected
    );
}

/// A fully qualified snapshot restore can establish the new runtime's actual
/// native pair; pending metadata alone cannot close a child before startup.
/// Replacing the first pinned parent before child startup cancels qualification.
#[test]
fn runtime_persistent_close_qualified_restore_and_first_owner_replacement() {
    for replace_first_parent in [false, true] {
        let mut fixture = CleanupFixture::new();
        let child = fixture.spawn("agent-%1");
        let parent = idle_parent_turn(&mut fixture);
        fixture.service.checkpoint_agent_session_metadata().unwrap();
        fixture.service.terminate_all_pane_processes().unwrap();
        for pane in ["%1", child.strip_prefix("agent-").unwrap()] {
            fixture
                .service
                .session
                .set_pane_live_state(pane, false)
                .unwrap();
        }
        assert_eq!(
            fixture
                .service
                .restore_agent_sessions_from_transcript_store()
                .unwrap(),
            2
        );
        assert_eq!(
            close_action(&mut fixture.service, &parent, &child).status,
            ActionStatus::Rejected
        );
        fixture
            .service
            .start_initial_pane_process(Some("cat"))
            .unwrap();
        if replace_first_parent {
            fixture
                .service
                .terminate_runtime_pane_process("%1", true)
                .unwrap();
            fixture
                .service
                .start_initial_pane_process(Some("cat"))
                .unwrap();
        }
        fixture
            .service
            .session
            .set_pane_live_state("%1", true)
            .unwrap();
        fixture
            .service
            .restart_restored_pane_processes(Some("cat"))
            .unwrap();
        let native = fixture.native_identity(&child);
        let result = close_action(&mut fixture.service, &parent, &child);
        if replace_first_parent {
            assert_eq!(result.status, ActionStatus::Rejected);
            fixture.assert_native_identity(&child, native);
        } else {
            assert_eq!(result.status, ActionStatus::Succeeded);
            assert!(!fixture.present(&child));
            assert_ne!(
                mez_mux::process::process_start_token_for_pid(native.0),
                Some(native.1)
            );
        }
    }
}
