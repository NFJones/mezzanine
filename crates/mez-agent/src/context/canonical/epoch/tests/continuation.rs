//! Real live-body moves, private lifecycle continuity and source substitution
//! controls. Fixtures exercise controller methods without executing effects.

use super::*;

/// Re-protection is a new obligation even when the source frontier is unchanged.
/// A completed handle must not close that renewed allocation; a freshly captured
/// handle may close it, and only that exact completion verifies afterward.
#[test]
fn owned_context_retired_handle_cannot_close_renewed_same_frontier() {
    let mut source = owner();
    let group = ContextExecutionGroupId::new("renewed protection").unwrap();
    let frontier = source
        .append_execution_events([ContextConversationAppend::assistant(
            "assistant",
            "closed source",
            group.clone(),
        )])
        .unwrap()[0];
    let retired = source.capture_execution_owner(&group, frontier).unwrap();
    source.finish_execution_group(&retired).unwrap();
    let mut context = source.into_owned_context();
    context.retain_pending_execution_group(&group).unwrap();
    let before = context.clone();
    assert!(
        context
            .update_replay_owner(|owner| owner.finish_execution_group(&retired))
            .is_err(),
        "retired source fence closed a renewed protection"
    );
    assert_eq!(context, before);
    context.retain_pending_execution_group(&group).unwrap();
    assert_eq!(
        context, before,
        "still-pending retention must remain idempotent"
    );
    let renewed = context
        .update_replay_owner(|owner| owner.capture_execution_owner(&group, frontier))
        .unwrap();
    assert_ne!(renewed, retired);
    context
        .update_replay_owner(|owner| owner.finish_execution_group(&renewed))
        .unwrap();
    context
        .update_replay_owner(|owner| {
            assert!(owner.execution_group_finished(&renewed));
            assert!(!owner.execution_group_finished(&retired));
            Ok(())
        })
        .unwrap();
}

/// An older pending snapshot may have the same source and pending flag, but a
/// different protection allocation. Rebase and repeated retention cannot roll
/// the private fence backward and make the older handle usable again.
#[test]
fn owned_context_rejects_pending_allocation_rollback() {
    let mut source = owner();
    let group = ContextExecutionGroupId::new("pending allocation rollback").unwrap();
    let frontier = source
        .append_execution_events([ContextConversationAppend::assistant(
            "assistant",
            "unchanged source",
            group.clone(),
        )])
        .unwrap()[0];
    let retired = source.capture_execution_owner(&group, frontier).unwrap();
    let stale = source.clone().into_owned_context();
    source.finish_execution_group(&retired).unwrap();
    let mut current = source.into_owned_context();
    current.retain_pending_execution_group(&group).unwrap();
    let frozen = current.chronology().to_vec();
    current.rebase_chronology_suffix(&frozen, &stale).unwrap();
    let before = current.clone();
    assert!(
        current
            .update_replay_owner(|owner| owner.finish_execution_group(&retired))
            .is_err()
    );
    assert!(current.retain_pending_execution_group(&group).is_err());
    assert_eq!(current, before);
}

/// Append fences are captured at actual source admission and survive live body
/// moves. An older fence cannot close a newer append; exact completed-source
/// verification survives retries but cannot be reconstructed by a cold checkpoint.
#[test]
fn owned_context_append_fences_preserve_original_retirement_source() {
    let mut context = owner().into_owned_context();
    let group = ContextExecutionGroupId::new("actual append group").unwrap();
    context
        .append_assistant_event("assistant", "original source", group.clone())
        .unwrap();
    let first = context
        .replay_execution_append_owner(&group)
        .unwrap()
        .clone();
    let sequence = context
        .append_evidence_event(
            ContextSourceKind::ActionResult,
            "settled evidence",
            "actual result",
            group.clone(),
            None,
            true,
        )
        .unwrap();
    let completed = context
        .replay_execution_append_owner(&group)
        .unwrap()
        .clone();
    assert_eq!(completed.frontier(), sequence);
    assert_ne!(first, completed);
    assert!(
        context
            .update_replay_owner(|owner| owner.finish_execution_group(&first))
            .is_err()
    );
    context
        .update_replay_owner(|owner| owner.finish_execution_group(&completed))
        .unwrap();
    assert!(context.replay_execution_append_owner(&group).is_none());
    let live = ReplayEpochContext::from_owned_context(
        ReplayConversationId::new("conversation-a").unwrap(),
        context,
    )
    .unwrap();
    assert!(live.execution_group_finished(&completed));
    let cold = ReplayEpochContext::from_checkpoint(
        ReplayConversationId::new("conversation-a").unwrap(),
        live.checkpoint().unwrap(),
        AgentContext::empty(),
    )
    .unwrap();
    assert!(!cold.execution_group_finished(&completed));
}

