//! Actual pane prompt admission from a cold durable one-head epoch.

use super::*;
use crate::test_support::replay_epoch::{CONVERSATION, Fixture, OLD_PROMPT, SAVED_POLICY};

/// The normal pane prompt path must load an epoch even with zero ordinary/pane
/// history counts, preserve native source identity in a fresh runtime and compose
/// current instructions/new prompt rather than reimporting frozen flat blocks.
#[test]
fn runtime_prompt_restores_single_head_epoch_without_flattening_source() {
    let fixture = Fixture::new();
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(fixture.store.clone());
    let _primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", CONVERSATION, 0)
        .unwrap();
    let mut context = service
        .agent_context_for_pane_prompt("%1", "CURRENT_NEW_TASK", 0)
        .unwrap();
    context.validate_durable().unwrap();
    context
        .insert_task_preludes_before_active_user(vec![ContextBlock::task_prelude(
            ContextSourceKind::SkillInstruction,
            "late current skill",
            "current task skill instructions",
        )])
        .unwrap();
    context.validate_durable().unwrap();
    assert_eq!(
        context.metadata().prompt_cache_session_id.as_deref(),
        Some(CONVERSATION)
    );
    assert_eq!(
        context.chronology()[0],
        fixture.source.context().chronology()[0]
    );
    assert_eq!(context.chronology()[1].block().content, OLD_PROMPT);
    assert_eq!(
        context.chronology()[1].record(),
        fixture.source.context().chronology()[1].record()
    );
    assert_eq!(context.chronology()[1].sequence(), fixture.instruction);
    assert_eq!(
        context.chronology()[1].retention(),
        mez_agent::ContextRetention::Summarizable
    );
    assert_eq!(
        context.chronology()[2],
        fixture.source.context().chronology()[2]
    );
    assert!(context.execution_group_is_pending(&fixture.group));
    assert!(
        !context
            .blocks()
            .iter()
            .any(|block| block.content == SAVED_POLICY
                || block.content.contains("COVERED_SOURCE_SENTINEL"))
    );
    assert_eq!(
        context.chronology().last().unwrap().block().content,
        "CURRENT_NEW_TASK"
    );
    assert!(fixture.store.inspect(CONVERSATION).unwrap().is_empty());
    service.terminate_all_pane_processes().unwrap();
}

/// The actual peer-turn entry point must preserve an unfinished saved user task.
/// This differs from an explicit new user prompt: peer-triggered framing grants
/// no new user authorship and cannot turn protected intent into historical data.
#[test]
fn runtime_peer_context_preserves_single_head_user_task() {
    let fixture = Fixture::new();
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(fixture.store.clone());
    let _primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", CONVERSATION, 0)
        .unwrap();
    let prepared = service.peer_message_turn_context("%1", 0).unwrap();
    assert_eq!(prepared.delivered_message_count, 0);
    prepared.context.validate_durable().unwrap();
    assert_eq!(
        prepared.context.chronology()[..3],
        fixture.source.context().chronology()[..]
    );
    assert_eq!(
        prepared.context.chronology()[1].retention(),
        mez_agent::ContextRetention::Exact
    );
    assert_eq!(
        prepared
            .context
            .chronology()
            .iter()
            .filter(|event| event.block().source == ContextSourceKind::UserInstruction)
            .count(),
        1
    );
    assert!(prepared.context.execution_group_is_pending(&fixture.group));
    service.terminate_all_pane_processes().unwrap();
}
