//! Full canonical continuation fit, bounded retries and later-arrival pressure.
//! Calibrate only fixture input through the production estimator, then freeze
//! an independent actual operation with that cap. No in-flight profile changes,
//! fake closed source or retry-count sentinels bypass its ownership fences.

use super::*;
use std::sync::Arc;

const SHORT: &str = "shorter final summary";

fn size(service: &mut RuntimeSessionService, summary: &str) -> usize {
    service
        .observed_compaction_candidate_input_tokens_for_tests("%1", summary)
        .unwrap()
}

fn late_arrival(service: &mut RuntimeSessionService, turn: &str) -> mez_agent::ConversationEvent {
    service
        .agent_turn_contexts_mut()
        .get_mut(turn)
        .unwrap()
        .update_replay_owner(|owner| {
            owner.append_execution_events([mez_agent::ContextConversationAppend::assistant(
                "late raw evidence",
                "late raw evidence ".repeat(800),
                mez_agent::ContextExecutionGroupId::new("late-fit-source")?,
            )])?;
            Ok(())
        })
        .unwrap();
    service.agent_turn_contexts()[turn]
        .chronology()
        .last()
        .unwrap()
        .clone()
}

fn fixture(
    large: &str,
    late: bool,
) -> (RuntimeSessionService, AgentTranscriptStore, String, usize) {
    let (mut calibration, _, calibration_turn) = observed_history::queued();
    if late {
        late_arrival(&mut calibration, &calibration_turn);
    }
    let short = size(&mut calibration, SHORT);
    let large_size = size(&mut calibration, large);
    assert!(large_size > short + 2);
    let cap = ((short + (large_size - short) / 2) * 20).div_ceil(19);
    let safe = cap - cap.div_ceil(20);
    let (service, store, turn) = observed_history::queued_with_cap(cap);
    (service, store, turn, safe)
}

