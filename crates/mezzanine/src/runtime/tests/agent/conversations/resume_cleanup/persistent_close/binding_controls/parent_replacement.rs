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
        assert!(fixture.service.persistent_subagent(&child).is_none());
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
