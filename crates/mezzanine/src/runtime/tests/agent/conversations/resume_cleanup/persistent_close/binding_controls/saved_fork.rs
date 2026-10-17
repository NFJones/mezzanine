//! Saved-fork composition with version-seven persistent-close qualification.
//!
//! The source is a real fixture-owned persistent child with true qualification.
//! The validated deferred fork path must create a fresh Root, not copy source
//! ownership, lineage, scope or native cleanup authority from saved history.

use super::*;

/// A genuine persistent source checkpoints as owned, but its saved fork is a
/// fresh Root with false qualification and no parent lifecycle authority. The
/// original child stays owned and native-live; old-parent close of the new root
/// rejects opaquely without removing or signaling that separate native owner.
#[test]
fn runtime_persistent_close_saved_fork_root_never_inherits_source_qualification() {
    let mut fixture = CleanupFixture::new();
    let source = fixture.spawn("agent-%1");
    let source_pane = source.strip_prefix("agent-").unwrap();
    let source_owner = fixture
        .service
        .persistent_subagent(&source)
        .unwrap()
        .clone();
    let source_native = fixture.native_identity(&source);
    fixture.save(&source_owner.conversation_id, None);
    fixture
        .service
        .agent_shell_store_mut()
        .record_transcript_entries(source_pane, 1)
        .unwrap();
    let before = fixture
        .store
        .load_agent_session_metadata(fixture.service.session().id.as_str())
        .unwrap();
    assert!(
        before
            .iter()
            .find(|record| record.pane_id == source_pane)
            .unwrap()
            .persistent_subagent_owned
    );
    fixture
        .service
        .session
        .select_pane_global(&fixture.primary, source_pane)
        .unwrap();
    fixture
        .service
        .execute_agent_shell_command(&fixture.primary, "/fork close-coupled-root")
        .unwrap();
    let reply = fixture
        .service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .unwrap();
    assert!(reply.contains("forked=true"), "{reply}");
    let target = fixture
        .service
        .agent_shell_store()
        .sessions()
        .find(|session| session.session_id == "close-coupled-root")
        .unwrap()
        .clone();
    let target_agent = format!("agent-{}", target.pane_id);
    assert_eq!(
        target.conversation_kind,
        mez_agent::AgentConversationKind::Root
    );
    assert!(fixture.service.subagent_lineage(&target_agent).is_none());
    assert!(fixture.service.persistent_subagent(&target_agent).is_none());
    assert!(
        !fixture
            .service
            .has_subagent_scope_declaration(&target_agent)
    );
    assert_eq!(
        fixture.service.persistent_subagent(&source),
        Some(&source_owner)
    );
    fixture.assert_native_identity(&source, source_native);
    // Inspect ordinary current capture, not a history-derived target header.
    fixture.service.checkpoint_agent_session_metadata().unwrap();
    let records = fixture
        .store
        .load_agent_session_metadata(fixture.service.session().id.as_str())
        .unwrap();
    assert!(
        !records
            .iter()
            .find(|record| record.pane_id == target.pane_id)
            .unwrap()
            .persistent_subagent_owned
    );
    assert!(
        records
            .iter()
            .find(|record| record.pane_id == source_pane)
            .unwrap()
            .persistent_subagent_owned
    );
    let target_native = fixture.native_identity(&target_agent);
    let parent = parent_turn(&mut fixture);
    let unavailable = denial(&close_action(
        &mut fixture.service,
        &parent,
        "agent-absent-fork-fixture",
    ));
    assert_eq!(
        denial(&close_action(&mut fixture.service, &parent, &target_agent)),
        unavailable
    );
    fixture.assert_native_identity(&target_agent, target_native);
    fixture.assert_native_identity(&source, source_native);
    assert_eq!(
        fixture.service.persistent_subagent(&source),
        Some(&source_owner)
    );
}
