//! Mechanical source/call-count fixtures use actual canonical owners. Fake
//! summaries establish controller behavior, never model semantic completeness.

use super::*;
use crate::{
    AgentContext, ContextBlock, ContextSourceKind, ReplayCompactionPreparation,
    ReplayConversationId, ReplayEpochContext, ReplayHeadPublication, ReplayTailPolicy,
};

fn owner() -> ReplayEpochContext {
    ReplayEpochContext::new(
        ReplayConversationId::new("engine-test").unwrap(),
        AgentContext::new(vec![ContextBlock::stable_instruction(
            ContextSourceKind::System,
            "policy",
            "exact policy outside summary input",
        )])
        .unwrap(),
    )
    .unwrap()
}

fn plan(owner: &ReplayEpochContext, retained: u8) -> ReplayCompactionPlan {
    match owner
        .prepare_compaction(ReplayTailPolicy::new(100, retained).unwrap())
        .unwrap()
    {
        ReplayCompactionPreparation::Ready(plan) => plan,
        other => panic!("expected eligible canonical source, got {other:?}"),
    }
}

fn source(plan: ReplayCompactionPlan) -> FrozenEpochCompactionSource {
    FrozenEpochCompactionSource::render(plan, 1_000_000, |events| {
        Ok(events
            .iter()
            .map(|event| event.block().content.as_str())
            .collect())
    })
    .unwrap()
}

fn limits(capacity: usize) -> EpochCompactionLimits {
    EpochCompactionLimits {
        input_tokens: capacity + 10,
        framing_tokens: 10,
        source_bytes: 1_000_000,
        summary_bytes: 128 * 1024,
        buffered_summary_bytes: 1024 * 1024,
        work_nodes: 10_000,
        in_flight: 4,
        expected_summary_tokens: 100,
    }
}

fn measure(input: &str) -> AgentContextResult<usize> {
    Ok(input.len() + 10)
}

fn work(engine: &mut EpochCompactionEngine) -> EpochCompactionWork {
    match engine.next(measure).unwrap() {
        EpochCompactionStep::Work(work) => work,
        other => panic!("expected model work, got {other:?}"),
    }
}

/// Advisory word/ratio targets and the next ordinary request are deliberately
/// absent from acceptance. Even an output larger than auxiliary INPUT capacity
/// is final when it covers the whole source and fits genuine parser bounds.
#[test]
fn fitting_source_and_nonreducing_over_advisory_final_take_exactly_one_call() {
    for output in [
        "a usable final".to_string(),
        "long valid final ".repeat(200),
    ] {
        let mut owner = owner();
        owner.append_fact("fact", "short source").unwrap();
        let frozen = plan(&owner, 0);
        let expected = frozen.clone();
        let mut engine = EpochCompactionEngine::new(source(frozen), limits(100), measure).unwrap();
        let request = work(&mut engine);
        assert_eq!(request.input(), "short source");
        assert_eq!(request.raw_coverage().len(), 1);
        assert_eq!(request.raw_coverage()[0], 0..12);
        engine.complete(request.id(), output.clone()).unwrap();
        // Final acceptance must not even consult a prospective fit estimator.
        assert!(matches!(
            engine.next(|_| panic!("final was remeasured")).unwrap(),
            EpochCompactionStep::Complete
        ));
        let result = engine.into_result().unwrap();
        assert_eq!(result.summary(), output);
        assert_eq!(result.plan(), &expected);
        assert_eq!(result.work_count(), 1);
        assert_eq!(result.serial_depth(), 1);
        let (plan, summary) = result.into_parts();
        owner
            .publish_compaction(plan, ReplayHeadPublication::Summary(summary))
            .unwrap();
        assert_eq!(owner.revision().get(), 1);
        assert_eq!(owner.context().chronology().len(), 1);
    }
}

