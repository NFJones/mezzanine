//! Routed seeds retain the admitted canonical context's actual source owner.
//!
//! Real loop admission and routing selection must not replace imported identity,
//! accounting or original archive ownership with a later store installation.

use super::*;
use mez_agent::AutoSizingRoutingSelection;

/// Admits one real parent over distinct equal legacy rows, before routing runs.
fn admitted_parent(with_history: bool) -> (RuntimeSessionService, AgentTranscriptStore, String) {
    let original = AgentTranscriptStore::new(temp_root("routed-original-records"));
    let mut service = test_runtime_service();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    if with_history {
        service.set_agent_transcript_store(original.clone());
    }
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1", screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    for sequence in 1..=3 {
        original
            .append(&TranscriptEntry {
                conversation_id: "routed-seed-original".into(),
                sequence,
                created_at_unix_seconds: 1,
                role: TranscriptRole::Assistant,
                turn_id: "prior-turn".into(),
                agent_id: "agent-%1".into(),
                pane_id: "%1".into(),
                content: "ORIGINAL_ROUTED_SEED 雪 {\"code\":[1,2,3]}".into(),
            })
            .unwrap();
    }
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "routed-seed-original", 3)
        .unwrap();
    service
        .execute_agent_shell_loop_command("%1", "/loop implement original routed seed")
        .unwrap();
    let parent = service
        .agent_loop_turns_for_tests()
        .iter()
        .find(|(_, turn)| turn.pane_id == "%1")
        .map(|(id, _)| id.clone())
        .unwrap();
    (service, original, parent)
}

/// Starts the actual runtime-managed worker without a paid provider request.
fn select_worker(service: &mut RuntimeSessionService, parent: &str) -> mez_agent::AgentTurnRecord {
    let profile = service.agent_turn_model_profile(parent).unwrap().clone();
    service
        .apply_routing_selected_transition(
            &AgentId::opaque("agent-%1").unwrap(),
            parent,
            AutoSizingRoutingSelection {
                selected_profile: profile,
                selected_profile_name: "default".into(),
                routing_token_usage_by_model: Default::default(),
                decision_summary: None,
                fallback: None,
            },
        )
        .unwrap();
    let id = service
        .routed_workflow_for_tests(parent)
        .unwrap()
        .child_turn_id
        .clone()
        .unwrap();
    service.agent_turn_ledger().turn(&id).unwrap().clone()
}

/// Store replacement between actual parent admission and routing cannot rebind
/// a copied seed. Original distinct records, imported boundary and source store
/// accompany the real child while its own ephemeral target policy stays true.
#[test]
fn routed_seed_preserves_original_records_source_and_imported_boundary_after_replacement() {
    let (mut service, original, parent) = admitted_parent(true);
    let boundary = service.agent_turn_imported_history_sequence_high_water(&parent);
    assert!(boundary > 0);
    let frozen_end = service.agent_turn_contexts()[&parent].event_sequence_high_water_mark();
    assert!(frozen_end > boundary);
    let records = service.agent_turn_contexts()[&parent]
        .chronology()
        .iter()
        .filter(|event| event.block().source == ContextSourceKind::TranscriptAssistant)
        .map(|event| event.record())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 3);
    assert_ne!(records[0].occurrence_id(), records[1].occurrence_id());
    assert_eq!(
        service.agent_turn_history_source(&parent).unwrap().store(),
        Some(original.clone())
    );
    let replacement = AgentTranscriptStore::new(temp_root("routed-foreign-records"));
    for mut row in original.inspect("routed-seed-original").unwrap() {
        row.content = "FOREIGN_ROUTED_SEED".into();
        replacement.append(&row).unwrap();
    }
    service.set_agent_transcript_store(replacement.clone());
    let worker = select_worker(&mut service, &parent);
    let source = service
        .agent_turn_history_source(&worker.turn_id)
        .expect("actual routed child must retain the original seed source");
    assert_eq!(source.store(), Some(original.clone()));
    assert_eq!(source.conversation_id(), Some("routed-seed-original"));
    assert_eq!(source.ephemeral(), Some(true));
    assert_eq!(
        service.agent_turn_imported_history_sequence_high_water(&worker.turn_id),
        frozen_end
    );
    let child = &service.agent_turn_contexts()[&worker.turn_id];
    let inherited = child
        .chronology()
        .iter()
        .filter(|event| event.block().source == ContextSourceKind::TranscriptAssistant)
        .map(|event| event.record())
        .collect::<Vec<_>>();
    assert_eq!(inherited, records);
    assert!(
        child
            .blocks()
            .iter()
            .any(|block| block.content.contains("ORIGINAL_ROUTED_SEED"))
    );
    assert!(
        !child
            .blocks()
            .iter()
            .any(|block| block.content.contains("FOREIGN_ROUTED_SEED"))
    );
    assert!(
        replacement
            .inspect("routed-seed-original")
            .unwrap()
            .iter()
            .all(|row| row.content == "FOREIGN_ROUTED_SEED")
    );
    service.terminate_all_pane_processes().unwrap();
}

