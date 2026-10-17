//! Outgoing persistent metadata during transactional parent replacement.
//!
//! Fenced descendant cleanup is a distinct original obligation. Revoking the
//! old parent's explicit-close metadata must preserve that obligation but must
//! not allow later child removal to manufacture a new old-parent tombstone.

use super::*;

mod fresh_spawn;

/// Restarts only this fixture's session/store with new runtime/native owners.
/// Original manager children are terminated and reaped before replacement; the
/// enclosing fixture owns every new child and cleans it on assertion failure.
fn restore_fresh_runtime(fixture: &mut CleanupFixture) {
    let mut session = fixture.service.session().clone();
    fixture.service.terminate_all_pane_processes().unwrap();
    let panes = session
        .windows()
        .iter()
        .flat_map(|window| window.panes())
        .map(|pane| pane.id.to_string())
        .collect::<Vec<_>>();
    for pane in panes {
        session.set_pane_live_state(&pane, false).unwrap();
    }
    let mut restarted = RuntimeServiceFixture::new()
        .control_socket(fixture.root.join("restored.sock"))
        .build_with_session(session);
    restarted.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    restarted.set_agent_transcript_store(fixture.store.clone());
    // Transfer the runtime before any new native startup, so fixture Drop also
    // terminates/reaps new children if a later startup/assertion fails.
    fixture.service = restarted;
    fixture
        .service
        .restore_agent_sessions_from_transcript_store()
        .unwrap();
    fixture
        .service
        .start_initial_pane_process(Some("cat"))
        .unwrap();
    fixture
        .service
        .session
        .set_pane_live_state("%1", true)
        .unwrap();
    fixture
        .service
        .restart_restored_pane_processes(Some("cat"))
        .unwrap();
}

