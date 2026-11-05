//! Stable accounting across physical inherited-tail publication and cold replay.
//!
//! Current child policy supplies all instruction bytes. Original accounting
//! must survive even when prefix exclusion removes its only raw carrier.

use super::*;

/// The parent stores its current trusted preference snapshot only in a covered
/// first row, then forks an assistant-only raw tail. Actual initial publication
/// and cold history must preserve the unchanged configured slot's occurrence
/// without copying the covered user row or installing old instruction bytes.
#[test]
fn runtime_fork_inheritance_preserves_stable_records_without_raw_carrier() {
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
    assert_eq!(child_rows[0].content, "inherited assistant tail");
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