#[test]
fn oversized_unicode_source_preserves_every_byte_and_fragment_witness() {
    let mut owner = owner();
    let original = format!(
        "  {}\n\tAFTER4096💡\n{}\tEND  ",
        "α💡 ".repeat(900),
        "🦀 z\n".repeat(700)
    );
    owner.append_fact("large", original.clone()).unwrap();
    let source = source(plan(&owner, 0));
    let units = source.units().to_vec();
    assert_eq!(source.text(), original);
    let mut engine = EpochCompactionEngine::new(source, limits(1024), measure).unwrap();
    let mut raw = BTreeMap::new();
    let mut calls = 0;
    loop {
        match engine.next(measure).unwrap() {
            EpochCompactionStep::Work(work) => {
                assert!(measure(work.input()).unwrap() <= 1034);
                // Parallel leaves contain raw only. Later necessary merges
                // contain no original bytes and must combine >=2 summaries.
                if !work.raw_coverage().is_empty() {
                    assert_eq!(work.raw_coverage().len(), 1);
                    let range = work.raw_coverage()[0].clone();
                    assert_eq!(work.input(), &original[range.clone()]);
                    assert!(
                        raw.insert(range.start, (range.end, work.input().to_owned()))
                            .is_none()
                    );
                } else {
                    assert!(
                        work.input()
                            .matches("[private compaction intermediate]")
                            .count()
                            >= 2
                    );
                }
                engine
                    .complete(work.id(), format!("summary {calls}"))
                    .unwrap();
                calls += 1;
            }
            EpochCompactionStep::Complete => break,
            EpochCompactionStep::Waiting => panic!("no claim remains outstanding"),
        }
    }
    let mut next = 0;
    let mut reconstructed = String::new();
    for (start, (end, bytes)) in raw {
        assert_eq!(start, next);
        next = end;
        reconstructed.push_str(&bytes);
    }
    assert_eq!(next, original.len());
    assert_eq!(reconstructed, original);
    let result = engine.into_result().unwrap();
    assert_eq!(result.source_units(), units);
    assert_eq!(result.work_count(), calls);
    assert!(calls > 2);
}

/// A multibyte first character can straddle the byte midpoint. It must fit
/// intact when capacity permits it, rather than incorrectly fail as indivisible.
#[test]
fn utf8_split_at_first_character_and_exact_capacity_is_lossless() {
    let ranges = packing::split_raw("💡x", 0..5, 14, 10, &measure).unwrap();
    assert_eq!(ranges, vec![0..4, 4..5]);
    assert!(packing::split_raw("💡", 0..4, 13, 10, &measure).is_err());
}

#[test]
fn every_utf8_split_capacity_preserves_boundaries_and_complete_bytes() {
    for text in ["a💡b🦀cα\n  \tend", "💡x", "a💡", "ααα", "ascii"] {
        let largest_character = text.chars().map(char::len_utf8).max().unwrap();
        for capacity in largest_character..=text.len() {
            let ranges =
                packing::split_raw(text, 0..text.len(), capacity + 10, 100, &measure).unwrap();
            let mut next = 0;
            let mut rebuilt = String::new();
            for range in ranges {
                assert_eq!(range.start, next);
                assert!(range.len() <= capacity);
                rebuilt.push_str(&text[range.clone()]);
                next = range.end;
            }
            assert_eq!(rebuilt, text);
            assert_eq!(next, text.len());
        }
    }
}

#[test]
fn previous_head_once_equal_text_occurrences_and_arrivals_across_three_epochs() {
    let mut owner = owner();
    for revision in 1..=3 {
        owner.append_fact("equal", "same text").unwrap();
        owner.append_fact("equal", "same text").unwrap();
        let frozen = plan(&owner, 0);
        let units = frozen.summary_units();
        assert_eq!(units.len(), if revision == 1 { 2 } else { 4 });
        let selected = frozen
            .summary_input()
            .iter()
            .map(|event| event.record().occurrence_id())
            .collect::<Vec<_>>();
        assert_eq!(
            selected
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            selected.len()
        );
        let arrival = owner
            .append_fact("arrival", format!("arrival {revision}"))
            .unwrap();
        let mut engine =
            EpochCompactionEngine::new(source(frozen), limits(10_000), measure).unwrap();
        let work = work(&mut engine);
        assert!(!work.input().contains(&format!("arrival {revision}")));
        if revision > 1 {
            assert_eq!(
                work.input()
                    .matches(&format!("head {}", revision - 1))
                    .count(),
                1
            );
        }
        engine
            .complete(work.id(), format!("head {revision}"))
            .unwrap();
        let result = engine.into_result().unwrap();
        assert_eq!(
            result
                .source_units()
                .iter()
                .flat_map(|unit| unit.occurrences().iter().copied())
                .collect::<Vec<_>>(),
            selected
        );
        let (plan, summary) = result.into_parts();
        owner
            .publish_compaction(plan, ReplayHeadPublication::Summary(summary))
            .unwrap();
        assert_eq!(owner.revision().get(), revision);
        assert_eq!(owner.members().len(), 1);
        assert_eq!(owner.members()[0].origin(), arrival);
    }
}