/// Real idle children and authoritative already-absent children cannot retain
/// explicit-close authority across P1 -> P2 -> P1. A failed replacement restores
/// the exact outgoing record and cleanup proof; successful replacement revokes
/// it before a later ordinary child removal can publish retired evidence.
#[test]
fn runtime_persistent_close_parent_return_cannot_recreate_retired_evidence() {
    for absent_child in [false, true] {
        let mut fixture = CleanupFixture::new();
        let child = if absent_child {
            "agent-%997".to_string()
        } else {
            fixture.spawn("agent-%1")
        };
        let parent = idle_parent_turn(&mut fixture);
        if absent_child {
            fixture.service.set_persistent_subagent(
                &child,
                crate::runtime::RuntimePersistentSubagent {
                    conversation_id: "absent-parent-return-fixture".to_string(),
                    parent_agent_id: parent.agent_id.clone(),
                    parent_conversation_id: parent.conversation_id.clone(),
                    objective: "original absent-child obligation".to_string(),
                    close_authority_revoked: false,
                },
            );
        }
        let original = fixture.service.persistent_subagent(&child).unwrap().clone();
        let native = (!absent_child).then(|| fixture.native_identity(&child));
        let generation = fixture
            .service
            .local_cleanup_parent_state_for_tests("agent-%1");
        fixture.save(&parent.conversation_id, None);
        fixture.save("parent-second-binding", None);
        fixture
            .service
            .fail_next_agent_resume_after_authority_restore_for_tests();
        let failure = fixture
            .service
            .execute_agent_shell_resume_command("%1", "/resume parent-second-binding")
            .unwrap_err();
        assert!(failure.message().contains("post-authority restoration"));
        assert_eq!(fixture.service.persistent_subagent(&child), Some(&original));
        assert_eq!(
            fixture
                .service
                .local_cleanup_parent_state_for_tests("agent-%1"),
            generation
        );
        if let Some(native) = native {
            fixture.assert_native_identity(&child, native);
            assert!(
                fixture
                    .service
                    .subagent_cleanup_child_is_owned_by_turn(&child, &parent)
            );
        }
        fixture.resume("agent-%1", "parent-second-binding");
        let retained = fixture.service.persistent_subagent(&child).unwrap();
        assert_eq!(
            retained.parent_conversation_id,
            original.parent_conversation_id
        );
        assert_eq!(retained.conversation_id, original.conversation_id);
        assert!(retained.close_authority_revoked);
        fixture.resume("agent-%1", &parent.conversation_id);
        assert_eq!(
            close_action(&mut fixture.service, &parent, &child).status,
            ActionStatus::Rejected
        );
        if let Some(native) = native {
            fixture.assert_native_identity(&child, native);
            assert_input(
                &mut fixture,
                &child,
                native.0,
                "idle-child-survived-parent-return",
            );
            fixture.close(&child);
        }
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

/// Revocation is negative-only and survives the original Local capability's
/// first native installation plus exact adapter handoff. Existing original
/// metadata and genuine descendant cleanup remain usable, never explicit close.
#[test]
fn runtime_persistent_close_revocation_survives_local_native_handoff() {
    let mut fixture = CleanupFixture::without_parent_process();
    let child = fixture.spawn("agent-%1");
    let native = fixture.native_identity(&child);
    let parent = idle_parent_turn(&mut fixture);
    fixture.save("local-revocation-parent", None);
    fixture.resume("agent-%1", "local-revocation-parent");
    fixture
        .service
        .start_initial_pane_process(Some("cat"))
        .unwrap();
    let _adapter = fixture.handoff("agent-%1");
    let retained = fixture.service.persistent_subagent(&child).unwrap();
    assert_eq!(retained.parent_conversation_id, parent.conversation_id);
    assert!(retained.close_authority_revoked);
    assert_eq!(
        close_action(&mut fixture.service, &parent, &child).status,
        ActionStatus::Rejected
    );
    fixture.assert_native_identity(&child, native);
    assert_eq!(
        fixture
            .service
            .close_subagent_descendants_for_parent_conversation(
                "agent-%1",
                &parent.conversation_id,
                "stale original callback remains inert"
            )
            .unwrap(),
        0
    );
    assert_eq!(
        fixture
            .service
            .close_subagent_descendants_for_parent_agent(
                "agent-%1",
                "current owner retains original fenced obligation"
            )
            .unwrap(),
        1
    );
    assert!(!fixture.present(&child));
    assert_ne!(
        mez_mux::process::process_start_token_for_pid(native.0),
        Some(native.1)
    );
    assert!(
        fixture
            .service
            .retired_persistent_subagent(&child)
            .is_none()
    );
}

/// Revoked runtime records cannot be encoded as an active restore-owned pair.
/// The durable child history remains explicitly resumable, while a matching
/// parent UUID after checkpoint restore cannot manufacture fresh close proof.
#[test]
fn runtime_persistent_close_revoked_pair_cannot_rehydrate_from_checkpoint() {
    let mut fixture = CleanupFixture::new();
    let child = fixture.spawn("agent-%1");
    let pane = child.strip_prefix("agent-").unwrap();
    let original_child = fixture
        .service
        .persistent_subagent(&child)
        .unwrap()
        .conversation_id
        .clone();
    fixture.save(&original_child, None);
    let parent = idle_parent_turn(&mut fixture);
    fixture.save(&parent.conversation_id, None);
    fixture.save("checkpoint-revocation-parent", None);
    fixture.resume("agent-%1", "checkpoint-revocation-parent");
    fixture.resume("agent-%1", &parent.conversation_id);
    assert!(
        fixture
            .service
            .persistent_subagent(&child)
            .unwrap()
            .close_authority_revoked
    );
    fixture.service.checkpoint_agent_session_metadata().unwrap();
    let checkpoint = fixture
        .store
        .load_agent_session_metadata(fixture.service.session().id.as_str())
        .unwrap();
    let child_checkpoint = checkpoint
        .iter()
        .find(|record| record.pane_id == pane)
        .unwrap();
    assert!(!child_checkpoint.persistent_subagent_owned);
    assert!(!fixture.store.inspect(&original_child).unwrap().is_empty());
    restore_fresh_runtime(&mut fixture);
    assert!(fixture.service.persistent_subagent(&child).is_none());
    let native = fixture.native_identity(&child);
    assert_eq!(
        close_action(&mut fixture.service, &parent, &child).status,
        ActionStatus::Rejected
    );
    fixture.assert_native_identity(&child, native);
}

/// Record absence after independent same-ID return or actual native replacement
/// cannot be confused with a fresh persistent grant when checkpointing. Fresh
/// runtime restoration retains historical bindings but no close/scopes proof.
#[test]
fn runtime_persistent_close_orphan_checkpoint_cannot_recreate_ownership() {
    for mode in ["independent-return", "native-child", "native-parent"] {
        let mut fixture = CleanupFixture::new();
        let child = fixture.spawn("agent-%1");
        let pane = child.strip_prefix("agent-").unwrap();
        let original = fixture
            .service
            .persistent_subagent(&child)
            .unwrap()
            .conversation_id
            .clone();
        fixture.save(&original, None);
        let parent = idle_parent_turn(&mut fixture);
        if mode == "independent-return" {
            fixture.save("orphan-intermediate", Some("agent-%1"));
            fixture.resume(&child, "orphan-intermediate");
            fixture.resume(&child, &original);
        } else {
            let replaced = if mode == "native-parent" { "%1" } else { pane };
            let descriptor = fixture.service.find_pane_descriptor(replaced).unwrap();
            fixture
                .service
                .terminate_runtime_pane_process(replaced, true)
                .unwrap();
            fixture
                .service
                .start_pane_process_with_start_directory(descriptor, Some("cat"), None)
                .unwrap();
        }
        assert!(fixture.service.persistent_subagent(&child).is_none());
        fixture.service.checkpoint_agent_session_metadata().unwrap();
        let records = fixture
            .store
            .load_agent_session_metadata(fixture.service.session().id.as_str())
            .unwrap();
        let record = records
            .iter()
            .find(|record| record.pane_id == pane)
            .unwrap();
        assert_eq!(record.conversation_id, original);
        assert!(!record.persistent_subagent_owned);
        restore_fresh_runtime(&mut fixture);
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
        assert!(
            !fixture
                .service
                .subagent_lineage_has_live_parent_authority(&child)
        );
        assert!(!fixture.service.has_subagent_scope_declaration(&child));
        let native = fixture.native_identity(&child);
        assert_eq!(
            close_action(&mut fixture.service, &parent, &child).status,
            ActionStatus::Rejected
        );
        fixture.assert_native_identity(&child, native);
    }
}

/// A genuine sealed first-loop projection checkpoints the durable child CID
/// with qualification, rather than the temporary UUID or a label-derived grant.
#[test]
fn runtime_persistent_close_owned_loop_checkpoints_exact_durable_qualification() {
    let mut fixture = CleanupFixture::new();
    let child = fixture.spawn("agent-%1");
    let pane = child.strip_prefix("agent-").unwrap();
    let original = fixture
        .service
        .persistent_subagent(&child)
        .unwrap()
        .conversation_id
        .clone();
    fixture
        .service
        .execute_agent_shell_loop_command(pane, "/loop --fork --limit 1 checkpoint owned loop")
        .unwrap();
    assert_ne!(
        fixture
            .service
            .agent_shell_store()
            .get(pane)
            .unwrap()
            .session_id,
        original
    );
    fixture.service.checkpoint_agent_session_metadata().unwrap();
    let records = fixture
        .store
        .load_agent_session_metadata(fixture.service.session().id.as_str())
        .unwrap();
    let record = records
        .iter()
        .find(|record| record.pane_id == pane)
        .unwrap();
    assert_eq!(record.conversation_id, original);
    assert!(record.persistent_subagent_owned);
}

/// A true checkpoint marker cannot qualify a historical child against a
/// different parent conversation installed by the same fresh restore. Native
/// startup does not substitute for the missing original-parent pair.
#[test]
fn runtime_persistent_close_true_checkpoint_requires_original_parent_pair() {
    let mut fixture = CleanupFixture::new();
    let child = fixture.spawn("agent-%1");
    let pane = child.strip_prefix("agent-").unwrap();
    fixture.save("wrong-qualified-parent", None);
    fixture.service.checkpoint_agent_session_metadata().unwrap();
    let session_id = fixture.service.session().id.as_str().to_string();
    let mut records = fixture
        .store
        .load_agent_session_metadata(&session_id)
        .unwrap();
    assert!(
        records
            .iter()
            .find(|record| record.pane_id == pane)
            .unwrap()
            .persistent_subagent_owned
    );
    let parent = records
        .iter_mut()
        .find(|record| record.pane_id == "%1")
        .unwrap();
    parent.conversation_id = "wrong-qualified-parent".to_string();
    parent.transcript_entries = 1;
    parent.primary_display_name = None;
    fixture
        .store
        .save_agent_session_metadata_checkpoint(&session_id, &records)
        .unwrap();
    restore_fresh_runtime(&mut fixture);
    assert!(fixture.service.persistent_subagent(&child).is_none());
    let current_parent = parent_turn(&mut fixture);
    assert!(
        !fixture
            .service
            .subagent_cleanup_child_is_owned_by_turn(&child, &current_parent)
    );
    let native = fixture.native_identity(&child);
    assert_eq!(
        close_action(&mut fixture.service, &current_parent, &child).status,
        ActionStatus::Rejected
    );
    fixture.assert_native_identity(&child, native);
}
