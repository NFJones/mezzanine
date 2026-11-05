//! Incremental totals are checked against an independent cached-metadata oracle
//! across lifecycle changes and three publications. No provider is contacted.

use super::*;

fn owner() -> ReplayEpochContext {
    ReplayEpochContext::new(
        ReplayConversationId::new("accounted-epochs").unwrap(),
        AgentContext::new_durable(vec![ContextBlock::stable_instruction(
            ContextSourceKind::System,
            "policy",
            "exact stable source",
        )])
        .unwrap(),
    )
    .unwrap()
}

/// The oracle visits cached metadata, never hashes/counts source text. It also
/// verifies exact origin association rather than accepting equal-sized members.
fn assert_totals(owner: &ReplayEpochContext) {
    let mut expected = ReplayEpochInputEstimates::default();
    for slot in owner.context().stable_slots() {
        expected.stable += slot.record().estimated_input_tokens();
    }
    let offset = usize::from(owner.head().is_some());
    for (index, event) in owner.context().chronology().iter().enumerate() {
        let cost = event.record().estimated_input_tokens();
        if index < offset {
            expected.summary += cost;
            assert_eq!(
                owner.head().unwrap().occurrence_id(),
                event.record().occurrence_id()
            );
        } else {
            let member = &owner.members()[index - offset];
            assert_eq!(member.occurrence_id(), event.record().occurrence_id());
            assert_eq!(member.origin(), event.sequence());
            expected.raw += cost;
            if owner.event_is_replaceable(event) {
                expected.eligible += cost;
            }
        }
    }
    assert_eq!(owner.input_estimates(), expected);
    assert_eq!(
        expected.total_input_tokens().unwrap(),
        expected.stable + expected.summary + expected.raw
    );
    owner.validate_membership().unwrap();
}

/// Reads only the existing current-thread instrumentation, with no production
/// cache or process-wide synchronization that could obscure recount behavior.
fn computations() -> usize {
    crate::context::canonical::block_record::ESTIMATE_COMPUTATIONS.with(std::cell::Cell::get)
}

/// Lifecycle retirement changes eligibility only for its exact owner/group.
/// Reopening a closed group removes its earlier members from eligible totals;
/// peer/task completion and stable refresh preserve raw occurrence records.
#[test]
fn incremental_totals_follow_exact_lifecycle_and_group_reopening() {
    let mut owner = owner();
    assert_totals(&owner);
    owner
        .append_fact("fact", "é😀 dense {\"x\":[1,2,3]}")
        .unwrap();
    let peer = owner
        .append_pending_peer("peer", "unconsumed peer")
        .unwrap();
    let task = owner.begin_instruction_task().unwrap();
    owner
        .append_instruction(&task, ContextBlock::user_event("prompt", "exact task"))
        .unwrap();
    assert_totals(&owner);
    let before = owner.input_estimates();
    assert!(before.eligible_raw_input_tokens() < before.raw_input_tokens());
    let origins = owner.context().block_records();
    assert!(owner.release_peer(peer).unwrap());
    owner.complete_instruction_task(&task).unwrap();
    assert_eq!(owner.context().block_records(), origins);
    assert_totals(&owner);
    assert_eq!(
        owner.input_estimates().eligible_raw_input_tokens(),
        owner.input_estimates().raw_input_tokens()
    );
    let group = ContextExecutionGroupId::new("one execution group").unwrap();
    let first = owner
        .append_execution_events([ContextConversationAppend::assistant(
            "assistant",
            "response",
            group.clone(),
        )])
        .unwrap();
    assert_totals(&owner);
    let fence = owner.capture_execution_owner(&group, first[0]).unwrap();
    owner.finish_execution_group(&fence).unwrap();
    assert_totals(&owner);
    assert_eq!(
        owner.input_estimates().eligible,
        owner.input_estimates().raw
    );
    let original_cost = owner
        .context()
        .chronology()
        .last()
        .unwrap()
        .record()
        .estimated_input_tokens();
    let previous = owner.input_estimates();
    let second = owner
        .append_execution_events([ContextConversationAppend::evidence(
            ContextSourceKind::ActionResult,
            "action",
            "late settled result",
            group.clone(),
            None,
            true,
        )])
        .unwrap();
    assert_totals(&owner);
    assert_eq!(
        owner.input_estimates().eligible,
        previous.eligible - original_cost
    );
    let fence = owner.capture_execution_owner(&group, second[0]).unwrap();
    owner.finish_execution_group(&fence).unwrap();
    assert_totals(&owner);
    let slots = owner.context().stable_slots().to_vec();
    let before = computations();
    owner.replace_stable_slots(slots).unwrap();
    owner.validate_membership().unwrap();
    let _ = owner.input_estimates();
    assert_eq!(
        computations(),
        before,
        "index/projection operations must not recount unchanged bytes"
    );
    assert_totals(&owner);
}

