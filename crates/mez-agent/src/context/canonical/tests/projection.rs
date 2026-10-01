//! Projection, cache policy, and accounting regressions for canonical context.

use super::*;

/// Verifies cloned request message collections retain shared immutable
/// transcript storage until request-local mutation requires detaching it.
#[test]
fn model_messages_clone_shares_storage_until_copy_on_write_mutation() {
    let original = ModelMessages::from(vec![ModelMessage {
        role: ModelMessageRole::User,
        source: ContextSourceKind::UserInstruction,
        placement: crate::ContextPlacement::ConversationAppend,
        content: "inspect the workspace".to_string(),
    }]);
    let mut continuation = original.clone();
    assert!(original.shares_storage_with(&continuation));
    continuation.push(ModelMessage {
        role: ModelMessageRole::Context,
        source: ContextSourceKind::ActionResult,
        placement: crate::ContextPlacement::ConversationAppend,
        content: "workspace inspected".to_string(),
    });
    assert!(!original.shares_storage_with(&continuation));
    assert_eq!(original.len(), 1);
    assert_eq!(continuation.len(), 2);
    assert_eq!(original[0].content, "inspect the workspace");
    assert_eq!(continuation[1].content, "workspace inspected");
}

/// Verifies context blocks expose only stable-prefix or append-only cache
/// metadata without changing their source, label, or content shape.
#[test]
fn context_block_cache_metadata_classifies_two_phase_sources() {
    let project = ContextBlock {
        source: ContextSourceKind::ProjectGuidance,
        placement: crate::ContextPlacement::StablePrefix,
        label: "project guidance".to_string(),
        content: "follow repo guidance".to_string(),
    };
    let scheduler = ContextBlock {
        source: ContextSourceKind::RuntimeHint,
        placement: crate::ContextPlacement::ConversationAppend,
        label: "scheduler state".to_string(),
        content: "state=idle".to_string(),
    };
    let action = ContextBlock {
        source: ContextSourceKind::ActionResult,
        placement: crate::ContextPlacement::ConversationAppend,
        label: "action result".to_string(),
        content: "command output".to_string(),
    };
    let transcript_tool = ContextBlock {
        source: ContextSourceKind::TranscriptTool,
        placement: crate::ContextPlacement::ConversationAppend,
        label: "historical tool result".to_string(),
        content: "prior command output".to_string(),
    };
    let committed_evidence = ContextBlock {
        source: ContextSourceKind::CommittedEvidence,
        placement: crate::ContextPlacement::ConversationAppend,
        label: "committed evidence".to_string(),
        content: "compact prior action evidence".to_string(),
    };
    let pane_identity = ContextBlock {
        source: ContextSourceKind::Configuration,
        placement: crate::ContextPlacement::ConversationAppend,
        label: "pane identity".to_string(),
        content: "pane_id=%1 window_name=0".to_string(),
    };
    assert_eq!(project.placement, crate::ContextPlacement::StablePrefix);
    assert_eq!(project.stability(), ContextStability::Static);
    assert_eq!(project.cache_policy(), ContextCachePolicy::Eligible);
    assert!(project.stable_prefix_eligible());
    assert_eq!(
        scheduler.placement,
        crate::ContextPlacement::ConversationAppend
    );
    assert_eq!(scheduler.stability(), ContextStability::SessionStable);
    assert_eq!(scheduler.cache_policy(), ContextCachePolicy::Eligible);
    assert!(scheduler.stable_prefix_eligible());
    assert_eq!(transcript_tool.stability(), ContextStability::SessionStable);
    assert_eq!(transcript_tool.cache_policy(), ContextCachePolicy::Eligible);
    assert!(transcript_tool.stable_prefix_eligible());
    assert_eq!(
        committed_evidence.stability(),
        ContextStability::SessionStable
    );
    assert_eq!(
        committed_evidence.cache_policy(),
        ContextCachePolicy::Eligible
    );
    assert!(committed_evidence.stable_prefix_eligible());
    assert!(committed_evidence.recoverable_for_compaction());
    assert_eq!(pane_identity.stability(), ContextStability::SessionStable);
    assert_eq!(pane_identity.cache_policy(), ContextCachePolicy::Eligible);
    assert!(pane_identity.stable_prefix_eligible());
    assert!(action.recoverable_for_compaction());
}

