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

/// The real pane prompt entry point must keep one head and exact retained native
/// source after older covered rows and display copies finally append. Zero pane
/// counts cannot hide the epoch or cause those delayed originals to reenter.
#[test]
fn runtime_prompt_deduplicates_delayed_original_epoch_rows() {
    let fixture = Fixture::new();
    for row in fixture.delayed_original_rows() {
        fixture.store.append(&row).unwrap();
    }
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(fixture.store.clone());
    service
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
    let context = service
        .agent_context_for_pane_prompt("%1", "CURRENT_NEW_TASK", 0)
        .unwrap();
    assert_eq!(
        context.chronology()[0],
        fixture.source.context().chronology()[0]
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
            .any(|block| block.content.contains("COVERED_SOURCE_SENTINEL")
                || block.content.contains("MUST_NOT_REENTER"))
    );
    assert_eq!(
        context.chronology().last().unwrap().block().content,
        "CURRENT_NEW_TASK"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// A real admitted context must retain the supersession witness needed by the
/// existing durable publisher. Cold reconstruction of identical descriptive
/// state cannot substitute for that live transition against the pending parent.
/// Publication covers only eligible old intent; native source and current task
/// remain exact and no ordinary append or provider/action is manufactured.
#[test]
fn runtime_admitted_epoch_keeps_live_witness_for_durable_publication() {
    use crate::test_support::replay_epoch::source_origin;
    use mez_agent::{
        AgentContext, ReplayCompactionPreparation, ReplayConversationId, ReplayEpochContext,
        ReplayHeadPublication, ReplayTailPolicy,
    };
    let fixture = Fixture::new();
    let delayed = fixture.delayed_original_rows();
    fixture.store.append_many(&delayed).unwrap();
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(fixture.store.clone());
    service
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
    // Seal the current prompt, then exercise the actual sparse prelude insertion
    // across a live-owner round trip, rather than only before the first export.
    context.update_replay_owner(|_| Ok(())).unwrap();
    context
        .insert_task_preludes_before_active_user(vec![ContextBlock::task_prelude(
            ContextSourceKind::SkillInstruction,
            "late skill",
            "current task instructions",
        )])
        .unwrap();
    let live = ReplayEpochContext::from_owned_context(
        ReplayConversationId::new(CONVERSATION).unwrap(),
        context,
    )
    .unwrap();
    live.validate_source_extension(&fixture.source).unwrap();
    let cold = ReplayEpochContext::from_checkpoint(
        ReplayConversationId::new(CONVERSATION).unwrap(),
        live.checkpoint().unwrap(),
        AgentContext::empty(),
    )
    .unwrap();
    let prepare = |source: &ReplayEpochContext| {
        let plan = match source
            .prepare_compaction(ReplayTailPolicy::new(1000, 0).unwrap())
            .unwrap()
        {
            ReplayCompactionPreparation::Ready(plan) => plan,
            other => panic!("expected historical intent, got {other:?}"),
        };
        fixture
            .store
            .prepare_replay_epoch(
                source_origin(),
                CONVERSATION,
                Some(fixture.identity.clone()),
                source,
                plan,
                ReplayHeadPublication::Summary("model head 4".into()),
            )
            .unwrap()
    };
    assert!(fixture.store.publish_replay_epoch(&prepare(&cold)).is_err());
    let proposal = prepare(&live);
    let mut body = live.into_owned_context();
    let original_native_source = body
        .replay_execution_append_owner(&fixture.group)
        .unwrap()
        .clone();
    let current_prefix = body.stable_slots().to_vec();
    let current_metadata = body.metadata().clone();
    // This arrival is not in the prepared checkpoint. A real worker publishes
    // that checkpoint, and its ACK must adopt the exact head into this body.
    body.append_runtime_fact_event("during write", "actual runtime arrival")
        .unwrap();
    let arrival = body.chronology().last().unwrap().clone();
    let ack = std::thread::scope(|scope| {
        scope
            .spawn(|| fixture.store.publish_replay_epoch(&proposal))
            .join()
            .unwrap()
            .unwrap()
    });
    ack.adopt(&fixture.store, &proposal, &mut body).unwrap();
    assert_eq!(body.stable_slots(), current_prefix);
    assert_eq!(body.metadata(), &current_metadata);
    assert_eq!(body.chronology().last(), Some(&arrival));
    assert_eq!(
        body.replay_execution_append_owner(&fixture.group),
        Some(&original_native_source)
    );
    assert!(body.execution_group_is_pending(&fixture.group));
    let before = body.clone();
    ack.adopt(&fixture.store, &proposal, &mut body).unwrap();
    assert_eq!(body, before);
    let (_, restored) = fixture
        .store
        .load_replay_epoch(CONVERSATION, AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(restored.context().chronology()[0], body.chronology()[0]);
    assert_eq!(
        restored.context().chronology()[0].block().content,
        "model head 4"
    );
    assert_eq!(
        restored.context().chronology()[1],
        fixture.source.context().chronology()[2]
    );
    assert!(
        restored
            .context()
            .execution_group_is_pending(&fixture.group)
    );
    assert_eq!(
        restored
            .context()
            .chronology()
            .last()
            .unwrap()
            .block()
            .content,
        "CURRENT_NEW_TASK"
    );
    assert_eq!(fixture.store.inspect(CONVERSATION).unwrap(), delayed);
    service.terminate_all_pane_processes().unwrap();
}
