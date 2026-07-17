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
            placement: crate::ContextPlacement::EphemeralTail,
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

    assert_eq!(context.blocks[0].source, ContextSourceKind::Policy);
    assert_eq!(context.blocks[1].source, ContextSourceKind::ProjectGuidance);
    assert_eq!(context.blocks[2].source, ContextSourceKind::ProjectGuidance);
    assert!(
        context.blocks[1]
            .label
            .starts_with("active repository instructions (scope .")
    );
    assert!(
        context.blocks[2]
            .label
            .starts_with("active repository instructions (scope ./src")
    );
    assert!(!context.blocks[1].label.contains("AGENTS.md"));
    assert!(!context.blocks[2].label.contains("AGENTS.md"));
    assert!(context.blocks[2].label.contains("truncated"));
    assert_eq!(context.blocks[3].source, ContextSourceKind::UserInstruction);
}

#[test]
/// Verifies project guidance replacement removes stale instruction blocks.
///
/// Provider continuations refresh stored turn context before each request, so
/// the replacement helper must keep one current project-guidance block instead of
/// accumulating old guidance after file edits or repeated model round trips.
fn project_guidance_context_replaces_existing_guidance_blocks() {
    let context = AgentContext::new(vec![
        ContextBlock {
            source: ContextSourceKind::Policy,
            placement: crate::ContextPlacement::StablePrefix,
            label: "permission policy".to_string(),
            content: "approval_policy=Ask".to_string(),
        },
        ContextBlock {
            source: ContextSourceKind::ProjectGuidance,
            placement: crate::ContextPlacement::StablePrefix,
            label: "project guidance".to_string(),
            content: "stale guidance".to_string(),
        },
        ContextBlock {
            source: ContextSourceKind::UserInstruction,
            placement: crate::ContextPlacement::EphemeralTail,
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
        content: "fresh guidance".to_string(),
    }];

    let context = set_project_guidance_context(context, &files, 2).unwrap();

    let guidance = context
        .blocks
        .iter()
        .filter(|block| block.source == ContextSourceKind::ProjectGuidance)
        .collect::<Vec<_>>();
    assert_eq!(guidance.len(), 1);
    assert!(guidance[0].content.contains("fresh guidance"));
    assert!(
        guidance[0]
            .content
            .contains("If a higher-priority instruction prevents following this file")
    );
    assert_eq!(context.blocks[0].source, ContextSourceKind::Policy);
    assert_eq!(context.blocks[1].source, ContextSourceKind::ProjectGuidance);
    assert_eq!(context.blocks[2].source, ContextSourceKind::UserInstruction);
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
        placement: crate::ContextPlacement::EphemeralTail,
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

    assert_eq!(context.blocks.len(), 2);
    assert_eq!(context.blocks[0].source, ContextSourceKind::ProjectGuidance);
    assert!(
        context.blocks[0]
            .label
            .starts_with("active repository instructions (scope ./src")
    );
    assert!(!context.blocks[0].label.contains("AGENTS.md"));
    assert!(
        context.blocks[0]
            .content
            .contains("Repository instruction contract")
    );
    assert!(
        context.blocks[0]
            .content
            .contains(r#"<repository_instructions scope="./src""#)
    );
    assert!(!context.blocks[0].content.contains("AGENTS.md"));
    assert!(context.blocks[0].content.contains("src guidance"));
    assert!(
        context.blocks[0]
            .content
            .contains("</repository_instructions>")
    );
}

#[test]
/// Verifies active repository instruction text is embedded into the system
/// prompt instead of replayed as a separate user-context block.
///
/// This protects the prompt shape that prevents the model from spending an
/// early action rediscovering repository guidance that was already loaded.
fn project_guidance_is_templated_into_system_prompt() {
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
            placement: crate::ContextPlacement::EphemeralTail,
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
        request.messages[0]
            .content
            .contains("Embedded active repository instruction contents")
    );
    assert!(
        request.messages[0]
            .content
            .contains("run just test before handoff")
    );
    assert!(!request.messages[0].content.contains("AGENTS.md"));
    assert!(
        request
            .messages
            .iter()
            .skip(1)
            .all(|message| message.source != ContextSourceKind::ProjectGuidance)
    );
}

#[test]
/// Verifies idle scheduler context remains available when the active task is
/// about scheduling or parallel work. This keeps useful controller state
/// discoverable for subagent and concurrency tasks without adding it to every
/// unrelated provider turn.
fn scheduler_context_keeps_relevant_idle_state_compact() {
    let context = AgentContext::new(vec![ContextBlock {
        source: ContextSourceKind::UserInstruction,
        placement: crate::ContextPlacement::EphemeralTail,
        label: "user".to_string(),
        content: "spawn subagents for this task".to_string(),
    }])
    .unwrap();
    let scheduler = AgentScheduler::new(2).unwrap();

    let context = append_scheduler_context(context, &scheduler).unwrap();
    let scheduler_context = context
        .blocks
        .iter()
        .find(|block| block.label == "scheduler state")
        .unwrap();
    assert_eq!(
        scheduler_context.content,
        "state=idle\nmax_concurrent_agents=2"
    );
    assert!(!scheduler_context.content.contains("running_turns=none"));
    assert!(!scheduler_context.content.contains("queued_turns=none"));
}

#[test]
/// Verifies idle scheduler context is omitted from ordinary turns.
///
/// Empty scheduler state consumes volatile prompt space without improving the
/// provider's next action unless the user is asking about scheduling,
/// subagents, or concurrency.
fn scheduler_context_omits_unrelated_idle_state() {
    let context = AgentContext::new(vec![ContextBlock {
        source: ContextSourceKind::UserInstruction,
        placement: crate::ContextPlacement::EphemeralTail,
        label: "user".to_string(),
        content: "do the task".to_string(),
    }])
    .unwrap();
    let scheduler = AgentScheduler::new(2).unwrap();

    let context = append_scheduler_context(context, &scheduler).unwrap();

    assert!(
        context
            .blocks
            .iter()
            .all(|block| block.label != "scheduler state")
    );
}

#[test]
/// Verifies scheduler context precedes project and user context while
/// permission policy stays runtime-owned.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn scheduler_context_precedes_project_and_user_context_without_permission_context() {
    let context = AgentContext::new(vec![
        ContextBlock {
            source: ContextSourceKind::ProjectGuidance,
            placement: crate::ContextPlacement::StablePrefix,
            label: "project".to_string(),
            content: "follow style".to_string(),
        },
        ContextBlock {
            source: ContextSourceKind::UserInstruction,
            placement: crate::ContextPlacement::EphemeralTail,
            label: "user".to_string(),
            content: "do the task".to_string(),
        },
    ])
    .unwrap();
    let mut scheduler = AgentScheduler::new(2).unwrap();
    scheduler
        .enqueue(crate::ScheduledWork {
            turn_id: "turn-queued".to_string(),
            agent_id: "agent-queued".to_string(),
            pane_id: Some("%1".to_string()),
            kind: crate::ScheduledWorkKind::ShellCapable,
        })
        .unwrap();

    let context = append_permission_policy_context(context).unwrap();
    let context = append_scheduler_context(context, &scheduler).unwrap();

    assert_eq!(context.blocks[0].label, "scheduler state");
    assert_eq!(context.blocks[1].source, ContextSourceKind::ProjectGuidance);
    assert!(
        context
            .blocks
            .iter()
            .all(|block| block.label != "permission policy")
    );
    assert!(context.blocks[0].content.contains("queued=1"));
    assert!(context.blocks[0].content.contains("agent-queued"));
}
