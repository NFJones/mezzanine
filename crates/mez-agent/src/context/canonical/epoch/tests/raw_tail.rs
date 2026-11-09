//! Summary-free retention keeps exact obligations even when optional source
//! follows them. These tests exercise the canonical planner and adopter; no
//! model, persistence acknowledgment or execution effect is fabricated.

use super::*;

fn omission_owner() -> ReplayEpochContext {
    ReplayEpochContext::new_for_head_policy(
        ReplayConversationId::new("conversation-a").unwrap(),
        owner().context().clone(),
        ReplayHeadPolicy::IntentionalOmission,
    )
    .unwrap()
}

fn raw_plan(live: &ReplayEpochContext, policy: ReplayTailPolicy) -> ReplayCompactionPlan {
    match live.prepare_compaction(policy).unwrap() {
        ReplayCompactionPreparation::Ready(plan) => plan,
        other => panic!("expected raw omission progress: {other:?}"),
    }
}

fn raw_policy(context: usize, fixed: usize, limit: Option<usize>) -> ReplayTailPolicy {
    ReplayTailPolicy::new(context, 100)
        .unwrap()
        .with_raw_tail_limits(fixed, limit)
}

fn raw_ids(live: &ReplayEpochContext) -> Vec<ContextBlockOccurrenceId> {
    live.members()
        .iter()
        .map(ReplayEpochMember::occurrence_id)
        .collect()
}

/// A pending peer at the front cannot force all later optional history to
/// remain. Zero optional allowance removes only optional original occurrences,
/// retaining the actual peer and any post-frontier arrival without a head.
#[test]
fn raw_tail_omits_optional_source_after_a_protected_group() {
    let mut live = omission_owner();
    let peer = live
        .append_pending_peer("peer", "exact pending source")
        .unwrap();
    live.append_fact("optional", "discard this old optional fact")
        .unwrap();
    live.append_fact("optional", "discard this newer optional fact")
        .unwrap();
    let protected = live.context().chronology()[0].clone();
    let frozen = match live
        .prepare_compaction(ReplayTailPolicy::new(1_000, 0).unwrap())
        .unwrap()
    {
        ReplayCompactionPreparation::Ready(plan) => plan,
        other => panic!("optional source must be removable around protection: {other:?}"),
    };
    assert!(!frozen.requires_model_summary());
    let arrival = live.append_fact("arrival", "late exact source").unwrap();
    let late = live.context().chronology().last().unwrap().clone();
    live.publish_compaction(frozen, ReplayHeadPublication::IntentionalOmission)
        .unwrap();
    assert!(live.head().is_none());
    assert_eq!(live.context().chronology(), &[protected, late]);
    assert_eq!(live.members()[1].origin(), arrival);
    assert!(live.context().peer_message_is_pending(peer));
    assert!(live.release_peer(peer).unwrap());
}

/// Stable instructions, exact peer source and request framing reserve capacity
/// before the one optional allowance. Exact source may exceed that allowance.
#[test]
fn raw_tail_accounts_for_fixed_stable_and_protected_input_once() {
    let mut live = owner();
    live.append_pending_peer("peer", "required peer source ".repeat(30))
        .unwrap();
    live.append_fact("fact", "older optional source ".repeat(40))
        .unwrap();
    live.append_fact("fact", "newest small source").unwrap();
    let source = live.context().chronology().to_vec();
    let protected_cost = source[0].record().estimated_input_tokens();
    let newest_cost = source[2].record().estimated_input_tokens();
    assert!(protected_cost > newest_cost);
    let fixed = 17;
    let capacity =
        live.input_estimates().stable_input_tokens() + protected_cost + fixed + newest_cost;
    let before = live.clone();
    let frozen = raw_plan(&live, raw_policy(capacity, fixed, None));
    assert_eq!(
        live, before,
        "planning cannot change committed mode or source"
    );
    assert_eq!(frozen.retained_optional_units(), Some(1));
    assert_eq!(
        frozen.retained_raw_events().cloned().collect::<Vec<_>>(),
        vec![source[0].clone(), source[2].clone()]
    );
    assert_eq!(
        frozen.input_estimates().retained_raw_input_tokens(),
        protected_cost + newest_cost
    );
    live.publish_compaction(frozen, ReplayHeadPublication::IntentionalOmission)
        .unwrap();
    assert_eq!(
        live.context().chronology(),
        &[source[0].clone(), source[2].clone()]
    );
    assert_eq!(live.input_estimates().summary_input_tokens(), 0);
    assert_eq!(
        live.input_estimates().raw_input_tokens(),
        protected_cost + newest_cost
    );
}

