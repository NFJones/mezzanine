//! Native MMP registration continuity across independent saved-root resume.
//!
//! Actual delegation, native process ownership, saved binding and real prompt
//! admission supply the regression evidence. Role/capability labels remain
//! immutable routing metadata, never proof of current parent/cleanup authority.
//! Preparation executes only after the real work is proven to contain no auth,
//! MCP or accounting I/O; no model/provider transport or action is dispatched.

use super::*;
use mez_core::ids::AgentId;

/// A genuine persistent worker C1 can independently resume saved ROOT C2 in its
/// existing pane and start real work without changing the native registration.
/// The intended base oracle is a role conflict at real prompt admission after
/// resume/native preconditions pass; execution must establish that first failure.
/// Candidate acceptance also exercises the full capability
/// vector through objective fanout, plus ordinary real-turn sender/preparation
/// owners; neither forged sender fields nor synthetic ledger work can qualify it.
#[tokio::test(flavor = "current_thread")]
async fn runtime_saved_root_resume_keeps_native_role_for_real_prompt() {
    let mut fixture = Box::new(CleanupFixture::new());
    let child = fixture.spawn("agent-%1");
    let pane = child.strip_prefix("agent-").unwrap().to_owned();
    let child_id = AgentId::opaque(child.clone()).unwrap();
    let parent_id = AgentId::opaque("agent-%1").unwrap();
    let original_native = fixture.native_identity(&child);
    let original_descriptor = fixture.service.find_pane_descriptor(&pane).unwrap();
    let original_identity = fixture
        .service
        .message_service()
        .registered_identity(&child_id)
        .unwrap()
        .clone();
    // skip_initial_turn does not promise a parent MMP registration/subscription.
    // Preserve optional observations without manufacturing an unrelated setup;
    // None equality is NOT a positive receipt/cursor preservation control.
    let parent_identity = fixture
        .service
        .message_service()
        .registered_identity(&parent_id)
        .cloned();
    let parent_cursor = fixture
        .service
        .message_service()
        .subscription(&parent_id)
        .cloned();
    assert_eq!(original_identity.role.as_deref(), Some("worker"));
    assert_eq!(original_identity.capabilities.len(), 3);
    assert!(
        original_identity
            .capabilities
            .iter()
            .any(|cap| cap == "agent-harness")
    );
    assert!(
        original_identity
            .capabilities
            .iter()
            .any(|cap| cap == "subagent")
    );
    let original_conversation = fixture
        .service
        .agent_shell_store()
        .get(&pane)
        .unwrap()
        .session_id
        .clone();
    assert!(fixture.service.subagent_lineage(&child).is_some());

    let target = "c433-generated-root-c2";
    fixture.save(target, None);
    fixture.resume(&child, target);
    let session = fixture.service.agent_shell_store().get(&pane).unwrap();
    assert_ne!(session.session_id, original_conversation);
    assert_eq!(session.session_id, target);
    assert_eq!(
        session.conversation_kind,
        mez_agent::AgentConversationKind::Root
    );
    assert!(!fixture.service.has_subagent_authority_state(&child));
    assert!(fixture.service.subagent_lineage(&child).is_none());
    assert!(fixture.service.persistent_subagent(&child).is_none());
    fixture.assert_native_identity(&child, original_native);
    let rebound_descriptor = fixture.service.find_pane_descriptor(&pane).unwrap();
    assert_eq!(rebound_descriptor.pane_id, original_descriptor.pane_id);
    assert_eq!(rebound_descriptor.window_id, original_descriptor.window_id);
    assert_eq!(rebound_descriptor.size, original_descriptor.size);
    assert_eq!(
        fixture
            .service
            .message_service()
            .subscription(&parent_id)
            .cloned(),
        parent_cursor
    );

    // This must be the real admission owner. An inserted queued ledger record
    // would skip the lineage-gated refresh that currently rejects ROOT C2.
    let started = fixture
        .service
        .start_agent_prompt_turn(&pane, "generated c433 root task")
        .expect("supported independent ROOT resume must admit a genuine prompt");
    let turn = fixture
        .service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .unwrap()
        .clone();
    assert_eq!(turn.agent_id, child);
    assert_eq!(turn.pane_id, pane);
    assert_eq!(turn.conversation_id, target);
    assert_eq!(turn.trigger, mez_agent::AgentTurnTrigger::UserPrompt);
    assert!(turn.parent_turn_id.is_none());
    assert_eq!(
        fixture.service.runtime_agent_kind(&child).as_str(),
        "primary"
    );

    // A role-only fix still fails this ordinary generic-capability refresh.
    fixture
        .service
        .sync_runtime_agent_objectives_for_conversation(target, Some("generated c433 objective"))
        .expect("objective fanout must preserve established native role and exact capabilities");
    fixture
        .service
        .sync_runtime_agent_objectives_for_conversation(target, None)
        .unwrap();
    let sender = fixture
        .service
        .runtime_message_sender_identity(&turn)
        .unwrap();
    assert_eq!(sender.agent_id, original_identity.agent_id);
    assert_eq!(sender.pane_id, original_identity.pane_id);
    assert_eq!(sender.window_id, original_identity.window_id);
    assert_eq!(sender.role, original_identity.role);
    assert_eq!(sender.capabilities, original_identity.capabilities);
    assert_eq!(
        fixture
            .service
            .message_service()
            .registered_identity(&parent_id)
            .cloned(),
        parent_identity
    );
    assert_eq!(
        fixture
            .service
            .message_service()
            .subscription(&parent_id)
            .cloned(),
        parent_cursor
    );

    // Inspect actual extracted work, never overwrite populated work to make an
    // unsafe provider/MCP path look inert. Empty branches execute no external I/O.
    let preparation = fixture
        .service
        .prepare_agent_provider_work(&started.turn_id)
        .unwrap();
    assert!(
        preparation.auth_store.is_none(),
        "fixture must not resolve real credentials"
    );
    assert!(
        preparation.mcp_plans.is_empty(),
        "fixture must not open MCP transports"
    );
    assert_eq!(preparation.attempted_mcp_servers, 0);
    assert!(
        preparation.accounting_projects.is_none(),
        "fixture must not open accounting stores"
    );
    let prepared = RuntimeSessionService::execute_agent_provider_preparation(preparation).await;
    fixture
        .service
        .apply_agent_provider_preparation(prepared)
        .unwrap();
    fixture.assert_native_identity(&child, original_native);
    assert!(!fixture.service.has_subagent_authority_state(&child));
    fixture.service.stop_agent_turn_for_pane(&pane).unwrap();
}
