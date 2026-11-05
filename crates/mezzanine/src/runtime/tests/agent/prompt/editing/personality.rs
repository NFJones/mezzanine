//! Agent prompt editing personality tests.

use super::*;

/// A stopped turn persists the actual stable preference snapshots. Cold
/// replay must restore them at their original producer, even though configured
/// prompt/personality slots used to be appended after history import. Equal
/// trusted content retains identity and costs across a real stopped turn.
#[test]
fn runtime_cold_prompt_reuses_original_stable_preference_records() {
    /// Completes the actual stopped-turn read, append owner and typed outcome.
    fn stop_and_commit(service: &mut RuntimeSessionService) {
        service.stop_agent_turn_for_pane("%1").unwrap();
        // Terminal bookkeeping is worker-owned. Complete its actual checked read
        // and exact append proposal before opening a cold service over the archive.
        let work = service.claim_bookkeeping_candidates().pop().unwrap();
        let history = work.execute();
        assert!(
            service
                .complete_bookkeeping_candidate(work, history)
                .unwrap()
        );
        let mut committed = 0;
        for effect in service
            .drain_transcript_persistence_transition()
            .side_effects
        {
            if let RuntimeSideEffect::PersistTranscriptEntries { store, path, batch } = effect {
                let bytes = store.commit_accounted_transcript_batch(&batch, 1).unwrap();
                assert!(
                    service
                        .apply_persistence_transition(
                            crate::runtime::PersistenceEvent::TranscriptCompleted {
                                conversation_id: batch.conversation_id().into(),
                                first_sequence: batch.first_sequence(),
                                batch,
                                path,
                                bytes,
                            }
                        )
                        .unwrap()
                        .applied
                );
                committed += 1;
            }
        }
        assert!(committed > 0);
    }
    let root = temp_root("stable-preference-cold-records");
    let path = root.join("config.toml");
    let text = "[agents]\ncustom_system_prompt = \"preserve user work\"\ndefault_personality = \"careful\"\n[personalities.careful]\nname = \"Careful\"\nsystem_prompt = \"use evidence\"\nresponse_style = \"terse\"\n";
    fs::write(&path, text).unwrap();
    let store = AgentTranscriptStore::new(root.join("transcripts"));
    let configure = |text: &str| {
        let mut service = test_runtime_service();
        service
            .replace_config_layers(vec![ConfigLayer {
                name: "primary".into(),
                path: Some(path.clone()),
                format: ConfigFormat::Toml,
                scope: ConfigScope::Primary,
                trusted: true,
                text: text.into(),
            }])
            .unwrap();
        service.set_agent_transcript_store(store.clone());
        service.persistence.enable_transcript_adapter();
        service
            .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        service
    };
    let mut first = configure(text);
    let turn = first.start_agent_prompt_turn("%1", "first prompt").unwrap();
    let conversation_id = first
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let original = first
        .agent_turn_contexts()
        .get(&turn.turn_id)
        .unwrap()
        .clone();
    stop_and_commit(&mut first);
    let rows = store.inspect(&conversation_id).unwrap();
    assert!(!rows.is_empty());
    let (_, hint_record, stable) = rows
        .iter()
        .filter_map(|row| {
            mez_agent::transcript::TranscriptContextRecord::from_transcript_content(&row.content)
                .unwrap()
        })
        .map(|record| record.into_parts())
        .find(|(event, _, _)| {
            matches!(
                event,
                mez_agent::TranscriptContextEvent::InterruptedTurn { .. }
            )
        })
        .unwrap();
    assert_eq!(stable, Some(original.stable_block_records()));
    let mut cold = configure(text);
    cold.agent_shell_store_mut()
        .bind_conversation("%1", &conversation_id, rows.len() as u64)
        .unwrap();
    let next = cold.start_agent_prompt_turn("%1", "next prompt").unwrap();
    let restored = cold.agent_turn_contexts().get(&next.turn_id).unwrap();
    assert_eq!(
        restored
            .chronology()
            .iter()
            .find(|event| {
                event.block().source == ContextSourceKind::RuntimeHint
                    && event.block().label == "interrupted turn context"
            })
            .unwrap()
            .record(),
        hint_record
    );
    for label in [
        "configured agent system prompt",
        "agent personality system prompt",
        "agent shell personality",
    ] {
        let before = original
            .stable_slots()
            .iter()
            .find(|slot| slot.block().label == label)
            .unwrap();
        let after = restored
            .stable_slots()
            .iter()
            .find(|slot| slot.block().label == label)
            .unwrap();
        assert_eq!(
            before, after,
            "cold replay changed original preference slot {label}"
        );
        assert_eq!(
            restored
                .stable_slots()
                .iter()
                .filter(|slot| slot.block().label == label)
                .count(),
            1
        );
    }
    stop_and_commit(&mut cold);
    let changed_text = text.replace("preserve user work", "current trusted instructions 雪");
    fs::write(&path, &changed_text).unwrap();
    let mut changed = configure(&changed_text);
    let count = store.inspect(&conversation_id).unwrap().len() as u64;
    changed
        .agent_shell_store_mut()
        .bind_conversation("%1", &conversation_id, count)
        .unwrap();
    let archive = store.transcript_path(&conversation_id).unwrap();
    let raw = fs::read(&archive).unwrap();
    let next = changed
        .start_agent_prompt_turn("%1", "changed source")
        .unwrap();
    let revised = changed.agent_turn_contexts().get(&next.turn_id).unwrap();
    for label in [
        "configured agent system prompt",
        "agent personality system prompt",
        "agent shell personality",
    ] {
        let before = original
            .stable_slots()
            .iter()
            .find(|slot| slot.block().label == label)
            .unwrap();
        let after = revised
            .stable_slots()
            .iter()
            .find(|slot| slot.block().label == label)
            .unwrap();
        assert_eq!(
            before.record().occurrence_id(),
            after.record().occurrence_id()
        );
        if label == "configured agent system prompt" {
            assert_eq!(after.block().content, "current trusted instructions 雪");
            assert_ne!(before.record(), after.record());
            assert_ne!(before.source_fingerprint(), after.source_fingerprint());
        } else {
            assert_eq!(before, after);
        }
    }
    assert_eq!(fs::read(&archive).unwrap(), raw);
    stop_and_commit(&mut changed);
    fs::write(&path, "[agents]\n").unwrap();
    let mut removed = configure("[agents]\n");
    let count = store.inspect(&conversation_id).unwrap().len() as u64;
    removed
        .agent_shell_store_mut()
        .bind_conversation("%1", &conversation_id, count)
        .unwrap();
    let next = removed
        .start_agent_prompt_turn("%1", "removed preferences")
        .unwrap();
    let context = removed.agent_turn_contexts().get(&next.turn_id).unwrap();
    assert!(context.stable_slots().iter().all(|slot| {
        ![
            "configured agent system prompt",
            "agent personality system prompt",
            "agent shell personality",
        ]
        .contains(&slot.block().label.as_str())
    }));
    fs::remove_dir_all(root).unwrap();
}

