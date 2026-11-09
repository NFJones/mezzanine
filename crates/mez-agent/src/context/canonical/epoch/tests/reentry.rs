//! Automatic admission follows the exact published head and originally handled
//! closed source. These pure fixtures exercise real planning/publication and
//! checkpoint transfer; no runtime/model receipt is fabricated.

use super::*;

/// Extending pending A around old handled D removes D's eligibility; it does
/// not make independently retained C fresh. Real closure still rearms work.
#[test]
fn automatic_reentry_eligibility_loss_from_protected_arrivals_is_not_fresh() {
    let mut source = owner();
    source.append_fact("selected", "source S").unwrap();
    source.append_fact("retained", "handled source C").unwrap();
    let pending = ContextExecutionGroupId::new("pending spanning A").unwrap();
    source
        .append_execution_events([ContextConversationAppend::assistant(
            "pending",
            "unfinished A",
            pending.clone(),
        )])
        .unwrap();
    source.append_fact("retained", "handled source D").unwrap();
    source
        .publish_compaction(
            selected(&source),
            ReplayHeadPublication::Summary("head".into()),
        )
        .unwrap();
    assert!(!source.automatic_compaction_is_fresh(policy()).unwrap());
    source
        .append_execution_events([ContextConversationAppend::assistant(
            "pending",
            "later unfinished A",
            pending.clone(),
        )])
        .unwrap();
    assert!(!source.automatic_compaction_is_fresh(policy()).unwrap());
    let cold = ReplayEpochContext::from_checkpoint(
        source.conversation_id().clone(),
        source.checkpoint().unwrap(),
        AgentContext::empty(),
    )
    .unwrap();
    assert!(!cold.automatic_compaction_is_fresh(policy()).unwrap());
    finish_group(&mut source, &pending).unwrap();
    assert!(source.automatic_compaction_is_fresh(policy()).unwrap());
}

fn policy() -> ReplayTailPolicy {
    ReplayTailPolicy::new(1_000, 100).unwrap()
}

fn selected(owner: &ReplayEpochContext) -> ReplayCompactionPlan {
    match owner.prepare_compaction(policy()).unwrap() {
        ReplayCompactionPreparation::Ready(plan) => plan,
        other => panic!("closed fixture source must be selectable: {other:?}"),
    }
}

fn retained() -> ReplayEpochContext {
    let mut source = owner();
    source.append_fact("first", "equal source").unwrap();
    source.append_fact("second", "equal source").unwrap();
    source.append_fact("third", "equal source").unwrap();
    source
        .publish_compaction(
            selected(&source),
            ReplayHeadPublication::Summary("head".into()),
        )
        .unwrap();
    source
}

#[test]
fn automatic_reentry_retains_manual_selection_and_relevant_configuration_identity() {
    let mut source = retained();
    assert!(!source.automatic_compaction_is_fresh(policy()).unwrap());
    assert!(
        matches!(
            source.prepare_compaction(policy()).unwrap(),
            ReplayCompactionPreparation::Ready(_)
        ),
        "explicit manual still uses the same eligible prefix"
    );
    assert!(
        source
            .automatic_compaction_is_fresh(ReplayTailPolicy::new(1_001, 100).unwrap())
            .unwrap()
    );
    assert!(
        source
            .automatic_compaction_is_fresh(ReplayTailPolicy::new(1_000, 99).unwrap())
            .unwrap()
    );
    let projection = |model| {
        ReplayProviderProjection::new(
            crate::ProviderApiCompatibility::OpenAiResponses,
            "provider-a",
            model,
        )
        .unwrap()
    };
    source
        .select_provider_projection(projection("model-a"))
        .unwrap();
    assert!(source.automatic_compaction_is_fresh(policy()).unwrap());
    source
        .publish_compaction(
            selected(&source),
            ReplayHeadPublication::Summary("second head".into()),
        )
        .unwrap();
    assert!(!source.automatic_compaction_is_fresh(policy()).unwrap());
    source
        .select_provider_projection(projection("model-b"))
        .unwrap();
    assert!(source.automatic_compaction_is_fresh(policy()).unwrap());
    source
        .select_provider_projection(projection("model-a"))
        .unwrap();
    assert!(!source.automatic_compaction_is_fresh(policy()).unwrap());
    let conversation = source.conversation_id().clone();
    let continued =
        ReplayEpochContext::from_owned_context(conversation, source.into_owned_context()).unwrap();
    assert!(!continued.automatic_compaction_is_fresh(policy()).unwrap());
}