#[test]
fn inherited_tail_remains_outside_source_and_partial_results_cannot_publish() {
    let mut owner = owner();
    owner.append_fact("old", "old source ".repeat(50)).unwrap();
    owner.append_fact("tail", "RAW TAIL").unwrap();
    let frozen = plan(&owner, 50);
    assert_eq!(frozen.retained_tail().len(), 1);
    let tail = frozen.retained_tail()[0].record().occurrence_id();
    let mut engine = EpochCompactionEngine::new(source(frozen), limits(1000), measure).unwrap();
    let request = work(&mut engine);
    assert!(!request.input().contains("RAW TAIL"));
    assert!(!request.input().contains("exact policy"));
    engine.complete(request.id(), "head".into()).unwrap();
    let (completed_plan, summary) = engine.into_result().unwrap().into_parts();
    owner
        .publish_compaction(completed_plan, ReplayHeadPublication::Summary(summary))
        .unwrap();
    assert_eq!(owner.members()[0].occurrence_id(), tail);
    owner.append_fact("new", "new source".repeat(100)).unwrap();
    let frozen = plan(&owner, 50);
    assert!(
        frozen
            .summary_input()
            .iter()
            .any(|event| event.record().occurrence_id() == tail)
    );
    let mut engine = EpochCompactionEngine::new(source(frozen), limits(200), measure).unwrap();
    let request = work(&mut engine);
    engine
        .complete(request.id(), "private intermediate".into())
        .unwrap();
    assert!(engine.into_result().is_err());
    assert_eq!(owner.revision().get(), 1);
}

#[test]
fn real_input_rejection_repacks_whole_units_and_preserves_exact_transport_retry() {
    let mut owner = owner();
    for text in ["a".repeat(100), "b".repeat(100), "c".repeat(100)] {
        owner.append_fact("unit", text).unwrap();
    }
    let mut engine =
        EpochCompactionEngine::new(source(plan(&owner, 0)), limits(400), measure).unwrap();
    let first = work(&mut engine);
    let retry = first.clone();
    assert_eq!(first.id(), retry.id());
    assert_eq!(first.input(), retry.input());
    engine.reject_input(first.id(), 160, measure).unwrap();
    assert!(engine.complete(first.id(), "stale".into()).is_err());
    let first = work(&mut engine);
    assert_eq!(first.input(), "a".repeat(100));
    engine.complete(first.id(), "s".into()).unwrap();
    let second = work(&mut engine);
    assert_eq!(second.input(), "b".repeat(100));
    engine.complete(second.id(), "s".into()).unwrap();
    let third = work(&mut engine);
    assert_eq!(third.input(), "c".repeat(100));
}

#[test]
fn bounded_fanout_out_of_order_completion_and_duplicate_fences() {
    let mut owner = owner();
    for _ in 0..30 {
        owner.append_fact("unit", "x".repeat(400)).unwrap();
    }
    let mut bounds = limits(500);
    bounds.in_flight = 2;
    let mut engine = EpochCompactionEngine::new(source(plan(&owner, 0)), bounds, measure).unwrap();
    assert_eq!(engine.schedule().selected, CompactionSchedule::Parallel);
    let a = work(&mut engine);
    let b = work(&mut engine);
    assert!(matches!(
        engine.next(measure).unwrap(),
        EpochCompactionStep::Waiting
    ));
    engine.complete(b.id(), "second".into()).unwrap();
    assert!(engine.complete(b.id(), "duplicate".into()).is_err());
    let c = work(&mut engine);
    assert!(c.raw_coverage()[0].start >= b.coverage().end);
    engine.complete(a.id(), "first".into()).unwrap();
    engine.complete(c.id(), "third".into()).unwrap();
    while let EpochCompactionStep::Work(work) = engine.next(measure).unwrap() {
        engine.complete(work.id(), "merged".into()).unwrap();
    }
    assert!(engine.into_result().is_ok());
}