/// Three publications retain older-born raw origins and append-only arrivals,
/// exchange the actual summary occurrence, and freeze disjoint selected/tail
/// totals. Repeated planning/validation never recalculates text estimates.
#[test]
fn three_accounted_epochs_preserve_tail_birth_and_frozen_cost_partition() {
    let mut owner = owner();
    for _ in 0..4 {
        owner.append_fact("raw", "same equal source").unwrap();
    }
    let raw_cost = owner.context().chronology()[0]
        .record()
        .estimated_input_tokens();
    let initial_ids = owner
        .members()
        .iter()
        .map(ReplayEpochMember::occurrence_id)
        .collect::<BTreeSet<_>>();
    assert_eq!(initial_ids.len(), 4);
    let mut heads = BTreeSet::new();
    for round in 0..3 {
        assert_totals(&owner);
        let before = computations();
        let ReplayCompactionPreparation::Ready(plan) = owner
            .prepare_compaction(ReplayTailPolicy::new(raw_cost * 2, 100).unwrap())
            .unwrap()
        else {
            panic!("eligible prefix required");
        };
        assert_eq!(computations(), before);
        let costs = plan.input_estimates();
        assert_eq!(
            costs.selected_raw_input_tokens() + costs.retained_raw_input_tokens(),
            owner.input_estimates().raw_input_tokens()
        );
        assert_eq!(
            costs.source_input_tokens().unwrap(),
            plan.summary_input()
                .iter()
                .map(|event| event.record().estimated_input_tokens())
                .sum::<usize>()
        );
        assert_eq!(
            costs.retained_raw_input_tokens(),
            plan.retained_tail()
                .iter()
                .map(|event| event.record().estimated_input_tokens())
                .sum::<usize>()
        );
        let retained = owner.members()[plan.prefix_members..].to_vec();
        owner.append_fact("raw", "same equal source").unwrap();
        let arrival = owner.members().last().unwrap().clone();
        assert_eq!(arrival.born_in().get(), round);
        owner
            .publish_compaction(
                plan,
                ReplayHeadPublication::Summary(format!("summary round {round}")),
            )
            .unwrap();
        assert_totals(&owner);
        assert!(heads.insert(owner.head().unwrap().occurrence_id()));
        assert!(!initial_ids.contains(&owner.head().unwrap().occurrence_id()));
        assert_eq!(&owner.members()[..retained.len()], retained);
        assert_eq!(owner.members().last(), Some(&arrival));
        assert!(owner.members()[0].born_in() < owner.revision());
    }
    assert_eq!(owner.revision().get(), 3);
    assert_eq!(heads.len(), 3);
}

/// Exhausted/corrupt internal totals cannot wrap, saturate or partially adopt
/// new source. Injected maxima stand in for unallocatable real history volume;
/// independently changing an equal-sized member ID is still detected.
#[test]
fn exhausted_aggregate_and_wrong_origin_reject_without_adoption() {
    assert!(
        ReplayEpochInputEstimates {
            stable: usize::MAX,
            summary: 1,
            raw: 0,
            eligible: 0
        }
        .total_input_tokens()
        .is_err()
    );
    assert!(
        ReplayCompactionInputEstimates {
            selected_raw: usize::MAX,
            prior_summary: 1,
            retained_raw: 0
        }
        .source_input_tokens()
        .is_err()
    );
    let mut owner = owner();
    owner.append_fact("raw", "original source").unwrap();
    let valid = owner.clone();
    owner.accounting.totals.raw = usize::MAX;
    let before = owner.clone();
    assert!(owner.append_fact("raw", "new original source").is_err());
    assert_eq!(owner, before);
    assert!(
        owner
            .prepare_compaction(ReplayTailPolicy::new(100, 0).unwrap())
            .is_err()
    );
    owner = valid;
    owner.members[0].occurrence_id = ContextBlockOccurrenceId::allocate();
    assert!(owner.validate_membership().is_err());
}