#[test]
fn automatic_reentry_does_not_consume_late_arrivals_or_newly_closed_instructions() {
    let mut source = owner();
    source.append_fact("old first", "equal source").unwrap();
    source.append_fact("old tail", "equal source").unwrap();
    let frozen = selected(&source);
    source.append_fact("post-freeze", "equal source").unwrap();
    source
        .publish_compaction(frozen, ReplayHeadPublication::Summary("head".into()))
        .unwrap();
    assert!(source.automatic_compaction_is_fresh(policy()).unwrap());
    let restored = ReplayEpochContext::from_checkpoint(
        source.conversation_id().clone(),
        source.checkpoint().unwrap(),
        AgentContext::empty(),
    )
    .unwrap();
    assert!(restored.automatic_compaction_is_fresh(policy()).unwrap());

    let mut source = owner();
    source.append_fact("old first", "source").unwrap();
    source.append_fact("old tail", "source").unwrap();
    let task = source.begin_instruction_task().unwrap();
    source
        .append_instruction(
            &task,
            ContextBlock::user_event("task", "protected original task"),
        )
        .unwrap();
    source
        .publish_compaction(
            selected(&source),
            ReplayHeadPublication::Summary("head".into()),
        )
        .unwrap();
    source
        .append_instruction(
            &task,
            ContextBlock::user_event("steering", "new protected steering"),
        )
        .unwrap();
    assert!(!source.automatic_compaction_is_fresh(policy()).unwrap());
    source.complete_instruction_task(&task).unwrap();
    assert!(source.automatic_compaction_is_fresh(policy()).unwrap());
}

#[test]
fn automatic_reentry_checkpoint_version_four_preserves_head_binding_and_legacy_unknowns() {
    let source = retained();
    let checkpoint = source.checkpoint().unwrap();
    let value = serde_json::to_value(&checkpoint).unwrap();
    assert_eq!(value["version"], 5);
    assert!(value["compaction_reentry"].is_object());
    let mapped = checkpoint
        .try_map_payloads(|_, body| Ok::<_, ()>(body))
        .unwrap();
    let restored = ReplayEpochContext::from_checkpoint(
        source.conversation_id().clone(),
        mapped,
        AgentContext::empty(),
    )
    .unwrap();
    assert!(!restored.automatic_compaction_is_fresh(policy()).unwrap());
    for version in [1, 2, 3] {
        let mut legacy = value.clone();
        legacy["version"] = version.into();
        legacy.as_object_mut().unwrap().remove("compaction_reentry");
        if version < 3 {
            legacy
                .as_object_mut()
                .unwrap()
                .remove("execution_intervals");
        }
        if version < 2 {
            legacy
                .as_object_mut()
                .unwrap()
                .remove("retired_mcp_retrievals");
        }
        let restored = ReplayEpochContext::from_checkpoint(
            source.conversation_id().clone(),
            serde_json::from_value(legacy).unwrap(),
            AgentContext::empty(),
        )
        .unwrap();
        assert_eq!(restored.head(), source.head());
        assert_eq!(
            restored.context().chronology(),
            source.context().chronology()
        );
        assert!(
            restored.automatic_compaction_is_fresh(policy()).unwrap(),
            "legacy data cannot manufacture handled-source facts"
        );
    }
    // Genuine v4 data omits per-occurrence evidence and re-encodes exactly.
    let mut legacy = value.clone();
    legacy["version"] = 4.into();
    legacy["compaction_reentry"]["input"]
        .as_object_mut()
        .unwrap()
        .remove("handled_members");
    let decoded: ReplayEpochCheckpoint = serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(serde_json::to_value(&decoded).unwrap(), legacy);
    let restored = ReplayEpochContext::from_checkpoint(
        source.conversation_id().clone(),
        decoded,
        AgentContext::empty(),
    )
    .unwrap();
    assert!(!restored.automatic_compaction_is_fresh(policy()).unwrap());
    for (field, replacement) in [
        (
            "head",
            serde_json::json!(String::from(ContextBlockOccurrenceId::allocate())),
        ),
        ("revision", serde_json::json!(2)),
    ] {
        let mut corrupt = value.clone();
        corrupt["compaction_reentry"][field] = replacement;
        assert!(
            ReplayEpochContext::from_checkpoint(
                source.conversation_id().clone(),
                serde_json::from_value(corrupt).unwrap(),
                AgentContext::empty()
            )
            .is_err()
        );
    }
    let mut wrong_version = value;
    wrong_version["version"] = 4.into();
    assert!(
        ReplayEpochContext::from_checkpoint(
            source.conversation_id().clone(),
            serde_json::from_value(wrong_version).unwrap(),
            AgentContext::empty()
        )
        .is_err()
    );
}

#[test]
fn automatic_reentry_source_revision_preserves_occurrence_but_rearms_actual_changed_bytes() {
    let source = retained();
    let raw = source.context().chronology()[1].record().occurrence_id();
    let revised = source
        .checkpoint()
        .unwrap()
        .revise_raw_member(raw, "edited source".into())
        .unwrap();
    let restored = ReplayEpochContext::from_checkpoint(
        source.conversation_id().clone(),
        revised,
        AgentContext::empty(),
    )
    .unwrap();
    assert_eq!(restored.head(), source.head());
    assert_eq!(
        restored.context().chronology()[1].record().occurrence_id(),
        raw
    );
    assert!(restored.automatic_compaction_is_fresh(policy()).unwrap());
}