/// Restoring a live body must retain actual task handles and completed-source
/// witnesses, while cold descriptive restoration gets a new incarnation.
#[test]
fn owned_context_preserves_incarnation_lifecycle_and_provider_view() {
    let mut source = owner();
    let task = source.begin_instruction_task().unwrap();
    source
        .append_instruction(&task, ContextBlock::user_event("task", "old intent"))
        .unwrap();
    let parent = source.clone();
    source
        .select_provider_projection(
            ReplayProviderProjection::new(
                crate::ProviderApiCompatibility::OpenAiResponses,
                "provider",
                "model",
            )
            .unwrap(),
        )
        .unwrap();
    let mut context = source.into_owned_context();
    let unchanged = context.chronology().to_vec();
    context
        .update_replay_owner(|live| live.complete_instruction_task(&task))
        .unwrap();
    context
        .append_runtime_fact_event("arrival", "new source")
        .unwrap();
    let live = ReplayEpochContext::from_owned_context(
        ReplayConversationId::new("conversation-a").unwrap(),
        context,
    )
    .unwrap();
    live.validate_source_extension(&parent).unwrap();
    assert_eq!(live.context().chronology()[0].block(), unchanged[0].block());
    assert_eq!(
        live.context().chronology()[0].record(),
        unchanged[0].record()
    );
    assert_eq!(live.members().last().unwrap().born_in, live.revision());
    assert!(live.provider_projection().is_some());
    assert_eq!(
        live.provider_input_estimates()
            .unwrap()
            .total_input_tokens()
            .unwrap(),
        live.input_estimates().total_input_tokens().unwrap()
    );
    let cold = ReplayEpochContext::from_checkpoint(
        ReplayConversationId::new("conversation-a").unwrap(),
        live.checkpoint().unwrap(),
        AgentContext::empty(),
    )
    .unwrap();
    assert!(cold.validate_source_extension(&parent).is_err());
    assert!(
        ReplayEpochContext::from_owned_context(
            ReplayConversationId::new("conversation-a").unwrap(),
            cold.context().clone()
        )
        .is_err()
    );
}

/// Callback failure must leave the actual context and its private task state
/// untouched, including a mutation completed before the callback failed.
#[test]
fn owned_context_updates_are_atomic_and_reject_foreign_namespace() {
    let mut source = owner();
    let task = source.begin_instruction_task().unwrap();
    source
        .append_instruction(&task, ContextBlock::user_event("task", "intent"))
        .unwrap();
    let mut context = source.into_owned_context();
    let before = context.clone();
    let result: AgentContextResult<()> = context.update_replay_owner(|live| {
        live.complete_instruction_task(&task)?;
        Err(AgentContextError::new("injected controller refusal"))
    });
    assert!(result.is_err());
    assert_eq!(context, before);
    assert!(
        ReplayEpochContext::from_owned_context(
            ReplayConversationId::new("other-conversation").unwrap(),
            context.clone()
        )
        .is_err()
    );
    assert!(
        ReplayEpochContext::new(
            ReplayConversationId::new("conversation-a").unwrap(),
            context.clone()
        )
        .is_err()
    );
    assert!(
        ReplayEpochContext::from_checkpoint(
            ReplayConversationId::new("conversation-a").unwrap(),
            owner().checkpoint().unwrap(),
            owner().into_owned_context()
        )
        .is_err()
    );
    context
        .update_replay_owner(|live| live.complete_instruction_task(&task))
        .unwrap();
    assert_eq!(
        context.chronology()[0].retention(),
        ContextRetention::Summarizable
    );
}

