//! Outgoing persistent metadata during transactional parent replacement.
//!
//! Fenced descendant cleanup is a distinct original obligation. Revoking the
//! old parent's explicit-close metadata must preserve that obligation but must
//! not allow later child removal to manufacture a new old-parent tombstone.

use super::*;

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
                "original fenced cleanup remains mandatory"
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
    assert!(!checkpoint.iter().any(|record| record.pane_id == pane));
    assert!(!fixture.store.inspect(&original_child).unwrap().is_empty());
    fixture
        .service
        .restore_agent_sessions_from_transcript_store()
        .unwrap();
    assert!(fixture.service.persistent_subagent(&child).is_none());
    assert_eq!(
        close_action(&mut fixture.service, &parent, &child).status,
        ActionStatus::Rejected
    );
}
