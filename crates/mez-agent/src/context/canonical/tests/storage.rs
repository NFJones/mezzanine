//! Canonical storage projection and atomic mutation rollback regressions.

use super::*;

/// Verifies prepared request construction projects durable context exactly
/// without creating a request-local model-visible suffix.
#[test]
fn prepared_model_context_projects_only_durable_context() {
    let durable = AgentContext::new_durable(vec![
        ContextBlock::stable_instruction(ContextSourceKind::Policy, "policy", "stable"),
        ContextBlock::user_event("user prompt", "do the work"),
    ])
    .unwrap();
    let original = durable.clone();
    let prepared = PreparedModelContext::new(durable).unwrap();
    assert_eq!(prepared.durable(), &original);
    assert_eq!(prepared.len(), 2);
    let ordered = prepared.to_agent_context();
    assert_eq!(ordered.blocks[1].source, ContextSourceKind::UserInstruction);
    assert_eq!(ordered, original);
}

/// Verifies a prepared request can only be created from valid two-phase
/// durable context and retains its exact projection.
#[test]
fn prepared_model_context_requires_valid_durable_context() {
    let durable =
        AgentContext::new_durable(vec![ContextBlock::user_event("user prompt", "do the work")])
            .unwrap();
    let prepared = PreparedModelContext::new(durable.clone()).unwrap();
    assert_eq!(prepared.to_agent_context(), durable);
}

/// Verifies the typed collections are the source of truth for their
/// read-only provider projection and retain stable replacement identity.
#[test]
fn agent_context_projects_typed_stable_and_event_storage_in_order() {
    let mut durable = AgentContext::new_durable(vec![ContextBlock::user_event(
        "user prompt",
        "inspect chronology",
    )])
    .unwrap();
    durable
        .replace_stable_source_slots(
            ContextSourceKind::ProjectGuidance,
            vec![
                StableContextBlock::new(
                    StableContextSlotId::new("project-guidance").unwrap(),
                    StableContextSourceFingerprint::new("a".repeat(64)).unwrap(),
                    ContextBlock::stable_instruction(
                        ContextSourceKind::ProjectGuidance,
                        "active repository instructions",
                        "preserve chronology",
                    ),
                )
                .unwrap(),
            ],
        )
        .unwrap();
    let group = ContextExecutionGroupId::new("execution-1").unwrap();
    durable
        .append_assistant_event("assistant action", "inspect files", group.clone())
        .unwrap();
    durable
        .append_evidence_event(
            ContextSourceKind::ActionResult,
            "action result inspect",
            "files inspected",
            group,
            None,
            true,
        )
        .unwrap();
    let prepared = PreparedModelContext::new(durable.clone()).unwrap();
    let projected = prepared.to_agent_context();
    assert_eq!(projected.stable_slots().len(), 1);
    assert_eq!(projected.chronology().len(), 3);
    assert_eq!(
        projected
            .blocks()
            .iter()
            .map(|block| block.label.as_str())
            .collect::<Vec<_>>(),
        [
            "active repository instructions",
            "user prompt",
            "assistant action",
            "action result inspect"
        ]
    );
    assert_eq!(prepared.durable(), &durable);
}

/// Verifies atomic chronological appends preserve the same projection and
/// sequence order as the existing one-event mutation APIs.
#[test]
fn agent_context_batch_append_matches_sequential_events() {
    let group = ContextExecutionGroupId::new("batch-execution").unwrap();
    let mut sequential = AgentContext::new_durable(vec![ContextBlock::user_event(
        "user prompt",
        "inspect the batch context",
    )])
    .unwrap();
    let mut batched = sequential.clone();
    sequential
        .append_assistant_event("assistant", "inspect files", group.clone())
        .unwrap();
    sequential
        .append_evidence_event(
            ContextSourceKind::ActionResult,
            "action result inspect",
            "files inspected",
            group.clone(),
            None,
            true,
        )
        .unwrap();
    let sequences = batched
        .append_conversation_events([
            ContextConversationAppend::assistant("assistant", "inspect files", group.clone()),
            ContextConversationAppend::evidence(
                ContextSourceKind::ActionResult,
                "action result inspect",
                "files inspected",
                group,
                None,
                true,
            ),
        ])
        .unwrap();
    assert_eq!(sequences.len(), 2);
    assert!(sequences[0] < sequences[1]);
    assert_eq!(batched, sequential);
    assert_eq!(batched.event_sequence_high_water_mark(), sequences[1].get());
}

