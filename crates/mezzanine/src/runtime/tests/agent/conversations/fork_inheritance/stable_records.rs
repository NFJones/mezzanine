//! Stable accounting across physical inherited-tail publication and cold replay.
//!
//! Current child policy supplies all instruction bytes. Original accounting
//! must survive even when prefix exclusion removes its only raw carrier.

use super::*;

/// Compares descriptive identity and cached costs while current configuration
/// continues to supply the actual instruction bytes.
fn assert_original_stable(context: &mez_agent::AgentContext, original: &mez_agent::AgentContext) {
    let slot = |body: &mez_agent::AgentContext| {
        body.stable_slots()
            .iter()
            .find(|slot| slot.block().label == "configured agent system prompt")
            .unwrap()
            .clone()
    };
    assert_eq!(slot(context).block(), slot(original).block());
    assert_eq!(slot(context).record(), slot(original).record());
}

/// Real covered stable carrier and independent assistant-only raw tail, shared
/// by durable and NoStore copy regressions. This is historical source, not a
/// fabricated task closure or ordinary append acknowledgment.
fn covered_source() -> (
    RuntimeSessionService,
    AgentTranscriptStore,
    mez_core::ids::ClientId,
    mez_agent::AgentContext,
) {
    let root = temp_root("inherited-stable-carrier");
    let store = AgentTranscriptStore::new(&root);
    let mut service = test_runtime_service();
    let settings = "[agents]\ncustom_system_prompt = \"current trusted guidance 雪\"\n";
    let config = root.join("config.toml");
    fs::write(&config, settings).unwrap();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".into(),
            path: Some(config),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: settings.into(),
        }])
        .unwrap();
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", CONVERSATION, 0)
        .unwrap();
    let original = service
        .agent_context_for_pane_prompt("%1", "covered original user", 0)
        .unwrap();
    let event = original.chronology().last().unwrap();
    let carrier = mez_agent::transcript::TranscriptContextRecord::new(
        mez_agent::TranscriptContextEvent::user_event(
            event.sequence().get(),
            event.block().label.clone(),
            event.block().content.clone(),
        )
        .unwrap(),
        event.record(),
        Some(original.stable_block_records()),
    )
    .unwrap()
    .to_transcript_content()
    .unwrap();
    let mut first = row(1);
    first.role = TranscriptRole::System;
    first.content = carrier;
    let mut tail = row(2);
    tail.role = TranscriptRole::Assistant;
    tail.content = "inherited assistant tail".into();
    let batch = TranscriptAppendBatch::owned_rows(vec![first, tail]).unwrap();
    store.commit_accounted_transcript_batch(&batch, 1).unwrap();
    store
        .save_compaction_epoch(CONVERSATION, 1, "covered original summary")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", CONVERSATION, 2)
        .unwrap();
    (service, store, primary, original)
}

/// The parent stores its current trusted preference snapshot only in a covered
/// first row, then forks an assistant-only raw tail. Actual initial publication
/// and cold history must preserve the unchanged configured slot's occurrence
/// without copying the covered user row or installing old instruction bytes.
#[test]
fn runtime_fork_inheritance_preserves_stable_records_without_raw_carrier() {
    let (mut service, store, primary, original) = covered_source();
    let slot = |context: &mez_agent::AgentContext| {
        context
            .stable_slots()
            .iter()
            .find(|slot| slot.block().label == "configured agent system prompt")
            .unwrap()
            .clone()
    };
    let expected = slot(&original);
    let parent = service
        .agent_context_for_pane_prompt("%1", "inspect captured parent", 0)
        .unwrap();
    assert_eq!(slot(&parent).record(), expected.record());
    let raw = fs::read(store.transcript_path(CONVERSATION).unwrap()).unwrap();
    let (pane, cid, turn) = spawn_child(&mut service, &primary, false);
    let initial = slot(service.agent_turn_contexts().get(&turn.unwrap()).unwrap());
    let child_rows = store.inspect(&cid).unwrap();
    assert_eq!(child_rows.len(), 1);
    let inherited = mez_agent::transcript::TranscriptContextRecord::from_transcript_content(
        &child_rows[0].content,
    )
    .unwrap()
    .unwrap();
    assert!(
        matches!(inherited.event(), mez_agent::TranscriptContextEvent::ExecutionBlock {
        content, ..
    } if content == "inherited assistant tail")
    );
    let original_tail = parent
        .chronology()
        .iter()
        .find(|event| event.block().content == "inherited assistant tail")
        .unwrap();
    assert_eq!(inherited.record(), original_tail.record());
    service.set_agent_transcript_store(AgentTranscriptStore::new(store.root()));
    let cold = service
        .agent_context_for_pane_prompt(&pane, "inspect cold child", 0)
        .unwrap();
    let restored = slot(&cold);
    assert_eq!(
        fs::read(store.transcript_path(CONVERSATION).unwrap()).unwrap(),
        raw
    );
    service.terminate_all_pane_processes().unwrap();
    assert_eq!(initial.block(), expected.block());
    assert_eq!(restored.block(), expected.block());
    assert_eq!(
        initial.record(),
        expected.record(),
        "initial child lost stable accounting when its raw carrier was omitted"
    );
    assert_eq!(restored.record(), expected.record());
}