#[test]
fn usable_intermediate_that_cannot_merge_fails_without_shortening_calls() {
    let mut owner = owner();
    owner.append_fact("source", "x".repeat(200)).unwrap();
    let mut engine =
        EpochCompactionEngine::new(source(plan(&owner, 0)), limits(100), measure).unwrap();
    let first = work(&mut engine);
    engine
        .complete(first.id(), "large valid summary".repeat(100))
        .unwrap();
    let second = work(&mut engine);
    engine
        .complete(second.id(), "another valid summary".repeat(100))
        .unwrap();
    assert!(
        engine
            .next(measure)
            .unwrap_err()
            .message()
            .contains("necessary merge")
    );
    assert_eq!(engine.work_count, 2);
    assert!(engine.into_result().is_err());
}

#[test]
fn empty_and_resource_invalid_output_preserve_claim_and_never_become_final() {
    let mut owner = owner();
    owner.append_fact("source", "source").unwrap();
    let mut engine =
        EpochCompactionEngine::new(source(plan(&owner, 0)), limits(100), measure).unwrap();
    let first = work(&mut engine);
    assert!(engine.complete(first.id(), " \n\t".into()).is_err());
    assert!(
        engine
            .complete(first.id(), "x".repeat(128 * 1024 + 1))
            .is_err()
    );
    assert_eq!(engine.claims[&first.id()].work.input(), first.input());
    engine
        .complete(first.id(), "valid repaired summary".into())
        .unwrap();
    assert_eq!(engine.into_result().unwrap().work_count(), 1);
}

#[test]
fn source_is_redacted_before_splitting_and_debug_does_not_disclose_content() {
    let mut owner = owner();
    owner
        .append_fact("sensitive", "PRIVATE-CREDENTIAL\n  💡 ".repeat(100))
        .unwrap();
    let source = FrozenEpochCompactionSource::render(plan(&owner, 0), 100_000, |events| {
        Ok(events
            .iter()
            .map(|event| {
                event
                    .block()
                    .content
                    .replace("PRIVATE-CREDENTIAL", "[redacted]")
            })
            .collect())
    })
    .unwrap();
    assert!(!source.text().contains("PRIVATE-CREDENTIAL"));
    let mut engine = EpochCompactionEngine::new(source, limits(100), measure).unwrap();
    let request = work(&mut engine);
    assert!(!request.input().contains("CREDENTIAL"));
    assert!(!format!("{request:?}").contains("redacted"));
    // Drop cancels private work; the authoritative source remains unchanged.
    drop(engine);
    assert_eq!(owner.revision().get(), 0);
    assert!(
        owner.context().chronology()[0]
            .block()
            .content
            .contains("PRIVATE-CREDENTIAL")
    );
}

#[test]
fn resource_bounds_and_schedule_estimates_are_not_semantic_acceptance() {
    let mut owner = owner();
    owner.append_fact("source", "x".repeat(1000)).unwrap();
    let mut bounds = limits(100);
    bounds.work_nodes = 2;
    assert!(EpochCompactionEngine::new(source(plan(&owner, 0)), bounds, measure).is_err());
    assert!(
        FrozenEpochCompactionSource::render(plan(&owner, 0), 999, |_| Ok("x".repeat(1000)))
            .is_err()
    );
    let mut bounds = limits(1000);
    bounds.expected_summary_tokens = 100;
    let serial = CompactionScheduleEstimate::new(
        10_000,
        EpochCompactionLimits {
            in_flight: 1,
            ..bounds
        },
    );
    let parallel = CompactionScheduleEstimate::new(10_000, bounds);
    assert_eq!(serial.initial_source_batches, 10);
    assert_eq!(serial.selected, CompactionSchedule::Rolling);
    assert_eq!(parallel.selected, CompactionSchedule::Parallel);
    assert!(parallel.parallel_depth < parallel.rolling_depth);
}

