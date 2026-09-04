//! Model Context tests for guidance behavior.
//!
//! This bounded leaf owns the named behavioral scenarios.

use super::*;

#[test]
/// Verifies project guidance context is inserted before user prompt.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn project_guidance_context_is_inserted_before_user_prompt() {
    let context = AgentContext::new(vec![
        ContextBlock {
            source: ContextSourceKind::Policy,
            placement: crate::ContextPlacement::StablePrefix,
            label: "policy".to_string(),
            content: "stay safe".to_string(),
        },
        ContextBlock {
            source: ContextSourceKind::UserInstruction,
            placement: crate::ContextPlacement::ConversationAppend,
            label: "user".to_string(),
            content: "change the code".to_string(),
        },
    ])
    .unwrap();
    let files = vec![
        DiscoveredInstructionFile {
            path: "./AGENTS.md".to_string(),
            scope_root: ".".to_string(),
            bytes: 10,
            truncated: false,
            content: "root guidance".to_string(),
        },
        DiscoveredInstructionFile {
            path: "./src/AGENTS.md".to_string(),
            scope_root: "./src".to_string(),
            bytes: 20,
            truncated: true,
            content: "src guidance".to_string(),
        },
    ];

    let context = append_project_guidance_context(context, &files, 2).unwrap();

    assert_eq!(context.blocks()[0].source, ContextSourceKind::Policy);
    assert_eq!(
        context.blocks()[1].source,
        ContextSourceKind::ProjectGuidance
    );
    assert_eq!(context.blocks()[1].label, "active repository instructions");
    assert!(!context.blocks()[1].label.contains("AGENTS.md"));
    assert!(
        context.blocks()[1]
            .content
            .contains(r#"<repository_instructions scope=".""#)
    );
    assert!(
        context.blocks()[1]
            .content
            .contains(r#"<repository_instructions scope="./src""#)
    );
    assert!(context.blocks()[1].content.contains("truncated=\"true\""));
    assert_eq!(
        context.blocks()[2].source,
        ContextSourceKind::UserInstruction
    );
}

#[test]
/// Verifies project guidance follows earlier durable environment evidence while
/// remaining a prelude before the active user prompt.
///
/// Runtime prompt construction discovers repository instructions after pane and
/// environment state. The snapshot must keep that causal ordering without
/// appearing after the active user event.
fn project_guidance_context_follows_task_environment_before_user_prompt() {
    let context = AgentContext::new(vec![
        ContextBlock {
            source: ContextSourceKind::Configuration,
            placement: crate::ContextPlacement::ConversationAppend,
            label: "environment signature".to_string(),
            content: "os=linux".to_string(),
        },
        ContextBlock {
            source: ContextSourceKind::UserInstruction,
            placement: crate::ContextPlacement::ConversationAppend,
            label: "user".to_string(),
            content: "change the code".to_string(),
        },
    ])
    .unwrap();
    let files = vec![DiscoveredInstructionFile {
        path: "./AGENTS.md".to_string(),
        scope_root: ".".to_string(),
        bytes: 13,
        truncated: false,
        content: "run all tests".to_string(),
    }];

    let context = append_project_guidance_context(context, &files, 1).unwrap();

    assert_eq!(context.blocks()[0].label, "environment signature");
    assert_eq!(
        context.blocks()[1].source,
        ContextSourceKind::ProjectGuidance
    );
    assert_eq!(
        context.blocks()[2].source,
        ContextSourceKind::UserInstruction
    );
    context.validate_placement_order().unwrap();
}

#[test]
/// Verifies an updated project-guidance snapshot is appended before the active
/// user prompt without rewriting the prior durable snapshot.
///
/// Each discovery is durable evidence of the instructions governing a prompt.
/// A changed snapshot must therefore retain the prior bytes and append its
/// successor at the deterministic prompt boundary.
fn project_guidance_context_change_retains_prior_snapshot() {
    let context = AgentContext::new(vec![
        ContextBlock {
            source: ContextSourceKind::Policy,
            placement: crate::ContextPlacement::StablePrefix,
            label: "permission policy".to_string(),
            content: "approval_policy=Ask".to_string(),
        },
        ContextBlock {
            source: ContextSourceKind::UserInstruction,
            placement: crate::ContextPlacement::ConversationAppend,
            label: "user".to_string(),
            content: "do the task".to_string(),
        },
    ])
    .unwrap();
    let files = vec![DiscoveredInstructionFile {
        path: "./AGENTS.md".to_string(),
        scope_root: ".".to_string(),
        bytes: 15,
        truncated: false,
        content: "first guidance".to_string(),
    }];

    let first = set_project_guidance_context(context, &files, 2).unwrap();
    let updated_files = vec![DiscoveredInstructionFile {
        path: "./AGENTS.md".to_string(),
        scope_root: ".".to_string(),
        bytes: 14,
        truncated: false,
        content: "fresh guidance".to_string(),
    }];
    let context = set_project_guidance_context(first, &updated_files, 2).unwrap();

    let guidance = context
        .blocks()
        .iter()
        .filter(|block| block.source == ContextSourceKind::ProjectGuidance)
        .collect::<Vec<_>>();
    assert_eq!(guidance.len(), 2);
    assert!(guidance[0].content.contains("first guidance"));
    assert!(guidance[1].content.contains("fresh guidance"));
    assert!(
        guidance[1]
            .content
            .contains("If a higher-priority instruction prevents following this file")
    );
    assert_eq!(context.blocks()[0].source, ContextSourceKind::Policy);
    assert_eq!(
        context.blocks()[1].source,
        ContextSourceKind::ProjectGuidance
    );
    assert_eq!(
        context.blocks()[2].source,
        ContextSourceKind::ProjectGuidance
    );
    assert_eq!(
        context.blocks()[3].source,
        ContextSourceKind::UserInstruction
    );
}

#[test]
/// Verifies repeated discovery of byte-identical repository instructions is an
/// exact chronological no-op.
///
/// Provider continuations may rediscover guidance before every request. An
/// unchanged snapshot must not duplicate a durable prelude or alter its
/// sequence.
fn project_guidance_context_noop_refresh_preserves_snapshot() {
    let files = vec![DiscoveredInstructionFile {
        path: "./AGENTS.md".to_string(),
        scope_root: ".".to_string(),
        bytes: 15,
        truncated: false,
        content: "stable guidance".to_string(),
    }];
    let original = set_project_guidance_context(
        AgentContext::new(vec![ContextBlock::user_event("user", "do the task")]).unwrap(),
        &files,
        2,
    )
    .unwrap();
    let refreshed = set_project_guidance_context(original.clone(), &files, 2).unwrap();

    assert_eq!(refreshed, original);
    assert_eq!(
        refreshed
            .chronology()
            .iter()
            .filter(|event| event.block().source == ContextSourceKind::ProjectGuidance)
            .count(),
        1
    );
}

#[test]
/// Verifies a real guidance change appends a successor snapshot before the
/// active user prompt while retaining the original snapshot bytes.
fn project_guidance_context_change_appends_successor_snapshot() {
    let before_files = vec![DiscoveredInstructionFile {
        path: "./AGENTS.md".to_string(),
        scope_root: ".".to_string(),
        bytes: 14,
        truncated: false,
        content: "old guidance".to_string(),
    }];
    let after_files = vec![DiscoveredInstructionFile {
        path: "./AGENTS.md".to_string(),
        scope_root: ".".to_string(),
        bytes: 14,
        truncated: false,
        content: "new guidance".to_string(),
    }];
    let base = AgentContext::new(vec![
        ContextBlock::stable_instruction(ContextSourceKind::Policy, "policy", "stay safe"),
        ContextBlock::user_event("user", "do the task"),
    ])
    .unwrap();
    let before = set_project_guidance_context(base, &before_files, 2).unwrap();

    let after = set_project_guidance_context(before, &after_files, 2).unwrap();

    assert_eq!(after.blocks()[0].label, "policy");
    assert_eq!(after.blocks()[1].label, "active repository instructions");
    assert!(after.blocks()[1].content.contains("old guidance"));
    assert_eq!(after.blocks()[2].label, "active repository instructions");
    assert!(after.blocks()[2].content.contains("new guidance"));
    assert_eq!(after.blocks()[3].label, "user");
}

#[test]
/// Verifies project guidance context respects file limit and skips empty content.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn project_guidance_context_respects_file_limit_and_skips_empty_content() {
    let context = AgentContext::new(vec![ContextBlock {
        source: ContextSourceKind::UserInstruction,
        placement: crate::ContextPlacement::ConversationAppend,
        label: "user".to_string(),
        content: "do the task".to_string(),
    }])
    .unwrap();
    let files = vec![
        DiscoveredInstructionFile {
            path: "./AGENTS.md".to_string(),
            scope_root: ".".to_string(),
            bytes: 0,
            truncated: false,
            content: String::new(),
        },
        DiscoveredInstructionFile {
            path: "./src/AGENTS.md".to_string(),
            scope_root: "./src".to_string(),
            bytes: 12,
            truncated: false,
            content: "src guidance".to_string(),
        },
    ];

    let context = append_project_guidance_context(context, &files, 2).unwrap();

    assert_eq!(context.blocks().len(), 2);
    assert_eq!(
        context.blocks()[0].source,
        ContextSourceKind::ProjectGuidance
    );
    assert_eq!(context.blocks()[0].label, "active repository instructions");
    assert!(!context.blocks()[0].label.contains("AGENTS.md"));
    assert!(
        context.blocks()[0]
            .content
            .contains("Repository instruction contract")
    );
    assert!(
        context.blocks()[0]
            .content
            .contains(r#"<repository_instructions scope="./src""#)
    );
    assert!(!context.blocks()[0].content.contains("AGENTS.md"));
    assert!(context.blocks()[0].content.contains("src guidance"));
    assert!(
        context.blocks()[0]
            .content
            .contains("</repository_instructions>")
    );
}

#[test]
/// Verifies active repository instruction text is a durable neutral prelude
/// rather than mutable system-prefix content.
///
/// This protects the two-phase prompt shape: the fixed system contract stays
/// immutable while the discovered repository snapshot remains chronological.
fn project_guidance_is_rendered_as_durable_context_prelude() {
    let files = vec![DiscoveredInstructionFile {
        path: "./AGENTS.md".to_string(),
        scope_root: ".".to_string(),
        bytes: 24,
        truncated: false,
        content: "run just test before handoff".to_string(),
    }];
    let context = append_project_guidance_context(
        AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            placement: crate::ContextPlacement::ConversationAppend,
            label: "user".to_string(),
            content: "fix the bug".to_string(),
        }])
        .unwrap(),
        &files,
        2,
    )
    .unwrap();
    let request = assemble_test_model_request(&context);

    assert_eq!(request.messages[0].role, ModelMessageRole::System);
    assert!(
        !request.messages[0]
            .content
            .contains("run just test before handoff")
    );
    assert!(!request.messages[0].content.contains("AGENTS.md"));
    assert!(request.messages.iter().skip(1).any(|message| {
        message.source == ContextSourceKind::ProjectGuidance
            && message.role == ModelMessageRole::Context
            && message.placement == crate::ContextPlacement::ConversationAppend
            && message.content.contains("run just test before handoff")
    }));
}
