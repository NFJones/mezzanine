//! Model Context tests for compaction behavior.
//!
//! This bounded leaf owns the named behavioral scenarios.

use super::*;

#[test]
/// Verifies explicit bulk compaction prefers older recoverable history before
/// the recent context tail.
///
/// Provider-limit recovery and manual compaction both use this helper after a
/// concrete trigger has fired, which keeps fresh correction signals visible
/// while summarizing older recoverable history.
fn explicit_context_compaction_preserves_recent_recoverable_tail_when_possible() {
    let mut blocks = Vec::new();
    for index in 0..20 {
        blocks.push(ContextBlock {
            source: ContextSourceKind::Transcript,
            label: format!("transcript {index}"),
            content: format!("transcript-{index} {}", "history-word ".repeat(7_000)),
        });
    }

    let (context, report) =
        compact_model_context_for_budget(AgentContext::new(blocks).unwrap(), 80 * 1024).unwrap();

    assert!(report.changed());
    let summary = context
        .blocks
        .iter()
        .find(|block| block.source == ContextSourceKind::Memory)
        .expect("oldest transcript should be present in summary inventory");
    let recent_history = context
        .blocks
        .iter()
        .find(|block| block.label == "transcript 19")
        .expect("recent transcript should remain present");

    assert!(summary.content.contains("[context compacted]"));
    assert!(summary.content.contains("label=transcript 0"));
    assert!(recent_history.content.contains("transcript-19"));
    assert!(!recent_history.content.contains("[context compacted]"));
}

#[test]
/// Verifies explicit compaction keeps current execution evidence and repo guidance
/// exact while folding older unrelated context into a bulk summary.
///
/// Removing generated provider-visible evidence summaries must not make provider-limit
/// compaction drop the newest raw action-result evidence that the next
/// continuation still needs to reference directly.
fn explicit_context_compaction_protects_guidance_and_recent_action_result() {
    let mut blocks = vec![
        ContextBlock {
            source: ContextSourceKind::ProjectGuidance,
            label: "project guidance".to_string(),
            content: "run just test before handoff".to_string(),
        },
        ContextBlock {
            source: ContextSourceKind::ActionResult,
            label: "action result".to_string(),
            content: format!(
                "[action_result a1 shell_command succeeded]\ncommand: rg cache\noutput: fresh evidence large-action-marker {}",
                "large exact evidence ".repeat(2_000)
            ),
        },
    ];
    for index in 0..40 {
        blocks.push(ContextBlock {
            source: ContextSourceKind::Memory,
            label: format!("old memory {index}"),
            content: "old unrelated context ".repeat(20),
        });
    }

    let (context, report) = compact_model_context_for_budget_with_retained_tail_percent(
        AgentContext::new(blocks).unwrap(),
        600,
        10,
    )
    .unwrap();

    assert!(report.changed());
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ProjectGuidance
            && block.content.contains("run just test")
    }));
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult && block.content.contains("fresh evidence")
    }));
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block.content.contains("large-action-marker")
    }));
    assert!(
        context
            .blocks
            .iter()
            .any(|block| block.source == ContextSourceKind::Memory
                && block.content.contains("[context compacted]"))
    );
}

#[test]
/// Verifies explicit context compaction reports the configured retained tail.
///
/// The retained raw suffix is a runtime setting, so compaction summaries must
/// reflect the configured percentage instead of the default value that older
/// builds hard-coded into every summary.
fn explicit_context_compaction_uses_configured_retained_tail_percent() {
    let (context, report) = compact_model_context_for_budget_with_retained_tail_percent(
        AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::Transcript,
            label: "older transcript".to_string(),
            content: "x".repeat(2 * 1024 * 1024),
        }])
        .unwrap(),
        8 * 1024,
        25,
    )
    .unwrap();

    assert!(report.changed());
    let memory_block = context
        .blocks
        .iter()
        .find(|block| block.source == ContextSourceKind::Memory)
        .expect("bulk compaction memory should be present");

    assert!(memory_block.content.contains("retained_tail_percent=25"));
}