fn assert_unpublished(
    service: &mut RuntimeSessionService,
    store: &AgentTranscriptStore,
    turn: &str,
) {
    let conversation = &service
        .agent_turn_ledger()
        .turn(turn)
        .unwrap()
        .conversation_id;
    assert!(
        store
            .load_replay_epoch(conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .is_none()
    );
    assert!(service.take_replay_epoch_attempt_for_tests().is_none());
    assert!(!service.agent_provider_task_is_pending(turn));
}

#[test]
fn runtime_observed_compaction_retries_oversized_final_request() {
    let large = "large-summary ".repeat(900);
    let (mut service, store, turn, safe) = fixture(&large, false);
    assert!(size(&mut service, SHORT) < safe);
    assert!(size(&mut service, &large) > safe);
    let before = service.agent_turn_contexts()[&turn].clone();
    let original = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .clone();
    complete_runtime_test_compaction(&mut service, "%1", &large);
    assert_unpublished(&mut service, &store, &turn);
    assert_eq!(service.agent_turn_contexts()[&turn], before);
    let retry = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    let crate::runtime::RuntimeAgentCompactionTarget::ActiveTurn {
        final_request_retry,
        ..
    } = &retry.target
    else {
        panic!("active target")
    };
    assert_eq!(final_request_retry.attempts, 1);
    assert_eq!(
        retry.request.max_output_tokens,
        original.request.max_output_tokens
    );
    assert!(Arc::ptr_eq(
        retry.replay_source.as_ref().unwrap(),
        original.replay_source.as_ref().unwrap()
    ));
    complete_runtime_test_compaction(&mut service, "%1", SHORT);
    replay_observed::adopt(&mut service, &turn);
    replay_observed::assert_head(&service, &store, &turn, SHORT);
    assert!(
        !service.agent_turn_contexts()[&turn]
            .blocks()
            .iter()
            .any(|block| block.content.contains("TYPED_OLD_WORK"))
    );
}

/// All four oversized outputs actually shrink; the fifth fits. No counter or
/// previous-size field is injected to simulate passing an arbitrary retry cap.
#[test]
fn runtime_observed_compaction_allows_fourth_improving_final_retry() {
    let smallest = "large-summary ".repeat(1_500);
    let (mut service, store, turn, safe) = fixture(&smallest, false);
    assert!(size(&mut service, &smallest) > safe);
    let original = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .replay_source
        .clone()
        .unwrap();
    let before = service.agent_turn_contexts()[&turn].clone();
    let mut previous = None;
    for (index, words) in [6_000, 4_500, 3_000, 1_500].into_iter().enumerate() {
        complete_runtime_test_compaction(&mut service, "%1", &"large-summary ".repeat(words));
        assert_unpublished(&mut service, &store, &turn);
        assert!(
            service.agent_turn_contexts().contains_key(&turn),
            "improving candidate {index} retired its original turn"
        );
        assert_eq!(service.agent_turn_contexts()[&turn], before);
        let retry = service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap();
        assert!(Arc::ptr_eq(
            retry.replay_source.as_ref().unwrap(),
            &original
        ));
        let crate::runtime::RuntimeAgentCompactionTarget::ActiveTurn {
            final_request_retry,
            ..
        } = &retry.target
        else {
            panic!("active target")
        };
        assert_eq!(final_request_retry.attempts, index as u32 + 1);
        let current = (
            final_request_retry.last_input_tokens.unwrap(),
            final_request_retry.summary_ceiling.unwrap(),
        );
        if let Some((tokens, ceiling)) = previous {
            assert!(current.0 < tokens && current.1 < ceiling);
        }
        previous = Some(current);
    }
    complete_runtime_test_compaction(&mut service, "%1", SHORT);
    replay_observed::adopt(&mut service, &turn);
    replay_observed::assert_head(&service, &store, &turn, SHORT);
}

#[test]
fn runtime_observed_compaction_rejects_non_reducing_final_retry() {
    let large = "large-summary ".repeat(900);
    let (mut service, store, turn, safe) = fixture(&large, false);
    assert!(size(&mut service, &large) > safe);
    let original_facts = service.agent_turn_contexts()[&turn]
        .chronology()
        .iter()
        .filter(|event| {
            event.block().source == ContextSourceKind::CommittedEvidence
                && event.block().label == "canonical recovery facts"
        })
        .map(|event| event.record())
        .collect::<Vec<_>>();
    assert!(
        !original_facts.is_empty(),
        "actual ordinary execution supplied its own facts"
    );
    complete_runtime_test_compaction(&mut service, "%1", "");
    assert!(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .is_some()
    );
    complete_runtime_test_compaction(&mut service, "%1", &large);
    assert_unpublished(&mut service, &store, &turn);
    assert!(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .is_some()
    );
    complete_runtime_test_compaction(&mut service, "%1", &large);
    assert_unpublished(&mut service, &store, &turn);
    assert!(!service.agent_is_compacting("%1"));
    assert_eq!(
        service.agent_turn_ledger().turn(&turn).unwrap().state,
        AgentTurnState::Failed
    );
    let conversation = &service
        .agent_turn_ledger()
        .turn(&turn)
        .unwrap()
        .conversation_id;
    assert!(!service.agent_turn_executions().contains_key(&turn));
    let rows = store.inspect(conversation).unwrap();
    // These facts came from real ordinary executions. Auxiliary failure may
    // retain them, but must not manufacture another ordinary recovery record.
    let mut seen = std::collections::BTreeSet::new();
    for row in rows.iter().filter(|row| {
        row.role == mez_agent::transcript::TranscriptRole::System
            && row.content.contains("canonical recovery facts")
    }) {
        let record =
            mez_agent::transcript::TranscriptContextRecord::from_transcript_content(&row.content)
                .unwrap()
                .unwrap()
                .record();
        assert!(original_facts.contains(&record));
        assert!(seen.insert(record.occurrence_id()));
    }
    assert_eq!(seen.len(), original_facts.len());
    let failure = &rows
        .iter()
        .find(|row| {
            row.turn_id == turn
                && row.role == mez_agent::transcript::TranscriptRole::Assistant
                && row.content.contains("Auxiliary compaction:")
        })
        .expect("actual terminal assistant provenance")
        .content;
    assert!(failure.contains("phase=CompleteCandidate"), "{failure}");
    assert!(failure.contains("corrections_admitted=1"), "{failure}");
    assert!(failure.contains("source_generation=Some("), "{failure}");
    assert!(failure.contains("bytes=Some("), "{failure}");
    assert!(!failure.contains(&large));
}

/// The initially fitting result becomes oversized only after an actual pending
/// original arrival. The frozen selected prefix excludes it throughout retries.
#[test]
fn runtime_observed_compaction_retries_refreshed_only_overflow() {
    let large = "large-summary ".repeat(600);
    let (mut service, store, turn, safe) = fixture(&large, true);
    assert!(size(&mut service, &large) < safe);
    let source = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .replay_source
        .clone()
        .unwrap();
    let arrival = late_arrival(&mut service, &turn);
    assert!(size(&mut service, SHORT) < safe);
    assert!(size(&mut service, &large) > safe);
    complete_runtime_test_compaction(&mut service, "%1", &large);
    assert_unpublished(&mut service, &store, &turn);
    let retry = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    assert!(Arc::ptr_eq(retry.replay_source.as_ref().unwrap(), &source));
    assert!(
        !retry
            .request
            .messages
            .last()
            .unwrap()
            .content
            .contains("late raw evidence")
    );
    complete_runtime_test_compaction(&mut service, "%1", SHORT);
    replay_observed::adopt(&mut service, &turn);
    replay_observed::assert_head(&service, &store, &turn, SHORT);
    let after = &service.agent_turn_contexts()[&turn];
    assert!(after.chronology().contains(&arrival));
    assert!(
        !after
            .blocks()
            .iter()
            .any(|block| block.content.contains("TYPED_OLD_WORK"))
    );
}