/// An older small fact would fit after the newest fact, but it is separated by
/// an oversized optional unit. It must not backfill the remaining allowance.
#[test]
fn raw_tail_never_backfills_past_a_nonfitting_newer_unit() {
    let mut live = omission_owner();
    live.append_fact("fact", "same small source").unwrap();
    live.append_fact("fact", "large optional source ".repeat(100))
        .unwrap();
    live.append_fact("fact", "same small source").unwrap();
    let newest = live.context().chronology()[2].clone();
    let cost = newest.record().estimated_input_tokens();
    assert_eq!(
        live.context().chronology()[0]
            .record()
            .estimated_input_tokens(),
        cost
    );
    let capacity = live.input_estimates().stable_input_tokens() + 2 * cost;
    let frozen = raw_plan(&live, raw_policy(capacity, 0, None));
    assert_eq!(frozen.retained_optional_units(), Some(1));
    assert_eq!(frozen.removed_raw_members().count(), 2);
    live.publish_compaction(frozen, ReplayHeadPublication::IntentionalOmission)
        .unwrap();
    assert_eq!(live.context().chronology(), &[newest]);
}

#[test]
fn raw_tail_can_omit_every_optional_unit_when_the_newest_is_oversized() {
    let mut live = omission_owner();
    live.append_fact("fact", "small old source").unwrap();
    let small_cost = live.context().chronology()[0]
        .record()
        .estimated_input_tokens();
    live.append_fact("fact", "αβ😀 dense([1,2,3]); ".repeat(100))
        .unwrap();
    let capacity = live.input_estimates().stable_input_tokens() + small_cost;
    let frozen = raw_plan(&live, raw_policy(capacity, 0, None));
    assert!(frozen.retained_tail().is_empty());
    assert_eq!(frozen.retained_optional_units(), Some(0));
    assert!(frozen.summary_input().is_empty());
    live.publish_compaction(frozen, ReplayHeadPublication::IntentionalOmission)
        .unwrap();
    assert!(live.head().is_none());
    assert!(live.members().is_empty());
    assert!(live.context().chronology().is_empty());
    assert_eq!(live.context().stable_slots().len(), 1);
}

/// Repeated dense code has equal bytes/costs but distinct original identities.
/// Retention is global; the active prompt remains exact even above 1% allowance.
#[test]
fn raw_tail_retention_one_ten_and_one_hundred_preserves_exact_occurrences() {
    let mut source = owner();
    source
        .append_fact("covered", "covered before raw mode")
        .unwrap();
    source
        .publish_compaction(
            plan(&source),
            ReplayHeadPublication::Summary("previous head".into()),
        )
        .unwrap();
    let dense = "let ü = [1,2,3]; // é😀\n".repeat(6);
    for _ in 0..6 {
        source.append_fact("code", dense.clone()).unwrap();
    }
    let task = source.begin_instruction_task().unwrap();
    source
        .append_instruction(
            &task,
            ContextBlock::user_event("prompt", "exact current instruction"),
        )
        .unwrap();
    let rows = source.context().chronology()[1..].to_vec();
    let cost = rows[0].record().estimated_input_tokens();
    assert!(cost > 10 && cost <= 100);
    assert_eq!(
        rows[..6]
            .iter()
            .map(|event| event.record().occurrence_id())
            .collect::<BTreeSet<_>>()
            .len(),
        6
    );
    assert!(
        source.input_estimates().stable_input_tokens()
            + 6 * cost
            + rows[6].record().estimated_input_tokens()
            + 13
            < 1_000
    );
    for (percent, expected) in [(1, 0), (10, 100 / cost), (100, 6)] {
        let mut live = source.clone();
        let policy = ReplayTailPolicy::new(1_000, percent)
            .unwrap()
            .with_raw_tail_limits(13, None);
        let frozen = raw_plan(&live, policy);
        assert_eq!(frozen.retained_optional_units(), Some(expected));
        live.publish_compaction(frozen, ReplayHeadPublication::IntentionalOmission)
            .unwrap();
        assert_eq!(live.context().chronology(), &rows[6 - expected..]);
        assert!(live.head().is_none());
        assert_eq!(
            live.context().chronology().last().unwrap().retention(),
            ContextRetention::Exact
        );
    }
}

