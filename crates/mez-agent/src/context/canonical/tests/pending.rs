//! Pending execution ownership survives later chronology and request clones.
//!
//! These tests exercise the canonical owner, compaction planner and both apply
//! boundaries. A later peer event is not proof that an outstanding task closed.

use super::*;
use crate::{
    apply_model_context_compaction_plan, plan_model_context_compaction_at_consumed_sequence,
};

/// Exact committed reply sources remain canonical while a request's wake queue
/// is full. Both direct and previously frozen model compaction reject dropping
/// them, with no model-visible metadata mutation; explicit release restores
/// eligibility. A user event cannot be used as a fabricated peer-source pin.
#[test]
fn request_bound_pending_peer_source_survives_compaction_until_handoff() {
    let mut context =
        AgentContext::new_durable(vec![ContextBlock::user_event("user", "coordinate")]).unwrap();
    let peer = context
        .append_peer_message_event("actual reply", "untrusted response ".repeat(300))
        .unwrap();
    let user = context.chronology()[0].sequence();
    let plan = plan_model_context_compaction_at_consumed_sequence(
        &context,
        100,
        0,
        context.event_sequence_high_water_mark(),
    )
    .unwrap();
    assert!(plan.changes_context());
    assert!(context.retain_pending_peer_message(user).is_err());
    let visible = context.blocks().to_vec();
    context.retain_pending_peer_message(peer).unwrap();
    assert_eq!(context.blocks(), visible);
    assert!(context.peer_message_is_pending(peer));
    assert!(apply_model_context_compaction_plan(context.clone(), &plan, "summary").is_err());
    let summary = ContextBlock::reference_event(
        ContextSourceKind::CompactionSelective,
        "summary",
        "summarized",
    );
    let original = context.clone();
    assert!(
        context
            .compact_execution_ranges(vec![(1..2, summary.clone())])
            .is_err()
    );
    assert!(
        context
            .compact_execution_ranges_into_summary(std::iter::once(1..2).collect(), summary)
            .is_err()
    );
    assert_eq!(context, original);
    assert!(
        context
            .replace_after_compaction(vec![ContextBlock::user_event("user", "replacement")])
            .is_err()
    );
    assert_eq!(context, original);
    assert!(context.release_pending_peer_message(peer));
    assert!(!context.release_pending_peer_message(peer));
    assert!(
        plan_model_context_compaction_at_consumed_sequence(
            &context,
            100,
            0,
            context.event_sequence_high_water_mark()
        )
        .unwrap()
        .changes_context()
    );
}

/// Builds a recoverable execution followed by exact mail, so the older heuristic
/// would consider the execution closed despite a still-live runtime obligation.
fn pending_context() -> (AgentContext, ContextExecutionGroupId) {
    let mut context = AgentContext::new_durable(vec![ContextBlock::user_event(
        "user prompt",
        "delegate and retain the task",
    )])
    .unwrap();
    let group = ContextExecutionGroupId::new("original-outstanding-join").unwrap();
    context
        .append_assistant_event(
            "original assistant",
            "original action ".repeat(100),
            group.clone(),
        )
        .unwrap();
    context
        .append_evidence_event(
            ContextSourceKind::ActionResult,
            "settled sibling",
            "sibling evidence ".repeat(100),
            group.clone(),
            None,
            true,
        )
        .unwrap();
    context
        .append_peer_message_event("child question", "which format?")
        .unwrap();
    (context, group)
}

/// Only existing typed groups can be retained. Repetition preserves a single
/// protection entry; prepared provider contexts keep it through cloning without
/// changing any model-visible block, event sequence or durable metadata.
#[test]
fn pending_execution_group_retention_is_checked_and_not_model_visible() {
    let (mut context, group) = pending_context();
    let original = context.clone();
    let absent = ContextExecutionGroupId::new("absent").unwrap();
    assert!(context.retain_pending_execution_group(&absent).is_err());
    assert_eq!(context, original);
    context.retain_pending_execution_group(&group).unwrap();
    context.retain_pending_execution_group(&group).unwrap();
    assert_eq!(context.blocks(), original.blocks());
    assert_eq!(context.chronology(), original.chronology());
    assert_eq!(context.metadata(), original.metadata());
    let prepared = PreparedModelContext::new(context.clone()).unwrap();
    assert!(prepared.durable().execution_group_is_pending(&group));
    assert!(
        prepared
            .to_agent_context()
            .execution_group_is_pending(&group)
    );
    assert!(context.release_pending_execution_group(&group));
    assert!(!context.release_pending_execution_group(&group));
    assert_eq!(context, original);
}

/// Later peer mail does not make a runtime-owned group eligible for model
/// summarization. Releasing the final obligation restores ordinary eligibility;
/// no consumed-event high-water or source bytes need to be fabricated.
#[test]
fn pending_execution_group_blocks_compaction_plan_until_released() {
    let (mut context, group) = pending_context();
    let boundary = context.event_sequence_high_water_mark();
    assert!(
        plan_model_context_compaction_at_consumed_sequence(&context, 1000, 0, boundary)
            .unwrap()
            .changes_context()
    );
    context.retain_pending_execution_group(&group).unwrap();
    assert!(
        !plan_model_context_compaction_at_consumed_sequence(&context, 1000, 0, boundary)
            .unwrap()
            .changes_context()
    );
    context.release_pending_execution_group(&group);
    assert!(
        plan_model_context_compaction_at_consumed_sequence(&context, 1000, 0, boundary)
            .unwrap()
            .changes_context()
    );
}

