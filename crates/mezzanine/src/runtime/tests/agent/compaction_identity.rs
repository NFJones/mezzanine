//! Durable epoch replay feeds shared summary classification and planning.
//!
//! Tests obtain summary blocks from the actual persisted product epoch renderer,
//! rather than constructing a label that consumers already recognize. Exact
//! steering separates the restored summary from later consumed work; it must
//! remain anchored while the planner progresses to that work.

use super::*;
use mez_agent::AgentContext;

/// A persisted prefix summary with its canonical memory UUID must retain
/// summary semantics after replay. The following exact steering event makes
/// that summary a complete segment of its own. A second compaction must select
/// later raw work, preserve the restored summary and exact barrier, and leave
/// the summary-only context unchanged when no later work exists.
#[test]
fn runtime_persisted_uuid_summary_skips_only_summary_segment() {
    let conversation = "persisted-summary-identity";
    let (service, _store) = persisted_prefix_fixture(conversation);
    let history = service.runtime_agent_history_epoch_context("%1").unwrap();
    assert_eq!(history.blocks.len(), 1);
    let prefix = history.blocks[0].clone();
    assert_eq!(
        prefix.label,
        format!(
            "memory {} (conversation)",
            mez_agent::memory::canonical_memory_uuid(&format!("compact-{conversation}"))
        )
    );
    assert!(
        mez_agent::context_block_is_compaction_summary(&prefix),
        "persisted replay lost summary identity: {prefix:?}"
    );
    let barrier = ContextBlock::user_event("steering", "preserve this instruction exactly");
    let context = AgentContext::new_durable(vec![
        prefix.clone(),
        barrier.clone(),
        ContextBlock::assistant_event("later decision", "later decision ".repeat(300)),
        ContextBlock::evidence_event(
            ContextSourceKind::ActionResult,
            "later result",
            "later result ".repeat(300),
        ),
    ])
    .unwrap();
    let plan = mez_agent::plan_model_context_compaction_at_consumed_sequence(
        &context,
        2_000,
        1,
        context.event_sequence_high_water_mark(),
    )
    .unwrap();
    assert_eq!(plan.replacement_blocks().len(), 2);
    assert!(
        plan.replacement_blocks()
            .iter()
            .all(|block| block.content.contains("later"))
    );
    let (compacted, _) =
        mez_agent::apply_model_context_compaction_plan(context, &plan, "later work summarized")
            .unwrap();
    assert_eq!(compacted.chronology()[0].block(), &prefix);
    assert_eq!(compacted.chronology()[1].block(), &barrier);
    let only_summary = AgentContext::new_durable(vec![prefix, barrier]).unwrap();
    let unchanged = mez_agent::plan_model_context_compaction_at_consumed_sequence(
        &only_summary,
        2_000,
        1,
        only_summary.event_sequence_high_water_mark(),
    )
    .unwrap();
    assert!(!unchanged.changes_context());
}

/// Reads actual persisted replay repeatedly, then changes only an ordinary
/// UUID memory or publishes different summary content. Summary presence and
/// appends must not invent new compaction; real summary-content changes must
/// be recognized, with provider/model switch diagnostics retaining precedence.
#[test]
fn runtime_replayed_uuid_summary_continuity_preserves_transition_meaning() {
    use mez_agent::{
        ContextContinuityBreakReason, context_continuity_diagnostics, context_continuity_snapshot,
    };

    let conversation = "persisted-summary-continuity";
    let (service, store) = persisted_prefix_fixture(conversation);
    let original = AgentContext::new_durable(
        service
            .runtime_agent_history_epoch_context("%1")
            .unwrap()
            .blocks,
    )
    .unwrap();
    let previous = context_continuity_snapshot(&original, "openai", "test", "turn");
    assert_eq!(previous.compaction_summary_sha256.len(), 1);
    let replayed = AgentContext::new_durable(
        service
            .runtime_agent_history_epoch_context("%1")
            .unwrap()
            .blocks,
    )
    .unwrap();
    assert_eq!(
        context_continuity_snapshot(&replayed, "openai", "test", "turn").compaction_summary_sha256,
        previous.compaction_summary_sha256
    );
    assert_eq!(
        context_continuity_diagnostics(&replayed, "openai", "test", "turn", Some(&previous))
            .break_reason,
        ContextContinuityBreakReason::AppendOnly
    );
    let mut appended = replayed.clone();
    appended
        .append_reference_event(
            ContextSourceKind::RuntimeHint,
            "later state",
            "later exact state",
        )
        .unwrap();
    assert_eq!(
        context_continuity_diagnostics(&appended, "openai", "test", "turn", Some(&previous))
            .break_reason,
        ContextContinuityBreakReason::AppendOnly
    );
    let label = format!(
        "memory {} (conversation)",
        mez_agent::memory::canonical_memory_uuid("ordinary-continuity-note")
    );
    let mut notes = replayed.blocks().to_vec();
    notes.push(ContextBlock::reference_event(
        ContextSourceKind::Memory,
        label,
        "ordinary original note",
    ));
    let note_context = AgentContext::new_durable(notes.clone()).unwrap();
    let note_previous = context_continuity_snapshot(&note_context, "openai", "test", "turn");
    assert_eq!(
        note_previous.compaction_summary_sha256,
        previous.compaction_summary_sha256
    );
    notes.last_mut().unwrap().content = "ordinary changed note".into();
    let changed_note = AgentContext::new_durable(notes).unwrap();
    assert_eq!(
        context_continuity_diagnostics(
            &changed_note,
            "openai",
            "test",
            "turn",
            Some(&note_previous)
        )
        .break_reason,
        ContextContinuityBreakReason::UnexpectedRewrite
    );
    store
        .save_compaction_epoch(conversation, 1, "new model-authored durable summary")
        .unwrap();
    let changed = AgentContext::new_durable(
        service
            .runtime_agent_history_epoch_context("%1")
            .unwrap()
            .blocks,
    )
    .unwrap();
    assert_eq!(
        context_continuity_diagnostics(&changed, "openai", "test", "turn", Some(&previous))
            .break_reason,
        ContextContinuityBreakReason::Compaction
    );
    assert_eq!(
        context_continuity_diagnostics(&changed, "deepseek", "test", "turn", Some(&previous))
            .break_reason,
        ContextContinuityBreakReason::ProviderSwitch
    );
    assert_eq!(
        context_continuity_diagnostics(&changed, "openai", "other-model", "turn", Some(&previous))
            .break_reason,
        ContextContinuityBreakReason::ModelSwitch
    );
}

/// Publishes a real one-row prefix epoch and binds its product replay owner.
/// All paths, content and conversation identifiers are generated test fixtures;
/// no saved user conversation or provider operation is read or dispatched.
fn persisted_prefix_fixture(conversation: &str) -> (RuntimeSessionService, AgentTranscriptStore) {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root(conversation));
    store
        .append(&TranscriptEntry {
            conversation_id: conversation.to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::Assistant,
            turn_id: "summarized-turn".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "previous raw work".to_string(),
        })
        .unwrap();
    store
        .save_compaction_epoch(conversation, 1, "original durable summary")
        .unwrap();
    service.set_agent_transcript_store(store.clone());
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", conversation, 0)
        .unwrap();
    (service, store)
}