/// Verifies a later invalid event rolls back an entire append batch,
/// including its allocated sequence range and prior valid entries.
#[test]
fn agent_context_batch_append_is_atomic_when_later_event_is_invalid() {
    let group = ContextExecutionGroupId::new("batch-rollback").unwrap();
    let mut context = AgentContext::new_durable(vec![ContextBlock::user_event(
        "user prompt",
        "keep the original context",
    )])
    .unwrap();
    let original = context.clone();
    let error = context
        .append_conversation_events([
            ContextConversationAppend::assistant("assistant", "valid first event", group),
            ContextConversationAppend {
                block: ContextBlock::user_event("user prompt", "duplicate active prompt"),
                semantic_kind: ContextSemanticKind::UserEvent,
                retention: ContextRetention::Exact,
                execution_group_id: None,
                provider_owner: None,
                recoverable_for_compaction: false,
            },
        ])
        .unwrap_err();
    assert!(error.message().contains("only one active user prompt"));
    assert_eq!(context, original);
}

/// Verifies duplicate active prompts are rejected atomically instead of
/// advancing the event high-water mark or leaving a partial event behind.
#[test]
fn agent_context_rejects_duplicate_active_prompt_atomically() {
    let mut context =
        AgentContext::new_durable(vec![ContextBlock::user_event("user prompt", "first task")])
            .unwrap();
    let original = context.clone();
    let error = context
        .append_user_event("user prompt", "second task")
        .unwrap_err();
    assert!(error.message().contains("only one active user prompt"));
    assert_eq!(context, original);
}

/// Verifies the compatibility insertion boundary is transactional even
/// when its inferred event would violate a whole-context invariant.
#[test]
fn agent_context_rejects_invalid_compatibility_insertion_atomically() {
    let mut context =
        AgentContext::new_durable(vec![ContextBlock::user_event("user prompt", "first task")])
            .unwrap();
    let original = context.clone();
    let error = context
        .insert_typed_block(
            ContextBlock::user_event("user prompt", "second task"),
            ContextSemanticKind::UserEvent,
            ContextRetention::Exact,
            false,
        )
        .unwrap_err();
    assert!(error.message().contains("only one active user prompt"));
    assert_eq!(context, original);
}

/// Verifies evidence cannot commit without a preceding assistant execution
/// in the same ownership group and that the failed append is atomic.
#[test]
fn agent_context_rejects_unowned_evidence_atomically() {
    let mut context = AgentContext::new_durable(vec![ContextBlock::user_event(
        "user prompt",
        "run the check",
    )])
    .unwrap();
    let original = context.clone();
    let error = context
        .append_evidence_event(
            ContextSourceKind::ActionResult,
            "action result check",
            "passed",
            ContextExecutionGroupId::new("missing-assistant").unwrap(),
            None,
            true,
        )
        .unwrap_err();
    assert!(error.message().contains("preceding owning assistant"));
    assert_eq!(context, original);
}

/// Verifies predicate-based cleanup cannot remove an assistant owner while
/// leaving its evidence behind or expose the invalid intermediate state.
#[test]
fn agent_context_rejects_causality_breaking_retention_atomically() {
    let mut context = AgentContext::new_durable(vec![ContextBlock::user_event(
        "user prompt",
        "run the check",
    )])
    .unwrap();
    let group = ContextExecutionGroupId::new("execution-1").unwrap();
    context
        .append_assistant_event("assistant action", "run check", group.clone())
        .unwrap();
    context
        .append_evidence_event(
            ContextSourceKind::ActionResult,
            "action result check",
            "passed",
            group,
            None,
            true,
        )
        .unwrap();
    let original = context.clone();
    let error = context
        .retain_blocks(|block| block.source != ContextSourceKind::TranscriptAssistant)
        .unwrap_err();
    assert!(error.message().contains("preceding owning assistant"));
    assert_eq!(context, original);
}
