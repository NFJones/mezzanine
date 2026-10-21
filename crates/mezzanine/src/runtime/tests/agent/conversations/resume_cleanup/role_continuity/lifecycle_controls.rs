//! Native registration continuity does not revive historical parent authority.
//!
//! Actual same-conversation, independent-subagent, return, failed-resume and
//! cancelled-history paths retain their own live-owner/generation checks. Tests
//! never insert queued turns, alter registration roles or fabricate cleanup
//! grants; the real prompt owner must admit the final current conversation.

use super::action_flows::{
    input_receipt, parent_receiver, prepare_without_io, project, saved_root, spawn_role,
};
use super::*;

/// Creates only archive material for the actual existing C1 contract. Rewriting
/// that immutable contract would fabricate a different historical relationship.
fn retain_original_c1(fixture: &CleanupFixture, child: &str, conversation: &str) {
    fixture
        .store
        .append(&TranscriptEntry {
            conversation_id: conversation.into(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::User,
            turn_id: "c433-owned-c1-history".into(),
            agent_id: child.into(),
            pane_id: child.strip_prefix("agent-").unwrap().into(),
            content: "C433_ORIGINAL_C1_HISTORY".into(),
        })
        .unwrap();
}

/// Same-CID resume retains a genuine live delegation; independent subagent C2
/// and ROOT-C2->C1 return retain history but cannot reconstitute that authority.
/// All three final bindings must still admit real work with unchanged native
/// role/capabilities, native incarnation and a fresh shell-written input receipt.
#[tokio::test(flavor = "current_thread")]
async fn runtime_native_role_same_cid_subagent_and_return_keep_real_admission() {
    for mode in ["same-cid", "saved-subagent", "independent-return"] {
        let mut fixture = Box::new(CleanupFixture::new());
        let scope = project(&mut fixture, "project-a");
        parent_receiver(&mut fixture);
        let child = spawn_role(&mut fixture, "worker");
        let pane = child.strip_prefix("agent-").unwrap().to_owned();
        let id = AgentId::opaque(child.clone()).unwrap();
        let native = fixture.native_identity(&child);
        let original = fixture
            .service
            .message_service()
            .registered_identity(&id)
            .unwrap()
            .clone();
        let c1 = fixture
            .service
            .agent_shell_store()
            .get(&pane)
            .unwrap()
            .session_id
            .clone();
        retain_original_c1(&fixture, &child, &c1);
        let final_conversation = match mode {
            "same-cid" => {
                fixture.resume(&child, &c1);
                assert!(
                    fixture
                        .service
                        .subagent_lineage_has_live_parent_authority(&child)
                );
                assert!(fixture.service.persistent_subagent(&child).is_some());
                c1.clone()
            }
            "saved-subagent" => {
                fixture.save("c433-structural-child-c2", Some("agent-%1"));
                fixture.resume(&child, "c433-structural-child-c2");
                assert!(
                    !fixture
                        .service
                        .subagent_lineage_has_live_parent_authority(&child)
                );
                assert!(fixture.service.persistent_subagent(&child).is_none());
                "c433-structural-child-c2".into()
            }
            _ => {
                saved_root(
                    &mut fixture,
                    "c433-return-root-c2",
                    &scope,
                    "C433_RETURN_ROOT_HISTORY",
                );
                fixture.resume(&child, "c433-return-root-c2");
                assert!(!fixture.service.has_subagent_authority_state(&child));
                fixture.resume(&child, &c1);
                assert!(
                    !fixture
                        .service
                        .subagent_lineage_has_live_parent_authority(&child)
                );
                assert!(fixture.service.persistent_subagent(&child).is_none());
                c1.clone()
            }
        };
        fixture.assert_native_identity(&child, native);
        let started = fixture
            .service
            .start_agent_prompt_turn(&pane, "generated real current binding task")
            .unwrap();
        let turn = fixture
            .service
            .agent_turn_ledger()
            .turn(&started.turn_id)
            .unwrap();
        assert_eq!(turn.agent_id, child);
        assert_eq!(turn.conversation_id, final_conversation);
        let identity = fixture
            .service
            .message_service()
            .registered_identity(&id)
            .unwrap();
        assert_eq!(identity.role, original.role);
        assert_eq!(identity.capabilities, original.capabilities);
        prepare_without_io(&mut fixture, &started.turn_id).await;
        input_receipt(&mut fixture, &child, native.0, mode);
        fixture.service.stop_agent_turn_for_pane(&pane).unwrap();
    }
}

/// A real late resume failure restores the same live child authority, canonical
/// project, registration and receiver/receipt projections. This case creates a
/// subscription but no populated mail; the project-resume case supplies that
/// separate positive witness. The
/// following actual C1 prompt proves rollback does not merely restore labels.
#[tokio::test(flavor = "current_thread")]
async fn runtime_native_role_failed_resume_restores_current_identity_and_admission() {
    let mut fixture = Box::new(CleanupFixture::new());
    project(&mut fixture, "project-a");
    parent_receiver(&mut fixture);
    let child = spawn_role(&mut fixture, "worker");
    let pane = child.strip_prefix("agent-").unwrap().to_owned();
    let id = AgentId::opaque(child.clone()).unwrap();
    let native = fixture.native_identity(&child);
    let c1 = fixture
        .service
        .agent_shell_store()
        .get(&pane)
        .unwrap()
        .session_id
        .clone();
    fixture
        .service
        .message_service_mut()
        .subscribe_from_retained_start(&id)
        .unwrap();
    let before = fixture.service.message_service().snapshot_state();
    let identity = fixture
        .service
        .message_service()
        .registered_identity(&id)
        .unwrap()
        .clone();
    let b = fixture.root.join("project-b");
    fs::create_dir(&b).unwrap();
    let b = b.canonicalize().unwrap();
    saved_root(
        &mut fixture,
        "c433-rollback-root-c2",
        &b,
        "C433_ROLLBACK_TARGET_HISTORY",
    );
    assert!(
        fixture
            .service
            .subagent_lineage_has_live_parent_authority(&child)
    );
    fixture
        .service
        .fail_next_agent_resume_after_authority_restore_for_tests();
    let error = fixture
        .service
        .execute_agent_shell_resume_command(&pane, "/resume c433-rollback-root-c2")
        .unwrap_err();
    assert!(
        error
            .message()
            .contains("injected resume post-authority restoration failure")
    );
    assert_eq!(
        fixture
            .service
            .agent_shell_store()
            .get(&pane)
            .unwrap()
            .session_id,
        c1
    );
    assert!(
        fixture
            .service
            .subagent_lineage_has_live_parent_authority(&child)
    );
    assert!(fixture.service.persistent_subagent(&child).is_some());
    assert_eq!(
        fixture.service.message_service().registered_identity(&id),
        Some(&identity)
    );
    let after = fixture.service.message_service().snapshot_state();
    assert_eq!(after.subscriptions, before.subscriptions);
    assert_eq!(after.retained_messages, before.retained_messages);
    assert_eq!(after.accepted_messages, before.accepted_messages);
    fixture.assert_native_identity(&child, native);
    let started = fixture
        .service
        .start_agent_prompt_turn(&pane, "generated post rollback real C1 task")
        .unwrap();
    assert_eq!(
        fixture
            .service
            .agent_turn_ledger()
            .turn(&started.turn_id)
            .unwrap()
            .conversation_id,
        c1
    );
    prepare_without_io(&mut fixture, &started.turn_id).await;
    input_receipt(&mut fixture, &child, native.0, "rollback-preserved");
    fixture.service.stop_agent_turn_for_pane(&pane).unwrap();
}

/// Real queued/claimed prompt-history work is explicitly cancelled by its
/// command owner before supported ROOT replacement. Its late claim/result must
/// not use unchanged native ID/role to admit an old-context turn into C2, charge
/// another provider task, replace project membership or resurrect parent state.
#[tokio::test(flavor = "current_thread")]
async fn runtime_native_role_stale_history_cannot_adopt_rebound_root() {
    for claimed in [false, true] {
        let mut fixture = Box::new(CleanupFixture::new());
        project(&mut fixture, "project-a");
        parent_receiver(&mut fixture);
        let child = spawn_role(&mut fixture, "worker");
        let pane = child.strip_prefix("agent-").unwrap().to_owned();
        let native = fixture.native_identity(&child);
        let b = fixture.root.join("project-b");
        fs::create_dir(&b).unwrap();
        let b = b.canonicalize().unwrap();
        saved_root(
            &mut fixture,
            "c433-stale-root-c2",
            &b,
            "C433_FRESH_ROOT_HISTORY",
        );
        let primary = fixture.primary.clone();
        fixture
            .service
            .begin_agent_prompt_history_preparation(primary, &pane, "C433_CANCELLED_C1_PROMPT")
            .unwrap();
        let dispatch = fixture
            .service
            .take_pending_agent_prompt_history()
            .pop()
            .unwrap();
        let outcome = if claimed {
            assert!(
                fixture
                    .service
                    .claim_agent_prompt_history_preparation(&dispatch)
            );
            Some(
                crate::runtime::control::execute_runtime_agent_prompt_history_work(
                    dispatch.history_work.clone(),
                ),
            )
        } else {
            None
        };
        // Invoke the real cancellation owner; do not overwrite a claim/ledger.
        fixture.service.agent.cancel_agent_command(&pane);
        fixture.resume(&child, "c433-stale-root-c2");
        let after_resume = fixture.service.message_service().snapshot_state();
        if let Some(outcome) = outcome {
            assert!(
                !fixture
                    .service
                    .complete_agent_prompt_history_preparation(&dispatch, outcome)
                    .unwrap()
            );
        } else {
            assert!(
                !fixture
                    .service
                    .claim_agent_prompt_history_preparation(&dispatch)
            );
        }
        assert!(!fixture.service.agent_command_is_active(&pane));
        assert!(fixture.service.agent_turn_ledger().turns().is_empty());
        assert!(fixture.service.pending_agent_provider_tasks().is_empty());
        assert_eq!(
            fixture.service.message_service().snapshot_state(),
            after_resume
        );
        assert!(!fixture.service.has_subagent_authority_state(&child));
        fixture.assert_native_identity(&child, native);
        let started = fixture
            .service
            .start_agent_prompt_turn(&pane, "generated fresh C2 prompt")
            .unwrap();
        let context = fixture
            .service
            .agent_turn_contexts()
            .get(&started.turn_id)
            .unwrap();
        assert!(
            context
                .blocks()
                .iter()
                .any(|block| block.content.contains("C433_FRESH_ROOT_HISTORY"))
        );
        assert!(
            !context
                .blocks()
                .iter()
                .any(|block| block.content.contains("C433_CANCELLED_C1_PROMPT"))
        );
        prepare_without_io(&mut fixture, &started.turn_id).await;
        fixture.service.stop_agent_turn_for_pane(&pane).unwrap();
    }
}