/// Canonically valid generic edits cannot impersonate source retained by an
/// actual owner: altered bytes/framing/metadata, drops, reordering and sequence
/// rewrites are each rejected by the private original seals.
#[test]
fn owned_context_rejects_rewritten_original_source() {
    let mut source = owner();
    source.append_fact("first", "first source").unwrap();
    source.append_fact("second", "second source").unwrap();
    let context = source.into_owned_context();
    for case in 0..5 {
        let mut edited = context.clone();
        match case {
            0 => {
                edited.chronology[0].block.content = "substituted source".into();
                edited.chronology[0].record = edited.chronology[0]
                    .record
                    .revised(&edited.chronology[0].block)
                    .unwrap();
            }
            1 => {
                edited.chronology.remove(0);
            }
            2 => {
                let first = edited.chronology[0].sequence;
                let second = edited.chronology[1].sequence;
                edited.chronology.swap(0, 1);
                edited.chronology[0].sequence = first;
                edited.chronology[1].sequence = second;
            }
            3 => {
                edited.chronology[0].recoverable_for_compaction = false;
            }
            4 => {
                edited.chronology[0].sequence = ContextEventSequence::new(1).unwrap();
            }
            _ => unreachable!(),
        }
        edited.rebuild_projections();
        edited.validate_durable().unwrap();
        assert!(
            ReplayEpochContext::from_owned_context(
                ReplayConversationId::new("conversation-a").unwrap(),
                edited
            )
            .is_err(),
            "source substitution case {case}"
        );
    }
}

/// Only the actual pre-dispatch prelude inserter may use a reserved sequence
/// gap. A canonically valid hand-constructed insertion has no private witness.
#[test]
fn owned_context_qualifies_actual_late_preludes_without_moving_source() {
    let mut source = owner();
    let task = source.begin_instruction_task().unwrap();
    let prompt = source
        .append_instruction(&task, ContextBlock::user_event("task", "intent"))
        .unwrap();
    let mut context = source.into_owned_context();
    let before = context.chronology()[0].clone();
    context
        .insert_task_preludes_before_active_user(vec![ContextBlock::task_prelude(
            ContextSourceKind::SkillInstruction,
            "skill",
            "current skill instructions",
        )])
        .unwrap();
    let live = ReplayEpochContext::from_owned_context(
        ReplayConversationId::new("conversation-a").unwrap(),
        context.clone(),
    )
    .unwrap();
    assert_eq!(live.context().chronology()[1], before);
    assert_eq!(live.members()[1].origin(), prompt);
    assert_eq!(live.members().len(), 2);
    let mut forged = owner();
    let task = forged.begin_instruction_task().unwrap();
    forged
        .append_instruction(&task, ContextBlock::user_event("task", "intent"))
        .unwrap();
    let mut forged = forged.into_owned_context();
    forged.chronology.insert(0, context.chronology()[0].clone());
    forged.rebuild_projections();
    forged.validate_durable().unwrap();
    assert!(
        ReplayEpochContext::from_owned_context(
            ReplayConversationId::new("conversation-a").unwrap(),
            forged
        )
        .is_err()
    );
}

/// Actual generic runtime handoff must retain its controller witness through
/// context moves. Decoding the same completed state does not acquire that proof.
#[test]
fn owned_context_runtime_peer_release_preserves_actual_transition() {
    let mut source = owner();
    let peer = source
        .append_pending_peer("peer", "untrusted source")
        .unwrap();
    let parent = source.clone();
    let mut context = source.into_owned_context();
    assert!(context.release_pending_peer_message(peer));
    assert!(!context.release_pending_peer_message(peer));
    let live = ReplayEpochContext::from_owned_context(
        ReplayConversationId::new("conversation-a").unwrap(),
        context,
    )
    .unwrap();
    live.validate_source_extension(&parent).unwrap();
    let cold = ReplayEpochContext::from_checkpoint(
        ReplayConversationId::new("conversation-a").unwrap(),
        live.checkpoint().unwrap(),
        AgentContext::empty(),
    )
    .unwrap();
    assert!(cold.validate_source_extension(&parent).is_err());
}