/// Verifies `/personality` completion includes user-configured personality
/// profile ids.
///
/// Personality profiles have no built-in names, so completion must be sourced
/// from live runtime config rather than from a static candidate list.
#[test]
fn runtime_agent_prompt_personality_autocompletes_configured_profile() {
    let mut service = test_runtime_service();
    let root = temp_root("runtime-agent-personality-complete");
    let path = root.join("config.toml");
    fs::write(
        &path,
        "[personalities.careful]\nname = \"Careful\"\nresponse_style = \"terse\"\n",
    )
    .unwrap();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: Some(path.clone()),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: fs::read_to_string(&path).unwrap(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.request_agent_prompt_selector_extra_candidates_refresh("%1");

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"/personality car".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"\t".to_vec()),
                ],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(
        service
            .agent_prompt_inputs_for_tests()
            .get("%1")
            .unwrap()
            .prompt
            .buffer
            .line(),
        "/personality careful "
    );
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(root);
}

/// Verifies that `/personality` mutates live pane-scoped agent preferences and
/// that those preferences are appended to the next prompt context. This makes
/// the slash command affect provider input instead of only acknowledging a
/// runtime placeholder.
#[test]
fn runtime_agent_shell_personality_feeds_prompt_context() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let personality = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"personality","method":"agent/shell/command","params":{"idempotency_key":"personality","input":"/personality concise"}}"#,
        &primary,
    );
    assert!(personality.contains(r#""kind":"mutated""#), "{personality}");
    assert!(
        personality.contains(r#""command":"personality""#),
        "{personality}"
    );
    assert!(personality.contains("style=concise"), "{personality}");
    assert!(
        personality.contains("source=runtime-personality"),
        "{personality}"
    );
    assert!(!personality.contains("requires_runtime"), "{personality}");

    let prompt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"preference-prompt","method":"agent/shell/command","params":{"idempotency_key":"preference-prompt","input":"prepare work"}}"#,
        &primary,
    );
    assert!(prompt.contains(r#""state":"running""#), "{prompt}");
    let context = service.agent_turn_contexts().get("turn-1").unwrap();
    assert!(
        context
            .blocks()
            .iter()
            .any(|block| block.label == "agent shell personality"
                && block.content.contains("concise"))
    );
}