/// A noncontiguous group owns its entire span, including intervening facts.
/// Packing must reuse the canonical partition instead of adjacent equal-ID
/// runs. An active instruction task remains outside the selected source.
#[test]
fn whole_noncontiguous_execution_span_and_active_task_use_canonical_boundaries() {
    use crate::{ContextConversationAppend, ContextExecutionGroupId};
    let mut owner = owner();
    owner.append_fact("before", "p".repeat(100)).unwrap();
    let group = ContextExecutionGroupId::new("original-group").unwrap();
    owner
        .append_execution_events([ContextConversationAppend::assistant(
            "first",
            "a".repeat(100),
            group.clone(),
        )])
        .unwrap();
    owner.append_fact("between", "i".repeat(100)).unwrap();
    let frontier = owner
        .append_execution_events([ContextConversationAppend::assistant(
            "last",
            "z".repeat(100),
            group.clone(),
        )])
        .unwrap()[0];
    let closure = owner.capture_execution_owner(&group, frontier).unwrap();
    owner.finish_execution_group(&closure).unwrap();
    let task = owner.begin_instruction_task().unwrap();
    owner
        .append_instruction(
            &task,
            ContextBlock::user_event("active", "ACTIVE CONSTRAINT"),
        )
        .unwrap();
    let frozen = plan(&owner, 0);
    let units = frozen.summary_units();
    assert_eq!(units.len(), 2);
    assert_eq!(units[1].len(), 3);
    assert_eq!(frozen.retained_tail().len(), 1);
    let mut engine = EpochCompactionEngine::new(source(frozen), limits(350), measure).unwrap();
    let first = work(&mut engine);
    assert_eq!(first.input(), "p".repeat(100));
    engine.complete(first.id(), "first summary".into()).unwrap();
    let grouped = work(&mut engine);
    assert_eq!(
        grouped.input(),
        format!("{}{}{}", "a".repeat(100), "i".repeat(100), "z".repeat(100))
    );
    assert_eq!(grouped.raw_coverage().len(), 1);
    assert_eq!(grouped.raw_coverage()[0], 100..400);
    engine
        .complete(grouped.id(), "group summary".into())
        .unwrap();
    let merge = work(&mut engine);
    assert!(!merge.input().contains("ACTIVE CONSTRAINT"));
    assert!(merge.raw_coverage().is_empty());
    engine
        .complete(
            merge.id(),
            "goals, decisions, references and blocker".into(),
        )
        .unwrap();
    let result = engine.into_result().unwrap();
    assert!(!format!("{result:?}").contains("goals, decisions"));
    let (plan, summary) = result.into_parts();
    owner
        .publish_compaction(plan, ReplayHeadPublication::Summary(summary))
        .unwrap();
    assert_eq!(
        owner.context().chronology().last().unwrap().block().content,
        "ACTIVE CONSTRAINT"
    );
}

/// Provider cost/projection selection stays frozen even when another caller
/// selects a new view. Source witnesses are original occurrences, not labels.
#[test]
fn frozen_provider_projection_and_occurrence_identity_survive_reduction() {
    use crate::{ProviderApiCompatibility, ReplayProviderProjection};
    let mut owner = owner();
    owner.append_fact("equal", "same text").unwrap();
    owner.append_fact("equal", "same text").unwrap();
    let projection = ReplayProviderProjection::new(
        ProviderApiCompatibility::OpenAiResponses,
        "original",
        "model-a",
    )
    .unwrap();
    owner
        .select_provider_projection(projection.clone())
        .unwrap();
    let frozen = plan(&owner, 0);
    let estimates = frozen.input_estimates();
    owner
        .select_provider_projection(
            ReplayProviderProjection::new(
                ProviderApiCompatibility::AnthropicMessages,
                "later",
                "model-b",
            )
            .unwrap(),
        )
        .unwrap();
    let mut engine = EpochCompactionEngine::new(source(frozen), limits(100), measure).unwrap();
    let request = work(&mut engine);
    engine.complete(request.id(), "final".into()).unwrap();
    let result = engine.into_result().unwrap();
    assert_eq!(result.plan().provider_projection(), Some(&projection));
    assert_eq!(result.plan().input_estimates(), estimates);
    assert_ne!(
        result.source_units()[0].occurrences(),
        result.source_units()[1].occurrences()
    );
    let before = owner.clone();
    let (plan, summary) = result.into_parts();
    assert!(
        owner
            .publish_compaction(plan, ReplayHeadPublication::Summary(summary))
            .is_err()
    );
    assert_eq!(owner, before);
}