/// The whole overlapping ownership span is mandatory, including an intervening
/// neutral fact. Omitting later optional source must leave its exact fence usable.
#[test]
fn raw_tail_preserves_an_open_noncontiguous_group_and_its_closure_handle() {
    let mut live = omission_owner();
    let group = ContextExecutionGroupId::new("pending-group").unwrap();
    live.append_execution_events([ContextConversationAppend::assistant(
        "assistant",
        "first",
        group.clone(),
    )])
    .unwrap();
    live.append_fact("interleaved", "inside the complete ownership span")
        .unwrap();
    let frontier = live
        .append_execution_events([ContextConversationAppend::assistant(
            "assistant",
            "last",
            group.clone(),
        )])
        .unwrap()[0];
    let fence = live.capture_execution_owner(&group, frontier).unwrap();
    let required = live.context().chronology().to_vec();
    live.append_fact("optional", "outside the pending span")
        .unwrap();
    let frozen = raw_plan(&live, ReplayTailPolicy::new(1_000, 0).unwrap());
    assert_eq!(frozen.removed_raw_members().count(), 1);
    live.publish_compaction(frozen, ReplayHeadPublication::IntentionalOmission)
        .unwrap();
    assert_eq!(live.context().chronology(), required);
    live.finish_execution_group(&fence).unwrap();
    assert!(live.execution_group_finished(&fence));
}

#[test]
fn raw_tail_protected_irreducibility_and_overflow_preserve_source() {
    let mut live = omission_owner();
    live.append_pending_peer("peer", "mandatory source")
        .unwrap();
    live.append_fact("optional", "optional source").unwrap();
    let before = live.clone();
    assert_eq!(
        live.prepare_compaction(raw_policy(1, 0, None)).unwrap(),
        ReplayCompactionPreparation::NoWork(ReplayCompactionNoWork::ProtectedInputExceedsBudget)
    );
    assert!(
        live.prepare_compaction(raw_policy(1_000, usize::MAX, None))
            .is_err()
    );
    assert_eq!(live, before);
}

/// A real rejection adapter can monotonically lower the whole-unit frontier.
/// Each publication removes original optional membership; repeating the same
/// allowance without a stricter frontier reports NoWork instead of a new epoch.
#[test]
fn raw_tail_optional_frontier_strictly_descends_to_irreducibility() {
    let mut live = omission_owner();
    for _ in 0..3 {
        live.append_fact("fact", "equal original occurrence")
            .unwrap();
    }
    let original = raw_ids(&live);
    for limit in [2, 1, 0] {
        let frozen = raw_plan(&live, raw_policy(10_000, 0, Some(limit)));
        assert_eq!(frozen.retained_optional_units(), Some(limit));
        live.publish_compaction(frozen, ReplayHeadPublication::IntentionalOmission)
            .unwrap();
        assert_eq!(raw_ids(&live), original[3 - limit..]);
        let revision = live.revision();
        assert!(matches!(
            live.prepare_compaction(raw_policy(10_000, 0, None))
                .unwrap(),
            ReplayCompactionPreparation::NoWork(_)
        ));
        assert_eq!(live.revision(), revision);
    }
    assert_eq!(live.revision().get(), 3);
    assert!(live.members().is_empty());
}

#[test]
fn raw_tail_prepared_adoption_preserves_live_arrivals_and_duplicate_ack() {
    let mut live = owner();
    live.append_fact("old", "remove before peer").unwrap();
    let peer = live
        .append_pending_peer("peer", "keep exact source")
        .unwrap();
    live.append_fact("old", "remove after peer").unwrap();
    let before = live.clone();
    let frozen = raw_plan(&live, raw_policy(1_000, 0, Some(0)));
    let prepared = live
        .prepare_compaction_publication(frozen, ReplayHeadPublication::IntentionalOmission)
        .unwrap();
    assert_eq!(live, before);
    let mut cold = ReplayEpochContext::from_checkpoint(
        ReplayConversationId::new("conversation-a").unwrap(),
        live.checkpoint().unwrap(),
        owner().context().clone(),
    )
    .unwrap();
    let unchanged = cold.clone();
    assert!(cold.adopt_compaction_publication(&prepared).is_err());
    assert_eq!(cold, unchanged);
    live.append_fact("arrival", "exact late original").unwrap();
    let late = live.context().chronology().last().unwrap().clone();
    assert!(live.release_peer(peer).unwrap());
    live.adopt_compaction_publication(&prepared).unwrap();
    assert!(live.head().is_none());
    assert_eq!(live.context().chronology().len(), 2);
    assert_eq!(live.context().chronology()[1], late);
    assert_eq!(live.members()[1].born_in().get(), 0);
    live.validate_source_extension(prepared.candidate())
        .unwrap();
    let adopted = live.clone();
    live.adopt_compaction_publication(&prepared).unwrap();
    assert_eq!(live, adopted);
}