/// Both real copy consumers pass through the NoStore freeze before publication.
/// A covered archive carrier cannot be replaced by a newly counted child slot.
fn nostore_stable_copy(subagent: bool) {
    let (mut service, store, primary, original) = covered_source();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let parent = service
            .agent_context_for_pane_prompt("%1", "inspect source", 0)
            .unwrap();
        assert_original_stable(&parent, &original);
        let frozen = service.freeze_agent_replay_history("%1").unwrap().unwrap();
        service
            .bind_ephemeral_replay_history("%1", "nostore-stable-parent", &frozen, None)
            .unwrap();
        let current = service
            .agent_context_for_pane_prompt("%1", "inspect NoStore", 0)
            .unwrap();
        assert_original_stable(&current, &original);
        let audit = fs::read(store.transcript_path(CONVERSATION).unwrap()).unwrap();
        let (pane, cid, initial) = if subagent {
            let (pane, cid, turn) = spawn_child(&mut service, &primary, false);
            let initial = service.agent_turn_contexts()[&turn.unwrap()].clone();
            (pane, cid, initial)
        } else {
            service.start_initial_pane_process(Some("cat")).unwrap();
            service
                .execute_agent_shell_command(&primary, "/fork stable-memory-copy")
                .unwrap();
            let settled = service
                .run_pending_deferred_agent_command_for_tests()
                .unwrap()
                .unwrap();
            assert!(settled.contains("forked=true"), "{settled}");
            let child = service
                .agent_shell_store()
                .sessions()
                .find(|session| session.session_id == "stable-memory-copy")
                .unwrap()
                .clone();
            let initial = service
                .agent_context_for_pane_prompt(&child.pane_id, "inspect saved", 0)
                .unwrap();
            (child.pane_id, child.session_id, initial)
        };
        assert_original_stable(&initial, &original);
        assert_eq!(
            fs::read(store.transcript_path(CONVERSATION).unwrap()).unwrap(),
            audit
        );
        assert!(
            !store
                .transcript_path("nostore-stable-parent")
                .unwrap()
                .exists()
        );
        assert!(service.agent_token_usage_for_conversation(&cid).is_empty());

        // A genuinely new runtime cannot borrow a warm slot cache from either
        // parent or initial child admission. The child's own durable receipt is
        // the only stable-accounting baseline available to its cold history.
        let mut cold = test_runtime_service();
        cold.replace_config_layers(service.integration.config_layers().to_vec())
            .unwrap();
        cold.set_agent_transcript_store(AgentTranscriptStore::new(store.root()));
        cold.attach_primary("cold-copy", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        cold.agent_shell_store_mut().enter_or_resume("%1").unwrap();
        cold.agent_shell_store_mut()
            .bind_conversation("%1", &cid, 0)
            .unwrap();
        let reopened = cold
            .agent_context_for_pane_prompt("%1", "inspect cold", 0)
            .unwrap();
        assert_original_stable(&reopened, &original);
        assert!(service.agent_shell_store().get(&pane).is_some());
    }));
    let cleanup = crate::runtime::tests::agent::fixture_teardown::settle(&mut service);
    match result {
        Ok(()) => cleanup.unwrap(),
        Err(panic) => {
            if let Err(errors) = cleanup {
                eprintln!("stable-copy fixture cleanup also failed: {errors:?}");
            }
            std::panic::resume_unwind(panic);
        }
    }
}

#[test]
fn nostore_saved_fork_preserves_stable_records_after_source_freeze() {
    nostore_stable_copy(false);
}

#[test]
fn nostore_subagent_fork_preserves_stable_records_after_source_freeze() {
    nostore_stable_copy(true);
}
