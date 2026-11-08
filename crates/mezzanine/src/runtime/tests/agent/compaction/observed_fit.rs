//! Full canonical continuation fit, bounded retries and later-arrival pressure.
//! Calibrate only fixture input through the production estimator, then freeze
//! an independent actual operation with that cap. No in-flight profile changes,
//! fake closed source or retry-count sentinels bypass its ownership fences.

use super::*;

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

/// High prospective request estimates do not regenerate usable finals.
#[test]
fn runtime_observed_compaction_accepts_each_usable_length_without_final_fit_retry() {
    for words in [900, 1500, 3000, 4500, 6000] {
        let large = format!("{}END", "large-summary ".repeat(words));
        let (mut service, store, turn, safe) = fixture(&large, false);
        assert!(size(&mut service, SHORT) < safe);
        assert!(size(&mut service, &large) > safe);
        let sent = run_runtime_test_compaction_wave(
            &mut service,
            "%1",
            Ok(runtime_test_compaction_response(&large)),
        );
        assert_eq!(sent.len(), 1);
        assert!(service.pending_agent_compaction_tasks().is_empty());
        replay_observed::adopt(&mut service, &turn);
        replay_observed::assert_head(&service, &store, &turn, &large);
        assert!(
            !service.agent_turn_contexts()[&turn]
                .blocks()
                .iter()
                .any(|block| block.content.contains("TYPED_OLD_WORK"))
        );
    }
}

/// Invalid output may repair once; a usable nonreducing final then publishes.
#[test]
fn runtime_observed_compaction_accepts_nonreducing_final_after_empty_repair() {
    let large = format!("{}END", "large-summary ".repeat(900));
    let (mut service, store, turn, safe) = fixture(&large, false);
    assert!(size(&mut service, &large) > safe);
    complete_runtime_test_compaction(&mut service, "%1", "");
    assert!(service.take_replay_epoch_attempt_for_tests().is_none());
    let sent = run_runtime_test_compaction_wave(
        &mut service,
        "%1",
        Ok(runtime_test_compaction_response(&large)),
    );
    assert_eq!(sent.len(), 1);
    replay_observed::adopt(&mut service, &turn);
    replay_observed::assert_head(&service, &store, &turn, &large);
}

/// Post-frontier source stays exact even when it raises the next ordinary
/// estimate. Successful inference cannot pull it into another summary.
#[test]
fn runtime_observed_compaction_retains_late_arrival_without_refreshed_fit_retry() {
    let large = format!("{}END", "large-summary ".repeat(600));
    let (mut service, store, turn, safe) = fixture(&large, true);
    assert!(size(&mut service, &large) < safe);
    let arrival = late_arrival(&mut service, &turn);
    assert!(size(&mut service, &large) > safe);
    let sent = run_runtime_test_compaction_wave(
        &mut service,
        "%1",
        Ok(runtime_test_compaction_response(&large)),
    );
    assert_eq!(sent.len(), 1);
    assert!(
        !sent[0]
            .messages
            .last()
            .unwrap()
            .content
            .contains("late raw evidence")
    );
    assert!(service.pending_agent_compaction_tasks().is_empty());
    replay_observed::adopt(&mut service, &turn);
    replay_observed::assert_head(&service, &store, &turn, &large);
    assert!(
        service.agent_turn_contexts()[&turn]
            .chronology()
            .contains(&arrival)
    );
}