/// A context-held native group remains protected until its actual declaration
/// and result pair close. Runtime release preserves that frontier witness and
/// reopening by an appended call invalidates old closure handles and proofs.
#[test]
fn owned_context_runtime_native_release_requires_complete_actual_source() {
    let mut source = owner();
    let group = ContextExecutionGroupId::new("native").unwrap();
    let provider = crate::ProviderContinuityOwner::new(
        crate::ProviderApiCompatibility::OpenAiChatCompletions,
        "provider",
    )
    .unwrap();
    let call =
        crate::ProviderTranscriptEvent::validated_openai_chat_completions_assistant_tool_call(
            "provider".into(),
            "declaration".into(),
            vec![serde_json::json!({
                "id":"call", "type":"function", "function":{"name":crate::MAAP_ACTION_BATCH_TOOL_NAME, "arguments":"{}"}
            })],
        )
        .unwrap();
    let declaration = ContextConversationAppend::provider_assistant(
        "native call",
        call.to_transcript_content(),
        group.clone(),
        provider.clone(),
    );
    let frontier = source
        .append_execution_events([declaration.clone()])
        .unwrap()[0];
    let stale = source.capture_execution_owner(&group, frontier).unwrap();
    let parent = source.clone();
    let mut context = source.into_owned_context();
    let before = context.clone();
    assert!(!context.release_pending_execution_group(&group));
    assert_eq!(context, before);
    let result = crate::ProviderTranscriptEvent::OpenAiChatCompletionsToolResult {
        provider_id: "provider".into(),
        tool_call_id: "call".into(),
        content: "settled result".into(),
    };
    let result_sequences = context
        .append_conversation_events([ContextConversationAppend::evidence(
            ContextSourceKind::TranscriptTool,
            "native result",
            result.to_transcript_content(),
            group.clone(),
            Some(provider),
            true,
        )])
        .unwrap();
    assert!(!context.release_pending_execution_group(&group));
    let completed = context
        .update_replay_owner(|live| live.capture_execution_owner(&group, result_sequences[0]))
        .unwrap();
    context
        .update_replay_owner(|live| live.finish_execution_group(&completed))
        .unwrap();
    let live = ReplayEpochContext::from_owned_context(
        ReplayConversationId::new("conversation-a").unwrap(),
        context.clone(),
    )
    .unwrap();
    live.validate_source_extension(&parent).unwrap();
    context.append_conversation_events([declaration]).unwrap();
    assert!(context.execution_group_is_pending(&group));
    assert!(!context.release_pending_execution_group(&group));
    assert!(
        context
            .update_replay_owner(|live| live.finish_execution_group(&completed))
            .is_err()
    );
    assert!(
        context
            .update_replay_owner(|live| live.finish_execution_group(&stale))
            .is_err()
    );
    let reopened = ReplayEpochContext::from_owned_context(
        ReplayConversationId::new("conversation-a").unwrap(),
        context,
    )
    .unwrap();
    assert!(reopened.context().execution_group_is_pending(&group));
    assert!(!reopened.source_lifecycle.closed_groups.contains_key(&group));
}

/// New peer and imported execution arrivals must be protected immediately in
/// the single live body. Reacquiring an owner cannot be the first time a pending
/// obligation becomes visible to other canonical readers.
#[test]
fn owned_context_protects_actual_and_imported_arrivals_immediately() {
    let mut context = owner().into_owned_context();
    let peer = context
        .append_peer_message_event("peer", "untrusted input")
        .unwrap();
    assert!(context.peer_message_is_pending(peer));
    let group = ContextExecutionGroupId::new("imported").unwrap();
    let mut suffix = AgentContext::empty();
    suffix
        .append_assistant_event("assistant", "new declaration", group.clone())
        .unwrap();
    context.append_imported_history(&suffix).unwrap();
    assert!(context.execution_group_is_pending(&group));
    let task = context
        .update_replay_owner(|live| live.begin_instruction_task())
        .unwrap();
    context
        .update_replay_owner(|live| {
            live.append_instruction(&task, ContextBlock::user_event("task", "new intent"))
        })
        .unwrap();
    context
        .update_replay_owner(|live| live.complete_instruction_task(&task))
        .unwrap();
    assert!(context.release_pending_peer_message(peer));
    assert!(!context.release_pending_execution_group(&group));
    assert!(context.execution_group_is_pending(&group));
}

/// A legitimate late prelude can fall below the parent's append reservation.
/// Only the actual insertion witness permits that source extension; a cold
/// checkpoint of the same bytes cannot invent that controller transition.
#[test]
fn owned_context_late_prelude_publication_requires_actual_insertion_witness() {
    let mut source = owner();
    let old_task = source.begin_instruction_task().unwrap();
    source
        .append_instruction(&old_task, ContextBlock::user_event("old", "old task"))
        .unwrap();
    source.complete_instruction_task(&old_task).unwrap();
    let parent = source.clone();
    let mut context = source.into_owned_context();
    context
        .append_user_event("user prompt", "new task")
        .unwrap();
    context
        .insert_task_preludes_before_active_user(vec![ContextBlock::task_prelude(
            ContextSourceKind::SkillInstruction,
            "late skill",
            "current instructions",
        )])
        .unwrap();
    assert!(context.chronology()[1].sequence().get() < parent.context.next_event_sequence);
    let live = ReplayEpochContext::from_owned_context(
        ReplayConversationId::new("conversation-a").unwrap(),
        context,
    )
    .unwrap();
    live.validate_source_extension(&parent).unwrap();
    let cold = ReplayEpochContext::from_checkpoint(
        ReplayConversationId::new("conversation-a").unwrap(),
        live.checkpoint().unwrap(),
        AgentContext::empty(),
    )
    .unwrap();
    assert!(cold.validate_source_extension(&parent).is_err());
}