/// Newly protected or extended selected groups must fail atomically, even when
/// the new group occurrence arrives after the frozen omission frontier.
#[test]
fn raw_tail_rejects_new_protection_and_crossing_execution_groups() {
    for extend in [false, true] {
        let mut live = omission_owner();
        let group = ContextExecutionGroupId::new("selected-group").unwrap();
        live.append_execution_events([ContextConversationAppend::assistant(
            "assistant",
            "original output",
            group.clone(),
        )])
        .unwrap();
        finish_group(&mut live, &group).unwrap();
        let frozen = raw_plan(&live, ReplayTailPolicy::new(1_000, 0).unwrap());
        if extend {
            live.append_execution_events([ContextConversationAppend::assistant(
                "assistant",
                "late output",
                group.clone(),
            )])
            .unwrap();
        } else {
            let mut body = live.into_owned_context();
            body.retain_pending_execution_group(&group).unwrap();
            live = ReplayEpochContext::from_owned_context(
                ReplayConversationId::new("conversation-a").unwrap(),
                body,
            )
            .unwrap();
        }
        let before = live.clone();
        assert!(
            live.publish_compaction(frozen, ReplayHeadPublication::IntentionalOmission)
                .is_err()
        );
        assert_eq!(live, before);
    }
}

/// Policy transitions consume only actual current membership, including across
/// cold checkpoints. A prior head can be intentionally removed without raw work.
#[test]
fn raw_tail_summary_mode_transitions_never_restore_omitted_source() {
    let mut live = owner();
    live.append_fact("old", "must never return").unwrap();
    let original = raw_ids(&live);
    live.publish_compaction(
        plan(&live),
        ReplayHeadPublication::Summary("first summary".into()),
    )
    .unwrap();
    live.append_fact("raw", "omit from the current epoch")
        .unwrap();
    let frozen = raw_plan(&live, raw_policy(1_000, 0, Some(0)));
    live.publish_compaction(frozen, ReplayHeadPublication::IntentionalOmission)
        .unwrap();
    for second_raw in [false, true] {
        live = ReplayEpochContext::from_checkpoint(
            ReplayConversationId::new("conversation-a").unwrap(),
            live.checkpoint().unwrap(),
            owner().context().clone(),
        )
        .unwrap();
        assert!(live.head().is_none());
        assert!(live.members().is_empty());
        assert!(!raw_ids(&live).iter().any(|id| original.contains(id)));
        if second_raw {
            break;
        }
        live.append_fact("fresh", "only current raw source")
            .unwrap();
        let policy = ReplayTailPolicy::new(1_000, 0)
            .unwrap()
            .with_head_policy(ReplayHeadPolicy::Summary);
        assert!(live.automatic_compaction_is_fresh(policy).unwrap());
        let summary = raw_plan(&live, policy);
        assert!(summary.requires_model_summary());
        assert_eq!(summary.summary_input().len(), 1);
        assert_eq!(
            summary.summary_input()[0].block().content,
            "only current raw source"
        );
        live.publish_compaction(
            summary,
            ReplayHeadPublication::Summary("new summary".into()),
        )
        .unwrap();
        let frozen = raw_plan(&live, raw_policy(1_000, 0, None));
        assert_eq!(frozen.removed_raw_members().count(), 0);
        assert_eq!(frozen.removed_events().count(), 1);
        live.publish_compaction(frozen, ReplayHeadPublication::IntentionalOmission)
            .unwrap();
    }
    assert_eq!(live.revision().get(), 4);
}

#[test]
fn raw_tail_plan_cannot_render_or_accept_model_summary_output() {
    let mut live = owner();
    live.append_fact("fact", "original source").unwrap();
    let frozen = raw_plan(&live, raw_policy(1_000, 0, Some(0)));
    let rendered =
        crate::epoch_compaction::FrozenEpochCompactionSource::render(frozen.clone(), 1_000, |_| {
            panic!("raw mode must never render model input")
        });
    assert!(rendered.is_err());
    let before = live.clone();
    for text in ["", "nonempty but unrequested model output"] {
        assert!(
            live.publish_compaction(frozen.clone(), ReplayHeadPublication::Summary(text.into()))
                .is_err()
        );
        assert_eq!(live, before);
    }
    live.publish_compaction(frozen, ReplayHeadPublication::IntentionalOmission)
        .unwrap();
    assert!(live.head().is_none());
}
