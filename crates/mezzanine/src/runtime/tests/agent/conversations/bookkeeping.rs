//! Checked chronology candidate acceptance across conversation replacement.
//!
//! Accepted interruption history belongs to the original conversation, not the
//! current pane binding. These tests keep append admission separate from disk I/O.

use super::*;

/// Replacing a stopped conversation while its checked read is outstanding must
/// preserve the original interruption rows without updating replacement counters.
#[test]
fn runtime_bookkeeping_candidate_survives_conversation_replacement() {
    let store = AgentTranscriptStore::new(temp_root("bookkeeping-replacement"));
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
    service.persistence.enable_transcript_adapter();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .start_agent_prompt_turn("%1", "retain the original interrupted prompt")
        .unwrap();
    service.stop_agent_turn_for_pane("%1").unwrap();
    let original = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let work = service.claim_bookkeeping_candidates().pop().unwrap();
    let history = work.execute();
    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"replace","method":"agent/shell/command","params":{"idempotency_key":"replace","input":"/new"}}"#,
        &primary,
    );
    assert!(response.contains("new=true"), "{response}");
    let replacement = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    assert_ne!(replacement, original);
    assert!(
        service
            .complete_bookkeeping_candidate(work, history)
            .unwrap(),
        "accepted chronology must survive replacement"
    );
    let effects = service
        .drain_transcript_persistence_transition()
        .side_effects;
    let rows = effects
        .into_iter()
        .filter_map(|effect| match effect {
            RuntimeSideEffect::PersistTranscriptEntries { entries, .. } => Some(entries),
            _ => None,
        })
        .flatten()
        .collect::<Vec<_>>();
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|row| row.conversation_id == original));
    assert!(rows.iter().any(|row| {
        row.content
            .contains("retain the original interrupted prompt")
    }));
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .transcript_entries,
        0
    );
    store.append_many(&rows).unwrap();
    assert_eq!(store.inspect(&original).unwrap(), rows);
    service.terminate_all_pane_processes().unwrap();
}

/// Removing the original pane binding must not discard accepted interruption
/// history or recreate a live session while admitting its durable append.
#[test]
fn runtime_bookkeeping_candidate_survives_removed_session() {
    let (mut service, store, conversation) = stopped_candidate_fixture("removed");
    let work = service.claim_bookkeeping_candidates().pop().unwrap();
    service.agent_shell_store_mut().remove_session("%1");
    let history = work.execute();
    assert!(
        service
            .complete_bookkeeping_candidate(work, history)
            .unwrap()
    );
    assert!(service.agent_shell_store().get("%1").is_none());
    let rows = candidate_rows(&mut service);
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|row| row.conversation_id == conversation));
    store.append_many(&rows).unwrap();
    assert_eq!(store.inspect(&conversation).unwrap(), rows);
    service.terminate_all_pane_processes().unwrap();
}

/// Failed archive checks retain the accepted candidate and fence history;
/// duplicate completion cannot replace the failure with invented empty history.
#[test]
fn runtime_bookkeeping_candidate_failed_read_stays_fenced() {
    let (mut service, _, conversation) = stopped_candidate_fixture("failed-read");
    let work = service.claim_bookkeeping_candidates().pop().unwrap();
    let duplicate = work.clone();
    assert!(
        service
            .complete_bookkeeping_candidate(
                work,
                Err(MezError::invalid_state("required archive missing"))
            )
            .is_err()
    );
    assert!(service.persistence.bookkeeping_pending(&conversation));
    assert!(service.claim_bookkeeping_candidates().is_empty());
    assert!(
        !service
            .complete_bookkeeping_candidate(duplicate, Ok((Vec::new(), 1)))
            .unwrap()
    );
    assert!(service.start_agent_prompt_turn("%1", "continue").is_err());
    assert!(service.prepare_subagent_fork_read_work("agent-%1").is_err());
    assert!(candidate_rows(&mut service).is_empty());
    service.terminate_all_pane_processes().unwrap();
}

/// Only the oldest candidate for a conversation can be claimed. The next
/// candidate sees accepted pending rows and deduplicates exact repeated history.
#[test]
fn runtime_bookkeeping_candidates_accept_in_order_without_duplicates() {
    let (mut service, store, conversation) = stopped_candidate_fixture("ordered");
    let work = service.claim_bookkeeping_candidates().pop().unwrap();
    service
        .persistence
        .queue_bookkeeping_candidate(work.candidate.clone());
    assert!(service.claim_bookkeeping_candidates().is_empty());
    let history = work.execute();
    assert!(
        service
            .complete_bookkeeping_candidate(work.clone(), history)
            .unwrap()
    );
    assert!(
        !service
            .complete_bookkeeping_candidate(work, Ok((Vec::new(), 1)))
            .unwrap()
    );
    let next = service.claim_bookkeeping_candidates().pop().unwrap();
    let history = next.execute();
    assert!(
        service
            .complete_bookkeeping_candidate(next, history)
            .unwrap()
    );
    assert!(!service.persistence.bookkeeping_pending(&conversation));
    let rows = candidate_rows(&mut service);
    assert_eq!(
        rows.iter()
            .filter(|row| matches!(
                mez_agent::TranscriptContextEvent::from_transcript_content(&row.content),
                Some(mez_agent::TranscriptContextEvent::InterruptedTurn { .. })
            ))
            .count(),
        1
    );
    store.append_many(&rows).unwrap();
    assert_eq!(store.inspect(&conversation).unwrap(), rows);
    service.terminate_all_pane_processes().unwrap();
}

/// Creates an adapter-owned stopped turn with one unchecked interruption candidate.
fn stopped_candidate_fixture(label: &str) -> (RuntimeSessionService, AgentTranscriptStore, String) {
    let store = AgentTranscriptStore::new(temp_root(&format!("bookkeeping-{label}")));
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
    service.persistence.enable_transcript_adapter();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let conversation = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    service
        .start_agent_prompt_turn("%1", "retained candidate prompt")
        .unwrap();
    service.stop_agent_turn_for_pane("%1").unwrap();
    (service, store, conversation)
}

/// Returns accepted append rows, excluding presentation and metadata effects.
fn candidate_rows(service: &mut RuntimeSessionService) -> Vec<TranscriptEntry> {
    service
        .drain_transcript_persistence_transition()
        .side_effects
        .into_iter()
        .filter_map(|effect| match effect {
            RuntimeSideEffect::PersistTranscriptEntries { entries, .. } => Some(entries),
            _ => None,
        })
        .flatten()
        .collect()
}