/// A real final response for the runtime's routed completion planner. No model
/// request is sent: the action result supplies the exact final output owner.
fn final_execution(turn: &mez_agent::AgentTurnRecord, text: &str) -> mez_agent::AgentTurnExecution {
    let action = mez_agent::AgentAction {
        id: format!("final-{}", turn.turn_id),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Final,
            text: text.into(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
        },
    };
    mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".into(),
            model: "test".into(),
            raw_text: text.into(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: None,
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: vec![mez_agent::ActionResult::succeeded(
            turn,
            &action,
            vec![text.into()],
            None,
        )],
        final_turn: true,
        terminal_state: AgentTurnState::Completed,
    }
}

/// Retiring an actual managed child must retain its last execution context,
/// not only its initial seed. Repeat through malformed handoff and bounded
/// repair while a foreign store is installed; all original records and the
/// imported boundary must survive, including equal rows and late evidence.
#[test]
fn routed_seed_retirement_handoff_and_repair_preserve_the_actual_canonical_owner() {
    let (mut service, original, parent) = admitted_parent(true);
    let worker = select_worker(&mut service, &parent);
    let context = service
        .agent_turn_contexts_mut()
        .get_mut(&worker.turn_id)
        .unwrap();
    context
        .append_reference_event(
            ContextSourceKind::LocalMessage,
            "late execution",
            "LATE_EXECUTION 雪",
        )
        .unwrap();
    let original_events = context.chronology().to_vec();
    let replacement = AgentTranscriptStore::new(temp_root("routed-retirement-foreign"));
    let mut foreign_rows = original.inspect("routed-seed-original").unwrap();
    for row in &mut foreign_rows {
        row.content = "FOREIGN_RETIRED_SEED".into();
    }
    replacement.append_many(&foreign_rows).unwrap();
    service.set_agent_transcript_store(replacement.clone());

    let mut current = worker;
    for output in ["actual worker final", "invalid handoff"] {
        service
            .agent_turn_ledger_mut()
            .finish_turn(&current.turn_id, AgentTurnState::Completed)
            .unwrap();
        service
            .clear_terminal_agent_turn_runtime_state(&current.turn_id)
            .unwrap();
        assert!(!service.agent_turn_contexts().contains_key(&current.turn_id));
        assert!(
            service
                .agent_turn_history_source(&current.turn_id)
                .is_none()
        );
        assert_eq!(
            service.agent_turn_imported_history_sequence_high_water(&current.turn_id),
            0
        );
        assert!(
            service
                .handle_routed_child_execution_result(&current, &final_execution(&current, output))
                .unwrap()
        );
        let id = service
            .routed_workflow_for_tests(&parent)
            .unwrap()
            .child_turn_id
            .clone()
            .unwrap();
        assert_ne!(id, current.turn_id);
        let next = &service.agent_turn_contexts()[&id];
        assert_eq!(
            &next.chronology()[..original_events.len()],
            original_events.as_slice()
        );
        assert_eq!(
            service.agent_turn_imported_history_sequence_high_water(&id),
            next.chronology()[next.chronology().len() - 2]
                .sequence()
                .get()
        );
        let source = service.agent_turn_history_source(&id).unwrap();
        assert_eq!(source.store(), Some(original.clone()));
        assert_eq!(source.conversation_id(), Some("routed-seed-original"));
        assert_eq!(source.ephemeral(), Some(true));
        current = service.agent_turn_ledger().turn(&id).unwrap().clone();
    }
    assert!(
        service.agent_turn_contexts()[&current.turn_id]
            .blocks()
            .iter()
            .any(|block| block.label == "routed handoff validation feedback")
    );
    assert_eq!(
        replacement.inspect("routed-seed-original").unwrap(),
        foreign_rows
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Source absence is captured at real parent admission. Installing storage
/// afterward must not retroactively supply an archive for a routed worker.
#[test]
fn routed_seed_keeps_admitted_source_absence_after_store_installation() {
    let (mut service, _uninstalled, parent) = admitted_parent(false);
    assert!(
        service
            .agent_turn_history_source(&parent)
            .unwrap()
            .store()
            .is_none()
    );
    assert_eq!(
        service.agent_turn_imported_history_sequence_high_water(&parent),
        0
    );
    service.set_agent_transcript_store(AgentTranscriptStore::new(temp_root("routed-late-install")));
    let frozen_end = service.agent_turn_contexts()[&parent].event_sequence_high_water_mark();
    let worker = select_worker(&mut service, &parent);
    let source = service.agent_turn_history_source(&worker.turn_id).unwrap();
    assert!(source.store().is_none());
    assert!(source.conversation_id().is_none());
    assert_eq!(source.ephemeral(), Some(true));
    assert_eq!(
        service.agent_turn_imported_history_sequence_high_water(&worker.turn_id),
        frozen_end
    );
    service.terminate_all_pane_processes().unwrap();
}