/// Private pending protection may be released only through the real controller
/// API. Removing descriptive flags from an otherwise valid body cannot certify
/// either peer handoff or execution retirement against its retained source.
#[test]
fn owned_context_rejects_unwitnessed_protection_removal() {
    let mut source = owner();
    let peer = source
        .append_pending_peer("peer", "untrusted input")
        .unwrap();
    let group = ContextExecutionGroupId::new("pending group").unwrap();
    source
        .append_execution_events([ContextConversationAppend::assistant(
            "assistant",
            "declaration",
            group.clone(),
        )])
        .unwrap();
    let context = source.into_owned_context();
    for peer_case in [true, false] {
        let mut edited = context.clone();
        if peer_case {
            edited.pending_peer_messages.remove(&peer);
        } else {
            edited.pending_execution_groups.remove(&group);
        }
        edited.validate_durable().unwrap();
        assert!(
            ReplayEpochContext::from_owned_context(
                ReplayConversationId::new("conversation-a").unwrap(),
                edited
            )
            .is_err()
        );
    }
}

/// Live ownership transport must reuse the existing immutable cost records;
/// sealing and reacquisition do not allocate or discard estimates for unchanged
/// text, even while refreshing provider-view totals from those owned records.
#[test]
fn owned_context_round_trip_reuses_owned_estimates() {
    let mut source = owner();
    source
        .append_fact("fact", "unchanged original source")
        .unwrap();
    let before = crate::context::canonical::block_record::ESTIMATE_COMPUTATIONS.with(|n| n.get());
    let context = source.into_owned_context();
    let mut source = ReplayEpochContext::from_owned_context(
        ReplayConversationId::new("conversation-a").unwrap(),
        context,
    )
    .unwrap();
    source
        .select_provider_projection(
            ReplayProviderProjection::new(
                crate::ProviderApiCompatibility::OpenAiResponses,
                "provider",
                "model",
            )
            .unwrap(),
        )
        .unwrap();
    let _ = ReplayEpochContext::from_owned_context(
        ReplayConversationId::new("conversation-a").unwrap(),
        source.into_owned_context(),
    )
    .unwrap();
    assert_eq!(
        crate::context::canonical::block_record::ESTIMATE_COMPUTATIONS.with(|n| n.get()),
        before
    );
}

/// A public compatibility rebase can carry an older pending snapshot even when
/// all event bytes match. Actual re-retention must update the private protection
/// fence immediately, so that stale snapshot cannot erase a new obligation or
/// reuse a former peer/group release witness during later owner conversion.
#[test]
fn owned_context_rejects_stale_rebase_after_source_is_protected_again() {
    let mut source = owner();
    let peer = source.append_pending_peer("peer", "original peer").unwrap();
    source.release_peer(peer).unwrap();
    let group = ContextExecutionGroupId::new("original group").unwrap();
    source
        .append_execution_events([ContextConversationAppend::assistant(
            "assistant",
            "closed original response",
            group.clone(),
        )])
        .unwrap();
    finish_group(&mut source, &group).unwrap();
    let context = source.into_owned_context();
    for peer_case in [true, false] {
        let mut current = context.clone();
        if peer_case {
            current.retain_pending_peer_message(peer).unwrap();
        } else {
            current.retain_pending_execution_group(&group).unwrap();
        }
        let frozen = current.chronology().to_vec();
        current.rebase_chronology_suffix(&frozen, &context).unwrap();
        current.validate_durable().unwrap();
        assert!(
            ReplayEpochContext::from_owned_context(
                ReplayConversationId::new("conversation-a").unwrap(),
                current
            )
            .is_err(),
            "stale protection snapshot for peer={peer_case}"
        );
    }
}