/// Verifies the narrow block constructors assign the canonical semantic
/// and retention contracts without conflating those contracts with cache
/// placement or provider transport role.
#[test]
fn context_block_constructors_expose_semantic_and_retention_contracts() {
    let stable = ContextBlock::stable_instruction(
        ContextSourceKind::Policy,
        "stable policy",
        "invariant=true",
    );
    let skill = ContextBlock::task_prelude(
        ContextSourceKind::SkillInstruction,
        "active skill",
        "follow the workflow",
    );
    let user = ContextBlock::user_event("user prompt", "perform the task");
    let assistant = ContextBlock::assistant_event("assistant action", "run tests");
    let evidence = ContextBlock::evidence_event(
        ContextSourceKind::ActionResult,
        "action result action-1",
        "tests passed",
    );
    let reference = ContextBlock::reference_event(
        ContextSourceKind::LocalMessage,
        "local message",
        "agent-%2: avoid file.rs",
    );
    let runtime = ContextBlock::reference_event(
        ContextSourceKind::RuntimeHint,
        "runtime state",
        "cwd=/workspace",
    );
    assert_eq!(
        stable.semantic_kind(),
        ContextSemanticKind::AmbientInstruction
    );
    assert_eq!(stable.retention(), ContextRetention::Exact);
    assert_eq!(skill.semantic_kind(), ContextSemanticKind::TaskPrelude);
    assert_eq!(skill.retention(), ContextRetention::Exact);
    assert_eq!(user.semantic_kind(), ContextSemanticKind::UserEvent);
    assert_eq!(user.retention(), ContextRetention::Exact);
    assert_eq!(
        assistant.semantic_kind(),
        ContextSemanticKind::AssistantEvent
    );
    assert_eq!(assistant.retention(), ContextRetention::ExecutionGroup);
    assert_eq!(evidence.semantic_kind(), ContextSemanticKind::EvidenceEvent);
    assert_eq!(evidence.retention(), ContextRetention::ExecutionGroup);
    assert_eq!(
        reference.semantic_kind(),
        ContextSemanticKind::ReferenceEvent
    );
    assert_eq!(reference.retention(), ContextRetention::Exact);
    assert_eq!(runtime.semantic_kind(), ContextSemanticKind::ReferenceEvent);
    assert_eq!(runtime.retention(), ContextRetention::Exact);
}

/// Verifies accounting metadata follows the typed block projection without
/// changing rendered input and refreshes after a replacement of stable text.
#[test]
fn context_block_accounting_is_not_model_visible_and_rebuilds_with_content() {
    let stable = ContextBlock::stable_instruction(ContextSourceKind::Policy, "policy", "small");
    let mut context = AgentContext::new_durable(vec![stable.clone()]).unwrap();
    let original = context
        .metadata_for_block(0)
        .unwrap()
        .estimated_input_tokens();
    assert_eq!(
        original,
        crate::provider_text_input_token_estimate(&format!(
            "{}{}",
            super::model_context_block_header(&stable),
            stable.content
        ))
    );
    let original_messages = context.blocks().to_vec();
    assert_eq!(original_messages, vec![stable]);
    context
        .append_user_event("user prompt", "new task")
        .unwrap();
    assert_eq!(
        context
            .metadata_for_block(0)
            .unwrap()
            .estimated_input_tokens(),
        original
    );
    let user = &context.blocks()[1];
    assert_eq!(
        context
            .metadata_for_block(1)
            .unwrap()
            .estimated_input_tokens(),
        crate::provider_text_input_token_estimate(&format!(
            "{}{}",
            super::model_context_block_header(user),
            user.content
        ))
    );
    assert!(
        !context.blocks()[0]
            .content
            .contains("estimated_input_tokens")
    );
    assert!(
        !context.blocks()[1]
            .content
            .contains("estimated_input_tokens")
    );
    context
        .replace_stable_slots(vec![
            StableContextBlock::new(
                StableContextSlotId::new("policy").unwrap(),
                StableContextSourceFingerprint::new("a".repeat(64)).unwrap(),
                ContextBlock::stable_instruction(
                    ContextSourceKind::Policy,
                    "policy",
                    "a much longer replacement policy",
                ),
            )
            .unwrap(),
        ])
        .unwrap();
    assert!(
        context
            .metadata_for_block(0)
            .unwrap()
            .estimated_input_tokens()
            > original
    );
    assert_eq!(context.blocks()[1].content, "new task");
}