/// Protection added after a plan was frozen must still defeat application.
/// Direct range APIs and whole-history replacement likewise cannot discard
/// pending source. Every rejected mutation leaves the canonical context exact.
#[test]
fn pending_execution_group_rejects_stale_plan_and_destructive_mutations() {
    let (mut context, group) = pending_context();
    let plan = plan_model_context_compaction_at_consumed_sequence(
        &context,
        1000,
        0,
        context.event_sequence_high_water_mark(),
    )
    .unwrap();
    assert!(plan.changes_context());
    context.retain_pending_execution_group(&group).unwrap();
    let original = context.clone();
    assert!(apply_model_context_compaction_plan(context.clone(), &plan, "summary").is_err());
    let summary =
        ContextBlock::reference_event(ContextSourceKind::CompactionSelective, "summary", "settled");
    assert!(
        context
            .compact_execution_ranges(vec![(1..3, summary.clone())])
            .is_err()
    );
    assert_eq!(context, original);
    assert!(
        context
            .compact_execution_ranges_into_summary(std::iter::once(1..3).collect(), summary)
            .is_err()
    );
    assert_eq!(context, original);
    assert!(
        context
            .replace_after_compaction(vec![ContextBlock::user_event("user prompt", "replacement")])
            .is_err()
    );
    assert_eq!(context, original);
    assert!(
        context
            .replace_imported_history_prefix_through_sequence(
                context.event_sequence_high_water_mark(),
                Vec::new()
            )
            .is_err()
    );
    assert_eq!(context, original);
}

/// Staged compaction must use the live runtime owner's current protection,
/// even when no chronology changed. A newly retained group defeats a frozen
/// plan, and a released obligation must not be resurrected by the old candidate.
#[test]
fn pending_execution_groups_follow_live_staged_rebase_ownership() {
    let (mut live, group) = pending_context();
    let frozen = live.chronology().to_vec();
    let plan = plan_model_context_compaction_at_consumed_sequence(
        &live,
        1000,
        0,
        live.event_sequence_high_water_mark(),
    )
    .unwrap();
    let mut candidate = live.clone();
    live.retain_pending_execution_group(&group).unwrap();
    candidate.rebase_chronology_suffix(&frozen, &live).unwrap();
    assert!(candidate.execution_group_is_pending(&group));
    assert!(apply_model_context_compaction_plan(candidate.clone(), &plan, "summary").is_err());
    live.release_pending_execution_group(&group);
    candidate.rebase_chronology_suffix(&frozen, &live).unwrap();
    assert!(!candidate.execution_group_is_pending(&group));
    assert!(apply_model_context_compaction_plan(candidate, &plan, "summary").is_ok());
}

/// If staging already removed a now-protected group, rebase must reject that
/// candidate atomically. Newly appended protected groups instead travel with
/// their exact live suffix and remain ineligible for compaction after adoption.
#[test]
fn pending_execution_groups_fence_compacted_candidate_and_follow_new_suffix() {
    let (mut live, group) = pending_context();
    let frozen = live.chronology().to_vec();
    let plan = plan_model_context_compaction_at_consumed_sequence(
        &live,
        1000,
        0,
        live.event_sequence_high_water_mark(),
    )
    .unwrap();
    let (mut candidate, _) =
        apply_model_context_compaction_plan(live.clone(), &plan, "summary").unwrap();
    let original = candidate.clone();
    live.retain_pending_execution_group(&group).unwrap();
    assert!(candidate.rebase_chronology_suffix(&frozen, &live).is_err());
    assert_eq!(candidate, original);
    live.release_pending_execution_group(&group);
    let new_group = ContextExecutionGroupId::new("new-joined-source").unwrap();
    live.append_assistant_event("new assistant", "new action", new_group.clone())
        .unwrap();
    live.append_evidence_event(
        ContextSourceKind::ActionResult,
        "new sibling",
        "settled sibling",
        new_group.clone(),
        None,
        true,
    )
    .unwrap();
    live.append_peer_message_event("later question", "new question")
        .unwrap();
    live.retain_pending_execution_group(&new_group).unwrap();
    assert_eq!(
        candidate.rebase_chronology_suffix(&frozen, &live).unwrap(),
        3
    );
    assert!(candidate.execution_group_is_pending(&new_group));
    assert!(
        candidate
            .chronology()
            .ends_with(&live.chronology()[frozen.len()..])
    );
    let next_plan = plan_model_context_compaction_at_consumed_sequence(
        &candidate,
        1000,
        0,
        candidate.event_sequence_high_water_mark(),
    )
    .unwrap();
    assert!(
        next_plan
            .replacement_blocks()
            .iter()
            .all(|block| block.label != "new assistant" && block.label != "new sibling")
    );
}
