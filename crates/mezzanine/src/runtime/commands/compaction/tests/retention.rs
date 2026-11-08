//! Retained-tail policy uses the canonical owner and its cached estimates.
//! Original regression names survive retirement of the role/count selector;
//! completion below is the actual pure controller fence, never inferred text.

use super::*;
use mez_agent::{
    ReplayCompactionPlan, ReplayCompactionPreparation, ReplayConversationId, ReplayEpochContext,
    ReplayTailPolicy,
};

fn source(count: usize, words: usize) -> ReplayEpochContext {
    let mut owner = ReplayEpochContext::new(
        ReplayConversationId::new("retention-policy").unwrap(),
        AgentContext::empty(),
    )
    .unwrap();
    for _ in 0..count {
        owner
            .append_fact("source", "context-word ".repeat(words))
            .unwrap();
    }
    owner
}

fn plan(owner: &ReplayEpochContext, budget: usize, percent: u8) -> ReplayCompactionPlan {
    match owner
        .prepare_compaction(ReplayTailPolicy::new(budget, percent).unwrap())
        .unwrap()
    {
        ReplayCompactionPreparation::Ready(plan) => plan,
        other => panic!("expected eligible original source: {other:?}"),
    }
}

fn retained(owner: &ReplayEpochContext, plan: &ReplayCompactionPlan) -> usize {
    owner.members().len() - plan.summary_input().len()
}

/// Produces actual closed abstract assistant/evidence groups. Native execution
/// and its observed effect custody are qualified by the separate runtime tests.
fn close_group(owner: &mut ReplayEpochContext, name: &str, content: &str) {
    let group = mez_agent::ContextExecutionGroupId::new(name).unwrap();
    let mut appends = Vec::new();
    for _ in 0..2 {
        appends.push(mez_agent::ContextConversationAppend::assistant(
            "answer",
            content,
            group.clone(),
        ));
        appends.push(mez_agent::ContextConversationAppend::evidence(
            ContextSourceKind::ActionResult,
            "evidence",
            content,
            group.clone(),
            None,
            true,
        ));
    }
    let frontier = owner
        .append_execution_events(appends)
        .unwrap()
        .pop()
        .unwrap();
    let original = owner.capture_execution_owner(&group, frontier).unwrap();
    owner.finish_execution_group(&original).unwrap();
}

#[test]
fn runtime_compact_retained_transcript_entries_uses_context_budget_tail() {
    let owner = source(12, 230);
    assert_eq!(retained(&owner, &plan(&owner, 20_000, 10)), 2);
}

/// The canonical reservation includes an exactly fitting newest unit. The
/// oldest whole prefix still advances, including when all source would fit.
#[test]
fn runtime_compact_tail_excludes_equal_estimated_token_cost() {
    let owner = source(2, 1);
    let unit = owner.input_estimates().raw_input_tokens() / 2;
    let selected = plan(&owner, unit, 100);
    assert_eq!(selected.summary_input().len(), 1);
    assert_eq!(retained(&owner, &selected), 1);
}

#[test]
fn runtime_compact_retained_transcript_entries_honors_configured_tail_percent() {
    let owner = source(12, 230);
    assert!(
        retained(&owner, &plan(&owner, 10_000, 20)) > retained(&owner, &plan(&owner, 10_000, 10))
    );
}

#[test]
fn runtime_compact_forced_retained_entries_leaves_summary_input() {
    for count in [12, 1] {
        let owner = source(count, 230);
        let selected = plan(&owner, 20_000, 10);
        assert_eq!(retained(&owner, &selected), if count == 1 { 0 } else { 2 });
        assert_eq!(selected.summary_input()[0], owner.context().chronology()[0]);
    }
}

#[test]
fn runtime_compact_entries_for_summary_excludes_retained_tail() {
    let owner = source(12, 230);
    let selected = plan(&owner, 10_000, 10);
    let count = retained(&owner, &selected);
    assert!(count > 0 && count < 12);
    assert_eq!(
        selected.summary_input(),
        &owner.context().chronology()[..12 - count]
    );
}

#[test]
fn runtime_compact_transcript_tail_preserves_complete_execution_groups() {
    let mut owner = source(0, 0);
    close_group(&mut owner, "one", "original member");
    let group_cost = owner.input_estimates().raw_input_tokens();
    close_group(&mut owner, "two", "original member");
    let selected = plan(&owner, group_cost * 10, 10);
    assert_eq!(selected.summary_input(), &owner.context().chronology()[..4]);
    assert_eq!(retained(&owner, &selected), 4);
}

#[test]
fn runtime_compact_retained_tail_does_not_exempt_oversized_newest_group() {
    let mut owner = source(0, 0);
    close_group(&mut owner, "oversized", &"oversized member ".repeat(20));
    let selected = plan(&owner, 10, 10);
    assert_eq!(selected.summary_input(), owner.context().chronology());
    assert_eq!(retained(&owner, &selected), 0);
}

#[test]
fn runtime_compact_transcript_summary_stops_before_open_execution_group() {
    let mut owner = source(1, 8);
    owner
        .append_execution_events([mez_agent::ContextConversationAppend::assistant(
            "unfinished",
            "pending original group",
            mez_agent::ContextExecutionGroupId::new("open").unwrap(),
        )])
        .unwrap();
    let selected = plan(&owner, 100, 10);
    assert_eq!(selected.summary_input(), &owner.context().chronology()[..1]);
    assert_eq!(retained(&owner, &selected), 1);
}

/// An MCP reset marker has zero model content, while its current catalog stays
/// exact. Progress before that catalog cannot borrow authority to summarize it.
#[test]
fn runtime_compact_transcript_summary_accepts_mcp_epoch_control_group() {
    let original = source(1, 1);
    let mut context = original.into_owned_context();
    context
        .append_reference_event(
            ContextSourceKind::McpCatalogSnapshot,
            mez_agent::MCP_CATALOG_SNAPSHOT_CONTEXT_LABEL,
            "available_servers=0 available_tools=0 unavailable_servers=0",
        )
        .unwrap();
    let owner = ReplayEpochContext::from_owned_context(
        ReplayConversationId::new("retention-policy").unwrap(),
        context,
    )
    .unwrap();
    let selected = plan(&owner, 100, 10);
    assert_eq!(selected.summary_input(), &owner.context().chronology()[..1]);
    assert_eq!(retained(&owner, &selected), 1);
}
